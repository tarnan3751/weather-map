//! Checks against the real services. Run with:
//! `cargo test -p wx-obs --test live -- --ignored --nocapture`

use serde_json::Value;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use wx_core::{FeedState, HealthRegistry, Hub};
use wx_obs::{Obs, ObsConfig};

#[tokio::test]
#[ignore = "talks to the internet"]
async fn live_services() {
    let hub = Hub::new();
    let config = ObsConfig {
        user_agent: "weather-map-dev".into(),
        ..Default::default()
    };
    let obs = Obs::new(hub.clone(), config, None).unwrap();
    let health = HealthRegistry::new(hub);
    let cancel = CancellationToken::new();
    obs.spawn(&health, cancel.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = obs.router();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::new();
    let get = |path: &str| http.get(format!("{base}{path}")).send();

    for name in ["stations", "stations-ndbc", "stations-amedas"] {
        for _ in 0..120 {
            if health
                .get(name)
                .is_some_and(|h| h.state == FeedState::Healthy)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let h = health.get(name);
        println!("{name}: {:?}", h.map(|h| (h.state, h.detail, h.last_error)));
    }
    let body = get("/api/stations").await.unwrap().bytes().await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let feats = json["features"].as_array().unwrap();
    let mut by_kind = std::collections::BTreeMap::new();
    for p in feats.iter().map(|f| &f["properties"]) {
        *by_kind
            .entry((p["kind"].as_str().unwrap(), p["source"].as_str().unwrap()))
            .or_insert(0) += 1;
    }
    println!(
        "stations: {} features, {} bytes, by (kind, source): {by_kind:?}",
        feats.len(),
        body.len()
    );
    println!("sources: {}", json["sources"]);
    let view = get("/api/stations?bbox=-130,24,-66,50")
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    println!("conus view: {} bytes", view.len());
    let pacific: Value = get("/api/stations?bbox=165,40,-150,70")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    println!(
        "antimeridian view: {} stations",
        pacific["features"].as_array().unwrap().len()
    );

    // MET Norway only: a handful of calls, never the NWS from here.
    for (lat, lon) in [(-33.87, 151.21), (59.91, 10.75)] {
        let started = Instant::now();
        let r = get(&format!("/api/point?lat={lat}&lon={lon}"))
            .await
            .unwrap();
        let status = r.status();
        let v: Value = r.json().await.unwrap();
        println!("point {lat},{lon}: {status} in {:?}", started.elapsed());
        if status.is_success() {
            let n = |k: &str| v[k].as_array().map_or(0, Vec::len);
            println!(
                "  source={} hourly={} daily={}",
                v["source"],
                n("hourly"),
                n("daily")
            );
            println!(
                "  hourly[0]={}\n  daily[0]={}",
                v["hourly"][0], v["daily"][0]
            );
        } else {
            println!("  {v}");
        }
    }
    cancel.cancel();
}
