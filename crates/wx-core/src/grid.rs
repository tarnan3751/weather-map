//! Decoded gridded data, before it is cut into tiles.

use crate::proj::GridProjection;

/// One decoded GRIB2 message.
#[derive(Debug, Clone)]
pub struct Field {
    pub nx: usize,
    pub ny: usize,
    pub proj: GridProjection,
    /// `values[j * nx + i]` in the file's native units; `NaN` where missing.
    pub values: Vec<f32>,
    /// True when vector components are relative to the grid's own axes and
    /// need [`GridProjection::wind_rotation`] to become east/north.
    pub grid_relative_winds: bool,
}

/// A layer's bands on their source grid, in the layer's stored units
/// (Celsius, m/s, mm, ...). `NaN` where missing. This is what the tiler samples.
#[derive(Debug, Clone)]
pub struct Raster {
    pub nx: usize,
    pub ny: usize,
    pub proj: GridProjection,
    /// One `Vec` of `nx * ny` values per band, in `LayerSpec::bands` order.
    pub bands: Vec<Vec<f32>>,
}

impl Raster {
    pub fn len(&self) -> usize {
        self.nx * self.ny
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
