//! Taiwan Central Weather Administration radar composite (JSON), data id
//! `O-A0059-001` on the `cwaopendata` bucket.
//!
//! `cwaopendata.dataset.datasetInfo.parameterSet` gives the grid
//! (`StartPointLongitude`, `StartPointLatitude`, `GridResolution` in
//! degrees, `GridDimensionX` by `GridDimensionY`) and the time
//! (`DateTime`, ISO 8601 with offset); `contents.content` is one string of
//! comma-separated reflectivities, dBZ, west to east then south to north
//! from the lower-left point, which is a grid point (cell centre). `-999`
//! is outside radar range or removed by quality control and becomes `NaN`;
//! `-99` is no echo and becomes [`NO_ECHO_DBZ`].
//!
//! # Datum
//!
//! The grid is stated in TWD67 (Hu-Tzu-Shan datum, International 1967
//! ellipsoid), which sits about 830 m east and 200 m south of WGS84 in
//! Taiwan, two thirds of a cell. The grid is shifted by the TWD67 to WGS84
//! difference at Taiwan's centre (121 E, 23.5 N) from the standard 7-parameter
//! transformation (dx -752 m, dy -358 m, dz -179 m; rotations -1.1698e-6,
//! 1.8398e-6, 9.822e-7 rad; scale +23.29 ppm), the one that gives the
//! familiar TM2 offsets of +828 m E, -207 m N: +0.00812 degrees of longitude,
//! -0.00178 of latitude. Across the whole 11.5 degree grid the true shift
//! departs from that by under 0.0005 degrees, a twentieth of a cell.

use anyhow::{Context, Result, anyhow, ensure};
use chrono::DateTime;
use serde::Deserialize;
use wx_core::Field;
use wx_core::proj::{GridProjection, LatLonGrid};

use crate::{Decoded, NO_ECHO_DBZ, Quantity, checked_points};

/// Longitude to add to a TWD67 longitude in Taiwan for WGS84, degrees.
pub const TWD67_TO_WGS84_DLON: f64 = 0.008_12;
/// Latitude to add to a TWD67 latitude in Taiwan for WGS84, degrees.
pub const TWD67_TO_WGS84_DLAT: f64 = -0.001_78;

#[derive(Deserialize)]
struct Root {
    cwaopendata: Open,
}

#[derive(Deserialize)]
struct Open {
    dataset: Dataset,
}

#[derive(Deserialize)]
struct Dataset {
    #[serde(rename = "datasetInfo")]
    info: Info,
    contents: Contents,
}

#[derive(Deserialize)]
struct Info {
    #[serde(rename = "parameterSet")]
    params: Params,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Params {
    start_point_longitude: String,
    start_point_latitude: String,
    grid_resolution: String,
    date_time: String,
    grid_dimension_x: String,
    grid_dimension_y: String,
    reflectivity: Option<String>,
}

#[derive(Deserialize)]
struct Contents {
    content: String,
}

pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    let root: Root = serde_json::from_slice(bytes).context("not a CWA O-A0059-001 JSON document")?;
    let p = root.cwaopendata.dataset.info.params;
    if let Some(unit) = &p.reflectivity {
        ensure!(unit == "dBZ", "reflectivity unit {unit:?}");
    }
    let num = |s: &str, what: &str| -> Result<f64> {
        let v: f64 = s.trim().parse().map_err(|_| anyhow!("{what} is {s:?}"))?;
        ensure!(v.is_finite(), "{what} is {s:?}");
        Ok(v)
    };
    let lon0 = num(&p.start_point_longitude, "StartPointLongitude")?;
    let lat0 = num(&p.start_point_latitude, "StartPointLatitude")?;
    let res = num(&p.grid_resolution, "GridResolution")?;
    ensure!(res > 0.0 && res < 1.0, "GridResolution {res}");
    let dim = |s: &str, what: &str| -> Result<usize> { s.trim().parse().map_err(|_| anyhow!("{what} is {s:?}")) };
    let nx = dim(&p.grid_dimension_x, "GridDimensionX")?;
    let ny = dim(&p.grid_dimension_y, "GridDimensionY")?;
    let n = checked_points("CWA radar", nx, ny)?;
    let valid = DateTime::parse_from_rfc3339(p.date_time.trim())
        .with_context(|| format!("DateTime {:?}", p.date_time))?
        .to_utc();

    let mut values = Vec::with_capacity(n);
    for (k, item) in root.cwaopendata.dataset.contents.content.split(',').enumerate() {
        ensure!(k < n, "content holds more than {nx} x {ny} values");
        let v: f32 = item.trim().parse().map_err(|_| anyhow!("content value {k} is {item:?}"))?;
        values.push(if v == -999.0 || !v.is_finite() {
            f32::NAN
        } else if v == -99.0 {
            NO_ECHO_DBZ
        } else {
            v
        });
    }
    ensure!(values.len() == n, "content holds {} values for {nx} x {ny}", values.len());

    let proj = GridProjection::LatLon(LatLonGrid {
        lon0: lon0 + TWD67_TO_WGS84_DLON,
        lat0: lat0 + TWD67_TO_WGS84_DLAT,
        dlon: res,
        dlat: res,
        periodic: false,
    });
    let field = Field { nx, ny, proj, values, grid_relative_winds: false };
    Ok(Decoded::observed(field, Quantity::Reflectivity, valid))
}
