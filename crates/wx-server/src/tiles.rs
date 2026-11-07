//! Serving tiles. A tile is rendered from a stack of parts (one stored grid
//! each) the first time anyone asks for that stack at that position, saved,
//! and from then on read straight from the store.

use anyhow::Context;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use moka::future::Cache;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use wx_core::frame::MAX_COMBO_PARTS;
use wx_core::{FrameCatalog, Part, TileCoord, keys, layer};
use wx_pipeline::FrameGrid;

/// Grids that renders in flight may hold between them, beyond what the grid
/// cache keeps. A render takes one slot per part in its stack, so a
/// world-wide low-zoom radar tile (a dozen grids of up to 60 MB each) cannot
/// run alongside several others. Together with the cache's budget this is
/// what bounds the server's memory under a burst of first-time requests.
const GRID_SLOTS: u32 = 12;
/// How long a request waits for its slots before it is told to retry.
const RENDER_QUEUE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileOutcome {
    Tile(Bytes),
    /// The stack is valid and has no data in this tile.
    Empty,
    /// No such layer, a stack no frame has for this tile, or a zoom deeper
    /// than the stack offers.
    NotFound,
    /// Too many tiles are being rendered; try again shortly.
    Busy,
}

#[derive(Debug)]
enum LoadError {
    Busy,
    /// A part's grid is gone: the part expired between the catalog saying
    /// yes and the render. Nothing is saved; the client is told 404.
    Gone,
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for LoadError {
    fn from(e: anyhow::Error) -> Self {
        LoadError::Failed(e)
    }
}

pub struct TileService {
    store: Arc<dyn ObjectStore>,
    catalog: Arc<FrameCatalog>,
    /// Recently served tiles. Besides saving store reads for the tiles
    /// everyone is looking at (the newest radar frame), it makes concurrent
    /// requests for the same missing tile share one render.
    hot: Cache<String, Option<Bytes>>,
    /// Decoded grids, the expensive input to rendering.
    grids: Cache<String, Arc<FrameGrid>>,
    grid_slots: Arc<Semaphore>,
}

impl TileService {
    pub fn new(
        store: Arc<dyn ObjectStore>,
        catalog: Arc<FrameCatalog>,
        grid_cache_mb: u64,
    ) -> Self {
        let hot = Cache::builder()
            .max_capacity(96 * 1024 * 1024)
            .weigher(|k: &String, v: &Option<Bytes>| {
                (k.len() + v.as_ref().map_or(0, Bytes::len) + 64) as u32
            })
            .time_to_live(Duration::from_secs(600))
            .build();
        let grids = Cache::builder()
            .max_capacity(grid_cache_mb.max(1) * 1024 * 1024)
            .weigher(|_: &String, g: &Arc<FrameGrid>| {
                g.approx_bytes().min(u32::MAX as usize) as u32
            })
            .time_to_idle(Duration::from_secs(900))
            .build();
        Self {
            store,
            catalog,
            hot,
            grids,
            grid_slots: Arc::new(Semaphore::new(GRID_SLOTS as usize)),
        }
    }

    /// The tile at `coord` rendered from `combo`, the part ids the client
    /// named, top first.
    pub async fn get(
        &self,
        layer_id: &str,
        coord: TileCoord,
        combo: &[&str],
    ) -> anyhow::Result<TileOutcome> {
        if layer::layer(layer_id).is_none()
            || combo.is_empty()
            || combo.len() > MAX_COMBO_PARTS
            || !combo.iter().all(|id| Part::is_valid_id(id))
        {
            return Ok(TileOutcome::NotFound);
        }
        // The catalog is the gatekeeper: only a stack that some frame has (or
        // had a few minutes ago) for this very tile is ever rendered or
        // stored, so a client cannot make the server work on, or keep,
        // arbitrary combinations.
        let Some(parts) = self.catalog.resolve(layer_id, coord, combo, Utc::now()) else {
            return Ok(TileOutcome::NotFound);
        };

        let key = keys::tile(layer_id, combo, coord);
        let loaded = self
            .hot
            .try_get_with_by_ref(&key, self.load(layer_id, coord, parts, &key))
            .await;
        match loaded {
            Ok(Some(bytes)) => Ok(TileOutcome::Tile(bytes)),
            Ok(None) => Ok(TileOutcome::Empty),
            Err(e) => match &*e {
                LoadError::Busy => Ok(TileOutcome::Busy),
                LoadError::Gone => Ok(TileOutcome::NotFound),
                LoadError::Failed(e) => Err(anyhow::anyhow!("{e:#}")),
            },
        }
    }

    /// `Ok(None)` means the tile exists and is empty.
    async fn load(
        &self,
        layer_id: &str,
        coord: TileCoord,
        parts: Vec<Part>,
        key: &str,
    ) -> Result<Option<Bytes>, LoadError> {
        let path = Path::from(key);
        match self.store.get(&path).await {
            // A zero-length object marks a tile already found empty.
            Ok(found) => {
                let bytes = found.bytes().await.context("reading a stored tile")?;
                return Ok((!bytes.is_empty()).then_some(bytes));
            }
            Err(object_store::Error::NotFound { .. }) => {}
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context("reading a stored tile")
                    .into());
            }
        }

        // The slots cover loading the grids and rendering from them, so the
        // grids alive outside the cache never number more than GRID_SLOTS.
        let want = (parts.len() as u32).min(GRID_SLOTS);
        let slots = self.grid_slots.clone().acquire_many_owned(want);
        let Ok(slots) = tokio::time::timeout(RENDER_QUEUE_TIMEOUT, slots).await else {
            return Err(LoadError::Busy);
        };
        let slots = slots.context("render slots closed")?;

        // Side by side: the slots already count every grid of the stack.
        let loads = parts.iter().map(|part| self.grid(layer_id, &part.id));
        let mut grids = Vec::with_capacity(parts.len());
        for grid in futures::future::try_join_all(loads).await? {
            grids.push(grid.ok_or(LoadError::Gone)?);
        }
        // The slots travel into the blocking task: if the client goes away
        // the render still finishes, and it must keep counting until it does.
        let rendered = tokio::task::spawn_blocking(move || {
            let _slots = slots;
            let stack: Vec<&FrameGrid> = grids.iter().map(Arc::as_ref).collect();
            wx_pipeline::render_stack(&stack, coord)
        })
        .await
        .context("tile render task failed")?;
        let bytes = rendered.map(Bytes::from);
        let payload = PutPayload::from(bytes.clone().unwrap_or_default());
        if let Err(e) = self.store.put(&path, payload).await {
            // Serving the tile matters more than saving it; it will simply be
            // rendered again next time.
            tracing::warn!(%path, "could not save a rendered tile: {e}");
        }
        Ok(bytes)
    }

    /// What the value at a point of one frame is made of: the contributing
    /// parts, top first, each with its share of the blend (the shares sum to
    /// 1). `None` when the layer has no frame at `valid`; an empty list when
    /// no part has data there.
    pub async fn provenance(
        &self,
        layer_id: &str,
        valid: DateTime<Utc>,
        lon: f64,
        lat: f64,
    ) -> anyhow::Result<Option<Vec<(Part, f64)>>> {
        let Some(parts) = self.catalog.frame_parts(layer_id, valid) else {
            return Ok(None);
        };
        // Grids are looked at one after another, so one slot covers the lot.
        let _slot = self
            .grid_slots
            .clone()
            .acquire_owned()
            .await
            .context("render slots closed")?;
        let mut shares = Vec::new();
        // The same arithmetic as the compositor: a part gets its own weight
        // times whatever the parts above it left over.
        let mut left = 1.0;
        for part in parts {
            if !part.bounds.contains(lon, lat) {
                continue;
            }
            // A grid that has just been deleted contributes nothing.
            let Some(grid) = self.grid(layer_id, &part.id).await? else {
                continue;
            };
            let weight = grid.coverage_at(lon, lat);
            if weight <= 0.0 {
                continue;
            }
            shares.push((part, left * weight));
            left *= 1.0 - weight;
            if left < 1e-3 {
                break;
            }
        }
        let total: f64 = shares.iter().map(|(_, share)| share).sum();
        if total > 0.0 {
            for (_, share) in &mut shares {
                *share /= total;
            }
        }
        Ok(Some(shares))
    }

    async fn grid(&self, layer_id: &str, part_id: &str) -> anyhow::Result<Option<Arc<FrameGrid>>> {
        let key = keys::grid(layer_id, part_id);
        let store = self.store.clone();
        let path = Path::from(key.as_str());
        let mut inserted = false;
        let loaded = self
            .grids
            .try_get_with_by_ref(&key, async {
                inserted = true;
                let bytes = match store.get(&path).await {
                    Ok(found) => found.bytes().await?,
                    Err(e) => return Err(e),
                };
                let grid = tokio::task::spawn_blocking(move || FrameGrid::from_bytes(&bytes))
                    .await
                    .map_err(|e| object_store::Error::Generic {
                        store: "grid",
                        source: Box::new(e),
                    })?
                    .map_err(|e| object_store::Error::Generic {
                        store: "grid",
                        source: e.into(),
                    })?;
                Ok::<_, object_store::Error>(Arc::new(grid))
            })
            .await;
        // The cache only evicts when it gets around to its housekeeping, and
        // after a burst nothing else would prompt it. Each grid is tens of
        // megabytes, so make it catch up now. Only an insert can take it
        // over budget.
        if inserted {
            self.grids.run_pending_tasks().await;
        }
        match loaded {
            Ok(grid) => Ok(Some(grid)),
            Err(e) if matches!(*e, object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("loading a part's grid: {e}")),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Timelike};
    use object_store::memory::InMemory;
    use std::path::PathBuf;
    use wx_core::frame::{publish_boxes, tile_parts};
    use wx_core::proj::{TILE_CELLS, lat_to_y, lon_to_x};
    use wx_core::{Hub, Timeline};

    fn fixture(name: &str) -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    }

    pub fn tile_at(z: u8, lon: f64, lat: f64) -> TileCoord {
        let n = (1u32 << z) as f64;
        TileCoord::new(z, (lon_to_x(lon) * n) as u32, (lat_to_y(lat) * n) as u32).unwrap()
    }

    /// The run a part valid a couple of hours from now would come from, so
    /// nothing in these tests expires while they run.
    fn recent_run() -> DateTime<Utc> {
        let now = Utc::now();
        now.with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap()
            - ChronoDuration::hours(1)
    }

    /// Store one real HRRR temperature part and put it on the catalog.
    /// Returns the store, the catalog and the part.
    pub async fn hrrr_temp() -> (Arc<InMemory>, Arc<FrameCatalog>, Part) {
        let spec = layer::layer("temp").unwrap();
        let source = wx_core::source::source("hrrr").unwrap();
        let input = spec.input_for(source).unwrap();
        let grib = fixture("hrrr_t10z_f06__TMP_2mabove.grib2");
        let decoded = wx_pipeline::decode(&grib, input.fields).unwrap();
        let raster = wx_pipeline::derive(spec, input, &decoded.fields).unwrap();
        let grid = FrameGrid::build(spec, &raster);
        let (bounds, solid) = publish_boxes(grid.bounds(), grid.solid());
        let part = Part::forecast("hrrr", recent_run(), 180, bounds, solid, source.max_zoom);

        let store = Arc::new(InMemory::new());
        store
            .put(
                &Path::from(keys::grid("temp", &part.id)),
                grid.to_bytes().into(),
            )
            .await
            .unwrap();
        let catalog = Arc::new(FrameCatalog::new(Hub::new()));
        catalog.insert("temp", part.clone(), Utc::now()).unwrap();
        (store, catalog, part)
    }

    /// The stack the one frame on `timeline` has for `coord`, as ids.
    pub fn stack(timeline: &Timeline, coord: TileCoord) -> Vec<String> {
        let frame = timeline.frames.last().expect("a frame");
        let parts: Vec<Part> = frame
            .parts
            .iter()
            .map(|id| timeline.parts.iter().find(|p| &p.id == id).unwrap().clone())
            .collect();
        tile_parts(&parts, coord)
            .into_iter()
            .map(|p| p.id.clone())
            .collect()
    }

    fn refs(ids: &[String]) -> Vec<&str> {
        ids.iter().map(String::as_str).collect()
    }

    #[tokio::test]
    async fn renders_once_then_reads_the_store() {
        let (store, catalog, part) = hrrr_temp().await;
        let svc = TileService::new(store.clone(), catalog.clone(), 64);
        let coord = tile_at(5, -87.63, 41.88);
        let combo = stack(&catalog.timeline("temp"), coord);
        assert_eq!(combo, vec![part.id.clone()]);

        let TileOutcome::Tile(first) = svc.get("temp", coord, &refs(&combo)).await.unwrap() else {
            panic!("expected a tile");
        };
        let tile = wx_tile::decode(&first).unwrap();
        assert_eq!(
            (tile.width as usize, tile.height as usize),
            (TILE_CELLS + 1, TILE_CELLS + 1)
        );
        // Chicago in early November: a believable temperature in the middle.
        let band = &layer::layer("temp").unwrap().bands[0];
        let q = tile.bands[0][(TILE_CELLS / 2) * (TILE_CELLS + 1) + TILE_CELLS / 2];
        let celsius = band.dequantize(q).expect("data over Chicago");
        assert!((-30.0..40.0).contains(&celsius), "{celsius}");

        // It was saved under the stack's key, and a fresh service reads it
        // from there even with the grid gone.
        let key = Path::from(keys::tile("temp", &refs(&combo), coord));
        assert_eq!(store.get(&key).await.unwrap().bytes().await.unwrap(), first);
        store
            .delete(&Path::from(keys::grid("temp", &part.id)))
            .await
            .unwrap();
        let cold = TileService::new(store, catalog, 64);
        assert_eq!(
            cold.get("temp", coord, &refs(&combo)).await.unwrap(),
            TileOutcome::Tile(first)
        );
    }

    #[tokio::test]
    async fn empty_tiles_are_remembered_as_empty() {
        let (store, catalog, part) = hrrr_temp().await;
        let svc = TileService::new(store.clone(), catalog, 64);
        // Inside the grid's lon/lat envelope but outside the Lambert grid
        // itself: the south-west corner of the box, over the Pacific, where
        // the grid's own edge is ten degrees further east.
        let coord = tile_at(6, part.bounds.west + 0.5, part.bounds.south + 0.5);
        let combo = [part.id.as_str()];
        assert_eq!(
            svc.get("temp", coord, &combo).await.unwrap(),
            TileOutcome::Empty
        );
        let key = Path::from(keys::tile("temp", &combo, coord));
        assert!(
            store
                .get(&key)
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            svc.get("temp", coord, &combo).await.unwrap(),
            TileOutcome::Empty
        );
    }

    #[tokio::test]
    async fn refuses_what_no_frame_has() {
        let (store, catalog, part) = hrrr_temp().await;
        let svc = TileService::new(store, catalog, 64);
        let chicago = tile_at(5, -87.63, 41.88);
        let combo = [part.id.as_str()];
        // Unknown layer, unknown part, a part named twice, nothing named.
        assert_eq!(
            svc.get("nope", chicago, &combo).await.unwrap(),
            TileOutcome::NotFound
        );
        assert_eq!(
            svc.get("temp", chicago, &["hrrr-00010100-0"])
                .await
                .unwrap(),
            TileOutcome::NotFound
        );
        assert_eq!(
            svc.get("temp", chicago, &[combo[0], combo[0]])
                .await
                .unwrap(),
            TileOutcome::NotFound
        );
        assert_eq!(
            svc.get("temp", chicago, &[]).await.unwrap(),
            TileOutcome::NotFound
        );
        // A tile the part does not touch (Europe).
        assert_eq!(
            svc.get("temp", tile_at(5, 10.0, 50.0), &combo)
                .await
                .unwrap(),
            TileOutcome::NotFound
        );
        // Deeper than the source offers.
        let deep = tile_at(part.max_zoom + 1, -87.63, 41.88);
        assert_eq!(
            svc.get("temp", deep, &combo).await.unwrap(),
            TileOutcome::NotFound
        );
    }

    #[tokio::test]
    async fn provenance_names_what_is_under_a_point() {
        let (store, catalog, part) = hrrr_temp().await;
        let svc = TileService::new(store, catalog, 64);
        let chicago = svc
            .provenance("temp", part.valid, -87.63, 41.88)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chicago.len(), 1);
        assert_eq!(chicago[0].0.id, part.id);
        assert!((chicago[0].1 - 1.0).abs() < 1e-9);
        // Nothing covers Europe; a time that is not a frame is not an answer.
        let berlin = svc
            .provenance("temp", part.valid, 13.4, 52.5)
            .await
            .unwrap()
            .unwrap();
        assert!(berlin.is_empty());
        let other = part.valid + ChronoDuration::hours(5);
        assert!(
            svc.provenance("temp", other, -87.63, 41.88)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            svc.provenance("nope", part.valid, -87.63, 41.88)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_part_whose_grid_is_gone_is_not_found_and_nothing_is_saved() {
        let (store, catalog, part) = hrrr_temp().await;
        store
            .delete(&Path::from(keys::grid("temp", &part.id)))
            .await
            .unwrap();
        let svc = TileService::new(store.clone(), catalog, 64);
        let coord = tile_at(5, -87.63, 41.88);
        let combo = [part.id.as_str()];
        assert_eq!(
            svc.get("temp", coord, &combo).await.unwrap(),
            TileOutcome::NotFound
        );
        let key = Path::from(keys::tile("temp", &combo, coord));
        assert!(matches!(
            store.get(&key).await,
            Err(object_store::Error::NotFound { .. })
        ));
    }
}
