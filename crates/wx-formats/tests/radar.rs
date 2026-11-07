//! Radar decoders on real files: OPERA, DWD, DPC, JMA, CWA.

mod common;

use chrono::TimeDelta;
use common::*;
use wx_formats::{MAX_RAIN_RATE_MMH, NO_ECHO_DBZ, Quantity, cwa, dpc, dwd, jma, opera};

#[test]
fn opera_dbzh() {
    let d = opera::decode(&fixture("opera_20261106T1815_DBZH.h5")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (3800, 4400));
    assert_eq!(d.quantity, Quantity::Reflectivity);
    assert_eq!(d.valid, utc(2026, 11, 6, 18, 15, 0));
    assert_eq!((d.run, d.lead, d.period), (None, TimeDelta::zero(), TimeDelta::zero()));
    value(f, 3399, 3088, 80.5, 0.0);
    value(f, 1890, 2850, 9.0, 0.0);
    value(f, 1700, 2700, 0.0, 0.0);
    // Covered and empty versus not covered at all.
    value(f, 1900, 2200, NO_ECHO_DBZ, 0.0);
    value(f, 0, 0, f32::NAN, 0.0);
    assert_eq!(count(f, f32::is_nan), 8_319_554);
    // 7 419 164 undetect cells and 39 echoes of exactly -32 dBZ.
    assert_eq!(count(f, |v| v == NO_ECHO_DBZ), 7_419_164 + 39);
    // pyproj, cell centres from the outer upper-left corner.
    position(f, 0, 0, -39.519_134_847, 67.022_719_193, 1e-8);
    position(f, 3399, 3088, 28.347_803_112, 44.434_412_707, 1e-8);
    position(f, 1627, 2526, 5.397_383_508, 51.072_897_419, 1e-8);
    // The Helchteren radar (RMI, 51.0692 N 5.4058 E) sits in its own blind
    // zone: seven undetect cells amid echoes. Its centroid is 0.30 cells
    // from the radar here, and would be 0.68 cells away under the GeoTIFF's
    // placement, half a cell north-west.
    let hole = [(1627, 2525), (1627, 2526), (1628, 2526), (1627, 2527), (1628, 2527), (1627, 2528), (1628, 2528)];
    for &(i, j) in &hole {
        value(f, i, j, NO_ECHO_DBZ, 0.0);
    }
    for (i, j) in [(1627, 2524), (1626, 2526), (1629, 2526), (1626, 2528), (1629, 2527), (1628, 2529)] {
        assert!(at(f, i, j) > NO_ECHO_DBZ, "({i}, {j}) should be an echo around the hole");
    }
    let ci = hole.iter().map(|h| h.0 as f64).sum::<f64>() / 7.0;
    let cj = hole.iter().map(|h| h.1 as f64).sum::<f64>() / 7.0;
    let (si, sj) = f.proj.lonlat_to_ij(5.4058, 51.0692);
    let here = (ci - si).hypot(cj - sj);
    let tiff = (ci - si - 0.5).hypot(cj - sj - 0.5);
    assert!(
        here < 0.35 && tiff > 0.6,
        "blind zone {here:.2} cells from the radar, {tiff:.2} under the GeoTIFF placement"
    );
}

#[test]
fn opera_rate_is_clamped() {
    let d = opera::decode(&fixture("opera_20261106T1800_RATE.h5")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (1900, 2200));
    assert_eq!(d.quantity, Quantity::RainRate);
    assert_eq!(d.valid, utc(2026, 11, 6, 18, 0, 0));
    assert_eq!(d.period, TimeDelta::zero());
    // 612.15 mm/h in the file.
    value(f, 1698, 1539, MAX_RAIN_RATE_MMH, 0.0);
    assert_eq!(count(f, |v| v == MAX_RAIN_RATE_MMH), 2);
    value(f, 850, 1400, 0.21, 1e-6);
    value(f, 950, 1430, 0.0, 0.0);
    value(f, 0, 0, f32::NAN, 0.0);
    assert_eq!(count(f, |v| v > 0.0 && v < MAX_RAIN_RATE_MMH), 127_549);
    position(f, 0, 0, -39.502_483_656, 67.022_603_895, 1e-8);
    position(f, 850, 1400, 6.624_478_268, 48.643_923_276, 1e-8);
}

#[test]
fn opera_acrr_period_comes_from_the_file() {
    let d = opera::decode(&fixture("opera_20261106T1800_ACRR.h5")).unwrap();
    let f = &d.field;
    assert_eq!(d.quantity, Quantity::Accumulation);
    assert_eq!(d.valid, utc(2026, 11, 6, 18, 0, 0));
    assert_eq!(d.period, TimeDelta::hours(1));
    value(f, 1698, 1539, 171.06, 1e-4);
    value(f, 850, 1400, 0.54, 1e-6);
    value(f, 950, 1430, 0.0, 0.0);
    value(f, 633, 550, f32::NAN, 0.0);
    position(f, 1899, 2199, 29.413_120_064, 31.998_875_155, 1e-8);
    position(f, 633, 550, -3.712_828_649, 63.357_862_821, 1e-8);
}

#[test]
fn dwd_hx_250m() {
    let d = dwd::decode(&fixture("dwd_composite_hx_20261106_1815-hd5")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (4400, 4800));
    assert_eq!(d.quantity, Quantity::Reflectivity);
    assert_eq!(d.valid, utc(2026, 11, 6, 18, 15, 0));
    assert_eq!(d.run, None);
    value(f, 1091, 2704, 64.370_13, 1e-4);
    value(f, 1500, 3800, 16.675_568, 1e-4);
    value(f, 1200, 3700, 6.107_701, 1e-4);
    value(f, 2200, 2400, NO_ECHO_DBZ, 0.0);
    value(f, 0, 0, f32::NAN, 0.0);
    let n = (f.nx * f.ny) as f64;
    assert!((count(f, f32::is_nan) as f64 / n - 0.472_106).abs() < 1e-6);
    assert!((count(f, |v| v == NO_ECHO_DBZ) as f64 / n - 0.483_904).abs() < 1e-6);
    position(f, 0, 0, 1.465_525_445, 55.861_162_835, 1e-8);
    position(f, 1500, 3800, 7.888_532_274, 48.008_986_187, 1e-8);
    position(f, 4399, 4799, 16.579_572_672, 45.685_751_637, 1e-8);
}

#[test]
fn dwd_ry_rate() {
    let d = dwd::decode(&fixture("dwd_raa01-ry_20261106_1815-hdf5")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (1100, 1200));
    assert_eq!(d.quantity, Quantity::RainRate);
    assert_eq!(d.valid, utc(2026, 11, 6, 18, 15, 0));
    value(f, 303, 273, 47.38, 1e-4);
    value(f, 550, 600, 0.0, 0.0);
    value(f, 0, 0, f32::NAN, 0.0);
    position(f, 0, 0, 1.472_196_485, 55.858_389_754, 1e-8);
    position(f, 303, 273, 6.471_685_116, 53.759_661_625, 1e-8);
    // The 1 km and 250 m grids share their outer corner.
    let hx = dwd::decode(&fixture("dwd_composite_hx_20261106_1815-hd5")).unwrap().field;
    let a = f.proj.ij_to_lonlat(-0.5, -0.5);
    let b = hx.proj.ij_to_lonlat(-0.5, -0.5);
    // ry states its corners to 1e-8 degrees, hx to 1e-15.
    assert!((a.0 - b.0).abs() < 1e-7 && (a.1 - b.1).abs() < 1e-7, "{a:?} vs {b:?}");
}

#[test]
fn dwd_wn_nowcast_tar() {
    let steps = dwd::decode_tar(&fixture("dwd_composite_wn_20261106_1815_cut4of25.tar")).unwrap();
    let leads: Vec<i64> = steps.iter().map(|s| s.lead.num_minutes()).collect();
    assert_eq!(leads, [0, 5, 60, 120]);
    for s in &steps {
        assert_eq!(s.run, Some(utc(2026, 11, 6, 18, 15, 0)));
        assert_eq!(s.valid, utc(2026, 11, 6, 18, 15, 0) + s.lead);
        assert_eq!(s.quantity, Quantity::Reflectivity);
        assert_eq!((s.field.nx, s.field.ny), (1100, 1200));
        value(&s.field, 300, 950, NO_ECHO_DBZ, 0.0);
        value(&s.field, 0, 0, f32::NAN, 0.0);
    }
    value(&steps[0].field, 272, 676, 64.370_13, 1e-4);
    value(&steps[0].field, 380, 920, NO_ECHO_DBZ, 0.0);
    value(&steps[1].field, 380, 920, -1.773_519, 1e-4);
    value(&steps[2].field, 380, 920, 8.721_102, 1e-4);
    value(&steps[3].field, 380, 920, 17.076_954, 1e-4);
    assert_eq!(count(&steps[3].field, f32::is_nan), 633_625);
    position(&steps[2].field, 0, 0, 1.472_196_485, 55.858_389_754, 1e-8);
    position(&steps[2].field, 303, 273, 6.471_685_116, 53.759_661_625, 1e-8);
}

#[test]
fn dpc_sri_and_vmi() {
    let d = dpc::decode(&fixture("dpc_SRI_20261106_1820.tif")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (1200, 1400));
    assert_eq!(d.quantity, Quantity::RainRate);
    assert_eq!(d.valid, utc(2026, 11, 6, 18, 20, 0));
    value(f, 618, 614, 39.865, 1e-4);
    value(f, 600, 650, 0.0, 0.0);
    value(f, 0, 0, f32::NAN, 0.0);
    assert_eq!(count(f, f32::is_nan), 739_288);
    assert_eq!(count(f, |v| v == 0.0), 805_060);
    // pyproj: +proj=tmerc +lat_0=42 +lon_0=12.5 +k=1 +ellps=WGS84.
    position(f, 0, 0, 4.530_652_401, 47.566_868_978, 1e-8);
    position(f, 1199, 1399, 19.067_792_900, 35.069_859_247, 1e-8);
    position(f, 618, 614, 12.724_418_699, 42.319_380_279, 1e-8);

    let d = dpc::decode(&fixture("dpc_VMI_20261106_1820.tif")).unwrap();
    let f = &d.field;
    assert_eq!(d.quantity, Quantity::Reflectivity);
    value(f, 872, 1282, 45.0, 0.0);
    value(f, 600, 650, NO_ECHO_DBZ, 0.0);
    value(f, 0, 0, f32::NAN, 0.0);
    assert_eq!(count(f, f32::is_nan), 760_221);
    assert_eq!(count(f, |v| v == NO_ECHO_DBZ), 813_229);
    position(f, 480, 260, 10.971_169_132, 45.495_384_066, 1e-8);
}

#[test]
fn jma_one_tile() {
    let tile = fixture("jma_hrpns_20261106T1820_6_56_25.png");
    let f = jma::decode_tiles(6, 56, 25, 1, 1, &[Some(&tile)]).unwrap();
    assert_eq!((f.nx, f.ny), (256, 256));
    value(&f, 64, 0, 0.5, 0.0);
    value(&f, 65, 0, 3.0, 0.0);
    // Palette entry 1 (transparent, drawn around rain) and entry 0 are dry.
    value(&f, 0, 0, 0.0, 0.0);
    value(&f, 255, 255, 0.0, 0.0);
    for (rate, n) in [(0.5, 1266), (3.0, 670), (7.5, 69), (15.0, 49), (25.0, 19), (40.0, 11), (65.0, 1), (90.0, 0)] {
        assert_eq!(count(&f, |v| v == rate), n, "{rate} mm/h");
    }
    // pyproj, EPSG:3857 pixel centres.
    position(&f, 64, 0, 136.417_236_328, 36.589_068_371, 1e-8);
    position(&f, 255, 255, 140.614_013_672, 31.961_483_557, 1e-8);
}

#[test]
fn jma_outside_the_domain_and_missing_tiles_are_nan() {
    let tile = fixture("jma_hrpns_20261106T1820_4_14_6.png");
    let f = jma::decode_tiles(4, 14, 6, 1, 1, &[Some(&tile)]).unwrap();
    value(&f, 4, 46, 3.0, 0.0);
    assert_eq!(count(&f, |v| v == 90.0), 3);
    // 157.5 E is east of the nowcast domain.
    value(&f, 255, 255, f32::NAN, 0.0);
    position(&f, 4, 46, 135.395_507_812, 37.822_802_434, 1e-8);
    position(&f, 255, 255, 157.456_054_687, 21.983_801_417, 1e-8);

    let a = fixture("jma_hrpns_20261106T1820_8_226_100.png");
    let b = fixture("jma_hrpns_20261106T1820_8_227_100_empty.png");
    let f = jma::decode_tiles(8, 226, 100, 3, 1, &[Some(&a), Some(&b), None]).unwrap();
    assert_eq!((f.nx, f.ny), (768, 256));
    assert_eq!(count(&f, |v| v == 0.5), 24);
    // The empty RGBA tile is dry, the missing one unknown.
    value(&f, 300, 100, 0.0, 0.0);
    value(&f, 600, 100, f32::NAN, 0.0);
    let (lon, _) = f.proj.ij_to_lonlat(256.0 - 0.5, 0.0);
    assert!((lon - (227.0 / 256.0 * 360.0 - 180.0)).abs() < 1e-9, "tile edge at {lon}");
}

#[test]
fn jma_refuses_what_is_not_a_tile() {
    let tile = fixture("jma_hrpns_20261106T1820_6_56_25.png");
    assert!(jma::decode_tiles(6, 56, 25, 2, 1, &[Some(&tile)]).is_err());
    assert!(jma::decode_tiles(6, 63, 25, 2, 1, &[Some(&tile), None]).is_err());
    assert!(jma::decode_tiles(6, 56, 25, 1, 1, &[Some(b"<!DOCTYPE html>")]).is_err());
    assert!(jma::decode_tiles(6, 56, 25, 1, 1, &[Some(&tile[..700])]).is_err());
}

#[test]
fn cwa_radar_json() {
    let d = cwa::decode(&fixture("cwa_O-A0059-001_20261106T1820_cols420-660_rows0-300.json")).unwrap();
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (240, 300));
    assert_eq!(d.quantity, Quantity::Reflectivity);
    assert_eq!(d.valid, utc(2026, 11, 6, 18, 20, 0));
    value(f, 51, 292, 52.0, 0.0);
    value(f, 0, 0, NO_ECHO_DBZ, 0.0);
    value(f, 162, 0, f32::NAN, 0.0);
    assert_eq!(count(f, f32::is_nan), 1289);
    assert_eq!(count(f, |v| v == NO_ECHO_DBZ), 68_868);
    // pyproj TWD67 -> WGS84 with the 7-parameter Helmert; the decoder's
    // constant shift is within 0.0005 degrees of it everywhere.
    position(f, 0, 0, 120.257_852, 17.998_257, 5e-4);
    position(f, 239, 299, 123.245_483, 21.735_616, 5e-4);
    position(f, 51, 292, 120.895_517, 21.648_225, 5e-4);
}
