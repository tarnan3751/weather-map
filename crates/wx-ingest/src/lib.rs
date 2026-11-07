//! Ingest. Every source in the registry that has an adapter is followed by
//! supervised background tasks that notice new data at the provider, fetch
//! just what the catalog wants, build each part (one source's data for one
//! layer at one time) on a blocking thread, store its grid, and put it on the
//! catalog, which tells every open browser. Tiles are not made here: the tile
//! server renders them on demand from stacks of parts.
//!
//! * [`remote`]: HTTPS GETs, listings and the rules every request follows
//!   (politeness per host, fairness across tasks, retries, size limits).
//! * [`idx`]: GRIB2 inventories and byte ranges.
//! * [`driver`]: the seams. A family implements [`driver::Model`] or
//!   [`driver::Observed`]; the drivers do the scheduling, fetching, step
//!   differencing, building, storing and inserting.
//! * [`families`]: one module per family, registered in
//!   [`families::adapter`].
//! * `engine`: the path every part takes once its data has arrived.
//! * `janitor`: deleting dropped and orphaned parts.
//!
//! # Adding a family
//!
//! The family's registry module (`wx_core::families::<family>`) already has
//! its rows, inputs, horizons and layout notes. The adapter is one file:
//!
//! 1. `src/families/<family>.rs`: a struct implementing [`driver::Model`]
//!    (runs with lead times, including nowcasts) or [`driver::Observed`]
//!    (one file per observation time). A model says when runs start
//!    ([`driver::Cycle`]), which leads a product has in a run, when each is
//!    due, which file holds each [`driver::Need`], what of a run is listed,
//!    and how to get a file's needs; an observation family says its
//!    [`driver::Cadence`], what is listed since a time, and how to get a
//!    file's needs. Take every constant from the registry module; keep a
//!    small table for whatever differs by `SourceDef::region`, and reject
//!    unknown regions in `check`.
//! 2. Getting needs: [`driver::Cx`] has the helpers. `grib_with_idx` for a
//!    GRIB2 file with a NOAA-style `.idx`, wgrib2 `.index` or `.inv` beside
//!    it; `grib_by_inventory` when the family parses its own index (ECMWF's
//!    JSON) into [`idx::IdxEntry`] values; `whole` for small files; `get`
//!    with `range` and `if_changed` for anything else; `list_s3` and
//!    `list_html` for listings; `cpu` for splitting an
//!    archive. [`remote::codec`] inflates gzip and bzip2. Shared listing and
//!    naming helpers are in [`families::common`].
//! 3. Decoding: return [`driver::Payload::Grib`] for GRIB2 whose fields are
//!    named NOAA's way, [`driver::grib_picking`] for GRIB2 named any other
//!    way, and [`driver::Payload::Custom`] with a `wx-formats` decoder for
//!    HDF5, GeoTIFF, NetCDF or PNG (add `wx-formats` to this crate's
//!    dependencies then). Hand back native units with fill values as NaN.
//! 4. Step accumulations: nothing to do if the files hold run totals; return
//!    [`driver::Steps::Windows`] from `Model::steps` if they hold each
//!    hour's own record.
//! 5. Register the adapter in [`families::adapter`] (one line). A family
//!    with observed sources and nowcast sources registers one of each,
//!    chosen by the source's `SourceDef::kind`.
//! 6. Test it against [`remote::memory::MemoryRemote`]: publish files at
//!    URLs, serve S3 listings with `serve_s3`, run a `ModelTask` or
//!    `ObservedTask` cycle at a fixed time, and check the catalog and store;
//!    put live checks behind `#[ignore]`.
//!
//! No family schedules, retries, decodes on its own threads, touches the
//! catalog or writes to the store.

pub(crate) mod driver;
pub(crate) mod families;
pub(crate) mod idx;
pub(crate) mod remote;

#[cfg(all(
    test,
    feature = "radar",
    feature = "satellite",
    feature = "nbm",
    feature = "arome"
))]
mod bench;
mod clock;
mod engine;
mod janitor;
mod limits;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod testing;

use driver::model::ModelTask;
use driver::observed::ObservedTask;
use engine::Engine;
use families::Adapter;
use object_store::ObjectStore;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wx_core::{
    Backoff, FrameCatalog, HealthRegistry, LAYERS, LayerSpec, SOURCES, SourceDef, spawn_feed,
};

pub use janitor::DELETE_GRACE;
pub use remote::{HostPolicy, Traffic};

#[derive(Debug, Clone)]
pub struct IngestConfig {
    /// What to run: family names (`"mrms"`, every source of the family) or
    /// source ids (`"mrms_ak"`). The default is every source of every family
    /// that is enabled in the registry and has an adapter. Empty runs
    /// nothing but the janitor.
    pub feeds: Vec<String>,
    /// Sent with every request. Providers ask for a way to reach whoever
    /// runs the client.
    pub user_agent: String,
    /// Forecast parts are never fetched for valid times further ahead of now
    /// than this, whatever a family's own horizon.
    pub horizon_hours: u32,
    /// Furthest HRRR forecast hour: 18 covers every run, up to 48 uses the
    /// four long runs a day.
    pub hrrr_max_hour: u32,
    /// Parts decoded and built at once across all sources. Decoding a
    /// 24-million-point radar mosaic takes a few hundred megabytes.
    pub max_builds: usize,
    /// Requests in flight at once across all hosts.
    pub max_downloads: usize,
    /// Politeness per host, over the defaults ([`HostPolicy::default_for`]):
    /// a pattern matches a host equal to it or ending in `.` and it.
    pub hosts: Vec<(String, HostPolicy)>,
}

impl IngestConfig {
    /// Everything but the selection.
    fn base() -> Self {
        Self {
            feeds: Vec::new(),
            user_agent: concat!("weather-map-ingest/", env!("CARGO_PKG_VERSION")).to_string(),
            horizon_hours: 240,
            hrrr_max_hour: 18,
            max_builds: 2,
            max_downloads: 8,
            hosts: Vec::new(),
        }
    }

    /// Whether `name` is a family or a source id of the registry, so a
    /// selection naming it is understood.
    pub fn is_known(name: &str) -> bool {
        SOURCES
            .iter()
            .any(|s| s.id == name || s.family.as_str() == name)
    }

    /// Families that are enabled in the registry and have an adapter, by name.
    pub fn supported() -> Vec<String> {
        let base = Self::base();
        let mut out: Vec<String> = Vec::new();
        for s in SOURCES {
            let name = s.family.as_str().to_string();
            if s.family.enabled() && families::adapter(s, &base).is_some() && !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            feeds: Self::supported(),
            ..Self::base()
        }
    }
}

/// What follows one source: a model with the products the source feeds,
/// or an observation family on one of them.
enum Kind {
    Model(Arc<dyn driver::Model>, Vec<&'static str>),
    Observed(Arc<dyn driver::Observed>, &'static str),
}

/// A running task, as `spawn_feed` takes it.
type Task = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;

/// One supervised task to start.
struct TaskSpec {
    name: String,
    source: &'static SourceDef,
    kind: Kind,
    every: Duration,
}

/// The ingest subsystem. Cheap to clone.
#[derive(Clone)]
pub struct Ingest {
    engine: Arc<Engine>,
    tasks: Arc<Vec<TaskSpec>>,
}

impl std::fmt::Debug for Ingest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ingest")
            .field("tasks", &self.task_names())
            .finish()
    }
}

impl Ingest {
    /// `store` receives grids under the keys in `wx_core::keys`. Builds
    /// clients only: no I/O. Fails on a selection naming an unknown family
    /// or source, one that is not enabled, has no adapter yet, or whose
    /// region the adapter does not know.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        catalog: Arc<FrameCatalog>,
        config: IngestConfig,
    ) -> anyhow::Result<Self> {
        let remote = remote::Remote::new(remote::RemoteConfig {
            user_agent: config.user_agent.clone(),
            max_downloads: config.max_downloads,
            hosts: config.hosts.clone(),
            ..remote::RemoteConfig::default()
        })?;
        Self::with_remote(store, catalog, config, remote)
    }

    /// [`Ingest::new`] over another remote (tests use an in-memory one).
    pub(crate) fn with_remote(
        store: Arc<dyn ObjectStore>,
        catalog: Arc<FrameCatalog>,
        config: IngestConfig,
        remote: remote::Remote,
    ) -> anyhow::Result<Self> {
        Self::with_registry(store, catalog, config, remote, LAYERS, SOURCES)
    }

    fn with_registry(
        store: Arc<dyn ObjectStore>,
        catalog: Arc<FrameCatalog>,
        config: IngestConfig,
        remote: remote::Remote,
        layers: &'static [LayerSpec],
        sources: &'static [SourceDef],
    ) -> anyhow::Result<Self> {
        let selected = select(&config, sources)?;
        let mut tasks: Vec<TaskSpec> = Vec::new();
        for (source, explicit) in selected {
            let Some(adapter) = families::adapter(source, &config) else {
                anyhow::bail!("no adapter for the {} family yet", source.family.as_str());
            };
            let check = match &adapter {
                Adapter::Model(m) => m.check(source),
                Adapter::Observed(o) => o.check(source),
            };
            if let Err(e) = check {
                if explicit {
                    return Err(e.context(format!("source {}", source.id)));
                }
                tracing::warn!(source = source.id, "not followed: {e:#}");
                continue;
            }
            let products = driver::products_of(layers, source);
            match adapter {
                Adapter::Model(m) => tasks.push(TaskSpec {
                    name: source.id.to_string(),
                    source,
                    every: m
                        .cycle(source)
                        .every
                        .to_std()
                        .unwrap_or(Duration::from_secs(3600)),
                    kind: Kind::Model(m, products),
                }),
                Adapter::Observed(o) => {
                    for product in products {
                        let cadence = o.cadence(source, product);
                        let first = driver::inputs_of(layers, source)
                            .into_iter()
                            .find(|(_, i)| i.product == product)
                            .map_or(product, |(l, _)| l.id);
                        tasks.push(TaskSpec {
                            name: o.task_name(source, product, first),
                            source,
                            kind: Kind::Observed(o.clone(), product),
                            every: cadence
                                .stride
                                .unwrap_or(cadence.period)
                                .to_std()
                                .unwrap_or(Duration::from_secs(120)),
                        });
                    }
                }
            }
        }
        let mut names: Vec<&str> = tasks.iter().map(|t| t.name.as_str()).collect();
        names.sort_unstable();
        if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
            anyhow::bail!("two tasks are named {}", w[0]);
        }
        let engine = Engine::new(store, catalog, config, remote, layers);
        Ok(Self {
            engine: Arc::new(engine),
            tasks: Arc::new(tasks),
        })
    }

    /// Start every task plus `janitor`, which sweeps the catalog and deletes
    /// the grids and tiles of dropped parts. Task names are as
    /// [`Ingest::task_names`] lists them. Call once.
    pub fn spawn(&self, health: &HealthRegistry, cancel: CancellationToken) -> Vec<JoinHandle<()>> {
        let backoff = Backoff::default();
        let mut handles = Vec::new();
        for spec in self.tasks.iter() {
            // One state per task, kept across its restarts: what each run
            // listed and what went missing is not thrown away (and listed
            // again) because one request failed.
            let source = spec.source;
            let start: Box<dyn Fn(wx_core::FeedCtx) -> Task + Send + Sync> = match &spec.kind {
                Kind::Model(m, products) => {
                    let state = Arc::new(tokio::sync::Mutex::new(ModelTask::new(
                        self.engine.clone(),
                        m.clone(),
                        source,
                        products,
                    )));
                    Box::new(move |ctx| Box::pin(driver::model::run(state.clone(), ctx)))
                }
                Kind::Observed(o, product) => {
                    let state = Arc::new(tokio::sync::Mutex::new(ObservedTask::new(
                        self.engine.clone(),
                        o.clone(),
                        source,
                        product,
                    )));
                    Box::new(move |ctx| Box::pin(driver::observed::run(state.clone(), ctx)))
                }
            };
            handles.push(spawn_feed(
                &spec.name,
                spec.every,
                health,
                cancel.clone(),
                backoff,
                start,
            ));
        }
        let engine = self.engine.clone();
        handles.push(spawn_feed(
            "janitor",
            Duration::from_secs(60),
            health,
            cancel,
            backoff,
            move |ctx| {
                janitor::run(
                    engine.store.clone(),
                    engine.catalog.clone(),
                    engine.deletions.clone(),
                    engine.layers,
                    ctx,
                )
            },
        ));
        handles
    }

    /// Requests and bytes per host since start, busiest first.
    pub fn traffic(&self) -> Vec<Traffic> {
        self.engine.remote.traffic()
    }

    /// Names of the supervised tasks [`Ingest::spawn`] starts, in order:
    /// sources in registry order, then `janitor`.
    pub fn task_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tasks.iter().map(|t| t.name.clone()).collect();
        names.push("janitor".to_string());
        names
    }
}

/// The sources a selection names, in registry order, each with whether it
/// was named by its id (rather than through its family).
fn select(
    config: &IngestConfig,
    sources: &'static [SourceDef],
) -> anyhow::Result<Vec<(&'static SourceDef, bool)>> {
    for name in &config.feeds {
        let known = sources
            .iter()
            .any(|s| s.id == name || s.family.as_str() == name);
        anyhow::ensure!(known, "unknown family or source {name:?}");
    }
    let mut out = Vec::new();
    for s in sources {
        let by_id = config.feeds.iter().any(|n| n == s.id);
        let by_family = config.feeds.iter().any(|n| n == s.family.as_str());
        if !(by_id || by_family) {
            continue;
        }
        anyhow::ensure!(
            s.family.enabled(),
            "the {} family is not enabled in the registry",
            s.family.as_str()
        );
        out.push((s, by_id));
    }
    Ok(out)
}

/// A mutex's guard, also after a holder panicked: every update here leaves
/// its state consistent.
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests;
