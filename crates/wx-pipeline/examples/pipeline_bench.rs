//! Timings of decoding, building, storing, loading and tiling parts, and a
//! digest of everything they produce, so two implementations can be shown
//! to agree bit for bit. Single-thread figures are the thread's CPU time
//! (on macOS), so that other work on the machine counts less.
//!
//! `cargo run --release -p wx-pipeline --example pipeline_bench [decode|grid|render|all]`;
//! with `LEVELS=1` the render section also compares zlib levels.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Instant;

use wx_core::layer::{BandKind, Derive, FieldSel, Stat, layer};
use wx_core::proj::{
    Earth, GeostationaryGrid, LaeaGrid, LambertGrid, LatLonGrid, MercatorGrid, PolarStereoGrid,
    RotatedLatLonGrid, Sweep, TILE_SAMPLES, TransverseMercatorGrid, lat_to_y, lon_to_x,
};
use wx_core::{Family, GridProjection, InputSpec, LayerSpec, Raster, TileCoord};
use wx_pipeline::{FrameGrid, decode, decode_messages, derive, render_stack, scan};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

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

const fn sel(var: &'static str, level: &'static str) -> FieldSel {
    FieldSel {
        var,
        level,
        stat: Stat::Instant,
    }
}

/// Best of `n` runs, in milliseconds of wall time: the best run is the one
/// least disturbed by whatever else the machine does.
fn time(n: usize, mut f: impl FnMut()) -> f64 {
    (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e3
        })
        .fold(f64::INFINITY, f64::min)
}

/// The calling thread's CPU time in milliseconds, where the platform gives it.
fn thread_cpu_ms() -> Option<f64> {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn clock_gettime_nsec_np(clock: u32) -> u64;
        }
        const CLOCK_THREAD_CPUTIME_ID: u32 = 16;
        Some(unsafe { clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID) } as f64 / 1e6)
    }
    #[cfg(not(target_os = "macos"))]
    None
}

/// Best of `n` runs on a pool of one thread, in milliseconds of its CPU
/// time (of wall time where that is not available).
fn one_thread(n: usize, mut f: impl FnMut() + Send) -> f64 {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    pool.install(|| {
        if thread_cpu_ms().is_none() {
            return time(n, f);
        }
        (0..n)
            .map(|_| {
                let t = thread_cpu_ms().unwrap();
                f();
                thread_cpu_ms().unwrap() - t
            })
            .fold(f64::INFINITY, f64::min)
    })
}

/// Consume a result the optimizer must not see through.
fn keep<T>(x: T) {
    std::hint::black_box(x);
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// A smooth synthetic raster for every band of `spec`, with a hole of
/// missing values and a ragged edge, so coverage ramps and fallbacks are
/// exercised.
fn synthetic(spec: &LayerSpec, proj: GridProjection, nx: usize, ny: usize, seed: f32) -> FrameGrid {
    let band = |b: usize| -> Vec<f32> {
        let kind = &spec.bands[b];
        (0..nx * ny)
            .map(|k| {
                let (i, j) = ((k % nx) as f32, (k / nx) as f32);
                let hole = (i - 0.6 * nx as f32).hypot(j - 0.4 * ny as f32) < 0.08 * nx as f32;
                if hole || (j < 3.0 && i < 0.5 * nx as f32) {
                    return f32::NAN;
                }
                let s = (i * 0.013 + seed).sin() * (j * 0.017 + b as f32).cos();
                match kind.kind {
                    BandKind::Categorical => (s * 3.0 + 3.0).floor().clamp(0.0, 5.0),
                    BandKind::Continuous => {
                        let (lo, hi) = (
                            kind.min_value(),
                            kind.max_value().min(kind.min_value() + 200.0),
                        );
                        lo + (hi - lo) * (0.5 + 0.45 * s)
                    }
                }
            })
            .collect()
    };
    FrameGrid::build(
        spec,
        &Raster {
            nx,
            ny,
            proj,
            bands: (0..spec.bands.len()).map(band).collect(),
        },
    )
}

fn tile_at(z: u8, lon: f64, lat: f64) -> TileCoord {
    let n = f64::from(1u32 << z);
    TileCoord::new(z, (lon_to_x(lon) * n) as u32, (lat_to_y(lat) * n) as u32).unwrap()
}

#[derive(Default)]
struct Digest(DefaultHasher);

impl Digest {
    fn add(&mut self, v: impl Hash) {
        v.hash(&mut self.0);
    }
    fn hex(&self) -> String {
        format!("{:016x}", self.0.finish())
    }
}

/// What a grid tells about itself: shape, levels, boxes and weights.
fn grid_digest(g: &FrameGrid, d: &mut Digest) {
    let bits = |b: wx_core::LonLatBox| [b.west, b.south, b.east, b.north].map(f64::to_bits);
    d.add((
        g.dimensions(),
        g.level_count(),
        bits(g.bounds()),
        g.solid().map(bits),
    ));
    let b = g.bounds();
    for k in 0..400 {
        let lon = b.west + (k % 20) as f64 * 0.05 * (b.east - b.west);
        let lat = b.south + (k / 20) as f64 * 0.05 * (b.north - b.south);
        d.add(g.coverage_at(lon, lat).to_bits());
    }
}

fn decoding() {
    println!("== decode (best of 5, all cores)");
    let tmp = [sel("TMP", "2 m above ground")];
    let refc = [sel("REFC", "entire atmosphere")];
    for (name, sels) in [
        (
            "MRMS_MergedReflectivityQCComposite_00.50_20261106-114838.grib2.gz",
            &[][..],
        ),
        ("hrrr_t10z_f06__TMP_2mabove.grib2", &tmp),
        ("hrrr_t10z_f06__REFC_entire.grib2", &refc),
        ("gfs_t06z_f024__TMP_2mabove.grib2", &tmp),
        ("nbm_t10z_f001_TMP2m.grib2", &tmp),
        ("hrdps_t12z_PT003H__PRATE_Sfc.grib2", &[]),
        ("gdps_t12z_PT003H__DewPoint_AGL-2m.grib2", &[]),
        ("icond2_t12z_003__t_2m.grib2", &[]),
        ("ifs0p1_t00z_3h__2t_sfc.grib2", &[]),
        ("aromeom_polyn_t12z_SP1_001H__2t_tsnowp.grib2", &[]),
        ("arome/aromeom_guyane_t12z_SP1_012H.grib2", &[]),
    ] {
        let bytes = fixture(name);
        let mut d = Digest::default();
        // Single-field files through `decode`, packages through `scan`.
        let (ms, what) = if !sels.is_empty() || name.ends_with(".gz") {
            let set = decode(&bytes, sels).unwrap();
            set.fields
                .iter()
                .for_each(|f| d.add(f.values.iter().map(|v| v.to_bits()).collect::<Vec<_>>()));
            (
                time(5, || drop(decode(&bytes, sels).unwrap())),
                format!("{} x {}", set.fields[0].nx, set.fields[0].ny),
            )
        } else {
            let infos = scan(&bytes).unwrap();
            let which: Vec<usize> = (0..infos.len())
                .filter(|&k| infos[k].unsupported.is_none())
                .collect();
            let packing: Vec<u16> = which.iter().map(|&k| infos[k].packing_template).collect();
            for m in decode_messages(&bytes, &which).unwrap() {
                d.add(
                    m.field
                        .values
                        .iter()
                        .map(|v| v.to_bits())
                        .collect::<Vec<_>>(),
                );
            }
            let ms = time(5, || drop(decode_messages(&bytes, &which).unwrap()));
            (ms, format!("{} fields, packing {packing:?}", which.len()))
        };
        println!("{name}: {ms:.2} ms ({what}) digest {}", d.hex());
    }
}

struct Parts {
    mrms: FrameGrid,
    hrrr: FrameGrid,
    gfs: FrameGrid,
    hrdps: FrameGrid,
    extra: Vec<FrameGrid>,
}

fn grids() -> Parts {
    println!("== grids (best of 5, all cores; one thread in CPU time)");
    let radar = layer("radar").unwrap();
    let rule = input(Derive::Reflectivity);
    let set = decode(
        &fixture("MRMS_MergedReflectivityQCComposite_00.50_20261106-114838.grib2.gz"),
        &[],
    )
    .unwrap();
    println!(
        "MRMS derive: {:.2} ms",
        time(5, || drop(derive(radar, &rule, &set.fields).unwrap()))
    );
    let raster = derive(radar, &rule, &set.fields).unwrap();
    let build = time(5, || drop(FrameGrid::build(radar, &raster)));
    let build1 = one_thread(5, || drop(FrameGrid::build(radar, &raster)));
    println!("MRMS 7000 x 3500 build: {build:.1} ms ({build1:.1} ms on one thread)");
    let mrms = FrameGrid::build(radar, &raster);
    let bytes = mrms.to_bytes();
    println!(
        "MRMS to_bytes: {:.1} ms, {:.2} MB",
        time(5, || drop(mrms.to_bytes())),
        bytes.len() as f64 / 1e6
    );
    let load = time(7, || drop(FrameGrid::from_bytes(&bytes).unwrap()));
    let load1 = one_thread(5, || drop(FrameGrid::from_bytes(&bytes).unwrap()));
    println!("MRMS from_bytes: {load:.1} ms ({load1:.1} ms on one thread)");
    let t = Instant::now();
    let solid = mrms.solid();
    println!(
        "MRMS solid(): {:.1} ms -> {solid:?}",
        t.elapsed().as_secs_f64() * 1e3
    );

    let part = |name: &str| {
        let set = decode(&fixture(name), &[sel("REFC", "entire atmosphere")]).unwrap();
        FrameGrid::build(radar, &derive(radar, &rule, &set.fields).unwrap())
    };
    let (hrrr, gfs) = (
        part("hrrr_t10z_f06__REFC_entire.grib2"),
        part("gfs_t06z_f024__REFC_entire.grib2"),
    );
    for (name, g) in [("HRRR", &hrrr), ("GFS", &gfs)] {
        let b = g.to_bytes();
        println!(
            "{name} from_bytes: {:.1} ms",
            time(5, || drop(FrameGrid::from_bytes(&b).unwrap()))
        );
    }
    let temp = layer("temp").unwrap();
    let t = decode(
        &fixture("gfs_t06z_f024__TMP_2mabove.grib2"),
        &[sel("TMP", "2 m above ground")],
    )
    .unwrap();
    let r = derive(
        temp,
        &input(Derive::Linear {
            mul: 1.0,
            add: -273.15,
        }),
        &t.fields,
    )
    .unwrap();
    println!(
        "GFS temp build: {:.1} ms",
        time(5, || drop(FrameGrid::build(temp, &r)))
    );

    // HRDPS-like: 2.5 km rotated lat/lon over Canada and the northern US.
    let rotated = LatLonGrid {
        lon0: 345.17878,
        lat0: -12.302501,
        dlon: 0.0225,
        dlat: 0.0225,
        periodic: false,
    };
    let rotated =
        GridProjection::RotatedLatLon(RotatedLatLonGrid::new(-36.08852, 245.305142, rotated));
    let hrdps = synthetic(radar, rotated, 2540, 1290, 0.3);
    // Sixteen regional grids round the world, four of each projection.
    let extra = (0..16)
        .map(|k| {
            let (lon, lat) = (-170.0 + k as f64 * 22.0, -50.0 + (k % 6) as f64 * 20.0);
            let e = Earth::WGS84;
            let proj = match k % 4 {
                0 => GridProjection::LatLon(LatLonGrid {
                    lon0: lon,
                    lat0: lat + 10.0,
                    dlon: 0.02,
                    dlat: -0.02,
                    periodic: false,
                }),
                1 => GridProjection::Mercator(MercatorGrid::from_first_point(
                    e,
                    lat,
                    lon + 10.0,
                    lat - 5.0,
                    lon,
                    2500.0,
                    2500.0,
                )),
                2 => GridProjection::LambertAzimuthal(LaeaGrid::from_first_point(
                    e,
                    lat,
                    lon + 10.0,
                    lat - 8.0,
                    lon,
                    2000.0,
                    2000.0,
                )),
                _ => GridProjection::TransverseMercator(TransverseMercatorGrid::from_first_point(
                    e,
                    lat,
                    lon + 5.0,
                    1.0,
                    lat - 5.0,
                    lon,
                    1000.0,
                    1000.0,
                )),
            };
            synthetic(radar, proj, 1000, 800, k as f32)
        })
        .collect();
    Parts {
        mrms,
        hrrr,
        gfs,
        hrdps,
        extra,
    }
}

/// Per-sample costs of a part's projection where the compositor uses it:
/// combining a column's and a row's terms, the whole forward map, and the
/// coverage weight; and the cost of the grid's bounding box.
fn projection_costs(name: &str, g: &FrameGrid) {
    let b = g.bounds();
    let t = tile_at(7, 0.5 * (b.west + b.east), 0.5 * (b.south + b.north));
    let lonlat: Vec<(f64, f64)> = (0..TILE_SAMPLES).map(|k| t.sample_lonlat(k, k)).collect();
    let proj = g.projection();
    let lon_t: Vec<_> = lonlat.iter().map(|p| proj.lon_terms(p.0)).collect();
    let lat_t: Vec<_> = lonlat.iter().map(|p| proj.lat_terms(p.1)).collect();
    let per_sample = |ms: f64| ms * 1e6 / (TILE_SAMPLES * TILE_SAMPLES) as f64;
    let every = |f: &(dyn Fn(f64, f64) + Sync)| {
        per_sample(one_thread(5, || {
            lonlat
                .iter()
                .for_each(|&(_, lat)| lonlat.iter().for_each(|&(lon, _)| f(lon, lat)))
        }))
    };
    let terms = per_sample(one_thread(5, || {
        lat_t
            .iter()
            .for_each(|lt| lon_t.iter().for_each(|ln| keep(proj.ij_from_terms(ln, lt))))
    }));
    let forward = every(&|lon, lat| keep(proj.lonlat_to_ij(lon, lat)));
    let coverage = every(&|lon, lat| keep(g.coverage_at(lon, lat)));
    let (nx, ny) = g.dimensions();
    let bounds = one_thread(5, || keep(proj.bounds(nx, ny))) * 1e3;
    println!(
        "{name}: {terms:.1} ns a sample from terms, {forward:.1} ns from lon/lat, coverage_at {coverage:.1} ns, bounds() {bounds:.0} us"
    );
}

fn rendering(p: &Parts) {
    println!("== render_stack (median over tiles of the best of 5, one thread)");
    let mut d = Digest::default();
    for g in [&p.mrms, &p.hrrr, &p.gfs, &p.hrdps]
        .into_iter()
        .chain(&p.extra)
    {
        grid_digest(g, &mut d);
        grid_digest(&FrameGrid::from_bytes(&g.to_bytes()).unwrap(), &mut d);
    }
    println!("grid digest {}", d.hex());

    let (mut samples, mut encoded) = (Digest::default(), Digest::default());
    let mut record = |bytes: Option<Vec<u8>>| match bytes {
        Some(b) => {
            samples.add(&wx_tile::decode(&b).unwrap().bands);
            encoded.add(&b);
            b.len()
        }
        None => {
            samples.add("empty");
            0
        }
    };
    let places = [
        (-85.0, 44.0),
        (-97.0, 38.0),
        (-75.0, 41.0),
        (-120.0, 47.0),
        (-110.0, 33.0),
        (-90.0, 30.0),
    ];
    let tiles: Vec<TileCoord> = (4..=8u8)
        .flat_map(|z| places.map(|(lon, lat)| tile_at(z, lon, lat)))
        .collect();
    let stack = [&p.mrms, &p.hrdps, &p.hrrr, &p.gfs];
    for (label, parts) in [
        ("4-part stack", &stack[..]),
        ("MRMS alone", &stack[..1]),
        ("HRRR alone", &stack[2..3]),
        ("GFS alone", &stack[3..4]),
        ("rotated alone", &stack[1..2]),
    ] {
        let (mut times, mut enc, mut size) = (Vec::new(), Vec::new(), 0);
        for &t in &tiles {
            let out = render_stack(parts, t);
            if let Some(tile) = out.as_deref().map(|b| wx_tile::decode(b).unwrap()) {
                enc.push(one_thread(5, || drop(wx_tile::encode(&tile))));
            }
            size += record(out);
            times.push(one_thread(5, || drop(render_stack(parts, t))));
        }
        let max = times.iter().copied().fold(0.0, f64::max);
        println!(
            "{label}: {} tiles z4-z8 ({} kB): median {:.2} ms, max {max:.2} ms, of which WXT1 encode median {:.2} ms",
            times.len(),
            size / 1000,
            median(times),
            median(enc)
        );
    }
    if std::env::var("LEVELS").is_ok() {
        let decoded: Vec<_> = tiles
            .iter()
            .filter_map(|&t| render_stack(&stack, t))
            .map(|b| wx_tile::decode(&b).unwrap())
            .collect();
        for level in 1..=6 {
            let size: usize = decoded
                .iter()
                .map(|t| wx_tile::encode_with(t, level).len())
                .sum();
            let ms = time(5, || {
                decoded
                    .iter()
                    .for_each(|t| drop(wx_tile::encode_with(t, level)))
            });
            println!(
                "zlib level {level}: {} kB for {} tiles, {:.3} ms a tile",
                size / 1000,
                decoded.len(),
                ms / decoded.len() as f64
            );
        }
    }
    let t = tiles[tiles.len() / 2];
    println!(
        "4-part stack, tile {t:?}, all cores: {:.2} ms",
        time(9, || drop(render_stack(&stack, t)))
    );
    for g in p.extra.iter().step_by(5) {
        let b = g.bounds();
        let at = |z| tile_at(z, 0.5 * (b.west + b.east), 0.5 * (b.south + b.north));
        let times = [5u8, 7, 9].map(|z| {
            record(render_stack(&[g], at(z)));
            one_thread(5, || drop(render_stack(&[g], at(z))))
        });
        println!(
            "projection {} alone: {times:.2?} ms at z5/z7/z9",
            g.projection().to_params().0
        );
    }
    let mut twenty: Vec<&FrameGrid> = vec![&p.mrms, &p.hrdps, &p.hrrr];
    twenty.extend(p.extra.iter());
    twenty.push(&p.gfs);
    let z0 = TileCoord::new(0, 0, 0).unwrap();
    record(render_stack(&twenty, z0));
    let one = one_thread(5, || drop(render_stack(&twenty, z0)));
    let many = time(5, || drop(render_stack(&twenty, z0)));
    println!("z0 tile from 20 parts: {one:.1} ms on one thread, {many:.1} ms on all cores");

    // Two-band (rate and type) and three-band (wind) layers, stacked: Lambert
    // over polar stereographic over geostationary over global lat/lon.
    for id in ["precip", "wind"] {
        let spec = layer(id).unwrap();
        let lambert = LambertGrid::new(
            38.5,
            38.5,
            262.5,
            21.138123,
            237.280472,
            3000.0,
            3000.0,
            6_371_229.0,
        );
        let a = synthetic(spec, GridProjection::Lambert(lambert), 1799, 1059, 0.0);
        let stereo = PolarStereoGrid::from_first_point(
            Earth::sphere(6_371_229.0),
            false,
            60.0,
            210.0,
            40.53,
            181.43,
            3000.0,
            3000.0,
        );
        let c = synthetic(
            spec,
            GridProjection::PolarStereographic(stereo),
            1299,
            919,
            2.0,
        );
        let geos = GeostationaryGrid::new(
            Earth::GRS80,
            35_786_023.0,
            -75.0,
            Sweep::X,
            112e-6,
            -0.151844,
            -112e-6,
            0.151844,
        );
        let geo = synthetic(spec, GridProjection::Geostationary(geos), 2712, 2712, 3.0);
        let global = LatLonGrid {
            lon0: 0.0,
            lat0: 90.0,
            dlon: 0.25,
            dlat: -0.25,
            periodic: true,
        };
        let b = synthetic(spec, GridProjection::LatLon(global), 1440, 721, 1.0);
        let stack = [&a, &c, &geo, &b];
        if id == "precip" {
            println!("== projections (best of 5, one thread)");
            let named = [
                ("lat/lon", &b),
                ("Lambert", &a),
                ("polar stereographic", &c),
                ("geostationary", &geo),
            ];
            let more = [
                ("rotated", &p.hrdps),
                ("Mercator", &p.extra[1]),
                ("LAEA", &p.extra[2]),
                ("transverse Mercator", &p.extra[3]),
            ];
            named
                .into_iter()
                .chain(more)
                .for_each(|(name, g)| projection_costs(name, g));
        }
        let times = tiles.iter().map(|&t| {
            record(render_stack(&stack, t));
            one_thread(5, || drop(render_stack(&stack, t)))
        });
        let times: Vec<f64> = times.collect();
        for t in [
            tile_at(3, -150.0, 60.0),
            tile_at(5, -160.0, 58.0),
            tile_at(4, -60.0, -10.0),
        ] {
            record(render_stack(&stack, t));
        }
        println!(
            "{id} 4-part stack ({} bands): median {:.2} ms",
            spec.bands.len(),
            median(times)
        );
    }
    println!("tile samples digest {}", samples.hex());
    println!("encoded tiles digest {}", encoded.hex());
}

fn main() {
    let what = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    if what == "decode" || what == "all" {
        decoding();
    }
    if what != "decode" {
        let parts = grids();
        if what != "grid" {
            rendering(&parts);
        }
    }
}
