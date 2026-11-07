//! The WMO register of official alerting sources
//! (`https://severeweather.wmo.int/json/sources.json`), and which feed of
//! each source is read.
//!
//! A snapshot of the register ships with the crate so a first start works
//! while that host is down; the live copy replaces it once fetched, and is
//! kept in the database between restarts.

use serde_json::Value;
use std::collections::HashSet;

/// The register as fetched on 2026-11-06.
pub(crate) const SNAPSHOT: &[u8] = include_bytes!("../../data/wmo-sources.json");

/// `(name, country name)` of a source in the bundled register.
pub(crate) fn snapshot_names(id: &str) -> Option<(String, Option<String>)> {
    static SOURCES: std::sync::LazyLock<Vec<Source>> =
        std::sync::LazyLock::new(|| parse(SNAPSHOT).expect("the bundled register parses"));
    SOURCES
        .iter()
        .find(|s| s.id == id)
        .map(|s| (s.name.clone(), s.country_name.clone()))
}

/// A live register with fewer sources than this is treated as broken.
const MIN_SOURCES: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Source {
    /// `sourceId`, e.g. `de-dwd-en`.
    pub id: String,
    /// ISO 3166-1 alpha-2 of the alerting authority, lower case.
    pub country: Option<String>,
    pub country_name: Option<String>,
    /// The authority's name as the register gives it.
    pub name: String,
    /// WMO Regional Association, 1 (Africa) to 6 (Europe). A source in
    /// several (the UK, Russia) is counted in the last one listed, which is
    /// where its home country is.
    pub region: Option<u8>,
    /// The register's point for the country (`latlng`), as (lat, lon). Alerts
    /// without a shape are listed at it.
    pub anchor: Option<(f64, f64)>,
    /// The one feed read for this source (see [`choose_feed`]).
    pub feed: String,
    /// The feed lists only alerts in effect now, so an alert that leaves it
    /// has ended (MeteoAlarm). Other feeds keep old items.
    pub lists_current_only: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RegisterError {
    #[error("register is not JSON: {0}")]
    Json(String),
    #[error("register has no sources list")]
    Shape,
    #[error("register lists only {0} usable sources")]
    TooFew(usize),
}

/// Every source worth reading: official, operating, with an http(s) feed,
/// and not one of the United States' (the NWS path covers those).
pub(crate) fn parse(body: &[u8]) -> Result<Vec<Source>, RegisterError> {
    let doc: Value =
        serde_json::from_slice(body).map_err(|e| RegisterError::Json(e.to_string()))?;
    let list = doc
        .get("sources")
        .and_then(Value::as_array)
        .ok_or(RegisterError::Shape)?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entry in list.iter().take(2000) {
        let s = entry.get("source").unwrap_or(entry);
        if let Some(source) = parse_source(s)
            && seen.insert(source.id.clone())
        {
            out.push(source);
        }
    }
    if out.len() < MIN_SOURCES {
        return Err(RegisterError::TooFew(out.len()));
    }
    Ok(out)
}

fn parse_source(s: &Value) -> Option<Source> {
    let text = |k: &str| {
        s.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let id = text("sourceId").filter(|id| {
        id.len() <= 64
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    })?;
    if s.get("sourceIsOfficial").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    if text("capAlertFeedStatus").is_some_and(|st| !st.eq_ignore_ascii_case("operating")) {
        return None;
    }
    let country = text("authorityCountry")
        .map(str::to_ascii_lowercase)
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_lowercase()));
    let feeds: Vec<String> = text("capAlertFeed")?
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .filter_map(|f| url::Url::parse(f).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
        .map(|u| u.to_string())
        .collect();
    // The United States is read from api.weather.gov by the NWS path.
    let us = country.as_deref() == Some("us")
        || id.starts_with("us-")
        || feeds
            .iter()
            .any(|f| host_of(f).is_some_and(|h| h == "api.weather.gov"));
    if us {
        return None;
    }
    let feed = choose_feed(&feeds)?;
    let lists_current_only = is_meteoalarm(&feed);
    let name = s
        .get("byLanguage")
        .and_then(Value::as_array)
        .and_then(|l| l.iter().find_map(|b| b.get("name").and_then(Value::as_str)))
        .map(|n| n.trim().chars().take(200).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.to_string());
    let region = text("region")
        .and_then(|r| r.split(',').next_back())
        .and_then(|r| r.trim().parse::<u8>().ok());
    let anchor = text("latlng").and_then(|ll| {
        let (lat, lon) = ll.split_once(',')?;
        let (lat, lon): (f64, f64) = (lat.trim().parse().ok()?, lon.trim().parse().ok()?);
        (lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0)
            .then_some((lat, lon))
    });
    Some(Source {
        id: id.to_string(),
        country,
        country_name: text("countryName").map(|n| n.chars().take(100).collect()),
        name,
        region,
        anchor,
        feed,
        lists_current_only,
    })
}

/// One feed per source. Sources that list several give the same alerts
/// once per language, each copy a separate CAP message with its own
/// identifier; reading them all would draw every alert several times. The
/// English one is read when there is one (most messages carry every
/// language anyway), else the first.
fn choose_feed(feeds: &[String]) -> Option<String> {
    feeds
        .iter()
        .find(|f| names_english(f))
        .or_else(|| feeds.first())
        .cloned()
}

fn names_english(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let path_and_query = lower
        .split_once("://")
        .map_or(lower.as_str(), |(_, rest)| rest);
    let path_and_query = path_and_query.split_once('/').map_or("", |(_, rest)| rest);
    path_and_query
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|t| t == "en" || t == "eng" || t == "english")
}

/// MeteoAlarm's per-country Atom (`feeds.meteoalarm.org/feeds/meteoalarm-legacy-atom-<country>`).
fn is_meteoalarm(feed: &str) -> bool {
    host_of(feed).is_some_and(|h| h == "feeds.meteoalarm.org")
        || feed.contains("/meteoalarm-legacy-atom-")
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_snapshot_parses_without_the_united_states() {
        let sources = parse(SNAPSHOT).unwrap();
        assert_eq!(
            sources.len(),
            154,
            "156 in the register, less the two US feeds"
        );
        assert!(sources.iter().all(|s| s.country.as_deref() != Some("us")));
        assert!(sources.iter().all(|s| !s.feed.contains("api.weather.gov")));
        let ids: HashSet<&str> = sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids.len(), sources.len());

        let dwd = sources.iter().find(|s| s.id == "de-dwd-en").unwrap();
        assert_eq!(
            dwd.feed,
            "https://www.dwd.de/DWD/warnungen/cap-feed/en/rss.xml"
        );
        assert_eq!(dwd.country.as_deref(), Some("de"));
        assert_eq!(dwd.region, Some(6));
        assert!(!dwd.lists_current_only);

        let bg = sources.iter().find(|s| s.id == "bg-meteo-bg").unwrap();
        assert!(
            bg.lists_current_only,
            "MeteoAlarm lists current alerts only"
        );
        assert_eq!(
            bg.feed,
            "https://feeds.meteoalarm.org/feeds/meteoalarm-legacy-atom-bulgaria"
        );

        // Indonesia lists Indonesian first; the English feed is read.
        let id = sources.iter().find(|s| s.id == "id-inatews-id").unwrap();
        assert_eq!(id.feed, "https://www.bmkg.go.id/alerts/nowcast/en");
        let uk = sources.iter().find(|s| s.id == "uk-metoffice-en").unwrap();
        assert_eq!(uk.region, Some(6), "the last of several regions");
        assert!(uk.anchor.is_some());
    }

    #[test]
    fn names_of_sources_that_left_the_register() {
        let (name, country) = snapshot_names("de-dwd-en").unwrap();
        assert!(name.starts_with("Germany: "), "{name}");
        assert_eq!(country.as_deref(), Some("Germany"));
        assert!(
            snapshot_names("us-noaa-nws-en").is_none(),
            "the US is not a world source"
        );
        assert!(snapshot_names("xx-nowhere").is_none());
    }

    #[test]
    fn feed_choice() {
        let f = |v: &[&str]| choose_feed(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(
            f(&["https://x.org/ar/rss.xml", "https://x.org/en/rss.xml"]).unwrap(),
            "https://x.org/en/rss.xml"
        );
        assert_eq!(
            f(&["https://a/feed?lang=no", "https://a/feed?lang=en"]).unwrap(),
            "https://a/feed?lang=en"
        );
        assert_eq!(
            f(&["https://a/rss_fi-FI.rss", "https://a/rss_en-GB.rss"]).unwrap(),
            "https://a/rss_en-GB.rss"
        );
        assert_eq!(
            f(&["https://a/x-pt/rss.xml", "https://b/y"]).unwrap(),
            "https://a/x-pt/rss.xml"
        );
        // "en" in a host name says nothing about the language.
        assert_eq!(
            f(&["https://a/1", "https://en.example/2"]).unwrap(),
            "https://a/1"
        );
        assert!(f(&[]).is_none());
    }

    #[test]
    fn hostile_registers_are_refused() {
        assert!(parse(b"<html>").is_err());
        assert!(parse(br#"{"sources": []}"#).is_err());
        let one = br#"{"sources":[{"source":{"sourceId":"xx-a","capAlertFeed":"https://a.example/rss"}}]}"#;
        assert!(matches!(parse(one), Err(RegisterError::TooFew(1))));
        let bad = serde_json::json!({ "sourceId": "../../etc", "capAlertFeed": "https://a/b" });
        assert!(parse_source(&bad).is_none());
        let ftp = serde_json::json!({ "sourceId": "xx-a", "capAlertFeed": "ftp://a/b, file:///etc/passwd" });
        assert!(parse_source(&ftp).is_none());
    }
}
