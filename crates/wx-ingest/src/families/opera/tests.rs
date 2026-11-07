use super::*;
use crate::driver::observed::{ObservedTask, next_due};
use crate::remote::memory::MemoryRemote;
use crate::testing;
use std::sync::Arc;

/// A host the test remote does not space requests on.
const TEST_BUCKET: &str = "https://opera.example.org/openradar-24h";
const DBZH: &str = "DBZH";
const ACRR: &str = "ACRR";

use crate::testing::at as t;

fn opera() -> &'static SourceDef {
    crate::testing::source("opera")
}

fn remote() -> Arc<MemoryRemote> {
    let mem = MemoryRemote::new();
    mem.serve_s3(&S3Bucket::at(TEST_BUCKET), 1000);
    mem
}

fn url(product: &str, v: DateTime<Utc>) -> String {
    S3Bucket::at(TEST_BUCKET).url(&key("COMP", product, v))
}

fn task(engine: Arc<crate::engine::Engine>, product: &'static str) -> ObservedTask {
    ObservedTask::new(engine, Arc::new(Opera::at(TEST_BUCKET)), opera(), product)
}

#[test]
fn keys_and_times() {
    let k = "2026/11/06/OPERA/COMP/OPERA@20261106T1815@0@DBZH.h5";
    assert_eq!(key("COMP", DBZH, t(18, 15)), k);
    assert_eq!(parse_key(DBZH, k), Some(t(18, 15)));
    assert_eq!(parse_key(ACRR, k), None);
    assert_eq!(parse_key(DBZH, &k.replace(".h5", ".tiff")), None);
    assert_eq!(parse_key(DBZH, &k.replace("T1815", "T1875")), None);
    assert_eq!(parse_key(DBZH, ""), None);
    // `start-after` takes every product of the time it names, nothing earlier.
    let before = key_before("COMP", t(18, 15));
    assert!(before.as_str() < k && k.starts_with(&before));
    assert!(before.as_str() > key("COMP", ACRR, t(18, 0)).as_str());
    assert!(key_before("COMP", t(18, 20)).as_str() > k);
    assert_eq!(dir("COMP", t(0, 0).date_naive()), "2026/11/06/OPERA/COMP/");
    // The bucket escapes `@`.
    assert!(url(DBZH, t(18, 15)).ends_with("OPERA%4020261106T1815%400%40DBZH.h5"));
    let o = Opera::default();
    assert!(o.check(opera()).is_ok());
    let mut s = *opera();
    s.region = "../x";
    assert!(o.check(&s).is_err());
}

#[test]
fn polls_when_the_next_file_is_due() {
    let radar = cadence(DBZH);
    assert_eq!(radar.period, Duration::minutes(5));
    assert_eq!(
        next_due(&radar, Some(t(18, 15))),
        Some(t(18, 24) + Duration::seconds(30))
    );
    let rain = cadence(ACRR);
    assert_eq!(rain.period, Duration::minutes(15));
    assert_eq!(
        next_due(&rain, Some(t(18, 0))),
        Some(t(18, 27) + Duration::seconds(30))
    );
}

/// The listing starts after the oldest time wanted and keeps only the
/// product's HDF5 files.
#[tokio::test]
async fn newest_files_are_found_with_one_small_listing() {
    let mem = remote();
    let bucket = S3Bucket::at(TEST_BUCKET);
    for m in [0, 5, 10, 15, 20] {
        for p in [DBZH, "RATE", ACRR] {
            if p == DBZH || m % 15 == 0 {
                mem.put(&url(p, t(18, m)), &b"x"[..]);
                mem.put(
                    &bucket.url(&key("COMP", p, t(18, m)).replace(".h5", ".tiff")),
                    &b"x"[..],
                );
            }
        }
    }
    let remote = testing::quick_remote(&mem);
    let cx = Cx::new(&remote, "test", t(18, 25));
    let o = Opera::at(TEST_BUCKET);
    let files = o.list(&cx, opera(), DBZH, t(18, 10)).await.unwrap();
    let times: Vec<_> = files.iter().map(|f| f.valid).collect();
    assert_eq!(times, [t(18, 10), t(18, 15), t(18, 20)]);
    assert!(
        files
            .iter()
            .all(|f| f.name.ends_with("@DBZH.h5") && f.size == Some(1))
    );
    let rain = o.list(&cx, opera(), ACRR, t(18, 0)).await.unwrap();
    assert_eq!(
        rain.iter().map(|f| f.valid).collect::<Vec<_>>(),
        [t(18, 0), t(18, 15)]
    );
    let lists = mem.requests();
    assert_eq!(lists.len(), 2, "{lists:?}");
    assert!(
        lists[0].contains("start-after=2026%2F11%2F06%2FOPERA%2FCOMP%2FOPERA%4020261106T1810"),
        "{}",
        lists[0]
    );
}

/// A composite with no new file for half an hour is a failing task, not a
/// quiet one.
#[tokio::test]
async fn a_stopped_composite_fails_the_task() {
    let mem = remote();
    mem.put(&url(DBZH, t(18, 15)), &b"x"[..]);
    let remote = testing::quick_remote(&mem);
    let o = Opera::at(TEST_BASE_FOR_STALE);
    let cx = Cx::new(&remote, "test", t(18, 40));
    assert_eq!(o.list(&cx, opera(), DBZH, t(18, 5)).await.unwrap().len(), 1);
    let cx = Cx::new(&remote, "test", t(18, 55));
    let e = o.list(&cx, opera(), DBZH, t(18, 5)).await.unwrap_err();
    assert!(e.to_string().contains("stopped"), "{e}");
}

const TEST_BASE_FOR_STALE: &str = TEST_BUCKET;

/// The real 18:15 composite becomes `opera-2611061815`; then a poll with
/// nothing new costs one listing.
#[tokio::test]
async fn a_listed_composite_becomes_a_part() {
    let mem = remote();
    mem.put(
        &url(DBZH, t(18, 15)),
        testing::fixture("formats/opera_20261106T1815_DBZH.h5"),
    );
    let (engine, store) = testing::engine(&mem);
    let mut task = task(engine.clone(), DBZH);
    assert_eq!(task.name, "opera.radar");
    let report = task.cycle(t(18, 20)).await.unwrap();
    assert_eq!(report.tally.produced, 1, "{:?}", report.tally);
    let part = engine.catalog.latest("radar", "opera").unwrap();
    assert_eq!(part.id, "opera-2611061815");
    assert_eq!(part.max_zoom, opera().max_zoom);
    // The radars' reach, from Iceland to Romania, Malta to the North Cape.
    let b = part.bounds;
    assert!(
        b.west < -20.0 && b.east > 30.0 && b.south < 36.0 && b.north > 70.0,
        "{b:?}"
    );
    assert!(
        testing::keys(store.as_ref())
            .await
            .contains(&wx_core::keys::grid("radar", &part.id))
    );
    mem.clear_log();
    let again = task.cycle(t(18, 21)).await.unwrap();
    assert_eq!(again.tally, Default::default());
    let r = mem.requests();
    assert!(r.len() == 1 && r[0].contains("list-type=2"), "{r:?}");
}

#[tokio::test]
async fn the_hourly_accumulation_feeds_rain_1h() {
    let mem = remote();
    mem.put(
        &url(ACRR, t(18, 0)),
        testing::fixture("formats/opera_20261106T1800_ACRR.h5"),
    );
    let (engine, _) = testing::engine(&mem);
    let mut task = task(engine.clone(), ACRR);
    assert_eq!(task.name, "opera.rain_1h");
    let report = task.cycle(t(18, 14)).await.unwrap();
    assert_eq!(report.tally.produced, 1, "{:?}", report.tally);
    assert_eq!(
        engine.catalog.latest("rain_1h", "opera").unwrap().id,
        "opera-2611061800"
    );
    assert!(engine.catalog.latest("radar", "opera").is_none());
}

/// A DBZH file standing where ACRR should be is refused, not shown as rain.
#[tokio::test]
async fn a_file_of_the_wrong_product_is_charged() {
    let mem = remote();
    mem.put(
        &url(ACRR, t(18, 0)),
        testing::fixture("formats/opera_20261106T1815_DBZH.h5"),
    );
    let (engine, _) = testing::engine(&mem);
    let report = task(engine.clone(), ACRR).cycle(t(18, 14)).await.unwrap();
    assert_eq!(
        (report.tally.produced, report.tally.failed),
        (0, 1),
        "{:?}",
        report.tally
    );
    assert!(engine.catalog.latest("rain_1h", "opera").is_none());
}

#[tokio::test]
#[ignore = "network"]
async fn live_opera_newest_files() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let o = Opera::default();
    for product in [DBZH, ACRR] {
        let files = o
            .list(&cx, opera(), product, now - Duration::hours(1))
            .await
            .unwrap();
        let newest = files
            .iter()
            .max_by_key(|f| f.valid)
            .expect("a file in the last hour");
        eprintln!(
            "{product}: {} files, newest {} ({:?} bytes)",
            files.len(),
            newest.valid,
            newest.size
        );
        assert!(now - newest.valid < Duration::minutes(30));
    }
    eprintln!("{:?}", remote.traffic());
}

/// Where the grids of a live run were stored (`ingest_once --out`).
pub(crate) fn live_store() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("WX_RADAR_STORE").map(std::path::PathBuf::from);
    if dir.is_none() {
        eprintln!("WX_RADAR_STORE not set: skipped");
    }
    dir
}

/// The newest stored radar grid of `source` valid at one of `times` (all
/// times when empty), with its time.
pub(crate) fn stored(
    dir: &std::path::Path,
    source: &str,
    times: &[String],
) -> Option<(String, wx_pipeline::FrameGrid)> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join("grids/radar"))
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            n.strip_prefix(source)
                .and_then(|r| r.strip_prefix('-'))
                .is_some_and(|r| {
                    r.len() == "2611062100.wxg".len()
                        && (times.is_empty() || times.iter().any(|t| r.starts_with(t.as_str())))
                })
        })
        .collect();
    names.sort();
    let name = names.pop()?;
    let bytes = std::fs::read(dir.join("grids/radar").join(&name)).ok()?;
    let time = name[source.len() + 1..source.len() + 11].to_string();
    Some((time, wx_pipeline::FrameGrid::from_bytes(&bytes).unwrap()))
}

/// The times (`yymmddHHMM`) `source` has grids for.
pub(crate) fn stored_times(dir: &std::path::Path, source: &str) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir.join("grids/radar"))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| {
            let r = n
                .strip_prefix(source)?
                .strip_prefix('-')?
                .strip_suffix(".wxg")?;
            (r.len() == 10).then(|| r.to_string())
        })
        .collect();
    out.sort();
    out
}

/// Samples of grids rendered at zoom `z` over tiles `xs` by `ys`, as one
/// image (each tile's last row and column, shared with the next, dropped).
pub(crate) struct Mosaic {
    pub w: usize,
    pub h: usize,
    pub v: Vec<f32>,
}

pub(crate) fn mosaic(
    grid: &wx_pipeline::FrameGrid,
    z: u8,
    xs: std::ops::RangeInclusive<u32>,
    ys: std::ops::RangeInclusive<u32>,
) -> Mosaic {
    let band = wx_core::layer::layer("radar").unwrap().bands[0];
    let n = wx_tile::TILE_SAMPLES;
    let side = n - 1;
    let (cols, rows) = (xs.clone().count(), ys.clone().count());
    let (w, h) = (cols * side, rows * side);
    let mut v = vec![f32::NAN; w * h];
    for (ty, y) in ys.enumerate() {
        for (tx, x) in xs.clone().enumerate() {
            let Some(bytes) = grid.render(wx_core::TileCoord::new(z, x, y).unwrap()) else {
                continue;
            };
            let tile = wx_tile::decode(&bytes).unwrap();
            for r in 0..side {
                for c in 0..side {
                    let q = tile.bands[0][r * n + c];
                    v[(ty * side + r) * w + tx * side + c] = band.dequantize(q).unwrap_or(f32::NAN);
                }
            }
        }
    }
    Mosaic { w, h, v }
}

/// How two mosaics' echoes (`>= echo`) agree where both have data, with `b`
/// shifted by up to `max` samples each way: `(samples both see, echo samples
/// of a, overlap at no shift, best shift, overlap there)`, overlap as
/// intersection over union.
pub(crate) fn agreement(
    a: &Mosaic,
    b: &Mosaic,
    max: i32,
    echo: f32,
) -> (usize, usize, f64, (i32, i32), f64) {
    assert_eq!((a.w, a.h), (b.w, b.h));
    let iou = |dx: i32, dy: i32| {
        let (mut both, mut inter, mut union) = (0usize, 0usize, 0usize);
        for j in max..a.h as i32 - max {
            for i in max..a.w as i32 - max {
                let va = a.v[j as usize * a.w + i as usize];
                let vb = b.v[(j + dy) as usize * b.w + (i + dx) as usize];
                if va.is_nan() || vb.is_nan() {
                    continue;
                }
                both += 1;
                let (ea, eb) = (va >= echo, vb >= echo);
                inter += (ea && eb) as usize;
                union += (ea || eb) as usize;
            }
        }
        (
            both,
            if union == 0 {
                0.0
            } else {
                inter as f64 / union as f64
            },
        )
    };
    let (both, at0) = iou(0, 0);
    let echoes = a.v.iter().filter(|v| **v >= echo).count();
    let mut best = ((0, 0), at0);
    for dy in -max..=max {
        for dx in -max..=max {
            let (_, s) = iou(dx, dy);
            if s > best.1 + 1e-9 {
                best = ((dx, dy), s);
            }
        }
    }
    (both, echoes, at0, best.0, best.1)
}

/// Intersection over union of `a`'s and shifted `b`'s echoes, ignoring a
/// border of `margin` samples.
pub(crate) fn iou_at(a: &Mosaic, b: &Mosaic, dx: i32, dy: i32, echo: f32, margin: i32) -> f64 {
    let (mut inter, mut union) = (0usize, 0usize);
    for j in margin..a.h as i32 - margin {
        for i in margin..a.w as i32 - margin {
            let va = a.v[j as usize * a.w + i as usize];
            let vb = b.v[(j + dy) as usize * b.w + (i + dx) as usize];
            if va.is_nan() || vb.is_nan() {
                continue;
            }
            inter += (va >= echo && vb >= echo) as usize;
            union += (va >= echo || vb >= echo) as usize;
        }
    }
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// The grids of a live run, compared where two independent radar networks
/// see the same rain at the same time: OPERA and DWD over Germany, OPERA and
/// DPC over the Alps and northern Italy. A shifted or flipped grid shows as
/// a best match away from no shift. Samples at zoom 8 are about 0.4 km
/// apart at these latitudes.
#[test]
#[ignore = "needs a live run's store"]
fn live_positions_agree_across_sources() {
    let Some(dir) = live_store() else { return };
    for (other, z, xs, ys) in [
        ("dwd_radar", 8u8, 132..=141, 82..=89),
        ("dpc", 8u8, 132..=140, 88..=93),
    ] {
        let common: Vec<String> = stored_times(&dir, other)
            .into_iter()
            .filter(|t| stored_times(&dir, "opera").contains(t))
            .collect();
        let Some((time, b)) = stored(&dir, other, &common) else {
            eprintln!("{other}: no time in common with OPERA");
            continue;
        };
        let (_, a) = stored(&dir, "opera", std::slice::from_ref(&time)).unwrap();
        let ma = mosaic(&a, z, xs.clone(), ys.clone());
        let mb = mosaic(&b, z, xs, ys);
        for echo in [15.0, 25.0, 35.0] {
            let (both, echoes, at0, shift, best) = agreement(&ma, &mb, 8, echo);
            eprintln!(
                "OPERA vs {other} at {time}, >= {echo} dBZ: {both} samples seen by both, {echoes} OPERA echoes; \
                 overlap {at0:.3} unshifted, best {best:.3} at {shift:?} samples (x east, y south)"
            );
        }
        // Further out: the overlap at shifts of 10 to 40 samples east-west
        // and north-south, which a misplaced grid would peak at.
        let profile = |along_x: bool| {
            [-40, -20, -10, 0, 10, 20, 40]
                .map(|d| {
                    let (dx, dy) = if along_x { (d, 0) } else { (0, d) };
                    format!("{d}: {:.3}", iou_at(&ma, &mb, dx, dy, 25.0, 40))
                })
                .join(", ")
        };
        eprintln!(
            "OPERA vs {other}, >= 25 dBZ, shifted east: {}; south: {}",
            profile(true),
            profile(false)
        );
    }
}

/// Every source's newest stored grid of a live run, rendered as the tile
/// server would at a low zoom, written to `WX_RADAR_DUMP` as raw `f32`
/// samples with a line of metadata, for drawing against coastlines.
#[test]
#[ignore = "needs a live run's store"]
fn live_dump_rendered_grids() {
    let Some(dir) = live_store() else { return };
    let Some(out) = std::env::var_os("WX_RADAR_DUMP").map(std::path::PathBuf::from) else {
        eprintln!("WX_RADAR_DUMP not set: skipped");
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    let views: [(
        &str,
        u8,
        std::ops::RangeInclusive<u32>,
        std::ops::RangeInclusive<u32>,
    ); 7] = [
        ("opera", 4, 6..=10, 2..=6),
        ("dwd_radar", 6, 32..=35, 19..=22),
        ("dwd_nowcast", 6, 32..=35, 19..=22),
        ("dpc", 6, 32..=35, 22..=25),
        ("jma_radar", 5, 26..=29, 11..=14),
        ("jma_nowcast", 5, 26..=29, 11..=14),
        ("cwa", 6, 52..=54, 26..=28),
    ];
    for (source, z, xs, ys) in views {
        let mut names: Vec<String> = std::fs::read_dir(dir.join("grids/radar"))
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.strip_prefix(source).is_some_and(|r| r.starts_with('-')))
            .collect();
        names.sort();
        let Some(name) = names.pop() else { continue };
        let grid = wx_pipeline::FrameGrid::from_bytes(
            &std::fs::read(dir.join("grids/radar").join(&name)).unwrap(),
        )
        .unwrap();
        let m = mosaic(&grid, z, xs.clone(), ys.clone());
        let stem = name.trim_end_matches(".wxg");
        let bytes: Vec<u8> = m.v.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(out.join(format!("{stem}.f32")), bytes).unwrap();
        std::fs::write(
            out.join(format!("{stem}.txt")),
            format!(
                "{} {} {z} {} {} {}\n",
                m.w,
                m.h,
                xs.start(),
                ys.start(),
                stem
            ),
        )
        .unwrap();
        eprintln!("{stem}: {} x {} at zoom {z}", m.w, m.h);
    }
}

/// Two sources at one time over one view, dumped like
/// `live_dump_rendered_grids` does: `WX_RADAR_PAIR` is
/// `a,b,time,z,x0,x1,y0,y1`.
#[test]
#[ignore = "needs a live run's store"]
fn live_dump_a_pair() {
    let (Some(dir), Some(out), Some(pair)) = (
        live_store(),
        std::env::var_os("WX_RADAR_DUMP").map(std::path::PathBuf::from),
        std::env::var("WX_RADAR_PAIR").ok(),
    ) else {
        return;
    };
    let p: Vec<&str> = pair.split(',').collect();
    let n = |k: usize| p[k].parse::<u32>().unwrap();
    let (z, xs, ys) = (n(3) as u8, n(4)..=n(5), n(6)..=n(7));
    std::fs::create_dir_all(&out).unwrap();
    for source in [p[0], p[1]] {
        let (time, grid) = stored(&dir, source, &[p[2].to_string()]).unwrap();
        let m = mosaic(&grid, z, xs.clone(), ys.clone());
        let stem = format!("pair-{source}-{time}-z{z}");
        let bytes: Vec<u8> = m.v.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(out.join(format!("{stem}.f32")), bytes).unwrap();
        std::fs::write(
            out.join(format!("{stem}.txt")),
            format!("{} {} {z} {} {} {stem}\n", m.w, m.h, xs.start(), ys.start()),
        )
        .unwrap();
    }
}
