//! Real files from 2026-11-06, checked against values read independently
//! (grib-rs's own grid geometry during the feasibility spike; eccodes 2.50's
//! nearest-point values and corner coordinates for the regional NOAA and
//! Canadian samples), both on the decoded field and through a rendered tile.

use chrono::{TimeZone, Utc};
use grib::LatLons;
use wx_core::layer::{Derive, FieldSel, Stat, layer};
use wx_core::proj::{lat_to_y, lon_to_x};
use wx_core::{Family, Field, GridProjection, InputSpec, LayerSpec, Raster, TileCoord};
use wx_pipeline::{FrameGrid, decode, derive};

const NYC: (f64, f64) = (-74.0, 40.71);
const CHICAGO: (f64, f64) = (-87.63, 41.88);
const HOUSTON: (f64, f64) = (-95.37, 29.76);

const K_TO_C: Derive = Derive::Linear {
    mul: 1.0,
    add: -273.15,
};

/// An input with the rule under test, independent of the registry's rows.
fn input(derive: Derive) -> InputSpec {
    InputSpec {
        family: Family::Hrrr,
        sources: &[],
        product: "",
        fields: &[],
        derive,
        hold_minutes: 60,
        fill_past: false,
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

const fn sel(var: &'static str, level: &'static str) -> FieldSel {
    FieldSel {
        var,
        level,
        stat: Stat::Instant,
    }
}

const TMP_2M: FieldSel = sel("TMP", "2 m above ground");
const DPT_2M: FieldSel = sel("DPT", "2 m above ground");
const UGRD_10M: FieldSel = sel("UGRD", "10 m above ground");
const VGRD_10M: FieldSel = sel("VGRD", "10 m above ground");
const REFC: FieldSel = sel("REFC", "entire atmosphere");
const TCDC: FieldSel = sel("TCDC", "entire atmosphere");
const CSNOW: FieldSel = sel("CSNOW", "surface");
const ASNOW: FieldSel = FieldSel {
    var: "ASNOW",
    level: "surface",
    stat: Stat::RunTotal,
};

/// Values of the `r`-neighbourhood of the grid point nearest `(lon, lat)`,
/// found through the decoded field's projection.
fn around(values: &[f32], f: &Field, (lon, lat): (f64, f64), r: i64) -> Vec<f32> {
    let (i, j) = f.proj.lonlat_to_ij(lon, lat);
    let (i, j) = (i.round() as i64, j.round() as i64);
    let mut out = Vec::new();
    for dj in -r..=r {
        for di in -r..=r {
            let (ii, jj) = (i + di, j + dj);
            if ii >= 0 && jj >= 0 && (ii as usize) < f.nx && (jj as usize) < f.ny {
                out.push(values[jj as usize * f.nx + ii as usize]);
            }
        }
    }
    out
}

/// The reference value must be one of the decoded values within one grid
/// cell of the point (the spike recorded city coordinates to 0.01 degree).
fn assert_reference(what: &str, f: &Field, at: (f64, f64), want: f32, tol: f32) {
    let near = around(&f.values, f, at, 1);
    let best = near
        .iter()
        .map(|v| (v - want).abs())
        .fold(f32::INFINITY, f32::min);
    println!("{what} at {at:?}: reference {want}, neighbourhood {near:?}");
    assert!(
        best <= tol,
        "{what} at {at:?}: reference {want} not within one cell: {near:?}"
    );
}

/// Renders the tile holding `at` at zoom `z`, decodes it, and returns the
/// dequantized lattice sample nearest the point.
fn tile_value(
    grid: &FrameGrid,
    l: &LayerSpec,
    band: usize,
    z: u8,
    (lon, lat): (f64, f64),
) -> Option<f32> {
    let n = (1u64 << z) as f64;
    let (x, y) = (lon_to_x(lon) * n, lat_to_y(lat) * n);
    let t = TileCoord::new(z, x.floor() as u32, y.floor() as u32).unwrap();
    let bytes = grid.render(t)?;
    let tile = wx_tile::decode(&bytes).unwrap();
    let col = ((x - x.floor()) * 256.0).round() as usize;
    let row = ((y - y.floor()) * 256.0).round() as usize;
    l.bands[band].dequantize(tile.bands[band][row * 257 + col])
}

/// The tile's sample sits within half a lattice step of the point, so it
/// must lie within the range of the derived values around it.
fn assert_tile(
    what: &str,
    raster: &Raster,
    f: &Field,
    band: usize,
    got: Option<f32>,
    at: (f64, f64),
    step: f32,
) {
    let near: Vec<f32> = around(&raster.bands[band], f, at, 2)
        .into_iter()
        .filter(|v| !v.is_nan())
        .collect();
    let lo = near.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = near.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let got = got.unwrap_or_else(|| panic!("{what} at {at:?}: tile has no data"));
    println!("{what} at {at:?}: tile {got}, derived values nearby {lo}..{hi}");
    assert!(
        got >= lo - step && got <= hi + step,
        "{what} at {at:?}: tile value {got} outside {lo}..{hi}"
    );
}

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
}

// ---------------------------------------------------------------------------
// HRRR
// ---------------------------------------------------------------------------

#[test]
fn hrrr_temperature() {
    let set = decode(&fixture("hrrr_t10z_f06__TMP_2mabove.grib2"), &[TMP_2M]).unwrap();
    assert_eq!(set.reference_time, utc(2026, 11, 6, 10, 0, 0));
    assert_eq!(set.valid_time, utc(2026, 11, 6, 16, 0, 0));
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (1799, 1059));
    assert!(f.grid_relative_winds);
    for (at, want) in [(NYC, 293.25), (CHICAGO, 279.56), (HOUSTON, 293.37)] {
        assert_reference("HRRR TMP", f, at, want, 0.011);
    }

    let temp = layer("temp").unwrap();
    let raster = derive(temp, &input(K_TO_C), &set.fields).unwrap();
    let grid = FrameGrid::build(temp, &raster);
    for (at, want) in [(NYC, 293.25), (CHICAGO, 279.56), (HOUSTON, 293.37)] {
        let got = tile_value(&grid, temp, 0, 6, at);
        assert_tile("HRRR temp tile", &raster, f, 0, got, at, 0.011);
        assert!((got.unwrap() - (want - 273.15)).abs() < 1.5);
    }
}

#[test]
fn hrrr_reflectivity_clouds_and_winds() {
    let refc = decode(&fixture("hrrr_t10z_f06__REFC_entire.grib2"), &[REFC]).unwrap();
    assert_reference("HRRR REFC", &refc.fields[0], NYC, 31.4, 0.05);
    let radar = layer("radar").unwrap();
    let raster = derive(radar, &input(Derive::Reflectivity), &refc.fields).unwrap();
    let grid = FrameGrid::build(radar, &raster);
    let got = tile_value(&grid, radar, 0, 6, NYC);
    assert_tile(
        "HRRR radar tile",
        &raster,
        &refc.fields[0],
        0,
        got,
        NYC,
        0.11,
    );
    // HRRR's no-echo floor is -10 dBZ; the layer stores it as -30.
    let f = &refc.fields[0];
    let floor = f.values.iter().filter(|&&v| v <= -10.0).count();
    let stored = raster.bands[0].iter().filter(|&&v| v == -30.0).count();
    assert!(floor > f.values.len() / 2 && floor == stored);

    let tcdc = decode(&fixture("hrrr_t10z_f06__TCDC_entire.grib2"), &[TCDC]).unwrap();
    for (at, want) in [(NYC, 100.0), (CHICAGO, 25.0), (HOUSTON, 1.0)] {
        assert_reference("HRRR TCDC", &tcdc.fields[0], at, want, 0.01);
    }
    let clouds = layer("clouds").unwrap();
    let raster = derive(
        clouds,
        &input(Derive::Linear { mul: 1.0, add: 0.0 }),
        &tcdc.fields,
    )
    .unwrap();
    let grid = FrameGrid::build(clouds, &raster);
    for at in [NYC, CHICAGO, HOUSTON] {
        let got = tile_value(&grid, clouds, 0, 6, at);
        assert_tile(
            "HRRR clouds tile",
            &raster,
            &tcdc.fields[0],
            0,
            got,
            at,
            0.011,
        );
    }

    // The two wind files concatenated in the wrong order: matched by content.
    let mut both = fixture("hrrr_t10z_f06__VGRD_10m.grib2");
    both.extend(fixture("hrrr_t10z_f06__UGRD_10m.grib2"));
    let wind = decode(&both, &[UGRD_10M, VGRD_10M]).unwrap();
    let (u, v) = (&wind.fields[0], &wind.fields[1]);
    assert!(u.grid_relative_winds && v.grid_relative_winds);
    assert_reference("HRRR UGRD (grid-relative)", u, NYC, 1.51, 0.011);
    assert_reference("HRRR VGRD (grid-relative)", v, NYC, -0.11, 0.011);
    // Earth-relative at New York: rotated by n * (lon - LoV), about 14.6
    // degrees, with the speed unchanged.
    let gust = Field {
        values: vec![0.0; u.values.len()],
        ..u.clone()
    };
    let wl = layer("wind").unwrap();
    let raster = derive(wl, &input(Derive::Wind), &[u.clone(), v.clone(), gust]).unwrap();
    let (i, j) = u.proj.lonlat_to_ij(NYC.0, NYC.1);
    let k = j.round() as usize * u.nx + i.round() as usize;
    let (gu, gv) = (u.values[k], v.values[k]);
    let (eu, ev) = (raster.bands[0][k], raster.bands[1][k]);
    let turn = (gv.atan2(gu) - ev.atan2(eu)).to_degrees();
    println!("HRRR wind at NYC: grid ({gu}, {gv}) -> earth ({eu}, {ev}), turned {turn:.2} deg");
    assert!((gu.hypot(gv) - eu.hypot(ev)).abs() < 1e-4);
    assert!((turn - 14.63).abs() < 0.2, "{turn}");
    let grid = FrameGrid::build(wl, &raster);
    for band in [0, 1] {
        let got = tile_value(&grid, wl, band, 6, NYC);
        assert_tile(
            "HRRR earth-relative wind tile",
            &raster,
            u,
            band,
            got,
            NYC,
            0.011,
        );
    }
}

#[test]
fn hrrr_sub_hourly_times_and_ambiguity() {
    let a = fixture("hrrr_t10z_subh01__REFC_entireatmosphere_15min.grib2");
    let b = fixture("hrrr_t10z_subh01__REFC_entireatmosphere_45min.grib2");
    let sa = decode(&a, &[REFC]).unwrap();
    let sb = decode(&b, &[REFC]).unwrap();
    assert_eq!(sa.reference_time, utc(2026, 11, 6, 10, 0, 0));
    assert_eq!(sa.valid_time, utc(2026, 11, 6, 10, 15, 0));
    assert_eq!(sb.valid_time, utc(2026, 11, 6, 10, 45, 0));
    let mut both = a;
    both.extend(b);
    let err = format!("{:#}", decode(&both, &[REFC]).unwrap_err());
    assert!(err.contains("2 GRIB messages hold REFC"), "{err}");
}

#[test]
fn hrrr_accumulated_snow() {
    let bytes = fixture("hrrr_t10z_f06__ASNOW_surface.grib2");
    let set = decode(&bytes, &[ASNOW]).unwrap();
    // `0-6 hour acc fcst`: valid at the end of the interval, not its start.
    assert_eq!(set.reference_time, utc(2026, 11, 6, 10, 0, 0));
    assert_eq!(set.valid_time, utc(2026, 11, 6, 16, 0, 0));
    let instant = FieldSel {
        stat: Stat::Instant,
        ..ASNOW
    };
    let err = format!("{:#}", decode(&bytes, &[instant]).unwrap_err());
    assert!(err.contains("no GRIB message holds ASNOW"), "{err}");
    let snow = layer("snow").unwrap();
    let raster = derive(
        snow,
        &input(Derive::Linear {
            mul: 1000.0,
            add: 0.0,
        }),
        &set.fields,
    )
    .unwrap();
    let max = raster.bands[0].iter().copied().fold(0.0f32, f32::max);
    assert!((max - 253.08).abs() < 0.1, "deepest snow {max} mm");
}

#[test]
fn categorical_snow_numbering_differs_between_models() {
    let hrrr = decode(&fixture("hrrr_t10z_f06__CSNOW_surface.grib2"), &[CSNOW]).unwrap();
    let gfs = decode(
        &fixture("gfs_t06z_f024__CSNOW_surface_24hourfcst.grib2"),
        &[CSNOW],
    )
    .unwrap();
    for f in [&hrrr.fields[0], &gfs.fields[0]] {
        assert!(f.values.iter().all(|&v| v == 0.0 || v == 1.0));
        assert!(f.values.contains(&1.0));
    }
    // Rates and flags from one model on one grid.
    let prate = decode(
        &fixture("hrrr_t10z_f06__PRATE_surface.grib2"),
        &[sel("PRATE", "surface")],
    )
    .unwrap();
    let zero = Field {
        values: vec![0.0; prate.fields[0].values.len()],
        ..prate.fields[0].clone()
    };
    let precip = layer("precip").unwrap();
    let fields = [
        prate.fields[0].clone(),
        zero.clone(),
        hrrr.fields[0].clone(),
        zero.clone(),
        zero,
    ];
    let r = derive(precip, &input(Derive::PrecipTypeRate), &fields).unwrap();
    let snowing = r.bands[1].iter().filter(|&&t| t == 2.0).count();
    let flagged = hrrr.fields[0]
        .values
        .iter()
        .zip(&r.bands[0])
        .filter(|(f, rate)| **f == 1.0 && **rate >= 0.01)
        .count();
    assert_eq!(snowing, flagged);
    assert!(snowing > 0);
}

/// wx-core's projection, as `decode` builds it, against grib-rs's own
/// latitudes and longitudes for every 97th grid point.
#[test]
fn georeferencing_agrees_with_grib_rs() {
    for (name, gz) in [
        ("hrrr_t10z_f06__TMP_2mabove.grib2", false),
        ("gfs_t06z_f024__TMP_2mabove.grib2", false),
        ("hrrrak_t12z_f01__TMP_2mabove.grib2", false),
        ("rtma_pr_t17z__TMP_2mabove.grib2", false),
        (
            "MRMS_RadarOnly_QPE_01H_00.00_20261106-114800.grib2.gz",
            true,
        ),
    ] {
        let mut bytes = fixture(name);
        if gz {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&bytes[..]), &mut out)
                .unwrap();
            bytes = out;
        }
        let set = decode(&bytes, &[]).unwrap();
        let worst = disagreement_with_grib_rs(&bytes, 0, &set.fields[0]);
        println!("{name}: worst disagreement {worst:.2e} degrees");
        assert!(worst < 1e-4, "{name}: {worst}");
    }
}

/// Largest lon/lat difference, in degrees, between `f`'s projection and
/// grib-rs's coordinates for submessage `index` of `bytes`, every 97th point.
fn disagreement_with_grib_rs(bytes: &[u8], index: usize, f: &Field) -> f64 {
    let g = grib::from_bytes(bytes).unwrap();
    let (_, sm) = g.iter().nth(index).unwrap();
    let mut worst: f64 = 0.0;
    for (k, (lat, lon)) in sm.latlons().unwrap().enumerate().step_by(97) {
        let (i, j) = ((k % f.nx) as f64, (k / f.nx) as f64);
        let (mlon, mlat) = f.proj.ij_to_lonlat(i, j);
        let dlon = (mlon - lon).rem_euclid(360.0);
        let dlon = dlon.min(360.0 - dlon);
        worst = worst.max(dlon.max((mlat - lat).abs()));
    }
    worst
}

// ---------------------------------------------------------------------------
// GFS
// ---------------------------------------------------------------------------

#[test]
fn gfs_temperature_and_reflectivity() {
    let set = decode(&fixture("gfs_t06z_f024__TMP_2mabove.grib2"), &[TMP_2M]).unwrap();
    assert_eq!(set.reference_time, utc(2026, 11, 6, 6, 0, 0));
    assert_eq!(set.valid_time, utc(2026, 11, 7, 6, 0, 0));
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (1440, 721));
    assert!(matches!(&f.proj, wx_core::GridProjection::LatLon(g) if g.periodic && g.dlat < 0.0));
    for (at, want) in [(NYC, 288.63), (CHICAGO, 279.05), (HOUSTON, 296.46)] {
        assert_reference("GFS TMP", f, at, want, 0.011);
    }
    let temp = layer("temp").unwrap();
    let raster = derive(temp, &input(K_TO_C), &set.fields).unwrap();
    let grid = FrameGrid::build(temp, &raster);
    for at in [NYC, CHICAGO, HOUSTON] {
        let got = tile_value(&grid, temp, 0, 4, at);
        assert_tile("GFS temp tile", &raster, f, 0, got, at, 0.011);
    }

    let refc = decode(&fixture("gfs_t06z_f024__REFC_entire.grib2"), &[REFC]).unwrap();
    assert_reference("GFS REFC", &refc.fields[0], NYC, -20.0, 0.01);
    let radar = layer("radar").unwrap();
    let raster = derive(radar, &input(Derive::Reflectivity), &refc.fields).unwrap();
    let grid = FrameGrid::build(radar, &raster);
    assert_eq!(
        tile_value(&grid, radar, 0, 4, NYC),
        Some(-30.0),
        "no echo is stored as -30"
    );
}

#[test]
fn gfs_winds_are_earth_relative() {
    let mut both = fixture("gfs_t06z_f024__UGRD_10mabove.grib2");
    both.extend(fixture("gfs_t06z_f024__VGRD_10mabove.grib2"));
    let set = decode(&both, &[VGRD_10M, UGRD_10M]).unwrap();
    assert!(!set.fields[0].grid_relative_winds);
    let alone = decode(&fixture("gfs_t06z_f024__VGRD_10mabove.grib2"), &[VGRD_10M]).unwrap();
    assert_eq!(
        set.fields[0].values, alone.fields[0].values,
        "request order is kept"
    );
}

#[test]
fn alternate_rows_are_reversed() {
    // Relabel the GFS grid as boustrophedon (0x10): the same bytes then
    // hold every other row backwards.
    let gfs = fixture("gfs_t06z_f024__TMP_2mabove.grib2");
    // GFS section 3 starts at byte 37 of the message.
    const SCAN: usize = 37 + 71;
    let mut b = gfs.clone();
    b[SCAN] = 0x10;
    let orig = decode(&gfs, &[TMP_2M]).unwrap();
    let alt = decode(&b, &[TMP_2M]).unwrap();
    let (o, a) = (&orig.fields[0], &alt.fields[0]);
    for j in [0, 1, 200, 361, 720] {
        for i in [0, 1, 700, 1439] {
            let want = if j % 2 == 1 {
                o.values[j * 1440 + 1439 - i]
            } else {
                o.values[j * 1440 + i]
            };
            assert_eq!(a.values[j * 1440 + i], want, "({i}, {j})");
        }
    }
    // South-to-north scanning with a first point at 90 N contradicts the
    // stated last point: refuse rather than flip the globe.
    let mut b = gfs.clone();
    b[SCAN] = 0x40;
    assert!(format!("{:#}", decode(&b, &[TMP_2M]).unwrap_err()).contains("inconsistent"));
}

/// Mean absolute difference between vertical and between horizontal
/// neighbours. Boustrophedon rows decoded the wrong way round make
/// vertical neighbours come from opposite ends of the grid.
fn striping(f: &Field) -> (f64, f64) {
    let (mut down, mut across, mut n) = (0.0, 0.0, 0usize);
    for j in (0..f.ny - 1).step_by(3) {
        for i in (0..f.nx - 1).step_by(3) {
            let v = f.values[j * f.nx + i];
            let (r, d) = (f.values[j * f.nx + i + 1], f.values[(j + 1) * f.nx + i]);
            if v.is_nan() || r.is_nan() || d.is_nan() {
                continue;
            }
            down += f64::from((v - d).abs());
            across += f64::from((v - r).abs());
            n += 1;
        }
    }
    (down / n as f64, across / n as f64)
}

/// The grid point nearest a reference point eccodes reported, and its value.
fn assert_nearest(
    what: &str,
    f: &Field,
    (lon, lat): (f64, f64),
    want: f32,
    tol: f32,
    at: (f64, f64),
) {
    let (i, j) = f.proj.lonlat_to_ij(lon, lat);
    let (i, j) = (i.round(), j.round());
    let (glon, glat) = f.proj.ij_to_lonlat(i, j);
    let dlon = (glon - at.0).rem_euclid(360.0);
    assert!(
        dlon.min(360.0 - dlon) < 2e-4 && (glat - at.1).abs() < 2e-4,
        "{what}: nearest point to ({lon}, {lat}) is ({glon}, {glat}), eccodes says {at:?}"
    );
    let got = f.values[j as usize * f.nx + i as usize];
    println!("{what} at ({lon}, {lat}): {got}, eccodes {want}");
    assert!(
        (got - want).abs() <= tol,
        "{what} at ({lon}, {lat}): {got} vs eccodes {want}"
    );
}

// ---------------------------------------------------------------------------
// NOAA beyond the lower 48 and NBM: Mercator (3.10), polar stereographic
// (3.20), boustrophedon rows (0x50)
// ---------------------------------------------------------------------------

#[test]
fn nbm_conus_boustrophedon_rows() {
    let set = decode(&fixture("nbm_t10z_f001_TMP2m.grib2"), &[TMP_2M]).unwrap();
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (2345, 1597));
    let (down, across) = striping(f);
    println!("NBM CONUS: vertical {down:.3} K, horizontal {across:.3} K");
    assert!(down < 2.0 * across, "striped: {down} vs {across}");
    for (at, want, near) in [
        ((-74.0, 40.71), 292.8, (285.98706, 40.71417)),
        ((-87.63, 41.88), 278.6, (272.38318, 41.87646)),
        ((-95.37, 29.76), 291.9, (264.63278, 29.77044)),
        ((-122.4, 37.8), 287.7, (237.61237, 37.79856)),
        ((-104.99, 39.74), 276.5, (255.00887, 39.74983)),
        ((-80.19, 25.76), 298.2, (279.81256, 25.75943)),
    ] {
        assert_nearest("NBM CONUS TMP", f, at, want, 0.06, near);
    }
}

#[test]
fn rtma_mercator_grids() {
    for (name, nx, ny, last, points) in [
        (
            "rtma_hi_t17z__TMP_2mabove.grib2",
            321,
            225,
            (206.131063, 23.087866),
            vec![
                ((-157.86, 21.31), 298.68, (202.13555, 21.31585)),
                ((-155.08, 19.72), 296.97, (204.91088, 19.72502)),
                ((-159.37, 22.0), 295.93, (200.62827, 22.00516)),
            ],
        ),
        (
            "rtma_pr_t17z__TMP_2mabove.grib2",
            353,
            257,
            (296.015535, 19.73616),
            vec![
                ((-66.1, 18.45), 301.96, (293.89816, 18.44747)),
                ((-67.15, 18.2), 299.96, (292.84545, 18.19763)),
                ((-64.9, 18.34), 299.64, (295.09442, 18.34531)),
            ],
        ),
        (
            "rtma_gu_t17z__TMP_2mabove.grib2",
            193,
            193,
            (148.280176, 16.794486),
            vec![
                ((144.79, 13.47), 298.04, (144.7871, 13.46925)),
                ((145.75, 15.18), 297.22, (145.74411, 15.18458)),
            ],
        ),
    ] {
        let bytes = fixture(name);
        let set = decode(&bytes, &[TMP_2M]).unwrap();
        let f = &set.fields[0];
        assert_eq!((f.nx, f.ny), (nx, ny), "{name}");
        assert!(matches!(f.proj, GridProjection::Mercator(_)), "{name}");
        for (at, want, near) in points {
            assert_nearest(name, f, at, want, 0.011, near);
        }
        // The far corner where eccodes puts it from the first point and the
        // grid lengths. (grib-rs spaces Mercator rows and columns between the
        // first point and the stated last point instead, which the templates
        // round: for Guam that is 1.8e-4 degree off.)
        let (lon, lat) = f.proj.ij_to_lonlat((nx - 1) as f64, (ny - 1) as f64);
        let dlon = (lon - last.0).rem_euclid(360.0);
        assert!(
            dlon.min(360.0 - dlon) < 2e-6 && (lat - last.1).abs() < 2e-6,
            "{name}: ({lon}, {lat})"
        );
        let temp = layer("temp").unwrap();
        let raster = derive(temp, &input(K_TO_C), &set.fields).unwrap();
        let grid = FrameGrid::build(temp, &raster);
        let at = points_inside(f)[0];
        let got = tile_value(&grid, temp, 0, 8, at);
        assert_tile(name, &raster, f, 0, got, at, 0.011);
        assert!(
            grid.solid().is_some(),
            "{name}: a complete analysis is solid"
        );
    }
}

/// A point well inside a decoded field's grid.
fn points_inside(f: &Field) -> Vec<(f64, f64)> {
    vec![f.proj.ij_to_lonlat(f.nx as f64 * 0.5, f.ny as f64 * 0.4)]
}

#[test]
fn rtma_mercator_winds_need_no_rotation() {
    let mut both = fixture("rtma_hi_t17z__UGRD_10mabove.grib2");
    both.extend(fixture("rtma_hi_t17z__VGRD_10mabove.grib2"));
    let set = decode(&both, &[UGRD_10M, VGRD_10M]).unwrap();
    let (u, v) = (&set.fields[0], &set.fields[1]);
    // Flagged grid-relative, but a Mercator grid's axes are east and north.
    assert!(u.grid_relative_winds && v.grid_relative_winds && !u.proj.rotates_winds());
    assert_nearest(
        "RTMA HI UGRD",
        u,
        (-157.86, 21.31),
        -1.21,
        0.011,
        (202.13555, 21.31585),
    );
    assert_nearest(
        "RTMA HI VGRD",
        v,
        (-157.86, 21.31),
        2.45,
        0.011,
        (202.13555, 21.31585),
    );
    let wind = layer("wind").unwrap();
    let gust = Field {
        values: vec![0.0; u.values.len()],
        ..u.clone()
    };
    let r = derive(wind, &input(Derive::Wind), &[u.clone(), v.clone(), gust]).unwrap();
    assert_eq!(r.bands[0], u.values);
    assert_eq!(r.bands[1], v.values);
}

#[test]
fn nbm_mercator_and_polar_boustrophedon() {
    let pr = decode(&fixture("nbm_t17z_f001_pr__TMP_2mabove.grib2"), &[TMP_2M]).unwrap();
    let f = &pr.fields[0];
    assert_eq!((f.nx, f.ny), (339, 225));
    assert!(matches!(f.proj, GridProjection::Mercator(_)));
    for (at, want, near) in [
        ((-66.1, 18.45), 300.9, (293.89818, 18.44747)),
        ((-67.15, 18.2), 299.8, (292.84547, 18.19764)),
        ((-64.9, 18.34), 300.6, (295.09444, 18.34531)),
        ((-65.5, 18.0), 300.8, (294.49631, 18.00434)),
    ] {
        assert_nearest("NBM PR TMP", f, at, want, 0.06, near);
    }
    let ak = decode(&fixture("nbm_t17z_f001_ak__TMP_2mabove.grib2"), &[TMP_2M]).unwrap();
    let f = &ak.fields[0];
    assert_eq!((f.nx, f.ny), (1649, 1105));
    assert!(matches!(f.proj, GridProjection::PolarStereographic(_)));
    let (down, across) = striping(f);
    println!("NBM AK: vertical {down:.3} K, horizontal {across:.3} K");
    assert!(down < 2.0 * across, "striped: {down} vs {across}");
    for (at, want, near) in [
        ((-149.9, 61.22), 270.7, (210.11159, 61.21438)),
        ((-147.72, 64.84), 266.3, (212.24931, 64.83072)),
        ((-156.79, 71.29), 268.0, (203.1879, 71.30048)),
        ((-134.42, 58.3), 279.8, (225.60526, 58.30572)),
        ((179.0, 52.0), 277.2, (179.01439, 52.00148)),
    ] {
        assert_nearest("NBM AK TMP", f, at, want, 0.06, near);
    }
}

#[test]
fn hrrr_alaska() {
    let bytes = fixture("hrrrak_t12z_f01__TMP_2mabove.grib2");
    let set = decode(&bytes, &[TMP_2M]).unwrap();
    assert_eq!(set.reference_time, utc(2026, 11, 6, 12, 0, 0));
    assert_eq!(set.valid_time, utc(2026, 11, 6, 13, 0, 0));
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (1299, 919));
    for (at, want, near) in [
        ((-149.9, 61.22), 271.7116, (210.08244, 61.22677)),
        ((-147.72, 64.84), 270.7741, (212.27704, 64.85105)),
        ((-156.79, 71.29), 266.7741, (203.18422, 71.28521)),
        ((-134.42, 58.3), 279.3991, (225.57618, 58.29506)),
        ((175.0, 55.0), 275.6491, (174.98862, 54.98835)),
    ] {
        assert_nearest("HRRR AK TMP", f, at, want, 0.011, near);
    }
    let worst = disagreement_with_grib_rs(&bytes, 0, f);
    println!("HRRR AK: worst disagreement with grib-rs {worst:.2e} degrees");
    assert!(worst < 1e-4, "{worst}");
    // The grid crosses the antimeridian, and its tiles are on both sides.
    let temp = layer("temp").unwrap();
    let raster = derive(temp, &input(K_TO_C), &set.fields).unwrap();
    let grid = FrameGrid::build(temp, &raster);
    assert!(grid.bounds().wraps(), "{:?}", grid.bounds());
    for at in [(-149.9, 61.22), (175.0, 55.0)] {
        let got = tile_value(&grid, temp, 0, 6, at);
        assert_tile("HRRR AK temp tile", &raster, f, 0, got, at, 0.011);
    }
    let solid = grid.solid().expect("solid");
    println!("HRRR AK bounds {:?}, solid {solid:?}", grid.bounds());

    // 10 m wind is grid-relative: rotated by (lon - LoV) on a north-polar grid.
    let u = decode(
        &fixture("hrrrak_t12z_f01__UGRD_10mabove.grib2"),
        &[UGRD_10M],
    )
    .unwrap();
    let u = &u.fields[0];
    assert!(u.grid_relative_winds);
    assert_nearest(
        "HRRR AK UGRD",
        u,
        (-149.9, 61.22),
        0.1136,
        0.011,
        (210.08244, 61.22677),
    );
    let zero = Field {
        values: vec![0.0; u.values.len()],
        ..u.clone()
    };
    let wl = layer("wind").unwrap();
    let r = derive(wl, &input(Derive::Wind), &[u.clone(), zero.clone(), zero]).unwrap();
    let (i, j) = u.proj.lonlat_to_ij(-147.72, 64.84);
    let k = j.round() as usize * u.nx + i.round() as usize;
    let (lon, _) = u.proj.ij_to_lonlat(i.round(), j.round());
    let (gu, eu, ev) = (u.values[k], r.bands[0][k], r.bands[1][k]);
    let turn = (0f32.atan2(gu) - ev.atan2(eu)).to_degrees();
    let turn = (turn + 180.0).rem_euclid(360.0) - 180.0;
    let want = (lon + 135.0) as f32;
    println!(
        "HRRR AK wind at Fairbanks: grid ({gu}, 0) -> earth ({eu}, {ev}), turned {turn:.2} deg"
    );
    assert!((gu.abs() - eu.hypot(ev)).abs() < 1e-4);
    assert!((turn - want).abs() < 0.01, "{turn} vs {want}");
}

#[test]
fn east_to_west_rows_are_reversed() {
    // Relabel the GFS grid as scanned east to west from 359.75 E: the same
    // bytes then describe the mirror image of the original field.
    let gfs = fixture("gfs_t06z_f024__TMP_2mabove.grib2");
    let mut b = gfs.clone();
    b[37 + 71] = 0x80;
    b[37 + 50..37 + 54].copy_from_slice(&359_750_000u32.to_be_bytes());
    b[37 + 59..37 + 63].copy_from_slice(&0u32.to_be_bytes());
    let orig = decode(&gfs, &[TMP_2M]).unwrap();
    let mirrored = decode(&b, &[TMP_2M]).unwrap();
    let (o, m) = (&orig.fields[0], &mirrored.fields[0]);
    assert_eq!(o.proj, m.proj, "columns still start at 0 E and run east");
    for j in [0, 200, 360, 720] {
        for i in [0, 1, 700, 1439] {
            assert_eq!(m.values[j * 1440 + i], o.values[j * 1440 + 1439 - i]);
        }
    }
}

#[test]
fn refuses_oversized_grids_before_unpacking() {
    // A self-consistent header for 20000 x 20000 points: the unpacker would
    // size its buffers from it.
    let mut b = fixture("gfs_t06z_f024__TMP_2mabove.grib2");
    b[37 + 6..37 + 10].copy_from_slice(&400_000_000u32.to_be_bytes());
    b[37 + 30..37 + 34].copy_from_slice(&20_000u32.to_be_bytes());
    b[37 + 34..37 + 38].copy_from_slice(&20_000u32.to_be_bytes());
    let err = format!("{:#}", decode(&b, &[TMP_2M]).unwrap_err());
    assert!(err.contains("larger than any feed uses"), "{err}");
}

// ---------------------------------------------------------------------------
// MRMS
// ---------------------------------------------------------------------------

#[test]
fn mrms_composite_reflectivity() {
    let set = decode(
        &fixture("MRMS_MergedReflectivityQCComposite_00.50_20261106-114838.grib2.gz"),
        &[],
    )
    .unwrap();
    assert_eq!(set.reference_time, utc(2026, 11, 6, 11, 48, 38));
    assert_eq!(set.valid_time, set.reference_time);
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (7000, 3500));
    let n = f.values.len() as f64;
    let no_coverage = f.values.iter().filter(|&&v| v == -999.0).count() as f64 / n;
    let no_echo = f.values.iter().filter(|&&v| v == -99.0).count() as f64 / n;
    println!(
        "MRMS: -999 covers {:.2} %, -99 covers {:.2} %",
        no_coverage * 100.0,
        no_echo * 100.0
    );
    assert!((no_coverage - 0.3336).abs() < 0.0001 && (no_echo - 0.6159).abs() < 0.0001);
    assert_reference("MRMS composite", f, (-74.0, 40.7), 28.5, 0.01);
    let atlantic = (-70.0, 35.0);
    let oklahoma = (-97.5, 35.5);
    assert!(
        around(&f.values, f, atlantic, 1)
            .iter()
            .all(|&v| v == -999.0)
    );
    assert!(
        around(&f.values, f, oklahoma, 1)
            .iter()
            .all(|&v| v == -99.0)
    );

    let radar = layer("radar").unwrap();
    let raster = derive(radar, &input(Derive::Reflectivity), &set.fields).unwrap();
    let grid = FrameGrid::build(radar, &raster);
    let max_zoom = 7;
    let got = tile_value(&grid, radar, 0, max_zoom, (-74.0, 40.7));
    assert_tile("MRMS radar tile", &raster, f, 0, got, (-74.0, 40.7), 0.11);
    assert_eq!(tile_value(&grid, radar, 0, max_zoom, oklahoma), Some(-30.0));
    assert_eq!(
        tile_value(&grid, radar, 0, max_zoom, atlantic),
        None,
        "no coverage is no data"
    );
    // The radar's own edges: out over the Pacific there is no grid at all.
    assert_eq!(tile_value(&grid, radar, 0, 5, (-135.0, 40.0)), None);
}

#[test]
fn mrms_rainfall() {
    let set = decode(
        &fixture("MRMS_RadarOnly_QPE_01H_00.00_20261106-114800.grib2.gz"),
        &[],
    )
    .unwrap();
    assert_eq!(set.valid_time, utc(2026, 11, 6, 11, 48, 0));
    let f = &set.fields[0];
    assert_reference("MRMS 1 h QPE", f, (-74.0, 40.7), 0.7, 0.01);
    let rain = layer("rain_1h").unwrap();
    let raster = derive(rain, &input(Derive::PrecipDepth), &set.fields).unwrap();
    let missing = raster.bands[0].iter().filter(|v| v.is_nan()).count();
    let negative = f.values.iter().filter(|&&v| v < 0.0).count();
    assert_eq!(missing, negative, "-3 (no coverage) becomes no data");
    let grid = FrameGrid::build(rain, &raster);
    let got = tile_value(&grid, rain, 0, 7, (-74.0, 40.7));
    assert_tile("MRMS rain tile", &raster, f, 0, got, (-74.0, 40.7), 0.051);
}

#[test]
fn mrms_grid_round_trips() {
    let set = decode(
        &fixture("MRMS_MergedReflectivityQCComposite_00.50_20261106-114838.grib2.gz"),
        &[],
    )
    .unwrap();
    let radar = layer("radar").unwrap();
    let raster = derive(radar, &input(Derive::Reflectivity), &set.fields).unwrap();
    let grid = FrameGrid::build(radar, &raster);
    let bytes = grid.to_bytes();
    println!("MRMS .wxg: {} bytes", bytes.len());
    assert!(bytes.len() < 5_000_000);
    let back = FrameGrid::from_bytes(&bytes).unwrap();
    let t = TileCoord::new(7, 37, 48).unwrap();
    assert_eq!(grid.render(t), back.render(t));
    assert!(grid.render(t).is_some());
}

// ---------------------------------------------------------------------------
// Bad input
// ---------------------------------------------------------------------------

fn inflated(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::GzDecoder::new(&fixture(name)[..]),
        &mut out,
    )
    .unwrap();
    out
}

fn patch(mut b: Vec<u8>, at: usize, bytes: &[u8]) -> Vec<u8> {
    b[at..at + bytes.len()].copy_from_slice(bytes);
    b
}

fn decode_err(bytes: &[u8], wanted: &[FieldSel]) -> String {
    format!("{:#}", decode(bytes, wanted).unwrap_err())
}

/// Messages whose headers would make grib-rs allocate without bound (a
/// failed allocation aborts the process, which no guard can catch), built
/// by changing a few bytes of real files. Each must be refused by the
/// pre-unpacking checks, which the error text identifies.
#[test]
fn hostile_packing_is_refused_before_unpacking() {
    // HRRR 2 m temperature and categorical snow: section 5 (template 5.3) at
    // byte 152. Octets 32-35 hold the group count, 38-41 the group length
    // reference, 43-46 the true length of the last group, 48 the order of
    // spatial differencing.
    const S5: usize = 152;
    let tmp = fixture("hrrr_t10z_f06__TMP_2mabove.grib2");
    let snow = fixture("hrrr_t10z_f06__CSNOW_surface.grib2");
    let too_many = "group lengths describe more than the 1905141 values encoded";

    // The snow file's last group already has width zero, so grib-rs's own
    // check (width times length bits of data) passes it at any length and
    // the unpacker would allocate 2^32 values for it.
    let b = patch(snow.clone(), S5 + 42, &u32::MAX.to_be_bytes());
    assert!(decode_err(&b, &[CSNOW]).contains(too_many));
    // The same for every group at once, zero-width ones included.
    let b = patch(snow.clone(), S5 + 37, &0x7fff_ffffu32.to_be_bytes());
    assert!(decode_err(&b, &[CSNOW]).contains(too_many));
    // A last group with data, and one length too many.
    let b = patch(tmp.clone(), S5 + 42, &0xffff_fff0u32.to_be_bytes());
    assert!(decode_err(&b, &[TMP_2M]).contains(too_many));
    let b = patch(tmp.clone(), S5 + 42, &26u32.to_be_bytes());
    assert!(
        decode_err(&b, &[TMP_2M]).contains(too_many),
        "real last length is 25"
    );
    let b = patch(tmp.clone(), S5 + 31, &u32::MAX.to_be_bytes());
    assert!(decode_err(&b, &[TMP_2M]).contains("4294967295 groups for 1905141 values"));
    let b = patch(tmp.clone(), S5 + 47, &[3]);
    assert!(decode_err(&b, &[TMP_2M]).contains("spatial differencing of order 3"));

    // MRMS: section 7 at byte 170, so the PNG starts at 175 and its IHDR
    // chunk's width, height, bit depth and CRC at 191, 195, 199 and 204.
    // A header claiming 2^30 x 2^30 pixels of 16 bits, with a valid CRC,
    // would have grib-rs allocate 2^61 bytes before decoding a row.
    let mrms = inflated("MRMS_MergedReflectivityQCComposite_00.50_20261106-114838.grib2.gz");
    let ihdr = |mut b: Vec<u8>, width: u32, height: u32, depth: u8| {
        b = patch(b, 191, &width.to_be_bytes());
        b = patch(b, 195, &height.to_be_bytes());
        b = patch(b, 199, &[depth]);
        let mut crc = flate2::Crc::new();
        crc.update(&b[187..204]);
        patch(b, 204, &crc.sum().to_be_bytes())
    };
    assert_eq!(
        ihdr(mrms.clone(), 7000, 3500, 16),
        mrms,
        "the IHDR offsets are right"
    );
    let b = ihdr(mrms.clone(), 1 << 30, 1 << 30, 16);
    assert!(decode_err(&b, &[]).contains("PNG of 1073741824 x 1073741824 pixels does not hold"));
    let b = ihdr(mrms.clone(), 7000, 3501, 16);
    assert!(decode_err(&b, &[]).contains("PNG of 7000 x 3501 pixels"));
    let b = ihdr(mrms.clone(), 7000, 3500, 8);
    assert!(decode_err(&b, &[]).contains("does not hold 16-bit values"));
    let b = patch(mrms.clone(), 175, b"\x89PNX");
    assert!(decode_err(&b, &[]).contains("does not start with a PNG header"));

    // GFS 2 m temperature: section 5 at byte 143 (template number in octets
    // 10-11), section 6 at 192 (bit-map indicator in octet 6).
    let gfs = fixture("gfs_t06z_f024__TMP_2mabove.grib2");
    let b = patch(gfs.clone(), 143 + 9, &4u16.to_be_bytes());
    let err = decode_err(&b, &[TMP_2M]);
    assert!(
        err.contains("data representation template 5.4 is not supported"),
        "{err}"
    );
    // Complex packing's octets read as JPEG 2000's and CCSDS's templates.
    let b = patch(gfs.clone(), 143 + 9, &40u16.to_be_bytes());
    let err = decode_err(&b, &[TMP_2M]);
    assert!(
        err.contains("does not start with a JPEG 2000 codestream"),
        "{err}"
    );
    let b = patch(gfs.clone(), 143 + 9, &42u16.to_be_bytes());
    let err = decode_err(&b, &[TMP_2M]);
    assert!(err.contains("CCSDS block size 0"), "{err}");
    let b = patch(gfs.clone(), 143 + 9, &200u16.to_be_bytes());
    assert!(decode(&b, &[TMP_2M]).is_err());
    // grib-rs allocates a section body at the length its header gives
    // before reading it: section 3 (at byte 37) claiming 4 GiB, or too
    // little to hold its own header.
    let b = patch(gfs.clone(), 37, &0xffff_fff0u32.to_be_bytes());
    assert!(decode_err(&b, &[TMP_2M]).contains("section 3 at byte 37 claims 4294967280 bytes"));
    let b = patch(gfs.clone(), 37, &3u32.to_be_bytes());
    assert!(decode_err(&b, &[TMP_2M]).contains("section 3 at byte 37 claims 3 bytes"));
    let b = patch(gfs.clone(), 192 + 5, &[254]);
    assert!(decode_err(&b, &[TMP_2M]).contains("bit-map indicator 254"));
    // GFS soil moisture is masked over water: a bit-map (section 6 at 192,
    // the map itself from 198) marks the 350995 points with values.
    let soil = fixture("gfs_t06z_f024__SOILW_0-0.1mbelowground_24hour.grib2");
    let set = decode(&soil, &[]).unwrap();
    let missing = set.fields[0].values.iter().filter(|v| v.is_nan()).count();
    assert_eq!(missing, 1440 * 721 - 350_995);
    let mut b = soil.clone();
    b[198 + 1000] ^= 0x80;
    assert!(decode_err(&b, &[]).contains("values are encoded"));
    // An 8-bit PNG (MRMS precipitation flags) passes its header check.
    let flags = decode(
        &inflated("MRMS_PrecipFlag_00.00_20261106-114800.grib2.gz"),
        &[],
    )
    .unwrap();
    assert_eq!(flags.fields[0].values.len(), 7000 * 3500);
    // Simple packing (HRRR 10 m u-wind, section 5 at 152): one value short.
    let u = fixture("hrrr_t10z_f06__UGRD_10m.grib2");
    let b = patch(u, 152 + 5, &1_905_140u32.to_be_bytes());
    assert!(
        decode_err(&b, &[UGRD_10M]).contains("1905140 values are encoded for a grid of 1905141")
    );
}

#[test]
fn malformed_grib_is_an_error_not_a_panic() {
    assert!(decode(b"", &[TMP_2M]).is_err());
    assert!(decode(b"definitely not GRIB", &[TMP_2M]).is_err());
    assert!(decode(&[0x1f, 0x8b, 1, 2, 3], &[]).is_err(), "broken gzip");
    let good = fixture("hrrr_t10z_f06__TCDC_entire.grib2");
    assert!(
        decode(&good[..good.len() / 2], &[TCDC]).is_err(),
        "truncated"
    );
    assert!(decode(&good, &[sel("NOPE", "surface")]).is_err());
    let two = [good.clone(), good.clone()].concat();
    // Identical repeats are fine, but not without a selector.
    assert!(decode(&two, &[TCDC]).is_ok());
    assert!(decode(&two, &[]).is_err());
    // Corrupt header bytes: whatever happens must be an Ok or an Err.
    for pos in (16..200).step_by(3) {
        let mut b = good.clone();
        b[pos] ^= 0xa5;
        let _ = decode(&b, &[TCDC]);
    }
    // Corrupt packed data.
    for pos in (300..good.len() - 4).step_by(good.len() / 40) {
        let mut b = good.clone();
        b[pos] ^= 0xff;
        let _ = decode(&b, &[TCDC]);
    }
}

// ---------------------------------------------------------------------------
// RTMA (not a fixture: ~84 MB). Point WX_RTMA_FILE at a whole
// `rtma2p5.tHHz.2dvaranl_ndfd.grb2_wexp`, or this is skipped.
// ---------------------------------------------------------------------------

#[test]
fn rtma_if_available() {
    let path = std::env::var("WX_RTMA_FILE").unwrap_or_else(|_| {
        "/private/tmp/claude-501/-Users-tahseenarnan-Development/d60ae49e-5172-4faa-aff4-42700185b923/scratchpad/dl/rtma/rtma_t11z_anl.grb2".into()
    });
    let Ok(bytes) = std::fs::read(&path) else {
        println!("skipped: no RTMA file at {path}");
        return;
    };
    let gust = sel("GUST", "10 m above ground");
    let set = decode(&bytes, &[TMP_2M, DPT_2M, UGRD_10M, VGRD_10M, gust]).unwrap();
    assert_eq!(set.valid_time, utc(2026, 11, 6, 11, 0, 0));
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (2345, 1597));
    match &f.proj {
        wx_core::GridProjection::Lambert(g) => assert_eq!(g.radius, 6_371_200.0),
        other => panic!("{other:?}"),
    }
    // TMP is the third message of the analysis file.
    let worst = disagreement_with_grib_rs(&bytes, 2, f);
    println!("RTMA: worst disagreement with grib-rs {worst:.2e} degrees");
    assert!(worst < 1e-4, "{worst}");
    // Reference values from the spike, grib-rs geometry, nearest point.
    let refs = [
        ("TMP", [290.82, 276.62, 291.5]),
        ("DPT", [290.11, 273.29, 290.3]),
        ("UGRD", [0.03, 2.43, 0.19]),
        ("VGRD", [1.21, -3.49, -0.17]),
        ("GUST", [1.86, 9.26, 1.97]),
    ];
    for (k, (name, want)) in refs.iter().enumerate() {
        for (at, w) in [NYC, CHICAGO, HOUSTON].into_iter().zip(want) {
            assert_reference(&format!("RTMA {name}"), &set.fields[k], at, *w, 0.011);
        }
    }
    let feels = layer("feels_like").unwrap();
    let raster = derive(feels, &input(Derive::FeelsLike), &set.fields[..4]).unwrap();
    let grid = FrameGrid::build(feels, &raster);
    for at in [NYC, CHICAGO, HOUSTON] {
        let got = tile_value(&grid, feels, 0, 6, at);
        assert_tile("RTMA feels-like tile", &raster, f, 0, got, at, 0.011);
    }
    let wind = layer("wind").unwrap();
    let w = [
        set.fields[2].clone(),
        set.fields[3].clone(),
        set.fields[4].clone(),
    ];
    let raster = derive(wind, &input(Derive::Wind), &w).unwrap();
    let grid = FrameGrid::build(wind, &raster);
    let got = tile_value(&grid, wind, 2, 6, CHICAGO);
    assert_tile("RTMA gust tile", &raster, f, 2, got, CHICAGO, 0.011);
}
