//! Italian Civil Protection (DPC) national radar mosaic (GeoTIFF, CC BY-SA
//! 4.0), from `radar-api.protezionecivile.it`.
//!
//! | Product | Quantity | Values |
//! |---|---|---|
//! | `SRI` surface rainfall intensity | mm/h | -9999 outside, 0 dry, > 0 rain |
//! | `VMI` vertical maximum intensity | dBZ | -9999 outside, -9998 no echo, 10 to 45 by 0.5 |
//!
//! One float32 band of 1200 x 1400 cells, LZW strips. Any other negative
//! value is treated as missing.
//!
//! # Grid
//!
//! The GeoKeys state a projected CRS with no EPSG code: transverse
//! Mercator (`ProjCoordTrans = 1`) with natural origin 42 N 12.5 E on WGS84
//! (`GeographicType = 4326`), no scale factor (so 1), no false offsets,
//! metres. `ModelTiepoint (0, 0) -> (-600000, 650000)` with `PixelIsArea`
//! and 1000 m cells: the mosaic spans x -600 to +600 km and y +650 to -750
//! km about the origin, its first cell centred at (-599500, 649500). Read
//! literally through [`geotiff`](crate::geotiff); the tests check it
//! against the OPERA composite over the Alps, where both see the same rain.
//!
//! # Time
//!
//! The file's `DocumentName` is the producer's path, ending
//! `SRI_adj_DD-MM-YYYY_HH-MM.tif` or `VMI_DD-MM-YYYY_HH-MM.tif`, in UTC;
//! it gives both the product and the valid time. [`decode_as`] takes them
//! from the caller instead, for a file without one.

use anyhow::{Context, Result, anyhow, ensure};
use chrono::{DateTime, NaiveDateTime, Utc};
use wx_core::Field;

use crate::geotiff;
use crate::{Decoded, NO_ECHO_DBZ, Quantity, clamp_rate};

/// Which DPC product a file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    /// Surface rainfall intensity, mm/h.
    Sri,
    /// Vertical maximum intensity, dBZ.
    Vmi,
}

const OUTSIDE: f32 = -9999.0;
const NO_ECHO: f32 = -9998.0;

/// Decode a DPC GeoTIFF whose `DocumentName` names the product and time.
pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    let tif = geotiff::read(bytes)?;
    let name = tif.document_name.clone().ok_or_else(|| anyhow!("no DocumentName: use decode_as"))?;
    let (product, valid) = parse_document_name(&name)?;
    finish(tif, product, valid)
}

/// Decode a DPC GeoTIFF as `product` valid at `valid`.
pub fn decode_as(bytes: &[u8], product: Product, valid: DateTime<Utc>) -> Result<Decoded> {
    finish(geotiff::read(bytes)?, product, valid)
}

fn finish(tif: geotiff::GeoTiff, product: Product, valid: DateTime<Utc>) -> Result<Decoded> {
    ensure!(tif.bands.len() == 1, "{} bands, expected 1", tif.bands.len());
    ensure!(
        matches!(tif.def.kind, crate::projdef::Kind::TransverseMercator { .. }),
        "projection {:?}, expected DPC's transverse Mercator",
        tif.def.kind
    );
    let band = tif.bands.into_iter().next().expect("one band");
    let values: Vec<f32> = match product {
        Product::Sri => band
            .into_iter()
            .map(|v| {
                if v == NO_ECHO {
                    0.0
                } else if v < 0.0 || !v.is_finite() {
                    f32::NAN
                } else {
                    clamp_rate(v)
                }
            })
            .collect(),
        Product::Vmi => band
            .into_iter()
            .map(|v| {
                if v == NO_ECHO {
                    NO_ECHO_DBZ
                } else if v == OUTSIDE || !v.is_finite() || v < -100.0 {
                    f32::NAN
                } else {
                    v
                }
            })
            .collect(),
    };
    let quantity = match product {
        Product::Sri => Quantity::RainRate,
        Product::Vmi => Quantity::Reflectivity,
    };
    let field = Field { nx: tif.nx, ny: tif.ny, proj: tif.proj, values, grid_relative_winds: false };
    Ok(Decoded::observed(field, quantity, valid))
}

/// Product and time from a path such as
/// `/data2/RADAR/TIF/2026/11/06/SRI_adj/SRI_adj_06-11-2026_18-20.tif`.
fn parse_document_name(name: &str) -> Result<(Product, DateTime<Utc>)> {
    let base = name.rsplit('/').next().unwrap_or(name);
    let stem = base.strip_suffix(".tif").or_else(|| base.strip_suffix(".tiff")).unwrap_or(base);
    let product = if stem.starts_with("SRI") {
        Product::Sri
    } else if stem.starts_with("VMI") {
        Product::Vmi
    } else {
        anyhow::bail!("DocumentName {name:?} names neither SRI nor VMI");
    };
    let stamp =
        stem.len().checked_sub(16).map(|i| &stem[i..]).ok_or_else(|| anyhow!("DocumentName {name:?} has no time"))?;
    let t = NaiveDateTime::parse_from_str(stamp, "%d-%m-%Y_%H-%M")
        .with_context(|| format!("DocumentName {name:?}: time {stamp:?} is not DD-MM-YYYY_HH-MM"))?;
    Ok((product, t.and_utc()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn document_names() {
        let (p, t) = parse_document_name("/data2/RADAR/TIF/2026/11/06/SRI_adj/SRI_adj_06-11-2026_18-20.tif").unwrap();
        assert_eq!(p, Product::Sri);
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 6, 18, 20, 0).unwrap());
        let (p, _) = parse_document_name("/data2/RADAR/TIF/2026/11/06/VMI/VMI_06-11-2026_18-20.tif").unwrap();
        assert_eq!(p, Product::Vmi);
        for bad in ["IDL TIFF file", "SRI.tif", "SRI_adj_31-02-2026_18-20.tif", "HRD_06-11-2026_18-20.tif"] {
            assert!(parse_document_name(bad).is_err(), "{bad}");
        }
    }
}
