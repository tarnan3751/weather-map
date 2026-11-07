//! DMI HARMONIE, 2 km: DINI (Denmark, Iceland, the Netherlands, Ireland: in
//! practice north-west Europe from Iberia's north to Finland, the UK and
//! Iceland) and IG (Iceland, Greenland, Svalbard). Research:
//! `docs/sources/europe-models.md` §7; field list from the saved `.index`
//! files.
//!
//! * **Where**: `https://dmi-opendata.s3.eu-north-1.amazonaws.com/`,
//!   anonymous S3 (listing works). The key-free API
//!   `https://opendataapi.dmi.dk/v1/forecastdata/collections` hands out the
//!   same `href`s.
//! * **Layout**: one file per run and valid hour holding all 93 surface
//!   fields, 0.5-0.62 GB, so always fetch by byte range:
//!   `forecastdata/HARMONIE_<region>_SF/HARMONIE_<region>_SF_<RUN>_<VALID>.grib`
//!   with `<RUN>`, `<VALID>` like `2026-11-06T150000Z`, plus `.grib.index`
//!   in wgrib2 `-s` form: `n:offset:d=YYYYMMDDHH:VAR:level:range:ENS=hi-res ctl`
//!   (IG's lines end in `:` instead: it writes product template 4.0, DINI the
//!   control-member form 4.1). A field's length is the next offset minus its
//!   own (the last runs to the end of the file). `var` and `level` of a
//!   [`FieldSel`] are the fourth and fifth fields; instantaneous ranges read
//!   `3 hour fcst` (`anl` at step 0), run totals `0-3 hour acc fcst`
//!   (`0-0 day acc fcst` at step 0), last-hour maxima `2-3 hour max fcst`.
//! * **Runs**: every 3 hours; DINI hourly to 60 h, IG hourly to 72 h. Only
//!   every [`INGEST_EVERY_HOURS`] are followed (see **Volume**).
//! * **Latency** (the 2026-11-06 runs): DINI step 0 1 h 24-1 h 39 after the
//!   run, the last step 2 h 24-3 h 03; IG 1 h 23-1 h 30 and 2 h 29-3 h 00,
//!   and IG's steps land out of order.
//! * **Grids**: Lambert (3.30), 2000 m; DINI 1906 x 1606, LaD 55.5, LoV 8 W;
//!   IG 1339 x 1589, LaD 72, LoV 36 W. South to north. **Winds are
//!   grid-relative** (flags 56): rotated by `sin(LaD) (lon - LoV)`, which
//!   turns DMI's own `WDIR` (true north) out of `UGRD`/`VGRD` to 0.01 degree.
//!   Packing 5.0, no bitmap. The two have the same fields except two radiation
//!   fields.
//! * **Values**: `TPRATE` is, despite its name, total precipitation kg m-2
//!   accumulated from the run start (`tp`); `TSPRATE` total solid (snow plus
//!   graupel) likewise (`titspf`). No type code and no reflectivity.
//!   **Total cloud** is `FRACCC:0 m above ground` (`cc`, 0.6.32, a fraction
//!   0..1). Checked on DINI 18 UTC +3 h, 2026-11-06: it lies between the
//!   largest of `LCDC`/`MCDC`/`HCDC` (which are percent) and their capped sum
//!   at 99.8 % of points, correlates 0.997 with that largest layer, is 0.99
//!   where the hour's precipitation exceeds 0.1 mm, and averages 73.3 %
//!   against ICON-EU's `clct` 73.2 % over the same area, with the same
//!   frontal bands. The unnamed `discipline=0 center=94 local_table=1
//!   parmcat=6 parm=199` (0.013-1.0) is not cloud cover: it is 0.995 where
//!   every layer is clear and falls to 0.10 under rain heavier than 1 mm/h
//!   (correlation -0.83 with low cloud). What it is is not known.
//! * **Volume**: a whole file is 0.50-0.62 GB, but only the needed fields are
//!   fetched: 61.2 MB a step for DINI (8 fields, three at 16 bits, five at
//!   24), 42.6 MB for IG. A DINI run is 3.7 GB, an IG run 3.1 GB, plus up to
//!   about 0.13 GB of run totals fetched a second time while a run arrives
//!   in batches. Every run would be 31 and 26 GB a day; every sixth hour is
//!   15.4 and 12.8 GB.
//! * **Licence**: CC BY 4.0, "no additional restrictions". Credit the
//!   source, link the licence and say the data were changed.

use super::{
    AHEAD, CC_BY_4, FRACTION_TO_PERCENT, K_TO_C, PAST, Product, SNOW_FROM_SWE, per_hour, run_total,
    sel, step_max,
};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const RUN_EVERY_HOURS: u32 = 3;
/// Runs followed: those starting on multiples of this (00, 06, 12, 18
/// UTC). Following every run would fetch about 31 GB a day for DINI and
/// 26 GB for IG (see **Volume**); every second run halves that, and a run is
/// then at most 6 h plus its 3 h latency old.
pub const INGEST_EVERY_HOURS: u32 = 6;
pub const DINI_HORIZON_HOURS: u32 = 60;
pub const IG_HORIZON_HOURS: u32 = 72;

pub const DINI: SourceDef = SourceDef {
    id: "dini",
    family: Family::Dmi,
    region: "DINI",
    title: "DMI HARMONIE DINI",
    provider: "DMI",
    attribution: "DMI HARMONIE (CC BY 4.0), modified",
    attribution_url: "https://www.dmi.dk/",
    licence: CC_BY_4,
    kind: FrameKind::Forecast,
    rank: 630,
    res_km: 2.0,
    max_zoom: 6,
};
pub const IG: SourceDef = SourceDef {
    id: "ig",
    region: "IG",
    title: "DMI HARMONIE IG",
    rank: 631,
    ..DINI
};

pub const ROWS: &[SourceDef] = &[DINI, IG];

/// Surface fields (the `_SF` collections).
pub const PRODUCT: &str = "SF";

const TMP_2M: FieldSel = sel("TMP", "2 m above ground");
const DPT_2M: FieldSel = sel("DPT", "2 m above ground");
const UGRD_10M: FieldSel = sel("UGRD", "10 m above ground");
const VGRD_10M: FieldSel = sel("VGRD", "10 m above ground");
const GUST_10M: FieldSel = step_max("GUST", "10 m above ground");
const TOTAL_1H: FieldSel = per_hour("TPRATE", "surface");
const SOLID_1H: FieldSel = per_hour("TSPRATE", "surface");
const SOLID_TOTAL: FieldSel = run_total("TSPRATE", "surface");
/// Total cloud cover, a fraction (see **Values**).
const CLOUD: FieldSel = sel("FRACCC", "0 m above ground");

/// The product these inputs read, and how long a part holds: its hour.
const MODEL: Product = Product {
    family: Family::Dmi,
    product: PRODUCT,
    hold_minutes: 60,
};

pub const RADAR: InputSpec = MODEL.input(
    &[TOTAL_1H],
    Derive::ReflectivityFromRate { to_mmh: 1.0 },
    AHEAD,
);
/// Graupel is in the solid part, so graupel showers read as MIX or SNOW.
pub const PRECIP: InputSpec = MODEL.input(
    &[TOTAL_1H, SOLID_1H],
    Derive::PrecipPhases { to_mmh: 1.0 },
    AHEAD,
);
pub const SNOW: InputSpec = MODEL.input(&[SOLID_TOTAL], SNOW_FROM_SWE, AHEAD);
pub const TEMP: InputSpec = MODEL.input(&[TMP_2M], K_TO_C, PAST);
pub const FEELS_LIKE: InputSpec = MODEL.input(
    &[TMP_2M, DPT_2M, UGRD_10M, VGRD_10M],
    Derive::FeelsLike,
    PAST,
);
pub const DEW_POINT: InputSpec = MODEL.input(&[DPT_2M], K_TO_C, PAST);
pub const WIND: InputSpec = MODEL.input(&[UGRD_10M, VGRD_10M, GUST_10M], Derive::Wind, PAST);
pub const CLOUDS: InputSpec = MODEL.input(&[CLOUD], FRACTION_TO_PERCENT, AHEAD);
