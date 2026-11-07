//! JMA: the Japan Meteorological Agency's high-resolution precipitation
//! nowcast, from the Web-Mercator tiles its own map uses, under
//! `https://www.jma.go.jp/bosai/jmatile/data/nowc/`:
//!
//! * `targetTimes_N1.json` (observed, `basetime == validtime`, every 5
//!   minutes, 3 hours back) and `targetTimes_N2.json` (the newest run's
//!   forecast, `validtime` every 5 minutes to +60), newest first, UTC
//!   `YYYYMMDDhhmmss`. Both carry an `ETag`, so polling them while nothing
//!   is new costs a 304.
//! * Tiles: `{basetime}/none/{validtime}/surf/hrpns/{z}/{x}/{y}.png`, 256
//!   px palette PNGs of rain-rate classes, decoded by
//!   `wx_formats::jma::decode_tiles` (each class at its middle rate).
//!   Older runs stay on the server after `targetTimes_N2.json` moves on.
//!
//! `jma_radar` ([`JmaRadar`]) takes each observed time and `jma_nowcast`
//! ([`JmaNowcast`]) follows a run every [`reg::NOWCAST_FOLLOW_MINUTES`],
//! both at zoom [`reg::TILE_ZOOM`] (only even zooms have data). Only the
//! tiles that touch JMA's coverage ([`reg::TILES`]) are fetched, a few at
//! a time; the rest of their rectangle is unknown (NaN), and so is every
//! pixel outside the coverage polygon JMA publishes with each analysis
//! (`hrpns_nd`, asked for once an hour). No frame is built without a
//! polygon: while one cannot be had, the last one read stands, or the copy
//! built in ([`BUILT_IN_COVERAGE`]).
//!
//! A tile that is not there (404) means different things. In an analysis,
//! where JMA serves an empty PNG for a dry tile, it is a tile the server
//! does not have: when it is the first tile asked for, the time is not
//! there yet (and for the newest time listed, which must be there, the
//! task fails); any other is waited for [`HOLES_AFTER`], then left a hole.
//! In a forecast JMA answers 404 for every dry tile, so there it is dry; a
//! run older than the one `targetTimes_N2.json` names is read only when
//! `targetTimes_N1.json` has its analysis, and a lead with no tile at all
//! is dry only when another lead of the run had tiles or the run's
//! analysis is dry everywhere. Otherwise it is waited for, or, when the
//! analysis has no tiles either, the task fails: paths that have changed
//! show as a failing source, not a dry forecast. So does a time list that
//! has stopped ([`STALE`]).

use crate::driver::{
    Cadence, Cx, Cycle, Decoded, Decoder, Fetched, Listing, Model, ModelFile, Need, ObsFile,
    Observed, Payload,
};
use crate::lock;
use crate::remote::{FetchError, Validators, codec};
use anyhow::{Context, ensure};
use bytes::{BufMut, Bytes, BytesMut};
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use futures::{StreamExt, TryStreamExt};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use wx_core::SourceDef;
use wx_core::families::jma as reg;
use wx_core::proj::{lat_to_y, lon_to_x, x_to_lon, y_to_lat};

pub const BASE: &str = "https://www.jma.go.jp/bosai/jmatile/data/nowc";
/// Tiles of one time in flight at once (the host's own limit applies too).
const TILE_CONCURRENCY: usize = 3;
/// A tile is 300 bytes to a few kilobytes.
const MAX_TILE: u64 = 1024 * 1024;
/// A time list is about 4 kB.
const MAX_LIST: u64 = 1024 * 1024;
/// Mark a slot without a tile in a packed tile set: unknown, or dry.
const UNKNOWN: u32 = u32::MAX;
const DRY: u32 = u32::MAX - 1;
/// The coverage changes when a radar goes down or comes back: asked for
/// this often (168 kB gzipped).
pub const COVERAGE_EVERY: Duration = Duration::hours(1);
/// After failing to get the polygon, asked for again this soon.
pub const COVERAGE_RETRY: Duration = Duration::minutes(2);
/// An analysis with tiles not there waits this long after its time for
/// them before it is built with holes.
pub const HOLES_AFTER: Duration = Duration::minutes(10);
/// A time list naming nothing newer than this long ago has stopped: a
/// failing source, not a quiet one.
pub const STALE: Duration = Duration::minutes(30);
/// The polygon gzipped, at most.
const MAX_COVERAGE: u64 = 8 * 1024 * 1024;
/// Nowcast runs older than this are not fetched: their early leads are
/// already observed and their later ones covered by newer runs. Without
/// it, a start with an empty catalog (no observation yet retiring old
/// leads) would fetch every run of the last 90 minutes, 288 tiles each.
pub const OLDEST_RUN: Duration = Duration::minutes(20);

/// One entry of a time list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub base: DateTime<Utc>,
    pub valid: DateTime<Utc>,
}

/// `YYYYMMDDhhmmss`, as the time lists and tile URLs write times.
pub fn stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%d%H%M%S").to_string()
}

fn parse_stamp(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%S")
        .ok()
        .map(|t| t.and_utc())
}

/// The entries of a time list that carry `element`.
pub fn parse_targets(body: &[u8], element: &str) -> Result<Vec<Target>, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
    let entries = v.as_array().ok_or("not a JSON array")?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let time = |k: &str| e.get(k).and_then(|s| s.as_str()).and_then(parse_stamp);
        let (Some(base), Some(valid)) = (time("basetime"), time("validtime")) else {
            return Err(format!("an entry without basetime and validtime: {e}"));
        };
        let has = e
            .get("elements")
            .and_then(|l| l.as_array())
            .is_some_and(|l| l.iter().any(|x| x.as_str() == Some(element)));
        if has {
            out.push(Target { base, valid });
        }
    }
    Ok(out)
}

/// A time list, asked for again only when it has changed.
#[derive(Default)]
struct TimeList {
    cache: Mutex<Option<(Validators, Vec<Target>)>>,
}

impl TimeList {
    async fn get(&self, cx: &Cx<'_>, url: &str) -> Result<Vec<Target>, FetchError> {
        let since = lock(&self.cache).as_ref().map(|(v, _)| v.clone());
        let mut get = cx.get(url).max_bytes(MAX_LIST);
        if let Some(v) = &since {
            get = get.if_changed(v);
        }
        let Some(reply) = get.send().await? else {
            return Err(FetchError::service(url, "the time list is gone (404)"));
        };
        if reply.not_modified
            && let Some((_, targets)) = lock(&self.cache).as_ref()
        {
            return Ok(targets.clone());
        }
        let targets =
            parse_targets(&reply.body, reg::PRODUCT).map_err(|e| FetchError::object(url, e))?;
        *lock(&self.cache) = Some((reply.validators, targets.clone()));
        Ok(targets)
    }
}

/// The tiles of one zoom that are fetched: a rectangle `x0.., y0..` of
/// `cols` by `rows` tiles, of which those marked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileSet {
    pub z: u8,
    pub x0: u32,
    pub y0: u32,
    pub cols: u32,
    pub rows: u32,
    /// Row by row from `(x0, y0)`.
    pub wanted: Vec<bool>,
}

impl TileSet {
    /// From columns `(x, first y, last y)`, as the registry lists them.
    pub fn new(z: u8, columns: &[(u32, u32, u32)]) -> Self {
        let x0 = columns.iter().map(|c| c.0).min().unwrap_or(0);
        let x1 = columns.iter().map(|c| c.0).max().unwrap_or(0);
        let y0 = columns.iter().map(|c| c.1).min().unwrap_or(0);
        let y1 = columns.iter().map(|c| c.2).max().unwrap_or(0);
        let (cols, rows) = (x1 - x0 + 1, y1 - y0 + 1);
        let mut wanted = vec![false; (cols * rows) as usize];
        for &(x, first, last) in columns {
            for y in first..=last {
                wanted[((y - y0) * cols + (x - x0)) as usize] = true;
            }
        }
        Self {
            z,
            x0,
            y0,
            cols,
            rows,
            wanted,
        }
    }

    /// The tiles fetched, `(slot, x, y)`, row by row.
    pub fn tiles(&self) -> Vec<(usize, u32, u32)> {
        (0..self.wanted.len())
            .filter(|&k| self.wanted[k])
            .map(|k| {
                (
                    k,
                    self.x0 + k as u32 % self.cols,
                    self.y0 + k as u32 / self.cols,
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.wanted.iter().filter(|w| **w).count()
    }
}

/// The tiles fetched of every analysis and every forecast lead.
pub fn tiles() -> TileSet {
    TileSet::new(reg::TILE_ZOOM, reg::TILES)
}

pub fn tile_url(base: &str, target: Target, z: u8, x: u32, y: u32) -> String {
    format!(
        "{base}/{}/none/{}/surf/{}/{z}/{x}/{y}.png",
        stamp(target.base),
        stamp(target.valid),
        reg::PRODUCT
    )
}

/// One slot of a tile rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot<B> {
    Tile(B),
    /// Not fetched, or not there: unknown.
    Unknown,
    /// Answered 404 where that means no rain (a forecast).
    Dry,
}

/// What a tile answered 404 means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Absent {
    /// In an analysis JMA serves an empty PNG where it is dry, so a 404 is
    /// a tile it does not have, or a time not written yet.
    Unknown,
    /// In a forecast JMA answers 404 where it is dry (checked 2026-11-06:
    /// the same tiles of the analysis were empty PNGs).
    Dry,
}

/// Every slot of `set`'s rectangle in order: a little-endian `u32` length
/// ([`UNKNOWN`] or [`DRY`] for a slot without a tile) and that many bytes.
fn pack(set: &TileSet, tiles: &[(usize, Slot<Bytes>)]) -> Bytes {
    let total: usize = tiles
        .iter()
        .map(|(_, t)| if let Slot::Tile(b) = t { b.len() } else { 0 })
        .sum();
    let mut out = BytesMut::with_capacity(total + 4 * set.wanted.len());
    let mut slots: Vec<&Slot<Bytes>> = vec![&Slot::Unknown; set.wanted.len()];
    for (k, t) in tiles {
        slots[*k] = t;
    }
    for s in slots {
        match s {
            Slot::Tile(b) => {
                out.put_u32_le(b.len() as u32);
                out.put_slice(b);
            }
            Slot::Unknown => out.put_u32_le(UNKNOWN),
            Slot::Dry => out.put_u32_le(DRY),
        }
    }
    out.freeze()
}

fn unpack(bytes: &[u8], slots: usize) -> anyhow::Result<Vec<Slot<&[u8]>>> {
    let mut out = Vec::with_capacity(slots);
    let mut at = 0usize;
    for k in 0..slots {
        let len = bytes.get(at..at + 4).context("packed tiles cut short")?;
        let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]);
        at += 4;
        match len {
            UNKNOWN => out.push(Slot::Unknown),
            DRY => out.push(Slot::Dry),
            _ => {
                let end = at + len as usize;
                out.push(Slot::Tile(
                    bytes
                        .get(at..end)
                        .with_context(|| format!("packed tile {k} cut short"))?,
                ));
                at = end;
            }
        }
    }
    ensure!(
        at == bytes.len(),
        "{} bytes after the packed tiles",
        bytes.len() - at
    );
    Ok(out)
}

/// Decodes a tile set packed by [`fetch_tiles`]. A dry slot is dry where
/// the decoder takes an empty pixel as dry (inside its domain); pixels
/// `observed` marks false are unknown.
pub fn decoder(set: TileSet, observed: Arc<Vec<bool>>) -> Decoder {
    let set = Arc::new(set);
    Arc::new(move |bytes: &[u8], _: &Need| {
        let slots = unpack(bytes, set.wanted.len())?;
        let tiles: Vec<Option<&[u8]>> = slots
            .iter()
            .map(|s| {
                if let Slot::Tile(b) = s {
                    Some(*b)
                } else {
                    None
                }
            })
            .collect();
        let mut field =
            wx_formats::jma::decode_tiles(set.z, set.x0, set.y0, set.cols, set.rows, &tiles)?;
        let size = wx_formats::jma::TILE_SIZE;
        let world = (1u64 << set.z) as f64 * size as f64;
        for (k, _) in slots.iter().enumerate().filter(|(_, s)| **s == Slot::Dry) {
            let (tc, tr) = (k % set.cols as usize, k / set.cols as usize);
            for row in 0..size {
                let j = tr * size + row;
                let lat = y_to_lat((set.y0 as f64 * size as f64 + j as f64 + 0.5) / world);
                for col in 0..size {
                    let i = tc * size + col;
                    let lon = x_to_lon((set.x0 as f64 * size as f64 + i as f64 + 0.5) / world);
                    if wx_formats::jma::JMA_DOMAIN.contains(lon, lat) {
                        field.values[j * field.nx + i] = 0.0;
                    }
                }
            }
        }
        ensure!(
            observed.len() == field.values.len(),
            "a coverage mask for another tile set"
        );
        for (v, seen) in field.values.iter_mut().zip(observed.iter()) {
            if !seen {
                *v = f32::NAN;
            }
        }
        Ok(Decoded { field, valid: None })
    })
}

/// The tiles of one time as fetched.
#[derive(Debug, Clone)]
pub struct Fetch {
    /// Every slot, packed for the decoder.
    pub packed: Bytes,
    /// Tiles the server answered 404 for.
    pub absent: usize,
    /// Tiles asked for.
    pub of: usize,
}

/// The tiles of `set` at `target`. With [`Absent::Unknown`] the first tile
/// is asked for alone, and `None` is the answer when it is not there: the
/// time is not there, and a time listed before its tiles exist (or never
/// given any) costs one request each time it is looked at, not one per
/// tile.
pub async fn fetch_tiles(
    cx: &Cx<'_>,
    base: &str,
    set: &TileSet,
    target: Target,
    absent: Absent,
) -> Result<Option<Fetch>, FetchError> {
    let all = set.tiles();
    let get = |x: u32, y: u32| {
        cx.get(tile_url(base, target, set.z, x, y))
            .max_bytes(MAX_TILE)
            .bytes()
    };
    let mut tiles: Vec<(usize, Slot<Bytes>)> = Vec::with_capacity(all.len());
    let mut rest = &all[..];
    if absent == Absent::Unknown
        && let Some((&(k0, x0, y0), after)) = all.split_first()
    {
        let Some(first) = get(x0, y0).await? else {
            return Ok(None);
        };
        tiles.push((k0, Slot::Tile(first)));
        rest = after;
    }
    let fetched: Vec<(usize, Option<Bytes>)> = futures::stream::iter(rest.iter().copied())
        .map(|(k, x, y)| async move { Ok::<_, FetchError>((k, get(x, y).await?)) })
        .buffered(TILE_CONCURRENCY)
        .try_collect()
        .await?;
    let mut not_there = 0;
    for (k, t) in fetched {
        tiles.push(match (t, absent) {
            (Some(b), _) => (k, Slot::Tile(b)),
            (None, Absent::Dry) => {
                not_there += 1;
                (k, Slot::Dry)
            }
            (None, Absent::Unknown) => {
                not_there += 1;
                (k, Slot::Unknown)
            }
        });
    }
    Ok(Some(Fetch {
        packed: pack(set, &tiles),
        absent: not_there,
        of: all.len(),
    }))
}

/// Whether the analysis at `at` is dry in every tile of `set`: `None` when
/// its tiles are not there.
async fn analysis_is_dry(
    cx: &Cx<'_>,
    base: &str,
    set: &TileSet,
    at: DateTime<Utc>,
) -> Result<Option<bool>, FetchError> {
    let target = Target {
        base: at,
        valid: at,
    };
    let Some(fetch) = fetch_tiles(cx, base, set, target, Absent::Unknown).await? else {
        return Ok(None);
    };
    let set = set.clone();
    let dry = cx
        .cpu(move || -> anyhow::Result<bool> {
            let slots = unpack(&fetch.packed, set.wanted.len())?;
            let tiles: Vec<Option<&[u8]>> = slots
                .iter()
                .map(|s| {
                    if let Slot::Tile(b) = s {
                        Some(*b)
                    } else {
                        None
                    }
                })
                .collect();
            let field =
                wx_formats::jma::decode_tiles(set.z, set.x0, set.y0, set.cols, set.rows, &tiles)?;
            Ok(field.values.iter().all(|v| v.is_nan() || *v <= 0.0))
        })
        .await
        .and_then(|r| r)
        .map_err(|e| FetchError::object(stamp(at), format!("{e:#}")))?;
    Ok(Some(dry))
}

/// The coverage polygon of the analysis at `at`.
pub fn coverage_url(base: &str, at: DateTime<Utc>) -> String {
    let s = stamp(at);
    format!(
        "{base}/{s}/none/{s}/surf/{p}_nd/data.geojson?id={p}_nd",
        p = reg::PRODUCT
    )
}

/// Which pixels of `set`'s rectangle JMA observes, row by row, from the
/// `hrpns_nd` GeoJSON: the area without data, as the world with the
/// observed area cut out of it. A pixel is observed where its centre is
/// inside an even number of the polygons' rings. Refused unless Tokyo is
/// observed and Shanghai is not, so a polygon read the wrong way round
/// does not blank Japan.
pub fn observed_pixels(geojson: &[u8], set: &TileSet) -> anyhow::Result<Vec<bool>> {
    let v: serde_json::Value = serde_json::from_slice(geojson).context("not JSON")?;
    let mut rings: Vec<Vec<(f64, f64)>> = Vec::new();
    for f in v
        .get("features")
        .and_then(|f| f.as_array())
        .context("no features")?
    {
        let g = f.get("geometry").context("a feature without a geometry")?;
        let coords = g
            .get("coordinates")
            .context("a geometry without coordinates")?;
        let polygons: Vec<&serde_json::Value> = match g.get("type").and_then(|t| t.as_str()) {
            Some("Polygon") => vec![coords],
            Some("MultiPolygon") => coords
                .as_array()
                .context("MultiPolygon coordinates")?
                .iter()
                .collect(),
            other => anyhow::bail!("a {other:?} geometry"),
        };
        for polygon in polygons {
            for ring in polygon.as_array().context("polygon rings")? {
                let points: Vec<(f64, f64)> = ring
                    .as_array()
                    .context("ring points")?
                    .iter()
                    .filter_map(|p| Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?)))
                    .collect();
                rings.push(points);
            }
        }
    }
    ensure!(!rings.is_empty(), "no rings");
    let size = wx_formats::jma::TILE_SIZE;
    let world = (1u64 << set.z) as f64 * size as f64;
    let (nx, ny) = (set.cols as usize * size, set.rows as usize * size);
    // Pixel coordinates: pixel (i, j) covers [i, i + 1) x [j, j + 1).
    let px = |lon: f64| lon_to_x(lon) * world - (set.x0 as usize * size) as f64;
    let py = |lat: f64| lat_to_y(lat) * world - (set.y0 as usize * size) as f64;
    // Where each row of pixel centres crosses a ring edge.
    let mut crossings: Vec<Vec<f64>> = vec![Vec::new(); ny];
    for ring in &rings {
        let n = ring.len();
        for k in 0..n {
            let (a, b) = (ring[k], ring[(k + 1) % n]);
            let (ya, yb) = (py(a.1), py(b.1));
            if ya == yb {
                continue;
            }
            let (lo, hi) = (ya.min(yb), ya.max(yb));
            let first = (lo - 0.5).ceil().max(0.0) as usize;
            let last = ((hi - 0.5).ceil().max(0.0) as usize).min(ny);
            let (xa, xb) = (px(a.0), px(b.0));
            for (j, row) in crossings.iter_mut().enumerate().take(last).skip(first) {
                let t = (j as f64 + 0.5 - ya) / (yb - ya);
                row.push(xa + t * (xb - xa));
            }
        }
    }
    let mut observed = vec![true; nx * ny];
    for (j, row) in crossings.iter_mut().enumerate() {
        row.sort_by(f64::total_cmp);
        for [enter, leave] in row.as_chunks::<2>().0 {
            let from = (enter - 0.5).ceil().max(0.0) as usize;
            let to = ((leave - 0.5).ceil().max(0.0) as usize).min(nx);
            for v in observed[j * nx..].iter_mut().take(to).skip(from) {
                *v = false;
            }
        }
    }
    let seen = |lon: f64, lat: f64| {
        let (i, j) = (px(lon).floor(), py(lat).floor());
        (i >= 0.0 && j >= 0.0 && (i as usize) < nx && (j as usize) < ny)
            .then(|| observed[j as usize * nx + i as usize])
    };
    ensure!(
        seen(139.69, 35.69) == Some(true) && seen(121.47, 31.23) == Some(false),
        "the coverage polygon does not cover Tokyo and leave out Shanghai"
    );
    Ok(observed)
}

/// Which pixels of a tile set JMA observes: see [`observed_pixels`].
type Mask = Arc<Vec<bool>>;

/// JMA's coverage polygon of 20:30 on 2026-11-06 as served (gzipped
/// GeoJSON), used until one has been read from the server.
pub const BUILT_IN_COVERAGE: &[u8] = include_bytes!("jma/coverage_20261106T2030.geojson.gz");

#[derive(Default)]
struct CoverageState {
    /// The newest polygon read, and when it was asked for.
    good: Option<(DateTime<Utc>, Mask)>,
    /// When asking last failed.
    failed_at: Option<DateTime<Utc>>,
    /// [`BUILT_IN_COVERAGE`] for the tile set, once needed.
    built_in: Option<Mask>,
}

/// The coverage polygon, asked for once in [`COVERAGE_EVERY`] while that
/// works and again [`COVERAGE_RETRY`] after a failure. No frame is built
/// without one: until a polygon has been read, and while asking fails, the
/// last one read stands, or the built-in copy.
#[derive(Default)]
struct Coverage {
    /// Held while the polygon is asked for, so frames built side by side
    /// share one request.
    state: tokio::sync::Mutex<CoverageState>,
}

impl Coverage {
    /// The pixels of `set` JMA observes, from the polygon of the analysis at
    /// `at` or the last one read. Failing to get or read a polygon never
    /// fails the frame; only a built-in copy that cannot be read does.
    async fn get(
        &self,
        cx: &Cx<'_>,
        base: &str,
        set: &TileSet,
        at: DateTime<Utc>,
    ) -> Result<Mask, FetchError> {
        let mut state = self.state.lock().await;
        if let Some((asked, mask)) = &state.good
            && cx.now - *asked < COVERAGE_EVERY
        {
            return Ok(mask.clone());
        }
        if state.failed_at.is_none_or(|t| cx.now - t >= COVERAGE_RETRY) {
            let url = coverage_url(base, at);
            match read_coverage(cx, &url, set).await {
                Ok(mask) => {
                    let mask = Arc::new(mask);
                    state.good = Some((cx.now, mask.clone()));
                    state.failed_at = None;
                    return Ok(mask);
                }
                Err(e) => {
                    tracing::warn!(%url, "JMA coverage polygon not had, using the last one: {e:#}");
                    state.failed_at = Some(cx.now);
                }
            }
        }
        if let Some((_, mask)) = &state.good {
            return Ok(mask.clone());
        }
        if let Some(mask) = &state.built_in {
            return Ok(mask.clone());
        }
        let set = set.clone();
        let mask = cx
            .cpu(move || {
                observed_pixels(&codec::inflate(BUILT_IN_COVERAGE, MAX_COVERAGE * 16)?, &set)
            })
            .await
            .and_then(|r| r)
            .map_err(|e| {
                FetchError::object("the built-in JMA coverage polygon", format!("{e:#}"))
            })?;
        let mask = Arc::new(mask);
        state.built_in = Some(mask.clone());
        Ok(mask)
    }
}

/// The polygon at `url`, read for `set`.
async fn read_coverage(cx: &Cx<'_>, url: &str, set: &TileSet) -> anyhow::Result<Vec<bool>> {
    let gz = cx
        .get(url)
        .max_bytes(MAX_COVERAGE)
        .bytes()
        .await?
        .context("not there (404)")?;
    let set = set.clone();
    cx.cpu(move || observed_pixels(&codec::inflate(&gz, MAX_COVERAGE * 16)?, &set))
        .await?
}

fn check_region(source: &SourceDef, want: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        source.region == want,
        "{}: the region must be {want}, the time list it follows",
        source.id
    );
    Ok(())
}

/// The observed analyses.
pub struct JmaRadar {
    base: String,
    times: TimeList,
    coverage: Coverage,
    /// The newest analysis `targetTimes_N1.json` lists, whose tiles must be
    /// there, and the newest found without them: the next listing fails
    /// while it is still the newest.
    newest: Mutex<Option<DateTime<Utc>>>,
    newest_absent: Mutex<Option<DateTime<Utc>>>,
}

impl Default for JmaRadar {
    fn default() -> Self {
        Self::at(BASE)
    }
}

impl JmaRadar {
    /// An adapter reading from another server (tests).
    pub fn at(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            times: TimeList::default(),
            coverage: Coverage::default(),
            newest: Mutex::default(),
            newest_absent: Mutex::default(),
        }
    }
}

pub fn times_url(base: &str, region: &str) -> String {
    format!("{base}/targetTimes_{region}.json")
}

#[async_trait::async_trait]
impl Observed for JmaRadar {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        check_region(source, reg::JMA_RADAR.region)
    }

    fn cadence(&self, _: &SourceDef, _: &str) -> Cadence {
        // targetTimes_N1.json was rewritten 26 seconds after 20:30 on
        // 2026-11-06.
        Cadence::every(
            Duration::minutes(reg::EVERY_MINUTES as i64),
            Duration::seconds(40),
            Duration::seconds(20),
        )
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        _: &str,
        from: DateTime<Utc>,
    ) -> Result<Vec<ObsFile>, FetchError> {
        let url = times_url(&self.base, source.region);
        let targets = self.times.get(cx, &url).await?;
        let newest = targets
            .iter()
            .filter(|t| t.base == t.valid)
            .map(|t| t.valid)
            .max();
        if newest.is_none_or(|t| t < cx.now - STALE) {
            return Err(FetchError::service(
                &url,
                format!("no analysis newer than {newest:?}: the list has stopped"),
            ));
        }
        if newest.is_some() && *lock(&self.newest_absent) == newest {
            return Err(FetchError::service(
                &url,
                format!(
                    "the newest analysis listed, {newest:?}, has no tiles: have the paths changed?"
                ),
            ));
        }
        *lock(&self.newest) = newest;
        Ok(targets
            .into_iter()
            .filter(|t| t.base == t.valid && t.valid >= from)
            .map(|t| ObsFile {
                valid: t.valid,
                name: stamp(t.valid),
                size: None,
            })
            .collect())
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let set = tiles();
        let target = Target {
            base: file.valid,
            valid: file.valid,
        };
        let newest = *lock(&self.newest);
        let Some(fetch) = fetch_tiles(cx, &self.base, &set, target, Absent::Unknown).await? else {
            // The list is written after the tiles: the newest it names not
            // being there is the server or its paths having changed.
            if newest == Some(file.valid) {
                *lock(&self.newest_absent) = newest;
                let url = tile_url(
                    &self.base,
                    target,
                    set.z,
                    set.tiles()[0].1,
                    set.tiles()[0].2,
                );
                return Err(FetchError::service(
                    url,
                    "the newest analysis listed has no tiles: have the paths changed?",
                ));
            }
            return Ok(Fetched::Missing);
        };
        if fetch.absent > 0 {
            // A tile that is not there yet is waited for a while, then left
            // a hole, so neither a slow tile nor a lost one holds the
            // frame up for good.
            if file.valid > cx.now - HOLES_AFTER {
                tracing::debug!(valid = %file.valid, missing = fetch.absent, "JMA tiles missing; waiting for them");
                return Ok(Fetched::Missing);
            }
            tracing::warn!(valid = %file.valid, missing = fetch.absent, of = fetch.of, "JMA tiles missing; the frame has holes there");
        }
        let observed = self.coverage.get(cx, &self.base, &set, file.valid).await?;
        Ok(Fetched::all(
            needs,
            Payload::Custom {
                bytes: fetch.packed,
                decode: decoder(set, observed),
            },
        ))
    }
}

/// The nowcast.
pub struct JmaNowcast {
    base: String,
    times: TimeList,
    /// The observed times, which say which older runs exist: a forecast
    /// tile that is not there reads as dry, so a run must be known to
    /// exist before its tiles are read.
    analyses: TimeList,
    coverage: Coverage,
    /// Runs some forecast tile of which was there, and runs whose analysis
    /// was found dry everywhere or not: what a lead with no tile at all is
    /// read against.
    with_tiles: Mutex<BTreeSet<DateTime<Utc>>>,
    dry_analysis: Mutex<BTreeMap<DateTime<Utc>, bool>>,
}

impl Default for JmaNowcast {
    fn default() -> Self {
        Self::at(BASE)
    }
}

impl JmaNowcast {
    /// An adapter reading from another server (tests).
    pub fn at(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            times: TimeList::default(),
            analyses: TimeList::default(),
            coverage: Coverage::default(),
            with_tiles: Mutex::default(),
            dry_analysis: Mutex::default(),
        }
    }
}

/// The name [`Model::file`] gives the tiles of `lead` in `run`.
pub fn lead_name(run: DateTime<Utc>, lead: u32) -> String {
    format!(
        "{}/{}",
        stamp(run),
        stamp(run + Duration::minutes(lead as i64))
    )
}

#[async_trait::async_trait]
impl Model for JmaNowcast {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        check_region(source, reg::JMA_NOWCAST.region)
    }

    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::minutes(reg::NOWCAST_FOLLOW_MINUTES as i64),
            spare: 1,
            relist: Duration::minutes(1),
            give_up: Duration::minutes(20),
        }
    }

    fn leads(&self, _: &SourceDef, _: &str, _: DateTime<Utc>) -> Vec<u32> {
        let step = reg::NOWCAST_STEP_MINUTES;
        (1..=reg::NOWCAST_HORIZON_MINUTES / step)
            .map(|k| k * step)
            .collect()
    }

    fn due(&self, _: &SourceDef, _: &str, run: DateTime<Utc>, _: u32) -> DateTime<Utc> {
        // targetTimes_N2.json was rewritten at 20:31:49 for the 20:30 run on
        // 2026-11-06.
        run + Duration::seconds(120)
    }

    fn file(&self, _: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        lead_name(run, need.lead)
    }

    /// The current run as `targetTimes_N2.json` lists it; an older run in
    /// full, as its tiles stay, if `targetTimes_N1.json` has its analysis
    /// and it is no older than [`OLDEST_RUN`]; nothing yet of a newer one.
    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        run: DateTime<Utc>,
        _: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let mut listing = Listing::new();
        if run < cx.now - OLDEST_RUN {
            return Ok(Some(listing));
        }
        let url = times_url(&self.base, source.region);
        let targets = self.times.get(cx, &url).await?;
        let current = targets.iter().map(|t| t.base).max();
        let Some(current) = current.filter(|t| *t >= cx.now - STALE) else {
            return Err(FetchError::service(
                &url,
                format!("no run newer than {current:?}: the list has stopped"),
            ));
        };
        if run == current {
            for t in targets.iter().filter(|t| t.base == run && t.valid > run) {
                listing.add(format!("{}/{}", stamp(run), stamp(t.valid)), None);
            }
        } else if run < current {
            let analyses = self
                .analyses
                .get(cx, &times_url(&self.base, reg::JMA_RADAR.region))
                .await?;
            if !analyses.iter().any(|t| t.base == run && t.valid == run) {
                return Ok(Some(listing));
            }
            for lead in self.leads(source, reg::PRODUCT, run) {
                listing.add(lead_name(run, lead), None);
            }
        }
        Ok(Some(listing))
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        run: DateTime<Utc>,
        file: &ModelFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let Some(lead) = needs.first().map(|n| n.lead) else {
            return Ok(Fetched::Got(Vec::new()));
        };
        if needs.iter().any(|n| n.lead != lead) || file.name != lead_name(run, lead) {
            return Err(FetchError::object(
                &file.name,
                "needs of several leads in one tile set",
            ));
        }
        let set = tiles();
        let target = Target {
            base: run,
            valid: run + Duration::minutes(lead as i64),
        };
        let Some(fetch) = fetch_tiles(cx, &self.base, &set, target, Absent::Dry).await? else {
            return Ok(Fetched::Missing);
        };
        if fetch.absent < fetch.of {
            let mut runs = lock(&self.with_tiles);
            runs.retain(|r| *r >= cx.now - Duration::hours(2));
            runs.insert(run);
        } else if !lock(&self.with_tiles).contains(&run) {
            // No tile at all: dry only if the run is known to have tiles
            // (another lead had some) or its analysis is dry everywhere.
            let known = lock(&self.dry_analysis).get(&run).copied();
            let dry = match known {
                Some(d) => Some(d),
                None => analysis_is_dry(cx, &self.base, &set, run).await?,
            };
            match dry {
                None => {
                    let url = tile_url(
                        &self.base,
                        target,
                        set.z,
                        set.tiles()[0].1,
                        set.tiles()[0].2,
                    );
                    return Err(FetchError::service(
                        url,
                        "neither the forecast nor its analysis has tiles: have the paths changed?",
                    ));
                }
                Some(d) => {
                    let mut cache = lock(&self.dry_analysis);
                    cache.retain(|r, _| *r >= cx.now - Duration::hours(2));
                    cache.insert(run, d);
                    if !d {
                        tracing::warn!(%run, lead, "a JMA forecast lead without tiles from a run with rain; waiting for it");
                        return Ok(Fetched::Missing);
                    }
                }
            }
        }
        let observed = self.coverage.get(cx, &self.base, &set, run).await?;
        Ok(Fetched::all(
            needs,
            Payload::Custom {
                bytes: fetch.packed,
                decode: decoder(set, observed),
            },
        ))
    }
}

#[cfg(test)]
mod tests;
