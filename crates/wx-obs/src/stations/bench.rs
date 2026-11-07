//! Full-size timings for `/api/stations` and the METAR parser: the fixtures
//! scaled up to the real feeds (about 5,000 airports, 850 buoys and 1,300
//! AMeDAS stations, 2.5 MB of JSON). Not run by default:
//! `cargo test --release -p wx-obs --lib bench -- --ignored --nocapture`

use super::*;
use crate::{Obs, ObsConfig};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use flate2::{Compression, write::GzEncoder};
use std::io::Write;
use std::time::Duration;
use tower::ServiceExt;
use tower_http::compression::CompressionLayer;

type Area = ((f64, f64), (f64, f64));
const WORLD: Area = ((-60.0, 75.0), (-180.0, 180.0));
const CONUS: Area = ((24.0, 50.0), (-125.0, -66.0));
const EUROPE: Area = ((35.0, 60.0), (-10.0, 30.0));
const JAPAN: Area = ((24.0, 46.0), (123.0, 146.0));

/// A deterministic pseudo-random value in `lo..hi` for each `seed` (splitmix64).
fn spread(seed: usize, (lo, hi): (f64, f64)) -> f64 {
    let mut z = (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    lo + (hi - lo) * ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}

/// The fixture's rows under new ids and positions until `n` are located:
/// about half in the US and a fifth in Europe, as in the real network.
fn metar_csv(n: usize) -> Vec<u8> {
    let fixture = &include_bytes!("../../tests/fixtures/metars.csv")[..];
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(fixture);
    let rows: Vec<csv::StringRecord> = reader.records().map(Result::unwrap).collect();
    let mut out = csv::WriterBuilder::new()
        .flexible(true)
        .from_writer(Vec::new());
    out.write_record(&rows[0]).unwrap();
    let mut located = 0;
    for (i, row) in rows[1..].iter().cycle().enumerate() {
        if located == n {
            break;
        }
        let mut f: Vec<String> = row.iter().map(String::from).collect();
        f[1] = format!("K{i:04}");
        if f[3] != "-99.9900" {
            located += 1;
            let (lat, lon) = match i % 20 {
                0..=8 => CONUS,
                9..=12 => EUROPE,
                _ => WORLD,
            };
            // Varied values, so the JSON compresses like the real thing.
            let temps = [
                (5, (-30.0, 35.0), 1),
                (6, (-35.0, 25.0), 1),
                (11, (29.0, 31.0), 2),
            ];
            let wind = [(7, (10.0, 360.0), 0), (8, (1.0, 30.0), 0)];
            for (col, range, decimals) in [(3, lat, 4), (4, lon, 4)]
                .into_iter()
                .chain(temps)
                .chain(wind)
            {
                f[col] = format!("{:.*}", decimals, spread(i * 16 + col, range));
            }
        }
        out.write_record(&f).unwrap();
    }
    out.into_inner().unwrap()
}

/// Every report minutes old, so nothing ages out during the run, and named.
fn fresh(mut v: Vec<Observation>) -> Vec<Observation> {
    for (i, o) in v.iter_mut().enumerate() {
        o.observed = Utc::now() - chrono::Duration::minutes(5 + i as i64 % 55);
        o.name = Some(format!("Station {i}, Somewhere"));
    }
    v
}

/// `n` stations like `base`'s, spread over `area`.
fn scaled(
    base: anyhow::Result<Vec<Observation>>,
    n: usize,
    prefix: &str,
    (lat, lon): Area,
) -> Vec<Observation> {
    let base = base.unwrap();
    let tenths = |seed, range| Some((spread(seed, range) * 10.0).round() / 10.0);
    let station = |i: usize| Observation {
        id: format!("{prefix}:{i:05}"),
        lat: spread(i * 8, lat),
        lon: spread(i * 8 + 1, lon),
        temp_c: tenths(i * 8 + 2, (-30.0, 35.0)),
        altimeter_hpa: tenths(i * 8 + 3, (980.0, 1040.0)),
        ..base[i % base.len()].clone()
    };
    fresh((0..n).map(station).collect())
}

/// The median of 25 timings after a warm-up.
async fn median<F: FnMut() -> Fut, Fut: Future>(mut f: F) -> Duration {
    f().await;
    let mut t = Vec::new();
    for _ in 0..25 {
        let started = Instant::now();
        f().await;
        t.push(started.elapsed());
    }
    t.sort();
    t[t.len() / 2]
}

/// A browser's request, optionally conditional: the status, ETag and bytes sent.
async fn get(app: &axum::Router, uri: &str, etag: &str) -> (u16, String, usize) {
    let req = Request::get(uri).header(header::ACCEPT_ENCODING, "gzip, deflate, br");
    let req = req.header(header::IF_NONE_MATCH, etag).body(Body::empty());
    let resp = app.clone().oneshot(req.unwrap()).await.unwrap();
    let etag = resp
        .headers()
        .get(header::ETAG)
        .map(|v| v.to_str().unwrap().to_string());
    let status = resp.status().as_u16();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, etag.unwrap_or_default(), body.len())
}

fn ms(d: Duration) -> String {
    format!("{:9.3} ms", d.as_secs_f64() * 1e3)
}

fn gzip(body: &[u8], level: u32) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::new(level));
    e.write_all(body).unwrap();
    e.finish().unwrap()
}

#[tokio::test]
#[ignore = "a benchmark: run in release with --ignored --nocapture"]
async fn bench_stations() {
    let at = |t: &str| t.parse::<DateTime<Utc>>().unwrap();
    let (csv, now) = (metar_csv(5_000), at("2026-11-06T12:30:00Z"));
    let gz = gzip(&csv, 6);
    let plain = median(|| async { metar::parse_metars(&csv, now) }).await;
    let gzipped = median(|| async { metar::parse_metars(&gz, now) }).await;
    let airports = fresh(metar::parse_metars(&csv, now).unwrap());
    println!(
        "parse_metars, {} bytes, {} airports:",
        csv.len(),
        airports.len()
    );
    println!("  plain {}, gzipped {}", ms(plain), ms(gzipped));

    let buoys = ndbc::parse_ndbc(
        include_bytes!("../../tests/fixtures/ndbc.txt"),
        at("2026-11-06T18:00:00Z"),
    );
    let table =
        amedas::parse_table(include_bytes!("../../tests/fixtures/amedastable.json")).unwrap();
    let map = include_bytes!("../../tests/fixtures/amedas_map.json");
    let jp = amedas::parse_snapshot(
        map,
        &table,
        at("2026-11-06T18:10:00Z"),
        at("2026-11-06T18:10:00Z"),
    );
    let obs = Obs::new(wx_core::Hub::new(), ObsConfig::default(), None).unwrap();
    let store = &obs.inner.stations;
    store.set(Source::Ndbc, scaled(buoys, 850, "ndbc", WORLD));
    store.set(Source::Amedas, scaled(jp, 1_300, "amedas", JAPAN));
    let set = median(|| async { store.set(Source::Metar, airports.clone()) }).await;
    let rebuild = median(|| async { merge(&store.feeds.lock().unwrap(), Utc::now()) }).await;
    println!(
        "airports into the store {}, rebuild after an age-out {}",
        ms(set),
        ms(rebuild)
    );

    let (body, features) = (
        store.current().snapshot.body.clone(),
        store.current().all.len(),
    );
    println!("whole world: {features} features, {} bytes", body.len());
    for level in [1, 6, 9] {
        let (t, size) = (
            median(|| async { gzip(&body, level) }).await,
            gzip(&body, level).len(),
        );
        println!("  gzip level {level}: {} -> {size} bytes", ms(t));
    }

    let (direct, layered) = (obs.router(), obs.router().layer(CompressionLayer::new()));
    println!(
        "{:<34}{:>13}{:>13}{:>22}",
        "Accept-Encoding: gzip", "router", "+gzip layer", "bytes sent"
    );
    for (label, query) in [
        ("whole world", ""),
        ("bbox CONUS", "?bbox=-125,24,-66,50"),
        ("bbox Europe", "?bbox=-10,35,30,60"),
        ("bbox Oklahoma", "?bbox=-100,34,-95,37"),
        ("bbox world, zoomed out", "?bbox=-180,-85,180,85"),
    ] {
        let uri = format!("/api/stations{query}");
        let (_, etag, plain) = get(&direct, &uri, "").await;
        let sent = get(&layered, &uri, "").await.2;
        let (a, b) = (
            median(|| get(&direct, &uri, "")).await,
            median(|| get(&layered, &uri, "")).await,
        );
        println!(
            "{label:<29}200  {}{}{:>22}",
            ms(a),
            ms(b),
            format!("{plain} / {sent}")
        );
        assert_eq!(get(&layered, &uri, &etag).await.0, 304, "{uri}");
        let (a, b) = (
            median(|| get(&direct, &uri, &etag)).await,
            median(|| get(&layered, &uri, &etag)).await,
        );
        println!("{label:<29}304  {}{}", ms(a), ms(b));
    }
}
