//! Rendering single parts and stacks of parts, coverage weights and the
//! solid box.

use super::*;
use crate::grid::BandMeta;
use wx_core::frame::{Part, publish_boxes, tile_parts, tile_parts_may, tile_parts_must};
use wx_core::layer::{Aggregate, BandSpec, LayerSpec, layer};
use wx_core::proj::{
    EARTH_RADIUS_NCEP, Earth, GeostationaryGrid, LaeaGrid, LambertGrid, LatLonGrid, MercatorGrid,
    PolarStereoGrid, RotatedLatLonGrid, Sweep, TransverseMercatorGrid,
};
use wx_core::{GridProjection, LonLatBox, Raster};

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn raster(proj: GridProjection, nx: usize, ny: usize, f: impl Fn(usize, usize) -> f32) -> Raster {
    Raster {
        nx,
        ny,
        proj,
        bands: vec![(0..nx * ny).map(|k| f(k % nx, k / nx)).collect()],
    }
}

fn latlon(lon0: f64, lat0: f64, d: f64) -> GridProjection {
    GridProjection::LatLon(LatLonGrid {
        lon0,
        lat0,
        dlon: d,
        dlat: -d,
        periodic: false,
    })
}

fn global(nx: usize, ny: usize) -> GridProjection {
    GridProjection::LatLon(LatLonGrid {
        lon0: 0.0,
        lat0: 90.0,
        dlon: 360.0 / nx as f64,
        dlat: -180.0 / (ny - 1) as f64,
        periodic: true,
    })
}

fn hrrr_proj() -> GridProjection {
    GridProjection::Lambert(LambertGrid::new(
        38.5,
        38.5,
        262.5,
        21.138123,
        237.280472,
        3000.0,
        3000.0,
        EARTH_RADIUS_NCEP,
    ))
}

/// A smooth field with a hole of missing data, on a regional lat/lon grid.
fn latlon_raster(nx: usize, ny: usize) -> Raster {
    raster(latlon(230.005, 54.995, 0.05), nx, ny, |i, j| {
        let (i, j) = (i as f32, j as f32);
        if (i - 300.0).hypot(j - 200.0) < 40.0 {
            f32::NAN
        } else {
            10.0 + 0.02 * i + 0.03 * j + (i * 0.05).sin() * 3.0
        }
    })
}

fn hrrr_like(nx: usize, ny: usize) -> Raster {
    raster(hrrr_proj(), nx, ny, |i, j| {
        (i as f32 * 0.013).sin() * 20.0 + j as f32 * 0.01
    })
}

fn one(g: &FrameGrid, t: TileCoord) -> Option<Tile> {
    render_stack(&[g], t)
}

/// Compares every tile in `tiles` with its east and south neighbours on
/// their shared edge; returns how many edges were compared.
fn assert_shared_edges(
    tiles: &[TileCoord],
    render: impl Fn(TileCoord) -> Option<Tile> + Sync,
) -> usize {
    tiles
        .par_iter()
        .map(|&t| {
            let Some(a) = render(t) else {
                return 0;
            };
            let mut compared = 0;
            if let Some(b) = TileCoord::new(t.z, t.x + 1, t.y).and_then(&render) {
                for band in 0..a.bands.len() {
                    for r in 0..N {
                        assert_eq!(
                            a.bands[band][r * N + N - 1],
                            b.bands[band][r * N],
                            "{t:?} east edge row {r}"
                        );
                    }
                }
                compared += 1;
            }
            if let Some(b) = TileCoord::new(t.z, t.x, t.y + 1).and_then(&render) {
                for band in 0..a.bands.len() {
                    assert_eq!(
                        &a.bands[band][(N - 1) * N..],
                        &b.bands[band][..N],
                        "{t:?} south edge"
                    );
                }
                compared += 1;
            }
            compared
        })
        .sum()
}

#[test]
fn neighbouring_tiles_share_identical_edges() {
    let temp = layer("temp").unwrap();
    for raster in [latlon_raster(1400, 700), hrrr_like(1799, 1059)] {
        let grid = FrameGrid::build(temp, &raster);
        assert!(grid.levels.len() > 3);
        let mut compared = 0;
        for z in 2..=6 {
            compared += assert_shared_edges(&grid.covering(z), |t| one(&grid, t));
        }
        assert!(compared > 50, "only {compared} edges compared");
    }
}

#[test]
fn edges_match_where_the_level_changes() {
    // Find a zoom and a pair of vertically adjacent tiles whose rows use
    // different levels, then check the shared row.
    let temp = layer("temp").unwrap();
    let grid = FrameGrid::build(temp, &latlon_raster(1400, 700));
    let mut found = 0;
    for z in 1..=7u8 {
        for t in grid.covering(z) {
            let top = grid.level_for(t.sample_lonlat(0, 0).1, z);
            let bottom = grid.level_for(t.sample_lonlat(0, N - 1).1, z);
            if top == bottom {
                continue;
            }
            found += 1;
            let (Some(a), Some(b)) = (
                one(&grid, t),
                TileCoord::new(z, t.x, t.y + 1).and_then(|s| one(&grid, s)),
            ) else {
                continue;
            };
            assert_eq!(&a.bands[0][(N - 1) * N..], &b.bands[0][..N]);
        }
    }
    assert!(found > 0, "no tile straddles a level change");
}

#[test]
fn levels_follow_zoom() {
    let temp = layer("temp").unwrap();
    let grid = FrameGrid::build(temp, &hrrr_like(1799, 1059));
    // 3 km grid: zoom 6 cells are about 1.9 km at 38.5 N, zoom 0 cells about 120 km.
    assert_eq!(grid.level_for(38.5, 6), 0);
    assert_eq!(grid.level_for(38.5, 5), 0);
    assert_eq!(grid.level_for(38.5, 4), 1);
    assert_eq!(grid.level_for(38.5, 0), 5);
    let mut last = 0;
    for z in (0..=8).rev() {
        let l = grid.level_for(38.5, z);
        assert!(l >= last);
        last = l;
    }
}

/// Reading a smooth field through any level gives the value at the
/// point: the level's half-sample shift is accounted for.
#[test]
fn every_level_samples_the_right_place() {
    let temp: &LayerSpec = layer("temp").unwrap();
    let band: BandSpec = temp.bands[0];
    assert_eq!(
        (band.kind, band.aggregate, band.scale),
        (BandKind::Continuous, Aggregate::Mean, 0.01)
    );
    let (nx, ny) = (1024, 512);
    let proj = GridProjection::LatLon(LatLonGrid {
        lon0: -120.0,
        lat0: 20.0,
        dlon: 0.04,
        dlat: 0.04,
        periodic: false,
    });
    // Linear in i and j: bilinear interpolation and block means are exact.
    let f = |i: f64, j: f64| 0.02 * i + 0.04 * j;
    let grid = FrameGrid::build(
        temp,
        &raster(proj.clone(), nx, ny, |i, j| f(i as f64, j as f64) as f32),
    );
    assert!(grid.levels.len() >= 4);
    let mut out = [0.0];
    for level in 0..grid.levels.len() {
        for (lon, lat) in [
            (-110.3, 25.7),
            (-100.01, 30.33),
            (-90.5, 35.1),
            (-80.77, 37.9),
        ] {
            let (i, j) = proj.lonlat_to_ij(lon, lat);
            grid.sample_raw(i, j, level, &mut out);
            let got = f64::from(band.dequantize(to_sample(out[0])).unwrap());
            // Quantization and the rounding of means, a few steps at most.
            assert!(
                (got - f(i, j)).abs() <= 0.01 * (level + 1) as f64,
                "level {level} at ({lon}, {lat}): {got} vs {}",
                f(i, j)
            );
        }
    }
}

#[test]
fn outside_is_no_data_and_missing_is_skipped() {
    let temp = layer("temp").unwrap();
    let raster = latlon_raster(1400, 700);
    let grid = FrameGrid::build(temp, &raster);
    let p = &raster.proj;
    // West of the grid's first column (230.005 E is -129.995).
    assert!(!grid.inside(p.lonlat_to_ij(-130.2, 40.0).0, 0.0));
    assert_eq!(grid.coverage_at(-130.2, 40.0), 0.0);
    // Half a cell west of the first sample is the grid's edge, with no
    // weight; a little east of it there is a little.
    assert!(grid.inside(-0.5, 100.0) && grid.coverage.weight(-0.5, 100.0) == 0.0);
    let (i, j) = p.lonlat_to_ij(-130.0 + 1e-9, 40.0);
    let w = grid.coverage.weight(i, j);
    assert!(grid.inside(i, j) && w > 0.0 && w < 0.01, "{w}");
    // Middle of the hole: no value, no weight.
    let mut out = [0.0];
    grid.sample_raw(300.0, 200.0, 0, &mut out);
    assert!(out[0].is_nan());
    assert_eq!(grid.coverage.weight(300.0, 200.0), 0.0);
    // On the hole's rim, valid neighbours are used alone.
    grid.sample_raw(339.6, 200.0, 0, &mut out);
    let got = temp.bands[0].dequantize(to_sample(out[0])).unwrap();
    let want = raster.bands[0][200 * 1400 + 340];
    assert!((got - want).abs() < 0.02, "{got} vs {want}");
    // A tile far away is not rendered at all.
    assert!(one(&grid, TileCoord::new(4, 0, 0).unwrap()).is_none());
}

#[test]
fn periodic_grids_wrap() {
    let temp = layer("temp").unwrap();
    let (nx, ny) = (1440, 721);
    let grid = FrameGrid::build(
        temp,
        &raster(global(nx, ny), nx, ny, |i, _| i as f32 * 0.01),
    );
    let mut out = [0.0];
    // Between the last column (359.75 E, value 14.39) and the first (0 E, value 0).
    let (i, j) = grid.proj.lonlat_to_ij(-0.125, 10.0);
    grid.sample_raw(i, j, 0, &mut out);
    let v = temp.bands[0].dequantize(to_sample(out[0])).unwrap();
    assert!((v - 7.195).abs() < 0.011, "{v}");
    // Zoom 0 covers the world and every level wraps.
    assert!(one(&grid, TileCoord::new(0, 0, 0).unwrap()).is_some());
    for level in 0..grid.levels.len() {
        let (i, j) = grid.proj.lonlat_to_ij(179.99, 0.0);
        grid.sample_raw(i, j, level, &mut out);
        assert!(!out[0].is_nan());
    }
    // A global grid has no east or west edge, and its pole rows are no edge.
    for (lon, lat) in [
        (0.0, 0.0),
        (-0.1, 10.0),
        (179.99, -20.0),
        (-180.0, 80.0),
        (10.0, 89.99),
        (10.0, -89.99),
    ] {
        assert_eq!(grid.coverage_at(lon, lat), 1.0, "({lon}, {lat})");
    }
    let solid = grid.solid().expect("a complete global grid is solid");
    assert_eq!((solid.west, solid.east), (-180.0, 180.0));
    assert_eq!((solid.south, solid.north), (-90.0, 90.0), "{solid:?}");
}

/// Tiles either side of the antimeridian agree where they meet, for a
/// global grid and for a part that crosses it, which renders on both sides.
#[test]
fn antimeridian() {
    let temp = layer("temp").unwrap();
    let (nx, ny) = (720, 361);
    let world = FrameGrid::build(
        temp,
        &raster(global(nx, ny), nx, ny, |i, j| {
            (i as f32 * 0.07).sin() * 9.0 + j as f32 * 0.05
        }),
    );
    // 170 E to 160 W at 0.02 degrees, with a hole on the antimeridian.
    let pacific = FrameGrid::build(
        temp,
        &raster(latlon(170.01, 65.0, 0.02), 1500, 1000, |i, j| {
            if (i as f32 - 500.0).hypot(j as f32 - 500.0) < 30.0 {
                f32::NAN
            } else {
                20.0 + i as f32 * 0.001
            }
        }),
    );
    assert!(pacific.bounds().wraps(), "{:?}", pacific.bounds());
    // RDPS's rotated grid at a quarter of its resolution: over the pole and
    // across the antimeridian.
    let rdps = FrameGrid::build(
        temp,
        &raster(
            GridProjection::RotatedLatLon(RotatedLatLonGrid::new(
                -31.758312,
                267.597031,
                LatLonGrid {
                    lon0: 306.141412,
                    lat0: -48.805954,
                    dlon: 0.361192,
                    dlat: 0.361192,
                    periodic: false,
                },
            )),
            285,
            262,
            |i, j| (i as f32 * 0.1).sin() * 5.0 + j as f32 * 0.03,
        ),
    );
    for z in 1..=6u8 {
        let last = (1u32 << z) - 1;
        for y in 0..(1u32 << z) {
            let (w, e) = (
                TileCoord::new(z, 0, y).unwrap(),
                TileCoord::new(z, last, y).unwrap(),
            );
            for parts in [
                vec![&world],
                vec![&pacific, &world],
                vec![&pacific],
                vec![&rdps, &world],
            ] {
                let (a, b) = (render_stack(&parts, e), render_stack(&parts, w));
                assert_eq!(a.is_some(), b.is_some(), "z{z} y{y}");
                if let (Some(a), Some(b)) = (a, b) {
                    for r in 0..N {
                        assert_eq!(
                            a.bands[0][r * N + N - 1],
                            b.bands[0][r * N],
                            "z{z} y{y} row {r}"
                        );
                    }
                }
            }
        }
        // The crossing part has data on both sides.
        let ty = (lat_to_y(60.0) * f64::from(1u32 << z)) as u32;
        for part in [&pacific, &rdps] {
            let has = |x: u32| render_stack(&[part], TileCoord::new(z, x, ty).unwrap()).is_some();
            assert!(has(0) && has(last), "z{z}");
        }
    }
}

/// The east edge of the last tile in a row and the west edge of the first
/// are one meridian; their per-column projection terms must agree to the
/// bit, which 180 and -180 do not for every projection.
#[test]
fn antimeridian_samples_see_one_longitude() {
    let rotated = GridProjection::RotatedLatLon(RotatedLatLonGrid::new(
        -31.758312,
        267.597031,
        LatLonGrid {
            lon0: 306.141412,
            lat0: -48.805954,
            dlon: 0.090298,
            dlat: 0.090298,
            periodic: false,
        },
    ));
    let z = 5;
    let (east, west) = (
        TileCoord::new(z, (1 << z) - 1, 7).unwrap(),
        TileCoord::new(z, 0, 7).unwrap(),
    );
    let (e, w) = (
        east.sample_lonlat(TILE_CELLS, 0).0,
        west.sample_lonlat(0, 0).0,
    );
    assert_eq!((e, w), (180.0, -180.0));
    assert_eq!(canonical_lon(e).to_bits(), canonical_lon(w).to_bits());
    let bits = |t: Terms| t.map(f64::to_bits);
    let mut differ = false;
    for p in [
        rotated,
        hrrr_proj(),
        global(1440, 721),
        latlon(170.01, 65.0, 0.02),
    ] {
        assert_eq!(
            bits(p.lon_terms(canonical_lon(e))),
            bits(p.lon_terms(canonical_lon(w)))
        );
        differ |= bits(p.lon_terms(e)) != bits(p.lon_terms(w));
    }
    assert!(
        differ,
        "taken as they come, 180 and -180 give different terms somewhere"
    );
}

#[test]
fn categorical_bands_take_the_nearest_sample() {
    let precip = layer("precip").unwrap();
    let (nx, ny) = (200, 100);
    let proj = GridProjection::LatLon(LatLonGrid {
        lon0: -100.0,
        lat0: 30.0,
        dlon: 0.1,
        dlat: 0.1,
        periodic: false,
    });
    let r = Raster {
        nx,
        ny,
        proj: proj.clone(),
        bands: vec![
            vec![1.0; nx * ny],
            (0..nx * ny)
                .map(|k| if k % nx < 100 { 1.0 } else { 2.0 })
                .collect(),
        ],
    };
    let grid = FrameGrid::build(precip, &r);
    let mut out = [0.0; 2];
    // Column 99 is rain, column 100 snow; 99.4 rounds to rain, 99.6 to snow.
    for (lon, want) in [(-100.0 + 9.94, 1.0), (-100.0 + 9.96, 2.0)] {
        let (i, j) = proj.lonlat_to_ij(lon, 35.0);
        grid.sample_raw(i, j, 0, &mut out);
        assert_eq!(precip.bands[1].dequantize(out[1] as u16), Some(want));
    }
    assert!(matches!(
        grid.bands[1],
        BandMeta {
            kind: BandKind::Categorical,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Coverage weights
// ---------------------------------------------------------------------------

/// A 1 km-like radar grid: ragged blobs of no coverage and a missing corner.
fn ragged(nx: usize, ny: usize, seed: u64) -> Raster {
    let mut rng = Rng(seed);
    let blobs: Vec<(f64, f64, f64)> = (0..40)
        .map(|_| {
            (
                rng.next() * nx as f64,
                rng.next() * ny as f64,
                3.0 + rng.next() * 40.0,
            )
        })
        .collect();
    raster(latlon(-105.005, 44.995, 0.01), nx, ny, move |i, j| {
        let (x, y) = (i as f64, j as f64);
        let corner = x + y < 120.0;
        let blob = blobs.iter().any(|&(bx, by, r)| {
            // Ragged rims: the radius wobbles with the angle.
            let a = (y - by).atan2(x - bx);
            (x - bx).hypot(y - by) < r * (1.0 + 0.3 * (5.0 * a).sin())
        });
        if corner || blob {
            f32::NAN
        } else {
            25.0 + (x * 0.01).sin() as f32
        }
    })
}

#[test]
fn weights_follow_ragged_coverage() {
    let temp = layer("temp").unwrap();
    let r = ragged(900, 700, 7);
    let g = FrameGrid::build(temp, &r);
    let (nx, ny) = (r.nx, r.ny);
    let ramp = crate::coverage::ramp_cells(&r.proj);
    let block = (ramp / 3.0).floor();
    // Brute-force distance from each sample to missing data or the edge.
    let missing: Vec<(f64, f64)> = (0..nx * ny)
        .filter(|&k| r.bands[0][k].is_nan())
        .map(|k| ((k % nx) as f64, (k / nx) as f64))
        .collect();
    let dist = |i: f64, j: f64| {
        let edge = (i + 0.5)
            .min(nx as f64 - 0.5 - i)
            .min(j + 0.5)
            .min(ny as f64 - 0.5 - j);
        missing
            .iter()
            .map(|&(a, b)| (a - i).hypot(b - j))
            .fold(edge, f64::min)
    };
    let mut rng = Rng(11);
    let (mut zero, mut full) = (0, 0);
    for _ in 0..4000 {
        let (i, j) = (
            (rng.next() * nx as f64).floor(),
            (rng.next() * ny as f64).floor(),
        );
        let w = g.coverage.weight(i, j);
        assert!((0.0..=1.0).contains(&w));
        if r.bands[0][j as usize * nx + i as usize].is_nan() {
            assert_eq!(w, 0.0, "missing sample ({i}, {j}) has weight {w}");
            zero += 1;
            continue;
        }
        let d = dist(i, j);
        // Full weight once every corner of the point's block is a ramp away
        // from every corner of the block holding the missing value.
        if d > ramp + 3.0 * block {
            assert_eq!(w, 1.0, "({i}, {j}) is {d} cells in with weight {w}");
            full += 1;
        }
        // And nothing is at full weight while a corner of its block is within
        // the ramp of a corner of that block.
        if d < ramp - 1.5 * block - 0.5 {
            assert!(w < 1.0, "({i}, {j}) is {d} cells in with weight {w}");
        }
    }
    assert!(zero > 100 && full > 500, "{zero} {full}");
    // Continuous: small steps change the weight a little, never a jump.
    for _ in 0..400 {
        let (mut i, j) = (
            rng.next() * (nx as f64 - 30.0),
            rng.next() * (ny as f64 - 1.0),
        );
        let mut w = g.coverage.weight(i, j);
        for _ in 0..200 {
            i += 0.1;
            let next = g.coverage.weight(i, j);
            assert!((next - w).abs() < 0.04, "jump at ({i}, {j}): {w} -> {next}");
            w = next;
        }
    }
    // The same weights after a round trip through `.wxg`.
    let back = FrameGrid::from_bytes(&g.to_bytes()).unwrap();
    assert_eq!(back.coverage, g.coverage);
}

#[test]
fn weights_rise_monotonically_from_an_edge() {
    let temp = layer("temp").unwrap();
    // 3 km Lambert: the ramp is 12 cells; 1 km lat/lon: 15 km of cells.
    for (r, ramp) in [
        (hrrr_like(400, 300), 12.0),
        (
            ragged(400, 300, 0),
            15_000.0 / (0.01f64.to_radians() * EARTH_RADIUS_NCEP),
        ),
    ] {
        let g = FrameGrid::build(temp, &raster(r.proj.clone(), r.nx, r.ny, |_, _| 1.0));
        let j = 150.0;
        let mut last = 0.0;
        for k in 0..400 {
            let i = -0.5 + k as f64 * 0.1;
            let w = g.coverage.weight(i, j);
            assert!(w >= last, "falls at {i}: {last} -> {w}");
            last = w;
        }
        assert_eq!(g.coverage.weight(-0.5, j), 0.0);
        let block = (ramp / 3.0).floor();
        assert_eq!(
            g.coverage.weight(ramp + block, j),
            1.0,
            "full {ramp} cells in"
        );
        assert!(g.coverage.weight(ramp * 0.5, j) < 0.9);
    }
}

// ---------------------------------------------------------------------------
// The solid box
// ---------------------------------------------------------------------------

fn assert_solid_conservative(g: &FrameGrid, samples: usize) -> Option<LonLatBox> {
    let solid = g.solid()?;
    let (_, published) = publish_boxes(g.bounds(), Some(solid));
    let mut rng = Rng(3);
    for b in [Some(solid), published].into_iter().flatten() {
        let width = if b.wraps() {
            b.east - b.west + 360.0
        } else {
            b.east - b.west
        };
        for _ in 0..samples {
            let lon = wx_core::proj::wrap_lon(b.west + rng.next() * width);
            let lat = b.south + rng.next() * (b.north - b.south);
            assert_eq!(g.coverage_at(lon, lat), 1.0, "({lon}, {lat}) in {b:?}");
        }
        // Its edges and corners too.
        for k in 0..=100 {
            let t = k as f64 / 100.0;
            for (lon, lat) in [
                (b.west + t * width, b.south),
                (b.west + t * width, b.north),
                (b.west, b.south + t * (b.north - b.south)),
                (b.west + width, b.south + t * (b.north - b.south)),
            ] {
                let lon = wx_core::proj::wrap_lon(lon);
                assert_eq!(g.coverage_at(lon, lat), 1.0, "edge ({lon}, {lat}) of {b:?}");
            }
        }
    }
    Some(solid)
}

#[test]
fn solid_box_is_conservative() {
    let temp = layer("temp").unwrap();
    let area = |b: &LonLatBox| {
        let w = if b.wraps() {
            b.east - b.west + 360.0
        } else {
            b.east - b.west
        };
        w * (b.north - b.south)
    };
    // A complete Lambert grid: most of it is solid.
    let hrrr = FrameGrid::build(temp, &hrrr_like(900, 600));
    let s = assert_solid_conservative(&hrrr, 5000).expect("solid");
    assert!(
        area(&s) > 0.4 * area(&hrrr.bounds()),
        "{s:?} in {:?}",
        hrrr.bounds()
    );
    // Ragged radar coverage.
    let radar = FrameGrid::build(temp, &ragged(900, 700, 7));
    assert_solid_conservative(&radar, 5000);
    // Alaska's polar stereographic grid across the antimeridian, with a gap.
    let ak = GridProjection::PolarStereographic(PolarStereoGrid::from_first_point(
        Earth::sphere(6_371_229.0),
        false,
        60.0,
        225.0,
        41.612949,
        185.117126,
        6000.0,
        6000.0,
    ));
    let alaska = FrameGrid::build(
        temp,
        &raster(ak, 650, 460, |i, j| {
            if (i as f32 - 300.0).hypot(j as f32 - 200.0) < 20.0 {
                f32::NAN
            } else {
                1.0
            }
        }),
    );
    let s = assert_solid_conservative(&alaska, 5000).expect("solid");
    assert!(alaska.bounds().wraps());
    println!("alaska solid {s:?} in {:?}", alaska.bounds());
    // A global grid with a hole.
    let (nx, ny) = (720, 361);
    let world = FrameGrid::build(
        temp,
        &raster(global(nx, ny), nx, ny, |i, j| {
            if (i as f32 - 20.0).hypot(j as f32 - 100.0) < 10.0 {
                f32::NAN
            } else {
                1.0
            }
        }),
    );
    let s = assert_solid_conservative(&world, 5000).expect("solid");
    println!("world solid {s:?}");
    // Speckled coverage has no box worth having.
    let speckled = FrameGrid::build(
        temp,
        &raster(latlon(-100.0, 40.0, 0.01), 400, 400, |i, j| {
            if (i * 7 + j * 13) % 37 == 0 {
                f32::NAN
            } else {
                1.0
            }
        }),
    );
    assert_eq!(speckled.solid(), None);
}

// ---------------------------------------------------------------------------
// Stacks
// ---------------------------------------------------------------------------

fn constant(proj: GridProjection, nx: usize, ny: usize, v: f32) -> FrameGrid {
    FrameGrid::build(layer("temp").unwrap(), &raster(proj, nx, ny, move |_, _| v))
}

/// The stored sample of the tile lattice point nearest `(lon, lat)`.
fn sample_at(parts: &[&FrameGrid], z: u8, lon: f64, lat: f64) -> Option<f32> {
    let n = f64::from(1u32 << z);
    let (x, y) = (lon_to_x(lon) * n, lat_to_y(lat) * n);
    let t = TileCoord::new(z, x.floor() as u32, y.floor() as u32).unwrap();
    let tile = render_stack(parts, t)?;
    let c = ((x - x.floor()) * TILE_CELLS as f64).round() as usize;
    let r = ((y - y.floor()) * TILE_CELLS as f64).round() as usize;
    layer("temp").unwrap().bands[0].dequantize(tile.bands[0][r * N + c])
}

/// The lon/lat of that lattice point.
fn lattice_point(z: u8, lon: f64, lat: f64) -> (f64, f64) {
    let n = f64::from(1u32 << z);
    let (x, y) = (lon_to_x(lon) * n, lat_to_y(lat) * n);
    let t = TileCoord::new(z, x.floor() as u32, y.floor() as u32).unwrap();
    let c = ((x - x.floor()) * TILE_CELLS as f64).round() as usize;
    let r = ((y - y.floor()) * TILE_CELLS as f64).round() as usize;
    t.sample_lonlat(c, r)
}

#[test]
fn stacks_blend_and_normalise() {
    // 10 degrees on a 0.05 degree regional grid over 20 on a global one.
    let top = constant(latlon(-110.0, 45.0, 0.05), 400, 300, 10.0);
    let bottom = constant(global(1440, 721), 1440, 721, 20.0);
    let z = 7;
    let mut blended = 0;
    for k in 0..200 {
        // West to east across the regional grid's west edge.
        let (lon, lat) = lattice_point(z, -110.5 + k as f64 * 0.01, 40.0);
        let a = top.coverage_at(lon, lat);
        let got = f64::from(sample_at(&[&top, &bottom], z, lon, lat).unwrap());
        let want = a * 10.0 + (1.0 - a) * 20.0;
        assert!(
            (got - want).abs() <= 0.0051,
            "({lon}, {lat}): a {a}, {got} vs {want}"
        );
        if a > 0.05 && a < 0.95 {
            blended += 1;
        }
        // The top part alone: wherever it has weight it shows its value, not
        // a faded one.
        match sample_at(&[&top], z, lon, lat) {
            Some(v) => assert!(
                a > 0.0 && (v - 10.0).abs() < 1e-4,
                "({lon}, {lat}): {v} at weight {a}"
            ),
            None => assert_eq!(a, 0.0),
        }
    }
    assert!(blended > 10, "{blended} samples in the seam");
    // Two parts whose edges coincide: normalised, the blend stays between
    // their values up to the edge instead of fading to nothing.
    let lower = constant(latlon(-110.0, 45.0, 0.05), 400, 300, 30.0);
    for k in 0..60 {
        let (lon, lat) = lattice_point(z, -110.1 + k as f64 * 0.01, 40.0);
        if let Some(v) = sample_at(&[&top, &lower], z, lon, lat) {
            assert!((9.995..=30.005).contains(&v), "({lon}, {lat}): {v}");
        }
    }
}

/// A regional grid of `nx` columns whose last weight block is partial (so
/// its weight is 0 although it holds data), with a hole whose rim blocks
/// hold data at weight 0 too.
fn trimmed(nx: usize, v: f32) -> Raster {
    raster(latlon(-110.0, 45.0, 0.05), nx, 90, move |i, j| {
        if (i as f32 - 40.0).hypot(j as f32 - 40.0) < 6.0 {
            f32::NAN
        } else {
            v + 0.01 * i as f32
        }
    })
}

/// Where no part has weight, a lone part still shows every value it has:
/// each tile sample is exactly the part's own sample wherever the part has
/// one, and no data elsewhere.
#[test]
fn zero_weight_data_is_shown_where_nothing_else_is() {
    let temp = layer("temp").unwrap();
    let r = trimmed(122, 10.0);
    // 0.05 degree cells: 12-cell ramp, blocks of 4, so columns 120 and 121
    // form a partial block.
    assert_eq!(crate::coverage::ramp_cells(&r.proj), 12.0);
    let g = FrameGrid::build(temp, &r);
    let z = 8;
    let (mut zero_weight, mut weighted, mut empty) = (0, 0, 0);
    for t in g.covering(z) {
        let tile = one(&g, t);
        let mut out = [0.0];
        for row in 0..N {
            let lat = t.sample_lonlat(0, row).1;
            let level = g.level_for(lat, z);
            for col in 0..N {
                let lon = canonical_lon(t.sample_lonlat(col, 0).0);
                let (i, j) = g.proj.lonlat_to_ij(lon, lat);
                let want = if g.inside(i, j) && g.bounds.contains(lon, lat) {
                    g.sample_raw(i, j, level, &mut out);
                    if out[0].is_nan() {
                        NODATA
                    } else {
                        if g.coverage.weight(i, j) == 0.0 {
                            zero_weight += 1;
                        } else {
                            weighted += 1;
                        }
                        to_sample(out[0])
                    }
                } else {
                    NODATA
                };
                empty += usize::from(want == NODATA);
                let got = tile.as_ref().map_or(NODATA, |t| t.bands[0][row * N + col]);
                assert_eq!(
                    got, want,
                    "{t:?} ({col}, {row}) at ({lon}, {lat}), index ({i}, {j})"
                );
            }
        }
    }
    assert!(
        zero_weight > 2000 && weighted > 100_000 && empty > 10_000,
        "{zero_weight} {weighted} {empty}"
    );
}

/// With several parts and no weight anywhere, the topmost part with a value
/// wins; where a part below has weight, that part shows instead, as before.
#[test]
fn zero_weight_fallback_takes_the_topmost_value() {
    let temp = layer("temp").unwrap();
    let top = FrameGrid::build(temp, &trimmed(122, 10.0));
    // The same footprint: the zero-weight blocks coincide.
    let twin = FrameGrid::build(temp, &trimmed(122, 20.0));
    // Wider, without the hole: weight 1 under the top part's rim.
    let wide = constant(latlon(-110.0, 45.0, 0.05), 200, 90, 30.0);
    let z = 9;
    // The partial block at column 120.6, and the hole's rim at column 40,
    // row 33.4 (inside a block that also holds missing values).
    for (i, j) in [(120.6, 40.0), (40.0, 33.4)] {
        let (lon, lat) = lattice_point(z, -110.0 + i * 0.05, 45.0 - j * 0.05);
        let (pi, pj) = top.proj.lonlat_to_ij(lon, lat);
        assert_eq!(top.coverage.weight(pi, pj), 0.0, "({i}, {j})");
        let mut out = [0.0];
        top.sample_raw(pi, pj, 0, &mut out);
        let own = temp.bands[0].dequantize(to_sample(out[0])).unwrap();
        assert!((own - (10.0 + 0.01 * i as f32)).abs() < 0.02, "{own}");
        let at = |parts: &[&FrameGrid]| sample_at(parts, z, lon, lat);
        assert_eq!(at(&[&top]), Some(own), "alone");
        assert_eq!(at(&[&top, &twin]), Some(own), "topmost of two at weight 0");
        assert_eq!(
            at(&[&twin, &top]).map(|v| (v - own - 10.0).abs() < 1e-3),
            Some(true),
            "the other way up"
        );
        assert_eq!(at(&[&top, &wide]), Some(30.0), "the part below has weight");
        assert_eq!(at(&[&top, &twin, &wide]), Some(30.0));
    }
    // In the middle of the hole nobody but the wide part has a value.
    let (lon, lat) = lattice_point(z, -110.0 + 40.0 * 0.05, 45.0 - 40.0 * 0.05);
    assert_eq!(sample_at(&[&top, &twin], z, lon, lat), None);
    assert_eq!(sample_at(&[&top, &twin, &wide], z, lon, lat), Some(30.0));

    // Categorical bands follow the same rule.
    let precip = layer("precip").unwrap();
    let typed = |nx: usize, code: f32| {
        let r = trimmed(nx, 0.0);
        let rate = r.bands[0].iter().map(|v| v.min(2.0)).collect();
        let kind = r.bands[0]
            .iter()
            .map(|v| if v.is_nan() { f32::NAN } else { code })
            .collect();
        FrameGrid::build(
            precip,
            &Raster {
                bands: vec![rate, kind],
                ..r
            },
        )
    };
    let (rain, snow, wide_snow) = (typed(122, 1.0), typed(122, 2.0), typed(200, 2.0));
    let (lon, lat) = lattice_point(z, -110.0 + 120.6 * 0.05, 45.0 - 40.0 * 0.05);
    let code = |parts: &[&FrameGrid]| {
        let n = f64::from(1u32 << z);
        let (x, y) = (lon_to_x(lon) * n, lat_to_y(lat) * n);
        let t = TileCoord::new(z, x.floor() as u32, y.floor() as u32).unwrap();
        let tile = render_stack(parts, t)?;
        let c = ((x - x.floor()) * TILE_CELLS as f64).round() as usize;
        let r = ((y - y.floor()) * TILE_CELLS as f64).round() as usize;
        precip.bands[1].dequantize(tile.bands[1][r * N + c])
    };
    assert_eq!(code(&[&rain]), Some(1.0));
    assert_eq!(code(&[&rain, &snow]), Some(1.0));
    assert_eq!(code(&[&snow, &rain]), Some(2.0));
    assert_eq!(code(&[&rain, &wide_snow]), Some(2.0));
}

/// Shared edges stay bit-identical where the fallback decides the sample:
/// stacks with no complete part underneath, from every stack rule.
#[test]
fn shared_edges_where_nothing_has_weight() {
    let temp = layer("temp").unwrap();
    let radar = FrameGrid::build(temp, &ragged(700, 500, 9));
    let regional = FrameGrid::build(
        temp,
        &raster(latlon(-107.0, 46.0, 0.05), 123, 141, |i, j| {
            if (i as f32 - 60.0).hypot(j as f32 - 90.0) < 9.0 {
                f32::NAN
            } else {
                (i as f32 * 0.05).sin() * 8.0 + j as f32 * 0.01
            }
        }),
    );
    let m = Mosaic::new(vec![radar, regional]);
    let mut compared = 0;
    for z in 5..=8u8 {
        let tiles = m.grids[1].covering(z);
        for pick in [tile_parts, tile_parts_must, tile_parts_may] {
            compared +=
                assert_shared_edges(&tiles, |t| render_stack(&m.stack(pick(&m.parts, t)), t));
        }
        compared += assert_shared_edges(&tiles, |t| {
            let pick = if (t.x + t.y) % 2 == 0 {
                tile_parts_must
            } else {
                tile_parts_may
            };
            render_stack(&m.stack(pick(&m.parts, t)), t)
        });
    }
    assert!(compared > 100, "{compared}");
}

#[test]
fn categorical_bands_follow_the_heavier_part() {
    let precip = layer("precip").unwrap();
    let typed = |proj: GridProjection, nx: usize, ny: usize, code: f32| {
        FrameGrid::build(
            precip,
            &Raster {
                nx,
                ny,
                proj,
                bands: vec![vec![2.0; nx * ny], vec![code; nx * ny]],
            },
        )
    };
    let rain = typed(latlon(-110.0, 45.0, 0.05), 400, 300, 1.0);
    let snow = typed(global(1440, 721), 1440, 721, 2.0);
    let z = 7;
    let (mut saw_rain, mut saw_snow) = (false, false);
    for k in 0..200 {
        let (lon, lat) = lattice_point(z, -110.5 + k as f64 * 0.01, 40.0);
        let n = f64::from(1u32 << z);
        let (x, y) = (lon_to_x(lon) * n, lat_to_y(lat) * n);
        let t = TileCoord::new(z, x.floor() as u32, y.floor() as u32).unwrap();
        let tile = render_stack(&[&rain, &snow], t).unwrap();
        let (c, r) = (
            ((x - x.floor()) * 256.0).round() as usize,
            ((y - y.floor()) * 256.0).round() as usize,
        );
        let code = precip.bands[1]
            .dequantize(tile.bands[1][r * N + c])
            .unwrap();
        let a = rain.coverage_at(lon, lat);
        // Rain weighs a, snow (1 - a).
        let want = if a > 1.0 - a { 1.0 } else { 2.0 };
        if a != 0.5 {
            assert_eq!(code, want, "({lon}, {lat}) at weight {a}");
        }
        saw_rain |= code == 1.0;
        saw_snow |= code == 2.0;
        // The rate blends.
        let rate = precip.bands[0]
            .dequantize(tile.bands[0][r * N + c])
            .unwrap();
        assert!((rate - 2.0).abs() < 0.006);
    }
    assert!(saw_rain && saw_snow);
}

#[test]
fn parts_with_another_quantization_are_converted() {
    let temp = layer("temp").unwrap();
    let mut coarse = *temp;
    let bands = [BandSpec {
        scale: 0.1,
        offset: -50.0,
        ..temp.bands[0]
    }];
    coarse.bands = Box::leak(Box::new(bands));
    let a = FrameGrid::build(
        temp,
        &raster(latlon(-110.0, 45.0, 0.05), 400, 300, |_, _| 12.34),
    );
    let b = FrameGrid::build(&coarse, &raster(global(720, 361), 720, 361, |_, _| 15.0));
    let v = sample_at(&[&a, &b], 6, -60.0, 10.0).unwrap();
    assert!((v - 15.0).abs() < 0.006, "{v}");
    let v = sample_at(&[&a, &b], 6, -100.0, 40.0).unwrap();
    assert!((v - 12.34).abs() < 0.006, "{v}");
}

#[test]
#[should_panic(expected = "same bands")]
fn parts_of_different_layers_are_refused() {
    let temp = layer("temp").unwrap();
    let wind = layer("wind").unwrap();
    let a = FrameGrid::build(
        temp,
        &raster(latlon(-110.0, 45.0, 0.05), 40, 30, |_, _| 1.0),
    );
    let r = raster(latlon(-110.0, 45.0, 0.05), 40, 30, |_, _| 1.0);
    let b = FrameGrid::build(
        wind,
        &Raster {
            bands: vec![r.bands[0].clone(); 3],
            ..r
        },
    );
    let _ = render_stack(&[&a, &b], TileCoord::new(3, 1, 2).unwrap());
}

/// Parts as the catalog would publish them, and the stacks `tile_parts`
/// and its tolerant variants pick from them.
struct Mosaic {
    grids: Vec<FrameGrid>,
    parts: Vec<Part>,
}

impl Mosaic {
    fn new(grids: Vec<FrameGrid>) -> Mosaic {
        let valid =
            chrono::DateTime::from_timestamp(1_790_000_000 - 1_790_000_000 % 60, 0).unwrap();
        let parts = grids
            .iter()
            .enumerate()
            .map(|(k, g)| {
                let (bounds, solid) = publish_boxes(g.bounds(), g.solid());
                Part::observed(&format!("p{k}"), valid, bounds, solid, 7)
            })
            .collect();
        Mosaic { grids, parts }
    }

    fn stack(&self, chosen: Vec<&Part>) -> Vec<&FrameGrid> {
        chosen
            .iter()
            .map(|p| &self.grids[self.parts.iter().position(|q| q.id == p.id).unwrap()])
            .collect()
    }
}

/// The renderer's rule meets the catalog's: tiles rendered from the stacks
/// `tile_parts` names (and from any stack the server would accept) agree on
/// every shared edge, across blend seams and solid-box cut-offs.
#[test]
fn shared_edges_across_blend_seams() {
    let temp = layer("temp").unwrap();
    let radar = FrameGrid::build(temp, &ragged(1600, 1200, 5));
    let hrrr = FrameGrid::build(temp, &hrrr_like(1799, 1059));
    let (nx, ny) = (1440, 721);
    let world = FrameGrid::build(
        temp,
        &raster(global(nx, ny), nx, ny, |i, j| {
            (i as f32 * 0.03).cos() * 15.0 + j as f32 * 0.02
        }),
    );
    let m = Mosaic::new(vec![radar, hrrr, world]);
    assert!(m.parts[1].solid.is_some() && m.parts[2].solid.is_some());
    let mut compared = 0;
    for z in 3..=6u8 {
        let tiles = m.grids[1].covering(z);
        for pick in [tile_parts, tile_parts_must, tile_parts_may] {
            compared +=
                assert_shared_edges(&tiles, |t| render_stack(&m.stack(pick(&m.parts, t)), t));
        }
        // A tile from one rule next to its neighbour from another.
        compared += assert_shared_edges(&tiles, |t| {
            let pick = if (t.x + t.y) % 2 == 0 {
                tile_parts_must
            } else {
                tile_parts_may
            };
            render_stack(&m.stack(pick(&m.parts, t)), t)
        });
    }
    assert!(compared > 300, "{compared}");
    // Where the stacks differ, the results still do not: a tile inside the
    // Lambert part's solid box, from the full stack and from the cut one.
    let s = m.parts[1].solid.unwrap();
    let z = 6;
    let inside: Vec<TileCoord> = m.grids[1]
        .covering(z)
        .into_iter()
        .filter(|t| s.contains_box(&t.bounds()))
        .collect();
    assert!(!inside.is_empty());
    for t in inside.into_iter().take(6) {
        let cut = m.stack(tile_parts(&m.parts, t));
        assert!(cut.len() < 3, "{t:?} keeps {} parts", cut.len());
        let full: Vec<&FrameGrid> = m.grids.iter().collect();
        assert_eq!(
            render_stack(&cut, t).unwrap(),
            render_stack(&full, t).unwrap(),
            "{t:?}"
        );
    }
}

#[test]
fn single_parts_are_unchanged_by_parts_elsewhere() {
    let temp = layer("temp").unwrap();
    let g = FrameGrid::build(temp, &latlon_raster(1400, 700));
    let far = constant(latlon(10.0, 50.0, 0.05), 200, 200, 3.0);
    for t in g.covering(5) {
        assert_eq!(
            render_stack(&[&g], t),
            render_stack(&[&g, &far], t),
            "{t:?}"
        );
        assert_eq!(
            render_stack(&[&g], t),
            render_stack(&[&far, &g], t),
            "{t:?}"
        );
    }
}

/// Each projection, end to end: a grid holding each sample's own latitude
/// renders tiles whose samples hold theirs.
#[test]
fn every_projection_renders_in_the_right_place() {
    let temp = layer("temp").unwrap();
    // Name, projection, size, and a point inside.
    type Case = (&'static str, GridProjection, usize, usize, (f64, f64));
    let grids: Vec<Case> = vec![
        ("lambert", hrrr_proj(), 1799, 1059, (-97.0, 38.0)),
        (
            "polar",
            GridProjection::PolarStereographic(PolarStereoGrid::from_first_point(
                Earth::sphere(6_371_229.0),
                false,
                60.0,
                225.0,
                41.612949,
                185.117126,
                3000.0,
                3000.0,
            )),
            1299,
            919,
            (-150.0, 62.0),
        ),
        (
            "mercator",
            GridProjection::Mercator(MercatorGrid::from_first_point(
                Earth::sphere(6_371_200.0),
                20.0,
                202.3,
                18.072699,
                198.474999,
                2500.0,
                2500.0,
            )),
            321,
            225,
            (-157.5, 20.5),
        ),
        (
            "rotated",
            GridProjection::RotatedLatLon(RotatedLatLonGrid::new(
                -36.08852,
                245.305142,
                LatLonGrid {
                    lon0: 345.17878,
                    lat0: -12.302501,
                    dlon: 0.0225 * 4.0,
                    dlat: 0.0225 * 4.0,
                    periodic: false,
                },
            )),
            635,
            323,
            (-95.0, 52.0),
        ),
        (
            "laea",
            GridProjection::LambertAzimuthal(LaeaGrid::from_projected(
                Earth::WGS84,
                55.0,
                10.0,
                -1_949_000.0,
                2_099_000.0,
                2000.0,
                -2000.0,
            )),
            1900,
            2200,
            (12.0, 50.0),
        ),
        (
            "tmerc",
            GridProjection::TransverseMercator(TransverseMercatorGrid::from_projected(
                Earth::WGS84,
                42.0,
                12.5,
                1.0,
                -599_500.0,
                649_500.0,
                1000.0,
                -1000.0,
            )),
            1200,
            1400,
            (12.0, 42.0),
        ),
        (
            "geos",
            GridProjection::Geostationary(GeostationaryGrid::new(
                Earth {
                    a: 6_378_137.0,
                    b: 6_356_752.314_14,
                },
                35_786_023.0,
                -75.0,
                Sweep::X,
                5.6e-05 * 4.0,
                -0.151_844,
                -5.6e-05 * 4.0,
                0.151_844,
            )),
            1356,
            1356,
            (-80.0, 30.0),
        ),
    ];
    for (name, proj, nx, ny, (lon, lat)) in grids {
        let r = raster(proj.clone(), nx, ny, |i, j| {
            proj.ij_to_lonlat(i as f64, j as f64).1 as f32
        });
        let g = FrameGrid::build(temp, &r);
        for z in [3u8, 6] {
            let (plon, plat) = lattice_point(z, lon, lat);
            let v =
                sample_at(&[&g], z, lon, lat).unwrap_or_else(|| panic!("{name}: no data at z{z}"));
            // Within the size of the source cells read at this zoom.
            let cell = proj.nominal_spacing_m() / 111_000.0 * (1 << g.level_for(plat, z)) as f64;
            assert!(
                (f64::from(v) - plat).abs() < 1.5 * cell + 0.01,
                "{name} z{z} at ({plon}, {plat}): {v}"
            );
        }
        assert!(
            g.solid().is_some(),
            "{name}: a complete grid has a solid box"
        );
        assert_solid_conservative(&g, 2000);
    }
}

// ---------------------------------------------------------------------------
// Rate and type
// ---------------------------------------------------------------------------

/// A precipitation part (rate mm/h, type code) from two functions of the
/// sample.
fn precip_part(
    proj: GridProjection,
    nx: usize,
    ny: usize,
    rate: impl Fn(usize, usize) -> f32,
    kind: impl Fn(usize, usize) -> f32,
) -> FrameGrid {
    let r = raster(proj, nx, ny, rate);
    let k = raster(r.proj.clone(), nx, ny, kind);
    FrameGrid::build(
        layer("precip").unwrap(),
        &Raster {
            bands: vec![r.bands[0].clone(), k.bands[0].clone()],
            ..r
        },
    )
}

/// Every sample of `tile` obeys the precipitation-type rules' own
/// invariant: NONE below the typing threshold, and (in fields where every
/// sample with a rate has a type) a type from it up. Returns how many
/// samples have a rate at or above it.
fn assert_rate_and_type_agree(tile: &Tile, every_rate_typed: bool) -> usize {
    let precip = layer("precip").unwrap();
    let (rate_band, kind_band) = (&precip.bands[0], &precip.bands[1]);
    let threshold = rate_band.quantize(wx_core::layer::TYPED_RATE_MIN_MMH);
    let none = kind_band.quantize(wx_core::layer::ptype::NONE);
    let mut typed = 0;
    for k in 0..N * N {
        let (r, t) = (tile.bands[0][k], tile.bands[1][k]);
        if r == NODATA || t == NODATA {
            continue;
        }
        if r < threshold {
            assert_eq!(
                t, none,
                "sample {k}: rate {r} below the threshold with type {t}"
            );
        } else {
            typed += 1;
            if every_rate_typed {
                assert_ne!(
                    t, none,
                    "sample {k}: rate {r} at or above the threshold with no type"
                );
            }
        }
    }
    typed
}

/// Between samples the rate is interpolated and the type was the nearest
/// sample's, so they disagreed wherever a light rate met no rate: a rate
/// at or above the threshold next to NONE (drawn as nothing), or a type at
/// a rate below it.
#[test]
fn rate_and_type_agree_between_samples() {
    // Light rain on a checkerboard of rain and dry samples, and a heavier
    // patch: 0.03 mm/h rain next to 0 mm/h NONE.
    let g = precip_part(
        latlon(-100.0, 40.0, 0.1),
        200,
        150,
        |i, j| {
            if (i + j) % 2 == 0 || (i > 120 && j > 80) {
                0.03
            } else {
                0.0
            }
        },
        |i, j| {
            if (i + j) % 2 == 0 || (i > 120 && j > 80) {
                1.0
            } else {
                0.0
            }
        },
    );
    let mut typed = 0;
    for z in [6u8, 8] {
        for t in g.covering(z) {
            if let Some(tile) = one(&g, t) {
                typed += assert_rate_and_type_agree(&tile, true);
            }
        }
    }
    assert!(typed > 10_000, "{typed}");
    // At the part's own samples nothing changes.
    let precip = layer("precip").unwrap();
    let z = 12;
    for (i, j, want) in [(10.0, 10.0, 1.0), (11.0, 10.0, 0.0), (150.0, 100.0, 1.0)] {
        let (lon, lat) = g.proj.ij_to_lonlat(i, j);
        let (plon, plat) = lattice_point(z, lon, lat);
        let n = f64::from(1u32 << z);
        let (x, y) = (lon_to_x(plon) * n, lat_to_y(plat) * n);
        let tc = TileCoord::new(z, x.floor() as u32, y.floor() as u32).unwrap();
        let tile = one(&g, tc).unwrap();
        let c = ((x - x.floor()) * TILE_CELLS as f64).round() as usize;
        let r = ((y - y.floor()) * TILE_CELLS as f64).round() as usize;
        let (pi, pj) = g.proj.lonlat_to_ij(plon, plat);
        if (pi - i).abs() < 0.05 && (pj - j).abs() < 0.05 {
            assert_eq!(
                precip.bands[1].dequantize(tile.bands[1][r * N + c]),
                Some(want)
            );
        }
    }
}

/// The same across a blend seam, where the rate is a weighted mean and the
/// type the heavier part's: a rain part over a dry, typeless global one.
/// Shared tile edges stay bit-identical.
#[test]
fn rate_and_type_agree_across_seams() {
    let rain = precip_part(
        latlon(-110.0, 45.0, 0.05),
        400,
        300,
        |i, _| if i % 7 == 0 { 0.0 } else { 0.4 },
        |i, _| if i % 7 == 0 { 0.0 } else { 2.0 },
    );
    let dry = precip_part(global(1440, 721), 1440, 721, |_, _| 0.0, |_, _| 0.0);
    let mut typed = 0;
    for z in [5u8, 7] {
        let tiles = rain.covering(z);
        for &t in &tiles {
            if let Some(tile) = render_stack(&[&rain, &dry], t) {
                typed += assert_rate_and_type_agree(&tile, true);
            }
        }
        assert_shared_edges(&tiles, |t| render_stack(&[&rain, &dry], t));
    }
    assert!(typed > 10_000, "{typed}");
    let m = Mosaic::new(vec![rain, dry]);
    for z in [4u8, 6] {
        let tiles = m.grids[0].covering(z);
        for pick in [tile_parts, tile_parts_must, tile_parts_may] {
            assert_shared_edges(&tiles, |t| render_stack(&m.stack(pick(&m.parts, t)), t));
        }
    }
}

/// The compositor pairs a layer's bands when there are exactly one
/// continuous and one categorical band; in the registry those layers are
/// the precipitation-type ones, a rate in mm/h and its type.
#[test]
fn categorical_bands_are_precipitation_types() {
    use wx_core::layer::{BandKind, Quantity, Render};
    for l in wx_core::LAYERS {
        if !l.bands.iter().any(|b| b.kind == BandKind::Categorical) {
            continue;
        }
        let Render::PrecipType { rate, ptype } = l.render else {
            panic!(
                "{}: categorical band outside a precipitation-type layer",
                l.id
            );
        };
        assert_eq!(l.bands.len(), 2, "{}", l.id);
        let (r, t) = (&l.bands[usize::from(rate)], &l.bands[usize::from(ptype)]);
        assert!(
            r.kind == BandKind::Continuous
                && r.quantity == Quantity::PrecipRate
                && r.unit == "mm/h",
            "{}",
            l.id
        );
        assert!(
            t.kind == BandKind::Categorical && t.quantity == Quantity::Category,
            "{}",
            l.id
        );
    }
}

/// A regional lat/lon grid wider than 270 degrees renders its east end.
#[test]
fn wide_regional_grids_render_their_east_end() {
    let temp = layer("temp").unwrap();
    let g = FrameGrid::build(
        temp,
        &raster(
            GridProjection::LatLon(LatLonGrid {
                lon0: -100.0,
                lat0: 60.0,
                dlon: 0.25,
                dlat: -0.25,
                periodic: false,
            }),
            1120,
            100,
            |i, _| i as f32 * 0.01,
        ),
    );
    let v = sample_at(&[&g], 5, 170.0, 50.0).expect("data at 170 E");
    assert!((v - 10.8).abs() < 0.02, "{v}");
    assert!(
        sample_at(&[&g], 5, -105.0, 50.0).is_none(),
        "the gap stays empty"
    );
}

// ---------------------------------------------------------------------------
// Bounds of the data
// ---------------------------------------------------------------------------

/// Every sample with a value, and every point a quarter of a cell from it,
/// is inside the bounds; so is the solid box.
fn assert_bounds_hold_the_data(g: &FrameGrid, r: &Raster) {
    let b = g.bounds();
    let inside = |lon: f64, lat: f64| {
        let lon = wx_core::proj::wrap_lon(lon);
        let lon_ok = if b.west == -180.0 && b.east == 180.0 {
            true
        } else if b.wraps() {
            lon >= b.west || lon <= b.east
        } else {
            lon >= b.west && lon <= b.east
        };
        lon_ok && lat >= b.south && lat <= b.north
    };
    let step = (r.nx * r.ny / 20_000).max(1);
    for k in (0..r.nx * r.ny).step_by(step) {
        if r.bands[0][k].is_nan() {
            continue;
        }
        let (i, j) = ((k % r.nx) as f64, (k / r.nx) as f64);
        for (di, dj) in [
            (0.0, 0.0),
            (-0.45, -0.45),
            (0.45, 0.45),
            (-0.45, 0.45),
            (0.45, -0.45),
        ] {
            let (lon, lat) = r.proj.ij_to_lonlat(i + di, j + dj);
            assert!(
                inside(lon, lat),
                "({i}, {j}) at ({lon}, {lat}) outside {b:?}"
            );
        }
    }
    if let Some(s) = g.solid() {
        let (sw, se) = (inside(s.west, s.south), inside(s.east, s.north));
        assert!(sw && se, "solid {s:?} outside {b:?}");
    }
}

/// A grid whose southern third is missing advertises bounds that stop at
/// its data, on lat/lon and Lambert grids alike.
#[test]
fn bounds_stop_at_the_data() {
    let temp = layer("temp").unwrap();
    // 45 N to 30 N at 0.05 degree; rows from 200 on (south of 35.025 N)
    // missing.
    let r = raster(latlon(-110.0, 45.0, 0.05), 400, 300, |i, j| {
        if j >= 200 { f32::NAN } else { i as f32 * 0.01 }
    });
    let g = FrameGrid::build(temp, &r);
    let whole = r.proj.bounds(400, 300);
    let b = g.bounds();
    let edge = 45.0 - 199.5 * 0.05;
    assert!(b.south <= edge && b.south > edge - 0.15, "{b:?}");
    assert!(whole.south < 30.03);
    assert!(
        (b.north - whole.north).abs() < 0.15 && (b.west - whole.west).abs() < 0.15,
        "{b:?}"
    );
    assert!((b.east - whole.east).abs() < 0.15, "{b:?}");
    assert_bounds_hold_the_data(&g, &r);
    // Tiles wholly south of the data are not covered.
    let z = 7;
    assert!(g.covering(z).iter().all(|t| t.bounds().north > b.south));
    assert!(
        g.covering(z).len()
            < FrameGrid::build(temp, &raster(r.proj.clone(), 400, 300, |_, _| 1.0))
                .covering(z)
                .len()
    );
    // HRRR's Lambert grid without its southern third (rows run south to
    // north).
    let r = hrrr_like(1799, 1059);
    let r = Raster {
        bands: vec![
            r.bands[0]
                .iter()
                .enumerate()
                .map(|(k, &v)| if k / 1799 < 353 { f32::NAN } else { v })
                .collect(),
        ],
        ..r
    };
    let g = FrameGrid::build(temp, &r);
    let lowest = (0..1799)
        .map(|i| r.proj.ij_to_lonlat(i as f64, 352.5).1)
        .fold(f64::INFINITY, f64::min);
    let b = g.bounds();
    assert!(
        b.south <= lowest && b.south > lowest - 0.1,
        "{b:?} vs {lowest}"
    );
    assert!(b.south > 25.0 && r.proj.bounds(1799, 1059).south < 21.2);
    assert_bounds_hold_the_data(&g, &r);
    // Complete grids keep the grid's own box.
    let full = FrameGrid::build(temp, &hrrr_like(400, 300));
    assert_eq!(full.bounds(), hrrr_proj().bounds(400, 300));
}

/// The data's box crosses the antimeridian when the data does and not when
/// only the grid does; a global grid with a hole still spans every
/// longitude.
#[test]
fn data_bounds_and_the_antimeridian() {
    let temp = layer("temp").unwrap();
    // 170 E to 160 W; the part east of 179 E missing, then the part west of
    // 181 E.
    let west_part = raster(latlon(170.01, 65.0, 0.02), 1500, 1000, |i, _| {
        if i >= 450 { f32::NAN } else { 1.0 }
    });
    let g = FrameGrid::build(temp, &west_part);
    let b = g.bounds();
    assert!(
        !b.wraps() && b.west > 169.9 && b.east < 179.1 && b.east > 178.9,
        "{b:?}"
    );
    assert_bounds_hold_the_data(&g, &west_part);
    let east_part = raster(latlon(170.01, 65.0, 0.02), 1500, 1000, |i, _| {
        if i < 550 { f32::NAN } else { 1.0 }
    });
    let g = FrameGrid::build(temp, &east_part);
    let b = g.bounds();
    assert!(
        !b.wraps() && b.west > -179.1 && b.west < -178.9 && b.east < -159.9,
        "{b:?}"
    );
    assert_bounds_hold_the_data(&g, &east_part);
    let across = raster(latlon(170.01, 65.0, 0.02), 1500, 1000, |i, j| {
        if j > 500 || i < 200 { f32::NAN } else { 1.0 }
    });
    let g = FrameGrid::build(temp, &across);
    let b = g.bounds();
    // Rows 0 to 500 (south to 54.99 N), blocks of 4 rows (0.08 degree).
    assert!(
        b.wraps() && b.west > 173.8 && b.south > 54.8 && b.south < 54.99,
        "{b:?}"
    );
    assert_bounds_hold_the_data(&g, &across);
    // A global grid with a hole: the poles are in the data.
    let (nx, ny) = (720, 361);
    let world = raster(global(nx, ny), nx, ny, |i, j| {
        if (i as f32 - 300.0).hypot(j as f32 - 180.0) < 40.0 {
            f32::NAN
        } else {
            1.0
        }
    });
    let g = FrameGrid::build(temp, &world);
    assert_eq!(g.bounds(), LonLatBox::new(-180.0, -90.0, 180.0, 90.0));
    // Only the northern half of a global grid.
    let north = raster(
        global(nx, ny),
        nx,
        ny,
        |_, j| if j > 180 { f32::NAN } else { 1.0 },
    );
    let g = FrameGrid::build(temp, &north);
    let b = g.bounds();
    assert!(
        b.west == -180.0 && b.east == 180.0 && b.north == 90.0,
        "{b:?}"
    );
    // Row 180 (the equator) ends a block of 4 rows of 0.5 degree.
    assert!(b.south < 0.0 && b.south > -3.0, "{b:?}");
    // And the same bounds after a trip through `.wxg`.
    let back = FrameGrid::from_bytes(&g.to_bytes()).unwrap();
    assert_eq!(back.bounds(), b);
}
