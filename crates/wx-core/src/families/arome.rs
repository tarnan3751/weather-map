//! Météo-France AROME at 0.025 degree: metropolitan France (domain EURW1S)
//! and the five overseas domains (AROME Outre-Mer). Research:
//! `docs/sources/europe-models.md` §3.1-3.2, §3.4. Checked against the
//! bucket's listings of every run from 2026-11-01 00 UTC to 2026-11-06
//! 15 UTC, and against decoded files of the 6 Nov 12 and 15 UTC runs.
//!
//! * **Where**: `https://meteofrance-pnt.s3.rbx.io.cloud.ovh.net/pnt/<RUN>/`,
//!   `<RUN>` like `2026-11-06T12:00:00Z`. Anonymous GET, S3 listing and byte
//!   ranges work; there are **no index files**. The bucket keeps about six
//!   days of runs.
//! * **France** (`region` EURW1S):
//!   `arome/0025/<PKG>/arome__0025__<PKG>__<GROUP>__<RUN>.grib2` with
//!   `<GROUP>` one of 00H06H (steps 0-6), 07H12H, 13H18H, 19H24H, 25H30H,
//!   31H36H, 37H42H, 43H48H (six steps each) and 49H51H (three). SP1 is
//!   54-58 MB per group (28 MB for 49H51H), 470 MB a run. Messages are
//!   grouped by parameter (all steps of one, then the next), 15 parameters,
//!   97 messages in 00H06H and 90 in the six-step groups; the adapter walks
//!   their headers. 8 runs a day (00, 03, ..., 21 UTC), every one hourly to
//!   51 h. The first group lands about 1 h 55 after the 00 and 03 UTC runs,
//!   4 h 20 after 06 and 18 UTC, and 3 h 05 to 3 h 25 after the others; the
//!   last 50 minutes later. Domain 37.5-55.4 N, 12 W-16 E (a trapezoid
//!   inside the rectangle, so a bitmap).
//! * **Overseas** (`region` ANTIL, GUYANE, INDIEN, NCALED, POLYN):
//!   `arome-om/<region>/0025/<PKG>/arome-om-<region>__0025__<PKG>__<HHH>H__<RUN>.grib2`,
//!   one file per hour, 15 messages (7 at step 0). SP1 files are 0.8-1.7 MB
//!   (GUYANE), 1.3-3.3 MB (NCALED, POLYN), 2.3-6.2 MB (ANTIL) and 6.9-14.3 MB
//!   (INDIEN); a run is 75, 137, 139, 275 and 650 MB. All five run four times
//!   a day (00, 06, 12, 18 UTC; every one of the 24 runs of 1-6 Nov had 49
//!   files per domain), hourly to 48 h. The first file lands about 6 h 26
//!   after the run (INDIEN 6 h 39), the last 7 h 08 (GUYANE) to 7 h 57
//!   (INDIEN) after; a whole run was an hour late once in those six days.
//! * **Grids**: template 3.0, 0.025 degree, north to south, earth-relative
//!   winds, packing **5.42 (CCSDS)**.
//! * **Fields**: everything used here is in package SP1 in both trees, so
//!   every [`FieldSel`] has `level` "SP1" and `var` the ecCodes short name
//!   ([`GRIB_IDS`] gives the GRIB numbers). Both trees' SP1 hold the same 15
//!   parameters (`10wdir`, `10si`, `max_i10fg`, `prmsl`, `10u`, `10v`,
//!   `max_10efg`, `max_10nfg`, `2t`, `2r`, `tp`, `0.6.1`, `tsnowp`, `ssrd`,
//!   `tgrp`), in every domain, so every input applies to every source. Neither
//!   has a dew point (France's SP2 has `2d`; one package is fetched, not
//!   two), so dew point and feels-like come from `2t` and `2r` (percent)
//!   everywhere. Accumulations (`tp`, `tsnowp`, `tgrp`, in kg m-2) run from
//!   the start of the run (`0-7` in 07H12H, `0-12` in the overseas `012H`);
//!   `tp` is rain plus snow plus graupel (the parameter PDF's `PRECIP`, and
//!   `tp - tsnowp - tgrp` is never below packing noise). `max_i10fg` is the
//!   maximum over the last hour (`11-12`). Total cloud (`0.6.1`, which
//!   ecCodes calls "unknown"; the PDF's `NEBUL`) is percent in both trees:
//!   0.0087-100 in the France 15 UTC 07H12H group and in the overseas files.
//!   Step 0 has no accumulations, no gust and no cloud: those parts start
//!   at step 1. The overseas domains are tropical, where `tsnowp` is a
//!   constant 0 (a 207-byte message); it is kept, since the model can snow
//!   on Réunion's summits.
//! * **Volume**: following every run of every source, with France walked
//!   (half of each group, 237 MB a run) and the overseas files downloaded
//!   whole: France 1.9 GB a day (3.8 GB whole), ANTIL 1.1, GUYANE 0.3,
//!   INDIEN 2.6, NCALED 0.55, POLYN 0.55, about 7 GB a day in all. Every run
//!   of `arome_fr` is followed: there is no reason to space them.
//! * **Licence**: Licence Ouverte 2.0 (Etalab). Name the source
//!   (Météo-France) and the date of last update (the run time of the part in
//!   view), and keep a link to the licence.
//! * Not used: the 0.01 degree France files (one 23 MB file an hour per
//!   package; SP1 has no dew point there either), ARPEGE (IFS and GDPS cover
//!   the globe, AROME the regions).

use super::{
    AHEAD, IDENTITY, K_TO_C, PAST, Product, SNOW_FROM_SWE, per_hour, run_total, sel, step_max,
};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

/// Every run is followed (see "Volume").
pub const FRANCE_RUN_EVERY_HOURS: u32 = 3;
pub const FRANCE_HORIZON_HOURS: u32 = 51;
/// All five domains, INDIEN included.
pub const OVERSEAS_RUN_EVERY_HOURS: u32 = 6;
pub const OVERSEAS_HORIZON_HOURS: u32 = 48;

/// How a [`FieldSel`] `var` is found among a package's messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GribId {
    pub short_name: &'static str,
    /// `(discipline, category, number)`.
    pub param: (u8, u8, u8),
    /// Type of the first fixed surface (103 height above ground, 1 ground).
    pub surface: u8,
    pub surface_value: u32,
}

const fn id(
    short_name: &'static str,
    param: (u8, u8, u8),
    surface: u8,
    surface_value: u32,
) -> GribId {
    GribId {
        short_name,
        param,
        surface,
        surface_value,
    }
}

/// The SP1 messages used here, identical in both trees. Accumulations
/// (`tp`, `tsnowp`, `tgrp`) and the gust use product template 4.8; the rest
/// 4.0.
pub const GRIB_IDS: &[GribId] = &[
    id("2t", (0, 0, 0), 103, 2),
    id("2r", (0, 1, 1), 103, 2),
    id("10u", (0, 2, 2), 103, 10),
    id("10v", (0, 2, 3), 103, 10),
    id("max_i10fg", (0, 2, 22), 103, 10),
    id("tp", (0, 1, 52), 1, 0),
    id("tsnowp", (0, 1, 66), 1, 0),
    id("tgrp", (0, 1, 75), 1, 0),
    id("0.6.1", (0, 6, 1), 1, 0),
];

pub const AROME_FR: SourceDef = SourceDef {
    id: "arome_fr",
    family: Family::Arome,
    region: "EURW1S",
    title: "AROME France",
    provider: "Météo-France",
    attribution: "Météo-France",
    attribution_url: "https://meteo.data.gouv.fr/",
    licence: "Licence Ouverte 2.0 (Etalab)",
    kind: FrameKind::Forecast,
    rank: 640,
    res_km: 2.5,
    max_zoom: 6,
};
pub const AROME_ANTILLES: SourceDef = SourceDef {
    id: "arome_antilles",
    region: "ANTIL",
    title: "AROME Antilles",
    rank: 641,
    ..AROME_FR
};
pub const AROME_GUYANE: SourceDef = SourceDef {
    id: "arome_guyane",
    region: "GUYANE",
    title: "AROME Guyane",
    rank: 642,
    ..AROME_FR
};
pub const AROME_INDIEN: SourceDef = SourceDef {
    id: "arome_indien",
    region: "INDIEN",
    title: "AROME Indian Ocean",
    rank: 643,
    ..AROME_FR
};
pub const AROME_NCALED: SourceDef = SourceDef {
    id: "arome_ncaled",
    region: "NCALED",
    title: "AROME New Caledonia",
    rank: 644,
    ..AROME_FR
};
pub const AROME_POLYN: SourceDef = SourceDef {
    id: "arome_polyn",
    region: "POLYN",
    title: "AROME Polynesia",
    rank: 645,
    ..AROME_FR
};

pub const ROWS: &[SourceDef] = &[
    AROME_FR,
    AROME_ANTILLES,
    AROME_GUYANE,
    AROME_INDIEN,
    AROME_NCALED,
    AROME_POLYN,
];

/// The 0.025 degree tree.
pub const PRODUCT: &str = "0025";

const T_2M: FieldSel = sel("2t", "SP1");
const RH_2M: FieldSel = sel("2r", "SP1");
const U_10M: FieldSel = sel("10u", "SP1");
const V_10M: FieldSel = sel("10v", "SP1");
const GUST: FieldSel = step_max("max_i10fg", "SP1");
const CLOUD: FieldSel = sel("0.6.1", "SP1");
const TP_1H: FieldSel = per_hour("tp", "SP1");
const SNOW_1H: FieldSel = per_hour("tsnowp", "SP1");
const SNOW_TOTAL: FieldSel = run_total("tsnowp", "SP1");

/// The product these inputs read, and how long a part holds: its hour.
const MODEL: Product = Product {
    family: Family::Arome,
    product: PRODUCT,
    hold_minutes: 60,
};

pub const RADAR: InputSpec = MODEL.input(
    &[TP_1H],
    Derive::ReflectivityFromRate { to_mmh: 1.0 },
    AHEAD,
);
/// Snow is the solid part; graupel (`tgrp`, inside `tp`) counts as liquid.
pub const PRECIP: InputSpec = MODEL.input(
    &[TP_1H, SNOW_1H],
    Derive::PrecipPhases { to_mmh: 1.0 },
    AHEAD,
);
pub const SNOW: InputSpec = MODEL.input(&[SNOW_TOTAL], SNOW_FROM_SWE, AHEAD);
pub const TEMP: InputSpec = MODEL.input(&[T_2M], K_TO_C, PAST);
pub const FEELS_LIKE: InputSpec =
    MODEL.input(&[T_2M, RH_2M, U_10M, V_10M], Derive::FeelsLikeRh, PAST);
pub const DEW_POINT: InputSpec = MODEL.input(&[T_2M, RH_2M], Derive::DewPointFromRh, PAST);
pub const WIND: InputSpec = MODEL.input(&[U_10M, V_10M, GUST], Derive::Wind, PAST);
pub const CLOUDS: InputSpec = MODEL.input(&[CLOUD], IDENTITY, AHEAD);
