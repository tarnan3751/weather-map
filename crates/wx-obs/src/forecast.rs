//! Turning api.weather.gov forecast documents into the neutral point forecast.

use crate::model::{self, DailyPoint, HourlyPoint, PointForecast, fold};
use crate::round1;
use anyhow::{Context, bail};
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub const SOURCE: &str = "nws";
pub const ATTRIBUTION: &str = "Forecast from the US National Weather Service";

/// The fields of `/points` that we use.
#[derive(Debug, Clone, PartialEq)]
pub struct PointInfo {
    pub office: String,
    pub grid_id: String,
    pub grid_x: u32,
    pub grid_y: u32,
    pub forecast_url: String,
    pub hourly_url: String,
    pub place: Option<String>,
    pub time_zone: Option<String>,
}

pub fn parse_point(doc: &Value) -> anyhow::Result<PointInfo> {
    let p = &doc["properties"];
    let text = |v: &Value| v.as_str().map(str::to_string);
    let office = text(&p["cwa"])
        .or_else(|| text(&p["gridId"]))
        .context("no cwa")?;
    let loc = &p["relativeLocation"]["properties"];
    let place = match (loc["city"].as_str(), loc["state"].as_str()) {
        (Some(c), Some(s)) => Some(format!("{c}, {s}")),
        (Some(c), None) => Some(c.to_string()),
        _ => None,
    };
    Ok(PointInfo {
        grid_id: text(&p["gridId"]).unwrap_or_else(|| office.clone()),
        office,
        grid_x: p["gridX"].as_u64().context("no gridX")? as u32,
        grid_y: p["gridY"].as_u64().context("no gridY")? as u32,
        forecast_url: text(&p["forecast"]).context("no forecast url")?,
        hourly_url: text(&p["forecastHourly"]).context("no hourly url")?,
        place,
        time_zone: text(&p["timeZone"]),
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Quantity {
    value: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Period {
    name: String,
    start_time: Option<DateTime<FixedOffset>>,
    end_time: Option<DateTime<Utc>>,
    is_daytime: bool,
    temperature: Option<f64>,
    temperature_unit: Option<String>,
    probability_of_precipitation: Option<Quantity>,
    wind_speed: Option<String>,
    wind_direction: Option<String>,
    short_forecast: String,
    detailed_forecast: String,
}

fn temp_c(p: &Period) -> Option<f64> {
    let f = matches!(p.temperature_unit.as_deref(), Some("F" | "f"));
    p.temperature
        .map(|t| round1(if f { (t - 32.0) * 5.0 / 9.0 } else { t }))
}

fn pop(p: &Period) -> Option<u8> {
    let q = p.probability_of_precipitation.as_ref()?;
    q.value.filter(|v| v.is_finite()).map(model::percent)
}

/// "SSW" -> 203. `VAR` and anything unrecognised has no direction.
pub fn compass_deg(s: &str) -> Option<u16> {
    const POINTS: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    let s = s.trim();
    let i = POINTS.iter().position(|p| p.eq_ignore_ascii_case(s))?;
    Some((i as f64 * 22.5).round() as u16)
}

/// "15 km/h", "10 to 20 mph": for a range the stronger end, the one that
/// matters for a chart.
fn wind_ms(p: &Period) -> Option<f64> {
    let (mut top, mut unit) = (None, String::new());
    for tok in p.wind_speed.as_deref()?.split_whitespace() {
        match tok.parse::<f64>() {
            Ok(n) => fold(&mut top, n, f64::max),
            Err(_) if tok != "to" => unit = tok.to_ascii_lowercase(),
            Err(_) => {}
        }
    }
    let per_unit = match unit.as_str() {
        "km/h" | "kph" => 1.0 / 3.6,
        "mph" => 0.447_04,
        "kt" | "kts" | "kt." => 1852.0 / 3600.0,
        "m/s" => 1.0,
        _ => return None,
    };
    Some(round1(top? * per_unit))
}

fn periods(doc: &Value) -> anyhow::Result<Vec<Period>> {
    let periods = doc["properties"]["periods"]
        .as_array()
        .context("forecast has no periods")?;
    // A period that does not fit is dropped rather than failing the forecast.
    let fits = |v: &Value| serde_json::from_value(v.clone()).ok();
    Ok(periods.iter().filter_map(fits).collect())
}

/// The NWS words each half-day period itself ("Friday Night"), so the written
/// text keeps those labels when day and night are joined.
fn labelled(p: &Period) -> Option<String> {
    let d = p.detailed_forecast.trim();
    match (d.is_empty(), p.name.is_empty()) {
        (true, _) => None,
        (false, true) => Some(d.to_string()),
        (false, false) => Some(format!("{}: {d}", p.name)),
    }
}

/// Half-day periods become local calendar days: the daytime period gives the
/// high, the night period the low, and both give the text. The date is the
/// one in the period's own offset, which is the forecast office's local day.
fn daily_from_periods(periods: Vec<Period>) -> Vec<DailyPoint> {
    let mut days: BTreeMap<NaiveDate, DailyPoint> = BTreeMap::new();
    for p in periods {
        let (Some(start), Some(end)) = (p.start_time, p.end_time) else {
            continue;
        };
        let date = start.naive_local().date();
        let day = days.entry(date).or_insert_with(|| DailyPoint {
            date,
            end,
            ..Default::default()
        });
        day.end = day.end.max(end);
        day.pop = day.pop.max(pop(&p));
        // An early-morning period and that evening's both land on one date;
        // the low of the date is the colder.
        match temp_c(&p) {
            Some(t) if p.is_daytime => fold(&mut day.high_c, t, f64::max),
            Some(t) => fold(&mut day.low_c, t, f64::min),
            None => {}
        }
        // The daytime wording describes the day; night only fills a gap.
        if (p.is_daytime || day.condition.is_none())
            && let Some(code) = model::code_from_nws(&p.short_forecast, true)
        {
            day.condition = Some(model::strip_daypart(&code).to_string());
        }
        if let Some(t) = labelled(&p) {
            day.text = Some(
                day.text
                    .take()
                    .map_or(t.clone(), |prev| format!("{prev} {t}")),
            );
        }
    }
    days.into_values().collect()
}

pub fn normalise(hourly_doc: &Value, daily_doc: &Value) -> anyhow::Result<PointForecast> {
    let hourly: Vec<HourlyPoint> = periods(hourly_doc)?
        .into_iter()
        .filter_map(|p| {
            let condition = model::code_from_nws(&p.short_forecast, p.is_daytime);
            Some(HourlyPoint {
                time: p.start_time?.with_timezone(&Utc),
                span_h: 1,
                temp_c: temp_c(&p),
                pop: pop(&p),
                wind_ms: wind_ms(&p),
                wind_dir_deg: p.wind_direction.as_deref().and_then(compass_deg),
                condition,
                text: Some(p.short_forecast).filter(|s| !s.is_empty()),
                ..Default::default()
            })
        })
        .collect();
    let daily = daily_from_periods(periods(daily_doc)?);
    if hourly.is_empty() && daily.is_empty() {
        bail!("forecast documents held no usable periods");
    }
    let generated = |doc: &Value| {
        ["generatedAt", "updateTime"]
            .iter()
            .find_map(|k| model::time(&doc["properties"][k]))
    };
    let generated_at = generated(hourly_doc)
        .or_else(|| generated(daily_doc))
        .unwrap_or_else(Utc::now);
    let (source, attribution) = (SOURCE.into(), ATTRIBUTION.into());
    Ok(PointForecast {
        source,
        attribution,
        generated_at,
        hourly,
        daily,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn docs() -> (Value, Value) {
        (
            fixture(include_str!("../tests/fixtures/hourly.json")),
            fixture(include_str!("../tests/fixtures/daily.json")),
        )
    }

    #[test]
    fn point_lookup() {
        let p = parse_point(&fixture(include_str!("../tests/fixtures/points.json"))).unwrap();
        assert_eq!(p.office, "OUN");
        assert_eq!((p.grid_x, p.grid_y), (100, 83));
        assert_eq!(p.place.as_deref(), Some("Norman, OK"));
        assert_eq!(p.time_zone.as_deref(), Some("America/Chicago"));
        assert!(p.hourly_url.ends_with("/forecast/hourly"));
        assert!(parse_point(&fixture("{}")).is_err());
    }

    #[test]
    fn hourly_units_and_nulls() {
        let (h, d) = docs();
        let f = normalise(&h, &d).unwrap();
        assert_eq!((f.source.as_str(), f.hourly.len()), ("nws", 4));
        let a = &f.hourly[0];
        assert_eq!(a.time.to_rfc3339(), "2026-11-06T12:00:00+00:00");
        assert_eq!(a.span_h, 1);
        assert_eq!(a.temp_c, Some(9.0));
        assert_eq!(a.pop, Some(0));
        assert_eq!(a.precip_mm, None);
        assert_eq!(a.wind_ms, Some(3.6)); // 13 km/h
        assert_eq!(a.wind_dir_deg, Some(0));
        assert!(a.text.is_some() && a.condition.is_some());
        assert_eq!(f.hourly[1].pop, None);
        assert_eq!(f.hourly[2].wind_ms, Some(5.6)); // "10 to 20 km/h" -> upper end
        assert_eq!(f.generated_at.to_rfc3339(), "2026-11-06T12:12:15+00:00");
    }

    #[test]
    fn daily_merges_half_days_into_local_dates_in_si() {
        let (h, d) = docs();
        let f = normalise(&h, &d).unwrap();
        // Overnight, Friday and Friday Night all fall on 6 November local time.
        assert_eq!(f.daily.len(), 1, "{:?}", f.daily);
        let day = &f.daily[0];
        assert_eq!(day.date.to_string(), "2026-11-06");
        assert_eq!(day.low_c, Some(5.0)); // 41 F on Friday night, colder than 45 F overnight
        assert_eq!(day.high_c, Some(13.9)); // 57 F
        assert_eq!(day.condition.as_deref(), Some("fog"));
        let text = day.text.as_deref().unwrap();
        assert!(text.starts_with("Overnight: Patchy fog."), "{text}");
        assert!(text.contains("Friday:"), "{text}");
        assert!(day.end > "2026-11-06T18:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn odd_values() {
        assert_eq!(compass_deg("SSW"), Some(203));
        assert_eq!(compass_deg("NNW"), Some(338));
        assert_eq!(compass_deg("VAR"), None);
        let doc = fixture(
            r#"{"properties":{"periods":[
              {"startTime":"2026-11-06T06:00:00-06:00","endTime":"2026-11-06T07:00:00-06:00","isDaytime":true,
               "temperature":null,"windSpeed":"calm","windDirection":"","shortForecast":"x",
               "probabilityOfPrecipitation":{"value":null},"relativeHumidity":null},
              {"startTime":"garbage"},
              {"temperature":"hot"}]}}"#,
        );
        let f = normalise(&doc, &doc).unwrap();
        assert_eq!(f.hourly.len(), 1);
        assert_eq!(f.hourly[0].temp_c, None);
        assert_eq!(f.hourly[0].wind_ms, None);
        assert_eq!(f.daily.len(), 1);
        assert!(normalise(&fixture("{}"), &fixture("{}")).is_err());
        let empty = fixture(r#"{"properties":{"periods":[]}}"#);
        assert!(normalise(&empty, &empty).is_err());
    }
}
