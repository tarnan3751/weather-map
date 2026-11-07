//! Parsing of the NOAA National Data Buoy Center `latest_obs.txt`: one
//! fixed-width line per buoy or coastal station, `MM` for a missing value,
//! already in metric units except visibility (nautical miles).

use crate::metar::{Kind, Observation, Source, current, located, num, to_text, wind_dir};
use crate::round1;
use anyhow::bail;
use chrono::{DateTime, TimeZone, Utc};
use std::collections::HashMap;

const NMI_M: f64 = 1852.0;

/// "MM" and the occasional "999" style sentinels are not measurements.
fn value(tok: Option<&&str>) -> Option<f64> {
    let t = tok?;
    if *t == "MM" { None } else { num(t) }
}

/// Moored buoys have numeric ids ("46026", "32ST0"); the coastal and pier
/// stations have letter ids ("AAMC1", "BURL1").
fn kind_of(id: &str) -> Kind {
    if id.starts_with(|c: char| c.is_ascii_digit()) {
        Kind::Buoy
    } else {
        Kind::Station
    }
}

fn label(id: &str, kind: Kind) -> String {
    match kind {
        Kind::Buoy => format!("Buoy {id}"),
        _ => format!("Station {id}"),
    }
}

/// Newest report per station, [`current`] at `now`. Bad rows
/// are skipped; fails only when the file has no NDBC header.
pub fn parse_ndbc(bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Vec<Observation>> {
    let text = to_text(bytes)?;
    let mut cols: Option<HashMap<&str, usize>> = None;
    let mut out = Vec::new();
    for line in text.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.is_empty() {
            continue;
        }
        if toks[0].starts_with('#') {
            if cols.is_none() && toks[0] == "#STN" {
                cols = Some(
                    toks.iter()
                        .enumerate()
                        .map(|(i, t)| (t.trim_start_matches('#'), i))
                        .collect(),
                );
            }
            continue;
        }
        let Some(cols) = &cols else { continue };
        let get = |name: &str| cols.get(name).and_then(|&i| toks.get(i));
        let int = |name: &str| value(get(name)).map(|v| v as u32);
        let (Some(lat), Some(lon)) = (value(get("LAT")), value(get("LON"))) else {
            continue;
        };
        let (Some(y), Some(mo), Some(d), Some(h), Some(mi)) =
            (int("YYYY"), int("MM"), int("DD"), int("hh"), int("mm"))
        else {
            continue;
        };
        let Some(observed) = Utc.with_ymd_and_hms(y as i32, mo, d, h, mi, 0).single() else {
            continue;
        };
        if !located(lat, lon) || !current(observed, now) {
            continue;
        }
        let id = toks[0];
        let kind = kind_of(id);
        let speed = value(get("WSPD"));
        out.push(Observation {
            lon,
            lat,
            id: format!("ndbc:{id}"),
            name: Some(label(id, kind)),
            kind,
            source: Source::Ndbc,
            observed,
            temp_c: value(get("ATMP")).map(round1),
            dewpoint_c: value(get("DEWP")).map(round1),
            wind_dir_deg: wind_dir(value(get("WDIR")), speed),
            wind_speed_ms: speed.map(round1),
            wind_gust_ms: value(get("GST")).map(round1),
            visibility_m: value(get("VIS")).map(|v| (v * NMI_M).round()),
            altimeter_hpa: value(get("PRES")).map(round1),
            water_temp_c: value(get("WTMP")).map(round1),
            wave_height_m: value(get("WVHT")).map(round1),
            ..Default::default()
        });
    }
    if cols.is_none() {
        bail!("no #STN header row in NDBC file");
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metar::by_id;

    const TXT: &str = include_str!("../tests/fixtures/ndbc.txt");

    fn now() -> DateTime<Utc> {
        "2026-11-06T18:00:00Z".parse().unwrap()
    }

    #[test]
    fn parses_units_kinds_and_missing_values() {
        let v = parse_ndbc(TXT.as_bytes(), now()).unwrap();
        assert_eq!(v.len(), 10);
        let b = by_id(&v, "ndbc:41013");
        assert_eq!((b.kind, b.source), (Kind::Buoy, Source::Ndbc));
        assert_eq!(b.name.as_deref(), Some("Buoy 41013"));
        assert_eq!((b.lat, b.lon), (33.436, -77.764));
        assert_eq!(b.observed.to_rfc3339(), "2026-11-06T17:50:00+00:00");
        assert_eq!(b.wind_dir_deg, Some(170));
        assert_eq!(b.wind_speed_ms, Some(7.0));
        assert_eq!(b.wind_gust_ms, Some(9.0));
        assert_eq!(b.wave_height_m, Some(1.2));
        assert_eq!(b.altimeter_hpa, Some(1011.4));
        assert_eq!(b.temp_c, Some(24.2));
        assert_eq!(b.dewpoint_c, Some(22.2));
        assert_eq!(b.water_temp_c, None);
        assert_eq!(b.visibility_m, None);
        // Nautical miles to metres.
        assert_eq!(by_id(&v, "ndbc:62107").visibility_m, Some(9260.0));
        // Letter ids are coastal stations.
        assert_eq!(by_id(&v, "ndbc:AAMC1").kind, Kind::Station);
        assert_eq!(
            by_id(&v, "ndbc:AAMC1").name.as_deref(),
            Some("Station AAMC1")
        );
        // Southern hemisphere and negative longitude survive.
        let s = by_id(&v, "ndbc:32ST0");
        assert_eq!((s.lat, s.lon), (-22.0, -85.0));
    }

    #[test]
    fn calm_or_missing_wind_has_no_direction() {
        let v = parse_ndbc(TXT.as_bytes(), now()).unwrap();
        let calm = by_id(&v, "ndbc:41010");
        assert_eq!((calm.wind_speed_ms, calm.wind_dir_deg), (Some(0.0), None));
        let none = by_id(&v, "ndbc:62107");
        assert_eq!((none.wind_speed_ms, none.wind_dir_deg), (None, None));
    }

    #[test]
    fn old_future_and_malformed_rows_are_dropped() {
        let v = parse_ndbc(TXT.as_bytes(), "2026-11-06T21:30:00Z".parse().unwrap()).unwrap();
        assert!(
            v.is_empty(),
            "everything is over three hours old: {}",
            v.len()
        );
        let early = parse_ndbc(TXT.as_bytes(), "2026-11-06T15:00:00Z".parse().unwrap()).unwrap();
        assert!(early.is_empty(), "reports from the future are not trusted");
        let text = format!("{TXT}garbage line\n99999 abc def 2026 11 06 17 50\n");
        assert_eq!(parse_ndbc(text.as_bytes(), now()).unwrap().len(), 10);
    }

    #[test]
    fn rejects_non_ndbc_input() {
        assert!(parse_ndbc(b"<html>nope</html>", now()).is_err());
    }
}
