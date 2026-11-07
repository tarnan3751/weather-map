//! DWD radar composites for Germany: the 250 m column-maximum reflectivity
//! (`dwd_radar`) and the 2-hour radar nowcast in 5-minute steps
//! (`dwd_nowcast`). Research: `docs/sources/radar-world.md` §1.
//!
//! * **Where**: `https://opendata.dwd.de/weather/radar/composite/`,
//!   anonymous, nginx indexes (80 kB each, no `ETag` or `Last-Modified` on
//!   them), exactly two days kept (577 files of 5 minutes), static files
//!   with `ETag` and byte ranges. Every file is named by its nominal time;
//!   each product also has a `LATEST` alias overwritten every cycle, which
//!   the adapter does not use.
//! * **Observed** (`hx`): `hx/composite_hx_<YYYYMMDD>_<HHMM>-hd5` (no `.h5`
//!   extension; alias `composite_hx_LATEST-hd5`), DBZH "top view", 4800 x
//!   4400 cells of 250 m (21 M), 0.2 to 1 MB. Written about 1.5 minutes
//!   after its time (20:21:34 for 20:20 on 2026-11-06).
//! * **Nowcast** (`wn`): `wn/composite_wn_<YYYYMMDD>_<HHMM>.tar` (alias
//!   `composite_wn__LATEST.tar`, note the double underscore), an
//!   uncompressed GNU tar of 25 members
//!   `composite_wn_<YYYYMMDD>_<HHMM>_<NNN>-hd5`, `<NNN>` the lead in minutes
//!   000..120 every 5 minutes; DBZH, 1200 x 1100 cells of 1 km, 1 to 3 MB
//!   a tar. A run starts every 5 minutes and is written about 3 minutes
//!   after its time (20:23:02 for 20:20). Member 000 is the analysis and is
//!   not used (the observed product covers that time).
//! * **Container**: ODIM HDF5, superblock 0, `/dataset1/data1/data` uint16,
//!   one deflate chunk. `dBZ = raw * 0.002929821616590115 - 64.00292982161659`.
//!   `nodata` 65535 (outside the composite) becomes NaN and `undetect` 0
//!   (covered, no echo) becomes no echo in the decoder; never apply the
//!   gain to either.
//! * **Grid**: RADOLAN polar stereographic, `+proj=stere +lat_0=90 +lat_ts=60
//!   +lon_0=10 +a=6378137 +b=6356752.3142451802`, row 0 north. `x_0, y_0`
//!   (`543196.83521776402, 3622588.8619310018` for the 1 km grid,
//!   `543571.83521776402, 3622213.8619310022` for 250 m) put the centre of
//!   the upper-left cell at projected (0, 0), so the two grids share their
//!   outer corner (`UL_lon, UL_lat`): the offsets differ by exactly the 375
//!   m between the two half cells. Cell centres agree with pyproj to 1e-8
//!   degrees (the `wx-formats` tests). Against OPERA over southern Germany
//!   at 21:10 on 2026-11-06 the 250 m composite's echoes overlap best with
//!   no shift (0.26 at 25 dBZ and above, 0.05 with 4 km either way), and
//!   its no-data wedge points at the Feldberg radar.
//! * **Licence**: DWD open data, CC BY 4.0 with the source named (DWD's
//!   copyright pages returned 404 on the day; UNVERIFIED). Credit "Quelle:
//!   Deutscher Wetterdienst".
//! * Not used: the RADOLAN rates `ry`/`yw` and totals `rw`/`sf` (OPERA covers
//!   rainfall totals over Germany), `rv`/`rs` (accumulation nowcasts).

use super::icon::{DWD, DWD_ATTRIBUTION, DWD_URL};
use super::{CC_BY_4, input};
use crate::layer::{Derive, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const EVERY_MINUTES: u32 = 5;
pub const NOWCAST_STEP_MINUTES: u32 = 5;
pub const NOWCAST_HORIZON_MINUTES: u32 = 120;
/// Runs start every 5 minutes; one in this many minutes is followed, as
/// each replaces the previous one's 24 parts (a judgement).
pub const NOWCAST_FOLLOW_MINUTES: u32 = 10;

pub const DWD_RADAR: SourceDef = SourceDef {
    id: "dwd_radar",
    family: Family::DwdRadar,
    region: "",
    title: "DWD radar",
    provider: DWD,
    attribution: DWD_ATTRIBUTION,
    attribution_url: DWD_URL,
    licence: CC_BY_4,
    kind: FrameKind::Observed,
    rank: 950,
    res_km: 0.25,
    max_zoom: 9,
};
pub const DWD_NOWCAST: SourceDef = SourceDef {
    id: "dwd_nowcast",
    title: "DWD radar nowcast",
    kind: FrameKind::Forecast,
    rank: 710,
    res_km: 1.0,
    max_zoom: 7,
    ..DWD_RADAR
};

pub const ROWS: &[SourceDef] = &[DWD_RADAR, DWD_NOWCAST];

/// Hold: twice the 5-minute cadence plus 3 minutes of latency.
pub const RADAR: InputSpec =
    input(Family::DwdRadar, "hx", &[], Derive::Reflectivity, 13, false).only(&["dwd_radar"]);
/// Hold: the 5-minute step.
pub const NOWCAST_RADAR: InputSpec =
    input(Family::DwdRadar, "wn", &[], Derive::Reflectivity, 5, false).only(&["dwd_nowcast"]);
