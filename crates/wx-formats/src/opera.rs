//! EUMETNET OPERA pan-European radar composites (ODIM HDF5, CC BY 4.0).
//!
//! | File | Quantity | Grid | Cadence |
//! |---|---|---|---|
//! | `OPERA@...@DBZH.h5` | maximum reflectivity, dBZ | 3800 x 4400 x 1 km | 5 min |
//! | `OPERA@...@RATE.h5` | instantaneous rain rate, mm/h | 1900 x 2200 x 2 km | 15 min |
//! | `OPERA@...@ACRR.h5` | 1 h accumulation, mm | 1900 x 2200 x 2 km | 15 min |
//!
//! All three are float64 already in physical units (gain 1, offset 0) with
//! `nodata = -9999000` (no radar covers the cell; `NaN` here) and
//! `undetect = -8888000` (covered, nothing detected; [`NO_ECHO_DBZ`] or 0
//! here). RATE carries hail and clutter cores up to 600 mm/h; rates are cut
//! at [`MAX_RAIN_RATE_MMH`] and accumulations at that rate over their
//! period. The accumulation window comes from the file
//! (`dataset1/what/starttime` to `endtime`; one hour in every file seen).
//!
//! The grid is `+proj=laea +lat_0=55 +lon_0=10 +x_0=1950000 +y_0=-2100000
//! +ellps=WGS84`, placed from `where/UL_lon, UL_lat` as the outer corner of
//! the upper-left cell (the ODIM rule), which projects to exactly (0, 0):
//! DBZH and RATE then cover the same 3800 by 4400 km rectangle, as their
//! identical corner attributes say. The GeoTIFF versions of the same
//! products put the tie point half a cell further north-west (`-500.0003,
//! 499.9999` for DBZH: the projected HDF5 corner minus half a cell, from a
//! conversion that took the corner for a cell centre).
//!
//! The HDF5 placement is the better supported of the two, though not beyond
//! doubt. The blind zone of the Helchteren radar (seven undetect cells
//! amid echoes) has its centroid 0.30 cells from the radar's position under
//! it and 0.68 under the GeoTIFF's (tested; the radar at 51.0692 N 5.4058
//! E). Against DWD's own 250 m composite of the same 18:10 volume scans the
//! best match over Germany is 60 m from the HDF5 placement, but echo motion
//! between the two products' scan times blurs that by several hundred
//! metres (a fit that models the motion lands between the two), so on its
//! own it does not decide the question.
//!
//! [`NO_ECHO_DBZ`]: crate::NO_ECHO_DBZ
//! [`MAX_RAIN_RATE_MMH`]: crate::MAX_RAIN_RATE_MMH

use anyhow::{Result, ensure};

use crate::Decoded;
use crate::odim;

/// Decode an OPERA composite: whichever of DBZH, RATE or ACRR it holds.
pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    let c = odim::decode(bytes, &["DBZH", "RATE", "ACRR"])?;
    ensure!(
        c.projdef.contains("+proj=laea"),
        "OPERA composite with projdef {:?}, expected the OPERA LAEA grid",
        c.projdef
    );
    ensure!(c.decoded.run.is_none(), "OPERA composite flagged as a forecast");
    Ok(c.decoded)
}
