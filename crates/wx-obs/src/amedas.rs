//! JMA AMeDAS: about 1,300 automatic stations in Japan. Three key-free files
//! under one base: `const/amedastable.json` (stations), `data/latest_time.txt`
//! (the newest snapshot time, with a +09:00 offset) and
//! `data/map/<YYYYMMDDHHMM00>.json` (every station's latest values, in JST).
//!
//! Terms relied on (https://www.jma.go.jp/jma/en/copyright.html, read
//! 2026-11-06): JMA website content may be used under the Public Data License
//! (Version 1.0), which is CC BY compatible. The user must cite the source
//! ("Source: Japan Meteorological Agency website"), say if the content was
//! edited, and not imply it was made by the Government of Japan. The
//! Meteorological Service Act restricts issuing forecasts and warnings, not
//! showing observations. We fetch one 250 KB snapshot per 10 minutes.

use crate::metar::{Kind, Observation, Source, current, wind_dir};
use crate::{round, round1};
use anyhow::{Context, bail};
use chrono::{DateTime, FixedOffset, Utc};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct AmedasStation {
    pub lat: f64,
    pub lon: f64,
    pub name: String,
}

/// `{ "44132": { "lat": [35, 41.4], "lon": [139, 45.6], "enName": "Tokyo" } }`;
/// coordinates are degrees and decimal minutes.
pub fn parse_table(bytes: &[u8]) -> anyhow::Result<HashMap<String, AmedasStation>> {
    let doc: Value = serde_json::from_slice(bytes).context("AMeDAS station table")?;
    let Some(map) = doc.as_object() else {
        bail!("AMeDAS station table is not an object")
    };
    let dm = |v: &Value| -> Option<f64> {
        let a = v.as_array()?;
        Some(a.first()?.as_f64()? + a.get(1)?.as_f64()? / 60.0)
    };
    let table: HashMap<_, _> = map
        .iter()
        .filter_map(|(id, s)| {
            let (lat, lon) = (dm(&s["lat"])?, dm(&s["lon"])?);
            let name = s["enName"].as_str().filter(|n| !n.is_empty()).unwrap_or(id);
            Some((
                id.clone(),
                AmedasStation {
                    lat: round(lat, 4),
                    lon: round(lon, 4),
                    name: name.to_string(),
                },
            ))
        })
        .collect();
    if table.is_empty() {
        bail!("AMeDAS station table is empty");
    }
    Ok(table)
}

/// "2026-11-07T03:10:00+09:00".
pub fn parse_latest_time(text: &str) -> anyhow::Result<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(text.trim()).context("AMeDAS latest_time")
}

/// The snapshot's path under `data/map/`.
pub fn snapshot_name(t: DateTime<FixedOffset>) -> String {
    format!("{}.json", t.format("%Y%m%d%H%M%S"))
}

/// A `[value, quality]` pair; quality 0 is a normal value.
fn element(v: &Value, key: &str) -> Option<f64> {
    let pair = v.get(key)?.as_array()?;
    if pair.get(1).and_then(Value::as_i64).unwrap_or(0) != 0 {
        return None;
    }
    pair.first()?.as_f64().filter(|x| x.is_finite())
}

/// Stations of `table` that have a report in the snapshot taken at
/// `observed`. Wind direction arrives as a 16-point code (1 = NNE ... 16 = N,
/// 0 = calm).
pub fn parse_snapshot(
    bytes: &[u8],
    table: &HashMap<String, AmedasStation>,
    observed: DateTime<Utc>,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Observation>> {
    if !current(observed, now) {
        return Ok(Vec::new());
    }
    let doc: Value = serde_json::from_slice(bytes).context("AMeDAS snapshot")?;
    let Some(map) = doc.as_object() else {
        bail!("AMeDAS snapshot is not an object")
    };
    let mut out: Vec<_> = map
        .iter()
        .filter_map(|(id, v)| {
            let st = table.get(id)?;
            let speed = element(v, "wind");
            Some(Observation {
                lon: st.lon,
                lat: st.lat,
                id: format!("amedas:{id}"),
                name: Some(st.name.clone()),
                kind: Kind::Station,
                source: Source::Amedas,
                observed,
                temp_c: element(v, "temp").map(round1),
                wind_dir_deg: wind_dir(element(v, "windDirection").map(|c| c * 22.5), speed),
                wind_speed_ms: speed.map(round1),
                visibility_m: element(v, "visibility").map(f64::round),
                altimeter_hpa: element(v, "normalPressure").map(round1),
                ..Default::default()
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metar::by_id;

    const TABLE: &str = include_str!("../tests/fixtures/amedastable.json");
    const MAP: &str = include_str!("../tests/fixtures/amedas_map.json");

    fn now() -> DateTime<Utc> {
        "2026-11-06T18:30:00Z".parse().unwrap()
    }

    fn observed() -> DateTime<Utc> {
        parse_latest_time("2026-11-07T03:10:00+09:00")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn table_coordinates_are_degrees_and_minutes() {
        let t = parse_table(TABLE.as_bytes()).unwrap();
        let w = &t["11016"];
        assert_eq!(w.name, "Wakkanai");
        assert!(
            (w.lat - 45.415).abs() < 1e-9 && (w.lon - 141.6783).abs() < 1e-4,
            "{w:?}"
        );
        assert!(parse_table(b"{}").is_err());
        assert!(parse_table(b"[1]").is_err());
    }

    #[test]
    fn snapshot_name_uses_the_jst_clock() {
        let t = parse_latest_time("2026-11-07T03:10:00+09:00\n").unwrap();
        assert_eq!(snapshot_name(t), "20261107031000.json");
        assert_eq!(
            t.with_timezone(&Utc).to_rfc3339(),
            "2026-11-06T18:10:00+00:00"
        );
        assert!(parse_latest_time("soon").is_err());
    }

    #[test]
    fn converts_values_and_wind_codes() {
        let t = parse_table(TABLE.as_bytes()).unwrap();
        let v = parse_snapshot(MAP.as_bytes(), &t, observed(), now()).unwrap();
        assert_eq!(v.len(), 6);
        let w = by_id(&v, "amedas:11016");
        assert_eq!((w.kind, w.source), (Kind::Station, Source::Amedas));
        assert_eq!(w.temp_c, Some(-1.2));
        assert_eq!(w.wind_speed_ms, Some(12.4));
        assert_eq!(w.wind_dir_deg, Some(225)); // code 10 = SW
        assert_eq!(w.altimeter_hpa, Some(996.6)); // sea-level pressure
        assert_eq!(w.visibility_m, Some(280.0));
        assert_eq!(w.observed.to_rfc3339(), "2026-11-06T18:10:00+00:00");
        // Stations without a pressure sensor simply lack the value.
        let soya = by_id(&v, "amedas:11001");
        assert_eq!(soya.altimeter_hpa, None);
        let naha = by_id(&v, "amedas:91197");
        assert_eq!(naha.wind_dir_deg, Some(23)); // code 1 = NNE, 22.5 rounded
    }

    #[test]
    fn flagged_values_and_unknown_stations_are_skipped() {
        let t = parse_table(TABLE.as_bytes()).unwrap();
        let mut snap: Value = serde_json::from_str(MAP).unwrap();
        snap["11016"]["temp"] = serde_json::json!([5.0, 5]);
        snap["11016"]["windDirection"] = serde_json::json!([0, 0]);
        snap["99999"] = serde_json::json!({"temp": [1.0, 0]});
        let v = parse_snapshot(snap.to_string().as_bytes(), &t, observed(), now()).unwrap();
        assert_eq!(v.len(), 6);
        let w = by_id(&v, "amedas:11016");
        assert_eq!((w.temp_c, w.wind_dir_deg), (None, None));
    }

    #[test]
    fn an_old_snapshot_yields_nothing() {
        let t = parse_table(TABLE.as_bytes()).unwrap();
        let later = "2026-11-06T22:30:00Z".parse().unwrap();
        let v = parse_snapshot(MAP.as_bytes(), &t, observed(), later).unwrap();
        assert!(v.is_empty());
        assert!(parse_snapshot(b"nope", &t, observed(), now()).is_err());
    }
}
