//! Load generator for a running server, for measuring the serving paths.
//! Start the server with no feeds, so nothing is downloaded and the store
//! only changes by the tiles it renders:
//!
//! ```text
//! FEEDS= BIND_ADDR=127.0.0.1:8081 ./target/release/wx-server
//! cargo run --release -p wx-server --example load -- <scenario> [key=value ...]
//! ```
//!
//! Every scenario prints requests per second, a latency distribution, the
//! statuses seen, the bytes received and, given `pid=`, the server's peak
//! resident memory. Common keys: `base=http://127.0.0.1:8081`,
//! `store=data/store`, `conc=` clients, `secs=` duration, `seed=`.
//!
//! * `tiles-hot layer=radar n=200 [stored=1]`: `n` tiles of the layer's
//!   frames (with `stored=1` only ones the store already holds, so the first
//!   pass reads them from it), fetched once, then over and over for `secs`.
//! * `tiles-cold layers=temp,wind zmin=4 zmax=6 n=300`: tiles the store does
//!   not hold yet (first-time renders), spread over the layers' frames, each
//!   requested once.
//! * `get path=/api/frames/radar [revalidate=1]`: one URL over and over,
//!   optionally sending back the `ETag` it got (`If-None-Match`).
//! * `paths file=urls.txt [revalidate=1]`: the paths in a file, round robin.
//! * `alert-tiles zmin=2 zmax=8 n=400 [repeat=1]`: alert vector tiles of the
//!   current revision over the world; `repeat=1` loops over them for `secs`.
//! * `ws clients=200 secs=30`: open WebSocket connections and count what
//!   arrives.

use futures::StreamExt;
use reqwest::header::{ACCEPT_ENCODING, ETAG, HeaderMap, HeaderValue, IF_NONE_MATCH};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use wx_core::frame::tile_parts;
use wx_core::{Part, TileCoord, Timeline, keys};

struct Args(HashMap<String, String>);

impl Args {
    fn get<T: std::str::FromStr>(&self, key: &str, default: T) -> T {
        self.0
            .get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
    fn str(&self, key: &str, default: &str) -> String {
        self.0
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }
}

/// xorshift64*: reproducible picks without another dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[derive(Default)]
struct Stats {
    latencies_ms: Vec<f64>,
    statuses: BTreeMap<u16, usize>,
    bytes: u64,
    errors: usize,
}

impl Stats {
    fn merge(&mut self, other: Stats) {
        self.latencies_ms.extend(other.latencies_ms);
        for (s, n) in other.statuses {
            *self.statuses.entry(s).or_default() += n;
        }
        self.bytes += other.bytes;
        self.errors += other.errors;
    }

    fn print(&mut self, label: &str, elapsed: Duration, peak_rss_kb: Option<u64>) {
        let l = &mut self.latencies_ms;
        l.sort_by(f64::total_cmp);
        let q = |p: f64| {
            l.get(((l.len() as f64 - 1.0) * p).round() as usize)
                .copied()
                .unwrap_or(0.0)
        };
        let n = l.len();
        println!(
            "{label}: {n} requests in {:.2}s = {:.0} req/s | ms p50 {:.2} p90 {:.2} p99 {:.2} max {:.2} | statuses {:?} | errors {} | {:.1} MB received{}",
            elapsed.as_secs_f64(),
            n as f64 / elapsed.as_secs_f64(),
            q(0.5),
            q(0.9),
            q(0.99),
            q(1.0),
            self.statuses,
            self.errors,
            self.bytes as f64 / 1e6,
            peak_rss_kb.map_or(String::new(), |k| format!(
                " | peak RSS {:.0} MB",
                k as f64 / 1024.0
            )),
        );
    }
}

/// Samples the server's resident memory until dropped.
struct RssWatch {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    start: u64,
}

fn rss_kb(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

impl RssWatch {
    fn start(pid: Option<u32>) -> Option<Self> {
        let pid = pid?;
        let stop = Arc::new(AtomicBool::new(false));
        let start = rss_kb(pid).unwrap_or(0);
        let peak = Arc::new(AtomicU64::new(start));
        let (s, p) = (stop.clone(), peak.clone());
        std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                if let Some(kb) = rss_kb(pid) {
                    p.fetch_max(kb, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        Some(Self { stop, peak, start })
    }
    fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }
}

impl Drop for RssWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn client(conc: usize) -> reqwest::Client {
    reqwest::Client::builder()
        // Bodies are counted as sent; a browser would inflate them itself.
        .no_gzip()
        .pool_max_idle_per_host(conc * 2)
        .timeout(Duration::from_secs(120))
        .build()
        .expect("a client")
}

/// One request; records it in `stats` and returns its status and headers.
async fn fetch(
    http: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
    stats: &mut Stats,
) -> Option<(u16, HeaderMap)> {
    let started = Instant::now();
    let Ok(r) = http.get(url).headers(headers).send().await else {
        stats.errors += 1;
        return None;
    };
    let (status, headers) = (r.status().as_u16(), r.headers().clone());
    let Ok(body) = r.bytes().await else {
        stats.errors += 1;
        return None;
    };
    stats
        .latencies_ms
        .push(started.elapsed().as_secs_f64() * 1e3);
    *stats.statuses.entry(status).or_default() += 1;
    stats.bytes += body.len() as u64;
    Some((status, headers))
}

fn base_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
    h
}

/// Request every path once, `conc` at a time.
async fn once_each(
    http: &reqwest::Client,
    base: &str,
    paths: Vec<String>,
    conc: usize,
) -> (Stats, Duration) {
    let paths = Arc::new(paths);
    let next = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let workers: Vec<_> = (0..conc)
        .map(|_| {
            let (http, base, paths, next) =
                (http.clone(), base.to_string(), paths.clone(), next.clone());
            tokio::spawn(async move {
                let mut stats = Stats::default();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = paths.get(i) else { break };
                    fetch(&http, &format!("{base}{p}"), base_headers(), &mut stats).await;
                }
                stats
            })
        })
        .collect();
    let mut all = Stats::default();
    for w in workers {
        all.merge(w.await.unwrap());
    }
    (all, started.elapsed())
}

/// Request the paths round robin for `secs`, `conc` at a time, optionally
/// revalidating with the ETag each path answered first.
async fn for_a_while(
    http: &reqwest::Client,
    base: &str,
    paths: Vec<String>,
    conc: usize,
    secs: f64,
    revalidate: bool,
) -> (Stats, Duration) {
    let mut etags: HashMap<String, HeaderValue> = HashMap::new();
    if revalidate {
        for p in &paths {
            let mut s = Stats::default();
            if let Some((_, h)) = fetch(http, &format!("{base}{p}"), base_headers(), &mut s).await
                && let Some(e) = h.get(ETAG)
            {
                etags.insert(p.clone(), e.clone());
            }
        }
        println!(
            "revalidating {} of {} paths with their ETag",
            etags.len(),
            paths.len()
        );
    }
    let (paths, etags) = (Arc::new(paths), Arc::new(etags));
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    let started = Instant::now();
    let workers: Vec<_> = (0..conc)
        .map(|w| {
            let (http, base, paths, etags) =
                (http.clone(), base.to_string(), paths.clone(), etags.clone());
            tokio::spawn(async move {
                let mut stats = Stats::default();
                let mut i = w;
                while Instant::now() < deadline {
                    let p = &paths[i % paths.len()];
                    let mut h = base_headers();
                    if let Some(e) = etags.get(p) {
                        h.insert(IF_NONE_MATCH, e.clone());
                    }
                    fetch(&http, &format!("{base}{p}"), h, &mut stats).await;
                    i += conc;
                }
                stats
            })
        })
        .collect();
    let mut all = Stats::default();
    for w in workers {
        all.merge(w.await.unwrap());
    }
    (all, started.elapsed())
}

async fn timeline(http: &reqwest::Client, base: &str, layer: &str) -> Timeline {
    let v: serde_json::Value = http
        .get(format!("{base}/api/frames/{layer}"))
        .send()
        .await
        .expect("the server answers")
        .json()
        .await
        .expect("a timeline");
    Timeline {
        frames: serde_json::from_value(v["frames"].clone()).unwrap(),
        parts: serde_json::from_value(v["parts"].clone()).unwrap(),
    }
}

/// Tiles of `layer` at `zmin..=zmax`, spread over its frames, that some
/// frame offers at that zoom. With `store`, only tiles the store holds
/// (`true`) or has never saved (`false`). Returns URL paths.
fn pick_tiles(
    tl: &Timeline,
    layer: &str,
    zs: (u8, u8),
    n: usize,
    store: Option<(&PathBuf, bool)>,
    rng: &mut Rng,
    seen: &mut HashSet<String>,
) -> Vec<String> {
    let by_id: HashMap<&str, &Part> = tl.parts.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut out = Vec::new();
    if tl.frames.is_empty() {
        return out;
    }
    let mut tries = 0;
    while out.len() < n && tries < n * 400 {
        tries += 1;
        let frame = &tl.frames[rng.below(tl.frames.len() as u64) as usize];
        let parts: Vec<Part> = frame
            .parts
            .iter()
            .filter_map(|id| by_id.get(id.as_str()).map(|p| (*p).clone()))
            .collect();
        let z = zs.0 + rng.below(u64::from(zs.1 - zs.0) + 1) as u8;
        let side = 1u64 << z;
        let coord = TileCoord::new(z, rng.below(side) as u32, rng.below(side) as u32).unwrap();
        let stack = tile_parts(&parts, coord);
        if stack.is_empty() || stack.iter().map(|p| p.max_zoom).max().unwrap_or(0) < z {
            continue;
        }
        let ids: Vec<&str> = stack.iter().map(|p| p.id.as_str()).collect();
        let key = keys::tile(layer, &ids, coord);
        if !seen.insert(key.clone()) {
            continue;
        }
        if let Some((store, want_stored)) = store
            && store.join(&key).exists() != want_stored
        {
            continue;
        }
        out.push(format!(
            "/tiles/{layer}/{}/{}/{}/{}.wxt",
            coord.z,
            coord.x,
            coord.y,
            ids.join(",")
        ));
    }
    out
}

#[tokio::main]
async fn main() {
    let mut argv = std::env::args().skip(1);
    let scenario = argv.next().unwrap_or_else(|| {
        eprintln!("usage: load <tiles-hot|tiles-cold|get|paths|alert-tiles|ws> [key=value ...]");
        std::process::exit(2);
    });
    let args = Args(
        argv.filter_map(|a| {
            a.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect(),
    );
    let base = args.str("base", "http://127.0.0.1:8081");
    let conc: usize = args.get("conc", 16);
    let secs: f64 = args.get("secs", 10.0);
    let pid: Option<u32> = args.0.get("pid").and_then(|p| p.parse().ok());
    let store = PathBuf::from(args.str("store", "data/store"));
    let mut rng = Rng(args.get("seed", 0x9e37_79b9_7f4a_7c15u64) | 1);
    let http = client(conc);

    match scenario.as_str() {
        "tiles-hot" => {
            let layer = args.str("layer", "radar");
            let tl = timeline(&http, &base, &layer).await;
            let mut seen = HashSet::new();
            // `stored=1`: only tiles already saved, so the warm-up pass
            // measures reads from the store.
            let stored = (args.get("stored", 0) == 1).then_some((&store, true));
            let paths = pick_tiles(
                &tl,
                &layer,
                (args.get("zmin", 2), args.get("zmax", 5)),
                args.get("n", 200),
                stored,
                &mut rng,
                &mut seen,
            );
            let (mut warm, t) = once_each(&http, &base, paths.clone(), conc).await;
            warm.print("first pass", t, None);
            let rss = RssWatch::start(pid);
            let (mut s, t) = for_a_while(&http, &base, paths, conc, secs, false).await;
            s.print(
                &format!("tiles-hot {layer} conc={conc}"),
                t,
                rss.as_ref().map(RssWatch::peak),
            );
        }
        "tiles-cold" => {
            let layers = args.str("layers", "temp,wind,precip,clouds");
            let layers: Vec<&str> = layers.split(',').collect();
            let n: usize = args.get("n", 300);
            let zs = (args.get("zmin", 4), args.get("zmax", 6));
            let mut seen = HashSet::new();
            let mut per_layer = Vec::new();
            for layer in &layers {
                let tl = timeline(&http, &base, layer).await;
                println!(
                    "{layer}: {} frames, {} parts",
                    tl.frames.len(),
                    tl.parts.len()
                );
                per_layer.push(pick_tiles(
                    &tl,
                    layer,
                    zs,
                    n.div_ceil(layers.len()),
                    Some((&store, false)),
                    &mut rng,
                    &mut seen,
                ));
            }
            // Interleave the layers so they load their grids side by side.
            let mut paths = Vec::new();
            for i in 0..per_layer.iter().map(Vec::len).max().unwrap_or(0) {
                paths.extend(per_layer.iter().filter_map(|l| l.get(i).cloned()));
            }
            paths.truncate(n);
            let rss = RssWatch::start(pid);
            let (mut s, t) = once_each(&http, &base, paths, conc).await;
            s.print(
                &format!("tiles-cold {layers:?} z{}-{} conc={conc}", zs.0, zs.1),
                t,
                rss.as_ref().map(RssWatch::peak),
            );
            if let Some(r) = rss {
                println!(
                    "RSS before {:.0} MB, after {:.0} MB",
                    r.start as f64 / 1024.0,
                    pid.and_then(rss_kb).unwrap_or(0) as f64 / 1024.0
                );
            }
        }
        "get" => {
            let path = args.str("path", "/api/status");
            let rss = RssWatch::start(pid);
            let (mut s, t) = for_a_while(
                &http,
                &base,
                vec![path.clone()],
                conc,
                secs,
                args.get("revalidate", 0) == 1,
            )
            .await;
            s.print(
                &format!("GET {path} conc={conc}"),
                t,
                rss.as_ref().map(RssWatch::peak),
            );
        }
        "paths" => {
            let file = args.str("file", "urls.txt");
            let paths: Vec<String> = std::fs::read_to_string(&file)
                .expect("the file")
                .lines()
                .map(str::to_string)
                .filter(|l| !l.is_empty())
                .collect();
            let rss = RssWatch::start(pid);
            let (mut s, t) = for_a_while(
                &http,
                &base,
                paths,
                conc,
                secs,
                args.get("revalidate", 0) == 1,
            )
            .await;
            s.print(
                &format!("paths {file} conc={conc}"),
                t,
                rss.as_ref().map(RssWatch::peak),
            );
        }
        "alert-tiles" => {
            let tj: serde_json::Value = http
                .get(format!("{base}/api/alerts/tiles.json"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let revision = tj["revision"].as_i64().expect("a revision");
            let (zmin, zmax): (u8, u8) = (args.get("zmin", 2), args.get("zmax", 8));
            let n: usize = args.get("n", 400);
            let mut set = HashSet::new();
            let mut paths = Vec::new();
            // Tiles over land in the northern mid-latitudes, where alerts are.
            while paths.len() < n {
                let z = zmin + rng.below(u64::from(zmax - zmin) + 1) as u8;
                let side = 1u64 << z;
                let x = rng.below(side);
                let y = side / 4 + rng.below((side / 4).max(1));
                if set.insert((z, x, y)) {
                    paths.push(format!("/api/alerts/tiles/{revision}/{z}/{x}/{y}.mvt"));
                }
            }
            let rss = RssWatch::start(pid);
            let (mut first, t) = once_each(&http, &base, paths.clone(), conc).await;
            first.print(
                &format!("alert-tiles first pass rev={revision} conc={conc}"),
                t,
                rss.as_ref().map(RssWatch::peak),
            );
            if args.get("repeat", 1) == 1 {
                let (mut s, t) = for_a_while(&http, &base, paths, conc, secs, false).await;
                s.print("alert-tiles again", t, rss.as_ref().map(RssWatch::peak));
            }
        }
        "ws" => {
            let clients: usize = args.get("clients", 200);
            let url = base.replacen("http", "ws", 1) + "/ws";
            let received = Arc::new(AtomicUsize::new(0));
            let bytes = Arc::new(AtomicU64::new(0));
            let mut tasks = Vec::new();
            for _ in 0..clients {
                let (url, received, bytes) = (url.clone(), received.clone(), bytes.clone());
                tasks.push(tokio::spawn(async move {
                    let (mut ws, _) = tokio_tungstenite::connect_async(url)
                        .await
                        .expect("connect");
                    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(secs);
                    while let Ok(Some(Ok(m))) = tokio::time::timeout_at(deadline, ws.next()).await {
                        received.fetch_add(1, Ordering::Relaxed);
                        bytes.fetch_add(m.len() as u64, Ordering::Relaxed);
                    }
                    let _ = ws.close(None).await;
                }));
            }
            let rss = RssWatch::start(pid);
            for t in tasks {
                let _ = t.await;
            }
            println!(
                "ws: {clients} clients, {} messages, {:.1} MB in {secs}s{}",
                received.load(Ordering::Relaxed),
                bytes.load(Ordering::Relaxed) as f64 / 1e6,
                rss.map_or(String::new(), |r| format!(
                    ", peak RSS {:.0} MB",
                    r.peak() as f64 / 1024.0
                ))
            );
        }
        other => {
            eprintln!("unknown scenario {other:?}");
            std::process::exit(2);
        }
    }
}
