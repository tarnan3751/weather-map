//! DWD ICON regional models: ICON-EU (Europe, 0.0625 degree, about 6.5 km)
//! and ICON-D2 (Germany and neighbours, 0.02 degree, about 2.2 km), both on
//! their regular lat-lon grids. Research: `docs/sources/europe-models.md` §1.
//!
//! * **Where**: `https://opendata.dwd.de/weather/nwp/<region>/grib/<HH>/<var>/`,
//!   anonymous, Apache listings (directory times are not reliable; list
//!   `<HH>/t_2m/` and look for the run's last step).
//! * **Layout**: one bzip2-compressed GRIB2 file per variable and step (step
//!   in hours, 3 digits). The [`FieldSel`] `var` is the lowercase variable
//!   directory and `level` is empty; the file token is the directory name
//!   upper-cased for ICON-EU and `2d_` plus the directory name for ICON-D2:
//!   * `icon-eu/grib/12/t_2m/icon-eu_europe_regular-lat-lon_single-level_2026110612_003_T_2M.grib2.bz2`
//!   * `icon-d2/grib/12/t_2m/icon-d2_germany_regular-lat-lon_single-level_2026110612_003_2d_t_2m.grib2.bz2`
//! * **ICON-EU**: runs every 3 hours; 00/06/12/18 UTC reach 120 h (hourly to
//!   78 h, then 3-hourly); 03/09/15/21 UTC are hourly to 30 h, then 36, 42
//!   and 48 h (`u_10m`, `v_10m` and `vmax_10m` hourly to 51 h). Measured on
//!   2026-11-06: step 0 lands 2 h 39-2 h 43 after the run, step 78 3 h 29,
//!   step 30 of a short run 2 h 49. Grid 3.0, 1377 x 657, 29.5-70.5 N,
//!   23.5 W-62.5 E, south to north. Packing **5.42 (CCSDS)**.
//! * **ICON-D2**: runs every 3 hours, hourly to 48 h; step 0 lands
//!   44 minutes after the run, step 48 1 h 20 (`ww` has no step 0 and is
//!   done by about 1 h 08). Grid 3.0, 1215 x 746, 43.18-58.08 N,
//!   3.94 W-20.34 E, south to north, packing 5.0 with a bitmap: the corners
//!   of the rectangle are missing (83 % valid; ecCodes shows 9999). The
//!   `tot_prec`, `snow_gsp`, `prs_gsp` and `dbz_cmax` files hold four
//!   messages, at :00, :15, :30 and :45 of the file's hour, with the step in
//!   minutes (`195m` in the `003` file; accumulations `0m-195m`); the `048`
//!   file holds only its :00 message; the others hold one.
//! * **Values**: winds earth-relative. `tot_prec`, `snow_gsp`, `snow_con` kg
//!   m-2 accumulated from the run start; `vmax_10m` the maximum over the
//!   previous output interval (the last hour); `ww` the WMO present-weather
//!   code (see [`PtypeTable::WmoWw`], checked against the 2026-11-06 runs);
//!   `clct` percent; `dbz_cmax` dBZ with a -150 no-echo floor. ICON-EU's
//!   `snow_con` (`csfwe`, 0.1.55) is there for every step, a run total like
//!   `snow_gsp`; ICON-D2 has one too but is convection-permitting, so its
//!   convective snow is small and is left out.
//! * **Volume** (bzip2 as served, the inputs below, 2026-11-06): ICON-D2
//!   503 MB a run (`dbz_cmax` 146 MB of it), 4.0 GB a day at every run;
//!   ICON-EU 639 MB for the hourly 78 h of a long run and 248 MB for a short
//!   one, 3.5 GB a day. Both are followed at every run.
//! * **Licence**: DWD open data, CC BY 4.0 with the source named. Credit
//!   "Quelle: Deutscher Wetterdienst" (the exact wording of DWD's
//!   acknowledgement template is UNVERIFIED).
//! * Not adopted: ICON global (icosahedral grid only), ICON-EU's 3-hourly tail
//!   beyond 78 h (the hourly rule), `icon-d2-ruc` (not on the server).
//!
//! [`PtypeTable::WmoWw`]: crate::layer::PtypeTable::WmoWw

use super::{
    AHEAD, CC_BY_4, IDENTITY, K_TO_C, PAST, Product, SNOW_FROM_SWE, SWE_MM_TO_SNOW_MM, input,
    per_hour, run_total, sel, step_max,
};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec, PtypeTable};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const RUN_EVERY_HOURS: u32 = 3;
/// ICON-EU's long runs.
pub const EU_LONG_RUN_HOURS: [u32; 4] = [0, 6, 12, 18];
/// ICON-EU's hourly part on its 00/06/12/18 UTC runs.
pub const EU_HORIZON_HOURS: u32 = 78;
/// ICON-EU's 03/09/15/21 UTC runs end here.
pub const EU_SHORT_RUN_HORIZON_HOURS: u32 = 30;
pub const D2_HORIZON_HOURS: u32 = 48;
/// The sub-hourly messages of ICON-D2's four-message files.
pub const D2_SUBHOURLY_STEP_MINUTES: u32 = 15;

pub const DWD: &str = "DWD";
pub const DWD_ATTRIBUTION: &str = "Quelle: Deutscher Wetterdienst";
pub const DWD_URL: &str = "https://opendata.dwd.de/";

pub const ICON_EU: SourceDef = SourceDef {
    id: "icon_eu",
    family: Family::Icon,
    region: "icon-eu",
    title: "ICON-EU",
    provider: DWD,
    attribution: DWD_ATTRIBUTION,
    attribution_url: DWD_URL,
    licence: CC_BY_4,
    kind: FrameKind::Forecast,
    rank: 400,
    res_km: 6.5,
    max_zoom: 5,
};
/// Ranked above DINI and AROME France: DINI's domain contains all of
/// ICON-D2's, so below it ICON-D2 would never show, and ICON-D2 is the
/// freshest of the three (complete about 1 h 20 after its run, against 2 h 43
/// and 3 h 54) and the only one with simulated reflectivity.
pub const ICON_D2: SourceDef = SourceDef {
    id: "icon_d2",
    region: "icon-d2",
    title: "ICON-D2",
    rank: 660,
    res_km: 2.2,
    max_zoom: 6,
    ..ICON_EU
};

pub const ROWS: &[SourceDef] = &[ICON_EU, ICON_D2];

/// The single-level, regular lat-lon files, hour by hour.
pub const PRODUCT: &str = "regular-lat-lon_single-level";
/// The same files read every [`D2_SUBHOURLY_STEP_MINUTES`]: the four
/// messages of ICON-D2's sub-hourly files. A product of its own, because the
/// leads a product has are what an adapter schedules.
pub const SUBHOURLY: &str = "regular-lat-lon_single-level_15min";

const T_2M: FieldSel = sel("t_2m", "");
const TD_2M: FieldSel = sel("td_2m", "");
const U_10M: FieldSel = sel("u_10m", "");
const V_10M: FieldSel = sel("v_10m", "");
const VMAX_10M: FieldSel = step_max("vmax_10m", "");
const CLCT: FieldSel = sel("clct", "");
const WW: FieldSel = sel("ww", "");
const DBZ_CMAX: FieldSel = sel("dbz_cmax", "");
const TOT_PREC_1H: FieldSel = per_hour("tot_prec", "");
const SNOW_GSP: FieldSel = run_total("snow_gsp", "");
const SNOW_CON: FieldSel = run_total("snow_con", "");

/// ICON-D2's own column-maximum reflectivity, every 15 minutes.
pub const RADAR_D2: InputSpec = input(
    Family::Icon,
    SUBHOURLY,
    &[DBZ_CMAX],
    Derive::Reflectivity,
    15,
    AHEAD,
)
.only(&["icon_d2"]);
/// The product these inputs read, and how long a part holds: its hour.
const MODEL: Product = Product {
    family: Family::Icon,
    product: PRODUCT,
    hold_minutes: 60,
};

/// ICON-EU has no reflectivity: derived from the hour's precipitation.
pub const RADAR_EU: InputSpec = MODEL
    .input(
        &[TOT_PREC_1H],
        Derive::ReflectivityFromRate { to_mmh: 1.0 },
        AHEAD,
    )
    .only(&["icon_eu"]);
pub const PRECIP: InputSpec = MODEL.input(
    &[TOT_PREC_1H, WW],
    Derive::PrecipTypeCode {
        table: PtypeTable::WmoWw,
        to_mmh: 1.0,
    },
    AHEAD,
);
/// Grid-scale plus convective snowfall, liquid equivalent.
pub const SNOW_EU: InputSpec = MODEL
    .input(
        &[SNOW_GSP, SNOW_CON],
        Derive::Sum {
            mul: SWE_MM_TO_SNOW_MM,
            add: 0.0,
        },
        AHEAD,
    )
    .only(&["icon_eu"]);
pub const SNOW_D2: InputSpec = MODEL
    .input(&[SNOW_GSP], SNOW_FROM_SWE, AHEAD)
    .only(&["icon_d2"]);
pub const TEMP: InputSpec = MODEL.input(&[T_2M], K_TO_C, PAST);
pub const FEELS_LIKE: InputSpec =
    MODEL.input(&[T_2M, TD_2M, U_10M, V_10M], Derive::FeelsLike, PAST);
pub const DEW_POINT: InputSpec = MODEL.input(&[TD_2M], K_TO_C, PAST);
pub const WIND: InputSpec = MODEL.input(&[U_10M, V_10M, VMAX_10M], Derive::Wind, PAST);
pub const CLOUDS: InputSpec = MODEL.input(&[CLCT], IDENTITY, AHEAD);
