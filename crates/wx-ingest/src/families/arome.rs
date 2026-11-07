//! AROME: Météo-France's 2.5 km model over metropolitan France and five
//! overseas domains, in the public bucket at [`HOST`], which has no index
//! files. Runs, horizons, fields, sizes and what was checked are in the
//! registry module (`wx_core::families::arome`).
//!
//! * France (`EURW1S`):
//!   `pnt/<RUN>/arome/0025/SP1/arome__0025__SP1__07H12H__<RUN>.grib2`, one
//!   file per group of hours ([`group`]), runs every 3 hours to 51 h.
//! * Overseas (`ANTIL`, `GUYANE`, `INDIEN`, `NCALED`, `POLYN`):
//!   `pnt/<RUN>/arome-om/GUYANE/0025/SP1/arome-om-GUYANE__0025__SP1__012H__<RUN>.grib2`,
//!   one file per hour, runs every 6 hours to 48 h.
//!
//! `<RUN>` is `2026-11-06T12:00:00Z`, the package (`SP1`) is the selectors'
//! `level`. A run is listed per package; the bucket has no partial objects,
//! so whatever is listed is complete.
//!
//! # Fetching
//!
//! The adapter makes each file's inventory itself: one [`IdxEntry`] per
//! message, `var` the short name [`GRIB_IDS`] gives its numbers and level
//! (or `d.c.n` for parameters no layer reads), `level` the package, and its
//! time from the product template, so [`Cx::grib_by_inventory`] can fetch
//! any of its messages by range and [`idx::find`] picks them as for NOAA's
//! inventories.
//!
//! * A file of up to [`WHOLE_UP_TO`] bytes (every overseas file, 0.8 to
//!   14 MB) is downloaded whole: its inventory is a scan of the bytes and
//!   its payloads are slices of them.
//! * A larger one (the France groups, 28 to 58 MB) is walked: one ranged GET
//!   of [`HEAD`] bytes at each message's start gives its length and what it
//!   holds, then the wanted messages are fetched, in a few ranges since a
//!   parameter's steps sit together (3 for the 8 SP1 fields of a group).
//!   Measured on the 6 Nov 15 UTC 07H12H group: the walk took 90 requests and
//!   21 s at the host's pace (200 ms apart), then 28.1 MB came in 3 ranges;
//!   downloading it whole took 3.7 s for 55.8 MB. Walking halves what France
//!   costs (237 MB a run and 1.9 GB a day instead of 470 MB and 3.8 GB) for
//!   about 800 small requests a run, spread over the hour the run takes to
//!   land, and only the wanted half is held in memory.
//!
//! Inventories are kept by file name and size ([`KEPT_INVENTORIES`] of
//! them), so a later need from a file already seen (the start of a step that
//! ends in the next file, a part that failed, a lead coming within the
//! horizon) costs one ranged GET of its messages, never another walk or
//! whole download. A re-published file has another size and is looked at
//! afresh; an inventory that turns out not to match its file is forgotten.
//!
//! Step 0 holds no accumulation, gust or cloud ([`at_step_zero`]): those
//! needs are answered `None` (without a request when nothing else is wanted
//! from the file), and their parts given up.
//!
//! # When files land
//!
//! [`arrival`] is the median of the bucket's `LastModified` over every run
//! of 1-6 Nov 2026. France lands about 2, 3 or 4.5 hours after its run
//! depending on the run's hour ([`FRANCE_ARRIVAL`]), a group every 6 or 7
//! minutes; the overseas domains about 6 h 26 after theirs (INDIEN 6 h 39),
//! a file every minute or so ([`layout`]). The driver lists a run from ten
//! minutes before its first file is due, never earlier. Past hours of a run
//! that lands late are offered like any other, so the inputs that fill the
//! past take them.

use crate::driver::{
    Cx, Cycle, Decoder, Fetched, Listing, Model, ModelFile, Need, Payload, grib_picking,
};
use crate::idx::{self, ByteRange, IdxEntry, IdxTime};
use crate::remote::{FetchError, S3Bucket};
use anyhow::{Context, ensure};
use chrono::{DateTime, Duration, Timelike, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wx_core::families::arome::{self as reg, GRIB_IDS, GribId};
use wx_core::layer::Stat;
use wx_core::{LAYERS, SourceDef};
use wx_pipeline::{HeadScan, MessageInfo, Timing};

pub use wx_core::families::arome::PRODUCT;

/// The bucket, addressed virtual-hosted style.
pub const HOST: &str = "https://meteofrance-pnt.s3.rbx.io.cloud.ovh.net";

/// Files up to this size are downloaded whole; larger ones are walked (see
/// "Fetching"). The overseas files reach 14.3 MB, the France groups start
/// at 28 MB.
pub const WHOLE_UP_TO: u64 = 24 * 1024 * 1024;

/// Bytes read at each message's start while walking: section 0 gives the
/// length, sections 1 to 5 and the start of 6 say what it holds (about
/// 230 bytes in these files).
pub const HEAD: u64 = 512;

/// Inventories kept per source: more than a France run's window of groups
/// (8 runs of 9) and an overseas run's 49 files twice over.
pub const KEPT_INVENTORIES: usize = 128;

/// A walk giving up past this many messages has met a broken file (a France
/// group has 97).
const MAX_MESSAGES: usize = 2000;

/// Code table 4.5: the ground or water surface.
const GROUND: u8 = 1;

/// Minutes after a France run that its first group (00H06H) and its last
/// (49H51H) usually land, by the run's hour (00, 03, ..., 21 UTC). The 06
/// and 18 UTC runs are the late ones.
pub const FRANCE_ARRIVAL: [(u32, u32); 8] = [
    (121, 170),
    (113, 164),
    (263, 307),
    (207, 255),
    (188, 242),
    (186, 233),
    (258, 308),
    (204, 253),
];

/// Groups in a France run.
const FRANCE_GROUPS: u32 = 9;

/// Where one region's runs are, and when they land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The overseas domain's code (`GUYANE`, as in its paths); `None` for
    /// France.
    pub domain: Option<&'static str>,
    /// Hours between runs.
    pub every: u32,
    /// Last forecast hour.
    pub horizon: u32,
    /// Overseas only: minutes after the run that its first file (`000H`) and
    /// its last usually land. France's depend on the run ([`FRANCE_ARRIVAL`]).
    pub arrival: (u32, u32),
}

const fn overseas(domain: &'static str, arrival: (u32, u32)) -> Layout {
    Layout {
        domain: Some(domain),
        every: reg::OVERSEAS_RUN_EVERY_HOURS,
        horizon: reg::OVERSEAS_HORIZON_HOURS,
        arrival,
    }
}

pub fn layout(region: &str) -> Option<Layout> {
    Some(match region {
        "EURW1S" => Layout {
            domain: None,
            every: reg::FRANCE_RUN_EVERY_HOURS,
            horizon: reg::FRANCE_HORIZON_HOURS,
            arrival: (0, 0),
        },
        "ANTIL" => overseas("ANTIL", (389, 441)),
        "GUYANE" => overseas("GUYANE", (386, 428)),
        "INDIEN" => overseas("INDIEN", (399, 477)),
        "NCALED" => overseas("NCALED", (387, 432)),
        "POLYN" => overseas("POLYN", (386, 432)),
        _ => return None,
    })
}

/// The France group holding forecast hour `hour`: its first and last hour
/// (`(0, 6)`, `(7, 12)`, ..., `(49, 51)`) and its place in the run.
pub fn group(hour: u32) -> (u32, u32, u32) {
    let index = if hour <= 6 { 0 } else { (hour - 1) / 6 };
    let first = if index == 0 { 0 } else { 6 * index + 1 };
    let last = (6 * index + 6).min(reg::FRANCE_HORIZON_HOURS);
    (first, last, index)
}

/// `2026-11-06T12:00:00Z`.
pub fn run_label(run: DateTime<Utc>) -> String {
    run.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The directory of one package of a run.
pub fn package_prefix(l: &Layout, run: DateTime<Utc>, product: &str, package: &str) -> String {
    let r = run_label(run);
    match l.domain {
        None => format!("pnt/{r}/arome/{product}/{package}/"),
        Some(d) => format!("pnt/{r}/arome-om/{d}/{product}/{package}/"),
    }
}

/// The file holding forecast hour `hour` of `package` in `run`.
pub fn file_key(l: &Layout, run: DateTime<Utc>, product: &str, package: &str, hour: u32) -> String {
    let (dir, r) = (package_prefix(l, run, product, package), run_label(run));
    match l.domain {
        None => {
            let (first, last, _) = group(hour);
            format!("{dir}arome__{product}__{package}__{first:02}H{last:02}H__{r}.grib2")
        }
        Some(d) => format!("{dir}arome-om-{d}__{product}__{package}__{hour:03}H__{r}.grib2"),
    }
}

/// When the file holding forecast hour `hour` of `run` usually lands.
pub fn arrival(l: &Layout, run: DateTime<Utc>, hour: u32) -> DateTime<Utc> {
    let (first, last, at, of) = match l.domain {
        None => {
            let (first, last) = FRANCE_ARRIVAL[(run.hour() / 3) as usize % FRANCE_ARRIVAL.len()];
            (first, last, group(hour).2, FRANCE_GROUPS - 1)
        }
        Some(_) => (
            l.arrival.0,
            l.arrival.1,
            hour.min(l.horizon),
            l.horizon.max(1),
        ),
    };
    let spread = i64::from(last.saturating_sub(first)) * 60 * i64::from(at) / i64::from(of);
    let seconds = i64::from(first) * 60 + spread;
    run + Duration::seconds(seconds)
}

/// Whether `need` asks for what step 0 never holds: an accumulation, the
/// gust (a maximum over the hour before) or the total cloud (the registry's
/// cloud input), the one instantaneous field it lacks.
pub fn at_step_zero(need: &Need) -> bool {
    need.lead == 0
        && need.field.is_some_and(|f| {
            f.stat != Stat::Instant || reg::CLOUDS.fields.iter().any(|c| c.var == f.var)
        })
}

/// The short name of the [`GRIB_IDS`] entry a message is.
fn short_name(info: &MessageInfo) -> Option<&'static str> {
    let is = |g: &GribId| {
        let (d, c, n) = g.param;
        let value = (g.surface != GROUND).then_some(f64::from(g.surface_value));
        info.is(d, c, n) && info.at_level(g.surface, value)
    };
    GRIB_IDS.iter().find(|g| is(g)).map(|g| g.short_name)
}

/// A message's time as an inventory line would put it: minutes from the
/// reference time.
fn time_of(info: &MessageInfo) -> IdxTime {
    let minutes = |t: DateTime<Utc>| u32::try_from((t - info.reference_time).num_minutes()).ok();
    match info.time {
        Some(Timing::Instant(t)) => {
            minutes(t).map_or(IdxTime::Other, |minutes| IdxTime::At { minutes })
        }
        Some(Timing::Interval {
            start,
            end,
            process,
        }) => match (minutes(start), minutes(end), process) {
            (Some(from), Some(to), Some(1)) => IdxTime::Acc { from, to },
            (Some(from), Some(to), Some(2)) => IdxTime::Max { from, to },
            _ => IdxTime::Other,
        },
        None => IdxTime::Other,
    }
}

/// The inventory line of one message of a `package` file.
pub fn entry(info: &MessageInfo, range: ByteRange, package: &str) -> IdxEntry {
    IdxEntry {
        range,
        var: short_name(info).map_or_else(
            || format!("{}.{}.{}", info.discipline, info.category, info.number),
            str::to_string,
        ),
        level: package.to_string(),
        time: time_of(info),
    }
}

/// [`entry`], refusing a message of another run.
fn entry_of_run(
    info: &MessageInfo,
    range: ByteRange,
    package: &str,
    run: DateTime<Utc>,
    url: &str,
) -> Result<IdxEntry, FetchError> {
    if info.reference_time != run {
        return Err(FetchError::object(
            url,
            format!(
                "the message at byte {} is of the {} run",
                range.start,
                run_label(info.reference_time)
            ),
        ));
    }
    Ok(entry(info, range, package))
}

/// The inventory of a whole file of `run`, from [`wx_pipeline::scan`].
pub fn inventory(
    infos: &[MessageInfo],
    package: &str,
    run: DateTime<Utc>,
    url: &str,
) -> Result<Vec<IdxEntry>, FetchError> {
    infos
        .iter()
        .map(|m| {
            entry_of_run(
                m,
                ByteRange::at(m.offset as u64, m.length as u64),
                package,
                run,
                url,
            )
        })
        .collect()
}

/// Decodes a need's message: the one field of `run` its selector names at
/// its lead, by the same rule that picked it from the inventory.
pub fn decoder(run: DateTime<Utc>) -> Decoder {
    grib_picking(move |info, need| {
        need.field.is_some_and(|sel| {
            info.reference_time == run
                && entry(info, ByteRange::new(0, 0), sel.level).matches(&sel, need.lead)
        })
    })
}

/// Inventories of files already seen. See "Fetching".
#[derive(Default)]
struct Inventories {
    state: Mutex<Kept>,
}

#[derive(Default)]
struct Kept {
    files: HashMap<String, KeptFile>,
    /// Counts uses, to forget the least recently used.
    stamp: u64,
}

struct KeptFile {
    /// The file's size when the inventory was made.
    size: Option<u64>,
    entries: Arc<Vec<IdxEntry>>,
    used: u64,
}

impl Inventories {
    fn lock(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The inventory of `name` as it was when it had `size` bytes.
    fn get(&self, name: &str, size: Option<u64>) -> Option<Arc<Vec<IdxEntry>>> {
        let mut k = self.lock();
        k.stamp += 1;
        let stamp = k.stamp;
        let kept = k.files.get_mut(name)?;
        if size.is_some() && kept.size != size {
            return None;
        }
        kept.used = stamp;
        Some(kept.entries.clone())
    }

    fn keep(&self, name: &str, size: Option<u64>, entries: Vec<IdxEntry>) -> Arc<Vec<IdxEntry>> {
        let entries = Arc::new(entries);
        let mut k = self.lock();
        k.stamp += 1;
        let stamp = k.stamp;
        let kept = KeptFile {
            size,
            entries: entries.clone(),
            used: stamp,
        };
        k.files.insert(name.to_string(), kept);
        if k.files.len() > KEPT_INVENTORIES
            && let Some(oldest) = k
                .files
                .iter()
                .min_by_key(|(_, f)| f.used)
                .map(|(n, _)| n.clone())
        {
            k.files.remove(&oldest);
        }
        entries
    }

    fn forget(&self, name: &str) {
        self.lock().files.remove(name);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().files.len()
    }
}

pub struct Arome {
    bucket: S3Bucket,
    inventories: Inventories,
    /// [`WHOLE_UP_TO`]; tests walk their cut-down France groups.
    whole_up_to: u64,
}

impl Default for Arome {
    fn default() -> Self {
        Self {
            bucket: S3Bucket::at(HOST),
            inventories: Inventories::default(),
            whole_up_to: WHOLE_UP_TO,
        }
    }
}

impl Arome {
    /// An adapter that walks every file, as it does the France groups: the
    /// test fixtures are cut far below [`WHOLE_UP_TO`].
    #[cfg(test)]
    pub(crate) fn walking() -> Self {
        Self {
            whole_up_to: 0,
            ..Self::default()
        }
    }

    fn layout(source: &SourceDef) -> Result<Layout, FetchError> {
        layout(source.region).ok_or_else(|| {
            FetchError::object(
                source.id,
                format!("no AROME layout for region {:?}", source.region),
            )
        })
    }

    /// The packages `source`'s inputs on `products` read, with their product.
    fn packages(
        source: &SourceDef,
        products: &[&'static str],
    ) -> Vec<(&'static str, &'static str)> {
        let mut out = Vec::new();
        for (_, input) in crate::driver::inputs_of(LAYERS, source) {
            if !products.contains(&input.product) {
                continue;
            }
            for f in input.fields {
                if !out.contains(&(input.product, f.level)) {
                    out.push((input.product, f.level));
                }
            }
        }
        out
    }

    /// Walk a remote file of `size` bytes message by message. `None` when it
    /// is gone.
    async fn walk(
        &self,
        cx: &Cx<'_>,
        url: &str,
        size: u64,
        package: &str,
        run: DateTime<Utc>,
    ) -> Result<Option<Vec<IdxEntry>>, FetchError> {
        let broken = |at: u64, e: &dyn std::fmt::Display| {
            FetchError::object(url, format!("walking, at byte {at}: {e}"))
        };
        let mut out = Vec::new();
        let mut at = 0u64;
        while at < size {
            if out.len() >= MAX_MESSAGES {
                return Err(broken(at, &format!("more than {MAX_MESSAGES} messages")));
            }
            let Some(mut head) = cx
                .get(url)
                .range(ByteRange::new(at, (at + HEAD).min(size)))
                .bytes()
                .await?
            else {
                return Ok(None);
            };
            let len =
                wx_pipeline::message_length(&head).map_err(|e| broken(at, &format!("{e:#}")))?;
            if len < 16 || len > size - at {
                return Err(broken(
                    at,
                    &format!("a message of {len} bytes in a file of {size}"),
                ));
            }
            let info = loop {
                match wx_pipeline::scan_head(&head).map_err(|e| broken(at, &format!("{e:#}")))? {
                    HeadScan::Described(info) => break info,
                    HeadScan::NeedBytes(n) if n > head.len() && n as u64 <= len => {
                        let Some(more) = cx
                            .get(url)
                            .range(ByteRange::at(at, n as u64))
                            .bytes()
                            .await?
                        else {
                            return Ok(None);
                        };
                        head = more;
                    }
                    HeadScan::NeedBytes(n) => {
                        return Err(broken(at, &format!("its headers need {n} bytes")));
                    }
                }
            };
            out.push(entry_of_run(
                &info,
                ByteRange::at(at, len),
                package,
                run,
                url,
            )?);
            at += len;
        }
        Ok(Some(out))
    }

    /// Download a small file whole; its payloads are slices of it.
    async fn whole(
        &self,
        cx: &Cx<'_>,
        url: &str,
        file: &ModelFile,
        needs: &[Need],
        package: &str,
        run: DateTime<Utc>,
    ) -> Result<Fetched, FetchError> {
        let limit = file.size.unwrap_or(4 * WHOLE_UP_TO).max(WHOLE_UP_TO);
        let Some(bytes) = cx.get(url).max_bytes(limit).bytes().await? else {
            return Ok(Fetched::Missing);
        };
        let scanned = {
            let b = bytes.clone();
            cx.cpu(move || wx_pipeline::scan(&b)).await
        };
        let infos = scanned
            .and_then(|r| r)
            .map_err(|e| FetchError::object(url, format!("{e:#}")))?;
        let entries = inventory(&infos, package, run, url)?;
        let entries = self
            .inventories
            .keep(&file.name, Some(bytes.len() as u64), entries);
        let decode = decoder(run);
        let slice = |need: &Need| -> Option<Payload> {
            let r = idx::find(&entries, need.field.as_ref()?, need.lead)?.range;
            let piece = bytes.slice(usize::try_from(r.start).ok()?..usize::try_from(r.end?).ok()?);
            Some(Payload::Custom {
                bytes: piece,
                decode: decode.clone(),
            })
        };
        Ok(Fetched::Got(needs.iter().map(slice).collect()))
    }

    /// The needs' messages by range, from a known inventory. Each message's
    /// head is checked against its line: a file re-published with the same
    /// size has the old inventory's offsets on other messages.
    async fn ranges(
        &self,
        cx: &Cx<'_>,
        url: &str,
        file: &ModelFile,
        entries: &[IdxEntry],
        needs: &[Need],
        run: DateTime<Utc>,
    ) -> Result<Fetched, FetchError> {
        let holds = |bytes: &[u8], need: &Need| {
            need.field.is_some_and(|sel| {
                matches!(wx_pipeline::scan_head(bytes), Ok(HeadScan::Described(m))
                    if m.reference_time == run && entry(&m, ByteRange::new(0, 0), sel.level).matches(&sel, need.lead))
            })
        };
        let got = cx
            .grib_by_inventory(url, entries, file.size, needs)
            .await
            .and_then(|f| match f {
                Fetched::Got(payloads) => {
                    for (p, need) in payloads.iter().zip(needs) {
                        if let Some(Payload::Grib(bytes)) = p
                            && !holds(bytes, need)
                        {
                            return Err(FetchError::object(
                                url,
                                "the file changed since its inventory was made",
                            ));
                        }
                    }
                    Ok(Fetched::Got(payloads))
                }
                missing => Ok(missing),
            });
        let decode = decoder(run);
        let wrap = |p: Option<Payload>| match p {
            Some(Payload::Grib(bytes)) => Some(Payload::Custom {
                bytes,
                decode: decode.clone(),
            }),
            other => other,
        };
        match got {
            Ok(Fetched::Got(payloads)) => {
                Ok(Fetched::Got(payloads.into_iter().map(wrap).collect()))
            }
            Ok(Fetched::Missing) => {
                self.inventories.forget(&file.name);
                Ok(Fetched::Missing)
            }
            Err(e) => {
                self.inventories.forget(&file.name);
                Err(e)
            }
        }
    }
}

#[async_trait::async_trait]
impl Model for Arome {
    /// Step 0 carries no gust and no cloud cover, so no part needs them there.
    fn absent(&self, _: &SourceDef, _: DateTime<Utc>, need: &Need) -> bool {
        at_step_zero(need)
    }

    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        Self::layout(source)?;
        for (layer, input) in crate::driver::inputs_of(LAYERS, source) {
            ensure!(
                input.product == PRODUCT,
                "{}: AROME's product is {PRODUCT:?}, not {:?}",
                layer.id,
                input.product
            );
            ensure!(
                !input.fields.is_empty(),
                "{}: AROME inputs name their fields",
                layer.id
            );
            for f in input.fields {
                ensure!(
                    !f.level.is_empty(),
                    "{}: {} names no package",
                    layer.id,
                    f.var
                );
                GRIB_IDS
                    .iter()
                    .find(|g| g.short_name == f.var)
                    .with_context(|| format!("{}: no GRIB numbers for {:?}", layer.id, f.var))?;
            }
        }
        Ok(())
    }

    fn cycle(&self, source: &SourceDef) -> Cycle {
        let l = layout(source.region);
        let overseas = l.is_some_and(|l| l.domain.is_some());
        Cycle {
            every: Duration::hours(i64::from(
                l.map_or(reg::FRANCE_RUN_EVERY_HOURS, |l| l.every),
            )),
            spare: 2,
            // A run takes 50 (France) to 80 minutes (INDIEN) to land.
            relist: Duration::minutes(5),
            // A whole overseas run was an hour late once in six days; France
            // runs every three hours, so a late one is soon replaced.
            give_up: Duration::minutes(if overseas { 120 } else { 90 }),
        }
    }

    fn leads(&self, source: &SourceDef, _: &str, run: DateTime<Utc>) -> Vec<u32> {
        let Some(l) = layout(source.region) else {
            return Vec::new();
        };
        if !run.hour().is_multiple_of(l.every) || run.minute() != 0 {
            return Vec::new();
        }
        (0..=l.horizon).map(|h| h * 60).collect()
    }

    fn due(&self, source: &SourceDef, _: &str, run: DateTime<Utc>, lead: u32) -> DateTime<Utc> {
        match layout(source.region) {
            Some(l) => arrival(&l, run, lead / 60),
            None => run,
        }
    }

    fn file(&self, source: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        let package = need.field.map_or("", |f| f.level);
        layout(source.region).map_or_else(String::new, |l| {
            file_key(&l, run, need.product(), package, need.lead / 60)
        })
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        run: DateTime<Utc>,
        products: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let l = Self::layout(source)?;
        let mut listing = Listing::new();
        for (product, package) in Self::packages(source, products) {
            for o in cx
                .list_s3(
                    &self.bucket,
                    &package_prefix(&l, run, product, package),
                    None,
                )
                .await?
            {
                listing.add(o.key, Some(o.size));
            }
        }
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
        if needs.iter().all(at_step_zero) {
            return Ok(Fetched::Got(vec![None; needs.len()]));
        }
        let url = self.bucket.url(&file.name);
        // One file is one package.
        let package = needs.iter().find_map(|n| n.field).map_or("", |f| f.level);
        if let Some(entries) = self.inventories.get(&file.name, file.size) {
            return self.ranges(cx, &url, file, &entries, needs, run).await;
        }
        match file.size {
            Some(size) if size > self.whole_up_to => {
                let Some(entries) = self.walk(cx, &url, size, package, run).await? else {
                    return Ok(Fetched::Missing);
                };
                let entries = self.inventories.keep(&file.name, Some(size), entries);
                self.ranges(cx, &url, file, &entries, needs, run).await
            }
            _ => self.whole(cx, &url, file, needs, package, run).await,
        }
    }
}

#[cfg(test)]
mod tests;
