//! Real messages from non-NOAA providers (2026-11-06 runs), cut byte for byte
//! from whole files: JPEG 2000 (ECCC), CCSDS (ECMWF, Météo-France), bit-maps
//! (ICON-D2, AROME, FMI), several lead times or parameters in one file, and
//! the provider-neutral message API. Expected values, grid corners and
//! bit-map extents were read with eccodes 2.50 (every value of every fixture
//! was also compared with eccodes once, outside these tests).

use chrono::{DateTime, TimeZone, Utc};
use wx_core::layer::{FieldSel, Stat, layer};
use wx_core::proj::{TILE_CELLS, TILE_SAMPLES, lat_to_y, lon_to_x};
use wx_core::{Field, GridProjection, Raster, TileCoord};
use wx_pipeline::{
    DecodedMessage, FrameGrid, HeadScan, MessageInfo, Surface, Timing, decode, decode_messages,
    message_length, scan, scan_head,
};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

/// Every field of a file, decoded through the message API.
fn decode_all(bytes: &[u8]) -> (Vec<MessageInfo>, Vec<DecodedMessage>) {
    let infos = scan(bytes).unwrap();
    let which: Vec<usize> = (0..infos.len()).collect();
    let decoded = decode_messages(bytes, &which).unwrap();
    (infos, decoded)
}

/// `(index in file order, eccodes value or None for missing)`, compared to
/// `tol` (f32 rounding unless the test says otherwise).
fn assert_values(what: &str, f: &Field, points: &[(usize, Option<f64>)], tol: f64) {
    for &(k, want) in points {
        let got = f.values[k];
        match want {
            None => assert!(got.is_nan(), "{what} [{k}]: {got}, eccodes has it missing"),
            Some(w) => assert!(
                (f64::from(got) - w).abs() <= tol * w.abs().max(1.0),
                "{what} [{k}]: {got}, eccodes {w}"
            ),
        }
    }
}

/// The corners `(k, lat, lon)` (eccodes' coordinates, to 1e-6 degree) are
/// where the field's projection puts its samples.
fn assert_corners(what: &str, f: &Field, corners: &[(usize, f64, f64)], tol: f64) {
    for &(k, lat, lon) in corners {
        let (i, j) = ((k % f.nx) as f64, (k / f.nx) as f64);
        let (glon, glat) = f.proj.ij_to_lonlat(i, j);
        let dlon = (glon - lon).rem_euclid(360.0);
        assert!(
            (glat - lat).abs() <= tol && dlon.min(360.0 - dlon) <= tol,
            "{what} sample {k} ({i}, {j}) is at ({glat}, {glon}), eccodes says ({lat}, {lon})"
        );
    }
}

/// A row, how many of its points have values, and the first and last that
/// do.
type RowExtent = (usize, usize, Option<(usize, usize)>);

/// Valid values per row and the first and last valid column, as eccodes
/// reads the bit-map.
fn assert_rows(what: &str, f: &Field, rows: &[RowExtent]) {
    for &(j, count, ends) in rows {
        let row = &f.values[j * f.nx..(j + 1) * f.nx];
        let valid: Vec<usize> = (0..f.nx).filter(|&i| !row[i].is_nan()).collect();
        assert_eq!(valid.len(), count, "{what} row {j}");
        assert_eq!(
            valid.first().zip(valid.last()).map(|(a, b)| (*a, *b)),
            ends,
            "{what} row {j}"
        );
    }
}

fn missing(f: &Field) -> usize {
    f.values.iter().filter(|v| v.is_nan()).count()
}

fn latlon(f: &Field) -> wx_core::proj::LatLonGrid {
    match &f.proj {
        GridProjection::LatLon(g) => g.clone(),
        other => panic!("not a lat/lon grid: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// JPEG 2000 (5.40): ECCC
// ---------------------------------------------------------------------------

/// HRDPS (2.5 km rotated grid, 3.3 million points) and RDPS precipitation
/// rates, 11 and 12 bits per value: exact.
#[test]
fn eccc_jpeg2000_rates() {
    let set = decode(&fixture("hrdps_t12z_PT003H__PRATE_Sfc.grib2"), &[]).unwrap();
    assert_eq!(set.reference_time, utc(2026, 11, 6, 12, 0));
    assert_eq!(set.valid_time, utc(2026, 11, 6, 15, 0));
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (2540, 1290));
    assert!(f.grid_relative_winds && matches!(f.proj, GridProjection::RotatedLatLon(_)));
    assert_corners(
        "HRDPS",
        f,
        &[
            (0, 39.626032, -133.62952),
            (2539, 27.284598, -66.966424),
            (3_274_060, 66.56854, -152.730672),
            (3_276_599, 47.876456, -40.70856),
        ],
        // eccodes' rotation is good to about 1e-5 degree (the corners agree
        // with pyproj to 1e-8 in `decode`'s unit tests).
        2e-5,
    );
    assert_values(
        "HRDPS PRATE",
        f,
        &[
            (0, Some(0.0)),
            (144_174, Some(0.02378)),
            (1_196_691, Some(4e-5)),
            (1_231_086, Some(1e-5)),
            (2_939_809, Some(8e-5)),
            (2_943_256, Some(0.00083)),
            (3_276_599, Some(0.0)),
        ],
        1e-6,
    );
    assert!(f.values.iter().all(|v| (0.0..0.1).contains(v)));

    let set = decode(&fixture("rdps_t12z_PT003H__PrecipRate_Sfc.grib2"), &[]).unwrap();
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (1140, 1045));
    assert_values(
        "RDPS PrecipRate",
        f,
        &[
            (124_044, Some(0.01797)),
            (590_396, Some(6e-5)),
            (1_080_356, Some(0.00042)),
            (1_190_160, Some(1e-5)),
            (1_191_299, Some(0.0)),
        ],
        1e-6,
    );
}

/// GDPS: a global lat/lon grid from the South Pole northward, starting at
/// Greenwich, 9 bits per value: exact.
#[test]
fn gdps_jpeg2000_global_grid() {
    let set = decode(&fixture("gdps_t12z_PT003H__DewPoint_AGL-2m.grib2"), &[]).unwrap();
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (2400, 1201));
    let g = latlon(f);
    assert!(g.periodic, "2400 columns of 0.15 degree go round the earth");
    assert_eq!((g.lon0, g.lat0, g.dlat), (0.0, -90.0, 0.15));
    assert!((g.dlon - 0.15).abs() < 1e-12);
    assert_corners(
        "GDPS",
        f,
        &[
            (0, -90.0, 0.0),
            (2399, -90.0, 359.85),
            (2_880_000, 90.0, 0.0),
            (2_882_399, 90.0, 359.85),
        ],
        1e-9,
    );
    assert_values(
        "GDPS dew point",
        f,
        &[
            (0, Some(226.152_313_232_421_88)),
            (2_880_000, Some(261.152_313_232_421_9)),
            (202_231, Some(213.902_313_232_421_88)),
            (821_714, Some(276.902_313_232_421_9)),
            (1_293_280, Some(300.652_313_232_421_9)),
            (1_666_870, Some(296.402_313_232_421_9)),
            (2_723_590, Some(250.402_313_232_421_88)),
        ],
        1e-6,
    );
    // Global and complete: full weight everywhere, poles included.
    let temp = layer("temp").unwrap();
    let grid = FrameGrid::build(temp, &raster(f, -273.15));
    for (lon, lat) in [
        (0.0, 0.0),
        (-0.07, 45.0),
        (179.99, -60.0),
        (10.0, 89.99),
        (-120.0, -89.99),
    ] {
        assert_eq!(grid.coverage_at(lon, lat), 1.0, "({lon}, {lat})");
    }
    let b = grid.bounds();
    assert_eq!(
        (b.west, b.east, b.south, b.north),
        (-180.0, 180.0, -90.0, 90.0)
    );
}

/// HRDPS's 24-bit liquid-equivalent snowfall. The JPEG 2000 decoder works
/// in f32, which holds 24-bit integers but not its wavelet's sums of them:
/// some points come out a few packing steps (1e-6 kg m-2 here) high or low.
#[test]
fn eccc_jpeg2000_24_bit_values_are_within_a_few_packing_steps() {
    let bytes = fixture("hrdps_t12z_PT003H__WEASN-Accum1h_Sfc.grib2");
    let (infos, decoded) = decode_all(&bytes);
    assert_eq!(infos[0].packing_template, 40);
    assert_eq!(
        decoded[0].time,
        Timing::Interval {
            start: utc(2026, 11, 6, 14, 0),
            end: utc(2026, 11, 6, 15, 0),
            process: Some(1),
        }
    );
    let f = &decoded[0].field;
    let step = 1e-6;
    for (k, want) in [
        (0, 0.0),
        (2_118_506, 15.808_105),
        (2_321_939, 0.152_588),
        (2_347_129, 0.213_623),
        (2_939_809, 0.366_211),
        (3_021_240, 0.335_693),
    ] {
        let got = f64::from(f.values[k]);
        assert!(
            (got - want).abs() <= 3.0 * step + 1e-6 * want,
            "[{k}] {got} vs {want}"
        );
    }
    assert!(
        f.values
            .iter()
            .all(|&v| v > -4.0 * step as f32 && v < 100.0)
    );
}

// ---------------------------------------------------------------------------
// CCSDS (5.42): ECMWF and Météo-France
// ---------------------------------------------------------------------------

fn raster(f: &Field, add: f32) -> Raster {
    Raster {
        nx: f.nx,
        ny: f.ny,
        proj: f.proj.clone(),
        bands: vec![f.values.iter().map(|v| v + add).collect()],
    }
}

/// ECMWF IFS 0.1 degree: 6.5 million points whose first column is the
/// antimeridian (180 E) and whose rows run from the North Pole to the South.
#[test]
fn ecmwf_ccsds_grid_starts_at_the_antimeridian() {
    let bytes = fixture("ifs0p1_t00z_3h__2t_sfc.grib2");
    let (infos, decoded) = decode_all(&bytes);
    let info = &infos[0];
    assert!(info.is(0, 0, 0) && info.at_level(103, Some(2.0)));
    assert_eq!(
        (
            info.grid_template,
            info.packing_template,
            info.num_points,
            info.bitmap
        ),
        (0, 42, 3600 * 1801, false)
    );
    assert_eq!(info.valid_time(), Some(utc(2026, 11, 6, 3, 0)));
    let f = &decoded[0].field;
    assert_eq!((f.nx, f.ny), (3600, 1801));
    let g = latlon(f);
    assert!(g.periodic);
    assert_eq!((g.lon0, g.lat0), (180.0, 90.0));
    assert!((g.dlon - 0.1).abs() < 1e-12 && (g.dlat + 0.1).abs() < 1e-12);
    assert_corners(
        "IFS",
        f,
        &[
            (0, 90.0, -180.0),
            (3599, 90.0, 179.9),
            (6_480_000, -90.0, -180.0),
            (6_483_599, -90.0, 179.9),
        ],
        1e-9,
    );
    assert_values(
        "IFS 2t",
        f,
        &[
            (0, Some(261.708_969_116_210_94)),
            (6_480_000, Some(235.708_969_116_210_94)),
            (1_848_258, Some(289.708_969_116_210_94)),
            (2_161_200, Some(298.708_969_116_210_94)),
            (3_241_800, Some(299.958_969_116_210_94)),
            (3_984_600, Some(317.708_969_116_210_94)),
            (5_663_764, Some(272.458_969_116_210_94)),
            (6_043_080, Some(221.958_969_116_210_94)),
        ],
        1e-6,
    );
    // Column 0 is 180 E, column 1800 is Greenwich.
    let (i, _) = f.proj.lonlat_to_ij(0.0, 0.0);
    assert!((i - 1800.0).abs() < 1e-9, "{i}");
    let (i, _) = f.proj.lonlat_to_ij(-180.0, 0.0);
    assert!(i.abs() < 1e-9, "{i}");

    // Rendered: no seam at the antimeridian, which is also the grid's own
    // first and last column.
    let temp = layer("temp").unwrap();
    let grid = FrameGrid::build(temp, &raster(f, -273.15));
    let b = grid.bounds();
    assert_eq!((b.west, b.east), (-180.0, 180.0));
    for (lon, lat) in [
        (180.0, 0.0),
        (-180.0, 0.0),
        (179.95, 50.0),
        (-179.97, -30.0),
    ] {
        assert_eq!(grid.coverage_at(lon, lat), 1.0, "({lon}, {lat})");
    }
    for z in [1u8, 3, 5] {
        let last = (1u32 << z) - 1;
        for y in [0, (1u32 << z) / 3, (1u32 << z) / 2, last] {
            let east = wx_tile::decode(&grid.render(TileCoord::new(z, last, y).unwrap()).unwrap())
                .unwrap();
            let west =
                wx_tile::decode(&grid.render(TileCoord::new(z, 0, y).unwrap()).unwrap()).unwrap();
            for r in 0..TILE_SAMPLES {
                assert_eq!(
                    east.bands[0][r * TILE_SAMPLES + TILE_SAMPLES - 1],
                    west.bands[0][r * TILE_SAMPLES],
                    "z{z} y{y} row {r}"
                );
            }
        }
    }
    // Between the last column (179.9 E) and the first (180 E): the mean of
    // the two, read through a tile at full resolution.
    let z = 12;
    let n = f64::from(1u32 << z);
    let (lon, lat) = (179.95, 40.0);
    let (x, y) = (lon_to_x(lon) * n, lat_to_y(lat) * n);
    let t = TileCoord::new(z, x.floor() as u32, y.floor() as u32).unwrap();
    let tile = wx_tile::decode(&grid.render(t).unwrap()).unwrap();
    let (c, r) = (
        ((x - x.floor()) * TILE_CELLS as f64).round() as usize,
        ((y - y.floor()) * TILE_CELLS as f64).round() as usize,
    );
    let (slon, slat) = t.sample_lonlat(c, r);
    let got = temp.bands[0]
        .dequantize(tile.bands[0][r * TILE_SAMPLES + c])
        .unwrap();
    let (fi, fj) = f.proj.lonlat_to_ij(slon, slat);
    let (j0, tj) = (fj.floor() as usize, fj - fj.floor());
    let ti = fi - 3599.0;
    assert!((0.0..=1.0).contains(&ti), "{fi}");
    let at = |i: usize, j: usize| f64::from(f.values[j * 3600 + i]) - 273.15;
    let want = (1.0 - tj) * ((1.0 - ti) * at(3599, j0) + ti * at(0, j0))
        + tj * ((1.0 - ti) * at(3599, j0 + 1) + ti * at(0, j0 + 1));
    assert!((f64::from(got) - want).abs() < 0.011, "{got} vs {want}");
}

/// AROME 0.025 degree: two lead times of one parameter back to back (the
/// layout of its 6-hour packages), CCSDS with a bit-map over the trapezoidal
/// domain's missing corners.
#[test]
fn arome_ccsds_with_bitmap_and_two_lead_times() {
    let bytes = fixture("arome0025_t12z_SP1_00H06H__tp_0-1_0-2.grib2");
    let (infos, decoded) = decode_all(&bytes);
    assert_eq!(infos.len(), 2);
    assert_eq!(infos[1].offset, 261_949);
    for (k, info) in infos.iter().enumerate() {
        assert!(
            info.is(0, 1, 52) && info.at_level(1, None) && info.bitmap,
            "{info:?}"
        );
        assert_eq!(info.reference_time, utc(2026, 11, 6, 12, 0));
        assert_eq!(
            info.time,
            Some(Timing::Interval {
                start: utc(2026, 11, 6, 12, 0),
                end: utc(2026, 11, 6, 13 + k as u32, 0),
                process: Some(1),
            })
        );
        assert_eq!(info.unsupported, None);
    }
    // A selector cannot tell the two apart: the provider-neutral path can.
    let tp = FieldSel {
        var: "TPRATE",
        level: "surface",
        stat: Stat::RunTotal,
    };
    let err = format!("{:#}", decode(&bytes, &[tp]).unwrap_err());
    assert!(err.contains("from different runs, times or grids"), "{err}");
    let rows = [
        (0, 0, None),
        (179, 1037, Some((43, 1079))),
        (358, 951, Some((86, 1036))),
        (537, 878, Some((122, 999))),
        (716, 18, Some((154, 968))),
    ];
    for d in &decoded {
        let f = &d.field;
        assert_eq!((f.nx, f.ny), (1121, 717));
        assert_eq!(missing(f), 138_076);
        assert_rows("AROME tp", f, &rows);
        assert_corners(
            "AROME",
            f,
            &[
                (0, 55.4, -12.0),
                (1120, 55.4, 16.0),
                (802_636, 37.5, -12.0),
                (803_756, 37.5, 16.0),
            ],
            1e-9,
        );
    }
    assert_values(
        "AROME tp 0-1",
        &decoded[0].field,
        &[
            (1581, Some(0.0)),
            (267_919, None),
            (171_881, Some(0.693_359_375)),
            (467_247, Some(0.039_550_781_25)),
            (567_930, Some(9.941_406_25)),
            (585_866, Some(29.832_031_25)),
        ],
        1e-6,
    );
    assert_values(
        "AROME tp 0-2",
        &decoded[1].field,
        &[
            (267_919, None),
            (171_746, Some(0.566_406_25)),
            (467_247, Some(0.051_757_812_5)),
            (569_258, Some(2.111_328_125)),
            (589_235, Some(32.301_757_812_5)),
        ],
        1e-6,
    );
    // The last hour's amount from the two run totals.
    let hour = wx_pipeline::step_per_hour(&decoded[1].field, &decoded[0].field, 1.0).unwrap();
    assert_eq!(missing(&hour), 138_076);
    let k = 467_247;
    assert!((hour.values[k] - (0.051_757_812_5 - 0.039_550_781_25) as f32).abs() < 1e-6);

    // Masked points are no data in tiles, and tiles agree on shared edges
    // over the masked corners and the domain's edge.
    let temp = layer("temp").unwrap();
    let grid = FrameGrid::build(temp, &raster(&decoded[0].field, 0.0));
    let at = |lon: f64, lat: f64| {
        let z = 8;
        let n = f64::from(1u32 << z);
        let (x, y) = (lon_to_x(lon) * n, lat_to_y(lat) * n);
        let t = TileCoord::new(z, x.floor() as u32, y.floor() as u32).unwrap();
        let tile = wx_tile::decode(&grid.render(t)?).ok()?;
        let c = ((x - x.floor()) * TILE_CELLS as f64).round() as usize;
        let r = ((y - y.floor()) * TILE_CELLS as f64).round() as usize;
        temp.bands[0].dequantize(tile.bands[0][r * TILE_SAMPLES + c])
    };
    assert_eq!(at(-11.9, 55.3), None, "north-west corner is masked");
    assert!(at(2.35, 48.85).is_some(), "Paris");
    assert_shared_edges(&grid, 6);
}

/// AROME overseas (Polynesia): CCSDS without a bit-map east of the
/// antimeridian, and a field with no bits per value (no snow in the
/// tropics), which is its reference value throughout.
#[test]
fn arome_overseas_ccsds_and_constant_fields() {
    let bytes = fixture("aromeom_polyn_t12z_SP1_001H__2t_tsnowp.grib2");
    let (infos, decoded) = decode_all(&bytes);
    assert_eq!(infos.len(), 2);
    assert!(infos[0].is(0, 0, 0) && infos[1].is(0, 1, 66));
    assert_eq!(infos[1].length, 207, "no data section to speak of");
    let f = &decoded[0].field;
    assert_eq!((f.nx, f.ny), (521, 507));
    assert_corners(
        "POLYN",
        f,
        &[
            (0, -12.6, 202.5),
            (520, -12.6, 215.5),
            (263_626, -25.25, 202.5),
            (264_146, -25.25, 215.5),
        ],
        1e-9,
    );
    assert_values(
        "POLYN 2t",
        f,
        &[
            (0, Some(299.806_182_861_328_1)),
            (520, Some(300.818_145_751_953_1)),
            (17_055, Some(303.204_864_501_953_1)),
            (106_603, Some(290.804_718_017_578_1)),
            (230_746, Some(294.857_208_251_953_1)),
            (264_146, Some(293.630_401_611_328_1)),
        ],
        1e-6,
    );
    assert!(decoded[1].field.values.iter().all(|&v| v == 0.0));
    assert_eq!(decoded[1].time.valid(), utc(2026, 11, 6, 13, 0));
    // The grid is west of the antimeridian in -180..180 terms.
    let b = FrameGrid::build(layer("temp").unwrap(), &raster(f, -273.15)).bounds();
    assert!(b.west > -157.6 && b.east < -144.4 && !b.wraps(), "{b:?}");
}

// ---------------------------------------------------------------------------
// Bit-maps with simple packing: ICON-D2 and FMI
// ---------------------------------------------------------------------------

/// ICON-D2's regular grid: the icosahedral domain resampled to a rectangle,
/// with its corners masked by a bit-map; rows run south to north.
#[test]
fn icon_d2_bitmap() {
    let bytes = fixture("icond2_t12z_003__t_2m.grib2");
    let set = decode(
        &bytes,
        &[FieldSel {
            var: "TMP",
            level: "2 m above ground",
            stat: Stat::Instant,
        }],
    )
    .unwrap();
    assert_eq!(set.valid_time, utc(2026, 11, 6, 15, 0));
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (1215, 746));
    let g = latlon(f);
    assert!(!g.periodic && g.dlat > 0.0);
    assert!((g.lon0 - 356.06).abs() < 1e-9 && (g.lat0 - 43.18).abs() < 1e-9);
    assert_eq!(missing(f), 151_528);
    assert_rows(
        "ICON-D2",
        f,
        &[
            (0, 42, Some((181, 222))),
            (186, 965, Some((141, 1105))),
            (373, 1037, Some((99, 1135))),
            (559, 1128, Some((48, 1175))),
            (745, 55, Some((628, 811))),
        ],
    );
    assert_corners(
        "ICON-D2",
        f,
        &[
            (0, 43.18, -3.94),
            (1214, 43.18, 20.34),
            (905_175, 58.08, -3.94),
            (906_389, 58.08, 20.34),
        ],
        1e-9,
    );
    assert_values(
        "ICON-D2 2t",
        f,
        &[
            (0, None),
            (906_389, None),
            (453_195, None),
            (18_606, Some(290.464_019_775_390_6)),
            (162_135, Some(261.868_316_650_390_6)),
            (302_130, Some(276.371_734_619_140_6)),
            (557_433, Some(279.624_176_025_390_6)),
            (797_477, Some(282.980_621_337_890_6)),
            (852_261, Some(283.098_785_400_390_6)),
        ],
        1e-6,
    );
    let grid = FrameGrid::build(layer("temp").unwrap(), &raster(f, -273.15));
    assert_eq!(grid.coverage_at(-3.9, 43.2), 0.0, "masked corner");
    assert_eq!(grid.coverage_at(10.0, 50.5), 1.0, "middle of the domain");
    assert_shared_edges(&grid, 6);
}

/// FMI's download service: seven parameters in one small file on a Lambert
/// grid with a bit-map, among them a gust maximum over the last hour and a
/// precipitation rate with no bits per value.
#[test]
fn fmi_several_parameters_in_one_file() {
    let bytes = fixture("fmi_harmonie_t15z_6h__multi.grib2");
    let infos = scan(&bytes).unwrap();
    let params: Vec<(u8, u8, u8)> = infos
        .iter()
        .map(|m| (m.discipline, m.category, m.number))
        .collect();
    assert_eq!(
        params,
        [
            (0, 0, 0),
            (0, 2, 2),
            (0, 2, 3),
            (0, 2, 22),
            (0, 1, 7),
            (0, 6, 1),
            (0, 0, 6)
        ]
    );
    assert!(
        infos
            .iter()
            .all(|m| m.grid_template == 30 && m.bitmap && m.unsupported.is_none())
    );
    assert_eq!(
        infos[3].time,
        Some(Timing::Interval {
            start: utc(2026, 11, 6, 20, 0),
            end: utc(2026, 11, 6, 21, 0),
            process: Some(2),
        })
    );
    assert_eq!(
        infos[1].surface,
        Some(Surface {
            kind: 103,
            value: Some(10.0)
        })
    );
    // Chosen fields, in the order asked, duplicates allowed.
    let d = decode_messages(&bytes, &[5, 0, 3, 0]).unwrap();
    assert_eq!(d.len(), 4);
    assert!(d[0].info.is(0, 6, 1) && d[1].info.is(0, 0, 0) && d[2].info.is(0, 2, 22));
    assert_eq!(d[1].field.values, d[3].field.values);
    let t = &d[1].field;
    assert_eq!((t.nx, t.ny), (22, 48));
    assert!(matches!(t.proj, GridProjection::Lambert(_)));
    assert_corners(
        "FMI",
        t,
        &[
            (0, 58.990952, 18.961655),
            (21, 58.958547426, 19.873003734),
            (1034, 60.043270671, 19.092076682),
            (1055, 60.009763397, 20.033208026),
        ],
        1e-5,
    );
    assert_values(
        "FMI 2t",
        t,
        &[
            (0, Some(278.850_006)),
            (352, Some(277.449_982)),
            (922, Some(279.25)),
        ],
        1e-6,
    );
    assert_values(
        "FMI tcc",
        &d[0].field,
        &[(1034, Some(97.300_003)), (154, Some(34.700_001))],
        1e-6,
    );
    assert_values(
        "FMI gust",
        &d[2].field,
        &[(726, Some(4.800_000_2)), (0, Some(8.0))],
        1e-6,
    );
    let rate = &decode_messages(&bytes, &[4]).unwrap()[0].field;
    assert!(rate.values.iter().all(|&v| v == 0.0));
    assert!(
        decode_messages(&bytes, &[7])
            .unwrap_err()
            .to_string()
            .contains("no field 7")
    );
    // The NOAA path still works on such a file where the names are NOAA's.
    let set = decode(
        &bytes,
        &[
            FieldSel {
                var: "TMP",
                level: "2 m above ground",
                stat: Stat::Instant,
            },
            FieldSel {
                var: "GUST",
                level: "10 m above ground",
                stat: Stat::StepMax,
            },
        ],
    )
    .unwrap();
    assert_eq!(set.fields[1].values, d[2].field.values);
    assert_eq!(set.valid_time, utc(2026, 11, 6, 21, 0));
}

// ---------------------------------------------------------------------------
// Walking files without an index
// ---------------------------------------------------------------------------

/// Section 0 gives each message's length, so a file is walked 16 bytes at
/// a time; a message's first few hundred bytes describe it as the scan of
/// the whole message does.
#[test]
fn walking_message_heads() {
    for name in [
        "fmi_harmonie_t15z_6h__multi.grib2",
        "arome0025_t12z_SP1_00H06H__tp_0-1_0-2.grib2",
        "aromeom_polyn_t12z_SP1_001H__2t_tsnowp.grib2",
        "ifs0p1_t00z_3h__2t_sfc.grib2",
        "hrdps_t12z_PT003H__PRATE_Sfc.grib2",
        "gfs_t06z_f024__TMP_2mabove.grib2",
        "hrrr_t10z_f06__TMP_2mabove.grib2",
    ] {
        let bytes = fixture(name);
        let infos = scan(&bytes).unwrap();
        let mut at = 0usize;
        let mut walked = 0;
        while at < bytes.len() {
            let len = message_length(&bytes[at..at + 16]).unwrap() as usize;
            let info = &infos[walked];
            assert_eq!((info.offset, info.length), (at, len), "{name}");
            // Ask for 64 bytes, then as many as the answer says, as an
            // adapter making range requests would.
            let mut want = 64;
            let head = loop {
                match scan_head(&bytes[at..at + want.min(len)]).unwrap() {
                    HeadScan::Described(m) => break m,
                    HeadScan::NeedBytes(n) => {
                        assert!(n > want && n <= len, "{name}: {n} after {want}");
                        want = n;
                    }
                }
            };
            assert!(want < 2000, "{name}: {want} bytes of head");
            assert_eq!(
                *head,
                MessageInfo {
                    offset: 0,
                    ..info.clone()
                },
                "{name}"
            );
            at += len;
            walked += 1;
        }
        assert_eq!(walked, infos.len(), "{name}");
    }
    assert!(
        message_length(b"GRIB\0\0\0\x01\0\0\0\0\0\0\0\x40").is_err(),
        "GRIB1"
    );
    assert!(message_length(b"GRIB").is_err());
    assert!(message_length(b"BIRG\0\0\0\x02\0\0\0\0\0\0\0\x40").is_err());
    assert_eq!(
        message_length(b"GRIB\0\0\0\x02\0\0\0\0\0\x01\0\x00").unwrap(),
        65_536
    );
    assert!(matches!(
        scan_head(b"GRIB").unwrap(),
        HeadScan::NeedBytes(16)
    ));
    assert!(
        scan_head(b"GRIB\0\0\0\x02\0\0\0\0\0\0\0\x40\0\0\0\x15\x09").is_err(),
        "section 9"
    );
}

#[test]
fn the_message_api_refuses_what_it_cannot_place() {
    // Offsets into an inflated buffer would not be the caller's offsets.
    let gz = fixture("MRMS_PrecipFlag_00.00_20261106-114800.grib2.gz");
    assert!(scan(&gz).unwrap_err().to_string().contains("gzip"));
    assert!(decode_messages(&gz, &[0]).is_err());
    // Several files' ranges concatenated in any order scan as one.
    let a = fixture("aromeom_polyn_t12z_SP1_001H__2t_tsnowp.grib2");
    let b = fixture("ifs0p1_t00z_3h__2t_sfc.grib2");
    let both = [b.clone(), a.clone()].concat();
    let infos = scan(&both).unwrap();
    assert_eq!(infos.len(), 3);
    assert_eq!(infos[1].offset, b.len());
    let d = decode_messages(&both, &[2, 0]).unwrap();
    assert_eq!((d[0].field.nx, d[1].field.nx), (521, 3600));
}

/// Tiles of one part at zoom `z` agree with their east and south
/// neighbours on the samples they share.
fn assert_shared_edges(grid: &FrameGrid, z: u8) {
    let n = TILE_SAMPLES;
    let render = |t: TileCoord| grid.render(t).map(|b| wx_tile::decode(&b).unwrap());
    let mut compared = 0;
    for t in grid.covering(z) {
        let Some(a) = render(t) else { continue };
        if let Some(b) = TileCoord::new(z, t.x + 1, t.y).and_then(render) {
            for r in 0..n {
                assert_eq!(
                    a.bands[0][r * n + n - 1],
                    b.bands[0][r * n],
                    "{t:?} east, row {r}"
                );
            }
            compared += 1;
        }
        if let Some(b) = TileCoord::new(z, t.x, t.y + 1).and_then(render) {
            assert_eq!(&a.bands[0][(n - 1) * n..], &b.bands[0][..n], "{t:?} south");
            compared += 1;
        }
    }
    assert!(compared > 4, "{compared} edges");
}

// ---------------------------------------------------------------------------
// Bad input
// ---------------------------------------------------------------------------

fn patch(mut b: Vec<u8>, at: usize, bytes: &[u8]) -> Vec<u8> {
    b[at..at + bytes.len()].copy_from_slice(bytes);
    b
}

fn decode_err(bytes: &[u8]) -> String {
    format!("{:#}", decode_messages(bytes, &[0]).unwrap_err())
}

/// JPEG 2000 headers the decoder would size buffers from (image area, tile
/// count, components, precincts), and codestreams it would decode to
/// something other than one plane of integers, are refused before it runs:
/// an allocation that fails aborts the process, which no guard can catch.
#[test]
fn hostile_jpeg2000_is_refused_before_unpacking() {
    // HRDPS PRATE: section 5 at byte 155 (bits per value in its octet 20),
    // section 7 at 184, so the codestream starts at 189: SIZ's Xsiz, Ysiz at
    // 197 and 201, XTsiz, YTsiz at 213 and 217, Csiz at 229, Ssiz, XRsiz at
    // 231 and 232; a comment, then COD at 270 (Scod at 274, the transform at
    // 283); QCD; the tile-part (SOT) at 305, Psot at 311.
    let j2k = fixture("hrdps_t12z_PT003H__PRATE_Sfc.grib2");
    assert_eq!(&j2k[189..193], b"\xff\x4f\xff\x51");
    assert_eq!(&j2k[270..272], b"\xff\x52");
    assert_eq!(&j2k[305..307], b"\xff\x90");
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            patch(j2k.clone(), 197, &[0x7f, 0xff, 0xff, 0xff]),
            "JPEG 2000 image of 2147483647 x 1290 points does not hold the 3276600 values",
        ),
        (
            patch(
                patch(j2k.clone(), 197, &(1u32 << 20).to_be_bytes()),
                201,
                &(1u32 << 20).to_be_bytes(),
            ),
            "does not hold the 3276600 values",
        ),
        (
            patch(
                patch(j2k.clone(), 213, &16u32.to_be_bytes()),
                217,
                &16u32.to_be_bytes(),
            ),
            "in 12879 tiles is not supported",
        ),
        (patch(j2k.clone(), 213, &0u32.to_be_bytes()), "tile grid"),
        (
            patch(j2k.clone(), 229, &3u16.to_be_bytes()),
            "with 3 components",
        ),
        (patch(j2k.clone(), 231, &[0x1f]), "samples of 32 bits"),
        (patch(j2k.clone(), 231, &[0x8b]), "signed"),
        (patch(j2k.clone(), 232, &[2]), "subsampled"),
        (patch(j2k.clone(), 283, &[0]), "irreversible"),
        // Precinct sizes announced but absent from the marker.
        (
            patch(j2k.clone(), 274, &[1]),
            "coding style marker is malformed",
        ),
        (
            patch(j2k.clone(), 305, &[0xff, 0x93]),
            "unexpected JPEG 2000 marker 0xff93",
        ),
        (
            patch(j2k.clone(), 311, &u32::MAX.to_be_bytes()),
            "claims 4294967295 bytes",
        ),
        (
            patch(j2k.clone(), 189, b"\x00\x00\x00\x0c"),
            "does not start with a JPEG 2000 codestream",
        ),
    ];
    for (bytes, want) in cases {
        let err = decode_err(&bytes);
        assert!(err.contains(want), "want {want:?}, got {err}");
    }
    // With no bits per value the codestream is never read.
    let constant = patch(patch(j2k.clone(), 155 + 19, &[0]), 197, &[0xff; 4]);
    assert!(decode_messages(&constant, &[0]).is_ok());
}

/// CCSDS parameters rust-aec would reject, or that grib-rs would size its
/// output from or read back wrongly, are refused before unpacking.
#[test]
fn hostile_ccsds_is_refused_before_unpacking() {
    // Polynesia 2t: section 5 at byte 143: bits per value at 162, the CCSDS
    // options at 164, block size at 165, reference sample interval at 166.
    let aec = fixture("aromeom_polyn_t12z_SP1_001H__2t_tsnowp.grib2");
    assert_eq!((aec[162], aec[164], aec[165]), (16, 0x0e, 32));
    for (at, bytes, want) in [
        (162, &[255u8][..], "255-bit CCSDS values"),
        (162, &[33], "33-bit CCSDS values"),
        (165, &[7], "block size 7"),
        (166, &[0, 0], "reference sample interval 0"),
        (166, &[0x13, 0x88], "reference sample interval 5000"),
        (164, &[0x0a], "CCSDS options 0x0a"),
        (164, &[0x0f], "CCSDS options 0x0f"),
    ] {
        let err = decode_err(&patch(aec.clone(), at, bytes));
        assert!(err.contains(want), "want {want:?}, got {err}");
    }
    // 20 bits in 4 octets each is not how grib-rs reads them back.
    let err = decode_err(&patch(patch(aec.clone(), 162, &[20]), 164, &[0x0c]));
    assert!(err.contains("20-bit samples in 4 octets"), "{err}");
}

/// Corrupt packed data, or headers past the checks above, must give an
/// error or some values, never a panic or an abort.
#[test]
fn corrupt_jpeg2000_and_ccsds_are_errors_not_panics() {
    let mut runs = 0;
    // Where the packed data starts: RDPS's codestream (laid out as HRDPS's)
    // and the two CCSDS streams.
    for (name, start) in [
        ("rdps_t12z_PT003H__PrecipRate_Sfc.grib2", 189),
        ("aromeom_polyn_t12z_SP1_001H__2t_tsnowp.grib2", 179),
        ("arome0025_t12z_SP1_00H06H__tp_0-1_0-2.grib2", 100_673),
    ] {
        let good = fixture(name);
        let len = message_length(&good).unwrap() as usize;
        // Header bytes one by one, then the packed data at intervals.
        let positions = (start..start + 120).chain((start..len - 4).step_by((len - start) / 40));
        for pos in positions {
            let mut b = good.clone();
            b[pos] ^= 0x5a;
            let _ = decode_messages(&b, &[0]);
            runs += 1;
        }
        // Packed data cut short (the section and message lengths shrunk to
        // match).
        let s7 = (16..len).find(|&k| {
            good[k + 4] == 7 && {
                let l = u32::from_be_bytes(good[k..k + 4].try_into().unwrap()) as usize;
                k + l == len - 4
            }
        });
        if let Some(s7) = s7 {
            let cut = (len - 4 - s7) / 2;
            let mut b = good[..len - 4 - cut].to_vec();
            b.extend_from_slice(b"7777");
            let new_len = b.len() as u64;
            b[8..16].copy_from_slice(&new_len.to_be_bytes());
            let l = u32::from_be_bytes(b[s7..s7 + 4].try_into().unwrap()) - cut as u32;
            b[s7..s7 + 4].copy_from_slice(&l.to_be_bytes());
            assert!(decode_messages(&b, &[0]).is_err(), "{name} truncated");
        }
    }
    assert!(runs > 450, "{runs}");
}

// ---------------------------------------------------------------------------
// Whole samples from the research scratchpad (not fixtures: tens of MB).
// `cargo test --release -p wx-pipeline --test providers -- --ignored --nocapture`
// ---------------------------------------------------------------------------

const RESEARCH: &str = "/private/tmp/claude-501/-Users-tahseenarnan-Development/d60ae49e-5172-4faa-aff4-42700185b923/scratchpad/research";

fn research(path: &str) -> Option<Vec<u8>> {
    let full = format!("{RESEARCH}/{path}");
    let bytes = std::fs::read(&full).ok();
    if bytes.is_none() {
        println!("skipped: no sample at {full}");
    }
    bytes
}

const fn sel(var: &'static str, level: &'static str, stat: Stat) -> FieldSel {
    FieldSel { var, level, stat }
}

/// DMI writes its deterministic run as the control member of an ensemble
/// (templates 4.1 and 4.11): its `.index` names match them, a last-hour
/// gust maximum included.
#[test]
#[ignore]
fn dmi_control_member_records_match_inventory_names() {
    let Some(bytes) = research("europe-models/dmi/dini_15Z_3h_subset.grib") else {
        return;
    };
    let infos = scan(&bytes).unwrap();
    let templates: Vec<u16> = infos.iter().map(|m| m.product_template).collect();
    assert_eq!(templates, [1, 1, 11, 11, 11, 1]);
    let set = decode(
        &bytes,
        &[
            sel("TMP", "2 m above ground", Stat::Instant),
            sel("GUST", "10 m above ground", Stat::StepMax),
            sel("TPRATE", "surface", Stat::RunTotal),
            sel("TSPRATE", "surface", Stat::RunTotal),
            sel(
                "var discipline=0 center=94 local_table=1 parmcat=6 parm=199",
                "0 m above ground",
                Stat::Instant,
            ),
        ],
    )
    .unwrap();
    assert_eq!(set.valid_time, utc(2026, 11, 6, 18, 0));
    let f = &set.fields[0];
    assert_eq!((f.nx, f.ny), (1906, 1606));
    assert!(f.grid_relative_winds && matches!(f.proj, GridProjection::Lambert(_)));
    let (t, cloud) = (&set.fields[0].values, &set.fields[4].values);
    assert!(t.iter().all(|v| (200.0..320.0).contains(v)));
    assert!(cloud.iter().all(|v| (0.0..=1.0001).contains(v)));
    // Only the run total exists, so the last hour is not in this file.
    let err = format!(
        "{:#}",
        decode(&bytes, &[sel("TPRATE", "surface", Stat::StepPerHour)]).unwrap_err()
    );
    assert!(
        err.contains("no GRIB message holds TPRATE:surface"),
        "{err}"
    );
}

/// NBM's one-hour precipitation windows are `StepPerHour` records; the
/// probability record with the same name is not.
#[test]
#[ignore]
fn nbm_one_hour_window_is_the_step_per_hour_record() {
    let Some(bytes) = research("noaa/nbm/f001.hi.grib2") else {
        return;
    };
    let set = decode(&bytes, &[sel("APCP", "surface", Stat::StepPerHour)]).unwrap();
    assert_eq!(
        set.valid_time - set.reference_time,
        chrono::TimeDelta::hours(1)
    );
    let infos = scan(&bytes).unwrap();
    let hits: Vec<&MessageInfo> = infos.iter().filter(|m| m.is(0, 1, 8)).collect();
    assert!(hits.len() >= 2, "deterministic and probability records");
    let det = hits.iter().find(|m| m.product_template == 8).unwrap();
    let d = decode_messages(&bytes, &[infos.iter().position(|m| m == *det).unwrap()]).unwrap();
    assert_eq!(d[0].field.values, set.fields[0].values);
}

/// ICON-D2 sub-hourly output: four instants a quarter of an hour apart in
/// one file, and run totals in minutes.
#[test]
#[ignore]
fn icon_d2_quarter_hours() {
    let Some(bytes) = research(
        "europe-models/dwd/icon-d2_germany_regular-lat-lon_single-level_2026110612_003_2d_dbz_cmax.grib2",
    ) else {
        return;
    };
    let infos = scan(&bytes).unwrap();
    let times: Vec<_> = infos.iter().map(|m| m.valid_time().unwrap()).collect();
    assert_eq!(times, [0, 15, 30, 45].map(|m| utc(2026, 11, 6, 15, m)));
    let d = decode_messages(&bytes, &[3, 0]).unwrap();
    assert_eq!(d[0].time, Timing::Instant(utc(2026, 11, 6, 15, 45)));
    assert_eq!(missing(&d[0].field), missing(&d[1].field));
    let Some(bytes) = research(
        "europe-models/dwd/icon-d2_germany_regular-lat-lon_single-level_2026110612_006_2d_tot_prec.grib2",
    ) else {
        return;
    };
    let infos = scan(&bytes).unwrap();
    let ends: Vec<_> = infos
        .iter()
        .map(|m| match m.time {
            Some(Timing::Interval {
                start,
                end,
                process: Some(1),
            }) => {
                assert_eq!(start, utc(2026, 11, 6, 12, 0));
                end
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        ends,
        [(18, 0), (18, 15), (18, 30), (18, 45)].map(|(h, m)| utc(2026, 11, 6, h, m))
    );
}

/// Decode times per field on this machine, the scan included: the JPEG
/// 2000 and CCSDS decoders run on one thread per field.
#[test]
#[ignore]
fn decode_timings() {
    use std::time::{Duration, Instant};
    let best = |f: &dyn Fn()| {
        (0..3)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let mut cases: Vec<(String, Vec<u8>)> = vec![
        (
            "ECMWF IFS 0.1 2t (fixture, CCSDS 9 bit)".into(),
            fixture("ifs0p1_t00z_3h__2t_sfc.grib2"),
        ),
        (
            "HRDPS PRATE (fixture, JPEG 2000 12 bit)".into(),
            fixture("hrdps_t12z_PT003H__PRATE_Sfc.grib2"),
        ),
        (
            "GDPS dew point (fixture, JPEG 2000 9 bit)".into(),
            fixture("gdps_t12z_PT003H__DewPoint_AGL-2m.grib2"),
        ),
    ];
    for (label, path) in [
        (
            "HRDPS TMP (JPEG 2000 16 bit)",
            "canada-world/hrdps/20261106T12Z_MSC_HRDPS_TMP_AGL-2m_RLatLon0.0225_PT003H.grib2",
        ),
        (
            "HRDPS APCP (JPEG 2000 24 bit)",
            "canada-world/hrdps/20261106T12Z_MSC_HRDPS_APCP_Sfc_RLatLon0.0225_PT003H.grib2",
        ),
        (
            "GDPS AirTemp (JPEG 2000 12 bit)",
            "canada-world/gdps/20261106T12Z_MSC_GDPS_AirTemp_AGL-2m_LatLon0.15_PT003H.grib2",
        ),
        (
            "GDPS PrecipRate (JPEG 2000 24 bit)",
            "canada-world/gdps/20261106T12Z_MSC_GDPS_PrecipRate_Sfc_LatLon0.15_PT003H.grib2",
        ),
        (
            "ECMWF IFS 0.1, 12 fields (CCSDS)",
            "europe-models/ecmwf/ifs0p1_00z_3h_subset.grib2",
        ),
        (
            "AROME 0.01 SP1, 6 fields (CCSDS, bit-map)",
            "europe-models/meteofrance/arome__001__SP1__03H__2026-11-06T12:00:00Z.grib2",
        ),
    ] {
        if let Some(b) = research(path) {
            cases.push((label.into(), b));
        }
    }
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    for (label, bytes) in &cases {
        let n = scan(bytes).unwrap().len();
        let all: Vec<usize> = (0..n).collect();
        let points = scan(bytes).unwrap()[0].num_points;
        let scan_t = best(&|| {
            scan(bytes).unwrap();
        });
        let par = best(&|| {
            decode_messages(bytes, &all).unwrap();
        });
        let seq = one.install(|| {
            best(&|| {
                decode_messages(bytes, &all).unwrap();
            })
        });
        println!(
            "{label}: {n} field(s) of {points} points; scan {:.2} ms; decode {:.0} ms on {} threads, {:.0} ms on one ({:.0} ms per field)",
            ms(scan_t),
            ms(par),
            rayon::current_num_threads(),
            ms(seq),
            ms(seq) / n as f64
        );
    }
}

/// Fields whose templates are not decoded are still listed, with the
/// reason, and refused by name: ICON's global icosahedral grid (3.101) and
/// AROME's simulated satellite image (product template 4.32).
#[test]
#[ignore]
fn unsupported_templates_are_listed_with_the_reason() {
    for (path, why) in [
        (
            "europe-models/dwd/icon_global_icosahedral_single-level_2026110612_003_T_2M.grib2",
            "grid definition template 3.101 is not supported",
        ),
        (
            "europe-models/meteofrance/arome__001__SP3__03H__2026-11-06T12:00:00Z.grib2",
            "product definition template 4.32 is not supported",
        ),
    ] {
        let Some(bytes) = research(path) else {
            continue;
        };
        let infos = scan(&bytes).unwrap();
        let info = &infos[0];
        let reason = info.unsupported.as_deref().unwrap_or_default();
        assert!(reason.contains(why), "{path}: {reason}");
        println!("{path}: {info:?}");
        let err = format!("{:#}", decode_messages(&bytes, &[0]).unwrap_err());
        assert!(err.contains(why), "{err}");
    }
}

// ---------------------------------------------------------------------------
// Lambert definitions (template 3.30), from the HRRR fixture rewritten
// ---------------------------------------------------------------------------

/// GRIB's sign-and-magnitude micro-degrees.
fn micro_degrees(v: f64) -> [u8; 4] {
    let m = (v.abs() * 1e6).round() as u32;
    (if v < 0.0 { m | 0x8000_0000 } else { m }).to_be_bytes()
}

/// HRRR's 2 m temperature with its section 3 (at byte 37) rewritten: La1 at
/// byte 75, LaD at 84, the projection centre flag at 100, Latin1 and Latin2
/// at 102 and 106.
fn hrrr_lambert(la1: f64, lad: f64, latin1: f64, latin2: f64, flag: u8) -> Vec<u8> {
    let mut b = fixture("hrrr_t10z_f06__TMP_2mabove.grib2");
    for (at, v) in [(75, la1), (84, lad), (102, latin1), (106, latin2)] {
        b[at..at + 4].copy_from_slice(&micro_degrees(v));
    }
    b[100] = flag;
    b
}

const TMP_2M: FieldSel = FieldSel {
    var: "TMP",
    level: "2 m above ground",
    stat: Stat::Instant,
};

/// Dx and Dy are taken as lengths on the projection plane, as eccodes and
/// wgrib2 take them, which agrees with template 3.30 (lengths at LaD) only
/// when LaD is a standard parallel; elsewhere the grid is refused rather than
/// placed by one reading or the other. Corners from eccodes.
#[test]
fn lambert_lad_must_be_a_standard_parallel() {
    let original = hrrr_lambert(21.138123, 38.5, 38.5, 38.5, 0);
    assert_eq!(
        original,
        fixture("hrrr_t10z_f06__TMP_2mabove.grib2"),
        "offsets are right"
    );
    let err = format!(
        "{:#}",
        decode(&hrrr_lambert(21.138123, 50.0, 38.5, 38.5, 0), &[TMP_2M]).unwrap_err()
    );
    assert!(err.contains("LaD = 50 off its standard parallels"), "{err}");
    // A secant cone with LaD on its second parallel.
    let set = decode(&hrrr_lambert(21.138123, 45.0, 38.5, 45.0, 0), &[TMP_2M]).unwrap();
    assert_corners(
        "secant",
        &set.fields[0],
        &[
            (1798, 21.318760, 287.068887),
            (1_903_342, 47.481323, 225.073590),
            (1_905_140, 47.759863, 299.025265),
            (1_080_300, 40.559620, 262.093510),
        ],
        2e-5,
    );
}

/// A cone over the South Pole (standard parallels at 38.5 S, flag 0x80)
/// decodes where eccodes puts it, is stored and loaded, and renders; the
/// flag and the parallels must agree.
#[test]
fn southern_lambert_grids() {
    let south = hrrr_lambert(-21.138123, -38.5, -38.5, -38.5, 0x80);
    let set = decode(&south, &[TMP_2M]).unwrap();
    let f = &set.fields[0];
    assert_corners(
        "south",
        f,
        &[
            (0, -21.138123, 237.280472),
            (1798, -21.140547, 287.710282),
            (1_903_342, 3.171065, 243.340928),
            (1_905_140, 3.169552, 281.651895),
            (1_080_300, -9.271745, 262.520027),
        ],
        2e-5,
    );
    for (flag, latin) in [(0u8, -38.5), (0x80, 38.5)] {
        let b = hrrr_lambert(-21.138123, latin, latin, latin, flag);
        let err = format!("{:#}", decode(&b, &[TMP_2M]).unwrap_err());
        assert!(err.contains("do not define a cone over the"), "{err}");
    }
    let err = format!(
        "{:#}",
        decode(&hrrr_lambert(-21.1, -38.5, -38.5, -38.5, 0xc0), &[TMP_2M]).unwrap_err()
    );
    assert!(err.contains("bipolar"), "{err}");
    // Each sample holding its own latitude renders where it belongs.
    let temp = layer("temp").unwrap();
    let lat_raster = Raster {
        nx: f.nx,
        ny: f.ny,
        proj: f.proj.clone(),
        bands: vec![
            (0..f.nx * f.ny)
                .map(|k| f.proj.ij_to_lonlat((k % f.nx) as f64, (k / f.nx) as f64).1 as f32)
                .collect(),
        ],
    };
    let grid = FrameGrid::build(temp, &lat_raster);
    let back = FrameGrid::from_bytes(&grid.to_bytes()).expect("a southern cone loads");
    assert_eq!(back.projection(), grid.projection());
    for (lon, lat) in [(-97.5, -9.3), (-110.0, -15.0), (-80.0, 0.0)] {
        let z = 7;
        let n = f64::from(1u32 << z);
        let (x, y) = (lon_to_x(lon) * n, lat_to_y(lat) * n);
        let t = TileCoord::new(z, x.floor() as u32, y.floor() as u32).unwrap();
        let tile = wx_tile::decode(&back.render(t).unwrap()).unwrap();
        let (c, r) = (
            ((x - x.floor()) * TILE_CELLS as f64).round() as usize,
            ((y - y.floor()) * TILE_CELLS as f64).round() as usize,
        );
        let got = temp.bands[0]
            .dequantize(tile.bands[0][r * TILE_SAMPLES + c])
            .unwrap();
        let slat = t.sample_lonlat(c, r).1;
        assert!(
            (f64::from(got) - slat).abs() < 0.05,
            "({lon}, {lat}): {got} vs {slat}"
        );
        assert_eq!(back.coverage_at(lon, lat), 1.0);
    }
}

/// The reviewer's case: GDPS alone at 107 W 34 N, zoom 6, showed a rate of
/// 0.02 mm/h with type NONE, which its rule never gives at or above
/// 0.01 mm/h. Every sample of the tiles around it now agrees with the rule.
#[test]
#[ignore]
fn gdps_rate_and_type_agree_in_tiles() {
    let (Some(rate), Some(kind)) = (
        research("canada-world/gdps/20261106T12Z_MSC_GDPS_PrecipRate_Sfc_LatLon0.15_PT003H.grib2"),
        research(
            "canada-world/gdps/20261106T12Z_MSC_GDPS_PrecipType-Instant_Sfc_LatLon0.15_PT003H.grib2",
        ),
    ) else {
        return;
    };
    let fields = [
        decode(&rate, &[]).unwrap().fields.remove(0),
        decode(&kind, &[]).unwrap().fields.remove(0),
    ];
    let precip = layer("precip").unwrap();
    let input = wx_core::InputSpec {
        family: wx_core::Family::Gdps,
        sources: &[],
        product: "",
        fields: &[],
        derive: wx_core::layer::Derive::PrecipTypeCode {
            table: wx_core::layer::PtypeTable::Eccc,
            to_mmh: 3600.0,
        },
        hold_minutes: 60,
        fill_past: false,
    };
    let grid = FrameGrid::build(
        precip,
        &wx_pipeline::derive(precip, &input, &fields).unwrap(),
    );
    let threshold = precip.bands[0].quantize(wx_core::layer::TYPED_RATE_MIN_MMH);
    let none = precip.bands[1].quantize(wx_core::layer::ptype::NONE);
    let z = 6;
    let n = f64::from(1u32 << z);
    let (x, y) = ((lon_to_x(-107.0) * n) as u32, (lat_to_y(34.0) * n) as u32);
    let (mut wet, mut checked) = (0, 0);
    for ty in y - 3..=y + 3 {
        for tx in x - 3..=x + 3 {
            let Some(bytes) = grid.render(TileCoord::new(z, tx, ty).unwrap()) else {
                continue;
            };
            let tile = wx_tile::decode(&bytes).unwrap();
            for k in 0..TILE_SAMPLES * TILE_SAMPLES {
                let (r, t) = (tile.bands[0][k], tile.bands[1][k]);
                if r == 0 || t == 0 {
                    continue;
                }
                checked += 1;
                if r < threshold {
                    assert_eq!(t, none, "tile ({tx}, {ty}) sample {k}");
                } else if t != none {
                    wet += 1;
                }
            }
        }
    }
    println!("{checked} samples, {wet} with a rate and a type");
    assert!(checked > 100_000);
}

/// HRDPS with its cells south of 45 N masked, as the adapter masks the
/// United States: its bounds stop at the data instead of reaching 27 N.
#[test]
fn masked_hrdps_bounds_stop_at_its_data() {
    let set = decode(&fixture("hrdps_t12z_PT003H__PRATE_Sfc.grib2"), &[]).unwrap();
    let f = &set.fields[0];
    let values: Vec<f32> = (0..f.nx * f.ny)
        .map(|k| {
            let lat = f.proj.ij_to_lonlat((k % f.nx) as f64, (k / f.nx) as f64).1;
            if lat < 45.0 { f32::NAN } else { f.values[k] }
        })
        .collect();
    let raster = Raster {
        nx: f.nx,
        ny: f.ny,
        proj: f.proj.clone(),
        bands: vec![values],
    };
    let grid = FrameGrid::build(layer("temp").unwrap(), &raster);
    let b = grid.bounds();
    let whole = f.proj.bounds(f.nx, f.ny);
    println!("masked HRDPS: {b:?}, grid {whole:?}");
    assert!(whole.south < 27.3);
    assert!(b.south > 44.8 && b.south < 45.0, "{b:?}");
    assert!(
        b.north == whole.north && b.west == whole.west && b.east == whole.east,
        "{b:?}"
    );
    let back = FrameGrid::from_bytes(&grid.to_bytes()).unwrap();
    assert_eq!(back.bounds(), b, "computed again on loading");
}
