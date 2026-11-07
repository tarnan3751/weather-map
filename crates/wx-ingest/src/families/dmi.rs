//! DMI HARMONIE: DINI and IG, 2 km, in the public `dmi-opendata` bucket at
//! `forecastdata/HARMONIE_{region}_SF/HARMONIE_{region}_SF_{run}_{valid}.grib`
//! (times like `2026-11-06T180000Z`) with a wgrib2-style `.index` beside each
//! file, fetched by byte range.
//!
//! A file holds every surface field of one valid hour, 0.5-0.62 GB, so only
//! the needed messages are fetched: 61 MB a step for DINI, 43 MB for IG. The
//! index is written after its file, so a file counts as there once both are
//! listed. Field names are the index's (`TMP:2 m above ground`), which
//! `wx_pipeline::decode` reads, so payloads are plain GRIB.
//!
//! Winds are grid-relative on the Lambert grids; the decoder reports it and
//! `derive` rotates them to east and north. `TPRATE` and `TSPRATE` are run
//! totals despite their names, differenced into each hour's amount by the
//! driver. Runs start every 3 hours but only those on
//! `INGEST_EVERY_HOURS` are followed (`wx_core::families::dmi` says why);
//! horizons are the registry's.

use crate::driver::{Cx, Cycle, Fetched, Listing, Model, ModelFile, Need};
use crate::families::common;
use crate::remote::{FetchError, S3Bucket};
use chrono::{DateTime, Duration, Timelike, Utc};
use std::sync::LazyLock;
use wx_core::SourceDef;
use wx_core::families::dmi as reg;

pub const BUCKET_URL: &str = "https://dmi-opendata.s3.eu-north-1.amazonaws.com";
static S3: LazyLock<S3Bucket> = LazyLock::new(|| S3Bucket::at(BUCKET_URL));

/// One region's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// As in the keys: `DINI`, `IG`.
    pub name: &'static str,
    pub horizon_hours: u32,
    /// When step 0 usually lands, and how much later each further step.
    first: Duration,
    per_step: Duration,
}

/// Arrival from the 2026-11-06 runs: DINI's step 0 landed 1 h 24 to 1 h 39
/// after the run and step 60 2 h 24 to 3 h 03; IG's step 0 1 h 23 to 1 h 30
/// and step 72 2 h 29 to 3 h 00, its steps out of order. The estimates sit
/// at the early end; files later than that are listed again until
/// `give_up` after it.
pub const DINI: Layout = Layout {
    name: "DINI",
    horizon_hours: reg::DINI_HORIZON_HOURS,
    first: Duration::minutes(85),
    per_step: Duration::seconds(65),
};
pub const IG: Layout = Layout {
    name: "IG",
    horizon_hours: reg::IG_HORIZON_HOURS,
    first: Duration::minutes(83),
    per_step: Duration::seconds(60),
};

pub fn layout(region: &str) -> Option<Layout> {
    match region {
        "DINI" => Some(DINI),
        "IG" => Some(IG),
        _ => None,
    }
}

fn stamp(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H%M%SZ").to_string()
}

/// Every key of `run` starts with this.
fn run_prefix(l: Layout, run: DateTime<Utc>) -> String {
    format!(
        "forecastdata/HARMONIE_{0}_SF/HARMONIE_{0}_SF_{1}_",
        l.name,
        stamp(run)
    )
}

pub fn file_key(l: Layout, run: DateTime<Utc>, hour: u32) -> String {
    format!(
        "{}{}.grib",
        run_prefix(l, run),
        stamp(run + Duration::hours(i64::from(hour)))
    )
}

/// When step `hour` of a run usually appears.
pub fn arrival(l: Layout, run: DateTime<Utc>, hour: u32) -> DateTime<Utc> {
    run + l.first + l.per_step * hour as i32
}

#[derive(Default)]
pub struct Dmi;

fn layout_of(source: &SourceDef) -> Result<Layout, FetchError> {
    common::region("DMI", source, layout(source.region))
}

#[async_trait::async_trait]
impl Model for Dmi {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        layout_of(source)?;
        Ok(())
    }

    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::hours(i64::from(reg::INGEST_EVERY_HOURS)),
            spare: 1,
            // A run lands over about 70 minutes; looking every ten fetches it
            // in about seven batches.
            relist: Duration::minutes(10),
            give_up: Duration::minutes(45),
        }
    }

    fn leads(&self, source: &SourceDef, _: &str, run: DateTime<Utc>) -> Vec<u32> {
        let Some(l) = layout(source.region) else {
            return Vec::new();
        };
        if !run.hour().is_multiple_of(reg::INGEST_EVERY_HOURS) {
            return Vec::new();
        }
        (0..=l.horizon_hours).map(|h| h * 60).collect()
    }

    fn due(&self, source: &SourceDef, _: &str, run: DateTime<Utc>, lead: u32) -> DateTime<Utc> {
        arrival(layout(source.region).unwrap_or(DINI), run, lead / 60)
    }

    fn file(&self, source: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        file_key(layout(source.region).unwrap_or(DINI), run, need.lead / 60)
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        run: DateTime<Utc>,
        _: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let listed = cx
            .list_s3(&S3, &run_prefix(layout_of(source)?, run), None)
            .await?;
        Ok(Some(Listing::with_sidecars(&listed, ".index")))
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: DateTime<Utc>,
        file: &ModelFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        cx.grib_with_sidecar(&S3.url(&file.name), ".index", file.size, needs)
            .await
    }
}

#[cfg(test)]
mod tests;
