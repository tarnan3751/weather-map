//! One real pass over every source in the WMO register into a throwaway
//! database: each feed once, the new CAP documents they list (up to the
//! per-source budget), and NUTS boundaries from GISCO. Not part of the
//! normal run: it needs the network, sends a couple of thousand requests
//! spread over many hosts, and must not be looped.
//!
//! DATABASE_URL=postgres://wx:wx@127.0.0.1:5433/wx \
//!   cargo test -p wx-alerts --test live_world -- --ignored --nocapture

mod common;

use sqlx::PgPool;
use std::collections::BTreeMap;
use std::time::Instant;
use wx_alerts::{Alerts, AlertsConfig, WorldConfig};
use wx_core::Hub;

fn region_name(region: Option<u8>) -> &'static str {
    match region {
        Some(1) => "Africa",
        Some(2) => "Asia",
        Some(3) => "South America",
        Some(4) => "N/C America",
        Some(5) => "SW Pacific",
        Some(6) => "Europe",
        _ => "unknown",
    }
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "talks to about 150 hosts worldwide"]
async fn live_world_pass(pool: PgPool) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();
    let config = AlertsConfig {
        world: WorldConfig {
            user_agent: "weather-map-dev".to_string(),
            ..WorldConfig::default()
        },
        ..AlertsConfig::default()
    };
    let alerts = Alerts::new(pool.clone(), Hub::new(), config).unwrap();
    let started = Instant::now();
    let report = alerts.world_poll_once().await.unwrap();
    println!("pass took {:.1?}: {report:?}", started.elapsed());

    let sources = alerts.world_sources().await;
    // (source) -> (geom_source -> count) for active alerts.
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT source, geom_source, count(*) FROM alerts WHERE origin = 'cap' AND active GROUP BY 1, 2",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut by_source: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for (s, g, n) in rows {
        by_source.entry(s).or_default().insert(g, n);
    }

    #[derive(Default, Debug)]
    struct Region {
        sources: usize,
        with_alerts: usize,
        empty: usize,
        failing: usize,
        alerts: i64,
        drawn_cap: i64,
        drawn_nuts: i64,
        unshaped: i64,
        backlog: usize,
    }
    let mut regions: BTreeMap<&str, Region> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut unshaped_sources = Vec::new();
    for s in &sources {
        let r = regions.entry(region_name(s.region)).or_default();
        r.sources += 1;
        r.backlog += s.backlog;
        let counts = by_source.get(&s.id).cloned().unwrap_or_default();
        let total: i64 = counts.values().sum();
        match s.state {
            "failing" => {
                r.failing += 1;
                failures.push(format!(
                    "{} ({}): {}",
                    s.id,
                    s.feed,
                    s.last_error.clone().unwrap_or_default()
                ));
            }
            _ if total > 0 => r.with_alerts += 1,
            _ => r.empty += 1,
        }
        r.alerts += total;
        r.drawn_cap += counts.get("cap").copied().unwrap_or(0);
        r.drawn_nuts += counts.get("nuts").copied().unwrap_or(0);
        let none = counts.get("none").copied().unwrap_or(0);
        r.unshaped += none;
        if none > 0 {
            unshaped_sources.push(format!("{} {}/{} unshaped", s.id, none, total));
        }
    }
    println!(
        "\n{:<14} {:>7} {:>7} {:>6} {:>7} {:>7} {:>7} {:>6} {:>8} {:>8}",
        "region",
        "sources",
        "alerts",
        "empty",
        "failing",
        "active",
        "polygon",
        "nuts",
        "unshaped",
        "backlog"
    );
    for (name, r) in &regions {
        println!(
            "{name:<14} {:>7} {:>7} {:>6} {:>7} {:>7} {:>7} {:>6} {:>8} {:>8}",
            r.sources,
            r.with_alerts,
            r.empty,
            r.failing,
            r.alerts,
            r.drawn_cap,
            r.drawn_nuts,
            r.unshaped,
            r.backlog
        );
    }
    println!("\nfailing sources:");
    for f in &failures {
        println!("  {f}");
    }
    println!("\nsources with alerts that cannot be drawn:");
    for u in &unshaped_sources {
        println!("  {u}");
    }
    let codes: Vec<(String, i64)> = sqlx::query_as(
        "SELECT split_part(c, ':', 1), count(DISTINCT id) FROM alerts, unnest(region_codes) c
         WHERE origin = 'cap' AND active AND geom IS NULL GROUP BY 1 ORDER BY 2 DESC",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    println!("\ncode systems named by unshaped alerts: {codes:?}");
    let nuts: Vec<(i16, i64)> =
        sqlx::query_as("SELECT edition, count(*) FROM nuts_regions GROUP BY 1 ORDER BY 1")
            .fetch_all(&pool)
            .await
            .unwrap();
    println!("NUTS regions cached by edition: {nuts:?}");
    let docs: Vec<(String, i64)> = sqlx::query_as(
        "SELECT split_part(outcome, ':', 1), count(*) FROM cap_documents GROUP BY 1 ORDER BY 2 DESC",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    println!("documents by outcome: {docs:?}");
    let doc_errors: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT source, left(outcome, 90), count(*) FROM cap_documents
         WHERE outcome LIKE 'failed%' OR outcome LIKE 'invalid%' GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 25",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    println!("document failures by source:");
    for (s, o, n) in doc_errors {
        println!("  {s}: {n} x {o}");
    }
    let (bad, vertices): (i64, Option<i64>) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE NOT (ST_IsValid(geom) AND ST_IsPolygonCCW(geom) AND ST_XMin(geom) >= -180 AND ST_XMax(geom) <= 180)),
                sum(ST_NPoints(geom))::bigint
         FROM alerts WHERE origin = 'cap' AND geom IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    println!(
        "invalid shapes: {bad}; stored vertices: {}",
        vertices.unwrap_or(0)
    );
    let notify: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM alerts a WHERE origin = 'cap' AND active AND geom IS NOT NULL
           AND a.severity IN ('Severe', 'Extreme') AND coalesce(a.urgency, '') <> 'Past'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    println!("drawn alerts that would notify a saved place: {notify}");

    // What a map costs: the world tile at zoom 0, a few zoom-3 tiles, and
    // the default list.
    let base = common::serve(alerts.router()).await;
    let http = reqwest::Client::builder().no_gzip().build().unwrap();
    let rev = alerts.revision().await;
    for (z, x, y) in [(0, 0, 0), (3, 4, 2), (3, 2, 3), (3, 7, 4)] {
        let t = Instant::now();
        let tile = http
            .get(format!("{base}/api/alerts/tiles/{rev}/{z}/{x}/{y}.mvt"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let cold = t.elapsed();
        let t = Instant::now();
        http.get(format!("{base}/api/alerts/tiles/{rev}/{z}/{x}/{y}.mvt"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        println!(
            "tile {z}/{x}/{y}: {} bytes, {cold:.0?} cold, {:.0?} cached",
            tile.len(),
            t.elapsed()
        );
    }
    let t = Instant::now();
    let list = http
        .get(format!("{base}/api/alerts"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    println!("/api/alerts: {} bytes in {:.0?}", list.len(), t.elapsed());
    assert_eq!(bad, 0);
}
