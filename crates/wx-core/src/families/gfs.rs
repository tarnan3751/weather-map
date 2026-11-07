//! NOAA/NCEP Global Forecast System (GFS), 0.25 degree. The fallback global
//! base under GDPS, and the only global source of simulated reflectivity.
//! Research: `docs/sources/noaa-regional.md` §7.
//!
//! * **Where**: `https://noaa-gfs-bdp-pds.s3.amazonaws.com/`, anonymous.
//! * **Layout**: `gfs.<YYYYMMDD>/<HH>/atmos/gfs.t<HH>z.pgrb2.0p25.f<FFF>` with
//!   an `.idx`; files are 500-550 MB, so fetch fields by byte range.
//! * **Runs**: 00, 06, 12, 18 UTC; f000-f120 hourly, then 3-hourly to f384.
//! * **Latency**: the hourly part is out 3.5-4.5 hours after the run, the
//!   whole run about 5 h 15.
//! * **Grid**: template 3.0, 1440 x 721, first point 90 N 0 E, earth-relative
//!   winds, packing 5.3.
//! * **Gotchas**: match the full level string (`RH:2 mb` shares a prefix with
//!   `RH:2 m above ground`, `UGRD:10 mb` with `UGRD:10 m above ground`).
//!   `APCP:surface` appears twice; the run total is the one whose window
//!   starts at `0-`. `PRATE`, the categorical flags and `TCDC` also appear as
//!   `0-N hour ave fcst`; take the instantaneous record. pgrb2 has no
//!   `ASNOW`, so GFS is not on the snow layer.
//! * **Licence**: US government work, public domain. Credit "NOAA/NCEP GFS".

use super::{AHEAD, IDENTITY, K_TO_C, NOAA, NOAA_URL, PAST, Product, US_PUBLIC_DOMAIN, sel};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const RUN_EVERY_HOURS: u32 = 6;
/// Hourly steps reach this far.
pub const HOURLY_HORIZON_HOURS: u32 = 120;
/// Ingested this far, 3-hourly after [`HOURLY_HORIZON_HOURS`]: nothing ranks
/// below GFS, so its 3-hourly steps cannot make anything else stutter.
pub const HORIZON_HOURS: u32 = 168;

pub const GFS: SourceDef = SourceDef {
    id: "gfs",
    family: Family::Gfs,
    region: "",
    title: "GFS",
    provider: NOAA,
    attribution: "NOAA/NCEP GFS",
    attribution_url: NOAA_URL,
    licence: US_PUBLIC_DOMAIN,
    kind: FrameKind::Forecast,
    rank: 100,
    res_km: 27.0,
    max_zoom: 4,
};

pub const ROWS: &[SourceDef] = &[GFS];

pub const PRODUCT: &str = "pgrb2.0p25";

const TMP_2M: FieldSel = sel("TMP", "2 m above ground");
const DPT_2M: FieldSel = sel("DPT", "2 m above ground");
const UGRD_10M: FieldSel = sel("UGRD", "10 m above ground");
const VGRD_10M: FieldSel = sel("VGRD", "10 m above ground");
const GUST_SFC: FieldSel = sel("GUST", "surface");
/// No echo is -20 dBZ.
const REFC: FieldSel = sel("REFC", "entire atmosphere");
const TCDC: FieldSel = sel("TCDC", "entire atmosphere");
const PRECIP_FIELDS: &[FieldSel] = &[
    sel("PRATE", "surface"),
    sel("CRAIN", "surface"),
    sel("CSNOW", "surface"),
    sel("CICEP", "surface"),
    sel("CFRZR", "surface"),
];

/// The largest step ingested (3 hours, after 120 h).
const HOLD: u32 = 180;

/// The product these inputs read, and how long a part holds.
const MODEL: Product = Product {
    family: Family::Gfs,
    product: PRODUCT,
    hold_minutes: HOLD,
};

pub const RADAR: InputSpec = MODEL.input(&[REFC], Derive::Reflectivity, AHEAD);
pub const PRECIP: InputSpec = MODEL.input(PRECIP_FIELDS, Derive::PrecipTypeRate, AHEAD);
pub const TEMP: InputSpec = MODEL.input(&[TMP_2M], K_TO_C, PAST);
pub const FEELS_LIKE: InputSpec = MODEL.input(
    &[TMP_2M, DPT_2M, UGRD_10M, VGRD_10M],
    Derive::FeelsLike,
    PAST,
);
pub const DEW_POINT: InputSpec = MODEL.input(&[DPT_2M], K_TO_C, PAST);
pub const WIND: InputSpec = MODEL.input(&[UGRD_10M, VGRD_10M, GUST_SFC], Derive::Wind, PAST);
pub const CLOUDS: InputSpec = MODEL.input(&[TCDC], IDENTITY, AHEAD);
