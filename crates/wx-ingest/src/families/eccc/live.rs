//! Checks against the real Datamart. They need the network, so they are
//! ignored by default: `cargo test -p wx-ingest --features eccc --release
//! -- --ignored live_eccc --nocapture`.
//!
//! `live_eccc_step_timings` lists the newest runs and builds every part of
//! one step per model (about 30 MB). `live_eccc_sample` reads grids stored
//! by a live run of the `ingest_once` example and prints their values at
//! places, for comparing with observations.

use crate::driver::{Cx, Fetched, Model, ModelFile, Need, Payload, inputs_of};
use crate::families::{gdps, hrdps};
use chrono::{DateTime, Duration, Utc};
use std::time::Instant;
use wx_core::{LAYERS, SourceDef, source};

/// The newest run of `model` whose step `hour` has every file the registry
/// reads, with that step's needs grouped by file.
async fn newest_step(
    cx: &Cx<'_>,
    model: &dyn Model,
    s: &SourceDef,
    hour: u32,
) -> (DateTime<Utc>, Vec<(String, Option<u64>, Vec<Need>)>) {
    let runs = model.cycle(s).runs(cx.now, Duration::hours(24));
    for run in runs {
        let product = inputs_of(LAYERS, s)[0].1.product;
        if model.leads(s, product, run).is_empty() {
            continue;
        }
        let Some(listing) = model.list(cx, s, run, &[product]).await.unwrap() else {
            continue;
        };
        let mut files: Vec<(String, Option<u64>, Vec<Need>)> = Vec::new();
        let mut complete = true;
        for (_, input) in inputs_of(LAYERS, s) {
            for f in input.fields {
                let need = Need {
                    input,
                    field: Some(crate::driver::fetched_as(*f, model.steps(s, product))),
                    lead: hour * 60,
                };
                let name = model.file(s, run, &need);
                let Some(size) = listing.size(&name) else {
                    complete = false;
                    continue;
                };
                match files.iter_mut().find(|(n, ..)| *n == name) {
                    Some((.., needs)) => needs.push(need),
                    None => files.push((name, size, vec![need])),
                }
            }
        }
        if complete {
            return (run, files);
        }
    }
    panic!("{}: no complete step {hour} in a day", s.id);
}

#[tokio::test]
#[ignore = "network"]
async fn live_eccc_step_timings() {
    let remote = crate::testing::live::remote();
    let cx = Cx::new(&remote, "live", Utc::now());
    let models: [(&str, Box<dyn Model>); 2] = [
        ("gdps", Box::new(gdps::Gdps::default())),
        ("hrdps", Box::new(hrdps::Hrdps::default())),
    ];
    for (id, model) in models {
        let s = source::source(id).unwrap();
        let (run, files) = newest_step(&cx, model.as_ref(), s, 6).await;
        let mut payloads: Vec<(Need, Payload)> = Vec::new();
        let mut bytes = 0;
        let started = Instant::now();
        for (name, size, needs) in &files {
            let file = ModelFile {
                name: name.clone(),
                size: *size,
            };
            let Fetched::Got(got) = model.fetch(&cx, s, run, &file, needs).await.unwrap() else {
                panic!("{name}: missing");
            };
            for (k, (n, p)) in needs.iter().zip(got).enumerate() {
                let p = p.unwrap_or_else(|| panic!("{name}: no payload"));
                if let (0, Payload::Custom { bytes: b, .. }) = (k, &p) {
                    bytes += b.len();
                }
                payloads.push((*n, p));
            }
        }
        eprintln!(
            "{id} run {run} step 6: {} files, {:.1} MB in {:?}",
            files.len(),
            bytes as f64 / 1e6,
            started.elapsed()
        );
        for (layer, input) in inputs_of(LAYERS, s) {
            let started = Instant::now();
            let fields: Vec<wx_core::Field> = input
                .fields
                .iter()
                .map(|f| {
                    let (n, p) = payloads
                        .iter()
                        .find(|(n, _)| n.input == input && n.field.unwrap().var == f.var)
                        .unwrap();
                    let Payload::Custom { bytes, decode } = p else {
                        unreachable!()
                    };
                    decode(bytes, n).unwrap().field
                })
                .collect();
            let decoded = started.elapsed();
            let raster = wx_pipeline::derive(layer, input, &fields).unwrap();
            let grid = wx_pipeline::FrameGrid::build(layer, &raster);
            let stored = grid.to_bytes().len();
            eprintln!(
                "  {:<10} decode {:>6.0} ms, derive and grid {:>6.0} ms, stored {:.1} MB, solid {:?}",
                layer.id,
                decoded.as_secs_f64() * 1e3,
                (started.elapsed() - decoded).as_secs_f64() * 1e3,
                stored as f64 / 1e6,
                grid.solid()
            );
        }
    }
    eprintln!("{:?}", remote.traffic());
}

/// `ECCC_STORE`: the `--out` directory of an `ingest_once` run;
/// `ECCC_VALID`: the valid time to read, `YYMMDDHH`.
#[tokio::test]
#[ignore = "reads a live run's store"]
async fn live_eccc_sample() {
    let dir = std::env::var("ECCC_STORE").expect("ECCC_STORE");
    let valid = std::env::var("ECCC_VALID").expect("ECCC_VALID");
    let valid = chrono::NaiveDateTime::parse_from_str(&format!("{valid}00"), "%y%m%d%H%M")
        .unwrap()
        .and_utc();
    let places = [
        ("CYYZ Toronto", -79.63, 43.68),
        ("CYUL Montreal", -73.74, 45.47),
        ("CYVR Vancouver", -123.18, 49.19),
        ("CYYC Calgary", -114.02, 51.13),
        ("CYWG Winnipeg", -97.24, 49.91),
        ("CYQX Gander", -54.57, 48.94),
        ("KMSP Minneapolis", -93.22, 44.88),
        ("EGLL London", -0.46, 51.48),
        ("RJTT Tokyo", 139.78, 35.55),
        ("YSSY Sydney", 151.18, -33.95),
        ("SBGR Sao Paulo", -46.47, -23.43),
    ];
    for id in ["gdps", "hrdps"] {
        for layer_id in ["temp", "wind"] {
            // The newest run's part at that valid time.
            let prefix = format!("{dir}/{}", wx_core::keys::layer_grid_prefix(layer_id));
            let mut best: Option<(i64, std::path::PathBuf)> = None;
            for e in std::fs::read_dir(&prefix).unwrap() {
                let path = e.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                let Some(rest) = name.strip_prefix(&format!("{id}-")) else {
                    continue;
                };
                let mut it = rest.trim_end_matches(".wxg").split('-');
                let (Some(run), Some(lead)) = (it.next(), it.next()) else {
                    continue;
                };
                let run = chrono::NaiveDateTime::parse_from_str(&format!("{run}00"), "%y%m%d%H%M")
                    .unwrap()
                    .and_utc();
                let lead: i64 = lead.parse().unwrap();
                if run + Duration::minutes(lead) == valid
                    && best.as_ref().is_none_or(|(r, _)| run.timestamp() > *r)
                {
                    best = Some((run.timestamp(), path));
                }
            }
            let Some((_, path)) = best else {
                eprintln!("{id} {layer_id}: no part valid at {valid}");
                continue;
            };
            let grid = wx_pipeline::FrameGrid::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
            let mut sampler = crate::testing::live::Sampler::new(&grid, layer_id, 8);
            eprintln!(
                "{id} {layer_id} {}:",
                path.file_name().unwrap().to_string_lossy()
            );
            for (name, lon, lat) in places {
                let bands = sampler
                    .at(lon, lat)
                    .filter(|b| !b.is_empty() && !b[0].is_nan());
                match layer_id {
                    "temp" => eprintln!("  {name:<18} {:?} C", bands.map(|b| b[0])),
                    _ => {
                        if let Some([u, v, ..]) = bands.as_deref() {
                            let (u, v) = (*u, *v);
                            let from = (-u).atan2(-v).to_degrees().rem_euclid(360.0);
                            eprintln!(
                                "  {name:<18} from {from:5.0} deg at {:.1} m/s ({:.0} kt)",
                                u.hypot(v),
                                u.hypot(v) * 1.943_84
                            );
                        } else {
                            eprintln!("  {name:<18} none");
                        }
                    }
                }
            }
        }
    }
}
