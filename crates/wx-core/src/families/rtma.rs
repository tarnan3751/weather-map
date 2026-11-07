//! NOAA/NCEP Real-Time Mesoscale Analysis (RTMA): hourly 2 m and 10 m
//! analyses over the contiguous US, Alaska, Hawaii, Puerto Rico and Guam.
//! Research: `docs/sources/noaa-regional.md` §2.
//!
//! * **Where**: `https://noaa-rtma-pds.s3.amazonaws.com/`, anonymous.
//! * **Layout**: one directory per region and day, one analysis file per
//!   hour with an `.idx` ([`DIRECTORIES`] has the exact names):
//!   `<prefix>.<YYYYMMDD>/<prefix>.t<HH>z.<suffix>`. The first guess
//!   (`2dvarges`) and error (`2dvarerr`) files are not used.
//! * **Fields**: one group of about 13 `anl` records, `.idx` lines
//!   `n:offset:d=YYYYMMDDHH:VAR:level:anl:`; per-field byte ranges from the
//!   offsets. No precipitation. `GUST` is at `10 m above ground`. `TCDC`
//!   exists (`entire atmosphere (considered as a single layer)`) but the cloud
//!   layer is forecast-only, so it is not used.
//! * **Grids**: CONUS Lambert (3.30); Alaska polar stereographic (3.20,
//!   1649 x 1105, 2976.6 m, the same grid as NBM Alaska) with grid-relative
//!   winds; Hawaii, Puerto Rico and Guam Mercator (3.10), whose winds are
//!   flagged grid-relative but whose axes are east and north already.
//!   Packing 5.0.
//! * **Cadence**: hourly. Alaska, Hawaii, Puerto Rico and Guam analyses are
//!   written 35-40 minutes after the hour, CONUS about 50 minutes after.
//! * **Coverage note**: the Alaska grid is huge (40.5-75.4 N, from 150 E
//!   across the dateline to 93.6 W) and has values over British Columbia and
//!   Washington, so `rtma` ranks above `rtma_ak` and wins there.
//! * **Licence**: US government work, public domain. Credit "NOAA/NCEP RTMA".
//! * Not adopted: URMA (`noaa-urma-pds`), about 6.7 hours late.

use super::{K_TO_C, NOAA, NOAA_URL, Product, US_PUBLIC_DOMAIN, sel};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const EVERY_MINUTES: u32 = 60;

/// `(SourceDef::region, directory and file prefix, file suffix after
/// t<HH>z.)`. Each file has an `.idx` beside it.
pub const DIRECTORIES: &[(&str, &str, &str)] = &[
    ("conus", "rtma2p5", "2dvaranl_ndfd.grb2_wexp"),
    ("ak", "akrtma", "2dvaranl_ndfd_3p0.grb2"),
    ("hi", "hirtma", "2dvaranl_ndfd.grb2"),
    ("pr", "prrtma", "2dvaranl_ndfd.grb2"),
    ("gu", "gurtma", "2dvaranl_ndfd.grb2"),
];

pub const RTMA: SourceDef = SourceDef {
    id: "rtma",
    family: Family::Rtma,
    region: "conus",
    title: "RTMA",
    provider: NOAA,
    attribution: "NOAA/NCEP RTMA",
    attribution_url: NOAA_URL,
    licence: US_PUBLIC_DOMAIN,
    kind: FrameKind::Observed,
    rank: 801,
    res_km: 2.5,
    max_zoom: 6,
};
pub const RTMA_AK: SourceDef = SourceDef {
    id: "rtma_ak",
    region: "ak",
    title: "RTMA Alaska",
    rank: 800,
    res_km: 3.0,
    ..RTMA
};
pub const RTMA_HI: SourceDef = SourceDef {
    id: "rtma_hi",
    region: "hi",
    title: "RTMA Hawaii",
    rank: 802,
    ..RTMA
};
pub const RTMA_PR: SourceDef = SourceDef {
    id: "rtma_pr",
    region: "pr",
    title: "RTMA Puerto Rico",
    rank: 803,
    res_km: 1.25,
    max_zoom: 7,
    ..RTMA
};
pub const RTMA_GU: SourceDef = SourceDef {
    id: "rtma_gu",
    region: "gu",
    title: "RTMA Guam",
    rank: 804,
    ..RTMA
};

pub const ROWS: &[SourceDef] = &[RTMA, RTMA_AK, RTMA_HI, RTMA_PR, RTMA_GU];

/// The product names the analysis; the file name varies by region (see
/// [`DIRECTORIES`]).
pub const PRODUCT: &str = "2dvaranl_ndfd";

const TMP_2M: FieldSel = sel("TMP", "2 m above ground");
const DPT_2M: FieldSel = sel("DPT", "2 m above ground");
const UGRD_10M: FieldSel = sel("UGRD", "10 m above ground");
const VGRD_10M: FieldSel = sel("VGRD", "10 m above ground");
const GUST_10M: FieldSel = sel("GUST", "10 m above ground");

/// Twice the hourly cadence plus the CONUS latency of about 50 minutes.
const HOLD: u32 = 170;

/// The product these inputs read, and how long a part holds.
const MODEL: Product = Product {
    family: Family::Rtma,
    product: PRODUCT,
    hold_minutes: HOLD,
};

pub const TEMP: InputSpec = MODEL.input(&[TMP_2M], K_TO_C, false);
pub const FEELS_LIKE: InputSpec = MODEL.input(
    &[TMP_2M, DPT_2M, UGRD_10M, VGRD_10M],
    Derive::FeelsLike,
    false,
);
pub const DEW_POINT: InputSpec = MODEL.input(&[DPT_2M], K_TO_C, false);
pub const WIND: InputSpec = MODEL.input(&[UGRD_10M, VGRD_10M, GUST_10M], Derive::Wind, false);
