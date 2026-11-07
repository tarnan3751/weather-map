//! Italian national radar composite from the Dipartimento della Protezione
//! Civile (DPC), through the backend of `radar.protezionecivile.it`.
//! Research: `docs/sources/radar-world.md` §4.
//!
//! * **Where**: an undocumented web-app API (read from the code of
//!   `radar.protezionecivile.it`), so it can change without notice; the
//!   adapter checks every answer and turns any surprise into "source
//!   unavailable". Newest time: `GET https://radar-api.protezionecivile.it/findLastProductByType?type=VMI&lang=it`
//!   gives `{"total":1,"lastProducts":[{"productType":"VMI","time":<ms>,"period":"PT5M"}]}`;
//!   the web app takes earlier frames to be every `period` before it.
//!   Download: `POST https://radar-api.protezionecivile.it/downloadProduct`
//!   with body `{"productType":"VMI","productDate":<ms>}` gives
//!   `{"bucket":"s3-prod-dpc-radar","key":"VMI/DD-MM-YYYY-HH-MM.tif","url":<presigned S3 URL>}`,
//!   the URL valid for 300 s. A time without a product is answered HTTP
//!   500 `{"error":"S3 error: ... HeadObject operation: Forbidden"}`. Two
//!   requests per frame. A WebSocket (`wss://radar-wss.protezionecivile.it/`,
//!   STOMP topic `/topic/product`) announces new products (not used).
//! * **Product**: `VMI`, vertical maximum intensity in dBZ, 1 km. (`SRI`,
//!   surface rain intensity in mm/h, also exists.) Every 5 minutes, about
//!   7 minutes late (18:20 written 18:27:29 on 2026-11-06), 0.6 MB.
//! * **Container**: GeoTIFF, float32, one band, 1200 x 1400, LZW, one-row
//!   strips. -9999 is outside coverage (becomes NaN); -9998 is taken to be
//!   "below the minimum" and becomes -99 (no echo) before `derive`
//!   (UNVERIFIED); echoes run 10 to 45 dBZ in 0.5 dB steps.
//! * **Grid**: the GeoKeys say Transverse Mercator with natural origin 42 N,
//!   12.5 E on WGS 84, 1000 m cells, tie point (-600000, 650000): x from
//!   -600 to 600 km, y from 650 down to -750 km. Checked on 2026-11-06
//!   against OPERA over the Alps and northern Italy at 21:05: echoes
//!   overlap best with no shift (0.55 at 25 dBZ and above, against 0.45
//!   with 4 km either way and 0.2 with 16 km), so the grid is placed right
//!   to within about a kilometre.
//! * **Licence**: CC BY-SA 4.0 (linked from the site's code). **Share-alike**:
//!   tiles made from it may have to carry the same licence.

use super::input;
use crate::layer::{Derive, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const EVERY_MINUTES: u32 = 5;

pub const DPC: SourceDef = SourceDef {
    id: "dpc",
    family: Family::Dpc,
    region: "",
    title: "DPC radar",
    provider: "Dipartimento della Protezione Civile",
    attribution: "Dipartimento della Protezione Civile (CC BY-SA 4.0)",
    attribution_url: "https://radar.protezionecivile.it/",
    licence: "CC BY-SA 4.0",
    kind: FrameKind::Observed,
    rank: 925,
    res_km: 1.0,
    max_zoom: 7,
};

pub const ROWS: &[SourceDef] = &[DPC];

/// Hold: twice the 5-minute cadence plus the 7-minute latency.
pub const RADAR: InputSpec = input(Family::Dpc, "VMI", &[], Derive::Reflectivity, 17, false);
