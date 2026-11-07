//! Sources that publish one file per observation time: what a family says
//! ([`Observed`]) and the driver that does the rest.
//!
//! One task follows one product. Each cycle lists the files valid since a
//! little before the newest one handled (or, on a cold start, since the
//! start of the loop window less the input's hold, since a part stands in
//! for the time steps of its hold after it), keeps them in a backlog by
//! valid time, and processes the newest few. With a [`Cadence::stride`] only one file per stride bucket becomes
//! a part (the earliest that works, so the clock-aligned one where it
//! exists); buckets that already have a part, or that started before the
//! loop window, are skipped. A file whose parts failed or had to wait stays
//! in the backlog and is tried again; one the catalog no longer wants is
//! forgotten. Polling follows the product's cadence: the task sleeps until
//! the next file should be out, then looks every [`Cadence::poll`] until it
//! is.
//!
//! Valid times are taken to the whole minute (truncated), which is what
//! part ids carry.
//!
//! A cycle handles its whole batch before it reports an error that stops
//! the task (a service failure, the store failing), and the task's state
//! (its backlog, how far it has got) outlives the restart.

use super::{Cx, Fetched, Need, Payload};
use crate::clock;
use crate::engine::{Engine, Moment, PartJob, Produced, Tally, Trouble};
use crate::remote::FetchError;
use chrono::{DateTime, Duration, Timelike, Utc};
use futures::StreamExt;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use wx_core::{FeedCtx, InputSpec, LayerSpec, SourceDef};

/// Files processed between listings while backfilling, so a file that
/// arrives mid-backfill is noticed within a batch.
const BATCH: usize = 4;
/// Files of one task fetched and built at once.
const FILES_IN_FLIGHT: usize = 2;
/// Parts of one file built at once.
const PARTS_IN_FLIGHT: usize = 2;
/// Files stamped further ahead of the clock than this are mislabelled. One
/// would move the listing position into the future (hiding real files until
/// the clock caught up) and, as an observation, retire every forecast part
/// up to its timestamp.
const FUTURE_SLACK: Duration = Duration::minutes(5);

/// How a product's files come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    /// Usual time between files.
    pub period: Duration,
    /// Usual delay between a file's valid time and its appearance.
    pub latency: Duration,
    /// Keep one part per this much time instead of every file.
    pub stride: Option<Duration>,
    /// How often to look while a file is due.
    pub poll: Duration,
}

impl Cadence {
    /// Files every `period`, each appearing `latency` after its time, looked
    /// for every `poll` while one is due.
    pub fn every(period: Duration, latency: Duration, poll: Duration) -> Self {
        Self {
            period,
            latency,
            stride: None,
            poll,
        }
    }
}

/// One listed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObsFile {
    /// What the data describes (the driver truncates it to the minute).
    pub valid: DateTime<Utc>,
    /// The family's name for it: a key or a URL.
    pub name: String,
    pub size: Option<u64>,
}

/// What an observation family tells the driver. See the module docs, and
/// `families::mrms` for an example.
#[async_trait::async_trait]
pub trait Observed: Send + Sync + 'static {
    /// Whether this adapter can serve `source` (it knows its region).
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        let _ = source;
        Ok(())
    }

    /// The task following `product`, whose first layer is `first_layer`.
    /// Default `{source}.{first layer}`: `mrms.radar`, `mrms_ak.rain_1h`.
    fn task_name(&self, source: &SourceDef, product: &str, first_layer: &str) -> String {
        let _ = product;
        format!("{}.{first_layer}", source.id)
    }

    fn cadence(&self, source: &SourceDef, product: &str) -> Cadence;

    /// Files of `product` valid at or after `from` (more are fine; the
    /// driver filters), in any order.
    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        product: &str,
        from: DateTime<Utc>,
    ) -> Result<Vec<ObsFile>, FetchError>;

    /// The payload of each need, all in `file`.
    async fn fetch(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        product: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOutcome {
    /// The file is not there after all.
    Missing,
    Done(Tally),
}

/// What one cycle did.
pub struct Report {
    pub tally: Tally,
    /// Files still waiting in the backlog.
    pub remaining: usize,
}

/// One supervised task: one product of one source.
pub struct ObservedTask {
    engine: Arc<Engine>,
    family: Arc<dyn Observed>,
    source: &'static SourceDef,
    pub name: String,
    product: &'static str,
    cadence: Cadence,
    inputs: Vec<(&'static LayerSpec, &'static InputSpec)>,
    /// Listed files not yet handled, by valid time.
    backlog: BTreeMap<DateTime<Utc>, ObsFile>,
    /// Newest valid time handled (produced, or found not worth producing).
    /// Listing resumes a little before it.
    handled_through: Option<DateTime<Utc>>,
    /// Stride buckets this task has made a part for. The catalog says the
    /// same until the part expires; this keeps an expiring bucket from being
    /// refilled with a later file.
    taken: BTreeSet<DateTime<Utc>>,
}

impl ObservedTask {
    pub fn new(
        engine: Arc<Engine>,
        family: Arc<dyn Observed>,
        source: &'static SourceDef,
        product: &'static str,
    ) -> Self {
        let inputs: Vec<_> = super::inputs_of(engine.layers, source)
            .into_iter()
            .filter(|(_, i)| i.product == product)
            .collect();
        let first = inputs.first().map_or(product, |(l, _)| l.id);
        Self {
            name: family.task_name(source, product, first),
            cadence: family.cadence(source, product),
            engine,
            family,
            source,
            product,
            inputs,
            backlog: BTreeMap::new(),
            handled_through: None,
            taken: BTreeSet::new(),
        }
    }

    /// How far back a file can still be wanted: the catalog keeps an
    /// observed part until `valid + hold <= now - window`, and it stands in
    /// for the time steps of its hold, so the oldest useful file is the
    /// window and the hold before now.
    fn reach(&self) -> Duration {
        let minutes = self
            .inputs
            .iter()
            .map(|(l, i)| l.retention_minutes + i.hold_minutes)
            .max()
            .unwrap_or(0);
        Duration::minutes(minutes as i64)
    }

    /// How old the newest file may get before the task counts as stale: four
    /// of the product's intervals and its latency.
    pub fn max_data_age(&self) -> Duration {
        self.cadence.stride.unwrap_or(self.cadence.period) * 4 + self.cadence.latency
    }

    fn jobs(&self, valid: DateTime<Utc>) -> Vec<PartJob> {
        self.inputs
            .iter()
            .map(|(l, i)| PartJob::observed(l, i, self.source, valid))
            .collect()
    }

    async fn list(
        &mut self,
        now: DateTime<Utc>,
        window_start: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        // Re-list a little behind the newest file handled so one that lands
        // a moment out of order is not skipped. Capped at now, so a clock
        // that stepped back does not hide files.
        let overlap = self.cadence.period * 2;
        let from = self
            .handled_through
            .map_or(window_start, |t| (t.min(now) - overlap).max(window_start));
        let cx = Cx::new(&self.engine.remote, &self.name, now);
        let listed = self
            .family
            .list(&cx, self.source, self.product, from)
            .await?;
        for mut f in listed {
            let Some(valid) = f.valid.with_second(0).and_then(|t| t.with_nanosecond(0)) else {
                continue;
            };
            if valid < window_start || valid > now + FUTURE_SLACK {
                continue;
            }
            f.valid = valid;
            self.backlog.entry(valid).or_insert(f);
        }
        Ok(())
    }

    /// A file is done with: it leaves the backlog.
    fn handled(&mut self, valid: DateTime<Utc>) {
        self.backlog.remove(&valid);
        self.handled_through = Some(self.handled_through.map_or(valid, |t| t.max(valid)));
    }

    /// The newest file known, handled or not, for timing the next poll.
    fn newest_known(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let listed = self.backlog.keys().next_back().copied();
        listed.max(self.handled_through).map(|t| t.min(now))
    }

    /// Stride buckets that already have a part on every layer.
    fn filled(&self) -> BTreeSet<DateTime<Utc>> {
        let Some(stride) = self.cadence.stride else {
            return BTreeSet::new();
        };
        let mut filled: Option<BTreeSet<DateTime<Utc>>> = None;
        for (layer, _) in &self.inputs {
            let buckets: BTreeSet<_> = self
                .engine
                .catalog
                .shared_timeline(layer.id)
                .parts
                .iter()
                .filter(|p| p.source == self.source.id)
                .map(|p| clock::floor(p.valid, stride))
                .collect();
            filled = Some(match filled {
                None => buckets,
                Some(prev) => prev.intersection(&buckets).copied().collect(),
            });
        }
        let mut filled = filled.unwrap_or_default();
        filled.extend(self.taken.iter().copied());
        filled
    }

    pub async fn cycle(&mut self, now: DateTime<Utc>) -> anyhow::Result<Report> {
        let window_start = now - self.reach();
        self.list(now, window_start).await?;
        self.backlog.retain(|v, _| *v >= window_start);
        let stride = self.cadence.stride;
        if let Some(s) = stride {
            self.taken.retain(|b| *b + s >= window_start);
        }
        let filled = self.filled();
        let (picks, forget) = pick(&self.backlog, stride, &filled, window_start, |v| {
            self.jobs(v).iter().any(|j| self.engine.wanted(j, now))
        });
        for v in forget {
            self.handled(v);
        }
        let batch: Vec<(DateTime<Utc>, ObsFile, Vec<PartJob>)> = picks
            .iter()
            .take(BATCH)
            .filter_map(|v| {
                let file = self.backlog.get(v)?.clone();
                let jobs = self
                    .jobs(*v)
                    .into_iter()
                    .filter(|j| self.engine.wanted(j, now))
                    .collect();
                Some((*v, file, jobs))
            })
            .collect();
        let remaining = picks.len().saturating_sub(batch.len());
        let this = &*self;
        let moment = Moment::starting(now);
        let results: Vec<(DateTime<Utc>, anyhow::Result<FileOutcome>)> =
            futures::stream::iter(batch)
                .map(|(valid, file, jobs)| async move {
                    (valid, this.produce_file(&file, jobs, moment).await)
                })
                .buffer_unordered(FILES_IN_FLIGHT)
                .collect()
                .await;

        let mut tally = Tally::default();
        let mut first_error = None;
        for (valid, r) in results {
            match r {
                // A file with parts that failed or had to wait stays in the
                // backlog: it is tried again (failed parts until they are
                // given up, then it is forgotten).
                Ok(FileOutcome::Done(t)) if t.failed > 0 || t.busy > 0 => tally += t,
                Ok(FileOutcome::Done(t)) => {
                    tally += t;
                    self.handled(valid);
                    if let (Some(s), true) = (stride, t.produced > 0) {
                        self.taken.insert(clock::floor(valid, s));
                    }
                }
                Ok(FileOutcome::Missing) => self.handled(valid),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(Report { tally, remaining }),
        }
    }

    /// Fetch one file and build every part it feeds. A broken file is
    /// charged to its parts; only errors that stop the task are returned.
    async fn produce_file(
        &self,
        file: &ObsFile,
        jobs: Vec<PartJob>,
        moment: Moment,
    ) -> anyhow::Result<FileOutcome> {
        let now = moment.now();
        if jobs.is_empty() {
            return Ok(FileOutcome::Done(Tally::default()));
        }
        let mut needs: Vec<Need> = Vec::new();
        let mut spans = Vec::with_capacity(jobs.len());
        for j in &jobs {
            let n = j.needs();
            spans.push(needs.len()..needs.len() + n.len());
            needs.extend(n);
        }
        let cx = Cx::new(&self.engine.remote, &self.name, now);
        let mut tally = Tally::default();
        let e = match self
            .family
            .fetch(&cx, self.source, self.product, file, &needs)
            .await
        {
            Ok(Fetched::Missing) => return Ok(FileOutcome::Missing),
            Ok(Fetched::Got(p)) if p.len() == needs.len() => {
                return self.produce(file, jobs, spans, p, moment, tally).await;
            }
            Ok(Fetched::Got(p)) => FetchError::object(
                &file.name,
                format!("{} payloads for {} needs", p.len(), needs.len()),
            ),
            Err(e) => e,
        };
        let refs: Vec<&PartJob> = jobs.iter().collect();
        tally.failed += self.engine.charge(&file.name, &refs, Trouble::from(e))?;
        Ok(FileOutcome::Done(tally))
    }

    /// Build every part of `jobs` whose payloads (at `spans` of `payloads`)
    /// all came.
    async fn produce(
        &self,
        file: &ObsFile,
        jobs: Vec<PartJob>,
        spans: Vec<std::ops::Range<usize>>,
        payloads: Vec<Option<Payload>>,
        moment: Moment,
        mut tally: Tally,
    ) -> anyhow::Result<FileOutcome> {
        let mut work: Vec<(PartJob, Vec<Payload>)> = Vec::new();
        for (job, span) in jobs.into_iter().zip(spans) {
            match payloads[span]
                .iter()
                .cloned()
                .collect::<Option<Vec<Payload>>>()
            {
                Some(p) => work.push((job, p)),
                None => {
                    self.engine.unavailable(&file.name, &job);
                    tally.unavailable += 1;
                }
            }
        }
        drop(payloads);
        let results: Vec<Result<Produced, Trouble>> = futures::stream::iter(work)
            .map(|(job, p)| async move { self.engine.produce(&job, p, &self.name, moment).await })
            .buffer_unordered(PARTS_IN_FLIGHT)
            .collect()
            .await;
        let mut stop = None;
        for r in results {
            if let Some(e) = tally.count(&self.name, r) {
                stop.get_or_insert(e);
            }
        }
        match stop {
            Some(e) => Err(e),
            None => Ok(FileOutcome::Done(tally)),
        }
    }

    pub(crate) fn summary(
        &self,
        added: usize,
        remaining: usize,
    ) -> (Option<DateTime<Utc>>, String) {
        let first = self.inputs.first().map(|(l, _)| l.id);
        let newest = first
            .and_then(|l| self.engine.catalog.latest(l, self.source.id))
            .map(|p| p.valid);
        let parts = first.map_or(0, |l| {
            self.engine
                .catalog
                .shared_timeline(l)
                .parts
                .iter()
                .filter(|p| p.source == self.source.id)
                .count()
        });
        let mut detail = match newest {
            Some(t) => format!("newest file {}, {parts} parts", t.format("%d %H:%MZ")),
            None => "no file yet".to_string(),
        };
        if added > 0 {
            detail.push_str(&format!(", {added} new"));
        }
        if remaining > 0 {
            detail.push_str(&format!(", backfilling ({remaining} to go)"));
        }
        (newest, detail)
    }
}

/// Which listed files to process, newest first, and which to forget.
///
/// Without a stride every wanted file is taken. With one, each stride bucket
/// gets its earliest wanted file (the clock-aligned one when it exists), and
/// buckets that already have a part, or that started before the loop window
/// (their part would expire within minutes), get nothing.
pub(crate) fn pick(
    backlog: &BTreeMap<DateTime<Utc>, ObsFile>,
    stride: Option<Duration>,
    filled: &BTreeSet<DateTime<Utc>>,
    window_start: DateTime<Utc>,
    wanted: impl Fn(DateTime<Utc>) -> bool,
) -> (Vec<DateTime<Utc>>, Vec<DateTime<Utc>>) {
    let mut picks = Vec::new();
    let mut forget = Vec::new();
    let mut picked_buckets = BTreeSet::new();
    for &valid in backlog.keys() {
        let bucket = stride.map(|s| clock::floor(valid, s));
        let skip_bucket = bucket.is_some_and(|b| filled.contains(&b) || b < window_start);
        if skip_bucket || !wanted(valid) {
            forget.push(valid);
            continue;
        }
        match bucket {
            // A later file in a bucket that already has a pick waits: it is
            // the fallback if the pick turns out to be unusable.
            Some(b) if !picked_buckets.insert(b) => {}
            _ => picks.push(valid),
        }
    }
    picks.reverse();
    (picks, forget)
}

/// When the next file worth having should appear.
pub(crate) fn next_due(
    cadence: &Cadence,
    newest_listed: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let newest = newest_listed?;
    let next_valid = match cadence.stride {
        Some(s) => clock::floor(newest, s) + s,
        None => newest + cadence.period,
    };
    Some(next_valid + cadence.latency)
}

/// The supervised task. Its state is held across restarts.
pub async fn run(task: Arc<tokio::sync::Mutex<ObservedTask>>, ctx: FeedCtx) -> anyhow::Result<()> {
    let mut task = task.lock_owned().await;
    let cadence = task.cadence;
    ctx.expect_data_within(task.max_data_age().to_std().ok());
    let mut due: Option<DateTime<Utc>> = None;
    loop {
        let now = Utc::now();
        let mut remaining = 0;
        let mut added = 0;
        if due.is_none_or(|d| now >= d) {
            let c = task.cycle(now).await?;
            remaining = c.remaining;
            added = c.tally.produced;
            due = if remaining > 0 {
                None
            } else {
                next_due(&cadence, task.newest_known(now))
            };
        }
        let (newest, detail) = task.summary(added, remaining);
        ctx.ok(newest, detail);
        let pause = match (remaining, due) {
            (0, Some(d)) if d > now => {
                clock::wait(now, d, Duration::seconds(1), Duration::minutes(10))
            }
            // Overdue: look every `poll`, less often once it is clearly an
            // outage.
            (0, Some(d)) if now - d > Duration::hours(1) => cadence
                .poll
                .max(Duration::minutes(5))
                .to_std()
                .unwrap_or_default(),
            (0, Some(d)) if now - d > Duration::minutes(10) => cadence
                .poll
                .max(Duration::seconds(60))
                .to_std()
                .unwrap_or_default(),
            (0, _) => cadence.poll.to_std().unwrap_or_default(),
            _ => std::time::Duration::ZERO,
        };
        clock::pause(&ctx, pause).await;
    }
}
