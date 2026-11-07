//! Helpers shared by the crate's tests.

use crate::IngestConfig;
use crate::driver::model::{Model, ModelTask};
use crate::driver::observed::{Observed, ObservedTask};
use crate::engine::Engine;
use crate::remote::memory::MemoryRemote;
use crate::remote::{Remote, RemoteConfig, RetryPolicy, S3Bucket, Transport};
use chrono::{DateTime, TimeZone, Utc};
use futures::TryStreamExt;
use object_store::ObjectStore;
use object_store::memory::InMemory;
use std::sync::Arc;
use std::time::Duration;
use wx_core::{FrameCatalog, Hub, LAYERS, LayerSpec, SOURCES, SourceDef};

pub fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `h:m` UTC on 6 November 2026, the day the fixtures are from.
pub fn at(h: u32, m: u32) -> DateTime<Utc> {
    on(6, h, m)
}

/// `h:m` UTC on `day` November 2026.
pub fn on(day: u32, h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, day, h, m, 0).unwrap()
}

/// `h:m:s` UTC on 6 November 2026.
pub fn hms(h: u32, m: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 6, h, m, s).unwrap()
}

/// A source of the registry.
pub fn source(id: &str) -> &'static SourceDef {
    wx_core::source::source(id).unwrap_or_else(|| panic!("no source {id}"))
}

/// An in-memory network serving `buckets` (AWS bucket names) as S3.
pub fn remote(buckets: &[&str]) -> Arc<MemoryRemote> {
    let mem = MemoryRemote::new();
    for b in buckets {
        mem.serve_s3(&S3Bucket::aws(b), 1000);
    }
    mem
}

pub fn mrms_remote() -> Arc<MemoryRemote> {
    remote(&[crate::families::mrms::BUCKET])
}

/// A remote over `mem` that does not wait between retries or requests.
pub fn quick_remote(mem: &Arc<MemoryRemote>) -> Remote {
    quick(mem.clone())
}

/// A remote over `transport` that does not wait between retries, nor
/// between requests to the hosts tests use.
pub fn quick(transport: Arc<dyn Transport>) -> Remote {
    let fast = crate::HostPolicy {
        concurrency: 8,
        min_interval: Duration::ZERO,
    };
    let hosts = [
        "amazonaws.com",
        "example.org",
        "ovh.net",
        "inpe.br",
        "weather.gc.ca",
        "dwd.de",
        "noaa.gov",
        "jma.go.jp",
    ];
    Remote::with_transport(
        transport,
        RemoteConfig {
            hosts: hosts.iter().map(|h| (h.to_string(), fast)).collect(),
            retry: RetryPolicy {
                retries: 1,
                base: Duration::from_millis(1),
                max: Duration::from_millis(5),
            },
            ..RemoteConfig::default()
        },
    )
}

/// An engine over the real registries, reading from `mem` and storing into
/// a fresh in-memory store, with a fresh catalog.
pub fn engine(mem: &Arc<MemoryRemote>) -> (Arc<Engine>, Arc<InMemory>) {
    engine_on(quick_remote(mem))
}

/// [`engine`] over any remote.
pub fn engine_on(remote: Remote) -> (Arc<Engine>, Arc<InMemory>) {
    let store = Arc::new(InMemory::new());
    let catalog = Arc::new(FrameCatalog::with_registry(Hub::new(), LAYERS, SOURCES));
    let engine = Engine::new(
        store.clone(),
        catalog,
        IngestConfig::default(),
        remote,
        LAYERS,
    );
    (Arc::new(engine), store)
}

pub fn engine_with(
    mem: &Arc<MemoryRemote>,
    store: Arc<dyn ObjectStore>,
    layers: &'static [LayerSpec],
    sources: &'static [SourceDef],
) -> Arc<Engine> {
    let catalog = Arc::new(FrameCatalog::with_registry(Hub::new(), layers, sources));
    Arc::new(Engine::new(
        store,
        catalog,
        IngestConfig::default(),
        quick_remote(mem),
        layers,
    ))
}

/// A task following every product of the source `id` with `family`.
pub fn model_task(engine: &Arc<Engine>, family: impl Model, id: &str) -> ModelTask {
    let source = source(id);
    ModelTask::new(
        engine.clone(),
        Arc::new(family),
        source,
        &crate::driver::products_of(engine.layers, source),
    )
}

/// A task following `product` of the source `id` with `family`.
pub fn observed_task(
    engine: &Arc<Engine>,
    family: impl Observed,
    id: &str,
    product: &'static str,
) -> ObservedTask {
    ObservedTask::new(engine.clone(), Arc::new(family), source(id), product)
}

/// A GRIB2 file put together message by message, with its NOAA-style
/// inventory.
#[derive(Default)]
pub struct GribFile {
    pub bytes: Vec<u8>,
    pub inventory: String,
}

impl GribFile {
    /// Append `body` as `var:level:time` with `extra` after the time (`""`
    /// for none); its byte range.
    pub fn add(
        &mut self,
        var: &str,
        level: &str,
        time: &str,
        extra: &str,
        body: &[u8],
    ) -> (u64, u64) {
        let (start, n) = (self.bytes.len(), self.inventory.lines().count() + 1);
        self.inventory.push_str(&format!(
            "{n}:{start}:d=2026110610:{var}:{level}:{time}:{extra}\n"
        ));
        self.bytes.extend_from_slice(body);
        (start as u64, self.bytes.len() as u64)
    }

    /// Append a message a loose match would take: 100 kB, too far from its
    /// neighbours to be fetched with them.
    pub fn decoy(&mut self, var: &str, level: &str, time: &str, extra: &str) {
        let mut body = b"GRIB".to_vec();
        body.resize(100_000, 0);
        self.add(var, level, time, extra, &body);
    }
}

/// A model file built from fixture messages, each listed under the given
/// idx labels `(var, level, time, fixture file)`, and its inventory.
pub fn model_file(parts: &[(&str, &str, &str, &str)]) -> (Vec<u8>, String) {
    let mut file = GribFile::default();
    for (var, level, time, name) in parts {
        file.add(var, level, time, "", &fixture(name));
    }
    (file.bytes, file.inventory)
}

/// The grid `store` holds for part `id` on `layer`.
pub async fn stored_grid(store: &dyn ObjectStore, layer: &str, id: &str) -> wx_pipeline::FrameGrid {
    use object_store::ObjectStoreExt;
    let path = object_store::path::Path::from(wx_core::keys::grid(layer, id));
    let got = store
        .get(&path)
        .await
        .unwrap_or_else(|e| panic!("{layer}/{id}: {e}"));
    wx_pipeline::FrameGrid::from_bytes(&got.bytes().await.unwrap()).unwrap()
}

/// Every key in a store, sorted.
pub async fn keys(store: &dyn ObjectStore) -> Vec<String> {
    let mut keys: Vec<String> = store
        .list(None)
        .map_ok(|m| m.location.to_string())
        .try_collect()
        .await
        .unwrap();
    keys.sort();
    keys
}

pub fn leak<T>(v: Vec<T>) -> &'static [T] {
    Box::leak(v.into_boxed_slice())
}

/// The test binary's allocator: the system's, counting allocations and the
/// bytes live, for the benchmarks (meaningful with one test thread).
pub mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    static COUNT: AtomicUsize = AtomicUsize::new(0);
    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);

    struct Counting;

    fn grew(by: usize) {
        let live = LIVE.fetch_add(by, Relaxed) + by;
        PEAK.fetch_max(live, Relaxed);
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(l) };
            if !p.is_null() {
                COUNT.fetch_add(1, Relaxed);
                grew(l.size());
            }
            p
        }

        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc_zeroed(l) };
            if !p.is_null() {
                COUNT.fetch_add(1, Relaxed);
                grew(l.size());
            }
            p
        }

        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) };
            LIVE.fetch_sub(l.size(), Relaxed);
        }

        unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
            let q = unsafe { System.realloc(p, l, size) };
            if !q.is_null() {
                COUNT.fetch_add(1, Relaxed);
                if size >= l.size() {
                    grew(size - l.size());
                } else {
                    LIVE.fetch_sub(l.size() - size, Relaxed);
                }
            }
            q
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;

    /// Allocations so far.
    pub fn count() -> usize {
        COUNT.load(Relaxed)
    }

    /// Bytes allocated and not yet freed.
    pub fn live() -> usize {
        LIVE.load(Relaxed)
    }

    /// Start measuring a peak from what is live now.
    pub fn reset_peak() {
        PEAK.store(LIVE.load(Relaxed), Relaxed);
    }

    /// The most bytes live at once since [`reset_peak`].
    pub fn peak() -> usize {
        PEAK.load(Relaxed)
    }
}

/// Stores that misbehave on purpose.
pub mod stores {
    use futures::stream::BoxStream;
    use object_store::memory::InMemory;
    use object_store::path::Path;
    use object_store::{
        CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
        PutMultipartOptions, PutOptions, PutPayload, PutResult, Result,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::{Notify, Semaphore};

    /// Holds its first delete until released, announcing it has started.
    #[derive(Debug)]
    pub struct Gated {
        inner: Arc<InMemory>,
        armed: AtomicBool,
        pub entered: Arc<Notify>,
        pub release: Arc<Semaphore>,
    }

    impl Gated {
        pub fn new(inner: Arc<InMemory>) -> Self {
            Self {
                inner,
                armed: AtomicBool::new(true),
                entered: Arc::new(Notify::new()),
                release: Arc::new(Semaphore::new(0)),
            }
        }
    }

    impl std::fmt::Display for Gated {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Gated")
        }
    }

    #[async_trait::async_trait]
    impl ObjectStore for Gated {
        async fn put_opts(
            &self,
            p: &Path,
            payload: PutPayload,
            o: PutOptions,
        ) -> Result<PutResult> {
            self.inner.put_opts(p, payload, o).await
        }
        async fn put_multipart_opts(
            &self,
            p: &Path,
            o: PutMultipartOptions,
        ) -> Result<Box<dyn MultipartUpload>> {
            self.inner.put_multipart_opts(p, o).await
        }
        async fn get_opts(&self, p: &Path, o: GetOptions) -> Result<GetResult> {
            self.inner.get_opts(p, o).await
        }
        fn delete_stream(
            &self,
            locations: BoxStream<'static, Result<Path>>,
        ) -> BoxStream<'static, Result<Path>> {
            use futures::StreamExt;
            let inner = self.inner.clone();
            if !self.armed.swap(false, Ordering::SeqCst) {
                return inner.delete_stream(locations);
            }
            let (entered, release) = (self.entered.clone(), self.release.clone());
            futures::stream::once(async move {
                entered.notify_one();
                let _permit = release.acquire().await;
                inner.delete_stream(locations)
            })
            .flatten()
            .boxed()
        }
        fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, Result<ObjectMeta>> {
            self.inner.list(prefix)
        }
        async fn list_with_delimiter(&self, prefix: Option<&Path>) -> Result<ListResult> {
            self.inner.list_with_delimiter(prefix).await
        }
        async fn copy_opts(&self, from: &Path, to: &Path, o: CopyOptions) -> Result<()> {
            self.inner.copy_opts(from, to, o).await
        }
    }

    /// Counts its listing calls.
    #[derive(Debug, Default)]
    pub struct Counting {
        pub inner: InMemory,
        lists: std::sync::atomic::AtomicUsize,
    }

    impl Counting {
        /// Listing calls since the last time this was asked.
        pub fn take_lists(&self) -> usize {
            self.lists.swap(0, Ordering::SeqCst)
        }
    }

    impl std::fmt::Display for Counting {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Counting")
        }
    }

    #[async_trait::async_trait]
    impl ObjectStore for Counting {
        async fn put_opts(
            &self,
            p: &Path,
            payload: PutPayload,
            o: PutOptions,
        ) -> Result<PutResult> {
            self.inner.put_opts(p, payload, o).await
        }
        async fn put_multipart_opts(
            &self,
            p: &Path,
            o: PutMultipartOptions,
        ) -> Result<Box<dyn MultipartUpload>> {
            self.inner.put_multipart_opts(p, o).await
        }
        async fn get_opts(&self, p: &Path, o: GetOptions) -> Result<GetResult> {
            self.inner.get_opts(p, o).await
        }
        fn delete_stream(
            &self,
            locations: BoxStream<'static, Result<Path>>,
        ) -> BoxStream<'static, Result<Path>> {
            self.inner.delete_stream(locations)
        }
        fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, Result<ObjectMeta>> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            self.inner.list(prefix)
        }
        async fn list_with_delimiter(&self, prefix: Option<&Path>) -> Result<ListResult> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            self.inner.list_with_delimiter(prefix).await
        }
        async fn copy_opts(&self, from: &Path, to: &Path, o: CopyOptions) -> Result<()> {
            self.inner.copy_opts(from, to, o).await
        }
    }

    /// Refuses every write, like a destination store that is down.
    #[derive(Debug)]
    pub struct Broken;

    impl std::fmt::Display for Broken {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Broken")
        }
    }

    fn down() -> object_store::Error {
        object_store::Error::Generic {
            store: "Broken",
            source: "the store is down".into(),
        }
    }

    #[async_trait::async_trait]
    impl ObjectStore for Broken {
        async fn put_opts(&self, _: &Path, _: PutPayload, _: PutOptions) -> Result<PutResult> {
            Err(down())
        }
        async fn put_multipart_opts(
            &self,
            _: &Path,
            _: PutMultipartOptions,
        ) -> Result<Box<dyn MultipartUpload>> {
            Err(down())
        }
        async fn get_opts(&self, _: &Path, _: GetOptions) -> Result<GetResult> {
            Err(down())
        }
        fn delete_stream(
            &self,
            _: BoxStream<'static, Result<Path>>,
        ) -> BoxStream<'static, Result<Path>> {
            use futures::StreamExt;
            futures::stream::once(async { Err(down()) }).boxed()
        }
        fn list(&self, _: Option<&Path>) -> BoxStream<'static, Result<ObjectMeta>> {
            use futures::StreamExt;
            futures::stream::once(async { Err(down()) }).boxed()
        }
        async fn list_with_delimiter(&self, _: Option<&Path>) -> Result<ListResult> {
            Err(down())
        }
        async fn copy_opts(&self, _: &Path, _: &Path, _: CopyOptions) -> Result<()> {
            Err(down())
        }
    }
}

/// For the live checks: ignored tests against the real hosts, and the
/// stores their runs leave.
pub mod live {
    use crate::remote::{Remote, RemoteConfig};
    use chrono::{DateTime, Utc};
    use std::collections::HashMap;
    use wx_core::proj::{TILE_CELLS, lat_to_y, lon_to_x};
    use wx_core::{FrameCatalog, LayerSpec, Part, TileCoord};
    use wx_pipeline::FrameGrid;

    /// A remote over the real network, as a developer's client.
    pub fn remote() -> Remote {
        let config = RemoteConfig {
            user_agent: "weather-map-dev (live tests)".into(),
            ..RemoteConfig::default()
        };
        Remote::new(config).unwrap()
    }

    /// A grid a run stored under `dir`.
    pub fn stored(dir: &std::path::Path, layer: &str, id: &str) -> Option<FrameGrid> {
        let bytes = std::fs::read(dir.join(wx_core::keys::grid(layer, id))).ok()?;
        FrameGrid::from_bytes(&bytes).ok()
    }

    /// The part of `source` on `layer` valid nearest `at`.
    pub fn nearest(
        catalog: &FrameCatalog,
        layer: &str,
        source: &str,
        at: DateTime<Utc>,
    ) -> Option<Part> {
        let parts = catalog.timeline(layer).parts.into_iter();
        parts
            .filter(|p| p.source == source)
            .min_by_key(|p| (p.valid - at).num_seconds().abs())
    }

    /// A grid's bands at places as the map shows them: the nearest sample of
    /// the zoom-`z` tile rendered over each, tiles kept for the next place.
    pub struct Sampler<'a> {
        grid: &'a FrameGrid,
        layer: &'static LayerSpec,
        z: u8,
        tiles: HashMap<(u32, u32), Option<wx_tile::Tile>>,
    }

    impl<'a> Sampler<'a> {
        pub fn new(grid: &'a FrameGrid, layer: &str, z: u8) -> Self {
            let layer = wx_core::layer::layer(layer).unwrap();
            Self {
                grid,
                layer,
                z,
                tiles: HashMap::new(),
            }
        }

        /// Every band, missing values as NaN; `None` off the grid.
        pub fn at(&mut self, lon: f64, lat: f64) -> Option<Vec<f32>> {
            let n = f64::from(1u32 << self.z);
            let (x, y) = (lon_to_x(lon) * n, lat_to_y(lat) * n);
            let (tx, ty) = (x.floor().min(n - 1.0) as u32, y.floor().min(n - 1.0) as u32);
            let (grid, z) = (self.grid, self.z);
            let tile = self.tiles.entry((tx, ty)).or_insert_with(|| {
                let png = grid.render(TileCoord::new(z, tx, ty)?)?;
                wx_tile::decode(&png).ok()
            });
            let tile = tile.as_ref()?;
            let col = ((x - f64::from(tx)) * TILE_CELLS as f64).round() as usize;
            let row = ((y - f64::from(ty)) * TILE_CELLS as f64).round() as usize;
            let q = |b: usize| tile.bands[b][row * tile.width as usize + col];
            let bands = self.layer.bands.iter().enumerate();
            Some(
                bands
                    .map(|(b, spec)| spec.dequantize(q(b)).unwrap_or(f32::NAN))
                    .collect(),
            )
        }
    }
}
