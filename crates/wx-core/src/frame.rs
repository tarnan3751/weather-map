//! Parts, frames, and the catalog that assembles each layer's timeline.
//!
//! A **part** is one source's data for one layer at one valid time: one
//! stored grid (`keys::grid`). A **frame** is what the map shows at one time
//! step: an ordered stack of parts, best on top. Tiles are rendered from a
//! stack of parts, never from a frame as such, so two frames that share the
//! parts covering some tile share that tile.
//!
//! # How a timeline is assembled
//!
//! All of this is per layer. `hold` and `fill_past` come from the layer's
//! [`InputSpec`](crate::layer::InputSpec) for the part's source; `rank` from
//! the source registry. `now` is the latest moment given to
//! [`FrameCatalog::insert`] or [`FrameCatalog::sweep`], or the caller's own
//! when that is later: a caller with a slower clock cannot move time back,
//! and [`FrameCatalog::wants`] answers exactly as `insert` would.
//!
//! 1. **One part per source and valid time.** A part from a newer model run
//!    replaces the part the same source had for that valid time. A part from
//!    an older run is refused, and so is an observed part valid more than
//!    [`MAX_OBSERVED_AHEAD_MINUTES`] after `now`.
//! 2. **Ticks.** An observed part's tick is its valid time rounded *up* to a
//!    multiple of the layer's `observed_tick_minutes`. A forecast part's tick
//!    is its valid time. `latest_observed` is the newest tick of any observed
//!    part (none for a layer without observations).
//! 3. **Who takes part.** An observed part takes part at ticks up to and
//!    including `latest_observed`. A forecast part takes part at ticks after
//!    it; at ticks up to and including it only when its input says
//!    `fill_past`. So every frame after `latest_observed` holds forecast
//!    parts only. `latest_observed` is one moment for the whole layer: where
//!    a region's observations stop early, a forecast that does not fill the
//!    past stays off that region's ticks up to it all the same.
//! 4. **The time steps** are the ticks of the parts taking part there,
//!    except ticks older than `now - window` (`window` as in rule 7) that are
//!    at or before `latest_observed`, or on a layer with any `fill_past`
//!    input, any tick older than that: parts kept only for their hold make no
//!    frames there. A layer with neither observations nor `fill_past` inputs
//!    keeps past ticks for the forecast grace of rule 7. Without a `now` (a
//!    restore before any insert or sweep) older frames may show.
//! 5. **The frame at tick `t`** holds, for each source, that source's part
//!    with the greatest valid time `v <= t` among those taking part at `t`,
//!    provided `t - v < hold`. Parts are ordered by rank, highest first (ties
//!    by source id). A tick whose frame would be empty is not a time step.
//! 6. **Kind.** A frame is `Observed` when `t <= latest_observed`, otherwise
//!    `Forecast`.
//! 7. **Expiry** ([`FrameCatalog::sweep`]). With `window` the layer's
//!    `retention_minutes`: an observed part goes when `valid + hold <= now -
//!    window`, and so does a `fill_past` forecast part (it can no longer
//!    appear in any time step inside the window); any other forecast part
//!    goes when `valid < now - FORECAST_GRACE_MINUTES`, or when `valid + hold
//!    <= latest_observed` (it can no longer appear in any frame).
//!
//! ## Anchored layers
//!
//! On a layer with `anchored: true` (accumulations) every part is a forecast
//! whose `run` is the moment its accumulation starts. Each run is its own
//! accumulation, so rule 1 keeps one part per source, run and valid time.
//! Only runs starting on a multiple of [`ANCHOR_CYCLE_HOURS`] are taken. Only
//! parts whose run equals the layer's **anchor** are on the timeline; rules 2
//! to 6 then apply to those.
//!
//! Only sources whose feeds run count here (see
//! [`FrameCatalog::set_enabled_sources`]; by default all do): a disabled
//! source never acts as base, is never waited for and never holds the anchor
//! back, though parts it left behind restore, show and expire as any others.
//! The *registry base* is the lowest-ranked enabled source with an input on
//! the layer. It is the *base* as long as its newest run there is no more
//! than `2 * ANCHOR_CYCLE_HOURS` older than the newest run any enabled source
//! holds there. While it holds nothing, or has fallen that far behind, the
//! lowest-ranked enabled source that meets the condition stands in.
//!
//! With no anchor yet, the newest run that has [`MIN_ANCHOR_PARTS`] base
//! parts becomes it. While the registry base has produced nothing, a
//! stand-in may set this first anchor only once [`ANCHOR_PATIENCE_MINUTES`]
//! have passed since the layer first held a part (or since a restore); until
//! then parts wait off the timeline, so a run of the registry base that
//! arrives meanwhile sets it. From then on the anchor
//! moves to the newest run `r` for which either
//!
//! * the base source's parts for `r` include every valid time after `r` at
//!   which it has a part for the anchor, there is at least one such time,
//!   and every source with parts for the anchor has at least one part for
//!   `r` (finer models publish later than the base, and must not drop out);
//!   or
//! * [`ANCHOR_PATIENCE_MINUTES`] have passed since the base source's first
//!   part for `r` arrived, and it has at least [`MIN_ANCHOR_PARTS`] of them.
//!
//! The anchor never moves back and never goes away. While the base source has
//! too little for the anchor to compare against (a stand-in set it, or its
//! parts expired), only patience moves it, and the timeline shows whatever
//! parts there are for it. Parts from runs older than the anchor are dropped
//! when it moves, and refused; parts from newer runs are kept, off the
//! timeline, until it reaches them.
//!
//! # Which parts a tile needs
//!
//! [`tile_parts`] is the rule, and the web client implements the same one: of
//! a frame's parts, top first, take each whose `bounds` intersect the tile,
//! and stop after the first whose `solid` box contains the tile, because it
//! hides everything below. The tile is requested by naming those parts, so
//! the answer never changes and can be cached for good by anyone.
//!
//! Client and server do this arithmetic separately, so the server accepts any
//! answer between [`tile_parts_must`] and [`tile_parts_may`], which differ
//! only for boxes within [`COMBO_EPSILON_DEG`] of a tile edge.

use crate::events::{Hub, WsEvent};
use crate::layer::{FrameKind, LAYERS, LayerSpec};
use crate::proj::{LonLatBox, TileCoord};
use crate::source::{SOURCES, SourceDef};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Forecast parts that do not fill the past stay this long after their valid
/// time when no observation has overtaken them.
pub const FORECAST_GRACE_MINUTES: i64 = 90;

/// How long a frame's parts stay servable after the frame changes or leaves
/// its timeline, so a browser that was animating through it does not hit
/// missing tiles. Whoever deletes a dropped part's objects must wait longer.
pub const SERVE_GRACE_SECONDS: i64 = 240;

/// See "Anchored layers" in the module docs.
pub const MIN_ANCHOR_PARTS: usize = 6;
/// See "Anchored layers" in the module docs. Longer than the finer models
/// take to publish after the base, so it only rescues a run with leads or
/// models missing.
pub const ANCHOR_PATIENCE_MINUTES: i64 = 180;
/// Anchored layers take only runs starting on a multiple of this many hours
/// (UTC): models' long runs are at 00 and 12 UTC, and their short runs never
/// reach far enough to anchor on.
pub const ANCHOR_CYCLE_HOURS: i64 = 12;

/// An observation valid more than this far after `now` is refused: it comes
/// from a bad clock or file, and would otherwise end every forecast before it.
pub const MAX_OBSERVED_AHEAD_MINUTES: i64 = 5;

/// Tolerance, in degrees, for disagreements between client and server about
/// whether a part's box touches a tile.
pub const COMBO_EPSILON_DEG: f64 = 1e-6;

/// Most parts one tile request may name.
pub const MAX_COMBO_PARTS: usize = 48;

/// One source's data for one layer at one valid time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Part {
    /// Unique within the layer and safe in URLs and object keys:
    /// `[a-z0-9_-]+`, at most [`Part::MAX_ID_LEN`] bytes. See
    /// [`Part::observed`] / [`Part::forecast`].
    pub id: String,
    /// A `SourceDef::id`.
    pub source: String,
    /// The moment the data describes, on a whole minute.
    pub valid: DateTime<Utc>,
    /// Model run (reference) time. `None` for observations.
    pub run: Option<DateTime<Utc>>,
    pub kind: FrameKind,
    /// Lon/lat envelope of the grid, edges rounded outward to 0.01 degree.
    /// May cross the antimeridian (see [`LonLatBox`]).
    pub bounds: LonLatBox,
    /// A box everywhere inside which this part has data at full weight, so
    /// nothing beneath it shows. `None` when there is no such box worth
    /// having (patchy radar coverage).
    pub solid: Option<LonLatBox>,
    /// Deepest zoom tiles are rendered for from this part.
    pub max_zoom: u8,
}

impl Part {
    pub const MAX_ID_LEN: usize = 40;

    /// `mrms-2611061142`: source, then the valid time as `yymmddHHMM`.
    ///
    /// # Panics
    ///
    /// If `valid` is not on a whole minute.
    pub fn observed(
        source: &str,
        valid: DateTime<Utc>,
        bounds: LonLatBox,
        solid: Option<LonLatBox>,
        max_zoom: u8,
    ) -> Self {
        assert!(
            valid.timestamp() % 60 == 0 && valid.timestamp_subsec_nanos() == 0,
            "part valid times are whole minutes"
        );
        Self {
            id: format!("{source}-{}", valid.format("%y%m%d%H%M")),
            source: source.to_string(),
            valid,
            run: None,
            kind: FrameKind::Observed,
            bounds,
            solid,
            max_zoom,
        }
    }

    /// `hrrr-26110611-360`: source, the run as `yymmddHH`, then minutes into
    /// the run. A run that does not start on the hour (a nowcast) is written
    /// `yymmddHHMM`: `nowcast-2611061105-30`. The two forms cannot collide,
    /// as their middle fields differ in length.
    ///
    /// # Panics
    ///
    /// If `run` is not on a whole minute.
    pub fn forecast(
        source: &str,
        run: DateTime<Utc>,
        lead_minutes: u32,
        bounds: LonLatBox,
        solid: Option<LonLatBox>,
        max_zoom: u8,
    ) -> Self {
        assert!(
            run.timestamp().rem_euclid(60) == 0 && run.timestamp_subsec_nanos() == 0,
            "model runs start on a whole minute"
        );
        Self {
            id: Self::forecast_id(source, run, lead_minutes),
            source: source.to_string(),
            valid: run + chrono::Duration::minutes(lead_minutes as i64),
            run: Some(run),
            kind: FrameKind::Forecast,
            bounds,
            solid,
            max_zoom,
        }
    }

    /// The id [`Part::forecast`] gives, for callers that need it before the
    /// grid (and so the bounds) exists.
    pub fn forecast_id(source: &str, run: DateTime<Utc>, lead_minutes: u32) -> String {
        let run = if run.timestamp().rem_euclid(3600) == 0 {
            run.format("%y%m%d%H")
        } else {
            run.format("%y%m%d%H%M")
        };
        format!("{source}-{run}-{lead_minutes}")
    }

    /// The id [`Part::observed`] gives.
    pub fn observed_id(source: &str, valid: DateTime<Utc>) -> String {
        format!("{source}-{}", valid.format("%y%m%d%H%M"))
    }

    pub fn is_valid_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= Self::MAX_ID_LEN
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    }
}

/// Round a grid's envelope outward, and its solid box inward, to the 0.01
/// degree that [`Part::bounds`] and [`Part::solid`] are published at. Keeps
/// boxes that cross the antimeridian crossing it.
pub fn publish_boxes(
    bounds: LonLatBox,
    solid: Option<LonLatBox>,
) -> (LonLatBox, Option<LonLatBox>) {
    let out = |v: f64, lo: f64, hi: f64| ((v * 100.0).floor() / 100.0).clamp(lo, hi);
    let up = |v: f64, lo: f64, hi: f64| ((v * 100.0).ceil() / 100.0).clamp(lo, hi);
    let mut b = LonLatBox::new(
        out(bounds.west, -180.0, 180.0),
        out(bounds.south, -90.0, 90.0),
        up(bounds.east, -180.0, 180.0),
        up(bounds.north, -90.0, 90.0),
    );
    // Rounding outward can close the gap of a box that wraps almost all the
    // way round, which would otherwise read as a sliver.
    if bounds.wraps() && !b.wraps() {
        (b.west, b.east) = (-180.0, 180.0);
    }
    let s = solid.and_then(|s| {
        let s = LonLatBox::new(
            up(s.west, -180.0, 180.0),
            up(s.south, -90.0, 90.0),
            out(s.east, -180.0, 180.0),
            out(s.north, -90.0, 90.0),
        );
        // Rounding inward must not turn a sliver inside out, which for
        // longitudes would read as a box wrapping the other way round.
        let inverted = !solid.is_some_and(|o| o.wraps()) && s.west >= s.east;
        (s.south < s.north && !inverted).then_some(s)
    });
    (b, s)
}

/// One time step of one layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// The time step. Unique within the layer; a frame is identified by it.
    pub valid: DateTime<Utc>,
    pub kind: FrameKind,
    /// Part ids, top first.
    pub parts: Vec<String>,
}

/// A layer's timeline together with the parts its frames name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Timeline {
    /// Oldest first.
    pub frames: Vec<Frame>,
    /// Every part named by `frames`, in no particular order.
    pub parts: Vec<Part>,
}

/// The parts of a frame (given top first) that tile `coord` is rendered
/// from: each part whose bounds intersect the tile, stopping after the first
/// whose solid box contains it.
pub fn tile_parts(parts: &[Part], coord: TileCoord) -> Vec<&Part> {
    let tile = coord.bounds();
    let mut out = Vec::new();
    for p in parts {
        if !p.bounds.intersects(&tile) {
            continue;
        }
        out.push(p);
        if p.solid.is_some_and(|s| s.contains_box(&tile)) {
            break;
        }
    }
    out
}

/// The parts a correct answer for this tile must name: [`tile_parts`] with
/// every doubt resolved towards leaving a part out.
pub fn tile_parts_must(parts: &[Part], coord: TileCoord) -> Vec<&Part> {
    let tile = coord.bounds();
    let Some(inner) = tile.grown(-COMBO_EPSILON_DEG) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for p in parts {
        if p.bounds.intersects(&inner) {
            out.push(p);
        }
        // Hidden for certain only when the solid box clears the tile with
        // room to spare; but a part that merely might hide the rest ends the
        // list of parts that are needed for certain.
        if p.bounds
            .intersects(&tile.grown(COMBO_EPSILON_DEG).unwrap_or(tile))
            && p.solid.is_some_and(|s| s.contains_box(&inner))
        {
            break;
        }
    }
    out
}

/// The parts a correct answer for this tile may name: [`tile_parts`] with
/// every doubt resolved towards keeping a part in.
pub fn tile_parts_may(parts: &[Part], coord: TileCoord) -> Vec<&Part> {
    let tile = coord.bounds();
    let outer = tile.grown(COMBO_EPSILON_DEG).unwrap_or(tile);
    let mut out = Vec::new();
    for p in parts {
        if !p.bounds.intersects(&outer) {
            continue;
        }
        out.push(p);
        if p.solid.is_some_and(|s| s.contains_box(&outer)) {
            break;
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum Insert {
    /// The part was taken. `replaced` is the part of the same source and
    /// valid time, from an older run, that it displaced.
    Added { replaced: Option<Part> },
    /// The source already has a part for this valid time from the same or a
    /// newer run, the part has already expired, it is an observation dated
    /// too far ahead, it is from a run older than the anchor or off the
    /// anchor cycle, or its id is already taken; nothing changed.
    Refused,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CatalogError {
    #[error("unknown layer {0:?}")]
    UnknownLayer(String),
    #[error("layer {layer:?} takes nothing from source {source_id:?}")]
    NoSuchInput { layer: String, source_id: String },
}

/// The in-memory index of every part and the timelines built from them.
///
/// Feeds add parts, the tile server asks which parts a request may name, and
/// the catalog announces every change to a timeline on the [`Hub`] so open
/// browsers follow along. See the module docs for the rules.
pub struct FrameCatalog {
    hub: Hub,
    inner: std::sync::RwLock<catalog::State>,
}

impl FrameCatalog {
    /// A catalog over the built-in registries, [`LAYERS`] and [`SOURCES`].
    pub fn new(hub: Hub) -> Self {
        Self::with_registry(hub, LAYERS, SOURCES)
    }

    /// A catalog over other layer and source registries (tests use small
    /// fixed ones). Every layer and source the catalog knows comes from these.
    pub fn with_registry(
        hub: Hub,
        layers: &'static [LayerSpec],
        sources: &'static [SourceDef],
    ) -> Self {
        Self {
            hub,
            inner: std::sync::RwLock::new(catalog::State::with_registry(layers, sources)),
        }
    }

    /// Say which sources' feeds run; by default all do. Call it once at
    /// startup, before [`FrameCatalog::restore`]. Anchored layers choose
    /// their base among these only, and wait for no other (see "Anchored
    /// layers" in the module docs).
    pub fn set_enabled_sources(&self, ids: &[&str]) {
        self.inner.write().unwrap().set_enabled_sources(ids);
    }

    /// Load parts remembered from a previous run, then rebuild every
    /// timeline, without announcing anything. Parts for unknown layers or
    /// sources, parts off the anchor cycle and the losers of rule 1 are
    /// ignored, and none of them is reported as dropped. With no `now`,
    /// observations dated ahead are not checked, and frames older than the
    /// window (rule 4) may show until the first insert or sweep. An anchored
    /// layer's anchor starts at the oldest run with [`MIN_ANCHOR_PARTS`] base
    /// parts (the anchor the parts were saved under) and moves on by reach,
    /// but a stand-in cannot set it here; patience, and a stand-in's wait,
    /// are counted from the first insert or sweep after the restore.
    pub fn restore(&self, parts: impl IntoIterator<Item = (String, Part)>) {
        self.inner.write().unwrap().restore(parts);
    }

    /// Would a part like this be taken right now? Feeds call this before
    /// downloading anything, so data that is already held, already expired
    /// or (on an anchored layer) older than the anchor costs nothing.
    pub fn wants(
        &self,
        layer: &str,
        source: &str,
        valid: DateTime<Utc>,
        run: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        self.inner
            .read()
            .unwrap()
            .wants(layer, source, valid, run, now)
    }

    /// Add a finished part (its grid is already stored), rebuild the frames
    /// it touches and announce the change as one [`WsEvent::Timeline`].
    pub fn insert(
        &self,
        layer: &str,
        part: Part,
        now: DateTime<Utc>,
    ) -> Result<Insert, CatalogError> {
        let (outcome, event) = self.inner.write().unwrap().insert(layer, part, now)?;
        if let Some(event) = event {
            self.hub.send(event);
        }
        Ok(outcome)
    }

    /// A layer's timeline and the parts it names: a copy of
    /// [`FrameCatalog::shared_timeline`].
    pub fn timeline(&self, layer: &str) -> Timeline {
        Timeline::clone(&self.shared_timeline(layer))
    }

    /// A layer's timeline and the parts it names, shared with every other
    /// reader until the timeline next changes (empty for an unknown layer).
    /// Cheaper than [`FrameCatalog::timeline`], which copies every part.
    pub fn shared_timeline(&self, layer: &str) -> Arc<Timeline> {
        self.inner.read().unwrap().timeline(layer)
    }

    /// The parts of the frame at time step `valid` on a layer's timeline as
    /// it is now, top first. `None` when there is no such frame.
    pub fn frame_parts(&self, layer: &str, valid: DateTime<Utc>) -> Option<Vec<Part>> {
        self.inner.read().unwrap().frame_parts(layer, valid)
    }

    /// Every layer's timeline, keyed by layer id: copies of
    /// [`FrameCatalog::shared_timelines`].
    pub fn timelines(&self) -> BTreeMap<&'static str, Timeline> {
        self.shared_timelines()
            .into_iter()
            .map(|(id, t)| (id, Timeline::clone(&t)))
            .collect()
    }

    /// Every layer's timeline, keyed by layer id, shared as
    /// [`FrameCatalog::shared_timeline`] shares one.
    pub fn shared_timelines(&self) -> BTreeMap<&'static str, Arc<Timeline>> {
        self.inner.read().unwrap().timelines()
    }

    /// Every part the catalog holds, on a timeline or waiting for an anchor,
    /// as `(layer, part)`. This is what gets saved across restarts.
    pub fn parts(&self) -> Vec<(&'static str, Part)> {
        self.inner.read().unwrap().parts()
    }

    /// The newest part `source` has on a layer, by valid time.
    pub fn latest(&self, layer: &str, source: &str) -> Option<Part> {
        self.inner.read().unwrap().latest(layer, source)
    }

    /// Check a tile request. `combo` is the part ids the client named, top
    /// first. The answer is those parts when some frame that is on the
    /// timeline now, or was less than [`SERVE_GRACE_SECONDS`] ago, has parts
    /// `P` such that `combo` is an in-order selection from `P` with
    /// `tile_parts_must(P) ⊆ combo ⊆ tile_parts_may(P)`, and `coord.z` is no
    /// deeper than the largest `max_zoom` among the named parts. Otherwise
    /// `None`: the request names something that never was a tile.
    pub fn resolve(
        &self,
        layer: &str,
        coord: TileCoord,
        combo: &[&str],
        now: DateTime<Utc>,
    ) -> Option<Vec<Part>> {
        self.inner.read().unwrap().resolve(layer, coord, combo, now)
    }

    /// Apply the expiry rules and (on anchored layers) move anchors that have
    /// waited long enough, announcing what changed. Call about once a minute.
    pub fn sweep(&self, now: DateTime<Utc>) {
        let events = self.inner.write().unwrap().sweep(now);
        for e in events {
            self.hub.send(e);
        }
    }

    /// Parts that have left the catalog since the last call (expired,
    /// replaced by a newer run, or left behind by an anchor), with when they
    /// left. Their grids and tiles are the caller's to delete, no sooner than
    /// [`SERVE_GRACE_SECONDS`] after that moment.
    pub fn take_dropped(&self) -> Vec<Dropped> {
        self.inner.write().unwrap().take_dropped()
    }
}

/// A part that left the catalog. See [`FrameCatalog::take_dropped`].
#[derive(Debug, Clone, PartialEq)]
pub struct Dropped {
    pub layer: &'static str,
    pub part: Part,
    pub at: DateTime<Utc>,
}

/// Build the event announcing a timeline change. `frames` are the new or
/// changed frames, `removed` the time steps that no longer exist, and `parts`
/// the parts named by `frames` that no frame named before the change.
pub(crate) fn timeline_event(
    layer: &str,
    parts: Vec<Part>,
    frames: Vec<Frame>,
    removed: Vec<DateTime<Utc>>,
) -> WsEvent {
    WsEvent::Timeline {
        layer: layer.to_string(),
        parts,
        frames,
        removed,
    }
}

mod catalog;

#[cfg(test)]
mod tests;
