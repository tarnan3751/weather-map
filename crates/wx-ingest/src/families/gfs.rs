//! GFS: NOAA's global model, four runs a day, in `noaa-gfs-bdp-pds` at
//! `gfs.{YYYYMMDD}/{HH}/atmos/gfs.t{HH}z.{product}.f{FFF}` with an `.idx`
//! sidecar, fetched by byte range.
//!
//! GFS is the base of every timeline: it covers the world from the recent
//! past (for `fill_past` inputs) out to `horizon_hours`, and finer sources
//! are drawn over it where they have data. Steps are hourly to
//! `HOURLY_HORIZON_HOURS` and three-hourly to `HORIZON_HOURS` (the
//! registry's, `wx_core::families::gfs`). A run lands 3.5 to 5.3 hours after
//! its run time, and is used file by file as it arrives.
//!
//! Two quirks of its inventories are handled by `idx`: levels are matched
//! whole (`UGRD:10 mb` is not `UGRD:10 m above ground`), and of the two
//! `APCP:surface` lines a file holds from f009 on, a run total is the one
//! whose window starts at 0.

use crate::driver::{Cx, Cycle, Fetched, Listing, Model, ModelFile, Need};
use crate::remote::{FetchError, S3Bucket};
use chrono::{DateTime, Duration, Timelike, Utc};
use std::sync::LazyLock;
use wx_core::SourceDef;
use wx_core::families::gfs as reg;

pub const BUCKET: &str = "noaa-gfs-bdp-pds";
static S3: LazyLock<S3Bucket> = LazyLock::new(|| S3Bucket::aws(BUCKET));

/// Forecast hours ingested: hourly, then every third.
pub fn hours() -> impl Iterator<Item = u32> {
    let hourly = 0..=reg::HOURLY_HORIZON_HOURS;
    let three = (reg::HOURLY_HORIZON_HOURS + 3..=reg::HORIZON_HOURS).step_by(3);
    hourly.chain(three)
}

pub fn file_key(run: DateTime<Utc>, product: &str, hour: u32) -> String {
    format!("{}{hour:03}", run_prefix(run, product))
}

fn run_prefix(run: DateTime<Utc>, product: &str) -> String {
    let hh = run.hour();
    format!(
        "gfs.{}/{hh:02}/atmos/gfs.t{hh:02}z.{product}.f",
        run.format("%Y%m%d")
    )
}

/// When file `hour` of a run usually appears: f000 about 3h45 after the run,
/// f168 about 4h16, f384 about 5h15.
pub fn arrival(run: DateTime<Utc>, hour: u32) -> DateTime<Utc> {
    let h = hour as i64;
    let secs = if h <= 168 {
        225 * 60 + h * 31 * 60 / 168
    } else {
        256 * 60 + (h - 168) * 57 * 60 / 216
    };
    run + Duration::seconds(secs)
}

#[derive(Default)]
pub struct Gfs;

#[async_trait::async_trait]
impl Model for Gfs {
    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::hours(reg::RUN_EVERY_HOURS as i64),
            spare: 2,
            // A run takes 90 minutes to land; looking every five is plenty.
            relist: Duration::minutes(5),
            give_up: Duration::minutes(45),
        }
    }

    fn leads(&self, _: &SourceDef, _: &str, run: DateTime<Utc>) -> Vec<u32> {
        if !run.hour().is_multiple_of(reg::RUN_EVERY_HOURS) {
            return Vec::new();
        }
        hours().map(|h| h * 60).collect()
    }

    fn due(&self, _: &SourceDef, _: &str, run: DateTime<Utc>, lead: u32) -> DateTime<Utc> {
        arrival(run, lead / 60)
    }

    fn file(&self, _: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        file_key(run, need.product(), need.lead / 60)
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        run: DateTime<Utc>,
        products: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let mut listed = Vec::new();
        for p in products {
            listed.extend(cx.list_s3(&S3, &run_prefix(run, p), None).await?);
        }
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
mod tests {
    use super::*;
    use crate::driver::model::ModelTask;
    use crate::testing;
    use std::sync::Arc;

    use crate::testing::on as t;

    fn gfs() -> &'static SourceDef {
        crate::testing::source("gfs")
    }

    #[test]
    fn keys_leads_and_arrival() {
        let run = t(6, 6, 0);
        assert_eq!(
            file_key(run, "pgrb2.0p25", 24),
            "gfs.20261106/06/atmos/gfs.t06z.pgrb2.0p25.f024"
        );
        assert_eq!(arrival(run, 0), t(6, 9, 45));
        assert_eq!(arrival(run, 168), t(6, 10, 16));
        assert_eq!(arrival(run, 384), t(6, 11, 13));
        let g = Gfs;
        let leads = g.leads(gfs(), "pgrb2.0p25", run);
        assert_eq!(leads.len(), 121 + 16);
        assert_eq!(
            (leads[0], leads[1], leads[120], leads[121]),
            (0, 60, 120 * 60, 123 * 60)
        );
        assert_eq!(*leads.last().unwrap(), 168 * 60);
        assert!(g.leads(gfs(), "pgrb2.0p25", t(6, 7, 0)).is_empty());
        // At 12:13 the 06Z run is the newest started; the past window
        // (12 hours for temperature) and two spares reach back to 06Z
        // yesterday.
        let runs = g.cycle(gfs()).runs(t(6, 12, 13), Duration::hours(12));
        assert_eq!(runs.first(), Some(&t(6, 12, 0)));
        assert_eq!(runs.last(), Some(&t(5, 12, 0)));
    }

    /// GFS is the world's base now: every lead from the past window to the
    /// horizon is wanted, not only those beyond HRRR's reach.
    #[tokio::test]
    async fn it_fills_the_whole_timeline() {
        let mem = testing::remote(&[BUCKET]);
        let (engine, _) = testing::engine(&mem);
        let group = vec!["pgrb2.0p25"];
        let task = ModelTask::new(engine, Arc::new(Gfs), gfs(), &group);
        let now = t(6, 12, 13);
        let jobs = task.wanted(t(6, 6, 0), now);
        let temp: Vec<u32> = jobs
            .iter()
            .filter(|j| j.layer.id == "temp")
            .map(|j| j.lead / 60)
            .collect();
        // 06Z f000 is in the past window; the run's whole horizon is wanted.
        assert_eq!(temp.first(), Some(&0));
        assert_eq!(temp.last(), Some(&168));
        assert_eq!(temp.len(), 121 + 16);
        // Simulated radar does not fill the past: it starts 90 minutes back.
        let radar: Vec<u32> = jobs
            .iter()
            .filter(|j| j.layer.id == "radar")
            .map(|j| j.lead / 60)
            .collect();
        assert_eq!(radar.first(), Some(&5));
    }

    /// The real f024 inventory and messages: TMP at 2 m, not at 2 mb; wind at
    /// 10 m, not 10 mb.
    #[tokio::test]
    async fn real_messages_by_range() {
        let mem = testing::remote(&[BUCKET]);
        let bucket = S3Bucket::aws(BUCKET);
        let run = t(6, 6, 0);
        let (file, idx) = testing::model_file(&[
            (
                "TMP",
                "2 mb",
                "24 hour fcst",
                "gfs_t06z_f024__SOILW_0-0.1mbelowground_24hour.grib2",
            ),
            (
                "TMP",
                "2 m above ground",
                "24 hour fcst",
                "gfs_t06z_f024__TMP_2mabove.grib2",
            ),
            (
                "UGRD",
                "10 mb",
                "24 hour fcst",
                "gfs_t06z_f024__CSNOW_surface_24hourfcst.grib2",
            ),
            (
                "UGRD",
                "10 m above ground",
                "24 hour fcst",
                "gfs_t06z_f024__UGRD_10mabove.grib2",
            ),
            (
                "VGRD",
                "10 m above ground",
                "24 hour fcst",
                "gfs_t06z_f024__VGRD_10mabove.grib2",
            ),
        ]);
        let key = file_key(run, "pgrb2.0p25", 24);
        mem.put_s3(&bucket, &key, file);
        mem.put_s3(&bucket, &format!("{key}.idx"), idx);
        let (engine, store) = testing::engine(&mem);
        let group = vec!["pgrb2.0p25"];
        let mut task = ModelTask::new(engine.clone(), Arc::new(Gfs), gfs(), &group);
        let report = task.cycle(t(6, 12, 13), &|_, _| {}).await.unwrap();
        // temp from f024 (wind lacks its gust in this file).
        assert!(report.tally.produced >= 1, "{:?}", report.tally);
        let part = engine.catalog.latest("temp", "gfs").unwrap();
        assert_eq!(part.id, "gfs-26110606-1440");
        // The global grid's box spans the world.
        assert!(
            part.bounds.east - part.bounds.west > 359.0,
            "{:?}",
            part.bounds
        );
        assert!(
            testing::keys(&store)
                .await
                .contains(&wx_core::keys::grid("temp", &part.id))
        );
    }
}
