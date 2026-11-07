//! ECCC High Resolution Deterministic Prediction System (HRDPS), 2.5 km,
//! continental domain (Canada and the northern US). Research:
//! `docs/sources/canada-and-world.md` §1.1; checked against the Datamart's
//! listings and files of 5-6 November 2026.
//!
//! * **Where**: MSC Datamart, `https://dd.weather.gc.ca/` (mirror
//!   `https://hpfx.collab.science.gc.ca/`), anonymous.
//! * **Layout**: one field and one step per file:
//!   `/<YYYYMMDD>/WXO-DD/model_hrdps/continental/2.5km/<HH>/<hhh>/<YYYYMMDD>T<HH>Z_MSC_HRDPS_<var>_<level>_RLatLon0.0225_PT<hhh>H.grib2`,
//!   `<var>` and `<level>` being the [`FieldSel`] `var` and `level` (`TMP`,
//!   `AGL-2m`). The leading date is the day of publication (UTC). About 410
//!   files per step, `HRDPS-WEonG_*` post-processed fields among them; a
//!   step's directory appears with a JSON of highs and lows minutes before
//!   its GRIB2 files.
//! * **Runs**: 00, 06, 12, 18 UTC, hourly to 48 h, every field every step
//!   from step 1. **Step 0 lacks** `PRATE`, `PTYPE` and `WEASN`, and its
//!   `TCDC` is a placeholder of zeros packed in no bits
//!   ([`FIRST_STEP_LACKS`]).
//! * **Latency**: step 0 about 2 h 48 after the run, step 1 2 h 58, step 48
//!   3 h 16 (eight runs of 5-6 November); two of them were late: 00 UTC of
//!   6 November (step 1 at 4 h 39, step 48 at 6 h 42) and 18 UTC of
//!   5 November (step 48 at 4 h 08).
//! * **Grid**: template 3.1 rotated lat-lon, 2540 x 1290, 0.0225 degree,
//!   southern pole of rotation -36.08852, 245.305142, angle 0. The u/v
//!   components are relative to the rotated grid (flags 56) and must be
//!   rotated to earth-relative: off by up to 40 degrees at the domain's east
//!   end otherwise. `WDIR_AGL-10m` is in degrees true (it agrees with
//!   GDPS's `WindDir` to a few degrees). Packing 5.40 (JPEG 2000). True
//!   coverage 27.3-70.6 N, 152.7-40.7 W.
//! * **Values**: `PRATE` kg m-2 s-1 (instant); `PTYPE` ECCC's own codes 1..9
//!   (see [`PtypeTable::Eccc`]); `WEASN` liquid-equivalent snow, kg m-2 from
//!   the run start (`tsnowp`, template 4.8 `0-h`); `TCDC` percent; `GUST`
//!   instantaneous (4.0). No reflectivity at any step (none in the listings
//!   of steps 0, 3 and 48).
//! * **Volume** (index sizes of the 12 UTC run of 6 November): the nine
//!   files a step needs come to 17-18 MB (step 0, five real files: 14 MB),
//!   so 0.83 GB per run and 3.3 GB a day.
//! * **Overlap**: the domain covers most of the contiguous US north of about
//!   40 N and eastern Alaska, and HRDPS ranks above HRRR, NBM and HRRR
//!   Alaska. **The adapter must set to no-data every cell inside
//!   [`MASK_OUT`]** (the United States), so the US models show there and
//!   HRDPS shows in Canada.
//! * **Licence**: ECCC Data Server End-use Licence v2.1.1; credit "Data
//!   Source: Environment and Climate Change Canada".
//! * Also on the Datamart, not used: `HRDPS-WEonG_*` post-processed fields,
//!   `model_hrdlps/` (1 km nests, not examined), `model_hrdpa` (precipitation
//!   analysis, 6 h and 24 h).
//!
//! [`PtypeTable::Eccc`]: crate::layer::PtypeTable::Eccc

use super::gdps::{ECCC, ECCC_ATTRIBUTION, ECCC_LICENCE, ECCC_LICENCE_URL};
use super::{AHEAD, IDENTITY, K_TO_C, PAST, Product, SNOW_FROM_SWE, run_total, sel};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec, PtypeTable};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const RUN_EVERY_HOURS: u32 = 6;
pub const HORIZON_HOURS: u32 = 48;

pub const HRDPS: SourceDef = SourceDef {
    id: "hrdps",
    family: Family::Hrdps,
    region: "",
    title: "HRDPS",
    provider: ECCC,
    attribution: ECCC_ATTRIBUTION,
    attribution_url: ECCC_LICENCE_URL,
    licence: ECCC_LICENCE,
    kind: FrameKind::Forecast,
    rank: 610,
    res_km: 2.5,
    max_zoom: 6,
};

pub const ROWS: &[SourceDef] = &[HRDPS];

/// Polygons, as `(lon, lat)` degrees, whose cells the adapter sets to
/// no-data: the contiguous US with the sea south of it, and Alaska with the
/// sea west of it. The border is simplified to within about 20 km (a
/// judgement: the edge falls between two good models). A cell is masked when
/// its true centre is inside either polygon (even-odd rule).
pub const MASK_OUT: &[&[(f32, f32)]] = &[
    &[
        (-140.0, 20.0),
        (-140.0, 48.3),
        (-124.8, 48.5),
        (-123.3, 48.3),
        (-123.2, 48.7),
        (-123.1, 49.0),
        (-95.15, 49.0),
        (-95.15, 49.38),
        (-94.8, 49.3),
        (-94.6, 48.7),
        (-93.2, 48.6),
        (-92.0, 48.3),
        (-89.6, 48.0),
        (-88.4, 48.3),
        (-84.8, 46.9),
        (-84.4, 46.5),
        (-83.5, 45.9),
        (-82.5, 45.3),
        (-82.15, 44.0),
        (-82.4, 43.0),
        (-82.5, 42.6),
        (-83.1, 42.1),
        (-82.7, 41.7),
        (-81.5, 42.1),
        (-80.0, 42.4),
        (-79.3, 42.75),
        (-78.9, 42.9),
        (-79.1, 43.3),
        (-77.5, 43.6),
        (-76.3, 44.2),
        (-75.8, 44.4),
        (-74.7, 45.0),
        (-71.5, 45.0),
        (-71.1, 45.3),
        (-70.3, 45.9),
        (-70.0, 46.7),
        (-69.2, 47.45),
        (-68.3, 47.35),
        (-67.8, 47.1),
        (-67.8, 45.7),
        (-67.4, 45.2),
        (-67.0, 44.8),
        (-66.9, 44.4),
        (-66.0, 42.0),
        (-60.0, 40.0),
        (-60.0, 20.0),
    ],
    &[
        (-170.0, 75.0),
        (-141.0, 75.0),
        (-141.0, 60.3),
        (-139.1, 60.1),
        (-137.5, 58.9),
        (-136.5, 59.6),
        (-135.2, 59.6),
        (-133.7, 58.6),
        (-132.3, 57.5),
        (-131.8, 56.8),
        (-130.0, 56.0),
        (-130.6, 54.7),
        (-132.7, 54.6),
        (-140.0, 54.0),
        (-170.0, 50.0),
    ],
];

pub const PRODUCT: &str = "model_hrdps/continental/2.5km";

const TMP_2M: FieldSel = sel("TMP", "AGL-2m");
const DPT_2M: FieldSel = sel("DPT", "AGL-2m");
const U_10M: FieldSel = sel("UGRD", "AGL-10m");
const V_10M: FieldSel = sel("VGRD", "AGL-10m");
const GUST_10M: FieldSel = sel("GUST", "AGL-10m");
const PRATE: FieldSel = sel("PRATE", "Sfc");
const PTYPE: FieldSel = sel("PTYPE", "Sfc");
const WEASN: FieldSel = run_total("WEASN", "Sfc");
const TCDC: FieldSel = sel("TCDC", "Sfc");

/// Fields step 0 does not have (`TCDC` is there, but all zeros). None is
/// ever asked for there: snowfall has no step 0, and the other inputs
/// reading them do not fill the past, while step 0 lands about 2 h 48 after
/// its valid time, beyond `frame::FORECAST_GRACE_MINUTES` (the adapter's
/// tests hold this).
pub const FIRST_STEP_LACKS: &[FieldSel] = &[PRATE, PTYPE, WEASN, TCDC];

/// The product these inputs read, and how long a part holds: its hour.
const MODEL: Product = Product {
    family: Family::Hrdps,
    product: PRODUCT,
    hold_minutes: 60,
};

pub const RADAR: InputSpec = MODEL.input(
    &[PRATE],
    Derive::ReflectivityFromRate { to_mmh: 3600.0 },
    AHEAD,
);
pub const PRECIP: InputSpec = MODEL.input(
    &[PRATE, PTYPE],
    Derive::PrecipTypeCode {
        table: PtypeTable::Eccc,
        to_mmh: 3600.0,
    },
    AHEAD,
);
pub const SNOW: InputSpec = MODEL.input(&[WEASN], SNOW_FROM_SWE, AHEAD);
pub const TEMP: InputSpec = MODEL.input(&[TMP_2M], K_TO_C, PAST);
pub const FEELS_LIKE: InputSpec =
    MODEL.input(&[TMP_2M, DPT_2M, U_10M, V_10M], Derive::FeelsLike, PAST);
pub const DEW_POINT: InputSpec = MODEL.input(&[DPT_2M], K_TO_C, PAST);
pub const WIND: InputSpec = MODEL.input(&[U_10M, V_10M, GUST_10M], Derive::Wind, PAST);
pub const CLOUDS: InputSpec = MODEL.input(&[TCDC], IDENTITY, AHEAD);
