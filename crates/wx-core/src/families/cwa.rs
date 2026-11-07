//! Taiwan Central Weather Administration (CWA) composite reflectivity, from
//! its open-data bucket on AWS. Research: `docs/sources/canada-and-world.md`
//! §6. (The CWA open-data portal needs a key; this bucket does not.)
//!
//! * **Where**: `https://cwaopendata.s3.ap-northeast-1.amazonaws.com/Observation/O-A0059-001.json`
//!   (anonymous; overwritten in place each frame, so only the newest exists).
//!   Registry: `https://registry.opendata.aws/cwa_opendata/`. S3 answers
//!   byte ranges and `If-None-Match` (304), also together, so the adapter
//!   polls the first 4 kB, which hold `DateTime`, conditionally.
//! * **Container**: JSON. `GridDimensionX` 921, `GridDimensionY` 881,
//!   0.0125 degree, lower-left 115.0 E 18.0 N; `content` is one
//!   comma-separated string of 811401 floats in scientific notation, x west
//!   to east, then y south to north. dBZ, -99 no echo, -999 outside range or
//!   removed by quality control (the decoder makes them no echo and NaN).
//!   `DateTime` is local time with its offset (`2026-11-07T04:30:00+08:00`).
//!   About 8.9 MB per frame.
//! * **Datum**: TWD67, about 830 m east and 200 m south of WGS 84 in Taiwan;
//!   the decoder shifts the grid by the difference at Taiwan's centre
//!   (`wx_formats::cwa`). Checked on 2026-11-06 against CWA's own image of
//!   the 21:20 frame (`Observation/O-A0058-003.png`, 3600 px over 118-124
//!   E and 20.5-26.5 N, whose coastline matches OpenStreetMap's to 0.35
//!   km): the decoded echoes match the image's best about 1 km east and 1
//!   km south of it, so CWA draws the grid without the shift. Whether the
//!   shift is right is UNVERIFIED; either way the error is under a cell.
//! * **Cadence**: every 10 minutes, about 7 minutes late (20:30 written
//!   20:36:47 on 2026-11-06).
//! * **Licence**: Taiwan Open Government Data License; attribute CWA.

use super::input;
use crate::layer::{Derive, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const EVERY_MINUTES: u32 = 10;

pub const CWA: SourceDef = SourceDef {
    id: "cwa",
    family: Family::Cwa,
    region: "",
    title: "CWA radar",
    provider: "CWA",
    attribution: "Central Weather Administration (Taiwan)",
    attribution_url: "https://data.gov.tw/license",
    licence: "Taiwan Open Government Data License",
    kind: FrameKind::Observed,
    rank: 945,
    res_km: 1.4,
    max_zoom: 7,
};

pub const ROWS: &[SourceDef] = &[CWA];

/// Hold: twice the 10-minute cadence plus the 7-minute latency.
pub const RADAR: InputSpec = input(
    Family::Cwa,
    "O-A0059-001",
    &[],
    Derive::Reflectivity,
    27,
    false,
);
