//! Decoders for the products that are not GRIB: radar composites and
//! satellite imagery delivered as ODIM HDF5, NetCDF-4, GeoTIFF, palette PNG
//! tiles and JSON.
//!
//! Pure computation: every decoder takes a file's bytes and returns
//! [`Decoded`] fields (or, where the file carries no time, a bare [`Field`]).
//! Nothing here touches the network, the disk or the clock, and every
//! function is synchronous and CPU-bound and safe to call from many threads
//! at once: no decoder keeps state between calls.
//!
//! | Product | Decoder | Quantity |
//! |---|---|---|
//! | EUMETNET OPERA composite (ODIM HDF5) | [`opera::decode`] | DBZH dBZ, RATE mm/h, ACRR mm |
//! | DWD radar composites (ODIM HDF5, tar of nowcast steps) | [`dwd::decode`], [`dwd::decode_tar`] | hx/wn dBZ, ry mm/h, rv mm |
//! | NOAA CPC CMORPH2 rainfall (NetCDF-4) | [`cmorph::decode`] | mm/h |
//! | NOAA GMGSI global IR mosaic (NetCDF-4) | [`gmgsi::decode`] | K |
//! | GOES-R ABI L2 CMIP, emissive bands (NetCDF-4) | [`goes::decode`] | K |
//! | Italian DPC radar (GeoTIFF) | [`dpc::decode`] | SRI mm/h, VMI dBZ |
//! | JMA nowcast tiles (palette PNG) | [`jma::decode_tiles`] | mm/h |
//! | Taiwan CWA radar (JSON) | [`cwa::decode`] | dBZ |
//!
//! [`odim`], [`projdef`] and [`geotiff`] are the shared readers underneath,
//! usable for further ODIM composites and GeoTIFFs.
//!
//! # Conventions
//!
//! * Values are in the unit of their [`Quantity`], whatever the file stored
//!   (counts, scaled integers, sentinels).
//! * `NaN` means nothing observed the cell: outside radar coverage, off a
//!   satellite's disk, a fill value.
//! * A radar that looked and saw nothing is a real value, not `NaN`:
//!   [`NO_ECHO_DBZ`] for reflectivity, 0 for rates and accumulations. The
//!   layer pipeline treats anything at or below -10 dBZ as no echo.
//! * Rain rates are clamped to [`MAX_RAIN_RATE_MMH`] and accumulations to
//!   that rate times their period, because radar composites carry hail and
//!   clutter cores that convert to absurd rates (OPERA's RATE has 612 mm/h).
//! * `Field::proj` maps cell centres: sample `(i, j)` is the centre of the
//!   cell stored at `values[j * nx + i]`. Grids are converted to that
//!   convention from whatever the file states (ODIM outer corners, GeoTIFF
//!   tie points, NetCDF coordinate variables).
//! * No grid holds more than [`MAX_POINTS`] values; larger dimensions are
//!   refused before anything is allocated.
//!
//! # HDF5
//!
//! HDF5 and NetCDF-4 files are read with the pure-Rust `hdf5-reader` crate:
//! no C library, no build script, nothing to install, and no global state
//! (the C library serialises every call behind one lock). It reads every
//! sample these decoders were built from (superblocks 0, 1 and 2, contiguous
//! and chunked layouts, deflate with and without shuffle) value for value as
//! h5py does, bounds-checks every structure, and refuses allocations larger
//! than the file can justify.

mod cf;
pub mod cmorph;
pub mod cwa;
pub mod dpc;
pub mod dwd;
pub mod geotiff;
pub mod gmgsi;
pub mod goes;
mod hdf;
pub mod jma;
pub mod odim;
pub mod opera;
pub mod projdef;

use chrono::{DateTime, TimeDelta, Utc};
use wx_core::Field;

pub use wx_core;

/// Largest grid, in points, that any decoder produces: the same limit the
/// GRIB pipeline uses. The biggest product here, DWD's 250 m composite, has
/// 21 million; a GOES full disk 29 million.
pub const MAX_POINTS: usize = 40_000_000;

/// Reflectivity, dBZ, of a cell a radar observed and found empty.
pub const NO_ECHO_DBZ: f32 = -32.0;

/// Ceiling for rain rates, mm/h. At OPERA's Z-R relation (Z = 200 R^1.6) it
/// is 60 dBZ, beyond which radar returns are hail or clutter rather than
/// rain; in a 1 to 2 km, 5-minute composite, rates above it are artefacts.
pub const MAX_RAIN_RATE_MMH: f32 = 200.0;

/// What a field measures, which fixes the unit of its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quantity {
    /// Radar reflectivity, dBZ. [`NO_ECHO_DBZ`] where the radar saw nothing.
    Reflectivity,
    /// Liquid-equivalent precipitation rate, mm/h. 0 where it is dry.
    RainRate,
    /// Liquid-equivalent precipitation depth over [`Decoded::period`], mm.
    /// 0 where it stayed dry.
    Accumulation,
    /// Top-of-atmosphere brightness temperature, kelvin.
    BrightnessTemperature,
}

impl Quantity {
    pub const fn unit(&self) -> &'static str {
        match self {
            Quantity::Reflectivity => "dBZ",
            Quantity::RainRate => "mm/h",
            Quantity::Accumulation => "mm",
            Quantity::BrightnessTemperature => "K",
        }
    }
}

/// One decoded field and the times that place it on a timeline.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub field: Field,
    pub quantity: Quantity,
    /// The moment the values describe. For an accumulation or a mean over a
    /// period, the end of the period.
    pub valid: DateTime<Utc>,
    /// For a nowcast step, the time of the observation it was extrapolated
    /// from (`valid = run + lead`); a nowcast's own observation step (lead
    /// 0) carries it too. `None` for a plain observation.
    pub run: Option<DateTime<Utc>>,
    /// `valid - run` for a nowcast step; zero for observations.
    pub lead: TimeDelta,
    /// Length of the period ending at `valid` that the values sum or average
    /// over: an accumulation's window, a mean rate's averaging time. Zero for
    /// values at a moment.
    pub period: TimeDelta,
}

impl Decoded {
    fn observed(field: Field, quantity: Quantity, valid: DateTime<Utc>) -> Self {
        Decoded { field, quantity, valid, run: None, lead: TimeDelta::zero(), period: TimeDelta::zero() }
    }
}

/// `nx * ny`, refused when zero or above [`MAX_POINTS`].
pub(crate) fn checked_points(what: &str, nx: usize, ny: usize) -> anyhow::Result<usize> {
    let n = nx
        .checked_mul(ny)
        .filter(|&n| n > 0 && n <= MAX_POINTS)
        .ok_or_else(|| anyhow::anyhow!("{what}: {nx} x {ny} grid is empty or above the {MAX_POINTS} point limit"))?;
    Ok(n)
}

/// A rain rate in mm/h as stored: negative values (rounding below zero) are
/// dry, extremes are cut at [`MAX_RAIN_RATE_MMH`].
pub(crate) fn clamp_rate(v: f32) -> f32 {
    v.clamp(0.0, MAX_RAIN_RATE_MMH)
}
