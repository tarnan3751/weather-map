//! A real poll of api.weather.gov into a throwaway database. Not part of
//! the normal run (it needs the network and takes a minute or two):
//!
//! DATABASE_URL=postgres://wx:wx@127.0.0.1:5433/wx \
//!   cargo test -p wx-alerts --test live -- --ignored --nocapture

mod common;

use sqlx::PgPool;
use std::time::{Duration, Instant};
use wx_alerts::{Alerts, AlertsConfig};
use wx_core::{HealthRegistry, Hub};

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "talks to api.weather.gov"]
async fn live_poll_cycle(pool: PgPool) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_test_writer()
        .try_init();
    let config = AlertsConfig {
        user_agent: "weather-map-dev".to_string(),
        world: wx_alerts::WorldConfig {
            enabled: false,
            ..Default::default()
        },
        ..AlertsConfig::default()
    };
    let hub = Hub::new();
    let alerts = Alerts::new(pool.clone(), hub.clone(), config).unwrap();

    let started = Instant::now();
    let report = alerts.poll_once().await.unwrap();
    let first_cycle = started.elapsed();
    let (polygon, zoned): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE geom_source = 'polygon'),
                count(*) FILTER (WHERE geom_source <> 'polygon' AND cardinality(ugc) > 0)
         FROM alerts WHERE active",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let wanted: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT z) FROM alerts, unnest(ugc) z WHERE active AND geom_source <> 'polygon'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    println!("first cycle: {first_cycle:.2?}");
    println!(
        "feed: {} Actual alerts ({} skipped); {} with their own polygon, {} named by zones ({} distinct zones)",
        report.feed_alerts, report.skipped, polygon, zoned, wanted
    );

    // Let the zone workers and the shaper run until the backlog is gone.
    let health = HealthRegistry::new(hub);
    let cancel = tokio_util::sync::CancellationToken::new();
    let handles = alerts.spawn(&health, cancel.clone());
    let zones_started = Instant::now();
    let deadline = Duration::from_secs(20 * 60);
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let backlog = alerts.zone_backlog();
        let awaiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM alerts WHERE active AND geom IS NULL AND cardinality(ugc) > 0",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        if backlog == 0 {
            // One more shaper pass after the last zone landed.
            tokio::time::sleep(Duration::from_secs(2)).await;
            break;
        }
        if zones_started.elapsed() > deadline {
            println!("gave up waiting: {backlog} zones still queued, {awaiting} alerts awaiting");
            break;
        }
        if zones_started.elapsed().as_secs() % 20 < 2 {
            println!(
                "  {:>4.0?}: {backlog} zones to fetch, {awaiting} alerts awaiting",
                zones_started.elapsed()
            );
        }
    }
    let zone_time = zones_started.elapsed();
    cancel.cancel();
    for h in handles {
        h.await.unwrap();
    }

    let (stored, vertices, largest): (i64, Option<i64>, Option<i32>) = sqlx::query_as(
        "SELECT count(*), sum(ST_NPoints(geom))::bigint, max(ST_NPoints(geom)) FROM zones",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let (drawn, zones_drawn, still_waiting): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE geom IS NOT NULL),
                count(*) FILTER (WHERE geom_source = 'zones'),
                count(*) FILTER (WHERE geom IS NULL AND cardinality(ugc) > 0)
         FROM alerts WHERE active",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    println!(
        "zones: {stored} of {wanted} stored in {zone_time:.1?} ({} vertices, largest {}); {} alerts drawn ({} from zones), {} without a shape",
        vertices.unwrap_or(0),
        largest.unwrap_or(0),
        drawn,
        zones_drawn,
        still_waiting
    );

    // The payload every map downloads. A fresh instance on the same
    // database has nothing cached, so its first request times a cold build.
    let cold = Alerts::new(
        pool.clone(),
        Hub::new(),
        AlertsConfig {
            user_agent: "weather-map-dev".into(),
            world: wx_alerts::WorldConfig {
                enabled: false,
                ..Default::default()
            },
            ..AlertsConfig::default()
        },
    )
    .unwrap();
    let base = common::serve(cold.router()).await;
    let plain = reqwest::Client::builder().no_gzip().build().unwrap();
    let build = Instant::now();
    let json = plain
        .get(format!("{base}/api/alerts"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let cold_build = build.elapsed();
    let build = Instant::now();
    let gz = plain
        .get(format!("{base}/api/alerts"))
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let warm = build.elapsed();
    let full: Option<i64> = sqlx::query_scalar(
        "SELECT sum(length(ST_AsGeoJSON(geom, 6)))::bigint FROM alerts WHERE active AND geom IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let fc: serde_json::Value = serde_json::from_slice(&json).unwrap();
    println!(
        "/api/alerts: {} features, {} bytes ({} gzipped); cold build {cold_build:.2?}, cached {warm:.2?}; stored shapes as GeoJSON would be {} bytes",
        fc["features"].as_array().unwrap().len(),
        json.len(),
        gz.len(),
        full.unwrap_or(0)
    );
    let (bad_alerts, bad_zones): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM alerts WHERE geom IS NOT NULL AND NOT (ST_IsValid(geom) AND ST_SRID(geom) = 4326)),
                (SELECT count(*) FROM zones WHERE NOT (ST_IsValid(geom) AND ST_SRID(geom) = 4326))",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    println!("invalid stored shapes: {bad_alerts} alerts, {bad_zones} zones");
    let by_event: Vec<(String, i64)> = sqlx::query_as(
        "SELECT event, count(*) FROM alerts WHERE active AND geom IS NOT NULL GROUP BY event ORDER BY 2 DESC LIMIT 6",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    println!("most drawn: {by_event:?}");
    assert_eq!((bad_alerts, bad_zones), (0, 0));
}
