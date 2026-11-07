//! GOES-R series ABI Level 2 Cloud and Moisture Imagery, one emissive band
//! (NetCDF-4), such as `ABI-L2-CMIPF/.../OR_ABI-L2-CMIPF-M6C13_G19_s...nc`.
//!
//! `CMI` is int16 flagged `_Unsigned = "true"`: stored counts 0 to 4095,
//! fill 65535 (written as -1), brightness temperature `count *
//! scale_factor + add_offset` in kelvin (band 13: 0.06145332 and 89.62).
//! Fill is off the earth's disk or a missing scan line and becomes `NaN`.
//! Reflective bands (1 to 6, units `1`) are refused: they are not
//! temperatures.
//!
//! The grid is the ABI fixed grid in the file's `goes_imager_projection`:
//! geostationary at `perspective_point_height` over
//! `longitude_of_projection_origin`, ellipsoid from its semi-axes, sweep
//! `x`; column `i` looks along scan angle `x = x_add_offset + i *
//! x_scale_factor` radians, row `j` along `y` likewise (`y` decreasing
//! southward), both at cell centres. A full disk is 5424 x 5424 at 2 km.
//!
//! `valid` is the middle of the scan (`t`); the disk is scanned from north
//! to south over the ten minutes around it.

use anyhow::{Result, ensure};
use wx_core::Field;
use wx_core::proj::{Earth, GeostationaryGrid, GridProjection, Sweep};

use crate::cf::{Packing, time_at};
use crate::hdf::{H5, Values};
use crate::{Decoded, Quantity, checked_points};

pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    let h5 = H5::open(bytes)?;
    ensure!(h5.has_dataset("/CMI"), "no CMI variable: not an ABI L2 CMIP file");
    let units = h5.attr_str("/CMI", "units")?;
    ensure!(units == "K", "CMI units are {units:?}: only emissive (kelvin) bands are supported");

    // Grid.
    let gp = "/goes_imager_projection";
    let mapping = h5.attr_str(gp, "grid_mapping_name")?;
    ensure!(mapping == "geostationary", "grid mapping is {mapping:?}");
    let lat0 = h5.attr_f64(gp, "latitude_of_projection_origin")?;
    ensure!(lat0 == 0.0, "latitude_of_projection_origin is {lat0}");
    let earth = Earth { a: h5.attr_f64(gp, "semi_major_axis")?, b: h5.attr_f64(gp, "semi_minor_axis")? };
    ensure!(earth.is_plausible(), "implausible ellipsoid {earth:?}");
    let height = h5.attr_f64(gp, "perspective_point_height")?;
    ensure!((3.0e7..4.0e7).contains(&height), "perspective_point_height {height}");
    let sub_lon = h5.attr_f64(gp, "longitude_of_projection_origin")?;
    ensure!((-180.0..=180.0).contains(&sub_lon), "longitude_of_projection_origin {sub_lon}");
    let sweep = match h5.attr_str(gp, "sweep_angle_axis")?.as_str() {
        "x" => Sweep::X,
        "y" => Sweep::Y,
        other => anyhow::bail!("sweep_angle_axis {other:?}"),
    };
    let (x_offset, x_scale, nx) = scan_axis(&h5, "/x")?;
    let (y_offset, y_scale, ny) = scan_axis(&h5, "/y")?;
    checked_points("ABI CMI", nx, ny)?;
    let proj = GridProjection::Geostationary(GeostationaryGrid::new(
        earth, height, sub_lon, sweep, x_scale, x_offset, y_scale, y_offset,
    ));

    // Values.
    let (raw, shape) = h5.read("/CMI")?;
    ensure!(shape == [ny, nx], "CMI has shape {shape:?}, x and y say {ny} x {nx}");
    let p = Packing::of(&h5, "/CMI")?;
    let unsigned = p.unsigned && matches!(raw, Values::I16(_) | Values::I8(_));
    let bits = if matches!(raw, Values::I8(_)) { 256.0 } else { 65536.0 };
    let fill = p.fill.map(|f| if unsigned && f < 0.0 { f + bits } else { f });
    let values = raw.map(|x| {
        let x = if unsigned && x < 0.0 { x + bits } else { x };
        if Some(x) == fill || x < 0.0 { f32::NAN } else { (x * p.scale + p.offset) as f32 }
    });

    let t = h5.read("/t")?.0.to_f64();
    ensure!(t.len() == 1, "t has {} values", t.len());
    let valid = time_at(&h5.attr_str("/t", "units")?, t[0])?;
    let field = Field { nx, ny, proj, values, grid_relative_winds: false };
    Ok(Decoded::observed(field, Quantity::BrightnessTemperature, valid))
}

/// Offset and step, radians, and length of a scan-angle coordinate whose
/// stored integers count up by a constant step.
fn scan_axis(h5: &H5, path: &str) -> Result<(f64, f64, usize)> {
    let (raw, shape) = h5.read(path)?;
    ensure!(shape.len() == 1 && shape[0] >= 2, "{path} has shape {shape:?}");
    let p = Packing::of(h5, path)?;
    let v = raw.to_f64();
    let step = v[1] - v[0];
    ensure!(step != 0.0, "{path} does not change");
    for (i, &x) in v.iter().enumerate() {
        ensure!(x == v[0] + i as f64 * step, "{path}[{i}] = {x} breaks the constant step {step}");
    }
    let scale = p.scale * step;
    let offset = p.offset + p.scale * v[0];
    ensure!(
        scale.abs() > 1e-7 && scale.abs() < 1e-2 && offset.abs() < 0.2,
        "{path}: scan angles {offset} + i * {scale}"
    );
    Ok((offset, scale, v.len()))
}
