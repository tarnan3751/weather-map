//! HRDPS: ECCC's 2.5 km model over Canada and the northern US, on the
//! Datamart ([`super::eccc`]) under `model_hrdps/continental/2.5km`, one
//! file per field and step.
//!
//! Runs start every six hours and are ingested hourly to the registry's
//! horizon (`wx_core::families::hrdps`). A run's first step appears about
//! 2 h 48 after the run time, the others from 2 h 58 to 3 h 16; some runs
//! are hours late (the 00 UTC run of 6 November finished at 6 h 42).
//!
//! The first step has no rate, type or snowfall, and its cloud cover is a
//! placeholder of zeros ([`reg::FIRST_STEP_LACKS`]); none of them is ever
//! asked for there (see that constant).
//!
//! The grid is rotated lat/lon with winds relative to it; the pipeline turns
//! them earth-relative, as the grid says.
//!
//! # Over the United States
//!
//! HRDPS ranks above the US models, so cells inside the registry's
//! [`reg::MASK_OUT`] are set to missing as each field is decoded, and the
//! US models show there; the pipeline's coverage weights fade HRDPS out
//! towards that edge. Which cells those are is worked out once per grid
//! ([`Blanking`]).

use super::eccc::{self, Datamart, Layout};
use crate::driver::{Cx, Cycle, Fetched, Listing, Model, ModelFile, Need};
use crate::remote::FetchError;
use chrono::{DateTime, Duration, Timelike, Utc};
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wx_core::families::hrdps as reg;
use wx_core::{Family, Field, GridProjection, SourceDef};

pub const LAYOUT: Layout = Layout {
    product: reg::PRODUCT,
    model: "HRDPS",
    grid: "RLatLon0.0225",
};

/// Steps ingested, in hours.
pub fn hours() -> std::ops::RangeInclusive<u32> {
    0..=reg::HORIZON_HOURS
}

/// When step `hour` of a run usually appears: the first 2 h 48 after the
/// run, the second 2 h 58, the 48th 3 h 16.
pub fn arrival(run: DateTime<Utc>, hour: u32) -> DateTime<Utc> {
    let secs = match i64::from(hour) {
        0 => 168 * 60,
        h => 178 * 60 + (h - 1) * 18 * 60 / 47,
    };
    run + Duration::seconds(secs)
}

/// Grids blanked at once, at most (HRDPS has one).
const GRIDS_KEPT: usize = 4;

/// One grid's blanked cells, as runs of consecutive indices.
type Cells = Arc<Vec<Range<usize>>>;

/// Sets the cells of a field inside polygons to missing. Which cells those
/// are is computed once per grid.
pub struct Blanking {
    polygons: Vec<Polygon>,
    /// `(nx, ny, projection)` and its cells.
    grids: Mutex<Vec<(usize, usize, GridProjection, Cells)>>,
    computed: AtomicUsize,
}

impl Blanking {
    /// `polygons` as `(lon, lat)` degrees, each closed implicitly.
    pub fn new(polygons: &[&[(f32, f32)]]) -> Self {
        Self {
            polygons: polygons.iter().map(|p| Polygon::new(p)).collect(),
            grids: Mutex::default(),
            computed: AtomicUsize::new(0),
        }
    }

    /// Set every cell of `field` whose centre is inside a polygon to NaN.
    pub fn apply(&self, field: &mut Field) {
        for r in self.cells(field.nx, field.ny, &field.proj).iter() {
            field.values[r.clone()].fill(f32::NAN);
        }
    }

    /// The blanked cells of a grid, as index ranges.
    pub fn cells(&self, nx: usize, ny: usize, proj: &GridProjection) -> Cells {
        // Held while computing, so a grid is never worked out twice.
        let mut grids = self.grids.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((.., cells)) = grids
            .iter()
            .find(|(x, y, p, _)| (*x, *y) == (nx, ny) && p == proj)
        {
            return cells.clone();
        }
        let cells = Arc::new(self.compute(nx, ny, proj));
        self.computed.fetch_add(1, Ordering::Relaxed);
        if grids.len() >= GRIDS_KEPT {
            grids.remove(0);
        }
        grids.push((nx, ny, proj.clone(), cells.clone()));
        cells
    }

    /// How many grids have been worked out.
    #[cfg(test)]
    pub fn computed(&self) -> usize {
        self.computed.load(Ordering::Relaxed)
    }

    fn compute(&self, nx: usize, ny: usize, proj: &GridProjection) -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = Vec::new();
        for j in 0..ny {
            for i in 0..nx {
                let (lon, lat) = proj.ij_to_lonlat(i as f64, j as f64);
                if !self.polygons.iter().any(|p| p.contains(lon, lat)) {
                    continue;
                }
                let k = j * nx + i;
                match out.last_mut() {
                    Some(r) if r.end == k => r.end = k + 1,
                    _ => out.push(k..k + 1),
                }
            }
        }
        out
    }
}

/// A polygon with its bounding box, for the even-odd test.
struct Polygon {
    points: Vec<(f64, f64)>,
    west: f64,
    east: f64,
    south: f64,
    north: f64,
}

impl Polygon {
    fn new(points: &[(f32, f32)]) -> Self {
        let points: Vec<(f64, f64)> = points
            .iter()
            .map(|&(x, y)| (f64::from(x), f64::from(y)))
            .collect();
        let fold = |f: fn(f64, f64) -> f64, pick: fn(&(f64, f64)) -> f64, start: f64| {
            points.iter().map(pick).fold(start, f)
        };
        Self {
            west: fold(f64::min, |p| p.0, f64::INFINITY),
            east: fold(f64::max, |p| p.0, f64::NEG_INFINITY),
            south: fold(f64::min, |p| p.1, f64::INFINITY),
            north: fold(f64::max, |p| p.1, f64::NEG_INFINITY),
            points,
        }
    }

    /// Even-odd rule: a ray eastward from the point crosses the boundary an
    /// odd number of times.
    fn contains(&self, lon: f64, lat: f64) -> bool {
        if !(self.west..=self.east).contains(&lon) || !(self.south..=self.north).contains(&lat) {
            return false;
        }
        let mut inside = false;
        let n = self.points.len();
        for k in 0..n {
            let (a, b) = (self.points[k], self.points[(k + 1) % n]);
            if (a.1 > lat) != (b.1 > lat) && lon < a.0 + (lat - a.1) * (b.0 - a.0) / (b.1 - a.1) {
                inside = !inside;
            }
        }
        inside
    }
}

pub struct Hrdps {
    mart: Datamart,
    blanking: Arc<Blanking>,
}

impl Default for Hrdps {
    fn default() -> Self {
        Self::with_host(eccc::HOST)
    }
}

impl Hrdps {
    /// Reading from `host` (the Datamart, or its mirror).
    pub fn with_host(host: &str) -> Self {
        Self {
            mart: Datamart::new(host, LAYOUT, eccc::registry_levels(Family::Hrdps)),
            blanking: Arc::new(Blanking::new(reg::MASK_OUT)),
        }
    }

    #[cfg(test)]
    pub fn blanking(&self) -> &Blanking {
        &self.blanking
    }
}

#[async_trait::async_trait]
impl Model for Hrdps {
    /// Step 0 has no rate, type or snowfall, and its cloud cover is a
    /// placeholder of zeros (`FIRST_STEP_LACKS`).
    fn absent(&self, _: &SourceDef, _: DateTime<Utc>, need: &Need) -> bool {
        need.lead == 0
            && need.field.is_some_and(|f| {
                reg::FIRST_STEP_LACKS
                    .iter()
                    .any(|x| x.var == f.var && x.level == f.level)
            })
    }

    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        super::common::one_region(source, "HRDPS")
    }

    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::hours(i64::from(reg::RUN_EVERY_HOURS)),
            spare: 2,
            // A run takes about 30 minutes to land.
            relist: Duration::minutes(3),
            // Some runs are hours late.
            give_up: Duration::hours(4),
        }
    }

    fn leads(&self, _: &SourceDef, _: &str, run: DateTime<Utc>) -> Vec<u32> {
        if !run.hour().is_multiple_of(reg::RUN_EVERY_HOURS) || run.minute() != 0 {
            return Vec::new();
        }
        hours().map(|h| h * 60).collect()
    }

    fn due(&self, _: &SourceDef, _: &str, run: DateTime<Utc>, lead: u32) -> DateTime<Utc> {
        arrival(run, lead / 60)
    }

    fn file(&self, _: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        self.mart.file(run, need)
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        run: DateTime<Utc>,
        _: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let hours: Vec<u32> = self
            .leads(source, reg::PRODUCT, run)
            .iter()
            .map(|l| l / 60)
            .collect();
        Ok(Some(self.mart.list(cx, run, &hours).await?))
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        run: DateTime<Utc>,
        file: &ModelFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let blanking = self.blanking.clone();
        let decode = eccc::decoder_then(run, move |field| blanking.apply(field));
        self.mart.fetch(cx, run, file, needs, decode).await
    }
}

#[cfg(test)]
mod tests;
