//! Satellite decoders on real files: CMORPH2, GMGSI, GOES ABI.

mod common;

use chrono::TimeDelta;
use common::*;
use wx_formats::{Quantity, cmorph, gmgsi, goes};

#[test]
fn cmorph2_half_hour() {
    let d = cmorph::decode(&fixture("CMORPH2_0.25deg-30min_202611061700.RT.nc")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (1440, 720));
    assert_eq!(d.quantity, Quantity::RainRate);
    // The file is named after the start of its half hour.
    assert_eq!(d.valid, utc(2026, 11, 6, 17, 30, 0));
    assert_eq!(d.period, TimeDelta::minutes(30));
    value(f, 1363, 238, 62.77, 1e-4);
    value(f, 0, 0, 0.01, 1e-6);
    value(f, 720, 360, 1.34, 1e-5);
    value(f, 1000, 400, 0.0, 0.0);
    // Missing (-999) only along the top row.
    assert_eq!(count(f, f32::is_nan), 77);
    assert!((0..f.nx).filter(|&i| at(f, i, 719).is_nan()).count() == 77);
    assert_eq!(count(f, |v| v == 0.0), 791_165);
    // Cell centres from the lat and lon variables (lon 0..360, row 0 south).
    position(f, 0, 0, 0.125, -89.875, 1e-9);
    position(f, 1363, 238, 340.875 - 360.0, -30.375, 1e-9);
    position(f, 1439, 719, 359.875 - 360.0, 89.875, 1e-9);
    assert!(f.proj.is_periodic());
    let (i, _) = f.proj.lonlat_to_ij(-0.06, 0.0);
    assert!((i - 1439.26).abs() < 1e-9, "{i}");
}

#[test]
fn gmgsi_long_wave_ir() {
    let d = gmgsi::decode(&fixture("gmgsi_GLOBCOMPLIR_20261106T1700_rows700-1500_cols900-1700.nc")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (800, 800));
    assert_eq!(d.quantity, Quantity::BrightnessTemperature);
    assert_eq!(d.valid, utc(2026, 11, 6, 17, 0, 0));
    value(f, 0, 0, 235.0, 0.0);
    value(f, 799, 799, 295.0, 0.0);
    value(f, 123, 456, 295.5, 0.0);
    value(f, 700, 50, 279.5, 0.0);
    assert_eq!(count(f, f32::is_nan), 0);
    // The file's own lat and lon arrays, which are float32: 5e-5 degrees
    // is under a thousandth of a cell.
    position(f, 0, 0, -115.192_322, 49.759_842, 5e-5);
    position(f, 400, 300, -86.388_741, 33.785_118, 5e-5);
    position(f, 799, 799, -57.657_166, 0.000_015, 5e-5);
}

#[test]
fn gmgsi_no_coverage_is_missing_not_cold() {
    // 60 to 72.7 N, 20 to 100 E: no satellite sees it. Every dqf = 20 cell
    // holds count 255, which as data would be 163 K; row 0 has dqf fill.
    let d = gmgsi::decode(&fixture("gmgsi_GLOBCOMPLIR_20261106T1700_rows0-460_cols2700-3950_nocoverage.nc")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (1250, 460));
    value(f, 138, 67, f32::NAN, 0.0);
    value(f, 10, 0, f32::NAN, 0.0);
    value(f, 10, 1, 330.0 - 176.0 / 2.0, 0.0);
    value(f, 491, 355, 418.0 - 180.0, 0.0);
    assert_eq!(count(f, f32::is_nan), 128_985 + 1250);
    assert_eq!(count(f, |v| v < 165.0), 0, "no-coverage cells decoded as cloud tops");
    position(f, 138, 67, 24.361_036, 71.222_885, 5e-5);

    // dqf = 10: count 0, which as data would be 330 K.
    let d = gmgsi::decode(&fixture("gmgsi_GLOBCOMPLIR_20261106T1700_rows1790-1830_cols1530-1570_count0.nc")).unwrap();
    value(&d.field, 5, 7, f32::NAN, 0.0);
    value(&d.field, 23, 38, f32::NAN, 0.0);
    value(&d.field, 6, 7, 329.0, 0.0);
    assert_eq!(count(&d.field, f32::is_nan), 2);

    // Without dqf the reserved counts 0 and 255 are missing.
    let d = gmgsi::decode(&fixture("gmgsi_GLOBCOMPLIR_20261106T1700_rows0-120_cols2700-3100_nodqf.nc")).unwrap();
    value(&d.field, 110, 0, f32::NAN, 0.0);
    value(&d.field, 0, 1, 330.0 - 174.0 / 2.0, 0.0);
    assert_eq!(count(&d.field, f32::is_nan), 25_792);
}

#[test]
fn goes19_band13() {
    let d = goes::decode(&fixture("goes19_CMIPF_C13_s20263101810214_rows1200-1800_cols1800-2400.nc")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (600, 600));
    assert_eq!(d.quantity, Quantity::BrightnessTemperature);
    // The middle of the 18:10:21 to 18:19:53 scan.
    assert_eq!(d.valid, utc(2026, 11, 6, 18, 15, 7) + TimeDelta::milliseconds(410));
    value(f, 0, 0, 294.075_2, 1e-4);
    value(f, 599, 599, 293.583_57, 1e-4);
    value(f, 300, 300, 295.795_9, 1e-4);
    value(f, 150, 420, 299.667_45, 1e-4);
    // pyproj +proj=geos +sweep=x from the file's x and y.
    position(f, 0, 0, -94.964_214_178, 29.632_273_664, 1e-7);
    position(f, 599, 599, -80.948_696_222, 16.935_420_828, 1e-7);
    position(f, 300, 300, -87.291_447_080, 22.982_603_922, 1e-7);
}

#[test]
fn goes19_band13_at_the_limb() {
    let d = goes::decode(&fixture("goes19_CMIPF_C13_s20263101810214_rows2600-2800_cols0-300_limb.nc")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (300, 200));
    // Fill (65535, stored as -1) where the scan leaves the disk.
    value(f, 0, 0, f32::NAN, 0.0);
    assert_eq!(count(f, f32::is_nan), 190);
    value(f, 0, 111, 213.817_16, 1e-4);
    value(f, 299, 199, 280.063_84, 1e-4);
    position(f, 0, 111, -155.711_094_029, 0.010_413_645, 1e-6);
    position(f, 150, 0, -137.820_574_494, 2.217_135_586, 1e-7);
}
