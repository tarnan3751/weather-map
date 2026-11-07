//! HRRR: NOAA's 3 km model, in `noaa-hrrr-bdp-pds` at
//! `hrrr.{YYYYMMDD}/{dir}/hrrr.t{HH}z.{product}f{FF}{suffix}.grib2` with an
//! `.idx` sidecar, fetched by byte range.
//!
//! `wrfsfc` holds one forecast hour per file: f00-f18 for every run and
//! f00-f48 for the 00/06/12/18Z runs. `wrfsubh` holds four 15-minute steps
//! per file, f01-f18 only (f01 is 15, 30, 45 and 60 minutes), so one file
//! feeds four parts. Files appear 50 (f00) to 75 (f18) minutes after the run
//! time, 108 for f48.
//!
//! Regions ([`layout`]): CONUS runs every hour; Alaska every three hours,
//! with `.ak` before `.grib2`. Cadences and horizons are the registry's
//! (`wx_core::families::hrrr`), capped by `IngestConfig::hrrr_max_hour`.

use crate::driver::{Cx, Cycle, Fetched, Listing, Model, ModelFile, Need};
use crate::families::common;
use crate::remote::{FetchError, S3Bucket};
use chrono::{DateTime, Duration, Timelike, Utc};
use std::sync::LazyLock;
use wx_core::SourceDef;
use wx_core::families::hrrr as reg;
pub use wx_core::families::hrrr::SUBHOURLY;

pub const BUCKET: &str = "noaa-hrrr-bdp-pds";
static S3: LazyLock<S3Bucket> = LazyLock::new(|| S3Bucket::aws(BUCKET));

/// Where one region's runs are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub dir: &'static str,
    /// Before `.grib2`.
    pub suffix: &'static str,
    /// Hours between runs.
    pub every: i64,
}

const CONUS: Layout = Layout {
    dir: "conus",
    suffix: "",
    every: reg::CONUS_RUN_EVERY_HOURS as i64,
};

pub fn layout(region: &str) -> Option<Layout> {
    match region {
        "conus" => Some(CONUS),
        "alaska" => Some(Layout {
            dir: "alaska",
            suffix: ".ak",
            every: reg::ALASKA_RUN_EVERY_HOURS as i64,
        }),
        _ => None,
    }
}

/// Furthest forecast hour of `product` in `run` under the configured cap.
pub fn last_hour(product: &str, run: DateTime<Utc>, cap: u32) -> u32 {
    let run_max = if reg::LONG_RUN_HOURS.contains(&run.hour()) {
        reg::LONG_HORIZON_HOURS
    } else {
        reg::HORIZON_HOURS
    };
    let max = cap.min(run_max);
    if product == SUBHOURLY {
        max.min(reg::SUBHOURLY_HORIZON_HOURS)
    } else {
        max
    }
}

/// The file (forecast hour) holding a lead: sub-hourly file `h` holds the
/// four steps after hour `h - 1`.
pub fn file_hour(product: &str, lead: u32) -> u32 {
    if product == SUBHOURLY {
        lead.div_ceil(60)
    } else {
        lead / 60
    }
}

/// When file `hour` of a run usually appears.
pub fn arrival(run: DateTime<Utc>, hour: u32) -> DateTime<Utc> {
    let h = hour as i64;
    let secs = if h <= 18 {
        3000 + h * 250 / 3
    } else {
        4500 + (h - 18) * 66
    };
    run + Duration::seconds(secs)
}

pub fn file_key(l: Layout, run: DateTime<Utc>, product: &str, hour: u32) -> String {
    format!("{}{product}f{hour:02}{}.grib2", run_base(l, run), l.suffix)
}

fn run_base(l: Layout, run: DateTime<Utc>) -> String {
    format!(
        "hrrr.{}/{}/hrrr.t{:02}z.",
        run.format("%Y%m%d"),
        l.dir,
        run.hour()
    )
}

/// One listing prefix that covers every product of a run (`...t11z.wrfs`
/// for `wrfsfc` and `wrfsubh`) and not the big pressure-level files.
fn run_prefix(l: Layout, run: DateTime<Utc>, products: &[&str]) -> String {
    let mut common: &str = products.first().copied().unwrap_or("");
    for p in products {
        let n = common
            .bytes()
            .zip(p.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        common = &common[..n];
    }
    format!("{}{common}", run_base(l, run))
}

pub struct Hrrr {
    /// Furthest forecast hour to ingest; every run reaches 18.
    max_hour: u32,
}

impl Hrrr {
    pub fn new(max_hour: u32) -> Self {
        Self { max_hour }
    }
}

fn layout_of(source: &SourceDef) -> Result<Layout, FetchError> {
    common::region("HRRR", source, layout(source.region))
}

#[async_trait::async_trait]
impl Model for Hrrr {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        layout_of(source)?;
        Ok(())
    }

    fn cycle(&self, source: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::hours(layout(source.region).map_or(1, |l| l.every)),
            spare: 3,
            relist: Duration::seconds(30),
            // Files are occasionally 30+ minutes late.
            give_up: Duration::minutes(45),
        }
    }

    fn leads(&self, source: &SourceDef, product: &str, run: DateTime<Utc>) -> Vec<u32> {
        // Off-cycle run times (13Z for Alaska) have no files.
        let every = self.cycle(source).every.num_hours().max(1) as u32;
        if !run.hour().is_multiple_of(every) {
            return Vec::new();
        }
        let last = last_hour(product, run, self.max_hour);
        match product {
            // Sub-hourly f00 holds only the analysis, which radar beats.
            SUBHOURLY => {
                let step = reg::SUBHOURLY_STEP_MINUTES;
                (1..=last * 60 / step).map(|q| q * step).collect()
            }
            _ => (0..=last).map(|h| h * 60).collect(),
        }
    }

    fn due(&self, _: &SourceDef, product: &str, run: DateTime<Utc>, lead: u32) -> DateTime<Utc> {
        arrival(run, file_hour(product, lead))
    }

    fn file(&self, source: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        let l = layout(source.region).unwrap_or(CONUS);
        file_key(l, run, need.product(), file_hour(need.product(), need.lead))
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        run: DateTime<Utc>,
        products: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let listed = cx
            .list_s3(&S3, &run_prefix(layout_of(source)?, run, products), None)
            .await?;
        Ok(Some(Listing::with_sidecars(&listed, ".idx")))
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: DateTime<Utc>,
        file: &ModelFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        cx.grib_with_sidecar(&S3.url(&file.name), ".idx", file.size, needs)
            .await
    }
}

#[cfg(test)]
mod tests;
