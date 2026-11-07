//! MRMS: NOAA's radar mosaics and radar-estimated rainfall. Each product is
//! a directory per UTC day of small gzipped single-message files named by
//! valid time, in `noaa-mrms-pds`:
//! `{region}/{Product}/{YYYYMMDD}/MRMS_{Product}_{YYYYMMDD}-{HHMMSS}.grib2.gz`.
//!
//! `SourceDef::region` is the domain directory (`CONUS`, `ALASKA`, `HAWAII`,
//! `CARIB`, `GUAM`): every domain has the same product names and timing, so
//! another domain is one registry row. Each product runs as its own task
//! (`mrms.radar`, `mrms_ak.rain_1h`) so one that fails, or is slow to
//! backfill, never holds up the radar.

use crate::driver::{Cadence, Cx, Fetched, Need, ObsFile, Observed, Payload};
use crate::families::common;
use crate::remote::{FetchError, S3Bucket};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use std::sync::LazyLock;
use wx_core::SourceDef;

pub const BUCKET: &str = "noaa-mrms-pds";
static S3: LazyLock<S3Bucket> = LazyLock::new(|| S3Bucket::aws(BUCKET));
/// Whole files are a few megabytes; anything near this is not an MRMS file.
const MAX_FILE: u64 = 64 * 1024 * 1024;

#[derive(Default)]
pub struct Mrms;

/// What is known about a product's files.
pub fn cadence(product: &str) -> Cadence {
    let (period, latency, stride, poll) = match product {
        "MergedReflectivityQCComposite_00.50" => (120, 40, None, 20),
        // A new hourly total every two minutes would be 90 near-identical
        // parts per loop; one per ten minutes reads the same. Files land
        // about 2m50s after their timestamp (S3 LastModified, 2026-11-06).
        "RadarOnly_QPE_01H_00.00" => (120, 150, Some(600), 30),
        "RadarOnly_QPE_24H_00.00" | "RadarOnly_QPE_72H_00.00" => (3600, 210, None, 60),
        // Unknown products are treated like the two-minute ones.
        _ => (120, 60, None, 30),
    };
    Cadence {
        period: Duration::seconds(period),
        latency: Duration::seconds(latency),
        stride: stride.map(Duration::seconds),
        poll: Duration::seconds(poll),
    }
}

pub fn dir(region: &str, product: &str, day: NaiveDate) -> String {
    format!("{region}/{product}/{}/", day.format("%Y%m%d"))
}

/// A string that sorts immediately before the key of the file valid at `t`,
/// for use as `start-after`.
pub fn key_before(region: &str, product: &str, t: DateTime<Utc>) -> String {
    let d = t.format("%Y%m%d");
    format!(
        "{region}/{product}/{d}/MRMS_{product}_{d}-{}",
        t.format("%H%M%S")
    )
}

/// Valid time of a product file from its key.
pub fn parse_key(product: &str, key: &str) -> Option<DateTime<Utc>> {
    let name = key.rsplit('/').next()?;
    let stamp = name
        .strip_prefix("MRMS_")?
        .strip_prefix(product)?
        .strip_prefix('_')?
        .strip_suffix(".grib2.gz")?;
    NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S")
        .ok()
        .map(|t| t.and_utc())
}

#[async_trait::async_trait]
impl Observed for Mrms {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        anyhow::ensure!(
            !source.region.is_empty()
                && source
                    .region
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "{}: the region must be an MRMS domain directory such as CONUS",
            source.id
        );
        Ok(())
    }

    fn cadence(&self, _: &SourceDef, product: &str) -> Cadence {
        cadence(product)
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        product: &str,
        from: DateTime<Utc>,
    ) -> Result<Vec<ObsFile>, FetchError> {
        let region = source.region;
        let days = common::list_days(
            cx,
            &S3,
            from,
            |day| dir(region, product, day),
            &key_before(region, product, from),
        )
        .await?;
        Ok(common::obs_files(days.into_iter().flatten(), |key| {
            parse_key(product, key)
        }))
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        cx.whole(&S3.url(&file.name), needs, MAX_FILE, Payload::Grib)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::observed::{ObservedTask, next_due, pick};
    use crate::testing;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use crate::testing::hms as t;

    const REFL: &str = "MergedReflectivityQCComposite_00.50";

    fn file(v: DateTime<Utc>) -> ObsFile {
        ObsFile {
            valid: v,
            name: String::new(),
            size: None,
        }
    }

    #[test]
    fn keys_and_times() {
        let key = "CONUS/MergedReflectivityQCComposite_00.50/20261106/\
                   MRMS_MergedReflectivityQCComposite_00.50_20261106-114838.grib2.gz";
        assert_eq!(parse_key(REFL, key), Some(t(11, 48, 38)));
        assert_eq!(parse_key("RadarOnly_QPE_01H_00.00", key), None);
        assert_eq!(
            parse_key(REFL, "CONUS/x/20261106/MRMS_x_20261106-114838.grib2"),
            None
        );
        assert_eq!(parse_key(REFL, &key.replace("114838", "116038")), None);
        assert_eq!(parse_key(REFL, ""), None);
        let day = t(0, 0, 0).date_naive();
        assert_eq!(
            dir("CONUS", REFL, day),
            "CONUS/MergedReflectivityQCComposite_00.50/20261106/"
        );
        assert_eq!(
            dir("ALASKA", REFL, day),
            "ALASKA/MergedReflectivityQCComposite_00.50/20261106/"
        );
        let before = key_before("CONUS", REFL, t(11, 48, 38));
        assert!(before.as_str() < key && key.starts_with(&before));
        assert!(key_before("CONUS", REFL, t(11, 48, 39)).as_str() > key);
        assert!(key.starts_with(&dir("CONUS", REFL, day)));
    }

    #[test]
    fn every_file_without_a_stride_newest_first() {
        let backlog: BTreeMap<_, _> = [t(11, 44, 0), t(11, 46, 0), t(11, 48, 0)]
            .into_iter()
            .map(|v| (v, file(v)))
            .collect();
        let (picks, forget) = pick(&backlog, None, &BTreeSet::new(), t(10, 0, 0), |v| {
            v != t(11, 46, 0)
        });
        assert_eq!(picks, vec![t(11, 48, 0), t(11, 44, 0)]);
        assert_eq!(forget, vec![t(11, 46, 0)]);
    }

    #[test]
    fn one_file_per_stride_bucket() {
        // Every two minutes from 11:30 to 12:00.
        let backlog: BTreeMap<_, _> = (0..16)
            .map(|i| t(11, 30, 0) + Duration::minutes(2 * i))
            .map(|v| (v, file(v)))
            .collect();
        let stride = Some(Duration::minutes(10));
        // 11:30 already has a part; 11:40 is unusable, so 11:42 stands in.
        let filled: BTreeSet<_> = [t(11, 30, 0)].into();
        let (picks, forget) = pick(&backlog, stride, &filled, t(11, 20, 0), |v| {
            v != t(11, 40, 0)
        });
        assert_eq!(picks, vec![t(12, 0, 0), t(11, 50, 0), t(11, 42, 0)]);
        // A bucket that began before the loop window is not worth filling.
        let (picks, _) = pick(&backlog, stride, &BTreeSet::new(), t(11, 31, 0), |_| true);
        assert_eq!(picks, vec![t(12, 0, 0), t(11, 50, 0), t(11, 40, 0)]);
        assert!(forget.contains(&t(11, 30, 0)) && forget.contains(&t(11, 38, 0)));
        assert!(forget.contains(&t(11, 40, 0)));
        assert!(
            !forget.contains(&t(11, 44, 0)),
            "fallbacks stay in the backlog"
        );
    }

    #[test]
    fn polls_when_the_next_file_is_due() {
        let radar = cadence(REFL);
        assert_eq!(next_due(&radar, None), None);
        assert_eq!(next_due(&radar, Some(t(11, 48, 38))), Some(t(11, 51, 18)));
        let rain = cadence("RadarOnly_QPE_01H_00.00");
        assert_eq!(next_due(&rain, Some(t(11, 42, 0))), Some(t(11, 52, 30)));
        let daily = cadence("RadarOnly_QPE_24H_00.00");
        assert_eq!(next_due(&daily, Some(t(11, 0, 0))), Some(t(12, 3, 30)));
    }

    fn url(region: &str, product: &str, v: DateTime<Utc>) -> String {
        S3Bucket::aws(BUCKET).url(&format!(
            "{}MRMS_{product}_{}.grib2.gz",
            dir(region, product, v.date_naive()),
            v.format("%Y%m%d-%H%M%S")
        ))
    }

    /// The real 11:48:38 composite, listed and fetched through the remote,
    /// decoded, built, stored and inserted as `mrms-2611061148`.
    #[tokio::test]
    async fn a_listed_file_becomes_a_part() {
        let mem = testing::mrms_remote();
        let gz =
            testing::fixture("MRMS_MergedReflectivityQCComposite_00.50_20261106-114838.grib2.gz");
        mem.put(&url("CONUS", REFL, t(11, 48, 38)), gz);
        let (engine, store) = testing::engine(&mem);
        let source = crate::testing::source("mrms");
        let mut task = ObservedTask::new(engine.clone(), Arc::new(Mrms), source, REFL);
        assert_eq!(task.name, "mrms.radar");
        let report = task.cycle(t(11, 52, 0)).await.unwrap();
        assert_eq!(report.tally.produced, 1, "{:?}", report.tally);
        let part = engine.catalog.latest("radar", "mrms").unwrap();
        assert_eq!(part.id, "mrms-2611061148");
        assert_eq!(part.valid, t(11, 48, 0));
        assert_eq!(part.max_zoom, source.max_zoom);
        assert!(
            part.bounds.west < -120.0 && part.bounds.east > -70.0,
            "{:?}",
            part.bounds
        );
        assert!(
            testing::keys(&store)
                .await
                .contains(&wx_core::keys::grid("radar", &part.id))
        );
        // Handled: the next cycle fetches nothing.
        mem.clear_log();
        let again = task.cycle(t(11, 52, 20)).await.unwrap();
        assert_eq!(again.tally, Default::default());
        assert!(
            mem.requests().iter().all(|r| r.contains("list-type=2")),
            "{:?}",
            mem.requests()
        );
    }

    /// A file stamped 50 minutes ahead is ignored: it would move the listing
    /// position into the future and hide the real files that follow.
    #[tokio::test]
    async fn a_future_dated_file_does_not_hide_real_ones() {
        let mem = testing::mrms_remote();
        for v in [t(11, 56, 38), t(11, 58, 40), t(12, 50, 0)] {
            mem.put(&url("CONUS", REFL, v), &b"x"[..]);
        }
        let (engine, _) = testing::engine(&mem);
        let mut task = crate::testing::observed_task(&engine, Mrms, "mrms", REFL);
        let now = t(12, 0, 0);
        task.cycle(now).await.unwrap();
        // The 12:50 file is ignored; the two real ones are not worth
        // anything (they do not decode) but were looked at.
        assert!(mem.requests().iter().all(|r| !r.contains("125000")));
        assert!(mem.requests().iter().any(|r| r.contains("115840")));

        // The next real file is found.
        mem.put(&url("CONUS", REFL, t(12, 0, 37)), &b"x"[..]);
        mem.clear_log();
        task.cycle(t(12, 1, 30)).await.unwrap();
        assert!(
            mem.requests()
                .iter()
                .any(|r| r.contains("120037") && !r.contains("list-type"))
        );
    }

    #[test]
    fn domains_are_directories() {
        let m = Mrms;
        let mut s = *crate::testing::source("mrms");
        assert!(m.check(&s).is_ok());
        s.region = "";
        assert!(m.check(&s).is_err());
        s.region = "../x";
        assert!(m.check(&s).is_err());
    }
}
