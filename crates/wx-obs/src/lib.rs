//! Point data: station observations (airport METARs, NDBC buoys, JMA AMeDAS)
//! shown as dots on the map, and point forecasts for a tapped location (NWS
//! inside its coverage, MET Norway elsewhere, optionally the server's own
//! model as a last resort). Owns the HTTP routes for both.

mod amedas;
mod forecast;
mod metar;
mod metno;
mod model;
mod ndbc;
mod point;
mod stations;

use axum::Json;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use metar::Source;
use moka::future::Cache;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wx_core::{Backoff, FeedCtx, HealthRegistry, Hub, UpstreamGate, spawn_feed};

pub use model::{DailyPoint, HourlyPoint, PointForecast, PointModel};

#[derive(Debug, Clone)]
pub struct ObsConfig {
    /// Sent as `User-Agent` to every upstream. api.weather.gov and MET Norway
    /// both require it to identify the application and give a contact (a
    /// project URL or a non-personal address); the operator sets it.
    pub user_agent: String,
    /// `https://api.weather.gov` outside tests.
    pub nws_api_base: String,
    /// Where current METARs come from.
    pub metar_url: String,
    /// NDBC `latest_obs.txt`; `None` turns the buoy feed off.
    pub ndbc_url: Option<String>,
    /// Root of the JMA AMeDAS files (`https://www.jma.go.jp/bosai/amedas`);
    /// `None` turns the Japan feed off.
    pub amedas_base: Option<String>,
    /// `https://api.met.no` outside tests.
    pub met_api_base: String,
    /// The shortest time between requests for one point outside the Nordic
    /// region (30 minutes inside it). MET Norway asks for 3 hours; only tests
    /// lower it.
    pub met_min_refetch: Duration,
    /// Shared with every other user of api.weather.gov: paused when it
    /// rate-limits or blocks us, so all callers stand down together.
    pub nws_gate: UpstreamGate,
    /// The same for api.met.no, kept apart so one outage does not pause the other.
    pub met_gate: UpstreamGate,
}

impl Default for ObsConfig {
    fn default() -> Self {
        Self {
            user_agent: "weather-map".to_string(),
            nws_api_base: "https://api.weather.gov".to_string(),
            metar_url: "https://aviationweather.gov/data/cache/metars.cache.csv.gz".to_string(),
            ndbc_url: Some("https://www.ndbc.noaa.gov/data/latest_obs/latest_obs.txt".to_string()),
            amedas_base: Some("https://www.jma.go.jp/bosai/amedas".to_string()),
            met_api_base: "https://api.met.no".to_string(),
            met_min_refetch: Duration::from_secs(3 * 3600),
            nws_gate: UpstreamGate::new(),
            met_gate: UpstreamGate::new(),
        }
    }
}

const DAY: Duration = Duration::from_secs(24 * 3600);

pub(crate) fn round(v: f64, decimals: i32) -> f64 {
    let k = 10f64.powi(decimals);
    (v * k).round() / k
}

/// Most values are sent to one decimal.
pub(crate) fn round1(v: f64) -> f64 {
    round(v, 1)
}

pub(crate) fn header(headers: &HeaderMap, name: HeaderName) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_string)
}

/// The error body of every route: `{ "error": code, "message": ... }`.
pub(crate) fn error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = Json(serde_json::json!({ "error": code, "message": message }));
    (status, body).into_response()
}

#[derive(Clone)]
pub(crate) enum Lookup {
    Found(Arc<forecast::PointInfo>),
    Outside,
}

pub(crate) type GridKey = (String, u32, u32);

pub(crate) struct Inner {
    pub hub: Hub,
    pub config: ObsConfig,
    pub http: reqwest::Client,
    /// The fallback forecast supplied by the server, if any.
    pub model: Option<Arc<dyn PointModel>>,
    pub stations: stations::Store,
    /// Per feed, indexed by [`Source`]; outlives restarts of the feed task.
    pub feeds: [Mutex<stations::FeedMemory>; 3],
    pub metar_names: Mutex<stations::Table<HashMap<String, String>>>,
    pub amedas_stations: Mutex<stations::Table<HashMap<String, amedas::AmedasStation>>>,
    /// Points lookups, keyed by coordinates in 1e-3 degrees. A 404 is cached
    /// too: coverage does not change from minute to minute.
    pub points: Cache<(i64, i64), Lookup>,
    /// Caps our concurrent requests to api.weather.gov.
    pub upstream_slots: tokio::sync::Semaphore,
    /// Keys whose last fetch failed, remembered briefly so a burst of taps
    /// during an outage does not each start a fresh chain of requests.
    pub points_failed: Cache<(i64, i64), String>,
    pub grid_failed: Cache<GridKey, String>,
    /// Assembled NWS forecasts, keyed by grid square; concurrent misses share one fetch.
    pub forecasts: Cache<GridKey, Arc<PointForecast>>,
    /// Longer-lived copies of the above, served only when a refresh fails.
    pub stale: Cache<GridKey, Arc<PointForecast>>,
    /// Caps our concurrent requests to api.met.no.
    pub met_slots: tokio::sync::Semaphore,
    /// MET Norway forecasts per point (rounded to 0.01 degree), each behind
    /// its own lock so a point is fetched by one tap at a time and never
    /// before it expires. Idle points are dropped after a day.
    pub met_points: Cache<metno::Key, metno::Slot>,
    pub met_failed: Cache<metno::Key, String>,
}

fn cache<K, V>(n: u64, ttl: Duration) -> Cache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    Cache::builder().max_capacity(n).time_to_live(ttl).build()
}

/// The observations subsystem. Cheap to clone.
#[derive(Clone)]
pub struct Obs {
    inner: Arc<Inner>,
}

impl Obs {
    /// `model` answers point forecasts when the outside providers cannot.
    pub fn new(
        hub: Hub,
        config: ObsConfig,
        model: Option<Arc<dyn PointModel>>,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            inner: Arc::new(Inner {
                hub,
                config,
                http,
                model,
                stations: stations::Store::new(),
                feeds: Default::default(),
                metar_names: Default::default(),
                amedas_stations: Default::default(),
                points: cache(20_000, DAY),
                upstream_slots: tokio::sync::Semaphore::new(point::MAX_IN_FLIGHT),
                points_failed: cache(5_000, point::FAILURE_MEMORY),
                grid_failed: cache(2_000, point::FAILURE_MEMORY),
                forecasts: cache(2_000, Duration::from_secs(600)),
                stale: cache(2_000, Duration::from_secs(6 * 3600)),
                met_slots: tokio::sync::Semaphore::new(metno::MAX_IN_FLIGHT),
                met_points: Cache::builder()
                    .max_capacity(5_000)
                    .time_to_idle(DAY)
                    .build(),
                met_failed: cache(2_000, point::FAILURE_MEMORY),
            }),
        })
    }

    /// Drop the fresh forecast cache so the next tap refreshes (or falls back
    /// to the stale copy). For tests of that fallback.
    #[doc(hidden)]
    pub fn expire_fresh_forecasts(&self) {
        self.inner.forecasts.invalidate_all();
    }

    /// Start the supervised station feeds: `stations` (airports), and
    /// `stations-ndbc` and `stations-amedas` unless switched off in the config.
    /// NDBC publishes every 10 minutes; for AMeDAS only the tiny
    /// `latest_time.txt` is fetched every 5.
    pub fn spawn(&self, health: &HealthRegistry, cancel: CancellationToken) -> Vec<JoinHandle<()>> {
        let c = &self.inner.config;
        let feeds = [
            ("stations", Source::Metar, 5, Some(&c.metar_url)),
            ("stations-ndbc", Source::Ndbc, 10, c.ndbc_url.as_ref()),
            ("stations-amedas", Source::Amedas, 5, c.amedas_base.as_ref()),
        ];
        let mut handles = Vec::new();
        for (name, source, minutes, url) in feeds {
            let Some(url) = url.cloned() else { continue };
            let (inner, interval) = (self.inner.clone(), Duration::from_secs(minutes * 60));
            let run = move |ctx: FeedCtx| {
                let (inner, url) = (inner.clone(), url.clone());
                async move {
                    loop {
                        stations::refresh(&inner, &ctx, source, &url).await?;
                        if !ctx.sleep(interval).await {
                            return Ok(());
                        }
                    }
                }
            };
            let handle = spawn_feed(
                name,
                interval,
                health,
                cancel.clone(),
                Backoff::default(),
                run,
            );
            handles.push(handle);
        }
        handles
    }

    /// Routes for `/api/stations` and `/api/point`, with their state already
    /// attached. Paths are absolute; the server merges this router as is.
    pub fn router(&self) -> axum::Router {
        axum::Router::new()
            .route("/api/stations", axum::routing::get(stations::handler))
            .route("/api/point", axum::routing::get(point::handler))
            .with_state(self.inner.clone())
    }
}
