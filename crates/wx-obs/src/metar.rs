//! The station record shared by every feed, and parsing of the
//! aviationweather.gov METAR cache.

use crate::round1;
use anyhow::{Context, bail};
use chrono::{DateTime, Duration, Utc};
use flate2::read::GzDecoder;
use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;

/// Reports older than this are not shown: the airport has stopped reporting.
pub const MAX_AGE: Duration = Duration::hours(3);

/// A cap on the decompressed size so a bad URL cannot exhaust memory.
const MAX_TEXT_BYTES: u64 = 64 * 1024 * 1024;

const KT_TO_MS: f64 = 1852.0 / 3600.0;
const MILE_M: f64 = 1609.344;
const INHG_HPA: f64 = 33.863_886_666_7;

/// What kind of place reports. Lets the client choose a symbol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Airport,
    Buoy,
    Station,
}

/// Which feed a station came from; `name` and `attribution` are what the
/// client may show for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    #[default]
    Metar,
    Ndbc,
    Amedas,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Metar => "Airport weather reports (METAR)",
            Source::Ndbc => "NOAA National Data Buoy Center",
            Source::Amedas => "Japan Meteorological Agency AMeDAS",
        }
    }

    pub fn attribution(self) -> &'static str {
        match self {
            Source::Metar => "Source: Aviation Weather Center (NOAA/NWS), aviationweather.gov",
            Source::Ndbc => "Source: NOAA National Data Buoy Center, ndbc.noaa.gov",
            Source::Amedas => "Source: Japan Meteorological Agency website (jma.go.jp)",
        }
    }
}

/// One station's latest report in the units of the protocol. Absent
/// measurements are `None` and left out of the JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Observation {
    #[serde(skip)]
    pub lon: f64,
    #[serde(skip)]
    pub lat: f64,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub kind: Kind,
    pub source: Source,
    pub observed: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp_c: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dewpoint_c: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_dir_deg: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_speed_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_gust_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility_m: Option<f64>,
    /// Pressure at sea level: the altimeter setting for airports, the
    /// sea-level pressure for the other feeds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub altimeter_hpa: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub water_temp_c: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wave_height_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight_category: Option<String>,
    /// Sent only when the client asks for it (`raw=1`).
    #[serde(skip)]
    pub raw: Option<String>,
}

/// Gunzip when the bytes look like gzip, otherwise treat them as plain text
/// (a mirror or a test server may serve it already decompressed).
pub(crate) fn to_text(bytes: &[u8]) -> anyhow::Result<String> {
    let mut out = Vec::new();
    if bytes.starts_with(&[0x1f, 0x8b]) {
        GzDecoder::new(bytes)
            .take(MAX_TEXT_BYTES)
            .read_to_end(&mut out)
            .context("gunzip METAR file")?;
    } else {
        out.extend_from_slice(&bytes[..bytes.len().min(MAX_TEXT_BYTES as usize)]);
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// A clock a long way ahead of ours is as untrustworthy as an old one.
pub(crate) fn current(observed: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - observed <= MAX_AGE && observed - now <= Duration::hours(1)
}

pub(crate) fn located(lat: f64, lon: f64) -> bool {
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

/// Calm wind has no direction to draw, whatever the station says.
pub(crate) fn wind_dir(deg: Option<f64>, speed: Option<f64>) -> Option<u16> {
    let blowing = speed.is_some_and(|s| s > 0.0);
    deg.filter(|d| blowing && (1.0..=360.0).contains(d))
        .map(|d| d.round() as u16)
}

pub(crate) fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Visibility arrives as "10", "6+", "0.25" or occasionally "2 1/2".
fn parse_miles(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('+');
    if s.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for part in s.split_whitespace() {
        if let Some((n, d)) = part.split_once('/') {
            let (n, d) = (num(n)?, num(d)?);
            if d == 0.0 {
                return None;
            }
            total += n / d;
        } else {
            total += num(part)?;
        }
    }
    Some(total)
}

/// Parse the file into the newest usable report per station, inside the
/// world and no older than [`MAX_AGE`] at `now`. Bad rows are skipped.
/// Fails only when the file is not a METAR table at all.
pub fn parse_metars(bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Vec<Observation>> {
    let text = to_text(bytes)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());

    // The table is preceded by free-text lines, and some column names repeat
    // (sky_cover), so the header is found by content and the first of any
    // duplicate wins.
    let mut cols: Option<HashMap<String, usize>> = None;
    let mut best: HashMap<String, Observation> = HashMap::new();
    for record in reader.records() {
        let Ok(rec) = record else { continue };
        let Some(cols) = &cols else {
            // Collected last to first, so the first of a repeated name wins.
            let named = (0..rec.len()).rev().map(|i| (rec[i].trim().to_string(), i));
            cols = rec
                .iter()
                .any(|f| f == "station_id")
                .then(|| named.collect());
            continue;
        };
        let get = |k: &str| cols.get(k).and_then(|&i| rec.get(i)).map_or("", str::trim);
        let id = get("station_id");
        if id.is_empty() {
            continue;
        }
        let Ok(observed) = DateTime::parse_from_rfc3339(get("observation_time")) else {
            continue;
        };
        let observed = observed.with_timezone(&Utc);
        let (Some(lat), Some(lon)) = (num(get("latitude")), num(get("longitude"))) else {
            continue;
        };
        // Unlocated stations carry the placeholder -99.99.
        if !current(observed, now) || !located(lat, lon) || lat == -99.99 || lon == -99.99 {
            continue;
        }
        if best.get(id).is_some_and(|o| o.observed >= observed) {
            continue;
        }

        let raw = get("raw_text");
        let speed_kt = num(get("wind_speed_kt"));
        // Calm wind is reported as direction 0 with speed 0, variable wind as
        // direction 0 with a speed.
        let variable = raw.split_whitespace().any(|t| t.starts_with("VRB"));
        let obs = Observation {
            lon,
            lat,
            id: id.to_string(),
            kind: Kind::Airport,
            source: Source::Metar,
            observed,
            temp_c: num(get("temp_c")).map(round1),
            dewpoint_c: num(get("dewpoint_c")).map(round1),
            wind_dir_deg: wind_dir(num(get("wind_dir_degrees")).filter(|_| !variable), speed_kt),
            wind_speed_ms: speed_kt.map(|v| round1(v * KT_TO_MS)),
            wind_gust_ms: num(get("wind_gust_kt")).map(|v| round1(v * KT_TO_MS)),
            visibility_m: parse_miles(get("visibility_statute_mi")).map(|v| (v * MILE_M).round()),
            altimeter_hpa: num(get("altim_in_hg")).map(|v| round1(v * INHG_HPA)),
            // The source writes the literal text "null" when it has no category.
            flight_category: Some(get("flight_category"))
                .filter(|s| matches!(*s, "VFR" | "MVFR" | "IFR" | "LIFR"))
                .map(str::to_string),
            raw: Some(raw).filter(|s| !s.is_empty()).map(str::to_string),
            ..Default::default()
        };
        best.insert(id.to_string(), obs);
    }
    if cols.is_none() {
        bail!("no header row with station_id in METAR file");
    }
    let mut v: Vec<_> = best.into_values().collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(v)
}

/// Station names from stations.cache.json: id (ICAO) -> "Site, ST".
pub fn parse_station_names(bytes: &[u8]) -> anyhow::Result<HashMap<String, String>> {
    let text = to_text(bytes)?;
    let items: Vec<serde_json::Value> = serde_json::from_str(&text).context("stations json")?;
    let mut names = HashMap::new();
    for item in items {
        let key = item["icaoId"].as_str().or_else(|| item["id"].as_str());
        let site = item["site"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let (Some(key), Some(site)) = (key, site) else {
            continue;
        };
        let name = match item["state"].as_str().filter(|s| !s.is_empty()) {
            Some(state) => format!("{site}, {state}"),
            None => site.to_string(),
        };
        names.entry(key.to_string()).or_insert(name);
    }
    Ok(names)
}

#[cfg(test)]
pub(crate) fn by_id<'a>(v: &'a [Observation], id: &str) -> &'a Observation {
    v.iter().find(|o| o.id == id).expect(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    const CSV: &str = include_str!("../tests/fixtures/metars.csv");
    const NAMES: &str = include_str!("../tests/fixtures/stations.json");

    fn now() -> DateTime<Utc> {
        "2026-11-06T12:30:00Z".parse().unwrap()
    }

    #[test]
    fn converts_units_and_skips_unlocated() {
        let v = parse_metars(CSV.as_bytes(), now()).unwrap();
        let koun = by_id(&v, "KOUN");
        assert_eq!(koun.temp_c, Some(8.3));
        assert_eq!(koun.wind_dir_deg, Some(360));
        assert_eq!(koun.wind_speed_ms, Some(5.1));
        assert_eq!(koun.visibility_m, Some(16093.0));
        assert_eq!(koun.altimeter_hpa, Some(1020.3));
        assert_eq!(koun.flight_category.as_deref(), Some("VFR"));
        assert!(koun.raw.as_deref().unwrap().starts_with("METAR KOUN"));
        // Unlocated stations carry latitude -99.99.
        assert!(v.iter().all(|o| o.id != "KQEQ" && o.id != "KQFX"));
        assert!(v.len() > 20, "{}", v.len());
    }

    #[test]
    fn variable_and_calm_wind_have_no_direction() {
        let v = parse_metars(CSV.as_bytes(), now()).unwrap();
        let cyqa = by_id(&v, "CYQA");
        assert_eq!(cyqa.wind_dir_deg, None);
        assert_eq!(cyqa.wind_speed_ms, Some(1.0));
        let calm: Vec<_> = v.iter().filter(|o| o.wind_speed_ms == Some(0.0)).collect();
        assert!(!calm.is_empty());
        assert!(calm.iter().all(|o| o.wind_dir_deg.is_none()));
    }

    #[test]
    fn missing_values_are_null_and_header_is_found_by_name() {
        let koun = CSV.lines().find(|l| l.contains(",KOUN,")).unwrap();
        let blank = koun
            .replace(",KOUN,", ",KBLK,")
            .replace(",10+,30.13,", ",,,");
        let v = parse_metars(format!("{CSV}{blank}\n").as_bytes(), now()).unwrap();
        let k = by_id(&v, "KBLK");
        assert_eq!((k.visibility_m, k.altimeter_hpa), (None, None));
        assert!(v.iter().any(|o| o.temp_c.is_none()));
        assert!(v.iter().any(|o| o.wind_gust_ms.is_some()));
        let cygw = by_id(&v, "CYGW");
        assert_eq!(cygw.visibility_m, Some(4023.0)); // 2.5 SM
    }

    #[test]
    fn newest_wins_and_old_reports_drop() {
        // An older duplicate of KOUN (listed after the newer one) and a stale-only station.
        let koun = CSV.lines().find(|l| l.contains(",KOUN,")).unwrap();
        let older = koun
            .replace("2026-11-06T11:55", "2026-11-06T10:55")
            .replace(",8.3,", ",1.0,");
        let stale = koun
            .replace(",KOUN,", ",KOLD,")
            .replace("2026-11-06T11:55", "2026-11-06T08:55");
        let text = format!(
            "Some preamble line\n\nanother, with, commas\n{CSV}{older}\n{stale}\ngarbage row\n,,,\n"
        );
        let v = parse_metars(text.as_bytes(), now()).unwrap();
        assert_eq!(by_id(&v, "KOUN").temp_c, Some(8.3));
        assert!(v.iter().all(|o| o.id != "KOLD"));
        assert_eq!(v.iter().filter(|o| o.id == "KOUN").count(), 1);
    }

    #[test]
    fn the_whole_world_is_kept_but_not_unlocated_stations() {
        let v = parse_metars(CSV.as_bytes(), now()).unwrap();
        for id in ["EGLL", "PHNL", "YSSY", "RJTT", "FACT"] {
            let o = by_id(&v, id);
            assert_eq!((o.kind, o.source), (Kind::Airport, Source::Metar), "{id}");
        }
        let yssy = by_id(&v, "YSSY");
        assert!(yssy.lat < -33.0 && yssy.lon > 151.0);
        assert!(v.iter().all(|o| o.lat != -99.99 && o.lon != -99.99));
    }

    #[test]
    fn reads_gzip() {
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(CSV.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        let a = parse_metars(&gz, now()).unwrap();
        let b = parse_metars(CSV.as_bytes(), now()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_non_metar_input() {
        assert!(parse_metars(b"<html>nope</html>", now()).is_err());
        assert!(parse_metars(&[0x1f, 0x8b, 1, 2, 3], now()).is_err());
    }

    #[test]
    fn fractional_visibility() {
        assert_eq!(parse_miles("2 1/2"), Some(2.5));
        assert_eq!(parse_miles("10+"), Some(10.0));
        assert_eq!(parse_miles("1/4"), Some(0.25));
        assert_eq!(parse_miles(""), None);
        assert_eq!(parse_miles("1/0"), None);
    }

    #[test]
    fn station_names() {
        let names = parse_station_names(NAMES.as_bytes()).unwrap();
        assert_eq!(names["KOUN"], "Norman/Univ Oklahoma Arpt, OK");
        assert_eq!(names["CWSA"], "Sable Island, NS");
    }
}
