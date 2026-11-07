//! GRIB2 bytes to [`Field`]s.
//!
//! The input is split into messages here rather than by grib-rs so that each
//! message gets its own `grib::Grib2` (they hold the reader in a `RefCell`, so
//! one cannot decode two submessages at once) and the wanted fields decode in
//! parallel. grib-rs reads the headers and unpacks the values; everything that
//! decides where a value is on the earth (scan order, grid geometry) is done
//! here against the raw template numbers.
//!
//! Every field is first described from its headers alone ([`Sub`]). The
//! `.idx`-style selection of [`decode`] and the provider-neutral [`scan`] and
//! [`decode_messages`] all work from that description.

use std::f64::consts::PI;
use std::io::Read;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};

use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use grib::def::grib2::template::param_set::{
    ComplexPacking, DateTime as GribDateTime, EarthShape, EnsembleForecast, FixedSurface,
    ForecastTime, Horizontal, LatLonGrid as GribLatLon, ScaledValue, Statistical,
};
use grib::def::grib2::template::{
    Template3_1, Template3_10, Template3_20, Template3_30, Template5_42,
};
use grib::def::grib2::{
    DataRepresentationTemplate, GridDefinitionTemplate, ProductDefinitionTemplate, Section3Payload,
    Section5Payload,
};
use grib::{Grib2Read, Grib2SubmessageDecoder, SubMessage, SubMessageSection};
use rayon::prelude::*;
use wx_core::layer::FieldSel;
use wx_core::proj::{
    EARTH_RADIUS_NCEP, Earth, LambertGrid, LatLonGrid, MercatorGrid, PolarStereoGrid,
    RotatedLatLonGrid,
};
use wx_core::{Field, GridProjection};

use crate::select::{Product, Resolved, TimeKind, describe};
use crate::{DecodedMessage, DecodedSet, HeadScan, MessageInfo, Surface, Timing};

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
/// Far beyond any NOAA file (a whole GFS output file is about 500 MB), small
/// enough that a corrupt or hostile stream cannot exhaust memory.
const MAX_INFLATED: u64 = 2 << 30;
/// Section 0 and the closing `7777`: no message is shorter.
const MIN_MESSAGE_LEN: u64 = 16 + 4;

/// Flag table 3.4 bits.
const SCAN_I_WESTWARD: u8 = 0x80;
const SCAN_J_NORTHWARD: u8 = 0x40;
const SCAN_J_CONSECUTIVE: u8 = 0x20;
const SCAN_ALTERNATING_ROWS: u8 = 0x10;
const SCAN_OFFSET_ROWS: u8 = 0x0f;
/// Flag table 3.3.
const RES_I_INCREMENT_GIVEN: u8 = 0x20;
const RES_J_INCREMENT_GIVEN: u8 = 0x10;
const RES_GRID_RELATIVE_WINDS: u8 = 0x08;
/// Code table 6.0.
const BITMAP_PRESENT: u8 = 0;
const BITMAP_NONE: u8 = 255;
/// Code table 4.5: missing, used for "no second surface".
const SURFACE_MISSING: u8 = 255;

pub(crate) fn decode(input: &[u8], wanted: &[FieldSel]) -> Result<DecodedSet> {
    let inflated;
    let bytes: &[u8] = if input.starts_with(&GZIP_MAGIC) {
        inflated = inflate(input)?;
        &inflated
    } else {
        input
    };
    let (ranges, scanned) = scan_all(bytes)?;
    let resolved: Vec<Resolved> = wanted.iter().map(Resolved::new).collect::<Result<_>>()?;
    let all: Vec<Candidate> = scanned
        .iter()
        .enumerate()
        .flat_map(|(message, subs)| subs.iter().map(move |sub| Candidate { message, sub }))
        .collect();

    let chosen: Vec<(Candidate, String)> = if resolved.is_empty() {
        ensure!(
            all.len() == 1,
            "with no fields selected the input must hold exactly one GRIB message, found {}",
            all.len()
        );
        vec![(all[0], "the only message".to_string())]
    } else {
        resolved
            .iter()
            .map(|r| Ok((choose(r, &all)?, describe(&r.sel))))
            .collect::<Result<_>>()?
    };

    let metas: Vec<Meta> = chosen
        .iter()
        .map(|(c, label)| {
            c.sub
                .meta()
                .map_err(|e| anyhow!("{label} (GRIB message {}): {e}", c.message + 1))
        })
        .collect::<Result<_>>()?;
    let valid_time = metas[0].valid;
    for ((_, label), m) in chosen.iter().zip(&metas).skip(1) {
        ensure!(
            m.valid == valid_time,
            "selected fields describe different times: {} is valid at {valid_time}, {label} at {}",
            chosen[0].1,
            m.valid
        );
    }

    let fields: Vec<Field> = chosen
        .par_iter()
        .zip(metas.par_iter())
        .map(|((c, label), meta)| {
            decode_field(&bytes[ranges[c.message].clone()], c.sub.index, meta.grid)
                .with_context(|| format!("decoding {label}"))
        })
        .collect::<Result<_>>()?;

    Ok(DecodedSet {
        fields,
        reference_time: metas[0].reference,
        valid_time,
    })
}

pub(crate) fn scan(input: &[u8]) -> Result<Vec<MessageInfo>> {
    refuse_gzip(input)?;
    let (ranges, scanned) = scan_all(input)?;
    let mut out = Vec::with_capacity(scanned.iter().map(Vec::len).sum());
    for (n, (r, subs)) in ranges.iter().zip(&scanned).enumerate() {
        for sub in subs {
            out.push(
                sub.info(r.clone())
                    .with_context(|| format!("GRIB message {} (byte {})", n + 1, r.start))?,
            );
        }
    }
    Ok(out)
}

pub(crate) fn decode_messages(input: &[u8], which: &[usize]) -> Result<Vec<DecodedMessage>> {
    refuse_gzip(input)?;
    let (ranges, scanned) = scan_all(input)?;
    let all: Vec<Candidate> = scanned
        .iter()
        .enumerate()
        .flat_map(|(message, subs)| subs.iter().map(move |sub| Candidate { message, sub }))
        .collect();
    which
        .par_iter()
        .map(|&k| {
            let c = all
                .get(k)
                .ok_or_else(|| anyhow!("no field {k}: the input holds {}", all.len()))?;
            let r = &ranges[c.message];
            let what = format!(
                "field {k} (GRIB message {}, byte {})",
                c.message + 1,
                r.start
            );
            let info = c.sub.info(r.clone()).with_context(|| what.clone())?;
            if let Some(why) = &info.unsupported {
                bail!("{what}: {why}");
            }
            let (Ok(time), Ok(grid)) = (&c.sub.timing, &c.sub.grid) else {
                bail!("{what}: time or grid unreadable");
            };
            let field = decode_field(&input[r.clone()], c.sub.index, grid)
                .with_context(|| format!("decoding {what}"))?;
            Ok(DecodedMessage {
                info,
                time: *time,
                field,
            })
        })
        .collect()
}

/// Describes the first field of a message from its first bytes: Sections 0
/// to 5 and the first 6 bytes of Section 6 are copied into a message of
/// their own, closed with an empty Section 7, and scanned like any other.
pub(crate) fn scan_head(head: &[u8]) -> Result<HeadScan> {
    if head.len() < 16 {
        return Ok(HeadScan::NeedBytes(16));
    }
    let length = message_length(head)?;
    let end = usize::try_from(length - 4).unwrap_or(usize::MAX);
    let mut at = 16;
    let indicator = loop {
        let need = |n: usize| Ok(HeadScan::NeedBytes(n.min(end + 4)));
        if at + 5 > head.len() {
            return need(at + 5);
        }
        let len = u32::from_be_bytes(head[at..at + 4].try_into().expect("4 bytes")) as usize;
        let num = head[at + 4];
        ensure!(
            (1..=7).contains(&num) && len >= 5 && len <= end.saturating_sub(at),
            "section {num} at byte {at} claims {len} bytes, {} remain in the message",
            end.saturating_sub(at)
        );
        ensure!(
            num != 7,
            "section 7 at byte {at} comes before any section 6"
        );
        if num == 6 {
            ensure!(
                len >= 6,
                "section 6 at byte {at} is too short for its bit-map indicator"
            );
            if at + 6 > head.len() {
                return need(at + 6);
            }
            break head[at + 5];
        }
        if at + len > head.len() {
            return need(at + len);
        }
        at += len;
    };
    let mut message = head[..at].to_vec();
    message.extend_from_slice(&[0, 0, 0, 6, 6, indicator, 0, 0, 0, 5, 7]);
    message.extend_from_slice(b"7777");
    let total = message.len() as u64;
    message[8..16].copy_from_slice(&total.to_be_bytes());
    let subs = scan_message(&message)?;
    let mut info = subs[0].info(0..usize::try_from(length).unwrap_or(usize::MAX))?;
    info.offset = 0;
    Ok(HeadScan::Described(Box::new(info)))
}

fn refuse_gzip(input: &[u8]) -> Result<()> {
    ensure!(
        !input.starts_with(&GZIP_MAGIC),
        "gzip input: inflate it first, so that offsets refer to the GRIB bytes"
    );
    Ok(())
}

/// Each message's byte range, and the description of each of its fields.
type Scanned = (Vec<Range<usize>>, Vec<Vec<Sub>>);

/// Splits `bytes` into messages and describes every field of each.
fn scan_all(bytes: &[u8]) -> Result<Scanned> {
    let ranges = split_messages(bytes)?;
    let scanned = ranges
        .par_iter()
        .enumerate()
        .map(|(n, r)| {
            scan_message(&bytes[r.clone()])
                .with_context(|| format!("GRIB message {} (byte {})", n + 1, r.start))
        })
        .collect::<Result<_>>()?;
    Ok((ranges, scanned))
}

/// One field's description, read without unpacking its values. Problems
/// are kept rather than raised, so a field nobody asked for cannot fail a
/// decode.
#[derive(Debug)]
struct Sub {
    /// Position within its message.
    index: usize,
    discipline: u8,
    category: u8,
    number: u8,
    product_template: u16,
    /// What `.idx`-style selectors match against: `None` unless the product
    /// template is 4.0 or 4.8, the two NOAA's inventories describe.
    product: Option<Product>,
    surface: Option<Surface>,
    second_surface: Option<Surface>,
    reference: std::result::Result<DateTime<Utc>, String>,
    timing: std::result::Result<Timing, String>,
    grid_template: u16,
    num_points: usize,
    grid: std::result::Result<GridInfo, String>,
    packing_template: u16,
    bitmap: bool,
    /// Whether the packing template is one that is unpacked; the packed data
    /// itself is checked only when it is (see [`check_packing`]).
    packing: std::result::Result<(), String>,
}

/// What `decode` needs of a chosen field.
struct Meta<'a> {
    reference: DateTime<Utc>,
    valid: DateTime<Utc>,
    grid: &'a GridInfo,
}

impl Sub {
    fn meta(&self) -> std::result::Result<Meta<'_>, String> {
        let reference = *self
            .reference
            .as_ref()
            .map_err(|e| format!("reference time: {e}"))?;
        let valid = self.timing.as_ref()?.valid();
        let grid = self.grid.as_ref()?;
        Ok(Meta {
            reference,
            valid,
            grid,
        })
    }

    /// Why the field cannot be decoded, if it cannot.
    fn problem(&self) -> Option<String> {
        [
            self.timing.as_ref().err(),
            self.grid.as_ref().err(),
            self.packing.as_ref().err(),
        ]
        .into_iter()
        .flatten()
        .next()
        .cloned()
    }

    fn info(&self, message: Range<usize>) -> Result<MessageInfo> {
        let reference = *self
            .reference
            .as_ref()
            .map_err(|e| anyhow!("reference time: {e}"))?;
        Ok(MessageInfo {
            offset: message.start,
            length: message.len(),
            field: self.index,
            discipline: self.discipline,
            category: self.category,
            number: self.number,
            product_template: self.product_template,
            surface: self.surface,
            second_surface: self.second_surface,
            reference_time: reference,
            time: self.timing.as_ref().ok().copied(),
            grid_template: self.grid_template,
            num_points: self.num_points,
            grid: self
                .grid
                .as_ref()
                .ok()
                .map(|g| (g.nx, g.ny, g.proj.clone())),
            packing_template: self.packing_template,
            bitmap: self.bitmap,
            unsupported: self.problem(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GridInfo {
    pub nx: usize,
    pub ny: usize,
    pub proj: GridProjection,
    pub grid_relative_winds: bool,
    /// The file scans rows east to west; they are reversed after decoding so
    /// that `i` always runs eastward.
    pub reverse_rows: bool,
    /// Adjacent rows scan in opposite directions (flag 0x10): every other
    /// row, counting from the second, is reversed after decoding as well.
    pub alternate_rows: bool,
}

#[derive(Debug, Clone, Copy)]
struct Candidate<'a> {
    message: usize,
    sub: &'a Sub,
}

fn choose<'a>(r: &Resolved, all: &[Candidate<'a>]) -> Result<Candidate<'a>> {
    let hits: Vec<Candidate> = all
        .iter()
        .copied()
        .filter(|c| c.sub.product.as_ref().is_some_and(|p| r.matches(p)))
        .collect();
    let Some(first) = hits.first().copied() else {
        bail!("no GRIB message holds {}", describe(&r.sel));
    };
    // A field repeated within one run, time and grid does occur in NOAA files
    // and is harmless; anything else means the caller passed too much.
    let same = |c: &Candidate| match (c.sub.meta(), first.sub.meta()) {
        (Ok(a), Ok(b)) => a.reference == b.reference && a.valid == b.valid && a.grid == b.grid,
        _ => false,
    };
    ensure!(
        hits[1..].iter().all(same),
        "{} GRIB messages hold {} from different runs, times or grids; pass only the one wanted",
        hits.len(),
        describe(&r.sel)
    );
    Ok(first)
}

fn inflate(gz: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(gz.len().saturating_mul(2));
    flate2::read::MultiGzDecoder::new(gz)
        .take(MAX_INFLATED + 1)
        .read_to_end(&mut out)
        .context("inflating gzip input")?;
    ensure!(
        out.len() as u64 <= MAX_INFLATED,
        "gzip input inflates beyond {MAX_INFLATED} bytes"
    );
    Ok(out)
}

/// The total length Section 0 states, from the first 16 bytes of a message.
pub(crate) fn message_length(head: &[u8]) -> Result<u64> {
    ensure!(
        head.len() >= 16 && head.starts_with(b"GRIB"),
        "not the start of a GRIB message (\"GRIB\" and 12 more bytes of Section 0)"
    );
    let edition = head[7];
    ensure!(
        edition == 2,
        "GRIB edition {edition} message: only GRIB2 is supported"
    );
    let len = u64::from_be_bytes(head[8..16].try_into().expect("8 bytes"));
    ensure!(
        len >= MIN_MESSAGE_LEN,
        "GRIB message claims {len} bytes, too few for Section 0 and 7777"
    );
    Ok(len)
}

/// Byte ranges of the GRIB2 messages in `b`, which must be nothing but
/// complete messages back to back.
pub(crate) fn split_messages(b: &[u8]) -> Result<Vec<Range<usize>>> {
    let mut out = Vec::new();
    let mut off = 0;
    while off < b.len() {
        let rest = &b[off..];
        let len = message_length(rest).with_context(|| format!("at byte {off} of {}", b.len()))?;
        ensure!(
            len <= rest.len() as u64,
            "GRIB message at byte {off} claims {len} bytes but {} remain (truncated input?)",
            rest.len()
        );
        let len = len as usize;
        ensure!(
            &rest[len - 4..len] == b"7777",
            "GRIB message at byte {off} does not end with 7777 (truncated or corrupt)"
        );
        out.push(off..off + len);
        off += len;
    }
    ensure!(!out.is_empty(), "input holds no GRIB messages");
    Ok(out)
}

/// Runs grib-rs code, and the decoders it calls, turning a panic on
/// malformed input into an error.
fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        Err(anyhow!(
            "GRIB decoder failed while {what} (malformed message?)"
        ))
    })
}

fn grib_err(e: grib::GribError) -> anyhow::Error {
    anyhow!("{e}")
}

/// Requires sections 1 to 7 to tile `message` exactly between section 0
/// and the closing `7777`. grib-rs allocates each section's body at the
/// length its header states before reading it, so a length running past the
/// message would otherwise be allocated in full (and one under 5 bytes
/// underflows its arithmetic).
fn check_sections(message: &[u8]) -> Result<()> {
    let end = message.len() - 4;
    let mut at = 16;
    while at < end {
        ensure!(
            at + 5 <= end,
            "section header at byte {at} runs into the end of the message"
        );
        let len = u32::from_be_bytes(message[at..at + 4].try_into().expect("4 bytes")) as usize;
        let num = message[at + 4];
        ensure!(
            (1..=7).contains(&num) && len >= 5 && len <= end - at,
            "section {num} at byte {at} claims {len} bytes, {} remain in the message",
            end - at
        );
        at += len;
    }
    Ok(())
}

fn scan_message(bytes: &[u8]) -> Result<Vec<Sub>> {
    check_sections(bytes)?;
    guarded("reading message headers", || {
        let g = grib::from_bytes(bytes).map_err(grib_err)?;
        let subs: Vec<Sub> = g
            .iter()
            .enumerate()
            .map(|(index, (_, sm))| describe_field(bytes, index, &sm))
            .collect::<Result<_>>()?;
        ensure!(!subs.is_empty(), "message holds no fields");
        Ok(subs)
    })
}

/// Big-endian integer of `N` bytes at `at`; `None` past the end.
fn be<const N: usize>(b: &[u8], at: usize) -> Option<u64> {
    let bytes = b.get(at..at + N)?;
    Some(bytes.iter().fold(0, |v, &x| (v << 8) | u64::from(x)))
}

/// Describes one field. The numbers every template has in the same place
/// (template numbers, parameter, point counts, bit-map indicator) are read
/// from the raw sections, so that a field whose templates grib-rs cannot
/// parse is still listed; the rest comes from grib-rs's parsed templates.
fn describe_field<R: Grib2Read>(message: &[u8], index: usize, sm: &SubMessage<R>) -> Result<Sub> {
    let s3 = section_bytes(message, &sm.3, 3)?;
    let s4 = section_bytes(message, &sm.4, 4)?;
    let s5 = section_bytes(message, &sm.5, 5)?;
    let s6 = section_bytes(message, &sm.6, 6)?;
    let short = |n: u8| anyhow!("section {n} is too short for its own header fields");
    let num_points = be::<4>(s3, 6).ok_or_else(|| short(3))? as usize;
    let grid_template = be::<2>(s3, 12).ok_or_else(|| short(3))? as u16;
    let product_template = be::<2>(s4, 7).ok_or_else(|| short(4))? as u16;
    let (category, number) = match s4.get(9..11) {
        Some(b) => (b[0], b[1]),
        None => return Err(short(4)),
    };
    let packing_template = be::<2>(s5, 9).ok_or_else(|| short(5))? as u16;
    let indicator = *s6.get(5).ok_or_else(|| short(6))?;
    let discipline = sm.indicator().discipline;

    let unsupported = |why: String| {
        anyhow!("product definition template 4.{product_template} is not supported ({why})")
    };
    let (surfaces, product, timing) = match sm.section4() {
        Ok(s4) => match read_product(discipline, &s4.payload.template) {
            Some((surfaces, product, timing)) => (Some(surfaces), product, Ok(timing)),
            None => (None, None, Err(unsupported("not read".into()))),
        },
        Err(e) => (None, None, Err(unsupported(e.to_string()))),
    };
    let reference = sm
        .section1()
        .map_err(grib_err)
        .and_then(|s1| utc(&s1.payload.ref_time))
        .map_err(|e| format!("{e:#}"));
    let timing = match (&reference, timing) {
        (Ok(r), Ok(t)) => t.resolve(*r).map_err(|e| format!("{e:#}")),
        (Err(e), _) => Err(format!("reference time: {e}")),
        (_, Err(e)) => Err(format!("{e:#}")),
    };
    let grid = sm
        .section3()
        .map_err(grib_err)
        .and_then(|s3| read_grid(&s3.payload))
        .map_err(|e| format!("{e:#}"));
    Ok(Sub {
        index,
        discipline,
        category,
        number,
        product_template,
        product,
        surface: surfaces.and_then(|s: Surfaces| s.0),
        second_surface: surfaces.and_then(|s| s.1),
        reference,
        timing,
        grid_template,
        num_points,
        grid,
        packing_template,
        bitmap: indicator != BITMAP_NONE,
        packing: packing_supported(packing_template, indicator),
    })
}

/// The packing templates and bit-map indicators [`check_packing`] accepts.
fn packing_supported(template: u16, bitmap: u8) -> std::result::Result<(), String> {
    if !matches!(template, 0 | 2 | 3 | 40 | 41 | 42) {
        return Err(format!(
            "data representation template 5.{template} is not supported (only 5.0, 5.2, 5.3, \
             5.40, 5.41 and 5.42)"
        ));
    }
    if !matches!(bitmap, BITMAP_PRESENT | BITMAP_NONE) {
        return Err(format!(
            "bit-map indicator {bitmap} is not supported (only a bit-map in the field's own \
             section 6, or none)"
        ));
    }
    Ok(())
}

/// A product's timing before the reference time is known.
enum RawTiming {
    /// Templates 4.0 to 4.6: valid at the reference time plus an offset.
    Offset { unit: u8, len: i64 },
    /// Templates 4.8 to 4.12: from the forecast time to the end of the
    /// processing interval.
    Interval {
        start_unit: u8,
        start: i64,
        end: GribDateTime,
        ranges: Vec<(u8, u32)>,
        process: Option<u8>,
    },
}

impl RawTiming {
    fn resolve(self, reference: DateTime<Utc>) -> Result<Timing> {
        let add = |t: DateTime<Utc>, d: TimeDelta| {
            t.checked_add_signed(d)
                .ok_or_else(|| anyhow!("valid time out of range"))
        };
        match self {
            RawTiming::Offset { unit, len } => {
                Ok(Timing::Instant(add(reference, span(unit, len)?)?))
            }
            RawTiming::Interval {
                start_unit,
                start,
                end,
                ranges,
                process,
            } => {
                let start = add(reference, span(start_unit, start)?)?;
                // The end of the interval is stated outright. Rebuilding it
                // from the offsets is only a fallback for a producer that
                // leaves it blank, because a multi-range interval's offsets
                // need not add up.
                let end = match utc(&end) {
                    Ok(end) => end,
                    Err(_) => {
                        let mut t = start;
                        for (unit, len) in ranges {
                            t = add(t, span(unit, len.into())?)?;
                        }
                        t
                    }
                };
                Ok(Timing::Interval {
                    start,
                    end,
                    process,
                })
            }
        }
    }
}

type Surfaces = (Option<Surface>, Option<Surface>);
/// Category, number, forecast time, surfaces and, for statistically
/// processed fields, the interval.
type ProductParts<'a> = (
    u8,
    u8,
    &'a ForecastTime,
    &'a Horizontal,
    Option<&'a Statistical>,
);

/// The parts of a product definition this crate reads: every template
/// grib-rs parses has a forecast time and two fixed surfaces in the same
/// layout, and those for statistically processed fields add an interval.
fn product_parts(t: &ProductDefinitionTemplate) -> Option<ProductParts<'_>> {
    use ProductDefinitionTemplate as P;
    Some(match t {
        P::_4_0(t) => (
            t.param.category,
            t.param.num,
            &t.forecast_time,
            &t.horizontal,
            None,
        ),
        P::_4_1(t) => (
            t.param.category,
            t.param.num,
            &t.forecast_time,
            &t.horizontal,
            None,
        ),
        P::_4_2(t) => (
            t.param.category,
            t.param.num,
            &t.forecast_time,
            &t.horizontal,
            None,
        ),
        P::_4_5(t) => (
            t.param.category,
            t.param.num,
            &t.forecast_time,
            &t.horizontal,
            None,
        ),
        P::_4_6(t) => (
            t.param.category,
            t.param.num,
            &t.forecast_time,
            &t.horizontal,
            None,
        ),
        P::_4_8(t) => (
            t.param.category,
            t.param.num,
            &t.forecast_time,
            &t.horizontal,
            Some(&t.time_interval),
        ),
        P::_4_9(t) => (
            t.param.category,
            t.param.num,
            &t.forecast_time,
            &t.horizontal,
            Some(&t.time_interval),
        ),
        P::_4_10(t) => (
            t.param.category,
            t.param.num,
            &t.forecast_time,
            &t.horizontal,
            Some(&t.time_interval),
        ),
        P::_4_11(t) => (
            t.param.category,
            t.param.num,
            &t.forecast_time,
            &t.horizontal,
            Some(&t.time_interval),
        ),
        P::_4_12(t) => (
            t.param.category,
            t.param.num,
            &t.forecast_time,
            &t.horizontal,
            Some(&t.time_interval),
        ),
        _ => return None,
    })
}

fn read_product(
    discipline: u8,
    t: &ProductDefinitionTemplate,
) -> Option<(Surfaces, Option<Product>, RawTiming)> {
    let (category, number, forecast, h, interval) = product_parts(t)?;
    let surface = |s: &FixedSurface| {
        (s.surface_type != SURFACE_MISSING).then(|| Surface {
            kind: s.surface_type,
            value: Some(surface_value(s)).filter(|v| !v.is_nan()),
        })
    };
    let surfaces = (surface(&h.first_surface), surface(&h.second_surface));
    let process = interval.and_then(|i| {
        (i.time_ranges.len() == 1).then(|| i.time_ranges[0].statistical_process_type)
    });
    // `.idx` selectors describe one forecast: templates 4.0 and 4.8, and
    // their ensemble forms 4.1 and 4.11 for a control member (code table
    // 4.6: 0 high, 1 low resolution), which is how DMI writes its
    // deterministic run (`ENS=hi-res ctl`). Perturbed members, ensemble
    // statistics and probabilities are never what a selector means.
    let window = interval.and_then(|i| match i.time_ranges.as_slice() {
        [r] => span(r.time_range.unit, r.time_range.len.into())
            .ok()
            .map(|d| d.num_seconds()),
        _ => None,
    });
    let statistical = TimeKind::Statistical {
        process,
        from_reference: forecast.time.len == 0,
        window,
    };
    let control = |e: &EnsembleForecast| e.ensemble_forecast_type <= 1;
    let time = match t {
        ProductDefinitionTemplate::_4_0(_) => Some(TimeKind::Instant),
        ProductDefinitionTemplate::_4_1(t) if control(&t.emsemble_forecast) => {
            Some(TimeKind::Instant)
        }
        ProductDefinitionTemplate::_4_8(_) => Some(statistical),
        ProductDefinitionTemplate::_4_11(t) if control(&t.emsemble_forecast) => Some(statistical),
        _ => None,
    };
    let product = time.map(|time| Product {
        discipline,
        category,
        number,
        surface: h.first_surface.surface_type,
        surface_value: surface_value(&h.first_surface),
        second_surface: h.second_surface.surface_type,
        time,
    });
    let timing = match interval {
        None => RawTiming::Offset {
            unit: forecast.time.unit,
            len: forecast.time.len.into(),
        },
        Some(i) => RawTiming::Interval {
            start_unit: forecast.time.unit,
            start: forecast.time.len.into(),
            end: i.end_of_overall_time_interval.clone(),
            ranges: i
                .time_ranges
                .iter()
                .map(|r| (r.time_range.unit, r.time_range.len))
                .collect(),
            process,
        },
    };
    Some((surfaces, product, timing))
}

fn surface_value(s: &FixedSurface) -> f64 {
    let v = &s.value;
    // grib-rs decodes GRIB's sign-and-magnitude integers, so "all bits set"
    // arrives as these.
    if v.scale_factor == -127 || v.scaled_value == -i32::MAX {
        f64::NAN
    } else {
        f64::from(v.scaled_value) * 10f64.powi(-i32::from(v.scale_factor))
    }
}

fn utc(t: &GribDateTime) -> Result<DateTime<Utc>> {
    Utc.with_ymd_and_hms(
        i32::from(t.year),
        u32::from(t.month),
        u32::from(t.day),
        u32::from(t.hour),
        u32::from(t.minute),
        u32::from(t.second),
    )
    .single()
    .ok_or_else(|| anyhow!("invalid date {t:?}"))
}

/// Code table 4.4 unit times a count.
fn span(unit: u8, len: i64) -> Result<TimeDelta> {
    let secs: i64 = match unit {
        0 => 60,
        1 => 3600,
        2 => 86_400,
        10 => 3 * 3600,
        11 => 6 * 3600,
        12 => 12 * 3600,
        13 => 1,
        other => bail!("unsupported time unit {other} (code table 4.4)"),
    };
    len.checked_mul(secs)
        .and_then(TimeDelta::try_seconds)
        .ok_or_else(|| anyhow!("time offset {len} in unit {unit} is out of range"))
}

fn read_grid(s3: &Section3Payload) -> Result<GridInfo> {
    ensure!(
        s3.grid_def_source == 0,
        "grid definition source {} is not supported (only explicit templates)",
        s3.grid_def_source
    );
    ensure!(
        s3.num_point_list_octets == 0,
        "quasi-regular grids (with a list of points per row) are not supported"
    );
    let num_points = s3.num_points as usize;
    match &s3.template {
        GridDefinitionTemplate::_3_0(t) => latlon_grid(&t.lat_lon, num_points),
        GridDefinitionTemplate::_3_1(t) => rotated_grid(t, num_points),
        GridDefinitionTemplate::_3_10(t) => mercator_grid(t, num_points),
        GridDefinitionTemplate::_3_20(t) => polar_stereo_grid(t, num_points),
        GridDefinitionTemplate::_3_30(t) => lambert_grid(t, num_points),
        _ => bail!(
            "grid definition template 3.{} is not supported (only 3.0, 3.1, 3.10, 3.20 and 3.30)",
            s3.template_num
        ),
    }
}

fn check_scan(scan: u8) -> Result<()> {
    ensure!(
        scan & SCAN_J_CONSECUTIVE == 0,
        "scanning mode 0x{scan:02x}: column-major point order is not supported"
    );
    ensure!(
        scan & SCAN_OFFSET_ROWS == 0,
        "scanning mode 0x{scan:02x}: staggered or shortened rows are not supported"
    );
    Ok(())
}

fn check_shape(ni: u32, nj: u32, num_points: usize) -> Result<(usize, usize)> {
    let (nx, ny) = (ni as usize, nj as usize);
    ensure!(
        nx >= 2 && ny >= 2 && nx.checked_mul(ny) == Some(num_points),
        "grid of {nx} x {ny} points does not match the {num_points} points declared"
    );
    ensure!(
        num_points <= crate::MAX_GRID_POINTS,
        "grid of {nx} x {ny} points is larger than any feed uses"
    );
    Ok((nx, ny))
}

/// Template 3.0's grid, also the rotated grid of template 3.1 (in rotated
/// coordinates). Angles are in the units [`angle_unit`] gives.
fn latlon_grid(t: &GribLatLon, num_points: usize) -> Result<GridInfo> {
    let g = &t.grid;
    let (nx, ny) = check_shape(g.ni, g.nj, num_points)?;
    let scan = t.scanning_mode.0;
    check_scan(scan)?;
    let unit = angle_unit(t)?;
    let lat1 = f64::from(g.first_point_lat) * unit;
    let lon1 = f64::from(g.first_point_lon) * unit;
    let lat2 = f64::from(g.last_point_lat) * unit;
    let lon2 = f64::from(g.last_point_lon) * unit;
    ensure!(
        lat1.abs() <= 90.0 + 1e-9 && lat2.abs() <= 90.0 + 1e-9,
        "latitudes {lat1}, {lat2} out of range"
    );
    let westward = scan & SCAN_I_WESTWARD != 0;
    let northward = scan & SCAN_J_NORTHWARD != 0;
    let flags = g.resolution_and_component_flags.0;
    let lon_span = if westward { lon1 - lon2 } else { lon2 - lon1 }.rem_euclid(360.0);
    let di = if flags & RES_I_INCREMENT_GIVEN != 0 && t.i_direction_inc != u32::MAX {
        f64::from(t.i_direction_inc) * unit
    } else {
        lon_span / (nx - 1) as f64
    };
    let dj = if flags & RES_J_INCREMENT_GIVEN != 0 && t.j_direction_inc != u32::MAX {
        f64::from(t.j_direction_inc) * unit
    } else {
        (lat2 - lat1).abs() / (ny - 1) as f64
    };
    ensure!(
        di.is_finite() && dj.is_finite() && di > 0.0 && dj > 0.0,
        "grid increments {di}, {dj} are not usable"
    );
    let dlat = if northward { dj } else { -dj };
    // The last point is stated independently of the scan flags and the
    // increments; disagreeing means one of them is not what we think, and the
    // grid would land in the wrong place.
    let lat_end = lat1 + (ny - 1) as f64 * dlat;
    ensure!(
        (lat_end - lat2).abs() <= 0.5 * dj + 1e-6,
        "grid definition is inconsistent: scanning mode 0x{scan:02x} and increment {dj} put the \
         last row at latitude {lat_end}, the message says {lat2}"
    );
    let lon_gap = (lon_span - (nx - 1) as f64 * di).rem_euclid(360.0);
    ensure!(
        lon_gap.min(360.0 - lon_gap) <= 0.5 * di + 1e-6,
        "grid definition is inconsistent: scanning mode 0x{scan:02x} and increment {di} do not \
         reach the last longitude {lon2}"
    );
    let periodic = (nx as f64 * di - 360.0).abs() <= 0.5 * di;
    let lon0 = if westward {
        lon1 - (nx - 1) as f64 * di
    } else {
        lon1
    };
    Ok(GridInfo {
        nx,
        ny,
        proj: GridProjection::LatLon(LatLonGrid {
            lon0,
            lat0: lat1,
            dlon: di,
            dlat,
            periodic,
        }),
        grid_relative_winds: flags & RES_GRID_RELATIVE_WINDS != 0,
        reverse_rows: westward,
        alternate_rows: scan & SCAN_ALTERNATING_ROWS != 0,
    })
}

/// Degrees per unit of a lat/lon template's angles: millionths unless a
/// basic angle and its subdivisions are given.
fn angle_unit(t: &GribLatLon) -> Result<f64> {
    let g = &t.grid;
    if g.initial_production_domain_basic_angle == 0
        || g.initial_production_domain_basic_angle == u32::MAX
    {
        return Ok(1e-6);
    }
    let sub = g.basic_angle_subdivisions;
    ensure!(
        sub != 0 && sub != u32::MAX,
        "basic angle subdivisions missing"
    );
    Ok(f64::from(g.initial_production_domain_basic_angle) / f64::from(sub))
}

/// Template 3.1: a lat/lon grid in coordinates whose southern pole has been
/// moved to the stated point.
fn rotated_grid(t: &Template3_1, num_points: usize) -> Result<GridInfo> {
    let mut info = latlon_grid(&t.lat_lon, num_points)?;
    let GridProjection::LatLon(grid) = info.proj else {
        unreachable!("latlon_grid makes lat/lon grids");
    };
    let unit = angle_unit(&t.lat_lon)?;
    let (lat, lon) = (
        f64::from(t.rotation.south_pole_lat) * unit,
        f64::from(t.rotation.south_pole_lon) * unit,
    );
    ensure!(
        lat.abs() <= 90.0,
        "southern pole latitude {lat} is out of range"
    );
    let angle = t.rotation.rot_angle;
    ensure!(
        angle == 0.0,
        "rotated grid with an angle of rotation of {angle} degrees is not supported"
    );
    info.proj = GridProjection::RotatedLatLon(RotatedLatLonGrid::new(lat, lon, grid));
    Ok(info)
}

fn scaled(s: &ScaledValue<u8, u32>) -> Option<f64> {
    (s.scale_factor != u8::MAX && s.scaled_value != u32::MAX)
        .then(|| f64::from(s.scaled_value) / 10f64.powi(i32::from(s.scale_factor)))
}

/// The figure of the earth (code table 3.2).
fn earth_shape(e: &EarthShape) -> Result<Earth> {
    let axes = |unit: f64| -> Result<Earth> {
        let (a, b) = (scaled(&e.major_axis), scaled(&e.minor_axis));
        let (Some(a), Some(b)) = (a, b) else {
            bail!("earth shape {} without its axes", e.shape);
        };
        Ok(Earth {
            a: a * unit,
            b: b * unit,
        })
    };
    let earth = match e.shape {
        0 => Earth::sphere(6_367_470.0),
        1 => Earth::sphere(
            scaled(&e.spherical_earth_radius)
                .ok_or_else(|| anyhow!("earth shape 1 without a radius"))?,
        ),
        2 => Earth::from_inverse_flattening(6_378_160.0, 297.0),
        3 => axes(1000.0)?,
        4 => Earth::GRS80,
        5 => Earth::WGS84,
        6 => Earth::sphere(EARTH_RADIUS_NCEP),
        7 => axes(1.0)?,
        8 => Earth::sphere(6_371_200.0),
        9 => Earth {
            a: 6_377_563.396,
            b: 6_356_256.909,
        },
        other => bail!("earth shape {other} is not supported"),
    };
    ensure!(
        earth.is_plausible(),
        "earth of {} by {} m is not plausible",
        earth.a,
        earth.b
    );
    Ok(earth)
}

/// Whether the computed position of the last grid point agrees with the one
/// the template states. Boustrophedon rows end at the west of the last row
/// when there is an even number of rows, so either end is accepted there.
fn check_last_point(
    proj: &GridProjection,
    nx: usize,
    ny: usize,
    alternate: bool,
    lon: f64,
    lat: f64,
) -> Result<()> {
    let (i, j) = proj.lonlat_to_ij(lon, lat);
    let near = |ci: f64| (i - ci).abs() <= 0.5 && (j - (ny - 1) as f64).abs() <= 0.5;
    ensure!(
        near((nx - 1) as f64) || (alternate && ny.is_multiple_of(2) && near(0.0)),
        "grid definition is inconsistent: the last grid point ({lat}, {lon}) falls at index ({i:.2}, {j:.2}) \
         of a {nx} x {ny} grid"
    );
    Ok(())
}

/// What templates 3.10, 3.20 and 3.30 share: the grid's shape and a scan
/// that runs eastward.
struct Plane {
    nx: usize,
    ny: usize,
    scan: u8,
}

impl Plane {
    fn new(what: &str, ni: u32, nj: u32, scan: u8, num_points: usize) -> Result<Plane> {
        let (nx, ny) = check_shape(ni, nj, num_points)?;
        check_scan(scan)?;
        ensure!(
            scan & SCAN_I_WESTWARD == 0,
            "scanning mode 0x{scan:02x}: {what} grids scanned east to west are not supported"
        );
        Ok(Plane { nx, ny, scan })
    }

    /// Grid lengths given in millimetres, as metres; `dy` is negative when
    /// rows run southward.
    fn lengths(&self, di: u32, dj: u32) -> Result<(f64, f64)> {
        ensure!(
            di != 0 && dj != 0 && di != u32::MAX && dj != u32::MAX,
            "grid lengths missing"
        );
        let dy = f64::from(dj) * 1e-3;
        let dy = if self.scan & SCAN_J_NORTHWARD != 0 {
            dy
        } else {
            -dy
        };
        Ok((f64::from(di) * 1e-3, dy))
    }

    fn alternate(&self) -> bool {
        self.scan & SCAN_ALTERNATING_ROWS != 0
    }

    fn info(self, proj: GridProjection, flags: u8) -> GridInfo {
        GridInfo {
            nx: self.nx,
            ny: self.ny,
            proj,
            grid_relative_winds: flags & RES_GRID_RELATIVE_WINDS != 0,
            reverse_rows: false,
            alternate_rows: self.alternate(),
        }
    }
}

/// Template 3.10. The "orientation of the grid" is ignored: NBM writes
/// values like 200 and 295 for grids that are plain Mercator (their last
/// point is where plain Mercator puts it, which is checked).
fn mercator_grid(t: &Template3_10, num_points: usize) -> Result<GridInfo> {
    let plane = Plane::new("Mercator", t.ni, t.nj, t.scanning_mode.0, num_points)?;
    let earth = earth_shape(&t.earth_shape)?;
    let deg = |v: i32| f64::from(v) * 1e-6;
    let (lat1, lat2, lad) = (deg(t.first_point_lat), deg(t.last_point_lat), deg(t.lad));
    let (lon1, lon2) = (
        f64::from(t.first_point_lon) * 1e-6,
        f64::from(t.last_point_lon) * 1e-6,
    );
    for (name, v) in [("La1", lat1), ("La2", lat2), ("LaD", lad)] {
        ensure!(v.abs() < 90.0, "{name} = {v} is out of range");
    }
    let (dx, dy) = plane.lengths(t.di, t.dj)?;
    // The central meridian is free in Mercator; the middle of the grid puts
    // the projection's cut on the far side of the earth.
    let lon0 = lon1 + 0.5 * (lon2 - lon1).rem_euclid(360.0);
    let proj = GridProjection::Mercator(MercatorGrid::from_first_point(
        earth, lad, lon0, lat1, lon1, dx, dy,
    ));
    check_last_point(&proj, plane.nx, plane.ny, plane.alternate(), lon2, lat2)?;
    Ok(plane.info(proj, t.resolution_and_component_flags.0))
}

/// Template 3.20. Dx and Dy are true at LaD, which is the stereographic
/// projection's latitude of true scale, so they are plane distances.
fn polar_stereo_grid(t: &Template3_20, num_points: usize) -> Result<GridInfo> {
    let plane = Plane::new(
        "polar stereographic",
        t.ni,
        t.nj,
        t.scanning_mode.0,
        num_points,
    )?;
    let centre = t.projection_centre.0;
    ensure!(
        centre & 0x40 == 0,
        "projection centre flag 0x{centre:02x}: bipolar projections are not supported"
    );
    let south = centre & 0x80 != 0;
    let earth = earth_shape(&t.earth_shape)?;
    let deg = |v: i32| f64::from(v) * 1e-6;
    let (lat1, lad) = (deg(t.first_point_lat), deg(t.lad));
    let (lon1, lov) = (f64::from(t.first_point_lon) * 1e-6, deg(t.lov));
    ensure!(lat1.abs() <= 90.0, "La1 = {lat1} is out of range");
    let hemisphere = if south { -lad } else { lad };
    ensure!(
        hemisphere > 0.0 && hemisphere <= 90.0,
        "LaD = {lad} is not in the hemisphere of the projection's pole"
    );
    let (dx, dy) = plane.lengths(t.dx, t.dy)?;
    let proj = GridProjection::PolarStereographic(PolarStereoGrid::from_first_point(
        earth, south, lad, lov, lat1, lon1, dx, dy,
    ));
    Ok(plane.info(proj, t.resolution_and_component_flags.0))
}

fn lambert_cone(lat1: f64, lat2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    if (p1 - p2).abs() < 1e-10 {
        p1.sin()
    } else {
        (p1.cos() / p2.cos()).ln()
            / ((PI / 4.0 + p2 / 2.0).tan() / (PI / 4.0 + p1 / 2.0).tan()).ln()
    }
}

fn lambert_grid(t: &Template3_30, num_points: usize) -> Result<GridInfo> {
    let plane = Plane::new("Lambert", t.ni, t.nj, t.scanning_mode.0, num_points)?;
    let centre = t.projection_centre.0;
    ensure!(
        centre & 0x40 == 0,
        "projection centre flag 0x{centre:02x}: bipolar Lambert grids are not supported"
    );
    let earth = earth_shape(&t.earth_shape)?;
    ensure!(
        earth.is_sphere(),
        "earth shape {}: Lambert grids are supported on a sphere only",
        t.earth_shape.shape
    );
    let radius = earth.a;
    let deg = |v: i32| f64::from(v) * 1e-6;
    let lat1 = deg(t.first_point_lat);
    let lon1 = f64::from(t.first_point_lon) * 1e-6;
    let lov = f64::from(t.lov) * 1e-6;
    let (latin1, latin2, lad) = (deg(t.latin1), deg(t.latin2), deg(t.lad));
    for (name, v) in [
        ("La1", lat1),
        ("Latin1", latin1),
        ("Latin2", latin2),
        ("LaD", lad),
    ] {
        ensure!(v.abs() < 90.0, "{name} = {v} is out of range");
    }
    // A cone over the South Pole has its standard parallels in the south
    // and a negative cone constant; the flag must say the same.
    let n = lambert_cone(latin1, latin2);
    let south = centre & 0x80 != 0;
    ensure!(
        n.is_finite() && n.abs() > 1e-6 && (n < 0.0) == south,
        "standard parallels {latin1}, {latin2} do not define a cone over the {} pole (projection \
         centre flag 0x{centre:02x})",
        if south { "South" } else { "North" }
    );
    // Template 3.30 gives Dx and Dy at LaD, which would make them ground
    // lengths there; eccodes and wgrib2 take them as lengths on the
    // projection plane whatever LaD is (LaD only fixes where y is measured
    // from). The two agree only where the map scale is 1, on a standard
    // parallel, which is where every producer we know of puts LaD; anywhere
    // else what the producer meant is not knowable, so it is refused.
    ensure!(
        t.lad == t.latin1 || t.lad == t.latin2,
        "Lambert grid with LaD = {lad} off its standard parallels ({latin1}, {latin2}) is not \
         supported: template 3.30 gives Dx and Dy at LaD, eccodes and wgrib2 on the projection \
         plane, and the two differ there"
    );
    let (dx, dy) = plane.lengths(t.dx, t.dy)?;
    let proj = GridProjection::Lambert(LambertGrid::new(
        latin1, latin2, lov, lat1, lon1, dx, dy, radius,
    ));
    Ok(plane.info(proj, t.resolution_and_component_flags.0))
}

fn decode_field(message: &[u8], sub: usize, grid: &GridInfo) -> Result<Field> {
    Ok(Field {
        nx: grid.nx,
        ny: grid.ny,
        proj: grid.proj.clone(),
        values: decode_values(message, sub, grid)?,
        grid_relative_winds: grid.grid_relative_winds,
    })
}

fn decode_values(message: &[u8], sub: usize, grid: &GridInfo) -> Result<Vec<f32>> {
    let mut values = guarded("unpacking values", || {
        let g = grib::from_bytes(message).map_err(grib_err)?;
        let (_, sm) = g
            .iter()
            .nth(sub)
            .ok_or_else(|| anyhow!("submessage {sub} is missing"))?;
        let s5 = sm.section5().map_err(grib_err)?;
        let s6 = section_bytes(message, &sm.6, 6)?;
        let s7 = section_bytes(message, &sm.7, 7)?;
        check_packing(&s5.payload, s6, s7, grid.nx * grid.ny)?;
        let decoder = Grib2SubmessageDecoder::from(sm).map_err(grib_err)?;
        let values: Vec<f32> = decoder.dispatch().map_err(grib_err)?.collect();
        Ok(values)
    })?;
    ensure!(
        values.len() == grid.nx * grid.ny,
        "decoded {} values for a {} x {} grid",
        values.len(),
        grid.nx,
        grid.ny
    );
    if grid.reverse_rows || grid.alternate_rows {
        values
            .par_chunks_exact_mut(grid.nx)
            .enumerate()
            .for_each(|(j, row)| {
                if grid.reverse_rows != (grid.alternate_rows && j % 2 == 1) {
                    row.reverse();
                }
            });
    }
    Ok(values)
}

/// The raw bytes of one section of `message`, header included.
fn section_bytes<'m>(message: &'m [u8], s: &SubMessageSection, num: u8) -> Result<&'m [u8]> {
    let (offset, size) = (s.body.offset, s.body.size);
    let bytes = offset
        .checked_add(size)
        .and_then(|end| message.get(offset..end))
        .filter(|b| b.len() >= 5 && b[4] == num && s.body.num == num)
        .ok_or_else(|| anyhow!("section {num} is not where its header says"))?;
    Ok(bytes)
}

/// Refuses packed data whose unpacking would allocate more than the grid
/// needs. grib-rs sizes buffers from numbers its own checks leave open: a
/// complex-packing group of width zero is materialized at its stated length
/// (its check of the data size counts width times length, which is zero),
/// and a PNG is decoded into a buffer sized from the PNG's own header. An
/// allocation that fails aborts the process, so these must be bounded first.
///
/// What passes guarantees that nothing unpacked exceeds the encoded point
/// count, which is at most the grid's point count (`MAX_GRID_POINTS`):
/// the encoded count is checked against the bit-map; complex packing's group
/// lengths, every one scaled by its reference and increment and the last
/// one taken from the header, sum to no more than it; and a PNG holds exactly
/// that many pixels of exactly `num_bits` each.
fn check_packing(s5: &Section5Payload, s6: &[u8], s7: &[u8], num_points: usize) -> Result<()> {
    let encoded = s5.num_encoded_points as usize;
    ensure!(s6.len() >= 6, "bit-map section is truncated");
    match s6[5] {
        255 => ensure!(
            encoded == num_points,
            "{encoded} values are encoded for a grid of {num_points} points without a bit-map"
        ),
        0 => {
            let bitmap = &s6[6..];
            ensure!(
                bitmap.len() >= num_points.div_ceil(8),
                "bit-map is shorter than the grid"
            );
            let full = num_points / 8;
            let mut set: usize = bitmap[..full].iter().map(|b| b.count_ones() as usize).sum();
            if !num_points.is_multiple_of(8) {
                set += (bitmap[full] >> (8 - num_points % 8)).count_ones() as usize;
            }
            ensure!(
                set == encoded,
                "bit-map marks {set} points but {encoded} values are encoded"
            );
        }
        other => bail!("bit-map indicator {other} is not supported"),
    }
    let data = &s7[5..];
    match &s5.template {
        DataRepresentationTemplate::_5_0(_) => Ok(()),
        DataRepresentationTemplate::_5_2(t) => {
            check_groups(&t.complex, t.simple.num_bits, data, 0, encoded)
        }
        DataRepresentationTemplate::_5_3(t) => {
            let order = t.spatial_diff_order;
            let octets = t.num_extra_desc_octets;
            ensure!(
                matches!(order, 1 | 2) && (1..=4).contains(&octets),
                "spatial differencing of order {order} with {octets}-octet descriptors is not supported"
            );
            // The first values and the overall minimum come before the groups.
            let descriptors = (usize::from(order) + 1) * usize::from(octets);
            check_groups(&t.complex, t.simple.num_bits, data, descriptors, encoded)
        }
        // With no bits per value the image or stream is never decoded: the
        // field is the reference value throughout.
        DataRepresentationTemplate::_5_40(t) if t.simple.num_bits == 0 => Ok(()),
        DataRepresentationTemplate::_5_40(_) => check_j2k(data, encoded),
        DataRepresentationTemplate::_5_41(t) if t.simple.num_bits == 0 => Ok(()),
        DataRepresentationTemplate::_5_41(t) => check_png(data, t.simple.num_bits, encoded),
        DataRepresentationTemplate::_5_42(t) if t.simple.num_bits == 0 => Ok(()),
        DataRepresentationTemplate::_5_42(t) => check_ccsds(t, data),
        _ => bail!(
            "{}",
            packing_supported(s5.template_num, s6[5])
                .err()
                .unwrap_or_else(|| format!(
                    "data representation template 5.{} could not be read",
                    s5.template_num
                ))
        ),
    }
}

/// Sums complex packing's group lengths (code table 5.2 / 5.3 layout:
/// descriptors, then group references, widths and lengths, each padded to
/// whole octets) and requires them to fit in `encoded` points.
fn check_groups(
    c: &ComplexPacking,
    num_bits: u8,
    data: &[u8],
    descriptors: usize,
    encoded: usize,
) -> Result<()> {
    let groups = c.num_groups as usize;
    // Every group holds at least one point; this also bounds the work below.
    ensure!(
        groups >= 1 && groups <= encoded,
        "{groups} groups for {encoded} values"
    );
    ensure!(
        num_bits <= 31 && c.num_group_width_bits <= 32 && c.num_group_len_bits <= 32,
        "complex packing field widths out of range"
    );
    let octets = |bits: u8| (groups * usize::from(bits)).div_ceil(8);
    let start = descriptors + octets(num_bits) + octets(c.num_group_width_bits);
    let lengths = data
        .get(start..start + octets(c.num_group_len_bits))
        .ok_or_else(|| anyhow!("group lengths run past the end of the data section"))?;
    let (bits, scale, base) = (
        usize::from(c.num_group_len_bits),
        u64::from(c.group_len_inc),
        u64::from(c.group_len_ref),
    );
    let too_many = || anyhow!("group lengths describe more than the {encoded} values encoded");
    let mut total = u64::from(c.group_len_last);
    ensure!(total <= encoded as u64, too_many());
    // The stream holds a length for every group but the last, whose true
    // length is in section 5. Each term is below 2^41 and the running total
    // stays at or below `encoded`, so this cannot overflow.
    for k in 0..groups - 1 {
        total += read_bits(lengths, k * bits, bits) * scale + base;
        ensure!(total <= encoded as u64, too_many());
    }
    Ok(())
}

/// `n <= 32` bits at bit offset `start`, most significant first (GRIB's
/// packing order); bits past the end of `b` read as zero.
fn read_bits(b: &[u8], start: usize, n: usize) -> u64 {
    if n == 0 {
        return 0;
    }
    let first = start / 8;
    let window = match b.get(first..first + 8) {
        Some(w) => u64::from_be_bytes(w.try_into().expect("8 bytes")),
        None => (0..8).fold(0u64, |w, k| {
            (w << 8) | u64::from(*b.get(first + k).unwrap_or(&0))
        }),
    };
    (window >> (64 - start % 8 - n)) & ((1u64 << n) - 1)
}

/// The PNG's header must describe exactly `encoded` pixels of `num_bits <= 32`
/// bits each, so the buffer grib-rs decodes it into (rows of whole bytes,
/// sized from this header) is at most 4 bytes per value.
fn check_png(data: &[u8], num_bits: u8, encoded: usize) -> Result<()> {
    ensure!(
        num_bits <= 32,
        "{num_bits}-bit PNG values are not supported"
    );
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let ihdr = data
        .get(..29)
        .filter(|h| h[..8] == SIGNATURE && h[8..12] == 13u32.to_be_bytes() && &h[12..16] == b"IHDR")
        .ok_or_else(|| anyhow!("data section does not start with a PNG header"))?;
    let width = u32::from_be_bytes(ihdr[16..20].try_into().expect("4 bytes"));
    let height = u32::from_be_bytes(ihdr[20..24].try_into().expect("4 bytes"));
    let (depth, colour) = (ihdr[24], ihdr[25]);
    let channels = match colour {
        0 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        other => bail!("PNG colour type {other} is not supported"),
    };
    ensure!(
        u32::from(depth) * channels == u32::from(num_bits),
        "PNG of {channels} x {depth}-bit samples does not hold {num_bits}-bit values"
    );
    ensure!(
        u64::from(width) * u64::from(height) == encoded as u64,
        "PNG of {width} x {height} pixels does not hold the {encoded} values encoded"
    );
    Ok(())
}

/// JPEG 2000 codestream markers (ISO/IEC 15444-1, Annex A).
const J2K_SOC: u64 = 0xff4f;
const J2K_SIZ: u64 = 0xff51;
const J2K_COD: u64 = 0xff52;
const J2K_COC: u64 = 0xff53;
const J2K_SOT: u64 = 0xff90;
const J2K_SOD: u64 = 0xff93;
/// hayro-jpeg2000 decodes in f32, so beyond 24 bits integers are not even
/// representable. Up to 16 bits the decode is exact (every value of ECCC's
/// 9 to 16-bit fields matches eccodes); at 24 bits its wavelet sums pass
/// 2^24 and lose their last bit, which leaves ECCC's 24-bit fields
/// (precipitation amounts and rates, type codes) up to 3 packing steps off
/// at 3 to 15 % of points (a step is 1e-9 to 1e-5 of the field's unit: no
/// rule's threshold notices).
const J2K_MAX_PRECISION: u8 = 24;
/// Tiles may be no smaller than this on average (encoders for GRIB write a
/// single tile).
const J2K_MIN_TILE_POINTS: u64 = 4096;
/// Wavelet transform number in COD and COC (Table A.20): the reversible
/// 5/3 wavelet. The other, 9/7, is lossy and decodes to non-integers.
const J2K_REVERSIBLE: u8 = 1;

/// Refuses JPEG 2000 codestreams that hayro-jpeg2000 would size from
/// unchecked numbers, or that would not decode to one plane of `encoded`
/// integers. The decoder allocates the image plane from the SIZ marker, a
/// tile structure for every tile of the SIZ tile grid (present in the
/// codestream or not), and precinct and code-block structures for every
/// precinct of every tile, sized by the COD and COC markers of the main
/// header and of each tile-part header.
///
/// So the data must be a raw codestream (not a JP2 file) whose SIZ marker
/// describes one unsigned, unsubsampled component of at most 24 bits, on an
/// image area of exactly `encoded` points in at most one tile per 4096
/// points; and every COD and COC marker must use the reversible wavelet and
/// precincts no smaller than 4 x 4 in their sub-bands (exponents of at least
/// 2 at resolution 0 and 3 above it), so that no precinct or code-block
/// covers fewer than 16 points.
fn check_j2k(data: &[u8], encoded: usize) -> Result<()> {
    let truncated = || anyhow!("JPEG 2000 codestream is truncated");
    ensure!(
        be::<2>(data, 0) == Some(J2K_SOC) && be::<2>(data, 2) == Some(J2K_SIZ),
        "data section does not start with a JPEG 2000 codestream (SOC and SIZ markers)"
    );
    // Lsiz, Rsiz, eight 4-octet sizes and offsets, Csiz, then three octets
    // per component.
    let siz = data.get(4..4 + 41).ok_or_else(truncated)?;
    let lsiz = be::<2>(siz, 0).ok_or_else(truncated)?;
    let csiz = be::<2>(siz, 36).ok_or_else(truncated)?;
    ensure!(
        lsiz == 41 && csiz == 1,
        "JPEG 2000 image with {csiz} components (marker length {lsiz}) is not supported: GRIB \
         values are one component"
    );
    let at = |k: usize| be::<4>(siz, 4 + 4 * k).expect("within the SIZ marker");
    let (xsiz, ysiz, xo, yo) = (at(0), at(1), at(2), at(3));
    let (xt, yt, xto, yto) = (at(4), at(5), at(6), at(7));
    ensure!(xo < xsiz && yo < ysiz, "JPEG 2000 image area is empty");
    let (width, height) = (xsiz - xo, ysiz - yo);
    ensure!(
        width * height == encoded as u64,
        "JPEG 2000 image of {width} x {height} points does not hold the {encoded} values encoded"
    );
    ensure!(
        xt > 0 && yt > 0 && xto <= xo && yto <= yo && xto + xt > xo && yto + yt > yo,
        "JPEG 2000 tile grid ({xt} x {yt} from {xto}, {yto}) does not cover the image"
    );
    let tiles = (xsiz - xto).div_ceil(xt) * (ysiz - yto).div_ceil(yt);
    let max_tiles = (encoded as u64 / J2K_MIN_TILE_POINTS).max(1);
    ensure!(
        tiles <= max_tiles,
        "JPEG 2000 image in {tiles} tiles is not supported (at most {max_tiles} for {encoded} \
         points)"
    );
    let (ssiz, xr, yr) = (siz[38], siz[39], siz[40]);
    let precision = (ssiz & 0x7f) + 1;
    ensure!(
        ssiz & 0x80 == 0 && precision <= J2K_MAX_PRECISION,
        "JPEG 2000 samples of {precision} bits{} are not supported (unsigned, at most {J2K_MAX_PRECISION})",
        if ssiz & 0x80 != 0 { ", signed," } else { "" }
    );
    ensure!(
        xr == 1 && yr == 1,
        "subsampled JPEG 2000 components ({xr} x {yr}) are not supported"
    );

    // Marker segments of the main header, then of each tile-part header.
    let mut pos = 4 + 41;
    pos = check_j2k_header(data, pos, J2K_SOT)?;
    while be::<2>(data, pos) == Some(J2K_SOT) {
        let lsot = be::<2>(data, pos + 2).ok_or_else(truncated)?;
        let isot = be::<2>(data, pos + 4).ok_or_else(truncated)?;
        let psot = be::<4>(data, pos + 6).ok_or_else(truncated)? as usize;
        ensure!(
            lsot == 10 && isot < tiles,
            "JPEG 2000 tile-part at byte {pos} is malformed (length {lsot}, tile {isot} of {tiles})"
        );
        let end = check_j2k_header(data, pos + 12, J2K_SOD)?;
        if psot == 0 {
            // The last tile-part, running to the end of the codestream.
            break;
        }
        ensure!(
            psot >= end - pos && pos + psot <= data.len(),
            "JPEG 2000 tile-part at byte {pos} claims {psot} bytes, {} remain",
            data.len() - pos
        );
        pos += psot;
    }
    Ok(())
}

/// Walks marker segments from `pos` until the marker `until`, checking
/// every COD and COC on the way; returns the position just after `until`
/// (SOD) or at it (SOT).
fn check_j2k_header(data: &[u8], mut pos: usize, until: u64) -> Result<usize> {
    let truncated = |pos: usize| anyhow!("JPEG 2000 header is truncated at byte {pos}");
    loop {
        let marker = be::<2>(data, pos).ok_or_else(|| truncated(pos))?;
        if marker == until {
            return Ok(if until == J2K_SOD { pos + 2 } else { pos });
        }
        // Header marker segments, all with a length: SIZ to COM.
        ensure!(
            (0xff50..=0xff6f).contains(&marker) && marker != J2K_SIZ,
            "unexpected JPEG 2000 marker 0x{marker:04x} at byte {pos}"
        );
        let len = be::<2>(data, pos + 2).ok_or_else(|| truncated(pos))? as usize;
        let segment = data
            .get(pos + 4..pos + 2 + len)
            .filter(|_| len >= 2)
            .ok_or_else(|| truncated(pos))?;
        match marker {
            // Scod, progression order, layers, MCT, then the coding style.
            J2K_COD => check_j2k_coding(segment.get(5..), segment.first().copied())?,
            // Ccoc (one byte for one component), Scoc, then the coding style.
            J2K_COC => check_j2k_coding(segment.get(2..), segment.get(1).copied())?,
            _ => {}
        }
        pos += 2 + len;
    }
}

/// A COD or COC marker's coding style parameters: decomposition levels,
/// code-block size, code-block style, transform, then, when bit 0 of the
/// style octet is set, one precinct size per resolution level.
fn check_j2k_coding(parameters: Option<&[u8]>, style: Option<u8>) -> Result<()> {
    let malformed = || anyhow!("JPEG 2000 coding style marker is malformed");
    let p = parameters.filter(|p| p.len() >= 5).ok_or_else(malformed)?;
    let style = style.ok_or_else(malformed)?;
    let levels = usize::from(p[0]);
    ensure!(
        p[4] == J2K_REVERSIBLE,
        "JPEG 2000 with the irreversible (lossy, 9/7) wavelet is not supported"
    );
    if style & 1 != 0 {
        let sizes = p.get(5..5 + levels + 1).ok_or_else(malformed)?;
        for (r, &s) in sizes.iter().enumerate() {
            let min = if r == 0 { 2 } else { 3 };
            ensure!(
                s & 0x0f >= min && s >> 4 >= min,
                "JPEG 2000 precincts of 2^{} x 2^{} at resolution {r} are smaller than supported",
                s & 0x0f,
                s >> 4
            );
        }
    }
    Ok(())
}

/// Code table 5.42 note 3 (libaec's flags) bits.
const CCSDS_SIGNED: u8 = 0x01;
const CCSDS_3BYTE: u8 = 0x02;
const CCSDS_MSB: u8 = 0x04;
/// CCSDS 121.0-B-3 limits the reference sample interval to 4096 blocks.
const CCSDS_MAX_RSI: u16 = 4096;

/// Refuses CCSDS parameters that rust-aec rejects or that grib-rs would
/// read back wrongly. grib-rs sizes the output from `num_bits` (whole
/// octets per sample) before rust-aec checks anything, so that is bounded
/// here first: at most 32 bits, 4 octets per value. grib-rs then reads the
/// samples most significant octet first, 3 octets for 17 to 24 bits, so the
/// stream must say it was written that way; and GRIB's packed values are
/// unsigned.
fn check_ccsds(t: &Template5_42, data: &[u8]) -> Result<()> {
    let (bits, mask) = (t.simple.num_bits, t.mask);
    ensure!(bits <= 32, "{bits}-bit CCSDS values are not supported");
    ensure!(
        matches!(t.block_size, 8 | 16 | 32 | 64),
        "CCSDS block size {} is not supported (8, 16, 32 or 64)",
        t.block_size
    );
    ensure!(
        (1..=CCSDS_MAX_RSI).contains(&t.ref_sample_interval),
        "CCSDS reference sample interval {} is out of range",
        t.ref_sample_interval
    );
    ensure!(
        mask & CCSDS_MSB != 0 && mask & CCSDS_SIGNED == 0,
        "CCSDS options 0x{mask:02x} are not supported (samples must be unsigned and stored most \
         significant octet first)"
    );
    ensure!(
        !(17..=24).contains(&bits) || mask & CCSDS_3BYTE != 0,
        "CCSDS options 0x{mask:02x}: {bits}-bit samples in 4 octets are not supported"
    );
    ensure!(!data.is_empty(), "CCSDS data section is empty");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_spans() {
        assert_eq!(span(0, 15).unwrap(), TimeDelta::minutes(15));
        assert_eq!(span(1, 6).unwrap(), TimeDelta::hours(6));
        assert_eq!(span(2, 1).unwrap(), TimeDelta::days(1));
        assert_eq!(span(11, 2).unwrap(), TimeDelta::hours(12));
        assert!(span(3, 1).is_err(), "months have no fixed length");
        assert!(span(1, i64::MAX).is_err());
    }

    #[test]
    fn message_framing() {
        assert!(split_messages(b"").is_err());
        assert!(split_messages(b"GRIB").is_err());
        let mut m = b"GRIB\0\0\0\x02".to_vec();
        m.extend_from_slice(&24u64.to_be_bytes());
        m.extend_from_slice(b"7777");
        // Claims 24 bytes, holds 20.
        assert!(split_messages(&m).is_err());
        m.splice(16..16, *b"xxxx");
        assert_eq!(split_messages(&m).unwrap(), vec![0..24]);
        let mut two = m.clone();
        two.extend_from_slice(&m);
        assert_eq!(split_messages(&two).unwrap(), vec![0..24, 24..48]);
        two.push(0);
        assert!(split_messages(&two).is_err(), "trailing garbage");
        let mut grib1 = m.clone();
        grib1[7] = 1;
        assert!(format!("{:#}", split_messages(&grib1).unwrap_err()).contains("edition 1"));
        let mut bad_end = m;
        bad_end[23] = b'6';
        assert!(split_messages(&bad_end).is_err());
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/../../fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// ECCC's rotated grids (template 3.1). Their values are JPEG 2000,
    /// which cannot be unpacked yet, but the grid definition reads, and its
    /// corners land where pyproj's `ob_tran` (and, to its float rounding,
    /// eccodes) puts them.
    #[test]
    fn rotated_grid_definitions() {
        for (name, nx, ny, corners) in [
            (
                "hrdps_t12z_PT003H__PRATE_Sfc.grib2",
                2540,
                1290,
                [
                    (39.626_033_829_7, -133.629_519_519_4),
                    (47.876_458_657_8, -40.708_565_707_1),
                ],
            ),
            (
                "rdps_t12z_PT003H__PrecipRate_Sfc.grib2",
                1140,
                1045,
                [
                    (-3.771_776_324_9, -124.613_050_821_6),
                    (50.039_624_386_4, 32.107_236_853_0),
                ],
            ),
        ] {
            let subs = scan_message(&fixture(name)).unwrap();
            let grid = subs[0].grid.as_ref().unwrap();
            assert_eq!((grid.nx, grid.ny), (nx, ny), "{name}");
            assert!(
                grid.grid_relative_winds && !grid.reverse_rows && !grid.alternate_rows,
                "{name}"
            );
            assert!(
                matches!(grid.proj, GridProjection::RotatedLatLon(_)),
                "{name}"
            );
            for ((lat, lon), (i, j)) in corners
                .into_iter()
                .zip([(0.0, 0.0), ((nx - 1) as f64, (ny - 1) as f64)])
            {
                let (glon, glat) = grid.proj.ij_to_lonlat(i, j);
                assert!(
                    (glat - lat).abs() < 1e-8 && (glon - lon).abs() < 1e-8,
                    "{name} ({i}, {j}): {glon} {glat}"
                );
            }
        }
    }

    #[test]
    fn earth_shapes() {
        let shape = |shape: u8| EarthShape {
            shape,
            spherical_earth_radius: ScaledValue {
                scale_factor: 0,
                scaled_value: 6_371_200,
            },
            major_axis: ScaledValue {
                scale_factor: 1,
                scaled_value: 63_781_370,
            },
            minor_axis: ScaledValue {
                scale_factor: 1,
                scaled_value: 63_567_523,
            },
        };
        assert_eq!(earth_shape(&shape(1)).unwrap(), Earth::sphere(6_371_200.0));
        assert_eq!(earth_shape(&shape(5)).unwrap(), Earth::WGS84);
        assert_eq!(
            earth_shape(&shape(6)).unwrap(),
            Earth::sphere(EARTH_RADIUS_NCEP)
        );
        let e = earth_shape(&shape(7)).unwrap();
        assert!((e.a - 6_378_137.0).abs() < 1e-6 && (e.b - 6_356_752.3).abs() < 1e-6);
        assert!(
            earth_shape(&shape(3)).is_err(),
            "kilometres: 6378137 km is no earth"
        );
        assert!(earth_shape(&shape(10)).is_err());
    }

    #[test]
    fn scan_flags() {
        assert!(check_scan(0x00).is_ok());
        assert!(check_scan(0x40).is_ok());
        assert!(check_scan(0x80).is_ok());
        assert!(check_scan(0x50).is_ok(), "boustrophedon rows");
        assert!(check_scan(0x20).is_err());
        assert!(check_scan(0x01).is_err());
    }
}
