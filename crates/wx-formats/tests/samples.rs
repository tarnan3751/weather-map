//! The full-size research samples, too big to keep as fixtures. Ignored by
//! default; run with the directory that holds them:
//!
//! ```text
//! WX_FORMATS_SAMPLES=/path/to/research cargo test --release -p wx-formats --test samples -- --ignored --nocapture
//! ```
//!
//! The directory has `radar/{opera,dwd,dpc,smhi}`, `satellite/{gmgsi,goes}`
//! and `canada-world/cwa` as the research notes describe. Decode times are
//! printed; they mean something only in a release build.

mod common;

use std::path::PathBuf;
use std::time::Instant;

use chrono::TimeDelta;
use common::*;
use wx_formats::{NO_ECHO_DBZ, Quantity, cwa, dwd, gmgsi, goes, odim, opera};

fn sample(rel: &str) -> Option<Vec<u8>> {
    let Some(dir) = std::env::var_os("WX_FORMATS_SAMPLES") else {
        eprintln!("WX_FORMATS_SAMPLES not set: skipped");
        return None;
    };
    let path = PathBuf::from(dir).join(rel);
    Some(std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    eprintln!("{what}: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    out
}

#[test]
#[ignore]
fn dwd_wn_all_25_steps() {
    let Some(b) = sample("radar/dwd/composite_wn__LATEST.tar") else { return };
    let steps = timed("DWD wn tar, 25 x 1100 x 1200", || dwd::decode_tar(&b).unwrap());
    let leads: Vec<i64> = steps.iter().map(|s| s.lead.num_minutes()).collect();
    assert_eq!(leads, (0..=120).step_by(5).collect::<Vec<_>>());
    for s in &steps {
        assert_eq!(s.run, Some(utc(2026, 11, 6, 18, 15, 0)));
        assert_eq!(s.valid, utc(2026, 11, 6, 18, 15, 0) + s.lead);
    }
    value(&steps[24].field, 380, 920, 17.076_954, 1e-4);
}

#[test]
#[ignore]
fn dwd_rv_accumulation_steps() {
    let Some(b) = sample("radar/dwd/composite_rv_LATEST.tar") else { return };
    let steps = dwd::decode_tar(&b).unwrap();
    assert_eq!(steps.len(), 25);
    for s in &steps {
        assert_eq!(s.quantity, Quantity::Accumulation);
        assert_eq!(s.period, TimeDelta::minutes(5));
        assert_eq!(s.run, Some(utc(2026, 11, 6, 18, 10, 0)));
    }
    // Step 0 is the observed 18:05 to 18:10, step 1 the forecast 18:10 to 18:15.
    assert_eq!(steps[0].valid, utc(2026, 11, 6, 18, 10, 0));
    assert_eq!(steps[1].valid, utc(2026, 11, 6, 18, 15, 0));
    assert_eq!(steps[24].lead, TimeDelta::minutes(120));
    value(&steps[1].field, 380, 920, 0.001, 1e-6);
    value(&steps[24].field, 380, 920, 0.027, 1e-6);
}

#[test]
#[ignore]
fn opera_and_dwd_hx_timing() {
    let Some(b) = sample("radar/opera/opera_DBZH.h5") else { return };
    let d = timed("OPERA DBZH 3800 x 4400 float64", || opera::decode(&b).unwrap());
    assert_eq!(d.field.values.len(), 16_720_000);
    let Some(b) = sample("radar/dwd/composite_hx_LATEST-hd5") else { return };
    let d = timed("DWD hx 4400 x 4800 uint16", || dwd::decode(&b).unwrap());
    assert_eq!(d.field.values.len(), 21_120_000);
}

#[test]
#[ignore]
fn goes_full_disks() {
    for (name, sub_lon, raw_centre, t) in [
        ("satellite/goes/CMIPF_C13_G19_s20263101810214.nc", -75.0, 244.482_37, 847_260_907.409_681),
        ("satellite/goes/CMIPF_C13_G18_s20263101810223.nc", -137.0, 234.649_84, 847_260_908.348_454),
    ] {
        let Some(b) = sample(name) else { return };
        let d = timed(&format!("GOES C13 full disk 5424 x 5424, {name}"), || goes::decode(&b).unwrap());
        let f = &d.field;
        assert_eq!((f.nx, f.ny), (5424, 5424));
        let j2000 = utc(2000, 1, 1, 12, 0, 0);
        assert_eq!(d.valid, j2000 + TimeDelta::milliseconds((t * 1000.0_f64).round() as i64));
        value(f, 2712, 2712, raw_centre, 1e-4);
        // Off the disk is NaN: about 6.37 million cells, the corners of the square.
        let nan = count(f, f32::is_nan);
        assert!((6_373_600..6_373_700).contains(&nan), "{nan}");
        // pyproj +proj=geos +sweep=x; the second satellite is the first moved 62 degrees.
        position(f, 2712, 2712, sub_lon + 0.009_003_520, -0.009_064_199, 1e-7);
        position(f, 1000, 1500, sub_lon - 38.316_240_069, 23.756_194_109, 1e-7);
        position(f, 4000, 4500, sub_lon + 32.562_752_926, -36.853_538_848, 1e-7);
    }
}

#[test]
#[ignore]
fn gmgsi_full_grid_matches_its_lat_lon_arrays() {
    let Some(b) =
        sample("satellite/gmgsi/GLOBCOMPLIR_v3r0_blend_s202611061700000_e202611061709599_c202611061733476.nc")
    else {
        return;
    };
    let d = timed("GMGSI LW 4999 x 3000", || gmgsi::decode(&b).unwrap());
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (4999, 3000));
    value(f, 2500, 1499, 330.0 - 81.0 / 2.0, 0.0);
    value(f, 100, 100, 330.0 - 152.0 / 2.0, 0.0);
    value(f, 4900, 2900, 418.0 - 197.0, 0.0);
    // Missing wherever dqf is not 0: row 0 (fill), 129 007 no-coverage
    // cells (count 255) and two count-0 cells.
    assert_eq!(count(f, f32::is_nan), 4999 + 129_007 + 2);
    assert_eq!(count(f, |v| v < 165.0), 0);
    // Cell centres against NOAA's own float32 lat and lon arrays (read with
    // h5py), within 5e-5 degrees: under a thousandth of a cell.
    for (i, j, lon, lat) in [
        (0, 0, 179.999_618_530_273_44, 72.715_408_325_195_31),
        (1, 0, -179.928_375_244_140_62, 72.715_408_325_195_31),
        (4998, 1499, 179.900_375_366_210_94, 0.000_015_258_789_062_5),
        (2500, 2999, 0.022_008_959_203_958_51, -72.736_770_629_882_81),
        (3333, 700, 60.005_470_275_878_906, 49.759_841_918_945_31),
    ] {
        position(f, i, j, lon, lat, 5e-5);
    }
}

#[test]
#[ignore]
fn cwa_full_frame() {
    let Some(b) = sample("canada-world/cwa/O-A0059-001.json") else { return };
    let d = timed("CWA radar JSON 921 x 881 (8.9 MB)", || cwa::decode(&b).unwrap());
    let f = &d.field;
    assert_eq!((f.nx, f.ny), (921, 881));
    assert_eq!(count(f, f32::is_nan), 200_231);
    assert_eq!(count(f, |v| v == NO_ECHO_DBZ), 587_952);
}

#[test]
#[ignore]
fn smhi_composite_through_the_generic_reader() {
    // Superblock 1, uint8, a projdef without x_0/y_0: placed from its corner.
    let Some(b) = sample("radar/smhi/latest.h5") else { return };
    let c = odim::decode(&b, &[]).unwrap();
    let f = &c.decoded.field;
    assert_eq!((f.nx, f.ny), (458, 881));
    assert_eq!(c.quantity, "DBZH");
    assert_eq!(c.decoded.valid, utc(2026, 11, 6, 18, 25, 0));
    value(f, 286, 123, 50.4, 1e-4);
    value(f, 200, 400, NO_ECHO_DBZ, 0.0);
    value(f, 0, 0, f32::NAN, 0.0);
    // pyproj with the projdef's +towgs84=0,0,0, which converts WGS84 to
    // Bessel latitudes through geocentric coordinates; the decoder takes
    // the two as equal, up to 25 m (0.012 cells) off at the far corner.
    position(f, 0, 0, 5.354_662_182, 69.799_570_749, 3e-4);
    position(f, 286, 123, 20.135_076_039, 67.615_145_770, 3e-4);
    position(f, 457, 880, 22.749_432_831, 53.716_457_889, 3e-4);
}

#[test]
#[ignore]
fn gmgsi_calibration_against_goes19() {
    // GMGSI's 17:00 mosaic against GOES-19 band 13 at 18:15, at every third
    // GMGSI cell well inside the GOES-East disk. Where both look at the
    // same, mostly clear, warm scene (250 to 315 K) the difference must be
    // centred on zero; under the classic NOAA scale it would be about +45 K.
    let Some(g) =
        sample("satellite/gmgsi/GLOBCOMPLIR_v3r0_blend_s202611061700000_e202611061709599_c202611061733476.nc")
    else {
        return;
    };
    let Some(b) = sample("satellite/goes/CMIPF_C13_G19_s20263101810214.nc") else { return };
    let gm = gmgsi::decode(&g).unwrap().field;
    let gs = goes::decode(&b).unwrap().field;
    let mut warm = Vec::new();
    let mut cold = Vec::new();
    for j in (0..gm.ny).step_by(3) {
        for i in (0..gm.nx).step_by(3) {
            let (lon, lat) = gm.proj.ij_to_lonlat(i as f64, j as f64);
            if !(-115.0..-35.0).contains(&lon) || lat.abs() > 55.0 {
                continue;
            }
            let (x, y) = gs.proj.lonlat_to_ij(lon, lat);
            let (x, y) = (x.round(), y.round());
            if !(x >= 0.0 && y >= 0.0 && (x as usize) < gs.nx && (y as usize) < gs.ny) {
                continue;
            }
            let t_goes = at(&gs, x as usize, y as usize);
            let t_gmgsi = at(&gm, i, j);
            if t_goes.is_nan() || t_gmgsi.is_nan() {
                continue;
            }
            if (250.0..=315.0).contains(&t_gmgsi) {
                warm.push(t_gmgsi - t_goes);
            } else if t_gmgsi < 230.0 {
                cold.push(t_gmgsi - t_goes);
            }
        }
    }
    let median = |v: &mut Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    let (nw, nc) = (warm.len(), cold.len());
    let (mw, mc) = (median(&mut warm), median(&mut cold));
    eprintln!("GMGSI - GOES: warm median {mw:+.2} K over {nw} cells, cold median {mc:+.2} K over {nc}");
    assert!(nw > 150_000, "{nw} warm cells");
    assert!(mw.abs() < 1.5, "warm-range median difference {mw} K");
    // Cold cloud tops move and change within the hour; only a loose bound.
    assert!(mc.abs() < 8.0, "cold-range median difference {mc} K");
}
