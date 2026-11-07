//! Test doubles for the world path: one local server standing in for every
//! feed host, the WMO register and Eurostat GISCO, plus builders for
//! registers, feeds and CAP messages.

use super::{App, serve};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wx_alerts::WorldConfig;

#[derive(Clone)]
struct Doc {
    body: Vec<u8>,
    content_type: String,
    /// Answer conditional requests (ETag and Last-Modified).
    validators: bool,
    status: u16,
    delay: Option<Duration>,
}

#[derive(Default)]
struct Host {
    docs: HashMap<String, Doc>,
    hits: HashMap<String, usize>,
    conditional: HashMap<String, usize>,
    not_modified: HashMap<String, usize>,
    user_agents: Vec<String>,
}

type HostState = Arc<Mutex<Host>>;

pub struct MockWorld {
    pub base: String,
    state: HostState,
}

impl MockWorld {
    pub async fn start() -> Self {
        let state = HostState::default();
        let router = Router::new()
            .route("/{*path}", get(serve_doc))
            .with_state(state.clone());
        Self {
            base: serve(router).await,
            state,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn with<T>(&self, f: impl FnOnce(&mut Host) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }

    fn put_doc(&self, path: &str, body: impl Into<Vec<u8>>, content_type: &str, validators: bool) {
        let doc = Doc {
            body: body.into(),
            content_type: content_type.to_string(),
            validators,
            status: 200,
            delay: None,
        };
        self.with(|h| h.docs.insert(path.to_string(), doc));
    }

    /// A feed that answers conditional requests, like S3.
    pub fn put_feed(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.put_doc(path, body, "application/rss+xml", true);
    }

    /// A feed that sends no validators, like MeteoAlarm.
    pub fn put_feed_plain(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.put_doc(path, body, "application/atom+xml", false);
    }

    pub fn put_cap(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.put_doc(path, body, "application/cap+xml", false);
    }

    pub fn put_json(&self, path: &str, body: &Value) {
        self.put_doc(path, body.to_string(), "application/json", true);
    }

    pub fn fail(&self, path: &str, status: u16) {
        if !self.with(|h| h.docs.contains_key(path)) {
            self.put_doc(path, Vec::new(), "text/plain", false);
        }
        self.with(|h| h.docs.get_mut(path).unwrap().status = status);
    }

    pub fn slow(&self, path: &str, delay: Duration) {
        self.with(|h| h.docs.get_mut(path).map(|d| d.delay = Some(delay)));
    }

    pub fn hits(&self, path: &str) -> usize {
        self.with(|h| h.hits.get(path).copied().unwrap_or(0))
    }

    pub fn conditional_hits(&self, path: &str) -> usize {
        self.with(|h| h.conditional.get(path).copied().unwrap_or(0))
    }

    pub fn not_modified(&self, path: &str) -> usize {
        self.with(|h| h.not_modified.get(path).copied().unwrap_or(0))
    }

    pub fn total_hits(&self) -> usize {
        self.with(|h| h.hits.values().sum())
    }

    pub fn user_agents(&self) -> Vec<String> {
        self.with(|h| h.user_agents.clone())
    }
}

async fn serve_doc(
    State(state): State<HostState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    let path = format!("/{path}");
    let doc = {
        let mut h = state.lock().unwrap();
        *h.hits.entry(path.clone()).or_default() += 1;
        h.user_agents
            .push(super::header_str(&headers, header::USER_AGENT).unwrap_or_default());
        h.docs.get(&path).cloned()
    };
    let Some(doc) = doc else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(d) = doc.delay {
        tokio::time::sleep(d).await;
    }
    if doc.status != 200 {
        return StatusCode::from_u16(doc.status).unwrap().into_response();
    }
    if !doc.validators {
        return ([(header::CONTENT_TYPE, doc.content_type)], doc.body).into_response();
    }
    let etag = format!("\"{}\"", super::hex(&Sha256::digest(&doc.body)[..8]));
    let last_modified = "Fri, 06 Nov 2026 12:00:00 GMT".to_string();
    let inm = super::header_str(&headers, header::IF_NONE_MATCH);
    let mut h = state.lock().unwrap();
    if inm.is_some() || headers.contains_key(header::IF_MODIFIED_SINCE) {
        *h.conditional.entry(path.clone()).or_default() += 1;
    }
    if inm.as_deref() == Some(etag.as_str()) {
        *h.not_modified.entry(path).or_default() += 1;
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    let headers = [
        (header::CONTENT_TYPE, doc.content_type),
        (header::ETAG, etag),
        (header::LAST_MODIFIED, last_modified),
    ];
    (headers, doc.body).into_response()
}

/// The world path pointed at `mock`, fast enough for tests.
pub fn world_config(mock: &MockWorld) -> WorldConfig {
    WorldConfig {
        enabled: true,
        user_agent: "weather-map-test".to_string(),
        register_url: mock.url("/register.json"),
        register_refresh: Duration::from_secs(3600),
        poll_interval: Duration::from_millis(300),
        slow_poll_interval: Duration::from_millis(600),
        per_host_concurrency: 2,
        max_concurrency: 8,
        request_timeout: Duration::from_secs(2),
        max_backoff: Duration::from_secs(4),
        documents_per_poll: 100,
        max_item_age: Duration::from_secs(30 * 24 * 3600),
        nuts_base_url: Some(mock.url("/nuts")),
        nuts_fetch_interval: Duration::ZERO,
        allow_private_hosts: true,
    }
}

/// A source in the register's own format.
pub struct RegSource<'a> {
    pub id: &'a str,
    pub country: &'a str,
    pub country_name: &'a str,
    pub name: &'a str,
    pub region: &'a str,
    pub latlng: &'a str,
    pub feed: String,
}

/// A register with `sources` and enough filler sources (on a path that
/// answers 404) to pass the sanity check on the register's size.
pub fn register(mock: &MockWorld, sources: &[RegSource<'_>]) -> Value {
    let mut list: Vec<Value> = sources
        .iter()
        .map(|s| {
            json!({ "source": {
                "sourceId": s.id, "byLanguage": [{ "code": "en", "name": s.name }],
                "sourceIsOfficial": true, "capAlertFeed": s.feed, "capAlertFeedStatus": "operating",
                "region": s.region, "latlng": s.latlng, "countryName": s.country_name,
                "authorityCountry": s.country,
            }})
        })
        .collect();
    for i in list.len()..25 {
        list.push(json!({ "source": {
            "sourceId": format!("zz-filler-{i}"), "byLanguage": [{ "code": "en", "name": "Filler" }],
            "sourceIsOfficial": true, "capAlertFeed": mock.url(&format!("/filler/{i}/rss.xml")),
            "capAlertFeedStatus": "operating", "region": "1", "latlng": "0,0", "countryName": "Nowhere",
            "authorityCountry": "zz",
        }}));
    }
    json!({ "sources": list })
}

/// A source of the register, with a feed on `mock`.
pub fn source<'a>(mock: &MockWorld, id: &'a str, country: &'a str, feed: &str) -> RegSource<'a> {
    RegSource {
        id,
        country,
        country_name: "Testland",
        name: id,
        region: "6",
        latlng: "45.0,15.0",
        feed: mock.url(feed),
    }
}

/// The crate with its world path reading `sources` from `mock`.
pub async fn world_app(pool: PgPool, mock: &MockWorld, sources: &[RegSource<'_>]) -> App {
    mock.put_json("/register.json", &register(mock, sources));
    let config = world_config(mock);
    App::start_with(pool, move |c| c.world = config).await
}

/// CAP documents served at `/{dir}/{name}.xml` and listed, in that order,
/// in the RSS feed `/{dir}/rss.xml`.
pub fn serve_feed(mock: &MockWorld, dir: &str, docs: &[(&str, String)]) {
    for (n, xml) in docs {
        mock.put_cap(&format!("/{dir}/{n}.xml"), xml.clone());
    }
    let links: Vec<String> = docs
        .iter()
        .map(|(n, _)| mock.url(&format!("/{dir}/{n}.xml")))
        .collect();
    mock.put_feed(&format!("/{dir}/rss.xml"), rss(&links));
}

/// An RSS 2.0 feed listing `links`, newest first.
pub fn rss(links: &[String]) -> String {
    let items: String = links
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let date = (Utc::now() - ChronoDuration::minutes(i as i64)).to_rfc2822();
            format!("<item><title>Alert {i}</title><link>{l}</link><guid>{l}</guid><pubDate>{date}</pubDate></item>")
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0"><channel><title>t</title>{items}</channel></rss>"#
    )
}

/// A MeteoAlarm-style Atom feed: one entry per region per message.
pub fn meteoalarm_atom(entries: &[(String, &str)]) -> String {
    let expires = (Utc::now() + ChronoDuration::hours(12)).to_rfc3339();
    let body: String = entries
        .iter()
        .map(|(link, code)| {
            format!(
                r#"<entry><cap:geocode><valueName>NUTS3</valueName><value>{code}</value></cap:geocode>
                   <cap:expires>{expires}</cap:expires><cap:status>Actual</cap:status>
                   <link type="application/cap+xml" href="{link}"/><id>{link}?{code}</id></entry>"#
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><feed xmlns="http://www.w3.org/2005/Atom" xmlns:cap="urn:oasis:names:tc:emergency:cap:1.2">{body}</feed>"#
    )
}

/// A GISCO region file for `code`: a square around (lon, lat).
pub fn nuts_region(code: &str, name: &str, lon: f64, lat: f64, half: f64) -> Value {
    json!({
        "crs": { "type": "name", "properties": { "name": "urn:ogc:def:crs:EPSG::4326" } },
        "type": "FeatureCollection",
        "features": [{ "type": "Feature",
            "properties": { "NUTS_ID": code, "LEVL_CODE": 3, "NAME_LATN": name },
            "geometry": { "type": "MultiPolygon", "coordinates": [[[
                [lon - half, lat - half], [lon + half, lat - half], [lon + half, lat + half],
                [lon - half, lat + half], [lon - half, lat - half]]]] } }]
    })
}

/// Move every RFC 3339 time in `xml` by `delta`, keeping each offset, so a
/// recorded message is current whenever the test runs.
pub fn shift_times(xml: &str, delta: ChronoDuration) -> String {
    let b = xml.as_bytes();
    let mut out = String::with_capacity(xml.len());
    let mut i = 0;
    let mut copied = 0;
    while i + 20 <= b.len() {
        let looks = b[i..i + 4].iter().all(u8::is_ascii_digit)
            && b[i + 4] == b'-'
            && b[i + 10] == b'T'
            && b[i + 13] == b':';
        let mut replaced = false;
        if looks {
            for len in [25usize, 20, 32, 29, 24] {
                if i + len > b.len() {
                    continue;
                }
                let Some(cand) = xml.get(i..i + len) else {
                    continue;
                };
                if let Ok(t) = DateTime::parse_from_rfc3339(cand) {
                    out.push_str(&xml[copied..i]);
                    let shifted = (t + delta)
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, cand.ends_with('Z'));
                    out.push_str(&shifted);
                    i += len;
                    copied = i;
                    replaced = true;
                    break;
                }
            }
        }
        if !replaced {
            i += 1;
        }
    }
    out.push_str(&xml[copied..]);
    out
}

/// The time `delta` that makes a fixture's `<sent>` ten minutes ago.
pub fn delta_to_now(xml: &str) -> ChronoDuration {
    let start = xml.find("sent>").expect("a sent time") + 5;
    let end = start + xml[start..].find('<').unwrap();
    let sent = DateTime::parse_from_rfc3339(xml[start..end].trim())
        .unwrap()
        .with_timezone(&Utc);
    Utc::now() - ChronoDuration::minutes(10) - sent
}

pub fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/world/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// A fixture with its times moved so it was sent ten minutes ago.
pub fn current_fixture(name: &str) -> String {
    let xml = fixture(name);
    let delta = delta_to_now(&xml);
    shift_times(&xml, delta)
}

/// A synthetic CAP 1.2 message.
pub struct Cap<'a> {
    pub sender: &'a str,
    pub identifier: &'a str,
    pub msg_type: &'a str,
    pub references: Vec<(&'a str, DateTime<Utc>)>,
    /// The sender written in references, when not the message's own.
    pub reference_sender: Option<&'a str>,
    pub sent: DateTime<Utc>,
    pub event: &'a str,
    pub headline: &'a str,
    /// CAP `<category>`; empty leaves the element out.
    pub category: &'a str,
    pub severity: &'a str,
    pub urgency: &'a str,
    pub expires: Option<DateTime<Utc>>,
    /// Rings as (lat, lon).
    pub polygons: Vec<Vec<(f64, f64)>>,
    pub circles: Vec<(f64, f64, f64)>,
    pub geocodes: Vec<(&'a str, &'a str)>,
    /// Extra info blocks as (language, event, headline).
    pub translations: Vec<(&'a str, &'a str, &'a str)>,
}

impl<'a> Cap<'a> {
    pub fn alert(identifier: &'a str) -> Self {
        Self {
            sender: "met@example.org",
            identifier,
            msg_type: "Alert",
            references: Vec::new(),
            reference_sender: None,
            sent: Utc::now() - ChronoDuration::minutes(5),
            event: "Storm",
            headline: "Storm warning for the coast",
            category: "Met",
            severity: "Severe",
            urgency: "Immediate",
            expires: Some(Utc::now() + ChronoDuration::hours(6)),
            polygons: Vec::new(),
            circles: Vec::new(),
            geocodes: Vec::new(),
            translations: Vec::new(),
        }
    }

    pub fn xml(&self) -> String {
        let refs = if self.references.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = self
                .references
                .iter()
                .map(|(id, sent)| {
                    format!(
                        "{},{id},{}",
                        self.reference_sender.unwrap_or(self.sender),
                        sent.to_rfc3339()
                    )
                })
                .collect();
            format!("<references>{}</references>", list.join(" "))
        };
        let area = {
            let polys: String = self
                .polygons
                .iter()
                .map(|r| {
                    let pts: Vec<String> = r.iter().map(|(la, lo)| format!("{la},{lo}")).collect();
                    format!("<polygon>{}</polygon>", pts.join(" "))
                })
                .collect();
            let circles: String = self
                .circles
                .iter()
                .map(|(la, lo, r)| format!("<circle>{la},{lo} {r}</circle>"))
                .collect();
            let codes: String = self
                .geocodes
                .iter()
                .map(|(n, v)| {
                    format!("<geocode><valueName>{n}</valueName><value>{v}</value></geocode>")
                })
                .collect();
            format!("<area><areaDesc>Test area</areaDesc>{polys}{circles}{codes}</area>")
        };
        let expires = self
            .expires
            .map(|e| format!("<expires>{}</expires>", e.to_rfc3339()))
            .unwrap_or_default();
        let category = if self.category.is_empty() {
            String::new()
        } else {
            format!("<category>{}</category>", self.category)
        };
        let info = |lang: &str, event: &str, headline: &str| {
            format!(
                "<info><language>{lang}</language>{category}<event>{event}</event>
                 <urgency>{}</urgency><severity>{}</severity><certainty>Likely</certainty>
                 <onset>{}</onset>{expires}<senderName>Test Met Service</senderName>
                 <headline>{headline}</headline><description>Details for {event}.</description>
                 <instruction>Take care.</instruction><web>https://example.org/warnings</web>{area}</info>",
                self.urgency,
                self.severity,
                self.sent.to_rfc3339()
            )
        };
        let mut infos = info("en-GB", self.event, self.headline);
        for (lang, event, headline) in &self.translations {
            infos.push_str(&info(lang, event, headline));
        }
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <alert xmlns="urn:oasis:names:tc:emergency:cap:1.2">
              <identifier>{}</identifier><sender>{}</sender><sent>{}</sent>
              <status>Actual</status><msgType>{}</msgType><scope>Public</scope>{refs}{infos}
            </alert>"#,
            self.identifier,
            self.sender,
            self.sent.to_rfc3339(),
            self.msg_type
        )
    }
}

/// A square ring of (lat, lon) around a point.
pub fn box_ring(lat: f64, lon: f64, half: f64) -> Vec<(f64, f64)> {
    vec![
        (lat - half, lon - half),
        (lat - half, lon + half),
        (lat + half, lon + half),
        (lat + half, lon - half),
        (lat - half, lon - half),
    ]
}

// ---------------------------------------------------------------------------
// A minimal Mapbox Vector Tile reader (protobuf), enough to check what a
// tile holds.

#[derive(Debug, Default)]
pub struct MvtLayer {
    pub name: String,
    pub extent: u64,
    pub keys: Vec<String>,
    pub values: Vec<MvtValue>,
    pub features: Vec<MvtFeature>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MvtValue {
    String(String),
    Int(i64),
    Double(f64),
    Bool(bool),
}

#[derive(Debug, Default)]
pub struct MvtFeature {
    pub tags: Vec<u64>,
    pub geom_type: u64,
    pub geometry: Vec<u64>,
}

impl MvtLayer {
    /// A feature's properties by name.
    pub fn props(&self, f: &MvtFeature) -> HashMap<String, MvtValue> {
        f.tags
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[k, v]| {
                (
                    self.keys[k as usize].clone(),
                    self.values[v as usize].clone(),
                )
            })
            .collect()
    }
}

fn varint(b: &[u8], i: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let byte = b[*i];
        *i += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return v;
        }
        shift += 7;
    }
}

/// `(field, wire type, payload)` triples of one message.
fn fields(b: &[u8]) -> Vec<(u64, u64, Vec<u8>, u64)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let key = varint(b, &mut i);
        let (field, wire) = (key >> 3, key & 7);
        match wire {
            0 => {
                let v = varint(b, &mut i);
                out.push((field, wire, Vec::new(), v));
            }
            1 => {
                let v = u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
                i += 8;
                out.push((field, wire, Vec::new(), v));
            }
            2 => {
                let len = varint(b, &mut i) as usize;
                out.push((field, wire, b[i..i + len].to_vec(), 0));
                i += len;
            }
            5 => {
                let v = u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
                i += 4;
                out.push((field, wire, Vec::new(), u64::from(v)));
            }
            w => panic!("unexpected wire type {w}"),
        }
    }
    out
}

fn packed(b: &[u8]) -> Vec<u64> {
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        out.push(varint(b, &mut i));
    }
    out
}

pub fn decode_mvt(tile: &[u8]) -> Vec<MvtLayer> {
    fields(tile)
        .into_iter()
        .filter(|f| f.0 == 3)
        .map(|(_, _, layer, _)| {
            let mut l = MvtLayer::default();
            for (field, _, bytes, v) in fields(&layer) {
                match field {
                    1 => l.name = String::from_utf8(bytes).unwrap(),
                    2 => {
                        let mut f = MvtFeature::default();
                        for (ff, _, fb, fv) in fields(&bytes) {
                            match ff {
                                2 => f.tags = packed(&fb),
                                3 => f.geom_type = fv,
                                4 => f.geometry = packed(&fb),
                                _ => {}
                            }
                        }
                        l.features.push(f);
                    }
                    3 => l.keys.push(String::from_utf8(bytes).unwrap()),
                    4 => {
                        let (vf, vw, vb, vv) = fields(&bytes).into_iter().next().unwrap();
                        l.values.push(match (vf, vw) {
                            (1, _) => MvtValue::String(String::from_utf8(vb).unwrap()),
                            (3, _) => MvtValue::Double(f64::from_bits(vv)),
                            (2, _) => MvtValue::Double(f64::from(f32::from_bits(vv as u32))),
                            (4, _) | (5, _) => MvtValue::Int(vv as i64),
                            (6, _) => MvtValue::Int(((vv >> 1) as i64) ^ -((vv & 1) as i64)),
                            (7, _) => MvtValue::Bool(vv != 0),
                            other => panic!("unexpected value field {other:?}"),
                        });
                    }
                    5 => l.extent = v,
                    _ => {}
                }
            }
            l
        })
        .collect()
}
