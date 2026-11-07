//! CMORPH2: NOAA CPC's global satellite rainfall estimate, one 0.25 degree
//! file per half hour, the base of the radar layer where no radar looks and
//! the worldwide source of the rainfall totals. Layout, timing and licence
//! are in the registry module `wx_core::families::cmorph`.
//!
//! # Products
//!
//! * [`reg::RATE`]: each half-hour file is a part on the radar layer (its
//!   mean rate, which the layer turns into reflectivity), valid at the start
//!   of its half hour, so the part stands for the half hour it measured and
//!   then, through its hold, for the time until the next one arrives. (Valid
//!   at the end, a frame would show the half hour before the one it is in.)
//! * [`reg::TOTAL_1H`], [`reg::TOTAL_24H`], [`reg::TOTAL_72H`]: the rain
//!   that fell in the 2, 48 or 144 half hours ending at the valid time,
//!   `sum of rate x 0.5 h`, one part per half hour, valid at the end of its
//!   newest half hour. The adapter sums them itself and hands the engine the
//!   summed field, in mm.
//!
//! # The half-hour cache
//!
//! All four products are made from the same files, so the adapter (one per
//! source, shared by its four tasks) keeps every half hour the oldest wanted
//! total reaches back to (about 79 hours), decoded, in memory: as counts of 0.01 mm/h in a sparse form (a bit
//! per cell for "wet", a byte per wet cell, two for the few above 2.54
//! mm/h), about 0.4 MB a half hour and 60 MB for the lot instead of 4 MB
//! each as floats. A total is then a sum of integers, exact and a few tens
//! of milliseconds for 144 half hours. Each half hour is downloaded once per
//! version, by whichever task needs it first, one download at a time for
//! the whole family, so a cold start (144 files, about 85 MB) trickles in
//! one file at a time from the newest back and never holds more than one of
//! the host's two connections. Each total is published as soon as its half
//! hours are in: the radar part and the hourly total first, the 72-hour
//! total last. The cache lives as long as the process; a restart fetches it
//! again.
//!
//! # Missing half hours
//!
//! A total is made when at least 90 percent of its half hours are there
//! ([`min_present`]: 2 of 2, 44 of 48, 130 of 144), and not otherwise. A
//! cell is the sum over the half hours that have a value there, scaled up
//! to the full period; it is missing where fewer than 90 percent do (the
//! files leave the northernmost row empty). A half hour the listing does not
//! have, or whose file will not decode, counts as missing. A total that
//! falls short is not given up: it is tried again whenever it is listed
//! (each cycle while it is among the newest), so half hours CPC fills in
//! late still make it. The newest half hour of a total is waited for: it is
//! the one the total is named after.
//!
//! # Rewrites
//!
//! CPC rewrites every file each half hour for about 12 hours as more
//! microwave passes come in (the listing's times jump together). The day
//! listings are re-read at most once a minute while their files can still
//! change, and a half hour whose listed time is newer than the version held
//! is fetched again (conditionally, so an unchanged file costs a 304) before
//! it goes into a sum. So every new total is summed from the freshest
//! version of each of its half hours. Parts already on the catalog are not
//! replaced: the catalog keeps the first part it gets for a valid time, so a
//! half-hourly radar part and a total stay as first made, and the next
//! total, half an hour later, carries the update. Fetching every rewrite
//! costs about 24 files (14 MB) per half hour.
//!
//! # Listings
//!
//! The host is not S3 but an Apache index per UTC day, named after the
//! half-hour starts. Directories for future days exist and are empty; the
//! adapter reads only the days a window touches, up to today's. An index
//! that lists no file where some are due (a 404, an error page, a renamed
//! product) is an error the task retries, never an empty day; and only a
//! complete past day (48 files, no longer rewritten) is kept for hours.
//! Anything else is read again a minute later, so one bad answer cannot
//! hold back a total for longer than that.

use crate::clock;
use crate::driver::{Cadence, Cx, Decoded, Decoder, Fetched, Need, ObsFile, Observed, Payload};
use crate::families::common;
use crate::lock;
use crate::remote::{FetchError, Validators};
use anyhow::ensure;
use bytes::Bytes;
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use wx_core::families::cmorph as reg;
use wx_core::{Field, GridProjection, SourceDef};

/// The directory holding `YYYY/YYYYMM/YYYYMMDD/`.
pub const BASE: &str = "https://ftp.cpc.ncep.noaa.gov/precip/CMORPH2/CMORPH2NRT/DATA/";
/// Files are about 0.6 MB.
const MAX_FILE: u64 = 8 * 1024 * 1024;
/// A day listing that may still change is read again after this long; the
/// four tasks look at about the same moments, so they share one read.
const LISTING_REUSE: Duration = Duration::seconds(50);
/// Files this old are no longer rewritten (12 hours and a margin).
const SETTLED: Duration = Duration::hours(13);
/// A complete day whose files have all settled is read again after this
/// long.
const SETTLED_REUSE: Duration = Duration::hours(6);
/// A half hour's file is certainly due this long after the half hour
/// starts: an index without it is wrong, not early.
const SURELY_DUE: Duration = Duration::hours(2);
/// The share of a total's half hours that must be there, in tenths.
const MIN_PRESENT_TENTHS: usize = 9;

fn half() -> Duration {
    Duration::minutes(reg::EVERY_MINUTES as i64)
}

const HALVES_A_DAY: usize = 48;

/// Half hours a total needs at least: 90 percent of `n`, rounded up.
pub fn min_present(n: usize) -> usize {
    (n * MIN_PRESENT_TENTHS).div_ceil(10)
}

/// Half hours summed for `product`, `None` for the rate.
pub fn window(product: &str) -> Option<usize> {
    reg::TOTAL_FILES
        .iter()
        .find(|(p, _)| *p == product)
        .map(|(_, n)| *n as usize)
}

/// The valid time of `product`'s part made from (or ending with) the half
/// hour starting at `start`: the start for the rate, the end for a total.
pub fn valid_of(product: &str, start: DateTime<Utc>) -> DateTime<Utc> {
    match window(product) {
        None => start,
        Some(_) => start + half(),
    }
}

pub fn day_url(base: &str, day: NaiveDate) -> String {
    format!("{base}{}/", day.format("%Y/%Y%m/%Y%m%d"))
}

#[cfg(test)]
pub fn file_name(start: DateTime<Utc>) -> String {
    format!("CMORPH2_0.25deg-30min_{}.RT.nc", start.format("%Y%m%d%H%M"))
}

#[cfg(test)]
pub fn file_url(base: &str, start: DateTime<Utc>) -> String {
    format!("{}{}", day_url(base, start.date_naive()), file_name(start))
}

/// Start of the half hour a file covers, from its name.
pub fn parse_name(name: &str) -> Option<DateTime<Utc>> {
    let stamp = name
        .strip_prefix("CMORPH2_0.25deg-30min_")?
        .strip_suffix(".RT.nc")?;
    if stamp.len() != 12 {
        return None;
    }
    let t = NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M")
        .ok()?
        .and_utc();
    (t == clock::floor(t, half())).then_some(t)
}

/// One half hour's mean rates as counts of 0.01 mm/h (the files' own
/// packing), kept compactly: about three quarters of the cells are dry and
/// most wet ones hold less than 2.55 mm/h.
#[derive(Debug, Clone, PartialEq)]
pub struct Rates {
    nx: usize,
    ny: usize,
    proj: GridProjection,
    /// One bit per cell: a count above zero.
    wet: Vec<u64>,
    /// The count of each wet cell in cell order; 255 means it is in `large`.
    small: Vec<u8>,
    /// Counts of 255 and more, in cell order.
    large: Vec<u16>,
    /// Cells without a value.
    missing: Vec<u32>,
}

impl Rates {
    /// From a decoded field in mm/h. Every value must be a whole number of
    /// hundredths, as the files store them.
    pub fn from_field(f: &Field) -> anyhow::Result<Rates> {
        ensure!(
            f.values.len() == f.nx * f.ny && f.values.len() < u32::MAX as usize,
            "a {} x {} field",
            f.nx,
            f.ny
        );
        let mut r = Rates {
            nx: f.nx,
            ny: f.ny,
            proj: f.proj.clone(),
            wet: vec![0; f.values.len().div_ceil(64)],
            small: Vec::new(),
            large: Vec::new(),
            missing: Vec::new(),
        };
        for (k, &v) in f.values.iter().enumerate() {
            if v.is_nan() {
                r.missing.push(k as u32);
                continue;
            }
            let c = (f64::from(v) * 100.0).round();
            ensure!(
                (0.0..=f64::from(u16::MAX)).contains(&c),
                "a rate of {v} mm/h"
            );
            let c = c as u16;
            if c == 0 {
                continue;
            }
            r.wet[k / 64] |= 1 << (k % 64);
            if c < 255 {
                r.small.push(c as u8);
            } else {
                r.small.push(255);
                r.large.push(c);
            }
        }
        Ok(r)
    }

    /// Every wet cell and its count, in cell order.
    fn for_each_wet(&self, mut f: impl FnMut(usize, u32)) {
        let (mut s, mut l) = (0, 0);
        for (w, &bits) in self.wet.iter().enumerate() {
            let mut bits = bits;
            while bits != 0 {
                let k = w * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let c = match self.small[s] {
                    255 => {
                        l += 1;
                        self.large[l - 1]
                    }
                    c => u16::from(c),
                };
                s += 1;
                f(k, u32::from(c));
            }
        }
    }

    /// The field it came from, value for value.
    pub fn field(&self) -> Field {
        let mut values = vec![0f32; self.nx * self.ny];
        self.for_each_wet(|k, c| values[k] = (f64::from(c) * 0.01) as f32);
        for &k in &self.missing {
            values[k as usize] = f32::NAN;
        }
        Field {
            nx: self.nx,
            ny: self.ny,
            proj: self.proj.clone(),
            values,
            grid_relative_winds: false,
        }
    }

    #[cfg(test)]
    pub fn approx_bytes(&self) -> usize {
        self.wet.len() * 8 + self.small.len() + self.large.len() * 2 + self.missing.len() * 4
    }

    fn same_grid(&self, o: &Rates) -> bool {
        self.nx == o.nx && self.ny == o.ny && self.proj == o.proj
    }
}

/// The rain over `n` half hours from those of them that are here, in mm:
/// see "Missing half hours" in the module docs. An error when fewer than
/// [`min_present`] are here or they are on different grids.
pub fn total(halves: &[&Rates], n: usize) -> anyhow::Result<Field> {
    let min = min_present(n);
    ensure!(
        halves.len() >= min && halves.len() <= n,
        "{} of {n} half hours, at least {min} needed",
        halves.len()
    );
    let first = halves[0];
    ensure!(
        halves.iter().all(|r| r.same_grid(first)),
        "the half hours are on different grids"
    );
    let cells = first.nx * first.ny;
    let mut sum = vec![0u32; cells];
    let mut absent = vec![0u8; cells];
    for r in halves {
        r.for_each_wet(|k, c| sum[k] += c);
        for &k in &r.missing {
            absent[k as usize] += 1;
        }
    }
    let values = sum
        .iter()
        .zip(&absent)
        .map(|(&s, &a)| {
            let have = halves.len() - usize::from(a);
            if have < min {
                return f32::NAN;
            }
            // Counts of 0.01 mm/h for half an hour: 200 make a millimetre.
            let mm = f64::from(s) / 200.0;
            (if have == n {
                mm
            } else {
                mm * n as f64 / have as f64
            }) as f32
        })
        .collect();
    Ok(Field {
        nx: first.nx,
        ny: first.ny,
        proj: first.proj.clone(),
        values,
        grid_relative_winds: false,
    })
}

/// One file as a day listing shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listing {
    url: String,
    modified: Option<DateTime<Utc>>,
    size: Option<u64>,
}

/// One version of one half hour.
struct HalfHour {
    rates: Arc<Rates>,
    /// The listing's time for this version, when it showed one.
    listed: Option<DateTime<Utc>>,
    validators: Validators,
}

impl HalfHour {
    /// Whether this is the version `l` lists.
    fn current(&self, l: &Listing) -> bool {
        l.modified
            .is_none_or(|m| self.listed.is_some_and(|have| have >= m))
    }
}

struct Day {
    read_at: DateTime<Utc>,
    files: Arc<BTreeMap<DateTime<Utc>, Listing>>,
}

#[derive(Default)]
struct State {
    days: BTreeMap<NaiveDate, Day>,
    /// By the start of the half hour.
    halves: BTreeMap<DateTime<Utc>, Arc<HalfHour>>,
}

/// Decodes one file into its rates in mm/h and the start of its half hour.
pub type HalfDecoder = Arc<dyn Fn(&[u8]) -> anyhow::Result<(Field, DateTime<Utc>)> + Send + Sync>;

/// [`HalfDecoder`] for the real files.
fn decode_file(bytes: &[u8]) -> anyhow::Result<(Field, DateTime<Utc>)> {
    let d = wx_formats::cmorph::decode(bytes)?;
    ensure!(
        d.period == half(),
        "the file covers {} rather than half an hour",
        d.period
    );
    Ok((d.field, d.valid - d.period))
}

pub struct Cmorph {
    base: String,
    decode: HalfDecoder,
    state: Mutex<State>,
    /// One CMORPH download at a time for the whole family.
    downloading: tokio::sync::Mutex<()>,
    /// One listing read at a time, so tasks that look at once share it.
    listing: tokio::sync::Mutex<()>,
}

impl Default for Cmorph {
    fn default() -> Self {
        Self::at(BASE)
    }
}

/// What is known about one half hour of a window.
enum Got {
    Here(Arc<HalfHour>),
    /// Not listed, gone, or broken.
    Absent,
}

impl Cmorph {
    /// Files under `base` (a URL ending in `/`, laid out as CPC's `DATA/`).
    pub fn at(base: &str) -> Self {
        Self::with_decoder(base, Arc::new(decode_file))
    }

    /// Files under `base` read by `decode` (tests use small made-up files).
    pub fn with_decoder(base: &str, decode: HalfDecoder) -> Self {
        Self {
            base: base.to_string(),
            decode,
            state: Mutex::default(),
            downloading: tokio::sync::Mutex::new(()),
            listing: tokio::sync::Mutex::new(()),
        }
    }

    /// Half hours held, and roughly how much memory they take.
    #[cfg(test)]
    pub fn cached(&self) -> (usize, usize) {
        let s = lock(&self.state);
        (
            s.halves.len(),
            s.halves.values().map(|h| h.rates.approx_bytes()).sum(),
        )
    }

    /// The files of `day`, read again when the copy held is too old: see
    /// "Listings" in the module docs. An error when the index lists none
    /// although some are surely due.
    async fn day(
        &self,
        cx: &Cx<'_>,
        day: NaiveDate,
    ) -> Result<Arc<BTreeMap<DateTime<Utc>, Listing>>, FetchError> {
        let midnight = day.and_hms_opt(0, 0, 0).map_or(cx.now, |d| d.and_utc());
        let settled = midnight + Duration::days(1) + SETTLED <= cx.now;
        let fresh = |s: &State| {
            let d = s.days.get(&day)?;
            let reuse = if settled && d.files.len() == HALVES_A_DAY {
                SETTLED_REUSE
            } else {
                LISTING_REUSE
            };
            (cx.now - d.read_at < reuse).then(|| d.files.clone())
        };
        if let Some(f) = fresh(&lock(&self.state)) {
            return Ok(f);
        }
        let _one = self.listing.lock().await;
        if let Some(f) = fresh(&lock(&self.state)) {
            return Ok(f);
        }
        let url = day_url(&self.base, day);
        let entries = cx.list_html(&url).await?;
        let found = entries.is_some();
        let entries = entries.unwrap_or_default();
        let files: BTreeMap<DateTime<Utc>, Listing> = entries
            .into_iter()
            .filter(|e| !e.is_dir)
            .filter_map(|e| {
                let start = parse_name(&e.name)?;
                // Only files in their own day's directory.
                let listing = Listing {
                    url: e.url,
                    modified: e.modified,
                    size: e.size,
                };
                (start.date_naive() == day).then_some((start, listing))
            })
            .collect();
        // Half hours of the day whose files must be out by now.
        let due = (0..HALVES_A_DAY as i32)
            .filter(|k| midnight + half() * *k + SURELY_DUE <= cx.now)
            .count();
        if files.is_empty() && due > 0 {
            let what = if found {
                "lists no CMORPH2 file"
            } else {
                "is not there (404)"
            };
            return Err(FetchError::service(
                &url,
                format!("the index {what} although {due} are due"),
            ));
        }
        if files.len() < due {
            tracing::warn!(%url, listed = files.len(), due, "a CMORPH2 day index lacks files; reading it again next time");
        }
        let files = Arc::new(files);
        let mut s = lock(&self.state);
        s.days.insert(
            day,
            Day {
                read_at: cx.now,
                files: files.clone(),
            },
        );
        // Days no window reaches any more.
        let oldest = (cx.now - keep()).date_naive();
        s.days.retain(|d, _| *d >= oldest);
        Ok(files)
    }

    /// The listing of the half hour starting at `start`.
    async fn listed(
        &self,
        cx: &Cx<'_>,
        start: DateTime<Utc>,
    ) -> Result<Option<Listing>, FetchError> {
        Ok(self.day(cx, start.date_naive()).await?.get(&start).cloned())
    }

    /// The half hour starting at `start` in the version `l` lists: from the
    /// cache, or downloaded (conditionally when an older version is held).
    /// `None` when the file is not there after all; an error when it will
    /// not decode or the host fails.
    async fn half_hour(
        &self,
        cx: &Cx<'_>,
        start: DateTime<Utc>,
        l: &Listing,
    ) -> Result<Option<Arc<HalfHour>>, FetchError> {
        let cached = |s: &State| s.halves.get(&start).cloned();
        if let Some(h) = cached(&lock(&self.state)).filter(|h| h.current(l)) {
            return Ok(Some(h));
        }
        let _one = self.downloading.lock().await;
        let held = cached(&lock(&self.state));
        if let Some(h) = held.clone().filter(|h| h.current(l)) {
            return Ok(Some(h));
        }
        let mut get = cx.get(&l.url).max_bytes(MAX_FILE);
        if let Some(h) = &held {
            get = get.if_changed(&h.validators);
        }
        let Some(reply) = get.send().await? else {
            // Listed but gone: the listing was stale. Keep what is held.
            return Ok(held);
        };
        let fresh = if reply.not_modified {
            let Some(h) = held else {
                return Err(FetchError::object(
                    &l.url,
                    "answered 304 to an unconditional request",
                ));
            };
            HalfHour {
                rates: h.rates.clone(),
                listed: l.modified.or(h.listed),
                validators: if reply.validators == Validators::default() {
                    h.validators.clone()
                } else {
                    reply.validators
                },
            }
        } else {
            let (body, decode) = (reply.body, self.decode.clone());
            let rates = cx
                .cpu(move || {
                    let (field, holds) = decode(&body)?;
                    ensure!(
                        holds == start,
                        "the file is named after {start} but holds {holds}"
                    );
                    Rates::from_field(&field)
                })
                .await
                .map_err(|e| FetchError::service(&l.url, e))?
                .map_err(|e| FetchError::object(&l.url, format!("{e:#}")))?;
            HalfHour {
                rates: Arc::new(rates),
                listed: l.modified,
                validators: reply.validators,
            }
        };
        let fresh = Arc::new(fresh);
        let mut s = lock(&self.state);
        s.halves.insert(start, fresh.clone());
        let oldest = cx.now - keep();
        s.halves.retain(|t, _| *t >= oldest);
        Ok(Some(fresh))
    }

    /// The half hours of the `n` ending at `end`, newest first, until more
    /// are absent than a total can do without. `None` when the newest is not
    /// there yet (the total waits for it).
    async fn window(
        &self,
        cx: &Cx<'_>,
        end: DateTime<Utc>,
        n: usize,
    ) -> Result<Option<Vec<Got>>, FetchError> {
        let spare = n - min_present(n);
        let mut out = Vec::with_capacity(n);
        let mut absent = 0;
        for k in 1..=n {
            let start = end - half() * k as i32;
            let got = match self.listed(cx, start).await? {
                None => Got::Absent,
                Some(l) => match self.half_hour(cx, start, &l).await {
                    Ok(Some(h)) => Got::Here(h),
                    Ok(None) => Got::Absent,
                    Err(e) if e.failure == crate::remote::Failure::Object && k > 1 => {
                        tracing::warn!(url = %l.url, "a half hour of a total counts as missing: {e}");
                        Got::Absent
                    }
                    Err(e) => return Err(e),
                },
            };
            if k == 1 && matches!(got, Got::Absent) {
                return Ok(None);
            }
            absent += usize::from(matches!(got, Got::Absent));
            out.push(got);
            if absent > spare {
                break;
            }
        }
        Ok(Some(out))
    }
}

/// How far back the cache reaches: the oldest half hour of the oldest
/// total any rainfall layer still wants (its loop window and the hold
/// before it), and an hour's margin.
fn keep() -> Duration {
    let reach = wx_core::LAYERS
        .iter()
        .filter_map(|l| {
            let input = l.input_for(&reg::CMORPH)?;
            let n = window(input.product)? as i32;
            Some(
                half() * n + Duration::minutes(i64::from(l.retention_minutes + input.hold_minutes)),
            )
        })
        .max()
        .unwrap_or_default();
    reach + Duration::hours(1)
}

/// Every need decoded by `decode`, which holds what it is made from.
fn custom(decode: Decoder, needs: &[Need]) -> Fetched {
    Fetched::all(
        needs,
        Payload::Custom {
            bytes: Bytes::new(),
            decode,
        },
    )
}

#[async_trait::async_trait]
impl Observed for Cmorph {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        common::one_region(source, "CMORPH2")
    }

    fn cadence(&self, _: &SourceDef, product: &str) -> Cadence {
        // After the valid time: the start of the half hour for the rate, its
        // end for a total.
        let after_valid = valid_of(product, DateTime::UNIX_EPOCH) - DateTime::UNIX_EPOCH;
        Cadence::every(
            half(),
            Duration::minutes(reg::LATENCY_MINUTES as i64) - after_valid,
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
        if product != reg::RATE && window(product).is_none() {
            return Err(FetchError::object(product, "not a CMORPH2 product"));
        }
        let mut out = Vec::new();
        // A total valid at `from` ends a half hour that started before it.
        for day in clock::days(from - half(), cx.now) {
            for (start, l) in self.day(cx, day).await?.iter() {
                let valid = valid_of(product, *start);
                if valid >= from {
                    out.push(ObsFile {
                        valid,
                        name: l.url.clone(),
                        size: l.size,
                    });
                }
            }
        }
        Ok(out)
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        product: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let Some(n) = window(product) else {
            // The rate: this half hour alone.
            let start = file.valid;
            let l = self.listed(cx, start).await?.unwrap_or_else(|| Listing {
                url: file.name.clone(),
                modified: None,
                size: file.size,
            });
            let Some(h) = self.half_hour(cx, start, &l).await? else {
                return Ok(Fetched::Missing);
            };
            let rates = h.rates.clone();
            let decode: Decoder = Arc::new(move |_: &[u8], _: &Need| {
                Ok(Decoded {
                    field: rates.field(),
                    valid: Some(start),
                })
            });
            return Ok(custom(decode, needs));
        };
        let end = file.valid;
        let Some(window) = self.window(cx, end, n).await? else {
            return Ok(Fetched::Missing);
        };
        let here: Vec<Arc<Rates>> = window
            .into_iter()
            .filter_map(|g| match g {
                Got::Here(h) => Some(h.rates.clone()),
                Got::Absent => None,
            })
            .collect();
        if here.len() < min_present(n) {
            // Not given up: tried again the next time it is listed.
            tracing::warn!(product, %end, have = here.len(), of = n, "too few half hours for a total; trying again later");
            return Ok(Fetched::Missing);
        }
        let decode: Decoder = Arc::new(move |_: &[u8], _: &Need| {
            let halves: Vec<&Rates> = here.iter().map(Arc::as_ref).collect();
            Ok(Decoded {
                field: total(&halves, n)?,
                valid: Some(end),
            })
        });
        Ok(custom(decode, needs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::observed::ObservedTask;
    use crate::remote::memory::MemoryRemote;
    use crate::testing;
    use std::collections::BTreeSet;
    use wx_core::proj::LatLonGrid;

    const HOST: &str = "https://example.org/cmorph/DATA/";

    use crate::testing::on as t;

    #[test]
    fn names_and_windows() {
        let start = t(6, 17, 0);
        assert_eq!(file_name(start), "CMORPH2_0.25deg-30min_202611061700.RT.nc");
        assert_eq!(
            file_url(BASE, start),
            "https://ftp.cpc.ncep.noaa.gov/precip/CMORPH2/CMORPH2NRT/DATA/2026/202611/20261106/CMORPH2_0.25deg-30min_202611061700.RT.nc"
        );
        assert_eq!(parse_name(&file_name(start)), Some(start));
        assert_eq!(parse_name("CMORPH2_0.25deg-30min_202611061715.RT.nc"), None);
        assert_eq!(parse_name("CMORPH2_0.25deg-30min_202611061700.nc"), None);
        assert_eq!(parse_name("CMORPH2_0.05deg-30min_202611061700.RT.nc"), None);
        assert_eq!(
            (min_present(2), min_present(48), min_present(144)),
            (2, 44, 130)
        );
        assert_eq!(window(reg::RATE), None);
        assert_eq!(valid_of(reg::RATE, start), start);
        assert_eq!(valid_of(reg::TOTAL_24H, start), start + half());
        assert_eq!(
            (
                window(reg::TOTAL_1H),
                window(reg::TOTAL_24H),
                window(reg::TOTAL_72H)
            ),
            (Some(2), Some(48), Some(144))
        );
        // Every product of the registry is one of these four.
        for (_, input) in crate::driver::inputs_of(wx_core::LAYERS, &reg::CMORPH) {
            assert!(
                input.product == reg::RATE || window(input.product).is_some(),
                "{}",
                input.product
            );
        }
        assert!(keep() >= half() * 144 + Duration::hours(3));
    }

    #[test]
    fn the_real_file_round_trips_through_the_cache() {
        let d = wx_formats::cmorph::decode(&testing::fixture(
            "formats/CMORPH2_0.25deg-30min_202611061700.RT.nc",
        ))
        .unwrap();
        let r = Rates::from_field(&d.field).unwrap();
        let back = r.field();
        assert_eq!(
            (back.nx, back.ny, &back.proj),
            (d.field.nx, d.field.ny, &d.field.proj)
        );
        // The file's scale factor is a float32 0.01, so the decoded values
        // are a few parts in 1e8 off the hundredths kept here.
        for (a, b) in back.values.iter().zip(&d.field.values) {
            assert!(
                a.is_nan() == b.is_nan() && (a.is_nan() || (a - b).abs() <= 1e-6 * b.abs()),
                "{a} {b}"
            );
        }
        // 77 missing, 791 165 dry of 1 036 800; a quarter of the floats' size.
        assert_eq!(r.missing.len(), 77);
        assert_eq!(r.small.len(), 1_036_800 - 791_165 - 77);
        assert!(r.approx_bytes() < 400_000, "{}", r.approx_bytes());
        assert!(r.large.len() < r.small.len() / 10);
    }

    /// A 4 x 3 grid of `rate` mm/h, with `missing` cells empty.
    fn rates(rate: f32, missing: &[usize]) -> Rates {
        let mut values = vec![rate; 12];
        for &k in missing {
            values[k] = f32::NAN;
        }
        // One cell's rate is twice the others', one stays dry.
        values[5] *= 2.0;
        values[11] = 0.0;
        Rates::from_field(&Field {
            nx: 4,
            ny: 3,
            proj: GridProjection::LatLon(LatLonGrid {
                lon0: 0.125,
                lat0: -0.25,
                dlon: 0.25,
                dlat: 0.25,
                periodic: false,
            }),
            values,
            grid_relative_winds: false,
        })
        .unwrap()
    }

    #[test]
    fn totals_are_exact_sums() {
        // An hour: 1.5 mm/h then 2.25 mm/h is 0.75 + 1.125 mm.
        let (a, b) = (rates(1.5, &[]), rates(2.25, &[]));
        let hour = total(&[&a, &b], 2).unwrap();
        assert_eq!(hour.values[0], 1.875);
        assert_eq!(hour.values[5], 3.75);
        assert_eq!(hour.values[11], 0.0);
        // A day at 0.37 mm/h is 8.88 mm, and large counts add up too.
        let wet = rates(0.37, &[]);
        let storm = rates(61.07, &[]);
        let day: Vec<&Rates> = (0..48)
            .map(|k| if k == 7 { &storm } else { &wet })
            .collect();
        let d = total(&day, 48).unwrap();
        assert_eq!(d.values[0], ((47 * 37 + 6107) as f64 / 200.0) as f32);
        // 44 of 48 half hours: scaled to the day; 43 are too few.
        let some: Vec<&Rates> = vec![&wet; 44];
        assert_eq!(total(&some, 48).unwrap().values[0], 8.88);
        assert!(total(&some[..43], 48).is_err());
        // 46 of 48 half hours, some without a value in cell 3: that cell is
        // missing once it has values in fewer than 44, and scaled from
        // those it has until then.
        let holed = rates(0.37, &[3]);
        let mut mixed: Vec<&Rates> = vec![&wet; 43];
        mixed.extend(vec![&holed; 3]);
        let m = total(&mixed, 48).unwrap();
        assert!(m.values[3].is_nan());
        assert_eq!(m.values[0], 8.88);
        mixed.remove(0);
        mixed.insert(0, &wet);
        mixed.pop();
        mixed.push(&wet);
        let kept = total(&mixed, 48).unwrap().values[3];
        assert!((kept - 8.88).abs() < 1e-5, "{kept}");
        assert!(
            total(&[&a, &b, &a], 2).is_err(),
            "more half hours than the window"
        );
        let odd = Rates {
            nx: 3,
            ny: 4,
            ..a.clone()
        };
        assert!(total(&[&a, &odd], 2).is_err());
    }

    /// Made-up files: `start rate missing-cell-or-none`, read by [`toy`].
    fn toy_file(start: DateTime<Utc>, rate: f32) -> Bytes {
        Bytes::from(format!("{} {rate}", start.format("%Y%m%d%H%M")))
    }

    fn toy(bytes: &[u8]) -> anyhow::Result<(Field, DateTime<Utc>)> {
        let text = std::str::from_utf8(bytes)?;
        let (stamp, rate) = text
            .split_once(' ')
            .ok_or_else(|| anyhow::anyhow!("not a toy file"))?;
        let start = NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M")?.and_utc();
        let r = rates(rate.parse()?, &[]);
        Ok((r.field(), start))
    }

    /// CPC's host in memory: files by half hour, each day's Apache index
    /// rewritten whenever a file is.
    struct Host {
        mem: Arc<MemoryRemote>,
        /// Start, listed time.
        files: Mutex<BTreeMap<DateTime<Utc>, DateTime<Utc>>>,
    }

    impl Host {
        fn new() -> Self {
            Self {
                mem: testing::remote(&[]),
                files: Mutex::default(),
            }
        }

        fn publish(&self, start: DateTime<Utc>, body: Bytes, written: DateTime<Utc>) {
            self.mem.put_modified(&file_url(HOST, start), body, written);
            let mut files = self.files.lock().unwrap();
            files.insert(start, written);
            self.index(&files, start.date_naive());
        }

        fn remove(&self, start: DateTime<Utc>) {
            self.mem.remove(&file_url(HOST, start));
            let mut files = self.files.lock().unwrap();
            files.remove(&start);
            self.index(&files, start.date_naive());
        }

        /// Touch the listing only, as a rewrite with the same content looks.
        fn relist(&self, start: DateTime<Utc>, written: DateTime<Utc>) {
            let mut files = self.files.lock().unwrap();
            files.insert(start, written);
            self.index(&files, start.date_naive());
        }

        fn index(&self, files: &BTreeMap<DateTime<Utc>, DateTime<Utc>>, day: NaiveDate) {
            let mut html = String::from(
                "<html><body><table><tr><th><a href=\"?C=N;O=D\">Name</a></th></tr>\
                 <tr><td><a href=\"/cmorph/DATA/2026/202611/\">Parent Directory</a></td><td>&nbsp;</td><td align=\"right\">  - </td></tr>\n",
            );
            for (start, written) in files.range(day.and_hms_opt(0, 0, 0).unwrap().and_utc()..) {
                if start.date_naive() != day {
                    break;
                }
                let name = file_name(*start);
                html.push_str(&format!(
                    "<tr><td><a href=\"{name}\">{name}</a></td><td align=\"right\">{}  </td><td align=\"right\">565K</td></tr>\n",
                    written.format("%d-%b-%Y %H:%M")
                ));
            }
            html.push_str("</table></body></html>");
            self.mem.put(&day_url(HOST, day), html);
        }

        /// GETs of files (not listings), by URL.
        fn downloads(&self) -> Vec<String> {
            self.mem
                .requests()
                .into_iter()
                .filter(|r| r.ends_with(".RT.nc"))
                .collect()
        }
    }

    fn adapter() -> Arc<Cmorph> {
        Arc::new(Cmorph::with_decoder(HOST, Arc::new(toy)))
    }

    /// Every half hour from `first` to `last` (starts), at `rate`, each
    /// written 45 minutes after it starts.
    fn publish_range(
        host: &Host,
        first: DateTime<Utc>,
        last: DateTime<Utc>,
        rate: impl Fn(DateTime<Utc>) -> f32,
    ) {
        let mut s = first;
        while s <= last {
            host.publish(s, toy_file(s, rate(s)), s + Duration::minutes(45));
            s += half();
        }
    }

    fn task(
        engine: &Arc<crate::engine::Engine>,
        c: &Arc<Cmorph>,
        product: &'static str,
    ) -> ObservedTask {
        ObservedTask::new(engine.clone(), c.clone(), &reg::CMORPH, product)
    }

    /// The summed field `product` would hand the engine for the window
    /// ending at `end`.
    async fn summed(
        c: &Cmorph,
        engine: &crate::engine::Engine,
        product: &'static str,
        end: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Option<Field> {
        let input = crate::driver::inputs_of(wx_core::LAYERS, &reg::CMORPH)
            .into_iter()
            .find(|(_, i)| i.product == product)
            .unwrap()
            .1;
        let need = Need {
            input,
            field: input.fields.first().copied(),
            lead: 0,
        };
        let cx = Cx::new(&engine.remote, "test", now);
        let file = ObsFile {
            valid: end,
            name: file_url(HOST, end - half()),
            size: None,
        };
        match c
            .fetch(&cx, &reg::CMORPH, product, &file, &[need])
            .await
            .unwrap()
        {
            Fetched::Got(mut p) => match p.remove(0)? {
                Payload::Custom { bytes, decode } => Some(decode(&bytes, &need).unwrap().field),
                Payload::Grib(_) => unreachable!(),
            },
            Fetched::Missing => None,
        }
    }

    fn ids(engine: &crate::engine::Engine, layer: &str) -> Vec<String> {
        engine
            .catalog
            .timeline(layer)
            .parts
            .iter()
            .filter(|p| p.source == "cmorph")
            .map(|p| p.id.clone())
            .collect()
    }

    /// From an empty cache: three days of half hours trickle in, each
    /// downloaded once whichever task needs it first, and every layer gets
    /// its parts for the loop window.
    #[tokio::test]
    async fn a_cold_start_fills_in_every_total() {
        let host = Host::new();
        let now = t(6, 20, 40);
        // The newest half hour starts at 19:30 and ends at 20:00.
        publish_range(&host, t(3, 12, 0), t(6, 19, 30), |_| 0.5);
        let (engine, _) = testing::engine(&host.mem);
        let c = adapter();
        let mut radar = task(&engine, &c, reg::RATE);
        let mut hour = task(&engine, &c, reg::TOTAL_1H);
        let mut day = task(&engine, &c, reg::TOTAL_24H);
        let mut three = task(&engine, &c, reg::TOTAL_72H);
        assert_eq!(
            [&radar, &hour, &day, &three].map(|t| t.name.as_str()),
            [
                "cmorph.radar",
                "cmorph.rain_1h",
                "cmorph.rain_24h",
                "cmorph.rain_72h"
            ]
        );
        for t in [&mut radar, &mut hour, &mut day, &mut three] {
            // Backfills run cycle after cycle until nothing is left.
            for _ in 0..4 {
                if t.cycle(now).await.unwrap().remaining == 0 {
                    break;
                }
            }
        }
        // Radar: the half hours starting in the two-hour loop (18:40 on),
        // valid at their start. The totals: those ending in the three-hour
        // loop (17:40 on), valid at their end.
        assert_eq!(
            ids(&engine, "radar"),
            ["cmorph-2611061900", "cmorph-2611061930"]
        );
        let loop3 = [
            "cmorph-2611061800",
            "cmorph-2611061830",
            "cmorph-2611061900",
            "cmorph-2611061930",
            "cmorph-2611062000",
        ];
        for layer in ["rain_1h", "rain_24h", "rain_72h"] {
            assert_eq!(ids(&engine, layer), loop3, "{layer}");
            let part = engine.catalog.latest(layer, "cmorph").unwrap();
            assert!(
                part.bounds.west.abs() < 0.1 && part.bounds.east > 0.9,
                "{layer}: {:?}",
                part.bounds
            );
        }
        // Totals ending from 15:30 still stand in for the loop's first ticks
        // (their hold), so the half hours from 72 hours before 15:30 to 20:00
        // are needed: each fetched once.
        let got = host.downloads();
        let distinct: BTreeSet<&String> = got.iter().collect();
        assert_eq!(got.len(), distinct.len(), "downloaded twice: {got:?}");
        assert_eq!(got.len(), 153);
        assert!(
            !got.iter().any(|u| u.contains("202611031500"))
                && got.iter().any(|u| u.contains("202611031530"))
        );
        assert_eq!(c.cached().0, 153);
        // The sums: 0.5 mm/h for 1, 24 and 72 hours.
        for (product, mm) in [
            (reg::TOTAL_1H, 0.5),
            (reg::TOTAL_24H, 12.0),
            (reg::TOTAL_72H, 36.0),
        ] {
            let f = summed(&c, &engine, product, t(6, 20, 0), now)
                .await
                .unwrap();
            assert_eq!(
                (f.values[0], f.values[5], f.values[11]),
                (mm, 2.0 * mm, 0.0),
                "{product}"
            );
        }

        // Half an hour later one new file is all that is fetched.
        host.mem.clear_log();
        host.publish(t(6, 20, 0), toy_file(t(6, 20, 0), 0.5), t(6, 20, 45));
        let later = t(6, 21, 10);
        for t in [&mut radar, &mut hour, &mut day, &mut three] {
            assert_eq!(
                t.cycle(later).await.unwrap().tally.produced,
                1,
                "{}",
                t.name
            );
        }
        assert_eq!(host.downloads(), [file_url(HOST, t(6, 20, 0))]);
        // The four tasks shared one read of the day's listing.
        let lists = host
            .mem
            .requests()
            .into_iter()
            .filter(|r| r.ends_with("/20261106/"))
            .count();
        assert_eq!(lists, 1);
    }

    /// The 90 percent rule: a total is published, scaled to its full
    /// period, while no more than a tenth of its half hours are missing, and
    /// not while more are; it is not given up, so a half hour filled in late
    /// still makes it. The newest half hour is waited for.
    #[tokio::test]
    async fn missing_half_hours() {
        let host = Host::new();
        let now = t(6, 20, 40);
        publish_range(&host, t(5, 20, 0), t(6, 19, 30), |_| 1.0);
        // Four of the day's 48 are missing: published, scaled up.
        for s in [t(6, 2, 0), t(6, 2, 30), t(6, 3, 0), t(6, 11, 30)] {
            host.remove(s);
        }
        let (engine, _) = testing::engine(&host.mem);
        let f = summed(&adapter(), &engine, reg::TOTAL_24H, t(6, 20, 0), now)
            .await
            .unwrap();
        assert_eq!(f.values[0], 24.0);
        assert_eq!(f.values[5], 48.0);
        // A fifth, 19:00: the day ending at 20:00 is not made. (The one
        // ending at 19:30 is not even listed: its newest half hour is gone.)
        host.remove(t(6, 19, 0));
        let (engine, _) = testing::engine(&host.mem);
        let c = adapter();
        let mut day = task(&engine, &c, reg::TOTAL_24H);
        let report = day.cycle(now).await.unwrap();
        assert_eq!(report.tally.unavailable, 0, "{:?}", report.tally);
        let days = ids(&engine, "rain_24h");
        assert!(!days.contains(&"cmorph-2611062000".to_string()), "{days:?}");
        // An hour needs both its half hours.
        let mut hour = task(&engine, &c, reg::TOTAL_1H);
        hour.cycle(now).await.unwrap();
        assert_eq!(
            ids(&engine, "rain_1h"),
            [
                "cmorph-2611061800",
                "cmorph-2611061830",
                "cmorph-2611061900"
            ]
        );
        // The newest half hour of a total is waited for, not counted
        // missing: nothing has started at 20:00 yet.
        assert!(
            summed(&c, &engine, reg::TOTAL_1H, t(6, 20, 30), now)
                .await
                .is_none()
        );
        // CPC fills in 02:00 late: the day ending at 20:00 is made after all
        // (two minutes on, when the day's index is read again).
        host.publish(t(6, 2, 0), toy_file(t(6, 2, 0), 1.0), t(6, 20, 45));
        day.cycle(now + Duration::minutes(2)).await.unwrap();
        assert!(ids(&engine, "rain_24h").contains(&"cmorph-2611062000".to_string()));
    }

    /// An index that answers 404 or an error page for a past day is an
    /// error the task retries, not an empty day kept for hours; an
    /// incomplete one is used but read again a minute later.
    #[tokio::test]
    async fn a_bad_index_is_retried_not_kept() {
        let host = Host::new();
        let now = t(6, 20, 40);
        publish_range(&host, t(3, 12, 0), t(6, 19, 30), |_| 0.5);
        let (engine, _) = testing::engine(&host.mem);
        let c = adapter();
        let mut three = task(&engine, &c, reg::TOTAL_72H);
        let fourth = day_url(HOST, t(4, 0, 0).date_naive());
        let good = host.mem.requests();
        assert!(good.is_empty());
        host.mem.put(
            &fourth,
            "<html><body><h1>Service Unavailable</h1></body></html>",
        );
        let e = three
            .cycle(now)
            .await
            .err()
            .expect("an error page is an error");
        assert!(format!("{e:#}").contains("lists no CMORPH2 file"), "{e:#}");
        host.mem.remove(&fourth);
        let e = three
            .cycle(now + Duration::minutes(1))
            .await
            .err()
            .expect("a 404 is an error");
        assert!(format!("{e:#}").contains("is not there"), "{e:#}");
        assert!(ids(&engine, "rain_72h").is_empty());
        // The index comes back, one file short of the day: the totals are
        // made at the next try, and the short day is read again a minute
        // later rather than kept.
        host.remove(t(4, 9, 30));
        let later = now + Duration::minutes(2);
        for _ in 0..3 {
            three.cycle(later).await.unwrap();
        }
        assert_eq!(ids(&engine, "rain_72h").len(), 5);
        let reads = |m: &MemoryRemote| m.requests().iter().filter(|r| **r == fourth).count();
        let before = reads(&host.mem);
        let f = summed(
            &c,
            &engine,
            reg::TOTAL_72H,
            t(6, 18, 0),
            later + Duration::minutes(2),
        )
        .await
        .unwrap();
        assert_eq!(f.values[0], 36.0);
        assert_eq!(reads(&host.mem), before + 1);
        // A complete settled day is kept: no read of the fifth's index.
        let fifth = day_url(HOST, t(5, 0, 0).date_naive());
        let before = host.mem.requests().iter().filter(|r| **r == fifth).count();
        summed(
            &c,
            &engine,
            reg::TOTAL_72H,
            t(6, 18, 0),
            later + Duration::minutes(4),
        )
        .await
        .unwrap();
        assert_eq!(
            host.mem.requests().iter().filter(|r| **r == fifth).count(),
            before
        );
    }

    /// A rewritten half hour is fetched again before it goes into the next
    /// total; one relisted without a change costs a conditional request
    /// answered 304 (nothing to decode). Parts already made stay as they
    /// are.
    #[tokio::test]
    async fn a_rewritten_file_changes_the_next_total() {
        let host = Host::new();
        let now = t(6, 20, 40);
        publish_range(&host, t(6, 19, 0), t(6, 19, 30), |_| 1.0);
        let (engine, _) = testing::engine(&host.mem);
        let decoded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = decoded.clone();
        let c = Arc::new(Cmorph::with_decoder(
            HOST,
            Arc::new(move |b: &[u8]| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                toy(b)
            }),
        ));
        let decodes = || decoded.load(std::sync::atomic::Ordering::SeqCst);
        let mut hour = task(&engine, &c, reg::TOTAL_1H);
        hour.cycle(now).await.unwrap();
        assert_eq!(ids(&engine, "rain_1h"), ["cmorph-2611062000"]);
        assert_eq!(
            summed(&c, &engine, reg::TOTAL_1H, t(6, 20, 0), now)
                .await
                .unwrap()
                .values[0],
            1.0
        );
        assert_eq!(decodes(), 2);

        // At 21:15 CPC writes 20:00 and rewrites 19:30 with more rain (and
        // relists 19:00 unchanged).
        host.mem.clear_log();
        let at = t(6, 21, 15);
        host.publish(t(6, 19, 30), toy_file(t(6, 19, 30), 3.0), at);
        host.publish(t(6, 20, 0), toy_file(t(6, 20, 0), 1.0), at);
        host.relist(t(6, 19, 0), at);
        let later = t(6, 21, 20);
        hour.cycle(later).await.unwrap();
        assert_eq!(
            ids(&engine, "rain_1h"),
            ["cmorph-2611062000", "cmorph-2611062030"]
        );
        // 19:30 at 3 mm/h and 20:00 at 1 mm/h.
        assert_eq!(
            summed(&c, &engine, reg::TOTAL_1H, t(6, 20, 30), later)
                .await
                .unwrap()
                .values[0],
            2.0
        );
        // The new and the rewritten files are fetched and decoded; 19:00,
        // looked at again for the hour ending 19:30 (still short of 18:30),
        // only asked about.
        let mut got = host.downloads();
        got.sort();
        assert_eq!(
            got,
            [
                file_url(HOST, t(6, 19, 0)),
                file_url(HOST, t(6, 19, 30)),
                file_url(HOST, t(6, 20, 0))
            ]
        );
        assert_eq!(decodes(), 4);
        // The total ending at 20:00 (19:00 and 19:30) now sums the new 19:30
        // with nothing more fetched; the part made from the first version
        // stays.
        host.mem.clear_log();
        assert_eq!(
            summed(&c, &engine, reg::TOTAL_1H, t(6, 20, 0), later)
                .await
                .unwrap()
                .values[0],
            2.0
        );
        assert!(host.downloads().is_empty());
        assert_eq!(c.cached().0, 3);
    }

    /// The real 16:30 and 17:00 files: two radar parts and an hourly total
    /// on a global grid, from two downloads.
    #[tokio::test]
    async fn real_files_become_parts() {
        let host = Host::new();
        for (start, name) in [
            (
                t(6, 16, 30),
                "formats/CMORPH2_0.25deg-30min_202611061630.RT.nc",
            ),
            (
                t(6, 17, 0),
                "formats/CMORPH2_0.25deg-30min_202611061700.RT.nc",
            ),
        ] {
            host.publish(
                start,
                testing::fixture(name).into(),
                start + Duration::minutes(45),
            );
        }
        let (engine, store) = testing::engine(&host.mem);
        let c = Arc::new(Cmorph::at(HOST));
        let now = t(6, 18, 0);
        let mut radar = task(&engine, &c, reg::RATE);
        assert_eq!(radar.cycle(now).await.unwrap().tally.produced, 2);
        let mut hour = task(&engine, &c, reg::TOTAL_1H);
        assert_eq!(hour.cycle(now).await.unwrap().tally.produced, 1);
        // The rate parts at the start of their half hours, the total at the
        // end of its newest.
        assert_eq!(
            ids(&engine, "radar"),
            ["cmorph-2611061630", "cmorph-2611061700"]
        );
        assert_eq!(ids(&engine, "rain_1h"), ["cmorph-2611061730"]);
        assert_eq!(
            engine.catalog.latest("radar", "cmorph").unwrap().valid,
            t(6, 17, 0)
        );
        let part = engine.catalog.latest("radar", "cmorph").unwrap();
        assert_eq!((part.bounds.west, part.bounds.east), (-180.0, 180.0));
        assert!(
            part.bounds.south < -89.8 && part.bounds.north > 89.8,
            "{:?}",
            part.bounds
        );
        assert!(
            part.solid
                .is_some_and(|s| s.west == -180.0 && s.east == 180.0 && s.north > 80.0)
        );
        let keys = testing::keys(store.as_ref()).await;
        assert!(keys.contains(&wx_core::keys::grid("rain_1h", "cmorph-2611061730")));
        assert_eq!(host.downloads().len(), 2);
        // A file named after another half hour than it holds is refused.
        host.publish(
            t(6, 17, 30),
            testing::fixture("formats/CMORPH2_0.25deg-30min_202611061700.RT.nc").into(),
            t(6, 18, 15),
        );
        let report = radar.cycle(t(6, 18, 30)).await.unwrap();
        assert_eq!(
            (report.tally.produced, report.tally.failed),
            (0, 1),
            "{:?}",
            report.tally
        );
    }
}
