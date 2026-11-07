//! The three station feeds (airport METARs, NDBC buoys, JMA AMeDAS) and
//! `GET /api/stations`. Each feed refreshes on its own and replaces only its
//! own stations, so one failing feed leaves the others untouched.
//!
//! Every station's GeoJSON feature is serialized once, when its feed
//! delivers it, along with a hash of those bytes. An answer is then a copy of
//! the chosen features and its `ETag` a hash of their hashes: a request that
//! ends in a 304 serializes nothing, and the validator still changes exactly
//! when the stations in the answer do, so a view that another feed's refresh
//! did not touch keeps getting 304s.

use crate::metar::{MAX_AGE, Observation, Source};
use crate::{DAY, Inner, amedas, error, header, metar, ndbc, round};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use flate2::{Compression, write::GzEncoder};
use serde::Serialize;
use serde_json::json;
use std::borrow::Borrow;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::sync::OnceCell;
use wx_core::{FeedCtx, WsEvent};

/// Coordinates are sent to about 100 m: enough for a dot, and smaller.
const COORD_DECIMALS: i32 = 3;

/// What a feed remembers between refreshes.
#[derive(Default)]
pub(crate) struct FeedMemory {
    etag: Option<String>,
    last_modified: Option<String>,
    newest: Option<DateTime<Utc>>,
    count: usize,
}

/// A lookup table that changes about never (METAR station names, the AMeDAS
/// station list), fetched at most daily.
#[derive(Default)]
pub(crate) struct Table<T> {
    value: Option<Arc<T>>,
    fetched: Option<Instant>,
}

/// The station list sits next to the METAR file in the same cache directory.
fn names_url(metar_url: &str) -> Option<String> {
    let file = "metars.cache.csv.gz";
    metar_url
        .contains(file)
        .then(|| metar_url.replace(file, "stations.cache.json.gz"))
}

/// Send `req` and parse the body off the async threads; `None` for a 304.
async fn fetch<T: Send + 'static>(
    req: reqwest::RequestBuilder,
    parse: impl FnOnce(&[u8]) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<Option<(T, HeaderMap)>> {
    let resp = req.send().await?;
    if resp.status() == StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    let resp = resp.error_for_status()?;
    let headers = resp.headers().clone();
    let bytes = resp.bytes().await?;
    let parsed = tokio::task::spawn_blocking(move || parse(&bytes)).await??;
    Ok(Some((parsed, headers)))
}

async fn get_parsed<T: Send + 'static>(
    inner: &Inner,
    url: &str,
    parse: impl FnOnce(&[u8]) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let parsed = fetch(inner.http.get(url), parse).await?.map(|(v, _)| v);
    parsed.ok_or_else(|| anyhow::anyhow!("{url} answered 304 to an unconditional request"))
}

/// The table, fetched when missing or a day old. A failed fetch keeps the
/// old table, and is retried at the next refresh only if there was none.
async fn refresh_table<T: Send + Sync + 'static>(
    inner: &Inner,
    table: &Mutex<Table<T>>,
    url: &str,
    parse: fn(&[u8]) -> anyhow::Result<T>,
) -> anyhow::Result<Arc<T>> {
    let (old, due) = {
        let t = table.lock().unwrap();
        (t.value.clone(), t.fetched.is_none_or(|f| f.elapsed() > DAY))
    };
    if let Some(v) = old.clone().filter(|_| !due) {
        return Ok(v);
    }
    let fetched = get_parsed(inner, url, parse).await.map(Arc::new);
    let mut t = table.lock().unwrap();
    t.fetched = Some(Instant::now());
    match fetched {
        Ok(v) => Ok(t.value.insert(v).clone()),
        Err(e) => {
            tracing::warn!("{url} unavailable: {e:#}");
            old.ok_or(e)
        }
    }
}

/// A conditional GET of a feed's file, parsed off the async threads; `None`
/// when it has not changed.
async fn fetch_file(
    inner: &Inner,
    url: &str,
    memory: &Mutex<FeedMemory>,
    parse: impl FnOnce(&[u8], DateTime<Utc>) -> anyhow::Result<Vec<Observation>> + Send + 'static,
) -> anyhow::Result<Option<Vec<Observation>>> {
    let mut req = inner.http.get(url);
    {
        let m = memory.lock().unwrap();
        // A feed that has yielded nothing yet downloads in full.
        let validators = [
            (header::IF_NONE_MATCH, &m.etag),
            (header::IF_MODIFIED_SINCE, &m.last_modified),
        ];
        for (name, value) in validators.into_iter().filter(|_| m.count > 0) {
            if let Some(v) = value {
                req = req.header(name, v);
            }
        }
    }
    let Some((stations, h)) = fetch(req, move |b| parse(b, Utc::now())).await? else {
        return Ok(None);
    };
    let (etag, last_modified) = (header(&h, header::ETAG), header(&h, header::LAST_MODIFIED));
    let (newest, count) = (stations.iter().map(|o| o.observed).max(), stations.len());
    *memory.lock().unwrap() = FeedMemory {
        etag,
        last_modified,
        newest,
        count,
    };
    Ok(Some(stations))
}

/// JMA AMeDAS: the snapshot changes every 10 minutes, so the small
/// `latest_time.txt` is polled and the 250 KB map fetched only when it moves.
async fn fetch_amedas(
    inner: &Inner,
    base: &str,
    memory: &Mutex<FeedMemory>,
) -> anyhow::Result<Option<Vec<Observation>>> {
    let base = base.trim_end_matches('/');
    let latest = |b: &[u8]| amedas::parse_latest_time(&String::from_utf8_lossy(b));
    let at = get_parsed(inner, &format!("{base}/data/latest_time.txt"), latest).await?;
    let observed = at.with_timezone(&Utc);
    if memory.lock().unwrap().newest == Some(observed) {
        return Ok(None);
    }
    let url = format!("{base}/const/amedastable.json");
    let table = refresh_table(inner, &inner.amedas_stations, &url, amedas::parse_table).await?;
    let parse = move |b: &[u8]| amedas::parse_snapshot(b, &table, observed, Utc::now());
    let url = format!("{base}/data/map/{}", amedas::snapshot_name(at));
    let stations = get_parsed(inner, &url, parse).await?;
    let (newest, count) = (Some(observed), stations.len());
    *memory.lock().unwrap() = FeedMemory {
        newest,
        count,
        ..Default::default()
    };
    Ok(Some(stations))
}

/// One refresh of `source`'s feed from `url` (for AMeDAS the root of its files).
pub(crate) async fn refresh(
    inner: &Arc<Inner>,
    ctx: &FeedCtx,
    source: Source,
    url: &str,
) -> anyhow::Result<()> {
    let memory = &inner.feeds[source as usize];
    let fresh = match source {
        Source::Metar => {
            // Names are a nicety: without them the airports are still shown.
            let names = match names_url(url) {
                Some(u) => refresh_table(inner, &inner.metar_names, &u, metar::parse_station_names)
                    .await
                    .ok(),
                None => None,
            };
            let parse = move |bytes: &[u8], now| {
                let mut v = metar::parse_metars(bytes, now)?;
                for o in v.iter_mut() {
                    o.name = names.as_ref().and_then(|n| n.get(&o.id).cloned());
                }
                Ok(v)
            };
            fetch_file(inner, url, memory, parse).await?
        }
        Source::Ndbc => fetch_file(inner, url, memory, ndbc::parse_ndbc).await?,
        Source::Amedas => fetch_amedas(inner, url, memory).await?,
    };
    let FeedMemory { newest, count, .. } = *memory.lock().unwrap();
    let Some(stations) = fresh else {
        announce_expiry(inner);
        ctx.ok(newest, format!("{count} stations (unchanged)"));
        return Ok(());
    };
    // Serializing thousands of features is work for the blocking pool too.
    let store = inner.clone();
    announce(
        inner,
        tokio::task::spawn_blocking(move || store.stations.set(source, stations)).await?,
    );
    ctx.ok(newest, format!("{count} stations"));
    Ok(())
}

/// Tell clients the station set changed, and how many are served now.
fn announce(inner: &Inner, count: usize) {
    inner.hub.send(WsEvent::Stations {
        updated: Utc::now(),
        count,
    });
}

/// A refresh that brought nothing new still lets old reports expire; clients
/// are told when that changes what is served.
fn announce_expiry(inner: &Inner) {
    if let Some(count) = inner.stations.expire() {
        announce(inner, count);
    }
}

#[derive(Serialize)]
struct Geometry {
    r#type: &'static str,
    coordinates: [f64; 2],
}

#[derive(Serialize)]
struct Feature<'a> {
    r#type: &'static str,
    id: &'a str,
    geometry: Geometry,
    properties: &'a Observation,
}

/// One station as served: its report and its feature, ready to copy.
pub(crate) struct Station {
    obs: Observation,
    json: Vec<u8>,
    /// The feature with `raw`, for the stations that have it.
    with_raw: Option<Vec<u8>>,
    /// Of the feature and `raw`, so a validator built from it covers every byte served.
    hash: u64,
}

impl Station {
    fn new(obs: Observation) -> Self {
        let at = |v| round(v, COORD_DECIMALS);
        let geometry = Geometry {
            r#type: "Point",
            coordinates: [at(obs.lon), at(obs.lat)],
        };
        let feature = Feature {
            r#type: "Feature",
            id: &obs.id,
            geometry,
            properties: &obs,
        };
        let json = serde_json::to_vec(&feature).expect("a feature serializes");
        // `properties` comes last, so the feature ends `}}`: `raw` goes in before.
        let with_raw = obs.raw.as_ref().map(|raw| {
            let tail = format!(r#","raw":{}}}}}"#, json!(raw));
            [&json[..json.len() - 2], tail.as_bytes()].concat()
        });
        let mut h = DefaultHasher::new();
        (&json, &obs.raw).hash(&mut h);
        Self {
            hash: h.finish(),
            obs,
            json,
            with_raw,
        }
    }

    fn json(&self, raw: bool) -> &[u8] {
        self.with_raw
            .as_deref()
            .filter(|_| raw)
            .unwrap_or(&self.json)
    }
}

/// The `FeatureCollection` of `stations`; `sources` lists only the feeds present.
fn collection<S: Borrow<Station>>(stations: &[S], raw: bool) -> Bytes {
    let present: BTreeSet<Source> = stations.iter().map(|s| s.borrow().obs.source).collect();
    let info = |s: Source| {
        (
            s,
            json!({ "name": s.name(), "attribution": s.attribution() }),
        )
    };
    let sources: BTreeMap<_, _> = present.into_iter().map(info).collect();
    let len: usize = stations
        .iter()
        .map(|s| s.borrow().json(raw).len() + 1)
        .sum();
    let mut out = Vec::with_capacity(len + 512);
    write!(
        out,
        r#"{{"type":"FeatureCollection","sources":{},"features":["#,
        json!(sources)
    )
    .unwrap();
    for (i, s) in stations.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(s.borrow().json(raw));
    }
    out.extend_from_slice(b"]}");
    out.into()
}

/// The validator of `collection(stations, raw)`, without building it.
fn etag_of<S: Borrow<Station>>(stations: &[S], raw: bool) -> String {
    let mut h = DefaultHasher::new();
    raw.hash(&mut h);
    stations.iter().for_each(|s| s.borrow().hash.hash(&mut h));
    format!("\"{:016x}\"", h.finish())
}

/// The unfiltered answer, ready to send.
pub(crate) struct Snapshot {
    pub body: Bytes,
    pub etag: String,
    gzip: OnceCell<Bytes>,
}

impl Snapshot {
    /// `body` gzipped off the async threads by the first request that wants
    /// it; requests arriving meanwhile wait for the same result. `None` if
    /// that failed.
    async fn gzip(&self) -> Option<Bytes> {
        let body = self.body.clone();
        let compress = move || {
            let mut e = GzEncoder::new(Vec::new(), Compression::default());
            e.write_all(&body).expect("writing to memory");
            Bytes::from(e.finish().expect("writing to memory"))
        };
        let gzip = self
            .gzip
            .get_or_try_init(|| tokio::task::spawn_blocking(compress));
        gzip.await.ok().cloned()
    }
}

/// Every feed's stations, kept apart so a feed can replace just its own.
pub(crate) struct Store {
    feeds: Mutex<BTreeMap<Source, Vec<Arc<Station>>>>,
    merged: RwLock<Arc<Merged>>,
}

/// All stations by id, plus the unfiltered answer.
struct Merged {
    all: Vec<Arc<Station>>,
    snapshot: Snapshot,
    /// When the oldest report included passes [`MAX_AGE`]; after that the
    /// set is rebuilt without it.
    expires_at: Option<DateTime<Utc>>,
}

impl Merged {
    fn select(&self, bbox: Option<Bbox>) -> Vec<&Station> {
        let inside = |s: &&Station| bbox.is_none_or(|b| b.contains(s.obs.lon, s.obs.lat));
        self.all.iter().map(|s| s.as_ref()).filter(inside).collect()
    }
}

/// Reports older than [`MAX_AGE`] at `now` are not served, however long ago
/// their feed last delivered.
fn merge(feeds: &BTreeMap<Source, Vec<Arc<Station>>>, now: DateTime<Utc>) -> Merged {
    let mut all: Vec<_> = feeds
        .values()
        .flatten()
        .filter(|s| now - s.obs.observed <= MAX_AGE)
        .cloned()
        .collect();
    all.sort_by(|a, b| a.obs.id.cmp(&b.obs.id));
    let expires_at = all.iter().map(|s| s.obs.observed + MAX_AGE).min();
    let (body, etag, gzip) = (
        collection(&all, false),
        etag_of(&all, false),
        OnceCell::new(),
    );
    Merged {
        all,
        snapshot: Snapshot { body, etag, gzip },
        expires_at,
    }
}

impl Store {
    pub fn new() -> Self {
        let merged = RwLock::new(Arc::new(merge(&BTreeMap::new(), Utc::now())));
        Self {
            feeds: Mutex::default(),
            merged,
        }
    }

    /// Replace one feed's stations; returns the total served across feeds.
    pub fn set(&self, source: Source, stations: Vec<Observation>) -> usize {
        self.set_at(source, stations, Utc::now())
    }

    fn set_at(&self, source: Source, stations: Vec<Observation>, now: DateTime<Utc>) -> usize {
        let stations = stations
            .into_iter()
            .map(|o| Arc::new(Station::new(o)))
            .collect();
        let mut feeds = self.feeds.lock().unwrap();
        feeds.insert(source, stations);
        let merged = Arc::new(merge(&feeds, now));
        let total = merged.all.len();
        *self.merged.write().unwrap() = merged;
        total
    }

    fn current(&self) -> Arc<Merged> {
        self.current_at(Utc::now())
    }

    /// The served set at `now`, rebuilt if a report has aged out since it was built.
    fn current_at(&self, now: DateTime<Utc>) -> Arc<Merged> {
        let fresh = || {
            Some(self.merged.read().unwrap().clone())
                .filter(|m| m.expires_at.is_none_or(|e| now <= e))
        };
        if let Some(m) = fresh() {
            return m;
        }
        let feeds = self.feeds.lock().unwrap();
        // Requests that queued on the lock take the rebuild of the first.
        if let Some(m) = fresh() {
            return m;
        }
        let rebuilt = Arc::new(merge(&feeds, now));
        *self.merged.write().unwrap() = rebuilt.clone();
        rebuilt
    }

    /// Drop aged-out reports; the new total if the served set shrank.
    pub fn expire(&self) -> Option<usize> {
        let before = self.merged.read().unwrap().all.len();
        let after = self.current().all.len();
        (after != before).then_some(after)
    }
}

/// `west,south,east,north` in degrees. West greater than east means the box
/// crosses the antimeridian.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Bbox {
    west: f64,
    south: f64,
    east: f64,
    north: f64,
}

fn wrap_lon(lon: f64) -> f64 {
    (lon + 180.0).rem_euclid(360.0) - 180.0
}

impl Bbox {
    /// Longitudes outside -180..180 (a map panned past the date line) are
    /// wrapped; a box 360 degrees or more wide is the whole world.
    pub fn parse(text: &str) -> Option<Bbox> {
        let num = |p: &str| p.trim().parse().ok().filter(|x: &f64| x.is_finite());
        let v: Vec<f64> = text.split(',').map(num).collect::<Option<_>>()?;
        let [west, south, east, north] = v[..] else {
            return None;
        };
        if south > north || !(-90.0..=90.0).contains(&south) || !(-90.0..=90.0).contains(&north) {
            return None;
        }
        let world = east - west >= 360.0;
        let (west, east) = if world {
            (-180.0, 180.0)
        } else {
            (wrap_lon(west), wrap_lon(east))
        };
        Some(Bbox {
            west,
            south,
            east,
            north,
        })
    }

    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        if lat < self.south || lat > self.north {
            return false;
        }
        if self.west <= self.east {
            lon >= self.west && lon <= self.east
        } else {
            lon >= self.west || lon <= self.east
        }
    }
}

fn matches(if_none_match: &str, etag: &str) -> bool {
    let mut tags = if_none_match
        .split(',')
        .map(|t| t.trim().trim_start_matches("W/"));
    tags.any(|t| t == etag || t == "*")
}

/// Whether `Accept-Encoding` allows gzip.
fn accepts_gzip(accept_encoding: &str) -> bool {
    accept_encoding.split(',').any(|item| {
        let mut parts = item.split(';').map(str::trim);
        let coding = parts.next().unwrap_or("");
        let refused =
            parts.any(|p| p.strip_prefix("q=").and_then(|q| q.trim().parse().ok()) == Some(0.0));
        (coding.eq_ignore_ascii_case("gzip") || coding == "*") && !refused
    })
}

pub(crate) async fn handler(
    State(inner): State<Arc<Inner>>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let bbox = match q.get("bbox").map(|t| Bbox::parse(t)) {
        Some(None) => {
            let message = "bbox must be west,south,east,north in degrees";
            return error(StatusCode::BAD_REQUEST, "bad_request", message);
        }
        b => b.flatten(),
    };
    let raw = q.get("raw").is_some_and(|v| v == "1" || v == "true");
    announce_expiry(&inner);
    let merged = inner.stations.current();
    // A box that holds every station gets the unfiltered answer, precompressed.
    let picked = (bbox.is_some() || raw).then(|| merged.select(bbox));
    let picked = picked.filter(|v| raw || v.len() < merged.all.len());
    let etag = match &picked {
        Some(v) => etag_of(v, raw),
        None => merged.snapshot.etag.clone(),
    };
    let fresh = header(&headers, header::IF_NONE_MATCH).is_some_and(|v| matches(&v, &etag));
    let wants_gzip = header(&headers, header::ACCEPT_ENCODING).is_some_and(|v| accepts_gzip(&v));
    let common = [
        (header::CACHE_CONTROL, "no-cache"),
        (header::VARY, "Accept-Encoding"),
    ];
    let common = (common, [(header::ETAG, etag)]);
    if fresh {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    let gzip = match picked {
        None if wants_gzip => merged.snapshot.gzip().await,
        _ => None,
    };
    let encoding = gzip.as_ref().map(|_| [(header::CONTENT_ENCODING, "gzip")]);
    let body = match (gzip, picked) {
        (Some(gz), _) => gz,
        (None, Some(v)) => collection(&v, raw),
        (None, None) => merged.snapshot.body.clone(),
    };
    let geojson = [(header::CONTENT_TYPE, "application/geo+json")];
    (common, geojson, encoding, body).into_response()
}

#[cfg(test)]
mod bench;

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;

    const METARS: &[u8] = include_bytes!("../tests/fixtures/metars.csv");
    const NDBC: &[u8] = include_bytes!("../tests/fixtures/ndbc.txt");

    #[test]
    fn header_forms() {
        assert!(matches("\"a\"", "\"a\""));
        assert!(matches("W/\"a\", \"b\"", "\"a\""));
        assert!(matches("*", "\"a\""));
        assert!(!matches("\"b\"", "\"a\"") && !matches("", "\"a\""));
        for yes in ["gzip", "br, gzip;q=0.8", "*", "GZIP"] {
            assert!(accepts_gzip(yes), "{yes}");
        }
        for no in ["gzip;q=0", "br", "identity", ""] {
            assert!(!accepts_gzip(no), "{no}");
        }
    }

    #[test]
    fn names_url_is_derived() {
        let url = names_url("https://x/data/cache/metars.cache.csv.gz");
        assert_eq!(
            url.as_deref(),
            Some("https://x/data/cache/stations.cache.json.gz")
        );
        assert_eq!(names_url("http://127.0.0.1/m.csv"), None);
    }

    fn at(t: &str) -> DateTime<Utc> {
        t.parse().unwrap()
    }

    fn fixtures() -> Vec<Observation> {
        let airports = metar::parse_metars(METARS, at("2026-11-06T12:30:00Z")).unwrap();
        airports
            .into_iter()
            .chain(ndbc::parse_ndbc(NDBC, at("2026-11-06T18:00:00Z")).unwrap())
            .collect()
    }

    /// The fixtures, all observed at `t`, as airports and buoys.
    fn feeds_at(t: &str) -> (Vec<Observation>, Vec<Observation>) {
        let mut v = fixtures();
        v.iter_mut().for_each(|o| o.observed = at(t));
        v.into_iter().partition(|o| o.source == Source::Metar)
    }

    fn ids(obs: &[Observation], b: &str) -> Vec<String> {
        let b = Bbox::parse(b).unwrap();
        obs.iter()
            .filter(|o| b.contains(o.lon, o.lat))
            .map(|o| o.id.clone())
            .collect()
    }

    fn json(stations: &[Station], raw: bool) -> serde_json::Value {
        serde_json::from_slice(&collection(stations, raw)).unwrap()
    }

    #[test]
    fn bbox_parsing() {
        let sides = |t: &str| Bbox::parse(t).map(|b| (b.west, b.south, b.east, b.north));
        assert_eq!(
            sides("-100, 30, -90, 40"),
            Some((-100.0, 30.0, -90.0, 40.0))
        );
        for bad in [
            "",
            "1,2,3",
            "1,2,3,4,5",
            "a,b,c,d",
            "0,40,10,30",
            "0,-95,10,0",
            "0,0,10,91",
            "0,0,NaN,10",
        ] {
            assert!(Bbox::parse(bad).is_none(), "{bad}");
        }
        // A box a full turn wide or more is the world.
        let world = Bbox::parse("-200,-90,200,90").unwrap();
        assert!(world.contains(179.9, 0.0) && world.contains(-179.9, 0.0));
        // Longitudes past the date line are wrapped.
        assert_eq!(sides("170,0,190,10"), Some((170.0, 0.0, -170.0, 10.0)));
        assert_eq!(sides("-190,0,-170,10"), Some((170.0, 0.0, -170.0, 10.0)));
    }

    #[test]
    fn bbox_filters_by_position() {
        let obs = fixtures();
        let oklahoma = ids(&obs, "-100,34,-95,37");
        assert!(oklahoma.contains(&"KOUN".into()));
        assert!(!oklahoma.contains(&"EGLL".into()) && !oklahoma.contains(&"ndbc:41013".into()));
        // West of the antimeridian only.
        let europe = ids(&obs, "-10,48,5,60");
        assert!(europe.contains(&"EGLL".into()) && europe.contains(&"ndbc:62107".into()));
        assert!(!europe.contains(&"KOUN".into()));
        // Edges are inside.
        let point = Bbox::parse("-97.4713,35.2436,-97.4713,35.2436").unwrap();
        assert!(point.contains(-97.4713, 35.2436));
    }

    #[test]
    fn bbox_across_the_antimeridian() {
        let obs = fixtures();
        // Bering Sea buoy 46035 sits at 177.468 W; RJTT (139.8 E) and the
        // Korean buoy (126 E) are west of the line, KOUN far east of it.
        let b = Bbox::parse("170,50,-170,65").unwrap();
        assert!(b.west > b.east);
        let hit = ids(&obs, "170,50,-170,65");
        assert_eq!(hit, ["ndbc:46035"]);
        assert!(b.contains(175.0, 60.0) && b.contains(-175.0, 60.0));
        assert!(!b.contains(0.0, 60.0) && !b.contains(175.0, 70.0));
        // The same view expressed with longitudes beyond 180 selects the same stations.
        assert_eq!(ids(&obs, "170,50,190,65"), hit);
        assert_eq!(ids(&obs, "-190,50,-170,65"), hit);
        // A wide box that wraps selects both sides of the line.
        let wide = ids(&obs, "120,30,-120,70");
        for id in ["ndbc:46035", "ndbc:22101", "RJTT"] {
            assert!(wide.contains(&id.into()), "{id}");
        }
        assert!(!wide.contains(&"KOUN".into()));
    }

    #[test]
    fn geojson_is_compact_and_lists_only_used_sources() {
        let stations: Vec<Station> = fixtures().into_iter().map(Station::new).collect();
        let all = json(&stations, false);
        assert_eq!(all["type"], "FeatureCollection");
        assert_eq!(all["features"].as_array().unwrap().len(), stations.len());
        assert_eq!(all["sources"].as_object().unwrap().len(), 2);
        let credit = all["sources"]["ndbc"]["attribution"].as_str().unwrap();
        assert!(credit.contains("Data Buoy Center"));
        let koun = |v: &serde_json::Value| {
            v["features"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["id"] == "KOUN")
                .cloned()
        };
        let feature = koun(&all).unwrap();
        let p = feature["properties"].clone();
        assert_eq!([&p["kind"], &p["source"]], ["airport", "metar"]);
        assert!(p.get("raw").is_none(), "raw only on request");
        assert!(
            p.get("wind_gust_ms").is_none(),
            "unknown values are left out, not null"
        );
        assert!(p.get("lat").is_none() && p.get("lon").is_none());
        assert_eq!(feature["geometry"]["coordinates"], json!([-97.471, 35.244]));
        let mut with_raw = koun(&json(&stations, true)).unwrap()["properties"].clone();
        let raw = with_raw.as_object_mut().unwrap().remove("raw").unwrap();
        assert!(raw.as_str().unwrap().starts_with("METAR KOUN"));
        assert_eq!(with_raw, p, "raw=1 adds raw and nothing else");
        let buoys: Vec<&Station> = stations
            .iter()
            .filter(|s| s.obs.source == Source::Ndbc)
            .collect();
        let only_buoy: serde_json::Value =
            serde_json::from_slice(&collection(&buoys, false)).unwrap();
        let keys: Vec<_> = only_buoy["sources"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ndbc"]);
        assert_eq!(json(&[], false)["features"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_feed_replaces_only_its_own_stations() {
        let store = Store::new();
        let (metar, buoys) = feeds_at(&Utc::now().to_rfc3339());
        let (m, b) = (metar.len(), buoys.len());
        assert_eq!(store.set(Source::Metar, metar), m);
        assert_eq!(store.set(Source::Ndbc, buoys), m + b);
        assert_eq!(
            store.set(Source::Ndbc, Vec::new()),
            m,
            "an empty buoy feed leaves airports"
        );
        assert_eq!(store.current().all.len(), m);
    }

    /// The cached answer is the collection of what is served, its validator
    /// and gzip made from it.
    async fn check(m: &Merged) {
        let mut plain = Vec::new();
        GzDecoder::new(&m.snapshot.gzip().await.unwrap()[..])
            .read_to_end(&mut plain)
            .unwrap();
        assert_eq!(plain, m.snapshot.body);
        assert_eq!(m.snapshot.body, collection(&m.select(None), false));
        assert_eq!(m.snapshot.etag, etag_of(&m.all, false));
    }

    #[tokio::test]
    async fn reports_age_out_between_refreshes_and_dead_feeds_empty_out() {
        let store = Store::new();
        let ((metar, _), (_, buoys)) = (
            feeds_at("2026-11-06T11:50:00Z"),
            feeds_at("2026-11-06T17:50:00Z"),
        );
        let (m, b) = (metar.len(), buoys.len());
        assert_eq!(
            store.set_at(Source::Metar, metar, at("2026-11-06T12:00:00Z")),
            m
        );
        let total = store.set_at(Source::Ndbc, buoys, at("2026-11-06T18:00:00Z"));
        // The airports were already over the limit when the buoys arrived.
        assert_eq!(total, b);
        // Buoys are served until three hours after their report, then vanish
        // with no further refresh, and the served body changes with them.
        let before = store.current_at(at("2026-11-06T20:50:00Z"));
        assert_eq!(before.all.len(), b);
        let after = store.current_at(at("2026-11-06T20:50:01Z"));
        assert_eq!(after.all.len(), 0);
        assert_ne!(before.snapshot.etag, after.snapshot.etag);
        check(&before).await;
        check(&after).await;
        // A feed that keeps delivering is unaffected by one that stopped.
        let (fresh, _) = feeds_at("2026-11-06T20:45:00Z");
        let n = fresh.len();
        assert_eq!(
            store.set_at(Source::Metar, fresh, at("2026-11-06T20:55:00Z")),
            n
        );
        assert_eq!(store.current_at(at("2026-11-06T21:00:00Z")).all.len(), n);
    }

    #[tokio::test]
    async fn validators_change_exactly_with_what_is_served() {
        let store = Store::new();
        let noon = at("2026-11-06T12:00:00Z");
        let (metar, buoys) = feeds_at("2026-11-06T12:00:00Z");
        let views = [
            Bbox::parse("-100,34,-95,37"),
            Bbox::parse("-10,48,5,60"),
            None,
        ]; // Oklahoma, Europe, all
        let etags = |raw| views.map(|b| etag_of(&store.current_at(noon).select(b), raw));
        store.set_at(Source::Metar, metar.clone(), noon);
        let [ok, eu, all] = etags(false);
        let [ok_raw, ..] = etags(true);
        assert_eq!(all, store.current_at(noon).snapshot.etag);
        assert!(
            ok != eu && ok_raw != ok,
            "every distinct answer has its own"
        );
        check(&store.current_at(noon)).await;
        // Buoys arrive: Europe has one, Oklahoma none.
        store.set_at(Source::Ndbc, buoys, noon);
        let [ok2, eu2, all2] = etags(false);
        assert!(ok2 == ok && eu2 != eu && all2 != all);
        check(&store.current_at(noon)).await;
        // A change to nothing but an airport's raw text moves the raw validator.
        let mut v = metar;
        let koun = v.iter_mut().find(|o| o.id == "KOUN").unwrap();
        koun.raw = Some(format!("{} COR", koun.raw.as_deref().unwrap()));
        store.set_at(Source::Metar, v, noon);
        assert!(etags(true)[0] != ok_raw && etags(false)[2] != all2);
        check(&store.current_at(noon)).await;
    }
}
