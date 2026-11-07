//! Official alerts from the rest of the world: every source in the WMO
//! register that publishes a public CAP feed without an account or key.
//!
//! One supervised feed (`alerts-world`) runs a scheduler that polls each
//! source on its own clock: every `poll_interval` (slower for hosts that
//! send no validators), with jitter, conditional requests where the host
//! supports them, and exponential backoff per source on failure. A source
//! that fails or hangs only delays itself: requests have deadlines and
//! each source runs as its own task. New CAP documents a feed lists are
//! fetched once (they are immutable per identifier; what was fetched is
//! recorded in `cap_documents`), parsed defensively ([`cap`]), threaded
//! into hazards and stored ([`store`]); alerts that only name NUTS 3
//! regions get their shape from Eurostat GISCO ([`nuts`]).

pub(crate) mod cap;
pub(crate) mod geo;
pub(crate) mod http;
pub(crate) mod index;
pub(crate) mod nuts;
pub(crate) mod register;
pub(crate) mod store;
pub(crate) mod text;
pub(crate) mod xml;

use crate::Inner;
use crate::feed::Changes;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use http::{FetchError, Fetched, Http, Validators};
use index::{Item, Listing};
use register::Source;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinSet;
use wx_core::FeedCtx;

/// Feeds run to a few megabytes (MeteoAlarm Poland, NAADS).
const MAX_FEED_BYTES: usize = 16 << 20;
/// CAP documents with country-sized polygons reach a few hundred kilobytes.
const MAX_CAP_BYTES: usize = 8 << 20;
const MAX_REGISTER_BYTES: usize = 4 << 20;
/// New documents fetched at once for one source.
const DOCUMENT_CONCURRENCY: usize = 4;
/// A CAP document is small; one that takes longer than this is not coming.
const DOCUMENT_TIMEOUT: Duration = Duration::from_secs(20);
/// Changes are published (and the revision bumped) at most this often:
/// every new revision makes every open map fetch its tiles again.
const PUBLISH_EVERY: Duration = Duration::from_secs(10);
/// How often the local upkeep runs: expiry, NUTS shapes, health report.
const UPKEEP_EVERY: Duration = Duration::from_secs(30);
/// A document that failed is tried again after this, doubling per failure.
/// A listed document not yet stored can hold an update back (and, for feeds
/// that list current alerts only, keeps the alert it updates on the map),
/// so retries never wait long.
const DOCUMENT_RETRY_BASE: Duration = Duration::from_secs(120);
const DOCUMENT_RETRY_MAX: Duration = Duration::from_secs(30 * 60);
/// Wait after a document answered 404 or the like, or did not parse.
const DOCUMENT_REFUSED_WAIT: Duration = Duration::from_secs(3600);
/// Wait after a document failed `DOCUMENT_ATTEMPTS` times in a row.
const DOCUMENT_GIVE_UP_WAIT: Duration = Duration::from_secs(2 * 3600);
/// After this many failed fetches a document is left alone for a day.
const DOCUMENT_ATTEMPTS: u32 = 6;
/// Sources named in the health summary as the worst.
const WORST_SHOWN: usize = 5;

#[derive(Debug, Clone)]
pub struct WorldConfig {
    /// Read the world's sources. When false only the NWS path runs.
    pub enabled: bool,
    /// Sent as `User-Agent` to every host this path calls. It must not
    /// carry a personal contact.
    pub user_agent: String,
    /// The WMO register of alerting sources.
    pub register_url: String,
    /// How often the register is checked (with a conditional request).
    pub register_refresh: Duration,
    /// How often each source is polled.
    pub poll_interval: Duration,
    /// For hosts that send neither `ETag` nor `Last-Modified`, so every
    /// poll downloads the whole feed (MeteoAlarm).
    pub slow_poll_interval: Duration,
    /// Requests at once to one host, and in all.
    pub per_host_concurrency: usize,
    pub max_concurrency: usize,
    pub request_timeout: Duration,
    /// Longest wait between polls of a failing source.
    pub max_backoff: Duration,
    /// New CAP documents fetched per source per poll. Feeds that keep
    /// hundreds of old items are worked through over several polls, newest
    /// first.
    pub documents_per_poll: usize,
    /// Feed items dated longer ago than this are not fetched.
    pub max_item_age: Duration,
    /// Eurostat GISCO's NUTS distribution
    /// (`https://gisco-services.ec.europa.eu/distribution/v2/nuts`), for
    /// alerts that name NUTS 3 regions. `None` leaves them without a shape.
    /// The boundaries are licensed for non-commercial use only and must be
    /// credited (see [`NUTS_ATTRIBUTION`]).
    pub nuts_base_url: Option<String>,
    /// Minimum spacing between NUTS requests.
    pub nuts_fetch_interval: Duration,
    /// Accept loopback and private addresses. Only for tests that run local
    /// stand-ins for the feeds; never in production.
    pub allow_private_hosts: bool,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            user_agent: "weather-map-dev".to_string(),
            register_url: "https://severeweather.wmo.int/json/sources.json".to_string(),
            register_refresh: Duration::from_secs(24 * 3600),
            poll_interval: Duration::from_secs(120),
            slow_poll_interval: Duration::from_secs(240),
            per_host_concurrency: 2,
            max_concurrency: 16,
            request_timeout: Duration::from_secs(30),
            max_backoff: Duration::from_secs(3600),
            documents_per_poll: 100,
            max_item_age: Duration::from_secs(30 * 24 * 3600),
            nuts_base_url: Some(nuts::GISCO_BASE.to_string()),
            nuts_fetch_interval: Duration::from_millis(250),
            allow_private_hosts: false,
        }
    }
}

/// Credit the NUTS licence requires wherever shapes derived from it are shown.
pub const NUTS_ATTRIBUTION: &str = "© EuroGeographics for the administrative boundaries";

pub(crate) struct World {
    pub config: WorldConfig,
    pub http: Http,
    pub state: Mutex<WorldState>,
    /// Changes stored but not yet published.
    pub pending: Mutex<Changes>,
    pub nuts: nuts::NutsState,
    /// Documents fetched since last counted (for reports).
    fetched: AtomicUsize,
}

#[derive(Default)]
pub(crate) struct WorldState {
    pub sources: Vec<Arc<Source>>,
    pub runtime: HashMap<String, SourceRuntime>,
    pub register: RegisterState,
    /// What became of each CAP document URL fetched.
    pub documents: HashMap<String, DocState>,
    pub documents_loaded: bool,
}

#[derive(Default)]
pub(crate) struct RegisterState {
    pub loaded_from: Option<&'static str>,
    pub validators: Validators,
    pub fetched_at: Option<DateTime<Utc>>,
    pub next_check: Option<Instant>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DocState {
    pub done: bool,
    pub attempts: u32,
    pub retry_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceRuntime {
    pub validators: Validators,
    pub body_hash: Option<[u8; 32]>,
    pub next_due: Option<Instant>,
    pub running: bool,
    pub failures: u32,
    pub slow: bool,
    pub last_attempt: Option<DateTime<Utc>>,
    pub last_ok: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// Items in the feed at the last successful poll (after filtering).
    pub items: Arc<Vec<Item>>,
    /// The last listing that was not empty: what "listed before" means for
    /// feeds that list current alerts only.
    pub last_listed: Arc<Vec<Item>>,
    /// Of those, documents not fetched yet.
    pub backlog: usize,
    pub documents_failed: usize,
}

/// Health of one source, for `/api/alerts/sources`.
#[derive(Debug, Clone, Serialize)]
pub struct SourceHealth {
    pub id: String,
    pub name: String,
    pub country: Option<String>,
    pub country_name: Option<String>,
    pub region: Option<u8>,
    pub feed: String,
    /// `starting | healthy | empty | failing`.
    pub state: &'static str,
    pub items: usize,
    pub backlog: usize,
    pub active_alerts: i64,
    pub drawn_alerts: i64,
    pub last_ok: Option<DateTime<Utc>>,
    pub last_attempt: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
}

/// The outcome of [`crate::Alerts::world_poll_once`].
#[derive(Debug, Clone, Default)]
pub struct WorldReport {
    pub sources: usize,
    pub polled: usize,
    pub failed: usize,
    /// Sources whose feed answered but listed nothing current.
    pub empty: usize,
    pub documents_fetched: usize,
    pub documents_failed: usize,
    /// Alerts newly on the map.
    pub added: usize,
    pub notifications: usize,
}

impl World {
    pub fn new(config: WorldConfig) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !config.user_agent.trim().is_empty(),
            "world.user_agent must not be empty"
        );
        anyhow::ensure!(
            !config.poll_interval.is_zero(),
            "world.poll_interval must not be zero"
        );
        let http = Http::new(
            &config.user_agent,
            config.per_host_concurrency,
            config.max_concurrency,
            config.allow_private_hosts,
        )?;
        Ok(Self {
            nuts: nuts::NutsState::new(config.nuts_fetch_interval),
            http,
            state: Mutex::new(WorldState::default()),
            pending: Mutex::new(Changes::default()),
            fetched: AtomicUsize::new(0),
            config,
        })
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, WorldState> {
        crate::lock(&self.state)
    }

    fn add_pending(&self, ch: Changes) {
        crate::lock(&self.pending).absorb(ch);
    }

    fn take_pending(&self) -> Changes {
        std::mem::take(&mut *crate::lock(&self.pending))
    }
}

impl Inner {
    pub(crate) fn world(&self) -> &World {
        self.world.as_ref().expect("the world path is configured")
    }

    /// The `alerts-world` feed: the scheduler loop.
    pub(crate) async fn run_world_feed(self: &Arc<Self>, ctx: FeedCtx) -> anyhow::Result<()> {
        self.world_start().await?;
        let world = self.world();
        // No poll is running when the loop (re)starts, whatever a previous
        // run left behind.
        for rt in world.lock().runtime.values_mut() {
            rt.running = false;
        }
        let mut tasks: JoinSet<(String, SourceResult)> = JoinSet::new();
        // Background chores, one of each at a time, so that neither a slow
        // register host nor a batch of NUTS downloads holds up the polls.
        let mut register_task: Option<tokio::task::JoinHandle<()>> = None;
        let mut upkeep_task: Option<tokio::task::JoinHandle<()>> = None;
        let mut last_publish = Instant::now();
        let mut last_upkeep: Option<Instant> = None;
        loop {
            let register_due = world
                .lock()
                .register
                .next_check
                .is_none_or(|t| t <= Instant::now());
            if register_due && register_task.as_ref().is_none_or(|t| t.is_finished()) {
                let inner = self.clone();
                register_task = Some(tokio::spawn(async move { inner.refresh_register().await }));
            }
            // Sources that are due.
            for (src, validators, items) in self.take_sources(false) {
                let inner = self.clone();
                let id = src.id.clone();
                tasks.spawn(async move {
                    let r = inner.poll_source(&src, validators, items).await;
                    (id, r)
                });
            }
            if last_upkeep
                .is_none_or(|t| t.elapsed() >= UPKEEP_EVERY.min(world.config.poll_interval))
            {
                last_upkeep = Some(Instant::now());
                if upkeep_task.as_ref().is_none_or(|t| t.is_finished()) {
                    let inner = self.clone();
                    upkeep_task = Some(tokio::spawn(async move {
                        if let Err(e) = inner.world_upkeep().await {
                            tracing::warn!("world alerts upkeep failed: {e:#}");
                        }
                    }));
                }
                let (summary, newest) = self.world_summary().await;
                ctx.ok(newest, summary);
            }
            if last_publish.elapsed() >= PUBLISH_EVERY {
                last_publish = Instant::now();
                let ch = world.take_pending();
                self.publish(ch).await;
            }
            let wake = self.next_wake().min(Instant::now() + PUBLISH_EVERY);
            tokio::select! {
                Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                    match done {
                        Ok((id, result)) => self.finish_source(&id, result),
                        Err(e) if e.is_panic() => tracing::error!("a world alert source poll panicked"),
                        Err(_) => {}
                    }
                }
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(wake)) => {}
                _ = ctx.cancelled() => {
                    tasks.abort_all();
                    for t in [register_task.take(), upkeep_task.take()].into_iter().flatten() {
                        t.abort();
                    }
                    // Running polls were aborted: their sources are due again.
                    for rt in world.lock().runtime.values_mut() {
                        rt.running = false;
                    }
                    return Ok(());
                }
            }
        }
    }

    /// Load the register and what was fetched before (once per process),
    /// and ask for the live register before polling from the bundled one.
    pub(crate) async fn world_start(self: &Arc<Self>) -> anyhow::Result<()> {
        let world = self.world();
        if world.lock().register.loaded_from.is_none() {
            self.load_register().await;
        }
        if !world.lock().documents_loaded {
            let docs = store::load_documents(
                &self.pool,
                chrono::Duration::from_std(DOCUMENT_GIVE_UP_WAIT).unwrap_or_default(),
            )
            .await?;
            // Hazards stored as several threads are put back together before
            // anything else.
            let ch = store::rethread(self).await?;
            if !ch.added.is_empty() || !ch.removed.is_empty() || !ch.listed.is_empty() {
                tracing::info!(
                    removed = ch.removed.len() + ch.listed.len(),
                    "world alerts re-threaded: superseded copies of hazards ended"
                );
            }
            world.add_pending(ch);
            let mut st = world.lock();
            st.documents = docs;
            st.documents_loaded = true;
        }
        if world.lock().register.loaded_from == Some("snapshot") {
            self.refresh_register().await;
        }
        Ok(())
    }

    /// The register kept in the database if there is one, else the snapshot.
    async fn load_register(&self) {
        let world = self.world();
        let stored = store::load_register(&self.pool).await.unwrap_or_else(|e| {
            tracing::warn!("reading the stored alert register failed: {e:#}");
            None
        });
        let (sources, from, validators, fetched_at) =
            match stored.and_then(|r| register::parse(&r.body).ok().map(|s| (s, r))) {
                Some((sources, r)) => (sources, "database", r.validators, Some(r.fetched_at)),
                None => (
                    register::parse(register::SNAPSHOT).expect("the bundled register parses"),
                    "snapshot",
                    Validators::default(),
                    None,
                ),
            };
        let next_check = match fetched_at {
            Some(t) => {
                let age = (Utc::now() - t).to_std().unwrap_or_default();
                Instant::now() + world.config.register_refresh.saturating_sub(age)
            }
            None => Instant::now(),
        };
        let mut st = world.lock();
        st.register = RegisterState {
            loaded_from: Some(from),
            validators,
            fetched_at,
            next_check: Some(next_check),
            last_error: None,
        };
        set_sources(&mut st, sources, &world.config);
        tracing::info!(
            sources = st.sources.len(),
            from,
            "world alert register loaded"
        );
    }

    /// Fetch the live register if it changed; keep the current one on any
    /// failure.
    pub(crate) async fn refresh_register(&self) {
        let world = self.world();
        let validators = world.lock().register.validators.clone();
        let result = world
            .http
            .get(
                &world.config.register_url,
                &validators,
                world.config.request_timeout,
                MAX_REGISTER_BYTES,
            )
            .await;
        type Fresh = (Vec<Source>, Vec<u8>, Validators);
        let outcome: Result<Option<Fresh>, String> = match result {
            Ok(Fetched::NotModified) => Ok(None),
            Ok(Fetched::Body { bytes, validators }) => match register::parse(&bytes) {
                Ok(sources) => Ok(Some((sources, bytes, validators))),
                Err(e) => Err(e.to_string()),
            },
            Err(e) => Err(e.to_string()),
        };
        let save = {
            let mut st = world.lock();
            match outcome {
                Ok(fresh) => {
                    st.register.next_check = Some(Instant::now() + world.config.register_refresh);
                    st.register.last_error = None;
                    st.register.fetched_at = Some(Utc::now());
                    fresh.map(|(sources, body, validators)| {
                        st.register.validators = validators.clone();
                        st.register.loaded_from = Some("live");
                        set_sources(&mut st, sources, &world.config);
                        (body, validators)
                    })
                }
                Err(e) => {
                    tracing::warn!("alert register not refreshed: {e}");
                    st.register.last_error = Some(e);
                    // Try again in an hour, then keep using what we have.
                    st.register.next_check = Some(
                        Instant::now()
                            + Duration::from_secs(3600).min(world.config.register_refresh),
                    );
                    None
                }
            }
        };
        if let Some((body, validators)) = save
            && let Err(e) = store::save_register(&self.pool, &body, &validators).await
        {
            tracing::warn!("storing the alert register failed: {e:#}");
        }
    }

    /// The sources due now (or with `all`, every one), marked running. Each
    /// comes with its validators and its last listing (for a 304).
    fn take_sources(&self, all: bool) -> Vec<(Arc<Source>, Validators, Arc<Vec<Item>>)> {
        let now = Instant::now();
        let mut st = self.world().lock();
        let sources = st.sources.clone();
        let mut out = Vec::new();
        for src in sources {
            let rt = st.runtime.entry(src.id.clone()).or_default();
            if !all && (rt.running || rt.next_due.is_some_and(|t| t > now)) {
                continue;
            }
            rt.running = true;
            rt.last_attempt = Some(Utc::now());
            out.push((src, rt.validators.clone(), rt.items.clone()));
        }
        out
    }

    fn next_wake(&self) -> Instant {
        let world = self.world();
        let st = world.lock();
        let register = st
            .register
            .next_check
            .unwrap_or_else(|| Instant::now() + UPKEEP_EVERY);
        st.runtime
            .values()
            .filter(|rt| !rt.running)
            .filter_map(|rt| rt.next_due)
            .min()
            .unwrap_or_else(|| Instant::now() + UPKEEP_EVERY)
            .min(register)
            .max(Instant::now() + Duration::from_millis(50))
    }

    fn finish_source(&self, id: &str, result: SourceResult) {
        let world = self.world();
        let cfg = &world.config;
        let mut st = world.lock();
        let Some(rt) = st.runtime.get_mut(id) else {
            return;
        };
        rt.running = false;
        match result {
            SourceResult::Ok {
                validators,
                body_hash,
                items,
                backlog,
                documents_failed,
            } => {
                rt.failures = 0;
                rt.last_ok = Some(Utc::now());
                rt.last_error = None;
                if let Some(v) = validators {
                    rt.slow = !v.any();
                    rt.validators = v;
                }
                if let Some(h) = body_hash {
                    rt.body_hash = Some(h);
                }
                if !items.is_empty() {
                    rt.last_listed = items.clone();
                }
                rt.items = items;
                rt.backlog = backlog;
                rt.documents_failed = documents_failed;
                let base = if rt.slow {
                    cfg.slow_poll_interval
                } else {
                    cfg.poll_interval
                };
                // Work through a backlog sooner, but not in a burst.
                let base = if backlog > 0 {
                    base.min(Duration::from_secs(60))
                } else {
                    base
                };
                rt.next_due = Some(Instant::now() + jitter(base));
            }
            SourceResult::Failed { error, retry_after } => {
                rt.failures = rt.failures.saturating_add(1);
                rt.last_error = Some(error);
                let backoff = cfg
                    .poll_interval
                    .saturating_mul(2u32.saturating_pow(rt.failures.min(12)))
                    .min(cfg.max_backoff);
                let wait =
                    retry_after.map_or(backoff, |r| r.max(cfg.poll_interval).min(cfg.max_backoff));
                rt.next_due = Some(Instant::now() + jitter(wait));
            }
        }
    }

    /// One pass over every source now, outside the schedule, then the local
    /// upkeep and a publish. For tests and one-off checks.
    pub(crate) async fn world_poll_once(self: &Arc<Self>) -> anyhow::Result<WorldReport> {
        self.world_start().await?;
        let world = self.world();
        let due = self.take_sources(true);
        let mut report = WorldReport {
            sources: due.len(),
            ..WorldReport::default()
        };
        let results: Vec<(String, SourceResult)> = futures::stream::iter(due)
            .map(|(src, validators, items)| async move {
                let r = self.poll_source(&src, validators, items).await;
                (src.id.clone(), r)
            })
            .buffer_unordered(world.config.max_concurrency.max(1))
            .collect()
            .await;
        for (id, result) in results {
            report.polled += 1;
            match &result {
                SourceResult::Ok {
                    items,
                    documents_failed,
                    ..
                } => {
                    report.documents_failed += documents_failed;
                    if items.is_empty() {
                        report.empty += 1;
                    }
                }
                SourceResult::Failed { .. } => report.failed += 1,
            }
            self.finish_source(&id, result);
        }
        let swept = self.world_upkeep().await?;
        let ch = world.take_pending();
        report.added = ch.added.len();
        report.notifications = self.publish(ch).await + swept;
        report.documents_fetched = world.fetched.swap(0, Ordering::Relaxed);
        Ok(report)
    }

    /// Expire ended alerts, shape alerts from NUTS regions, and run the
    /// sweep when it is due. Changes are left for the next publish; returns
    /// the notifications the sweep created.
    async fn world_upkeep(self: &Arc<Self>) -> anyhow::Result<usize> {
        let mut ch = Changes::default();
        {
            let _writes = self.writes.lock().await;
            self.expire_ended(&mut ch).await?;
        }
        self.world().add_pending(ch);
        if self.world().config.nuts_base_url.is_some() {
            match nuts::shape_pending(self).await {
                Ok(ch) => self.world().add_pending(ch),
                Err(e) => tracing::warn!("NUTS shapes: {e:#}"),
            }
        }
        let mut swept = 0;
        if self.sweep_due() {
            match self.sweep().await {
                Ok(n) => {
                    swept = n;
                    self.sweep_done();
                }
                Err(e) => tracing::warn!("alerts sweep failed: {e:#}"),
            }
        }
        Ok(swept)
    }

    /// Poll one source: its feed, then the new documents it lists.
    async fn poll_source(
        self: &Arc<Self>,
        src: &Source,
        validators: Validators,
        last_items: Arc<Vec<Item>>,
    ) -> SourceResult {
        let world = self.world();
        let cfg = &world.config;
        let (previous, first_poll) = {
            let st = world.lock();
            let rt = st.runtime.get(&src.id);
            (
                rt.map(|r| r.last_listed.clone()).unwrap_or_default(),
                rt.is_none_or(|r| r.last_ok.is_none()),
            )
        };
        let fetched = world
            .http
            .get(&src.feed, &validators, cfg.request_timeout, MAX_FEED_BYTES)
            .await;
        let (items, new_validators, body_hash, message) = match fetched {
            Err(e) => return SourceResult::failed(&e),
            Ok(Fetched::NotModified) => (last_items, None, None, None),
            Ok(Fetched::Body { bytes, validators }) => {
                let hash: [u8; 32] = Sha256::digest(&bytes).into();
                let unchanged =
                    world.lock().runtime.get(&src.id).and_then(|r| r.body_hash) == Some(hash);
                if unchanged {
                    (last_items, Some(validators), Some(hash), None)
                } else {
                    let feed_url = match url::Url::parse(&src.feed) {
                        Ok(u) => u,
                        Err(e) => return SourceResult::error(format!("feed URL: {e}")),
                    };
                    match index::parse(&bytes, &feed_url) {
                        Ok(Listing::Items(items)) => (
                            Arc::new(self.current_items(items)),
                            Some(validators),
                            Some(hash),
                            None,
                        ),
                        Ok(Listing::Message(m)) => {
                            (Arc::new(Vec::new()), Some(validators), Some(hash), Some(*m))
                        }
                        Err(e) => return SourceResult::error(format!("feed: {e}")),
                    }
                }
            }
        };

        // The feed is itself a message.
        let mut stored_from_feed = Vec::new();
        if let Some(m) = message {
            stored_from_feed.push((src.feed.clone(), Ok(m)));
        }

        // Documents to fetch: not seen before, or due for another try.
        let now = Utc::now();
        let todo: Vec<String> = {
            let st = world.lock();
            items
                .iter()
                .filter(|i| {
                    st.documents
                        .get(&i.link)
                        .is_none_or(|d| !d.done && d.retry_at.is_none_or(|t| t <= now))
                })
                .map(|i| i.link.clone())
                .collect()
        };
        // In small batches: when a whole batch cannot be reached (a host
        // whose documents all time out), the rest wait for the next poll
        // instead of costing a timeout each.
        let wanted = todo.len();
        let mut fetches: Vec<(String, Result<cap::Message, DocError>)> = Vec::new();
        for batch in todo[..wanted.min(cfg.documents_per_poll)].chunks(DOCUMENT_CONCURRENCY) {
            let results = futures::future::join_all(batch.iter().map(|link| async move {
                let r = self.fetch_document(link).await;
                (link.clone(), r)
            }))
            .await;
            let unreachable = results
                .iter()
                .all(|(_, r)| matches!(r, Err(DocError::Fetch(e)) if !e.is_permanent()));
            fetches.extend(results);
            if unreachable {
                break;
            }
        }
        let backlog = wanted - fetches.len();
        world.fetched.fetch_add(fetches.len(), Ordering::Relaxed);
        let mut parsed: Vec<(String, cap::Message)> = Vec::new();
        let mut outcomes: Vec<(String, Result<store::Stored, DocError>)> = Vec::new();
        for (link, r) in stored_from_feed.into_iter().chain(fetches) {
            match r {
                Ok(m) => parsed.push((link, m)),
                Err(e) => outcomes.push((link, Err(e))),
            }
        }
        let documents_failed = outcomes.len();

        // For a feed that lists current alerts only, an alert it stops
        // listing has ended, but only on firm evidence: not on the first
        // poll (nothing to compare with), not on an empty feed (more often a
        // glitch than a whole country at once), not while a listed document
        // is not stored yet (it may be the update that replaces the alert
        // that dropped out), and only once it has been missing from two
        // listings in a row (a truncated feed is a single bad listing).
        let complete = {
            let parsed_links: HashSet<&str> = parsed.iter().map(|(l, _)| l.as_str()).collect();
            let st = world.lock();
            items.iter().all(|i| {
                parsed_links.contains(i.link.as_str())
                    || st.documents.get(&i.link).is_some_and(|d| d.done)
            })
        };
        let listed: Vec<String> = items
            .iter()
            .chain(previous.iter())
            .map(|i| i.link.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let may_end = src.lists_current_only && !first_poll && !items.is_empty() && complete;
        let stored = match store::apply(self, src, parsed, may_end.then_some(&listed[..])).await {
            Ok((ch, results)) => {
                world.add_pending(ch);
                results
            }
            // The documents are fetched again on the next poll.
            Err(e) => return SourceResult::error(format!("storing alerts: {e:#}")),
        };
        outcomes.extend(stored.into_iter().map(|(link, r)| (link, Ok(r))));
        self.record_documents(src, outcomes).await;

        SourceResult::Ok {
            validators: new_validators,
            body_hash,
            items,
            backlog,
            documents_failed,
        }
    }

    /// Feed items worth considering, newest first: Actual, not expired by the
    /// feed's own account, not older than `max_item_age`, on a fetchable URL.
    fn current_items(&self, mut items: Vec<Item>) -> Vec<Item> {
        let cfg = &self.world().config;
        let now = Utc::now();
        let oldest = now
            - chrono::Duration::from_std(cfg.max_item_age).unwrap_or(chrono::Duration::days(30));
        items.retain(|i| {
            !i.not_actual
                && i.expires.is_none_or(|e| e > now)
                && i.published.is_none_or(|p| p >= oldest)
                && http::check_url(&i.link, cfg.allow_private_hosts).is_ok()
        });
        // Stable: undated items keep their feed order, after dated ones.
        items.sort_by_key(|i| std::cmp::Reverse(i.published));
        items
    }

    async fn fetch_document(&self, link: &str) -> Result<cap::Message, DocError> {
        let world = self.world();
        let timeout = world.config.request_timeout.min(DOCUMENT_TIMEOUT);
        let body = match world
            .http
            .get(link, &Validators::default(), timeout, MAX_CAP_BYTES)
            .await
        {
            Ok(Fetched::Body { bytes, .. }) => bytes,
            Ok(Fetched::NotModified) => {
                return Err(DocError::Invalid("304 to an unconditional request".into()));
            }
            Err(e) => return Err(DocError::Fetch(e)),
        };
        // Parsing a large document takes a while; keep it off the scheduler.
        tokio::task::spawn_blocking(move || cap::parse(&body))
            .await
            .map_err(|_| DocError::Invalid("parser panicked".into()))?
            .map_err(|e| DocError::Invalid(e.to_string()))
    }

    /// Record what became of each document, and when to try a failed one
    /// again.
    async fn record_documents(
        &self,
        src: &Source,
        outcomes: Vec<(String, Result<store::Stored, DocError>)>,
    ) {
        let now = Utc::now();
        let rows: Vec<(String, DocState, String)> = {
            let mut st = self.world().lock();
            outcomes
                .into_iter()
                .map(|(link, outcome)| {
                    let attempts = st.documents.get(&link).map_or(0, |d| d.attempts) + 1;
                    let (done, attempts, wait, label) = match outcome {
                        Ok(r) => (true, attempts, None, r.label().to_string()),
                        Err(DocError::Invalid(why)) => (
                            attempts >= 3,
                            attempts,
                            Some(DOCUMENT_REFUSED_WAIT),
                            format!("invalid: {why}"),
                        ),
                        Err(DocError::Fetch(e)) => {
                            let permanent = e.is_permanent();
                            let give_up =
                                attempts >= DOCUMENT_ATTEMPTS || (permanent && attempts >= 3);
                            let wait = if give_up {
                                DOCUMENT_GIVE_UP_WAIT
                            } else if permanent {
                                DOCUMENT_REFUSED_WAIT
                            } else {
                                DOCUMENT_RETRY_BASE
                                    .saturating_mul(2u32.saturating_pow(attempts.min(10) - 1))
                                    .min(DOCUMENT_RETRY_MAX)
                            };
                            (
                                false,
                                if give_up { 0 } else { attempts },
                                Some(wait),
                                format!("failed: {e}"),
                            )
                        }
                    };
                    let retry_at =
                        wait.map(|w| now + chrono::Duration::from_std(w).unwrap_or_default());
                    let state = DocState {
                        done,
                        attempts,
                        retry_at,
                    };
                    st.documents.insert(link.clone(), state.clone());
                    (link, state, label)
                })
                .collect()
        };
        if let Err(e) = store::save_documents(&self.pool, &src.id, &rows).await {
            tracing::warn!(source = %src.id, "recording fetched documents failed: {e}");
        }
    }

    /// Per-source health, worst first.
    pub(crate) async fn world_sources(&self) -> Vec<SourceHealth> {
        let counts: HashMap<String, (i64, i64)> = sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT source, count(*), count(*) FILTER (WHERE geom IS NOT NULL)
             FROM alerts WHERE origin = 'cap' AND active GROUP BY source",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(s, a, d)| (s, (a, d)))
        .collect();
        let world = self.world();
        let st = world.lock();
        let mut out: Vec<SourceHealth> = st
            .sources
            .iter()
            .map(|src| {
                let rt = st.runtime.get(&src.id).cloned().unwrap_or_default();
                let state = if rt.failures > 0 {
                    "failing"
                } else if rt.last_ok.is_none() {
                    "starting"
                } else if rt.items.is_empty() {
                    "empty"
                } else {
                    "healthy"
                };
                let (active, drawn) = counts.get(&src.id).copied().unwrap_or((0, 0));
                SourceHealth {
                    id: src.id.clone(),
                    name: src.name.clone(),
                    country: src.country.clone(),
                    country_name: src.country_name.clone(),
                    region: src.region,
                    feed: src.feed.clone(),
                    state,
                    items: rt.items.len(),
                    backlog: rt.backlog,
                    active_alerts: active,
                    drawn_alerts: drawn,
                    last_ok: rt.last_ok,
                    last_attempt: rt.last_attempt,
                    last_error: rt.last_error.clone(),
                    consecutive_failures: rt.failures,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.consecutive_failures
                .cmp(&a.consecutive_failures)
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    /// One line for the status page, and when the newest message was taken in.
    async fn world_summary(&self) -> (String, Option<DateTime<Utc>>) {
        let sources = self.world_sources().await;
        let count = |s: &str| sources.iter().filter(|h| h.state == s).count();
        let (healthy, empty, failing) = (count("healthy"), count("empty"), count("failing"));
        let (active, drawn): (i64, i64) = (
            sources.iter().map(|s| s.active_alerts).sum(),
            sources.iter().map(|s| s.drawn_alerts).sum(),
        );
        let mut s = format!(
            "{} sources: {healthy} healthy, {empty} empty, {failing} failing; {active} alerts ({drawn} drawn)",
            sources.len()
        );
        let worst: Vec<String> = sources
            .iter()
            .filter(|h| h.consecutive_failures > 0)
            .take(WORST_SHOWN)
            .map(|h| {
                let why = h.last_error.as_deref().unwrap_or("failing");
                format!(
                    "{} ({}, x{})",
                    h.id,
                    why.chars().take(60).collect::<String>(),
                    h.consecutive_failures
                )
            })
            .collect();
        if !worst.is_empty() {
            s.push_str("; worst: ");
            s.push_str(&worst.join(", "));
        }
        // When the newest message was taken in, not its `sent`: some issuers
        // stamp messages ahead of the clock.
        let newest: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT least(max(first_seen), now()) FROM alerts WHERE origin = 'cap'",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or(None);
        (s, newest)
    }
}

/// Replace the source list, keeping the runtime state of sources that stay.
fn set_sources(st: &mut WorldState, sources: Vec<Source>, cfg: &WorldConfig) {
    let ids: HashSet<String> = sources.iter().map(|s| s.id.clone()).collect();
    st.runtime.retain(|id, _| ids.contains(id));
    let first = st.sources.is_empty();
    for (i, s) in sources.iter().enumerate() {
        let rt = st.runtime.entry(s.id.clone()).or_default();
        // A feed URL that changed starts afresh.
        if st
            .sources
            .iter()
            .any(|old| old.id == s.id && old.feed != s.feed)
        {
            *rt = SourceRuntime::default();
        }
        if first && rt.next_due.is_none() {
            // Spread the first polls over a few seconds rather than firing
            // every source in the same instant.
            let spread = Duration::from_millis(50) * i as u32;
            rt.next_due = Some(Instant::now() + spread.min(cfg.poll_interval / 4));
        }
    }
    st.sources = sources.into_iter().map(Arc::new).collect();
}

/// `d` spread by +/-10%.
fn jitter(d: Duration) -> Duration {
    d.mul_f64(0.9 + 0.2 * rand::random::<f64>())
}

pub(crate) enum SourceResult {
    Ok {
        validators: Option<Validators>,
        body_hash: Option<[u8; 32]>,
        items: Arc<Vec<Item>>,
        backlog: usize,
        documents_failed: usize,
    },
    Failed {
        error: String,
        retry_after: Option<Duration>,
    },
}

impl SourceResult {
    fn failed(e: &FetchError) -> Self {
        Self::Failed {
            error: format!("feed: {e}"),
            retry_after: e.retry_after(),
        }
    }

    fn error(error: String) -> Self {
        Self::Failed {
            error,
            retry_after: None,
        }
    }
}

enum DocError {
    Fetch(FetchError),
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Alerts, AlertsConfig};
    use sqlx::PgPool;

    fn alerts(pool: PgPool) -> Alerts {
        let config = AlertsConfig {
            world: WorldConfig {
                // Refused before anything is sent: loopback is not public.
                register_url: "http://127.0.0.1:9/json/sources.json".to_string(),
                ..WorldConfig::default()
            },
            ..AlertsConfig::default()
        };
        Alerts::new(pool, wx_core::Hub::new(), config).unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn the_register_comes_from_the_database_then_the_snapshot(pool: PgPool) {
        // Nothing stored and the host unreachable: the bundled snapshot.
        let a = alerts(pool.clone());
        a.inner.world_start().await.unwrap();
        a.inner.refresh_register().await;
        {
            let st = a.inner.world().lock();
            assert_eq!(st.register.loaded_from, Some("snapshot"));
            assert_eq!(st.sources.len(), 154);
            assert!(
                st.register
                    .last_error
                    .as_deref()
                    .unwrap()
                    .contains("not a public host")
            );
            assert!(
                st.register.next_check.unwrap() > Instant::now(),
                "tried again later, not at once"
            );
        }

        // A register fetched earlier is preferred, with its validators.
        let mut doc: serde_json::Value = serde_json::from_slice(register::SNAPSHOT).unwrap();
        doc["sources"].as_array_mut().unwrap().truncate(30);
        let v = Validators {
            etag: None,
            last_modified: Some("Wed, 04 Nov 2026 08:53:47 GMT".into()),
        };
        store::save_register(&pool, doc.to_string().as_bytes(), &v)
            .await
            .unwrap();
        let b = alerts(pool);
        b.inner.world_start().await.unwrap();
        let st = b.inner.world().lock();
        assert_eq!(st.register.loaded_from, Some("database"));
        assert!(st.sources.len() >= 28, "{}", st.sources.len());
        assert_eq!(st.register.validators.last_modified, v.last_modified);
        assert!(
            st.register.next_check.unwrap() > Instant::now() + Duration::from_secs(3600),
            "fresh: not checked again yet"
        );
    }

    #[test]
    fn backoff_and_schedule() {
        let w = World::new(WorldConfig::default()).unwrap();
        let src = Arc::new(register::parse(register::SNAPSHOT).unwrap().remove(0));
        let mut st = WorldState::default();
        set_sources(&mut st, vec![(*src).clone()], &w.config);
        assert!(
            st.runtime[&src.id].next_due.unwrap() <= Instant::now() + w.config.poll_interval / 4
        );
        let d = jitter(Duration::from_secs(100));
        assert!(d >= Duration::from_secs(90) && d <= Duration::from_secs(110));
    }
}
