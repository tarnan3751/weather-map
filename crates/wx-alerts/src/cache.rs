//! The `/api/alerts` list: a GeoJSON FeatureCollection of active alerts,
//! filtered by bounding box and country and cut at a limit. Responses are
//! serialized (and gzipped) once per alert-set revision and query rather
//! than once per request: every open map asks again after every change.
//!
//! Display shapes are much lighter than the stored ones. Storm-based
//! polygons are small and drawn as issued. Zone and region unions, and
//! large world polygons, are snapped to a grid (which also pinches off
//! slivers), simplified, stripped of parts and holes a few cells across,
//! and written with only the decimals the grid needs. The grid starts at
//! about 1/`DISPLAY_DETAIL` of the shape's larger side and is coarsened
//! until the shape fits `MAX_DISPLAY_POINTS`: a county keeps ~500 m detail,
//! while an Alaska Panhandle marine zone (69,000 vertices of fjords and
//! islands, named by half a dozen alerts at once) drops from 1.5 MB to about
//! 14 KB. A display shape is computed once per shape and reused: zone and
//! region shapes are keyed by their code list, so the next update over the
//! same zones costs nothing.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

/// Display grids for zone shapes, in degrees: values that print exactly
/// with 3, 2 or 1 decimals. 0.005 degrees is about one pixel at zoom 8.
const GRID_LADDER: [f64; 5] = [0.005, 0.01, 0.02, 0.05, 0.1];
/// The starting grid is the shape's larger side divided by this.
const DISPLAY_DETAIL: f64 = 500.0;
/// Coarser grids are tried until a shape has at most this many vertices.
const MAX_DISPLAY_POINTS: i32 = 1000;
/// Parts and holes smaller than this many grid cells across are dropped.
const MIN_FEATURE_CELLS: f64 = 4.0;
/// Fallback simplification when snapping fails.
const FALLBACK_SIMPLIFY_DEG: f64 = 0.01;
/// World polygons up to this many vertices are drawn as issued.
const AS_ISSUED_POINTS: i32 = 500;
/// Responses kept (each for one revision and query).
const MAX_RESPONSES: usize = 64;
/// Display shapes kept for alerts no longer in a response.
const MAX_DISPLAY_SHAPES: usize = 20_000;
pub(crate) const DEFAULT_LIMIT: usize = 1000;
pub(crate) const MAX_LIMIT: usize = 5000;

#[derive(Debug)]
pub(crate) struct Snapshot {
    pub json: Bytes,
    pub gzip: Bytes,
    pub etag: String,
    pub features: usize,
}

/// Every `category` value an alert can have (CAP's list).
pub(crate) const CATEGORIES: [&str; 12] = [
    "met",
    "geo",
    "safety",
    "security",
    "rescue",
    "fire",
    "health",
    "env",
    "transport",
    "infra",
    "cbrne",
    "other",
];

/// Properties of an alert as the map sees them (`/api/alerts`,
/// `/api/alerts/at`, and the base of `/api/alerts/detail`).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub(crate) struct AlertProps {
    pub id: String,
    pub event: String,
    pub severity: String,
    pub urgency: Option<String>,
    pub certainty: Option<String>,
    pub headline: Option<String>,
    pub area_desc: Option<String>,
    pub sent: Option<DateTime<Utc>>,
    pub onset: Option<DateTime<Utc>>,
    pub ends: Option<DateTime<Utc>>,
    /// For a world alert whose sender gave no expiry, the assumed one.
    pub expires: Option<DateTime<Utc>>,
    /// `expires` is assumed (a day after the alert starts), not the sender's.
    pub expires_assumed: bool,
    pub geom_source: String,
    pub source: String,
    pub country: Option<String>,
    pub language: String,
    pub category: String,
}

/// `$1` unshaped too, `$2` the country or NULL, `$3` and `$4` the box as
/// one or two envelopes or NULL, `$5` the limit.
const LIST_SQL: &str = concat!(
    "SELECT ",
    props_sql!(),
    ",
            CASE WHEN geom IS NULL THEN NULL
                 WHEN geom_source = 'zones'
                 THEN 'z:' || array_to_string(ugc, ',') || '-' || array_to_string(zones_missing, ',')
                 WHEN geom_source = 'nuts'
                 THEN 'n:' || array_to_string(region_codes, ',') || '-' || array_to_string(zones_missing, ',')
                 ELSE 'a:' || id END AS shape_key,
            ST_X(anchor) AS anchor_lon, ST_Y(anchor) AS anchor_lat
     FROM alerts
     WHERE active
       AND ($1 OR geom IS NOT NULL)
       AND ($2::text IS NULL OR country = $2)
       AND ($3::float8[] IS NULL
            OR (geom IS NOT NULL AND ST_Intersects(geom, ST_MakeEnvelope($3[1], $3[2], $3[3], $3[4], 4326)))
            OR (geom IS NOT NULL AND $4::float8[] IS NOT NULL
                AND ST_Intersects(geom, ST_MakeEnvelope($4[1], $4[2], $4[3], $4[4], 4326)))
            OR (geom IS NULL AND anchor && ST_MakeEnvelope($3[1], $3[2], $3[3], $3[4], 4326))
            OR (geom IS NULL AND $4::float8[] IS NOT NULL
                AND anchor && ST_MakeEnvelope($4[1], $4[2], $4[3], $4[4], 4326)))
     ORDER BY ",
    order_sql!(),
    "
     LIMIT $5"
);

#[derive(sqlx::FromRow)]
struct Row {
    #[sqlx(flatten)]
    props: AlertProps,
    shape_key: Option<String>,
    anchor_lon: Option<f64>,
    anchor_lat: Option<f64>,
}

#[derive(Serialize)]
struct FeatureProps<'a> {
    #[serde(flatten)]
    props: &'a AlertProps,
    anchor: Option<[f64; 2]>,
}

/// What `/api/alerts` was asked for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ListQuery {
    /// West, south, east, north. West greater than east crosses the
    /// antimeridian.
    pub bbox: Option<[f64; 4]>,
    pub country: Option<String>,
    pub limit: usize,
    /// Alerts without a shape too (listed at their anchor).
    pub unshaped: bool,
}

impl Default for ListQuery {
    fn default() -> Self {
        Self {
            bbox: None,
            country: None,
            limit: DEFAULT_LIMIT,
            unshaped: false,
        }
    }
}

impl ListQuery {
    fn key(&self) -> String {
        format!(
            "{:?}|{:?}|{}|{}",
            self.bbox, self.country, self.limit, self.unshaped
        )
    }
}

/// A response, or the build of one under way.
type Slot = Arc<OnceCell<Arc<Snapshot>>>;

#[derive(Default)]
struct Responses {
    /// By revision and query.
    map: HashMap<(i64, String), Slot>,
    order: VecDeque<(i64, String)>,
}

#[derive(Default)]
pub(crate) struct AlertsCache {
    responses: Mutex<Responses>,
    /// Display GeoJSON by shape key.
    display: Mutex<HashMap<String, Arc<str>>>,
    /// Alerts whose shape needed the plain fallback, so each is logged once.
    fell_back: Mutex<HashSet<String>>,
}

impl AlertsCache {
    /// Forget the display shapes of alerts whose own polygon changed.
    pub fn invalidate<'a>(&self, alert_ids: impl IntoIterator<Item = &'a String>) {
        let mut display = crate::lock(&self.display);
        for id in alert_ids {
            display.remove(&format!("a:{id}"));
        }
    }

    /// The response for `q` at `revision`, built if not cached.
    pub async fn list(
        &self,
        pool: &PgPool,
        revision: i64,
        q: &ListQuery,
    ) -> anyhow::Result<Arc<Snapshot>> {
        self.single_flight(revision, q, || async {
            let snap = self.build(pool, revision, q).await?;
            tracing::debug!(
                features = snap.features,
                bytes = snap.json.len(),
                "built /api/alerts response"
            );
            Ok(snap)
        })
        .await
    }

    /// The response for `q` at `revision`, from `build` if there is none.
    /// Requests for the same query while it is being built wait for that
    /// build; different queries build at the same time.
    async fn single_flight<F, Fut>(
        &self,
        revision: i64,
        q: &ListQuery,
        build: F,
    ) -> anyhow::Result<Arc<Snapshot>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<Arc<Snapshot>>>,
    {
        let key = (revision, q.key());
        let slot = {
            let mut r = crate::lock(&self.responses);
            match r.map.get(&key) {
                Some(slot) => slot.clone(),
                None => {
                    // Older revisions are never asked for again.
                    r.order.retain(|k| k.0 >= revision);
                    r.map.retain(|k, _| k.0 >= revision);
                    while r.order.len() >= MAX_RESPONSES {
                        if let Some(old) = r.order.pop_front() {
                            r.map.remove(&old);
                        }
                    }
                    let slot = Slot::default();
                    r.order.push_back(key.clone());
                    r.map.insert(key, slot.clone());
                    slot
                }
            }
        };
        slot.get_or_try_init(build).await.cloned()
    }

    async fn build(
        &self,
        pool: &PgPool,
        revision: i64,
        q: &ListQuery,
    ) -> anyhow::Result<Arc<Snapshot>> {
        // Two envelopes when the box crosses the antimeridian. Shapes are
        // tested exactly, not by bounding box: a shape split at the
        // antimeridian has a box spanning every longitude. An alert without
        // a shape is placed at its anchor.
        let (env_a, env_b) = match q.bbox {
            None => (None, None),
            Some([w, s, e, n]) if w > e => (Some([w, s, 180.0, n]), Some([-180.0, s, e, n])),
            Some(b) => (Some(b), None),
        };
        let mut rows: Vec<Row> = sqlx::query_as(LIST_SQL)
            .bind(q.unshaped)
            .bind(&q.country)
            .bind(env_a.map(|b| b.to_vec()))
            .bind(env_b.map(|b| b.to_vec()))
            .bind(q.limit as i64 + 1)
            .fetch_all(pool)
            .await?;
        let truncated = rows.len() > q.limit;
        rows.truncate(q.limit);

        let missing: Vec<(String, String)> = {
            let display = crate::lock(&self.display);
            let mut seen = HashSet::new();
            rows.iter()
                .filter_map(|r| r.shape_key.as_ref().map(|k| (r, k)))
                .filter(|(_, k)| !display.contains_key(*k) && seen.insert((*k).clone()))
                .map(|(r, k)| (r.props.id.clone(), k.clone()))
                .collect()
        };
        if !missing.is_empty() {
            let ids: Vec<String> = missing.iter().map(|(id, _)| id.clone()).collect();
            let shapes: HashMap<String, String> =
                self.display_shapes(pool, &ids).await?.into_iter().collect();
            let mut display = crate::lock(&self.display);
            if display.len() > MAX_DISPLAY_SHAPES {
                display.clear();
            }
            for (id, key) in missing {
                if let Some(g) = shapes.get(&id) {
                    display.insert(key, Arc::from(g.as_str()));
                }
            }
        }
        let display: HashMap<String, Arc<str>> = {
            let display = crate::lock(&self.display);
            rows.iter()
                .filter_map(|r| r.shape_key.as_ref())
                .filter_map(|k| display.get(k).map(|g| (k.clone(), g.clone())))
                .collect()
        };

        let mut json = Vec::with_capacity(64 * 1024);
        json.extend_from_slice(br#"{"type":"FeatureCollection","revision":"#);
        json.extend_from_slice(revision.to_string().as_bytes());
        json.extend_from_slice(br#","truncated":"#);
        json.extend_from_slice(if truncated { b"true" } else { b"false" });
        json.extend_from_slice(br#","features":["#);
        let mut features = 0;
        for row in &rows {
            let geometry = match &row.shape_key {
                Some(k) => match display.get(k) {
                    Some(g) => g.as_ref(),
                    // Shape removed between the two queries: skip it.
                    None => continue,
                },
                None => "null",
            };
            if features > 0 {
                json.push(b',');
            }
            json.extend_from_slice(br#"{"type":"Feature","id":"#);
            serde_json::to_writer(&mut json, &row.props.id)?;
            json.extend_from_slice(br#","geometry":"#);
            json.extend_from_slice(geometry.as_bytes());
            json.extend_from_slice(br#","properties":"#);
            let anchor = row
                .anchor_lon
                .zip(row.anchor_lat)
                .map(|(x, y)| [round6(x), round6(y)]);
            serde_json::to_writer(
                &mut json,
                &FeatureProps {
                    props: &row.props,
                    anchor,
                },
            )?;
            json.push(b'}');
            features += 1;
        }
        json.extend_from_slice(b"]}");

        let mut gz = GzEncoder::new(Vec::with_capacity(json.len() / 4), Compression::new(6));
        gz.write_all(&json)?;
        let gzip = gz.finish()?;
        let etag = format!(
            "W/\"{}\"",
            crate::world::geo::hex(&Sha256::digest(&json)[..12])
        );
        Ok(Arc::new(Snapshot {
            json: Bytes::from(json),
            gzip: Bytes::from(gzip),
            etag,
            features,
        }))
    }
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

impl AlertsCache {
    /// Display GeoJSON for the given alerts' current shapes.
    ///
    /// GEOS can throw on a shape (snapping to the grid can make a "side
    /// location conflict"), which fails the whole batch. The batch is then
    /// halved until the shapes that fail are alone; those get the plain
    /// fallback and everything else its usual display shape. Each alert that
    /// falls back is logged once, at debug level: the fallback is a sound
    /// shape and nothing needs doing about it.
    async fn display_shapes(
        &self,
        pool: &PgPool,
        ids: &[String],
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut out = Vec::with_capacity(ids.len());
        let mut todo: Vec<&[String]> = vec![ids];
        while let Some(batch) = todo.pop() {
            match budgeted_shapes(pool, batch).await {
                Ok(rows) => out.extend(rows),
                Err(sqlx::Error::Database(_)) if batch.len() > 1 => {
                    let (a, b) = batch.split_at(batch.len() / 2);
                    todo.push(a);
                    todo.push(b);
                }
                Err(sqlx::Error::Database(e)) => {
                    let id = &batch[0];
                    let first = {
                        let mut seen = crate::lock(&self.fell_back);
                        if seen.len() > 10_000 {
                            seen.clear();
                        }
                        seen.insert(id.clone())
                    };
                    if first {
                        tracing::debug!(alert = %id, "display simplification failed, drawing a plainer shape: {e}");
                    }
                    out.extend(fallback_shape(pool, id).await?);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(out)
    }
}

/// A display shape made without the steps that can fail: Douglas-Peucker
/// (never GEOS) keeping parts that would collapse, repaired, and the stored
/// shape itself if the repair leaves nothing. If even the repair throws,
/// the simplified shape is drawn as it is.
async fn fallback_shape(pool: &PgPool, id: &str) -> Result<Option<(String, String)>, sqlx::Error> {
    let repaired = sqlx::query_as(
        "SELECT a.id, ST_AsGeoJSON(CASE WHEN v.g IS NULL OR ST_IsEmpty(v.g) THEN a.geom ELSE v.g END, 3)
         FROM alerts a
         CROSS JOIN LATERAL (SELECT ST_Multi(ST_CollectionExtract(ST_MakeValid(ST_Simplify(a.geom, $2, true)), 3)) AS g) v
         WHERE a.id = $1 AND a.geom IS NOT NULL",
    )
    .bind(id)
    .bind(FALLBACK_SIMPLIFY_DEG)
    .fetch_optional(pool)
    .await;
    match repaired {
        Err(sqlx::Error::Database(_)) => {
            sqlx::query_as(
                "SELECT id, ST_AsGeoJSON(ST_Simplify(geom, $2, true), 3) FROM alerts WHERE id = $1 AND geom IS NOT NULL",
            )
            .bind(id)
            .bind(FALLBACK_SIMPLIFY_DEG)
            .fetch_optional(pool)
            .await
        }
        other => other,
    }
}

/// Round by round up the grid ladder: each round only re-simplifies the
/// shapes that came out over the vertex budget.
async fn budgeted_shapes(
    pool: &PgPool,
    ids: &[String],
) -> Result<Vec<(String, String)>, sqlx::Error> {
    let extents: Vec<(String, f64)> = sqlx::query_as(
        "SELECT id, greatest(ST_XMax(geom) - ST_XMin(geom), ST_YMax(geom) - ST_YMin(geom))
         FROM alerts WHERE id = ANY($1) AND geom IS NOT NULL",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    let mut todo: Vec<(String, usize)> = extents
        .into_iter()
        .map(|(id, extent)| (id, start_level(extent)))
        .collect();
    let mut done = Vec::with_capacity(todo.len());
    while !todo.is_empty() {
        let ids: Vec<String> = todo.iter().map(|(id, _)| id.clone()).collect();
        let grids: Vec<f64> = todo.iter().map(|(_, level)| GRID_LADDER[*level]).collect();
        let level_of: HashMap<String, usize> = todo.drain(..).collect();
        let rows: Vec<(String, i32, String)> = sqlx::query_as(DISPLAY_SQL)
            .bind(&ids)
            .bind(&grids)
            .bind(MIN_FEATURE_CELLS)
            .bind(AS_ISSUED_POINTS)
            .fetch_all(pool)
            .await?;
        for (id, points, geojson) in rows {
            let level = level_of.get(&id).copied().unwrap_or(GRID_LADDER.len() - 1);
            if points > MAX_DISPLAY_POINTS && level + 1 < GRID_LADDER.len() {
                todo.push((id, level + 1));
            } else {
                done.push((id, geojson));
            }
        }
    }
    Ok(done)
}

fn start_level(extent: f64) -> usize {
    GRID_LADDER
        .iter()
        .position(|g| extent <= g * DISPLAY_DETAIL)
        .unwrap_or(GRID_LADDER.len() - 1)
}

/// `$1` ids, `$2` the grid for each, `$3` the smallest feature in cells,
/// `$4` the most vertices a world polygon may have and be drawn as issued.
/// Returns the vertex count and the GeoJSON.
const DISPLAY_SQL: &str = "
    SELECT a.id, ST_NPoints(d.g), ST_AsGeoJSON(d.g, d.digits)
    FROM unnest($1::text[], $2::float8[]) AS t(id, grid)
    JOIN alerts a ON a.id = t.id AND a.geom IS NOT NULL
    CROSS JOIN LATERAL (
        SELECT a.geom_source = 'polygon' OR (a.geom_source = 'cap' AND ST_NPoints(a.geom) <= $4) AS as_issued
    ) i
    CROSS JOIN LATERAL (
        SELECT CASE WHEN i.as_issued THEN NULL
                    ELSE ST_SimplifyPreserveTopology(ST_ReducePrecision(a.geom, t.grid), t.grid)
               END AS simple,
               ($3 * t.grid) ^ 2 AS min_area
    ) s
    CROSS JOIN LATERAL (
        SELECT ST_Multi(ST_Collect(ARRAY(
                   SELECT ST_MakePolygon(ST_ExteriorRing(p.geom), coalesce(ARRAY(
                              SELECT ST_ExteriorRing(r.geom) FROM ST_DumpRings(p.geom) r
                              WHERE r.path[1] > 0 AND ST_Area(r.geom) >= s.min_area), '{}'))
                   FROM ST_Dump(s.simple) p
                   WHERE ST_Area(p.geom) >= s.min_area))) AS trimmed
    ) tr
    CROSS JOIN LATERAL (
        SELECT CASE WHEN i.as_issued THEN a.geom
                    WHEN tr.trimmed IS NOT NULL AND NOT ST_IsEmpty(tr.trimmed) THEN tr.trimmed
                    WHEN s.simple IS NOT NULL AND NOT ST_IsEmpty(s.simple) THEN s.simple
                    ELSE a.geom
               END AS g,
               CASE WHEN i.as_issued THEN 4
                    WHEN t.grid < 0.01 THEN 3 WHEN t.grid < 0.1 THEN 2 ELSE 1 END AS digits
    ) d";

/// `If-None-Match` against a weak ETag (weak comparison, RFC 9110 8.8.3.2).
pub(crate) fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    let bare = |t: &str| t.trim().trim_start_matches("W/").to_string();
    let ours = bare(etag);
    if_none_match
        .split(',')
        .any(|t| t.trim() == "*" || bare(t) == ours)
}

/// Whether `Accept-Encoding` allows gzip.
pub(crate) fn accepts_gzip(accept_encoding: &str) -> bool {
    accept_encoding.split(',').any(|item| {
        let mut parts = item.split(';');
        let coding = parts.next().unwrap_or("").trim();
        let q_zero = parts.any(|p| {
            let p = p.trim();
            p.strip_prefix("q=")
                .is_some_and(|q| q.trim().parse::<f32>().is_ok_and(|q| q == 0.0))
        });
        (coding.eq_ignore_ascii_case("gzip") || coding == "*") && !q_zero
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_starts_from_the_shape_size() {
        assert_eq!(GRID_LADDER[start_level(0.4)], 0.005, "a county");
        assert_eq!(GRID_LADDER[start_level(2.5)], 0.005);
        assert_eq!(
            GRID_LADDER[start_level(3.2)],
            0.01,
            "an Alaska Panhandle marine zone"
        );
        assert_eq!(GRID_LADDER[start_level(12.0)], 0.05);
        assert_eq!(GRID_LADDER[start_level(400.0)], 0.1);
    }

    #[tokio::test]
    async fn different_queries_build_at_once_and_each_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let snapshot = || {
            let (json, gzip) = (Bytes::new(), Bytes::new());
            Arc::new(Snapshot {
                json,
                gzip,
                etag: String::new(),
                features: 0,
            })
        };
        let cache = AlertsCache::default();
        let builds = AtomicUsize::new(0);
        // Each build waits until the other has started.
        let both = tokio::sync::Barrier::new(2);
        let build = || async {
            builds.fetch_add(1, Ordering::SeqCst);
            both.wait().await;
            Ok(snapshot())
        };
        let a = ListQuery::default();
        let b = ListQuery {
            country: Some("it".into()),
            ..ListQuery::default()
        };
        let together = async {
            tokio::join!(
                cache.single_flight(1, &a, build),
                cache.single_flight(1, &b, build)
            )
        };
        let (ra, rb) = tokio::time::timeout(std::time::Duration::from_secs(5), together)
            .await
            .expect("two queries build at the same time");
        assert!(!Arc::ptr_eq(&ra.unwrap(), &rb.unwrap()));

        // The same query asked for twice at once is built once, then kept.
        let c = ListQuery {
            limit: 5,
            ..ListQuery::default()
        };
        let slow = || async {
            builds.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            Ok(snapshot())
        };
        let (x, y) = tokio::join!(
            cache.single_flight(1, &c, slow),
            cache.single_flight(1, &c, slow)
        );
        assert!(Arc::ptr_eq(&x.unwrap(), &y.unwrap()));
        let again = cache.single_flight(1, &c, || async { Err(anyhow::anyhow!("built again")) });
        assert!(again.await.is_ok());
        assert_eq!(builds.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn etags() {
        let etag = "W/\"abc\"";
        assert!(etag_matches("W/\"abc\"", etag));
        assert!(etag_matches("\"abc\"", etag));
        assert!(etag_matches("\"x\", W/\"abc\"", etag));
        assert!(etag_matches("*", etag));
        assert!(!etag_matches("W/\"abd\"", etag));
    }

    #[test]
    fn gzip_negotiation() {
        assert!(accepts_gzip("gzip"));
        assert!(accepts_gzip("br, gzip;q=0.8"));
        assert!(accepts_gzip("*"));
        assert!(!accepts_gzip("gzip;q=0"));
        assert!(!accepts_gzip("br"));
        assert!(!accepts_gzip(""));
    }
}
