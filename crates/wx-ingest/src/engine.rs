//! The path every part takes once its data has arrived: build it on a
//! blocking thread under the shared build limit, store its grid, put it on
//! the catalog. The drivers decide what to fetch; this decides nothing but
//! whether a part is still wanted.
//!
//! Errors come in two kinds. A broken file (an inventory that does not match
//! it, a range past its end, a message that will not decode) is charged to
//! every part it was going to feed, logged, and the task moves on; a part
//! charged [`MAX_ATTEMPTS`] times is given up and never fetched again. An
//! error that means the task cannot work at all (network down, the service
//! failing, the destination store failing) is returned, and the supervisor
//! restarts the task with backoff.

use crate::IngestConfig;
use crate::driver::{Decoded, Need, Payload, Steps, fetched_as, is_step};
use crate::janitor::{Deletions, Protect};
use crate::limits::FairPool;
use crate::remote::{Failure, FetchError, Remote, codec};
use anyhow::{Context, ensure};
use chrono::{DateTime, Duration, Utc};
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wx_core::frame::publish_boxes;
use wx_core::layer::{FieldSel, Stat};
use wx_core::{
    Field, FrameCatalog, InputSpec, Insert, LayerSpec, LonLatBox, Part, SourceDef, keys,
};

/// Failures (broken file, failed build) per part before giving up on it.
pub const MAX_ATTEMPTS: u32 = 3;
/// Wants are judged this far ahead, so a part that would expire by the time
/// it is built and stored is not fetched at all.
const WANT_MARGIN: Duration = Duration::minutes(2);

/// How a part's step accumulations are made (see `driver`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The difference of the run totals at the part's lead and at `from`
    /// (zero at the start of the run), per hour.
    Totals { from: u32 },
    /// The file's own record for the hour ending at the lead.
    Window,
}

/// The time a cycle started at, read forward by the monotonic clock: what
/// inserts are stamped with. In production that is the wall clock; in tests
/// it follows the simulated time a cycle is run at.
#[derive(Debug, Clone, Copy)]
pub struct Moment {
    at: DateTime<Utc>,
    started: tokio::time::Instant,
}

impl Moment {
    pub fn starting(at: DateTime<Utc>) -> Self {
        Self {
            at,
            started: tokio::time::Instant::now(),
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.at + Duration::from_std(self.started.elapsed()).unwrap_or_default()
    }
}

/// One part to produce.
#[derive(Debug, Clone)]
pub struct PartJob {
    pub layer: &'static LayerSpec,
    pub input: &'static InputSpec,
    pub source: &'static SourceDef,
    /// `None` for observations.
    pub run: Option<DateTime<Utc>>,
    /// Minutes after the run; 0 for observations.
    pub lead: u32,
    pub valid: DateTime<Utc>,
    /// For inputs with step accumulations.
    pub step: Option<Step>,
    pub id: String,
}

impl PartJob {
    /// `valid` must be on a whole minute. Observations ask for their fields
    /// as the registry names them.
    pub fn observed(
        layer: &'static LayerSpec,
        input: &'static InputSpec,
        source: &'static SourceDef,
        valid: DateTime<Utc>,
    ) -> PartJob {
        PartJob {
            layer,
            input,
            source,
            run: None,
            lead: 0,
            valid,
            step: None,
            id: Part::observed_id(source.id, valid),
        }
    }

    /// A forecast part. `None` where the part cannot exist: an accumulation
    /// at the very start of its run. `previous` is the lead the family lists
    /// before `lead`, if any; `steps` how the product holds step amounts.
    pub fn forecast(
        layer: &'static LayerSpec,
        input: &'static InputSpec,
        source: &'static SourceDef,
        run: DateTime<Utc>,
        lead: u32,
        previous: Option<u32>,
        steps: Steps,
    ) -> Option<PartJob> {
        let stepped = input.fields.iter().any(|f| is_step(f.stat));
        let accumulates = stepped || input.fields.iter().any(|f| f.stat == Stat::RunTotal);
        if lead == 0 && accumulates {
            return None;
        }
        let step = stepped.then(|| match steps {
            Steps::RunTotals => Step::Totals {
                from: previous.unwrap_or(0),
            },
            Steps::Windows => Step::Window,
        });
        Some(PartJob {
            layer,
            input,
            source,
            run: Some(run),
            lead,
            valid: run + Duration::minutes(lead as i64),
            step,
            id: Part::forecast_id(source.id, run, lead),
        })
    }

    /// What to fetch, in the order [`build`] expects: one need per field at
    /// the part's lead, then, for steps made from run totals, one per step
    /// field at the lead its step starts at (none when that is the start of
    /// the run).
    pub fn needs(&self) -> Vec<Need> {
        let input = self.input;
        if input.fields.is_empty() {
            return vec![Need {
                input,
                field: None,
                lead: self.lead,
            }];
        }
        let steps = match self.step {
            Some(Step::Totals { .. }) => Steps::RunTotals,
            _ => Steps::Windows,
        };
        let mut out: Vec<Need> = input
            .fields
            .iter()
            .map(|f| Need {
                input,
                field: Some(fetched_as(*f, steps)),
                lead: self.lead,
            })
            .collect();
        if let Some(Step::Totals { from }) = self
            .step
            .filter(|s| matches!(s, Step::Totals { from } if *from > 0))
        {
            out.extend(
                input
                    .fields
                    .iter()
                    .filter(|f| is_step(f.stat))
                    .map(|f| Need {
                        input,
                        field: Some(fetched_as(*f, steps)),
                        lead: from,
                    }),
            );
        }
        out
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    /// Parts now on the catalog.
    pub produced: usize,
    /// Parts built but refused by the catalog (beaten to it).
    pub superseded: usize,
    /// Parts the data cannot provide (a field missing from the file).
    pub unavailable: usize,
    /// Parts that failed this time (broken file, failed build); each failure
    /// counts towards giving the part up.
    pub failed: usize,
    /// Parts skipped because their old objects were being deleted; they are
    /// tried again next cycle.
    pub busy: usize,
    /// Parts fetched but no longer wanted by the time their build came up
    /// (they expired while waiting, or something better arrived).
    pub unwanted: usize,
}

impl std::ops::AddAssign for Tally {
    fn add_assign(&mut self, o: Tally) {
        self.produced += o.produced;
        self.superseded += o.superseded;
        self.unavailable += o.unavailable;
        self.failed += o.failed;
        self.busy += o.busy;
        self.unwanted += o.unwanted;
    }
}

impl Tally {
    /// Count what came of producing a part: a part that failed is logged,
    /// and an error that stops the task is handed back.
    pub fn count(&mut self, task: &str, r: Result<Produced, Trouble>) -> Option<anyhow::Error> {
        match r {
            Ok(Produced::Inserted) => self.produced += 1,
            Ok(Produced::Superseded) => self.superseded += 1,
            Ok(Produced::Busy) => self.busy += 1,
            Ok(Produced::Unwanted) => self.unwanted += 1,
            Err(Trouble::File(e)) => {
                tracing::warn!(task, "part failed: {e:#}");
                self.failed += 1;
            }
            Err(Trouble::Feed(e)) => return Some(e),
        }
        None
    }

    /// `"3 new, 1 failed"` for a health line.
    pub fn summary(&self) -> String {
        let mut s = format!("{} new", self.produced);
        if self.failed > 0 {
            s.push_str(&format!(", {} failed", self.failed));
        }
        if self.unavailable > 0 {
            s.push_str(&format!(", {} unavailable", self.unavailable));
        }
        s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Produced {
    Inserted,
    /// The catalog refused it: something at least as good arrived first.
    Superseded,
    /// Its old objects are being deleted this moment; try next cycle.
    Busy,
    /// No longer wanted by the time its turn to build came: it expired
    /// while it waited, or something at least as good arrived.
    Unwanted,
}

/// What went wrong while producing parts.
#[derive(Debug)]
pub enum Trouble {
    /// This file, or one part from it, is broken.
    File(anyhow::Error),
    /// The task cannot work at the moment.
    Feed(anyhow::Error),
}

impl From<FetchError> for Trouble {
    fn from(e: FetchError) -> Self {
        match e.failure {
            Failure::Object => Trouble::File(e.into()),
            Failure::Service => Trouble::Feed(e.into()),
        }
    }
}

/// A built part, ready to store.
pub(crate) struct Built {
    pub grid: Vec<u8>,
    pub bounds: LonLatBox,
    pub solid: Option<LonLatBox>,
    pub decoded_valid: Option<DateTime<Utc>>,
}

pub struct Engine {
    pub store: Arc<dyn ObjectStore>,
    pub catalog: Arc<FrameCatalog>,
    pub remote: Remote,
    pub config: IngestConfig,
    /// The layer registry tasks read their inputs from (`LAYERS` except in
    /// tests).
    pub layers: &'static [LayerSpec],
    pub deletions: Arc<Deletions>,
    builds: FairPool,
    failures: Mutex<Failures>,
}

/// Failures per part, by layer and part id.
#[derive(Default)]
struct Failures {
    by_layer: HashMap<&'static str, HashMap<String, u32>>,
    parts: usize,
}

impl Engine {
    pub fn new(
        store: Arc<dyn ObjectStore>,
        catalog: Arc<FrameCatalog>,
        config: IngestConfig,
        remote: Remote,
        layers: &'static [LayerSpec],
    ) -> Self {
        Self {
            store,
            catalog,
            remote,
            builds: FairPool::new(config.max_builds),
            config,
            layers,
            deletions: Arc::new(Deletions::default()),
            failures: Mutex::default(),
        }
    }

    fn failures(&self) -> std::sync::MutexGuard<'_, Failures> {
        self.failures.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn given_up(&self, job: &PartJob) -> bool {
        let f = self.failures();
        let n = f
            .by_layer
            .get(job.layer.id)
            .and_then(|parts| parts.get(&job.id));
        n.is_some_and(|n| *n >= MAX_ATTEMPTS)
    }

    /// Count a failure against a part; `permanent` gives it up at once.
    pub fn record_failure(&self, job: &PartJob, permanent: bool) {
        let mut f = self.failures();
        // Bounded memory: forgetting old failures only costs a retry.
        if f.parts > 20_000 {
            *f = Failures::default();
        }
        let parts = f.by_layer.entry(job.layer.id).or_default();
        let new = !parts.contains_key(&job.id);
        let n = parts.entry(job.id.clone()).or_default();
        *n = if permanent { MAX_ATTEMPTS } else { *n + 1 };
        f.parts += usize::from(new);
    }

    /// `file` lacks a field `job` needs: the part is given up.
    pub fn unavailable(&self, file: &str, job: &PartJob) {
        tracing::warn!(file, layer = job.layer.id, part = %job.id, "the file lacks a field this part needs; giving it up");
        self.record_failure(job, true);
    }

    /// Whether the catalog would take a part of `source` on `layer` valid at
    /// `valid` (from `run`): not when it holds it already, it is expired,
    /// older than what the catalog has, off an anchored layer's cycle.
    pub fn takes(
        &self,
        layer: &LayerSpec,
        source: &SourceDef,
        valid: DateTime<Utc>,
        run: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        self.catalog
            .wants(layer.id, source.id, valid, run, now + WANT_MARGIN)
    }

    /// Whether this part may be worked on: its old objects are not being
    /// deleted this moment, and it has not kept failing.
    pub fn workable(&self, job: &PartJob) -> bool {
        !self.deletions.is_deleting(job.layer.id, &job.id) && !self.given_up(job)
    }

    /// Is this part worth producing now? See [`Engine::takes`] and
    /// [`Engine::workable`].
    pub fn wanted(&self, job: &PartJob, now: DateTime<Utc>) -> bool {
        self.takes(job.layer, job.source, job.valid, job.run, now) && self.workable(job)
    }

    /// Charge a broken file to every part it was going to feed, or pass on
    /// an error that stops the task.
    pub fn charge(&self, file: &str, jobs: &[&PartJob], trouble: Trouble) -> anyhow::Result<usize> {
        match trouble {
            Trouble::Feed(e) => Err(e),
            Trouble::File(e) => {
                for job in jobs {
                    self.record_failure(job, false);
                }
                tracing::warn!(file, parts = jobs.len(), "skipping a broken file: {e:#}");
                Ok(jobs.len())
            }
        }
    }

    /// Build, store and insert one part from its payloads (in
    /// [`PartJob::needs`] order). `lane` is the task's name; `clock` the
    /// cycle's.
    pub async fn produce(
        &self,
        job: &PartJob,
        payloads: Vec<Payload>,
        lane: &str,
        clock: Moment,
    ) -> Result<Produced, Trouble> {
        let layer = job.layer.id;
        // Refused while the janitor is deleting this part's old objects, so
        // the new ones cannot be caught by that delete.
        let Some(protect) = self.deletions.protect(layer, &job.id) else {
            tracing::debug!(layer, part = %job.id, "old objects are being deleted; next cycle");
            return Ok(Produced::Busy);
        };
        // Builds can queue for minutes behind other sources' during a
        // backfill: judge the part again when its turn comes, by the clock
        // its insert would carry, rather than build what the catalog would
        // refuse.
        let permit = self.builds.acquire(lane).await;
        if !self.wanted(job, clock.now()) {
            tracing::debug!(layer, part = %job.id, "no longer wanted when its build came up");
            return Ok(Produced::Unwanted);
        }
        let built = match self.build(job, payloads, permit).await {
            Ok(b) => b,
            Err(e) => {
                self.record_failure(job, false);
                return Err(Trouble::File(
                    e.context(format!("building {layer}/{}", job.id)),
                ));
            }
        };
        if let Some(decoded) = built.decoded_valid
            && (decoded - job.valid).num_seconds().abs() > 60
        {
            tracing::warn!(layer, part = %job.id, %decoded, "decoded valid time differs from the file's");
        }
        let key = keys::grid(layer, &job.id);
        self.store
            .put(&Path::from(key.as_str()), PutPayload::from(built.grid))
            .await
            .with_context(|| format!("storing {key}"))
            .map_err(Trouble::Feed)?;
        self.insert(job, built.bounds, built.solid, protect, clock.now())
    }

    fn insert(
        &self,
        job: &PartJob,
        bounds: LonLatBox,
        solid: Option<LonLatBox>,
        protect: Protect,
        now: DateTime<Utc>,
    ) -> Result<Produced, Trouble> {
        let layer = job.layer.id;
        let (bounds, solid) = publish_boxes(bounds, solid);
        let max_zoom = job.source.max_zoom;
        let part = match job.run {
            None => Part::observed(job.source.id, job.valid, bounds, solid, max_zoom),
            Some(run) => Part::forecast(job.source.id, run, job.lead, bounds, solid, max_zoom),
        };
        debug_assert_eq!(part.id, job.id);
        // Marked live before it can be dropped, so the janitor's view never
        // misses a part that is on the catalog.
        self.deletions.mark_live(layer, &job.id, now);
        let inserted = self.catalog.insert(layer, part, now);
        let outcome = match inserted {
            Ok(Insert::Added { .. }) => {
                tracing::debug!(layer, part = %job.id, "part added");
                Ok(Produced::Inserted)
            }
            // The grid is left to the orphan sweep: if the refusal was for an
            // id the catalog already holds, the stored grid is that part's.
            Ok(Insert::Refused) => {
                self.deletions.unmark_live(layer, &job.id);
                Ok(Produced::Superseded)
            }
            Err(e) => {
                self.deletions.unmark_live(layer, &job.id);
                self.record_failure(job, true);
                Err(Trouble::File(
                    anyhow::Error::new(e).context("inserting the part"),
                ))
            }
        };
        // On the catalog (or refused by it) the part needs no protection.
        drop(protect);
        outcome
    }

    /// Build on a blocking thread, holding `permit` (a shared build slot)
    /// until done.
    async fn build(
        &self,
        job: &PartJob,
        payloads: Vec<Payload>,
        permit: crate::limits::Permit,
    ) -> anyhow::Result<Built> {
        let _permit = permit;
        let job = job.clone();
        let task = tokio::task::spawn_blocking(move || build(&job, payloads));
        match task.await {
            Ok(r) => r,
            Err(e) if e.is_panic() => {
                let payload = e.into_panic();
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                anyhow::bail!("pipeline panicked: {msg}")
            }
            Err(e) => Err(anyhow::Error::new(e).context("build task")),
        }
    }
}

/// Decode, difference steps, derive and grid one part. Pure computation.
pub(crate) fn build(job: &PartJob, payloads: Vec<Payload>) -> anyhow::Result<Built> {
    let (fields, decoded_valid) = fields(job, payloads)?;
    let raster = wx_pipeline::derive(job.layer, job.input, &fields).context("derive")?;
    drop(fields);
    let grid = wx_pipeline::FrameGrid::build(job.layer, &raster);
    drop(raster);
    Ok(Built {
        bounds: grid.bounds(),
        solid: grid.solid(),
        grid: grid.to_bytes(),
        decoded_valid,
    })
}

/// A part's fields in input order, its steps differenced, from its payloads
/// (in [`PartJob::needs`] order), which go as soon as they are decoded.
fn fields(
    job: &PartJob,
    mut payloads: Vec<Payload>,
) -> anyhow::Result<(Vec<Field>, Option<DateTime<Utc>>)> {
    let needs = job.needs();
    ensure!(
        payloads.len() == needs.len(),
        "{} payloads for {} needs",
        payloads.len(),
        needs.len()
    );
    let current = needs.iter().filter(|n| n.lead == job.lead).count();
    let before = payloads.split_off(current);
    let (mut fields, decoded_valid) = decode_set(&needs[..current], payloads)?;
    if let Some(Step::Totals { from }) = job.step {
        let hours = f64::from(job.lead.saturating_sub(from)) / 60.0;
        ensure!(hours > 0.0, "a step from lead {from} to lead {}", job.lead);
        let mut earlier = before.into_iter().zip(&needs[current..]);
        for (k, sel) in job.input.fields.iter().enumerate() {
            if !is_step(sel.stat) {
                continue;
            }
            let start = if from == 0 {
                None
            } else {
                let (p, n) = earlier
                    .next()
                    .context("missing the accumulation at the start of the step")?;
                Some(decode_one(n, &p)?.0)
            };
            step_per_hour(&mut fields[k], start.as_ref(), hours).with_context(|| {
                format!("{}:{} from lead {from} to {}", sel.var, sel.level, job.lead)
            })?;
        }
    }
    Ok((fields, decoded_valid))
}

/// The fields at the part's own lead, in input order. GRIB payloads are
/// decoded in one call, which checks they describe the same time; a single
/// one where it lies, several end to end.
fn decode_set(
    needs: &[Need],
    payloads: Vec<Payload>,
) -> anyhow::Result<(Vec<Field>, Option<DateTime<Utc>>)> {
    if payloads.iter().all(|p| matches!(p, Payload::Grib(_))) {
        let wanted: Vec<FieldSel> = needs.iter().filter_map(|n| n.field).collect();
        let set = match payloads.as_slice() {
            [Payload::Grib(b)] => {
                wx_pipeline::decode(&codec::inflate(b, codec::MAX_INFLATED)?, &wanted)
            }
            _ => {
                let mut grib = Vec::new();
                for p in &payloads {
                    if let Payload::Grib(b) = p {
                        grib.extend_from_slice(&codec::inflate(b, codec::MAX_INFLATED)?);
                    }
                }
                wx_pipeline::decode(&grib, &wanted)
            }
        }
        .context("decode")?;
        return Ok((set.fields, Some(set.valid_time)));
    }
    let mut fields = Vec::with_capacity(needs.len());
    let mut valid = None;
    for (n, p) in needs.iter().zip(payloads) {
        let (f, v) = decode_one(n, &p)?;
        valid = valid.or(v);
        fields.push(f);
    }
    Ok((fields, valid))
}

fn decode_one(need: &Need, payload: &Payload) -> anyhow::Result<(Field, Option<DateTime<Utc>>)> {
    match payload {
        Payload::Grib(b) => {
            let bytes = codec::inflate(b, codec::MAX_INFLATED)?;
            let wanted: Vec<FieldSel> = need.field.into_iter().collect();
            let mut set = wx_pipeline::decode(&bytes, &wanted).context("decode")?;
            ensure!(
                set.fields.len() == 1,
                "decoded {} fields for one",
                set.fields.len()
            );
            Ok((set.fields.remove(0), Some(set.valid_time)))
        }
        Payload::Custom { bytes, decode } => {
            let Decoded { field, valid } = decode(bytes, need).context("decode")?;
            ensure!(
                field.values.len() == field.nx * field.ny,
                "the decoder returned {} values for {} x {}",
                field.values.len(),
                field.nx,
                field.ny
            );
            Ok((field, valid))
        }
    }
}

/// Turn the run total `end` into the amount accumulated since `start` (zero
/// when `None`), per hour of the step. A cell missing in either is missing.
/// A small decrease (packing noise) is passed on: `derive` counts negative
/// amounts as nothing.
pub(crate) fn step_per_hour(
    end: &mut Field,
    start: Option<&Field>,
    hours: f64,
) -> anyhow::Result<()> {
    let per = (1.0 / hours) as f32;
    match start {
        None => end.values.iter_mut().for_each(|v| *v *= per),
        Some(s) => {
            ensure!(
                s.nx == end.nx && s.ny == end.ny && s.proj == end.proj,
                "the two accumulations are on different grids"
            );
            for (v, s) in end.values.iter_mut().zip(&s.values) {
                *v = (*v - *s) * per;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use wx_core::{layer, source};

    fn hrrr_job(layer_id: &str, lead: u32) -> Option<PartJob> {
        let l = layer::layer(layer_id).unwrap();
        let s = source::source("hrrr").unwrap();
        let run = crate::clock::floor(Utc::now(), Duration::hours(1));
        PartJob::forecast(
            l,
            l.input_for(s)?,
            s,
            run,
            lead,
            lead.checked_sub(60),
            Steps::RunTotals,
        )
    }

    #[test]
    fn run_totals_skip_the_first_step() {
        assert!(hrrr_job("snow", 0).is_none());
        let snow = hrrr_job("snow", 60).unwrap();
        assert_eq!(snow.needs().len(), 1);
        let temp = hrrr_job("temp", 0).unwrap();
        assert_eq!(temp.valid, temp.run.unwrap());
        let run = temp.run.unwrap();
        assert_eq!(temp.id, Part::forecast_id("hrrr", run, 0));
        assert_eq!(
            hrrr_job("radar", 75).unwrap().id,
            format!("hrrr-{}-75", run.format("%y%m%d%H"))
        );
        assert_eq!(hrrr_job("feels_like", 60).unwrap().needs().len(), 4);
    }

    #[tokio::test]
    async fn a_part_that_keeps_failing_is_given_up() {
        let mem = testing::remote(&[]);
        let (engine, store) = testing::engine(&mem);
        let job = hrrr_job("temp", 60).unwrap();
        let now = Utc::now();
        for attempt in 1..=MAX_ATTEMPTS {
            assert!(engine.wanted(&job, now), "attempt {attempt}");
            let bad = vec![Payload::Grib(bytes::Bytes::from_static(
                b"GRIB but not really",
            ))];
            let r = engine
                .produce(&job, bad, "test", Moment::starting(now))
                .await;
            assert!(matches!(r, Err(Trouble::File(_))), "{r:?}");
        }
        // The task moves on instead of failing on this part forever.
        assert!(!engine.wanted(&job, now));
        assert!(testing::keys(store.as_ref()).await.is_empty());
        assert!(engine.catalog.latest("temp", "hrrr").is_none());
    }

    /// A build the catalog refuses because it holds the very same part
    /// leaves that part's grid alone.
    #[tokio::test]
    async fn a_refused_duplicate_keeps_the_grid() {
        let mem = testing::remote(&[]);
        let (engine, store) = testing::engine(&mem);
        let job = hrrr_job("temp", 60).unwrap();
        let tmp = || {
            vec![Payload::Grib(
                testing::fixture("hrrr_t10z_f06__TMP_2mabove.grib2").into(),
            )]
        };
        let now = Moment::starting(Utc::now());
        assert!(matches!(
            engine.produce(&job, tmp(), "test", now).await,
            Ok(Produced::Inserted)
        ));
        // Caught before it is built: the stored grid is the held part's.
        assert!(matches!(
            engine.produce(&job, tmp(), "test", now).await,
            Ok(Produced::Unwanted)
        ));
        assert!(
            testing::keys(store.as_ref())
                .await
                .contains(&keys::grid("temp", &job.id))
        );
        let part = engine.catalog.latest("temp", "hrrr").unwrap();
        assert_eq!(part.id, job.id);
        assert_eq!(part.max_zoom, source::source("hrrr").unwrap().max_zoom);
        assert!(part.solid.is_some(), "a model grid has a solid box");
    }
}
