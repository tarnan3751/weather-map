//! EUMETNET OPERA pan-European radar composites, from the open MeteoGate
//! bucket. Research: `docs/sources/radar-world.md` §2; the ACRR period was
//! read from the saved sample.
//!
//! * **Where**: `https://s3.waw3-1.cloudferro.com/openradar-24h/`, anonymous
//!   S3 (listing and GET), a rolling 24 hours. List
//!   `?list-type=2&prefix=<YYYY>/<MM>/<DD>/OPERA/COMP/` (1000 keys a page)
//!   with `start-after` set just before the oldest time still wanted: an
//!   hour of keys (both products and their GeoTIFF twins) is 11 kB.
//! * **Layout**: `<YYYY>/<MM>/<DD>/OPERA/COMP/OPERA@<YYYYMMDD>T<HHMM>@0@<product>.h5`
//!   (`@` percent-encoded as `%40` for strict clients); the time is the
//!   nominal (end) time. A GeoTIFF twin (`.tiff`) exists; use the HDF5.
//! * **Container**: ODIM HDF5 (H5rad 2.4), superblock 0, deflate.
//!   `/dataset1/data1/data` float64 with **physical** values (gain 1, offset
//!   0). `nodata` -9999000 (outside coverage) becomes NaN; `undetect`
//!   -8888000 (covered, nothing seen) becomes no echo for DBZH and 0 for
//!   ACRR, in the decoder (`wx_formats::opera`). A quality plane
//!   `quality1` (0..1) is not used.
//! * **Products**: `DBZH` maximum reflectivity, 1 km (3800 x 4400), every
//!   5 minutes, written 4m06s to 4m19s after its time (bucket
//!   `LastModified`, 2026-11-06), about 2 MB; some national inputs are
//!   advected by Météo-France (pysteps). `ACRR` "OPERA NIMBUS 1-hour
//!   accumulation composite" (start 17:00, end 18:00 for the 18:00 file),
//!   2 km, every 15 minutes, written 12m02s to 12m06s after its time, 1.5
//!   MB. Decoding, deriving and gridding a DBZH file takes about 0.1 s and
//!   320 MB at its peak (release build, 2026-11-06).
//! * **Grid**: `+proj=laea +lat_0=55.0 +lon_0=10.0 +x_0=1950000.0
//!   +y_0=-2100000.0 +ellps=WGS84`; the upper-left outer corner projects to
//!   (0, 0); row 0 is north.
//! * **Coverage**: about 24 countries. Missing: Austria, Italy, Greece,
//!   Latvia, Bulgaria, Serbia, Turkey, Ukraine, Russia, Luxembourg; Spain
//!   only partly.
//! * **Licence**: CC BY 4.0. The ORD documentation (`1-ORD-API-overview`,
//!   read 2026-11-06): "The property rights of OPERA composite products are
//!   held by EUMETNET, which has decided to distribute these products under
//!   the CC BY 4.0 license" (the "exceptions noted in metadata" concern the
//!   single-site data, not the composites). No particular credit wording is
//!   asked for; the attribution names EUMETNET OPERA and its members.

use super::{CC_BY_4, input};
use crate::layer::{Derive, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const DBZH_EVERY_MINUTES: u32 = 5;
pub const ACRR_EVERY_MINUTES: u32 = 15;

pub const OPERA: SourceDef = SourceDef {
    id: "opera",
    family: Family::Opera,
    region: "COMP",
    title: "OPERA",
    provider: "EUMETNET",
    attribution: "EUMETNET OPERA, contributing national meteorological services",
    attribution_url: "https://eumetnet.github.io/openradardata-documentation/",
    licence: CC_BY_4,
    kind: FrameKind::Observed,
    rank: 920,
    res_km: 1.0,
    max_zoom: 7,
};

pub const ROWS: &[SourceDef] = &[OPERA];

/// Hold: twice the 5-minute cadence plus the 4-minute latency.
pub const RADAR: InputSpec = input(Family::Opera, "DBZH", &[], Derive::Reflectivity, 14, false);
/// Hold: twice the 15-minute cadence plus the 12-minute latency, rounded up.
pub const RAIN_1H: InputSpec = input(Family::Opera, "ACRR", &[], Derive::PrecipDepth, 45, false);
