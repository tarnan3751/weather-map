//! `GET /api/point`: the forecast for one location from the best provider
//! that answers (NWS inside its coverage, MET Norway elsewhere, then the
//! server's own model), cached and de-duplicated.

use crate::forecast::{self, PointInfo};
use crate::model::{HourlyPoint, PointForecast};
use crate::{GridKey, Inner, Lookup, error, header, metno, round};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use moka::future::Cache;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::timeout;
use wx_core::UpstreamGate;

/// Upstream requests this crate may have in flight at once.
pub(crate) const MAX_IN_FLIGHT: usize = 4;
/// How long a failed lookup or forecast fetch is remembered.
pub(crate) const FAILURE_MEMORY: Duration = Duration::from_secs(30);
/// The longest a tap waits on the NWS before moving on to the next provider.
const NWS_BUDGET: Duration = Duration::from_secs(6);
/// The same for MET Norway, and for the server's own model.
const MET_BUDGET: Duration = Duration::from_secs(6);
const MODEL_BUDGET: Duration = Duration::from_secs(3);
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_DELAY: Duration = Duration::from_millis(250);
const NWS_PAUSES: Pauses = Pauses(30, 60, 300);
/// Points are looked up at about 100 m resolution so nearby taps share an
/// entry; the grid cell is 2.5 km, so the forecast is unaffected.
const POINT_DECIMALS: i32 = 3;

#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum FetchError {
    #[error("not found")]
    NotFound,
    /// The shared gate is closed; nothing was sent.
    #[error("upstream requests are paused")]
    Paused,
    #[error("{0}")]
    Failed(String),
}

impl FetchError {
    pub(crate) fn failed(e: impl std::fmt::Display) -> Self {
        FetchError::Failed(e.to_string())
    }
}

/// How long, in seconds, a host's refusals close its gate: after a 429
/// without `Retry-After`, the least after a 403, and the most for either.
pub(crate) struct Pauses(pub u64, pub u64, pub u64);

/// One of `slots`, once the host's gate is open. The gate is checked after
/// the wait: requests queued behind one that was refused must not go out just
/// because they queued first.
pub(crate) async fn slot<'a>(
    slots: &'a Semaphore,
    gate: &UpstreamGate,
) -> Result<SemaphorePermit<'a>, FetchError> {
    let permit = slots
        .acquire()
        .await
        .map_err(|_| FetchError::failed("shutting down"))?;
    match gate.paused_for() {
        Some(_) => Err(FetchError::Paused),
        None => Ok(permit),
    }
}

/// A 429 or 403 closes the host's gate, for as long as `Retry-After` asks
/// within `pauses`, and is never retried.
pub(crate) fn refusal(
    gate: &UpstreamGate,
    resp: &reqwest::Response,
    url: &str,
    pauses: &Pauses,
) -> Option<FetchError> {
    let asked = header(resp.headers(), header::RETRY_AFTER).and_then(|v| v.trim().parse().ok());
    let secs = |s| Duration::from_secs(s);
    let (wait, hint) = match resp.status() {
        StatusCode::TOO_MANY_REQUESTS => (secs(asked.unwrap_or(pauses.0).clamp(1, pauses.2)), ""),
        StatusCode::FORBIDDEN => (
            secs(asked.unwrap_or(0).clamp(pauses.1, pauses.2)),
            " (check the User-Agent)",
        ),
        _ => return None,
    };
    gate.pause(wait);
    let status = resp.status().as_u16();
    Some(FetchError::Failed(format!(
        "{url}: HTTP {status}{hint}, pausing {wait:?}"
    )))
}

/// GET a JSON document from api.weather.gov. A plain 5xx or network error is
/// retried once. A 404 is an answer, not a failure.
async fn get_json(inner: &Inner, url: &str) -> Result<Value, FetchError> {
    let gate = &inner.config.nws_gate;
    let mut last = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(RETRY_DELAY).await;
        }
        let _slot = slot(&inner.upstream_slots, gate).await?;
        let req = inner.http.get(url).timeout(ATTEMPT_TIMEOUT);
        let resp = match req
            .header(header::ACCEPT, "application/geo+json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last = format!("{url}: {e}");
                continue;
            }
        };
        if let Some(e) = refusal(gate, &resp, url, &NWS_PAUSES) {
            return Err(e);
        }
        match resp.status() {
            StatusCode::NOT_FOUND => return Err(FetchError::NotFound),
            s if s.is_server_error() => last = format!("{url}: HTTP {s}"),
            s if !s.is_success() => return Err(FetchError::failed(format!("{url}: HTTP {s}"))),
            _ => match resp.json::<Value>().await {
                Ok(v) => return Ok(v),
                Err(e) => last = format!("{url}: bad body: {e}"),
            },
        }
    }
    Err(FetchError::Failed(last))
}

fn with_si_units(url: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}units=si")
}

/// Degrees from a scaled integer, with trailing zeros trimmed ("30.0").
fn coord_text(scaled: i64) -> String {
    let s = format!("{:.3}", scaled as f64 / 10f64.powi(POINT_DECIMALS));
    let s = s.trim_end_matches('0');
    format!("{s}{}", if s.ends_with('.') { "0" } else { "" })
}

async fn lookup_point(inner: &Inner, key: (i64, i64)) -> Result<Lookup, Arc<FetchError>> {
    if let Some(msg) = inner.points_failed.get(&key).await {
        return Err(Arc::new(FetchError::Failed(msg)));
    }
    let base = inner.config.nws_api_base.trim_end_matches('/');
    let url = format!("{base}/points/{},{}", coord_text(key.0), coord_text(key.1));
    let lookup = async {
        match get_json(inner, &url).await {
            Ok(doc) => Ok(Lookup::Found(Arc::new(
                forecast::parse_point(&doc).map_err(FetchError::failed)?,
            ))),
            Err(FetchError::NotFound) => Ok(Lookup::Outside),
            Err(e) => Err(e),
        }
    };
    let result = inner.points.try_get_with(key, lookup).await;
    if let Err(e) = &result {
        remember_failure(&inner.points_failed, key, e).await;
    }
    result
}

/// A closed gate already answers instantly, so only real failures are remembered.
pub(crate) async fn remember_failure<K>(cache: &Cache<K, String>, key: K, e: &FetchError)
where
    K: Hash + Eq + Send + Sync + 'static,
{
    if !matches!(e, FetchError::Paused) {
        cache.insert(key, e.to_string()).await;
    }
}

async fn fetch_grid(
    inner: &Inner,
    info: &PointInfo,
    key: &GridKey,
) -> Result<Arc<PointForecast>, FetchError> {
    // Only the hourly series is metric: the period text is shown to readers
    // as the NWS words it, in US units.
    let hourly_url = with_si_units(&info.hourly_url);
    let (hourly, daily) = (
        get_json(inner, &hourly_url),
        get_json(inner, &info.forecast_url),
    );
    let (hourly, daily) = tokio::try_join!(hourly, daily).map_err(|e| match e {
        // A missing grid is the NWS failing, not the tap being out of range.
        FetchError::NotFound => FetchError::failed("forecast grid not found"),
        e => e,
    })?;
    let grid = Arc::new(forecast::normalise(&hourly, &daily).map_err(FetchError::failed)?);
    inner.stale.insert(key.clone(), grid.clone()).await;
    Ok(grid)
}

/// Fresh forecast if possible, else the last good copy, else the error.
async fn grid_forecast(
    inner: &Inner,
    info: &PointInfo,
    deadline: Instant,
) -> Result<Arc<PointForecast>, FetchError> {
    let key: GridKey = (info.grid_id.clone(), info.grid_x, info.grid_y);
    if let Some(g) = inner.forecasts.get(&key).await {
        return Ok(g);
    }
    let err = match inner.grid_failed.get(&key).await {
        Some(msg) => FetchError::Failed(msg),
        None => {
            let left = deadline.saturating_duration_since(Instant::now());
            let fetch = inner
                .forecasts
                .try_get_with(key.clone(), fetch_grid(inner, info, &key));
            match timeout(left, fetch).await {
                Ok(Ok(g)) => return Ok(g),
                Ok(Err(e)) => {
                    remember_failure(&inner.grid_failed, key.clone(), &e).await;
                    (*e).clone()
                }
                Err(_) => FetchError::failed("timed out waiting for the NWS"),
            }
        }
    };
    let stale = inner.stale.get(&key).await.ok_or(err.clone())?;
    tracing::warn!("serving stale forecast for {key:?}: {err}");
    Ok(stale)
}

/// Entries already over at `now`. Done per response, not when caching, so a
/// cached forecast stays correct as it ages.
fn trim_past(f: &PointForecast, now: DateTime<Utc>) -> PointForecast {
    let live = |h: &&HourlyPoint| h.time + chrono::Duration::hours(h.span_h.into()) > now;
    let hourly = f.hourly.iter().filter(live).cloned().collect();
    let daily = f.daily.iter().filter(|d| d.end > now).cloned().collect();
    PointForecast {
        hourly,
        daily,
        ..f.clone()
    }
}

#[derive(Serialize)]
struct PointResponse<'a> {
    lat: f64,
    lon: f64,
    #[serde(flatten)]
    forecast: &'a PointForecast,
}

fn parse_coord(q: &HashMap<String, String>, key: &str, limit: f64) -> Option<f64> {
    let v: f64 = q.get(key)?.trim().parse().ok()?;
    (v.is_finite() && v.abs() <= limit).then_some(v)
}

/// Boxes (south, north, west, east) around the 50 states and the territories
/// the NWS forecasts. Deliberately a little generous: where a box takes in
/// foreign land or open sea, `/points` answers 404 or 500 and the next
/// provider takes over.
const NWS_BOXES: [(f64, f64, f64, f64); 8] = [
    (24.4, 49.6, -125.0, -66.5),    // contiguous states
    (51.0, 71.6, -180.0, -129.9),   // Alaska
    (51.0, 54.0, 172.0, 180.0),     // western Aleutians
    (18.5, 22.5, -161.0, -154.5),   // Hawaii
    (17.5, 18.8, -68.0, -64.4),     // Puerto Rico, US Virgin Islands
    (12.9, 20.6, 144.5, 146.2),     // Guam, Northern Marianas
    (-14.6, -14.1, -171.0, -168.1), // American Samoa
    (5.0, 20.0, 163.0, 172.0),      // Marshall Islands, Wake (NWS Guam forecasts them)
];

pub(crate) fn nws_covers(lat: f64, lon: f64) -> bool {
    let inside =
        |&(s, n, w, e): &(f64, f64, f64, f64)| (s..=n).contains(&lat) && (w..=e).contains(&lon);
    NWS_BOXES.iter().any(inside)
}

/// The NWS forecast for the point, with its place and time zone.
async fn nws_forecast(inner: &Inner, lat: f64, lon: f64) -> Result<PointForecast, FetchError> {
    let scale = 10f64.powi(POINT_DECIMALS);
    let key = ((lat * scale).round() as i64, (lon * scale).round() as i64);
    let deadline = Instant::now() + NWS_BUDGET;
    let looked_up = match timeout(NWS_BUDGET, lookup_point(inner, key)).await {
        Ok(r) => r.map_err(|e| (*e).clone())?,
        Err(_) => return Err(FetchError::failed("timed out waiting for the NWS")),
    };
    let Lookup::Found(info) = looked_up else {
        return Err(FetchError::NotFound);
    };
    let grid = grid_forecast(inner, &info, deadline).await?;
    let (place, time_zone) = (info.place.clone(), info.time_zone.clone());
    Ok(PointForecast {
        place,
        time_zone,
        ..(*grid).clone()
    })
}

/// Providers in order until one answers: the NWS where it covers, MET
/// Norway, then the server's own model.
async fn any_forecast(inner: &Inner, lat: f64, lon: f64) -> Option<PointForecast> {
    if nws_covers(lat, lon) {
        match nws_forecast(inner, lat, lon).await {
            Ok(f) => return Some(f),
            Err(FetchError::NotFound) => {}
            Err(e) => tracing::warn!("NWS point forecast failed: {e}"),
        }
    }
    match timeout(MET_BUDGET, metno::forecast(inner, lat, lon)).await {
        Ok(Ok(f)) => return Some((*f).clone()),
        Ok(Err(e)) => tracing::warn!("MET Norway point forecast failed: {e}"),
        Err(_) => tracing::warn!("MET Norway point forecast timed out"),
    }
    let answer = timeout(MODEL_BUDGET, inner.model.as_ref()?.forecast(lat, lon)).await;
    let mut f = answer
        .inspect_err(|_| tracing::warn!("point model timed out"))
        .ok()??;
    if f.source.is_empty() {
        f.source = "model".to_string();
    }
    Some(f)
}

pub(crate) async fn handler(
    State(inner): State<Arc<Inner>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (Some(lat), Some(lon)) = (parse_coord(&q, "lat", 90.0), parse_coord(&q, "lon", 180.0))
    else {
        let message = "lat and lon must be numbers in range";
        return error(StatusCode::BAD_REQUEST, "bad_request", message);
    };
    let Some(forecast) = any_forecast(&inner, lat, lon).await else {
        return error(
            StatusCode::BAD_GATEWAY,
            "upstream",
            "no forecast provider answered",
        );
    };
    let forecast = &trim_past(&forecast, Utc::now());
    let body = PointResponse {
        lat: round(lat, 4),
        lon: round(lon, 4),
        forecast,
    };
    ([(header::CACHE_CONTROL, "public, max-age=60")], Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DailyPoint;

    #[test]
    fn coordinates_are_trimmed() {
        assert_eq!(coord_text(35_223), "35.223");
        assert_eq!(coord_text(-97_440), "-97.44");
        assert_eq!(coord_text(30_000), "30.0");
        assert_eq!(coord_text(-60_000), "-60.0");
    }

    #[test]
    fn past_periods_are_trimmed() {
        let h = |t: &str, span_h| HourlyPoint {
            time: t.parse().unwrap(),
            span_h,
            ..Default::default()
        };
        let d = |e: &str| DailyPoint {
            end: e.parse().unwrap(),
            ..Default::default()
        };
        let hourly = ["12", "13", "14", "06", "12"].map(|t| format!("2026-11-06T{t}:00:00Z"));
        let spans = [1, 1, 1, 6, 6];
        let f = PointForecast {
            hourly: hourly.iter().zip(spans).map(|(t, s)| h(t, s)).collect(),
            daily: vec![d("2026-11-06T14:00:00Z"), d("2026-11-06T18:00:00Z")],
            ..Default::default()
        };
        let t = trim_past(&f, "2026-11-06T14:29:00Z".parse().unwrap());
        // 13:00 ended at 14:00; the 12:00 six-hour step runs to 18:00.
        let times: Vec<_> = t
            .hourly
            .iter()
            .map(|h| (h.time.to_rfc3339(), h.span_h))
            .collect();
        assert_eq!(
            times,
            [
                ("2026-11-06T14:00:00+00:00".into(), 1),
                ("2026-11-06T12:00:00+00:00".into(), 6)
            ]
        );
        assert_eq!(t.daily.len(), 1);
        let t = trim_past(&f, "2026-11-06T14:00:00Z".parse().unwrap());
        assert_eq!(
            (t.hourly.len(), t.daily.len()),
            (2, 1),
            "a period ending exactly now is over"
        );
    }

    #[test]
    fn nws_coverage_boxes() {
        for (lat, lon) in [
            (35.2, -97.4),
            (64.8, -147.7),
            (21.3, -157.9),
            (18.2, -66.5),
            (13.5, 144.8),
            (-14.3, -170.7),
            (52.0, 178.0),
        ] {
            assert!(nws_covers(lat, lon), "{lat},{lon}");
        }
        for (lat, lon) in [
            (51.5, -0.12),
            (-1.29, 36.82),
            (30.0, -60.0),
            (35.7, 139.7),
            (19.4, -99.1),
            (40.0, -170.0),
        ] {
            assert!(!nws_covers(lat, lon), "{lat},{lon}");
        }
    }

    #[test]
    fn si_suffix() {
        assert_eq!(with_si_units("http://x/f"), "http://x/f?units=si");
        assert_eq!(with_si_units("http://x/f?a=1"), "http://x/f?a=1&units=si");
    }
}
