//! NOAA CPC CMORPH2 near-real-time global precipitation (NetCDF-4).
//!
//! One file per half hour, `CMORPH2_0.25deg-30min_YYYYMMDDHHMM.RT.nc`,
//! named after the start of its half hour. `cmorph` is int16 `(1, 720,
//! 1440)` with `scale_factor` 0.01 and `missing_value` -999, the mean rate
//! over the half hour in mm/h. The grid is regular 0.25 degree, cell centres
//! from 89.875 S northward (row 0 is the southernmost) and from 0.125 E
//! eastward, global and periodic in longitude; rows are kept in that order.
//!
//! The result is a [`Quantity::RainRate`] valid at the end of the half hour
//! (`time` is its start, `time_coverage_duration` its length) with `period`
//! the half hour. Zero is dry; `NaN` is missing (a few cells at the poles).

use anyhow::{Context, Result, ensure};
use wx_core::Field;
use wx_core::proj::{GridProjection, LatLonGrid};

use crate::cf::{Packing, iso_duration, time_at};
use crate::hdf::{H5, Values};
use crate::{Decoded, Quantity, checked_points, clamp_rate};

pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    let h5 = H5::open(bytes)?;
    ensure!(h5.has_dataset("/cmorph"), "no cmorph variable: not a CMORPH2 file");
    let units = h5.attr_str("/cmorph", "units")?;
    ensure!(units == "mm/hr" || units == "mm/h", "cmorph units are {units:?}, expected mm/hr");

    let lat = coordinate(&h5, "/lat")?;
    let lon = coordinate(&h5, "/lon")?;
    let (lat0, dlat) = regular(&lat, "lat")?;
    let (lon0, dlon) = regular(&lon, "lon")?;
    ensure!(dlon > 0.0, "longitudes run westward");
    let (nx, ny) = (lon.len(), lat.len());
    checked_points("CMORPH2", nx, ny)?;
    let periodic = (dlon * nx as f64 - 360.0).abs() < 1e-6;

    let (raw, shape) = h5.read("/cmorph")?;
    ensure!(
        shape.len() == 3 && shape[0] == 1 && shape[1] == ny && shape[2] == nx,
        "cmorph has shape {shape:?}, expected (1, {ny}, {nx})"
    );
    ensure!(matches!(raw, Values::I16(_)), "cmorph is {}, expected int16", raw.type_name());
    let p = Packing::of(&h5, "/cmorph")?;
    let values =
        raw.map(|x| if Some(x) == p.fill || x < 0.0 { f32::NAN } else { clamp_rate((x * p.scale + p.offset) as f32) });

    let t = h5.read("/time")?.0.to_f64();
    ensure!(t.len() == 1, "time has {} values", t.len());
    let start = time_at(&h5.attr_str("/time", "units")?, t[0])?;
    let period = iso_duration(&h5.attr_str("/", "time_coverage_duration")?).context("time_coverage_duration")?;

    let proj = GridProjection::LatLon(LatLonGrid { lon0, lat0, dlon, dlat, periodic });
    let field = Field { nx, ny, proj, values, grid_relative_winds: false };
    let mut d = Decoded::observed(field, Quantity::RainRate, start + period);
    d.period = period;
    Ok(d)
}

fn coordinate(h5: &H5, path: &str) -> Result<Vec<f64>> {
    let (v, shape) = h5.read(path)?;
    ensure!(shape.len() == 1 && shape[0] >= 2, "{path} has shape {shape:?}");
    Ok(v.to_f64())
}

/// First value and step of an evenly spaced coordinate, refused when any
/// value is more than a hundredth of a step off the line.
fn regular(v: &[f64], name: &str) -> Result<(f64, f64)> {
    let n = v.len();
    let step = (v[n - 1] - v[0]) / (n - 1) as f64;
    ensure!(step.is_finite() && step != 0.0, "{name} is not a coordinate");
    for (i, &x) in v.iter().enumerate() {
        let expect = v[0] + i as f64 * step;
        ensure!((x - expect).abs() < 0.01 * step.abs(), "{name}[{i}] = {x}, not evenly spaced (expected {expect})");
    }
    // The stored float32 values are rounded; snap the step to the
    // nearest thousandth of a degree when it is that close.
    let snapped = (step * 1000.0).round() / 1000.0;
    let step = if (snapped - step).abs() < 1e-7 { snapped } else { step };
    Ok((v[0], step))
}
