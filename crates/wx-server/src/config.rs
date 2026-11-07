//! Configuration from the environment. Every setting has a development
//! default except the database, so `cargo run -p wx-server` works after
//! `docker compose up -d db`.

use anyhow::{Context, bail};
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use wx_alerts::Vapid;
use wx_ingest::IngestConfig;

/// Names accepted in `FEEDS` besides the families and sources of the
/// registry: alerts (the NWS and the world's feeds) and station reports.
const OTHER_FEEDS: [&str; 2] = ["alerts", "stations"];

/// Used when the operator has not said who they are. Providers that require
/// a contact (the NWS, MET Norway) accept it for a while and may then refuse.
const DEFAULT_USER_AGENT: &str = "weather-map-dev";

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: SocketAddr,
    pub store_url: String,
    pub s3: S3Config,
    /// What to run. `None` is everything.
    pub feeds: Option<BTreeSet<String>>,
    /// Sent to every upstream host: who runs this server and how to reach
    /// them, for example `my-weather-map (https://example.org/contact)`.
    pub user_agent: String,
    /// Read alerts from the world's national feeds as well as the NWS.
    pub world_alerts: bool,
    pub vapid: Option<Vapid>,
    pub web_dist: PathBuf,
    /// Local PMTiles archive served at `/basemap/world.pmtiles` when it exists.
    pub basemap_path: PathBuf,
    /// Basemap hosted elsewhere (overrides the local file).
    pub basemap_url: Option<String>,
    pub hrrr_max_hour: u32,
    /// How far ahead of now forecasts are fetched, in hours.
    pub forecast_hours: u32,
    /// Parts decoded and built at once.
    pub max_builds: usize,
    /// Memory budget for grids kept warm for rendering tiles.
    pub grid_cache_mb: u64,
}

#[derive(Debug, Clone, Default)]
pub struct S3Config {
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn parsed<T: std::str::FromStr>(name: &str, default: T) -> anyhow::Result<T>
where
    T::Err: std::fmt::Display,
{
    match var(name) {
        Some(v) => v
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("{name}={v:?} is not valid: {e}")),
        None => Ok(default),
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = var("DATABASE_URL").context(
            "DATABASE_URL is not set. Start the database with `docker compose up -d db` \
             and copy .env.example to .env",
        )?;

        let feeds = match std::env::var("FEEDS") {
            // Set but empty means "run no feeds", which is useful for UI work.
            Ok(v) => Some(parse_feeds(&v)?),
            Err(_) => None,
        };

        let vapid = match (var("VAPID_PUBLIC_KEY"), var("VAPID_PRIVATE_KEY")) {
            (Some(public_key), Some(private_key)) => Some(Vapid {
                public_key,
                private_key,
                subject: var("VAPID_SUBJECT")
                    .context("VAPID_SUBJECT must be set alongside the VAPID keys")?,
            }),
            (None, None) => None,
            _ => bail!("set both VAPID_PUBLIC_KEY and VAPID_PRIVATE_KEY, or neither"),
        };

        Ok(Self {
            database_url,
            bind_addr: parsed("BIND_ADDR", SocketAddr::from(([127, 0, 0, 1], 8080)))?,
            store_url: var("STORE_URL").unwrap_or_else(|| "file://./data/store".to_string()),
            s3: S3Config {
                endpoint: var("S3_ENDPOINT"),
                region: var("S3_REGION"),
                access_key_id: var("S3_ACCESS_KEY_ID"),
                secret_access_key: var("S3_SECRET_ACCESS_KEY"),
            },
            feeds,
            user_agent: var("UPSTREAM_USER_AGENT")
                .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string()),
            world_alerts: parsed("WORLD_ALERTS", true)?,
            vapid,
            web_dist: var("WEB_DIST").map_or_else(|| PathBuf::from("./web/dist"), PathBuf::from),
            basemap_path: var("BASEMAP_PATH").map_or_else(
                || PathBuf::from("./data/basemap/world.pmtiles"),
                PathBuf::from,
            ),
            basemap_url: var("BASEMAP_URL"),
            hrrr_max_hour: parsed("HRRR_MAX_HOUR", 18)?,
            forecast_hours: parsed("FORECAST_HOURS", 168)?,
            max_builds: parsed("MAX_BUILDS", 2)?,
            grid_cache_mb: parsed("GRID_CACHE_MB", 600)?,
        })
    }

    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self {
            database_url: String::new(),
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            store_url: "memory://".to_string(),
            s3: S3Config::default(),
            feeds: Some(BTreeSet::new()),
            user_agent: "weather-map-test".to_string(),
            world_alerts: false,
            vapid: None,
            web_dist: PathBuf::from("./web/dist"),
            basemap_path: PathBuf::from("./does-not-exist.pmtiles"),
            basemap_url: None,
            hrrr_max_hour: 18,
            forecast_hours: 168,
            max_builds: 2,
            grid_cache_mb: 64,
        }
    }

    /// Whether `alerts` or `stations` is to run.
    pub fn feed_enabled(&self, name: &str) -> bool {
        self.feeds.as_ref().is_none_or(|f| f.contains(name))
    }

    /// Whether the operator has identified themselves to upstream hosts.
    pub fn has_own_user_agent(&self) -> bool {
        self.user_agent != DEFAULT_USER_AGENT
    }

    /// The sources the ingest subsystem will follow, by id.
    pub fn enabled_source_ids(&self) -> Vec<&'static str> {
        let names = self.ingest_feeds();
        wx_core::SOURCES
            .iter()
            .filter(|s| names.iter().any(|n| n == s.id || n == s.family.as_str()))
            .map(|s| s.id)
            .collect()
    }

    /// The families and sources to hand to the ingest subsystem.
    pub fn ingest_feeds(&self) -> Vec<String> {
        match &self.feeds {
            None => IngestConfig::supported(),
            Some(names) => names
                .iter()
                .filter(|n| !OTHER_FEEDS.contains(&n.as_str()))
                .cloned()
                .collect(),
        }
    }
}

fn parse_feeds(value: &str) -> anyhow::Result<BTreeSet<String>> {
    let mut feeds = BTreeSet::new();
    for name in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !OTHER_FEEDS.contains(&name) && !IngestConfig::is_known(name) {
            bail!(
                "FEEDS contains unknown feed {name:?}; use alerts, stations, a family \
                 (mrms, hrrr, icon, ...) or a source id (mrms_ak, icon_d2, ...); \
                 docs/sources/README.md lists them"
            );
        }
        feeds.insert(name.to_string());
    }
    Ok(feeds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feeds_parse() {
        let f = parse_feeds("mrms, alerts,,stations, icon_d2").unwrap();
        assert_eq!(f.len(), 4);
        assert!(f.contains("mrms") && f.contains("alerts") && f.contains("icon_d2"));
        assert!(parse_feeds("").unwrap().is_empty());
        assert!(parse_feeds("mrms,nexrad").is_err());
    }

    #[test]
    fn ingest_gets_only_its_own_names() {
        let mut cfg = Config::for_tests();
        cfg.feeds = Some(parse_feeds("alerts,stations,hrrr,mrms_ak").unwrap());
        assert_eq!(
            cfg.ingest_feeds(),
            vec!["hrrr".to_string(), "mrms_ak".to_string()]
        );
        assert!(cfg.feed_enabled("alerts") && !cfg.feed_enabled("nope"));
        let ids = cfg.enabled_source_ids();
        assert!(ids.contains(&"hrrr") && ids.contains(&"hrrr_ak") && ids.contains(&"mrms_ak"));
        assert!(!ids.contains(&"mrms") && !ids.contains(&"gfs"));
        cfg.feeds = None;
        assert!(cfg.feed_enabled("stations"));
        assert!(cfg.ingest_feeds().contains(&"gfs".to_string()));
    }
}
