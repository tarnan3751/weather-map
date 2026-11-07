//! Sources that publish model runs: what a family says ([`Model`]) and the
//! driver that does the rest.
//!
//! # One cycle
//!
//! Runs are taken newest first, from the newest one that has started (its
//! run time has passed) back far enough to cover the past part of every
//! timeline the source feeds (a `fill_past` input's window and hold), plus
//! [`Cycle::spare`] more so a late or failed run is covered by an older one.
//! For each run, every lead of every input whose valid time is at most
//! `horizon_hours` ahead is offered to the catalog (`wants`), which turns
//! away what it holds already, what is expired, what an anchored layer will
//! not take, and so on; recent past hours of `fill_past` inputs are wanted
//! like any other, from the newest run that has them. Parts needing a field
//! the family declares absent ([`Model::absent`]) are not made.
//!
//! The wanted parts' needs are grouped by file. The run is listed (at most
//! every [`Cycle::relist`]) when a file it needs should be out by now and is
//! not in the last listing; a file more than [`Cycle::give_up`] overdue no
//! longer causes a listing (unless a fetch found it missing less than that
//! ago), except for the newest run whose files are due, which is listed
//! again every [`LATE_RELIST`] for as long as it is wanted: a late run is
//! never given up for good. Every run is listed at least once. A part
//! touches none of its files until all of them are listed.
//!
//! Files are fetched in two passes over the runs: first those holding a
//! valid time up to now, run by run, then the rest. On a cold start or after
//! an outage the past part of every timeline is thus filled from the older
//! runs before the newest run's far leads. Each run is planned once per
//! cycle, so a part whose step starts in a file of the first pass and ends
//! in one of the second keeps what the first fetched, and every file is
//! fetched once. Each run is judged by the time it is reached, and each
//! file's parts again just before it is fetched (and each part when its
//! build slot comes up), so a long cycle does not fetch or build what has
//! expired meanwhile. A part is built as soon as all its needs have arrived;
//! one run's builds finish before the next run is looked at.
//!
//! A file the service cannot serve (after the remote layer's retries) is
//! missing for now: its parts wait, and the rest of the run and the older
//! runs still get their turn. The task fails, and the supervisor backs off,
//! only when a listing fails or [`SERVICE_FAILURES_IN_A_ROW`] files fail in
//! a row. Its state (listings, what went missing) outlives the restart.
//!
//! The task then sleeps until the earliest of: the next missing file's due
//! time, the next run's first file, a relist interval when something is
//! overdue, and ten minutes. It reports the newest run it has delivered as
//! its data time, and asks to be called stale when that run is older than a
//! cycle, the run's latency and `give_up` together ([`ModelTask::max_data_age`]).

use super::{Cx, Fetched, Need, Payload};
use crate::clock;
use crate::engine::{Engine, Moment, PartJob, Produced, Tally, Trouble};
use crate::remote::{Failure, FetchError, Listed};
use chrono::{DateTime, Duration, Utc};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use tokio::task::JoinSet;
use wx_core::frame::FORECAST_GRACE_MINUTES;
use wx_core::{FeedCtx, InputSpec, LayerSpec, SourceDef};

/// Look for a file this long before it is due.
const EARLY: Duration = Duration::minutes(10);
/// Longest sleep between cycles, so new runs and expiring parts are noticed.
const MAX_SLEEP: Duration = Duration::minutes(10);
/// Parts of one task being built at once (their payloads are held until the
/// build is done). The shared build limit applies on top.
const BUILDS_IN_FLIGHT: usize = 4;

/// When runs start and how persistently they are followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cycle {
    /// Runs start on multiples of this since midnight UTC.
    pub every: Duration,
    /// Runs older than the past window that are still looked at.
    pub spare: u32,
    /// Least time between two listings of one run.
    pub relist: Duration,
    /// A file this long past due no longer causes a listing.
    pub give_up: Duration,
}

impl Cycle {
    /// Run times to look at, newest first: from the newest that has started
    /// back to `past` before now and [`Cycle::spare`] runs beyond.
    pub fn runs(&self, now: DateTime<Utc>, past: Duration) -> Vec<DateTime<Utc>> {
        let every = self.every.max(Duration::minutes(1));
        let oldest = now - past - every * (self.spare as i32 + 1);
        let mut out = Vec::new();
        let mut r = clock::floor(now, every);
        while r >= oldest && out.len() < 2000 {
            out.push(r);
            r -= every;
        }
        out
    }
}

/// What of a run exists: file names (as [`Model::file`] gives them) and
/// their sizes when known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    files: HashMap<String, Option<u64>>,
}

impl Listing {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, name: impl Into<String>, size: Option<u64>) {
        self.files.insert(name.into(), size);
    }

    pub fn contains(&self, name: &str) -> bool {
        self.files.contains_key(name)
    }

    /// `None` when absent; `Some(None)` when present with no known size.
    pub fn size(&self, name: &str) -> Option<Option<u64>> {
        self.files.get(name).copied()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    fn remove(&mut self, name: &str) {
        self.files.remove(name);
    }

    /// The keys of an object listing that have their index next to them,
    /// with their sizes (see [`common::with_sidecars`]).
    ///
    /// [`common::with_sidecars`]: crate::families::common::with_sidecars
    pub fn with_sidecars(listed: &[Listed], sidecar: &str) -> Self {
        let mut out = Self::new();
        for l in crate::families::common::with_sidecars(listed, sidecar) {
            out.add(l.key.clone(), Some(l.size));
        }
        out
    }
}

/// How a product's files hold [`Stat::StepPerHour`] fields. See "Step
/// accumulations" in the `driver` docs.
///
/// [`Stat::StepPerHour`]: wx_core::layer::Stat::StepPerHour
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Steps {
    /// Accumulations from the start of the run only: the driver differences
    /// two run totals.
    #[default]
    RunTotals,
    /// The record for the hour ending at the lead: asked for as it is.
    Windows,
}

/// A file to fetch: its name as [`Model::file`] gave it, and its size when
/// the listing said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub name: String,
    pub size: Option<u64>,
}

/// What a run-based family tells the driver. See the module docs for what
/// the driver does with it, and `families` for examples.
#[async_trait::async_trait]
pub trait Model: Send + Sync + 'static {
    /// Whether this adapter can serve `source` (it knows its region).
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        let _ = source;
        Ok(())
    }

    fn cycle(&self, source: &SourceDef) -> Cycle;

    /// How `product`'s files hold step accumulations. Default: run totals.
    fn steps(&self, source: &SourceDef, product: &str) -> Steps {
        let _ = (source, product);
        Steps::RunTotals
    }

    /// Lead times in minutes that `product` has in `run`, ascending. The
    /// step of a step accumulation runs from the previous entry.
    fn leads(&self, source: &SourceDef, product: &str, run: DateTime<Utc>) -> Vec<u32>;

    /// When the file holding `lead` of `product` in `run` usually appears.
    fn due(
        &self,
        source: &SourceDef,
        product: &str,
        run: DateTime<Utc>,
        lead: u32,
    ) -> DateTime<Utc>;

    /// The file that holds `need` in `run`. Needs with the same file are
    /// fetched together; the name is also what [`Model::list`] reports.
    fn file(&self, source: &SourceDef, run: DateTime<Utc>, need: &Need) -> String;

    /// What of `run` exists now, for the task following `products`.
    /// `Ok(None)`: this family does not list, and files are fetched once
    /// due (a missing one is tried again after [`Cycle::relist`]).
    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        run: DateTime<Utc>,
        products: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let _ = (cx, source, run, products);
        Ok(None)
    }

    /// The payload of each need, all in `file`.
    async fn fetch(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        run: DateTime<Utc>,
        file: &ModelFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError>;

    /// Whether `need` is known not to exist in `run`: a field a product's
    /// first step never holds (HRDPS's rates, AROME's gust and accumulations
    /// at step 0). The driver makes no part that needs it, so it is neither
    /// asked for nor logged. A need a file lacks all the same is a surprise,
    /// and is logged as one. Default: nothing is known to be absent.
    fn absent(&self, source: &SourceDef, run: DateTime<Utc>, need: &Need) -> bool {
        let _ = (source, run, need);
        false
    }
}

/// A late run is listed again this often once every file it waits for is
/// past [`Cycle::give_up`], for as long as it is the newest run whose files
/// are due: a run that lands hours late is still picked up, and never needs
/// a restart to be found.
pub const LATE_RELIST: Duration = Duration::minutes(20);
/// Files in a row whose fetch fails with a service error (after the remote
/// layer's own retries) before the task fails and the supervisor backs off.
/// One object that keeps answering 5xx is passed over as missing for now; a
/// provider that is down stops the task.
pub const SERVICE_FAILURES_IN_A_ROW: u32 = 5;

#[derive(Debug, Default)]
struct RunState {
    listing: Option<Listing>,
    listed_at: Option<DateTime<Utc>>,
    /// The family does not list.
    unlisted: bool,
    /// Files a fetch found missing or could not reach, and when.
    missing_at: HashMap<String, DateTime<Utc>>,
    /// The newest run total fetched of each step field, kept while some part
    /// of the run is wanted: the next lead's step starts there, so when that
    /// lead lands in a later cycle its start is not fetched again. One
    /// message per step field and run (DMI's two are about 18 MB).
    totals: Vec<Total>,
}

/// A run total kept for the step that starts at it.
#[derive(Debug)]
struct Total {
    file: String,
    need: Need,
    payload: Payload,
}

impl RunState {
    /// The kept total that is `need` in `file`.
    fn total(&self, file: &str, need: &Need) -> Option<Payload> {
        let t = self
            .totals
            .iter()
            .find(|t| t.file == file && t.need.same_as(need))?;
        Some(t.payload.clone())
    }

    /// Keep `payload` as the newest total of its field, unless a later one
    /// is kept already.
    fn keep_total(&mut self, file: &str, need: &Need, payload: &Payload) {
        let same_field =
            |t: &Total| t.need.product() == need.product() && t.need.field == need.field;
        if self
            .totals
            .iter()
            .any(|t| same_field(t) && t.need.lead >= need.lead)
        {
            return;
        }
        self.totals.retain(|t| !same_field(t));
        self.totals.push(Total {
            file: file.to_string(),
            need: *need,
            payload: payload.detached(),
        });
    }
}

/// Which files one pass over the runs fetches. Files holding a valid time
/// up to now go first, so on a cold start (or after an outage) the past part
/// of every timeline fills from the older runs before the newest run's far
/// leads are fetched. A part goes with the file of its own lead, so a file
/// holding many leads (a sub-hourly file, a nowcast archive) is still
/// fetched once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    UpToNow,
    Ahead,
}

struct FileWork {
    name: String,
    /// Each need once: parts asking for the same field at the same lead
    /// share its payload.
    needs: Vec<Need>,
    /// `(job, slot, need)`: the payload slot of a part each need fills.
    users: Vec<(usize, usize, usize)>,
    first_lead: u32,
    due: DateTime<Utc>,
    /// It holds a valid time up to now, so it goes in the first pass.
    early: bool,
    /// Listed (or, for a family that does not list, due), and its size.
    present: bool,
    size: Option<u64>,
}

struct JobState {
    job: PartJob,
    payloads: Vec<Option<Payload>>,
    /// Built, given up, failed or no longer wanted: nothing more to do this
    /// cycle.
    done: bool,
}

/// One run's work for a cycle: the wanted parts, the files they need and
/// where each stands. Made once per cycle, so parts whose files fall in
/// both passes (a step starting before now and ending after) keep what the
/// first pass fetched, and every file is fetched once.
struct Plan {
    run: DateTime<Utc>,
    jobs: Vec<JobState>,
    files: Vec<FileWork>,
    /// When this run next wants a look.
    next: Option<DateTime<Utc>>,
    /// Some of its wanted files should be out by now.
    due: bool,
}

fn soon(next: &mut Option<DateTime<Utc>>, t: DateTime<Utc>) {
    *next = Some(next.map_or(t, |n| n.min(t)));
}

/// What one cycle did.
#[derive(Debug, Clone)]
pub struct Report {
    pub tally: Tally,
    /// When to look again.
    pub wake: DateTime<Utc>,
    /// The newest run with wanted files that should be out by now.
    pub newest_due: Option<DateTime<Utc>>,
}

/// One supervised task: one source, named after it, and the products it
/// follows. Its state (what each run listed, what went missing) outlives a
/// restart of the task.
pub struct ModelTask {
    engine: Arc<Engine>,
    family: Arc<dyn Model>,
    source: &'static SourceDef,
    name: String,
    products: Vec<&'static str>,
    inputs: Vec<(&'static LayerSpec, &'static InputSpec)>,
    runs: BTreeMap<DateTime<Utc>, RunState>,
    /// Files in a row whose fetch failed with a service error.
    service_failures: u32,
}

/// Hears `(data time, detail)` as a task makes progress within a cycle.
pub type Progress<'a> = dyn Fn(DateTime<Utc>, String) + Send + Sync + 'a;

impl ModelTask {
    pub fn new(
        engine: Arc<Engine>,
        family: Arc<dyn Model>,
        source: &'static SourceDef,
        products: &[&'static str],
    ) -> Self {
        let inputs = super::inputs_of(engine.layers, source)
            .into_iter()
            .filter(|(_, i)| products.contains(&i.product))
            .collect();
        Self {
            engine,
            family,
            source,
            name: source.id.to_string(),
            products: products.to_vec(),
            inputs,
            runs: BTreeMap::new(),
            service_failures: 0,
        }
    }

    /// How far back valid times may still be wanted: a `fill_past` part
    /// stays until it can no longer stand in for any time step of the
    /// window (its hold after it), other forecast parts for the grace.
    fn past(&self) -> Duration {
        self.inputs
            .iter()
            .map(|(l, i)| {
                if i.fill_past {
                    Duration::minutes(l.retention_minutes as i64 + i.hold_minutes as i64)
                } else {
                    Duration::minutes(FORECAST_GRACE_MINUTES)
                }
            })
            .max()
            .unwrap_or_default()
    }

    /// The parts of `run` the catalog wants now. Parts needing something the
    /// family knows is absent are not made.
    #[cfg(test)]
    pub fn wanted(&self, run: DateTime<Utc>, now: DateTime<Utc>) -> Vec<PartJob> {
        self.wanted_with_needs(run, now)
            .into_iter()
            .map(|(job, _)| job)
            .collect()
    }

    /// [`ModelTask::wanted`], each part with its needs. The catalog is asked
    /// first, so a part it holds costs nothing more.
    fn wanted_with_needs(
        &self,
        run: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Vec<(PartJob, Vec<Need>)> {
        let horizon = now + Duration::hours(self.engine.config.horizon_hours as i64);
        let mut by_product: Vec<(&str, Vec<u32>)> = Vec::new();
        let mut jobs = Vec::new();
        for (layer, input) in &self.inputs {
            let k = match by_product.iter().position(|(p, _)| *p == input.product) {
                Some(k) => k,
                None => {
                    by_product.push((
                        input.product,
                        self.family.leads(self.source, input.product, run),
                    ));
                    by_product.len() - 1
                }
            };
            let leads = &by_product[k].1;
            let steps = self.family.steps(self.source, input.product);
            for (k, &lead) in leads.iter().enumerate() {
                let valid = run + Duration::minutes(lead as i64);
                if valid > horizon {
                    break;
                }
                if !self.engine.takes(layer, self.source, valid, Some(run), now) {
                    continue;
                }
                let previous = k.checked_sub(1).map(|p| leads[p]);
                let Some(job) =
                    PartJob::forecast(layer, input, self.source, run, lead, previous, steps)
                else {
                    continue;
                };
                let needs = job.needs();
                if needs
                    .iter()
                    .any(|n| self.family.absent(self.source, run, n))
                    || !self.engine.workable(&job)
                {
                    continue;
                }
                jobs.push((job, needs));
            }
        }
        jobs
    }

    /// Run totals kept for steps that start at them, over every run.
    #[cfg(test)]
    pub fn kept_totals(&self) -> usize {
        self.runs.values().map(|s| s.totals.len()).sum()
    }

    /// One pass over every run worth looking at, newest first, then a
    /// second for what is left: the first plans each run (and lists it when
    /// it needs a listing) and fetches the files that hold a valid time up
    /// to now, the second the rest. Each run is judged at the time it is
    /// reached, and each file again just before it is fetched, so a long
    /// backfill does not fetch what expired while it ran.
    pub async fn cycle(
        &mut self,
        now: DateTime<Utc>,
        progress: &Progress<'_>,
    ) -> anyhow::Result<Report> {
        let cycle = self.family.cycle(self.source);
        let moment = Moment::starting(now);
        let runs = cycle.runs(now, self.past());
        self.runs.retain(|r, _| runs.contains(r));
        let mut tally = Tally::default();
        let mut wake = now + MAX_SLEEP;
        // The next run's first file.
        if let Some(next) = runs.first().map(|r| *r + cycle.every) {
            for (_, input) in &self.inputs {
                if let Some(first) = self.family.leads(self.source, input.product, next).first() {
                    wake = wake.min(self.family.due(self.source, input.product, next, *first));
                }
            }
        }
        let mut newest_due: Option<DateTime<Utc>> = None;
        let mut plans: Vec<Plan> = Vec::new();
        for &run in &runs {
            let newest = newest_due.is_none_or(|d| d == run);
            let Some(mut plan) = self.plan(run, moment, &cycle, newest).await? else {
                continue;
            };
            if plan.due && newest_due.is_none() {
                newest_due = Some(run);
            }
            tally += self
                .execute(&mut plan, moment, &cycle, Pass::UpToNow, progress)
                .await?;
            plans.push(plan);
        }
        for plan in &mut plans {
            tally += self
                .execute(plan, moment, &cycle, Pass::Ahead, progress)
                .await?;
            if let Some(w) = plan.next {
                wake = wake.min(w);
            }
        }
        if tally.busy > 0 || tally.failed > 0 {
            wake = wake.min(now + Duration::minutes(1));
        }
        Ok(Report {
            tally,
            wake: wake.max(now + Duration::seconds(5)),
            newest_due,
        })
    }

    /// The newest run with a file listed, and how many it had.
    #[cfg(test)]
    pub fn newest_listed(&self) -> Option<(DateTime<Utc>, usize)> {
        let mut runs = self.runs.iter().rev();
        runs.find_map(|(r, s)| {
            s.listing
                .as_ref()
                .filter(|l| !l.is_empty())
                .map(|l| (*r, l.len()))
        })
    }

    /// What `run` can give now: its wanted parts and their files, with the
    /// run listed first when a file it needs should be out and is not in the
    /// last listing. `newest`: no newer run has wanted files that are due,
    /// so if this one is late it is still listed now and then past
    /// [`Cycle::give_up`]. `None` when nothing of it is wanted.
    async fn plan(
        &mut self,
        run: DateTime<Utc>,
        moment: Moment,
        cycle: &Cycle,
        newest: bool,
    ) -> anyhow::Result<Option<Plan>> {
        let now = moment.now();
        let wanted = self.wanted_with_needs(run, now);
        let state = self.runs.entry(run).or_default();
        if wanted.is_empty() {
            // Nothing of the run is wanted, so no step will start at a total.
            state.totals.clear();
            return Ok(None);
        }
        let mut jobs = Vec::with_capacity(wanted.len());
        let mut files: Vec<FileWork> = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        for (j, (job, needs)) in wanted.into_iter().enumerate() {
            let mut payloads = vec![None; needs.len()];
            for (slot, need) in needs.into_iter().enumerate() {
                let name = self.family.file(self.source, run, &need);
                if need.lead < job.lead
                    && let Some(p) = state.total(&name, &need)
                {
                    payloads[slot] = Some(p);
                    continue;
                }
                let due = self.family.due(self.source, need.product(), run, need.lead);
                let k = if let Some(&k) = by_name.get(&name) {
                    k
                } else {
                    by_name.insert(name.clone(), files.len());
                    files.push(FileWork {
                        name,
                        needs: Vec::new(),
                        users: Vec::new(),
                        first_lead: need.lead,
                        due,
                        early: false,
                        present: false,
                        size: None,
                    });
                    files.len() - 1
                };
                let f = &mut files[k];
                // The part's own lead is its first need.
                f.early |= slot == 0 && job.valid <= now;
                f.first_lead = f.first_lead.min(need.lead);
                f.due = f.due.min(due);
                let n = match f.needs.iter().position(|m| m.same_as(&need)) {
                    Some(n) => n,
                    None => {
                        f.needs.push(need);
                        f.needs.len() - 1
                    }
                };
                f.users.push((j, slot, n));
            }
            jobs.push(JobState {
                job,
                payloads,
                done: false,
            });
        }
        files.sort_by(|a, b| (a.first_lead, &a.name).cmp(&(b.first_lead, &b.name)));
        let due = files.iter().any(|f| f.due - EARLY <= now);

        let source = self.source;
        let late_relist = LATE_RELIST.max(cycle.relist);
        let missed = |s: &RunState, name: &str| {
            s.missing_at
                .get(name)
                .is_some_and(|t| now - *t <= cycle.give_up)
        };
        let overdue: Vec<&FileWork> = files
            .iter()
            .filter(|f| {
                f.due - EARLY <= now && !state.listing.as_ref().is_some_and(|l| l.contains(&f.name))
            })
            .collect();
        let never = state.listed_at.is_none();
        let since = |t: Duration| state.listed_at.is_none_or(|l| now - l >= t);
        // Due and not given up; or listed before and found missing when
        // fetched (an inventory not written yet), for as long again; or, for
        // the newest run that should be out, late but not forgotten.
        let waiting = overdue
            .iter()
            .any(|f| never || now <= f.due + cycle.give_up || missed(state, &f.name));
        let late = newest && !overdue.is_empty();
        let list =
            !state.unlisted && ((waiting && since(cycle.relist)) || (late && since(late_relist)));
        if list {
            let cx = Cx::new(&self.engine.remote, &self.name, now);
            match self.family.list(&cx, source, run, &self.products).await {
                Ok(Some(l)) => state.listing = Some(l),
                Ok(None) => state.unlisted = true,
                Err(e) => {
                    return Err(
                        anyhow::Error::new(e).context(format!("listing run {}", label(run)))
                    );
                }
            }
            state.listed_at = Some(now);
        }

        // Where each file stands. A part touches none of its files until all
        // of them are there, so a step's start is not fetched while its end
        // is still to come.
        let mut next = None;
        for fw in &mut files {
            (fw.present, fw.size) = if state.unlisted {
                let retry = state
                    .missing_at
                    .get(&fw.name)
                    .is_none_or(|t| now - *t >= cycle.relist);
                (fw.due - EARLY <= now && retry, None)
            } else {
                match state.listing.as_ref().and_then(|l| l.size(&fw.name)) {
                    Some(size) => (true, size),
                    None => (false, None),
                }
            };
            if fw.present {
                continue;
            }
            let wait = if fw.due > now {
                Some(fw.due)
            } else if now <= fw.due + cycle.give_up || missed(state, &fw.name) {
                Some(now + cycle.relist)
            } else if newest {
                Some(state.listed_at.unwrap_or(now) + late_relist)
            } else {
                None
            };
            for &(j, _, _) in &fw.users {
                jobs[j].done = true;
            }
            if let Some(t) = wait {
                soon(&mut next, t);
            }
        }
        Ok(Some(Plan {
            run,
            jobs,
            files,
            next,
            due,
        }))
    }

    /// Fetch the plan's files of `pass` and build every part whose needs
    /// have all arrived. Builds finish before this returns, so an older run
    /// never duplicates work a newer one is still storing.
    async fn execute(
        &mut self,
        plan: &mut Plan,
        moment: Moment,
        cycle: &Cycle,
        pass: Pass,
        progress: &Progress<'_>,
    ) -> anyhow::Result<Tally> {
        let mut tally = Tally::default();
        let (run, source) = (plan.run, self.source);
        let mut builds: JoinSet<Result<Produced, Trouble>> = JoinSet::new();
        let mut stop: Option<anyhow::Error> = None;
        let Plan {
            jobs, files, next, ..
        } = plan;
        for fw in files.iter() {
            if fw.early != (pass == Pass::UpToNow) {
                continue;
            }
            // Judged again by the clock the parts would be stamped with:
            // what expired since the run was planned is not fetched.
            let at = moment.now();
            for &(j, _, _) in &fw.users {
                let st = &mut jobs[j];
                if !st.done && !self.engine.wanted(&st.job, at) {
                    st.done = true;
                }
            }
            if fw.users.iter().all(|&(j, _, _)| jobs[j].done) {
                continue;
            }
            let file = ModelFile {
                name: fw.name.clone(),
                size: fw.size,
            };
            let cx = Cx::new(&self.engine.remote, &self.name, at);
            let fetched = self.family.fetch(&cx, source, run, &file, &fw.needs).await;
            if !matches!(&fetched, Err(e) if e.failure == Failure::Service) {
                self.service_failures = 0;
            }
            match fetched {
                Ok(Fetched::Missing) => {
                    self.missing_for_now(run, fw, at, jobs);
                    soon(next, at + cycle.relist);
                }
                Ok(Fetched::Got(payloads)) if payloads.len() == fw.needs.len() => {
                    let state = self.runs.entry(run).or_default();
                    for (need, p) in fw.needs.iter().zip(&payloads) {
                        if let Some(p) = p
                            && need.is_step_total()
                        {
                            state.keep_total(&fw.name, need, p);
                        }
                    }
                    for &(j, slot, n) in &fw.users {
                        let st = &mut jobs[j];
                        if st.done {
                            continue;
                        }
                        match &payloads[n] {
                            Some(p) => st.payloads[slot] = Some(p.clone()),
                            None => {
                                self.engine.unavailable(&fw.name, &st.job);
                                tally.unavailable += 1;
                                st.done = true;
                            }
                        }
                    }
                    progress(run, format!("run {}: {}", label(run), fw.name));
                }
                Ok(Fetched::Got(payloads)) => {
                    let e = FetchError::object(
                        &fw.name,
                        format!("{} payloads for {} needs", payloads.len(), fw.needs.len()),
                    );
                    self.charge(jobs, fw, e, &mut tally)?;
                }
                // One object the service cannot serve is missing for now; the
                // rest of the run, and the older runs, still get their turn.
                Err(e) if e.failure == Failure::Service => {
                    self.service_failures += 1;
                    tracing::warn!(task = %self.name, file = %fw.name, "unreachable for now: {e}");
                    self.missing_for_now(run, fw, at, jobs);
                    soon(next, at + cycle.relist);
                    if self.service_failures >= SERVICE_FAILURES_IN_A_ROW {
                        let n = self.service_failures;
                        stop = Some(
                            anyhow::Error::new(e)
                                .context(format!("{n} files in a row unreachable")),
                        );
                        break;
                    }
                }
                Err(e) => self.charge(jobs, fw, e, &mut tally)?,
            }
            // Build every part this file completed.
            for &(j, _, _) in &fw.users {
                let st = &mut jobs[j];
                if st.done || st.payloads.iter().any(Option::is_none) {
                    continue;
                }
                st.done = true;
                // All present, as just checked.
                let payloads: Vec<Payload> =
                    st.payloads.iter_mut().filter_map(Option::take).collect();
                while builds.len() >= BUILDS_IN_FLIGHT {
                    if let Some(r) = builds.join_next().await {
                        self.record(r, &mut tally, &mut stop);
                    }
                }
                let (engine, job, lane) = (self.engine.clone(), st.job.clone(), self.name.clone());
                builds.spawn(async move { engine.produce(&job, payloads, &lane, moment).await });
            }
            if stop.is_some() {
                break;
            }
        }
        while let Some(r) = builds.join_next().await {
            self.record(r, &mut tally, &mut stop);
        }
        match stop {
            Some(e) => Err(e),
            None => Ok(tally),
        }
    }

    /// A file that is not there, or cannot be reached: its parts wait, and
    /// the run is listed again before it is asked for.
    fn missing_for_now(
        &mut self,
        run: DateTime<Utc>,
        fw: &FileWork,
        at: DateTime<Utc>,
        jobs: &mut [JobState],
    ) {
        let state = self.runs.entry(run).or_default();
        if let Some(l) = state.listing.as_mut() {
            l.remove(&fw.name);
        }
        state.missing_at.insert(fw.name.clone(), at);
        for &(j, _, _) in &fw.users {
            jobs[j].done = true;
        }
    }

    fn charge(
        &self,
        jobs: &mut [JobState],
        fw: &FileWork,
        e: FetchError,
        tally: &mut Tally,
    ) -> anyhow::Result<()> {
        let mut users: Vec<usize> = fw
            .users
            .iter()
            .map(|&(j, _, _)| j)
            .filter(|j| !jobs[*j].done)
            .collect();
        users.sort_unstable();
        users.dedup();
        let refs: Vec<&PartJob> = users.iter().map(|j| &jobs[*j].job).collect();
        tally.failed += self.engine.charge(&fw.name, &refs, Trouble::from(e))?;
        for j in users {
            jobs[j].done = true;
        }
        Ok(())
    }

    fn record(
        &self,
        r: Result<Result<Produced, Trouble>, tokio::task::JoinError>,
        tally: &mut Tally,
        stop: &mut Option<anyhow::Error>,
    ) {
        let e = match r {
            Ok(r) => tally.count(&self.name, r),
            Err(e) => Some(anyhow::anyhow!("a build task failed: {e}")),
        };
        if let Some(e) = e {
            stop.get_or_insert(e);
        }
    }

    /// The newest run this source has parts of on the catalog, with how
    /// many of its leads have one and how many it has within the horizon.
    pub fn delivered(&self, now: DateTime<Utc>) -> Option<(DateTime<Utc>, usize, usize)> {
        let mut layers: Vec<&str> = self.inputs.iter().map(|(l, _)| l.id).collect();
        layers.dedup();
        let mut newest: Option<(DateTime<Utc>, HashSet<DateTime<Utc>>)> = None;
        for layer in layers {
            for p in &self.engine.catalog.shared_timeline(layer).parts {
                let Some(run) = p.run.filter(|_| p.source == self.source.id) else {
                    continue;
                };
                match &mut newest {
                    Some((r, valid)) if *r == run => {
                        valid.insert(p.valid);
                    }
                    Some((r, _)) if *r > run => {}
                    _ => newest = Some((run, HashSet::from([p.valid]))),
                }
            }
        }
        let (run, valid) = newest?;
        let horizon = now + Duration::hours(self.engine.config.horizon_hours as i64);
        let leads: HashSet<u32> = self
            .inputs
            .iter()
            .flat_map(|(_, i)| self.family.leads(self.source, i.product, run))
            .filter(|l| run + Duration::minutes(*l as i64) <= horizon)
            .collect();
        Some((run, valid.len(), leads.len().max(valid.len())))
    }

    /// How old the newest delivered run may get before the task counts as
    /// stale: a run cycle, plus the longest a run takes to show its first
    /// file (over a day's runs), plus [`Cycle::give_up`].
    pub fn max_data_age(&self, now: DateTime<Utc>) -> Duration {
        let cycle = self.family.cycle(self.source);
        let every = cycle.every.max(Duration::minutes(1));
        let first = clock::floor(now, every);
        let mut latency = Duration::zero();
        let mut run = first;
        while run < first + Duration::hours(24) {
            for (_, input) in &self.inputs {
                if let Some(lead) = self.family.leads(self.source, input.product, run).first() {
                    latency =
                        latency.max(self.family.due(self.source, input.product, run, *lead) - run);
                }
            }
            run += every;
        }
        every + latency + cycle.give_up
    }

    /// The health line and data time: the newest run delivered, how much of
    /// it, and the newest run due when that is a different one.
    pub fn summary(&self, report: &Report, now: DateTime<Utc>) -> (Option<DateTime<Utc>>, String) {
        let delivered = self.delivered(now);
        let mut detail = match delivered {
            Some((run, have, of)) => format!("run {}, {have} of {of} leads", label(run)),
            None => "no run yet".to_string(),
        };
        if let Some(due) = report.newest_due
            && delivered.is_none_or(|(run, _, _)| run < due)
        {
            detail.push_str(&format!("; waiting for run {}", label(due)));
        }
        detail.push_str(&format!("; {}", report.tally.summary()));
        (delivered.map(|(run, _, _)| run), detail)
    }
}

/// `06 12Z`, or `06 12:05Z` for runs off the hour.
pub fn label(run: DateTime<Utc>) -> String {
    if run.timestamp() % 3600 == 0 {
        run.format("%d %HZ").to_string()
    } else {
        run.format("%d %H:%MZ").to_string()
    }
}

/// The supervised task. Its state is held across restarts, so a restart
/// resumes where the task was rather than listing every run again.
pub async fn run(task: Arc<tokio::sync::Mutex<ModelTask>>, ctx: FeedCtx) -> anyhow::Result<()> {
    let mut task = task.lock_owned().await;
    ctx.expect_data_within(task.max_data_age(Utc::now()).to_std().ok());
    loop {
        let now = Utc::now();
        let report = task.cycle(now, &|t, d| ctx.ok(Some(t), d)).await?;
        let (newest, detail) = task.summary(&report, Utc::now());
        ctx.ok(newest, detail);
        let pause = clock::wait(now, report.wake, Duration::seconds(5), MAX_SLEEP);
        clock::pause(&ctx, pause).await;
    }
}
