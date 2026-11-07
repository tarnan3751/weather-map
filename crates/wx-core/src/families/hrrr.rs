//! NOAA/NCEP High-Resolution Rapid Refresh (HRRR), 3 km, over the contiguous
//! US and Alaska. Research: `docs/sources/noaa-regional.md` §3 (Alaska); the
//! CONUS layout is the one `wx-ingest` already uses.
//!
//! * **Where**: `https://noaa-hrrr-bdp-pds.s3.amazonaws.com/`, anonymous.
//! * **Layout**: `hrrr.<YYYYMMDD>/<region>/hrrr.t<HH>z.<product>f<FF>.grib2`
//!   for CONUS and `....<product>f<FF>.ak.grib2` for Alaska, each with an
//!   `.idx`. `wrfsfc` holds one forecast hour; `wrfsubh` holds four
//!   15-minute steps (`f01` is 15, 30, 45 and 60 minutes). Alaska files are
//!   90-106 MB, so fetch fields by `.idx` byte range. A full-day listing
//!   passes 1000 keys: list `hrrr.<day>/<region>/hrrr.t<HH>z.wrfsfcf`.
//! * **Runs**: CONUS every hour; Alaska every 3 hours (00, 03, ..., 21).
//!   Runs at 00, 06, 12 and 18 UTC reach 48 h, the others 18 h; `wrfsubh`
//!   stops at 18 h on every run.
//! * **Latency**: CONUS f00 about 54 minutes after the run. Alaska f00 46-53
//!   minutes, f18 79 minutes, f48 84 minutes.
//! * **Grids**: CONUS Lambert (3.30); Alaska polar stereographic (3.20,
//!   1299 x 919, 3 km, LoV 135 W). Winds are grid-relative on both. Alaska
//!   packing mixes 5.0 and 5.3.
//! * **Gotcha**: Alaska's `ASNOW` run total switches its `.idx` unit from
//!   hours to days at 24 h (`0-6 hour acc fcst`, then `0-2 day acc fcst` at
//!   f48); match the statistic, not the string.
//! * **Licence**: US government work, public domain. Credit "NOAA/NCEP HRRR".

use super::{
    AHEAD, IDENTITY, K_TO_C, NOAA, NOAA_URL, PAST, Product, US_PUBLIC_DOMAIN, input, run_total, sel,
};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

/// CONUS runs start every hour, Alaska runs every 3 hours.
pub const CONUS_RUN_EVERY_HOURS: u32 = 1;
pub const ALASKA_RUN_EVERY_HOURS: u32 = 3;
/// Every run reaches this far, in hourly steps.
pub const HORIZON_HOURS: u32 = 18;
/// Runs starting at these hours (UTC) reach [`LONG_HORIZON_HOURS`].
pub const LONG_RUN_HOURS: [u32; 4] = [0, 6, 12, 18];
pub const LONG_HORIZON_HOURS: u32 = 48;
/// `wrfsubh` steps and reach, on every run.
pub const SUBHOURLY_STEP_MINUTES: u32 = 15;
pub const SUBHOURLY_HORIZON_HOURS: u32 = 18;

pub const HRRR: SourceDef = SourceDef {
    id: "hrrr",
    family: Family::Hrrr,
    region: "conus",
    title: "HRRR",
    provider: NOAA,
    attribution: "NOAA/NCEP HRRR",
    attribution_url: NOAA_URL,
    licence: US_PUBLIC_DOMAIN,
    kind: FrameKind::Forecast,
    rank: 600,
    res_km: 3.0,
    max_zoom: 6,
};
pub const HRRR_AK: SourceDef = SourceDef {
    id: "hrrr_ak",
    region: "alaska",
    title: "HRRR Alaska",
    rank: 601,
    ..HRRR
};

pub const ROWS: &[SourceDef] = &[HRRR, HRRR_AK];

pub const SURFACE: &str = "wrfsfc";
pub const SUBHOURLY: &str = "wrfsubh";

const TMP_2M: FieldSel = sel("TMP", "2 m above ground");
const DPT_2M: FieldSel = sel("DPT", "2 m above ground");
const UGRD_10M: FieldSel = sel("UGRD", "10 m above ground");
const VGRD_10M: FieldSel = sel("VGRD", "10 m above ground");
const GUST_SFC: FieldSel = sel("GUST", "surface");
const REFC: FieldSel = sel("REFC", "entire atmosphere");
const TCDC: FieldSel = sel("TCDC", "entire atmosphere");
/// Instantaneous rate and categorical type flags, in [`Derive::PrecipTypeRate`] order.
const PRECIP_FIELDS: &[FieldSel] = &[
    sel("PRATE", "surface"),
    sel("CRAIN", "surface"),
    sel("CSNOW", "surface"),
    sel("CICEP", "surface"),
    sel("CFRZR", "surface"),
];
/// Snow depth in metres, accumulated since the run started.
const ASNOW: FieldSel = run_total("ASNOW", "surface");

/// The sub-hourly files, so simulated storms move instead of cross-fading
/// hour to hour.
pub const RADAR: InputSpec = input(
    Family::Hrrr,
    SUBHOURLY,
    &[REFC],
    Derive::Reflectivity,
    15,
    AHEAD,
);
/// The product these inputs read, and how long a part holds: its hour.
const MODEL: Product = Product {
    family: Family::Hrrr,
    product: SURFACE,
    hold_minutes: 60,
};

pub const PRECIP: InputSpec = MODEL.input(PRECIP_FIELDS, Derive::PrecipTypeRate, AHEAD);
pub const SNOW: InputSpec = MODEL.input(
    &[ASNOW],
    Derive::Linear {
        mul: 1000.0,
        add: 0.0,
    },
    AHEAD,
);
pub const TEMP: InputSpec = MODEL.input(&[TMP_2M], K_TO_C, PAST);
pub const FEELS_LIKE: InputSpec = MODEL.input(
    &[TMP_2M, DPT_2M, UGRD_10M, VGRD_10M],
    Derive::FeelsLike,
    PAST,
);
pub const DEW_POINT: InputSpec = MODEL.input(&[DPT_2M], K_TO_C, PAST);
pub const WIND: InputSpec = MODEL.input(&[UGRD_10M, VGRD_10M, GUST_SFC], Derive::Wind, PAST);
pub const CLOUDS: InputSpec = MODEL.input(&[TCDC], IDENTITY, AHEAD);
