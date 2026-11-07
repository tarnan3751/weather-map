//! Alert shapes as Mapbox Vector Tiles, straight from PostGIS.
//!
//! The alert set has a revision number that rises whenever what the map
//! shows changes. Tile URLs carry it, so a tile is the same bytes for ever
//! and can be cached as immutable; a client that hears of a new revision
//! (the `alerts` event, or any list response) switches to the new URLs. A
//! revision must therefore never stand for two different sets of shapes:
//! it is kept in `alerts_revision`, every new one is also at least the time
//! of the change in milliseconds since 1970, and a starting server takes a
//! new one before serving anything. Neither a restart, a failed write of
//! the counter, nor a database restored from a backup can bring an old
//! number back (as long as the clock does not go back).
//!
//! Shapes are stored in Web Mercator by a trigger (`geom_mvt`, and a
//! coarse `geom_mvt_low` for zooms up to 5) and simplified per zoom to
//! about one tile unit before `ST_AsMVTGeom` clips them.
//!
//! Tiles of the current revision are kept in memory, built once however
//! many clients ask at the same moment.

use bytes::Bytes;
// SQL assembled from this crate's constants only, never from input.
use sqlx::PgPool;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::{OnceCell, Semaphore};

/// Layer name inside every tile.
pub(crate) const LAYER: &str = "alerts";
const EXTENT: u32 = 4096;
const MAX_ZOOM: u8 = 16;
/// Zoom past which clients should overzoom: tiles beyond add no detail.
pub(crate) const DETAIL_ZOOM: u8 = 12;
/// Tile bytes kept in memory.
const CACHE_BYTES: usize = 64 << 20;
/// Tile queries at once, so tiles never take every pooled connection.
const QUERY_CONCURRENCY: usize = 4;
const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.685_578_49;

pub(crate) struct Revision {
    current: AtomicI64,
    loaded: OnceCell<()>,
}

impl Revision {
    pub fn new() -> Self {
        Self {
            current: AtomicI64::new(0),
            loaded: OnceCell::new(),
        }
    }

    /// The current revision. The first call takes a new one: what this
    /// process serves may differ from what the last one served under the
    /// stored number.
    pub async fn get(&self, pool: &PgPool) -> i64 {
        self.loaded
            .get_or_init(|| async {
                self.advance(pool, 0).await;
            })
            .await;
        self.current.load(Ordering::Acquire)
    }

    /// Raise the revision after a change and return the new one.
    pub async fn bump(&self, pool: &PgPool) -> i64 {
        let before = self.get(pool).await;
        self.advance(pool, before).await
    }

    /// One more than both the stored revision and `before`, and at least now
    /// in milliseconds. If recording it fails, the number is still new (the
    /// clock makes it so) and the next one recorded is higher.
    async fn advance(&self, pool: &PgPool, before: i64) -> i64 {
        let floor = next_revision(before, chrono::Utc::now().timestamp_millis());
        let next = match sqlx::query_scalar::<_, i64>(
            "UPDATE alerts_revision SET revision = greatest(revision + 1, $1), changed_at = now() RETURNING revision",
        )
        .bind(floor)
        .fetch_one(pool)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("recording the alerts revision failed: {e}");
                floor
            }
        };
        self.current.fetch_max(next, Ordering::AcqRel);
        self.current.load(Ordering::Acquire)
    }
}

/// The revision after `before`, changed at `now_ms`.
fn next_revision(before: i64, now_ms: i64) -> i64 {
    (before + 1).max(now_ms)
}

type Key = (u8, u32, u32);

#[derive(Default)]
struct CacheState {
    revision: i64,
    tiles: HashMap<Key, Arc<OnceCell<Bytes>>>,
    order: VecDeque<Key>,
    bytes: usize,
}

pub(crate) struct TileCache {
    state: Mutex<CacheState>,
    queries: Semaphore,
}

impl TileCache {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(CacheState::default()),
            queries: Semaphore::new(QUERY_CONCURRENCY),
        }
    }

    fn cell(&self, revision: i64, key: Key) -> Arc<OnceCell<Bytes>> {
        let mut st = crate::lock(&self.state);
        if st.revision != revision {
            *st = CacheState {
                revision,
                ..CacheState::default()
            };
        }
        if let Some(c) = st.tiles.get(&key) {
            return c.clone();
        }
        let cell = Arc::new(OnceCell::new());
        st.tiles.insert(key, cell.clone());
        st.order.push_back(key);
        cell
    }

    fn account(&self, revision: i64, len: usize) {
        let mut st = crate::lock(&self.state);
        if st.revision != revision {
            return;
        }
        st.bytes += len;
        while st.bytes > CACHE_BYTES {
            let Some(old) = st.order.pop_front() else {
                break;
            };
            if let Some(cell) = st.tiles.remove(&old) {
                st.bytes = st.bytes.saturating_sub(cell.get().map_or(0, Bytes::len));
            }
        }
    }

    /// The tile at `z/x/y` for `revision` (which must be the current one).
    pub async fn get(
        &self,
        pool: &PgPool,
        revision: i64,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Bytes, sqlx::Error> {
        let cell = self.cell(revision, (z, x, y));
        let mut built = false;
        let tile = cell
            .get_or_try_init(|| async {
                built = true;
                let _slot = self
                    .queries
                    .acquire()
                    .await
                    .expect("the semaphore is never closed");
                render(pool, z, x, y).await.map(Bytes::from)
            })
            .await?
            .clone();
        if built {
            self.account(revision, tile.len());
        }
        Ok(tile)
    }
}

/// `true` for a tile address that exists.
pub(crate) fn valid(z: u8, x: u32, y: u32) -> bool {
    z <= MAX_ZOOM && x < (1u32 << z) && y < (1u32 << z)
}

/// Simplification tolerance in metres at zoom `z`: one tile unit, or none
/// where the stored shapes are already finer than that.
fn tolerance(z: u8) -> Option<f64> {
    (z < 10).then(|| EARTH_CIRCUMFERENCE_M / f64::from(1u32 << z) / f64::from(EXTENT))
}

/// `$1`, `$2`, `$3` the tile, `$4` the simplification tolerance or NULL.
/// One feature per active alert with a shape, the most severe last.
static TILE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT coalesce((
             SELECT ST_AsMVT(t, '{LAYER}', {EXTENT}, 'geom')
             FROM (
                 SELECT ST_AsMVTGeom(
                            CASE WHEN $4::float8 IS NULL THEN s.g ELSE ST_Simplify(s.g, $4, true) END,
                            e.env, {EXTENT}, 64, true) AS geom,
                        id, event, {severity} AS severity, ({level})::int AS level, {category} AS category,
                        left(coalesce(headline, event), 120) AS headline,
                        floor(extract(epoch FROM {ends}))::bigint AS expires,
                        (origin = 'cap' AND expires IS NULL) AS expires_assumed,
                        source, country, geom_source
                 FROM alerts a
                 CROSS JOIN (SELECT ST_TileEnvelope($1, $2, $3) AS env,
                                    ST_TileEnvelope($1, $2, $3, margin => 64.0 / {EXTENT}) AS menv) e
                 CROSS JOIN LATERAL (SELECT CASE WHEN $1 <= 5 THEN a.geom_mvt_low ELSE a.geom_mvt END AS g) s
                 WHERE active AND geom_mvt IS NOT NULL AND geom_mvt && e.menv
                 ORDER BY ({level}), sent NULLS FIRST, id
             ) t
             WHERE t.geom IS NOT NULL
         ), ''::bytea)",
        severity = severity_sql!(),
        level = level_sql!(),
        category = category_sql!(),
        ends = ends_sql!(),
    )
});

async fn render(pool: &PgPool, z: u8, x: u32, y: u32) -> Result<Vec<u8>, sqlx::Error> {
    sqlx::query_scalar(TILE_SQL.as_str())
        .bind(i32::from(z))
        .bind(x as i32)
        .bind(y as i32)
        .bind(tolerance(z))
        .fetch_one(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_addresses() {
        assert!(valid(0, 0, 0));
        assert!(!valid(0, 1, 0));
        assert!(valid(3, 7, 7));
        assert!(!valid(3, 8, 0));
        assert!(!valid(MAX_ZOOM + 1, 0, 0));
    }

    #[test]
    fn revisions_only_rise() {
        assert_eq!(next_revision(5, 1_793_990_000_000), 1_793_990_000_000);
        // Two changes in the same millisecond, or a clock that stepped back.
        assert_eq!(
            next_revision(1_793_990_000_000, 1_793_990_000_000),
            1_793_990_000_001
        );
        assert_eq!(
            next_revision(1_793_990_000_500, 1_793_990_000_000),
            1_793_990_000_501
        );
    }

    #[test]
    fn tolerance_is_one_tile_unit() {
        let t0 = tolerance(0).unwrap();
        assert!((t0 - 9783.94).abs() < 0.1, "{t0}");
        assert!((tolerance(5).unwrap() - t0 / 32.0).abs() < 1e-6);
        assert_eq!(tolerance(10), None);
    }
}
