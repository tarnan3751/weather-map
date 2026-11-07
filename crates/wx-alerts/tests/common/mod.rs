//! Test doubles: a mock api.weather.gov, a mock push service, a browser
//! subscriber that can decrypt what it receives, and NWS-shaped alerts; and
//! the crate under test, served on a real port.

#![allow(dead_code)]

pub mod world;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, Utc};
use hkdf::Hkdf;
use p256::elliptic_curve::Generate;
use p256::elliptic_curve::sec1::ToSec1Point;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgRow, Postgres};
use sqlx::{FromRow, PgPool};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wx_alerts::{Alerts, AlertsConfig, PollReport, Vapid, WorldConfig, WorldReport};
use wx_core::{HealthRegistry, Hub, WsEvent};

pub async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}")
}

/// Poll `check` until it holds, for up to 30 seconds.
pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !check().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Mock api.weather.gov

/// A status to answer with, and a Retry-After in seconds.
type Failure = (u16, Option<u64>);

#[derive(Default)]
struct Nws {
    features: Vec<Value>,
    /// Answer `/alerts/active` with this instead of the feed.
    alerts_failure: Option<Failure>,
    zones: HashMap<String, Value>,
    /// Statuses to answer for a zone before serving it.
    zone_failures: HashMap<String, VecDeque<u16>>,
    hits: HashMap<String, usize>,
    not_modified: usize,
    user_agents: Vec<String>,
    accepts: Vec<String>,
}

type NwsState = Arc<Mutex<Nws>>;

pub struct MockNws {
    pub base: String,
    state: NwsState,
}

impl MockNws {
    pub async fn start() -> Self {
        let state = NwsState::default();
        let router = Router::new()
            .route("/alerts/active", get(nws_alerts))
            .route("/zones/{kind}/{id}", get(nws_zone))
            .with_state(state.clone());
        Self {
            base: serve(router).await,
            state,
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Nws) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }

    pub fn set_alerts(&self, features: Vec<Value>) {
        self.with(|s| s.features = features);
    }

    /// Make `/alerts/active` answer `status` (with `Retry-After`), or work again.
    pub fn fail_alerts(&self, failure: Option<Failure>) {
        self.with(|s| s.alerts_failure = failure);
    }

    pub fn set_zone(&self, kind: &str, id: &str, feature: Value) {
        self.with(|s| s.zones.insert(format!("{kind}/{id}"), feature));
    }

    pub fn fail_zone(&self, kind: &str, id: &str, statuses: &[u16]) {
        self.with(|s| {
            s.zone_failures
                .insert(format!("{kind}/{id}"), statuses.iter().copied().collect())
        });
    }

    pub fn hits(&self, path: &str) -> usize {
        self.with(|s| s.hits.get(path).copied().unwrap_or(0))
    }

    pub fn zone_hits_total(&self) -> usize {
        self.with(|s| {
            s.hits
                .iter()
                .filter(|(k, _)| k.starts_with("/zones/"))
                .map(|(_, v)| v)
                .sum()
        })
    }

    pub fn not_modified_count(&self) -> usize {
        self.with(|s| s.not_modified)
    }

    pub fn user_agents(&self) -> Vec<String> {
        self.with(|s| s.user_agents.clone())
    }

    pub fn accepts(&self) -> Vec<String> {
        self.with(|s| s.accepts.clone())
    }
}

fn record(s: &mut Nws, path: String, headers: &HeaderMap) {
    *s.hits.entry(path).or_default() += 1;
    s.user_agents
        .push(header_str(headers, header::USER_AGENT).unwrap_or_default());
    s.accepts
        .push(header_str(headers, header::ACCEPT).unwrap_or_default());
}

async fn nws_alerts(State(state): State<NwsState>, headers: HeaderMap) -> Response {
    let mut s = state.lock().unwrap();
    record(&mut s, "/alerts/active".into(), &headers);
    if let Some((status, retry_after)) = s.alerts_failure {
        let status = StatusCode::from_u16(status).unwrap();
        return match retry_after {
            Some(secs) => (status, [(header::RETRY_AFTER, secs.to_string())]).into_response(),
            None => status.into_response(),
        };
    }
    let body = json!({
        "@context": [], "type": "FeatureCollection",
        "features": s.features, "title": "Current watches, warnings, and advisories",
    })
    .to_string();
    // Like api.weather.gov behind its CDN: a weak ETag. Unlike it, this one
    // honours If-None-Match, so both paths get exercised.
    let etag = format!("W/\"{}\"", hex(&Sha256::digest(body.as_bytes())[..8]));
    if header_str(&headers, header::IF_NONE_MATCH).as_deref() == Some(etag.as_str()) {
        s.not_modified += 1;
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    let content_type = "application/geo+json".to_string();
    (
        [(header::CONTENT_TYPE, content_type), (header::ETAG, etag)],
        body,
    )
        .into_response()
}

async fn nws_zone(
    State(state): State<NwsState>,
    Path((kind, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut s = state.lock().unwrap();
    let key = format!("{kind}/{id}");
    record(&mut s, format!("/zones/{key}"), &headers);
    if let Some(status) = s.zone_failures.get_mut(&key).and_then(VecDeque::pop_front) {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    match s.zones.get(&key) {
        Some(feature) => (
            [(header::CONTENT_TYPE, "application/geo+json")],
            feature.to_string(),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/problem+json")],
            r#"{"title":"Not Found"}"#,
        )
            .into_response(),
    }
}

/// A zone feature as `/zones/{kind}/{id}` returns it.
pub fn zone_feature(kind: &str, id: &str, ring: &[[f64; 2]]) -> Value {
    json!({
        "id": format!("https://api.weather.gov/zones/{kind}/{id}"),
        "type": "Feature",
        "geometry": { "type": "Polygon", "coordinates": [ring] },
        "properties": { "id": id, "type": if kind == "county" { "county" } else { "public" },
                        "name": format!("Zone {id}"), "state": &id[..2] }
    })
}

// ---------------------------------------------------------------------------
// Mock push service

#[derive(Debug, Clone)]
pub struct Captured {
    pub path: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

#[derive(Default)]
struct Push {
    received: Vec<Captured>,
    /// Statuses to answer, in order; 201 once exhausted.
    script: VecDeque<u16>,
}

type PushState = Arc<Mutex<Push>>;

pub struct MockPush {
    pub base: String,
    state: PushState,
}

impl MockPush {
    pub async fn start() -> Self {
        let state = PushState::default();
        let router = Router::new()
            .route("/push/{id}", post(push_receive))
            .with_state(state.clone());
        Self {
            base: serve(router).await,
            state,
        }
    }

    pub fn endpoint(&self, name: &str) -> String {
        format!("{}/push/{name}", self.base)
    }

    pub fn script(&self, statuses: &[u16]) {
        self.state.lock().unwrap().script = statuses.iter().copied().collect();
    }

    pub fn received(&self) -> Vec<Captured> {
        self.state.lock().unwrap().received.clone()
    }

    pub fn count(&self) -> usize {
        self.state.lock().unwrap().received.len()
    }

    /// Pushes received once things have had a moment to settle.
    pub async fn count_after_a_while(&self) -> usize {
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.count()
    }
}

async fn push_receive(
    State(state): State<PushState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let mut s = state.lock().unwrap();
    s.received.push(Captured {
        path: format!("/push/{id}"),
        headers,
        body,
    });
    StatusCode::from_u16(s.script.pop_front().unwrap_or(201)).unwrap()
}

// ---------------------------------------------------------------------------
// A browser's push subscription

pub struct Subscriber {
    pub endpoint: String,
    secret: p256::SecretKey,
    auth: [u8; 16],
}

impl Subscriber {
    pub fn new(endpoint: String) -> Self {
        let mut auth = [0u8; 16];
        getrandom::fill(&mut auth).unwrap();
        Self {
            endpoint,
            secret: p256::SecretKey::generate(),
            auth,
        }
    }

    /// What `PushSubscription.toJSON()` gives the page.
    pub fn json(&self) -> Value {
        json!({
            "endpoint": self.endpoint,
            "expirationTime": null,
            "keys": {
                "p256dh": B64.encode(self.secret.public_key().to_sec1_point(false).as_bytes()),
                "auth": B64.encode(self.auth),
            }
        })
    }

    /// RFC 8291 decryption, written from the RFC independently of the
    /// crate's encryptor.
    pub fn decrypt(&self, body: &[u8]) -> Vec<u8> {
        let salt = &body[..16];
        let rs = u32::from_be_bytes(body[16..20].try_into().unwrap());
        let idlen = body[20] as usize;
        let keyid = &body[21..21 + idlen];
        let record = &body[21 + idlen..];
        assert_eq!(rs, 4096);
        assert_eq!(idlen, 65);
        assert!(record.len() <= rs as usize);

        let sender = p256::PublicKey::from_sec1_bytes(keyid).unwrap();
        let shared = self.secret.diffie_hellman(&sender);
        let ua_public = self.secret.public_key().to_sec1_point(false);
        let (_, prk_key) =
            Hkdf::<Sha256>::extract(Some(&self.auth), shared.raw_secret_bytes().as_slice());
        let mut key_info = b"WebPush: info\x00".to_vec();
        key_info.extend_from_slice(ua_public.as_bytes());
        key_info.extend_from_slice(keyid);
        let mut ikm = [0u8; 32];
        prk_key.expand(&key_info, &mut ikm).unwrap();
        let (_, prk) = Hkdf::<Sha256>::extract(Some(salt), &ikm);
        let mut cek = [0u8; 16];
        let mut nonce = [0u8; 12];
        prk.expand(b"Content-Encoding: aes128gcm\x00", &mut cek)
            .unwrap();
        prk.expand(b"Content-Encoding: nonce\x00", &mut nonce)
            .unwrap();
        let mut plain = Aes128Gcm::new_from_slice(&cek)
            .unwrap()
            .decrypt(&Nonce::from(nonce), record)
            .expect("push body must decrypt with the subscriber's keys");
        while plain.last() == Some(&0) {
            plain.pop();
        }
        assert_eq!(plain.pop(), Some(2), "last-record delimiter");
        plain
    }

    /// The payload of a push sent to this subscription.
    pub fn open(&self, push: &Captured) -> Value {
        serde_json::from_slice(&self.decrypt(&push.body)).unwrap()
    }
}

// ---------------------------------------------------------------------------
// The crate under test, served on a real port

pub struct App {
    pub alerts: Alerts,
    pub pool: PgPool,
    pub base: String,
    pub http: reqwest::Client,
    pub nws: MockNws,
    pub push: MockPush,
    pub vapid: Option<Vapid>,
    pub hub: Hub,
    pub gate: wx_core::UpstreamGate,
    events: broadcast::Receiver<Arc<WsEvent>>,
}

pub fn test_vapid() -> Vapid {
    let (public_key, private_key) = wx_alerts::generate_vapid_keys();
    Vapid {
        public_key,
        private_key,
        subject: "mailto:alerts-test@example.com".to_string(),
    }
}

pub fn test_config(nws: &MockNws) -> AlertsConfig {
    AlertsConfig {
        user_agent: "weather-map-test".to_string(),
        api_base: nws.base.clone(),
        poll_interval: Duration::from_millis(200),
        vapid: Some(test_vapid()),
        allow_insecure_push_endpoints: true,
        zone_fetch_concurrency: 2,
        zone_fetch_interval: Duration::ZERO,
        retry_base_delay: Duration::from_millis(10),
        nws_gate: wx_core::UpstreamGate::new(),
        // The NWS tests never reach the rest of the world.
        world: WorldConfig {
            enabled: false,
            ..WorldConfig::default()
        },
    }
}

/// The feeds and their helpers, running until stopped.
pub struct Running {
    pub health: HealthRegistry,
    cancel: CancellationToken,
    handles: Vec<JoinHandle<()>>,
}

impl Running {
    pub fn start(alerts: &Alerts, hub: &Hub) -> Self {
        let health = HealthRegistry::new(hub.clone());
        let cancel = CancellationToken::new();
        let handles = alerts.spawn(&health, cancel.clone());
        Self {
            health,
            cancel,
            handles,
        }
    }

    pub async fn stop(self) -> HealthRegistry {
        self.cancel.cancel();
        for h in self.handles {
            h.await.unwrap();
        }
        self.health
    }
}

impl App {
    pub async fn start(pool: PgPool) -> Self {
        Self::start_with(pool, |_| {}).await
    }

    pub async fn start_with(pool: PgPool, adjust: impl FnOnce(&mut AlertsConfig)) -> Self {
        let nws = MockNws::start().await;
        let push = MockPush::start().await;
        let mut config = test_config(&nws);
        adjust(&mut config);
        let vapid = config.vapid.clone();
        let gate = config.nws_gate.clone();
        let hub = Hub::new();
        let events = hub.subscribe();
        let alerts = Alerts::new(pool.clone(), hub.clone(), config).unwrap();
        let base = serve(alerts.router()).await;
        let http = reqwest::Client::new();
        Self {
            alerts,
            pool,
            base,
            http,
            nws,
            push,
            vapid,
            hub,
            gate,
            events,
        }
    }

    /// A second instance on the same database, mocks, keys and gate (and
    /// with `world`, the world path pointed at it): what a restarted server
    /// would be. Its first cycle runs the sweep.
    pub fn restarted_with(&self, world: Option<&world::MockWorld>) -> Alerts {
        let mut config = test_config(&self.nws);
        config.vapid = self.vapid.clone();
        config.nws_gate = self.gate.clone();
        if let Some(mock) = world {
            config.world = world::world_config(mock);
        }
        Alerts::new(self.pool.clone(), Hub::new(), config).unwrap()
    }

    pub fn restarted(&self) -> Alerts {
        self.restarted_with(None)
    }

    pub fn spawn(&self) -> Running {
        Running::start(&self.alerts, &self.hub)
    }

    pub async fn poll(&self) -> PollReport {
        self.alerts.poll_once().await.unwrap()
    }

    pub async fn world_poll(&self) -> WorldReport {
        self.alerts.world_poll_once().await.unwrap()
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// `method path` from the device `token` (none if empty), with `body`
    /// as JSON unless it is null: the status and the answer as JSON (null
    /// when there is none).
    pub async fn call(&self, method: &str, path: &str, token: &str, body: Value) -> (u16, Value) {
        let mut req = self.http.request(method.parse().unwrap(), self.url(path));
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
        if !body.is_null() {
            req = req.json(&body);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.http.get(self.url(path)).send().await.unwrap()
    }

    /// The one row `sql` selects with `$1` bound to `key`.
    pub async fn one_for<T>(&self, sql: &'static str, key: &str) -> T
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        sqlx::query_as(sql)
            .bind(key)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// GET `path`, which must answer with success, as JSON.
    pub async fn get_json(&self, path: &str) -> Value {
        let resp = self.get(path).await;
        assert!(resp.status().is_success(), "{path}: {}", resp.status());
        resp.json().await.unwrap()
    }

    /// The alerts `/api/alerts/at` finds at a point.
    pub async fn at(&self, lat: f64, lon: f64) -> Vec<Value> {
        let v = self
            .get_json(&format!("/api/alerts/at?lat={lat}&lon={lon}"))
            .await;
        v["alerts"].as_array().unwrap().clone()
    }

    /// The ids of the alerts at a point.
    pub async fn covering(&self, lat: f64, lon: f64) -> Vec<String> {
        self.at(lat, lon)
            .await
            .iter()
            .map(|a| a["id"].as_str().unwrap().to_string())
            .collect()
    }

    pub async fn features(&self) -> Vec<Value> {
        self.get_json("/api/alerts").await["features"]
            .as_array()
            .unwrap()
            .clone()
    }

    pub async fn feature_ids(&self) -> Vec<String> {
        self.features()
            .await
            .iter()
            .map(|f| f["id"].as_str().unwrap().to_string())
            .collect()
    }

    /// `(added, updated, removed)` for every alerts event since last asked.
    pub fn alert_events(&mut self) -> Vec<(Vec<String>, Vec<String>, Vec<String>)> {
        let mut out = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            if let WsEvent::Alerts {
                added,
                updated,
                removed,
                ..
            } = &*ev
            {
                out.push((added.clone(), updated.clone(), removed.clone()));
            }
        }
        out
    }

    /// Revisions announced by `alerts` events since last asked.
    pub fn alert_revisions(&mut self) -> Vec<i64> {
        let mut out = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            if let WsEvent::Alerts { revision, .. } = &*ev {
                out.push(*revision);
            }
        }
        out
    }

    pub async fn register(&self) -> String {
        let resp = self
            .http
            .post(self.url("/api/device"))
            .send()
            .await
            .unwrap();
        resp.json::<Value>().await.unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// A device whose browser subscribed with the push endpoint `name`.
    pub async fn device(&self, name: &str) -> (String, Subscriber) {
        let token = self.register().await;
        let sub = Subscriber::new(self.push.endpoint(name));
        self.subscribe(&token, &sub).await;
        (token, sub)
    }

    pub async fn save_place(&self, token: &str, name: &str, lat: f64, lon: f64) -> Value {
        let place = json!({ "name": name, "lat": lat, "lon": lon });
        let resp = self
            .http
            .post(self.url("/api/places"))
            .bearer_auth(token)
            .json(&place)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        resp.json().await.unwrap()
    }

    pub async fn subscribe(&self, token: &str, sub: &Subscriber) {
        let url = self.url("/api/push/subscription");
        let resp = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&sub.json())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 204, "{}", resp.text().await.unwrap());
    }

    /// `(event_key, alert_id, status, error)` of every notification, oldest first.
    pub async fn notification_rows(&self) -> Vec<(String, String, String, Option<String>)> {
        self.rows(
            "SELECT event_key, alert_id, status, error FROM notifications ORDER BY created_at",
        )
        .await
    }

    /// The one value `sql` selects.
    pub async fn one<T>(&self, sql: &'static str) -> T
    where
        T: for<'r> sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres> + Send + Unpin,
    {
        sqlx::query_scalar(sql).fetch_one(&self.pool).await.unwrap()
    }

    /// The rows `sql` selects.
    pub async fn rows<T>(&self, sql: &'static str) -> Vec<T>
    where
        T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    {
        sqlx::query_as(sql).fetch_all(&self.pool).await.unwrap()
    }

    /// Change the database behind the crate's back.
    pub async fn exec(&self, sql: &'static str) {
        sqlx::query(sql).execute(&self.pool).await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// NWS-shaped alerts

/// Central time, as the Norman office writes it.
fn cst() -> FixedOffset {
    FixedOffset::west_opt(6 * 3600).unwrap()
}

/// Whole seconds and a local offset, as the NWS writes times.
pub fn nws_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&cst())
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

fn vtec_time(t: DateTime<Utc>) -> String {
    t.format("%y%m%dT%H%MZ").to_string()
}

/// A square ring around a point, `half` degrees on each side.
pub fn square(lon: f64, lat: f64, half: f64) -> Vec<[f64; 2]> {
    vec![
        [lon - half, lat - half],
        [lon + half, lat - half],
        [lon + half, lat + half],
        [lon - half, lat + half],
        [lon - half, lat - half],
    ]
}

pub struct AlertBuilder {
    id: String,
    event: String,
    severity: &'static str,
    message_type: &'static str,
    sent: DateTime<Utc>,
    ends: Option<DateTime<Utc>>,
    expires: DateTime<Utc>,
    geometry: Value,
    zones: Vec<(String, String)>,
    vtec: Option<(String, String, String, String, String)>,
    /// Written out as given, instead of `vtec`.
    vtec_strings: Option<Vec<String>>,
    references: Vec<(String, DateTime<Utc>)>,
    status: &'static str,
}

pub fn alert(id: &str, event: &str) -> AlertBuilder {
    let now = Utc::now();
    AlertBuilder {
        id: id.to_string(),
        event: event.to_string(),
        severity: "Extreme",
        message_type: "Alert",
        sent: now - ChronoDuration::minutes(1),
        ends: Some(now + ChronoDuration::minutes(45)),
        expires: now + ChronoDuration::minutes(45),
        geometry: Value::Null,
        zones: Vec::new(),
        vtec: None,
        vtec_strings: None,
        references: Vec::new(),
        status: "Actual",
    }
}

impl AlertBuilder {
    pub fn polygon(mut self, ring: Vec<[f64; 2]>) -> Self {
        self.geometry = json!({ "type": "Polygon", "coordinates": [ring] });
        self
    }

    pub fn zones(mut self, zones: &[(&str, &str)]) -> Self {
        self.zones = zones
            .iter()
            .map(|(k, i)| (k.to_string(), i.to_string()))
            .collect();
        self
    }

    /// `/O.{action}.{office}.{phen}.{sig}.{etn}.begin-end/`.
    pub fn vtec(mut self, action: &str, office: &str, phen: &str, sig: &str, etn: &str) -> Self {
        self.vtec = Some((
            action.into(),
            office.into(),
            phen.into(),
            sig.into(),
            etn.into(),
        ));
        self
    }

    pub fn message_type(mut self, t: &'static str) -> Self {
        self.message_type = t;
        self
    }

    pub fn severity(mut self, s: &'static str) -> Self {
        self.severity = s;
        self
    }

    pub fn status(mut self, s: &'static str) -> Self {
        self.status = s;
        self
    }

    pub fn sent(mut self, t: DateTime<Utc>) -> Self {
        self.sent = t;
        self
    }

    pub fn ends(mut self, t: DateTime<Utc>) -> Self {
        self.ends = Some(t);
        self.expires = t;
        self
    }

    /// No stated end, as river flood warnings: `expires` is only when the
    /// next statement is due, and the headline has no "until".
    pub fn open_ended(mut self, expires: DateTime<Utc>) -> Self {
        self.ends = None;
        self.expires = expires;
        self
    }

    /// Several VTEC strings, written as given.
    pub fn vtec_strings(mut self, strings: &[&str]) -> Self {
        self.vtec_strings = Some(strings.iter().map(|s| s.to_string()).collect());
        self
    }

    /// No VTEC at all (Civil Danger, Shelter In Place, ... warnings).
    pub fn no_vtec(mut self) -> Self {
        self.vtec = None;
        self.vtec_strings = None;
        self
    }

    pub fn references(mut self, id: &str, sent: DateTime<Utc>) -> Self {
        self.references.push((id.to_string(), sent));
        self
    }

    pub fn build(self) -> Value {
        let local = |t: DateTime<Utc>| t.with_timezone(&cst());
        let when = |t: DateTime<Utc>| local(t).format("%B %-d at %-I:%M%p CST").to_string();
        let headline = match self.ends {
            Some(e) => format!(
                "{} issued {} until {} by NWS Norman OK",
                self.event,
                when(self.sent),
                when(e)
            ),
            None => format!("{} issued {} by NWS Norman OK", self.event, when(self.sent)),
        };
        let mut parameters =
            json!({ "AWIPSidentifier": ["TOROUN"], "NWSheadline": [headline.to_uppercase()] });
        if let Some((action, office, phen, sig, etn)) = &self.vtec {
            let begin = if action == "NEW" {
                vtec_time(self.sent)
            } else {
                "000000T0000Z".to_string()
            };
            let end = self.ends.map_or("000000T0000Z".to_string(), vtec_time);
            parameters["VTEC"] = json!([format!(
                "/O.{action}.{office}.{phen}.{sig}.{etn}.{begin}-{end}/"
            )]);
        }
        if let Some(strings) = &self.vtec_strings {
            parameters["VTEC"] = json!(strings);
        }
        let ugc: Vec<&str> = self.zones.iter().map(|(_, id)| id.as_str()).collect();
        let affected: Vec<String> = self
            .zones
            .iter()
            .map(|(kind, id)| format!("https://api.weather.gov/zones/{kind}/{id}"))
            .collect();
        let references: Vec<Value> = self
            .references
            .iter()
            .map(|(id, sent)| {
                json!({ "@id": format!("https://api.weather.gov/alerts/{id}"),
                                      "identifier": id, "sender": "w-nws.webmaster@noaa.gov",
                                      "sent": nws_time(*sent) })
            })
            .collect();
        json!({
            "id": format!("https://api.weather.gov/alerts/{}", self.id),
            "type": "Feature",
            "geometry": self.geometry,
            "properties": {
                "@id": format!("https://api.weather.gov/alerts/{}", self.id),
                "@type": "wx:Alert",
                "id": self.id,
                "areaDesc": "Cleveland, OK; McClain, OK",
                "geocode": { "SAME": [], "UGC": ugc },
                "affectedZones": affected,
                "references": references,
                "sent": nws_time(self.sent),
                "effective": nws_time(self.sent),
                "onset": nws_time(self.sent),
                "expires": nws_time(self.expires),
                "ends": self.ends.map(nws_time),
                "status": self.status,
                "messageType": self.message_type,
                "category": "Met",
                "severity": self.severity,
                "certainty": "Observed",
                "urgency": "Immediate",
                "event": self.event,
                "sender": "w-nws.webmaster@noaa.gov",
                "senderName": "NWS Norman OK",
                "headline": headline,
                "description": "At 545 PM CST, a confirmed tornado was located near Norman.",
                "instruction": "TAKE COVER NOW!",
                "response": "Shelter",
                "parameters": parameters,
            }
        })
    }
}

/// Shift every time in a recorded feature so that `recorded_now` becomes
/// now, keeping each time's UTC offset: recorded alerts stay in effect
/// whenever the test runs.
pub fn shift_times(feature: &mut Value, recorded_now: DateTime<Utc>) {
    let delta = Utc::now() - recorded_now;
    let shift = |v: &mut Value| {
        if let Some(t) = v
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        {
            *v = Value::String((t + delta).to_rfc3339());
        }
    };
    let props = &mut feature["properties"];
    for key in ["sent", "effective", "onset", "expires", "ends"] {
        shift(&mut props[key]);
    }
    if let Some(refs) = props["references"].as_array_mut() {
        for r in refs {
            shift(&mut r["sent"]);
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
