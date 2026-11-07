//! GRIB2 decoding, derived fields and tiling.
//!
//! Pure computation: bytes in, bytes out. Nothing here touches the network,
//! the disk or the clock, and every function is synchronous and CPU-bound, so
//! async callers run it on a blocking thread.
//!
//! The path of one part (one source's data for one layer at one time):
//!
//! ```text
//! GRIB2 bytes --decode--> Fields --derive--> Raster --FrameGrid::build--> FrameGrid
//!                                                                          |
//!                         bounds() / solid(): published with the part      |
//!                         to_bytes(): stored; render_stack(): tiles from a stack of parts
//! ```
//!
//! A tile is drawn from a stack of parts, top first (see [`render_stack`]):
//! each part counts in proportion to its coverage weight
//! ([`FrameGrid::coverage_at`]), which fades from 1 inside its data to 0 at
//! the data's edge, so parts blend across their seams instead of showing a
//! step.
//!
//! # Picking messages out of a provider's files
//!
//! NOAA's files, and those with wgrib2-style inventories (DMI's `.index`,
//! CPTEC's `.inv`), are read with [`decode`] and selectors in the
//! inventory's words (`TMP:2 m above ground`). For every other provider,
//! [`scan`] lists each field of a buffer by its GRIB numbers (discipline,
//! category, number, surfaces), times, grid and packing, and
//! [`decode_messages`] decodes the ones the adapter picks. Four layouts
//! cover the providers.
//!
//! One variable at one time per file (ECCC's Datamart, most ICON files):
//!
//! ```no_run
//! # fn fetch(_: &str) -> Vec<u8> { unimplemented!() }
//! let bytes = fetch("20261106T12Z_MSC_HRDPS_TMP_AGL-2m_RLatLon0.0225_PT003H.grib2");
//! // With no selectors the file must hold exactly one field.
//! let set = wx_pipeline::decode(&bytes, &[])?;
//! assert_eq!(set.fields.len(), 1);
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! One variable at several lead times (ICON-D2's quarter-hours, the steps of
//! an AROME 0.025 degree package): pick by time, decode them all at once.
//! Run totals become the last hour's amount with [`step_per_hour`].
//!
//! ```no_run
//! # fn fetch(_: &str) -> Vec<u8> { unimplemented!() }
//! use wx_pipeline::{decode_messages, scan, step_per_hour};
//! let bytes = fetch("icon-d2_germany_regular-lat-lon_single-level_2026110612_006_2d_tot_prec.grib2");
//! let infos = scan(&bytes)?;
//! // Run totals ending at 18:00, 18:15, 18:30 and 18:45.
//! let mut by_time: Vec<usize> = (0..infos.len()).collect();
//! by_time.sort_by_key(|&k| infos[k].valid_time());
//! let totals = decode_messages(&bytes, &by_time)?;
//! for pair in totals.windows(2) {
//!     let hours = (pair[1].time.valid() - pair[0].time.valid()).as_seconds_f64() / 3600.0;
//!     let rate = step_per_hour(&pair[1].field, &pair[0].field, hours)?;
//!     # let _ = rate;
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! Byte ranges from the provider's own index (ECMWF's `.index` gives each
//! field's `_offset` and `_length`): fetch them, concatenate them in any
//! order, and pick by GRIB numbers.
//!
//! ```no_run
//! # fn fetch_range(_: std::ops::Range<u64>) -> Vec<u8> { unimplemented!() }
//! # fn index_entry(_: &str) -> (u64, u64) { unimplemented!() }
//! use wx_pipeline::{Timing, decode_messages, scan};
//! // `{"param": "2t", ..., "_offset": 85186067, "_length": 1365139}` and the
//! // line for `10fg`.
//! let mut bytes = Vec::new();
//! for param in ["2t", "10fg"] {
//!     let (offset, length) = index_entry(param);
//!     bytes.extend(fetch_range(offset..offset + length));
//! }
//! let infos = scan(&bytes)?;
//! let t2m = infos
//!     .iter()
//!     .position(|m| m.is(0, 0, 0) && m.at_level(103, Some(2.0)))
//!     .expect("2 m temperature");
//! let gust = infos
//!     .iter()
//!     .position(|m| {
//!         m.is(0, 2, 22) && matches!(m.time, Some(Timing::Interval { process: Some(2), .. }))
//!     })
//!     .expect("the gust maximum over the last hour");
//! let fields = decode_messages(&bytes, &[t2m, gust])?;
//! # let _ = fields;
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! A large package without an index (AROME): walk the remote file with
//! small range requests, reading each message's length from its first 16
//! bytes and what it holds from its first few hundred, then fetch only the
//! messages wanted.
//!
//! ```no_run
//! # fn fetch_range(_: std::ops::Range<u64>) -> Vec<u8> { unimplemented!() }
//! # let file_len = 0u64;
//! use wx_pipeline::{HeadScan, message_length, scan_head};
//! let mut at = 0;
//! let mut precipitation = Vec::new();
//! while at < file_len {
//!     let mut head = fetch_range(at..at + 512);
//!     let len = message_length(&head)?;
//!     let info = loop {
//!         match scan_head(&head)? {
//!             HeadScan::Described(info) => break info,
//!             HeadScan::NeedBytes(n) => head = fetch_range(at..at + n as u64),
//!         }
//!     };
//!     // Total precipitation since the run started, one message per step.
//!     if info.is(0, 1, 52) {
//!         precipitation.push((info.valid_time(), at..at + len));
//!     }
//!     at += len;
//! }
//! // Fetch those ranges, concatenate them, then `scan` and `decode_messages`.
//! # Ok::<(), anyhow::Error>(())
//! ```

mod coverage;
mod decode;
mod derive;
mod grid;
mod render;
mod select;
mod thermo;
mod wxg;

use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use wx_core::layer::FieldSel;
use wx_core::{Field, GridProjection, InputSpec, LayerSpec, LonLatBox, Raster, TileCoord};

/// Largest grid, in points per band, that is decoded or loaded: MRMS's 24.5
/// million with headroom. Buffers are sized from headers, and an allocation
/// that fails aborts the process instead of unwinding, so anything bigger is
/// refused before it is allocated. `decode`, `FrameGrid::build` and `.wxg`
/// share the limit so every grid that decodes can be stored and loaded again.
const MAX_GRID_POINTS: usize = 40_000_000;

/// The messages picked out of a GRIB2 file.
#[derive(Debug, Clone)]
pub struct DecodedSet {
    /// One field per requested selector, in request order.
    pub fields: Vec<Field>,
    /// Model run / analysis time from the first decoded message.
    pub reference_time: DateTime<Utc>,
    /// Time the data describes (reference time plus forecast offset; for
    /// accumulations, the end of the accumulation period).
    pub valid_time: DateTime<Utc>,
}

/// Decode the messages matching `wanted` from GRIB2 bytes.
///
/// `grib` holds one or more complete GRIB2 messages back to back: a whole
/// file, or just the byte ranges of the wanted fields concatenated in any
/// order. Messages are matched by what they contain (parameter and level),
/// not by position, and the result follows the order of `wanted`. It is an
/// error if a wanted field is absent.
///
/// With an empty `wanted` the input must contain exactly one message, which is
/// decoded whatever it is (MRMS files hold a single field with a local
/// parameter number).
///
/// A selector describes one forecast: a product of template 4.0 or 4.8, or
/// of their ensemble forms 4.1 and 4.11 for a control member, which is how
/// DMI writes its deterministic run (`ENS=hi-res ctl` in its `.index`).
/// Its statistic picks the time treatment: `Instant` a value at the valid
/// time; `RunTotal` an accumulation from the reference time; `StepPerHour`
/// an accumulation over exactly the hour before the valid time (NBM's
/// `5-6 hour acc fcst`, or `0-1 hour acc fcst` at step 1), whose amount is
/// then per hour as it stands (files with run totals only have no such
/// record: difference two with [`step_per_hour`]); `StepMax` a maximum over
/// whatever interval the file uses (`2-3 hour max fcst`). Variable names
/// are wgrib2's, including its `var discipline=0 center=94 local_table=1
/// parmcat=6 parm=199` for parameters it has no name for.
///
/// Grids: templates 3.0 (lat/lon), 3.1 (rotated lat/lon), 3.10 (Mercator),
/// 3.20 (polar stereographic) and 3.30 (Lambert conformal), scanned in any
/// direction, including boustrophedon rows (NBM's scanning mode 0x50).
/// Packing: templates 5.0, 5.2, 5.3, 5.40 (JPEG 2000, lossless), 5.41 (PNG)
/// and 5.42 (CCSDS), with or without a bit-map (points it leaves out decode
/// as NaN). JPEG 2000 images of more than 16 bits per value (ECCC's
/// precipitation fields use 24) come out up to a few packing steps off at a
/// few percent of points, a limit of the pure-Rust decoder, which works in
/// f32; everything else decodes to the value eccodes gives, to f32 rounding.
///
/// Gzip-compressed input (MRMS's `.grib2.gz`) is inflated first. It is also an
/// error if a selector matches several messages from different runs, times or
/// grids (a whole sub-hourly file holds four of each field), or if the
/// selected fields are not all valid at the same time.
///
/// Selectors are inventory names (NOAA's, DMI's, CPTEC's); for other
/// providers see [`scan`] and [`decode_messages`].
pub fn decode(grib: &[u8], wanted: &[FieldSel]) -> anyhow::Result<DecodedSet> {
    decode::decode(grib, wanted)
}

/// A fixed surface or level (GRIB2 code table 4.5) and its value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    /// Code table 4.5: 1 the ground or water surface, 100 an isobaric level
    /// (Pa), 101 mean sea level, 103 a height above ground (m), 106 a depth
    /// below land (m), 200 the entire atmosphere, and so on.
    pub kind: u8,
    /// In the table's unit; `None` when the message leaves it missing.
    pub value: Option<f64>,
}

/// When a field's values are valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    /// At one instant: product templates 4.0 to 4.6.
    Instant(DateTime<Utc>),
    /// Statistically processed over an interval: product templates 4.8 to
    /// 4.12. An accumulation since the run started has `start` equal to the
    /// reference time (ICON-D2's `0m-375m`); a gust maximum over the last
    /// hour starts an hour before `end` (ECMWF's `2-3`).
    Interval {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        /// Code table 4.10: 0 average, 1 accumulation, 2 maximum, 3 minimum,
        /// and so on; `None` when the interval is described by several time
        /// ranges.
        process: Option<u8>,
    },
}

impl Timing {
    /// The instant, or the end of the interval (what
    /// [`DecodedSet::valid_time`] reports).
    pub fn valid(&self) -> DateTime<Utc> {
        match *self {
            Timing::Instant(t) => t,
            Timing::Interval { end, .. } => end,
        }
    }
}

/// One field of a GRIB2 buffer, described from its headers without
/// unpacking its values (see [`scan`]).
#[derive(Debug, Clone, PartialEq)]
pub struct MessageInfo {
    /// Byte offset of the field's message (its Section 0) in the buffer.
    pub offset: usize,
    /// Length of the whole message in bytes, `7777` included.
    pub length: usize,
    /// Which field of its message this is: 0 unless the message repeats
    /// sections to hold several (some NOAA files do).
    pub field: usize,
    /// Code table 0.0: 0 meteorological, 1 hydrological, 2 land surface,
    /// 10 oceanographic, 209 MRMS's local products.
    pub discipline: u8,
    /// Parameter category and number within the discipline (code tables 4.1
    /// and 4.2, or the centre's local entries from 192 up): 0.0.0 is
    /// temperature, 0.1.52 total precipitation rate, 0.2.2 the u wind.
    pub category: u8,
    pub number: u8,
    /// Product definition template number (4.N).
    pub product_template: u16,
    /// The first and second fixed surfaces; `None` when the template's is
    /// missing (255), or the product template is not one that is read.
    pub surface: Option<Surface>,
    pub second_surface: Option<Surface>,
    /// Model run or analysis time (Section 1).
    pub reference_time: DateTime<Utc>,
    /// `None` when the product template is not one that is read (4.0 to
    /// 4.12, as far as grib-rs parses them) or its time unit is not fixed
    /// (months).
    pub time: Option<Timing>,
    /// Grid definition template number (3.N) and the number of grid points
    /// Section 3 declares.
    pub grid_template: u16,
    pub num_points: usize,
    /// Columns, rows and where they lie, as [`decode_messages`] would give
    /// them; `None` when the grid is not one that is decoded.
    pub grid: Option<(usize, usize, GridProjection)>,
    /// Data representation template number (5.N).
    pub packing_template: u16,
    /// Section 6 holds a bit-map: points it leaves out decode as NaN.
    pub bitmap: bool,
    /// Why [`decode_messages`] would refuse this field (its product, grid or
    /// packing template is not supported); `None` if it would decode it. The
    /// packed data itself is checked only when it is decoded.
    pub unsupported: Option<String>,
}

impl MessageInfo {
    /// Whether the field is parameter `discipline.category.number`.
    pub fn is(&self, discipline: u8, category: u8, number: u8) -> bool {
        (self.discipline, self.category, self.number) == (discipline, category, number)
    }

    /// Whether the field is on one surface of type `kind` (code table 4.5),
    /// not a layer between two, at `value` (to a millionth of it; `None`
    /// takes any value, as for the ground, whose value producers write as 0
    /// or leave missing).
    pub fn at_level(&self, kind: u8, value: Option<f64>) -> bool {
        let Some(s) = self.surface else {
            return false;
        };
        let value_ok = match (value, s.value) {
            (None, _) => true,
            (Some(want), Some(v)) => (v - want).abs() <= 1e-6 * want.abs().max(1.0),
            (Some(_), None) => false,
        };
        s.kind == kind && value_ok && self.second_surface.is_none()
    }

    /// The time the data describes: the instant, or the end of the interval.
    pub fn valid_time(&self) -> Option<DateTime<Utc>> {
        self.time.map(|t| t.valid())
    }
}

/// One field decoded by [`decode_messages`].
#[derive(Debug, Clone)]
pub struct DecodedMessage {
    /// Its description, as [`scan`] gives it.
    pub info: MessageInfo,
    /// Its time (always known for a field that decodes).
    pub time: Timing,
    pub field: Field,
}

/// What [`scan_head`] makes of the first bytes of a message.
#[derive(Debug, Clone, PartialEq)]
pub enum HeadScan {
    /// The message's first field. `offset` is 0 and `length` the whole
    /// message's.
    Described(Box<MessageInfo>),
    /// The headers run past the bytes given: this many from the start of
    /// the message are needed (more may be asked for again once they are
    /// there, when a later section turns out longer).
    NeedBytes(usize),
}

/// Every field in `grib`, described without unpacking any values: what
/// adapters for providers that do not use NOAA's names pick from.
///
/// `grib` holds complete GRIB2 messages back to back: a whole file, or
/// byte ranges taken from the provider's own index concatenated in any
/// order. Fields are listed in the order they appear, which is the index
/// [`decode_messages`] takes. Gzip input is refused (inflate it first, so
/// that the offsets refer to the GRIB bytes).
///
/// It is an error if the framing is broken (a message is truncated, its
/// sections do not add up, it is not GRIB2) or a reference time cannot be
/// read; anything else that would stop a field from decoding is reported in
/// its [`MessageInfo::unsupported`] instead, so one odd field does not hide
/// the rest of a package. Headers only are read, in parallel: a 90 MB ECMWF
/// file takes about a millisecond.
pub fn scan(grib: &[u8]) -> anyhow::Result<Vec<MessageInfo>> {
    decode::scan(grib)
}

/// Decode the fields of `grib` numbered `which` (positions in [`scan`]'s
/// list of the same bytes), in parallel, in `which` order.
///
/// Values are in the file's units, with the grid's rows ordered as for
/// [`decode`] (`i` runs east); points a bit-map leaves out are NaN. It is an
/// error if an index is out of range or names a field that is not supported
/// (see [`MessageInfo::unsupported`]), or if a chosen field's packed data
/// does not check out. Fields need not share a time or a grid: one call can
/// decode every lead time of a file.
pub fn decode_messages(grib: &[u8], which: &[usize]) -> anyhow::Result<Vec<DecodedMessage>> {
    decode::decode_messages(grib, which)
}

/// The total length of a GRIB2 message, from the first 16 bytes of its
/// Section 0 (`GRIB`, edition 2, then the length). With it, an adapter can
/// walk a remote file that has no index by fetching 16 bytes at each
/// message's start, and hop to the next.
///
/// It is an error if the bytes are not the start of a GRIB2 message.
pub fn message_length(head: &[u8]) -> anyhow::Result<u64> {
    decode::message_length(head)
}

/// Describe a message's first field from the first bytes of the message,
/// as [`scan`] would describe it: Sections 0 to 5 and the first 6 bytes of
/// Section 6 suffice (about 250 bytes for the lat/lon and Lambert grids
/// providers use, more when Section 2 holds local data). Providers' messages
/// hold one field each; for one that holds several, only the first is
/// described.
///
/// Returns [`HeadScan::NeedBytes`] when `head` stops short of that, with how
/// many bytes are needed as far as the sections in `head` tell. It is an
/// error if the bytes are not a GRIB2 message's start, or its sections do
/// not add up.
pub fn scan_head(head: &[u8]) -> anyhow::Result<HeadScan> {
    decode::scan_head(head)
}

/// A [`Stat::StepPerHour`](wx_core::layer::Stat::StepPerHour) field made
/// from two accumulations since the start of one run, for files that hold
/// no record of the last step alone (DMI, CPTEC, ICON's `tot_prec`,
/// AROME's `tp`): `(total - previous) / hours`, where `previous` is the run
/// total `hours` before `total`'s valid time.
///
/// A cell missing in either is missing; small negative differences (packing
/// noise) are passed on as they are, and the derive rules count negative
/// amounts as 0. It is an error if the two are not on the same grid or
/// `hours` is not positive. (A record of exactly the last hour, NBM's
/// `5-6 hour acc fcst`, is what [`decode`] gives for a `StepPerHour`
/// selector directly.)
pub fn step_per_hour(total: &Field, previous: &Field, hours: f64) -> anyhow::Result<Field> {
    derive::step_per_hour(total, previous, hours)
}

/// Turn one input's decoded fields into the layer's bands, in stored units,
/// following `input.derive` (see `wx_core::layer::Derive` for each rule).
/// `fields` are in `input.fields` order, or a single field for inputs with
/// no selectors.
pub fn derive(layer: &LayerSpec, input: &InputSpec, fields: &[Field]) -> anyhow::Result<Raster> {
    derive::derive(layer, input, fields)
}

/// One stack of parts, top first, rendered as one encoded WXT1 tile.
///
/// For every sample of the tile's lattice, part `k` contributes its value
/// `v_k` with effective weight `w_k = a_k * prod_{j < k} (1 - a_j)`, where
/// `a_k` is its coverage weight there (0 where it has no value), and the
/// sample is `sum(w_k v_k) / sum(w_k)`, normalised so the data does not fade
/// out at the outer edge of the lowest part: it ends. Each continuous band is
/// blended on its own; a categorical band takes the value of the part with
/// the largest `w_k` (the higher part on a tie).
///
/// A layer of one continuous and one categorical band (a precipitation
/// rate and its type) keeps the two consistent the way its derive rule does:
/// where the output rate is under `TYPED_RATE_MIN_MMH` the type is NONE, and
/// where it is at or above it but the type came out NONE, the type is that
/// of the nearest bilinear neighbour that has one (from the most-weighted
/// part offering one), so a light rate at the edge of a shower is drawn
/// rather than left typeless. At a part's own samples nothing changes.
///
/// Where the weights sum to 0 but some part has a value (the last weight
/// block inside the outer edge of the lowest part's data, where coverage
/// weights are already 0), the sample is the value of the topmost part that
/// has one, so the data reaches its last sample instead of stopping up to a
/// block short. Where no part has a value the sample is no data; `None` when
/// the whole tile is.
///
/// Every sample is a pure function of its longitude and latitude, the zoom
/// and the parts that have a value there (down to the first at full weight),
/// so tiles that share an edge agree on it bit for bit, also when their
/// stacks differ by parts that have no value on that edge (parts outside it)
/// or that lie below one at full weight there. A single part reproduces its
/// own samples exactly.
///
/// Parts may be quantized differently (they are converted to the first
/// part's scale and offset).
///
/// # Panics
///
/// If the parts do not all have the same number of bands of the same kinds:
/// they must be parts of one layer.
pub fn render_stack(parts: &[&FrameGrid], coord: TileCoord) -> Option<Vec<u8>> {
    render::render_stack(parts, coord).map(|t| wx_tile::encode(&t))
}

/// One part's data on its source grid, quantized, with reduced-resolution
/// overviews and its coverage weights, ready to be cut into tiles at any
/// zoom alone or stacked with others.
///
/// It is self-contained: `to_bytes` / `from_bytes` round-trip everything
/// needed to render tiles later, which is how close-up zoom levels are
/// produced only when someone asks for them.
#[derive(Debug, Clone)]
pub struct FrameGrid {
    /// Projection of the full-resolution level; coarser levels map through it.
    proj: GridProjection,
    periodic: bool,
    bands: Vec<grid::BandMeta>,
    /// Full resolution first, then each successive halving.
    levels: Vec<grid::Level>,
    bounds: LonLatBox,
    /// Any fixed longitude inside the grid, where pyramid levels are chosen.
    lon_ref: f64,
    /// Where each level's columns and rows sit, from the level sizes.
    axes: (grid::Axis, grid::Axis),
    coverage: coverage::Coverage,
    solid: OnceLock<Option<LonLatBox>>,
}

impl FrameGrid {
    /// Quantize `raster` with the layer's band specs and build the overviews.
    ///
    /// # Panics
    ///
    /// If `raster` does not hold one `nx * ny` plane per band of `layer`
    /// (what [`derive()`] returns always does), if it has more points than
    /// [`decode`] accepts (40 million), or if the layer has more bands than a
    /// tile can carry. An empty raster is fine: it renders no tiles and
    /// round-trips through [`FrameGrid::to_bytes`].
    pub fn build(layer: &LayerSpec, raster: &Raster) -> FrameGrid {
        assert_eq!(
            raster.bands.len(),
            layer.bands.len(),
            "{}: raster has {} bands, layer has {}",
            layer.id,
            raster.bands.len(),
            layer.bands.len()
        );
        assert!(
            (1..=wx_tile::MAX_BANDS).contains(&layer.bands.len()),
            "{}: a tile carries 1..={} bands",
            layer.id,
            wx_tile::MAX_BANDS
        );
        let n = raster.nx * raster.ny;
        assert!(
            n <= MAX_GRID_POINTS,
            "{}: {} x {} points is more than a stored grid may hold",
            layer.id,
            raster.nx,
            raster.ny
        );
        for (b, plane) in raster.bands.iter().enumerate() {
            assert_eq!(
                plane.len(),
                n,
                "{}: band {b} has the wrong length",
                layer.id
            );
        }
        let base = grid::Level {
            nx: raster.nx,
            ny: raster.ny,
            bands: layer
                .bands
                .iter()
                .zip(&raster.bands)
                .map(|(spec, values)| grid::quantize(spec, values))
                .collect(),
        };
        let bands: Vec<grid::BandMeta> = layer.bands.iter().map(grid::BandMeta::from).collect();
        FrameGrid::from_base(raster.proj.clone(), bands, base)
    }

    /// The overviews and the coverage map both read only level 0, so they
    /// are built side by side.
    fn from_base(proj: GridProjection, bands: Vec<grid::BandMeta>, base: grid::Level) -> FrameGrid {
        let (coverage, overviews) = rayon::join(
            || coverage::Coverage::build(&proj, &base),
            || grid::overviews(&base, &bands, proj.is_periodic()),
        );
        let mut levels = Vec::with_capacity(1 + overviews.len());
        levels.push(base);
        levels.extend(overviews);
        FrameGrid::assemble(proj, bands, levels, coverage)
    }

    fn assemble(
        proj: GridProjection,
        bands: Vec<grid::BandMeta>,
        levels: Vec<grid::Level>,
        coverage: coverage::Coverage,
    ) -> FrameGrid {
        let (nx, ny) = (levels[0].nx, levels[0].ny);
        let whole = proj.bounds(nx, ny);
        let bounds = coverage
            .data_bounds()
            .map_or(whole, |data| clamp_box(data, whole));
        let lon_ref = if nx > 0 && ny > 0 {
            proj.ij_to_lonlat((nx - 1) as f64 / 2.0, (ny - 1) as f64 / 2.0)
                .0
        } else {
            0.0
        };
        let axes = (
            grid::Axis::new(levels.iter().map(|l| l.nx)),
            grid::Axis::new(levels.iter().map(|l| l.ny)),
        );
        FrameGrid {
            periodic: proj.is_periodic(),
            proj,
            bands,
            levels,
            bounds,
            lon_ref,
            axes,
            coverage,
            solid: OnceLock::new(),
        }
    }

    /// Compact serialized form (the `.wxg` object).
    ///
    /// Only the full-resolution grid is stored, zstd-compressed; the
    /// overviews and coverage weights are rebuilt by [`FrameGrid::from_bytes`].
    pub fn to_bytes(&self) -> Vec<u8> {
        wxg::to_bytes(self, false, wxg::ZSTD_LEVEL)
    }

    /// Load a grid written by [`FrameGrid::to_bytes`]. Damaged bytes, and
    /// headers describing more than 40 million points per band, are errors;
    /// nothing is allocated before the header and chunk tables check out.
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<FrameGrid> {
        wxg::from_bytes(bytes)
    }

    /// Lon/lat box enclosing the grid's cells that hold data (each sample
    /// stands for the cell around it, so the box reaches half a sample beyond
    /// the outermost samples): the envelope of the coverage map's blocks that
    /// hold any value, rounded outward to whole blocks and then by half a
    /// block (a block is a third of the coverage ramp), or the whole
    /// grid's box when every block does. A grid whose cells over a region are
    /// masked (HRDPS over the United States, a cropped satellite disk) so
    /// advertises only where it has data. It crosses the antimeridian
    /// (`west > east`) when the data does and spans every longitude when the
    /// data holds a pole or goes round. Every point where the part has a
    /// value, so every point where [`FrameGrid::coverage_at`] is above 0, is
    /// inside it, and so is [`FrameGrid::solid`].
    ///
    /// Computed when the grid is built or loaded, never stored.
    pub fn bounds(&self) -> LonLatBox {
        self.bounds
    }

    /// A lon/lat box everywhere inside which the coverage weight is exactly 1,
    /// so nothing stacked below this part shows there; `None` when there is
    /// none at least a quarter of a degree on each side (patchy radar
    /// coverage). It is conservative with room to spare: rounded inward to
    /// 0.01 degree (`wx_core::frame::publish_boxes`) it still is.
    ///
    /// Found by a raster search on first call (milliseconds), then kept.
    pub fn solid(&self) -> Option<LonLatBox> {
        *self.solid.get_or_init(|| {
            let (nx, ny) = self.dimensions();
            self.coverage.solid(&self.proj, &self.bounds, nx, ny)
        })
    }

    /// Coverage weight at a point: 0 where the part has no data, rising
    /// smoothly to 1 about twelve source cells (at least 15 km) inside the
    /// edge of its data, be that the grid's edge or the edge of missing
    /// values within it.
    pub fn coverage_at(&self, lon: f64, lat: f64) -> f64 {
        let (i, j) = self.proj.lonlat_to_ij(lon, lat);
        if self.inside(i, j) {
            self.coverage.weight(i, j)
        } else {
            0.0
        }
    }

    /// Approximate heap size, for weighing cache entries.
    pub fn approx_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.coverage.approx_bytes()
            + self
                .levels
                .iter()
                .map(|l| l.bands.iter().map(|b| b.capacity() * 2).sum::<usize>())
                .sum::<usize>()
    }

    /// Tiles at zoom `z` that intersect the grid's bounds.
    pub fn covering(&self, z: u8) -> Vec<TileCoord> {
        if z > wx_core::proj::MAX_ZOOM || !self.has_data() {
            return Vec::new();
        }
        let b = self.bounds;
        b.tile_ranges(z)
            .into_iter()
            .flat_map(|(x0, y0, x1, y1)| {
                (y0..=y1).flat_map(move |y| (x0..=x1).filter_map(move |x| TileCoord::new(z, x, y)))
            })
            .filter(|t| t.bounds().intersects(&b))
            .collect()
    }

    /// Render one tile from this part alone: [`render_stack`] of one.
    pub fn render(&self, coord: TileCoord) -> Option<Vec<u8>> {
        render_stack(&[self], coord)
    }

    /// Samples per row and rows of the full-resolution grid.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.levels[0].nx, self.levels[0].ny)
    }

    /// Number of resolution levels, the full-resolution grid included.
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    pub fn projection(&self) -> &GridProjection {
        &self.proj
    }
}

/// `data` cut back to `whole` where it reaches beyond it (its growth by half
/// a block): latitudes always, longitudes when neither box wraps and `whole`
/// does not span them all. Every cell is inside `whole`, so nothing with
/// data is cut off.
fn clamp_box(data: LonLatBox, whole: LonLatBox) -> LonLatBox {
    let all_lons = whole.west == -180.0 && whole.east == 180.0;
    let (west, east) = if data.wraps() || whole.wraps() || all_lons {
        (data.west, data.east)
    } else {
        (data.west.max(whole.west), data.east.min(whole.east))
    };
    LonLatBox::new(
        west,
        data.south.max(whole.south),
        east,
        data.north.min(whole.north),
    )
}

#[cfg(test)]
mod tests;
