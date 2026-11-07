//! The server's own routes: configuration, sources, layers, timelines, tiles, status.
//! Alerts, places, push, stations and point forecasts bring their own routers.

use crate::body::Fixed;
use crate::config::Config;
use crate::frames::Timelines;
use crate::tiles::{TileOutcome, TileService};
use crate::ws::{self, Live};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use tokio_util::sync::CancellationToken;
use wx_core::proj::TILE_CELLS;
use wx_core::{FrameCatalog, HealthRegistry, LAYERS, Part, SOURCES, TileCoord};

/// Path the bundled basemap archive is served under.
pub const BASEMAP_ROUTE: &str = "/basemap/world.pmtiles";

/// Longest `{parts}` path segment a tile request may carry: the most parts a
/// stack may name, at their longest, with separators and the extension.
const MAX_STACK_LEN: usize =
    wx_core::frame::MAX_COMBO_PARTS * (Part::MAX_ID_LEN + 1) + ".wxt".len();

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub health: HealthRegistry,
    pub catalog: Arc<FrameCatalog>,
    /// Timelines as served.
    pub timelines: Arc<Timelines>,
    pub live: Live,
    pub tiles: Arc<TileService>,
    pub cancel: CancellationToken,
    pub started_at: DateTime<Utc>,
    /// Open WebSocket connections.
    pub clients: Arc<AtomicUsize>,
    /// What `/api/config` reports as the basemap, decided once at startup.
    pub basemap_url: Option<String>,
    /// Requests and bytes per upstream host since startup, for the status page.
    pub traffic: Arc<dyn Fn() -> Vec<wx_ingest::Traffic> + Send + Sync>,
}

/// JSON endpoints. The caller wraps these in response compression.
pub fn api(state: AppState) -> Router {
    Router::new()
        .route("/api/config", get(config))
        .route("/api/sources", get(sources))
        .route("/api/layers", get(layers))
        .route("/api/frames", get(all_frames))
        .route("/api/frames/{layer}", get(layer_frames))
        .route("/api/provenance", get(provenance))
        .route("/api/status", get(status))
        .with_state(state)
}

/// Endpoints that must not be wrapped in compression: tiles are already
/// compressed, and a WebSocket is not a response body.
pub fn raw(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws::handler))
        .route("/tiles/{layer}/{z}/{x}/{y}/{parts}", get(tile))
        .with_state(state)
}

pub fn error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({ "error": code, "message": message.into() })),
    )
        .into_response()
}

async fn config(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "tile_url": "/tiles/{layer}/{z}/{x}/{y}/{parts}.wxt",
        "tile_cells": TILE_CELLS,
        "basemap_url": state.basemap_url,
        "vapid_public_key": state.cfg.vapid.as_ref().map(|v| v.public_key.as_str()),
        "server_time": Utc::now(),
    }))
}

// The registries never change while the server runs.
static SOURCES_BODY: LazyLock<Fixed> = LazyLock::new(|| {
    let sources: Vec<serde_json::Value> = SOURCES
        .iter()
        .map(|def| {
            let mut v = serde_json::to_value(def).expect("a source row serializes");
            v["medium"] = json!(def.medium());
            v
        })
        .collect();
    Fixed::new(serde_json::to_vec(&json!({ "sources": sources })).expect("sources serialize"))
});

static LAYERS_BODY: LazyLock<Fixed> = LazyLock::new(|| {
    Fixed::new(serde_json::to_vec(&json!({ "layers": LAYERS })).expect("layers serialize"))
});

async fn sources(headers: HeaderMap) -> Response {
    SOURCES_BODY.respond(&headers, "public, max-age=300")
}

async fn layers(headers: HeaderMap) -> Response {
    LAYERS_BODY.respond(&headers, "public, max-age=300")
}

async fn all_frames(State(state): State<AppState>, headers: HeaderMap) -> Response {
    state.timelines.all(&headers).await
}

async fn layer_frames(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match state.timelines.layer(&id, &headers).await {
        Some(r) => r,
        None => error(
            StatusCode::NOT_FOUND,
            "unknown_layer",
            format!("no layer named {id:?}"),
        ),
    }
}

#[derive(Deserialize)]
struct ProvenanceQuery {
    layer: String,
    valid: DateTime<Utc>,
    lon: f64,
    lat: f64,
}

async fn provenance(
    State(state): State<AppState>,
    query: Result<Query<ProvenanceQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(q)) = query else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "expected layer, valid (RFC 3339), lon and lat",
        );
    };
    if !(q.lon.is_finite() && q.lat.is_finite() && q.lat.abs() <= 90.0 && q.lon.abs() <= 540.0) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_coordinates",
            "lon or lat is out of range",
        );
    }
    let lon = wx_core::proj::wrap_lon(q.lon);
    match state.tiles.provenance(&q.layer, q.valid, lon, q.lat).await {
        Ok(Some(shares)) => {
            let parts: Vec<serde_json::Value> = shares
                .iter()
                .map(|(part, share)| {
                    json!({
                        "id": part.id,
                        "source": part.source,
                        "share": (share * 1000.0).round() / 1000.0,
                    })
                })
                .collect();
            Json(json!({ "layer": q.layer, "valid": q.valid, "parts": parts })).into_response()
        }
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "unknown_frame",
            "no such layer or time step",
        ),
        Err(e) => {
            tracing::error!(layer = %q.layer, "provenance failed: {e:#}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "provenance_failed",
                "could not look that up",
            )
        }
    }
}

#[derive(Serialize)]
struct LayerStatus {
    id: &'static str,
    frames: usize,
    oldest_valid: Option<DateTime<Utc>>,
    newest_valid: Option<DateTime<Utc>>,
    /// Newest observed time step, when the layer has observations.
    newest_observed: Option<DateTime<Utc>>,
}

/// What one source currently contributes, across every layer.
#[derive(Serialize, Default)]
struct SourceStatus {
    id: &'static str,
    title: &'static str,
    provider: &'static str,
    /// Parts held, on a timeline or waiting.
    parts: usize,
    /// Layers it has at least one part on.
    layers: usize,
    newest_valid: Option<DateTime<Utc>>,
    /// Newest model run it has parts from. `None` for observations.
    newest_run: Option<DateTime<Utc>>,
}

async fn status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let now = Utc::now();
    let feeds: Vec<serde_json::Value> = state
        .health
        .snapshot()
        .into_iter()
        .map(|feed| {
            let stale = feed.is_stale(now);
            let mut v = serde_json::to_value(&feed).expect("feed health serializes");
            v["stale"] = json!(stale);
            v
        })
        .collect();
    // Layers in menu order, including ones with no frames yet.
    let layers: Vec<LayerStatus> = LAYERS
        .iter()
        .zip(state.timelines.summaries())
        .map(|(l, s)| LayerStatus {
            id: l.id,
            frames: s.frames,
            oldest_valid: s.oldest_valid,
            newest_valid: s.newest_valid,
            newest_observed: s.newest_observed,
        })
        .collect();

    let mut by_source: BTreeMap<&str, (SourceStatus, BTreeSet<&str>)> = BTreeMap::new();
    let parts = state.catalog.parts();
    for (layer_id, part) in &parts {
        let entry = by_source.entry(part.source.as_str()).or_default();
        entry.0.parts += 1;
        entry.0.newest_valid = entry.0.newest_valid.max(Some(part.valid));
        entry.0.newest_run = entry.0.newest_run.max(part.run);
        entry.1.insert(layer_id);
    }
    // Registry order (which is roughly top of the stack first), every source
    // listed so a silent one stands out.
    let sources: Vec<SourceStatus> = SOURCES
        .iter()
        .map(|def| {
            let (mut status, layers) = by_source.remove(def.id).unwrap_or_default();
            status.id = def.id;
            status.title = def.title;
            status.provider = def.provider;
            status.layers = layers.len();
            status
        })
        .collect();

    // Biggest downloads first: this is where an operator looks to see what
    // the server costs to run.
    let mut traffic = (state.traffic)();
    traffic.sort_by_key(|t| std::cmp::Reverse(t.bytes));
    let traffic: Vec<serde_json::Value> = traffic
        .iter()
        .map(|t| {
            json!({
                "host": t.host,
                "requests": t.list_requests + t.get_requests,
                "not_found": t.not_found,
                "retries": t.retries,
                "bytes": t.bytes,
            })
        })
        .collect();

    Json(json!({
        "server_time": now,
        "started_at": state.started_at,
        "clients": state.clients.load(Ordering::Relaxed),
        "feeds": feeds,
        "layers": layers,
        "sources": sources,
        "traffic": traffic,
    }))
}

async fn tile(
    State(state): State<AppState>,
    Path((layer_id, z, x, y, parts)): Path<(String, u8, u32, u32, String)>,
) -> Response {
    let not_found = || {
        (
            [(header::CACHE_CONTROL, "no-store")],
            error(
                StatusCode::NOT_FOUND,
                "unknown_tile",
                "no such layer, stack or zoom",
            ),
        )
            .into_response()
    };
    if parts.len() > MAX_STACK_LEN {
        return not_found();
    }
    let (Some(coord), Some(stack)) = (TileCoord::new(z, x, y), parts.strip_suffix(".wxt")) else {
        return not_found();
    };
    let combo: Vec<&str> = stack.split(',').collect();
    match state.tiles.get(&layer_id, coord, &combo).await {
        // The URL names the tile's inputs, so its bytes never change and
        // browsers and CDNs may keep them for good.
        Ok(TileOutcome::Tile(bytes)) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/octet-stream"),
                ),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=604800, immutable"),
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(TileOutcome::Empty) => (
            StatusCode::NO_CONTENT,
            [(header::CACHE_CONTROL, "public, max-age=604800, immutable")],
        )
            .into_response(),
        Ok(TileOutcome::Busy) => (
            [
                (header::RETRY_AFTER, "2"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                "too many tiles are being rendered; retry shortly",
            ),
        )
            .into_response(),
        Ok(TileOutcome::NotFound) => not_found(),
        Err(e) => {
            tracing::error!(layer = %layer_id, stack, ?coord, "tile failed: {e:#}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "tile_failed",
                "could not produce the tile",
            )
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tiles::tests::{hrrr_temp, tile_at};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use wx_core::Hub;

    /// The server's state over `catalog` and `hub`, with an empty tile store.
    pub(crate) fn state(catalog: Arc<FrameCatalog>, hub: &Hub) -> AppState {
        let store = Arc::new(object_store::memory::InMemory::new());
        state_with(store, catalog, hub)
    }

    fn state_with(
        store: Arc<dyn object_store::ObjectStore>,
        catalog: Arc<FrameCatalog>,
        hub: &Hub,
    ) -> AppState {
        AppState {
            cfg: Arc::new(Config::for_tests()),
            health: HealthRegistry::new(hub.clone()),
            live: Live::spawn(hub),
            timelines: Arc::new(Timelines::new(catalog.clone())),
            tiles: Arc::new(TileService::new(store, catalog.clone(), 64)),
            catalog,
            cancel: CancellationToken::new(),
            started_at: Utc::now(),
            clients: Arc::new(AtomicUsize::new(0)),
            basemap_url: None,
            traffic: Arc::new(Vec::new),
        }
    }

    async fn app() -> (Router, Part) {
        let (store, catalog, part) = hrrr_temp().await;
        let state = state_with(store, catalog, &Hub::new());
        (api(state.clone()).merge(raw(state)), part)
    }

    async fn get_raw(app: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, bytes::Bytes) {
        let res = app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let (parts, body) = res.into_parts();
        (
            parts.status,
            parts.headers,
            body.collect().await.unwrap().to_bytes(),
        )
    }

    async fn get_json(app: &Router, uri: &str) -> serde_json::Value {
        let (status, _, body) = get_raw(app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn sources_layers_frames_config_status() {
        let (app, part) = app().await;

        let layers = get_json(&app, "/api/layers").await;
        let layers = layers["layers"].as_array().unwrap();
        assert_eq!(layers.len(), LAYERS.len());
        assert_eq!(layers[0]["id"], "radar");
        assert_eq!(layers[0]["render"]["kind"], "scalar");
        assert_eq!(layers[0]["bands"][0]["quantity"], "reflectivity");

        let sources = get_json(&app, "/api/sources").await;
        let sources = sources["sources"].as_array().unwrap();
        assert_eq!(sources.len(), SOURCES.len());
        let hrrr = sources.iter().find(|s| s["id"] == "hrrr").unwrap();
        assert_eq!(hrrr["kind"], "forecast");
        assert_eq!(hrrr["medium"], "model");
        let mrms = sources.iter().find(|s| s["id"] == "mrms").unwrap();
        assert_eq!(mrms["medium"], "radar");
        assert!(hrrr["attribution"].as_str().is_some_and(|a| !a.is_empty()));

        let all = get_json(&app, "/api/frames").await;
        let temp = &all["timelines"]["temp"];
        assert_eq!(temp["frames"][0]["parts"][0], part.id.as_str());
        assert_eq!(temp["frames"][0]["kind"], "forecast");
        assert_eq!(temp["parts"][0]["source"], "hrrr");
        assert!(temp["parts"][0]["bounds"]["west"].as_f64().unwrap() < -120.0);
        assert_eq!(
            all["timelines"]["radar"]["frames"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        let one = get_json(&app, "/api/frames/temp").await;
        assert_eq!(one["frames"].as_array().unwrap().len(), 1);
        assert_eq!(one["parts"].as_array().unwrap().len(), 1);
        assert_eq!(
            get_raw(&app, "/api/frames/nope").await.0,
            StatusCode::NOT_FOUND
        );

        let config = get_json(&app, "/api/config").await;
        assert_eq!(config["tile_cells"], 256);
        assert!(config["basemap_url"].is_null() && config["vapid_public_key"].is_null());
        assert!(config["tile_url"].as_str().unwrap().contains("{parts}"));

        let status = get_json(&app, "/api/status").await;
        let temp = status["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["id"] == "temp")
            .unwrap();
        assert_eq!(temp["frames"], 1);
        let hrrr = status["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "hrrr")
            .unwrap();
        assert_eq!(
            (hrrr["parts"].as_u64(), hrrr["layers"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(status["clients"], 0);
        assert!(status["traffic"].as_array().unwrap().is_empty());

        assert_eq!(get_raw(&app, "/healthz").await.0, StatusCode::OK);

        let valid = part
            .valid
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let from = get_json(
            &app,
            &format!("/api/provenance?layer=temp&valid={valid}&lon=-87.63&lat=41.88"),
        )
        .await;
        assert_eq!(from["parts"][0]["source"], "hrrr");
        assert_eq!(from["parts"][0]["share"], 1.0);
        for bad in [
            "/api/provenance?layer=temp".to_string(),
            format!("/api/provenance?layer=temp&valid={valid}&lon=0&lat=95"),
        ] {
            assert_eq!(
                get_raw(&app, &bad).await.0,
                StatusCode::BAD_REQUEST,
                "{bad}"
            );
        }
        assert_eq!(
            get_raw(
                &app,
                "/api/provenance?layer=temp&valid=2020-01-01T00:00:00Z&lon=0&lat=0"
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn tiles_outlive_their_serving_window() {
        let serve = chrono::Duration::seconds(wx_core::frame::SERVE_GRACE_SECONDS);
        assert!(
            wx_ingest::DELETE_GRACE > serve,
            "the janitor must not delete a part's tiles while the tile endpoint still serves it"
        );
    }

    #[tokio::test]
    async fn tile_endpoint_statuses() {
        let (app, part) = app().await;
        let t = tile_at(4, -87.63, 41.88);
        let uri = format!("/tiles/temp/{}/{}/{}/{}.wxt", t.z, t.x, t.y, part.id);
        let (status, headers, body) = get_raw(&app, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "application/octet-stream");
        assert!(
            headers[header::CACHE_CONTROL]
                .to_str()
                .unwrap()
                .contains("immutable")
        );
        assert_eq!(&body[0..4], b"WXT1");

        for bad in [
            // Too deep, wrong extension, off the edge of the world, a stack
            // no frame has, an unknown layer, an over-long stack.
            format!("/tiles/temp/9/{}/{}/{}.wxt", t.x, t.y, part.id),
            format!("/tiles/temp/4/{}/{}/{}.png", t.x, t.y, part.id),
            format!("/tiles/temp/4/99/{}/{}.wxt", t.y, part.id),
            format!("/tiles/temp/4/{}/{}/{},{}.wxt", t.x, t.y, part.id, part.id),
            format!("/tiles/temp/4/{}/{}/NOT_A_PART.wxt", t.x, t.y),
            format!("/tiles/nope/4/{}/{}/{}.wxt", t.x, t.y, part.id),
            format!("/tiles/temp/4/{}/{}/{}.wxt", t.x, t.y, "a,".repeat(2000)),
        ] {
            let (status, headers, _) = get_raw(&app, &bad).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{bad}");
            assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{bad}");
        }
    }
}
