//! Database work per message for a realistic batch: a few hundred NWS
//! alerts and a few hundred CAP messages applied into a throwaway database,
//! with the statements, round trips and milliseconds that took. Not part of
//! the normal run:
//!
//! DATABASE_URL=postgres://wx:wx@127.0.0.1:5433/wx \
//!   cargo test -p wx-alerts --test bench_db -- --ignored --nocapture
//!
//! Statements are what sqlx logs (target `sqlx::query`). sqlx does not log
//! `BEGIN` and `SAVEPOINT`, so one is counted for every logged `COMMIT`,
//! `RELEASE` or `ROLLBACK`; and the pool pings a connection each time it
//! hands out an idle one, so those are counted too (through
//! `before_acquire`, which runs exactly then). Round trips are the sum.

mod common;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use common::world::*;
use common::*;
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, Once};
use std::time::Instant;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};

static PINGS: AtomicUsize = AtomicUsize::new(0);
static STATEMENTS: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());

/// Records every statement sqlx logs: its first words and how long it took.
struct Statements;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Statements {
    fn enabled(&self, meta: &tracing::Metadata<'_>, _: Context<'_, S>) -> bool {
        meta.target() == "sqlx::query"
    }

    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        #[derive(Default)]
        struct Fields {
            summary: String,
            statement: String,
            secs: f64,
        }
        impl Visit for Fields {
            fn record_str(&mut self, f: &Field, v: &str) {
                match f.name() {
                    "summary" => self.summary = v.to_string(),
                    "db.statement" => self.statement = v.to_string(),
                    _ => {}
                }
            }
            fn record_f64(&mut self, f: &Field, v: f64) {
                if f.name() == "elapsed_secs" {
                    self.secs = v;
                }
            }
            fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
        }
        let mut f = Fields::default();
        event.record(&mut f);
        let sql = if f.statement.is_empty() {
            f.summary
        } else {
            f.statement
        };
        let key: Vec<&str> = sql.split_whitespace().take(7).collect();
        STATEMENTS.lock().unwrap().push((key.join(" "), f.secs));
    }
}

struct Measure {
    started: Instant,
}

impl Measure {
    fn start() -> Self {
        STATEMENTS.lock().unwrap().clear();
        PINGS.store(0, Ordering::SeqCst);
        Self {
            started: Instant::now(),
        }
    }

    fn report(self, what: &str, messages: usize) {
        let wall = self.started.elapsed().as_secs_f64() * 1000.0;
        let statements = std::mem::take(&mut *STATEMENTS.lock().unwrap());
        let pings = PINGS.load(Ordering::SeqCst);
        let begins = statements
            .iter()
            .filter(|(k, _)| {
                k.starts_with("COMMIT") || k.starts_with("RELEASE") || k.starts_with("ROLLBACK")
            })
            .count();
        let db_ms: f64 = statements.iter().map(|(_, s)| s * 1000.0).sum();
        let trips = statements.len() + begins + pings;
        println!(
            "\n{what}: {messages} messages, {} statements logged + {begins} BEGIN/SAVEPOINT + {pings} pings = {trips} round trips \
             ({:.2} per message); {db_ms:.0} ms in statements, {wall:.0} ms wall ({:.2} ms per message)",
            statements.len(),
            trips as f64 / messages.max(1) as f64,
            wall / messages.max(1) as f64,
        );
        let mut by_kind: HashMap<&str, (usize, f64)> = HashMap::new();
        for (k, s) in &statements {
            let e = by_kind.entry(k.as_str()).or_default();
            e.0 += 1;
            e.1 += s * 1000.0;
        }
        let mut kinds: Vec<_> = by_kind.into_iter().collect();
        kinds.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(b.1.1.total_cmp(&a.1.1)));
        for (k, (n, ms)) in kinds.iter().take(14) {
            println!("  {n:5} x {ms:8.1} ms  {k}");
        }
    }
}

/// Three hundred NWS alerts: polygon warnings, zone alerts and the recorded
/// feed; then a second feed in which a hundred are continued under new ids.
fn nws_feeds() -> (Vec<Value>, Vec<Value>) {
    let sent = Utc::now() - ChronoDuration::minutes(5);
    let warning = |i: usize, id: &str| {
        let (lon, lat) = (-110.0 + (i % 40) as f64 * 0.5, 30.0 + (i / 40) as f64 * 0.5);
        let (event, severity) = match i % 3 {
            0 => ("Severe Thunderstorm Warning", "Severe"),
            _ => ("Flood Advisory", "Minor"),
        };
        alert(id, event)
            .severity(severity)
            .polygon(square(lon, lat, 0.2))
            .sent(sent)
    };
    let mut first: Vec<Value> = (0..240)
        .map(|i| {
            warning(i, &format!("urn:bench:p{i}"))
                .vtec("NEW", "KOUN", "SV", "W", &format!("{i:04}"))
                .build()
        })
        .collect();
    first.extend((0..54).map(|i| {
        alert(&format!("urn:bench:z{i}"), "Winter Storm Warning")
            .zones(&[
                ("forecast", format!("OKZ{:03}", i % 9).as_str()),
                ("county", format!("OKC{:03}", i % 7).as_str()),
            ])
            .vtec("NEW", "KOUN", "WS", "W", &format!("{:04}", 5000 + i))
            .build()
    }));
    let recorded: Value =
        serde_json::from_str(include_str!("fixtures/alerts_active.json")).unwrap();
    let recorded_now = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 11, 6, 12, 30, 0).unwrap();
    for f in recorded["features"].as_array().unwrap() {
        let mut f = f.clone();
        common::shift_times(&mut f, recorded_now);
        first.push(f);
    }
    let mut second = first.clone();
    for (i, f) in second.iter_mut().take(100).enumerate() {
        *f = warning(i, &format!("urn:bench:p{i}-u"))
            .vtec("CON", "KOUN", "SV", "W", &format!("{i:04}"))
            .message_type("Update")
            .references(&format!("urn:bench:p{i}"), sent)
            .build();
    }
    (first, second)
}

/// Message `n` (1 to 4) of thread `t`: each references the ones before it.
fn thread_message(t: usize, n: usize, t0: DateTime<Utc>) -> String {
    let ids: Vec<String> = (1..=n).map(|k| format!("T{t}-{k}")).collect();
    let mut c = Cap::alert(&ids[n - 1]);
    c.sent = t0 + ChronoDuration::minutes(10 * n as i64);
    c.msg_type = if n == 1 { "Alert" } else { "Update" };
    c.references = (1..n)
        .map(|k| {
            (
                ids[k - 1].as_str(),
                t0 + ChronoDuration::minutes(10 * k as i64),
            )
        })
        .collect();
    c.polygons = vec![box_ring(-40.0 + t as f64, 20.0 + n as f64 * 0.01, 0.4)];
    c.xml()
}

fn put_docs(mock: &MockWorld, dir: &str, xmls: Vec<String>) -> Vec<String> {
    xmls.into_iter()
        .enumerate()
        .map(|(i, xml)| {
            mock.put_cap(&format!("/{dir}/{i}.xml"), xml);
            mock.url(&format!("/{dir}/{i}.xml"))
        })
        .collect()
}

/// About three hundred CAP messages from three sources: eighty threads of
/// three messages that reference each other; shapeless messages and
/// re-issues that reference nothing, in a MeteoAlarm-style feed (which
/// lists only what is in force); and recorded messages from several
/// countries.
fn world_documents(mock: &MockWorld, t0: DateTime<Utc>) -> usize {
    let a = put_docs(
        mock,
        "a",
        (0..80)
            .flat_map(|t| (1..=3).map(move |n| thread_message(t, n, t0)))
            .collect(),
    );
    mock.put_feed("/a/rss.xml", rss(&a));

    let ids: Vec<String> = (0..60).map(|i| format!("B-{i}")).collect();
    let codes: Vec<String> = (0..30).map(|i| format!("XX{i:03}")).collect();
    let b_xml = (0..60)
        .map(|i| {
            let mut c = Cap::alert(&ids[i]);
            c.sender = "ma@example.org";
            if i < 30 {
                c.event = "Heat";
                c.geocodes = vec![("EMMA_ID", codes[i].as_str())];
            } else {
                // Ten hazards, each issued three times with no references.
                c.event = "Wind";
                c.sent = t0 + ChronoDuration::minutes(((i - 30) / 10) as i64 * 15);
                c.polygons = vec![box_ring(50.0 + ((i - 30) % 10) as f64, 10.0, 0.3)];
            }
            c.xml()
        })
        .collect();
    let b = put_docs(mock, "b", b_xml);
    let entries: Vec<(String, &str)> = b.iter().map(|l| (l.clone(), "XX001")).collect();
    mock.put_feed_plain(
        "/feeds/meteoalarm-legacy-atom-benchland",
        meteoalarm_atom(&entries),
    );

    let recorded = [
        "ca_cwhx_update_1.cap",
        "ca_cwhx_update_2.cap",
        "nu_drought_circle.xml",
        "dz_three_languages.xml",
        "lk_cap11.xml",
        "au_bom_three_blocks.xml",
        "de_dwd_exclude.xml",
        "ar_spanish_update.xml",
    ];
    let c = put_docs(
        mock,
        "c",
        recorded.iter().map(|f| current_fixture(f)).collect(),
    );
    mock.put_feed("/c/rss.xml", rss(&c));
    a.len() + b.len() + c.len()
}

/// The second wave: a fourth message in every thread.
fn world_updates(mock: &MockWorld, t0: DateTime<Utc>) -> usize {
    let mut links = put_docs(
        mock,
        "a2",
        (0..80).map(|t| thread_message(t, 4, t0)).collect(),
    );
    links.extend((0..240).map(|i| mock.url(&format!("/a/{i}.xml"))));
    mock.put_feed("/a/rss.xml", rss(&links));
    80
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "a measurement, run on demand"]
async fn database_work_per_message(pool_opts: PgPoolOptions, connect_opts: PgConnectOptions) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Statements))
            .unwrap();
    });
    let pool = pool_opts
        .before_acquire(|_, _| {
            Box::pin(async {
                PINGS.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            })
        })
        .connect_with(connect_opts)
        .await
        .unwrap();

    let mock = MockWorld::start().await;
    let mut config = world_config(&mock);
    config.documents_per_poll = 1000;
    let src = |id: &'static str, feed: &str| RegSource {
        id,
        country: "xx",
        country_name: "Benchland",
        name: id,
        region: "6",
        latlng: "45.0,15.0",
        feed: mock.url(feed),
    };
    mock.put_json(
        "/register.json",
        &register(
            &mock,
            &[
                src("xx-a", "/a/rss.xml"),
                src("xx-b", "/feeds/meteoalarm-legacy-atom-benchland"),
                src("xx-c", "/c/rss.xml"),
            ],
        ),
    );
    let app = App::start_with(pool.clone(), move |c| c.world = config).await;
    // The revision and the register are read once per process: not per message.
    app.alerts.revision().await;

    let (first, second) = nws_feeds();
    let (n1, n2) = (first.len(), 100);
    app.nws.set_alerts(first);
    let m = Measure::start();
    app.alerts.poll_once().await.unwrap();
    m.report("NWS, cold (every alert new)", n1);
    app.nws.set_alerts(second);
    let m = Measure::start();
    app.alerts.poll_once().await.unwrap();
    m.report(
        "NWS, 100 alerts continued under new ids, the rest unchanged",
        n2,
    );

    let t0 = Utc::now() - ChronoDuration::hours(1);
    let w1 = world_documents(&mock, t0);
    let m = Measure::start();
    app.alerts.world_poll_once().await.unwrap();
    m.report("World, cold (every message new)", w1);
    let w2 = world_updates(&mock, t0);
    let m = Measure::start();
    app.alerts.world_poll_once().await.unwrap();
    m.report(
        "World, an update in every thread, the rest already stored",
        w2,
    );

    let active: Vec<(String, i64)> = sqlx::query_as(
        "SELECT source, count(*) FROM alerts WHERE active AND origin = 'cap' GROUP BY 1 ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        active[..2],
        [("xx-a".to_string(), 80), ("xx-b".to_string(), 40)],
        "one per thread, shapeless message and hazard"
    );
}
