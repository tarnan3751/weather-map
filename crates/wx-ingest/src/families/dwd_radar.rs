//! DWD radar composites for Germany, from `opendata.dwd.de` (nginx
//! directory indexes, exactly two days kept, files named by their nominal
//! time):
//!
//! * `dwd_radar` ([`DwdRadar`], observed): `composite/hx/composite_hx_{YYYYMMDD}_{HHMM}-hd5`
//!   (no `.h5` extension), the 250 m column maximum, every 5 minutes, about
//!   1.5 minutes late, 0.2 to 1 MB.
//! * `dwd_nowcast` ([`DwdNowcast`], a model whose runs start every 5
//!   minutes): `composite/wn/composite_wn_{YYYYMMDD}_{HHMM}.tar`, an
//!   uncompressed tar of 25 ODIM files `composite_wn_{YYYYMMDD}_{HHMM}_{NNN}-hd5`,
//!   `NNN` the lead in minutes, 000 to 120; about 3 minutes late, 1 to 3 MB.
//!   One download feeds the 24 forecast parts; member 000, the analysis, is
//!   not used, since `hx` covers that time at four times the resolution.
//!
//! The `LATEST` aliases (`composite_hx_LATEST-hd5`, and
//! `composite_wn__LATEST.tar` with its double underscore) are not read:
//! they are overwritten under the reader, and the dated names say what they
//! hold.
//!
//! Finding new files: a directory index is 80 kB, too much to read every
//! 20 seconds while a file is due, so the adapter asks for the first byte
//! of the next file it expects (a 404 until it is written), and reads the
//! observed index only when more than a few times are unaccounted for (the
//! first cycle, after an outage). A nowcast tar is looked for the same way,
//! once a minute from its run's start until it is there, and only for runs
//! of the last [`OLDEST_RUN`].
//!
//! A new nowcast run is followed every [`reg::NOWCAST_FOLLOW_MINUTES`]
//! rather than every 5: each run's 24 parts replace the previous run's.

use crate::clock;
use crate::driver::{
    Cadence, Cx, Cycle, Decoded, Decoder, Fetched, Listing, Model, ModelFile, Need, ObsFile,
    Observed, Payload,
};
use crate::families::common;
use crate::idx::ByteRange;
use crate::lock;
use crate::remote::FetchError;
use anyhow::{Context, ensure};
use bytes::Bytes;
use chrono::{DateTime, Duration, NaiveDateTime, TimeDelta, Utc};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use wx_core::SourceDef;
use wx_core::families::dwd_radar as reg;
use wx_formats::Quantity;

pub const BASE: &str = "https://opendata.dwd.de/weather/radar/composite";
/// A 250 m composite is at most a few megabytes, a nowcast tar about 3.
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// More times than this unaccounted for, and the directory index is read
/// instead of asking for each.
const PROBE_AT_MOST: usize = 3;
/// A file is not asked for until this long after its time: none comes
/// sooner.
const FIRST_LOOK: Duration = Duration::seconds(60);
/// Members of one nowcast tar, at most.
const MAX_MEMBERS: usize = 100;
/// A run's tar appears about this long after the run starts (20:23:02 and
/// 20:28:07 for the 20:20 and 20:25 runs on 2026-11-06).
const DUE_AFTER: Duration = Duration::seconds(200);
/// Members of a whole nowcast tar: the analysis and every step.
pub const STEPS: usize = (reg::NOWCAST_HORIZON_MINUTES / reg::NOWCAST_STEP_MINUTES + 1) as usize;
/// A run's tar not whole this long after the run started is taken as it
/// is (they are written about 3 minutes after it).
pub const WHOLE_BY: Duration = Duration::minutes(15);
/// No observed file newer than this, or no nowcast run newer than this
/// found, is a source that has stopped or moved: a failing task, not a
/// quiet one.
pub const STALE: Duration = Duration::minutes(30);
/// A run's tar not there this long after the run started, with no newer
/// one found, is the nowcast having stopped.
pub const NO_RUN_FOR: Duration = Duration::minutes(15);
/// Nowcast runs older than this are not fetched: their early leads are
/// already observed and their later ones covered by newer runs. Without
/// it, a start with an empty catalog (no observation yet retiring old
/// leads) would fetch every run of the last 90 minutes.
pub const OLDEST_RUN: Duration = Duration::minutes(20);

fn step() -> Duration {
    Duration::minutes(reg::EVERY_MINUTES as i64)
}

/// `{base}/{product}/`.
pub fn dir_url(base: &str, product: &str) -> String {
    format!("{base}/{product}/")
}

/// The observed file valid at `t`.
pub fn obs_name(product: &str, t: DateTime<Utc>) -> String {
    format!("composite_{product}_{}-hd5", t.format("%Y%m%d_%H%M"))
}

/// Valid time of an observed file from its name; `None` for the `LATEST`
/// alias and anything else.
pub fn parse_obs_name(product: &str, name: &str) -> Option<DateTime<Utc>> {
    let stamp = name
        .strip_prefix("composite_")?
        .strip_prefix(product)?
        .strip_prefix('_')?
        .strip_suffix("-hd5")?;
    NaiveDateTime::parse_from_str(stamp, "%Y%m%d_%H%M")
        .ok()
        .map(|t| t.and_utc())
}

/// The nowcast tar of the run starting at `run`.
pub fn tar_name(product: &str, run: DateTime<Utc>) -> String {
    format!("composite_{product}_{}.tar", run.format("%Y%m%d_%H%M"))
}

/// The lead in minutes of a tar member of `run`, from its name
/// `composite_wn_{YYYYMMDD}_{HHMM}_{NNN}-hd5`; `None` for a member of
/// another run or anything else.
pub fn member_lead(product: &str, run: DateTime<Utc>, name: &str) -> Option<u32> {
    let base = name.rsplit('/').next()?;
    let rest = base
        .strip_prefix("composite_")?
        .strip_prefix(product)?
        .strip_prefix('_')?
        .strip_suffix("-hd5")?;
    let (stamp, lead) = rest.rsplit_once('_')?;
    if stamp != run.format("%Y%m%d_%H%M").to_string() || lead.len() != 3 {
        return None;
    }
    lead.parse().ok()
}

/// The regular files of a tar, and whether it was whole.
#[derive(Debug, Clone)]
pub struct Tar {
    pub members: Vec<(String, Bytes)>,
    /// It ended with an end-of-archive block. A tar cut off between two
    /// members (one being written, or a short download) does not.
    pub complete: bool,
}

/// The regular files of an uncompressed tar (POSIX ustar or GNU), as slices
/// of it. Header checksums are checked, so bytes that are not a tar are an
/// error rather than garbage members.
pub fn tar_members(tar: &Bytes) -> anyhow::Result<Tar> {
    const BLOCK: usize = 512;
    let field = |h: &[u8], at: usize, len: usize| -> String {
        let f = &h[at..at + len];
        let end = f.iter().position(|&b| b == 0).unwrap_or(len);
        String::from_utf8_lossy(&f[..end]).into_owned()
    };
    let octal = |h: &[u8], at: usize, len: usize| -> anyhow::Result<u64> {
        let s = field(h, at, len);
        let s = s.trim_matches(|c: char| c == ' ' || c == '\0');
        if s.is_empty() {
            return Ok(0);
        }
        u64::from_str_radix(s, 8).with_context(|| format!("tar header number {s:?}"))
    };
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut long_name: Option<String> = None;
    let complete = loop {
        let Some(h) = tar.get(at..at + BLOCK) else {
            // An archive cut off without its end-of-archive blocks: what came
            // before is whole, as each member's length was checked.
            ensure!(
                !out.is_empty() || tar.is_empty(),
                "no tar header at byte {at}"
            );
            break false;
        };
        if h.iter().all(|&b| b == 0) {
            break true;
        }
        let stored = octal(h, 148, 8)?;
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(k, &b)| {
                if (148..156).contains(&k) {
                    32
                } else {
                    b as u64
                }
            })
            .sum();
        ensure!(
            stored == sum,
            "tar header at byte {at}: checksum {stored} for {sum}"
        );
        let size = usize::try_from(octal(h, 124, 12)?).context("tar member size")?;
        let data = at + BLOCK;
        let end = data
            .checked_add(size)
            .filter(|&e| e <= tar.len())
            .with_context(|| {
                format!("tar member at byte {at} runs past the end of the archive ({size} bytes)")
            })?;
        match h[156] {
            b'0' | 0 | b'7' => {
                let mut name = long_name.take().unwrap_or_else(|| field(h, 0, 100));
                if &h[257..262] == b"ustar" && h[345] != 0 {
                    name = format!("{}/{name}", field(h, 345, 155));
                }
                ensure!(
                    out.len() < MAX_MEMBERS,
                    "more than {MAX_MEMBERS} members in the tar"
                );
                out.push((name, tar.slice(data..end)));
            }
            b'L' => long_name = Some(field(&tar[data..end], 0, size)),
            _ => {}
        }
        at = data + size.div_ceil(BLOCK) * BLOCK;
    };
    Ok(Tar {
        members: out,
        complete,
    })
}

/// Decodes a DWD composite of `product`'s quantity valid at `valid` (an
/// observation when `run` is `None`, otherwise the step `need.lead` minutes
/// into the nowcast from `run`).
pub fn decoder(run: Option<DateTime<Utc>>) -> Decoder {
    Arc::new(move |bytes: &[u8], need: &Need| {
        let d = wx_formats::dwd::decode(bytes)?;
        ensure!(
            d.quantity == Quantity::Reflectivity,
            "a DWD {} file holding {:?}",
            need.product(),
            d.quantity
        );
        match run {
            None => ensure!(
                d.run.is_none(),
                "an observed DWD {} file flagged as a forecast",
                need.product()
            ),
            Some(run) => {
                let starts = d.run.unwrap_or(d.valid);
                ensure!(
                    starts == run,
                    "a step of the nowcast from {starts} in the tar of the run from {run}"
                );
                ensure!(
                    d.lead == TimeDelta::minutes(need.lead as i64),
                    "the member for lead {} holds lead {}",
                    need.lead,
                    d.lead.num_minutes()
                );
            }
        }
        Ok(Decoded {
            field: d.field,
            valid: Some(d.valid),
        })
    })
}

/// The observed composites.
pub struct DwdRadar {
    base: String,
    /// Files known to exist (found by a probe or in an index), by valid
    /// time, so each is looked for once.
    seen: Mutex<BTreeMap<DateTime<Utc>, ObsFile>>,
}

impl Default for DwdRadar {
    fn default() -> Self {
        Self::at(BASE)
    }
}

impl DwdRadar {
    /// An adapter reading from another server (tests).
    pub fn at(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            seen: Mutex::default(),
        }
    }
}

/// Every time in `[from, to]` on the product's 5-minute steps.
fn slots(from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let mut t = clock::floor(from, step());
    if t < from {
        t += step();
    }
    let mut out = Vec::new();
    while t <= to && out.len() < 2000 {
        out.push(t);
        t += step();
    }
    out
}

#[async_trait::async_trait]
impl Observed for DwdRadar {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        common::one_region(source, "DWD's composite")
    }

    fn cadence(&self, _: &SourceDef, _: &str) -> Cadence {
        // 20:21:34 and 20:26:29 for 20:20 and 20:25 on 2026-11-06.
        Cadence::every(step(), Duration::seconds(100), Duration::seconds(20))
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        product: &str,
        from: DateTime<Utc>,
    ) -> Result<Vec<ObsFile>, FetchError> {
        let wanted = slots(from, cx.now - FIRST_LOOK);
        let unknown: Vec<DateTime<Utc>> = {
            let mut seen = lock(&self.seen);
            seen.retain(|t, _| *t + step() >= from);
            wanted
                .iter()
                .copied()
                .filter(|t| !seen.contains_key(t))
                .collect()
        };
        let dir = dir_url(&self.base, product);
        if unknown.len() > PROBE_AT_MOST {
            let Some(index) = cx.list_html(&dir).await? else {
                return Err(FetchError::service(&dir, "the directory is gone (404)"));
            };
            let mut seen = lock(&self.seen);
            for e in index {
                if let Some(t) = parse_obs_name(product, &e.name).filter(|t| *t + step() >= from) {
                    seen.insert(
                        t,
                        ObsFile {
                            valid: t,
                            name: e.url,
                            size: e.size,
                        },
                    );
                }
            }
        } else {
            for t in unknown {
                let url = format!("{dir}{}", obs_name(product, t));
                if cx
                    .get(&url)
                    .range(ByteRange::new(0, 1))
                    .send()
                    .await?
                    .is_some()
                {
                    lock(&self.seen).insert(
                        t,
                        ObsFile {
                            valid: t,
                            name: url,
                            size: None,
                        },
                    );
                }
            }
        }
        let seen = lock(&self.seen);
        let newest = seen.keys().next_back().copied();
        if newest.is_none_or(|t| t < cx.now - STALE) {
            return Err(FetchError::service(
                &dir,
                format!(
                    "no file since {}: the composite has stopped or moved",
                    newest.unwrap_or(from)
                ),
            ));
        }
        Ok(seen.range(from..).map(|(_, f)| f.clone()).collect())
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let fetched = cx
            .whole(
                &file.name,
                needs,
                MAX_FILE,
                Payload::decoded_by(decoder(None)),
            )
            .await?;
        if matches!(fetched, Fetched::Missing) {
            lock(&self.seen).remove(&file.valid);
        }
        Ok(fetched)
    }
}

/// The nowcast.
pub struct DwdNowcast {
    base: String,
    /// Runs followed: those starting on multiples of this.
    follow: Duration,
    /// The newest run whose tar was found.
    newest: Mutex<Option<DateTime<Utc>>>,
}

impl Default for DwdNowcast {
    fn default() -> Self {
        Self::at(BASE)
    }
}

impl DwdNowcast {
    /// An adapter reading from another server (tests).
    pub fn at(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            follow: Duration::minutes(reg::NOWCAST_FOLLOW_MINUTES as i64),
            newest: Mutex::default(),
        }
    }

    /// Follow the runs starting on multiples of `minutes` instead, down to
    /// every run (5).
    #[cfg(test)]
    pub fn following_every(self, minutes: u32) -> Self {
        let step = reg::NOWCAST_STEP_MINUTES.max(1);
        Self {
            follow: Duration::minutes(minutes.max(step).div_ceil(step) as i64 * step as i64),
            ..self
        }
    }
}

#[async_trait::async_trait]
impl Model for DwdNowcast {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        common::one_region(source, "DWD's composite")
    }

    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: self.follow,
            spare: 1,
            // A tar not there yet is looked for again this often.
            relist: Duration::minutes(1),
            give_up: Duration::minutes(20),
        }
    }

    fn leads(&self, _: &SourceDef, _: &str, _: DateTime<Utc>) -> Vec<u32> {
        let step = reg::NOWCAST_STEP_MINUTES;
        (1..=reg::NOWCAST_HORIZON_MINUTES / step)
            .map(|k| k * step)
            .collect()
    }

    fn due(&self, _: &SourceDef, _: &str, run: DateTime<Utc>, _: u32) -> DateTime<Utc> {
        run + DUE_AFTER
    }

    fn file(&self, _: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        format!(
            "{}{}",
            dir_url(&self.base, need.product()),
            tar_name(need.product(), run)
        )
    }

    /// The run's tar, if the first byte of it can be had. Runs older than
    /// [`OLDEST_RUN`] are not looked for.
    async fn list(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        run: DateTime<Utc>,
        products: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let mut listing = Listing::new();
        if run < cx.now - OLDEST_RUN {
            return Ok(Some(listing));
        }
        let mut newest = lock(&self.newest).filter(|r| *r >= cx.now - STALE);
        for product in products {
            let url = format!("{}{}", dir_url(&self.base, product), tar_name(product, run));
            if cx
                .get(&url)
                .range(ByteRange::new(0, 1))
                .send()
                .await?
                .is_some()
            {
                listing.add(url, None);
                newest = Some(newest.map_or(run, |n| n.max(run)));
            } else if run < cx.now - NO_RUN_FOR && newest.is_none_or(|n| n < run) {
                // One run can be skipped: the nowcast has stopped only if the
                // newest run that should be out is not there either.
                let latest = clock::floor(cx.now - DUE_AFTER, self.follow);
                let latest_url = format!(
                    "{}{}",
                    dir_url(&self.base, product),
                    tar_name(product, latest)
                );
                if latest > run
                    && cx
                        .get(&latest_url)
                        .range(ByteRange::new(0, 1))
                        .send()
                        .await?
                        .is_some()
                {
                    newest = Some(latest);
                } else {
                    return Err(FetchError::service(
                        url,
                        "no nowcast run this recent: the nowcast has stopped or moved",
                    ));
                }
            }
        }
        *lock(&self.newest) = newest;
        Ok(Some(listing))
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        run: DateTime<Utc>,
        file: &ModelFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let Some(tar) = cx.get(&file.name).max_bytes(MAX_FILE).bytes().await? else {
            return Ok(Fetched::Missing);
        };
        let Tar { members, complete } = cx
            .cpu(move || tar_members(&tar))
            .await
            .and_then(|r| r)
            .map_err(|e| FetchError::object(&file.name, format!("{e:#}")))?;
        // A tar being written, or cut short, would give up the leads it
        // lacks for good: it is looked for again until it is whole, or until
        // it has had long enough to be, and then taken as it is.
        let whole = complete && members.len() >= STEPS;
        if !whole {
            if cx.now < run + WHOLE_BY {
                tracing::info!(file = %file.name, members = members.len(), complete, "nowcast tar not whole yet");
                return Ok(Fetched::Missing);
            }
            tracing::warn!(file = %file.name, members = members.len(), complete, "nowcast tar still not whole; taking the steps it has");
        }
        let decode = decoder(Some(run));
        let payloads = needs
            .iter()
            .map(|n| {
                members
                    .iter()
                    .find(|(name, _)| member_lead(n.product(), run, name) == Some(n.lead))
                    .map(|(_, bytes)| Payload::Custom {
                        bytes: bytes.clone(),
                        decode: decode.clone(),
                    })
            })
            .collect::<Vec<_>>();
        if payloads.iter().all(Option::is_none) && !members.is_empty() {
            let names: Vec<&str> = members.iter().take(3).map(|(n, _)| n.as_str()).collect();
            return Err(FetchError::object(
                &file.name,
                format!("no member is a step of this run: {names:?}"),
            ));
        }
        Ok(Fetched::Got(payloads))
    }
}

#[cfg(test)]
mod tests;
