//! NOAA/NWS National Blend of Models (NBM): CONUS, Alaska, Hawaii, Puerto
//! Rico and Guam. The only sharp public forecast for Hawaii, Puerto Rico and
//! Guam. Research: `docs/sources/noaa-regional.md` §4.
//!
//! * **Where**: `https://noaa-nbm-grib2-pds.s3.amazonaws.com/`, anonymous
//!   (`noaa-nbm-pds` is an older-version bucket, not used).
//! * **Layout**: `blend.<YYYYMMDD>/<HH>/core/blend.t<HH>z.core.f<FFF>.<region>.grib2`
//!   with an `.idx`. Files carry ensemble statistics too (ak 73 MB, co
//!   165 MB), so fetch fields by byte range. Many `var:level` pairs repeat
//!   with a trailing qualifier (`ens std dev`, `prob >0.254`, `10% level`);
//!   the deterministic record is the one whose field after the time range is
//!   empty (`...:TMP:2 m above ground:1 hour fcst:`).
//! * **Runs**: published every hour, no f000. Hourly cycles step hourly
//!   f001-f049, then 3-hourly; the 00, 06, 12 and 18 UTC cycles (all four
//!   checked 2026-11-06) hourly f001-f048, then 3-hourly (f051 ... f099 and
//!   beyond), so a listing by the prefix `core.f0` holds 65 files per domain.
//!   Only those four are followed, see [`RUN_EVERY_HOURS`].
//! * **Latency**: the cycle is complete within a few minutes of its first
//!   file, which appears 45 minutes (17 UTC) to 69 minutes (12 UTC) after
//!   the cycle hour; 00 UTC 63, 06 UTC 59, 18 UTC 56-59.
//! * **Grids**: co Lambert (3.30), ak polar stereographic (3.20, the RTMA
//!   Alaska grid), hi, pr and gu Mercator (3.10; ignore the non-zero
//!   orientation octets). Every file uses scanning mode 0x50: alternate rows
//!   run in opposite directions, so odd rows must be reversed. Packing 5.3.
//! * **Fields**: no `UGRD`/`VGRD` (wind is speed `WIND` and from-direction
//!   `WDIR`, earth-relative); no `PRATE` (precipitation is `APCP` in 1-hour
//!   windows through f048: `0-1`, ..., `47-48 hour acc fcst`; f049 holds a
//!   6-hour window); no `REFC` (only `MAXREF` at 1000 m, an hourly maximum,
//!   not used); no type flags (only probabilities, ak and co); `APTMP` is
//!   missing in gu, so feels-like is computed from TMP, DPT and WIND for every
//!   region. `ASNOW` (ak, co) comes in 1-hour windows only, not run totals,
//!   so NBM is not on the anchored snow layer.
//! * **Coverage note**: the Alaska grid has values over British Columbia and
//!   Washington, so `nbm_co` ranks above `nbm_ak` and wins there.
//! * **Licence**: US government work, public domain. Credit "NOAA/NWS NBM".
//! * Not adopted: the `oc` domain (11 records, no surface weather).

use super::{
    AHEAD, IDENTITY, K_TO_C, NOAA, NOAA_URL, PAST, Product, US_PUBLIC_DOMAIN, per_hour, sel,
};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

/// Hours between the cycles that are followed (00, 06, 12, 18 UTC). NBM
/// publishes every hour, but each cycle costs about 860 MB of ranged
/// downloads for the 48 hourly steps of the five domains (CONUS 535 MB,
/// Alaska 271 MB, Hawaii 38 MB, Puerto Rico 10 MB, Guam 4 MB: eight fields
/// a step), and a newer run's parts replace an older run's at the same valid
/// time. Following every hour would download about 20 GB a day, every 3
/// hours about 7 GB, every 6 hours about 3.4 GB; blended guidance changes
/// little between cycles, so 6 is the choice. The synoptic cycles are the
/// ones documented to reach f048 hourly.
pub const RUN_EVERY_HOURS: u32 = 6;
/// Ingest horizon: the hourly part every cycle shares. f049 is hourly only
/// on the off-synoptic cycles and its `APCP` covers 6 hours, so it is left
/// out.
pub const HORIZON_HOURS: u32 = 48;

pub const NBM_CO: SourceDef = SourceDef {
    id: "nbm_co",
    family: Family::Nbm,
    region: "co",
    title: "NBM CONUS",
    provider: NOAA,
    attribution: "NOAA/NWS NBM",
    attribution_url: NOAA_URL,
    licence: US_PUBLIC_DOMAIN,
    kind: FrameKind::Forecast,
    rank: 590,
    res_km: 2.5,
    max_zoom: 6,
};
pub const NBM_AK: SourceDef = SourceDef {
    id: "nbm_ak",
    region: "ak",
    title: "NBM Alaska",
    rank: 580,
    res_km: 3.0,
    ..NBM_CO
};
pub const NBM_HI: SourceDef = SourceDef {
    id: "nbm_hi",
    region: "hi",
    title: "NBM Hawaii",
    rank: 650,
    ..NBM_CO
};
pub const NBM_PR: SourceDef = SourceDef {
    id: "nbm_pr",
    region: "pr",
    title: "NBM Puerto Rico",
    rank: 651,
    res_km: 1.25,
    max_zoom: 7,
    ..NBM_CO
};
pub const NBM_GU: SourceDef = SourceDef {
    id: "nbm_gu",
    region: "gu",
    title: "NBM Guam",
    rank: 652,
    ..NBM_CO
};

pub const ROWS: &[SourceDef] = &[NBM_CO, NBM_AK, NBM_HI, NBM_PR, NBM_GU];

pub const PRODUCT: &str = "core";

const TMP_2M: FieldSel = sel("TMP", "2 m above ground");
const DPT_2M: FieldSel = sel("DPT", "2 m above ground");
const WIND_10M: FieldSel = sel("WIND", "10 m above ground");
const WDIR_10M: FieldSel = sel("WDIR", "10 m above ground");
const GUST_10M: FieldSel = sel("GUST", "10 m above ground");
const TCDC_SFC: FieldSel = sel("TCDC", "surface");
/// kg m-2 in the 1-hour window ending at the valid time, which is already
/// the per-hour amount.
const APCP_1H: FieldSel = per_hour("APCP", "surface");

/// The product these inputs read, and how long a part holds: its hour.
const MODEL: Product = Product {
    family: Family::Nbm,
    product: PRODUCT,
    hold_minutes: 60,
};

pub const RADAR: InputSpec = MODEL.input(
    &[APCP_1H],
    Derive::ReflectivityFromRate { to_mmh: 1.0 },
    AHEAD,
);
pub const PRECIP: InputSpec = MODEL.input(
    &[APCP_1H, TMP_2M],
    Derive::PrecipTypeTemp { to_mmh: 1.0 },
    AHEAD,
);
pub const TEMP: InputSpec = MODEL.input(&[TMP_2M], K_TO_C, PAST);
pub const FEELS_LIKE: InputSpec =
    MODEL.input(&[TMP_2M, DPT_2M, WIND_10M], Derive::FeelsLikeSpeed, PAST);
pub const DEW_POINT: InputSpec = MODEL.input(&[DPT_2M], K_TO_C, PAST);
pub const WIND: InputSpec =
    MODEL.input(&[WIND_10M, WDIR_10M, GUST_10M], Derive::WindSpeedDir, PAST);
pub const CLOUDS: InputSpec = MODEL.input(&[TCDC_SFC], IDENTITY, AHEAD);
