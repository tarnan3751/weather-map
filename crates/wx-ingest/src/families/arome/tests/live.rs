//! A live check against Météo-France's bucket, ignored by default:
//!
//! ```text
//! AROME_LIVE_OUT=/some/dir AROME_LIVE_HORIZON=6 \
//!     cargo test -p wx-ingest --features arome --release -- --ignored live_arome --nocapture
//! ```
//!
//! It runs every AROME source as the server would, into a directory store
//! with an in-memory catalog, until the downloads have been idle for a
//! minute (at most `AROME_LIVE_SECS`, default 600), with forecasts capped at
//! `AROME_LIVE_HORIZON` hours ahead (default 6, which keeps it to well under
//! a gigabyte). Then it prints what arrived, times the build of one lead per
//! source, and compares the stored grids with current airport observations
//! (aviationweather.gov's METAR cache) and with the coldest summits, which a
//! flipped or shifted grid would put somewhere else.

use super::*;
use crate::testing::live;
use crate::{Ingest, IngestConfig};
use object_store::ObjectStore;
use object_store::local::LocalFileSystem;
use std::collections::BTreeMap;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use wx_core::HealthRegistry;

const USER_AGENT: &str = "weather-map-dev";

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A stored grid's bands at a point, all of them there.
async fn sample(
    store: &dyn ObjectStore,
    layer: &str,
    part: &str,
    lon: f64,
    lat: f64,
) -> Option<Vec<f32>> {
    let path = Path::from(wx_core::keys::grid(layer, part));
    let bytes = store.get(&path).await.ok()?.bytes().await.ok()?;
    let grid = wx_pipeline::FrameGrid::from_bytes(&bytes).ok()?;
    let values = live::Sampler::new(&grid, layer, 10).at(lon, lat)?;
    values.iter().all(|v| !v.is_nan()).then_some(values)
}

struct Metar {
    station: String,
    time: DateTime<Utc>,
    lat: f64,
    lon: f64,
    temp_c: Option<f64>,
    wind_dir: Option<f64>,
    wind_kt: Option<f64>,
}

/// The newest observation of each of `stations` in aviationweather.gov's
/// cache (`raw_text,station_id,observation_time,latitude,longitude,temp_c,
/// dewpoint_c,wind_dir_degrees,wind_speed_kt,...`).
async fn metars(remote: &Remote, stations: &[&str]) -> Vec<Metar> {
    let gz = remote
        .get("https://aviationweather.gov/data/cache/metars.cache.csv.gz")
        .lane("live")
        .bytes()
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8(crate::remote::codec::gunzip(&gz, 64 << 20).unwrap()).unwrap();
    let mut out: BTreeMap<String, Metar> = BTreeMap::new();
    for line in text.lines().skip(1) {
        // The raw report is quoted; the columns after it are plain.
        let Some((_, rest)) = line.rsplit_once("\",") else {
            continue;
        };
        let c: Vec<&str> = rest.split(',').collect();
        if c.len() < 8 || !stations.contains(&c[0]) {
            continue;
        }
        let num = |s: &str| s.parse::<f64>().ok();
        let Some(time) = DateTime::parse_from_rfc3339(c[1])
            .ok()
            .map(|t| t.with_timezone(&Utc))
        else {
            continue;
        };
        let m = Metar {
            station: c[0].to_string(),
            time,
            lat: num(c[2]).unwrap_or(f64::NAN),
            lon: num(c[3]).unwrap_or(f64::NAN),
            temp_c: num(c[4]),
            wind_dir: num(c[6]),
            wind_kt: num(c[7]),
        };
        if out.get(c[0]).is_none_or(|o| o.time < m.time) {
            out.insert(c[0].to_string(), m);
        }
    }
    out.into_values().collect()
}

/// Clockwise from north, where the wind blows from.
fn direction(u: f32, v: f32) -> f64 {
    (f64::from(-u).atan2(f64::from(-v)).to_degrees() + 360.0) % 360.0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "network"]
async fn live_arome_ingest() {
    let out = std::env::var("AROME_LIVE_OUT")
        .map_or_else(|_| std::env::temp_dir().join("wx-arome-live"), Into::into);
    let horizon = env_u64("AROME_LIVE_HORIZON", 6) as u32;
    let max_secs = env_u64("AROME_LIVE_SECS", 600);
    std::fs::create_dir_all(&out).unwrap();
    let store: Arc<dyn ObjectStore> = Arc::new(LocalFileSystem::new_with_prefix(&out).unwrap());
    let catalog = Arc::new(FrameCatalog::new(Hub::new()));
    let health = HealthRegistry::new(Hub::new());
    let config = IngestConfig {
        feeds: vec!["arome".into()],
        user_agent: USER_AGENT.into(),
        horizon_hours: horizon,
        ..IngestConfig::default()
    };
    let ingest = Ingest::new(store.clone(), catalog.clone(), config).unwrap();
    println!(
        "running {:?} into {} with a {horizon} h horizon",
        ingest.task_names(),
        out.display()
    );
    let cancel = CancellationToken::new();
    let started = Instant::now();
    let handles = ingest.spawn(&health, cancel.clone());
    let total = |i: &Ingest| {
        let t = i.traffic();
        (
            t.iter().map(|t| t.bytes).sum::<u64>(),
            t.iter()
                .map(|t| t.get_requests + t.list_requests)
                .sum::<u64>(),
        )
    };
    let (mut last, mut idle) = ((0, 0), 0);
    while started.elapsed().as_secs() < max_secs {
        tokio::time::sleep(StdDuration::from_secs(15)).await;
        let now = total(&ingest);
        idle = if now == last { idle + 1 } else { 0 };
        last = now;
        println!(
            "[{:>4}s] {:.1} MB in {} requests",
            started.elapsed().as_secs(),
            now.0 as f64 / 1e6,
            now.1
        );
        if idle >= 4 && started.elapsed().as_secs() >= 120 {
            break;
        }
    }
    cancel.cancel();
    for h in handles {
        let _ = h.await;
    }
    let secs = started.elapsed().as_secs_f64();

    println!("\n== parts by layer and source ==");
    let parts = catalog.parts();
    let mut by: BTreeMap<(&str, String), Vec<&Part>> = BTreeMap::new();
    for (l, p) in &parts {
        by.entry((l, p.source.clone())).or_default().push(p);
    }
    for ((l, s), ps) in &by {
        let first = ps.iter().map(|p| p.valid).min().unwrap();
        let last = ps.iter().map(|p| p.valid).max().unwrap();
        let mut runs: Vec<String> = ps
            .iter()
            .filter_map(|p| p.run)
            .map(|r| r.format("%d %HZ").to_string())
            .collect();
        runs.sort();
        runs.dedup();
        let (first, last) = (first.format("%d %H:%MZ"), last.format("%d %H:%MZ"));
        println!(
            "{l:<10} {s:<15} {:>3} parts, {first} .. {last}, runs {runs:?}",
            ps.len()
        );
    }
    println!("\n== one part per source ==");
    for s in wx_core::source::sources_of(wx_core::Family::Arome) {
        match catalog.latest("temp", s.id) {
            Some(p) => {
                let b = p.bounds;
                let text = |x: wx_core::LonLatBox| {
                    format!(
                        "{:.3},{:.3} .. {:.3},{:.3}",
                        x.west, x.south, x.east, x.north
                    )
                };
                let solid = p.solid.map_or("none".into(), text);
                println!(
                    "{:<15} temp/{}: bounds {}; solid {solid}",
                    s.id,
                    p.id,
                    text(b)
                );
            }
            None => println!("{:<15} no temperature part", s.id),
        }
    }
    println!("\n== tasks ==");
    for h in health.snapshot() {
        let error = h
            .last_error
            .map_or(String::new(), |e| format!(" | last error: {e}"));
        println!(
            "{:<15} {:?} restarts={} | {}{error}",
            h.name, h.state, h.restarts, h.detail
        );
    }
    println!("\n== traffic in {secs:.0} s ==");
    for t in ingest.traffic() {
        println!(
            "{:<45} {:>4} lists {:>5} gets {:>3} not found {:>3} retries {:>8.1} MB",
            t.host,
            t.list_requests,
            t.get_requests,
            t.not_found,
            t.retries,
            t.bytes as f64 / 1e6
        );
    }

    // Build times: one lead of the newest run of each source, every layer,
    // through a fresh adapter (so its files are fetched again) and an
    // engine of its own.
    println!("\n== build time per part ==");
    let remote = crate::testing::live::remote();
    let mem_store = Arc::new(InMemory::new());
    let timing = Arc::new(Engine::new(
        mem_store,
        Arc::new(FrameCatalog::new(Hub::new())),
        IngestConfig::default(),
        remote.clone(),
        LAYERS,
    ));
    let a = Arome::default();
    let now = Utc::now();
    for s in wx_core::source::sources_of(wx_core::Family::Arome) {
        let Some(p) = live::nearest(&catalog, "temp", s.id, now) else {
            continue;
        };
        let (Some(r), lead) = (
            p.run,
            ((p.valid - p.run.unwrap_or(p.valid)).num_minutes() as u32).max(60),
        ) else {
            continue;
        };
        let jobs: Vec<PartJob> = crate::driver::inputs_of(LAYERS, s)
            .into_iter()
            .filter_map(|(l, i)| {
                PartJob::forecast(l, i, s, r, lead, Some(lead - 60), Steps::RunTotals)
            })
            .collect();
        let listing = a
            .list(&Cx::new(&remote, "live", now), s, r, &[PRODUCT])
            .await
            .unwrap()
            .unwrap();
        let mut files: BTreeMap<String, Vec<(usize, usize, Need)>> = BTreeMap::new();
        for (j, job) in jobs.iter().enumerate() {
            for (k, n) in job.needs().into_iter().enumerate() {
                files.entry(a.file(s, r, &n)).or_default().push((j, k, n));
            }
        }
        let mut payloads: Vec<Vec<Option<Payload>>> =
            jobs.iter().map(|j| vec![None; j.needs().len()]).collect();
        let fetch_started = Instant::now();
        for (name, needs) in &files {
            let file = ModelFile {
                name: name.clone(),
                size: listing.size(name).flatten(),
            };
            let ns: Vec<Need> = needs.iter().map(|(_, _, n)| *n).collect();
            if let Fetched::Got(got) = a
                .fetch(&Cx::new(&remote, "live", now), s, r, &file, &ns)
                .await
                .unwrap()
            {
                for ((j, k, _), p) in needs.iter().zip(got) {
                    payloads[*j][*k] = p;
                }
            }
        }
        let fetched = fetch_started.elapsed();
        let (when, secs) = (r.format("%d %HZ"), fetched.as_secs_f64());
        let mut line = format!(
            "{:<15} {when} +{:>2}h (fetch {secs:.1} s):",
            s.id,
            lead / 60
        );
        for (job, p) in jobs.iter().zip(payloads) {
            let Some(p) = p.into_iter().collect::<Option<Vec<Payload>>>() else {
                line.push_str(&format!(" {} -", job.layer.id));
                continue;
            };
            let t0 = Instant::now();
            let produced = timing
                .produce(job, p, "live", crate::engine::Moment::starting(Utc::now()))
                .await;
            let ms = t0.elapsed().as_millis();
            line.push_str(&format!(
                " {} {ms} ms{}",
                job.layer.id,
                if produced.is_ok() { "" } else { " (failed)" }
            ));
        }
        println!("{line}");
    }

    // Against the airports and the summits.
    println!("\n== temperature and wind against METARs ==");
    let stations = [
        "LFPG", "LFPO", "LFML", "LFRB", "LFBO", "LFSB", "EGLL", "LEMD", "EDDF", "LIRF", "TFFF",
        "TFFR", "TJSJ", "SOCA", "SMJP", "FMEE", "FMEP", "FIMP", "FMCZ", "FMMI", "NWWW", "NWWM",
        "NVVV", "NTAA", "NTTB",
    ];
    let obs = metars(&remote, &stations).await;
    let (mut compared, mut close) = (0, 0);
    for m in &obs {
        let off = |p: &Part| (p.valid - m.time).num_seconds().abs();
        let mut best: Option<(&'static SourceDef, Part)> = None;
        for s in wx_core::source::sources_of(wx_core::Family::Arome) {
            if let Some(p) = live::nearest(&catalog, "temp", s.id, m.time)
                && p.bounds.contains(m.lon, m.lat)
                && best.as_ref().is_none_or(|(_, b)| off(&p) < off(b))
            {
                best = Some((s, p));
            }
        }
        let Some((s, p)) = best else {
            println!("{} ({:.2}, {:.2}): no AROME part", m.station, m.lat, m.lon);
            continue;
        };
        let Some(t) = sample(store.as_ref(), "temp", &p.id, m.lon, m.lat).await else {
            continue;
        };
        let wind = match live::nearest(&catalog, "wind", s.id, m.time) {
            Some(w) => sample(store.as_ref(), "wind", &w.id, m.lon, m.lat).await,
            None => None,
        };
        let dt = m.temp_c.map(|o| f64::from(t[0]) - o);
        if let Some(d) = dt {
            compared += 1;
            close += usize::from(d.abs() <= 4.0);
        }
        let wind_text = match (wind, m.wind_dir, m.wind_kt) {
            (Some(w), Some(dir), Some(kt)) => {
                let model = direction(w[0], w[1]);
                let off = ((model - dir + 540.0) % 360.0) - 180.0;
                format!(
                    "wind {:>3.0} deg {:>4.1} m/s vs {dir:>3.0} deg {:>4.1} m/s (off {off:>4.0})",
                    model,
                    f64::from(w[0].hypot(w[1])),
                    kt * 0.514
                )
            }
            _ => String::new(),
        };
        println!(
            "{} {:<15} {} obs {} model {}: {:>5.1} C vs {:>5.1} C (diff {:>4.1}) {wind_text}",
            m.station,
            s.id,
            p.id,
            m.time.format("%H:%MZ"),
            p.valid.format("%H:%MZ"),
            t[0],
            m.temp_c.unwrap_or(f64::NAN),
            dt.unwrap_or(f64::NAN)
        );
    }
    println!("{close} of {compared} within 4 C");

    println!("\n== summits against their coasts ==");
    let summits = [
        (
            "arome_fr",
            "Mont Blanc",
            6.8652,
            45.8326,
            "Geneva",
            6.109,
            46.238,
        ),
        (
            "arome_indien",
            "Piton des Neiges",
            55.4783,
            -21.0989,
            "Saint-Denis",
            55.51,
            -20.887,
        ),
        (
            "arome_polyn",
            "Mont Orohena",
            -149.4883,
            -17.6206,
            "Papeete",
            -149.607,
            -17.554,
        ),
        (
            "arome_antilles",
            "La Soufrière",
            -61.6636,
            16.0442,
            "Pointe-à-Pitre",
            -61.532,
            16.265,
        ),
        (
            "arome_ncaled",
            "Mont Humboldt",
            166.4167,
            -21.8833,
            "Nouméa",
            166.47,
            -22.26,
        ),
    ];
    let mut colder = 0;
    for (id, peak, plon, plat, coast, clon, clat) in summits {
        let Some(p) = live::nearest(&catalog, "temp", id, now) else {
            continue;
        };
        let top = sample(store.as_ref(), "temp", &p.id, plon, plat).await;
        let low = sample(store.as_ref(), "temp", &p.id, clon, clat).await;
        let (Some(top), Some(low)) = (top, low) else {
            continue;
        };
        println!(
            "{id:<15} {}: {peak} {:>5.1} C, {coast} {:>5.1} C",
            p.id, top[0], low[0]
        );
        colder += usize::from(top[0] + 5.0 < low[0]);
    }
    assert!(
        compared > 0 && close * 10 >= compared * 7,
        "{close} of {compared} within 4 C"
    );
    assert!(colder >= 4, "the summits are colder than their coasts");
}
