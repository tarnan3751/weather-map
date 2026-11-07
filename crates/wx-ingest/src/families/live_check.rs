//! A live check of model sources, run by hand: one cycle of each source
//! against the real hosts, into a store in a temporary directory with a
//! catalog of its own, then a report of what came of it. Past windows are
//! cut to three hours and the horizon to `WX_LIVE_HORIZON` hours (default 2)
//! so the downloads stay small; the store is kept for a look afterwards
//! (`WX_LIVE_DIR`, default a fresh directory under the system's).
//!
//! The values printed for each place are read back from the stored parts
//! through rendered tiles, as a browser would get them: 2 m temperature and
//! the 10 m wind (direction it blows from, true north) of the part nearest
//! the time of the check.

use crate::driver::model::ModelTask;
use crate::engine::Engine;
use crate::families::{self, Adapter};
use crate::testing::{self, live};
use crate::{IngestConfig, driver};
use chrono::Utc;
use object_store::ObjectStore;
use object_store::local::LocalFileSystem;
use std::sync::Arc;
use std::time::Instant;
use wx_core::{FrameCatalog, Hub, LAYERS, LayerSpec, Part, SOURCES, source};

/// `(name, lon, lat)`.
pub type Place = (&'static str, f64, f64);

pub async fn run(ids: &[&str], places: &[Place]) {
    let dir = std::env::var("WX_LIVE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join(format!("wx-live-{}", std::process::id())));
    std::fs::create_dir_all(&dir).unwrap();
    let horizon: u32 = std::env::var("WX_LIVE_HORIZON")
        .ok()
        .and_then(|h| h.parse().ok())
        .unwrap_or(2);
    let layers: &'static [LayerSpec] = testing::leak(
        LAYERS
            .iter()
            .map(|l| LayerSpec {
                retention_minutes: l.retention_minutes.min(180),
                ..*l
            })
            .collect(),
    );
    let store: Arc<dyn ObjectStore> = Arc::new(LocalFileSystem::new_with_prefix(&dir).unwrap());
    let catalog = Arc::new(FrameCatalog::with_registry(Hub::new(), layers, SOURCES));
    let config = IngestConfig {
        horizon_hours: horizon,
        user_agent: "weather-map-dev".into(),
        ..IngestConfig::default()
    };
    let remote = crate::testing::live::remote();
    let engine = Arc::new(Engine::new(
        store,
        catalog.clone(),
        config.clone(),
        remote,
        layers,
    ));
    println!("store {}; horizon {horizon} h", dir.display());
    for id in ids {
        let source = source::source(id).unwrap();
        let Some(Adapter::Model(model)) = families::adapter(source, &config) else {
            panic!("{id}: not a model family");
        };
        let mut task = ModelTask::new(
            engine.clone(),
            model,
            source,
            &driver::products_of(layers, source),
        );
        let started = Instant::now();
        let report = task.cycle(Utc::now(), &|_, _| {}).await;
        let secs = started.elapsed().as_secs_f64();
        match report {
            Ok(r) => println!(
                "{id}: {:?} in {secs:.1} s, newest listed {:?}",
                r.tally,
                task.newest_listed()
            ),
            Err(e) => println!("{id}: FAILED after {secs:.1} s: {e:#}"),
        }
    }
    for t in engine.remote.traffic() {
        println!(
            "traffic {}: {} listings, {} gets, {} missing, {} retries, {:.1} MB",
            t.host,
            t.list_requests,
            t.get_requests,
            t.not_found,
            t.retries,
            t.bytes as f64 / 1e6
        );
    }
    let now = Utc::now();
    for id in ids {
        for l in layers {
            let parts: Vec<Part> = catalog
                .timeline(l.id)
                .parts
                .into_iter()
                .filter(|p| p.source == *id)
                .collect();
            if parts.is_empty() {
                continue;
            }
            let (first, last) = (
                parts.iter().map(|p| p.valid).min().unwrap(),
                parts.iter().map(|p| p.valid).max().unwrap(),
            );
            println!(
                "parts {id} {}: {} from {first} to {last}",
                l.id,
                parts.len()
            );
        }
        let Some(temp) = live::nearest(&catalog, "temp", id, now) else {
            println!("{id}: no temperature part");
            continue;
        };
        println!(
            "part {id} {}: bounds {:?} solid {:?}",
            temp.id, temp.bounds, temp.solid
        );
        let wind = catalog
            .timeline("wind")
            .parts
            .into_iter()
            .find(|p| p.source == *id && p.valid == temp.valid);
        for (name, lon, lat) in places {
            let sample = |layer: &str, id: &str| {
                live::Sampler::new(&live::stored(&dir, layer, id)?, layer, 10).at(*lon, *lat)
            };
            let t = sample("temp", &temp.id);
            let w = wind.as_ref().and_then(|w| sample("wind", &w.id));
            let (dir_deg, speed, gust) = match w.as_deref() {
                Some([u, v, g, ..]) => (
                    (-u).atan2(-v).to_degrees().rem_euclid(360.0),
                    u.hypot(*v),
                    *g,
                ),
                _ => (f32::NAN, f32::NAN, f32::NAN),
            };
            println!(
                "place {id} {name} {lon:.3} {lat:.3} valid {} temp {:.2} C wind from {dir_deg:.0} at {speed:.1} m/s gust {gust:.1}",
                temp.valid.format("%Y-%m-%dT%H:%MZ"),
                t.map_or(f32::NAN, |v| v[0])
            );
        }
    }
}
