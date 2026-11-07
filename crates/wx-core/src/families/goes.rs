//! NOAA GOES-19 (East, 75 W) and GOES-18 (West, 137 W) ABI band 13
//! (10.3 um clean infrared), full disk every 10 minutes, 2 km: the satellite
//! layer over the Americas and the Pacific. Research:
//! `docs/sources/satellite.md` §2.
//!
//! * **Where**: `https://<region>.s3.amazonaws.com/` with `region`
//!   `noaa-goes19` or `noaa-goes18`, anonymous.
//! * **Layout**: `ABI-L2-CMIPF/<YYYY>/<DDD>/<HH>/OR_ABI-L2-CMIPF-M6C13_G19_s<YYYYDDDHHMMSSs>_e<...>_c<...>.nc`
//!   (`<DDD>` day of year; `G18` for GOES-18), about 24.7 MB. An hour prefix
//!   holds at most six band-13 files; the newest is the last key of the
//!   current or previous hour. The valid time is the scan start (`s`).
//! * **Container**: HDF5 (netCDF-4), superblock 2, shuffle + deflate 5.
//!   Dataset `CMI` int16 (5424, 5424), chunks (24, 5424), `scale_factor`
//!   0.06145332, `add_offset` 89.62, `_FillValue` -1: brightness temperature
//!   in K once scaled. The reader applies the scale and offset and turns the
//!   fill value into NaN.
//! * **Grid**: geostationary (`goes_imager_projection`: perspective height
//!   35786023 m, GRS80 ellipsoid, sweep `x`, longitude of origin -75 or -137);
//!   `x`, `y` are scan angles (int16, scale 5.6e-05, offset -0.151844 for `x`
//!   and 0.151844 for `y`, row 0 north).
//! * **Cropping** (a judgement): each adapter keeps only cells within
//!   [`MAX_VIEW_DEG`] of great circle from its sub-satellite point (beyond
//!   that GMGSI looks better than a slanted view), and GOES-18, which ranks
//!   on top, only cells west of [`SPLIT_LON`], the meridian halfway between
//!   the two, so each satellite shows the side it sees best. Cropped, the
//!   2026-11-06 disks keep 5314 x 5296 cells (GOES-19) and 4251 x 5296
//!   (GOES-18) of 5424 x 5424; the adapter stores them averaged over 2 x 2
//!   cells (4 km at the sub-satellite point), see
//!   `wx_ingest::families::goes::FACTOR`. Checked against GMGSI at 20:00
//!   that day over each satellite's side: correlation 0.99, median
//!   difference 0.0 K, and the Pacific coast along 23.5 S and 31 N within
//!   0.16 degrees of GFS's land mask.
//! * **Cadence**: every 10 minutes; a file is written about 10 minutes after
//!   its scan starts (the 20:20:21 scan of 2026-11-06 at 20:30:17).
//! * **Licence**: NOAA open data, public domain.
//! * Not used: `ABI-L2-RRQPEF` rain rate (stalled on both satellites since
//!   2026-11-02 about 17 UTC, and hemispheric), band 2 visible (432 MB a
//!   file), `MCMIPF` (all bands, 379 MB).

use super::{NOAA, NOAA_URL, US_PUBLIC_DOMAIN, input, sel};
use crate::layer::{Derive, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const EVERY_MINUTES: u32 = 10;
/// How long after its scan starts a file usually appears.
pub const LATENCY_MINUTES: u32 = 10;
pub const GOES19_LON: f32 = -75.0;
pub const GOES18_LON: f32 = -137.0;
/// GOES-18 keeps only cells west of this longitude.
pub const SPLIT_LON: f32 = -106.0;
/// Great-circle distance from the sub-satellite point beyond which cells are
/// dropped.
pub const MAX_VIEW_DEG: f32 = 70.0;

pub const GOES19: SourceDef = SourceDef {
    id: "goes19",
    family: Family::Goes,
    region: "noaa-goes19",
    title: "GOES-19",
    provider: NOAA,
    attribution: "NOAA GOES-19",
    attribution_url: NOAA_URL,
    licence: US_PUBLIC_DOMAIN,
    kind: FrameKind::Observed,
    rank: 70,
    res_km: 2.0,
    max_zoom: 6,
};
pub const GOES18: SourceDef = SourceDef {
    id: "goes18",
    region: "noaa-goes18",
    title: "GOES-18",
    attribution: "NOAA GOES-18",
    rank: 71,
    ..GOES19
};

pub const ROWS: &[SourceDef] = &[GOES19, GOES18];

/// Hold: twice the 10-minute cadence plus the 10-minute latency.
pub const SATELLITE: InputSpec = input(
    Family::Goes,
    "ABI-L2-CMIPF/C13",
    &[sel("CMI", "")],
    Derive::BrightnessTemp,
    30,
    false,
);
