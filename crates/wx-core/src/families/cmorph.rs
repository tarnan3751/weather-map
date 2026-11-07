//! NOAA CPC CMORPH2 near-real-time satellite precipitation, 0.25 degree,
//! global, every 30 minutes: the base under radar and the worldwide rainfall
//! totals. It is an observation but ranks below every model, so that ahead
//! of "now" the models cover it. Research: `docs/sources/satellite.md` §6.
//!
//! * **Where**: `https://ftp.cpc.ncep.noaa.gov/precip/CMORPH2/CMORPH2NRT/`
//!   (plain HTTPS on the FTP host, Apache listings; readme
//!   `readme_CMORPH2x_NRT.txt`).
//! * **Layout**: `DATA/<YYYY>/<YYYYMM>/<YYYYMMDD>/CMORPH2_0.25deg-30min_<YYYYMMDDHHMM>.RT.nc`,
//!   about 0.6 MB, `<HHMM>` the **start** of the half hour (1700 covers
//!   17:00:00-17:29:59). Future day and month directories exist empty, so
//!   take the newest file inside today's directory. A new file shows up
//!   about 13 minutes after the modification time the listing gives it (the
//!   20:00 file of 2026-11-06, listed as written at 20:45, answered 404 to a
//!   GET until 20:58, when the listing first showed it too): the listing
//!   does not lag behind the files, the times in it are early.
//! * **Container**: HDF5 (netCDF-4), superblock 0, one deflate chunk, no
//!   shuffle. Dataset `cmorph` int16 (1, 720, 1440), `scale_factor` 0.01,
//!   `missing_value` -999, mm/hr (a 30-minute mean rate); `frac` the frozen
//!   percentage (not used). `lat` runs -89.875 to 89.875 (row 0 is south),
//!   `lon` 0.125 to 359.875 (cell centres, 0..360).
//! * **Latency**: about one hour after the period starts (58 minutes for
//!   the 20:00 file of 2026-11-06, 59 for 17:30 the same day). Each file is then
//!   rewritten every 30 minutes for 12 hours as more microwave passes arrive
//!   (on 2026-11-06 the run at 20:15 wrote the 19:30 file and rewrote the 23
//!   before it back to 08:00, newest first, one every 30 seconds until
//!   20:26; the 00:00 file was last written at 12:25). The catalog keeps
//!   the first part it takes for a valid time, so a part is not replaced by
//!   a rewrite; the totals are summed from the newest version of each half
//!   hour, so each new total carries the rewrites.
//! * **Valid times**: a rate part on the radar layer is valid at the start
//!   of its half hour, so it stands for the half hour it measured and then,
//!   through the hold, until the next one arrives (valid at the end, the
//!   frame for 21:20 would show the 20:30 to 21:00 mean beside radars
//!   showing 21:20). A total is valid at the end of its newest half hour,
//!   the time the decoder gives (start plus `time_coverage_duration`),
//!   since it is the rain up to then.
//! * **Totals**: the adapter sums the half-hour files itself, `depth = sum of
//!   rate x 0.5 h`, over the 2, 48 or 144 newest files ([`TOTAL_FILES`]), and
//!   hands `derive` one field of depth in mm. A total is made when at least
//!   90 percent of its half hours are there (2 of 2, 44 of 48, 130 of 144)
//!   and never otherwise; a cell is the sum of the half hours with a value
//!   there scaled to the full period, missing where fewer than 90 percent
//!   have one (a judgement). A cold start downloads about 85 MB for 72 h.
//! * **Checked**: the hourly total ending 2026-11-06 20:30 against GFS's
//!   12Z +8 h precipitation rate: zonal means correlate 0.74 and put the
//!   tropical rain belt in the same 5 degree band (0 to 5 N), and on a 2.5
//!   degree lattice the two correlate 0.41 as they are and -0.01 shifted by
//!   180 degrees, so the 0..360 longitudes and the south-first rows land
//!   where they should.
//! * **Licence**: US government work, public domain. Credit "NOAA CPC
//!   CMORPH2". The readme names individual scientists; do not copy contacts.

use super::{NOAA, NOAA_URL, US_PUBLIC_DOMAIN, input, sel};
use crate::layer::{Derive, FieldSel, FrameKind, InputSpec};
use crate::source::{Family, SourceDef};

pub const ENABLED: bool = true;

pub const EVERY_MINUTES: u32 = 30;
/// How long after its start a half hour's file usually appears.
pub const LATENCY_MINUTES: u32 = 60;
/// `(product, half-hour files summed)`.
pub const TOTAL_FILES: &[(&str, u32)] = &[(TOTAL_1H, 2), (TOTAL_24H, 48), (TOTAL_72H, 144)];

pub const CMORPH: SourceDef = SourceDef {
    id: "cmorph",
    family: Family::Cmorph,
    region: "",
    title: "CMORPH2",
    provider: NOAA,
    attribution: "NOAA CPC CMORPH2",
    attribution_url: NOAA_URL,
    licence: US_PUBLIC_DOMAIN,
    kind: FrameKind::Observed,
    rank: 50,
    res_km: 28.0,
    max_zoom: 4,
};

pub const ROWS: &[SourceDef] = &[CMORPH];

/// One half-hour file's mean rate.
pub const RATE: &str = "rate_30min";
pub const TOTAL_1H: &str = "total_1h";
pub const TOTAL_24H: &str = "total_24h";
pub const TOTAL_72H: &str = "total_72h";

/// The dataset read from each file.
const CMORPH_RATE: FieldSel = sel("cmorph", "");

/// Hold for every input: twice the 30-minute cadence plus the hour of
/// latency, so the newest half hour stays on the newest radar time steps.
/// A rate part, valid at the start of its half hour, arrives about 60 to 90
/// minutes after that start (public 58 minutes after it on 2026-11-06,
/// then the next poll and the build) and is replaced 30 minutes later, so
/// it must stand for up to two hours: 150 minutes leaves half an hour of
/// slack. A total, valid at the end, arrives 30 to 60 minutes after it.
const HOLD: u32 = 150;

pub const RADAR: InputSpec = input(
    Family::Cmorph,
    RATE,
    &[CMORPH_RATE],
    Derive::ReflectivityFromRate { to_mmh: 1.0 },
    HOLD,
    false,
);
pub const RAIN_1H: InputSpec = input(
    Family::Cmorph,
    TOTAL_1H,
    &[CMORPH_RATE],
    Derive::PrecipDepth,
    HOLD,
    false,
);
pub const RAIN_24H: InputSpec = input(
    Family::Cmorph,
    TOTAL_24H,
    &[CMORPH_RATE],
    Derive::PrecipDepth,
    HOLD,
    false,
);
pub const RAIN_72H: InputSpec = input(
    Family::Cmorph,
    TOTAL_72H,
    &[CMORPH_RATE],
    Derive::PrecipDepth,
    HOLD,
    false,
);
