//! OPERA: EUMETNET's pan-European radar composites, from the anonymous
//! `openradar-24h` bucket on CloudFerro, which keeps a rolling 24 hours:
//! `{YYYY}/{MM}/{DD}/OPERA/{region}/OPERA@{YYYYMMDD}T{HHMM}@0@{product}.h5`,
//! the time being the nominal end of the scan cycle. `SourceDef::region` is
//! the directory (`COMP`). Each product runs as its own task: `opera.radar`
//! (`DBZH`, maximum reflectivity at 1 km every 5 minutes) and
//! `opera.rain_1h` (`ACRR`, the hour's accumulation at 2 km every 15
//! minutes).
//!
//! New files are found with one listing of the day's prefix that starts
//! after the oldest time still wanted, so a poll returns a page of a few
//! hundred bytes per file since then (both products and their GeoTIFF
//! twins, which are skipped).
//!
//! A DBZH file is about 2 MB but 16.7 million float64 cells: decoding,
//! deriving and gridding it is the biggest build of these sources after
//! DWD's 250 m composite, and runs under the shared build limit like
//! every other.

use crate::driver::{Cadence, Cx, Decoded, Decoder, Fetched, Need, ObsFile, Observed, Payload};
use crate::families::common;
use crate::remote::{FetchError, S3Bucket};
use anyhow::ensure;
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeDelta, Utc};
use std::sync::Arc;
use wx_core::SourceDef;
use wx_core::families::opera as reg;
use wx_formats::Quantity;

pub const BUCKET_URL: &str = "https://s3.waw3-1.cloudferro.com/openradar-24h";
/// Files are 1.5 to 2.1 MB; anything near this is not an OPERA composite.
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// No file of a product newer than this many of its periods (and its
/// latency) is a source that has stopped or moved: a failing task, not a
/// quiet one.
const STALE_PERIODS: i32 = 6;

pub struct Opera {
    bucket: S3Bucket,
}

impl Default for Opera {
    fn default() -> Self {
        Self::at(BUCKET_URL)
    }
}

impl Opera {
    /// An adapter reading from another endpoint (tests).
    pub fn at(base: &str) -> Self {
        Self {
            bucket: S3Bucket::at(base),
        }
    }
}

/// What is known about a product's files. Latencies are the bucket's
/// `LastModified` minus the file's time on 2026-11-06: DBZH 4m06s to
/// 4m19s, ACRR 12m02s to 12m06s.
pub fn cadence(product: &str) -> Cadence {
    let (every, latency, poll) = match product {
        p if p == reg::RAIN_1H.product => (reg::ACRR_EVERY_MINUTES, 750, 60),
        _ => (reg::DBZH_EVERY_MINUTES, 270, 30),
    };
    Cadence::every(
        Duration::minutes(every as i64),
        Duration::seconds(latency),
        Duration::seconds(poll),
    )
}

pub fn dir(region: &str, day: NaiveDate) -> String {
    format!("{}/OPERA/{region}/", day.format("%Y/%m/%d"))
}

/// A string that sorts immediately before the keys of the files valid at
/// `t`, whatever their product, for use as `start-after`.
pub fn key_before(region: &str, t: DateTime<Utc>) -> String {
    format!(
        "{}OPERA@{}",
        dir(region, t.date_naive()),
        t.format("%Y%m%dT%H%M")
    )
}

#[cfg(test)]
pub fn key(region: &str, product: &str, t: DateTime<Utc>) -> String {
    format!("{}@0@{product}.h5", key_before(region, t))
}

/// Valid time of a `product` file from its key; `None` for other products,
/// the GeoTIFF twins and anything else.
pub fn parse_key(product: &str, key: &str) -> Option<DateTime<Utc>> {
    let name = key.rsplit('/').next()?;
    let rest = name.strip_prefix("OPERA@")?.strip_suffix(".h5")?;
    let (stamp, rest) = rest.split_once('@')?;
    let (_, p) = rest.split_once('@')?;
    if p != product {
        return None;
    }
    NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M")
        .ok()
        .map(|t| t.and_utc())
}

/// Decodes an OPERA file, checking it holds what the product names: a
/// reflectivity for DBZH, a one-hour accumulation for ACRR.
pub fn decoder() -> Decoder {
    Arc::new(|bytes: &[u8], need: &Need| {
        let d = wx_formats::opera::decode(bytes)?;
        let product = need.product();
        if product == reg::RAIN_1H.product {
            ensure!(
                d.quantity == Quantity::Accumulation,
                "an OPERA {product} file holding {:?}",
                d.quantity
            );
            ensure!(
                d.period == TimeDelta::hours(1),
                "an OPERA {product} file over {} minutes",
                d.period.num_minutes()
            );
        } else {
            ensure!(
                d.quantity == Quantity::Reflectivity,
                "an OPERA {product} file holding {:?}",
                d.quantity
            );
        }
        Ok(Decoded {
            field: d.field,
            valid: Some(d.valid),
        })
    })
}

#[async_trait::async_trait]
impl Observed for Opera {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        anyhow::ensure!(
            !source.region.is_empty() && source.region.bytes().all(|b| b.is_ascii_alphanumeric()),
            "{}: the region must be an OPERA product directory such as COMP",
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
            &self.bucket,
            from,
            |day| dir(region, day),
            &key_before(region, from),
        )
        .await?;
        let out = common::obs_files(days.into_iter().flatten(), |key| parse_key(product, key));
        let c = cadence(product);
        let newest = out.iter().map(|f| f.valid).max();
        if newest.is_none_or(|t| t < cx.now - c.period * STALE_PERIODS - c.latency) {
            return Err(FetchError::service(
                format!(
                    "listing {}/{}",
                    self.bucket.base(),
                    dir(region, cx.now.date_naive())
                ),
                format!(
                    "no {product} file since {}: the composite has stopped or moved",
                    newest.unwrap_or(from)
                ),
            ));
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
        cx.whole(
            &self.bucket.url(&file.name),
            needs,
            MAX_FILE,
            Payload::decoded_by(decoder()),
        )
        .await
    }
}

#[cfg(test)]
pub(crate) mod tests;
