//! NOAA/NESDIS Global Mosaic of Geostationary Satellite Imagery (GMGSI),
//! longwave infrared, hourly: the worldwide base of the satellite layer.
//! Research: `docs/sources/satellite.md` §4; the count-to-temperature mapping
//! was checked against GOES-19 (below).
//!
//! * **Where**: `https://noaa-gmgsi-pds.s3.amazonaws.com/`, anonymous.
//! * **Layout**: `GMGSI_LW/<YYYY>/<MM>/<DD>/<HH>/GLOBCOMPLIR_v3r0_blend_s<YYYYMMDDHH>00000_e<YYYYMMDDHH>09599_c<creation>.nc`,
//!   one file an hour (7.3 MB). The creation stamp varies, so the file must
//!   be listed: a day holds 24, so the adapter lists
//!   `GMGSI_LW/<YYYY>/<MM>/<DD>/` (no delimiter, starting after the first
//!   hour it wants) and takes the files whose name and hour directory
//!   agree. (`GMGSI_SW`, `_WV`, `_VIS` exist too; not used.)
//! * **Container**: HDF5 (netCDF-4), superblock 2, shuffle + deflate 5.
//!   Dataset `data` float32 (1, 3000, 4999), chunks (1, 793, 1322), values
//!   0..255 (the `units` attribute says K; it is not), `_FillValue` -9999 (none
//!   seen). `lat`/`lon` 2-D arrays exist but are redundant.
//! * **Grid**: Mercator, row 0 north: `lat = atan(sinh((1499 - j) * 0.072
//!   deg in radians))`, 72.7 S to 72.7 N. Column 0 is a duplicate of 180;
//!   columns 1..4998 have `lon = -179.9284 + (i - 1) * 0.07201`.
//! * **Values**: McIDAS brightness counts, cold is high (the scale of
//!   [`Derive::McidasIrCount`]). The decoder turns them into kelvin, so the
//!   input's rule is [`Derive::BrightnessTemp`]. Checked on the 2026-11-06
//!   samples: GMGSI 17:00 against GOES-19 band 13 at 18:10 over 45,000 pixel
//!   pairs, median difference 0.1 K, median absolute difference 3.0 K
//!   (clouds moved in the 70 minutes between them). The mapping is good to a
//!   fraction of a kelvin for warm scenes (250 to 315 K, counts 30 to 160)
//!   and to about 5 K for the coldest cloud tops (below 242 K, where the
//!   scale steps 1 K per count and the comparison only brackets it).
//! * **Cadence**: hourly, written 35-40 minutes after the hour starts (19:00
//!   of 2026-11-06 at 19:39).
//! * **Blend**: Meteosat-10, Meteosat-9 (Indian Ocean), Himawari-9, GOES-18,
//!   GOES-19, so it covers Europe, Africa and Asia without a key.
//! * **Licence**: NOAA/NESDIS OSPO product, US government work, no
//!   restrictions stated. Credit "NOAA/NESDIS OSPO GMGSI". The files carry
//!   contact attributes; do not copy them.

use super::{NOAA, NOAA_URL, US_PUBLIC_DOMAIN, input, sel};
use crate::layer::{Derive, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const EVERY_MINUTES: u32 = 60;
/// How long after the hour its file usually appears.
pub const LATENCY_MINUTES: u32 = 40;

pub const GMGSI: SourceDef = SourceDef {
    id: "gmgsi",
    family: Family::Gmgsi,
    region: "",
    title: "GMGSI",
    provider: NOAA,
    attribution: "NOAA/NESDIS OSPO GMGSI",
    attribution_url: NOAA_URL,
    licence: US_PUBLIC_DOMAIN,
    kind: FrameKind::Observed,
    rank: 60,
    res_km: 8.0,
    max_zoom: 5,
};

pub const ROWS: &[SourceDef] = &[GMGSI];

/// Hold: twice the hourly cadence plus the 40-minute latency. The decoder
/// (`wx_formats::gmgsi`) turns the counts into kelvin with the mapping
/// above, so the rule is the one for brightness temperatures.
pub const SATELLITE: InputSpec = input(
    Family::Gmgsi,
    "GMGSI_LW",
    &[sel("data", "")],
    Derive::BrightnessTemp,
    160,
    false,
);
