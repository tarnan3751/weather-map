//! NOAA/NSSL Multi-Radar Multi-Sensor (MRMS) mosaics over the contiguous US,
//! Alaska, Hawaii, the Caribbean and Guam. Research: `docs/sources/noaa-regional.md` §1.
//!
//! * **Where**: `https://noaa-mrms-pds.s3.amazonaws.com/`, anonymous.
//! * **Layout**: `<region>/<product>/<YYYYMMDD>/MRMS_<product>_<YYYYMMDD>-<HHMMSS>.grib2.gz`:
//!   one gzipped GRIB2 message per file, no `.idx`, so [`InputSpec::fields`]
//!   is empty. Newest file: list `<region>/<product>/` with a delimiter for
//!   the day folders (ascending, 1000 keys a page; use `start-after`), then
//!   the last key of the newest day.
//! * **Grids**: template 3.0, first point north-west, longitudes 0..360,
//!   packing 5.41 (PNG). 0.01 degree for CONUS, Alaska and the Caribbean;
//!   0.005 degree for Hawaii and Guam. The QPE grids of Hawaii and Guam sit
//!   0.0005 degree (a tenth of a cell) off the reflectivity grids.
//! * **Cadence**: composite reflectivity every 2 minutes (the seconds drift),
//!   1-2 minutes late. 1 h QPE every 2 minutes on even minutes, 2-3 minutes
//!   late. 24 h and 72 h QPE hourly on the hour, 2-3 minutes late.
//! * **Values**: reflectivity -999 is no coverage, -99 no echo (both handled
//!   by [`Derive::Reflectivity`]); QPE is mm ending at the valid time, -3 no
//!   coverage.
//! * **Coverage notes**: Alaska stops at 176 W and is 63 % no-coverage; it
//!   overlaps CONUS over 50-55 N, 126-130 W.
//! * **Licence**: US government work, public domain. Credit "NOAA/NSSL MRMS".
//! * **UNVERIFIED**: Guam's QPE keys look sparser or start later than the
//!   other products (the first listing page ended at 2025-04).

use super::{NOAA, NOAA_URL, US_PUBLIC_DOMAIN, input};
use crate::layer::{Derive, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

/// Composite reflectivity cadence.
pub const RADAR_EVERY_MINUTES: u32 = 2;
/// 24 h and 72 h QPE cadence.
pub const LONG_QPE_EVERY_MINUTES: u32 = 60;

pub const MRMS: SourceDef = SourceDef {
    id: "mrms",
    family: Family::Mrms,
    region: "CONUS",
    title: "MRMS",
    provider: NOAA,
    attribution: "NOAA/NSSL MRMS",
    attribution_url: NOAA_URL,
    licence: US_PUBLIC_DOMAIN,
    kind: FrameKind::Observed,
    rank: 930,
    res_km: 1.0,
    max_zoom: 7,
};
pub const MRMS_AK: SourceDef = SourceDef {
    id: "mrms_ak",
    region: "ALASKA",
    title: "MRMS Alaska",
    rank: 931,
    ..MRMS
};
pub const MRMS_HI: SourceDef = SourceDef {
    id: "mrms_hi",
    region: "HAWAII",
    title: "MRMS Hawaii",
    rank: 932,
    res_km: 0.5,
    max_zoom: 8,
    ..MRMS
};
pub const MRMS_CARIB: SourceDef = SourceDef {
    id: "mrms_carib",
    region: "CARIB",
    title: "MRMS Caribbean",
    rank: 933,
    ..MRMS
};
pub const MRMS_GUAM: SourceDef = SourceDef {
    id: "mrms_guam",
    region: "GUAM",
    title: "MRMS Guam",
    rank: 934,
    res_km: 0.5,
    max_zoom: 8,
    ..MRMS
};

pub const ROWS: &[SourceDef] = &[MRMS, MRMS_AK, MRMS_HI, MRMS_CARIB, MRMS_GUAM];

/// Hold: twice the 2-minute cadence plus the 2-minute latency.
pub const RADAR: InputSpec = input(
    Family::Mrms,
    "MergedReflectivityQCComposite_00.50",
    &[],
    Derive::Reflectivity,
    6,
    false,
);
/// A running one-hour total every 2 minutes. Hold: two of the layer's
/// 10-minute ticks.
pub const RAIN_1H: InputSpec = input(
    Family::Mrms,
    "RadarOnly_QPE_01H_00.00",
    &[],
    Derive::PrecipDepth,
    20,
    false,
);
/// Hourly. Hold: twice the cadence plus the latency, so the newest total
/// stays on the half-hourly ticks other sources make.
pub const RAIN_24H: InputSpec = input(
    Family::Mrms,
    "RadarOnly_QPE_24H_00.00",
    &[],
    Derive::PrecipDepth,
    130,
    false,
);
/// As [`RAIN_24H`].
pub const RAIN_72H: InputSpec = input(
    Family::Mrms,
    "RadarOnly_QPE_72H_00.00",
    &[],
    Derive::PrecipDepth,
    130,
    false,
);
