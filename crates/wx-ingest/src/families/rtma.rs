//! RTMA: NOAA's hourly surface analyses (temperature, dew point, wind and
//! gusts) in `noaa-rtma-pds`, one file per hour with an `.idx` sidecar,
//! fetched by byte range. The analysis for hour HH appears 35 to 50 minutes
//! later.
//!
//! Each region has its own directory and file name, in the registry's
//! `wx_core::families::rtma::DIRECTORIES`; another region is a registry row
//! and a line there.

use crate::driver::{Cadence, Cx, Fetched, Need, ObsFile, Observed};
use crate::families::common;
use crate::remote::{FetchError, S3Bucket};
use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use std::sync::LazyLock;
use wx_core::SourceDef;
use wx_core::families::rtma as reg;

pub const BUCKET: &str = "noaa-rtma-pds";
static S3: LazyLock<S3Bucket> = LazyLock::new(|| S3Bucket::aws(BUCKET));

/// Where one region's files are: `{dir}.{YYYYMMDD}/{dir}.t{HH}z.{name}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub dir: &'static str,
    /// The file name after `t{HH}z.`.
    pub name: &'static str,
}

/// The layout for a `SourceDef::region`.
pub fn layout(region: &str) -> Option<Layout> {
    reg::DIRECTORIES
        .iter()
        .find(|(r, _, _)| *r == region)
        .map(|(_, dir, name)| Layout { dir, name })
}

#[cfg(test)]
pub fn file_key(l: Layout, hour: DateTime<Utc>) -> String {
    format!(
        "{d}.{}/{d}.t{:02}z.{}",
        hour.format("%Y%m%d"),
        hour.hour(),
        l.name,
        d = l.dir
    )
}

fn day_prefix(l: Layout, day: NaiveDate) -> String {
    format!("{d}.{}/{d}.t", day.format("%Y%m%d"), d = l.dir)
}

/// Analysis hour of an analysis file (not its `.idx`) from its key.
pub fn parse_key(l: Layout, key: &str) -> Option<DateTime<Utc>> {
    let (dir, name) = key.split_once('/')?;
    let day = dir.strip_prefix(l.dir)?.strip_prefix('.')?;
    let rest = name.strip_prefix(l.dir)?.strip_prefix(".t")?;
    let (hh, rest) = rest.split_once("z.")?;
    if rest != l.name || hh.len() != 2 || !hh.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let day = NaiveDate::parse_from_str(day, "%Y%m%d").ok()?;
    Some(day.and_hms_opt(hh.parse().ok()?, 0, 0)?.and_utc())
}

#[derive(Default)]
pub struct Rtma;

fn layout_of(source: &SourceDef) -> Result<Layout, FetchError> {
    common::region("RTMA", source, layout(source.region))
}

#[async_trait::async_trait]
impl Observed for Rtma {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        layout_of(source)?;
        Ok(())
    }

    /// One product, one task, named after the source.
    fn task_name(&self, source: &SourceDef, _: &str, _: &str) -> String {
        source.id.to_string()
    }

    fn cadence(&self, _: &SourceDef, _: &str) -> Cadence {
        Cadence::every(
            Duration::minutes(reg::EVERY_MINUTES as i64),
            Duration::minutes(45),
            Duration::seconds(60),
        )
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        _: &str,
        from: DateTime<Utc>,
    ) -> Result<Vec<ObsFile>, FetchError> {
        let l = layout_of(source)?;
        // Sorts before every file of hour `from` and later that day.
        let start_after = format!("{}{:02}", day_prefix(l, from.date_naive()), from.hour());
        let listed: Vec<_> =
            common::list_days(cx, &S3, from, |day| day_prefix(l, day), &start_after)
                .await?
                .concat();
        let indexed = common::with_sidecars(&listed, ".idx").into_iter().cloned();
        Ok(common::obs_files(indexed, |key| parse_key(l, key)))
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        cx.grib_with_sidecar(&S3.url(&file.name), ".idx", file.size, needs)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::observed::ObservedTask;
    use crate::testing;
    use std::sync::Arc;

    use crate::testing::at as t;

    #[test]
    fn keys_round_trip() {
        let conus = layout("conus").unwrap();
        let key = file_key(conus, t(11, 0));
        assert_eq!(key, "rtma2p5.20261106/rtma2p5.t11z.2dvaranl_ndfd.grb2_wexp");
        assert_eq!(parse_key(conus, &key), Some(t(11, 0)));
        assert_eq!(parse_key(conus, &format!("{key}.idx")), None);
        assert_eq!(
            parse_key(
                conus,
                "rtma2p5.20261106/rtma2p5.t11z.2dvarerr_ndfd.grb2_wexp"
            ),
            None
        );
        assert_eq!(parse_key(conus, "nonsense"), None);
        // `start-after` for hour 11 sorts before hour 11's files and after hour 10's.
        let start_after = format!("{}{:02}", day_prefix(conus, t(0, 0).date_naive()), 11);
        assert!(start_after.as_str() < key.as_str());
        assert!(start_after.as_str() > file_key(conus, t(10, 0)).as_str());
        let ak = layout("ak").unwrap();
        assert_eq!(
            file_key(ak, t(17, 0)),
            "akrtma.20261106/akrtma.t17z.2dvaranl_ndfd_3p0.grb2"
        );
        assert_eq!(
            parse_key(ak, "akrtma.20261106/akrtma.t17z.2dvaranl_ndfd_3p0.grb2"),
            Some(t(17, 0))
        );
        assert_eq!(
            file_key(layout("gu").unwrap(), t(17, 0)),
            "gurtma.20261106/gurtma.t17z.2dvaranl_ndfd.grb2"
        );
        assert!(layout("mars").is_none());
        // Every RTMA row of the registry has a layout.
        for s in wx_core::source::sources_of(wx_core::Family::Rtma) {
            assert!(Rtma.check(s).is_ok(), "{}", s.id);
        }
    }

    /// The real 11Z analysis messages behind a real-shaped inventory: one
    /// listing, one inventory GET and one ranged GET feed all four layers.
    #[tokio::test]
    async fn one_file_feeds_every_layer() {
        let mem = testing::remote(&[BUCKET]);
        let (file, idx) = testing::model_file(&[
            (
                "TMP",
                "2 m above ground",
                "anl",
                "rtma_hi_t17z__TMP_2mabove.grib2",
            ),
            (
                "UGRD",
                "10 m above ground",
                "anl",
                "rtma_hi_t17z__UGRD_10mabove.grib2",
            ),
            (
                "VGRD",
                "10 m above ground",
                "anl",
                "rtma_hi_t17z__VGRD_10mabove.grib2",
            ),
        ]);
        let conus = layout("conus").unwrap();
        let key = file_key(conus, t(17, 0));
        let bucket = S3Bucket::aws(BUCKET);
        mem.put_s3(&bucket, &key, file);
        mem.put_s3(&bucket, &format!("{key}.idx"), idx);
        let (engine, _) = testing::engine(&mem);
        let source = crate::testing::source("rtma");
        let mut task = ObservedTask::new(engine.clone(), Arc::new(Rtma), source, "2dvaranl_ndfd");
        assert_eq!(task.name, "rtma");
        let report = task.cycle(t(18, 0)).await.unwrap();
        // Temperature from the file; dew point, gusts and so on are missing
        // from this test file and are given up, not errors.
        assert_eq!(report.tally.produced, 1, "{:?}", report.tally);
        assert!(report.tally.unavailable >= 2, "{:?}", report.tally);
        assert_eq!(
            engine.catalog.latest("temp", "rtma").unwrap().id,
            "rtma-2611061700"
        );
        let gets: Vec<String> = mem.fetches();
        assert_eq!(gets.len(), 2, "{gets:?}");
    }
}
