//! NOAA/NESDIS Global Mosaic of Geostationary Satellite Imagery (GMGSI),
//! long-wave infrared (NetCDF-4).
//!
//! Hourly, `GMGSI_LW/.../GLOBCOMPLIR_v3r0_blend_sYYYYMMDDHH00000_...nc`:
//! Meteosat-10, Meteosat-9 IODC, Himawari-9, GOES-18 and GOES-19 blended on
//! one grid from 72.7 S to 72.7 N. `data` is float32 `(1, 3000, 4999)`
//! holding 0 to 255 display counts, cold clouds high, although its `units`
//! attribute says K.
//!
//! # Missing cells
//!
//! `dqf` (int8, same shape) is the "Data Quality Flag". Its attributes give
//! only that name and `_FillValue = -128`, no flag meanings; across the four
//! channels of the 2026-11-06 17Z files its values behave as:
//!
//! | `dqf` | Where | Counts |
//! |---|---|---|
//! | 0 | everywhere with data | 1 to 253, never 0 or 255 |
//! | 10 | a few cells (LW, SW), every night-side cell (VIS) | always 0 |
//! | 20 | no satellite sees the cell: 60 to 72.7 N between 20 and 100 E (the gap between Meteosat IODC and Himawari), and some far-southern cells | always 255 |
//! | -128 (fill) | exactly row 0 | assorted |
//!
//! So a cell is `NaN` unless `dqf` is 0. Read as data, the 255s would be
//! 163 K, the coldest cloud tops on the map. A file without `dqf` has its
//! counts 0 and 255 taken as missing instead.
//!
//! # Counts to brightness temperature
//!
//! ```text
//! T = 330 - C / 2   for C <= 176   (330 K down to 242 K, 0.5 K per count)
//! T = 418 - C       for C >  176   (241 K down to 163 K, 1 K per count)
//! ```
//!
//! This is the classic NOAA 8-bit IR scale with its two segments swapped;
//! the classic assignment (`418 - C` for the warm end) is up to 70 K too
//! warm here. Established against GOES-19 band 13 (10.3 um) 70 minutes
//! later at 450 000 pixels: from 250 to 315 K (counts 30 to 160, nearly all
//! clear-sky and low-cloud scenes) the median GOES temperature at each count
//! and the median count at each GOES temperature both follow the formula
//! within 1 to 2 K, and the decoded mosaic's median difference from GOES
//! over 200 000 such cells is +0.01 K (an ignored test redoes this). Colder
//! than 242 K the comparison only brackets the formula: at count 200 the
//! two regressions give 206 and 222 K against its 218 K, the spread being
//! cloud tops that changed within the hour, and the median difference below
//! 230 K is -5.7 K. Treat the warm segment as calibrated and the cold one
//! as good to about 5 K.
//!
//! # Grid
//!
//! Spherical Mercator, rows north to south, read from the file's 2-D `lat`
//! and `lon` variables rather than its rounded attributes: columns are
//! 0.0720090 degrees apart from 180.0004 W, rows evenly spaced in Mercator
//! y by the same angle, the equator on row 1499. The grid is checked to be
//! exactly that (every column within a hundredth of a cell of the fit) and
//! refused otherwise. 4999 columns span 359.973 degrees, leaving a gap of
//! 0.027 degrees at the antimeridian, where the projection's cut is put.

use anyhow::{Result, ensure};
use std::f64::consts::PI;
use wx_core::Field;
use wx_core::proj::{Earth, GridProjection, MercatorGrid, wrap_lon};

use hdf5_reader::SliceInfoElem;

use crate::cf::time_at;
use crate::hdf::H5;
use crate::{Decoded, Quantity, checked_points};

/// Count to brightness temperature, kelvin.
pub fn count_to_kelvin(c: f32) -> f32 {
    if c <= 176.0 { 330.0 - 0.5 * c } else { 418.0 - c }
}

/// Decode a GMGSI long-wave IR file to brightness temperature in kelvin.
pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    let h5 = H5::open(bytes)?;
    let title = h5.attr_str("/", "title")?;
    ensure!(title == "GLOBCOMPLIR", "title is {title:?}: only the long-wave IR mosaic (GLOBCOMPLIR) is calibrated");
    let shape = h5.dataset("/data")?.shape().to_vec();
    ensure!(shape.len() == 3 && shape[0] == 1, "data has shape {shape:?}, expected (1, rows, cols)");
    let (ny, nx) = (shape[1] as usize, shape[2] as usize);
    checked_points("GMGSI", nx, ny)?;
    // The grid first: its 2-D coordinate arrays are gone before the data
    // is read.
    let proj = grid(&h5, nx, ny)?;
    let (raw, shape) = h5.read("/data")?;
    ensure!(shape == [1, ny, nx], "data has shape {shape:?}, expected (1, {ny}, {nx})");
    let fill = h5.attr_f64_opt("/data", "_FillValue")?;
    let mut values =
        raw.map(|x| if Some(x) == fill || !(0.0..=255.0).contains(&x) { f32::NAN } else { count_to_kelvin(x as f32) });
    if h5.has_dataset("/dqf") {
        let (dqf, dshape) = h5.read("/dqf")?;
        ensure!(dshape == shape, "dqf has shape {dshape:?}, data {shape:?}");
        for (i, v) in values.iter_mut().enumerate() {
            if dqf.at(i) != 0.0 {
                *v = f32::NAN;
            }
        }
    } else {
        // Without a flag the reserved counts are all there is to go on.
        for (i, v) in values.iter_mut().enumerate() {
            let c = raw.at(i);
            if c == 0.0 || c == 255.0 {
                *v = f32::NAN;
            }
        }
    }
    drop(raw);

    let t = h5.read("/time")?.0.to_f64();
    ensure!(t.len() == 1, "time has {} values", t.len());
    let valid = time_at(&h5.attr_str("/time", "units")?, t[0])?;
    let field = Field { nx, ny, proj, values, grid_relative_winds: false };
    Ok(Decoded::observed(field, Quantity::BrightnessTemperature, valid))
}

/// The Mercator grid the `lat` and `lon` variables describe. Of their 2-D
/// arrays only the rows and columns checked are kept.
fn grid(h5: &H5, nx: usize, ny: usize) -> Result<GridProjection> {
    for path in ["/lon", "/lat"] {
        let shape = h5.dataset(path)?.shape().to_vec();
        ensure!(shape == [ny as u64, nx as u64], "{path} has shape {shape:?}, data is {ny} x {nx}");
    }
    // Indices `start..end` every `step`, as a selection.
    let range = |start: usize, end: usize, step: usize| SliceInfoElem::Slice {
        start: start as u64,
        end: end as u64,
        step: step.max(1) as u64,
    };
    // Rows 0 and ny / 2 (and ny - 1 too when ny is odd), then row ny - 1.
    let lon = h5.read_slice("/lon", &[range(0, ny, ny / 2), range(0, nx, 1)])?;
    let last = h5.read_slice("/lon", &[range(ny - 1, ny, 1), range(0, nx, 1)])?;
    ensure!(lon.len() >= nx && last.len() == nx, "lon came back short");
    // Longitudes of row 0, unwrapped so they increase steadily.
    let mut cols = Vec::with_capacity(nx);
    cols.push(lon.at(0));
    for i in 1..nx {
        let prev = cols[i - 1];
        cols.push(prev + wrap_lon(lon.at(i) - prev));
    }
    // Row ny / 2 is the second of `lon`'s, unless it is row 0.
    for (row, j) in [(&lon, usize::from(ny / 2 > 0)), (&last, 0)] {
        for i in (0..nx).step_by(97) {
            ensure!(row.at(j * nx + i) == lon.at(i), "lon varies down column {i}: not a Mercator grid");
        }
    }
    drop((lon, last));
    let (lon_first, dlon) = fit(&cols, "lon")?;
    ensure!(dlon > 0.0 && dlon * nx as f64 <= 360.0 + 1e-6, "columns are {dlon} degrees apart");

    // Columns 0 and nx / 2 (and nx - 1 too when nx is odd); column nx / 2
    // is the second, unless it is column 0.
    let lat = h5.read_slice("/lat", &[range(0, ny, 1), range(0, nx, nx / 2)])?;
    let per_row = lat.len() / ny;
    ensure!(per_row >= 1 && lat.len() == per_row * ny, "lat came back short");
    let middle = usize::from(nx / 2 > 0);
    let rows: Vec<f64> = (0..ny).map(|j| lat.at(j * per_row)).collect();
    for j in (0..ny).step_by(89) {
        ensure!(lat.at(j * per_row + middle) == rows[j], "lat varies along row {j}: not a Mercator grid");
    }
    ensure!(rows.iter().all(|l| l.abs() < 89.0), "latitudes reach a pole: not a Mercator grid");
    let ys: Vec<f64> = rows.iter().map(|l| (PI / 4.0 + l.to_radians() / 2.0).tan().ln()).collect();
    let (y_first, dy) = fit(&ys, "lat")?;
    // Mercator is conformal: rows are as far apart, in radians of Mercator
    // y, as columns are in radians of longitude.
    ensure!(
        (dy.abs() - dlon.to_radians()).abs() < 1e-4 * dy.abs(),
        "row spacing {dy} rad is not the column spacing {} rad: not a Mercator grid",
        dlon.to_radians()
    );

    // Put the cut (the meridian opposite lon0) in the middle of the gap
    // between the last column and the first.
    let gap = 360.0 - dlon * nx as f64;
    let cut = lon_first + (nx as f64 - 0.5) * dlon + 0.5 * gap;
    let lon0 = wrap_lon(cut - 180.0);
    let r = 6_378_137.0;
    let x0 = r * wrap_lon(lon_first - lon0).to_radians();
    Ok(GridProjection::Mercator(MercatorGrid::from_projected(
        Earth::sphere(r),
        0.0,
        lon0,
        x0,
        r * y_first,
        r * dlon.to_radians(),
        r * dy,
    )))
}

/// Least-squares line through `v[i]`, refused when any point is more than a
/// hundredth of a step off it.
fn fit(v: &[f64], name: &str) -> Result<(f64, f64)> {
    let n = v.len() as f64;
    ensure!(v.len() >= 2 && v.iter().all(|x| x.is_finite()), "{name} is not a coordinate");
    let mean_i = (n - 1.0) / 2.0;
    let mean_v = v.iter().sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (i, &x) in v.iter().enumerate() {
        let di = i as f64 - mean_i;
        sxy += di * (x - mean_v);
        sxx += di * di;
    }
    let step = sxy / sxx;
    let first = mean_v - step * mean_i;
    ensure!(step != 0.0, "{name} does not change");
    let worst = v.iter().enumerate().map(|(i, &x)| (x - (first + step * i as f64)).abs()).fold(0.0, f64::max);
    ensure!(worst < 0.01 * step.abs(), "{name} is not evenly spaced: {worst} off a step of {step}");
    Ok((first, step))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_to_kelvin() {
        assert_eq!(count_to_kelvin(0.0), 330.0);
        assert_eq!(count_to_kelvin(80.0), 290.0);
        assert_eq!(count_to_kelvin(176.0), 242.0);
        assert_eq!(count_to_kelvin(177.0), 241.0);
        assert_eq!(count_to_kelvin(255.0), 163.0);
    }
}
