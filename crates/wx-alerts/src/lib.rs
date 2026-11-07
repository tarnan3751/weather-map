//! Severe-weather alerts: poll the NWS active-alerts feed for the United
//! States and every official CAP feed in the WMO register for the rest of
//! the world, keep alerts as shapes in PostGIS, serve them as vector
//! tiles, find saved places inside them, and send Web Push notifications.
//! Also owns devices, saved places and push subscriptions, and the HTTP
//! routes for all of it.

// SQL fragments over `alerts`, as macros so that queries can be put
// together with `concat!` into constants.

/// When a world alert stops being in effect: its `expires`, or for a sender
/// that gave none a day after it starts.
macro_rules! until_sql {
    () => {
        "coalesce(expires, coalesce(onset, effective, sent) + interval '24 hours')"
    };
}

/// When an alert ends: NWS `ends`, else `expires` (for a world alert, as
/// [`until_sql`] has it).
macro_rules! ends_sql {
    () => {
        concat!(
            "coalesce(ends, CASE WHEN origin = 'cap' THEN ",
            until_sql!(),
            " ELSE expires END)"
        )
    };
}

/// The alert's CAP category, lower case, one of [`cache::CATEGORIES`], never
/// NULL: NWS rows keep theirs in the raw feature, and anything that is not
/// a CAP category is `other`.
macro_rules! category_sql {
    () => {
        "CASE WHEN lower(coalesce(category, raw->'properties'->>'category'))
                   IN ('met', 'geo', 'safety', 'security', 'rescue', 'fire', 'health',
                       'env', 'transport', 'infra', 'cbrne', 'other')
              THEN lower(coalesce(category, raw->'properties'->>'category')) ELSE 'other' END"
    };
}

/// Severity, `Unknown` for anything CAP does not define.
macro_rules! severity_sql {
    () => {
        "CASE WHEN severity IN ('Extreme', 'Severe', 'Moderate', 'Minor') THEN severity ELSE 'Unknown' END"
    };
}

/// Severity as a number for styling and ordering: 4 Extreme, 3 Severe,
/// 2 Moderate, 1 Minor, 0 Unknown.
macro_rules! level_sql {
    () => {
        "CASE severity WHEN 'Extreme' THEN 4 WHEN 'Severe' THEN 3 WHEN 'Moderate' THEN 2 WHEN 'Minor' THEN 1 ELSE 0 END"
    };
}

/// Most severe first, then newest.
macro_rules! order_sql {
    () => {
        concat!(level_sql!(), " DESC, sent DESC NULLS LAST, id")
    };
}

/// The columns behind `cache::AlertProps`.
macro_rules! props_sql {
    () => {
        concat!(
            "id, event, ",
            severity_sql!(),
            " AS severity, urgency, certainty, headline, area_desc, sent, onset, ends,
             CASE WHEN origin = 'cap' THEN ",
            until_sql!(),
            " ELSE expires END AS expires,
             (origin = 'cap' AND expires IS NULL) AS expires_assumed,
             geom_source, source, country, language, ",
            category_sql!(),
            " AS category"
        )
    };
}

/// Whether an alert notifies at all: from the NWS, events whose name ends
/// in "Warning"; from the rest of the world, CAP severity Severe or Extreme
/// with an urgency other than Past.
macro_rules! notifies_sql {
    () => {
        "(CASE WHEN origin = 'nws' THEN event LIKE '%Warning'
               ELSE severity IN ('Severe', 'Extreme') AND coalesce(urgency, '') <> 'Past' END)"
    };
}

mod api;
mod cache;
mod devices;
mod error;
mod feed;
mod notify;
mod nws;
mod push;
mod shapes;
mod tiles;
mod upstream;
mod validate;
mod vtec;
mod world;
mod zones;

pub use feed::PollReport;
pub use world::{NUTS_ATTRIBUTION, SourceHealth, WorldConfig, WorldReport};

use sqlx::PgPool;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wx_core::{Backoff, HealthRegistry, Hub, spawn_feed};

/// VAPID identity for Web Push.
#[derive(Clone)]
pub struct Vapid {
    /// Uncompressed P-256 public key, base64url without padding (what the
    /// browser's `applicationServerKey` expects).
    pub public_key: String,
    /// 32-byte P-256 private scalar, base64url without padding.
    pub private_key: String,
    /// `mailto:` or `https:` contact the push service can reach.
    pub subject: String,
}

// Configs get logged; the private key must never be.
impl std::fmt::Debug for Vapid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vapid")
            .field("public_key", &self.public_key)
            .field("private_key", &"<redacted>")
            .field("subject", &self.subject)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct AlertsConfig {
    /// Sent as `User-Agent` to api.weather.gov, which requires a contact.
    /// Never sent anywhere else: the world path has its own.
    pub user_agent: String,
    /// `https://api.weather.gov` outside tests.
    pub api_base: String,
    pub poll_interval: Duration,
    /// Without keys, alerts still work but nothing is pushed.
    pub vapid: Option<Vapid>,
    /// Accept `http://` and loopback push endpoints. Only for tests that
    /// run a local stand-in for a push service; never in production.
    pub allow_insecure_push_endpoints: bool,
    /// Zone boundaries fetched from api.weather.gov at once.
    pub zone_fetch_concurrency: usize,
    /// Minimum spacing between zone requests across all workers. A cold
    /// start needs about a thousand zones; unpaced, that burst gets the
    /// host refused (HTTP 403) by api.weather.gov's CDN.
    pub zone_fetch_interval: Duration,
    /// First delay before retrying a zone fetch or a push; doubles per try.
    pub retry_base_delay: Duration,
    /// Shared with everything else that calls api.weather.gov: a 403 or
    /// 429 pauses it, and nothing is sent there while it is paused.
    pub nws_gate: wx_core::UpstreamGate,
    /// Alerts from the rest of the world (the `alerts-world` feed).
    pub world: WorldConfig,
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            user_agent: "weather-map".to_string(),
            api_base: "https://api.weather.gov".to_string(),
            poll_interval: Duration::from_secs(30),
            vapid: None,
            allow_insecure_push_endpoints: false,
            zone_fetch_concurrency: 4,
            zone_fetch_interval: Duration::from_millis(250),
            retry_base_delay: Duration::from_secs(1),
            nws_gate: wx_core::UpstreamGate::new(),
            world: WorldConfig::default(),
        }
    }
}

/// Make a new VAPID key pair as `(public_key, private_key)`, both base64url.
pub fn generate_vapid_keys() -> (String, String) {
    push::vapid::generate()
}

/// The alerts subsystem. Cheap to clone.
#[derive(Clone)]
pub struct Alerts {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub pool: PgPool,
    pub hub: Hub,
    pub config: AlertsConfig,
    /// `api_base` without a trailing slash.
    pub api_base: String,
    pub nws: reqwest::Client,
    pub nws_limits: upstream::NwsLimits,
    pub push: push::PushClient,
    pub cache: cache::AlertsCache,
    pub zones: zones::ZoneQueue,
    pub poll: tokio::sync::Mutex<feed::PollState>,
    /// Held while alert rows are changed, by the poll cycle and the zone
    /// shaper, so "added"/"removed" are computed against a stable state.
    pub writes: tokio::sync::Mutex<()>,
    /// Notifications being delivered right now, so the sweep does not
    /// deliver them a second time.
    pub in_flight: Mutex<HashSet<(String, Uuid)>>,
    pub force_sweep: AtomicBool,
    /// When the safety-net sweep last ran (either feed runs it).
    pub last_sweep: Mutex<Option<Instant>>,
    /// The alert set's revision, which tile URLs carry.
    pub revision: tiles::Revision,
    pub tiles: tiles::TileCache,
    /// `None` when the world path is turned off.
    pub world: Option<world::World>,
}

impl Alerts {
    /// The pool's database already has the schema from `migrations/`.
    pub fn new(pool: PgPool, hub: Hub, config: AlertsConfig) -> anyhow::Result<Self> {
        let api_base = config.api_base.trim().trim_end_matches('/').to_string();
        let parsed = url::Url::parse(&api_base)
            .map_err(|e| anyhow::anyhow!("api_base {api_base:?}: {e}"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https"),
            "api_base must be an http(s) URL"
        );
        anyhow::ensure!(
            !config.user_agent.trim().is_empty(),
            "user_agent must not be empty"
        );
        anyhow::ensure!(
            config.zone_fetch_concurrency > 0,
            "zone_fetch_concurrency must be at least 1"
        );
        anyhow::ensure!(
            !config.poll_interval.is_zero(),
            "poll_interval must not be zero"
        );

        let nws = reqwest::Client::builder()
            .user_agent(config.user_agent.trim())
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        let signer = config
            .vapid
            .as_ref()
            .map(push::vapid::VapidSigner::new)
            .transpose()?;
        let push = push::PushClient::new(signer, config.retry_base_delay)?;
        let world = config
            .world
            .enabled
            .then(|| world::World::new(config.world.clone()))
            .transpose()?;
        Ok(Self {
            inner: Arc::new(Inner {
                pool,
                hub,
                api_base,
                nws,
                nws_limits: upstream::NwsLimits::new(
                    config.nws_gate.clone(),
                    config.retry_base_delay * 5,
                ),
                push,
                cache: cache::AlertsCache::default(),
                zones: zones::ZoneQueue::new(config.zone_fetch_interval),
                poll: tokio::sync::Mutex::new(feed::PollState::default()),
                writes: tokio::sync::Mutex::new(()),
                in_flight: Mutex::new(HashSet::new()),
                force_sweep: AtomicBool::new(false),
                last_sweep: Mutex::new(None),
                revision: tiles::Revision::new(),
                tiles: tiles::TileCache::new(),
                world,
                config,
            }),
        })
    }

    /// Start the supervised `alerts` feed (and any helper tasks it needs),
    /// and the `alerts-world` feed when the world path is on.
    ///
    /// Besides the feeds: `zone_fetch_concurrency` zone-boundary workers,
    /// the task that shapes alerts as their zones arrive, and an upkeep task
    /// that expires alerts and retries pushes while the feed is failing and
    /// its supervisor is backing off.
    pub fn spawn(&self, health: &HealthRegistry, cancel: CancellationToken) -> Vec<JoinHandle<()>> {
        let mut handles = Vec::new();
        let inner = self.inner.clone();
        handles.push(spawn_feed(
            "alerts",
            self.inner.config.poll_interval,
            health,
            cancel.clone(),
            Backoff::default(),
            move |ctx| {
                let inner = inner.clone();
                async move { inner.run_feed(ctx).await }
            },
        ));
        for _ in 0..self.inner.config.zone_fetch_concurrency {
            let inner = self.inner.clone();
            handles.push(keep_running(
                "alerts zone worker",
                cancel.clone(),
                move |c| zones::worker(inner.clone(), c),
            ));
        }
        let inner = self.inner.clone();
        handles.push(keep_running(
            "alerts zone shaper",
            cancel.clone(),
            move |c| zones::shaper(inner.clone(), c),
        ));
        if let Some(world) = &self.inner.world {
            let inner = self.inner.clone();
            handles.push(spawn_feed(
                "alerts-world",
                world.config.poll_interval,
                health,
                cancel.clone(),
                Backoff::default(),
                move |ctx| {
                    let inner = inner.clone();
                    async move { inner.run_world_feed(ctx).await }
                },
            ));
        }
        let inner = self.inner.clone();
        handles.push(keep_running("alerts upkeep", cancel, move |c| {
            inner.clone().upkeep(c)
        }));
        handles
    }

    /// One pass over every world source now, outside the feed's schedule:
    /// each feed once and the new documents it lists, then expiry, NUTS
    /// shapes and a publish. For tests and one-off checks. Errors when the
    /// world path is turned off.
    pub async fn world_poll_once(&self) -> anyhow::Result<WorldReport> {
        anyhow::ensure!(self.inner.world.is_some(), "the world path is turned off");
        self.inner.world_poll_once().await
    }

    /// Health of every world source, failing ones first.
    pub async fn world_sources(&self) -> Vec<SourceHealth> {
        match &self.inner.world {
            Some(_) => self.inner.world_sources().await,
            None => Vec::new(),
        }
    }

    /// The alert set's current revision.
    pub async fn revision(&self) -> i64 {
        self.inner.revision.get(&self.inner.pool).await
    }

    /// Routes for `/api/alerts…`, `/api/device`, `/api/places…` and
    /// `/api/push…`, with their state already attached. Paths are absolute
    /// (they start with `/api/`); the server merges this router as is.
    pub fn router(&self) -> axum::Router {
        api::router(self.clone())
    }

    /// Run one poll cycle now, outside the feed's schedule. The feed calls
    /// this itself; it is public for tests and one-off checks.
    pub async fn poll_once(&self) -> anyhow::Result<PollReport> {
        self.inner.poll_once().await
    }

    /// Zone boundaries queued or being fetched.
    pub fn zone_backlog(&self) -> usize {
        self.inner.zones.backlog()
    }
}

/// Locks here guard plain data that a panic cannot leave half-written, so a
/// poisoned one is used as it is.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Restart a helper task that panicked, until shutdown.
fn keep_running<F, Fut>(name: &'static str, cancel: CancellationToken, task: F) -> JoinHandle<()>
where
    F: Fn(CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            let result = tokio::spawn(task(cancel.clone())).await;
            if cancel.is_cancelled() {
                return;
            }
            match result {
                Err(e) if e.is_panic() => tracing::error!("{name} panicked; restarting"),
                _ => tracing::warn!("{name} stopped; restarting"),
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                _ = cancel.cancelled() => return,
            }
        }
    })
}
