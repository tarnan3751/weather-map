//! One module per family of sources. Each module holds everything about its
//! family: the [`SourceDef`] rows (one per region or product), one
//! [`InputSpec`] per layer the family feeds, the ingest horizon and cadence,
//! and where the data lives, how it is laid out, its licence and what is
//! still unverified.
//!
//! [`crate::source::SOURCES`] and [`crate::layer::LAYERS`] are assembled from
//! these consts, so an adapter author edits only their family's module.
//!
//! Research notes for every family are in `docs/sources/*.md` (fetched
//! 2026-11-06); the user-facing catalogue is `docs/sources/README.md`.
//! Numbers in the module docs come from those notes unless a comment says
//! they are a judgement.
//!
//! # Field selectors
//!
//! A [`FieldSel`] names a field in the provider's own vocabulary, which each
//! module states. In short: NOAA, DMI and CPTEC use their inventory strings
//! (`var` and `level` are the fourth and fifth `:`-separated fields of an
//! `.idx`, `.index` or `.inv` line); ECCC the variable and level tokens of
//! the file name; ICON the variable directory; ECMWF the `param` and
//! `levtype` of the `.index`; AROME the ecCodes short name and the package;
//! the HDF5 families the dataset name, with `level` empty. Products that are
//! one field per file (MRMS, the radar composites) list no fields. Turning a
//! selector into whatever the decoder needs is the adapter's job.
//!
//! # What every adapter does
//!
//! * Fetches nothing for a family whose [`enabled`] is false.
//! * Fetches a model's steps only up to its module's horizon constants: the
//!   hourly part of each run, because a 3-hourly source ranked above an
//!   hourly one would hide the hourly steps and make playback stutter (GFS,
//!   at the bottom, is the exception). It makes one part per step at
//!   `run + lead`.
//! * Finds, per source, the input that applies with
//!   [`LayerSpec::input_for`](crate::layer::LayerSpec::input_for), since a
//!   family's inputs on one layer can be limited to some of its sources.
//! * Hands `derive` fields in the file's native units, with fill values as
//!   NaN. Radar composites map their own "covered, nothing seen" value to -99
//!   (no echo) and "outside coverage" to NaN first; rainfall composites map
//!   "covered, nothing seen" to 0.
//! * Builds [`Stat::StepPerHour`] fields itself, as that variant says.

use crate::layer::{Derive, FieldSel, InputSpec, Stat};
use crate::source::{Family, SourceDef};

pub mod arome;
pub mod cmorph;
pub mod cptec;
pub mod cwa;
pub mod dmi;
pub mod dpc;
pub mod dwd_radar;
pub mod gdps;
pub mod gfs;
pub mod gmgsi;
pub mod goes;
pub mod hrdps;
pub mod hrrr;
pub mod icon;
pub mod ifs;
pub mod jma;
pub mod mrms;
pub mod nbm;
pub mod opera;
pub mod rtma;

/// Every family with the rows its module lists. `SOURCES` must hold exactly
/// these rows (a test checks it).
pub static ROWS: &[(Family, &[SourceDef])] = &[
    (Family::Mrms, mrms::ROWS),
    (Family::Rtma, rtma::ROWS),
    (Family::Hrrr, hrrr::ROWS),
    (Family::Gfs, gfs::ROWS),
    (Family::Nbm, nbm::ROWS),
    (Family::Gdps, gdps::ROWS),
    (Family::Hrdps, hrdps::ROWS),
    (Family::Icon, icon::ROWS),
    (Family::Dmi, dmi::ROWS),
    (Family::Arome, arome::ROWS),
    (Family::Cptec, cptec::ROWS),
    (Family::Ifs, ifs::ROWS),
    (Family::Cmorph, cmorph::ROWS),
    (Family::Opera, opera::ROWS),
    (Family::Dpc, dpc::ROWS),
    (Family::Jma, jma::ROWS),
    (Family::Cwa, cwa::ROWS),
    (Family::DwdRadar, dwd_radar::ROWS),
    (Family::Gmgsi, gmgsi::ROWS),
    (Family::Goes, goes::ROWS),
];

/// Whether the ingest side should fetch this family at all. A family that is
/// defined but not ingested yet keeps its rows (so its inputs stay checked
/// and documented) and has this false.
pub const fn enabled(family: Family) -> bool {
    match family {
        Family::Mrms => mrms::ENABLED,
        Family::Rtma => rtma::ENABLED,
        Family::Hrrr => hrrr::ENABLED,
        Family::Gfs => gfs::ENABLED,
        Family::Nbm => nbm::ENABLED,
        Family::Gdps => gdps::ENABLED,
        Family::Hrdps => hrdps::ENABLED,
        Family::Icon => icon::ENABLED,
        Family::Dmi => dmi::ENABLED,
        Family::Arome => arome::ENABLED,
        Family::Cptec => cptec::ENABLED,
        Family::Ifs => ifs::ENABLED,
        Family::Cmorph => cmorph::ENABLED,
        Family::Opera => opera::ENABLED,
        Family::Dpc => dpc::ENABLED,
        Family::Jma => jma::ENABLED,
        Family::Cwa => cwa::ENABLED,
        Family::DwdRadar => dwd_radar::ENABLED,
        Family::Gmgsi => gmgsi::ENABLED,
        Family::Goes => goes::ENABLED,
    }
}

// ---------------------------------------------------------------------------
// Shared pieces
// ---------------------------------------------------------------------------

/// Licence of US federal government works (NOAA).
pub(crate) const US_PUBLIC_DOMAIN: &str = "Public domain (US government work)";
pub(crate) const CC_BY_4: &str = "CC BY 4.0";
pub(crate) const NOAA: &str = "NOAA";
pub(crate) const NOAA_URL: &str = "https://www.noaa.gov/";

/// Model inputs whose recent hours stand in for observations where there are
/// none (temperature, feels-like, dew point, wind).
pub(crate) const PAST: bool = true;
/// Model inputs that only ever appear ahead of the newest observation.
pub(crate) const AHEAD: bool = false;

pub(crate) const K_TO_C: Derive = Derive::Linear {
    mul: 1.0,
    add: -273.15,
};
pub(crate) const IDENTITY: Derive = Derive::Linear { mul: 1.0, add: 0.0 };
/// Cloud cover given as a fraction 0..1.
pub(crate) const FRACTION_TO_PERCENT: Derive = Derive::Linear {
    mul: 100.0,
    add: 0.0,
};

/// Snow depth from liquid-equivalent snowfall: a 10:1 snow-to-liquid ratio
/// (1 kg m-2 of water is 10 mm of snow). A fixed ratio is a judgement; real
/// ratios run from about 5:1 (wet) to 20:1 (dry, cold). HRRR's `ASNOW` uses
/// its own variable ratio, so the snow layer mixes the two.
pub(crate) const SWE_MM_TO_SNOW_MM: f32 = 10.0;
/// [`SWE_MM_TO_SNOW_MM`] for a field in kg m-2.
pub(crate) const SNOW_FROM_SWE: Derive = Derive::Linear {
    mul: SWE_MM_TO_SNOW_MM,
    add: 0.0,
};

/// The value at the valid time.
pub(crate) const fn sel(var: &'static str, level: &'static str) -> FieldSel {
    FieldSel {
        var,
        level,
        stat: Stat::Instant,
    }
}

/// Accumulated from the start of the run.
pub(crate) const fn run_total(var: &'static str, level: &'static str) -> FieldSel {
    FieldSel {
        var,
        level,
        stat: Stat::RunTotal,
    }
}

/// Accumulated over the step ending at the valid time, per hour.
pub(crate) const fn per_hour(var: &'static str, level: &'static str) -> FieldSel {
    FieldSel {
        var,
        level,
        stat: Stat::StepPerHour,
    }
}

/// Maximum over the output interval ending at the valid time.
pub(crate) const fn step_max(var: &'static str, level: &'static str) -> FieldSel {
    FieldSel {
        var,
        level,
        stat: Stat::StepMax,
    }
}

/// What the inputs of one family's product share: an input is then its
/// fields, its derive rule and whether it fills the past.
#[derive(Clone, Copy)]
pub(crate) struct Product {
    pub family: Family,
    pub product: &'static str,
    pub hold_minutes: u32,
}

impl Product {
    /// [`input`] of this product.
    pub(crate) const fn input(
        self,
        fields: &'static [FieldSel],
        derive: Derive,
        fill_past: bool,
    ) -> InputSpec {
        input(
            self.family,
            self.product,
            fields,
            derive,
            self.hold_minutes,
            fill_past,
        )
    }
}

/// An input that applies to every source of `family`; narrow it with
/// [`InputSpec::only`].
pub(crate) const fn input(
    family: Family,
    product: &'static str,
    fields: &'static [FieldSel],
    derive: Derive,
    hold_minutes: u32,
    fill_past: bool,
) -> InputSpec {
    InputSpec {
        family,
        sources: &[],
        product,
        fields,
        derive,
        hold_minutes,
        fill_past,
    }
}
