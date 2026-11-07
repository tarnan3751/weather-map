//! End-to-end tests against a mock upstream served on a random local port.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Duration, Utc};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use futures::future::join_all;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tower_http::compression::CompressionLayer;
use wx_core::{FeedState, HealthRegistry, Hub, WsEvent};
use wx_obs::{Obs, ObsConfig, PointForecast, PointModel};

const METARS: &str = include_str!("fixtures/metars.csv");
const NAMES: &str = include_str!("fixtures/stations.json");
const POINTS: &str = include_str!("fixtures/points.json");
const HOURLY: &str = include_str!("fixtures/hourly.json");
const DAILY: &str = include_str!("fixtures/daily.json");
const NDBC: &str = include_str!("fixtures/ndbc.txt");
const AMEDAS_TABLE: &str = include_str!("fixtures/amedastable.json");
const AMEDAS_MAP: &str = include_str!("fixtures/amedas_map.json");
const MET: &str = include_str!("fixtures/metno_complete.json");

const ALL_FEEDS: &[&str] = &["stations", "stations-ndbc", "stations-amedas"];

#[derive(Default)]
struct Mock {
    base: Mutex<String>,
    points_hits: AtomicUsize,
    hourly_hits: AtomicUsize,
    daily_hits: AtomicUsize,
    metar_hits: AtomicUsize,
    metar_304: AtomicUsize,
    /// How many upcoming hourly requests answer 500.
    hourly_failures: AtomicUsize,
    always_fail_forecast: AtomicBool,
    metar_down: AtomicBool,
    points_down: AtomicBool,
    /// Every NWS route answers 429 with `Retry-After: 2` while set.
    rate_limited: AtomicBool,
    /// Every NWS route answers 403 while set.
    blocked: AtomicBool,
    inflight: AtomicUsize,
    max_inflight: AtomicUsize,
    /// Hours added to the forecast fixtures' times (they are re-based on now).
    time_offset_hours: AtomicI64,
    last_points_path: Mutex<String>,
    last_ua: Mutex<String>,
    last_accept: Mutex<String>,
    /// While set, `/points` answers 404 (a point the NWS does not cover).
    points_404: AtomicBool,
    ndbc_hits: AtomicUsize,
    ndbc_down: AtomicBool,
    amedas_latest_hits: AtomicUsize,
    amedas_table_hits: AtomicUsize,
    amedas_map_hits: AtomicUsize,
    amedas_down: AtomicBool,
    /// MET Norway answers 503 until this is set.
    met_up: AtomicBool,
    met_hits: AtomicUsize,
    met_304: AtomicUsize,
    /// A status the MET route answers with instead of data (429 gets `Retry-After: 30`).
    met_status: AtomicU16,
    /// Seconds from now that the next MET response says it expires.
    met_expires_secs: AtomicI64,
    /// Bumped to say the data changed: the `Last-Modified` moves.
    met_version: AtomicUsize,
    met_inflight: AtomicUsize,
    met_max_inflight: AtomicUsize,
    last_met_query: Mutex<String>,
    last_met_ua: Mutex<String>,
    last_met_ims: Mutex<Option<String>>,
}

fn header_of(h: &HeaderMap, name: header::HeaderName) -> Option<String> {
    Some(h.get(name)?.to_str().ok()?.to_string())
}

fn note_headers(m: &Mock, h: &HeaderMap) {
    *m.last_ua.lock().unwrap() = header_of(h, header::USER_AGENT).unwrap_or_default();
    *m.last_accept.lock().unwrap() = header_of(h, header::ACCEPT).unwrap_or_default();
}

/// Hold the request open briefly so concurrent ones really overlap, counting
/// the most in flight at once.
async fn overlap(inflight: &AtomicUsize, max: &AtomicUsize) {
    max.fetch_max(inflight.fetch_add(1, SeqCst) + 1, SeqCst);
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    inflight.fetch_sub(1, SeqCst);
}

fn refusal(m: &Mock) -> Option<Response> {
    if m.rate_limited.load(SeqCst) {
        return Some((StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "2")]).into_response());
    }
    m.blocked
        .load(SeqCst)
        .then(|| StatusCode::FORBIDDEN.into_response())
}

fn this_hour() -> DateTime<Utc> {
    let now = Utc::now();
    now - Duration::seconds(now.timestamp() % 3600)
        - Duration::nanoseconds(now.timestamp_subsec_nanos() as i64)
}

/// Re-base the fixture's period times so the first hourly period starts at the
/// top of the current hour (plus the mock's offset); `extra` shifts further.
fn rebase(doc: &str, m: &Mock, extra: Duration) -> String {
    let anchor: DateTime<Utc> = "2026-11-06T12:00:00Z".parse().unwrap();
    let delta = this_hour() - anchor + Duration::hours(m.time_offset_hours.load(SeqCst)) + extra;
    let mut v: Value = serde_json::from_str(doc).unwrap();
    for p in v["properties"]["periods"].as_array_mut().unwrap() {
        for k in ["startTime", "endTime"] {
            let t: DateTime<Utc> = p[k].as_str().unwrap().parse().unwrap();
            p[k] = Value::String((t + delta).to_rfc3339());
        }
    }
    v.to_string()
}

async fn points(State(m): State<Arc<Mock>>, h: HeaderMap, Path(coords): Path<String>) -> Response {
    m.points_hits.fetch_add(1, SeqCst);
    note_headers(&m, &h);
    overlap(&m.inflight, &m.max_inflight).await;
    if let Some(r) = refusal(&m) {
        return r;
    }
    *m.last_points_path.lock().unwrap() = coords.clone();
    if m.points_down.load(SeqCst) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    // Points off the continent have no grid.
    let lat: f64 = coords
        .split(',')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    if !(24.0..50.0).contains(&lat) || m.points_404.load(SeqCst) {
        return (
            StatusCode::NOT_FOUND,
            r#"{"title":"Data Unavailable For Requested Point"}"#,
        )
            .into_response();
    }
    let base = m.base.lock().unwrap().clone();
    POINTS
        .replace("https://api.weather.gov", &base)
        .into_response()
}

async fn forecast(
    State(m): State<Arc<Mock>>,
    h: HeaderMap,
    Path((_, _, file)): Path<(String, String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let hourly = file == "forecast/hourly";
    note_headers(&m, &h);
    let hits = if hourly {
        &m.hourly_hits
    } else {
        &m.daily_hits
    };
    hits.fetch_add(1, SeqCst);
    if let Some(r) = refusal(&m) {
        return r;
    }
    // Small delay so concurrent requests really overlap.
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    // Hourly is metric; the period forecast must come in the NWS's default US units.
    if q.get("units").map(String::as_str) != hourly.then_some("si") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let fail_once = hourly
        && m.hourly_failures
            .fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1))
            .is_ok();
    if fail_once || m.always_fail_forecast.load(SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    // Period forecasts are an hour later so the first one has not ended yet.
    let doc = if hourly {
        rebase(HOURLY, &m, Duration::zero())
    } else {
        rebase(DAILY, &m, Duration::hours(1))
    };
    doc.into_response()
}

/// Move every observation time in the fixture so the newest is ten minutes old.
fn shift_times(csv: &str, now: DateTime<Utc>) -> String {
    let anchor: DateTime<Utc> = "2026-11-06T12:15:00Z".parse().unwrap();
    let delta = now - Duration::minutes(10) - anchor;
    let mut out = String::new();
    let mut rest = csv;
    while let Some(i) = rest.find(",2026-11-06T") {
        out.push_str(&rest[..i + 1]);
        let t: DateTime<Utc> = rest[i + 1..i + 25].parse().unwrap();
        out.push_str(&(t + delta).format("%Y-%m-%dT%H:%M:%S.000Z").to_string());
        rest = &rest[i + 25..];
    }
    out.push_str(rest);
    out
}

fn gz(text: &str) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::fast());
    e.write_all(text.as_bytes()).unwrap();
    e.finish().unwrap()
}

async fn metars(State(m): State<Arc<Mock>>, h: HeaderMap) -> Response {
    m.metar_hits.fetch_add(1, SeqCst);
    if m.metar_down.load(SeqCst) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let etag = "\"v1\"";
    if header_of(&h, header::IF_NONE_MATCH).as_deref() == Some(etag) {
        m.metar_304.fetch_add(1, SeqCst);
        return StatusCode::NOT_MODIFIED.into_response();
    }
    ([(header::ETAG, etag)], gz(&shift_times(METARS, Utc::now()))).into_response()
}

/// Rewrite the date and time fields of every NDBC row to ten minutes ago.
fn fresh_ndbc(text: &str, now: DateTime<Utc>) -> String {
    let stamp = (now - Duration::minutes(10))
        .format("%Y %m %d %H %M")
        .to_string();
    let rows = text.lines().map(|l| {
        if l.starts_with('#') {
            return l.to_string();
        }
        let mut toks: Vec<&str> = l.split_whitespace().collect();
        toks.splice(3..8, stamp.split(' '));
        toks.join(" ")
    });
    rows.collect::<Vec<_>>().join("\n")
}

async fn ndbc(State(m): State<Arc<Mock>>) -> Response {
    m.ndbc_hits.fetch_add(1, SeqCst);
    if m.ndbc_down.load(SeqCst) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    fresh_ndbc(NDBC, Utc::now()).into_response()
}

async fn amedas_latest(State(m): State<Arc<Mock>>) -> Response {
    m.amedas_latest_hits.fetch_add(1, SeqCst);
    if m.amedas_down.load(SeqCst) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let jst = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
    let t = (Utc::now() - Duration::minutes(10)).with_timezone(&jst);
    t.format("%Y-%m-%dT%H:%M:00+09:00")
        .to_string()
        .into_response()
}

async fn amedas_table(State(m): State<Arc<Mock>>) -> &'static str {
    m.amedas_table_hits.fetch_add(1, SeqCst);
    AMEDAS_TABLE
}

async fn amedas_map(State(m): State<Arc<Mock>>, Path(name): Path<String>) -> Response {
    m.amedas_map_hits.fetch_add(1, SeqCst);
    let expected = name.ends_with("00.json") && name.len() == 19;
    assert!(expected, "unexpected snapshot name {name}");
    AMEDAS_MAP.into_response()
}

fn http_date(t: DateTime<Utc>) -> String {
    t.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// Move every step of the fixture so the first starts at the top of this hour.
fn rebase_met(doc: &str) -> String {
    let anchor: DateTime<Utc> = "2026-11-06T18:00:00Z".parse().unwrap();
    let delta = this_hour() - anchor;
    let mut v: Value = serde_json::from_str(doc).unwrap();
    for e in v["properties"]["timeseries"].as_array_mut().unwrap() {
        let t: DateTime<Utc> = e["time"].as_str().unwrap().parse().unwrap();
        e["time"] = Value::String((t + delta).format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }
    v.to_string()
}

async fn met(
    State(m): State<Arc<Mock>>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    m.met_hits.fetch_add(1, SeqCst);
    overlap(&m.met_inflight, &m.met_max_inflight).await;
    let ims = header_of(&h, header::IF_MODIFIED_SINCE);
    *m.last_met_ua.lock().unwrap() = header_of(&h, header::USER_AGENT).unwrap_or_default();
    *m.last_met_ims.lock().unwrap() = ims.clone();
    *m.last_met_query.lock().unwrap() = format!("lat={}&lon={}", q["lat"], q["lon"]);
    match m.met_status.load(SeqCst) {
        429 => {
            return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "30")]).into_response();
        }
        0 => {}
        status => return StatusCode::from_u16(status).unwrap().into_response(),
    }
    if !m.met_up.load(SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let expires = http_date(Utc::now() + Duration::seconds(m.met_expires_secs.load(SeqCst)));
    let lm = format!(
        "Fri, 06 Nov 2026 18:{:02}:00 GMT",
        m.met_version.load(SeqCst)
    );
    let validators = [
        (header::EXPIRES, expires),
        (header::LAST_MODIFIED, lm.clone()),
    ];
    if ims == Some(lm) {
        m.met_304.fetch_add(1, SeqCst);
        return (StatusCode::NOT_MODIFIED, validators).into_response();
    }
    (validators, rebase_met(MET)).into_response()
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

struct Rig {
    mock: Arc<Mock>,
    obs: Obs,
    hub: Hub,
    api: String,
    http: reqwest::Client,
    gate: wx_core::UpstreamGate,
    met_gate: wx_core::UpstreamGate,
}

/// Running station feeds, stopped when dropped.
struct Feeds {
    health: HealthRegistry,
    cancel: CancellationToken,
    handles: Vec<JoinHandle<()>>,
}

impl Drop for Feeds {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn rig() -> Rig {
    rig_with(None, |_| {}).await
}

/// A rig whose MET Norway answers.
async fn rig_met() -> Rig {
    let r = rig().await;
    r.mock.met_up.store(true, SeqCst);
    r
}

async fn rig_with(model: Option<Arc<dyn PointModel>>, tweak: impl FnOnce(&mut ObsConfig)) -> Rig {
    let mock = Arc::new(Mock::default());
    mock.met_expires_secs.store(3600, SeqCst);
    mock.met_version.store(28, SeqCst);
    let app = Router::new()
        .route("/points/{coords}", get(points))
        .route("/gridpoints/{office}/{xy}/{*file}", get(forecast))
        .route("/metars.cache.csv.gz", get(metars))
        .route("/stations.cache.json.gz", get(|| async { gz(NAMES) }))
        .route("/ndbc/latest_obs.txt", get(ndbc))
        .route("/amedas/data/latest_time.txt", get(amedas_latest))
        .route("/amedas/const/amedastable.json", get(amedas_table))
        .route("/amedas/data/map/{name}", get(amedas_map))
        .route("/weatherapi/locationforecast/2.0/complete", get(met))
        .with_state(mock.clone());
    let base = serve(app).await;
    *mock.base.lock().unwrap() = base.clone();
    let hub = Hub::new();
    let (gate, met_gate) = (wx_core::UpstreamGate::new(), wx_core::UpstreamGate::new());
    let mut config = ObsConfig {
        user_agent: "wx-obs-test".into(),
        nws_api_base: base.clone(),
        metar_url: format!("{base}/metars.cache.csv.gz"),
        ndbc_url: Some(format!("{base}/ndbc/latest_obs.txt")),
        amedas_base: Some(format!("{base}/amedas")),
        met_api_base: base.clone(),
        met_min_refetch: StdDuration::ZERO,
        nws_gate: gate.clone(),
        met_gate: met_gate.clone(),
    };
    tweak(&mut config);
    let obs = Obs::new(hub.clone(), config, model).unwrap();
    let api = serve(obs.router()).await;
    Rig {
        mock,
        obs,
        hub,
        api,
        http: reqwest::Client::new(),
        gate,
        met_gate,
    }
}

async fn wait_state(health: &HealthRegistry, name: &str, state: FeedState) {
    for _ in 0..200 {
        if health.get(name).is_some_and(|h| h.state == state) {
            return;
        }
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }
    panic!("{name} never became {state:?}: {:?}", health.get(name));
}

impl Rig {
    async fn get(&self, path: &str) -> reqwest::Response {
        let url = format!("{}{path}", self.api);
        self.http.get(url).send().await.unwrap()
    }

    /// The status of a conditional request.
    async fn revalidate(&self, path: &str, etag: &str) -> u16 {
        let req = self.http.get(format!("{}{path}", self.api));
        req.header(header::IF_NONE_MATCH, etag)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    async fn point(&self, query: impl std::fmt::Display) -> (u16, Value) {
        let r = self.get(&format!("/api/point?{query}")).await;
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn stations(&self, query: &str) -> Value {
        let r = self.get(&format!("/api/stations{query}")).await;
        assert_eq!(r.status(), 200, "{query}");
        r.json().await.unwrap()
    }

    async fn ids(&self, query: &str) -> Vec<String> {
        ids(&self.stations(query).await)
    }

    /// Start the station feeds and wait until the named ones are healthy.
    async fn feeds(&self, healthy: &[&str]) -> Feeds {
        let health = HealthRegistry::new(self.hub.clone());
        let cancel = CancellationToken::new();
        let handles = self.obs.spawn(&health, cancel.clone());
        for name in healthy {
            wait_state(&health, name, FeedState::Healthy).await;
        }
        Feeds {
            health,
            cancel,
            handles,
        }
    }
}

fn features(body: &Value) -> &Vec<Value> {
    body["features"].as_array().unwrap()
}

fn ids(body: &Value) -> Vec<String> {
    features(body)
        .iter()
        .map(|f| f["id"].as_str().unwrap().to_string())
        .collect()
}

fn feature<'a>(body: &'a Value, id: &str) -> &'a Value {
    features(body).iter().find(|f| f["id"] == id).expect(id)
}

fn sources(body: &Value) -> Vec<&String> {
    body["sources"].as_object().unwrap().keys().collect()
}

/// Statuses of concurrent taps, one per query.
async fn taps(r: &Rig, queries: impl Iterator<Item = String>) -> Vec<u16> {
    let answers = join_all(queries.map(|q| r.point(q))).await;
    answers.into_iter().map(|(s, _)| s).collect()
}

/// Concurrent taps on `n` distinct NWS points.
async fn storm(r: &Rig, n: usize) -> Vec<u16> {
    let lats = (0..n).map(|i| 30.0 + i as f64 * 0.1);
    taps(r, lats.map(|lat| format!("lat={lat}&lon=-97.4"))).await
}

#[tokio::test]
async fn stations_empty_until_first_refresh() {
    let body = rig().await.stations("").await;
    assert_eq!(body["type"], "FeatureCollection");
    assert_eq!(body["features"].as_array().unwrap().len(), 0);
    assert_eq!(body["sources"].as_object().unwrap().len(), 0);
}

#[tokio::test]
async fn stations_feeds_serve_one_collection_with_etag() {
    let r = rig().await;
    let mut events = r.hub.subscribe();
    let mut feeds = r.feeds(ALL_FEEDS).await;
    assert_eq!(feeds.handles.len(), 3);

    // The last event reports the total across feeds.
    let mut count = 0;
    while let Ok(ev) = events.try_recv() {
        if let WsEvent::Stations { count: c, .. } = &*ev {
            count = *c;
        }
    }

    let resp = r.get("/api/stations").await;
    assert_eq!(resp.status(), 200);
    let etag = resp.headers()[header::ETAG].to_str().unwrap().to_string();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(features(&body).len(), count);
    let by_id = |id| feature(&body, id);

    let koun = by_id("KOUN");
    assert_eq!(koun["properties"]["name"], "Norman/Univ Oklahoma Arpt, OK");
    assert_eq!(koun["properties"]["temp_c"], 8.3);
    assert_eq!(koun["properties"]["kind"], "airport");
    assert_eq!(koun["properties"]["source"], "metar");
    assert_eq!(koun["geometry"]["type"], "Point");
    // Airports anywhere are kept; unlocated ones are not.
    for id in ["EGLL", "YSSY", "RJTT", "FACT", "PHNL"] {
        by_id(id);
    }
    assert!(!ids(&body).contains(&"KQEQ".into()));

    let buoy = &by_id("ndbc:41013")["properties"];
    assert_eq!([&buoy["kind"], &buoy["source"]], ["buoy", "ndbc"]);
    assert_eq!(buoy["wave_height_m"], 1.2);
    assert_eq!(by_id("ndbc:AAMC1")["properties"]["kind"], "station");
    let jp = by_id("amedas:11016");
    assert_eq!(jp["properties"]["name"], "Wakkanai");
    assert_eq!(jp["properties"]["kind"], "station");
    assert_eq!(jp["properties"]["wind_dir_deg"], 225);

    assert_eq!(sources(&body), ["amedas", "metar", "ndbc"]);
    let credit = body["sources"]["amedas"]["attribution"].as_str().unwrap();
    assert!(credit.contains("Japan Meteorological Agency"));
    assert!(
        features(&body)
            .iter()
            .all(|f| f["properties"].get("raw").is_none())
    );
    assert_eq!(r.revalidate("/api/stations", &etag).await, 304);

    let age = Utc::now()
        - feeds
            .health
            .get("stations")
            .unwrap()
            .last_data_time
            .unwrap();
    assert!(age < Duration::minutes(30));
    let amedas = feeds.health.get("stations-amedas").unwrap();
    assert_eq!(amedas.detail, "6 stations");
    // One table, one snapshot, one poll of the time file.
    assert_eq!(r.mock.amedas_table_hits.load(SeqCst), 1);
    assert_eq!(r.mock.amedas_map_hits.load(SeqCst), 1);
    assert_eq!(r.mock.amedas_latest_hits.load(SeqCst), 1);

    feeds.cancel.cancel();
    for h in std::mem::take(&mut feeds.handles) {
        h.await.unwrap();
    }
}

#[tokio::test]
async fn stations_can_be_asked_for_by_bounding_box() {
    let r = rig().await;
    let _feeds = r.feeds(ALL_FEEDS).await;
    let all = r.ids("").await;

    let ok = r.ids("?bbox=-100,34,-95,37").await;
    assert!(ok.contains(&"KOUN".to_string()));
    assert!(!ok.contains(&"EGLL".to_string()) && ok.len() < all.len());

    // Japan: airport RJTT and the AMeDAS stations; the Korean buoy is west of the box.
    let jp = r.stations("?bbox=130,24,146,46").await;
    let jp_ids = ids(&jp);
    assert!(jp_ids.contains(&"RJTT".to_string()) && jp_ids.contains(&"amedas:11016".to_string()));
    assert!(!jp_ids.contains(&"KOUN".to_string()));
    assert_eq!(sources(&jp), ["amedas", "metar"]);

    // A view across the antimeridian: the Bering Sea buoy only.
    assert_eq!(r.ids("?bbox=170,50,-170,65").await, ["ndbc:46035"]);
    // The same view written with longitudes past 180.
    assert_eq!(r.ids("?bbox=170,50,190,65").await, ["ndbc:46035"]);
    // An empty view is a valid empty collection.
    assert_eq!(r.ids("?bbox=0,-80,5,-70").await.len(), 0);

    // raw text only on request.
    let raw = r.stations("?bbox=-100,34,-95,37&raw=1").await;
    let text = feature(&raw, "KOUN")["properties"]["raw"].as_str().unwrap();
    assert!(text.starts_with("METAR KOUN"));

    for bad in ["?bbox=1,2,3", "?bbox=a,b,c,d", "?bbox=0,40,10,30"] {
        let resp = r.get(&format!("/api/stations{bad}")).await;
        assert_eq!(resp.status(), 400, "{bad}");
        let v: Value = resp.json().await.unwrap();
        assert_eq!(v["error"], "bad_request");
    }

    // A filtered answer has its own validator.
    let path = "/api/stations?bbox=-100,34,-95,37";
    let first = r.get(path).await;
    let etag = first.headers()[header::ETAG].to_str().unwrap();
    assert_ne!(etag, r.get("/api/stations").await.headers()[header::ETAG]);
    assert_eq!(r.revalidate(path, etag).await, 304);
}

/// Through the compression layer the server wraps this router in, as a browser asks.
async fn layered(r: &Rig, uri: &str, gzip: bool) -> (HeaderMap, Vec<u8>) {
    let mut req = Request::get(uri);
    if gzip {
        req = req.header(header::ACCEPT_ENCODING, "gzip, deflate, br");
    }
    let app = r.obs.router().layer(CompressionLayer::new());
    let resp = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), 200, "{uri}");
    let headers = resp.headers().clone();
    let body = to_bytes(resp.into_body(), usize::MAX).await;
    (headers, body.unwrap().to_vec())
}

#[tokio::test]
async fn the_whole_world_is_sent_precompressed() {
    let r = rig().await;
    let _feeds = r.feeds(ALL_FEEDS).await;
    let header =
        |h: &HeaderMap, name: &str| header_of(h, name.parse().unwrap()).unwrap_or_default();
    let (h, plain) = layered(&r, "/api/stations", false).await;
    assert_eq!(header(&h, "content-encoding"), "");
    assert_eq!(header(&h, "content-length"), plain.len().to_string());
    let (gzh, gz) = layered(&r, "/api/stations", true).await;
    assert_eq!(header(&gzh, "content-encoding"), "gzip");
    assert_eq!(header(&gzh, "vary"), "Accept-Encoding");
    assert_eq!(header(&gzh, "content-length"), gz.len().to_string());
    assert_eq!(header(&gzh, "etag"), header(&h, "etag"));
    assert_eq!(header(&gzh, "cache-control"), "no-cache");
    let mut unzipped = Vec::new();
    GzDecoder::new(&gz[..]).read_to_end(&mut unzipped).unwrap();
    assert_eq!(unzipped, plain, "compressed once, not twice");
    // A box that holds every station is the same answer.
    let (wh, world) = layered(&r, "/api/stations?bbox=-180,-90,180,90", true).await;
    assert_eq!((header(&wh, "etag"), world), (header(&h, "etag"), gz));
    // A smaller one is compressed by the layer, with its own validator.
    let (bh, _) = layered(&r, "/api/stations?bbox=-130,20,-60,55", true).await;
    assert_eq!(header(&bh, "content-encoding"), "gzip");
    assert_ne!(header(&bh, "etag"), header(&h, "etag"));
}

#[tokio::test]
async fn a_failing_feed_does_not_blank_the_others() {
    // Airports down: buoys and Japan are still shown.
    let r = rig().await;
    r.mock.metar_down.store(true, SeqCst);
    let feeds = r.feeds(&["stations-ndbc", "stations-amedas"]).await;
    wait_state(&feeds.health, "stations", FeedState::BackingOff).await;
    let error = feeds.health.get("stations").unwrap().last_error.unwrap();
    assert!(error.contains("500"), "{error}");
    let shown = r.ids("").await;
    assert!(shown.iter().any(|i| i.starts_with("ndbc:")));
    assert!(shown.iter().any(|i| i.starts_with("amedas:")));
    assert!(shown.iter().all(|i| i != "KOUN"));
    drop(feeds);

    // Buoys and Japan down: airports are still shown.
    let r = rig().await;
    r.mock.ndbc_down.store(true, SeqCst);
    r.mock.amedas_down.store(true, SeqCst);
    let feeds = r.feeds(&["stations"]).await;
    wait_state(&feeds.health, "stations-ndbc", FeedState::BackingOff).await;
    wait_state(&feeds.health, "stations-amedas", FeedState::BackingOff).await;
    let shown = r.ids("").await;
    assert!(shown.contains(&"KOUN".to_string()) && shown.iter().all(|i| !i.contains(':')));
}

#[tokio::test]
async fn optional_feeds_can_be_switched_off() {
    let r = rig_with(None, |c| (c.ndbc_url, c.amedas_base) = (None, None)).await;
    let feeds = r.feeds(&["stations"]).await;
    assert_eq!(feeds.handles.len(), 1);
    let hits = r.mock.ndbc_hits.load(SeqCst) + r.mock.amedas_latest_hits.load(SeqCst);
    assert_eq!(hits, 0);
}

#[tokio::test]
async fn point_happy_path() {
    let r = rig().await;
    let (status, body) = r.point("lat=35.22261234&lon=-97.43951234").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["lat"], 35.2226);
    assert_eq!(body["lon"], -97.4395);
    assert_eq!(body["source"], "nws");
    assert_eq!(
        body["attribution"],
        "Forecast from the US National Weather Service"
    );
    assert_eq!(body["place"], "Norman, OK");
    assert_eq!(body["time_zone"], "America/Chicago");
    assert!(body.get("office").is_none());
    assert_eq!(body["hourly"].as_array().unwrap().len(), 4);
    let h0 = &body["hourly"][0];
    assert_eq!(h0["temp_c"], 9.0);
    assert_eq!(h0["span_h"], 1);
    assert!(h0["condition"].is_string() && h0["text"].is_string());
    assert!(h0["precip_mm"].is_null());
    assert!(body["hourly"][1]["pop"].is_null());
    // Half-day periods are merged into local dates, in SI.
    let daily = body["daily"].as_array().unwrap();
    assert!((1..=3).contains(&daily.len()), "{daily:?}");
    assert!(daily.iter().any(|d| d["high_c"] == 13.9));
    assert!(daily.iter().any(|d| d["low_c"] == 7.2 || d["low_c"] == 5.0));
    let text = |d: &Value, k: &str| d[k].as_str().unwrap_or("").to_string();
    assert!(daily.iter().all(|d| text(d, "date").len() == 10));
    assert!(daily.iter().any(|d| text(d, "text").contains("wind")));
    assert!(body["generated_at"].is_string());
    // More than four decimals would make the real API redirect.
    assert_eq!(*r.mock.last_points_path.lock().unwrap(), "35.223,-97.44");
    assert_eq!(*r.mock.last_ua.lock().unwrap(), "wx-obs-test");
    assert_eq!(*r.mock.last_accept.lock().unwrap(), "application/geo+json");
    assert_eq!(
        r.mock.met_hits.load(SeqCst),
        0,
        "inside NWS coverage MET Norway is not asked"
    );
}

#[tokio::test]
async fn point_validates_input() {
    let r = rig().await;
    for q in [
        "",
        "lat=35",
        "lat=abc&lon=1",
        "lat=95&lon=0",
        "lat=0&lon=181",
        "lat=NaN&lon=0",
    ] {
        let (status, body) = r.point(q).await;
        assert_eq!(status, 400, "{q}");
        assert_eq!(body["error"], "bad_request");
    }
    let hits = r.mock.points_hits.load(SeqCst) + r.mock.met_hits.load(SeqCst);
    assert_eq!(hits, 0);
}

#[tokio::test]
async fn persistent_upstream_failure_is_502() {
    let r = rig().await;
    r.mock.always_fail_forecast.store(true, SeqCst);
    let (status, body) = r.point("lat=35.2&lon=-97.4").await;
    assert_eq!(status, 502);
    assert_eq!(body["error"], "upstream");
    assert_eq!(
        r.mock.hourly_hits.load(SeqCst),
        2,
        "one quick retry, then give up"
    );
}

#[tokio::test]
async fn one_failure_is_retried() {
    let r = rig().await;
    r.mock.hourly_failures.store(1, SeqCst);
    let (status, body) = r.point("lat=35.2&lon=-97.4").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(r.mock.hourly_hits.load(SeqCst), 2);
}

#[tokio::test]
async fn nearby_points_share_one_forecast_fetch() {
    let r = rig().await;
    let (a, b) = tokio::join!(
        r.point("lat=35.2226&lon=-97.4395"),
        r.point("lat=35.2227&lon=-97.4396")
    );
    let (c, d) = tokio::join!(
        r.point("lat=35.2226&lon=-97.4395"),
        r.point("lat=35.2300&lon=-97.4300")
    );
    for (status, _) in [&a, &b, &c, &d] {
        assert_eq!(*status, 200);
    }
    assert_eq!(a.1["lat"], 35.2226);
    assert_eq!(
        b.1["lat"], 35.2227,
        "coordinates echo the request, not the cache entry"
    );
    // The first three share a ~100 m lookup; the fourth is a different one.
    assert_eq!(r.mock.points_hits.load(SeqCst), 2);
    assert_eq!(r.mock.hourly_hits.load(SeqCst), 1);
    assert_eq!(r.mock.daily_hits.load(SeqCst), 1);
}

#[tokio::test]
async fn rate_limit_storm_is_bounded_and_resumes() {
    let r = rig().await;
    r.mock.rate_limited.store(true, SeqCst);
    let statuses = storm(&r, 20).await;
    assert!(statuses.iter().all(|s| *s == 502), "{statuses:?}");
    let hits = r.mock.points_hits.load(SeqCst);
    assert!(hits <= 4, "{hits} upstream requests for 20 taps");
    let left = r.gate.paused_for().expect("gate paused");
    assert!(left > StdDuration::from_secs(1), "{left:?}");

    // Later taps are answered without touching the NWS.
    assert_eq!(r.point("lat=36.5&lon=-97.4").await.0, 502);
    assert_eq!(r.mock.points_hits.load(SeqCst), hits);

    // Once Retry-After has passed, requests flow again.
    r.mock.rate_limited.store(false, SeqCst);
    tokio::time::sleep(StdDuration::from_millis(2300)).await;
    assert_eq!(r.point("lat=37.5&lon=-97.4").await.0, 200);
    assert!(r.mock.points_hits.load(SeqCst) > hits);
}

#[tokio::test]
async fn forbidden_pauses_the_gate_for_a_minute() {
    let r = rig().await;
    r.mock.blocked.store(true, SeqCst);
    assert_eq!(r.point("lat=35.2&lon=-97.4").await.0, 502);
    assert_eq!(r.mock.points_hits.load(SeqCst), 1, "403 is not retried");
    let left = r.gate.paused_for().unwrap();
    assert!(left > StdDuration::from_secs(55), "{left:?}");
    assert!(
        r.met_gate.paused_for().is_none(),
        "the other host is unaffected"
    );
}

#[tokio::test]
async fn failures_are_remembered_briefly() {
    let r = rig().await;
    r.mock.always_fail_forecast.store(true, SeqCst);
    for _ in 0..5 {
        assert_eq!(r.point("lat=35.2&lon=-97.4").await.0, 502);
    }
    assert_eq!(r.mock.hourly_hits.load(SeqCst), 2, "one chain, not five");
    assert_eq!(r.mock.points_hits.load(SeqCst), 1);
    assert_eq!(
        r.mock.met_hits.load(SeqCst),
        1,
        "MET Norway failure is remembered too"
    );

    r.mock.points_down.store(true, SeqCst);
    for _ in 0..5 {
        assert_eq!(r.point("lat=40.2&lon=-90.4").await.0, 502);
    }
    assert_eq!(
        r.mock.points_hits.load(SeqCst),
        3,
        "one chain of two for the new point"
    );
}

#[tokio::test]
async fn upstream_concurrency_is_limited() {
    let r = rig().await;
    assert!(storm(&r, 16).await.iter().all(|s| *s == 200));
    let max = r.mock.max_inflight.load(SeqCst);
    assert!((2..=4).contains(&max), "max in flight {max}");
}

#[tokio::test]
async fn paused_gate_serves_the_stale_forecast() {
    let r = rig().await;
    assert_eq!(r.point("lat=35.2226&lon=-97.4395").await.0, 200);
    assert_eq!(r.point("lat=35.5&lon=-97.0").await.0, 200); // second point looked up while open
    let hits = r.mock.hourly_hits.load(SeqCst);
    r.obs.expire_fresh_forecasts();
    r.gate.pause(StdDuration::from_secs(30));
    let (status, body) = r.point("lat=35.5&lon=-97.0").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["source"], "nws",
        "stale NWS data beats another provider"
    );
    assert_eq!(body["hourly"].as_array().unwrap().len(), 4);
    assert_eq!(r.mock.hourly_hits.load(SeqCst), hits);
    assert_eq!(r.mock.met_hits.load(SeqCst), 0);
}

#[tokio::test]
async fn periods_already_over_are_dropped() {
    let r = rig().await;
    let (_, body) = r.point("lat=35.2&lon=-97.4").await;
    assert_eq!(body["hourly"].as_array().unwrap().len(), 4);

    // Same grid, but the forecast fixtures now start three hours ago.
    let r = rig().await;
    r.mock.time_offset_hours.store(-3, SeqCst);
    let (status, body) = r.point("lat=35.2&lon=-97.4").await;
    assert_eq!(status, 200);
    let hourly = body["hourly"].as_array().unwrap();
    assert_eq!(hourly.len(), 1, "{hourly:?}");
    let t: DateTime<Utc> = hourly[0]["time"].as_str().unwrap().parse().unwrap();
    assert!(t <= Utc::now() && t + Duration::hours(1) > Utc::now());
    assert!(!body["daily"].as_array().unwrap().is_empty());
}

// ---- routing and fallback

#[tokio::test]
async fn outside_the_nws_the_forecast_comes_from_met_norway() {
    let r = rig_met().await;
    let (status, body) = r.point("lat=-1.29212345&lon=36.82198765").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["source"], "met-no");
    assert_eq!(body["attribution"], "Based on data from MET Norway");
    let at = (body["lat"].as_f64(), body["lon"].as_f64());
    assert_eq!(at, (Some(-1.2921), Some(36.822)));
    assert!(body["place"].is_null() && body["time_zone"].is_null());
    assert!(body.get("office").is_none());
    let hourly = body["hourly"].as_array().unwrap();
    assert!(hourly.len() > 30);
    assert_eq!(hourly[0]["span_h"], 1);
    assert!(hourly[0]["temp_c"].is_number() && hourly[0]["wind_ms"].is_number());
    assert!(
        hourly[0]["pop"].is_null(),
        "no probability outside the Nordic region"
    );
    assert!(hourly[0]["condition"].is_string() && hourly[0]["text"].is_string());
    assert!(hourly.iter().any(|h| h["span_h"] == 6));
    let daily = body["daily"].as_array().unwrap();
    assert!(daily.len() >= 2 && daily[0]["low_c"].is_number() && daily[0]["text"].is_null());
    // Sent upstream with two decimals, the configured identity, and no NWS traffic.
    let query = r.mock.last_met_query.lock().unwrap().clone();
    assert_eq!(query, "lat=-1.29&lon=36.82");
    assert_eq!(*r.mock.last_met_ua.lock().unwrap(), "wx-obs-test");
    assert_eq!(r.mock.points_hits.load(SeqCst), 0);
    // London, Tokyo and Sydney are not NWS points either.
    for q in [
        "lat=51.5&lon=-0.12",
        "lat=35.7&lon=139.7",
        "lat=-33.9&lon=151.2",
    ] {
        assert_eq!(r.point(q).await.1["source"], "met-no", "{q}");
    }
    assert_eq!(r.mock.points_hits.load(SeqCst), 0);
}

#[tokio::test]
async fn a_point_the_nws_declines_falls_back_to_met_norway() {
    let r = rig_met().await;
    r.mock.points_404.store(true, SeqCst);
    // Inside a coverage box, but /points says no (Canadian prairies, say).
    let (status, body) = r.point("lat=49.0&lon=-100.0").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["source"], "met-no");
    assert_eq!(r.mock.points_hits.load(SeqCst), 1);
}

#[tokio::test]
async fn an_nws_outage_falls_back_to_met_norway() {
    let r = rig_met().await;
    r.mock.always_fail_forecast.store(true, SeqCst);
    let (status, body) = r.point("lat=35.2&lon=-97.4").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["source"], "met-no");

    // A paused NWS gate is skipped at once.
    let r = rig_met().await;
    r.gate.pause(StdDuration::from_secs(30));
    let (status, body) = r.point("lat=36.2&lon=-97.4").await;
    assert_eq!((status, body["source"].as_str()), (200, Some("met-no")));
    assert_eq!(r.mock.points_hits.load(SeqCst), 0);
}

#[tokio::test]
async fn met_norway_is_not_asked_again_before_it_expires() {
    let r = rig_met().await;
    // Same 0.01 degree cell, different taps.
    for q in [
        "lat=-1.2921&lon=36.8219",
        "lat=-1.2900&lon=36.8200",
        "lat=-1.2851&lon=36.8249",
    ] {
        assert_eq!(r.point(q).await.0, 200);
    }
    assert_eq!(r.mock.met_hits.load(SeqCst), 1);
    // The next cell is a different point.
    assert_eq!(r.point("lat=-1.31&lon=36.82").await.0, 200);
    assert_eq!(r.mock.met_hits.load(SeqCst), 2);
    assert!(
        r.mock.last_met_ims.lock().unwrap().is_none(),
        "a first request is unconditional"
    );
}

#[tokio::test]
async fn concurrent_taps_on_one_point_share_one_met_request() {
    let r = rig_met().await;
    let statuses = taps(&r, (0..6).map(|_| "lat=-1.29&lon=36.82".to_string())).await;
    assert!(statuses.iter().all(|s| *s == 200));
    assert_eq!(r.mock.met_hits.load(SeqCst), 1);
}

#[tokio::test]
async fn after_expiry_met_norway_gets_a_conditional_request_and_304_keeps_the_forecast() {
    let r = rig_met().await;
    r.mock.met_expires_secs.store(1, SeqCst);
    let (_, first) = r.point("lat=-1.29&lon=36.82").await;
    assert_eq!(r.mock.met_hits.load(SeqCst), 1);

    // Just before the expiry nothing is sent.
    assert_eq!(r.point("lat=-1.29&lon=36.82").await.0, 200);
    assert_eq!(r.mock.met_hits.load(SeqCst), 1);

    // After it, the request carries the exact Last-Modified we were given.
    r.mock.met_expires_secs.store(3600, SeqCst);
    tokio::time::sleep(StdDuration::from_millis(2100)).await;
    let (status, second) = r.point("lat=-1.29&lon=36.82").await;
    assert_eq!(status, 200);
    assert_eq!(r.mock.met_hits.load(SeqCst), 2);
    assert_eq!(r.mock.met_304.load(SeqCst), 1);
    let ims = r.mock.last_met_ims.lock().unwrap().clone();
    assert_eq!(ims.as_deref(), Some("Fri, 06 Nov 2026 18:28:00 GMT"));
    assert_eq!(first["generated_at"], second["generated_at"]);
    assert_eq!(
        first["hourly"], second["hourly"],
        "the 304 kept the cached forecast"
    );

    // The 304 carried a new Expires: quiet again.
    assert_eq!(r.point("lat=-1.29&lon=36.82").await.0, 200);
    assert_eq!(r.mock.met_hits.load(SeqCst), 2);
}

#[tokio::test]
async fn changed_data_replaces_the_forecast_and_the_validator() {
    let r = rig_met().await;
    r.mock.met_expires_secs.store(1, SeqCst);
    assert_eq!(r.point("lat=-1.29&lon=36.82").await.0, 200);
    r.mock.met_version.store(40, SeqCst);
    tokio::time::sleep(StdDuration::from_millis(2100)).await;
    assert_eq!(r.point("lat=-1.29&lon=36.82").await.0, 200);
    assert_eq!(
        r.mock.met_304.load(SeqCst),
        0,
        "the old validator no longer matches"
    );
    assert_eq!(r.mock.met_hits.load(SeqCst), 2);
    r.mock.met_expires_secs.store(1, SeqCst);
    tokio::time::sleep(StdDuration::from_millis(2100)).await;
    assert_eq!(r.point("lat=-1.29&lon=36.82").await.0, 200);
    let ims = r.mock.last_met_ims.lock().unwrap().clone();
    assert_eq!(ims.as_deref(), Some("Fri, 06 Nov 2026 18:40:00 GMT"));
}

#[tokio::test]
async fn the_published_minimum_between_requests_outlasts_a_short_expires() {
    let r = rig_with(None, |c| c.met_min_refetch = StdDuration::from_secs(3600)).await;
    r.mock.met_up.store(true, SeqCst);
    r.mock.met_expires_secs.store(1, SeqCst);
    assert_eq!(r.point("lat=-1.29&lon=36.82").await.0, 200);
    tokio::time::sleep(StdDuration::from_millis(1500)).await;
    assert_eq!(r.point("lat=-1.29&lon=36.82").await.0, 200);
    assert_eq!(r.mock.met_hits.load(SeqCst), 1);
}

#[tokio::test]
async fn met_norway_failure_serves_the_stale_forecast_without_hammering() {
    let r = rig_met().await;
    r.mock.met_expires_secs.store(1, SeqCst);
    let (_, first) = r.point("lat=-1.29&lon=36.82").await;
    r.mock.met_up.store(false, SeqCst);
    tokio::time::sleep(StdDuration::from_millis(2100)).await;
    for _ in 0..4 {
        let (status, body) = r.point("lat=-1.29&lon=36.82").await;
        assert_eq!(status, 200);
        assert_eq!(body["source"], "met-no");
        assert_eq!(body["generated_at"], first["generated_at"]);
    }
    assert_eq!(
        r.mock.met_hits.load(SeqCst),
        2,
        "one failed refresh, then quiet"
    );

    // A point never fetched has nothing stale to show: 502, and one try only.
    for _ in 0..3 {
        assert_eq!(r.point("lat=10.0&lon=20.0").await.0, 502);
    }
    assert_eq!(r.mock.met_hits.load(SeqCst), 3);
}

#[tokio::test]
async fn met_norway_rate_limit_pauses_only_its_own_gate() {
    let r = rig().await;
    r.mock.met_status.store(429, SeqCst);
    assert_eq!(r.point("lat=-1.29&lon=36.82").await.0, 502);
    let left = r.met_gate.paused_for().expect("MET gate paused");
    assert!(left > StdDuration::from_secs(25), "{left:?}");
    assert!(r.gate.paused_for().is_none());
    // While paused nothing is sent, for this point or another.
    assert_eq!(r.point("lat=48.8&lon=2.35").await.0, 502);
    assert_eq!(r.mock.met_hits.load(SeqCst), 1);
    // The NWS keeps working.
    assert_eq!(r.point("lat=35.2&lon=-97.4").await.0, 200);

    // And the other way round: an NWS pause does not stop MET Norway.
    let r = rig_met().await;
    r.gate.pause(StdDuration::from_secs(60));
    assert_eq!(r.point("lat=48.8&lon=2.35").await.1["source"], "met-no");
}

#[tokio::test]
async fn met_norway_403_pauses_for_a_long_time() {
    let r = rig().await;
    r.mock.met_status.store(403, SeqCst);
    assert_eq!(r.point("lat=-1.29&lon=36.82").await.0, 502);
    assert!(r.met_gate.paused_for().unwrap() > StdDuration::from_secs(300));
}

#[tokio::test]
async fn met_norway_concurrency_is_capped() {
    let r = rig_met().await;
    let queries = (0..12).map(|i| format!("lat={}&lon=20.0", 10.0 + i as f64));
    let statuses = taps(&r, queries).await;
    assert!(statuses.iter().all(|s| *s == 200));
    assert_eq!(r.mock.met_hits.load(SeqCst), 12);
    let max = r.mock.met_max_inflight.load(SeqCst);
    assert!((1..=2).contains(&max), "max in flight {max}");
}

struct FakeModel {
    calls: AtomicUsize,
    source: &'static str,
    answer: bool,
}

impl FakeModel {
    fn new(source: &'static str, answer: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            source,
            answer,
        })
    }
}

impl PointModel for FakeModel {
    fn forecast(
        &self,
        lat: f64,
        lon: f64,
    ) -> futures::future::BoxFuture<'_, Option<PointForecast>> {
        self.calls.fetch_add(1, SeqCst);
        let hour = wx_obs::HourlyPoint {
            time: Utc::now() - Duration::minutes(10),
            span_h: 1,
            temp_c: Some(21.5),
            precip_mm: Some(0.4),
            wind_ms: Some(3.0),
            wind_dir_deg: Some(90),
            condition: Some("cloudy".into()),
            text: Some("Cloudy".into()),
            ..Default::default()
        };
        let f = PointForecast {
            source: self.source.to_string(),
            attribution: format!("model at {lat},{lon}"),
            generated_at: Utc::now(),
            hourly: vec![hour],
            ..Default::default()
        };
        let answer = self.answer;
        Box::pin(async move { answer.then_some(f) })
    }
}

#[tokio::test]
async fn the_model_hook_answers_when_the_outside_providers_cannot() {
    let model = FakeModel::new("grid-model", true);
    let r = rig_with(Some(model.clone()), |_| {}).await;
    // MET Norway is down (the default in the mock): the hook is used.
    let (status, body) = r.point("lat=-1.29&lon=36.82").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["source"], "grid-model");
    assert_eq!(body["attribution"], "model at -1.29,36.82");
    assert_eq!(body["hourly"][0]["temp_c"], 21.5);
    assert_eq!(body["lat"], -1.29);
    assert_eq!(model.calls.load(SeqCst), 1);

    // An NWS point with the NWS and MET Norway both failing also reaches it.
    r.mock.always_fail_forecast.store(true, SeqCst);
    assert_eq!(
        r.point("lat=35.2&lon=-97.4").await.1["source"],
        "grid-model"
    );

    // Once MET Norway answers, the hook is not consulted.
    r.mock.met_up.store(true, SeqCst);
    let before = model.calls.load(SeqCst);
    assert_eq!(r.point("lat=48.8&lon=2.35").await.1["source"], "met-no");
    assert_eq!(model.calls.load(SeqCst), before);
}

#[tokio::test]
async fn a_model_that_declines_or_names_nothing() {
    let declines = FakeModel::new("grid-model", false);
    let r = rig_with(Some(declines.clone()), |_| {}).await;
    let (status, body) = r.point("lat=-1.29&lon=36.82").await;
    assert_eq!((status, body["error"].as_str()), (502, Some("upstream")));
    assert_eq!(declines.calls.load(SeqCst), 1);

    let r = rig_with(Some(FakeModel::new("", true)), |_| {}).await;
    assert_eq!(r.point("lat=-1.29&lon=36.82").await.1["source"], "model");
}

#[tokio::test]
async fn without_a_hook_total_failure_is_the_existing_502() {
    let (status, body) = rig().await.point("lat=-1.29&lon=36.82").await;
    assert_eq!(status, 502);
    assert_eq!(body["error"], "upstream");
    assert!(body["message"].is_string());
}
