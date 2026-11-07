//! What several families do the same way: finding a source's row in their
//! region tables, listing by day, and turning listings into files.

use crate::clock;
use crate::driver::{Cx, ObsFile};
use crate::remote::{FetchError, Listed, S3Bucket};
use chrono::{DateTime, NaiveDate, Utc};
use std::collections::HashSet;
use wx_core::SourceDef;

/// `source`'s entry in `family`'s table of regions (`found`), or an error
/// saying there is none.
pub fn region<T>(family: &str, source: &SourceDef, found: Option<T>) -> Result<T, FetchError> {
    found.ok_or_else(|| {
        FetchError::object(
            source.id,
            format!("no {family} layout for region {:?}", source.region),
        )
    })
}

/// A family with one domain names no region.
pub fn one_region(source: &SourceDef, family: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        source.region.is_empty(),
        "{}: {family} has one region, not {:?}",
        source.id,
        source.region
    );
    Ok(())
}

/// One listing per UTC day from `from`'s to now's, of `prefix(day)`, the
/// first day's starting after `start_after` (a key that sorts just before
/// the first file valid at `from`).
pub async fn list_days(
    cx: &Cx<'_>,
    bucket: &S3Bucket,
    from: DateTime<Utc>,
    prefix: impl Fn(NaiveDate) -> String,
    start_after: &str,
) -> Result<Vec<Vec<Listed>>, FetchError> {
    let mut out = Vec::new();
    for day in clock::days(from, cx.now) {
        let first = day == from.date_naive();
        out.push(
            cx.list_s3(bucket, &prefix(day), first.then_some(start_after))
                .await?,
        );
    }
    Ok(out)
}

/// The listed objects whose key `valid` gives a time, as files.
pub fn obs_files(
    listed: impl IntoIterator<Item = Listed>,
    valid: impl Fn(&str) -> Option<DateTime<Utc>>,
) -> Vec<ObsFile> {
    let file = |l: Listed| {
        Some(ObsFile {
            valid: valid(&l.key)?,
            size: Some(l.size),
            name: l.key,
        })
    };
    listed.into_iter().filter_map(file).collect()
}

/// The listed objects that have their index beside them (`x` when
/// `x{sidecar}` is listed too). Indexes are written after their files, so
/// a file counts as there once both are.
pub fn with_sidecars<'a>(listed: &'a [Listed], sidecar: &str) -> Vec<&'a Listed> {
    let keys: HashSet<&str> = listed.iter().map(|l| l.key.as_str()).collect();
    let indexed = |l: &&Listed| {
        !l.key.ends_with(sidecar) && keys.contains(format!("{}{sidecar}", l.key).as_str())
    };
    listed.iter().filter(indexed).collect()
}
