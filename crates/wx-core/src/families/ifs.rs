//! ECMWF IFS open data at 0.1 degree (about 9 km), published since
//! 2026-10-20. **Defined but not ingested yet** ([`ENABLED`] is false): it
//! steps 3-hourly, and under the hourly rule a 3-hourly source ranked above
//! GDPS and GFS would hide their hourly steps and make playback stutter. It is
//! switched on once time-blending between steps exists. Research:
//! `docs/sources/europe-models.md` §2.
//!
//! * **Where**: `https://data.ecmwf.int/forecasts/`, anonymous, byte ranges
//!   honoured. 2000 simultaneous connections overall.
//! * **Layout**:
//!   `<YYYYMMDD>/<HH>z/ifs/0p1/oper/<YYYYMMDD><HH>0000-<step>h-oper-fc-sfc.grib2`
//!   (79-98 MB) with a JSON-lines `.index`: one object per message with
//!   `param`, `levtype`, `step`, `_offset` and `_length`; messages are in no
//!   particular order, so always go through the index. A [`FieldSel`]'s
//!   `var` is the `param` and `level` the `levtype`. The `ifs` folder of a
//!   run appears only once that run is released.
//! * **Runs**: 00, 06, 12, 18 UTC; every 3 h to 144 h (00 and 12 UTC then
//!   6-hourly to 360 h).
//! * **Latency**: 0.1 degree 8 h 27 to 9 h 34 after the run, two hours after
//!   the 0.25 degree files (`ifs/0p25/oper/...-oper-fc.grib2`, same params,
//!   1440 x 721), which can stand in meanwhile.
//! * **Grid**: template 3.0, 3600 x 1801, north to south, **first column at
//!   180 E**, both poles; earth-relative winds; packing **5.42 (CCSDS)**, bits
//!   per value set per parameter.
//! * **Values** (checked in the sample): `tp` and `sf` in **metres** of
//!   water from the run start; `tprate` kg m-2 s-1; `ptype` WMO code table
//!   4.201 as ECMWF uses it (see [`PtypeTable::Ecmwf`]); `tcc` a fraction
//!   0..1; `10fg` the maximum over the last hour. No reflectivity, so IFS is
//!   not on the radar layer (GFS's reflectivity stays the global base there).
//! * **Licence**: CC BY 4.0 and the ECMWF Terms of Use. The wording of
//!   ECMWF's "appropriate attribution" page was not retrieved (UNVERIFIED).
//! * Not adopted: AIFS (6-hourly, no gust, no type).
//!
//! [`PtypeTable::Ecmwf`]: crate::layer::PtypeTable::Ecmwf

use super::{
    AHEAD, CC_BY_4, FRACTION_TO_PERCENT, K_TO_C, PAST, Product, SWE_MM_TO_SNOW_MM, run_total, sel,
    step_max,
};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec, PtypeTable};
use crate::source::{Family, SourceDef};

/// Not ingested until time-blending between steps exists.
pub const ENABLED: bool = false;

pub const RUN_EVERY_HOURS: u32 = 6;
pub const STEP_HOURS: u32 = 3;
pub const HORIZON_HOURS: u32 = 144;

pub const IFS: SourceDef = SourceDef {
    id: "ifs",
    family: Family::Ifs,
    region: "",
    title: "ECMWF IFS",
    provider: "ECMWF",
    attribution: "ECMWF (CC BY 4.0)",
    attribution_url: "https://data.ecmwf.int/forecasts/",
    licence: CC_BY_4,
    kind: FrameKind::Forecast,
    rank: 200,
    res_km: 9.0,
    max_zoom: 5,
};

pub const ROWS: &[SourceDef] = &[IFS];

/// The 0.1 degree surface files.
pub const PRODUCT: &str = "0p1/oper/fc-sfc";

const T_2M: FieldSel = sel("2t", "sfc");
const TD_2M: FieldSel = sel("2d", "sfc");
const U_10M: FieldSel = sel("10u", "sfc");
const V_10M: FieldSel = sel("10v", "sfc");
const GUST: FieldSel = step_max("10fg", "sfc");
const TPRATE: FieldSel = sel("tprate", "sfc");
const PTYPE: FieldSel = sel("ptype", "sfc");
const SF: FieldSel = run_total("sf", "sfc");
const TCC: FieldSel = sel("tcc", "sfc");

/// The 3-hour step.
const HOLD: u32 = 180;

/// The product these inputs read, and how long a part holds.
const MODEL: Product = Product {
    family: Family::Ifs,
    product: PRODUCT,
    hold_minutes: HOLD,
};

pub const PRECIP: InputSpec = MODEL.input(
    &[TPRATE, PTYPE],
    Derive::PrecipTypeCode {
        table: PtypeTable::Ecmwf,
        to_mmh: 3600.0,
    },
    AHEAD,
);
/// Metres of water to millimetres of snow.
pub const SNOW: InputSpec = MODEL.input(
    &[SF],
    Derive::Linear {
        mul: 1000.0 * SWE_MM_TO_SNOW_MM,
        add: 0.0,
    },
    AHEAD,
);
pub const TEMP: InputSpec = MODEL.input(&[T_2M], K_TO_C, PAST);
pub const FEELS_LIKE: InputSpec =
    MODEL.input(&[T_2M, TD_2M, U_10M, V_10M], Derive::FeelsLike, PAST);
pub const DEW_POINT: InputSpec = MODEL.input(&[TD_2M], K_TO_C, PAST);
pub const WIND: InputSpec = MODEL.input(&[U_10M, V_10M, GUST], Derive::Wind, PAST);
pub const CLOUDS: InputSpec = MODEL.input(&[TCC], FRACTION_TO_PERCENT, AHEAD);
