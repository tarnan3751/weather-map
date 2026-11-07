//! HTTP routes: alerts, devices, saved places, push subscriptions.

use crate::cache::{AlertProps, DEFAULT_LIMIT, ListQuery, MAX_LIMIT, accepts_gzip, etag_matches};
use crate::devices::{Device, new_token};
use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::notify::{self, Payload};
use crate::push::PushOptions;
use crate::{Alerts, tiles, validate};
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

/// Requests here are tiny; a push subscription is the largest.
const BODY_LIMIT: usize = 16 * 1024;
/// Subscriptions kept per device; older ones are dropped beyond this.
const MAX_SUBSCRIPTIONS_PER_DEVICE: i64 = 5;

pub(crate) fn router(state: Alerts) -> Router {
    Router::new()
        .route("/api/alerts", get(alerts_collection))
        .route("/api/alerts/detail", get(alert_detail))
        .route("/api/alerts/at", get(alerts_at))
        .route("/api/alerts/sources", get(alert_sources))
        .route("/api/alerts/tiles.json", get(tilejson))
        .route("/api/alerts/tiles/{revision}/{z}/{x}/{y}", get(alert_tile))
        .route("/api/device", post(register_device))
        .route("/api/places", get(list_places).post(create_place))
        .route("/api/places/{id}", patch(update_place).delete(delete_place))
        .route(
            "/api/push/subscription",
            post(subscribe).delete(unsubscribe),
        )
        .route("/api/push/test", post(test_push))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

type ApiResult<T> = Result<T, ApiError>;

#[derive(Deserialize)]
struct ListParams {
    /// `west,south,east,north`.
    bbox: Option<String>,
    country: Option<String>,
    limit: Option<usize>,
    unshaped: Option<bool>,
}

fn list_query(p: ListParams) -> Result<ListQuery, ApiError> {
    let bad = |code: &'static str, msg: &str| ApiError::bad_request(code, msg.to_string());
    let bad_bbox = || {
        bad(
            "invalid_bbox",
            "bbox must be west,south,east,north in degrees",
        )
    };
    let given = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let bbox = match given(&p.bbox) {
        None => None,
        Some(raw) => {
            let v: Vec<f64> = raw
                .split(',')
                .map(|x| x.trim().parse())
                .collect::<Result<_, _>>()
                .map_err(|_| bad_bbox())?;
            let &[w, s, e, n] = v.as_slice() else {
                return Err(bad_bbox());
            };
            let lon_ok = |x: f64| x.is_finite() && (-180.0..=180.0).contains(&x);
            let lat_ok = |y: f64| y.is_finite() && (-90.0..=90.0).contains(&y);
            if !(lon_ok(w) && lon_ok(e) && lat_ok(s) && lat_ok(n)) || s > n {
                return Err(bad_bbox());
            }
            Some([w, s, e, n])
        }
    };
    let country = match given(&p.country) {
        None => None,
        Some(c) if c.len() == 2 && c.bytes().all(|b| b.is_ascii_alphabetic()) => {
            Some(c.to_ascii_lowercase())
        }
        Some(_) => {
            return Err(bad(
                "invalid_country",
                "country must be an ISO 3166-1 alpha-2 code",
            ));
        }
    };
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(bad(
            "invalid_limit",
            &format!("limit must be between 1 and {MAX_LIMIT}"),
        ));
    }
    let unshaped = p.unshaped.unwrap_or(country.is_some());
    Ok(ListQuery {
        bbox,
        country,
        limit,
        unshaped,
    })
}

async fn alerts_collection(
    State(state): State<Alerts>,
    headers: HeaderMap,
    ApiQuery(params): ApiQuery<ListParams>,
) -> ApiResult<Response> {
    let q = list_query(params)?;
    let inner = &state.inner;
    let revision = inner.revision.get(&inner.pool).await;
    let snap = inner.cache.list(&inner.pool, revision, &q).await?;
    let etag = HeaderValue::from_str(&snap.etag).map_err(ApiError::internal)?;
    let common = [
        (header::ETAG, etag),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        (header::VARY, HeaderValue::from_static("Accept-Encoding")),
    ];
    let header_str = |name| {
        headers
            .get(name)
            .and_then(|v: &HeaderValue| v.to_str().ok())
            .unwrap_or("")
    };
    if etag_matches(header_str(header::IF_NONE_MATCH), &snap.etag) {
        return Ok((StatusCode::NOT_MODIFIED, common).into_response());
    }
    let content_type = (
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/geo+json"),
    );
    if accepts_gzip(header_str(header::ACCEPT_ENCODING)) {
        let encoding = (header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        Ok((common, [content_type, encoding], snap.gzip.clone()).into_response())
    } else {
        Ok((common, [content_type], snap.json.clone()).into_response())
    }
}

#[derive(Deserialize)]
struct DetailQuery {
    id: String,
}

#[derive(Serialize, sqlx::FromRow)]
struct AlertDetail {
    #[serde(flatten)]
    #[sqlx(flatten)]
    props: AlertProps,
    description: Option<String>,
    instruction: Option<String>,
    sender_name: Option<String>,
    message_type: Option<String>,
    status: Option<String>,
    /// Ended alerts are kept for a day, so a notification tapped late
    /// still opens; this says whether it is still in effect.
    active: bool,
    #[serde(skip)]
    origin: String,
    /// Every info block (one per language), as plain text.
    infos: Option<Value>,
    #[sqlx(skip)]
    source_name: Option<String>,
    #[sqlx(skip)]
    country_name: Option<String>,
}

async fn alert_detail(
    State(state): State<Alerts>,
    ApiQuery(q): ApiQuery<DetailQuery>,
) -> ApiResult<Response> {
    const DETAIL_SQL: &str = concat!(
        "SELECT ",
        props_sql!(),
        ", description, instruction, sender_name, message_type, status, active, origin, infos
         FROM alerts WHERE id = $1"
    );
    let detail: Option<AlertDetail> = sqlx::query_as(DETAIL_SQL)
        .bind(&q.id)
        .fetch_optional(&state.inner.pool)
        .await?;
    let Some(mut d) = detail else {
        return Err(ApiError::not_found("unknown alert"));
    };
    if d.origin == "nws" {
        d.source_name = Some(NWS_SOURCE_NAME.to_string());
        d.country_name = Some(NWS_COUNTRY_NAME.to_string());
        // One block, from the NWS message's own fields.
        d.infos = Some(json!([{
            "language": d.props.language,
            "event": d.props.event,
            "headline": d.props.headline,
            "description": d.description,
            "instruction": d.instruction,
            "area_desc": d.props.area_desc,
            "sender_name": d.sender_name,
            "web": null,
            "category": d.props.category,
            "severity": d.props.severity,
            "urgency": d.props.urgency,
            "certainty": d.props.certainty,
            "onset": d.props.onset,
            "expires": d.props.expires,
            "in_effect": true,
        }]));
    } else {
        // The register in use, else the bundled one (a source can leave the
        // register while its alerts are still stored).
        let live = state.inner.world.as_ref().and_then(|w| {
            let st = w.lock();
            st.sources
                .iter()
                .find(|s| s.id == d.props.source)
                .map(|s| (s.name.clone(), s.country_name.clone()))
        });
        if let Some((name, country)) =
            live.or_else(|| crate::world::register::snapshot_names(&d.props.source))
        {
            d.source_name = Some(name);
            d.country_name = country;
        }
    }
    Ok(axum::Json(d).into_response())
}

/// How NWS alerts name their issuer, in the register's "Country: authority"
/// form. `source` and `country` are the `alerts` table's defaults for NWS
/// rows (`us-nws`, `us`).
pub(crate) const NWS_SOURCE_NAME: &str = "United States of America: National Weather Service";
pub(crate) const NWS_COUNTRY_NAME: &str = "United States of America";

#[derive(Deserialize)]
struct PointQuery {
    lat: f64,
    lon: f64,
}

async fn alerts_at(
    State(state): State<Alerts>,
    ApiQuery(q): ApiQuery<PointQuery>,
) -> ApiResult<Response> {
    validate::coordinates(q.lat, q.lon)?;
    const AT_SQL: &str = concat!(
        "SELECT ",
        props_sql!(),
        " FROM alerts
         WHERE active AND geom IS NOT NULL AND ST_Covers(geom, ST_SetSRID(ST_MakePoint($2, $1), 4326))
         ORDER BY ",
        order_sql!()
    );
    let alerts: Vec<AlertProps> = sqlx::query_as(AT_SQL)
        .bind(q.lat)
        .bind(q.lon)
        .fetch_all(&state.inner.pool)
        .await?;
    Ok(axum::Json(json!({ "alerts": alerts })).into_response())
}

async fn alert_sources(State(state): State<Alerts>) -> ApiResult<Response> {
    let sources = state.world_sources().await;
    let count = |s: &str| sources.iter().filter(|h| h.state == s).count();
    let register = state.inner.world.as_ref().map(|w| {
        let st = w.lock();
        json!({
            "loaded_from": st.register.loaded_from,
            "fetched_at": st.register.fetched_at,
            "last_error": st.register.last_error,
        })
    });
    Ok(axum::Json(json!({
        "enabled": state.inner.world.is_some(),
        "register": register,
        "summary": {
            "sources": sources.len(),
            "healthy": count("healthy"),
            "empty": count("empty"),
            "failing": count("failing"),
            "starting": count("starting"),
        },
        "attributions": attributions(&state),
        "sources": sources,
    }))
    .into_response())
}

fn attributions(state: &Alerts) -> Vec<&'static str> {
    let nuts = state
        .inner
        .world
        .as_ref()
        .is_some_and(|w| w.config.nuts_base_url.is_some());
    if nuts {
        vec![crate::NUTS_ATTRIBUTION]
    } else {
        Vec::new()
    }
}

async fn tilejson(State(state): State<Alerts>) -> ApiResult<Response> {
    let revision = state.revision().await;
    let fields = json!({
        "id": "String", "event": "String", "severity": "String", "level": "Number",
        "category": "String", "headline": "String", "expires": "Number", "expires_assumed": "Boolean",
        "source": "String", "country": "String", "geom_source": "String",
    });
    let body = json!({
        "tilejson": "3.0.0",
        "name": "alerts",
        "revision": revision,
        "tiles": [format!("/api/alerts/tiles/{revision}/{{z}}/{{x}}/{{y}}.mvt")],
        "minzoom": 0,
        "maxzoom": tiles::DETAIL_ZOOM,
        "attribution": attributions(&state).join("; "),
        "vector_layers": [{ "id": tiles::LAYER, "minzoom": 0, "maxzoom": tiles::DETAIL_ZOOM, "fields": fields }],
    });
    Ok((
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))],
        axum::Json(body),
    )
        .into_response())
}

async fn alert_tile(
    State(state): State<Alerts>,
    Path((revision, z, x, y)): Path<(String, String, String, String)>,
) -> ApiResult<Response> {
    let not_found = || ApiError::not_found("no such tile");
    let y = y
        .strip_suffix(".mvt")
        .or_else(|| y.strip_suffix(".pbf"))
        .unwrap_or(&y);
    let (Ok(revision), Ok(z), Ok(x), Ok(y)) = (
        revision.parse::<i64>(),
        z.parse::<u8>(),
        x.parse::<u32>(),
        y.parse::<u32>(),
    ) else {
        return Err(not_found());
    };
    if !tiles::valid(z, x, y) {
        return Err(not_found());
    }
    let inner = &state.inner;
    let current = inner.revision.get(&inner.pool).await;
    if revision > current {
        return Err(ApiError::not_found("unknown revision"));
    }
    let tile = inner.tiles.get(&inner.pool, current, z, x, y).await?;
    // A tile of the current revision never changes. One asked for under an
    // older revision gets today's shapes, which must not be cached under
    // that old URL.
    let cache = if revision == current {
        HeaderValue::from_static("public, max-age=31536000, immutable")
    } else {
        HeaderValue::from_static("no-cache")
    };
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.mapbox-vector-tile"),
            ),
            (header::CACHE_CONTROL, cache),
        ],
        tile,
    )
        .into_response())
}

async fn register_device(State(state): State<Alerts>) -> ApiResult<Response> {
    let (token, hash) = new_token()?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO devices (token_hash) VALUES ($1) RETURNING id")
        .bind(hash.as_slice())
        .fetch_one(&state.inner.pool)
        .await?;
    Ok(axum::Json(json!({ "device_id": id, "token": token })).into_response())
}

#[derive(Debug, Serialize)]
struct Place {
    id: Uuid,
    name: String,
    lat: f64,
    lon: f64,
    notify: bool,
    created_at: DateTime<Utc>,
    alerts: Vec<PlaceAlert>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct PlaceAlert {
    #[serde(skip)]
    place_id: Uuid,
    id: String,
    event: String,
    severity: String,
    headline: Option<String>,
    ends: Option<DateTime<Utc>>,
}

/// The device's places (or one of them) with the alerts covering each.
async fn load_places(
    state: &Alerts,
    device: Uuid,
    only: Option<Uuid>,
) -> Result<Vec<Place>, sqlx::Error> {
    let pool = &state.inner.pool;
    let rows: Vec<(Uuid, String, f64, f64, bool, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, name, ST_Y(geom), ST_X(geom), notify, created_at
         FROM places
         WHERE device_id = $1 AND ($2::uuid IS NULL OR id = $2)
         ORDER BY created_at, id",
    )
    .bind(device)
    .bind(only)
    .fetch_all(pool)
    .await?;
    const COVERING_SQL: &str = concat!(
        "SELECT p.id AS place_id, a.id, a.event, ",
        severity_sql!(),
        " AS severity, a.headline, a.ends
         FROM places p
         JOIN alerts a ON a.active AND a.geom IS NOT NULL AND ST_Covers(a.geom, p.geom)
         WHERE p.device_id = $1 AND ($2::uuid IS NULL OR p.id = $2)
         ORDER BY ",
        level_sql!(),
        " DESC, a.sent DESC NULLS LAST, a.id"
    );
    let covering: Vec<PlaceAlert> = sqlx::query_as(COVERING_SQL)
        .bind(device)
        .bind(only)
        .fetch_all(pool)
        .await?;
    let mut by_place: HashMap<Uuid, Vec<PlaceAlert>> = HashMap::new();
    for a in covering {
        by_place.entry(a.place_id).or_default().push(a);
    }
    Ok(rows
        .into_iter()
        .map(|(id, name, lat, lon, notify, created_at)| Place {
            alerts: by_place.remove(&id).unwrap_or_default(),
            id,
            name,
            lat,
            lon,
            notify,
            created_at,
        })
        .collect())
}

async fn list_places(State(state): State<Alerts>, device: Device) -> ApiResult<Response> {
    let places = load_places(&state, device.id, None).await?;
    Ok(axum::Json(json!({ "places": places })).into_response())
}

#[derive(Deserialize)]
struct NewPlace {
    name: String,
    lat: f64,
    lon: f64,
    notify: Option<bool>,
}

async fn create_place(
    State(state): State<Alerts>,
    device: Device,
    ApiJson(body): ApiJson<NewPlace>,
) -> ApiResult<Response> {
    let name = validate::place_name(&body.name)?;
    validate::coordinates(body.lat, body.lon)?;
    let notify = body.notify.unwrap_or(true);

    let mut tx = state.inner.pool.begin().await?;
    // Serializes concurrent creates for one device, so the limit holds.
    sqlx::query("SELECT 1 FROM devices WHERE id = $1 FOR UPDATE")
        .bind(device.id)
        .execute(&mut *tx)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM places WHERE device_id = $1")
        .bind(device.id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= validate::MAX_PLACES_PER_DEVICE {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "too_many_places",
            format!(
                "a device can save at most {} places",
                validate::MAX_PLACES_PER_DEVICE
            ),
        ));
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO places (device_id, name, geom, notify)
         VALUES ($1, $2, ST_SetSRID(ST_MakePoint($4, $3), 4326), $5)
         RETURNING id",
    )
    .bind(device.id)
    .bind(&name)
    .bind(body.lat)
    .bind(body.lon)
    .bind(notify)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    if notify {
        notify_if_covered(&state, id).await;
    }
    let place = load_places(&state, device.id, Some(id))
        .await?
        .pop()
        .ok_or_else(|| ApiError::not_found("place was deleted"))?;
    Ok((StatusCode::CREATED, axum::Json(place)).into_response())
}

/// A place saved inside a warning that is already in effect is notified
/// now rather than on the next poll.
async fn notify_if_covered(state: &Alerts, place_id: Uuid) {
    match notify::match_places(&state.inner, None, Some(place_id)).await {
        Ok(rows) => notify::dispatch(&state.inner, rows),
        Err(e) => {
            state
                .inner
                .force_sweep
                .store(true, std::sync::atomic::Ordering::Release);
            tracing::warn!("matching a new place against alerts failed: {e}");
        }
    }
}

#[derive(Deserialize)]
struct PlacePatch {
    name: Option<String>,
    notify: Option<bool>,
}

fn place_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| ApiError::not_found("unknown place"))
}

async fn update_place(
    State(state): State<Alerts>,
    device: Device,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<PlacePatch>,
) -> ApiResult<Response> {
    let id = place_id(&id)?;
    let name = body.name.as_deref().map(validate::place_name).transpose()?;
    let updated: Option<bool> = sqlx::query_scalar(
        "UPDATE places SET name = coalesce($3, name), notify = coalesce($4, notify)
         WHERE id = $1 AND device_id = $2
         RETURNING notify",
    )
    .bind(id)
    .bind(device.id)
    .bind(name)
    .bind(body.notify)
    .fetch_optional(&state.inner.pool)
    .await?;
    let notify = updated.ok_or_else(|| ApiError::not_found("unknown place"))?;
    if notify && body.notify == Some(true) {
        notify_if_covered(&state, id).await;
    }
    let place = load_places(&state, device.id, Some(id))
        .await?
        .pop()
        .ok_or_else(|| ApiError::not_found("unknown place"))?;
    Ok(axum::Json(place).into_response())
}

async fn delete_place(
    State(state): State<Alerts>,
    device: Device,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let id = place_id(&id)?;
    let deleted = sqlx::query("DELETE FROM places WHERE id = $1 AND device_id = $2")
        .bind(id)
        .bind(device.id)
        .execute(&state.inner.pool)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found("unknown place"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct SubscriptionKeys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
struct NewSubscription {
    endpoint: String,
    keys: SubscriptionKeys,
}

async fn subscribe(
    State(state): State<Alerts>,
    device: Device,
    headers: HeaderMap,
    ApiJson(body): ApiJson<NewSubscription>,
) -> ApiResult<Response> {
    let endpoint = validate::push_endpoint(
        body.endpoint.trim(),
        state.inner.config.allow_insecure_push_endpoints,
    )?;
    validate::subscription_keys(&body.keys.p256dh, &body.keys.auth)?;
    let user_agent: Option<String> = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|ua| ua.chars().take(256).collect());

    let mut tx = state.inner.pool.begin().await?;
    // A browser that registers again moves its endpoint to the new device.
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO push_subscriptions (device_id, endpoint, p256dh, auth, user_agent)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (endpoint) DO UPDATE SET
             device_id = EXCLUDED.device_id, p256dh = EXCLUDED.p256dh, auth = EXCLUDED.auth,
             user_agent = EXCLUDED.user_agent, failure_count = 0
         RETURNING id",
    )
    .bind(device.id)
    .bind(endpoint.as_str())
    .bind(body.keys.p256dh.trim())
    .bind(body.keys.auth.trim())
    .bind(user_agent)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM push_subscriptions
         WHERE device_id = $1 AND id <> $2
           AND id NOT IN (SELECT id FROM push_subscriptions
                          WHERE device_id = $1 AND id <> $2
                          ORDER BY created_at DESC LIMIT $3)",
    )
    .bind(device.id)
    .bind(id)
    .bind(MAX_SUBSCRIPTIONS_PER_DEVICE - 1)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    // Warnings that found this device's places before it could receive
    // pushes (a page typically asks for permission after the first save, or
    // its old subscription expired): delivered now if still in effect,
    // however long ago they began.
    notify::requeue_waiting(&state.inner, Some(device.id)).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct EndpointOnly {
    endpoint: String,
}

async fn unsubscribe(
    State(state): State<Alerts>,
    device: Device,
    ApiJson(body): ApiJson<EndpointOnly>,
) -> ApiResult<Response> {
    sqlx::query("DELETE FROM push_subscriptions WHERE endpoint = $1 AND device_id = $2")
        .bind(body.endpoint.trim())
        .bind(device.id)
        .execute(&state.inner.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn test_push(State(state): State<Alerts>, device: Device) -> ApiResult<Response> {
    if !state.inner.push.configured() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "push_not_configured",
            "this server has no VAPID keys, so it cannot send push notifications",
        ));
    }
    let subs = notify::subscriptions(&state.inner.pool, device.id).await?;
    let payload = serde_json::to_vec(&Payload {
        title: "Test notification",
        body: "Notifications from the weather map are working.".to_string(),
        tag: "test",
        url: "/".to_string(),
        alert_id: None,
        place_id: None,
        severity: None,
    })
    .map_err(ApiError::internal)?;
    let opts = PushOptions {
        ttl_secs: 300,
        urgency: "normal",
        topic: None,
    };
    let outcome = notify::send_all(&state.inner, &subs, &payload, &opts).await?;
    Ok(axum::Json(json!({ "sent": outcome.sent, "failed": outcome.errors.len() })).into_response())
}
