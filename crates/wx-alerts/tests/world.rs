//! The world path end to end, against a throwaway PostGIS database
//! (`DATABASE_URL`, e.g. postgres://wx:wx@127.0.0.1:5433/wx), one local
//! server standing in for every feed host, the WMO register and GISCO, and
//! the mock push service. Real CAP messages from several countries are
//! replayed with their times moved to now.

mod common;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use common::world::*;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::time::Duration;

/// `(id, active, geom_source, language)` of the alerts from `source`.
async fn alerts_of(pool: &PgPool, source: &str) -> Vec<(String, bool, String, String)> {
    sqlx::query_as(
        "SELECT id, active, geom_source, language FROM alerts WHERE source = $1 ORDER BY sent, id",
    )
    .bind(source)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn covered(pool: &PgPool, id: &str, lat: f64, lon: f64) -> bool {
    sqlx::query_scalar(
        "SELECT ST_Covers(geom, ST_SetSRID(ST_MakePoint($3, $2), 4326)) FROM alerts WHERE id = $1",
    )
    .bind(id)
    .bind(lat)
    .bind(lon)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `(cap_identifier, event_key, active)` of every world message, by sent.
async fn threads(app: &App) -> Vec<(String, String, bool)> {
    app.rows("SELECT cap_identifier, event_key, active FROM alerts WHERE origin = 'cap' ORDER BY sent, cap_identifier")
        .await
}

/// The identifiers of the world messages in effect.
async fn in_effect(app: &App) -> Vec<String> {
    app.one("SELECT coalesce(array_agg(cap_identifier ORDER BY cap_identifier), '{}') FROM alerts WHERE origin = 'cap' AND active")
        .await
}

async fn id_of(pool: &PgPool, identifier: &str) -> String {
    sqlx::query_scalar("SELECT id FROM alerts WHERE cap_identifier = $1")
        .bind(identifier)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The properties of each feature of a tile, by alert id.
async fn tile(app: &App, path: &str) -> HashMap<String, HashMap<String, MvtValue>> {
    let body = app.get(path).await.bytes().await.unwrap();
    let layers = decode_mvt(&body);
    let Some(layer) = layers.first() else {
        return HashMap::new();
    };
    layer
        .features
        .iter()
        .map(|f| {
            let p = layer.props(f);
            let MvtValue::String(id) = p["id"].clone() else {
                panic!()
            };
            (id, p)
        })
        .collect()
}

#[sqlx::test(migrations = "../../migrations")]
async fn one_pass_reads_every_kind_of_source(pool: PgPool) {
    let mock = MockWorld::start().await;
    // Canada (NAADS-style Atom: an English and a French entry per message,
    // and a test message that must not even be fetched).
    let ca1 = mock.url("/naads/ca1.cap");
    let ca_test = mock.url("/naads/test.cap");
    mock.put_feed_plain(
        "/naads/feed.atom",
        format!(
            r#"<feed xmlns="http://www.w3.org/2005/Atom">
              <entry><link href="{ca_test}" rel="alternate"/><category term="status=Test"/></entry>
              <entry><link href="{ca1}" rel="alternate"/><category term="status=Actual"/><category term="language=en-CA"/></entry>
              <entry><link href="{ca1}" rel="alternate"/><category term="status=Actual"/><category term="language=fr-CA"/></entry>
            </feed>"#
        ),
    );
    mock.put_cap("/naads/ca1.cap", current_fixture("ca_cwhx_update_1.cap"));
    mock.put_cap("/naads/test.cap", "never fetched");
    // Niue (S3 mirror RSS): a current circle and an item that expired weeks ago.
    mock.put_feed(
        "/s3/nu/rss.xml",
        rss(&[mock.url("/s3/nu/drought.xml"), mock.url("/s3/nu/wind.xml")]),
    );
    mock.put_cap(
        "/s3/nu/drought.xml",
        current_fixture("nu_drought_circle.xml"),
    );
    mock.put_cap("/s3/nu/wind.xml", fixture("nu_wind_stale.xml"));
    // Croatia and Bulgaria through MeteoAlarm: NUTS 3 codes only.
    let hr = mock.url("/ma/warnings/hr1");
    mock.put_feed_plain(
        "/feeds/meteoalarm-legacy-atom-croatia",
        meteoalarm_atom(&[(hr.clone(), "HR033"), (hr, "HR033")]),
    );
    mock.put_cap(
        "/ma/warnings/hr1",
        current_fixture("hr_meteoalarm_nuts.xml"),
    );
    let bg = mock.url("/ma/warnings/bg1");
    mock.put_feed_plain(
        "/feeds/meteoalarm-legacy-atom-bulgaria",
        meteoalarm_atom(&[(bg, "BG332")]),
    );
    mock.put_cap(
        "/ma/warnings/bg1",
        current_fixture("bg_meteoalarm_nuts.xml"),
    );
    // GISCO: Zadarska only in the 2021 edition, Dobrich in 2024.
    let gisco = |code: &str, edition: u16| {
        format!("/nuts/distribution/{code}-region-01m-4326-{edition}.geojson")
    };
    mock.put_json(
        &gisco("HR033", 2021),
        &nuts_region("HR033", "Zadarska", 15.6, 44.1, 0.4),
    );
    mock.put_json(
        &gisco("BG332", 2024),
        &nuts_region("BG332", "Dobrich", 27.8, 43.6, 0.4),
    );
    // Italy: EMMA regions, for which there is no public boundary source;
    // Germany: a polygon with a part cut out of it; Algeria: French, English
    // and Arabic blocks; Sri Lanka: CAP 1.1.
    for (dir, name) in [
        ("it", "it_meteoalarm_emma_update.xml"),
        ("dwd", "de_dwd_exclude.xml"),
        ("dz", "dz_three_languages.xml"),
        ("lk", "lk_cap11.xml"),
    ] {
        serve_feed(&mock, dir, &[("1", current_fixture(name))]);
    }
    // Fiji-like: a polygon and a circle across the antimeridian.
    let mut fj = Cap::alert("FJ-1");
    fj.sender = "met@fiji.example";
    fj.polygons = vec![vec![
        (-17.0, 179.0),
        (-17.0, -179.0),
        (-18.5, -179.0),
        (-18.5, 179.0),
        (-17.0, 179.0),
    ]];
    fj.circles = vec![(-16.0, 179.95, 30.0)];
    serve_feed(&mock, "fj", &[("1", fj.xml())]);
    // A host that is down, and a US source that must be left to the NWS path.
    mock.fail("/down/rss.xml", 500);
    let us = RegSource {
        feed: "https://api.weather.gov/alerts/active.atom?region_type=land".into(),
        ..source(&mock, "us-noaa-nws-en", "us", "/never")
    };
    let sources = [
        source(&mock, "ca-msc-xx", "ca", "/naads/feed.atom"),
        source(&mock, "nu-gov-en", "nu", "/s3/nu/rss.xml"),
        source(
            &mock,
            "hr-meteo-hr",
            "hr",
            "/feeds/meteoalarm-legacy-atom-croatia",
        ),
        source(
            &mock,
            "bg-meteo-bg",
            "bg",
            "/feeds/meteoalarm-legacy-atom-bulgaria",
        ),
        source(&mock, "it-meteoam-it", "it", "/it/rss.xml"),
        source(&mock, "de-dwd-en", "de", "/dwd/rss.xml"),
        source(&mock, "dz-onm-fr", "dz", "/dz/rss.xml"),
        source(&mock, "lk-meteo-en", "lk", "/lk/rss.xml"),
        source(&mock, "fj-met-en", "fj", "/fj/rss.xml"),
        source(&mock, "xx-down", "xx", "/down/rss.xml"),
        us,
    ];
    let app = world_app(pool.clone(), &mock, &sources).await;
    let report = app.world_poll().await;
    assert_eq!(
        report.sources, 24,
        "ten sources, fourteen fillers, and not the US one"
    );
    assert!(report.failed >= 1, "{report:?}");
    assert!(mock.user_agents().iter().all(|ua| ua == "weather-map-test"));
    assert_eq!(
        mock.hits("/naads/test.cap"),
        0,
        "test messages are not fetched"
    );
    assert_eq!(
        mock.hits("/naads/ca1.cap"),
        1,
        "one message, fetched once for its two entries"
    );

    // Canada: drawn from its polygons, English shown, both languages kept.
    let ca = alerts_of(&pool, "ca-msc-xx").await;
    assert_eq!(ca.len(), 1);
    let (ca_id, active, geom_source, language) = &ca[0];
    assert!(active);
    assert_eq!((geom_source.as_str(), language.as_str()), ("cap", "en-CA"));
    assert!(ca_id.starts_with("cap:"));
    assert!(covered(&pool, ca_id, 44.4873, -65.6050).await);
    let detail = app
        .get_json(&format!("/api/alerts/detail?id={ca_id}"))
        .await;
    let langs: Vec<&str> = detail["infos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["language"].as_str().unwrap())
        .collect();
    assert_eq!(langs, ["en-CA", "fr-CA"]);
    assert_eq!(detail["source"], "ca-msc-xx");
    assert_eq!(detail["source_name"], "ca-msc-xx");
    assert_eq!(detail["country"], "ca");
    assert_eq!(detail["message_type"], "Update");
    assert!(detail["infos"][1]["description"].as_str().unwrap().len() > 20);

    // Niue: a true circle on the ground; the stale item is not stored.
    let nu = alerts_of(&pool, "nu-gov-en").await;
    assert_eq!(nu.len(), 1, "{nu:?}");
    let (area_km2,): (f64,) = app
        .one_for(
            "SELECT ST_Area(geom::geography) / 1e6 FROM alerts WHERE id = $1",
            &nu[0].0,
        )
        .await;
    let want = std::f64::consts::PI * 14.0 * 14.0;
    assert!(
        (area_km2 - want).abs() / want < 0.01,
        "{area_km2} km2, wanted {want}"
    );
    let outcome: String = app
        .one("SELECT outcome FROM cap_documents WHERE url LIKE '%/s3/nu/wind.xml'")
        .await;
    assert_eq!(outcome, "expired");

    // MeteoAlarm: shapes from NUTS regions, the older edition where needed.
    for (src, code, lat, lon) in [
        ("hr-meteo-hr", "HR033", 44.1, 15.6),
        ("bg-meteo-bg", "BG332", 43.6, 27.8),
    ] {
        let a = alerts_of(&pool, src).await;
        assert_eq!(a.len(), 1, "{src}");
        assert_eq!(
            (a[0].2.as_str(), a[0].3.as_str()),
            ("nuts", "en-GB"),
            "{src}"
        );
        assert!(covered(&pool, &a[0].0, lat, lon).await, "{src} {code}");
    }
    let editions: Vec<(String, i16)> = app
        .rows("SELECT code, edition FROM nuts_regions ORDER BY code")
        .await;
    assert_eq!(
        editions,
        [("BG332".to_string(), 2024), ("HR033".to_string(), 2021)]
    );

    // Italy: listed for its country, not drawn.
    let it = alerts_of(&pool, "it-meteoam-it").await;
    assert_eq!((it.len(), it[0].2.as_str()), (1, "none"));
    let listed = app.get_json("/api/alerts?country=it").await;
    assert_eq!(listed["features"].as_array().unwrap().len(), 1);
    assert_eq!(listed["features"][0]["geometry"], Value::Null);
    assert_eq!(
        listed["features"][0]["properties"]["anchor"],
        json!([15.0, 45.0])
    );
    assert!(
        app.features()
            .await
            .iter()
            .all(|f| f["properties"]["source"] != "it-meteoam-it")
    );

    // Germany: covered where the polygon is, not where it was cut out.
    let de = alerts_of(&pool, "de-dwd-en").await;
    assert!(covered(&pool, &de[0].0, 50.004966, 9.977398).await);
    assert!(
        !covered(&pool, &de[0].0, 50.2341265, 9.882277).await,
        "the excluded part"
    );

    // Algeria shows English; Sri Lanka's CAP 1.1 reads like 1.2.
    assert_eq!(alerts_of(&pool, "dz-onm-fr").await[0].3, "en-US");
    assert!(
        covered(
            &pool,
            &alerts_of(&pool, "lk-meteo-en").await[0].0,
            7.727295,
            81.300479
        )
        .await
    );

    // Across the antimeridian: split at 180 degrees, both sides covered.
    let fj = &alerts_of(&pool, "fj-met-en").await[0].0;
    let (parts, xmin, xmax): (i32, f64, f64) = app
        .one_for(
            "SELECT ST_NumGeometries(geom), ST_XMin(geom), ST_XMax(geom) FROM alerts WHERE id = $1",
            fj,
        )
        .await;
    assert!(parts >= 2, "{parts} parts");
    assert!(xmin >= -180.0 && xmax <= 180.0, "{xmin}..{xmax}");
    for (lat, lon) in [
        (-17.5, 179.5),
        (-17.5, -179.5),
        (-16.0, 179.95),
        (-16.0, -179.9),
    ] {
        assert!(covered(&pool, fj, lat, lon).await, "{lat},{lon}");
    }
    assert!(
        !covered(&pool, fj, -17.5, 0.0).await,
        "not a band round the planet"
    );

    // Every stored shape is valid, counter-clockwise and in range.
    let bad: i64 = app
        .one(
            "SELECT count(*) FROM alerts WHERE geom IS NOT NULL
               AND NOT (ST_IsValid(geom) AND ST_IsPolygonCCW(geom) AND ST_SRID(geom) = 4326
                        AND ST_XMin(geom) >= -180 AND ST_XMax(geom) <= 180)",
        )
        .await;
    assert_eq!(bad, 0);

    // Health: the US source is not read at all; the broken one is failing.
    let sources = app.get_json("/api/alerts/sources").await;
    let list = sources["sources"].as_array().unwrap();
    assert!(list.iter().all(|s| s["id"] != "us-noaa-nws-en"));
    assert_eq!(
        (&list[0]["id"], &list[0]["state"]),
        (&json!("xx-down"), &json!("failing")),
        "failing sources first"
    );
    assert!(list[0]["last_error"].as_str().unwrap().contains("500"));
    assert!(sources["summary"]["healthy"].as_u64().unwrap() >= 9);
    assert_eq!(
        sources["attributions"],
        json!([wx_alerts::NUTS_ATTRIBUTION])
    );

    // A second pass: validators are sent back, nothing is fetched twice.
    let before = mock.total_hits();
    app.world_poll().await;
    assert_eq!(mock.hits("/naads/ca1.cap"), 1);
    assert_eq!(mock.hits("/s3/nu/drought.xml"), 1);
    assert_eq!(
        mock.hits("/s3/nu/wind.xml"),
        1,
        "a stale item is not fetched again"
    );
    assert!(
        mock.not_modified("/s3/nu/rss.xml") >= 1,
        "the S3-style feed answered 304"
    );
    assert_eq!(
        mock.conditional_hits("/feeds/meteoalarm-legacy-atom-croatia"),
        0,
        "nothing to send back"
    );
    assert!(
        mock.total_hits() - before <= 26,
        "one request per source, no documents"
    );
}

/// Severe, then grown to cover a second place, then unchanged, then
/// cancelled: each place hears of it once.
#[sqlx::test(migrations = "../../migrations")]
async fn a_thread_notifies_each_place_once_across_updates(pool: PgPool) {
    let mock = MockWorld::start().await;
    let app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "xx-met-en", "xx", "/met/rss.xml")],
    )
    .await;
    let (token, phone) = app.device("phone").await;
    app.save_place(&token, "Home", 10.0, 20.0).await;
    app.save_place(&token, "Cabin", 10.0, 21.0).await;

    let t0 = Utc::now() - ChronoDuration::minutes(30);
    let mut a1 = Cap::alert("A-1");
    a1.sent = t0;
    a1.polygons = vec![box_ring(10.0, 20.0, 0.2)];
    a1.headline = "Severe storm for the coast";
    a1.translations = vec![("fr-FR", "Tempête", "Tempête sur la côte")];
    serve_feed(&mock, "met", &[("a1", a1.xml())]);
    assert_eq!(app.world_poll().await.notifications, 1);
    eventually("first push", || async { app.push.count() == 1 }).await;
    let payload = phone.open(&app.push.received()[0]);
    assert_eq!(payload["title"], "Storm");
    assert_eq!(
        payload["body"], "Home: Severe storm for the coast",
        "the alert's own headline"
    );
    assert_eq!(payload["severity"], "Severe");

    // The update grows the area over the cabin.
    let mut a2 = Cap::alert("A-2");
    a2.msg_type = "Update";
    a2.sent = t0 + ChronoDuration::minutes(10);
    a2.references = vec![("A-1", t0)];
    a2.polygons = vec![box_ring(10.0, 20.5, 0.8)];
    a2.headline = "Severe storm spreading inland";
    serve_feed(&mock, "met", &[("a1", a1.xml()), ("a2", a2.xml())]);
    assert_eq!(
        app.world_poll().await.notifications,
        1,
        "only the cabin is newly covered"
    );
    eventually("second push", || async { app.push.count() == 2 }).await;
    assert_eq!(
        phone.open(&app.push.received()[1])["body"],
        "Cabin: Severe storm spreading inland"
    );

    let t = threads(&app).await;
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].1, t[1].1, "one hazard");
    assert!(t[0].1.starts_with("cap:"));
    assert_eq!(
        (t[0].2, t[1].2),
        (false, true),
        "the update replaces the alert"
    );

    // The same again: nothing new to say.
    let mut a3 = Cap::alert("A-3");
    a3.msg_type = "Update";
    a3.sent = t0 + ChronoDuration::minutes(20);
    a3.references = vec![("A-1", t0), ("A-2", a2.sent)];
    a3.polygons = a2.polygons.clone();
    serve_feed(&mock, "met", &[("a2", a2.xml()), ("a3", a3.xml())]);
    assert_eq!(app.world_poll().await.notifications, 0);

    // Cancelled: off the map, and no place is covered any more.
    let mut a4 = Cap::alert("A-4");
    a4.msg_type = "Cancel";
    a4.sent = t0 + ChronoDuration::minutes(25);
    a4.references = vec![("A-3", a3.sent)];
    a4.polygons = a2.polygons.clone();
    serve_feed(&mock, "met", &[("a3", a3.xml()), ("a4", a4.xml())]);
    app.world_poll().await;
    assert!(in_effect(&app).await.is_empty());
    assert!(app.at(10.0, 20.0).await.is_empty());
    assert_eq!(
        app.push.count_after_a_while().await,
        2,
        "two places, two pushes"
    );
}

/// Silent until it is serious; urgency Past never notifies; messages
/// that arrive in the wrong order still form one thread; a message that
/// links two threads merges them.
#[sqlx::test(migrations = "../../migrations")]
async fn severity_order_and_merges(pool: PgPool) {
    let mock = MockWorld::start().await;
    let app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "xx-met-en", "xx", "/met/rss.xml")],
    )
    .await;
    let (token, _) = app.device("phone").await;
    app.save_place(&token, "Farm", 30.0, 40.0).await;
    let feed = |docs: &[(&str, String)]| serve_feed(&mock, "met", docs);
    let t0 = Utc::now() - ChronoDuration::minutes(40);
    let message = |id: &'static str, sender: &'static str, minutes: i64, lat: f64, lon: f64| {
        let mut c = Cap::alert(id);
        c.sender = sender;
        c.sent = t0 + ChronoDuration::minutes(minutes);
        c.polygons = vec![box_ring(lat, lon, 0.3)];
        c
    };

    let mut m1 = message("M-1", "met@example.org", 0, 30.0, 40.0);
    m1.severity = "Moderate";
    let mut past = message("P-1", "other@example.org", 0, 30.0, 40.0);
    past.urgency = "Past";
    feed(&[("m1", m1.xml()), ("p1", past.xml())]);
    assert_eq!(
        app.world_poll().await.notifications,
        0,
        "moderate and past are silent"
    );

    let mut m2 = message("M-2", "met@example.org", 5, 30.0, 40.0);
    m2.msg_type = "Update";
    m2.references = vec![("M-1", t0)];
    feed(&[("m2", m2.xml())]);
    assert_eq!(app.world_poll().await.notifications, 1, "it became severe");

    // Out of order: the update arrives before what it updates.
    let mut z1 = message("Z-1", "z@example.org", 0, -30.0, -40.0);
    z1.severity = "Minor";
    let mut z2 = message("Z-2", "z@example.org", 5, -30.0, -40.0);
    z2.msg_type = "Update";
    z2.severity = "Minor";
    z2.references = vec![("Z-1", t0)];
    feed(&[("z2", z2.xml())]);
    app.world_poll().await;
    feed(&[("z1", z1.xml()), ("z2", z2.xml())]);
    app.world_poll().await;
    let z: Vec<_> = threads(&app)
        .await
        .into_iter()
        .filter(|r| r.0.starts_with("Z-"))
        .collect();
    assert_eq!(z.len(), 2);
    assert_eq!(z[0].1, z[1].1);
    assert_eq!(
        (z[0].2, z[1].2),
        (false, true),
        "the older message stays replaced"
    );

    // Two threads (different events, so not one by the fallback), then
    // one message that references both.
    let x = message("X-1", "w@example.org", 0, 30.0, 40.0);
    let mut y = message("Y-1", "w@example.org", 1, 30.0, 40.0);
    y.event = "Flood";
    feed(&[("x", x.xml()), ("y", y.xml())]);
    assert_eq!(
        app.world_poll().await.notifications,
        2,
        "two hazards, at first"
    );
    let mut w = message("W-2", "w@example.org", 10, 30.0, 40.0);
    w.msg_type = "Update";
    w.references = vec![("X-1", t0), ("Y-1", y.sent)];
    feed(&[("w", w.xml())]);
    assert_eq!(
        app.world_poll().await.notifications,
        0,
        "merged, and already notified"
    );
    let keys: Vec<String> = app
        .one("SELECT array_agg(DISTINCT event_key) FROM alerts WHERE cap_sender = 'w@example.org'")
        .await;
    assert_eq!(keys.len(), 1, "{keys:?}");
    let (rows,): (i64,) = app
        .one_for(
            "SELECT count(*) FROM notifications WHERE event_key = $1",
            &keys[0],
        )
        .await;
    assert_eq!(
        rows, 1,
        "one notification row for the merged hazard and the farm"
    );
}

/// A sender that never links messages re-issues the same warning with a
/// new identifier: it is the same hazard while the windows overlap.
#[sqlx::test(migrations = "../../migrations")]
async fn feeds_without_references_fall_back_to_sender_event_and_area(pool: PgPool) {
    let mock = MockWorld::start().await;
    let app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "bz-nms-en", "bz", "/bz/rss.xml")],
    )
    .await;
    let (token, _) = app.device("phone").await;
    app.save_place(&token, "Belize City", 17.5, -88.2).await;
    let t0 = Utc::now() - ChronoDuration::hours(2);
    let issue = |id: &'static str,
                 sent: DateTime<Utc>,
                 onset: Option<DateTime<Utc>>,
                 hours: i64,
                 nudge: bool| {
        let mut c = Cap::alert(id);
        c.sender = "nms@example.bz";
        c.event = "SMALL CRAFT CAUTION";
        c.sent = sent;
        let start = onset.unwrap_or(sent);
        c.expires = Some(start + ChronoDuration::hours(hours));
        // Re-issues nudge the corners a little.
        let half = if nudge {
            0.5 + (sent.timestamp() % 7) as f64 * 0.0001
        } else {
            0.5
        };
        c.polygons = vec![box_ring(17.5, -88.2, half)];
        let xml = c.xml();
        match onset {
            Some(onset) => xml.replace(
                &format!("<onset>{}</onset>", sent.to_rfc3339()),
                &format!("<onset>{}</onset>", onset.to_rfc3339()),
            ),
            None => xml,
        }
    };
    serve_feed(&mock, "bz", &[("1", issue("BZ-1", t0, None, 12, true))]);
    assert_eq!(app.world_poll().await.notifications, 1);

    // The re-issue, and tomorrow's, which does not overlap: a hazard of its own.
    let tomorrow = Utc::now() + ChronoDuration::hours(20);
    let later = t0 + ChronoDuration::hours(1);
    serve_feed(
        &mock,
        "bz",
        &[
            ("3", issue("BZ-3", later, Some(tomorrow), 10, false)),
            ("2", issue("BZ-2", later, None, 12, true)),
            ("1", issue("BZ-1", t0, None, 12, true)),
        ],
    );
    app.world_poll().await;
    let rows: Vec<(String, String, bool)> =
        app.rows("SELECT cap_identifier, event_key, active FROM alerts WHERE origin = 'cap' ORDER BY cap_identifier").await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].1, rows[1].1, "the re-issue continues the hazard");
    assert_ne!(rows[0].1, rows[2].1, "tomorrow's is another one");
    assert_eq!((rows[0].2, rows[1].2), (false, true));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        app.one::<i64>("SELECT count(*) FROM notifications").await,
        2,
        "today's once, tomorrow's once"
    );
}

/// Tiles carry the revision; it rises on every change and only then.
#[sqlx::test(migrations = "../../migrations")]
async fn vector_tiles_and_the_revision(pool: PgPool) {
    let mock = MockWorld::start().await;
    let mut app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "xx-met-en", "xx", "/met/rss.xml")],
    )
    .await;
    let start = app.alerts.revision().await;

    let mut a = Cap::alert("T-1");
    a.polygons = vec![box_ring(48.85, 2.35, 0.5)];
    a.severity = "Extreme";
    a.headline = "Red storm warning for Paris and the surrounding departments of the Ile-de-France region, \
                  with gusts over 130 km/h expected from the evening until the early hours of tomorrow morning";
    serve_feed(&mock, "met", &[("1", a.xml())]);
    app.world_poll().await;
    let rev = app.alerts.revision().await;
    assert!(rev > start);
    assert_eq!(
        app.alert_revisions().last().copied(),
        Some(rev),
        "the event announces the revision"
    );

    let tj = app.get_json("/api/alerts/tiles.json").await;
    assert_eq!(tj["revision"], rev);
    assert_eq!(
        tj["tiles"][0],
        format!("/api/alerts/tiles/{rev}/{{z}}/{{x}}/{{y}}.mvt")
    );

    // Paris at zoom 5 is tile 16/11; at zoom 0 everything is in one tile.
    let id = id_of(&pool, "T-1").await;
    for (z, x, y) in [(0, 0, 0), (5, 16, 11), (9, 259, 176)] {
        let resp = app
            .get(&format!("/api/alerts/tiles/{rev}/{z}/{x}/{y}.mvt"))
            .await;
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()["content-type"],
            "application/vnd.mapbox-vector-tile"
        );
        assert_eq!(
            resp.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        let layers = decode_mvt(&resp.bytes().await.unwrap());
        assert_eq!(layers.len(), 1, "z{z}");
        let l = &layers[0];
        assert_eq!((l.name.as_str(), l.extent), ("alerts", 4096));
        assert_eq!(l.features.len(), 1);
        assert_eq!(l.features[0].geom_type, 3, "a polygon");
        let p = l.props(&l.features[0]);
        let s = |v: &str| MvtValue::String(v.to_string());
        assert_eq!(p["id"], s(&id));
        assert_eq!(p["severity"], s("Extreme"));
        assert_eq!(p["level"], MvtValue::Int(4));
        assert_eq!(p["category"], s("met"));
        assert_eq!(p["source"], s("xx-met-en"));
        assert_eq!(p["country"], s("xx"));
        assert_eq!(p["geom_source"], s("cap"));
        assert_eq!(p["event"], s("Storm"));
        let MvtValue::String(h) = &p["headline"] else {
            panic!()
        };
        assert_eq!(h.chars().count(), 120, "short headline");
        assert_eq!(p["expires"], MvtValue::Int(a.expires.unwrap().timestamp()));
    }
    // Far away: an empty tile.
    let resp = app.get(&format!("/api/alerts/tiles/{rev}/5/0/0.mvt")).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.bytes().await.unwrap().is_empty());
    // A revision that does not exist yet, and nonsense addresses.
    let next = rev + 1;
    for path in [
        format!("{next}/0/0/0.mvt"),
        format!("{rev}/3/8/0.mvt"),
        format!("{rev}/a/0/0"),
    ] {
        let resp = app.get(&format!("/api/alerts/tiles/{path}")).await;
        assert_eq!(resp.status(), 404, "{path}");
    }

    // Nothing changed: the same revision.
    app.world_poll().await;
    assert_eq!(app.alerts.revision().await, rev);

    // A new alert: a new revision, and the old one's tiles are served
    // current but not cacheable.
    let mut b = Cap::alert("T-2");
    b.polygons = vec![box_ring(40.4, -3.7, 0.5)];
    serve_feed(&mock, "met", &[("2", b.xml()), ("1", a.xml())]);
    app.world_poll().await;
    let rev2 = app.alerts.revision().await;
    assert!(rev2 > rev);
    let resp = app.get(&format!("/api/alerts/tiles/{rev}/0/0/0.mvt")).await;
    assert_eq!(resp.headers()["cache-control"], "no-cache");
    assert_eq!(
        decode_mvt(&resp.bytes().await.unwrap())[0].features.len(),
        2
    );

    // A restart takes a new revision, above every one handed out before,
    // even when the stored counter has gone back (a database restored from
    // a backup).
    app.exec("UPDATE alerts_revision SET revision = 2").await;
    let restarted = app.restarted();
    let rev3 = restarted.revision().await;
    assert!(rev3 > rev2, "{rev3} after {rev2}");
    assert!(
        rev3 >= Utc::now().timestamp_millis() - 60_000,
        "at least the time of the change in ms: {rev3}"
    );
    assert_eq!(
        restarted.revision().await,
        rev3,
        "and steady until something changes"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_list_filters_by_box_country_and_limit(pool: PgPool) {
    let mock = MockWorld::start().await;
    let app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "xx-met-en", "xx", "/met/rss.xml")],
    )
    .await;
    let alerts = [
        ("L-0", 10.0, 10.0, "Minor"),
        ("L-1", 10.5, 10.5, "Extreme"),
        ("L-2", -10.0, 179.8, "Severe"),
        ("L-3", 50.0, -100.0, "Moderate"),
    ];
    let docs: Vec<(&str, String)> = alerts
        .iter()
        .map(|&(id, lat, lon, severity)| {
            let mut c = Cap::alert(id);
            c.severity = severity;
            c.polygons = vec![box_ring(lat, lon, 0.1)];
            (id, c.xml())
        })
        .collect();
    serve_feed(&mock, "met", &docs);
    app.world_poll().await;

    let all = app.get_json("/api/alerts").await;
    let sev: Vec<&str> = all["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["properties"]["severity"].as_str().unwrap())
        .collect();
    assert_eq!(
        sev,
        ["Extreme", "Severe", "Moderate", "Minor"],
        "most severe first"
    );
    assert_eq!(all["truncated"], false);
    assert!(all["revision"].as_i64().unwrap() > 1);

    let count = |q: &'static str| {
        let app = &app;
        async move {
            app.get_json(&format!("/api/alerts?{q}")).await["features"]
                .as_array()
                .unwrap()
                .len()
        }
    };
    assert_eq!(count("bbox=9,9,11,11").await, 2);
    assert_eq!(
        count("bbox=179,-11,-179,-9").await,
        1,
        "a box across the antimeridian"
    );
    let limited = app.get_json("/api/alerts?limit=1").await;
    assert_eq!(limited["features"].as_array().unwrap().len(), 1);
    assert_eq!(limited["truncated"], true);
    assert_eq!(limited["features"][0]["properties"]["severity"], "Extreme");
    assert_eq!(count("country=xx").await, 4);
    assert_eq!(count("country=fr").await, 0);
    for bad in [
        "bbox=1,2,3",
        "bbox=0,10,1,5",
        "bbox=a,b,c,d",
        "limit=0",
        "limit=100000",
        "country=france",
    ] {
        let resp = app.get(&format!("/api/alerts?{bad}")).await;
        assert_eq!(resp.status(), 400, "{bad}");
    }
}

/// A source that hangs or fails delays only itself, backs off, and shows up
/// on the status page; the others keep their schedule.
#[sqlx::test(migrations = "../../migrations")]
async fn a_failing_source_does_not_hold_up_the_others(pool: PgPool) {
    let mock = MockWorld::start().await;
    let mut good = Cap::alert("G-1");
    good.polygons = vec![box_ring(1.0, 1.0, 0.1)];
    serve_feed(&mock, "good", &[("1", good.xml())]);
    mock.put_feed("/slow/rss.xml", rss(&[]));
    mock.slow("/slow/rss.xml", Duration::from_secs(30));
    mock.fail("/bad/rss.xml", 503);
    let sources = [
        source(&mock, "xx-good", "xx", "/good/rss.xml"),
        source(&mock, "xx-slow", "xx", "/slow/rss.xml"),
        source(&mock, "xx-bad", "xx", "/bad/rss.xml"),
    ];
    let app = world_app(pool.clone(), &mock, &sources).await;
    let running = app.spawn();
    eventually("the good source stored", || async {
        !alerts_of(&app.pool, "xx-good").await.is_empty()
    })
    .await;
    // Several cycles of the good source while the slow one times out.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let health = running.stop().await;
    assert!(
        mock.hits("/good/rss.xml") >= 4,
        "polled on schedule: {}",
        mock.hits("/good/rss.xml")
    );
    assert!(
        mock.hits("/bad/rss.xml") <= 5,
        "backing off: {}",
        mock.hits("/bad/rss.xml")
    );
    assert_eq!(mock.hits("/good/1.xml"), 1);
    let sources = app.alerts.world_sources().await;
    let state = |id: &str| sources.iter().find(|s| s.id == id).unwrap();
    assert_eq!(state("xx-good").state, "healthy");
    assert_eq!(state("xx-bad").state, "failing");
    assert!(
        state("xx-bad")
            .last_error
            .as_deref()
            .unwrap()
            .contains("503")
    );
    assert_eq!(state("xx-slow").state, "failing");
    assert!(
        state("xx-slow")
            .last_error
            .as_deref()
            .unwrap()
            .contains("timed out")
    );
    let feed = health.get("alerts-world").unwrap();
    assert!(
        feed.detail.contains("failing") && feed.detail.contains("worst: "),
        "{}",
        feed.detail
    );
}

/// Broken and hostile documents are refused and recorded, never fatal.
#[sqlx::test(migrations = "../../migrations")]
async fn hostile_documents_are_refused(pool: PgPool) {
    let mock = MockWorld::start().await;
    let xxe = r#"<?xml version="1.0"?><!DOCTYPE alert [<!ENTITY x SYSTEM "file:///etc/passwd">]>
        <alert xmlns="urn:oasis:names:tc:emergency:cap:1.2"><identifier>&x;</identifier></alert>"#;
    let mut evil = Cap::alert("E-1");
    // Markup in a text field, escaped as feeds send it.
    evil.headline = "&lt;script&gt;alert(1)&lt;/script&gt;Flood &lt;b&gt;warning&lt;/b&gt;";
    evil.polygons = vec![box_ring(5.0, 5.0, 0.1), vec![(1.0, 1.0), (1.0, 1.0)]];
    let evil = evil
        .xml()
        .replace("https://example.org/warnings", "javascript:alert(1)");
    serve_feed(
        &mock,
        "h",
        &[
            ("xxe", xxe.to_string()),
            ("html", "<html><body>Maintenance</body></html>".to_string()),
            ("huge", " ".repeat(9 << 20)),
            ("evil", evil),
        ],
    );
    let app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "xx-hostile", "xx", "/h/rss.xml")],
    )
    .await;
    let report = app.world_poll().await;
    assert_eq!(report.documents_failed, 3, "{report:?}");
    let outcomes: Vec<(String, String)> = app
        .rows("SELECT url, outcome FROM cap_documents ORDER BY url")
        .await;
    let outcome = |p: &str| {
        outcomes
            .iter()
            .find(|(u, _)| u.ends_with(p))
            .map(|(_, o)| o.clone())
            .unwrap()
    };
    assert!(
        outcome("xxe.xml").contains("entities"),
        "{}",
        outcome("xxe.xml")
    );
    assert!(outcome("html.xml").contains("not a CAP alert"));
    assert!(outcome("huge.xml").contains("larger than"));
    assert_eq!(outcome("evil.xml"), "stored");
    let (headline, valid): (String, bool) = app
        .rows("SELECT headline, ST_IsValid(geom) FROM alerts WHERE cap_identifier = 'E-1'")
        .await
        .remove(0);
    assert_eq!(headline, "Flood warning");
    assert!(valid);
    let detail = app
        .get_json(&format!(
            "/api/alerts/detail?id={}",
            id_of(&pool, "E-1").await
        ))
        .await;
    assert_eq!(
        detail["infos"][0]["web"],
        Value::Null,
        "only http(s) links are kept"
    );
}

/// A host whose documents cannot be reached costs one small batch per
/// poll, not a timeout for every document it lists.
#[sqlx::test(migrations = "../../migrations")]
async fn unreachable_documents_cost_one_batch_per_poll(pool: PgPool) {
    let mock = MockWorld::start().await;
    let links: Vec<String> = (0..20)
        .map(|i| mock.url(&format!("/dead/{i}.xml")))
        .collect();
    for i in 0..20 {
        mock.fail(&format!("/dead/{i}.xml"), 503);
    }
    mock.put_feed("/dead/rss.xml", rss(&links));
    let app = world_app(
        pool,
        &mock,
        &[source(&mock, "xx-dead", "xx", "/dead/rss.xml")],
    )
    .await;
    app.world_poll().await;
    let fetched: usize = (0..20).map(|i| mock.hits(&format!("/dead/{i}.xml"))).sum();
    assert_eq!(fetched, 4, "one batch");
    let s = app
        .alerts
        .world_sources()
        .await
        .into_iter()
        .find(|s| s.id == "xx-dead")
        .unwrap();
    assert_eq!(s.backlog, 16);
    assert_eq!(s.state, "healthy", "the feed itself answers");
}

/// Who sees an alert without a shape, and shapes split at the antimeridian.
#[sqlx::test(migrations = "../../migrations")]
async fn unshaped_alerts_and_the_antimeridian_in_the_list(pool: PgPool) {
    let mock = MockWorld::start().await;
    let mut it = Cap::alert("IT-1");
    it.sender = "it@example.org";
    it.geocodes = vec![("EMMA_ID", "IT001")];
    let mut fj = Cap::alert("FJ-1");
    fj.sender = "fj@example.org";
    fj.polygons = vec![vec![
        (-16.5, 178.5),
        (-16.5, -178.5),
        (-18.5, -178.5),
        (-18.5, 178.5),
        (-16.5, 178.5),
    ]];
    serve_feed(&mock, "it", &[("1", it.xml())]);
    serve_feed(&mock, "fj", &[("1", fj.xml())]);
    let it_src = RegSource {
        latlng: "42.3,12.8",
        ..source(&mock, "it-test", "it", "/it/rss.xml")
    };
    let fj_src = RegSource {
        latlng: "-17.7,178.1",
        ..source(&mock, "fj-test", "fj", "/fj/rss.xml")
    };
    let app = world_app(pool.clone(), &mock, &[it_src, fj_src]).await;
    app.world_poll().await;
    let span: f64 = app
        .one("SELECT ST_XMax(geom) - ST_XMin(geom) FROM alerts WHERE cap_identifier = 'FJ-1'")
        .await;
    assert!(
        span > 350.0,
        "stored split at 180, so its box spans every longitude: {span}"
    );

    let sources = |q: &str| {
        let app = &app;
        let q = q.to_string();
        async move {
            let v = app.get_json(&format!("/api/alerts{q}")).await;
            let mut s: Vec<String> = v["features"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["properties"]["source"].as_str().unwrap().to_string())
                .collect();
            s.sort();
            s
        }
    };
    // Without a shape: left out unless asked for; `country` asks for it.
    assert_eq!(sources("").await, ["fj-test"]);
    assert_eq!(sources("?country=it").await, ["it-test"]);
    assert!(sources("?country=it&unshaped=false").await.is_empty());
    assert_eq!(sources("?unshaped=true").await, ["fj-test", "it-test"]);
    // With a box: only through `unshaped=true`, and placed at the anchor.
    assert!(sources("?bbox=12,42,13,43").await.is_empty());
    assert_eq!(
        sources("?bbox=12,42,13,43&unshaped=true").await,
        ["it-test"]
    );
    assert!(sources("?bbox=0,0,1,1&unshaped=true").await.is_empty());
    assert_eq!(
        sources("?bbox=12,42,13,43&country=it").await,
        ["it-test"],
        "country turns unshaped on"
    );
    // Across the antimeridian: found from either side and across it, not
    // from the other side of the planet at the same latitudes.
    for q in [
        "?bbox=178,-19,180,-16",
        "?bbox=-180,-19,-178,-16",
        "?bbox=179,-19,-179,-16",
    ] {
        assert_eq!(sources(q).await, ["fj-test"], "{q}");
    }
    for q in [
        "?bbox=-30,-19,-10,-16",
        "?bbox=0,-19,10,-16",
        "?bbox=-30,-19,-10,-16&unshaped=true",
    ] {
        assert!(sources(q).await.is_empty(), "{q}");
    }
    // The same in tiles: zoom 2 tile 3/2 holds Fiji, tile 1/2 (the
    // Atlantic, same latitudes) does not.
    let rev = app.alerts.revision().await;
    let fiji = [id_of(&pool, "FJ-1").await];
    let ids = |x: u32| {
        let app = &app;
        async move {
            tile(app, &format!("/api/alerts/tiles/{rev}/2/{x}/2.mvt"))
                .await
                .into_keys()
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(ids(3).await, fiji);
    assert_eq!(ids(0).await, fiji, "the part west of 180");
    assert!(ids(1).await.is_empty());
}

/// What every endpoint says about the issuer, the category and the end
/// time, for an NWS alert and for world alerts.
#[sqlx::test(migrations = "../../migrations")]
async fn issuer_category_and_end_time_are_the_same_everywhere(pool: PgPool) {
    let mock = MockWorld::start().await;
    let mut no_end = Cap::alert("NE-1");
    no_end.expires = None;
    no_end.category = "";
    no_end.polygons = vec![box_ring(50.0, 10.0, 0.2)];
    let mut odd = Cap::alert("OD-1");
    odd.category = "Weather";
    odd.polygons = vec![box_ring(52.0, 10.0, 0.2)];
    serve_feed(&mock, "de", &[("1", no_end.xml()), ("2", odd.xml())]);
    let de = RegSource {
        name: "Germany: Deutscher Wetterdienst",
        country_name: "Germany",
        ..source(&mock, "de-dwd-en", "de", "/de/rss.xml")
    };
    let app = world_app(pool.clone(), &mock, &[de]).await;
    app.nws.set_alerts(vec![
        alert("urn:test:tor-1", "Tornado Warning")
            .polygon(square(-97.44, 35.22, 0.1))
            .vtec("NEW", "KOUN", "TO", "W", "0042")
            .build(),
    ]);
    app.poll().await;
    app.world_poll().await;

    let list = app.features().await;
    let by_id =
        |id: &str| -> Value { list.iter().find(|f| f["id"] == id).unwrap()["properties"].clone() };
    let (ne, od) = (id_of(&pool, "NE-1").await, id_of(&pool, "OD-1").await);

    // The NWS alert.
    let tor = by_id("urn:test:tor-1");
    assert_eq!(
        (&tor["source"], &tor["country"], &tor["category"]),
        (&json!("us-nws"), &json!("us"), &json!("met"))
    );
    assert_eq!(tor["expires_assumed"], false);
    let detail = app.get_json("/api/alerts/detail?id=urn:test:tor-1").await;
    assert_eq!(
        detail["source_name"],
        "United States of America: National Weather Service"
    );
    assert_eq!(detail["country_name"], "United States of America");
    assert_eq!(detail["infos"][0]["category"], "met");

    // World: no category is `other`, an unknown one too; names from the register.
    assert_eq!(by_id(&ne)["category"], "other");
    assert_eq!(by_id(&od)["category"], "other");
    let detail = app.get_json(&format!("/api/alerts/detail?id={ne}")).await;
    assert_eq!(detail["source"], "de-dwd-en");
    assert_eq!(detail["source_name"], "Germany: Deutscher Wetterdienst");
    assert_eq!(detail["country"], "de");
    assert_eq!(detail["country_name"], "Germany");
    assert_eq!(detail["infos"][0]["category"], "other");

    // No expiry from the sender: a day after onset, and marked as assumed.
    let p = by_id(&ne);
    assert_eq!(p["expires_assumed"], true);
    let time = |v: &Value| DateTime::parse_from_rfc3339(v.as_str().unwrap()).unwrap();
    let expires = time(&p["expires"]);
    assert_eq!(expires - time(&p["onset"]), ChronoDuration::hours(24));
    assert_eq!(
        detail["infos"][0]["expires"],
        Value::Null,
        "the block itself says what the sender said"
    );
    assert_eq!(by_id(&od)["expires_assumed"], false);
    assert_eq!(app.at(50.0, 10.0).await[0]["expires_assumed"], true);

    // Tiles say the same (zoom 0 holds everything).
    let rev = app.alerts.revision().await;
    let tile = tile(&app, &format!("/api/alerts/tiles/{rev}/0/0/0.mvt")).await;
    let s = |v: &str| MvtValue::String(v.to_string());
    let t = &tile["urn:test:tor-1"];
    assert_eq!(
        (&t["source"], &t["country"], &t["category"]),
        (&s("us-nws"), &s("us"), &s("met"))
    );
    assert_eq!(t["expires_assumed"], MvtValue::Bool(false));
    let t = &tile[&ne];
    assert_eq!(t["category"], s("other"));
    assert_eq!(t["expires_assumed"], MvtValue::Bool(true));
    assert_eq!(t["expires"], MvtValue::Int(expires.timestamp()));
    let categories = [
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
    for p in tile.values() {
        let MvtValue::String(c) = &p["category"] else {
            panic!()
        };
        assert!(categories.contains(&c.as_str()));
        let MvtValue::Int(level) = p["level"] else {
            panic!()
        };
        assert!((0..=4).contains(&level));
    }
}

/// A shape GEOS cannot snap to the display grid gets a plain but sound
/// display shape; the other shapes built with it keep their usual ones.
#[sqlx::test(migrations = "../../migrations")]
async fn a_shape_geos_refuses_does_not_spoil_the_others(pool: PgPool) {
    let app = App::start(pool.clone()).await;
    // A 4-degree zone with an island far below the display grid, and a
    // self-crossing shape (which no write path stores; it stands in for
    // whatever makes ST_ReducePrecision throw).
    let good = "MULTIPOLYGON(((-150 56,-146 56,-146 60,-150 60,-150 56)),((-140 58,-139.999 58,-139.999 58.001,-140 58.001,-140 58)))";
    let bad = "MULTIPOLYGON(((-120 40,-118 42,-118 40,-120 42,-120 40)))";
    for (id, wkt) in [("urn:test:good", good), ("urn:test:bad", bad)] {
        sqlx::query(
            "INSERT INTO alerts (id, event_key, event, severity, geom, geom_source, ugc, active)
             VALUES ($1, $1, 'Gale Warning', 'Moderate', ST_GeomFromText($2, 4326), 'zones', ARRAY[$1], true)",
        )
        .bind(id)
        .bind(wkt)
        .execute(&pool)
        .await
        .unwrap();
    }
    let reduce =
        sqlx::query("SELECT ST_ReducePrecision(geom, 0.005) FROM alerts WHERE id = 'urn:test:bad'");
    assert!(
        reduce.fetch_one(&pool).await.is_err(),
        "the stand-in must make GEOS throw"
    );

    for _ in 0..2 {
        let features = app.features().await;
        assert_eq!(features.len(), 2);
        let geom = |id: &str| features.iter().find(|f| f["id"] == id).unwrap()["geometry"].clone();
        assert_eq!(
            geom("urn:test:good")["coordinates"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "usual shape: the speck is dropped"
        );
        let fallback = geom("urn:test:bad");
        assert_eq!(fallback["type"], "MultiPolygon");
        let valid: bool = sqlx::query_scalar("SELECT ST_IsValid(ST_GeomFromGeoJSON($1))")
            .bind(fallback.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(valid, "the fallback is repaired: {fallback}");
    }
    // Both are in the tiles too: the trigger never fails a write.
    let rev = app.alerts.revision().await;
    assert_eq!(
        tile(&app, &format!("/api/alerts/tiles/{rev}/0/0/0.mvt"))
            .await
            .len(),
        2
    );
}

/// A message `id` of the thread rules' tests, sent `minutes` after `t0`,
/// with `refs`, over a box at (`lat`, `lon`).
fn threaded(
    id: &'static str,
    sender: &'static str,
    t0: DateTime<Utc>,
    minutes: i64,
    refs: Vec<(&'static str, DateTime<Utc>)>,
    (lat, lon, half): (f64, f64, f64),
) -> Cap<'static> {
    let mut c = Cap::alert(id);
    c.sender = sender;
    c.msg_type = if refs.is_empty() { "Alert" } else { "Update" };
    c.sent = t0 + ChronoDuration::minutes(minutes);
    c.references = refs;
    c.polygons = vec![box_ring(lat, lon, half)];
    c
}

/// MET Norway sends as one address and refers to its messages under another.
#[sqlx::test(migrations = "../../migrations")]
async fn a_reference_matches_on_the_identifier_within_the_source(pool: PgPool) {
    let mock = MockWorld::start().await;
    let app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "no-met-en", "no", "/no/rss.xml")],
    )
    .await;
    let (token, _) = app.device("phone").await;
    app.save_place(&token, "Bergen", 60.4, 5.3).await;
    let t0 = Utc::now() - ChronoDuration::minutes(30);
    let met_no = |id, minutes, refs, (lat, lon), half| {
        let mut c = threaded(
            id,
            "meteorologi@met.no",
            t0,
            minutes,
            refs,
            (lat, lon, half),
        );
        c.reference_sender = Some("noreply@met.no");
        c
    };
    // Each update a different area, so only the reference can link them.
    let a1 = met_no("2.49.0.1.578.0.1", 0, vec![], (60.4, 5.3), 0.2);
    serve_feed(&mock, "no", &[("a1", a1.xml())]);
    assert_eq!(app.world_poll().await.notifications, 1);
    let a2 = met_no(
        "2.49.0.1.578.0.2",
        10,
        vec![("2.49.0.1.578.0.1", t0)],
        (60.4, 5.3),
        0.4,
    );
    serve_feed(&mock, "no", &[("a2", a2.xml())]);
    assert_eq!(
        app.world_poll().await.notifications,
        0,
        "the same hazard: no second push"
    );
    let t = threads(&app).await;
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].1, t[1].1, "one thread: {t:?}");
    assert_eq!(
        (t[0].2, t[1].2),
        (false, true),
        "the update replaced the alert"
    );

    // The other way round: an update stored before the message it updates.
    let rain = |id, minutes, refs, half| {
        let mut c = met_no(id, minutes, refs, (61.0, 6.0), half);
        c.event = "Rain";
        c.msg_type = "Update";
        c.xml()
    };
    let t1 = t0 + ChronoDuration::minutes(1);
    serve_feed(
        &mock,
        "no",
        &[(
            "b2",
            rain("2.49.0.1.578.0.4", 6, vec![("2.49.0.1.578.0.3", t1)], 0.4),
        )],
    );
    app.world_poll().await;
    serve_feed(
        &mock,
        "no",
        &[("b1", rain("2.49.0.1.578.0.3", 1, vec![], 0.2))],
    );
    app.world_poll().await;
    let t: Vec<_> = threads(&app)
        .await
        .into_iter()
        .filter(|r| r.0.ends_with(".3") || r.0.ends_with(".4"))
        .collect();
    assert_eq!(t[0].1, t[1].1, "{t:?}");
    assert_eq!((t[0].2, t[1].2), (false, true));
}

/// Two updates that reference the same unseen message, and re-issues that
/// reference nothing stored, from a source that does use references.
#[sqlx::test(migrations = "../../migrations")]
async fn shared_and_missing_references_and_reissues_make_one_hazard(pool: PgPool) {
    let mock = MockWorld::start().await;
    let sources = [
        source(&mock, "au-bom-en", "au", "/au/rss.xml"),
        source(&mock, "ru-roshydromet-en", "ru", "/ru/rss.xml"),
    ];
    let app = world_app(pool.clone(), &mock, &sources).await;
    let t0 = Utc::now() - ChronoDuration::hours(1);
    let missing = t0 - ChronoDuration::hours(12);
    let update = |id, minutes, half| {
        let refs = vec![("IDW20100-old-1", missing), ("IDW20100-old-2", missing)];
        threaded(
            id,
            "CAP.Message@bom.gov.au",
            t0,
            minutes,
            refs,
            (-33.0, 125.0, half),
        )
        .xml()
    };
    // A correction two minutes later, of a different extent: only the
    // shared references tie the two together.
    serve_feed(
        &mock,
        "au",
        &[
            ("u1", update("IDW20100-a", 0, 0.5)),
            ("u2", update("IDW20100-b", 2, 0.9)),
        ],
    );
    // Roshydromet: it uses references elsewhere, yet re-issues this
    // warning three times as new alerts that reference nothing.
    let ru = |id, hours: i64, refs| {
        let mut c = threaded(
            id,
            "hmc@meteoinfo.ru",
            t0,
            hours * 60,
            refs,
            (55.0, 160.0, 0.5),
        );
        c.msg_type = "Alert";
        c.event = "Wind";
        c.expires = Some(Utc::now() + ChronoDuration::hours(20));
        c.xml()
    };
    let mut other = threaded(
        "RU-other",
        "hmc@meteoinfo.ru",
        t0,
        55,
        vec![("RU-gone", missing)],
        (56.0, 40.0, 0.5),
    );
    other.event = "Fog";
    serve_feed(
        &mock,
        "ru",
        &[
            ("o", other.xml()),
            ("w1", ru("RU-w1", -40, vec![])),
            ("w2", ru("RU-w2", -20, vec![])),
            (
                "w3",
                ru(
                    "RU-w3",
                    0,
                    vec![("RU-never-stored", t0 - ChronoDuration::hours(50))],
                ),
            ),
        ],
    );
    app.world_poll().await;
    let t = threads(&app).await;
    let of = |prefix: &str| {
        t.iter()
            .filter(|r| r.0.starts_with(prefix))
            .collect::<Vec<_>>()
    };
    let au = of("IDW20100");
    assert_eq!(au.len(), 2);
    assert_eq!(au[0].1, au[1].1, "{au:?}");
    assert_eq!(
        (au[0].2, au[1].2),
        (false, true),
        "the correction replaced the original"
    );
    let ru = of("RU-w");
    assert_eq!(ru.len(), 3);
    assert!(ru.iter().all(|r| r.1 == ru[0].1), "{ru:?}");
    assert_eq!(ru.iter().filter(|r| r.2).count(), 1, "one copy in effect");
    assert!(ru[2].2, "the newest");
    let active = in_effect(&app).await.len();
    assert_eq!(
        active, 3,
        "Australia's correction, Russia's newest wind warning and its fog warning"
    );
}

/// A real Bureau of Meteorology warning: two English blocks in effect and
/// one cancelling a third coast. And a block order where the ended block
/// comes first, as ECCC can send.
#[sqlx::test(migrations = "../../migrations")]
async fn every_block_in_effect_makes_the_alert(pool: PgPool) {
    let mock = MockWorld::start().await;
    let app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "au-bom-en", "au", "/au/rss.xml")],
    )
    .await;
    let (token, _) = app.device("phone").await;
    app.save_place(&token, "In B", 21.0, 21.0).await;
    app.save_place(&token, "In C", 22.0, 22.0).await;

    let block = |severity: &str,
                 urgency: &str,
                 response: &str,
                 headline: &str,
                 lat: f64,
                 lon: f64| {
        let ring: Vec<String> = box_ring(lat, lon, 0.3)
            .iter()
            .map(|(a, b)| format!("{a},{b}"))
            .collect();
        let expires = (Utc::now() + ChronoDuration::hours(6)).to_rfc3339();
        format!(
            "<info><language>en-AU</language><category>Met</category><event>Wind</event>
             <responseType>{response}</responseType><urgency>{urgency}</urgency><severity>{severity}</severity>
             <certainty>Likely</certainty><expires>{expires}</expires><headline>{headline}</headline>
             <area><areaDesc>{headline} area</areaDesc><polygon>{}</polygon></area></info>",
            ring.join(" ")
        )
    };
    let message = |id: &str,
                   msg_type: &str,
                   sent: DateTime<Utc>,
                   references: &str,
                   blocks: String| {
        format!(
            r#"<alert xmlns="urn:oasis:names:tc:emergency:cap:1.2"><identifier>{id}</identifier>
               <sender>bom@example.au</sender><sent>{}</sent><status>Actual</status><msgType>{msg_type}</msgType>
               <scope>Public</scope>{references}{blocks}</alert>"#,
            sent.to_rfc3339()
        )
    };
    let sent = Utc::now() - ChronoDuration::minutes(5);
    let blocks = [
        block(
            "Unknown",
            "Past",
            "AllClear",
            "Wind warning ended for A",
            20.0,
            20.0,
        ),
        block(
            "Moderate",
            "Expected",
            "Prepare",
            "Strong wind warning for B",
            21.0,
            21.0,
        ),
        block(
            "Severe",
            "Immediate",
            "Prepare",
            "Gale warning for C",
            22.0,
            22.0,
        ),
        block(
            "Unknown",
            "Unknown",
            "Monitor",
            "Cancellation of Wind Warning for D",
            23.0,
            23.0,
        ),
    ];
    let mb1 = message("MB-1", "Alert", sent, "", blocks.concat());
    serve_feed(
        &mock,
        "au",
        &[
            ("mb", mb1),
            ("bom", current_fixture("au_bom_three_blocks.xml")),
        ],
    );
    app.world_poll().await;

    let id = id_of(&pool, "MB-1").await;
    for (lat, lon, inside) in [
        (20.0, 20.0, false),
        (21.0, 21.0, true),
        (22.0, 22.0, true),
        (23.0, 23.0, false),
    ] {
        assert_eq!(covered(&pool, &id, lat, lon).await, inside, "{lat},{lon}");
    }
    let detail = app.get_json(&format!("/api/alerts/detail?id={id}")).await;
    assert_eq!(
        detail["severity"], "Severe",
        "the most severe block in effect"
    );
    assert_eq!(
        detail["headline"], "Strong wind warning for B",
        "the first block in effect"
    );
    assert_eq!(
        detail["area_desc"],
        "Strong wind warning for B area; Gale warning for C area"
    );
    let flags: Vec<bool> = detail["infos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["in_effect"].as_bool().unwrap())
        .collect();
    assert_eq!(flags, [false, true, true, false]);
    eventually("pushes", || async { app.push.count() == 2 }).await;

    // The real one: Eucla, and Gascoyne with Eucla, but not Esperance.
    let (area, codes): (String, Vec<String>) = app
        .rows("SELECT area_desc, region_codes FROM alerts WHERE cap_identifier LIKE 'AusBoM-IDW20100-%'")
        .await
        .remove(0);
    assert_eq!(
        area,
        "Western Australia: Eucla Coast; Western Australia: Gascoyne Coast and Eucla Coast"
    );
    assert_eq!(codes, ["AMOC-AREACODE:WA_MW006", "AMOC-AREACODE:WA_MW014"]);

    // An update whose every block says it is over ends the hazard.
    let references = format!(
        "<references>bom@example.au,MB-1,{}</references>",
        sent.to_rfc3339()
    );
    let over = [
        block("Minor", "Past", "AllClear", "Ended for B", 21.0, 21.0),
        block("Minor", "Past", "AllClear", "Ended for C", 22.0, 22.0),
    ];
    serve_feed(
        &mock,
        "au",
        &[(
            "mb2",
            message("MB-2", "Update", Utc::now(), &references, over.concat()),
        )],
    );
    app.world_poll().await;
    let on = in_effect(&app).await;
    assert!(!on.iter().any(|i| i.starts_with("MB-")), "{on:?}");
}

/// A MeteoAlarm country feed lists only what is in force; an alert leaves
/// it only on firm evidence.
#[sqlx::test(migrations = "../../migrations")]
async fn meteoalarm_alerts_end_only_on_firm_evidence(pool: PgPool) {
    let mock = MockWorld::start().await;
    let feed = "/feeds/meteoalarm-legacy-atom-testland";
    let app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "tl-met-xx", "tl", feed)],
    )
    .await;
    let t0 = Utc::now() - ChronoDuration::minutes(30);
    let msg = |id, minutes, refs, lat| {
        threaded(
            id,
            "met@testland.example",
            t0,
            minutes,
            refs,
            (lat, 10.0, 0.2),
        )
        .xml()
    };
    let list = |docs: &[&str]| {
        let entries: Vec<(String, &str)> = docs
            .iter()
            .map(|d| (mock.url(&format!("/ma/{d}")), "XX001"))
            .collect();
        mock.put_feed_plain(feed, meteoalarm_atom(&entries));
    };
    mock.put_cap("/ma/a", msg("A", 0, vec![], 50.0));
    list(&["a"]);
    app.world_poll().await;
    assert_eq!(in_effect(&app).await, ["A"]);

    // Its update is listed instead, but cannot be fetched: A stays.
    mock.put_cap("/ma/u", msg("U", 10, vec![("A", t0)], 50.0));
    mock.fail("/ma/u", 503);
    list(&["u"]);
    app.world_poll().await;
    assert_eq!(
        in_effect(&app).await,
        ["A"],
        "the warning still in force is not dropped"
    );
    let retry: DateTime<Utc> = app
        .one("SELECT retry_at FROM cap_documents WHERE url LIKE '%/ma/u'")
        .await;
    assert!(
        retry < Utc::now() + ChronoDuration::minutes(31),
        "retried soon: {retry}"
    );

    // Once fetched (here by a restarted server, which reads the retry time
    // from the database), the update replaces it.
    mock.put_cap("/ma/u", msg("U", 10, vec![("A", t0)], 50.0));
    app.exec("UPDATE cap_documents SET retry_at = now() WHERE url LIKE '%/ma/u'")
        .await;
    let restarted = app.restarted_with(Some(&mock));
    restarted.world_poll_once().await.unwrap();
    assert_eq!(in_effect(&app).await, ["U"]);

    // An empty feed ends nothing; one listing without U is not enough;
    // two in a row are.
    list(&[]);
    restarted.world_poll_once().await.unwrap();
    assert_eq!(in_effect(&app).await, ["U"], "an empty feed is no evidence");
    mock.put_cap("/ma/b", msg("B", 0, vec![], 40.0));
    list(&["b"]);
    restarted.world_poll_once().await.unwrap();
    assert_eq!(
        in_effect(&app).await,
        ["B", "U"],
        "missing from one listing"
    );
    list(&["b"]);
    restarted.world_poll_once().await.unwrap();
    assert_eq!(
        in_effect(&app).await,
        ["B"],
        "missing from two listings in a row"
    );
}

/// Alerts without a shape are in the lists, so their coming and going
/// raises the revision too.
#[sqlx::test(migrations = "../../migrations")]
async fn shapeless_alerts_raise_the_revision(pool: PgPool) {
    let mock = MockWorld::start().await;
    let mut app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "cn-cma-xx", "cn", "/cn/rss.xml")],
    )
    .await;
    let shapeless = |id: &'static str, msg_type: &'static str, refs| {
        let mut c = Cap::alert(id);
        c.sender = "cma@example.cn";
        c.event = if id.starts_with('E') {
            "Heat"
        } else {
            "Rainstorm"
        };
        c.msg_type = msg_type;
        c.references = refs;
        c.geocodes = vec![("CPEAS Geographic Code", "530826")];
        c.xml()
    };
    async fn listed(app: &App) -> usize {
        app.get_json("/api/alerts?country=cn").await["features"]
            .as_array()
            .unwrap()
            .len()
    }
    serve_feed(
        &mock,
        "cn",
        &[
            ("r1", shapeless("R-1", "Alert", vec![])),
            ("e1", shapeless("E-1", "Alert", vec![])),
        ],
    );
    app.world_poll().await;
    assert_eq!(listed(&app).await, 2);
    let rev = app.alerts.revision().await;
    app.alert_revisions();

    // One is cancelled.
    let cancel = shapeless(
        "R-2",
        "Cancel",
        vec![("R-1", Utc::now() - ChronoDuration::minutes(5))],
    );
    serve_feed(&mock, "cn", &[("r2", cancel)]);
    app.world_poll().await;
    let rev2 = app.alerts.revision().await;
    assert!(
        rev2 > rev,
        "a cancellation of an alert without a shape is a change"
    );
    assert_eq!(app.alert_revisions(), [rev2]);
    assert_eq!(listed(&app).await, 1);

    // The other runs out.
    app.exec(
        "UPDATE alerts SET expires = now() - interval '1 second' WHERE cap_identifier = 'E-1'",
    )
    .await;
    app.world_poll().await;
    assert!(app.alerts.revision().await > rev2, "and so is its expiry");
    assert_eq!(listed(&app).await, 0);
}

/// Hazards stored as several threads, or with a message in force that is
/// no longer in effect, are put back together when the server starts.
#[sqlx::test(migrations = "../../migrations")]
async fn hazards_stored_as_several_threads_are_rejoined_at_start(pool: PgPool) {
    let mock = MockWorld::start().await;
    let app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "no-met-en", "no", "/no/rss.xml")],
    )
    .await;
    let t0 = Utc::now() - ChronoDuration::minutes(40);
    let msg = |id, minutes, refs, lat| {
        let mut c = threaded(id, "meteorologi@met.no", t0, minutes, refs, (lat, 5.0, 0.2));
        c.reference_sender = Some("noreply@met.no");
        c.xml()
    };
    serve_feed(
        &mock,
        "no",
        &[
            ("v1", msg("V-1", 0, vec![], 60.0)),
            ("v2", msg("V-2", 10, vec![("V-1", t0)], 60.1)),
            ("d1", msg("D-1", 0, vec![], 70.0)),
            ("d2", msg("D-2", 5, vec![], 70.0)),
            ("x1", msg("X-1", 0, vec![], 65.0)),
        ],
    );
    app.world_poll().await;
    // Every message its own hazard, all in force, and one message whose
    // blocks all said it was over still in force.
    app.exec("UPDATE alerts SET event_key = 'cap:split-' || cap_identifier, active = true, ended_at = NULL WHERE origin = 'cap'")
        .await;
    app.exec("UPDATE alerts SET in_effect = false WHERE cap_identifier = 'X-1'")
        .await;

    app.restarted_with(Some(&mock))
        .world_poll_once()
        .await
        .unwrap();
    let t = threads(&app).await;
    let row = |i: &str| t.iter().find(|r| r.0 == i).unwrap().clone();
    assert_eq!(row("V-1").1, row("V-2").1);
    assert_eq!((row("V-1").2, row("V-2").2), (false, true));
    assert_eq!(row("D-1").1, row("D-2").1, "a re-issue of the same area");
    assert_eq!((row("D-1").2, row("D-2").2), (false, true));
    assert!(!row("X-1").2, "no longer in effect");
}

/// The feed's data time is when the newest message was taken in, never a
/// `sent` time an issuer stamped ahead of the clock.
#[sqlx::test(migrations = "../../migrations")]
async fn the_feeds_data_time_is_never_in_the_future(pool: PgPool) {
    let mock = MockWorld::start().await;
    let mut ahead = Cap::alert("AHEAD-1");
    ahead.sent = Utc::now() + ChronoDuration::minutes(90);
    ahead.polygons = vec![box_ring(1.0, 1.0, 0.1)];
    serve_feed(&mock, "ahead", &[("1", ahead.xml())]);
    let app = world_app(
        pool.clone(),
        &mock,
        &[source(&mock, "xx-ahead", "xx", "/ahead/rss.xml")],
    )
    .await;
    let running = app.spawn();
    eventually("the alert stored and reported", || async {
        running
            .health
            .get("alerts-world")
            .and_then(|h| h.last_data_time)
            .is_some()
            && !alerts_of(&app.pool, "xx-ahead").await.is_empty()
    })
    .await;
    // One more report after the alert was stored.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let reported = running
        .stop()
        .await
        .get("alerts-world")
        .unwrap()
        .last_data_time
        .unwrap();
    let stored: DateTime<Utc> = app
        .one("SELECT first_seen FROM alerts WHERE cap_identifier = 'AHEAD-1'")
        .await;
    assert!(reported <= Utc::now(), "{reported} is in the future");
    assert!(
        reported >= stored - ChronoDuration::seconds(1),
        "{reported}, taken in at {stored}"
    );
}
