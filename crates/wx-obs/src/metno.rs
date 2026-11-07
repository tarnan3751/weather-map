//! MET Norway Locationforecast 2.0 (`complete`): worldwide, keyless, under
//! terms this module follows:
//!
//! - called from the server only, with an identifying `User-Agent`;
//! - coordinates rounded to 2 decimals (the limit is 4);
//! - a point is never requested again before its `Expires`, nor sooner than
//!   30 minutes (Nordic region) or 3 hours (elsewhere) after the last request;
//! - once the data may have changed, the request is conditional
//!   (`If-Modified-Since` with the exact `Last-Modified` we were given);
//! - well under their 20 requests per second: [`MAX_IN_FLIGHT`] at a time;
//! - 429 and 403 pause the whole host; the credit is
//!   "Based on data from MET Norway".
//!
//! The `complete` variant has no precipitation probability outside the Nordic
//! region, so `pop` is usually `None`.

use crate::model::{self, DailyPoint, HourlyPoint, PointForecast, fold, percent};
use crate::point::{self, FetchError, Pauses};
use crate::{Inner, header, round1};
use anyhow::{Context, bail};
use axum::http::{StatusCode, header};
use chrono::{DateTime, Duration as Cd, FixedOffset, NaiveDate, Timelike, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const SOURCE: &str = "met-no";
pub const ATTRIBUTION: &str = "Based on data from MET Norway";

/// Requests to api.met.no in flight at once.
pub(crate) const MAX_IN_FLIGHT: usize = 2;
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
/// When the response carries no usable `Expires`.
const DEFAULT_EXPIRY: Duration = Duration::from_secs(30 * 60);
const MAX_EXPIRY: Duration = Duration::from_secs(12 * 3600);
const NORDIC_MIN_REFETCH: Duration = Duration::from_secs(30 * 60);
/// A 403 means our identification was refused, and repeating that risks a ban.
const PAUSES: Pauses = Pauses(60, 600, 600);

/// What the cache keeps for a point.
pub(crate) struct Entry {
    pub forecast: Arc<PointForecast>,
    pub last_modified: Option<String>,
    /// Nothing is sent for this point before this instant.
    pub next_fetch: Instant,
}

/// One per point; held across a fetch so concurrent taps share it.
pub(crate) type Slot = Arc<tokio::sync::Mutex<Option<Entry>>>;

pub(crate) type Key = (i32, i32);

pub(crate) fn key_of(lat: f64, lon: f64) -> Key {
    ((lat * 100.0).round() as i32, (lon * 100.0).round() as i32)
}

/// Roughly Norway, Sweden, Finland and Denmark, where MET refreshes hourly.
/// Drawn tight: a point wrongly left out only waits longer.
fn is_nordic(lat: f64, lon: f64) -> bool {
    (55.0..=71.5).contains(&lat) && (4.5..=31.0).contains(&lon)
}

fn url_for(base: &str, (lat, lon): Key) -> String {
    let base = base.trim_end_matches('/');
    let (lat, lon) = (lat as f64 / 100.0, lon as f64 / 100.0);
    format!("{base}/weatherapi/locationforecast/2.0/complete?lat={lat:.2}&lon={lon:.2}")
}

/// How long until the data may be asked for again: its `Expires`, but never
/// sooner than the published minimum between identical requests.
fn wait_until_next(expires: Option<&str>, now: DateTime<Utc>, min_refetch: Duration) -> Duration {
    let left =
        |t: DateTime<FixedOffset>| (t.with_timezone(&Utc) - now).to_std().unwrap_or_default();
    let from_header = expires
        .and_then(|e| DateTime::parse_from_rfc2822(e).ok())
        .map(left);
    from_header
        .unwrap_or(DEFAULT_EXPIRY)
        .clamp(min_refetch, MAX_EXPIRY.max(min_refetch))
}

/// Ask for the point, conditionally once `entry` holds data, and update
/// `entry` with the answer.
async fn fetch(
    inner: &Inner,
    key: Key,
    entry: &mut Option<Entry>,
    min_refetch: Duration,
) -> Result<Arc<PointForecast>, FetchError> {
    let gate = &inner.config.met_gate;
    let _slot = point::slot(&inner.met_slots, gate).await?;
    let url = url_for(&inner.config.met_api_base, key);
    let mut req = inner.http.get(&url).timeout(ATTEMPT_TIMEOUT);
    if let Some(lm) = entry.as_ref().and_then(|e| e.last_modified.as_deref()) {
        req = req.header(header::IF_MODIFIED_SINCE, lm);
    }
    let failed = |e: &dyn std::fmt::Display| FetchError::failed(format!("{url}: {e}"));
    let resp = req.send().await.map_err(|e| failed(&e))?;
    let expires = header(resp.headers(), header::EXPIRES);
    let next_fetch = Instant::now() + wait_until_next(expires.as_deref(), Utc::now(), min_refetch);
    if let Some(e) = point::refusal(gate, &resp, &url, &PAUSES) {
        return Err(e);
    }
    match (resp.status(), entry.as_mut()) {
        (StatusCode::NOT_MODIFIED, Some(e)) => {
            e.next_fetch = next_fetch;
            return Ok(e.forecast.clone());
        }
        (s, _) if !s.is_success() => return Err(failed(&format!("HTTP {s}"))),
        (StatusCode::NON_AUTHORITATIVE_INFORMATION, _) => {
            tracing::warn!("MET Norway marks this product deprecated (HTTP 203): {url}");
        }
        _ => {}
    }
    let last_modified = header(resp.headers(), header::LAST_MODIFIED);
    let body = resp.bytes().await.map_err(|e| failed(&e))?;
    let doc: Value =
        serde_json::from_slice(&body).map_err(|e| failed(&format!("bad body: {e}")))?;
    let forecast =
        Arc::new(parse(&doc, key.1 as f64 / 100.0, Utc::now()).map_err(FetchError::failed)?);
    *entry = Some(Entry {
        forecast: forecast.clone(),
        last_modified,
        next_fetch,
    });
    Ok(forecast)
}

/// The forecast for the point nearest `lat`, `lon`: cached, never fetched
/// before it expires, and served stale (briefly retried) when MET fails.
pub(crate) async fn forecast(
    inner: &Inner,
    lat: f64,
    lon: f64,
) -> Result<Arc<PointForecast>, FetchError> {
    let key = key_of(lat, lon);
    let slot = inner
        .met_points
        .get_with(key, async { Slot::default() })
        .await;
    let mut entry = slot.lock().await;
    if let Some(e) = entry.as_ref().filter(|e| Instant::now() < e.next_fetch) {
        return Ok(e.forecast.clone());
    }
    if entry.is_none()
        && let Some(msg) = inner.met_failed.get(&key).await
    {
        return Err(FetchError::Failed(msg));
    }
    let nordic = if is_nordic(lat, lon) {
        NORDIC_MIN_REFETCH
    } else {
        Duration::MAX
    };
    let err = match fetch(
        inner,
        key,
        &mut entry,
        inner.config.met_min_refetch.min(nordic),
    )
    .await
    {
        Ok(f) => return Ok(f),
        Err(e) => e,
    };
    let Some(e) = entry.as_mut() else {
        point::remember_failure(&inner.met_failed, key, &err).await;
        return Err(err);
    };
    tracing::warn!("serving stale MET Norway forecast for {key:?}: {err}");
    // Not asked again at once, but sooner than a normal expiry.
    e.next_fetch = Instant::now() + point::FAILURE_MEMORY;
    Ok(e.forecast.clone())
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite())
}

/// The block that describes the step starting at an entry: the 1 h block
/// where there is one, else the 6 h block.
struct Step<'a> {
    time: DateTime<Utc>,
    span_h: u8,
    instant: &'a Value,
    block: &'a Value,
    six: &'a Value,
}

fn steps(doc: &Value) -> anyhow::Result<Vec<Step<'_>>> {
    let series = doc["properties"]["timeseries"]
        .as_array()
        .context("no timeseries")?;
    Ok(series
        .iter()
        .filter_map(|e| {
            let time = e["time"].as_str()?.parse::<DateTime<Utc>>().ok()?;
            let data = &e["data"];
            let (span_h, block) = if data["next_1_hours"].is_object() {
                (1, &data["next_1_hours"])
            } else if data["next_6_hours"].is_object() {
                (6, &data["next_6_hours"])
            } else {
                return None;
            };
            Some(Step {
                time,
                span_h,
                instant: &data["instant"]["details"],
                block,
                six: &data["next_6_hours"],
            })
        })
        .collect())
}

/// Hours east of UTC for solar time at `lon`; MET gives no time zone.
fn offset_hours(lon: f64) -> i64 {
    (lon / 15.0).round() as i64
}

pub fn parse(doc: &Value, lon: f64, now: DateTime<Utc>) -> anyhow::Result<PointForecast> {
    let steps = steps(doc)?;
    if steps.is_empty() {
        bail!("MET Norway forecast held no usable steps");
    }
    let hourly: Vec<HourlyPoint> = steps
        .iter()
        .map(|s| {
            let symbol = s.block["summary"]["symbol_code"]
                .as_str()
                .filter(|c| !c.is_empty());
            HourlyPoint {
                time: s.time,
                span_h: s.span_h,
                temp_c: number(&s.instant["air_temperature"]).map(round1),
                pop: number(&s.block["details"]["probability_of_precipitation"]).map(percent),
                precip_mm: number(&s.block["details"]["precipitation_amount"]).map(round1),
                wind_ms: number(&s.instant["wind_speed"]).map(round1),
                wind_dir_deg: number(&s.instant["wind_from_direction"])
                    .map(|d| (d.round() as i64).rem_euclid(360) as u16),
                condition: symbol.map(str::to_string),
                text: symbol.map(model::condition_text),
            }
        })
        .collect();
    let daily = daily(&steps, offset_hours(lon));
    let generated_at = model::time(&doc["properties"]["meta"]["updated_at"]).unwrap_or(now);
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

/// Local calendar days (solar time from longitude) from the whole series.
/// Extremes come from the instant temperatures and, where steps are 6 h
/// apart, from the 6 h block's own min and max; the condition is the 6 h
/// symbol whose window is centred nearest local noon.
fn daily(steps: &[Step<'_>], offset_h: i64) -> Vec<DailyPoint> {
    // Each day with the distance from noon of the symbol chosen so far.
    let mut days: BTreeMap<NaiveDate, (DailyPoint, f64)> = BTreeMap::new();
    for s in steps {
        let local = s.time + Cd::hours(offset_h);
        let date = local.date_naive();
        let end = date
            .succ_opt()
            .and_then(|n| n.and_hms_opt(0, 0, 0))
            .map_or(Utc::now(), |n| n.and_utc() - Cd::hours(offset_h));
        let (d, best) = days.entry(date).or_insert_with(|| {
            (
                DailyPoint {
                    date,
                    end,
                    ..Default::default()
                },
                f64::INFINITY,
            )
        });
        let mut temps = vec![number(&s.instant["air_temperature"])];
        if s.span_h == 6 {
            temps.push(number(&s.six["details"]["air_temperature_max"]));
            temps.push(number(&s.six["details"]["air_temperature_min"]));
        }
        for t in temps.into_iter().flatten() {
            fold(&mut d.low_c, t, f64::min);
            fold(&mut d.high_c, t, f64::max);
        }
        if let Some(p) = number(&s.block["details"]["probability_of_precipitation"]) {
            d.pop = d.pop.max(Some(percent(p)));
        }
        if let Some(mm) = number(&s.block["details"]["precipitation_amount"]) {
            fold(&mut d.precip_mm, mm, |a, b| a + b);
        }
        if let Some(code) = s.six["summary"]["symbol_code"].as_str() {
            let dist = (local.hour() as f64 + local.minute() as f64 / 60.0 + 3.0 - 12.0).abs();
            if dist < *best {
                *best = dist;
                d.condition = Some(model::strip_daypart(code).to_string());
            }
        }
    }
    let rounded = |d: DailyPoint| DailyPoint {
        low_c: d.low_c.map(round1),
        high_c: d.high_c.map(round1),
        precip_mm: d.precip_mm.map(round1),
        ..d
    };
    days.into_values()
        .filter(|(d, _)| d.low_c.is_some())
        .map(|(d, _)| rounded(d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = include_str!("../tests/fixtures/metno_complete.json");

    fn doc() -> Value {
        serde_json::from_str(DOC).unwrap()
    }

    #[test]
    fn coordinates_and_urls_are_rounded_to_two_decimals() {
        assert_eq!(key_of(-1.29212, 36.82198), (-129, 3682));
        assert_eq!(
            url_for("https://api.met.no/", (-129, 3682)),
            "https://api.met.no/weatherapi/locationforecast/2.0/complete?lat=-1.29&lon=36.82"
        );
        assert_eq!(
            url_for("http://x", (5, -10)),
            "http://x/weatherapi/locationforecast/2.0/complete?lat=0.05&lon=-0.10"
        );
    }

    #[test]
    fn nordic_region() {
        assert!(is_nordic(59.91, 10.75)); // Oslo
        assert!(is_nordic(55.68, 12.57)); // Copenhagen
        assert!(!is_nordic(-1.29, 36.82));
        assert!(!is_nordic(51.5, -0.12));
    }

    #[test]
    fn wait_follows_expires_but_not_below_the_minimum() {
        let now: DateTime<Utc> = "2026-11-06T18:30:00Z".parse().unwrap();
        let hour = Duration::from_secs(3600);
        let wait = |e, min| wait_until_next(e, now, min);
        let e = Some("Fri, 06 Nov 2026 18:58:37 GMT");
        assert_eq!(wait(e, Duration::ZERO), Duration::from_secs(28 * 60 + 37));
        assert_eq!(wait(e, 3 * hour), 3 * hour);
        assert_eq!(
            wait(Some("Fri, 06 Nov 2026 18:00:00 GMT"), Duration::ZERO),
            Duration::ZERO
        );
        assert_eq!(wait(None, Duration::ZERO), DEFAULT_EXPIRY);
        assert_eq!(wait(Some("garbage"), Duration::ZERO), DEFAULT_EXPIRY);
        assert_eq!(
            wait(Some("Sat, 01 Jan 2028 00:00:00 GMT"), Duration::ZERO),
            MAX_EXPIRY
        );
    }

    #[test]
    fn hourly_uses_one_hour_blocks_then_six_hour_blocks() {
        let f = parse(&doc(), 36.82, Utc::now()).unwrap();
        assert_eq!(
            [&f.source, &f.attribution],
            ["met-no", "Based on data from MET Norway"]
        );
        assert_eq!(f.generated_at.to_rfc3339(), "2026-11-06T17:18:29+00:00");
        let a = &f.hourly[0];
        assert_eq!(a.time.to_rfc3339(), "2026-11-06T18:00:00+00:00");
        assert_eq!(a.span_h, 1);
        assert_eq!(a.temp_c, Some(16.9));
        assert_eq!(a.wind_ms, Some(5.1));
        assert_eq!(a.wind_dir_deg, Some(22));
        assert_eq!(a.precip_mm, Some(0.0));
        assert_eq!(a.condition.as_deref(), Some("cloudy"));
        assert_eq!(a.text.as_deref(), Some("Cloudy"));
        assert!(
            a.pop.is_none(),
            "complete has no probability outside the Nordic region"
        );
        let six = f.hourly.iter().find(|h| h.span_h == 6).expect("a 6 h step");
        assert!(six.precip_mm.is_some() && six.condition.is_some());
        assert!(f.hourly.windows(2).all(|w| w[0].time < w[1].time));
    }

    #[test]
    fn probability_is_passed_through_when_present() {
        let mut d = doc();
        d["properties"]["timeseries"][0]["data"]["next_1_hours"]["details"]["probability_of_precipitation"] =
            37.4.into();
        let f = parse(&d, 36.82, Utc::now()).unwrap();
        assert_eq!(f.hourly[0].pop, Some(37));
        assert_eq!(f.daily[0].pop, Some(37));
    }

    #[test]
    fn daily_uses_solar_local_dates_and_noon_symbols() {
        let f = parse(&doc(), 36.82, Utc::now()).unwrap(); // UTC+2 by longitude
        let first = &f.daily[0];
        // 18:00Z on the 6th is 20:00 local, still the 6th; 22:00Z starts the 7th.
        assert_eq!(first.date.to_string(), "2026-11-06");
        assert_eq!(f.daily[1].date.to_string(), "2026-11-07");
        assert!(first.low_c.unwrap() <= first.high_c.unwrap());
        assert!(f.daily[1].condition.is_some());
        assert!(f.daily.iter().all(|d| d.text.is_none()));
        let mut conditions = f.daily.iter().map(|d| d.condition.as_deref().unwrap_or(""));
        assert!(conditions.all(|c| !c.ends_with("_day")));
        let rainy = f.daily.iter().any(|d| d.precip_mm.is_some_and(|p| p > 0.0));
        assert!(rainy, "some day should sum rain: {:?}", f.daily);
        // The first local day ends at 22:00Z.
        assert_eq!(first.end.to_rfc3339(), "2026-11-06T22:00:00+00:00");
    }

    #[test]
    fn rejects_documents_without_steps() {
        let parsed = |doc: Value| parse(&doc, 0.0, Utc::now());
        assert!(parsed(serde_json::json!({})).is_err());
        assert!(parsed(serde_json::json!({"properties":{"timeseries":[]}})).is_err());
    }
}
