//! Shared by the integration tests: fixture loading and checks.
//!
//! Expected values come from h5py, netCDF4-python, tifffile, Pillow and
//! pyproj 3.8 (PROJ 9.8) run on the same files; positions are cell centres
//! computed by pyproj from the file's own metadata.

#![allow(dead_code)]

use chrono::{DateTime, TimeZone, Utc};
use wx_formats::wx_core::Field;

pub fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/formats/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

pub fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
}

pub fn at(f: &Field, i: usize, j: usize) -> f32 {
    assert!(i < f.nx && j < f.ny, "({i}, {j}) outside {} x {}", f.nx, f.ny);
    f.values[j * f.nx + i]
}

/// Value at `(i, j)` equals `want` to `tol`, `NaN` matching `NaN`.
pub fn value(f: &Field, i: usize, j: usize, want: f32, tol: f32) {
    let got = at(f, i, j);
    if want.is_nan() {
        assert!(got.is_nan(), "({i}, {j}): {got}, expected NaN");
    } else {
        assert!((got - want).abs() <= tol, "({i}, {j}): {got}, expected {want}");
    }
}

/// Cell `(i, j)` is centred at `(lon, lat)` to `tol` degrees, and the
/// projection maps its own answer back to `(i, j)`.
pub fn position(f: &Field, i: usize, j: usize, lon: f64, lat: f64, tol: f64) {
    let (glon, glat) = f.proj.ij_to_lonlat(i as f64, j as f64);
    let dlon = (glon - lon + 540.0).rem_euclid(360.0) - 180.0;
    assert!(dlon.abs() <= tol && (glat - lat).abs() <= tol, "({i}, {j}) at ({glon}, {glat}), expected ({lon}, {lat})");
    let (gi, gj) = f.proj.lonlat_to_ij(glon, glat);
    assert!(
        (gi - i as f64).abs() <= 1e-4 && (gj - j as f64).abs() <= 1e-4,
        "({glon}, {glat}) maps back to ({gi}, {gj}), expected ({i}, {j})"
    );
}

pub fn count(f: &Field, p: impl Fn(f32) -> bool) -> usize {
    f.values.iter().filter(|&&v| p(v)).count()
}
