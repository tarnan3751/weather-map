//! The catalog's state and rules. See the parent module for the contract.
//!
//! Each layer keeps its parts per source. After a change it rebuilds the
//! frames the change can reach and diffs them against what they were: an
//! insert that moves neither the latest observation nor the anchor reaches
//! only the ticks from the part's valid time to the end of its hold or the
//! source's next part, and its own tick; anything else rebuilds the whole
//! timeline. Frames that changed or left are kept, with the parts they
//! named, for [`SERVE_GRACE_SECONDS`] so tile requests naming them still
//! resolve. Tile requests find their frames through an index from part id to
//! the frames that name it. Readers share one [`Timeline`] per layer, built
//! on the first read after a change.

use super::{
    ANCHOR_CYCLE_HOURS, ANCHOR_PATIENCE_MINUTES, CatalogError, Dropped, FORECAST_GRACE_MINUTES,
    Frame, Insert, MAX_COMBO_PARTS, MAX_OBSERVED_AHEAD_MINUTES, MIN_ANCHOR_PARTS, Part,
    SERVE_GRACE_SECONDS, Timeline, tile_parts_may, tile_parts_must, timeline_event,
};
use crate::events::WsEvent;
use crate::layer::{FrameKind, LayerSpec};
use crate::proj::TileCoord;
use crate::source::SourceDef;
use chrono::{DateTime, Duration, Utc};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ops::Bound;
use std::sync::{Arc, OnceLock};

/// Every tick, as the span of a whole rebuild.
const ALL: (DateTime<Utc>, DateTime<Utc>) = (DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC);

/// One source's parts on one layer, by run key and then valid time. The run
/// key is the part's run on anchored layers, where every run is its own
/// accumulation, and `None` elsewhere, so that rule 1 leaves one part per
/// valid time.
type Runs = BTreeMap<Option<DateTime<Utc>>, BTreeMap<DateTime<Utc>, Arc<Part>>>;

pub(super) struct State {
    /// In registry order.
    layers: Vec<LayerState>,
    dropped: Vec<Dropped>,
    /// The latest `now` given to `insert` or `sweep`. Every rule measures
    /// against it, or against the caller's `now` when that is later.
    clock: Option<DateTime<Utc>>,
}

/// What one source contributes to one layer.
struct Input {
    source: &'static SourceDef,
    observed: bool,
    hold: Duration,
    fill_past: bool,
}

impl Input {
    /// Rule 3: does a part of this input take part at `tick`?
    fn takes_part(&self, tick: DateTime<Utc>, latest_observed: Option<DateTime<Utc>>) -> bool {
        if self.observed {
            latest_observed.is_some_and(|lo| tick <= lo)
        } else {
            self.fill_past || latest_observed.is_none_or(|lo| tick > lo)
        }
    }

    /// Rule 7, with `window` the layer's retention.
    fn expired(
        &self,
        window: Duration,
        valid: DateTime<Utc>,
        latest_observed: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        if self.observed || self.fill_past {
            valid + self.hold <= now - window
        } else {
            valid < now - Duration::minutes(FORECAST_GRACE_MINUTES)
                || latest_observed.is_some_and(|lo| valid + self.hold <= lo)
        }
    }
}

/// One frame of the current timeline.
struct Built {
    valid: DateTime<Utc>,
    kind: FrameKind,
    /// Top first.
    parts: Vec<Arc<Part>>,
}

impl Built {
    fn frame(&self) -> Frame {
        Frame {
            valid: self.valid,
            kind: self.kind,
            parts: self.parts.iter().map(|p| p.id.clone()).collect(),
        }
    }

    fn same_as(&self, other: &Built) -> bool {
        self.kind == other.kind
            && self.parts.len() == other.parts.len()
            && self
                .parts
                .iter()
                .zip(&other.parts)
                .all(|(a, b)| Arc::ptr_eq(a, b) || a.id == b.id)
    }
}

/// A frame as it was before it changed or left the timeline at `at`.
struct Retired {
    at: DateTime<Utc>,
    parts: Vec<Arc<Part>>,
}

struct LayerState {
    spec: &'static LayerSpec,
    /// Every source with an input on the layer, top first: rank, highest
    /// first, then source id.
    inputs: Vec<Input>,
    /// Held parts, indexed like `inputs`. Empty inner maps are removed, so a
    /// source has parts exactly when its entry is not empty.
    held: Vec<Runs>,
    /// Indexed like `inputs`: is the source's feed running? Disabled sources
    /// never act as an anchored layer's base and are never waited for.
    enabled: Vec<bool>,
    /// Ids of every held part.
    ids: HashSet<String>,
    /// Anchored layers only: the run the timeline is built from.
    anchor: Option<DateTime<Utc>>,
    /// Does any input fill the past? Then rule 4 cuts every tick before the
    /// window, not only those up to the latest observation.
    fills_past: bool,
    /// Anchored layers only: from when a stand-in's wait for the registry
    /// base before setting the first anchor is counted. The first `now` at
    /// which the layer holds a part; a restore starts it again.
    waiting_since: Option<DateTime<Utc>>,
    /// Anchored layers only: when each source's first part of each run
    /// arrived, keyed by input index and run.
    arrived: HashMap<(usize, DateTime<Utc>), DateTime<Utc>>,
    /// The timeline, oldest first.
    frames: Vec<Built>,
    /// For each part the timeline names, the time steps that name it.
    named_by: HashMap<String, Vec<DateTime<Utc>>>,
    /// Old forms of frames, oldest retirement first. Entry `k` has sequence
    /// number `retired_seq0 + k`.
    retired: VecDeque<Retired>,
    retired_seq0: u64,
    /// For each part a retired frame names, the sequence numbers of those
    /// frames.
    retired_by_id: HashMap<String, Vec<u64>>,
    /// `frames` as readers see them; emptied whenever `frames` changes.
    snapshot: OnceLock<Arc<Timeline>>,
}

impl State {
    pub fn with_registry(layers: &'static [LayerSpec], sources: &'static [SourceDef]) -> Self {
        State {
            layers: layers.iter().map(|l| LayerState::new(l, sources)).collect(),
            dropped: Vec::new(),
            clock: None,
        }
    }

    fn layer(&self, id: &str) -> Option<&LayerState> {
        self.layers.iter().find(|l| l.spec.id == id)
    }

    /// The moment the rules measure against for a caller's `now`.
    fn now(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        self.clock.map_or(now, |c| c.max(now))
    }

    /// Like [`State::now`], and remember it.
    fn advance(&mut self, now: DateTime<Utc>) -> DateTime<Utc> {
        let now = self.now(now);
        self.clock = Some(now);
        now
    }

    /// Parts that are not kept are ignored without being reported as
    /// dropped: they never were in this catalog, and there is no `now` to
    /// date their departure. Whoever cleans up the store finds their objects
    /// as orphans.
    pub fn restore(&mut self, parts: impl IntoIterator<Item = (String, Part)>) {
        for (layer, part) in parts {
            let Some(ls) = self.layers.iter_mut().find(|l| l.spec.id == layer) else {
                continue;
            };
            let Some(i) = ls.input_index(&part.source) else {
                continue;
            };
            if ls.spec.anchored && !ls.run_fits(part.run) {
                continue;
            }
            if ls.ids.contains(&part.id) || ls.holds_same_or_newer(i, part.valid, part.run) {
                continue;
            }
            ls.add(i, part);
        }
        for ls in &mut self.layers {
            if ls.spec.anchored {
                ls.waiting_since = None;
                ls.advance_anchor(None, true);
            }
            ls.rebuild(self.clock, false, ALL);
        }
    }

    pub fn set_enabled_sources(&mut self, ids: &[&str]) {
        for ls in &mut self.layers {
            ls.enabled = ls
                .inputs
                .iter()
                .map(|inp| ids.contains(&inp.source.id))
                .collect();
        }
    }

    pub fn wants(
        &self,
        layer: &str,
        source: &str,
        valid: DateTime<Utc>,
        run: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        let now = self.now(now);
        self.layer(layer)
            .and_then(|ls| ls.input_index(source).map(|i| ls.takes(i, valid, run, now)))
            .unwrap_or(false)
    }

    pub fn insert(
        &mut self,
        layer: &str,
        part: Part,
        now: DateTime<Utc>,
    ) -> Result<(Insert, Option<WsEvent>), CatalogError> {
        let now = self.advance(now);
        let ls = self
            .layers
            .iter_mut()
            .find(|l| l.spec.id == layer)
            .ok_or_else(|| CatalogError::UnknownLayer(layer.to_string()))?;
        let i = ls
            .input_index(&part.source)
            .ok_or_else(|| CatalogError::NoSuchInput {
                layer: layer.to_string(),
                source_id: part.source.clone(),
            })?;
        ls.prune_retired(now);
        if !ls.takes(i, part.valid, part.run, now) || ls.ids.contains(&part.id) {
            return Ok((Insert::Refused, None));
        }
        let (valid, run) = (part.valid, part.run);
        let (latest_observed, anchor) = (ls.latest_observed(), ls.anchor);
        let replaced = ls.add(i, part);
        let mut gone: Vec<Arc<Part>> = replaced.iter().cloned().collect();
        if ls.spec.anchored
            && let Some(run) = run
        {
            ls.arrived.entry((i, run)).or_insert(now);
            gone.extend(ls.advance_anchor(Some(now), false));
        }
        let span = if ls.anchor == anchor && ls.latest_observed() == latest_observed {
            ls.reach(i, valid, run)
        } else {
            ALL
        };
        let event = ls.rebuild(Some(now), true, span);
        let id = ls.spec.id;
        self.dropped.extend(gone.into_iter().map(|p| Dropped {
            layer: id,
            part: Arc::unwrap_or_clone(p),
            at: now,
        }));
        let replaced = replaced.map(Arc::unwrap_or_clone);
        Ok((Insert::Added { replaced }, event))
    }

    pub fn timeline(&self, layer: &str) -> Arc<Timeline> {
        self.layer(layer)
            .map(LayerState::timeline)
            .unwrap_or_default()
    }

    pub fn timelines(&self) -> BTreeMap<&'static str, Arc<Timeline>> {
        self.layers
            .iter()
            .map(|l| (l.spec.id, l.timeline()))
            .collect()
    }

    pub fn parts(&self) -> Vec<(&'static str, Part)> {
        let mut out = Vec::new();
        for ls in &self.layers {
            for runs in &ls.held {
                for m in runs.values() {
                    out.extend(m.values().map(|p| (ls.spec.id, Part::clone(p))));
                }
            }
        }
        out
    }

    pub fn latest(&self, layer: &str, source: &str) -> Option<Part> {
        let ls = self.layer(layer)?;
        let i = ls.input_index(source)?;
        ls.held[i]
            .values()
            .filter_map(|m| m.values().next_back())
            .max_by_key(|p| (p.valid, p.run))
            .map(|p| Part::clone(p))
    }

    pub fn resolve(
        &self,
        layer: &str,
        coord: TileCoord,
        combo: &[&str],
        now: DateTime<Utc>,
    ) -> Option<Vec<Part>> {
        self.layer(layer)?.resolve(coord, combo, now)
    }

    pub fn sweep(&mut self, now: DateTime<Utc>) -> Vec<WsEvent> {
        let now = self.advance(now);
        let mut events = Vec::new();
        for ls in &mut self.layers {
            ls.prune_retired(now);
            let anchor = ls.anchor;
            let mut gone = ls.expire(now);
            if ls.spec.anchored {
                gone.extend(ls.advance_anchor(Some(now), false));
            }
            let lo = ls.latest_observed();
            let stale = ls
                .frames
                .first()
                .is_some_and(|f| ls.before_window(f.valid, lo, Some(now)));
            if (!gone.is_empty() || ls.anchor != anchor || stale)
                && let Some(e) = ls.rebuild(Some(now), true, ALL)
            {
                events.push(e);
            }
            let id = ls.spec.id;
            self.dropped.extend(gone.into_iter().map(|p| Dropped {
                layer: id,
                part: Arc::unwrap_or_clone(p),
                at: now,
            }));
        }
        events
    }

    pub fn frame_parts(&self, layer: &str, valid: DateTime<Utc>) -> Option<Vec<Part>> {
        let ls = self.layer(layer)?;
        let k = ls.frames.binary_search_by_key(&valid, |f| f.valid).ok()?;
        Some(ls.frames[k].parts.iter().map(|p| Part::clone(p)).collect())
    }

    pub fn take_dropped(&mut self) -> Vec<Dropped> {
        std::mem::take(&mut self.dropped)
    }
}

impl LayerState {
    fn new(spec: &'static LayerSpec, sources: &'static [SourceDef]) -> Self {
        let mut inputs: Vec<Input> = sources
            .iter()
            .filter_map(|s| {
                spec.input_for(s).map(|inp| Input {
                    source: s,
                    observed: s.kind == FrameKind::Observed,
                    hold: Duration::minutes(inp.hold_minutes as i64),
                    fill_past: inp.fill_past,
                })
            })
            .collect();
        inputs.sort_by(|a, b| {
            b.source
                .rank
                .cmp(&a.source.rank)
                .then_with(|| a.source.id.cmp(b.source.id))
        });
        LayerState {
            spec,
            held: inputs.iter().map(|_| Runs::new()).collect(),
            enabled: vec![true; inputs.len()],
            inputs,
            ids: HashSet::new(),
            anchor: None,
            fills_past: spec.inputs.iter().any(|i| i.fill_past),
            waiting_since: None,
            arrived: HashMap::new(),
            frames: Vec::new(),
            named_by: HashMap::new(),
            retired: VecDeque::new(),
            retired_seq0: 0,
            retired_by_id: HashMap::new(),
            snapshot: OnceLock::new(),
        }
    }

    fn input_index(&self, source: &str) -> Option<usize> {
        self.inputs.iter().position(|inp| inp.source.id == source)
    }

    fn run_key(&self, run: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
        if self.spec.anchored { run } else { None }
    }

    /// The run key of the parts the timeline is built from, if any.
    fn timeline_key(&self) -> Option<Option<DateTime<Utc>>> {
        if self.spec.anchored {
            self.anchor.map(Some)
        } else {
            Some(None)
        }
    }

    fn window(&self) -> Duration {
        Duration::minutes(self.spec.retention_minutes as i64)
    }

    /// Rule 4: is `tick` one where parts kept for their hold make no frame?
    fn before_window(
        &self,
        tick: DateTime<Utc>,
        latest_observed: Option<DateTime<Utc>>,
        now: Option<DateTime<Utc>>,
    ) -> bool {
        now.is_some_and(|now| tick < now - self.window())
            && (self.fills_past || latest_observed.is_some_and(|lo| tick <= lo))
    }

    /// Rule 2.
    fn tick(&self, input: &Input, valid: DateTime<Utc>) -> DateTime<Utc> {
        if !input.observed {
            return valid;
        }
        let step = (self.spec.observed_tick_minutes.max(1) as i64) * 60;
        let s = valid.timestamp();
        let up = s.rem_euclid(step) != 0 || valid.timestamp_subsec_nanos() != 0;
        let t = (s.div_euclid(step) + up as i64) * step;
        DateTime::from_timestamp(t, 0).unwrap_or(valid)
    }

    /// Rule 2: the newest tick of any observed part.
    fn latest_observed(&self) -> Option<DateTime<Utc>> {
        self.inputs
            .iter()
            .zip(&self.held)
            .filter(|(inp, _)| inp.observed)
            .flat_map(|(inp, runs)| {
                runs.values()
                    .filter_map(|m| m.keys().next_back())
                    .map(|v| self.tick(inp, *v))
            })
            .max()
    }

    fn holds_same_or_newer(
        &self,
        i: usize,
        valid: DateTime<Utc>,
        run: Option<DateTime<Utc>>,
    ) -> bool {
        self.held[i]
            .get(&self.run_key(run))
            .and_then(|m| m.get(&valid))
            .is_some_and(|old| old.run >= run)
    }

    /// Anchored layers only: is a part of this run taken? It must start on
    /// the anchor cycle and not before the anchor.
    fn run_fits(&self, run: Option<DateTime<Utc>>) -> bool {
        run.is_some_and(|r| {
            r.timestamp().rem_euclid(ANCHOR_CYCLE_HOURS * 3600) == 0
                && self.anchor.is_none_or(|a| r >= a)
        })
    }

    /// Would a part of input `i` with this valid time and run be taken now?
    fn takes(
        &self,
        i: usize,
        valid: DateTime<Utc>,
        run: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        let input = &self.inputs[i];
        if self.spec.anchored && !self.run_fits(run) {
            return false;
        }
        if input.observed && valid > now + Duration::minutes(MAX_OBSERVED_AHEAD_MINUTES) {
            return false;
        }
        !input.expired(self.window(), valid, self.latest_observed(), now)
            && !self.holds_same_or_newer(i, valid, run)
    }

    /// Store a part, returning the one of the same source and valid time
    /// (and, on anchored layers, run) that it displaces.
    fn add(&mut self, i: usize, part: Part) -> Option<Arc<Part>> {
        let key = self.run_key(part.run);
        self.ids.insert(part.id.clone());
        let old = self.held[i]
            .entry(key)
            .or_default()
            .insert(part.valid, Arc::new(part));
        if let Some(o) = &old {
            self.ids.remove(&o.id);
        }
        old
    }

    /// Rule 7. Returns the parts removed.
    fn expire(&mut self, now: DateTime<Utc>) -> Vec<Arc<Part>> {
        let window = self.window();
        let mut gone = Vec::new();
        // Observations first, so that the latest observation the other
        // forecasts are measured against is one that stays.
        for (inp, runs) in self.inputs.iter().zip(&mut self.held) {
            if inp.observed || inp.fill_past {
                drain_oldest(runs, |v| inp.expired(window, v, None, now), &mut gone);
            }
        }
        let lo = self.latest_observed();
        for (inp, runs) in self.inputs.iter().zip(&mut self.held) {
            if !(inp.observed || inp.fill_past) {
                drain_oldest(runs, |v| inp.expired(window, v, lo, now), &mut gone);
            }
        }
        for p in &gone {
            self.ids.remove(&p.id);
        }
        gone
    }

    /// The lowest-ranked enabled source with an input on the layer.
    fn registry_base(&self) -> Option<usize> {
        self.enabled.iter().rposition(|e| *e)
    }

    /// The lowest-ranked enabled source whose newest run here is no more
    /// than two anchor cycles older than the newest run any enabled source
    /// holds here: the registry base, unless it holds nothing or has stalled.
    fn base(&self) -> Option<usize> {
        let newest = |runs: &Runs| runs.keys().next_back().copied().flatten();
        let latest = self
            .held
            .iter()
            .zip(&self.enabled)
            .filter(|(_, e)| **e)
            .filter_map(|(runs, _)| newest(runs))
            .max()?;
        let oldest = latest - Duration::hours(2 * ANCHOR_CYCLE_HOURS);
        self.held
            .iter()
            .zip(&self.enabled)
            .rposition(|(runs, e)| *e && newest(runs).is_some_and(|r| r >= oldest))
    }

    /// Move the anchor as far as the rules allow, and remove the parts of
    /// runs older than where it ends up, which are returned. Without `now`
    /// (on restore) only the reach rule can move it, and `oldest_first`
    /// starts a missing anchor at the oldest run with enough base parts
    /// rather than the newest.
    fn advance_anchor(&mut self, now: Option<DateTime<Utc>>, oldest_first: bool) -> Vec<Arc<Part>> {
        let Some(b) = self.base() else {
            return Vec::new();
        };
        // Restored runs have no arrival time; their patience starts now, as
        // does a stand-in's wait after a restore.
        if let Some(now) = now {
            for r in self.held[b].keys().flatten() {
                self.arrived.entry((b, *r)).or_insert(now);
            }
            if self.waiting_since.is_none() {
                self.waiting_since = Some(now);
            }
        }
        let patience = Duration::minutes(ANCHOR_PATIENCE_MINUTES);
        // A stand-in sets the first anchor only once the registry base, if it
        // has produced nothing, has had its patience.
        let may_start = self
            .registry_base()
            .is_none_or(|g| !self.held[g].is_empty())
            || now
                .zip(self.waiting_since)
                .is_some_and(|(now, since)| now - since >= patience);
        let runs = &self.held[b];
        let before = self.anchor;
        let mut anchor = before.or_else(|| {
            if !may_start {
                return None;
            }
            let mut enough = runs
                .iter()
                .filter(|(_, m)| m.len() >= MIN_ANCHOR_PARTS)
                .filter_map(|(r, _)| *r);
            if oldest_first {
                enough.next()
            } else {
                enough.next_back()
            }
        });
        while let Some(a) = anchor {
            let current = runs.get(&Some(a));
            let next = runs
                .range((Bound::Excluded(Some(a)), Bound::Unbounded))
                .rev()
                .filter_map(|(r, m)| r.map(|r| (r, m)))
                .find(|(r, m)| {
                    // Every enabled source on the anchor must have started on
                    // `r`.
                    let covered = current.is_some_and(|cur| covers(cur, m, *r))
                        && self.held.iter().zip(&self.enabled).all(|(other, e)| {
                            !*e || !other.contains_key(&Some(a)) || other.contains_key(&Some(*r))
                        });
                    let waited = m.len() >= MIN_ANCHOR_PARTS
                        && now
                            .zip(self.arrived.get(&(b, *r)))
                            .is_some_and(|(now, first)| now - *first >= patience);
                    covered || waited
                })
                .map(|(r, _)| r);
            match next {
                Some(r) => anchor = Some(r),
                None => break,
            }
        }
        self.anchor = anchor;
        let Some(a) = self.anchor.filter(|_| self.anchor != before) else {
            return Vec::new();
        };
        self.arrived.retain(|(_, r), _| *r >= a);
        let mut gone = Vec::new();
        for runs in &mut self.held {
            let keep = runs.split_off(&Some(a));
            for (_, m) in std::mem::replace(runs, keep) {
                gone.extend(m.into_values());
            }
        }
        for p in &gone {
            self.ids.remove(&p.id);
        }
        gone
    }

    /// The ticks whose frames a part of input `i` just stored at `valid`
    /// (under `run`'s key) can change, as a span `[from, to)`: from its valid
    /// time to the end of its hold or the source's next part, whichever comes
    /// first, and its own tick, which may become a time step. Empty for a
    /// part off the timeline (a run other than the anchor). Correct only
    /// while the latest observation and the anchor stay where they were.
    fn reach(
        &self,
        i: usize,
        valid: DateTime<Utc>,
        run: Option<DateTime<Utc>>,
    ) -> (DateTime<Utc>, DateTime<Utc>) {
        let key = self.run_key(run);
        if self.timeline_key() != Some(key) {
            return (valid, valid);
        }
        let input = &self.inputs[i];
        let next = self.held[i]
            .get(&key)
            .and_then(|m| m.range((Bound::Excluded(valid), Bound::Unbounded)).next())
            .map_or(ALL.1, |(v, _)| *v);
        let own = self.tick(input, valid) + Duration::nanoseconds(1);
        (valid, (valid + input.hold).min(next).max(own))
    }

    /// Rules 2 to 6 over the held parts, for the ticks in `[from, to)`, with
    /// rule 4's window measured from `now` when there is one.
    fn build(
        &self,
        now: Option<DateTime<Utc>>,
        (from, to): (DateTime<Utc>, DateTime<Utc>),
    ) -> Vec<Built> {
        let Some(key) = self.timeline_key().filter(|_| from < to) else {
            return Vec::new();
        };
        let lo = self.latest_observed();
        let lists: Vec<Option<&BTreeMap<DateTime<Utc>, Arc<Part>>>> =
            self.held.iter().map(|runs| runs.get(&key)).collect();

        // An observed part's tick is less than one tick step after its valid
        // time.
        let step = Duration::minutes(self.spec.observed_tick_minutes.max(1) as i64);
        let mut ticks = Vec::new();
        for (inp, list) in self.inputs.iter().zip(&lists) {
            let Some(m) = list else { continue };
            let first = if inp.observed {
                from.checked_sub_signed(step).unwrap_or(from)
            } else {
                from
            };
            for v in m.range(first..to).map(|(v, _)| *v) {
                let t = self.tick(inp, v);
                if (from..to).contains(&t)
                    && inp.takes_part(t, lo)
                    && !self.before_window(t, lo, now)
                {
                    ticks.push(t);
                }
            }
        }
        ticks.sort_unstable();
        ticks.dedup();

        // Per source, the parts from `from` on in valid order, and the newest
        // one so far, starting from the last before `from`.
        let mut cursors: Vec<_> = lists
            .iter()
            .map(|m| {
                let newest = m.and_then(|m| m.range(..from).next_back()).map(|(_, p)| p);
                (
                    m.map(|m| m.range(from..).map(|(_, p)| p).peekable()),
                    newest,
                )
            })
            .collect();
        let mut frames = Vec::with_capacity(ticks.len());
        for t in ticks {
            let mut parts = Vec::new();
            for (inp, (rest, newest)) in self.inputs.iter().zip(&mut cursors) {
                let Some(rest) = rest else { continue };
                while let Some(p) = rest.next_if(|p| p.valid <= t) {
                    *newest = Some(p);
                }
                if let Some(p) = *newest
                    && inp.takes_part(t, lo)
                    && t - p.valid < inp.hold
                {
                    parts.push(Arc::clone(p));
                }
            }
            if !parts.is_empty() {
                let kind = if lo.is_some_and(|lo| t <= lo) {
                    FrameKind::Observed
                } else {
                    FrameKind::Forecast
                };
                frames.push(Built {
                    valid: t,
                    kind,
                    parts,
                });
            }
        }
        frames
    }

    /// Drop the frames that rule 4 now cuts (always a run from the oldest),
    /// rebuild the frames at the ticks of `span` as of `now`, and diff the
    /// result against the previous timeline. Frames outside the
    /// span are taken to be unchanged. With `announce` (which needs `now`),
    /// the old forms of frames that changed or left are retired and the
    /// change is returned as an event; without, it is silent.
    fn rebuild(
        &mut self,
        now: Option<DateTime<Utc>>,
        announce: bool,
        span: (DateTime<Utc>, DateTime<Utc>),
    ) -> Option<WsEvent> {
        let event = self.rebuild_span(now, announce, span);
        #[cfg(test)]
        self.check_against_a_full_build(now);
        event
    }

    fn rebuild_span(
        &mut self,
        now: Option<DateTime<Utc>>,
        announce: bool,
        span: (DateTime<Utc>, DateTime<Utc>),
    ) -> Option<WsEvent> {
        let new = self.build(now, span);
        let lo = self.latest_observed();
        let cut = self
            .frames
            .iter()
            .take_while(|f| self.before_window(f.valid, lo, now))
            .count();
        let mut gone: Vec<Built> = self.frames.drain(..cut).collect();
        let mut removed: Vec<DateTime<Utc>> = gone.iter().map(|f| f.valid).collect();
        let start = self.frames.partition_point(|f| f.valid < span.0);
        let end = self.frames.partition_point(|f| f.valid < span.1).max(start);
        let fresh = start..start + new.len();
        let old: Vec<Built> = self.frames.splice(start..end, new).collect();
        let mut changed = Vec::new();
        let mut old = old.into_iter().peekable();
        for n in fresh {
            let f = &self.frames[n];
            while let Some(o) = old.next_if(|o| o.valid < f.valid) {
                removed.push(o.valid);
                gone.push(o);
            }
            match old.next_if(|o| o.valid == f.valid) {
                Some(o) if o.same_as(f) => {}
                Some(o) => {
                    gone.push(o);
                    changed.push(n);
                }
                None => changed.push(n),
            }
        }
        for o in old {
            removed.push(o.valid);
            gone.push(o);
        }
        if changed.is_empty() && gone.is_empty() {
            return None;
        }
        self.snapshot = OnceLock::new();

        // `named_by` still describes the timeline before the change.
        let mut new_parts = Vec::new();
        let mut seen = HashSet::new();
        for &n in &changed {
            for p in &self.frames[n].parts {
                if !self.named_by.contains_key(&p.id) && seen.insert(p.id.as_str()) {
                    new_parts.push(Part::clone(p));
                }
            }
        }
        for o in &gone {
            for p in &o.parts {
                unindex(&mut self.named_by, &p.id, &o.valid);
            }
        }
        for &n in &changed {
            let f = &self.frames[n];
            for p in &f.parts {
                index(&mut self.named_by, &p.id, f.valid);
            }
        }

        let now = now.filter(|_| announce)?;
        let frames = changed.iter().map(|&n| self.frames[n].frame()).collect();
        for o in gone {
            self.retire(o, now);
        }
        Some(timeline_event(self.spec.id, new_parts, frames, removed))
    }

    /// The invariant behind partial rebuilds: the timeline is what building
    /// it whole would give.
    #[cfg(test)]
    fn check_against_a_full_build(&self, now: Option<DateTime<Utc>>) {
        let whole: Vec<Frame> = self.build(now, ALL).iter().map(Built::frame).collect();
        let kept: Vec<Frame> = self.frames.iter().map(Built::frame).collect();
        assert_eq!(
            kept, whole,
            "{}: a partial rebuild went astray",
            self.spec.id
        );
        let named: usize = self.named_by.values().map(Vec::len).sum();
        assert_eq!(
            named,
            self.frames.iter().map(|f| f.parts.len()).sum::<usize>()
        );
    }

    fn retire(&mut self, f: Built, at: DateTime<Utc>) {
        let seq = self.retired_seq0 + self.retired.len() as u64;
        for p in &f.parts {
            index(&mut self.retired_by_id, &p.id, seq);
        }
        self.retired.push_back(Retired { at, parts: f.parts });
    }

    fn prune_retired(&mut self, now: DateTime<Utc>) {
        let grace = Duration::seconds(SERVE_GRACE_SECONDS);
        while self.retired.front().is_some_and(|r| now - r.at >= grace) {
            let Some(r) = self.retired.pop_front() else {
                break;
            };
            let seq = self.retired_seq0;
            self.retired_seq0 += 1;
            for p in &r.parts {
                unindex(&mut self.retired_by_id, &p.id, &seq);
            }
        }
    }

    fn resolve(&self, coord: TileCoord, combo: &[&str], now: DateTime<Utc>) -> Option<Vec<Part>> {
        let first = *combo.first()?;
        if combo.len() > MAX_COMBO_PARTS {
            return None;
        }
        let live = self
            .named_by
            .get(first)
            .into_iter()
            .flatten()
            .filter_map(|t| {
                let k = self.frames.binary_search_by_key(t, |f| f.valid).ok()?;
                Some(&self.frames[k].parts)
            });
        let grace = Duration::seconds(SERVE_GRACE_SECONDS);
        let retired = self
            .retired_by_id
            .get(first)
            .into_iter()
            .flatten()
            .filter_map(|seq| {
                let r = self
                    .retired
                    .get(seq.checked_sub(self.retired_seq0)? as usize)?;
                (now - r.at < grace).then_some(&r.parts)
            });
        live.chain(retired)
            .find_map(|parts| combo_in(parts, coord, combo))
    }

    fn timeline(&self) -> Arc<Timeline> {
        let build = || {
            let mut seen = HashSet::new();
            let mut parts = Vec::new();
            let frames = self
                .frames
                .iter()
                .map(|f| {
                    for p in &f.parts {
                        if seen.insert(p.id.as_str()) {
                            parts.push(Part::clone(p));
                        }
                    }
                    f.frame()
                })
                .collect();
            Arc::new(Timeline { frames, parts })
        };
        Arc::clone(self.snapshot.get_or_init(build))
    }
}

/// The reach rule: does `next`, the base source's parts for a run starting
/// at `start`, have a part at every valid time after `start` at which
/// `current`, its parts for the anchor, has one? There must be at least one
/// such time.
fn covers(
    current: &BTreeMap<DateTime<Utc>, Arc<Part>>,
    next: &BTreeMap<DateTime<Utc>, Arc<Part>>,
    start: DateTime<Utc>,
) -> bool {
    let mut needed = current
        .range((Bound::Excluded(start), Bound::Unbounded))
        .map(|(v, _)| v)
        .peekable();
    needed.peek().is_some() && needed.all(|v| next.contains_key(v))
}

/// Remove each run's oldest parts while `expired` holds for their valid time
/// (it must hold for every valid time before one it holds for).
fn drain_oldest(
    runs: &mut Runs,
    expired: impl Fn(DateTime<Utc>) -> bool,
    gone: &mut Vec<Arc<Part>>,
) {
    runs.retain(|_, m| {
        while let Some(e) = m.first_entry() {
            if !expired(*e.key()) {
                break;
            }
            gone.push(e.remove());
        }
        !m.is_empty()
    });
}

/// Add `value` to `key`'s entry, copying the key only for a new entry.
fn index<T>(index: &mut HashMap<String, Vec<T>>, key: &str, value: T) {
    match index.get_mut(key) {
        Some(v) => v.push(value),
        None => {
            index.insert(key.to_string(), vec![value]);
        }
    }
}

/// Remove one `value` from `key`'s entry, and the entry once it is empty.
fn unindex<T: PartialEq>(index: &mut HashMap<String, Vec<T>>, key: &str, value: &T) {
    if let Some(v) = index.get_mut(key) {
        if let Some(k) = v.iter().position(|x| x == value) {
            v.swap_remove(k);
        }
        if v.is_empty() {
            index.remove(key);
        }
    }
}

/// The parts `combo` names, when it is a correct answer for tile `coord` of a
/// frame made of `parts` (see [`super::FrameCatalog::resolve`]).
fn combo_in(parts: &[Arc<Part>], coord: TileCoord, combo: &[&str]) -> Option<Vec<Part>> {
    // Where each named part sits in the frame, which must be in order.
    let mut named = vec![false; parts.len()];
    let mut from = 0;
    for id in combo {
        let k = from + parts[from..].iter().position(|p| p.id == *id)?;
        named[k] = true;
        from = k + 1;
    }
    let deepest = parts
        .iter()
        .zip(&named)
        .filter(|(_, n)| **n)
        .map(|(p, _)| p.max_zoom)
        .max()?;
    if coord.z > deepest {
        return None;
    }
    // The tile rules only look at boxes, so they run on copies without the
    // strings.
    let boxes: Vec<Part> = parts
        .iter()
        .map(|p| Part {
            id: String::new(),
            source: String::new(),
            valid: p.valid,
            run: p.run,
            kind: p.kind,
            bounds: p.bounds,
            solid: p.solid,
            max_zoom: p.max_zoom,
        })
        .collect();
    let must = positions(&boxes, &tile_parts_must(&boxes, coord));
    let may = positions(&boxes, &tile_parts_may(&boxes, coord));
    let ok = (0..parts.len()).all(|k| (!must[k] || named[k]) && (!named[k] || may[k]));
    ok.then(|| {
        parts
            .iter()
            .zip(&named)
            .filter(|(_, n)| **n)
            .map(|(p, _)| Part::clone(p))
            .collect()
    })
}

/// Which of `all` are in `picked`, an in-order selection of references into it.
fn positions(all: &[Part], picked: &[&Part]) -> Vec<bool> {
    let mut out = vec![false; all.len()];
    let mut from = 0;
    for p in picked {
        if let Some(k) = all[from..].iter().position(|q| std::ptr::eq(q, *p)) {
            out[from + k] = true;
            from += k + 1;
        }
    }
    out
}
