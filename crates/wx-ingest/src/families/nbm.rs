//! NBM: NOAA's National Blend of Models, in `noaa-nbm-grib2-pds` at
//! `blend.{YYYYMMDD}/{HH}/core/blend.t{HH}z.core.f{FFF}.{domain}.grib2` with
//! an `.idx` sidecar, fetched by byte range. One file per forecast hour and
//! domain (`co`, `ak`, `hi`, `pr`, `gu`; the registry's `region` is the
//! domain), each a few hundred megabytes in CONUS because it carries the
//! ensemble statistics, of which only a handful of messages are wanted.
//!
//! Only the cycles every [`reg::RUN_EVERY_HOURS`] hours are followed (the
//! registry says why) and only their hourly steps f001-f048 (no f000). A
//! cycle's files appear about an hour after its hour, all within a few
//! minutes.
//!
//! Two quirks of the inventories are handled here:
//!
//! * Many `var:level:time` lines repeat with a qualifier after the time
//!   (`ens std dev`, `prob >0.254`, `10% level`), and the probability line
//!   can come before the deterministic one (`APCP`). Only lines with nothing
//!   after the time are candidates, see [`deterministic`].
//! * `APCP` holds the hour's own record (`5-6 hour acc fcst`), so
//!   [`Steps::Windows`]. From f006 on the file also holds the 6-hour total
//!   `0-6`, which the inventory matching leaves alone.

use crate::driver::{Cx, Cycle, Fetched, Listing, Model, ModelFile, Need, Steps};
use crate::families::common;
use crate::idx::{self, IdxEntry};
use crate::remote::{FetchError, Listed, S3Bucket};
use chrono::{DateTime, Duration, Timelike, Utc};
use std::sync::LazyLock;
use wx_core::SourceDef;
use wx_core::families::nbm as reg;

pub const BUCKET: &str = "noaa-nbm-grib2-pds";
static S3: LazyLock<S3Bucket> = LazyLock::new(|| S3Bucket::aws(BUCKET));

/// The domains NBM has files for, which are the registry's regions.
pub const DOMAINS: [&str; 5] = ["co", "ak", "hi", "pr", "gu"];

/// How long after the cycle hour the files usually appear (45 to 69 minutes
/// measured).
const LATENCY_MINUTES: i64 = 65;

fn domain(source: &SourceDef) -> Result<&'static str, FetchError> {
    common::region(
        "NBM",
        source,
        DOMAINS.iter().copied().find(|d| *d == source.region),
    )
}

/// Forecast hours ingested: hourly from f001.
pub fn hours() -> impl Iterator<Item = u32> {
    1..=reg::HORIZON_HOURS
}

pub fn file_key(run: DateTime<Utc>, domain: &str, hour: u32) -> String {
    format!("{}{hour:03}.{domain}.grib2", file_prefix(run))
}

/// Up to the forecast hour.
fn file_prefix(run: DateTime<Utc>) -> String {
    let hh = run.hour();
    format!(
        "blend.{}/{hh:02}/core/blend.t{hh:02}z.core.f",
        run.format("%Y%m%d")
    )
}

/// One listing prefix for a run's hourly files and the three-hourly ones up
/// to f099 (`f0`), which keeps a listing to one page of 1000 keys: 65 hours
/// of six domains with their inventories is 780, where the whole cycle
/// (f001 to f264) would be about 2000.
fn list_prefix(run: DateTime<Utc>) -> String {
    format!("{}0", file_prefix(run))
}

/// When the files of a run usually appear: all at once.
pub fn arrival(run: DateTime<Utc>) -> DateTime<Utc> {
    run + Duration::minutes(LATENCY_MINUTES)
}

/// The lines of an inventory that hold the deterministic field: nothing
/// after the forecast time. Message ends are those of the whole inventory,
/// so a message never stretches over the ones left out.
pub fn deterministic(text: &str) -> Vec<IdxEntry> {
    idx::parse_where(text, |rest| rest.split(':').all(|q| q.trim().is_empty()))
}

#[derive(Debug, Default)]
pub struct Nbm;

#[async_trait::async_trait]
impl Model for Nbm {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        domain(source)?;
        Ok(())
    }

    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::hours(reg::RUN_EVERY_HOURS as i64),
            // A run is hundreds of megabytes; an older one only fills what a
            // newer cannot, and the past window already reaches back.
            spare: 1,
            relist: Duration::minutes(2),
            give_up: Duration::minutes(45),
        }
    }

    fn steps(&self, _: &SourceDef, _: &str) -> Steps {
        Steps::Windows
    }

    fn leads(&self, _: &SourceDef, _: &str, run: DateTime<Utc>) -> Vec<u32> {
        if !run.hour().is_multiple_of(reg::RUN_EVERY_HOURS) {
            return Vec::new();
        }
        hours().map(|h| h * 60).collect()
    }

    fn due(&self, _: &SourceDef, _: &str, run: DateTime<Utc>, _: u32) -> DateTime<Utc> {
        arrival(run)
    }

    fn file(&self, source: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        file_key(run, source.region, need.lead / 60)
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        run: DateTime<Utc>,
        _: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let ours = format!(".{}.grib2", domain(source)?);
        let listed: Vec<Listed> = cx
            .list_s3(&S3, &list_prefix(run), None)
            .await?
            .into_iter()
            .filter(|l| l.key.ends_with(&ours) || l.key.ends_with(&format!("{ours}.idx")))
            .collect();
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
        let url = S3.url(&file.name);
        cx.grib_with_index(&url, &format!("{url}.idx"), file.size, needs, deterministic)
            .await
    }
}

#[cfg(test)]
mod tests;
