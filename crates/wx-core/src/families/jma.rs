//! Japan Meteorological Agency high-resolution precipitation nowcast tiles:
//! the observed analysis every 5 minutes (`jma_radar`) and its forecast to
//! +60 minutes (`jma_nowcast`). Research: `docs/sources/radar-world.md` §5
//! and `docs/sources/canada-and-world.md` §5.1.
//!
//! * **Where**: `https://www.jma.go.jp/bosai/jmatile/data/nowc/`, static
//!   files behind CloudFront (tiles `cache-control: max-age=86400`, time
//!   lists `max-age=60` with an `ETag`).
//! * **Times**: `targetTimes_<region>.json`, newest first, UTC
//!   `YYYYMMDDhhmmss`. `N1` (observed): `basetime == validtime`, every 5
//!   minutes, 3 hours back (37 entries). `N2` (forecast): the newest
//!   `basetime` only, `validtime` every 5 minutes from +5 to +60 minutes;
//!   the tiles of older runs stay on the server (a run 20 minutes old
//!   answered 200 on 2026-11-06). Elements `hrpns` (rate classes) and
//!   `hrpns_nd` (no-data area).
//! * **Tiles**: `<basetime>/none/<validtime>/surf/hrpns/<z>/<x>/<y>.png`,
//!   Web Mercator XYZ, 256 px. Only even zooms hold data: JMA's map asks
//!   for zooms 4 to 10 with `zoomUse="even"` (`nowc.properties*.xml`), and
//!   on 2026-11-06 the zoom-5 and zoom-7 tiles over rain that the zoom-4, 6
//!   and 8 tiles showed were empty placeholders in the analysis and 404 in
//!   the forecast. 4-bit palette PNG with
//!   transparency: indices 0 and 1 transparent (no rain), 2-9 the eight
//!   classes. The index is the class, not a measured rate. In an analysis
//!   a dry tile is a 334-byte transparent RGBA PNG; in a forecast it is a
//!   404 (the same tiles of the 21:00 analysis were served, those of its
//!   +10 minute forecast were not, on 2026-11-06), so there a missing tile
//!   is dry, and an older run is read only when `N1` has its analysis.
//! * **Classes**, from JMA's own legend (`/bosai/nowc/images/legend_en_normal_hrpns.svg`,
//!   fetched 2026-11-06): below 1, 1-5, 5-10, 10-20, 20-30, 30-50, 50-80
//!   and 80 mm/h and above, with the tiles' palette colours in that order
//!   (the legend draws the 20-30 class `#FFF500`, the tiles `#FAF500`).
//!   `wx_formats::jma::CLASSES` maps each to its arithmetic middle
//!   ([`CLASS_MMH`]).
//! * **Coverage**: `hrpns_nd` is not a tile but a gzipped GeoJSON,
//!   `<basetime>/none/<validtime>/surf/hrpns_nd/data.geojson?id=hrpns_nd`
//!   (168 kB): one polygon, the whole world with the observed area as its
//!   hole, which spans 120.50-148.99 E and 21.00-46.67 N on 2026-11-06.
//!   Japan, Busan and Taipei are inside it; Seoul, Shanghai, Fuzhou and
//!   Vladivostok are not. The adapter fetches only the tiles that touch the
//!   hole ([`TILES`]) and, with the polygon (asked for once an hour, and
//!   again minutes after a failure), makes every pixel outside it NaN;
//!   within it a transparent pixel is dry. No frame is built without a
//!   polygon: while none can be had, the last one read stands, or the copy
//!   of 2026-11-06 the adapter carries.
//! * **What the adapter delivers**: one field of rate in mm/h, each class at
//!   its middle and transparent pixels at 0, so the rate rule gives no echo
//!   there.
//! * **Tiles fetched** (a judgement): zoom [`TILE_ZOOM`] (2.0 km a pixel
//!   at 35 N), 24 tiles a frame: each analysis every 5 minutes, and the 12
//!   leads of one run in [`NOWCAST_FOLLOW_MINUTES`]. About 340 requests in
//!   10 minutes (0.6 a second) and under 1 MB an hour. Zoom 8 (0.5 km)
//!   would be about 230 tiles a frame; 1 km has no tiles. Two requests at
//!   a time to the host (the default for hosts other than S3) and the round
//!   trip to Tokyo allowed about 2.6 tiles a second on 2026-11-06, so
//!   filling the two-hour loop after a start (24 analyses) takes about four
//!   minutes.
//! * **Latency**: `N1` rewritten 26 seconds after its time, `N2` 1m49s.
//! * **Licence**: JMA website terms, Public Data License v1.0 (compatible
//!   with CC BY 4.0): cite "Source: Japan Meteorological Agency website" with
//!   the page URL, say the content was edited, and do not present it as made
//!   by the Japanese government.

use super::input;
use crate::layer::{Derive, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const EVERY_MINUTES: u32 = 5;
pub const NOWCAST_STEP_MINUTES: u32 = 5;
pub const NOWCAST_HORIZON_MINUTES: u32 = 60;
/// Runs start every 5 minutes; one in this many minutes is followed, as
/// each replaces the previous one's 12 parts (a judgement).
pub const NOWCAST_FOLLOW_MINUTES: u32 = 10;
/// 2.0 km per pixel at 35 N. Even: odd zooms have no data.
pub const TILE_ZOOM: u8 = 6;
/// The zoom-[`TILE_ZOOM`] tiles that touch the coverage polygon, as
/// columns `(x, first y, last y)`: 24 of the 6 by 7 rectangle.
pub const TILES: &[(u32, u32, u32)] = &[
    (53, 26, 28),
    (54, 24, 28),
    (55, 24, 27),
    (56, 22, 26),
    (57, 22, 25),
    (58, 22, 24),
];
/// Rate in mm/h that palette indices 2..=9 stand for: the middle of each
/// class, 0.5 for the lowest and 90 for the open-ended highest. The values
/// `wx_formats::jma::CLASSES` decodes to (the adapter's tests check they
/// agree).
pub const CLASS_MMH: [f32; 8] = [0.5, 3.0, 7.5, 15.0, 25.0, 40.0, 65.0, 90.0];

pub const JMA_RADAR: SourceDef = SourceDef {
    id: "jma_radar",
    family: Family::Jma,
    region: "N1",
    title: "JMA radar",
    provider: "JMA",
    attribution: "Source: Japan Meteorological Agency website (edited)",
    attribution_url: "https://www.jma.go.jp/bosai/nowc/",
    licence: "Public Data License v1.0 (Japan)",
    kind: FrameKind::Observed,
    rank: 940,
    res_km: 2.0,
    max_zoom: TILE_ZOOM,
};
pub const JMA_NOWCAST: SourceDef = SourceDef {
    id: "jma_nowcast",
    region: "N2",
    title: "JMA nowcast",
    kind: FrameKind::Forecast,
    rank: 720,
    ..JMA_RADAR
};

pub const ROWS: &[SourceDef] = &[JMA_RADAR, JMA_NOWCAST];

pub const PRODUCT: &str = "hrpns";

/// Hold: twice the 5-minute cadence plus 2 minutes of latency.
pub const RADAR: InputSpec = input(
    Family::Jma,
    PRODUCT,
    &[],
    Derive::ReflectivityFromRate { to_mmh: 1.0 },
    12,
    false,
)
.only(&["jma_radar"]);
/// Hold: the 5-minute step.
pub const NOWCAST_RADAR: InputSpec = input(
    Family::Jma,
    PRODUCT,
    &[],
    Derive::ReflectivityFromRate { to_mmh: 1.0 },
    5,
    false,
)
.only(&["jma_nowcast"]);
