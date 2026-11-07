//! The weather map server: one process that runs every data feed, serves the
//! API and tiles, and pushes live updates to open browsers.
//!
//! ```text
//! wx-server            run the server (same as `wx-server serve`)
//! wx-server gen-vapid  print a new Web Push key pair for .env
//! ```

mod body;
mod config;
mod frames;
mod persist;
mod routes;
mod store;
mod tiles;
mod ws;

use anyhow::{Context, bail};
use axum::Router;
use axum::http::{HeaderValue, header};
use chrono::Utc;
use config::Config;
use routes::AppState;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use wx_alerts::{Alerts, AlertsConfig};
use wx_core::{FrameCatalog, HealthRegistry, Hub, UpstreamGate};
use wx_ingest::{Ingest, IngestConfig};
use wx_obs::{Obs, ObsConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // A missing .env is fine: the environment may already be set (production).
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn,sqlx::query=error".into()),
        )
        .init();

    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve(Config::from_env()?).await,
        Some("gen-vapid") => {
            let (public, private) = wx_alerts::generate_vapid_keys();
            println!("VAPID_PUBLIC_KEY={public}");
            println!("VAPID_PRIVATE_KEY={private}");
            Ok(())
        }
        Some(other) => bail!("unknown command {other:?}; expected `serve` or `gen-vapid`"),
    }
}

async fn serve(cfg: Config) -> anyhow::Result<()> {
    // Every query here is short. PostgreSQL's JIT compiles the costlier
    // ones (alert display shapes, low-zoom alert tiles) and spends
    // longer compiling than they take to run: half a second per publish.
    let options = PgConnectOptions::from_str(&cfg.database_url)
        .context("DATABASE_URL is not a PostgreSQL URL")?
        .options([("jit", "off")]);
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options)
        .await
        .context("connecting to PostgreSQL (is `docker compose up -d db` running?)")?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("applying database migrations")?;

    // Held until the process exits.
    let _instance_lock = acquire_instance_lock(&pool).await?;

    let hub = Hub::new();
    let health = HealthRegistry::new(hub.clone());
    let catalog = Arc::new(FrameCatalog::new(hub.clone()));
    // Before anything is announced, so every connection hears all of it.
    let live = ws::Live::spawn(&hub);
    let store = store::open(&cfg)?;
    let cancel = CancellationToken::new();

    // Anchored layers wait for their global base on a cold start; they must
    // not wait for a source that is not running.
    let enabled = cfg.enabled_source_ids();
    catalog.set_enabled_sources(&enabled);
    let (restored, saved) = persist::restore(&pool, &catalog, &store).await?;
    tracing::info!(parts = restored, "restored the catalog");

    let mut tasks: Vec<JoinHandle<()>> = Vec::new();
    tasks.push(persist::spawn_mirror(
        pool.clone(),
        catalog.clone(),
        hub.clone(),
        saved,
        cancel.clone(),
    ));

    if !cfg.has_own_user_agent() {
        tracing::warn!(
            "UPSTREAM_USER_AGENT is not set. The NWS and MET Norway ask every client to say \
             who runs it and how to reach them, and may refuse this one"
        );
    }

    let ingest = Ingest::new(
        store.clone(),
        catalog.clone(),
        IngestConfig {
            feeds: cfg.ingest_feeds(),
            user_agent: cfg.user_agent.clone(),
            horizon_hours: cfg.forecast_hours,
            hrrr_max_hour: cfg.hrrr_max_hour,
            max_builds: cfg.max_builds,
            ..IngestConfig::default()
        },
    )?;
    // Always spawned: with no sources enabled this is just the janitor,
    // which is still needed to expire restored parts.
    tasks.extend(ingest.spawn(&health, cancel.clone()));

    // Alerts, zone boundaries and point forecasts all call api.weather.gov;
    // when it tells one of them to back off, all of them must.
    let nws_gate = UpstreamGate::new();

    let mut alerts_config = AlertsConfig {
        user_agent: cfg.user_agent.clone(),
        vapid: cfg.vapid.clone(),
        nws_gate: nws_gate.clone(),
        ..AlertsConfig::default()
    };
    alerts_config.world.enabled = cfg.world_alerts;
    alerts_config.world.user_agent = cfg.user_agent.clone();
    let alerts = Alerts::new(pool.clone(), hub.clone(), alerts_config)?;
    if cfg.feed_enabled("alerts") {
        tasks.extend(alerts.spawn(&health, cancel.clone()));
    }

    let obs = Obs::new(
        hub.clone(),
        ObsConfig {
            user_agent: cfg.user_agent.clone(),
            nws_gate: nws_gate.clone(),
            ..ObsConfig::default()
        },
        None,
    )?;
    if cfg.feed_enabled("stations") {
        tasks.extend(obs.spawn(&health, cancel.clone()));
    }

    let local_basemap = cfg.basemap_path.is_file();
    let basemap_url = cfg
        .basemap_url
        .clone()
        .or_else(|| local_basemap.then(|| routes::BASEMAP_ROUTE.to_string()));
    if basemap_url.is_none() {
        tracing::warn!(
            path = %cfg.basemap_path.display(),
            "no basemap archive found; run scripts/fetch-basemap.sh or set BASEMAP_URL"
        );
    }

    let state = AppState {
        tiles: Arc::new(tiles::TileService::new(
            store,
            catalog.clone(),
            cfg.grid_cache_mb,
        )),
        live,
        timelines: Arc::new(frames::Timelines::new(catalog.clone())),
        cfg: Arc::new(cfg.clone()),
        health,
        catalog,
        cancel: cancel.clone(),
        started_at: Utc::now(),
        clients: Arc::new(AtomicUsize::new(0)),
        basemap_url,
        traffic: {
            let ingest = ingest.clone();
            Arc::new(move || ingest.traffic())
        },
    };

    let api = routes::api(state.clone())
        .merge(alerts.router())
        .merge(obs.router())
        .layer(CompressionLayer::new());
    let mut app = Router::new().merge(api).merge(routes::raw(state));
    if local_basemap && cfg.basemap_url.is_none() {
        // ServeFile answers HTTP range requests, which is how PMTiles is read.
        app = app.route_service(routes::BASEMAP_ROUTE, ServeFile::new(&cfg.basemap_path));
    }
    let app = app
        .fallback_service(web_client(&cfg))
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(cfg.bind_addr)
        .await
        .with_context(|| format!("binding {}", cfg.bind_addr))?;
    tracing::info!(
        feeds = ?cfg.feeds,
        "weather map listening on http://{}",
        listener.local_addr()?
    );

    let shutdown = cancel.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            tracing::info!("shutting down");
            shutdown.cancel();
        })
        .await?;

    // Give feeds a moment to stop cleanly and the catalog a last save.
    cancel.cancel();
    let stop = async {
        for task in tasks {
            let _ = task.await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(10), stop)
        .await
        .is_err()
    {
        tracing::warn!("some background tasks did not stop in time");
    }
    Ok(())
}

/// Make sure this is the only server working this database.
///
/// The feeds, the janitor and the catalog mirror all assume they are alone: a
/// second instance would delete parts the first one owns and double the load
/// on every provider. A session-level advisory lock on a dedicated connection enforces
/// it, and frees itself if the process dies. A previous instance that is still
/// shutting down (a rolling deploy) is waited for briefly.
async fn acquire_instance_lock(
    pool: &sqlx::PgPool,
) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
    // Arbitrary but fixed: "wxsrv" in ASCII.
    const LOCK_KEY: i64 = 0x77_78_73_72_76;
    let mut conn = pool
        .acquire()
        .await
        .context("connecting for the instance lock")?;
    for attempt in 0..45 {
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(LOCK_KEY)
            .fetch_one(&mut *conn)
            .await
            .context("taking the instance lock")?;
        if locked {
            return Ok(conn);
        }
        if attempt == 0 {
            tracing::warn!("another instance holds this database; waiting for it to stop");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!(
        "another weather-map server is already running against this database. \
         Only one instance may run at a time (on Fly: `fly scale count 1`)."
    )
}

/// The built web client, with `index.html` for any path that is not a file.
fn web_client(cfg: &Config) -> Router {
    let index = cfg.web_dist.join("index.html");
    if !index.is_file() {
        tracing::warn!(
            dist = %cfg.web_dist.display(),
            "web client is not built; run `npm run build` in web/ (the API still works)"
        );
    }
    let files = ServeDir::new(&cfg.web_dist).fallback(ServeFile::new(index));
    Router::new()
        .fallback_service(files)
        // The service worker and HTML must be revalidated so a deploy takes
        // effect; fingerprinted assets under /assets set their own lifetime
        // through the reverse proxy or stay default.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        // Scripts, styles and the WebAssembly decoder are text-like and
        // shrink several times; images are left alone by the layer.
        .layer(CompressionLayer::new())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
