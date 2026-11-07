//! Timings, allocation counts and heap peaks of the ingest paths, against
//! the in-memory remote and the fixtures. Ignored by default; run in
//! release, on one test thread, with the output shown:
//!
//! ```text
//! cargo test -p wx-ingest --release --lib bench:: -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Each figure is the median of [`RUNS`] runs. The `peak_*` tests build one
//! part and nothing else, for `/usr/bin/time -l` around the test binary
//! (`--ignored --exact bench::peak_mrms`).

use crate::driver::model::{Cycle, Listing, Model, ModelFile, ModelTask};
use crate::driver::observed::ObservedTask;
use crate::driver::{Cx, Fetched, Need, ObsFile, Observed, Payload, Steps};
use crate::engine::{self, Engine, PartJob};
use crate::families::{arome, dwd_radar, goes, hrrr, mrms, nbm, opera, rtma};
use crate::remote::memory::MemoryRemote;
use crate::remote::{FetchError, S3Bucket, codec, list};
use crate::testing::{self, heap, leak};
use crate::{idx, janitor};
use chrono::{DateTime, Duration, TimeZone, Utc};
use object_store::memory::InMemory;
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use std::sync::Arc;
use std::time::Instant;
use wx_core::layer::{self, Derive, FieldSel, Stat};
use wx_core::proj::GeostationaryGrid;
use wx_core::{
    Family, Field, FrameCatalog, FrameKind, GridProjection, Hub, InputSpec, LayerSpec, Part,
    SOURCES, SourceDef, source,
};

const RUNS: usize = 7;

use crate::testing::on as t;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / 1e6
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1e3
}

/// The median wall time of `f` in milliseconds, and the allocations of one
/// run.
fn timed<T>(mut f: impl FnMut() -> T) -> (f64, usize) {
    let mut times = Vec::new();
    let mut allocs = 0;
    for _ in 0..RUNS {
        let (n, t) = (heap::count(), Instant::now());
        std::hint::black_box(f());
        times.push(ms(t));
        allocs = heap::count() - n;
    }
    (median(times), allocs)
}

/// How a case's file is fetched.
enum Via {
    Observed(Arc<dyn Observed>, &'static str, ObsFile),
    Model(Arc<dyn Model>, DateTime<Utc>, ModelFile),
}

/// One part's way from the remote to the catalog.
struct Case {
    name: &'static str,
    mem: Arc<MemoryRemote>,
    job: PartJob,
    via: Via,
    now: DateTime<Utc>,
    /// Whether a whole cycle is timed too (it needs the listings served).
    cycle: bool,
}

impl Case {
    async fn fetch(&self, cx: &Cx<'_>) -> Vec<Payload> {
        let needs = self.job.needs();
        let source = self.job.source;
        let got = match &self.via {
            Via::Observed(o, product, file) => o.fetch(cx, source, product, file, &needs).await,
            Via::Model(m, run, file) => m.fetch(cx, source, *run, file, &needs).await,
        };
        match got.unwrap() {
            Fetched::Got(p) => p.into_iter().map(|p| p.expect("every need")).collect(),
            Fetched::Missing => panic!("{}: missing", self.name),
        }
    }

    /// A task that makes this case's part (and the others its file feeds).
    fn cycle_task(&self, engine: Arc<Engine>) -> Task {
        match &self.via {
            Via::Observed(o, product, _) => Task::Observed(ObservedTask::new(
                engine,
                o.clone(),
                self.job.source,
                product,
            )),
            Via::Model(m, _, _) => {
                let products = crate::driver::products_of(engine.layers, self.job.source);
                Task::Model(ModelTask::new(
                    engine,
                    m.clone(),
                    self.job.source,
                    &products,
                ))
            }
        }
    }
}

enum Task {
    Observed(ObservedTask),
    Model(ModelTask),
}

impl Task {
    async fn cycle(&mut self, now: DateTime<Utc>) -> engine::Tally {
        match self {
            Task::Observed(t) => t.cycle(now).await.unwrap().tally,
            Task::Model(t) => t.cycle(now, &|_, _| {}).await.unwrap().tally,
        }
    }
}

fn job_observed(layer_id: &str, source_id: &str, valid: DateTime<Utc>) -> PartJob {
    let l = layer::layer(layer_id).unwrap();
    let s = source::source(source_id).unwrap();
    PartJob::observed(l, l.input_for(s).unwrap(), s, valid)
}

fn rtma_case() -> Case {
    let mem = testing::remote(&[rtma::BUCKET]);
    let (file, idx) = testing::model_file(&[
        (
            "TMP",
            "2 m above ground",
            "anl",
            "rtma_hi_t17z__TMP_2mabove.grib2",
        ),
        (
            "UGRD",
            "10 m above ground",
            "anl",
            "rtma_hi_t17z__UGRD_10mabove.grib2",
        ),
        (
            "VGRD",
            "10 m above ground",
            "anl",
            "rtma_hi_t17z__VGRD_10mabove.grib2",
        ),
    ]);
    let valid = t(6, 17, 0);
    let key = rtma::file_key(rtma::layout("conus").unwrap(), valid);
    let size = file.len() as u64;
    let bucket = S3Bucket::aws(rtma::BUCKET);
    mem.put_s3(&bucket, &key, file);
    mem.put_s3(&bucket, &format!("{key}.idx"), idx);
    let file = ObsFile {
        valid,
        name: key,
        size: Some(size),
    };
    Case {
        name: "RTMA (Hawaii cut as CONUS, 0.07 M points)",
        mem,
        job: job_observed("temp", "rtma", valid),
        via: Via::Observed(Arc::new(rtma::Rtma), "2dvaranl_ndfd", file),
        now: t(6, 18, 0),
        cycle: true,
    }
}

fn hrrr_case() -> Case {
    let mem = testing::remote(&[hrrr::BUCKET]);
    let (file, idx) = testing::model_file(&[
        (
            "TMP",
            "2 m above ground",
            "6 hour fcst",
            "hrrr_t10z_f06__TMP_2mabove.grib2",
        ),
        (
            "TCDC",
            "entire atmosphere",
            "6 hour fcst",
            "hrrr_t10z_f06__TCDC_entire.grib2",
        ),
        (
            "ASNOW",
            "surface",
            "0-6 hour acc fcst",
            "hrrr_t10z_f06__ASNOW_surface.grib2",
        ),
    ]);
    let run = t(6, 12, 0);
    let key = hrrr::file_key(
        hrrr::layout("conus").unwrap(),
        run,
        wx_core::families::hrrr::SURFACE,
        6,
    );
    let size = file.len() as u64;
    let bucket = S3Bucket::aws(hrrr::BUCKET);
    mem.put_s3(&bucket, &key, file);
    mem.put_s3(&bucket, &format!("{key}.idx"), idx);
    let (l, s) = (
        layer::layer("temp").unwrap(),
        crate::testing::source("hrrr"),
    );
    let job = PartJob::forecast(
        l,
        l.input_for(s).unwrap(),
        s,
        run,
        360,
        Some(300),
        Steps::RunTotals,
    )
    .unwrap();
    Case {
        name: "HRRR (CONUS, 1.9 M points)",
        mem,
        job,
        via: Via::Model(
            Arc::new(hrrr::Hrrr::new(18)),
            run,
            ModelFile {
                name: key,
                size: Some(size),
            },
        ),
        now: t(6, 14, 0),
        cycle: true,
    }
}

const REFL: &str = "MergedReflectivityQCComposite_00.50";

fn mrms_case() -> Case {
    let mem = testing::mrms_remote();
    let at = Utc.with_ymd_and_hms(2026, 11, 6, 11, 48, 38).unwrap();
    let key = format!(
        "{}MRMS_{REFL}_20261106-114838.grib2.gz",
        mrms::dir("CONUS", REFL, at.date_naive())
    );
    let gz = testing::fixture("MRMS_MergedReflectivityQCComposite_00.50_20261106-114838.grib2.gz");
    let size = gz.len() as u64;
    mem.put_s3(&S3Bucket::aws(mrms::BUCKET), &key, gz);
    let valid = t(6, 11, 48);
    let file = ObsFile {
        valid,
        name: key,
        size: Some(size),
    };
    Case {
        name: "MRMS (CONUS composite, 24.5 M points)",
        mem,
        job: job_observed("radar", "mrms", valid),
        via: Via::Observed(Arc::new(mrms::Mrms), REFL, file),
        now: t(6, 11, 52),
        cycle: true,
    }
}

const OPERA_BUCKET: &str = "https://opera.example.org/openradar-24h";

fn opera_case() -> Case {
    let mem = MemoryRemote::new();
    let bucket = S3Bucket::at(OPERA_BUCKET);
    mem.serve_s3(&bucket, 1000);
    let valid = t(6, 18, 15);
    let key = opera::key("COMP", "DBZH", valid);
    mem.put_s3(
        &bucket,
        &key,
        testing::fixture("formats/opera_20261106T1815_DBZH.h5"),
    );
    let file = ObsFile {
        valid,
        name: key,
        size: None,
    };
    Case {
        name: "OPERA DBZH (16.7 M points)",
        mem,
        job: job_observed("radar", "opera", valid),
        via: Via::Observed(Arc::new(opera::Opera::at(OPERA_BUCKET)), "DBZH", file),
        now: t(6, 18, 22),
        cycle: true,
    }
}

const DWD_BASE: &str = "https://dwd.example.org/weather/radar/composite";

fn dwd_case() -> Case {
    let mem = MemoryRemote::new();
    let valid = t(6, 18, 15);
    let url = format!(
        "{}{}",
        dwd_radar::dir_url(DWD_BASE, "hx"),
        dwd_radar::obs_name("hx", valid)
    );
    mem.put(
        &url,
        testing::fixture("formats/dwd_composite_hx_20261106_1815-hd5"),
    );
    let file = ObsFile {
        valid,
        name: url,
        size: None,
    };
    Case {
        name: "DWD hx (250 m composite)",
        mem,
        job: job_observed("radar", "dwd_radar", valid),
        via: Via::Observed(Arc::new(dwd_radar::DwdRadar::at(DWD_BASE)), "hx", file),
        now: t(6, 18, 20),
        cycle: false,
    }
}

/// What `engine::build` does, stage by stage.
fn decode(job: &PartJob, payloads: &[Payload]) -> Vec<Field> {
    let needs = job.needs();
    if payloads.iter().all(|p| matches!(p, Payload::Grib(_))) {
        let mut grib = Vec::new();
        for p in payloads {
            if let Payload::Grib(b) = p {
                grib.extend_from_slice(&codec::inflate(b, codec::MAX_INFLATED).unwrap());
            }
        }
        let wanted: Vec<FieldSel> = needs.iter().filter_map(|n| n.field).collect();
        return wx_pipeline::decode(&grib, &wanted).unwrap().fields;
    }
    needs
        .iter()
        .zip(payloads)
        .map(|(n, p)| match p {
            Payload::Custom { bytes, decode } => decode(bytes, n).unwrap().field,
            Payload::Grib(_) => unreachable!(),
        })
        .collect()
}

/// Where the time goes for one part, and the heap `engine::build` peaks at.
async fn part_path(case: Case) {
    let (engine, store) = testing::engine(&case.mem);
    let cx = Cx::new(&engine.remote, "bench", case.now);
    let mut fetch = Vec::new();
    let mut payloads = Vec::new();
    for _ in 0..RUNS {
        let t = Instant::now();
        payloads = case.fetch(&cx).await;
        fetch.push(ms(t));
    }
    let fetch = median(fetch);
    let fetched: usize = payloads
        .iter()
        .map(|p| match p {
            Payload::Grib(b) | Payload::Custom { bytes: b, .. } => b.len(),
        })
        .sum();
    let job = &case.job;
    let (decode_ms, _) = timed(|| decode(job, &payloads));
    // The heap each stage takes on top of what it is handed.
    let stage = |f: &mut dyn FnMut()| {
        let base = heap::live();
        heap::reset_peak();
        f();
        mb(heap::peak() - base)
    };
    let decode_peak = stage(&mut || drop(decode(job, &payloads)));
    let fields = decode(job, &payloads);
    let derive_peak =
        stage(&mut || drop(wx_pipeline::derive(job.layer, job.input, &fields).unwrap()));
    let points: usize = fields.iter().map(|f| f.values.len()).max().unwrap_or(0);
    let (derive_ms, _) = timed(|| wx_pipeline::derive(job.layer, job.input, &fields).unwrap());
    let raster = wx_pipeline::derive(job.layer, job.input, &fields).unwrap();
    let (grid_ms, _) = timed(|| wx_pipeline::FrameGrid::build(job.layer, &raster));
    let grid_peak = stage(&mut || drop(wx_pipeline::FrameGrid::build(job.layer, &raster)));
    let grid = wx_pipeline::FrameGrid::build(job.layer, &raster);
    let (bytes_ms, _) = timed(|| grid.to_bytes());
    let in_memory = (
        mb(fields.iter().map(|f| f.values.len() * 4).sum()),
        mb(grid.approx_bytes()),
    );
    let stored = grid.to_bytes();
    drop((fields, raster, grid));
    let (build_ms, _) = timed(|| engine::build(job, payloads.clone()).unwrap());
    let key = Path::from(wx_core::keys::grid(job.layer.id, &job.id));
    let mut put = Vec::new();
    for _ in 0..RUNS {
        let body = PutPayload::from(stored.clone());
        let t = Instant::now();
        store.put(&key, body).await.unwrap();
        put.push(ms(t));
    }
    let mut insert = Vec::new();
    for _ in 0..RUNS {
        let catalog = FrameCatalog::new(Hub::new());
        let built = engine::build(job, payloads.clone()).unwrap();
        let part = match job.run {
            None => Part::observed(
                job.source.id,
                job.valid,
                built.bounds,
                built.solid,
                job.source.max_zoom,
            ),
            Some(run) => Part::forecast(
                job.source.id,
                run,
                job.lead,
                built.bounds,
                built.solid,
                job.source.max_zoom,
            ),
        };
        let t = Instant::now();
        catalog.insert(job.layer.id, part, case.now).unwrap();
        insert.push(ms(t));
    }
    // The heap engine::build takes on top of its payloads.
    let base = heap::live();
    heap::reset_peak();
    let built = engine::build(job, payloads.clone()).unwrap();
    let peak = heap::peak() - base;
    drop(built);
    drop(payloads);
    // A whole cycle from a fresh engine: list, fetch, build, store, insert.
    let mut cycle = Vec::new();
    let mut produced = 0;
    for _ in 0..if case.cycle { RUNS.min(5) } else { 0 } {
        let (engine, _) = testing::engine(&case.mem);
        let mut task = case.cycle_task(engine);
        let t = Instant::now();
        produced = task.cycle(case.now).await.produced;
        cycle.push(ms(t));
    }
    println!(
        "{}: fetched {:.2} MB, {:.1} M points, stored {:.2} MB\n  fetch {fetch:.1} ms, decode {decode_ms:.1}, derive {derive_ms:.1}, \
         grid {grid_ms:.1}, serialise {bytes_ms:.1}, put {:.2}, insert {:.3}; engine::build {build_ms:.1} ms, heap peak {:.0} MB\n  \
         heap per stage: decode {decode_peak:.0} MB, derive {derive_peak:.0}, grid {grid_peak:.0}; fields {:.0} MB, grid {:.0} MB\n  \
         a cycle from cold ({produced} parts) {:.1} ms",
        case.name,
        mb(fetched),
        points as f64 / 1e6,
        mb(stored.len()),
        median(put),
        median(insert),
        mb(peak),
        in_memory.0,
        in_memory.1,
        if cycle.is_empty() {
            f64::NAN
        } else {
            median(cycle)
        },
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a benchmark"]
async fn part_paths() {
    for case in [
        rtma_case(),
        hrrr_case(),
        mrms_case(),
        opera_case(),
        dwd_case(),
    ] {
        part_path(case).await;
    }
}

/// A GOES-19 full disk at 2 km (5424 x 5424) on the fixture's fixed grid,
/// as `wx_formats::goes` would decode it, with off-disk cells missing.
fn goes_full_disk() -> Field {
    let cut = wx_formats::goes::decode(&testing::fixture(
        "formats/goes19_CMIPF_C13_s20263101810214_rows1200-1800_cols1800-2400.nc",
    ))
    .unwrap()
    .field;
    let GridProjection::Geostationary(g) = &cut.proj else {
        unreachable!()
    };
    let n = 5424;
    let grid = GeostationaryGrid::new(
        g.earth,
        g.height,
        g.sub_lon,
        g.sweep,
        g.x_scale,
        g.x_offset - 1800.0 * g.x_scale,
        g.y_scale,
        g.y_offset - 1200.0 * g.y_scale,
    );
    let mut values = vec![f32::NAN; n * n];
    for j in 0..n {
        for i in 0..n {
            let (lon, _) = grid.inverse(
                grid.x_offset + i as f64 * grid.x_scale,
                grid.y_offset + j as f64 * grid.y_scale,
            );
            if lon.is_finite() {
                values[j * n + i] = 200.0 + ((i * 7 + j * 13) % 900) as f32 * 0.1;
            }
        }
    }
    Field {
        nx: n,
        ny: n,
        proj: GridProjection::Geostationary(grid),
        values,
        grid_relative_winds: false,
    }
}

/// The GOES build from the decoded full disk on (the NetCDF decoder itself
/// is left out: the full-size file is not among the fixtures).
#[test]
#[ignore = "a benchmark"]
fn goes_part_path() {
    let field = goes_full_disk();
    let sat = *goes::satellite("noaa-goes19").unwrap();
    let job = job_observed("satellite", "goes19", t(6, 18, 10));
    let (crop_ms, _) =
        timed(|| goes::crop(field.clone(), &goes::Crop::of(&sat), goes::FACTOR).unwrap());
    let cropped = goes::crop(field.clone(), &goes::Crop::of(&sat), goes::FACTOR).unwrap();
    let (derive_ms, _) = timed(|| {
        wx_pipeline::derive(job.layer, job.input, std::slice::from_ref(&cropped)).unwrap()
    });
    let raster = wx_pipeline::derive(job.layer, job.input, std::slice::from_ref(&cropped)).unwrap();
    let (grid_ms, _) = timed(|| wx_pipeline::FrameGrid::build(job.layer, &raster));
    let grid = wx_pipeline::FrameGrid::build(job.layer, &raster);
    let (bytes_ms, _) = timed(|| grid.to_bytes());
    let stored = grid.to_bytes().len();
    drop((cropped, raster, grid));
    let base = heap::live();
    heap::reset_peak();
    let f = goes::crop(field, &goes::Crop::of(&sat), goes::FACTOR).unwrap();
    let raster = wx_pipeline::derive(job.layer, job.input, std::slice::from_ref(&f)).unwrap();
    drop(f);
    let grid = wx_pipeline::FrameGrid::build(job.layer, &raster);
    drop(raster);
    let bytes = grid.to_bytes();
    drop(grid);
    println!(
        "GOES-19 full disk (29.4 M points from the decoder): crop {crop_ms:.1} ms, derive {derive_ms:.1}, grid {grid_ms:.1}, \
         serialise {bytes_ms:.1}; stored {:.2} MB; heap peak over the decoded field {:.0} MB",
        mb(stored),
        mb(heap::peak() - base)
    );
    drop(bytes);
}

/// The CMORPH2 72-hour total from the real file repeated: packing a half
/// hour for the cache, summing 144, and the part.
#[test]
#[ignore = "a benchmark"]
fn cmorph_totals() {
    use crate::families::cmorph::{Rates, total};
    let d = wx_formats::cmorph::decode(&testing::fixture(
        "formats/CMORPH2_0.25deg-30min_202611061700.RT.nc",
    ))
    .unwrap();
    let (pack_ms, _) = timed(|| Rates::from_field(&d.field).unwrap());
    let r = Rates::from_field(&d.field).unwrap();
    let halves: Vec<&Rates> = vec![&r; 144];
    let (sum_ms, _) = timed(|| total(&halves, 144).unwrap());
    let job = job_observed("rain_72h", "cmorph", t(6, 17, 0));
    let f = total(&halves, 144).unwrap();
    let (build_ms, _) = timed(|| {
        let raster = wx_pipeline::derive(job.layer, job.input, std::slice::from_ref(&f)).unwrap();
        wx_pipeline::FrameGrid::build(job.layer, &raster).to_bytes()
    });
    println!(
        "CMORPH2: packing a half hour {pack_ms:.1} ms ({:.2} MB kept, 153 kept {:.0} MB), summing 144 {sum_ms:.1} ms, derive, grid and serialise {build_ms:.1} ms",
        mb(r.approx_bytes()),
        mb(153 * r.approx_bytes())
    );
}

/// GOES-19's full disk and GMGSI's full mosaic, decoder included, from the
/// research samples under `WX_SATELLITE_SAMPLES`
/// (`goes/CMIPF_C13_G19_s20263101810214.nc`, `gmgsi/GLOBCOMPLIR_...nc`).
#[test]
#[ignore = "needs the full-size research samples"]
fn research_samples() {
    let Some(dir) = std::env::var_os("WX_SATELLITE_SAMPLES").map(std::path::PathBuf::from) else {
        eprintln!("WX_SATELLITE_SAMPLES not set");
        return;
    };
    let sat = *goes::satellite("noaa-goes19").unwrap();
    let goes_file = std::fs::read(dir.join("goes/CMIPF_C13_G19_s20263101810214.nc")).unwrap();
    let gmgsi_file = std::fs::read(dir.join(
        "gmgsi/GLOBCOMPLIR_v3r0_blend_s202611061700000_e202611061709599_c202611061733476.nc",
    ))
    .unwrap();
    type Decode<'a> = Box<dyn Fn() -> Field + 'a>;
    let cases: [(&str, &str, &str, Decode); 2] = [
        (
            "GOES-19",
            "goes19",
            "satellite",
            Box::new(|| {
                let d = wx_formats::goes::decode(&goes_file).unwrap();
                goes::crop(d.field, &goes::Crop::of(&sat), goes::FACTOR).unwrap()
            }),
        ),
        (
            "GMGSI",
            "gmgsi",
            "satellite",
            Box::new(|| wx_formats::gmgsi::decode(&gmgsi_file).unwrap().field),
        ),
    ];
    for (name, source, layer, decode) in cases {
        let job = job_observed(layer, source, t(6, 17, 0));
        let (decode_ms, _) = timed(&decode);
        let base = heap::live();
        heap::reset_peak();
        let field = decode();
        let raster =
            wx_pipeline::derive(job.layer, job.input, std::slice::from_ref(&field)).unwrap();
        drop(field);
        let grid = wx_pipeline::FrameGrid::build(job.layer, &raster);
        drop(raster);
        let stored = grid.to_bytes().len();
        println!(
            "{name}: {:?} cells, decode {decode_ms:.0} ms, heap peak {:.0} MB, in memory {:.1} MB, stored {:.1} MB",
            grid.dimensions(),
            mb(heap::peak() - base),
            mb(grid.approx_bytes()),
            mb(stored)
        );
    }
}

/// One part and nothing else, for `/usr/bin/time -l`: fetched from the
/// in-memory remote, built, stored and inserted through the engine.
async fn peak(case: Case) {
    let (engine, _) = testing::engine(&case.mem);
    let cx = Cx::new(&engine.remote, "bench", case.now);
    let payloads = case.fetch(&cx).await;
    let base = heap::live();
    heap::reset_peak();
    let r = engine
        .produce(
            &case.job,
            payloads,
            "bench",
            engine::Moment::starting(case.now),
        )
        .await;
    assert!(matches!(r, Ok(engine::Produced::Inserted)), "{r:?}");
    println!(
        "{}: heap peak while producing {:.0} MB",
        case.name,
        mb(heap::peak() - base)
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a benchmark"]
async fn peak_mrms() {
    peak(mrms_case()).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a benchmark"]
async fn peak_opera() {
    peak(opera_case()).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a benchmark"]
async fn peak_dwd() {
    peak(dwd_case()).await;
}

#[test]
#[ignore = "a benchmark"]
fn peak_goes() {
    let field = goes_full_disk();
    let sat = *goes::satellite("noaa-goes19").unwrap();
    let job = job_observed("satellite", "goes19", t(6, 18, 10));
    let base = heap::live();
    heap::reset_peak();
    let f = goes::crop(field, &goes::Crop::of(&sat), goes::FACTOR).unwrap();
    let raster = wx_pipeline::derive(job.layer, job.input, std::slice::from_ref(&f)).unwrap();
    drop(f);
    let grid = wx_pipeline::FrameGrid::build(job.layer, &raster);
    drop(raster);
    let bytes = grid.to_bytes();
    drop(grid);
    println!(
        "GOES-19 full disk: heap peak from the decoded field {:.0} MB ({:.1} MB stored)",
        mb(heap::peak() - base),
        mb(bytes.len())
    );
}

/// Nothing at all, for the test binary's own footprint.
#[test]
#[ignore = "a benchmark"]
fn peak_baseline() {}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// Ten inputs of one product, fifty hourly leads a run, one file per lead;
/// nothing is fetched (the files are never listed).
struct Planned;

#[async_trait::async_trait]
impl Model for Planned {
    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::hours(6),
            spare: 0,
            relist: Duration::hours(1),
            give_up: Duration::minutes(30),
        }
    }

    fn leads(&self, _: &SourceDef, _: &str, _: DateTime<Utc>) -> Vec<u32> {
        (0..50).map(|h| h * 60).collect()
    }

    fn due(&self, _: &SourceDef, _: &str, run: DateTime<Utc>, lead: u32) -> DateTime<Utc> {
        run + Duration::hours(4) + Duration::minutes(i64::from(lead / 60))
    }

    fn file(&self, _: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        format!("{}/f{:03}", run.format("%Y%m%d%H"), need.lead / 60)
    }

    async fn list(
        &self,
        _: &Cx<'_>,
        _: &SourceDef,
        _: DateTime<Utc>,
        _: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        Ok(Some(Listing::new()))
    }

    async fn fetch(
        &self,
        _: &Cx<'_>,
        _: &SourceDef,
        _: DateTime<Utc>,
        _: &ModelFile,
        _: &[Need],
    ) -> Result<Fetched, FetchError> {
        Ok(Fetched::Missing)
    }
}

fn planned_registry() -> (&'static [LayerSpec], &'static SourceDef) {
    let layers = leak(
        (0..10)
            .map(|k| {
                let var: &'static str = Box::leak(format!("V{k}").into_boxed_str());
                let input = InputSpec {
                    family: Family::Gfs,
                    sources: &[],
                    product: "p",
                    fields: leak(vec![FieldSel {
                        var,
                        level: "sfc",
                        stat: Stat::Instant,
                    }]),
                    derive: Derive::Linear { mul: 1.0, add: 0.0 },
                    hold_minutes: 60,
                    fill_past: false,
                };
                LayerSpec {
                    id: Box::leak(format!("b{k}").into_boxed_str()),
                    inputs: leak(vec![input]),
                    anchored: false,
                    ..*layer::layer("temp").unwrap()
                }
            })
            .collect(),
    );
    let sources = leak(vec![SourceDef {
        id: "bench",
        family: Family::Gfs,
        region: "",
        kind: FrameKind::Forecast,
        rank: 100,
        max_zoom: 4,
        ..SOURCES[0]
    }]);
    (layers, &sources[0])
}

/// A cycle over runs of 50 leads and 10 inputs: with every part wanted and
/// no file out yet (planning alone), and with every part on the catalog
/// (the common case: nothing is new).
#[tokio::test]
#[ignore = "a benchmark"]
async fn planning() {
    let (layers, source) = planned_registry();
    let mem = MemoryRemote::new();
    let engine = testing::engine_with(&mem, Arc::new(InMemory::new()), layers, leak(vec![*source]));
    let mut task = ModelTask::new(engine.clone(), Arc::new(Planned), source, &["p"]);
    let now = t(6, 12, 30);
    task.cycle(now, &|_, _| {}).await.unwrap();
    let runs = Planned.cycle(source).runs(
        now,
        Duration::minutes(wx_core::frame::FORECAST_GRACE_MINUTES),
    );
    let jobs: usize = runs.iter().map(|r| task.wanted(*r, now).len()).sum();
    let mut times = Vec::new();
    let mut allocs = 0;
    for _ in 0..RUNS * 3 {
        let (n, t) = (heap::count(), Instant::now());
        task.cycle(now, &|_, _| {}).await.unwrap();
        times.push(ms(t));
        allocs = heap::count() - n;
    }
    println!(
        "planning {} runs, {jobs} parts wanted, nothing out yet: {:.0} us a cycle, {allocs} allocations",
        runs.len(),
        median(times) * 1e3
    );
    // Every part on the catalog.
    for run in &runs {
        for l in layers {
            for lead in (0..50).map(|h| h * 60) {
                let part = Part::forecast(
                    source.id,
                    *run,
                    lead,
                    wx_core::LonLatBox::new(-10.0, 40.0, 10.0, 55.0),
                    None,
                    4,
                );
                let _ = engine.catalog.insert(l.id, part, now);
            }
        }
    }
    let mut times = Vec::new();
    for _ in 0..RUNS * 3 {
        let (n, t) = (heap::count(), Instant::now());
        task.cycle(now, &|_, _| {}).await.unwrap();
        times.push(ms(t));
        allocs = heap::count() - n;
    }
    let report = task.cycle(now, &|_, _| {}).await.unwrap();
    let (n, t) = (heap::count(), Instant::now());
    let summary = task.summary(&report, now);
    let summary_us = ms(t) * 1e3;
    println!(
        "nothing new (every part on the catalog): {:.0} us a cycle, {allocs} allocations; health line {summary_us:.0} us, {} allocations ({})",
        median(times) * 1e3,
        heap::count() - n,
        summary.1
    );
}

// ---------------------------------------------------------------------------
// Listings and inventories
// ---------------------------------------------------------------------------

/// An Apache index of `n` files, as ECCC's step directories are.
fn apache_index(n: usize) -> String {
    let mut html = String::from(
        "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 3.2 Final//EN\">\n<html>\n <head>\n  <title>Index of /x/</title>\n </head>\n <body>\n<h1>Index of /x/</h1>\n<pre><img src=\"/icons/blank.gif\" alt=\"Icon \"> <a href=\"?C=N;O=D\">Name</a>                    <a href=\"?C=M;O=A\">Last modified</a>      <a href=\"?C=S;O=A\">Size</a><hr><img src=\"/icons/back.gif\" alt=\"[PARENTDIR]\"> <a href=\"/\">Parent Directory</a>                             -   \n",
    );
    for k in 0..n {
        let name = format!("20261106T12Z_MSC_HRDPS_VAR{k:03}_AGL-10m_RLatLon0.0225_PT003H.grib2");
        html.push_str(&format!(
            "<img src=\"/icons/unknown.gif\" alt=\"[   ]\"> <a href=\"{name}\">{name}</a>  2026-11-06 15:{:02}  598K  \n",
            k % 60
        ));
    }
    html.push_str("<hr></pre>\n</body></html>\n");
    html
}

/// An nginx index of `n` files, as DWD's composite directories are.
fn nginx_index(n: usize) -> String {
    let mut html = String::from(
        "<html>\r\n<head><title>Index of /weather/radar/composite/hx/</title></head>\r\n<body>\r\n<h1>Index of /weather/radar/composite/hx/</h1><hr><pre><a href=\"../\">../</a>\r\n",
    );
    for k in 0..n {
        let v = t(5, 0, 0) + Duration::minutes(5 * k as i64);
        let name = dwd_radar::obs_name("hx", v);
        html.push_str(&format!(
            "<a href=\"{name}\">{name}</a>                     {}              990978\r\n",
            v.format("%d-%b-%Y %H:%M:%S")
        ));
    }
    html.push_str("</pre><hr></body>\r\n</html>\r\n");
    html
}

/// A `ListObjectsV2` page of `n` keys.
fn s3_page(n: usize) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>noaa-mrms-pds</Name><Prefix>CONUS/</Prefix><IsTruncated>false</IsTruncated>",
    );
    for k in 0..n {
        xml.push_str(&format!(
            "<Contents><Key>CONUS/{REFL}/20261106/MRMS_{REFL}_20261106-{:02}{:02}38.grib2.gz</Key><LastModified>2026-11-06T11:51:18.000Z</LastModified><ETag>\"e{k}\"</ETag><Size>1283991</Size><StorageClass>STANDARD</StorageClass></Contents>",
            k / 30,
            (k * 2) % 60
        ));
    }
    xml.push_str("</ListBucketResult>");
    xml
}

#[test]
#[ignore = "a benchmark"]
fn listings_and_inventories() {
    let eccc = apache_index(400);
    let (us, allocs) = timed(|| {
        list::parse_html_index(&eccc, "https://dd.weather.gc.ca/20261106/WXO-DD/x/12/003/")
    });
    println!(
        "Apache index, 400 files ({} kB): {:.0} us, {allocs} allocations",
        eccc.len() / 1000,
        us * 1e3
    );
    let dwd = nginx_index(576);
    let (us, allocs) = timed(|| {
        list::parse_html_index(&dwd, "https://opendata.dwd.de/weather/radar/composite/hx/")
    });
    println!(
        "nginx index, 576 files ({} kB): {:.0} us, {allocs} allocations",
        dwd.len() / 1000,
        us * 1e3
    );
    let icon = String::from_utf8(testing::fixture("icon/listing_icon-d2_18_t_2m.html")).unwrap();
    let (us, _) = timed(|| {
        list::parse_html_index(
            &icon,
            "https://opendata.dwd.de/weather/nwp/icon-d2/grib/18/t_2m/",
        )
    });
    println!(
        "ICON-D2 index fixture ({} kB): {:.0} us",
        icon.len() / 1000,
        us * 1e3
    );
    let page = s3_page(1000);
    let (us, allocs) = timed(|| list::parse_s3_page(&page).unwrap());
    println!(
        "S3 page, 1000 keys ({} kB): {:.0} us, {allocs} allocations",
        page.len() / 1000,
        us * 1e3
    );
    for name in [
        "hrrr.t10z.wrfsfcf06.grib2.idx",
        "gfs.t06z.pgrb2.0p25.f024.idx",
        "cptec/cptec_t00z_f012.inv",
        "nbm/nbm_t18z_f006_co.idx",
    ] {
        let text = String::from_utf8(testing::fixture(name)).unwrap();
        let (us, allocs) = timed(|| idx::parse(&text));
        let lines = idx::parse(&text).len();
        println!(
            "inventory {name} ({lines} records): {:.0} us, {allocs} allocations",
            us * 1e3
        );
    }
    let text = String::from_utf8(testing::fixture("nbm/nbm_t18z_f006_co.idx")).unwrap();
    let (us, allocs) = timed(|| nbm::deterministic(&text));
    println!(
        "NBM deterministic lines of the CONUS inventory: {:.0} us, {allocs} allocations",
        us * 1e3
    );
}

/// AROME's walk of a 100-message file: one ranged GET of each message's head.
#[tokio::test]
#[ignore = "a benchmark"]
async fn arome_walk() {
    let group = testing::fixture("arome/arome0025_t12z_SP1_07H12H__tp_tcc_tsnowp_tgrp_7.grib2");
    let per = wx_pipeline::scan(&group).unwrap().len();
    let file: Vec<u8> = std::iter::repeat_n(group, 100usize.div_ceil(per))
        .flatten()
        .collect();
    let size = file.len() as u64;
    let messages = wx_pipeline::scan(&file).unwrap().len();
    let mem = MemoryRemote::new();
    let bucket = S3Bucket::at(arome::HOST);
    mem.serve_s3(&bucket, 1000);
    let run = t(6, 12, 0);
    let l = arome::layout("EURW1S").unwrap();
    let key = arome::file_key(&l, run, arome::PRODUCT, "SP1", 7);
    mem.put_s3(&bucket, &key, file);
    let (engine, _) = testing::engine(&mem);
    let s = crate::testing::source("arome_fr");
    let (rain, input) = crate::driver::inputs_of(wx_core::LAYERS, s)
        .into_iter()
        .find(|(_, i)| i.fields.iter().any(|f| f.var == "tp"))
        .unwrap();
    let job = PartJob::forecast(rain, input, s, run, 420, Some(360), Steps::RunTotals).unwrap();
    let needs = job.needs();
    let mut times = Vec::new();
    let mut requests = 0;
    for _ in 0..RUNS {
        let adapter = arome::Arome::walking();
        mem.clear_log();
        let cx = Cx::new(&engine.remote, "bench", t(6, 15, 0));
        let t = Instant::now();
        let got = adapter
            .fetch(
                &cx,
                s,
                run,
                &ModelFile {
                    name: key.clone(),
                    size: Some(size),
                },
                &needs,
            )
            .await
            .unwrap();
        times.push(ms(t));
        assert!(matches!(got, Fetched::Got(_)));
        requests = mem.requests().len();
    }
    println!(
        "AROME walk of {messages} messages ({:.1} MB): {:.1} ms, {requests} requests",
        mb(size as usize),
        median(times)
    );
}

// ---------------------------------------------------------------------------
// Cycles with nothing new
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "a benchmark"]
async fn idle_cycles() {
    let case = mrms_case();
    let (engine, _) = testing::engine(&case.mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(mrms::Mrms), case.job.source, REFL);
    assert_eq!(task.cycle(case.now).await.unwrap().tally.produced, 1);
    let mut times = Vec::new();
    let (mut allocs, mut requests) = (0, 0);
    for k in 0..RUNS {
        case.mem.clear_log();
        let (n, t) = (heap::count(), Instant::now());
        task.cycle(case.now + Duration::seconds(20 * (k as i64 + 1)))
            .await
            .unwrap();
        task.summary(0, 0);
        times.push(ms(t));
        allocs = heap::count() - n;
        requests = case.mem.requests().len();
    }
    println!(
        "MRMS radar, nothing new: {:.0} us a cycle and health line, {allocs} allocations, {requests} requests",
        median(times) * 1e3
    );
}

// ---------------------------------------------------------------------------
// The janitor
// ---------------------------------------------------------------------------

/// The orphan sweep over 10,000 grids on one layer, a tenth of them with
/// tiles, 9,500 held by the catalog.
#[tokio::test]
#[ignore = "a benchmark"]
async fn janitor_sweep() {
    let store = testing::stores::Counting::default();
    let catalog = FrameCatalog::new(Hub::new());
    let deletions = Arc::new(janitor::Deletions::default());
    let s = crate::testing::source("gfs");
    let temp = layer::layer("temp").unwrap();
    let now = t(6, 12, 0);
    let mut ids = Vec::new();
    for run in 0..50 {
        for lead in 0..200 {
            let run_at = now - Duration::days(30) + Duration::hours(6 * run);
            ids.push(Part::forecast_id(s.id, run_at, lead * 60));
        }
    }
    for (k, id) in ids.iter().enumerate() {
        store
            .put(
                &Path::from(wx_core::keys::grid("temp", id)),
                PutPayload::from_static(b"g"),
            )
            .await
            .unwrap();
        if k % 10 == 0 {
            for tile in 0..5 {
                let key = format!(
                    "{}6/{tile}/0.wxt",
                    wx_core::keys::part_tile_prefix("temp", id)
                );
                store
                    .put(&Path::from(key), PutPayload::from_static(b"t"))
                    .await
                    .unwrap();
            }
        }
    }
    // The catalog holds the newest parts of each run it would take.
    let mut held = 0;
    for run in 0..4 {
        let run_at = now - Duration::hours(6 * run);
        for lead in 0..200 {
            let part = Part::forecast(
                s.id,
                run_at,
                lead * 60,
                wx_core::LonLatBox::new(-180.0, -90.0, 180.0, 90.0),
                None,
                4,
            );
            if catalog
                .insert("temp", part, now)
                .is_ok_and(|i| matches!(i, wx_core::Insert::Added { .. }))
            {
                held += 1;
            }
        }
    }
    let mut times = Vec::new();
    let (mut lists, mut allocs) = (0, 0);
    for _ in 0..RUNS {
        store.take_lists();
        let (n, t0) = (heap::count(), Instant::now());
        // Nothing is old enough: everything is listed and judged, nothing deleted.
        let removed = janitor::reconcile(
            &store,
            &catalog,
            &deletions,
            std::slice::from_ref(temp),
            now,
            Duration::days(365),
        )
        .await
        .unwrap();
        times.push(ms(t0));
        assert!(removed.is_empty());
        lists = store.take_lists();
        allocs = heap::count() - n;
    }
    println!(
        "orphan sweep, {} grids, {} with tiles, {held} held: {:.1} ms, {lists} listings, {allocs} allocations",
        ids.len(),
        ids.len() / 10,
        median(times)
    );
}
