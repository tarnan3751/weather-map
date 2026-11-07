//! Timings of the frame catalog: the real registry's `temp` layer (27
//! sources) filled with one run per model, about 300 valid times; and the
//! scale test's layer of 25 sources with 300 valid times each. Medians, in
//! microseconds; the digest at the end compares implementations.
//!
//! `cargo run --release -p wx-core --example catalog_bench`

use chrono::{DateTime, Duration, TimeZone, Utc};
use std::hash::{Hash, Hasher};
use std::time::Instant;
use wx_core::frame::{Part, tile_parts};
use wx_core::layer::{InputSpec, LAYERS, LayerSpec, layer};
use wx_core::proj::{LonLatBox, TileCoord, lat_to_y, lon_to_x};
use wx_core::source::{Family, SourceDef};
use wx_core::{FrameCatalog, FrameKind, Hub, SOURCES};

const LAYER: &str = "temp";

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// How long `f` takes on each of `items`, in microseconds.
fn each_us<T>(items: impl IntoIterator<Item = T>, mut f: impl FnMut(T)) -> Vec<f64> {
    items
        .into_iter()
        .map(|x| {
            let t = Instant::now();
            f(x);
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect()
}

/// The median of [`each_us`].
fn median_us<T>(items: impl IntoIterator<Item = T>, f: impl FnMut(T)) -> f64 {
    median(each_us(items, f))
}

fn leak<T>(v: Vec<T>) -> &'static [T] {
    Box::leak(v.into_boxed_slice())
}

/// The `temp` layer's parts: twelve hours of each observed source, one run
/// of each model to 48 to 300 hours, on boxes round the globe.
fn temp_parts(now: DateTime<Utc>) -> Vec<Part> {
    let spec = layer(LAYER).unwrap();
    let sources: Vec<_> = SOURCES
        .iter()
        .filter(|s| spec.input_for(s).is_some())
        .collect();
    let mut out = Vec::new();
    for (k, s) in sources.iter().enumerate() {
        let (west, south) = (
            -170.0 + (k as f64 * 47.0) % 320.0,
            -60.0 + (k as f64 * 29.0) % 100.0,
        );
        let (b, solid) = if k + 1 == sources.len() {
            (LonLatBox::WORLD, Some(LonLatBox::WORLD))
        } else {
            let solid = LonLatBox::new(west + 2.0, south + 2.0, west + 28.0, south + 23.0);
            (
                LonLatBox::new(west, south, west + 30.0, south + 25.0),
                (!k.is_multiple_of(3)).then_some(solid),
            )
        };
        if s.kind == FrameKind::Observed {
            out.extend(
                (0..12).map(|h| Part::observed(s.id, now - Duration::hours(h), b, solid, 8)),
            );
        } else {
            let horizon = [48, 60, 84, 120, 300][k % 5];
            out.extend(
                (0..=horizon)
                    .map(|h| Part::forecast(s.id, now - Duration::hours(4), h * 60, b, solid, 8)),
            );
        }
    }
    out
}

/// The scale test's layer: 25 sources (observations every 2 minutes, a fine
/// model every 15 minutes, a coarse one hourly), 300 valid times each,
/// restored, then a newer run of one fine model inserted part by part.
fn big() {
    let families = [
        (Family::Mrms, FrameKind::Observed),
        (Family::Hrrr, FrameKind::Forecast),
        (Family::Gfs, FrameKind::Forecast),
    ];
    let sources: &'static [SourceDef] = leak(
        (0..25u16)
            .map(|k| {
                let (family, kind) = families[k as usize % 3];
                let id: &'static str = Box::leak(format!("s{k:02}").into_boxed_str());
                SourceDef {
                    id,
                    family,
                    kind,
                    rank: 1000 + k,
                    ..SOURCES[0]
                }
            })
            .collect(),
    );
    let inputs = [(Family::Mrms, 6), (Family::Hrrr, 15), (Family::Gfs, 180)].map(
        |(family, hold_minutes)| InputSpec {
            family,
            hold_minutes,
            fill_past: false,
            ..LAYERS[0].inputs[0]
        },
    );
    let spec = LayerSpec {
        id: "big",
        inputs: leak(inputs.to_vec()),
        retention_minutes: 24 * 60,
        observed_tick_minutes: 2,
        anchored: false,
        ..LAYERS[0]
    };
    let catalog = FrameCatalog::with_registry(Hub::new(), leak(vec![spec]), sources);
    let now = Utc.with_ymd_and_hms(2026, 11, 6, 12, 0, 0).unwrap();
    let region = |k: usize| {
        let w = -180.0 + 14.0 * k as f64;
        let e = if w + 30.0 > 180.0 {
            w + 30.0 - 360.0
        } else {
            w + 30.0
        };
        LonLatBox::new(w, -60.0 + k as f64, e, 60.0 - k as f64)
    };
    let mut parts = Vec::new();
    for (k, src) in sources.iter().enumerate() {
        let solid = region(k).grown(-2.0);
        for i in 0..300u32 {
            parts.push(match src.family {
                Family::Mrms => Part::observed(
                    src.id,
                    now - Duration::minutes(600 - 2 * i as i64),
                    region(k),
                    solid,
                    7,
                ),
                Family::Hrrr => Part::forecast(src.id, now, 15 * i, region(k), solid, 6),
                _ => Part::forecast(src.id, now, 60 * i, region(k), solid, 4),
            });
        }
    }
    let t = Instant::now();
    catalog.restore(parts.into_iter().map(|p| ("big".to_string(), p)));
    let restore = t.elapsed().as_secs_f64() * 1e3;
    let insert = median_us(0..300u32, |i| {
        let p = Part::forecast(
            sources[1].id,
            now + Duration::minutes(60),
            15 * i,
            region(1),
            None,
            6,
        );
        catalog.insert("big", p, now).unwrap();
    });
    let tl = catalog.timeline("big");
    println!(
        "big: restore {restore:.1} ms; insert median {insert:.1} us; {} frames, {} parts",
        tl.frames.len(),
        tl.parts.len()
    );
    println!(
        "big: timeline() median {:.1} us",
        median_us(0..50, |_| drop(catalog.timeline("big")))
    );
    println!(
        "big: sweep median {:.1} us",
        median_us(1..=60, |m| catalog.sweep(now + Duration::minutes(m)))
    );
}

fn main() {
    big();
    let now = Utc.with_ymd_and_hms(2026, 11, 6, 12, 0, 0).unwrap();
    let all = temp_parts(now);
    let hub = Hub::new();
    let _rx = hub.subscribe();
    let catalog = FrameCatalog::new(hub.clone());

    // Fill: the inserts of the last third land on a layer of about 200
    // valid times or more.
    let t0 = Instant::now();
    let per = each_us(&all, |p| {
        drop(catalog.insert(LAYER, p.clone(), now).unwrap())
    });
    let fill = t0.elapsed().as_secs_f64() * 1e3;
    let tl = catalog.timeline(LAYER);
    println!(
        "fill: {} parts in {fill:.1} ms; {} frames, {} parts named",
        all.len(),
        tl.frames.len(),
        tl.parts.len()
    );
    println!(
        "insert, last third of the fill: median {:.1} us",
        median(per[per.len() * 2 / 3..].to_vec())
    );

    // A newer run of every forecast source, one part at a time, into the full
    // layer: each replaces a part on the timeline.
    let newer: Vec<Part> = all
        .iter()
        .filter_map(|p| {
            let run = p.run? + Duration::hours(1);
            let lead = (p.valid - run).num_minutes();
            (lead >= 0).then(|| Part::forecast(&p.source, run, lead as u32, p.bounds, p.solid, 8))
        })
        .take(600)
        .collect();
    let replace = median_us(&newer, |p| {
        drop(catalog.insert(LAYER, p.clone(), now).unwrap())
    });
    println!(
        "insert replacing a part on the full layer: median {replace:.1} us ({} inserts)",
        newer.len()
    );
    let refused = median_us(0..1000, |_| {
        drop(catalog.insert(LAYER, newer[0].clone(), now).unwrap())
    });
    println!("insert refused: {refused:.2} us");

    let tl = catalog.timeline(LAYER);
    let copy = median_us(0..200, |_| drop(catalog.timeline(LAYER)));
    println!(
        "timeline({LAYER}): median {copy:.1} us ({} frames, {} parts)",
        tl.frames.len(),
        tl.parts.len()
    );
    println!(
        "timelines(): median {:.1} us",
        median_us(0..50, |_| drop(catalog.timelines()))
    );
    let json = median_us(0..50, |_| {
        drop(serde_json::to_vec(&catalog.timelines()).unwrap())
    });
    println!("timelines() as JSON: median {json:.1} us");
    println!(
        "shared_timeline({LAYER}): median {:.2} us",
        median_us(0..200, |_| drop(catalog.shared_timeline(LAYER)))
    );
    let json = median_us(0..50, |_| {
        drop(serde_json::to_vec(&catalog.shared_timelines()).unwrap())
    });
    println!("shared_timelines() as JSON: median {json:.1} us");
    let frame = median_us(tl.frames.iter().step_by(3), |f| {
        drop(catalog.frame_parts(LAYER, f.valid))
    });
    println!("frame_parts: median {frame:.2} us");

    // Tile requests: for frames across the timeline, tiles at zooms 2 to 8
    // around each part's box, named as a client would.
    let by_id: std::collections::HashMap<&str, &Part> =
        tl.parts.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut requests = Vec::new();
    for f in tl.frames.iter().step_by(7) {
        let parts: Vec<Part> = f
            .parts
            .iter()
            .map(|id| by_id[id.as_str()].clone())
            .collect();
        for (k, p) in parts.iter().enumerate().step_by(3) {
            let (z, lon, lat) = (2 + (k % 7) as u8, p.bounds.west + 5.0, p.bounds.south + 5.0);
            let n = f64::from(1u32 << z);
            let coord =
                TileCoord::new(z, (lon_to_x(lon) * n) as u32, (lat_to_y(lat) * n) as u32).unwrap();
            let combo: Vec<String> = tile_parts(&parts, coord)
                .iter()
                .map(|p| p.id.clone())
                .collect();
            if !combo.is_empty() {
                requests.push((coord, combo));
            }
        }
    }
    let mut found = 0;
    let resolve = median_us(
        requests.iter().cycle().take(5 * requests.len()),
        |(coord, combo)| {
            let ids: Vec<&str> = combo.iter().map(String::as_str).collect();
            found += usize::from(catalog.resolve(LAYER, *coord, &ids, now).is_some());
        },
    );
    println!(
        "resolve: median {resolve:.2} us ({} requests, {found} resolved)",
        5 * requests.len()
    );

    // Sweeps: one a minute for two hours, which expire observations and
    // forecasts that have passed.
    let mut worst = 0.0f64;
    let sweep = median_us(1..=120, |m| {
        let t = Instant::now();
        catalog.sweep(now + Duration::minutes(m));
        worst = worst.max(t.elapsed().as_secs_f64() * 1e6);
    });
    let left = catalog.timeline(LAYER).frames.len();
    println!("sweep (every layer): median {sweep:.1} us, max {worst:.1} us; {left} frames left");

    // A digest of what the catalog holds, to compare implementations.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&catalog.timelines())
        .unwrap()
        .hash(&mut h);
    let mut held: Vec<String> = catalog
        .parts()
        .into_iter()
        .map(|(l, p)| format!("{l}/{}", p.id))
        .collect();
    held.sort();
    held.hash(&mut h);
    println!("digest {:016x}", h.finish());

    // The first read after a change builds the shared copy.
    let mut reads = Vec::new();
    for p in all.iter().filter(|p| p.run.is_some()).take(50) {
        let run = p.run.unwrap() + Duration::hours(2);
        let lead = (p.valid - p.run.unwrap()).num_minutes() as u32 - 60;
        catalog
            .insert(
                LAYER,
                Part::forecast(&p.source, run, lead, p.bounds, p.solid, 8),
                now,
            )
            .unwrap();
        reads.extend(each_us([()], |_| drop(catalog.shared_timeline(LAYER))));
    }
    println!(
        "shared_timeline({LAYER}) first read after an insert: median {:.1} us",
        median(reads)
    );
}
