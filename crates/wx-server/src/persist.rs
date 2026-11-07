//! Keeps the `parts` table in step with the in-memory catalog, so a restart
//! comes back with the timelines it had instead of an empty map.

use anyhow::Context;
use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use object_store::ObjectStore;
use object_store::path::Path;
use sqlx::{PgPool, Row};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wx_core::{FrameCatalog, FrameKind, Hub, LonLatBox, Part, WsEvent, keys};

/// What the table is known to hold, as `(layer, part id)`. This process is
/// the only writer (see the instance lock), so after the first read the
/// table never has to be read again to know what to change.
pub type Saved = HashSet<(String, String)>;

/// Load remembered parts into the catalog. Returns how many it took, and
/// what the table holds afterwards for [`spawn_mirror`].
///
/// The table is only trusted as far as the store agrees: a part whose grid
/// is not there (the store was emptied or swapped, or the janitor got ahead
/// of the last save) is forgotten, so the feeds build it again instead of
/// the map showing a hole.
pub async fn restore(
    pool: &PgPool,
    catalog: &FrameCatalog,
    store: &Arc<dyn ObjectStore>,
) -> anyhow::Result<(usize, Saved)> {
    let rows = sqlx::query(
        "SELECT layer, id, source, valid, run, kind, west, south, east, north, \
                solid_west, solid_south, solid_east, solid_north, max_zoom FROM parts",
    )
    .fetch_all(pool)
    .await
    .context("loading saved parts")?;
    let mut remembered: Vec<(String, Part)> = Vec::with_capacity(rows.len());
    for row in rows {
        let kind: String = row.try_get("kind")?;
        let max_zoom: i16 = row.try_get("max_zoom")?;
        let solid = match (
            row.try_get::<Option<f64>, _>("solid_west")?,
            row.try_get::<Option<f64>, _>("solid_south")?,
            row.try_get::<Option<f64>, _>("solid_east")?,
            row.try_get::<Option<f64>, _>("solid_north")?,
        ) {
            (Some(w), Some(s), Some(e), Some(n)) => Some(LonLatBox::new(w, s, e, n)),
            _ => None,
        };
        remembered.push((
            row.try_get("layer")?,
            Part {
                id: row.try_get("id")?,
                source: row.try_get("source")?,
                valid: row.try_get::<DateTime<Utc>, _>("valid")?,
                run: row.try_get::<Option<DateTime<Utc>>, _>("run")?,
                kind: if kind == "observed" {
                    FrameKind::Observed
                } else {
                    FrameKind::Forecast
                },
                bounds: LonLatBox::new(
                    row.try_get("west")?,
                    row.try_get("south")?,
                    row.try_get("east")?,
                    row.try_get("north")?,
                ),
                solid,
                max_zoom: max_zoom.clamp(0, u8::MAX as i16) as u8,
            },
        ));
    }

    // One listing of the grids rather than a request per part: on S3 that
    // is a few dozen requests instead of thousands.
    let total = remembered.len();
    let stored: HashSet<String> = store
        .list(Some(&Path::from("grids")))
        .map_ok(|meta| meta.location.to_string())
        .try_collect()
        .await
        .context("listing the stored grids")?;
    let present: Vec<(String, Part)> = remembered
        .into_iter()
        .filter(|(layer, part)| stored.contains(&keys::grid(layer, &part.id)))
        .collect();
    if present.len() < total {
        tracing::warn!(
            missing = total - present.len(),
            "some remembered parts have no grid in the store; they will be rebuilt"
        );
    }
    catalog.restore(present);
    // Expire what went stale while the server was down. Nobody is listening
    // yet, so the announcements go nowhere.
    catalog.sweep(Utc::now());

    let mut saved = sqlx::query("SELECT layer, id FROM parts")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| Ok((r.try_get("layer")?, r.try_get("id")?)))
        .collect::<Result<Saved, sqlx::Error>>()?;
    sync(pool, catalog, &mut saved).await?;
    Ok((saved.len(), saved))
}

/// Make the table match the catalog, given what the table holds now.
pub async fn sync(pool: &PgPool, catalog: &FrameCatalog, saved: &mut Saved) -> anyhow::Result<()> {
    let held = catalog.parts();
    let mut wanted: HashMap<(String, String), &Part> = HashMap::with_capacity(held.len());
    for (layer, part) in &held {
        wanted.insert((layer.to_string(), part.id.clone()), part);
    }
    let missing: Vec<(&(String, String), &Part)> = wanted
        .iter()
        .filter(|(key, _)| !saved.contains(*key))
        .map(|(key, part)| (key, *part))
        .collect();
    let extra: Vec<(String, String)> = saved
        .iter()
        .filter(|key| !wanted.contains_key(*key))
        .cloned()
        .collect();
    if missing.is_empty() && extra.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    if !missing.is_empty() {
        // One statement with a column per array, whatever the count: the
        // same prepared statement every time.
        let col =
            |f: fn(&Part) -> Option<f64>| missing.iter().map(|(_, p)| f(p)).collect::<Vec<_>>();
        sqlx::query(
            "INSERT INTO parts (layer, id, source, valid, run, kind, west, south, east, north, \
             solid_west, solid_south, solid_east, solid_north, max_zoom) \
             SELECT * FROM UNNEST($1::text[], $2::text[], $3::text[], $4::timestamptz[], \
                 $5::timestamptz[], $6::text[], $7::float8[], $8::float8[], $9::float8[], \
                 $10::float8[], $11::float8[], $12::float8[], $13::float8[], $14::float8[], \
                 $15::int2[]) \
             ON CONFLICT (layer, id) DO NOTHING",
        )
        .bind(
            missing
                .iter()
                .map(|((layer, _), _)| layer.as_str())
                .collect::<Vec<_>>(),
        )
        .bind(
            missing
                .iter()
                .map(|(_, p)| p.id.as_str())
                .collect::<Vec<_>>(),
        )
        .bind(
            missing
                .iter()
                .map(|(_, p)| p.source.as_str())
                .collect::<Vec<_>>(),
        )
        .bind(missing.iter().map(|(_, p)| p.valid).collect::<Vec<_>>())
        .bind(missing.iter().map(|(_, p)| p.run).collect::<Vec<_>>())
        .bind(
            missing
                .iter()
                .map(|(_, p)| match p.kind {
                    FrameKind::Observed => "observed",
                    FrameKind::Forecast => "forecast",
                })
                .collect::<Vec<_>>(),
        )
        .bind(col(|p| Some(p.bounds.west)))
        .bind(col(|p| Some(p.bounds.south)))
        .bind(col(|p| Some(p.bounds.east)))
        .bind(col(|p| Some(p.bounds.north)))
        .bind(col(|p| p.solid.map(|s| s.west)))
        .bind(col(|p| p.solid.map(|s| s.south)))
        .bind(col(|p| p.solid.map(|s| s.east)))
        .bind(col(|p| p.solid.map(|s| s.north)))
        .bind(
            missing
                .iter()
                .map(|(_, p)| p.max_zoom as i16)
                .collect::<Vec<_>>(),
        )
        .execute(&mut *tx)
        .await?;
    }
    if !extra.is_empty() {
        let layers: Vec<&str> = extra.iter().map(|(l, _)| l.as_str()).collect();
        let ids: Vec<&str> = extra.iter().map(|(_, id)| id.as_str()).collect();
        sqlx::query(
            "DELETE FROM parts p USING UNNEST($1::text[], $2::text[]) AS gone(layer, id) \
             WHERE p.layer = gone.layer AND p.id = gone.id",
        )
        .bind(layers)
        .bind(ids)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let added: Vec<(String, String)> = missing.into_iter().map(|(key, _)| key.clone()).collect();
    saved.extend(added);
    for key in &extra {
        saved.remove(key);
    }
    Ok(())
}

/// Save catalog changes shortly after they happen.
pub fn spawn_mirror(
    pool: PgPool,
    catalog: Arc<FrameCatalog>,
    hub: Hub,
    mut saved: Saved,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = hub.subscribe();
        loop {
            // Wait for a timeline to change. Not every change to the catalog
            // shows on a timeline (a part waiting for its layer's anchor does
            // not), so also look every half minute. Falling behind the hub is
            // harmless because `sync` compares everything.
            let idle = tokio::time::sleep(Duration::from_secs(30));
            tokio::pin!(idle);
            loop {
                tokio::select! {
                    ev = rx.recv() => match ev {
                        Ok(ev) if matches!(*ev, WsEvent::Timeline { .. }) => break,
                        Ok(_) => {}
                        Err(RecvError::Lagged(_)) => break,
                        Err(RecvError::Closed) => return,
                    },
                    _ = &mut idle => break,
                    _ = cancel.cancelled() => {
                        if let Err(e) = sync(&pool, &catalog, &mut saved).await {
                            tracing::warn!("saving the catalog at shutdown failed: {e:#}");
                        }
                        return;
                    }
                }
            }
            // Parts arrive in bursts (a model run is hundreds); save once per burst.
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                _ = cancel.cancelled() => {}
            }
            rx = rx.resubscribe();
            if let Err(e) = sync(&pool, &catalog, &mut saved).await {
                tracing::warn!("saving the catalog failed: {e:#}");
            }
            if cancel.is_cancelled() {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;
    use futures::StreamExt;
    use object_store::ObjectStoreExt;
    use std::time::Instant;
    use wx_core::frame::publish_boxes;

    /// `n` forecast parts of every HRRR-fed layer, valid over the next hours.
    fn parts(n: usize) -> Vec<(&'static str, Part)> {
        let now = Utc::now();
        let run = now
            .with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap()
            - chrono::Duration::hours(1);
        let source = wx_core::source::source("hrrr").unwrap();
        let (bounds, solid) = publish_boxes(
            LonLatBox::new(-134.0, 21.0, -60.0, 53.0),
            Some(LonLatBox::new(-122.0, 26.0, -72.0, 47.0)),
        );
        let layers: Vec<&'static str> = wx_core::LAYERS
            .iter()
            .filter(|l| l.input_for(source).is_some() && !l.anchored)
            .map(|l| l.id)
            .collect();
        (0..n)
            .map(|i| {
                let lead = 120 + 15 * (i / layers.len()) as u32;
                let part = Part::forecast("hrrr", run, lead, bounds, solid, source.max_zoom);
                (layers[i % layers.len()], part)
            })
            .collect()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn the_table_follows_the_catalog_and_restores_what_the_store_has(pool: PgPool) {
        let catalog = FrameCatalog::new(Hub::new());
        let held = parts(40);
        for (layer, p) in &held {
            catalog.insert(layer, p.clone(), Utc::now()).unwrap();
        }
        let mut saved = Saved::new();
        sync(&pool, &catalog, &mut saved).await.unwrap();
        assert_eq!(saved.len(), 40);

        // Only half the grids are in the store: the rest are forgotten.
        let store: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
        for (layer, p) in held.iter().step_by(2) {
            store
                .put(&Path::from(keys::grid(layer, &p.id)), vec![1u8].into())
                .await
                .unwrap();
        }
        let fresh = FrameCatalog::new(Hub::new());
        let (count, saved) = restore(&pool, &fresh, &store).await.unwrap();
        assert_eq!((count, saved.len()), (20, 20));
        let mut back: Vec<String> = fresh.parts().into_iter().map(|(_, p)| p.id).collect();
        let mut want: Vec<String> = held.iter().step_by(2).map(|(_, p)| p.id.clone()).collect();
        back.sort();
        want.sort();
        assert_eq!(back, want);
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM parts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 20);
    }

    /// `cargo test --release -p wx-server sync_after_a_burst -- --ignored --nocapture`
    #[sqlx::test(migrations = "../../migrations")]
    #[ignore = "a measurement"]
    async fn sync_after_a_burst(pool: PgPool) {
        let catalog = FrameCatalog::new(Hub::new());
        let mut saved = Saved::new();
        for burst in [300, 3000] {
            for (layer, p) in parts(burst + saved.len()).into_iter().skip(saved.len()) {
                catalog.insert(layer, p, Utc::now()).unwrap();
            }
            let started = Instant::now();
            sync(&pool, &catalog, &mut saved).await.unwrap();
            let insert = started.elapsed();
            let started = Instant::now();
            sync(&pool, &catalog, &mut saved).await.unwrap();
            println!(
                "{burst} new parts ({} held): saved in {:.1} ms (one INSERT); a sync with nothing to do {:.1} ms",
                saved.len(),
                insert.as_secs_f64() * 1e3,
                started.elapsed().as_secs_f64() * 1e3
            );
        }
    }

    /// What checking remembered parts against the store costs: one `HEAD`
    /// per grid (as restores did) or one listing. Read-only.
    /// `STORE_DIR=data/store cargo test --release -p wx-server store_check_costs -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "a measurement on a real store"]
    async fn store_check_costs() {
        let dir = std::env::var("STORE_DIR").unwrap_or_else(|_| "../../data/store".into());
        let store: Arc<dyn ObjectStore> =
            Arc::new(object_store::local::LocalFileSystem::new_with_prefix(dir).unwrap());
        for _ in 0..3 {
            let started = Instant::now();
            let listed: Vec<Path> = store
                .list(Some(&Path::from("grids")))
                .map_ok(|m| m.location)
                .try_collect()
                .await
                .unwrap();
            let list = started.elapsed();
            let started = Instant::now();
            let found = futures::stream::iter(listed.iter())
                .map(|p| {
                    let store = store.clone();
                    async move { store.head(p).await.is_ok() }
                })
                .buffer_unordered(32)
                .filter(|ok| futures::future::ready(*ok))
                .count()
                .await;
            println!(
                "{} grids: one listing {:.0} ms, a HEAD each (32 at a time) {:.0} ms",
                found,
                list.as_secs_f64() * 1e3,
                started.elapsed().as_secs_f64() * 1e3
            );
        }
    }
}
