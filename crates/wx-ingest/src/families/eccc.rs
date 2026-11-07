//! ECCC's MSC Datamart (`dd.weather.gc.ca`), where GDPS and HRDPS are
//! published: one field at one step per GRIB2 file, in a directory per date,
//! run and step:
//!
//! ```text
//! {host}/{YYYYMMDD}/WXO-DD/{product}/{HH}/{hhh}/{YYYYMMDD}T{HH}Z_MSC_{MODEL}_{var}_{level}_{grid}_PT{hhh}H.grib2
//! ```
//!
//! `{product}` is the registry's (`model_gdps/15km`) and `{var}`, `{level}`
//! a selector's (`AirTemp`, `AGL-2m`). The leading date is the day the file
//! was published (UTC), not the run's: a run still publishing at midnight
//! goes on in the next day's tree, so a run is looked for in both once that
//! day has begun.
//!
//! # Listings
//!
//! Directories are Apache indexes, with the time each entry last changed to
//! the minute. A step's directory appears minutes before its GRIB2 files
//! (it starts with a small JSON of highs and lows) and fills over several
//! minutes, so what exists is read from the steps' own indexes, not from
//! which step directories there are. Listing a run reads the run's index,
//! then the index of each step whose time changed since that step was last
//! read; an index is read again also when it was read less than
//! [`SETTLED`] after its time, since a change later in that same minute
//! would not show. Step indexes are asked for with Apache's `P` pattern,
//! narrowed to this model's files at the levels the registry reads, which
//! cuts each to about a fifth (a server that ignores the pattern sends the
//! whole index, which is then filtered here).
//!
//! # Decoding
//!
//! A file holds one message and its name says which field. The decoder
//! takes it only if its times are what the selector asks for: the run's
//! reference time, and an instant at the step ([`Stat::Instant`]), an
//! accumulation from the run start ([`Stat::RunTotal`]) or over the hour
//! before the step ([`Stat::StepPerHour`], the `-Accum1h` files), or a
//! maximum ([`Stat::StepMax`]).

use crate::driver::{Cx, Decoder, Fetched, Listing, ModelFile, Need, Payload, grib_picking};
use crate::lock;
use crate::remote::FetchError;
use crate::remote::list::parse_html_index;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wx_core::Family;
use wx_core::layer::Stat;
use wx_pipeline::{MessageInfo, Timing};

/// The Datamart. Its mirror `https://hpfx.collab.science.gc.ca` has the same
/// layout.
pub const HOST: &str = "https://dd.weather.gc.ca";
/// Bodies larger than this are refused. The largest field read here is
/// about 4 MB.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Step indexes are listed again until they were read at least this long
/// after the time they show, which is to the minute (with room for clocks
/// that disagree by a few seconds).
pub const SETTLED: Duration = Duration::minutes(2);
/// Step indexes of runs older than this are forgotten.
const KEEP_LISTINGS: Duration = Duration::days(3);
/// Index pages larger than this are refused (a full step index is about
/// 100 KB).
const MAX_INDEX_BYTES: u64 = 8 * 1024 * 1024;

/// How one model is laid out on the Datamart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The directory under `WXO-DD/`: the registry's product.
    pub product: &'static str,
    /// The model in file names: `GDPS`.
    pub model: &'static str,
    /// The grid in file names: `LatLon0.15`.
    pub grid: &'static str,
}

impl Layout {
    /// `20261106T12Z_MSC_GDPS_AirTemp_AGL-2m_LatLon0.15_PT003H.grib2`.
    pub fn file_name(&self, run: DateTime<Utc>, var: &str, level: &str, hour: u32) -> String {
        format!(
            "{}_{var}_{level}_{}_PT{hour:03}H.grib2",
            self.prefix(run),
            self.grid
        )
    }

    /// What every file of this model in `run` starts with.
    fn prefix(&self, run: DateTime<Utc>) -> String {
        format!("{}_MSC_{}", run.format("%Y%m%dT%HZ"), self.model)
    }

    /// The directory of `run` in the tree of date `tree`, with its slash.
    pub fn run_dir(&self, host: &str, tree: NaiveDate, run: DateTime<Utc>) -> String {
        format!(
            "{host}/{}/WXO-DD/{}/{}/",
            tree.format("%Y%m%d"),
            self.product,
            run.format("%H")
        )
    }
}

/// The date trees that can hold `run` at `now`: its own day, and the next
/// once that has begun.
pub fn trees(run: DateTime<Utc>, now: DateTime<Utc>) -> Vec<NaiveDate> {
    let day = run.date_naive();
    let mut out = vec![day];
    if let Some(next) = day.succ_opt()
        && now.date_naive() >= next
    {
        out.push(next);
    }
    out
}

/// An Apache `P` pattern that every file of `prefix` at one of `levels`
/// matches (others may too): `{prefix}_*_` and a character class for each of
/// the first three characters of the levels, then `*`. `None` when the
/// levels hold characters a pattern would read as syntax, or there are none.
pub fn index_pattern(prefix: &str, levels: &[&str]) -> Option<String> {
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    };
    if levels.is_empty() || !levels.iter().all(|l| plain(l)) || !plain(&prefix.replace('_', "")) {
        return None;
    }
    let depth = levels.iter().map(|l| l.len()).min().unwrap_or(0).min(3);
    let mut classes = String::new();
    for k in 0..depth {
        let mut chars: Vec<char> = levels.iter().filter_map(|l| l.chars().nth(k)).collect();
        chars.sort_unstable();
        chars.dedup();
        classes.push('[');
        classes.extend(chars);
        classes.push(']');
    }
    Some(format!("{prefix}_*_{classes}*"))
}

/// The levels the registry reads from `family`'s files.
pub fn registry_levels(family: Family) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (_, input) in wx_core::layer::layers_for(family) {
        for f in input.fields {
            if !out.contains(&f.level) {
                out.push(f.level);
            }
        }
    }
    out
}

/// The query for `pattern`, with the brackets escaped.
fn pattern_query(pattern: &str) -> String {
    format!("?P={}", pattern.replace('[', "%5B").replace(']', "%5D"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StepKey {
    run: DateTime<Utc>,
    tree: NaiveDate,
    hour: u32,
}

#[derive(Debug, Clone)]
struct StepIndex {
    /// The step directory's time in the run's index.
    modified: Option<DateTime<Utc>>,
    listed_at: DateTime<Utc>,
    /// This model's GRIB2 files in it, with their sizes as the index shows
    /// them (rounded).
    files: Vec<(String, Option<u64>)>,
}

impl StepIndex {
    /// Whether this reading still holds for a directory now shown with
    /// time `modified`.
    fn holds(&self, modified: Option<DateTime<Utc>>) -> bool {
        match (self.modified, modified) {
            (Some(was), Some(now)) => was == now && self.listed_at >= was + SETTLED,
            _ => false,
        }
    }
}

/// One model on the Datamart: names, listings and fetches. Remembers the
/// step indexes it has read, so a run is listed by reading only what
/// changed.
pub struct Datamart {
    host: String,
    layout: Layout,
    levels: Vec<&'static str>,
    steps: Mutex<HashMap<StepKey, StepIndex>>,
}

impl Datamart {
    /// `levels`: what the step indexes are narrowed to (see
    /// [`index_pattern`]).
    pub fn new(host: &str, layout: Layout, levels: Vec<&'static str>) -> Self {
        Self {
            host: host.trim_end_matches('/').to_string(),
            layout,
            levels,
            steps: Mutex::default(),
        }
    }

    /// The file holding `need` in `run`.
    pub fn file(&self, run: DateTime<Utc>, need: &Need) -> String {
        let (var, level) = need.field.map_or(("", ""), |f| (f.var, f.level));
        self.layout.file_name(run, var, level, need.lead / 60)
    }

    /// The step index URL that is asked for: narrowed by a pattern when the
    /// levels allow one.
    pub fn step_index_url(&self, tree: NaiveDate, run: DateTime<Utc>, hour: u32) -> String {
        let dir = format!("{}{hour:03}/", self.layout.run_dir(&self.host, tree, run));
        match index_pattern(&self.layout.prefix(run), &self.levels) {
            Some(p) => format!("{dir}{}", pattern_query(&p)),
            None => dir,
        }
    }

    /// The files of `run` at steps `hours` that exist now, in every date
    /// tree that can hold them.
    pub async fn list(
        &self,
        cx: &Cx<'_>,
        run: DateTime<Utc>,
        hours: &[u32],
    ) -> Result<Listing, FetchError> {
        let mut listing = Listing::new();
        let prefix = self.layout.prefix(run);
        for tree in trees(run, cx.now) {
            let run_dir = self.layout.run_dir(&self.host, tree, run);
            let Some(entries) = cx.list_html(&run_dir).await? else {
                continue;
            };
            for e in entries.iter().filter(|e| e.is_dir) {
                let Some(hour) = e.name.parse::<u32>().ok().filter(|h| hours.contains(h)) else {
                    continue;
                };
                let key = StepKey { run, tree, hour };
                let known = lock(&self.steps)
                    .get(&key)
                    .filter(|s| s.holds(e.modified))
                    .cloned();
                let index = match known {
                    Some(s) => s,
                    None => {
                        let url = self.step_index_url(tree, run, hour);
                        let dir = format!("{run_dir}{hour:03}/");
                        let body = cx
                            .get(&url)
                            .listing()
                            .max_bytes(MAX_INDEX_BYTES)
                            .bytes()
                            .await?;
                        let suffix = format!("_PT{hour:03}H.grib2");
                        let files = body
                            .map(|b| parse_html_index(&String::from_utf8_lossy(&b), &dir))
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|f| {
                                !f.is_dir
                                    && f.name.starts_with(&format!("{prefix}_"))
                                    && f.name.ends_with(&suffix)
                            })
                            .map(|f| (f.name, f.size))
                            .collect();
                        let s = StepIndex {
                            modified: e.modified,
                            listed_at: cx.now,
                            files,
                        };
                        lock(&self.steps).insert(key, s.clone());
                        s
                    }
                };
                for (name, size) in index.files {
                    listing.add(name, size);
                }
            }
        }
        let oldest = cx.now - KEEP_LISTINGS;
        lock(&self.steps).retain(|k, _| k.run >= oldest);
        Ok(listing)
    }

    /// Where `name` of step `hour` was listed; the run's own day when it was
    /// not.
    pub fn url(&self, run: DateTime<Utc>, hour: u32, name: &str) -> String {
        let tree = lock(&self.steps)
            .iter()
            .filter(|(k, s)| {
                k.run == run && k.hour == hour && s.files.iter().any(|(n, _)| n == name)
            })
            .map(|(k, _)| k.tree)
            .min()
            .unwrap_or_else(|| run.date_naive());
        format!(
            "{}{hour:03}/{name}",
            self.layout.run_dir(&self.host, tree, run)
        )
    }

    /// The whole file, as the payload of every need (all of one field),
    /// decoded with `decode`.
    pub async fn fetch(
        &self,
        cx: &Cx<'_>,
        run: DateTime<Utc>,
        file: &ModelFile,
        needs: &[Need],
        decode: Decoder,
    ) -> Result<Fetched, FetchError> {
        let hour = needs.first().map_or(0, |n| n.lead / 60);
        let url = self.url(run, hour, &file.name);
        cx.whole(&url, needs, MAX_FILE_BYTES, Payload::decoded_by(decode))
            .await
    }
}

/// Whether `info` is what `need` asks for in `run` (see "Decoding" in the
/// module docs).
pub fn holds(info: &MessageInfo, need: &Need, run: DateTime<Utc>) -> bool {
    let Some(time) = info.time else {
        return false;
    };
    let valid = run + Duration::minutes(i64::from(need.lead));
    if info.reference_time != run || time.valid() != valid {
        return false;
    }
    let Some(sel) = need.field else {
        return true;
    };
    match (sel.stat, time) {
        (Stat::Instant, Timing::Instant(_)) => true,
        (Stat::RunTotal, Timing::Interval { start, process, .. }) => {
            start == run && process == Some(1)
        }
        (
            Stat::StepPerHour,
            Timing::Interval {
                start,
                end,
                process,
            },
        ) => end - start == Duration::hours(1) && process == Some(1),
        (Stat::StepMax, Timing::Interval { process, .. }) => process == Some(2),
        _ => false,
    }
}

/// The decoder for one run's files.
pub fn decoder(run: DateTime<Utc>) -> Decoder {
    grib_picking(move |info, need| holds(info, need, run))
}

/// The same decoder, with `then` applied to each field it decodes.
pub fn decoder_then(
    run: DateTime<Utc>,
    then: impl Fn(&mut wx_core::Field) + Send + Sync + 'static,
) -> Decoder {
    let inner = decoder(run);
    Arc::new(move |bytes: &[u8], need: &Need| {
        let mut d = inner(bytes, need)?;
        then(&mut d.field);
        Ok(d)
    })
}

#[cfg(test)]
mod live;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
