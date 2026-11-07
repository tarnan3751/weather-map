//! Run the ingest tasks against the providers' live servers for a while, into
//! a local directory with an in-memory catalog, then print what arrived:
//! parts per layer and source, the boxes of one part per source, the stored
//! grids, task health and traffic per host.
//!
//! ```text
//! cargo run -p wx-ingest --release --example ingest_once -- \
//!     --out /tmp/wx-store --secs 600 --feeds mrms,rtma,hrrr,gfs \
//!     [--hrrr-max-hour 18] [--horizon-hours 240] [--max-builds 2]
//! ```

use anyhow::Context;
use futures::TryStreamExt;
use object_store::ObjectStore;
use object_store::local::LocalFileSystem;
use object_store::path::Path;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use wx_core::{FrameCatalog, HealthRegistry, Hub, LAYERS, Part, WsEvent, keys};
use wx_ingest::{Ingest, IngestConfig};

struct Args {
    out: std::path::PathBuf,
    secs: u64,
    config: IngestConfig,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut out = std::env::temp_dir().join("wx-ingest-once");
    let mut secs = 300;
    let mut config = IngestConfig {
        user_agent: concat!(
            "weather-map-ingest/",
            env!("CARGO_PKG_VERSION"),
            " (live check)"
        )
        .to_string(),
        ..IngestConfig::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--out" => out = value()?.into(),
            "--secs" => secs = value()?.parse()?,
            "--hrrr-max-hour" => config.hrrr_max_hour = value()?.parse()?,
            "--horizon-hours" => config.horizon_hours = value()?.parse()?,
            "--max-builds" => config.max_builds = value()?.parse()?,
            "--feeds" => config.feeds = value()?.split(',').map(|f| f.trim().to_string()).collect(),
            other => anyhow::bail!("unknown argument {other:?}"),
        }
    }
    Ok(Args { out, secs, config })
}

async fn count(store: &dyn ObjectStore, prefix: &str) -> anyhow::Result<(usize, u64)> {
    let metas: Vec<_> = store.list(Some(&Path::from(prefix))).try_collect().await?;
    Ok((metas.len(), metas.iter().map(|m| m.size).sum()))
}

fn boxes(p: &Part) -> String {
    let b = p.bounds;
    let solid = p.solid.map_or("none".to_string(), |s| {
        format!(
            "{:.2},{:.2} .. {:.2},{:.2}",
            s.west, s.south, s.east, s.north
        )
    });
    format!(
        "bounds {:.2},{:.2} .. {:.2},{:.2}; solid {solid}",
        b.west, b.south, b.east, b.north
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wx_ingest=info".into()),
        )
        .init();
    let args = parse_args()?;
    std::fs::create_dir_all(&args.out)?;
    let store: Arc<dyn ObjectStore> =
        Arc::new(LocalFileSystem::new_with_prefix(&args.out)?.with_automatic_cleanup(true));
    let hub = Hub::new();
    let catalog = Arc::new(FrameCatalog::new(hub.clone()));
    let health = HealthRegistry::new(hub.clone());
    let ingest = Ingest::new(store.clone(), catalog.clone(), args.config.clone())?;

    // Show timeline changes and task health as the browsers would hear them.
    let mut events = hub.subscribe();
    let started = Instant::now();
    let printer = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(ev) => match &*ev {
                    WsEvent::Timeline {
                        layer,
                        parts,
                        frames,
                        removed,
                    } => {
                        if !parts.is_empty() || !removed.is_empty() {
                            let ids: Vec<&str> = parts.iter().map(|p| p.id.as_str()).collect();
                            println!(
                                "[{:>5.0}s] {layer:<10} {} frames changed, {} removed, new parts {ids:?}",
                                started.elapsed().as_secs_f64(),
                                frames.len(),
                                removed.len()
                            );
                        }
                    }
                    WsEvent::Feed { feed } => println!(
                        "[{:>5.0}s] task {:<18} {:?} {}{}",
                        started.elapsed().as_secs_f64(),
                        feed.name,
                        feed.state,
                        feed.detail,
                        feed.last_error
                            .as_ref()
                            .map_or(String::new(), |e| format!(" | last error: {e}"))
                    ),
                    _ => {}
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    println!("(missed {n} events)")
                }
                Err(_) => return,
            }
        }
    });

    let cancel = CancellationToken::new();
    println!(
        "running {:?} for {}s into {}",
        ingest.task_names(),
        args.secs,
        args.out.display()
    );
    let handles = ingest.spawn(&health, cancel.clone());
    let meter = {
        let ingest = ingest.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            tick.tick().await;
            loop {
                tick.tick().await;
                let mb: f64 = ingest.traffic().iter().map(|t| t.bytes as f64).sum::<f64>() / 1e6;
                println!(
                    "[{:>5.0}s] downloaded {mb:.1} MB so far",
                    started.elapsed().as_secs_f64()
                );
            }
        })
    };
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(args.secs)) => {}
        _ = tokio::signal::ctrl_c() => println!("interrupted"),
    }
    cancel.cancel();
    for h in handles {
        let _ = h.await;
    }
    meter.abort();
    drop(hub);
    printer.abort();

    println!("\n== parts on the catalog, by layer and source ==");
    let mut example: BTreeMap<String, (String, Part)> = BTreeMap::new();
    for (layer, timeline) in catalog.timelines() {
        if timeline.parts.is_empty() {
            continue;
        }
        let mut by_source: BTreeMap<&str, (usize, Option<&Part>, Option<&Part>)> = BTreeMap::new();
        for p in &timeline.parts {
            let e = by_source.entry(p.source.as_str()).or_default();
            e.0 += 1;
            e.1 = Some(e.1.map_or(p, |o| if p.valid < o.valid { p } else { o }));
            e.2 = Some(e.2.map_or(p, |o| if p.valid > o.valid { p } else { o }));
            example
                .entry(p.source.clone())
                .or_insert_with(|| (layer.to_string(), p.clone()));
        }
        println!("{layer:<10} {} frames", timeline.frames.len());
        for (source, (n, first, last)) in by_source {
            println!(
                "    {source:<10} {n:>4} parts, {} .. {}",
                first.map_or(String::new(), |p| p.valid.format("%d %H:%MZ").to_string()),
                last.map_or(String::new(), |p| p.valid.format("%d %H:%MZ").to_string())
            );
        }
    }

    println!("\n== one part per source ==");
    for (source, (layer, p)) in &example {
        println!("{source:<10} {layer}/{}: {}", p.id, boxes(p));
    }

    println!("\n== stored grids ==");
    for layer in LAYERS {
        let (grids, bytes) = count(store.as_ref(), &keys::layer_grid_prefix(layer.id)).await?;
        if grids > 0 {
            println!(
                "{:<10} {grids:>4} grids, {:.1} MB",
                layer.id,
                bytes as f64 / 1e6
            );
        }
    }

    println!("\n== tasks ==");
    for h in health.snapshot() {
        println!(
            "{:<18} {:?} restarts={} failures={} data={} | {}{}",
            h.name,
            h.state,
            h.restarts,
            h.consecutive_failures,
            h.last_data_time
                .map_or("-".to_string(), |t| t.format("%d %H:%MZ").to_string()),
            h.detail,
            h.last_error
                .map_or(String::new(), |e| format!(" | last error: {e}"))
        );
    }

    println!("\n== traffic in {:.0}s ==", started.elapsed().as_secs_f64());
    let mut total = 0;
    for t in ingest.traffic() {
        total += t.bytes;
        println!(
            "{:<38} {:>4} lists {:>5} gets {:>4} not found {:>3} retries {:>9.1} MB",
            t.host,
            t.list_requests,
            t.get_requests,
            t.not_found,
            t.retries,
            t.bytes as f64 / 1e6
        );
    }
    println!("total {:.1} MB", total as f64 / 1e6);
    Ok(())
}
