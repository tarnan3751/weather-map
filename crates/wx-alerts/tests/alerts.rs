//! End-to-end tests against a throwaway PostGIS database (`#[sqlx::test]`
//! needs `DATABASE_URL`, e.g. postgres://wx:wx@127.0.0.1:5433/wx), a mock
//! api.weather.gov and a mock push service.

mod common;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use chrono::{Datelike, Duration as ChronoDuration, TimeZone, Utc};
use common::*;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::io::Read;
use std::time::Duration;

const NORMAN: (f64, f64) = (35.22, -97.44);

fn tornado(id: &str) -> AlertBuilder {
    alert(id, "Tornado Warning")
        .polygon(square(NORMAN.1, NORMAN.0, 0.1))
        .vtec("NEW", "KOUN", "TO", "W", "0042")
}

fn event_key(etn: &str) -> String {
    format!("KOUN.TO.W.{etn}.{}", Utc::now().year())
}

fn utc(y: i32, m: u32, d: u32, h: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
}

fn ids(of: &[&str]) -> Vec<String> {
    of.iter().map(|s| s.to_string()).collect()
}

/// Make a stored alert's end time pass, without waiting for it.
async fn end_now(pool: &PgPool, id: &str) {
    sqlx::query("UPDATE alerts SET ends = now() - interval '1 second', expires = now() - interval '1 second' WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

async fn keys_of(app: &App) -> Vec<(String, String)> {
    app.rows("SELECT id, event_key FROM alerts ORDER BY sent, id")
        .await
}

/// Store a zone boundary directly (no fetch).
async fn put_zone(pool: &PgPool, kind: &str, id: &str, ring: &[[f64; 2]], fetched_days_ago: i32) {
    let geometry = json!({ "type": "Polygon", "coordinates": [ring] }).to_string();
    sqlx::query(
        "INSERT INTO zones (id, kind, geom, fetched_at)
         VALUES ($1, $2, ST_Multi(ST_SetSRID(ST_GeomFromGeoJSON($3), 4326)), now() - make_interval(days => $4))",
    )
    .bind(id)
    .bind(kind)
    .bind(geometry)
    .bind(fetched_days_ago)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn polygon_warning_is_drawn_and_served(pool: PgPool) {
    let mut app = App::start(pool).await;
    app.nws.set_alerts(vec![tornado("urn:test:tor-1").build()]);

    let report = app.poll().await;
    assert!(report.feed_changed);
    assert_eq!((report.added, report.drawn, report.warnings), (1, 1, 1));
    assert_eq!(
        app.alert_events(),
        [(ids(&["urn:test:tor-1"]), vec![], vec![])]
    );
    assert_eq!(app.nws.user_agents()[0], "weather-map-test");
    assert_eq!(app.nws.accepts()[0], "application/geo+json");

    let resp = app.get("/api/alerts").await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "application/geo+json");
    let etag = resp.headers()["etag"].to_str().unwrap().to_string();
    let fc: Value = resp.json().await.unwrap();
    assert_eq!(fc["type"], "FeatureCollection");
    let f = &fc["features"][0];
    assert_eq!(f["id"], "urn:test:tor-1");
    assert_eq!(f["geometry"]["type"], "MultiPolygon");
    let p = &f["properties"];
    assert_eq!(p["event"], "Tornado Warning");
    assert_eq!(p["severity"], "Extreme");
    assert_eq!(p["urgency"], "Immediate");
    assert_eq!(p["certainty"], "Observed");
    assert_eq!(p["geom_source"], "polygon");
    assert_eq!(p["area_desc"], "Cleveland, OK; McClain, OK");
    assert!(p["ends"].as_str().unwrap().ends_with('Z'), "times are UTC");

    // Conditional and compressed requests.
    let conditional = app
        .http
        .get(app.url("/api/alerts"))
        .header("if-none-match", &etag);
    let resp = conditional.send().await.unwrap();
    assert_eq!(resp.status(), 304);
    assert_eq!(resp.headers()["etag"].to_str().unwrap(), etag);
    let raw = reqwest::Client::builder().no_gzip().build().unwrap();
    let resp = raw
        .get(app.url("/api/alerts"))
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["content-encoding"], "gzip");
    let gz = resp.bytes().await.unwrap();
    let mut json = String::new();
    flate2::read::GzDecoder::new(&gz[..])
        .read_to_string(&mut json)
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), fc);

    // Point queries.
    assert_eq!(app.covering(NORMAN.0, NORMAN.1).await, ["urn:test:tor-1"]);
    assert!(app.at(40.0, -100.0).await.is_empty());

    let detail = app.get_json("/api/alerts/detail?id=urn:test:tor-1").await;
    assert_eq!(detail["event"], "Tornado Warning");
    assert_eq!(detail["instruction"], "TAKE COVER NOW!");
    assert_eq!(detail["sender_name"], "NWS Norman OK");
    assert_eq!(detail["message_type"], "Alert");
    assert_eq!(detail["status"], "Actual");
    assert!(detail["description"].as_str().unwrap().contains("tornado"));
    let (status, err) = app
        .call("GET", "/api/alerts/detail?id=nope", "", Value::Null)
        .await;
    assert_eq!((status, &err["error"]), (404, &json!("not_found")));

    // Nothing changed: the mock answers 304, nothing is announced, and the
    // map snapshot keeps its ETag.
    assert!(!app.poll().await.feed_changed);
    assert_eq!(app.nws.not_modified_count(), 1);
    assert!(app.alert_events().is_empty());
    let resp = app.get("/api/alerts").await;
    assert_eq!(resp.headers()["etag"].to_str().unwrap(), etag);
}

#[sqlx::test(migrations = "../../migrations")]
async fn zone_alert_gets_its_shape_once_zones_arrive(pool: PgPool) {
    let mut app = App::start(pool.clone()).await;
    // Two neighbouring zones; the county one fails twice before answering.
    app.nws.set_zone(
        "forecast",
        "OKZ025",
        zone_feature("forecast", "OKZ025", &square(-97.5, 35.5, 0.25)),
    );
    app.nws.set_zone(
        "county",
        "OKC027",
        zone_feature("county", "OKC027", &square(-97.0, 35.5, 0.25)),
    );
    app.nws.fail_zone("county", "OKC027", &[503, 429]);
    let warning = |id: &str, action: &str| {
        alert(id, "Winter Storm Warning")
            .severity("Moderate")
            .zones(&[("forecast", "OKZ025"), ("county", "OKC027")])
            .vtec(action, "KOUN", "WS", "W", "0003")
    };
    app.nws
        .set_alerts(vec![warning("urn:test:wsw-1", "NEW").build()]);

    let running = app.spawn();
    eventually("zone alert drawn", || async {
        !app.features().await.is_empty()
    })
    .await;
    let features = app.features().await;
    assert_eq!(features.len(), 1);
    assert_eq!(features[0]["properties"]["geom_source"], "zones");
    assert_eq!(app.nws.hits("/zones/forecast/OKZ025"), 1);
    assert_eq!(
        app.nws.hits("/zones/county/OKC027"),
        3,
        "two retries, then success"
    );

    // The union covers both zones.
    for lon in [-97.6, -96.9] {
        assert_eq!(
            app.covering(35.5, lon).await,
            ["urn:test:wsw-1"],
            "lon {lon}"
        );
    }
    let zones: Vec<(String, String, String)> = app
        .rows("SELECT id, kind, ST_GeometryType(geom) FROM zones ORDER BY id")
        .await;
    let multi = |id: &str, kind: &str| {
        (
            id.to_string(),
            kind.to_string(),
            "ST_MultiPolygon".to_string(),
        )
    };
    assert_eq!(
        zones,
        [multi("OKC027", "county"), multi("OKZ025", "forecast")]
    );
    assert_eq!(
        running.health.get("alerts").unwrap().state,
        wx_core::FeedState::Healthy
    );
    let added: Vec<String> = app
        .alert_events()
        .into_iter()
        .flat_map(|(a, _, _)| a)
        .collect();
    assert_eq!(added, ["urn:test:wsw-1"]);

    // The next update names the same zones: no refetch, shape reused at once.
    app.nws.set_alerts(vec![
        warning("urn:test:wsw-2", "CON")
            .message_type("Update")
            .build(),
    ]);
    eventually("update drawn", || async {
        app.feature_ids()
            .await
            .contains(&"urn:test:wsw-2".to_string())
    })
    .await;
    assert_eq!(app.nws.zone_hits_total(), 4);
    assert_eq!(
        app.features().await.len(),
        1,
        "the superseded message left the feed"
    );
    running.stop().await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn map_shapes_are_light_but_point_queries_use_the_full_shape(pool: PgPool) {
    let app = App::start(pool).await;
    // A 4-degree zone with a 20,000-vertex boundary and small islands.
    let (cx, cy, r) = (-150.0, 58.0, 2.0);
    let n = 20_000;
    let outer: Vec<[f64; 2]> = (0..=n)
        .map(|i| {
            let a = std::f64::consts::TAU * (i % n) as f64 / n as f64;
            // A little wiggle so simplification has something to remove.
            let rr = r + 0.001 * (i as f64 * 0.7).sin();
            [cx + rr * a.cos(), cy + rr * a.sin()]
        })
        .collect();
    let mut rings = vec![outer];
    rings.extend((0..10).map(|i| square(cx - 1.0 + i as f64 * 0.2, cy, 0.01)));
    let mut zone = zone_feature("forecast", "PKZ999", &[]);
    zone["geometry"] = json!({ "type": "Polygon", "coordinates": rings });
    app.nws.set_zone("forecast", "PKZ999", zone);
    app.nws.set_alerts(vec![
        alert("urn:test:gale-1", "Gale Warning")
            .severity("Moderate")
            .zones(&[("forecast", "PKZ999")])
            .vtec("NEW", "PAFC", "GL", "W", "0101")
            .build(),
    ]);
    let running = app.spawn();
    eventually("gale warning drawn", || async {
        !app.features().await.is_empty()
    })
    .await;
    running.stop().await;

    let feature = &app.features().await[0];
    let polygons = feature["geometry"]["coordinates"].as_array().unwrap();
    assert!(
        polygons.iter().all(|p| p.as_array().unwrap().len() == 1),
        "islands smaller than a few cells are dropped"
    );
    let points: usize = polygons
        .iter()
        .map(|p| p[0].as_array().unwrap().len())
        .sum();
    assert!(
        (10..=1000).contains(&points),
        "{points} vertices on the map"
    );
    for c in polygons[0][0]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|xy| xy.as_array().unwrap())
    {
        let s = c.to_string();
        assert!(s.split_once('.').map_or(0, |(_, d)| d.len()) <= 3, "{s}");
    }
    // Inside an island: not covered, because queries use the stored shape.
    assert!(app.at(cy, cx - 1.0).await.is_empty());
    assert_eq!(app.covering(cy + 0.1, cx - 1.0).await, ["urn:test:gale-1"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn recorded_feed_end_to_end(pool: PgPool) {
    let feed: Value = serde_json::from_str(include_str!("fixtures/alerts_active.json")).unwrap();
    let recorded_now = Utc.with_ymd_and_hms(2026, 11, 6, 12, 30, 0).unwrap();
    let mut features = feed["features"].as_array().unwrap().clone();
    for f in &mut features {
        shift_times(f, recorded_now);
    }
    let app = App::start(pool.clone()).await;
    app.nws.set_alerts(features);
    // Two of the Red Flag Warning's five fire zones are recorded; the mock
    // answers 404 for every other zone.
    for id in ["CAZ369", "CAZ370"] {
        let path = format!(
            "{}/tests/fixtures/zone_fire_{id}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        app.nws.set_zone(
            "fire",
            id,
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap(),
        );
    }

    let report = app.poll().await;
    assert_eq!(
        report.feed_alerts, 6,
        "the KEEPALIVE test message is not Actual"
    );
    assert_eq!(report.drawn, 3, "the three alerts with their own polygon");

    let running = app.spawn();
    eventually(
        "red flag warning drawn from its known fire zones",
        || async {
            app.features()
                .await
                .iter()
                .any(|f| f["properties"]["event"] == "Red Flag Warning")
        },
    )
    .await;
    running.stop().await;

    let features = app.features().await;
    let mut events: Vec<&str> = features
        .iter()
        .map(|f| f["properties"]["event"].as_str().unwrap())
        .collect();
    events.sort();
    assert_eq!(
        events,
        [
            "Flash Flood Warning",
            "Flood Advisory",
            "Marine Weather Statement",
            "Red Flag Warning"
        ]
    );
    // Inside CAZ369 (Western Santa Monica Mountains).
    let at = app.at(34.08, -118.80).await;
    assert_eq!(
        (&at[0]["event"], &at[0]["geom_source"]),
        (&json!("Red Flag Warning"), &json!("zones"))
    );
    // The year in the key follows the (shifted) send time.
    let keys: Vec<String> = app
        .one("SELECT array_agg(event_key ORDER BY event_key) FROM alerts")
        .await;
    for prefix in [
        "KLOX.FW.W.0006.",
        "KPHI.FF.W.0158.",
        "KMQT.SC.Y.0169.",
        "PAJK.SR.W.4972.",
        "urn:oid:",
    ] {
        assert!(
            keys.iter().any(|k| k.starts_with(prefix)),
            "{prefix} in {keys:?}"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn warning_notifies_once_per_hazard_per_place(pool: PgPool) {
    let app = App::start(pool).await;
    let (token, sub) = app.device("phone").await;
    let place = app.save_place(&token, "Home", NORMAN.0, NORMAN.1).await;
    assert_eq!(place["alerts"], json!([]));

    let first_sent = Utc::now() - ChronoDuration::minutes(2);
    app.nws
        .set_alerts(vec![tornado("urn:test:tor-1").sent(first_sent).build()]);
    assert_eq!(app.poll().await.notifications, 1);
    eventually("push delivered", || async { app.push.count() == 1 }).await;

    let push = &app.push.received()[0];
    assert_eq!(push.path, "/push/phone");
    let h = |name: &str| {
        push.headers
            .get(name)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(h("content-encoding"), "aes128gcm");
    assert_eq!(h("content-type"), "application/octet-stream");
    assert_eq!(h("urgency"), "high");
    let ttl: i64 = h("ttl").parse().unwrap();
    assert!(
        (60..=45 * 60).contains(&ttl),
        "TTL runs until the warning ends: {ttl}"
    );
    let topic = h("topic");
    assert!(
        topic.len() <= 32
            && topic
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );

    // VAPID: a JWT for this push service's origin, signed by our key.
    let authz = h("authorization");
    let (jwt, k) = authz
        .strip_prefix("vapid t=")
        .unwrap()
        .split_once(", k=")
        .unwrap();
    let vapid = app.vapid.as_ref().unwrap();
    assert_eq!(k, vapid.public_key);
    let parts: Vec<&str> = jwt.split('.').collect();
    let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["aud"], app.push.base);
    assert_eq!(claims["sub"], vapid.subject);
    let exp = claims["exp"].as_i64().unwrap() - Utc::now().timestamp();
    assert!(exp > 0 && exp <= 3600, "short-lived token: {exp}s");
    let key = VerifyingKey::from_sec1_bytes(&B64.decode(k).unwrap()).unwrap();
    let signature = Signature::from_slice(&B64.decode(parts[2]).unwrap()).unwrap();
    key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .unwrap();

    // The body decrypts with the subscriber's keys.
    let payload = sub.open(push);
    assert_eq!(payload["title"], "Tornado Warning");
    let body = payload["body"].as_str().unwrap();
    assert!(
        body.starts_with("Home: Tornado Warning until ") && body.ends_with(" CST"),
        "{body}"
    );
    assert_eq!(payload["tag"], event_key("0042"));
    assert_eq!(payload["alert_id"], "urn:test:tor-1");
    assert_eq!(payload["place_id"], place["id"]);
    assert_eq!(payload["severity"], "Extreme");
    assert_eq!(
        payload["url"],
        "/?alert=urn%3Atest%3Ator-1&lat=35.22&lon=-97.44"
    );

    // The NWS re-issues the warning under a new id (a continuation, shape
    // nudged east) and splits off a second segment: same VTEC event.
    let con = |id: &str| {
        tornado(id)
            .vtec("CON", "KOUN", "TO", "W", "0042")
            .message_type("Update")
            .references("urn:test:tor-1", first_sent)
    };
    app.nws.set_alerts(vec![
        con("urn:test:tor-2")
            .polygon(square(NORMAN.1 + 0.02, NORMAN.0, 0.1))
            .build(),
        con("urn:test:tor-2b").build(),
    ]);
    let report = app.poll().await;
    assert_eq!(
        (report.added, report.removed, report.notifications),
        (2, 1, 0)
    );
    assert_eq!(
        app.push.count_after_a_while().await,
        1,
        "one hazard, one push"
    );
    let rows = app.notification_rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].0.as_str(), rows[0].2.as_str()),
        (event_key("0042").as_str(), "sent")
    );
    let (sent_at, last_success): (Option<chrono::DateTime<Utc>>, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as(
            "SELECT n.sent_at, s.last_success_at FROM notifications n, push_subscriptions s",
        )
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert!(sent_at.is_some() && last_success.is_some());

    // A different event number is a different hazard.
    app.nws.set_alerts(vec![
        tornado("urn:test:tor-3")
            .vtec("NEW", "KOUN", "TO", "W", "0043")
            .build(),
    ]);
    app.poll().await;
    eventually("second hazard pushed", || async { app.push.count() == 2 }).await;
    let pushes = app.push.received();
    assert_eq!(sub.open(&pushes[1])["tag"], event_key("0043"));
    assert_ne!(pushes[1].headers["topic"], pushes[0].headers["topic"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn push_service_answers_are_handled(pool: PgPool) {
    let app = App::start(pool).await;
    let (token, _) = app.device("flaky").await;
    app.save_place(&token, "Home", NORMAN.0, NORMAN.1).await;

    // 429, then 503, then accepted: retried and delivered.
    app.push.script(&[429, 503]);
    app.nws.set_alerts(vec![tornado("urn:test:tor-1").build()]);
    app.poll().await;
    eventually("delivered after retries", || async {
        app.notification_rows()
            .await
            .first()
            .is_some_and(|r| r.2 == "sent")
    })
    .await;
    assert_eq!(app.push.count(), 3);

    // 410 Gone: the subscription is deleted.
    app.push.script(&[410]);
    app.nws.set_alerts(vec![
        tornado("urn:test:tor-9")
            .vtec("NEW", "KOUN", "TO", "W", "0099")
            .build(),
    ]);
    app.poll().await;
    eventually("gone subscription removed", || async {
        app.one::<i64>("SELECT count(*) FROM push_subscriptions")
            .await
            == 0
    })
    .await;
    // With its only subscription gone the device has nowhere to receive it:
    // waiting for a new subscription, not failed.
    eventually("notification waiting for a subscription", || async {
        app.notification_rows()
            .await
            .iter()
            .any(|r| r.2 == "no_subscription")
    })
    .await;
    let rows = app.notification_rows().await;
    assert!(
        rows[1].3.as_deref().unwrap().contains("expired"),
        "{rows:?}"
    );

    // Subscribing again delivers the warning that was waiting.
    let picky = Subscriber::new(app.push.endpoint("picky"));
    app.subscribe(&token, &picky).await;
    eventually("waiting warning delivered", || async {
        app.notification_rows().await.iter().all(|r| r.2 == "sent")
    })
    .await;
    let redelivered = app.push.received().pop().unwrap();
    assert_eq!(redelivered.path, "/push/picky");
    assert_eq!(picky.open(&redelivered)["alert_id"], "urn:test:tor-9");

    // A 400 counts against the subscription but keeps it.
    let test_push = || app.call("POST", "/api/push/test", &token, Value::Null);
    let failures = || app.one::<i32>("SELECT failure_count FROM push_subscriptions");
    app.push.script(&[400]);
    assert_eq!(test_push().await.1, json!({ "sent": 0, "failed": 1 }));
    assert_eq!(failures().await, 1);
    assert_eq!(test_push().await.1, json!({ "sent": 1, "failed": 0 }));
    let last = app.push.received().pop().unwrap();
    assert_eq!(picky.open(&last)["title"], "Test notification");
    let failures = failures().await;
    assert_eq!(failures, 0, "consecutive failures reset on success");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_push_service_outage_is_retried_later_and_after_a_restart(pool: PgPool) {
    let app = App::start(pool.clone()).await;
    let (token, sub) = app.device("phone").await;
    app.save_place(&token, "Home", NORMAN.0, NORMAN.1).await;

    // Every attempt fails with 503: the row stays pending, not failed.
    app.push.script(&[503, 503, 503]);
    app.nws.set_alerts(vec![tornado("urn:test:tor-1").build()]);
    app.poll().await;
    eventually("delivery attempted", || async {
        let rows = app.notification_rows().await;
        rows.first()
            .is_some_and(|r| r.3.as_deref().is_some_and(|e| e.starts_with("will retry")))
    })
    .await;
    assert_eq!(app.notification_rows().await[0].2, "pending");
    assert_eq!(app.push.count(), 3);

    // Meanwhile the NWS re-issues the warning under a new id.
    app.nws.set_alerts(vec![
        tornado("urn:test:tor-2")
            .vtec("CON", "KOUN", "TO", "W", "0042")
            .message_type("Update")
            .references("urn:test:tor-1", Utc::now() - ChronoDuration::minutes(3))
            .build(),
    ]);
    let report = app.poll().await;
    assert_eq!(
        (report.added, report.removed, report.notifications),
        (1, 1, 0)
    );

    // A restarted server sweeps up the pending row once it is a little old,
    // and delivers the warning as it now stands.
    app.exec("UPDATE notifications SET created_at = now() - interval '3 minutes'")
        .await;
    let restarted = app.restarted();
    restarted.poll_once().await.unwrap();
    eventually("redelivered", || async {
        app.notification_rows().await[0].2 == "sent"
    })
    .await;
    assert_eq!(app.push.count(), 4);
    let payload = sub.open(&app.push.received()[3]);
    assert_eq!(payload["alert_id"], "urn:test:tor-2");
    assert_eq!(payload["tag"], event_key("0042"));
    let rows = app.notification_rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "urn:test:tor-2");

    // Had the warning ended instead, nothing would be sent.
    app.push.script(&[503, 503, 503]);
    app.nws.set_alerts(vec![
        tornado("urn:test:tor-7")
            .vtec("NEW", "KOUN", "TO", "W", "0077")
            .build(),
    ]);
    restarted.poll_once().await.unwrap();
    let state_of_7 = |status: &'static str| {
        let app = &app;
        move || async move {
            app.notification_rows()
                .await
                .iter()
                .any(|r| r.1 == "urn:test:tor-7" && r.2 == status)
        }
    };
    eventually("second hazard pending", state_of_7("pending")).await;
    app.nws.set_alerts(vec![]);
    restarted.poll_once().await.unwrap();
    app.exec("UPDATE notifications SET created_at = now() - interval '3 minutes' WHERE alert_id = 'urn:test:tor-7'")
        .await;
    app.restarted().poll_once().await.unwrap();
    eventually("ended hazard given up", state_of_7("failed")).await;
    assert_eq!(
        app.push.count(),
        7,
        "three failed tries, no delivery after the warning ended"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn alerts_that_leave_the_feed_or_end_are_removed_and_announced(pool: PgPool) {
    let mut app = App::start(pool.clone()).await;
    let storm = alert("urn:test:svr-1", "Severe Thunderstorm Warning")
        .severity("Severe")
        .polygon(square(-98.0, 36.0, 0.1))
        .vtec("NEW", "KOUN", "SV", "W", "0100")
        .build();
    app.nws
        .set_alerts(vec![tornado("urn:test:tor-1").build(), storm.clone()]);
    app.poll().await;
    assert_eq!(app.features().await.len(), 2);
    app.alert_events();

    // The tornado warning leaves the feed.
    app.nws.set_alerts(vec![storm]);
    assert_eq!(app.poll().await.removed, 1);
    assert_eq!(
        app.alert_events(),
        [(vec![], vec![], ids(&["urn:test:tor-1"]))]
    );
    assert_eq!(app.feature_ids().await, ["urn:test:svr-1"]);
    let (active, ended): (bool, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as("SELECT active, ended_at FROM alerts WHERE id = 'urn:test:tor-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!active && ended.is_some());

    // Still in the (unchanged) feed, but its end time passes. Moved in the
    // database rather than waited for, so machine load cannot change which
    // poll sees it end.
    end_now(&pool, "urn:test:svr-1").await;
    assert!(!app.poll().await.feed_changed);
    assert_eq!(
        app.alert_events(),
        [(vec![], vec![], ids(&["urn:test:svr-1"]))]
    );
    assert!(app.features().await.is_empty());

    // Back in the feed (the API's servers disagree now and then).
    app.nws.set_alerts(vec![tornado("urn:test:tor-1").build()]);
    app.poll().await;
    assert_eq!(
        app.alert_events(),
        [(ids(&["urn:test:tor-1"]), vec![], vec![])]
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_alert_the_database_refuses_is_skipped_and_the_others_stored(pool: PgPool) {
    let app = App::start(pool.clone()).await;
    app.exec(
        "CREATE FUNCTION refuse_bad() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN IF NEW.id = 'urn:test:bad' THEN RAISE EXCEPTION 'refused'; END IF; RETURN NEW; END $$",
    )
    .await;
    app.exec("CREATE TRIGGER refuse_bad BEFORE INSERT OR UPDATE ON alerts FOR EACH ROW EXECUTE FUNCTION refuse_bad()")
        .await;
    app.nws.set_alerts(vec![
        tornado("urn:test:tor-1").build(),
        tornado("urn:test:bad")
            .vtec("NEW", "KOUN", "TO", "W", "0043")
            .build(),
        tornado("urn:test:tor-2")
            .vtec("NEW", "KOUN", "TO", "W", "0044")
            .build(),
    ]);
    let report = app.poll().await;
    assert_eq!((report.feed_alerts, report.added), (3, 2));
    let mut drawn = app.feature_ids().await;
    drawn.sort();
    assert_eq!(drawn, ["urn:test:tor-1", "urn:test:tor-2"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn saving_a_place_inside_an_active_warning_notifies_at_once(pool: PgPool) {
    let app = App::start(pool).await;
    app.nws.set_alerts(vec![
        tornado("urn:test:tor-1").build(),
        alert("urn:test:adv-1", "Wind Advisory")
            .severity("Minor")
            .polygon(square(NORMAN.1, NORMAN.0, 0.5))
            .vtec("NEW", "KOUN", "WI", "Y", "0007")
            .build(),
    ]);
    app.poll().await;
    let (token, sub) = app.device("phone").await;

    let place = app
        .save_place(&token, "Campus", NORMAN.0 + 0.01, NORMAN.1)
        .await;
    let covering: Vec<&str> = place["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        covering,
        ["Tornado Warning", "Wind Advisory"],
        "most severe first"
    );
    assert_eq!(place["alerts"][0]["severity"], "Extreme");
    // No poll in between: the place itself triggers the match.
    eventually("push for the new place", || async { app.push.count() == 1 }).await;
    let payload = sub.open(&app.push.received()[0]);
    assert_eq!(
        payload["title"], "Tornado Warning",
        "advisories do not notify"
    );
    assert!(payload["body"].as_str().unwrap().starts_with("Campus: "));

    // Toggling notify off and on does not notify the same hazard again.
    let path = |place: &Value| format!("/api/places/{}", place["id"].as_str().unwrap());
    for notify in [false, true] {
        let (status, patched) = app
            .call("PATCH", &path(&place), &token, json!({ "notify": notify }))
            .await;
        assert_eq!((status, &patched["notify"]), (200, &json!(notify)));
    }
    // A place saved with notify off is matched when it is switched on.
    let office =
        json!({ "name": "Office", "lat": NORMAN.0 - 0.01, "lon": NORMAN.1, "notify": false });
    let (_, office) = app.call("POST", "/api/places", &token, office).await;
    assert_eq!(app.push.count_after_a_while().await, 1);
    app.call("PATCH", &path(&office), &token, json!({ "notify": true }))
        .await;
    eventually("push once notify is on", || async { app.push.count() == 2 }).await;

    // Outside every warning: nothing.
    app.save_place(&token, "Far away", 40.0, -100.0).await;
    assert_eq!(app.push.count_after_a_while().await, 2);
    assert_eq!(app.notification_rows().await.len(), 2);
}

/// Permission granted a while after the place was saved, even on day two of
/// a long warning.
#[sqlx::test(migrations = "../../migrations")]
async fn subscribing_after_saving_a_place_still_delivers_the_current_warning(pool: PgPool) {
    let app = App::start(pool).await;
    app.nws.set_alerts(vec![tornado("urn:test:tor-1").build()]);
    app.poll().await;
    let token = app.register().await;
    app.save_place(&token, "Home", NORMAN.0, NORMAN.1).await;
    eventually("recorded without a subscription", || async {
        app.notification_rows()
            .await
            .first()
            .is_some_and(|r| r.2 == "no_subscription")
    })
    .await;
    app.exec("UPDATE notifications SET created_at = now() - interval '2 days'")
        .await;

    let sub = Subscriber::new(app.push.endpoint("phone"));
    app.subscribe(&token, &sub).await;
    eventually("push after subscribing", || async { app.push.count() == 1 }).await;
    assert_eq!(
        sub.open(&app.push.received()[0])["title"],
        "Tornado Warning"
    );
    eventually("marked sent", || async {
        app.notification_rows().await[0].2 == "sent"
    })
    .await;
    // Subscribing again does not resend.
    app.subscribe(&token, &sub).await;
    assert_eq!(app.push.count_after_a_while().await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn ended_or_cancelled_warnings_never_notify(pool: PgPool) {
    let app = App::start(pool).await;
    let (token, _) = app.device("phone").await;
    app.save_place(&token, "Home", NORMAN.0, NORMAN.1).await;

    let ended = Utc::now() - ChronoDuration::minutes(1);
    app.nws.set_alerts(vec![
        tornado("urn:test:can")
            .vtec("CAN", "KOUN", "TO", "W", "0042")
            .message_type("Update")
            .build(),
        tornado("urn:test:cancel")
            .vtec("CON", "KOUN", "TO", "W", "0050")
            .message_type("Cancel")
            .build(),
        tornado("urn:test:over")
            .vtec("NEW", "KOUN", "TO", "W", "0051")
            .ends(ended)
            .build(),
        tornado("urn:test:drill")
            .vtec("NEW", "KOUN", "TO", "W", "0052")
            .status("Exercise")
            .build(),
    ]);
    let report = app.poll().await;
    assert_eq!(
        (report.feed_alerts, report.drawn, report.notifications),
        (3, 0, 0)
    );
    assert!(app.features().await.is_empty());
    assert!(app.at(NORMAN.0, NORMAN.1).await.is_empty());
    // A place saved now does not pick them up either.
    app.save_place(&token, "Campus", NORMAN.0, NORMAN.1).await;
    assert_eq!(app.push.count_after_a_while().await, 0);
    assert!(app.notification_rows().await.is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_cancellation_retires_the_message_it_references(pool: PgPool) {
    let mut app = App::start(pool).await;
    let original = tornado("urn:test:tor-1").build();
    app.nws.set_alerts(vec![original.clone()]);
    app.poll().await;
    assert_eq!(app.features().await.len(), 1);
    app.alert_events();

    // The cancellation arrives while the API still lists the original.
    let sent = Utc::now() - ChronoDuration::minutes(1);
    app.nws.set_alerts(vec![
        original,
        tornado("urn:test:tor-1-can")
            .vtec("CAN", "KOUN", "TO", "W", "0042")
            .message_type("Update")
            .references("urn:test:tor-1", sent)
            .build(),
    ]);
    app.poll().await;
    assert_eq!(
        app.alert_events(),
        [(vec![], vec![], ids(&["urn:test:tor-1"]))]
    );
    assert!(app.features().await.is_empty());

    // So a place saved now is not told about a cancelled warning.
    let (token, _) = app.device("phone").await;
    let place = app.save_place(&token, "Home", NORMAN.0, NORMAN.1).await;
    assert_eq!(place["alerts"], json!([]));
    assert_eq!(app.push.count_after_a_while().await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn without_vapid_keys_notifications_are_recorded_not_sent(pool: PgPool) {
    let app = App::start_with(pool, |c| c.vapid = None).await;
    let (subscribed, _) = app.device("phone").await;
    app.save_place(&subscribed, "Home", NORMAN.0, NORMAN.1)
        .await;
    let silent = app.register().await;
    app.save_place(&silent, "Home", NORMAN.0, NORMAN.1).await;

    app.nws.set_alerts(vec![tornado("urn:test:tor-1").build()]);
    assert_eq!(app.poll().await.notifications, 2);
    eventually("both recorded", || async {
        app.notification_rows()
            .await
            .iter()
            .all(|r| r.2 != "pending")
    })
    .await;
    let mut statuses: Vec<(String, Option<String>)> = app
        .notification_rows()
        .await
        .into_iter()
        .map(|r| (r.2, r.3))
        .collect();
    statuses.sort();
    assert_eq!(statuses[0].0, "failed");
    assert!(statuses[0].1.as_deref().unwrap().contains("not configured"));
    assert_eq!(statuses[1], ("no_subscription".to_string(), None));
    assert_eq!(app.push.count(), 0);

    let (status, err) = app
        .call("POST", "/api/push/test", &subscribed, Value::Null)
        .await;
    assert_eq!(
        (status, &err["error"]),
        (503, &json!("push_not_configured"))
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn device_and_place_contract(pool: PgPool) {
    let app = App::start_with(pool.clone(), |c| c.allow_insecure_push_endpoints = false).await;

    let (_, reg) = app.call("POST", "/api/device", "", Value::Null).await;
    let token = reg["token"].as_str().unwrap().to_string();
    assert_eq!(B64.decode(&token).unwrap().len(), 32);
    uuid::Uuid::parse_str(reg["device_id"].as_str().unwrap()).unwrap();
    let stored: Vec<u8> = app.one("SELECT token_hash FROM devices").await;
    let hash = Sha256::digest(token.as_bytes()).to_vec();
    assert_eq!(stored, hash, "only the hash is kept");

    // Authentication.
    let unknown = "A".repeat(43);
    for auth in ["", "nope", &unknown] {
        let (status, body) = app.call("GET", "/api/places", auth, Value::Null).await;
        assert_eq!((status, &body["error"]), (401, &json!("unauthorized")));
        assert!(body["message"].is_string());
    }

    // Validation, all in the error shape.
    let place = |body: Value| app.call("POST", "/api/places", &token, body);
    for (body, code) in [
        (
            json!({ "name": "Home", "lat": 91.0, "lon": 0.0 }),
            "invalid_coordinates",
        ),
        (
            json!({ "name": "Home", "lat": 0.0, "lon": -181.0 }),
            "invalid_coordinates",
        ),
        (
            json!({ "name": "  ", "lat": 0.0, "lon": 0.0 }),
            "invalid_name",
        ),
        (
            json!({ "name": "x".repeat(81), "lat": 0.0, "lon": 0.0 }),
            "invalid_name",
        ),
        (json!({ "name": "Home", "lat": "north" }), "invalid_body"),
    ] {
        let (status, err) = place(body).await;
        assert_eq!(err["error"], code);
        assert!(status == 400 || status == 422, "{status}");
    }
    let not_json = app.http.post(app.url("/api/places")).bearer_auth(&token);
    let not_json = not_json
        .header("content-type", "application/json")
        .body("{not json");
    let resp = not_json.send().await.unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(resp.json::<Value>().await.unwrap()["error"], "invalid_body");

    // Twenty places, then 409.
    let mut places = Vec::new();
    for i in 0..20 {
        let lat = 35.0 + i as f64 * 0.01;
        let p = app
            .save_place(&token, &format!("Place {i}"), lat, -97.0)
            .await;
        assert_eq!((&p["notify"], &p["lat"]), (&json!(true), &json!(lat)));
        assert!(p["created_at"].as_str().unwrap().ends_with('Z'));
        places.push(format!("/api/places/{}", p["id"].as_str().unwrap()));
    }
    let (status, err) = place(json!({ "name": "One too many", "lat": 0.0, "lon": 0.0 })).await;
    assert_eq!((status, &err["error"]), (409, &json!("too_many_places")));

    let (_, list) = app.call("GET", "/api/places", &token, Value::Null).await;
    let names: Vec<&str> = list["places"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!((names.len(), names[0]), (20, "Place 0"));

    // Rename, and another device cannot touch it.
    let (status, renamed) = app
        .call("PATCH", &places[0], &token, json!({ "name": " Home " }))
        .await;
    assert_eq!((status, &renamed["name"]), (200, &json!("Home")));
    let other = app.register().await;
    for (method, path, body) in [
        ("PATCH", places[0].as_str(), json!({ "name": "Mine" })),
        ("DELETE", &places[0], Value::Null),
        ("DELETE", "/api/places/not-a-uuid", Value::Null),
    ] {
        let (status, err) = app.call(method, path, &other, body).await;
        assert_eq!((status, &err["error"]), (404, &json!("not_found")));
    }
    for status in [204, 404] {
        assert_eq!(
            app.call("DELETE", &places[0], &token, Value::Null).await.0,
            status
        );
    }

    // Push subscriptions must point at an https push service.
    let sub = Subscriber::new("https://push.example.com/wpush/v2/abc".to_string());
    let mut insecure = sub.json();
    insecure["endpoint"] = json!("http://push.example.com/wpush/v2/abc");
    let mut bad_keys = sub.json();
    bad_keys["keys"]["auth"] = json!("c2hvcnQ");
    let no_keys = json!({ "endpoint": "https://push.example.com/x" });
    for body in [insecure, bad_keys, no_keys] {
        let (status, err) = app
            .call("POST", "/api/push/subscription", &token, body)
            .await;
        assert!(status == 400 || status == 422, "{status}");
        let err = &err["error"];
        assert!(
            err == "invalid_subscription" || err == "invalid_body",
            "{err}"
        );
    }
    let subscriptions = || app.one::<i64>("SELECT count(*) FROM push_subscriptions");
    app.subscribe(&token, &sub).await;
    app.subscribe(&token, &sub).await; // idempotent
    assert_eq!(subscriptions().await, 1);
    let gone = json!({ "endpoint": sub.endpoint });
    assert_eq!(
        app.call("DELETE", "/api/push/subscription", &token, gone)
            .await
            .0,
        204
    );
    assert_eq!(subscriptions().await, 0);
    let (_, sent) = app
        .call("POST", "/api/push/test", &token, Value::Null)
        .await;
    assert_eq!(sent, json!({ "sent": 0, "failed": 0 }));

    // Point queries are validated too.
    for q in ["lat=abc&lon=1", "lat=91&lon=0", "lon=1"] {
        let (status, err) = app
            .call("GET", &format!("/api/alerts/at?{q}"), "", Value::Null)
            .await;
        assert_eq!(status, 400, "{q}");
        let err = &err["error"];
        assert!(
            err == "invalid_query" || err == "invalid_coordinates",
            "{err}"
        );
    }
}

/// A hazard keeps its key across the new year and across updates of
/// warnings without VTEC, even though `references` reach back only one
/// message (or none).
#[sqlx::test(migrations = "../../migrations")]
async fn a_hazard_keeps_its_key_across_new_year_and_updates_without_vtec(pool: PgPool) {
    let app = App::start(pool.clone()).await;
    let (token, _) = app.device("phone").await;
    app.save_place(&token, "Home", NORMAN.0, NORMAN.1).await;
    let notifications = |messages: Vec<Value>| {
        let app = &app;
        async move {
            let mut created = Vec::new();
            for message in messages {
                app.nws.set_alerts(vec![message]);
                created.push(app.poll().await.notifications);
            }
            created
        }
    };
    let flood = |id: &str| {
        alert(id, "Flood Warning")
            .severity("Severe")
            .polygon(square(NORMAN.1, NORMAN.0, 0.1))
    };
    let con = |id: &str| {
        flood(id)
            .vtec("CON", "KOUN", "FL", "W", "0045")
            .message_type("Update")
    };

    // Issued on New Year's Eve and continued into January; each update
    // references only the one before, and the last references nothing.
    let chain = vec![
        flood("urn:test:fl-1")
            .vtec("NEW", "KOUN", "FL", "W", "0045")
            .sent(utc(2025, 12, 31, 23))
            .build(),
        con("urn:test:fl-2")
            .sent(utc(2026, 1, 1, 10))
            .references("urn:test:fl-1", utc(2025, 12, 31, 23))
            .build(),
        con("urn:test:fl-3")
            .sent(utc(2026, 1, 2, 10))
            .references("urn:test:fl-2", utc(2026, 1, 1, 10))
            .build(),
        con("urn:test:fl-4").sent(utc(2026, 1, 3, 10)).build(),
    ];
    assert_eq!(
        notifications(chain).await,
        [1, 0, 0, 0],
        "one hazard, one notification"
    );

    // No VTEC: every update has a new id and references the one before.
    let t0 = Utc::now() - ChronoDuration::minutes(30);
    let at = |minutes: i64| t0 + ChronoDuration::minutes(minutes);
    let civil = |id: &str, minutes: i64| {
        alert(id, "Civil Danger Warning")
            .polygon(square(NORMAN.1, NORMAN.0, 0.2))
            .no_vtec()
            .sent(at(minutes))
    };
    let updates = vec![
        civil("urn:test:cd-1", 0).build(),
        civil("urn:test:cd-2", 10)
            .message_type("Update")
            .references("urn:test:cd-1", at(0))
            .build(),
        civil("urn:test:cd-3", 20)
            .message_type("Update")
            .references("urn:test:cd-2", at(10))
            .build(),
    ];
    assert_eq!(notifications(updates).await, [1, 0, 0]);

    for (id, key) in keys_of(&app).await {
        let want = if id.starts_with("urn:test:fl-") {
            "KOUN.FL.W.0045.2025"
        } else {
            "urn:test:cd-1"
        };
        assert_eq!(key, want, "{id}");
    }
    assert_eq!(app.notification_rows().await.len(), 2);
}

/// Several VTEC strings: the alert is about its live event.
#[sqlx::test(migrations = "../../migrations")]
async fn an_upgraded_watch_and_an_advisory_turned_warning_are_warnings(pool: PgPool) {
    let app = App::start(pool.clone()).await;
    let (token, _) = app.device("phone").await;
    app.save_place(&token, "Home", NORMAN.0, NORMAN.1).await;
    let year = Utc::now().year() % 100;
    let shape = square(NORMAN.1, NORMAN.0, 0.3);

    let watch_sent = Utc::now() - ChronoDuration::hours(6);
    app.nws.set_alerts(vec![
        alert("urn:test:wsa", "Winter Storm Watch")
            .severity("Moderate")
            .polygon(shape.clone())
            .vtec("NEW", "KOUN", "WS", "A", "0003")
            .sent(watch_sent)
            .build(),
    ]);
    assert_eq!(app.poll().await.notifications, 0, "watches do not notify");

    // The upgrade references the watch it replaces.
    let warning = |id: &str, shape: Vec<[f64; 2]>| {
        alert(id, "Winter Storm Warning")
            .severity("Severe")
            .polygon(shape)
    };
    app.nws.set_alerts(vec![
        warning("urn:test:wsw", shape.clone())
            .vtec_strings(&[
                &format!("/O.UPG.KOUN.WS.A.0003.{year}1107T0000Z-{year}1107T1200Z/"),
                &format!("/O.NEW.KOUN.WS.W.0005.{year}1106T1800Z-{year}1107T1200Z/"),
            ])
            .references("urn:test:wsa", watch_sent)
            .build(),
        warning("urn:test:wsw-adv", square(-100.0, 40.0, 0.3))
            .vtec_strings(&[
                &format!("/O.EXP.KGLD.WW.Y.0010.000000T0000Z-{year}1106T1800Z/"),
                &format!("/O.NEW.KGLD.WS.W.0002.{year}1106T1800Z-{year}1107T1200Z/"),
            ])
            .build(),
    ]);
    assert_eq!(app.poll().await.notifications, 1, "the warning notifies");
    let drawn = app.feature_ids().await;
    assert_eq!(drawn.len(), 2, "both warnings are drawn: {drawn:?}");
    let keys: std::collections::HashMap<String, String> = keys_of(&app).await.into_iter().collect();
    assert!(
        keys["urn:test:wsw"].starts_with("KOUN.WS.W.0005."),
        "{keys:?}"
    );
    assert!(
        keys["urn:test:wsw-adv"].starts_with("KGLD.WS.W.0002."),
        "{keys:?}"
    );
}

/// A warning with no stated end does not borrow `expires` as its end.
#[sqlx::test(migrations = "../../migrations")]
async fn an_open_ended_warning_is_until_further_notice(pool: PgPool) {
    let app = App::start(pool).await;
    let (token, sub) = app.device("phone").await;
    app.save_place(&token, "Home", NORMAN.0, NORMAN.1).await;
    app.nws.set_alerts(vec![
        alert("urn:test:river", "Flood Warning")
            .severity("Moderate")
            .polygon(square(NORMAN.1, NORMAN.0, 0.1))
            .vtec("CON", "KOUN", "FL", "W", "0012")
            .open_ended(Utc::now() + ChronoDuration::hours(3))
            .build(),
    ]);
    app.poll().await;
    eventually("push delivered", || async { app.push.count() == 1 }).await;
    assert_eq!(
        sub.open(&app.push.received()[0])["body"],
        "Home: Flood Warning until further notice"
    );
}

/// While the feed cannot be fetched (an outage, or a pause asked for by
/// api.weather.gov), ended warnings still leave the map.
#[sqlx::test(migrations = "../../migrations")]
async fn ended_warnings_leave_the_map_while_the_feed_is_unavailable(pool: PgPool) {
    let mut app = App::start(pool.clone()).await;
    app.nws.set_alerts(vec![
        tornado("urn:test:tor-1").build(),
        tornado("urn:test:tor-2")
            .polygon(square(-98.0, 36.0, 0.1))
            .vtec("NEW", "KOUN", "TO", "W", "0043")
            .build(),
    ]);
    app.poll().await;
    app.alert_events();

    // Down.
    app.nws.fail_alerts(Some((503, None)));
    end_now(&pool, "urn:test:tor-1").await;
    let err = app.alerts.poll_once().await.unwrap_err();
    assert!(format!("{err:#}").contains("503"), "{err:#}");
    assert_eq!(
        app.alert_events(),
        [(vec![], vec![], ids(&["urn:test:tor-1"]))]
    );
    assert_eq!(app.feature_ids().await, ["urn:test:tor-2"]);

    // A 429 pauses every api.weather.gov caller, for as long as it says.
    app.nws.fail_alerts(Some((429, Some(7))));
    app.alerts.poll_once().await.unwrap_err();
    let left = app.gate.paused_for().expect("the shared gate is paused");
    assert!(
        left > Duration::from_secs(5) && left <= Duration::from_secs(7),
        "{left:?}"
    );

    // Paused: nothing is sent, the local work still runs.
    let asked = app.nws.hits("/alerts/active");
    end_now(&pool, "urn:test:tor-2").await;
    let err = app.alerts.poll_once().await.unwrap_err();
    assert!(format!("{err:#}").contains("back off"), "{err:#}");
    assert_eq!(
        app.nws.hits("/alerts/active"),
        asked,
        "nothing sent while paused"
    );
    assert_eq!(
        app.alert_events(),
        [(vec![], vec![], ids(&["urn:test:tor-2"]))]
    );
    assert!(app.features().await.is_empty());
}

/// Pending pushes are retried while the feed is down, and a CDN block
/// pauses everyone for a minute.
#[sqlx::test(migrations = "../../migrations")]
async fn pushes_are_retried_while_the_feed_is_down(pool: PgPool) {
    let app = App::start(pool.clone()).await;
    let (token, _) = app.device("phone").await;
    app.save_place(&token, "Home", NORMAN.0, NORMAN.1).await;
    app.push.script(&[503, 503, 503]);
    app.nws.set_alerts(vec![tornado("urn:test:tor-1").build()]);
    app.poll().await;
    eventually("left pending", || async {
        app.notification_rows()
            .await
            .first()
            .is_some_and(|r| r.2 == "pending" && r.3.is_some())
    })
    .await;
    app.exec("UPDATE notifications SET created_at = now() - interval '3 minutes'")
        .await;

    // The CDN refuses us; a restarted server cannot fetch the feed, but its
    // sweep still delivers.
    app.nws.fail_alerts(Some((403, None)));
    let restarted = app.restarted();
    let err = restarted.poll_once().await.unwrap_err();
    assert!(format!("{err:#}").contains("403"), "{err:#}");
    assert!(
        app.gate.paused_for().unwrap() > Duration::from_secs(55),
        "a 403 pauses for a minute"
    );
    eventually("delivered during the outage", || async {
        app.notification_rows().await[0].2 == "sent"
    })
    .await;
    let asked = app.nws.hits("/alerts/active");
    restarted.poll_once().await.unwrap_err();
    assert_eq!(
        app.nws.hits("/alerts/active"),
        asked,
        "paused: not asked again"
    );
}

/// Zone workers wait while api.weather.gov has asked us to back off.
#[sqlx::test(migrations = "../../migrations")]
async fn zone_fetches_wait_while_the_gate_is_paused(pool: PgPool) {
    let app = App::start(pool).await;
    app.nws.set_zone(
        "forecast",
        "OKZ025",
        zone_feature("forecast", "OKZ025", &square(-97.5, 35.5, 0.25)),
    );
    app.nws.set_alerts(vec![
        alert("urn:test:wsw-1", "Winter Storm Warning")
            .zones(&[("forecast", "OKZ025")])
            .vtec("NEW", "KOUN", "WS", "W", "0003")
            .build(),
    ]);
    // Stored and its zone queued, then everyone is told to back off.
    app.poll().await;
    app.gate.pause(Duration::from_millis(1500));
    let running = app.spawn();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        app.nws.hits("/zones/forecast/OKZ025"),
        0,
        "nothing sent while paused"
    );
    eventually("zone fetched once the pause is over", || async {
        !app.features().await.is_empty()
    })
    .await;
    running.stop().await;
}

/// A zone that keeps failing does not keep its alert off the map; the
/// shape grows when it arrives, and places in it are then notified.
#[sqlx::test(migrations = "../../migrations")]
async fn a_zone_that_keeps_failing_does_not_hold_its_alert_back(pool: PgPool) {
    let mut app = App::start(pool).await;
    let (token, _) = app.device("phone").await;
    // West zone fine; east zone keeps timing out at the server.
    app.nws.set_zone(
        "forecast",
        "OKZ025",
        zone_feature("forecast", "OKZ025", &square(-97.5, 35.5, 0.25)),
    );
    app.nws.set_zone(
        "county",
        "OKC027",
        zone_feature("county", "OKC027", &square(-97.0, 35.5, 0.25)),
    );
    app.nws.fail_zone("county", "OKC027", &[504; 400]);
    app.save_place(&token, "East", 35.5, -96.9).await;
    app.nws.set_alerts(vec![
        alert("urn:test:wsw-1", "Winter Storm Warning")
            .zones(&[("forecast", "OKZ025"), ("county", "OKC027")])
            .vtec("NEW", "KOUN", "WS", "W", "0003")
            .build(),
    ]);
    let running = app.spawn();
    eventually("drawn from the zone that is known", || async {
        app.covering(35.5, -97.6).await == ["urn:test:wsw-1"]
    })
    .await;
    assert!(app.at(35.5, -96.9).await.is_empty());
    assert!(
        app.notification_rows().await.is_empty(),
        "the place is outside the shape so far"
    );

    // The county comes back.
    app.nws.fail_zone("county", "OKC027", &[]);
    eventually("shape completed", || async {
        app.covering(35.5, -96.9).await == ["urn:test:wsw-1"]
    })
    .await;
    eventually("the place in it is notified", || async {
        app.push.count() == 1
    })
    .await;
    // Phones are told before maps: the event follows the snapshot rebuild.
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !events.iter().any(
        |(_, updated, _): &(Vec<String>, Vec<String>, Vec<String>)| {
            updated.contains(&"urn:test:wsw-1".to_string())
        },
    ) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no update announced: {events:?}"
        );
        events.extend(app.alert_events());
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    running.stop().await;
    assert_eq!(
        events[0].0,
        ["urn:test:wsw-1"],
        "first drawn from the known zone: {events:?}"
    );
    assert!(
        events.iter().all(|(_, _, removed)| removed.is_empty()),
        "never announced as removed: {events:?}"
    );
}

/// Retired zones (404 for ever) do not stop the others being refreshed.
#[sqlx::test(migrations = "../../migrations")]
async fn zone_refresh_does_not_stall_behind_retired_zones(pool: PgPool) {
    let app = App::start(pool.clone()).await;
    for n in 1..=50 {
        put_zone(
            &pool,
            "forecast",
            &format!("AAZ{n:03}"),
            &square(-100.0, 40.0, 0.1),
            200,
        )
        .await;
    }
    for n in 51..=55 {
        let id = format!("AAZ{n:03}");
        put_zone(&pool, "forecast", &id, &square(-100.0, 40.0, 0.1), 100).await;
        app.nws.set_zone(
            "forecast",
            &id,
            zone_feature("forecast", &id, &square(-101.0, 41.0, 0.1)),
        );
    }
    let retired_tried_once =
        || (1..=50).all(|n| app.nws.hits(&format!("/zones/forecast/AAZ{n:03}")) == 1);

    // Two server lifetimes: the first sweep takes the fifty oldest, which
    // are all retired.
    let running = app.spawn();
    eventually("the retired zones were tried", || async {
        retired_tried_once()
    })
    .await;
    running.stop().await;
    let running = Running::start(&app.restarted(), &app.hub);
    eventually("the other zones were refreshed", || async {
        app.one::<i64>("SELECT count(*) FROM zones WHERE fetched_at > now() - interval '1 day'")
            .await
            == 5
    })
    .await;
    running.stop().await;
    assert!(retired_tried_once(), "retired zones not retried at once");
}

/// A subscription that arrived while a delivery was being decided (written
/// here behind the API's back) is found by the sweep.
#[sqlx::test(migrations = "../../migrations")]
async fn a_warning_waiting_for_a_subscription_is_delivered_by_the_sweep(pool: PgPool) {
    let app = App::start(pool.clone()).await;
    app.nws.set_alerts(vec![tornado("urn:test:tor-1").build()]);
    app.poll().await;
    let racer = app.register().await;
    app.save_place(&racer, "Dorm", NORMAN.0 - 0.01, NORMAN.1)
        .await;
    eventually("recorded without a subscription", || async {
        app.notification_rows()
            .await
            .iter()
            .any(|r| r.2 == "no_subscription")
    })
    .await;
    let keys = Subscriber::new(app.push.endpoint("racer")).json();
    sqlx::query(
        "INSERT INTO push_subscriptions (device_id, endpoint, p256dh, auth)
         SELECT p.device_id, $1, $2, $3 FROM places p WHERE p.name = 'Dorm'",
    )
    .bind(keys["endpoint"].as_str().unwrap())
    .bind(keys["keys"]["p256dh"].as_str().unwrap())
    .bind(keys["keys"]["auth"].as_str().unwrap())
    .execute(&pool)
    .await
    .unwrap();
    app.restarted().poll_once().await.unwrap();
    eventually("delivered by the sweep", || async {
        app.push.received().iter().any(|p| p.path == "/push/racer")
    })
    .await;
    eventually("all sent", || async {
        app.notification_rows().await.iter().all(|r| r.2 == "sent")
    })
    .await;
}

/// A same-id update that changes a zone alert's zones is an update, and its
/// new shape is matched against places at once.
#[sqlx::test(migrations = "../../migrations")]
async fn a_same_id_update_that_changes_the_zones_is_an_update(pool: PgPool) {
    let mut app = App::start(pool.clone()).await;
    put_zone(&pool, "forecast", "OKZ025", &square(-97.5, 35.5, 0.25), 0).await;
    put_zone(&pool, "county", "OKC027", &square(-97.0, 35.5, 0.25), 0).await;
    let (token, _) = app.device("phone").await;
    app.save_place(&token, "East", 35.5, -96.9).await;

    let warning = |zones: &[(&str, &str)]| {
        alert("urn:test:wsw-1", "Winter Storm Warning")
            .zones(zones)
            .vtec("NEW", "KOUN", "WS", "W", "0003")
            .build()
    };
    app.nws.set_alerts(vec![warning(&[("forecast", "OKZ025")])]);
    app.poll().await;
    assert_eq!(
        app.alert_events(),
        [(ids(&["urn:test:wsw-1"]), vec![], vec![])]
    );

    // Same id, a county added.
    app.nws.set_alerts(vec![warning(&[
        ("forecast", "OKZ025"),
        ("county", "OKC027"),
    ])]);
    let report = app.poll().await;
    assert_eq!(
        app.alert_events(),
        [(vec![], ids(&["urn:test:wsw-1"]), vec![])]
    );
    assert_eq!((report.added, report.updated, report.removed), (0, 1, 0));
    assert_eq!(
        report.notifications, 1,
        "the place in the new county, in the same cycle"
    );
    assert_eq!(app.covering(35.5, -96.9).await, ["urn:test:wsw-1"]);
}
