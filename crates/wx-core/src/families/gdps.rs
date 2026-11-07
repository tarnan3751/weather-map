//! ECCC Global Deterministic Prediction System (GDPS), 15 km, hourly: the
//! main global base. Research: `docs/sources/canada-and-world.md` §1, §1.3;
//! checked against the Datamart's listings and files of 5-6 November 2026.
//!
//! * **Where**: MSC Datamart, `https://dd.weather.gc.ca/` (mirror
//!   `https://hpfx.collab.science.gc.ca/`, same layout), anonymous.
//! * **Layout**: one field and one step per file:
//!   `/<YYYYMMDD>/WXO-DD/model_gdps/15km/<HH>/<hhh>/<YYYYMMDD>T<HH>Z_MSC_GDPS_<var>_<level>_LatLon0.15_PT<hhh>H.grib2`,
//!   where `<var>` and `<level>` are the [`FieldSel`] `var` and `level`
//!   (`AirTemp`, `AGL-2m`). The leading date is the day of publication
//!   (UTC), which for these runs is the run's own. About 400 files per step
//!   (Apache index about 96 KB); a step's directory appears with a JSON of
//!   highs and lows minutes before its GRIB2 files, which land over a few
//!   minutes (ozone and UV an hour later).
//! * **Runs**: 00 and 12 UTC only. Steps are **hourly to 84 h, then every
//!   third hour to 240 h** (directories 85, 86, 88, ... exist but hold only
//!   a JSON; checked on the 12 UTC run of 6 November 2026, where 84, 87 and
//!   120 held about 400 GRIB2 files each and 85, 86 and 119 none). Ingested
//!   hourly to [`HORIZON_HOURS`].
//! * **Latency** (our fields): steps 1-13 together about 3 h 45-3 h 52 after
//!   the run, step 0 with them or a few minutes later, step 120 at 4 h 26
//!   (5 November) to 5 h 04 (6 November 12 UTC); the 00 UTC run of
//!   6 November was an hour later throughout (step 120 at 6 h 04).
//! * **Grid**: template 3.0, 2400 x 1201, 0.15 degree, south to north,
//!   longitudes 0..359.85, earth-relative winds (flags 48). Packing 5.40
//!   (JPEG 2000).
//! * **Values**: `PrecipRate` kg m-2 s-1; `TotalCloudCover` percent 0..100;
//!   `Snow-Accum` liquid-equivalent kg m-2 from the run start (`tsnowp`,
//!   template 4.8 `0-h`); `Precip-Accum1h` and `Snow-Accum1h` the same over
//!   the hour before the step (4.8 `h-1-h`), from step 1; `WindGust`
//!   instantaneous (4.0). Step 0 has no rate or accumulations.
//! * **Precipitation type**: `PrecipType-Instant` follows ECCC's own table
//!   ([`PtypeTable::Eccc`]) but exists **only every third step** (3, 6, 9,
//!   ...; step 0 holds a placeholder of 6, "none", everywhere), so it cannot
//!   type hourly steps. [`PRECIP`] types each hour from the shares of its
//!   own amounts instead ([`Derive::PrecipPhases`]): rain, snow or a mix,
//!   but never freezing rain or ice pellets, which GDPS does forecast
//!   (`FreezingRain-Accum1h`, `IcePellets-Accum1h`) and this rule cannot
//!   show. HRDPS, above it over Canada, has them.
//! * **Volume** (index sizes of the 12 UTC run of 6 November): the nine
//!   files a step needs come to 14-15 MB (step 0, six files: 7.7 MB), so
//!   1.2 GB per run to 84 h and 2.4 GB a day.
//! * No reflectivity: GDPS is not on the radar layer, where GFS's simulated
//!   reflectivity is the global base instead of a reflectivity derived from
//!   GDPS's rate.
//! * **Licence**: ECCC Data Server End-use Licence v2.1.1 (commercial use,
//!   copying and redistribution allowed). Required credit "Data Source:
//!   Environment and Climate Change Canada", with a link to the licence where
//!   possible.
//! * `model_gdps/10km/` holds only a sea-ice concentration analysis
//!   (`GDPS-Analysis_IICECONC_SFC_RLatLon0.09x0.09_PT000H.nc`, NetCDF, 29 MB,
//!   every 6 hours): not used.
//!
//! [`PtypeTable::Eccc`]: crate::layer::PtypeTable::Eccc

use super::{AHEAD, IDENTITY, K_TO_C, PAST, Product, SNOW_FROM_SWE, per_hour, run_total, sel};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const RUN_HOURS: [u32; 2] = [0, 12];
/// The last hourly step: past it the model is three-hourly, and a
/// three-hourly source above GFS would make playback stutter.
pub const HORIZON_HOURS: u32 = 84;

pub const ECCC: &str = "ECCC";
pub const ECCC_ATTRIBUTION: &str = "Data Source: Environment and Climate Change Canada";
/// The licence page, which the credit should link to.
pub const ECCC_LICENCE_URL: &str = "https://eccc-msc.github.io/open-data/licence/readme_en/";
pub const ECCC_LICENCE: &str = "ECCC Data Server End-use Licence v2.1.1";

pub const GDPS: SourceDef = SourceDef {
    id: "gdps",
    family: Family::Gdps,
    region: "",
    title: "GDPS",
    provider: ECCC,
    attribution: ECCC_ATTRIBUTION,
    attribution_url: ECCC_LICENCE_URL,
    licence: ECCC_LICENCE,
    kind: FrameKind::Forecast,
    rank: 150,
    res_km: 15.0,
    max_zoom: 5,
};

pub const ROWS: &[SourceDef] = &[GDPS];

pub const PRODUCT: &str = "model_gdps/15km";

const TMP_2M: FieldSel = sel("AirTemp", "AGL-2m");
const DPT_2M: FieldSel = sel("DewPoint", "AGL-2m");
const U_10M: FieldSel = sel("WindU", "AGL-10m");
const V_10M: FieldSel = sel("WindV", "AGL-10m");
const GUST_10M: FieldSel = sel("WindGust", "AGL-10m");
/// The hour's own amounts (`Precip-Accum1h_Sfc`, `Snow-Accum1h_Sfc`).
const PRECIP_1H: FieldSel = per_hour("Precip-Accum1h", "Sfc");
const SNOW_1H: FieldSel = per_hour("Snow-Accum1h", "Sfc");
const SNOW_ACCUM: FieldSel = run_total("Snow-Accum", "Sfc");
const CLOUD: FieldSel = sel("TotalCloudCover", "Sfc");

/// The product these inputs read, and how long a part holds: its hour.
const MODEL: Product = Product {
    family: Family::Gdps,
    product: PRODUCT,
    hold_minutes: 60,
};

/// Typed by the snow share of each hour's amount: the type code is
/// three-hourly (see the module docs).
pub const PRECIP: InputSpec = MODEL.input(
    &[PRECIP_1H, SNOW_1H],
    Derive::PrecipPhases { to_mmh: 1.0 },
    AHEAD,
);
pub const SNOW: InputSpec = MODEL.input(&[SNOW_ACCUM], SNOW_FROM_SWE, AHEAD);
pub const TEMP: InputSpec = MODEL.input(&[TMP_2M], K_TO_C, PAST);
pub const FEELS_LIKE: InputSpec =
    MODEL.input(&[TMP_2M, DPT_2M, U_10M, V_10M], Derive::FeelsLike, PAST);
pub const DEW_POINT: InputSpec = MODEL.input(&[DPT_2M], K_TO_C, PAST);
pub const WIND: InputSpec = MODEL.input(&[U_10M, V_10M, GUST_10M], Derive::Wind, PAST);
pub const CLOUDS: InputSpec = MODEL.input(&[CLOUD], IDENTITY, AHEAD);
