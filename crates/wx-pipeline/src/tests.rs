//! `FrameGrid` round trips and the `.wxg` format.

use super::*;
use wx_core::layer::layer;
use wx_core::proj::{
    EARTH_RADIUS_NCEP, Earth, GeostationaryGrid, LaeaGrid, LambertGrid, LatLonGrid, MercatorGrid,
    PolarStereoGrid, RotatedLatLonGrid, Sweep, TransverseMercatorGrid,
};

fn lambert_raster(nx: usize, ny: usize, bands: usize) -> Raster {
    let proj = GridProjection::Lambert(LambertGrid::new(
        38.5,
        38.5,
        262.5,
        21.138123,
        237.280472,
        3000.0,
        3000.0,
        EARTH_RADIUS_NCEP,
    ));
    let planes = (0..bands)
        .map(|b| {
            (0..nx * ny)
                .map(|k| {
                    let (i, j) = ((k % nx) as f32, (k / nx) as f32);
                    if ((i + j) as usize).is_multiple_of(97) {
                        f32::NAN
                    } else {
                        ((i * 0.01 + b as f32).sin() * 12.0 + j * 0.02).round_ties_even()
                    }
                })
                .collect()
        })
        .collect();
    Raster {
        nx,
        ny,
        proj,
        bands: planes,
    }
}

fn global_raster() -> Raster {
    let (nx, ny) = (1440, 721);
    Raster {
        nx,
        ny,
        proj: GridProjection::LatLon(LatLonGrid {
            lon0: 0.0,
            lat0: 90.0,
            dlon: 0.25,
            dlat: -0.25,
            periodic: true,
        }),
        bands: vec![
            (0..nx * ny)
                .map(|k| ((k % nx) as f32 * 0.02).cos() * 30.0)
                .collect(),
        ],
    }
}

fn assert_same(a: &FrameGrid, b: &FrameGrid) {
    assert_eq!(a.proj, b.proj);
    assert_eq!(a.bands, b.bands);
    assert_eq!(a.levels, b.levels, "levels, overviews included");
    assert_eq!(a.bounds, b.bounds);
    assert_eq!(a.lon_ref.to_bits(), b.lon_ref.to_bits());
    assert_eq!(a.coverage, b.coverage, "coverage weights");
    assert_eq!(a.solid(), b.solid());
}

#[test]
fn round_trips_through_bytes() {
    let wind = layer("wind").unwrap();
    let temp = layer("temp").unwrap();
    let precip = layer("precip").unwrap();
    let mut ptype = lambert_raster(700, 500, 2);
    for v in &mut ptype.bands[1] {
        *v = (v.abs() % 6.0).floor();
    }
    for (l, raster) in [
        (wind, lambert_raster(900, 600, 3)),
        (temp, global_raster()),
        (precip, ptype),
    ] {
        let g = FrameGrid::build(l, &raster);
        let bytes = g.to_bytes();
        let back = FrameGrid::from_bytes(&bytes).unwrap();
        assert_same(&g, &back);
        // Overviews stored rather than rebuilt decode to the same grid.
        let full = wxg::to_bytes(&g, true, wxg::ZSTD_LEVEL);
        assert!(full.len() > bytes.len());
        assert_same(&g, &FrameGrid::from_bytes(&full).unwrap());
        // And render the same tiles.
        for t in g.covering(4).into_iter().take(6) {
            assert_eq!(g.render(t), back.render(t), "{t:?}");
        }
    }
}

#[test]
fn rejects_damaged_bytes() {
    let g = FrameGrid::build(layer("wind").unwrap(), &lambert_raster(300, 200, 3));
    let bytes = g.to_bytes();
    assert!(FrameGrid::from_bytes(&[]).is_err());
    assert!(FrameGrid::from_bytes(b"WXG1").is_err());
    assert!(
        FrameGrid::from_bytes(&bytes[..bytes.len() - 1]).is_err(),
        "truncated"
    );
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(FrameGrid::from_bytes(&longer).is_err(), "trailing bytes");
    let mut magic = bytes.clone();
    magic[0] = b'X';
    assert!(FrameGrid::from_bytes(&magic).is_err());
    // Every byte of the header is covered by its checksum, and every chunk
    // by zstd's: flipping any byte must give an error, never a panic or a
    // silently different grid. (A chunk's row count can change harmlessly
    // when the plane fits in one chunk either way.)
    let mut errors = 0;
    for pos in (0..bytes.len()).step_by(7).chain([4, 6, 8, 12, 16, 17, 18]) {
        let mut b = bytes.clone();
        b[pos] ^= 0x5a;
        match FrameGrid::from_bytes(&b) {
            Err(_) => errors += 1,
            Ok(back) => assert_same(&g, &back),
        }
    }
    assert!(errors > bytes.len() / 8);
}

/// Rewrites a lat/lon `.wxg` header's size and band count and re-signs it,
/// as a writer that is not ours could.
fn forged(mut b: Vec<u8>, nx: u32, ny: u32) -> Vec<u8> {
    b[8..12].copy_from_slice(&nx.to_le_bytes());
    b[12..16].copy_from_slice(&ny.to_le_bytes());
    let bands = usize::from(b[16]);
    let header_len = 4 + 2 + 2 + 4 + 4 + 4 + 5 * 8 + 10 * bands;
    let mut crc = flate2::Crc::new();
    crc.update(&b[..header_len]);
    b[header_len..header_len + 4].copy_from_slice(&crc.sum().to_le_bytes());
    b
}

#[test]
fn oversized_headers_are_refused_before_allocating() {
    let g = FrameGrid::build(layer("temp").unwrap(), &global_raster());
    let bytes = g.to_bytes();
    // Untouched, the forging helper reproduces the original.
    assert_eq!(forged(bytes.clone(), 1440, 721), bytes);
    for (nx, ny) in [
        (1 << 31, 1 << 31),
        (u32::MAX, 2),
        (8000, 5001),
        (40_000_001, 1),
    ] {
        let err = FrameGrid::from_bytes(&forged(bytes.clone(), nx, ny))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("larger than any feed produces"),
            "{nx} x {ny}: {err}"
        );
    }
    // At the limit the size is accepted, and the chunk tables (which no
    // longer match) are rejected before any plane is allocated.
    let err = FrameGrid::from_bytes(&forged(bytes, 8000, 5000))
        .unwrap_err()
        .to_string();
    assert!(err.contains("chunk layout"), "{err}");
}

#[test]
fn empty_grids_round_trip_and_render_nothing() {
    let temp = layer("temp").unwrap();
    let wind = layer("wind").unwrap();
    for (l, nx, ny) in [(temp, 0, 0), (temp, 0, 5), (wind, 7, 0)] {
        let raster = Raster {
            nx,
            ny,
            proj: global_raster().proj,
            bands: vec![vec![]; l.bands.len()],
        };
        let g = FrameGrid::build(l, &raster);
        assert_eq!(g.dimensions(), (nx, ny));
        assert!(g.render(TileCoord::new(0, 0, 0).unwrap()).is_none());
        assert!(g.covering(3).is_empty());
        assert_eq!(g.solid(), None);
        let back = FrameGrid::from_bytes(&g.to_bytes()).unwrap();
        assert_same(&g, &back);
        assert!(back.render(TileCoord::new(0, 0, 0).unwrap()).is_none());
    }
}

#[test]
#[should_panic(expected = "more than a stored grid may hold")]
fn build_refuses_grids_that_could_not_be_stored() {
    let n = MAX_GRID_POINTS + 1;
    let raster = Raster {
        nx: n,
        ny: 1,
        proj: global_raster().proj,
        bands: vec![vec![0.0; n]],
    };
    let _ = FrameGrid::build(layer("temp").unwrap(), &raster);
}

#[test]
fn covering() {
    let temp = layer("temp").unwrap();
    let g = FrameGrid::build(temp, &lambert_raster(1799, 1059, 1));
    let b = g.bounds();
    // The HRRR grid's published corners, half a cell further out.
    assert!(b.west < -134.09 && b.west > -134.2, "{b:?}");
    assert!(b.east > -60.92 && b.east < -60.8, "{b:?}");
    assert!(b.south < 21.14 && b.south > 21.0, "{b:?}");
    // The north edge bulges above the corners (47.84 N) in the middle.
    assert!(b.north > 52.0 && b.north < 53.5, "{b:?}");
    for z in 0..=5 {
        let tiles = g.covering(z);
        assert!(!tiles.is_empty());
        for t in &tiles {
            assert!(t.bounds().intersects(&b));
        }
        // Every tile with data is among them.
        let n = 1u32 << z;
        for y in 0..n.min(16) {
            for x in 0..n.min(16) {
                let t = TileCoord::new(z, x, y).unwrap();
                if g.render(t).is_some() {
                    assert!(tiles.contains(&t), "{t:?}");
                }
            }
        }
    }
    assert_eq!(g.covering(0), vec![TileCoord::new(0, 0, 0).unwrap()]);
    // A part across the antimeridian covers tiles at both ends of a row.
    let ak = FrameGrid::build(
        temp,
        &Raster {
            nx: 650,
            ny: 460,
            proj: GridProjection::PolarStereographic(PolarStereoGrid::from_first_point(
                Earth::sphere(6_371_229.0),
                false,
                60.0,
                225.0,
                41.612949,
                185.117126,
                6000.0,
                6000.0,
            )),
            bands: vec![vec![1.0; 650 * 460]],
        },
    );
    let xs: Vec<u32> = ak.covering(3).iter().map(|t| t.x).collect();
    assert!(xs.contains(&0) && xs.contains(&7), "{xs:?}");
}

/// Every projection survives the trip through `.wxg` unchanged.
#[test]
fn every_projection_round_trips() {
    let temp = layer("temp").unwrap();
    let projections = [
        GridProjection::PolarStereographic(PolarStereoGrid::from_projected(
            Earth::WGS84,
            false,
            60.0,
            10.0,
            -542_696.8,
            -3_623_088.9,
            1000.0,
            -1000.0,
        )),
        GridProjection::Mercator(MercatorGrid::from_first_point(
            Earth::sphere(6_371_200.0),
            20.0,
            202.3,
            18.072699,
            198.474999,
            2500.0,
            2500.0,
        )),
        GridProjection::RotatedLatLon(RotatedLatLonGrid::new(
            -36.08852,
            245.305142,
            LatLonGrid {
                lon0: 345.17878,
                lat0: -12.302501,
                dlon: 0.0225,
                dlat: 0.0225,
                periodic: false,
            },
        )),
        GridProjection::LambertAzimuthal(LaeaGrid::from_projected(
            Earth::WGS84,
            55.0,
            10.0,
            -1_949_500.0,
            2_099_500.0,
            1000.0,
            -1000.0,
        )),
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
        GridProjection::Geostationary(GeostationaryGrid::new(
            Earth::GRS80,
            35_786_023.0,
            -75.0,
            Sweep::X,
            5.6e-4,
            -0.151_844,
            -5.6e-4,
            0.151_844,
        )),
    ];
    for proj in projections {
        let (nx, ny) = (300, 250);
        let r = Raster {
            nx,
            ny,
            proj: proj.clone(),
            bands: vec![(0..nx * ny).map(|k| (k % 97) as f32).collect()],
        };
        let g = FrameGrid::build(temp, &r);
        let back = FrameGrid::from_bytes(&g.to_bytes()).unwrap();
        assert_same(&g, &back);
        assert_eq!(back.projection(), &proj);
    }
}

#[test]
fn version_1_grids_are_refused() {
    let g = FrameGrid::build(layer("temp").unwrap(), &global_raster());
    let mut b = g.to_bytes();
    b[4..6].copy_from_slice(&1u16.to_le_bytes());
    let err = FrameGrid::from_bytes(&b).unwrap_err().to_string();
    assert!(err.contains("unsupported .wxg version 1"), "{err}");
    // A projection kind we do not know, correctly signed, is refused too.
    let mut b = g.to_bytes();
    b[18] = 99;
    let mut crc = flate2::Crc::new();
    let header_len = 4 + 2 + 2 + 4 + 4 + 4 + 5 * 8 + 10;
    crc.update(&b[..header_len]);
    b[header_len..header_len + 4].copy_from_slice(&crc.sum().to_le_bytes());
    let err = FrameGrid::from_bytes(&b).unwrap_err().to_string();
    assert!(err.contains("projection 99"), "{err}");
}

/// Measures whether `.wxg` should store the overviews or rebuild them.
/// `cargo test --release -p wx-pipeline wxg_store_or_rebuild -- --ignored --nocapture`
#[test]
#[ignore]
fn wxg_store_or_rebuild() {
    use std::time::Instant;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/MRMS_MergedReflectivityQCComposite_00.50_20261106-114838.grib2.gz"
    );
    let bytes = std::fs::read(path).unwrap();
    let radar = layer("radar").unwrap();
    let src = wx_core::InputSpec {
        family: wx_core::Family::Mrms,
        sources: &[],
        product: "",
        fields: &[],
        derive: wx_core::layer::Derive::Reflectivity,
        hold_minutes: 6,
        fill_past: false,
    };
    let src = &src;
    let set = decode(&bytes, &[]).unwrap();
    let raster = derive(radar, src, &set.fields).unwrap();
    let g = FrameGrid::build(radar, &raster);
    let best = |f: &dyn Fn()| {
        (0..5)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    for level in [1, 3, 6, 9] {
        for store in [false, true] {
            let mut out = Vec::new();
            let enc = best(&|| {
                let _ = wxg::to_bytes(&g, store, level);
            });
            out.push(wxg::to_bytes(&g, store, level));
            let dec = best(&|| {
                let _ = FrameGrid::from_bytes(&out[0]).unwrap();
            });
            println!(
                "zstd {level} store_overviews={store}: {:.2} MB, encode {enc:?}, decode {dec:?}",
                out[0].len() as f64 / 1e6
            );
        }
    }
    let t = Instant::now();
    let base = g.levels[0].clone();
    let _ = grid::build_pyramid(base, &g.bands, false);
    println!("pyramid rebuild alone: {:?}", t.elapsed());
}
