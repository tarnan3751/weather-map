//! CPTEC/INPE WRF over South America, 0.07 degree (about 7 km). Research:
//! `docs/sources/canada-and-world.md` §7.2.
//!
//! * **Where**: `https://dataserver.cptec.inpe.br/dataserver_modelos/wrf/ams_07km/brutos/<YYYY>/<MM>/<DD>/00/`
//!   (the older `ftp.cptec.inpe.br/modelos/tempo/WRF/ams_07km/brutos/`
//!   redirects here), anonymous.
//! * **Layout**: one file per valid time, all levels, 160-195 MB:
//!   `WRF_cpt_07KM_<YYYYMMDDHH run>_<YYYYMMDDHH valid>.grib2`, with a
//!   wgrib-style inventory `WRF_cpt_07KM_<run>_<valid>.inv` (no `.grib2` in
//!   its name; `.grib2.idx` is a binary GrADS index) holding byte offsets
//!   (also a `.ctl`). Fetch fields by byte range; `var` and `level` of a
//!   [`FieldSel`] are the inventory's fourth and fifth fields. The
//!   directory listing is an HTML index with sizes rounded to `190M`.
//! * **Runs**: 00 UTC only, f000 to f180 hourly (181 files, checked
//!   2026-11-06).
//! * **Latency**: f000 about 5 h 20 after the run, f012 5 h 34, f048 5 h 59,
//!   f120 6 h 50, f180 7 h 08 (file times on the server).
//! * **Messages per hour wanted**: eight, about 5.2 MB (`TMP`, `DPT`, `UGRD`,
//!   `VGRD`, `GUST`, `REFC`, `TCDC`, `APCP`, 0.3-0.8 MB each), so a run's
//!   120 hours cost about 630 MB.
//! * **Level names**: `REFC` and `TCDC` are on `entire atmosphere
//!   (considered as a single layer)` (surface type 200), not the shorter
//!   `entire atmosphere` (type 10). `GUST` is on `surface`. At f000 `APCP`
//!   is an empty `0-0 day acc fcst` and the instants are `anl`.
//! * **Grid**: template 3.0, 1019 x 1081, 57.9 S-17.7 N, 90.67-19.41 W,
//!   south to north, earth-relative winds, packing 5.3.
//! * **Values**: `APCP` kg m-2 from the run start; `REFC` dB, -20 when there
//!   is no echo; `TCDC` percent. No type, no snowfall.
//! * **Licence**: nothing on the data server (no readme, no `robots.txt`).
//!   The CPTEC site (`www.cptec.inpe.br`) says that all of its content is
//!   published under Creative Commons Attribution-NoDerivatives 3.0
//!   Unported. Whether that covers the data server is UNVERIFIED, and "no
//!   derivatives" is a restriction a map of reprocessed forecasts may
//!   run into; credited to CPTEC/INPE either way.
//! * **Dew point**: the inventory line is `DPT:2 m above ground`, as assumed.

use super::{AHEAD, IDENTITY, K_TO_C, PAST, Product, per_hour, sel};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const RUN_HOURS: [u32; 1] = [0];
/// Hourly steps reach at least 180 h; 120 h is the ingest horizon, a
/// judgement: past five days a 7 km regional run adds little over GDPS, and
/// each step is a separate 204 MB file to range into.
pub const HORIZON_HOURS: u32 = 120;

pub const CPTEC: SourceDef = SourceDef {
    id: "cptec",
    family: Family::Cptec,
    region: "ams_07km",
    title: "CPTEC WRF",
    provider: "CPTEC/INPE",
    attribution: "CPTEC/INPE",
    attribution_url: "https://dataserver.cptec.inpe.br/",
    licence: "CC BY-ND 3.0 per the CPTEC site; not stated on the data server (UNVERIFIED)",
    kind: FrameKind::Forecast,
    rank: 410,
    res_km: 7.0,
    max_zoom: 5,
};

pub const ROWS: &[SourceDef] = &[CPTEC];

pub const PRODUCT: &str = "WRF_cpt_07KM";

const TMP_2M: FieldSel = sel("TMP", "2 m above ground");
const DPT_2M: FieldSel = sel("DPT", "2 m above ground");
const UGRD_10M: FieldSel = sel("UGRD", "10 m above ground");
const VGRD_10M: FieldSel = sel("VGRD", "10 m above ground");
const GUST_SFC: FieldSel = sel("GUST", "surface");
const REFC: FieldSel = sel("REFC", "entire atmosphere (considered as a single layer)");
const TCDC: FieldSel = sel("TCDC", "entire atmosphere (considered as a single layer)");
const APCP_1H: FieldSel = per_hour("APCP", "surface");

/// The product these inputs read, and how long a part holds: its hour.
const MODEL: Product = Product {
    family: Family::Cptec,
    product: PRODUCT,
    hold_minutes: 60,
};

pub const RADAR: InputSpec = MODEL.input(&[REFC], Derive::Reflectivity, AHEAD);
pub const PRECIP: InputSpec = MODEL.input(
    &[APCP_1H, TMP_2M],
    Derive::PrecipTypeTemp { to_mmh: 1.0 },
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
