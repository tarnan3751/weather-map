//! GMGSI: NOAA's hourly global mosaic of geostationary infrared imagery, the
//! worldwide base of the satellite layer. Layout, grid and calibration are
//! in the registry module `wx_core::families::gmgsi`.
//!
//! One file an hour under an hour prefix, with a creation stamp in its name
//! that cannot be guessed; a UTC day has 24, so one listing of the day
//! directory (from the hour wanted on) finds every file and the newest. The
//! valid time is the hour in the name, which is the time the decoder reads
//! from the file.

use crate::clock;
use crate::driver::{Cadence, Cx, Decoded, Fetched, Need, ObsFile, Observed, Payload};
use crate::families::common;
use crate::remote::{FetchError, S3Bucket};
use anyhow::ensure;
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Timelike, Utc};
use std::sync::{Arc, LazyLock};
use wx_core::SourceDef;
use wx_core::families::gmgsi as reg;

pub const BUCKET: &str = "noaa-gmgsi-pds";
static S3: LazyLock<S3Bucket> = LazyLock::new(|| S3Bucket::aws(BUCKET));
/// Files are about 7.3 MB.
const MAX_FILE: u64 = 32 * 1024 * 1024;

#[derive(Default)]
pub struct Gmgsi;

/// The file-name prefix of a product directory's files. Only the long-wave
/// mosaic is calibrated (`wx_formats::gmgsi`).
pub fn file_prefix(product: &str) -> Option<&'static str> {
    match product {
        "GMGSI_LW" => Some("GLOBCOMPLIR"),
        _ => None,
    }
}

pub fn day_prefix(product: &str, day: NaiveDate) -> String {
    format!("{product}/{}/", day.format("%Y/%m/%d"))
}

/// Sorts before every key of the hour starting at `hour` and after those of
/// earlier hours that day.
pub fn hour_prefix(product: &str, hour: DateTime<Utc>) -> String {
    format!(
        "{}{:02}/",
        day_prefix(product, hour.date_naive()),
        hour.hour()
    )
}

/// Valid time (the start of the hour) of a product file from its key:
/// `{product}/YYYY/MM/DD/HH/{prefix}_v3r0_blend_sYYYYMMDDHH00000_e..._c....nc`.
pub fn parse_key(product: &str, key: &str) -> Option<DateTime<Utc>> {
    let prefix = file_prefix(product)?;
    let rest = key.strip_prefix(product)?.strip_prefix('/')?;
    let (dirs, name) = rest.rsplit_once('/')?;
    let stamp = name.strip_prefix(prefix)?.split_once("_s")?.1;
    if !name.ends_with(".nc") || stamp.len() < 12 {
        return None;
    }
    let valid = NaiveDateTime::parse_from_str(&stamp[..12], "%Y%m%d%H%M")
        .ok()?
        .and_utc();
    // The directories must agree with the name.
    (dirs == valid.format("%Y/%m/%d/%H").to_string()).then_some(valid)
}

#[async_trait::async_trait]
impl Observed for Gmgsi {
    fn cadence(&self, _: &SourceDef, _: &str) -> Cadence {
        let minutes = |m: u32| Duration::minutes(i64::from(m));
        Cadence::every(
            minutes(reg::EVERY_MINUTES),
            minutes(reg::LATENCY_MINUTES),
            Duration::seconds(60),
        )
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        product: &str,
        from: DateTime<Utc>,
    ) -> Result<Vec<ObsFile>, FetchError> {
        if file_prefix(product).is_none() {
            return Err(FetchError::object(
                product,
                "not a calibrated GMGSI product",
            ));
        }
        let start_after = hour_prefix(product, clock::floor(from, Duration::hours(1)));
        let days =
            common::list_days(cx, &S3, from, |day| day_prefix(product, day), &start_after).await?;
        let mut out = Vec::new();
        for (day, listed) in clock::days(from, cx.now).into_iter().zip(days) {
            let count = listed.len();
            let files = common::obs_files(listed, |key| parse_key(product, key));
            // Files of the product and none the adapter can read: the names
            // have changed, and the task would otherwise look healthy while
            // it finds nothing. (No files at all is an outage.)
            if files.is_empty() && count > 0 {
                return Err(FetchError::object(
                    format!("{}/{}", S3.base(), day_prefix(product, day)),
                    format!(
                        "{count} files but none named as a {product} hour: has the naming changed?"
                    ),
                ));
            }
            out.extend(files);
        }
        Ok(out)
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let hour = file.valid;
        let decode: crate::driver::Decoder = Arc::new(move |bytes: &[u8], _: &Need| {
            let d = wx_formats::gmgsi::decode(bytes)?;
            ensure!(
                d.valid == hour,
                "the file says {} but is named after {hour}",
                d.valid
            );
            Ok(Decoded {
                field: d.field,
                valid: Some(d.valid),
            })
        });
        cx.whole(
            &S3.url(&file.name),
            needs,
            MAX_FILE,
            Payload::decoded_by(decode),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use chrono::TimeZone;

    const PRODUCT: &str = "GMGSI_LW";
    const CUT: &str = "formats/gmgsi_GLOBCOMPLIR_20261106T1700_rows700-1500_cols900-1700.nc";

    use crate::testing::at as t;

    fn key(hour: u32, created: &str) -> String {
        format!(
            "GMGSI_LW/2026/11/06/{hour:02}/GLOBCOMPLIR_v3r0_blend_s20261106{hour:02}00000_e20261106{hour:02}09599_c{created}.nc"
        )
    }

    #[test]
    fn keys() {
        assert_eq!(reg::SATELLITE.product, PRODUCT);
        let k = key(17, "202611061733476");
        assert_eq!(parse_key(PRODUCT, &k), Some(t(17, 0)));
        assert_eq!(parse_key(PRODUCT, &k.replace("/17/", "/16/")), None);
        assert_eq!(
            parse_key(PRODUCT, &k.replace("GLOBCOMPLIR", "GLOBCOMPVIS")),
            None
        );
        assert_eq!(
            parse_key("GMGSI_VIS", &k.replace("GMGSI_LW", "GMGSI_VIS")),
            None
        );
        assert_eq!(parse_key(PRODUCT, "GMGSI_LW/2026/11/06/17/"), None);
        let before = hour_prefix(PRODUCT, t(17, 0));
        assert!(before < k && before > key(16, "202611061633473"));
        assert!(k.starts_with(&day_prefix(PRODUCT, t(0, 0).date_naive())));
    }

    /// The cut of the real 17Z mosaic, found in the day listing among other
    /// hours and products, becomes a part valid at 17:00.
    #[tokio::test]
    async fn the_newest_hour_becomes_a_part() {
        let mem = testing::remote(&[BUCKET]);
        let bucket = S3Bucket::aws(BUCKET);
        mem.put_s3(&bucket, &key(17, "202611061733476"), testing::fixture(CUT));
        // Older than the loop and its 160-minute hold: 11:00 + 2:40 is before 14:45.
        mem.put_s3(
            &bucket,
            &key(11, "202611061133470"),
            &b"too old for the loop"[..],
        );
        mem.put_s3(&bucket, "GMGSI_VIS/2026/11/06/17/GLOBCOMPVIS_v3r0_blend_s202611061700000_e202611061709599_c202611061739001.nc", &b"x"[..]);
        let (engine, _) = testing::engine(&mem);
        let mut task = crate::testing::observed_task(&engine, Gmgsi, "gmgsi", PRODUCT);
        assert_eq!(task.name, "gmgsi.satellite");
        let report = task.cycle(t(17, 45)).await.unwrap();
        assert_eq!(report.tally.produced, 1, "{:?}", report.tally);
        let part = engine.catalog.latest("satellite", "gmgsi").unwrap();
        assert_eq!(part.id, "gmgsi-2611061700");
        // The cut covers 115 to 58 W, the equator to 50 N.
        let b = part.bounds;
        assert!(
            b.west < -115.0 && b.west > -115.3 && b.east > -57.7 && b.east < -57.5,
            "{b:?}"
        );
        assert!(b.south < 0.1 && b.north > 49.7 && b.north < 49.9, "{b:?}");
        assert!(part.solid.is_some());
        let gets: Vec<String> = mem.fetches();
        assert_eq!(gets.len(), 1, "{gets:?}");
        // One listing of the day, starting at the hour the loop window less
        // the part's hold reaches back to (14:45 less 2:40).
        let lists: Vec<String> = mem.listings();
        assert_eq!(lists.len(), 1, "{lists:?}");
        assert!(
            lists[0].contains("start-after=GMGSI_LW%2F2026%2F11%2F06%2F12%2F"),
            "{lists:?}"
        );
    }

    /// Files under the product that no longer parse stop the task with an
    /// error instead of letting it look healthy while it finds nothing.
    #[tokio::test]
    async fn renamed_files_are_an_error_not_silence() {
        let mem = testing::remote(&[BUCKET]);
        let bucket = S3Bucket::aws(BUCKET);
        mem.put_s3(
            &bucket,
            &key(17, "202611061733476").replace("GLOBCOMPLIR_", "GLOBCOMP_LIR_"),
            &b"x"[..],
        );
        let (engine, _) = testing::engine(&mem);
        let mut task = crate::testing::observed_task(&engine, Gmgsi, "gmgsi", PRODUCT);
        let e = task.cycle(t(17, 45)).await.err().expect("an error");
        assert!(format!("{e:#}").contains("has the naming changed"), "{e:#}");
    }

    /// Before midnight's file is written, the listing of the new day is
    /// empty and the previous day's newest hour is found.
    #[tokio::test]
    async fn across_midnight() {
        let mem = testing::remote(&[BUCKET]);
        let bucket = S3Bucket::aws(BUCKET);
        mem.put_s3(
            &bucket,
            &key(23, "202611062333476"),
            testing::fixture(CUT).clone(),
        );
        let (engine, _) = testing::engine(&mem);
        let mut task = crate::testing::observed_task(&engine, Gmgsi, "gmgsi", PRODUCT);
        let now = Utc.with_ymd_and_hms(2026, 11, 7, 0, 20, 0).unwrap();
        let report = task.cycle(now).await.unwrap();
        // It is named 23:00 but says 17:00 inside: refused as broken.
        assert_eq!(report.tally.failed, 1, "{:?}", report.tally);
        let lists: Vec<String> = mem.listings();
        assert_eq!(lists.len(), 2, "{lists:?}");
        assert!(lists[1].contains("2026%2F11%2F07%2F"));
    }
}
