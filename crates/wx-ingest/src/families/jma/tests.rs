use super::*;
use crate::driver::model::ModelTask;
use crate::driver::observed::ObservedTask;
use crate::remote::memory::MemoryRemote;
use crate::testing;

/// A host the test remote does not space requests on.
const TEST_BASE: &str = "https://jma.example.org/bosai/jmatile/data/nowc";

use crate::testing::at as t;

fn radar() -> &'static SourceDef {
    crate::testing::source("jma_radar")
}

fn nowcast() -> &'static SourceDef {
    crate::testing::source("jma_nowcast")
}

/// A time list as JMA writes it, newest first.
fn times(targets: &[Target]) -> String {
    let entries: Vec<String> = targets
        .iter()
        .map(|t| {
            format!(
                "  {{\"basetime\": \"{}\", \"validtime\": \"{}\", \"elements\": [\"hrpns\", \"hrpns_nd\"]}}\n",
                stamp(t.base),
                stamp(t.valid)
            )
        })
        .collect();
    format!("[\n{}]\n", entries.join(" ,"))
}

fn rain_tile() -> Vec<u8> {
    testing::fixture("formats/jma_hrpns_20261106T1820_6_56_25.png")
}

fn empty_tile() -> Vec<u8> {
    testing::fixture("formats/jma_hrpns_20261106T1820_7_113_50_empty.png")
}

/// Every tile of `set` at `target`: the rain tile at `(rx, ry)`, the empty
/// one elsewhere, none at `hole`.
fn publish(
    mem: &MemoryRemote,
    set: &TileSet,
    target: Target,
    rain: (u32, u32),
    hole: Option<(u32, u32)>,
) {
    for (_, x, y) in set.tiles() {
        if Some((x, y)) == hole {
            continue;
        }
        let body = if (x, y) == rain {
            rain_tile()
        } else {
            empty_tile()
        };
        mem.put(&tile_url(TEST_BASE, target, set.z, x, y), body);
    }
}

/// Every tile of `set` at `target` dry (the empty tile).
fn publish_dry(mem: &MemoryRemote, set: &TileSet, target: Target) {
    for (_, x, y) in set.tiles() {
        mem.put(&tile_url(TEST_BASE, target, set.z, x, y), empty_tile());
    }
}

fn tile_centre(z: u8, x: u32, y: u32) -> (f64, f64) {
    let n = (1u64 << z) as f64;
    let lon = (x as f64 + 0.5) / n * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * (y as f64 + 0.5) / n))
        .sinh()
        .atan()
        .to_degrees();
    (lon, lat)
}

#[test]
fn tile_sets() {
    let n = tiles();
    assert_eq!(
        (n.z, n.x0, n.y0, n.cols, n.rows, n.count()),
        (6, 53, 22, 6, 7, 24)
    );
    assert_eq!(n.z % 2, 0, "odd zooms have no data");
    let all = n.tiles();
    assert_eq!(all[0], (3, 56, 22));
    assert!(
        all.contains(&(3 * 6 + 3, 56, 25)),
        "the fixture's tile is fetched"
    );
    // Tokyo, Naha and Sapporo are fetched.
    for (lon, lat) in [(139.69_f64, 35.69_f64), (127.68, 26.21), (141.35, 43.06)] {
        let k = 1u64 << n.z;
        let x = ((lon + 180.0) / 360.0 * k as f64) as u32;
        let y =
            ((1.0 - lat.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0 * k as f64) as u32;
        assert!(all.iter().any(|t| (t.1, t.2) == (x, y)), "{x}/{y}");
    }
    let target = Target {
        base: t(20, 30),
        valid: t(21, 30),
    };
    assert_eq!(
        tile_url(BASE, target, 6, 56, 25),
        "https://www.jma.go.jp/bosai/jmatile/data/nowc/20261106203000/none/20261106213000/surf/hrpns/6/56/25.png"
    );
    assert_eq!(lead_name(t(20, 30), 60), "20261106203000/20261106213000");
    // The registry's rates are the decoder's.
    let decoded: Vec<f32> = wx_formats::jma::CLASSES.iter().map(|c| c.1).collect();
    assert_eq!(decoded, reg::CLASS_MMH);
}

#[test]
fn packed_tiles_round_trip() {
    let set = TileSet::new(4, &[(14, 6, 7), (15, 7, 7)]);
    assert_eq!((set.cols, set.rows, set.count()), (2, 2, 3));
    let tiles = vec![
        (0, Slot::Tile(Bytes::from_static(b"ab"))),
        (2, Slot::Dry),
        (3, Slot::Tile(Bytes::from_static(b"c"))),
    ];
    let packed = pack(&set, &tiles);
    let back = unpack(&packed, 4).unwrap();
    assert_eq!(
        back,
        [
            Slot::Tile(&b"ab"[..]),
            Slot::Unknown,
            Slot::Dry,
            Slot::Tile(&b"c"[..])
        ]
    );
    assert!(unpack(&packed[..packed.len() - 1], 4).is_err());
    assert!(unpack(&packed, 3).is_err());
}

#[test]
fn time_lists() {
    let n1 = times(&[
        Target {
            base: t(20, 30),
            valid: t(20, 30),
        },
        Target {
            base: t(20, 25),
            valid: t(20, 25),
        },
    ]);
    let parsed = parse_targets(n1.as_bytes(), "hrpns").unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].valid, t(20, 30));
    assert!(parse_targets(n1.as_bytes(), "thns").unwrap().is_empty());
    assert!(parse_targets(b"{}", "hrpns").is_err());
    assert!(parse_targets(b"[{\"basetime\": \"2026\"}]", "hrpns").is_err());
}

/// The observed time list costs a 304 while nothing is new; a frame is 24
/// tiles, and one the server lacks is waited for, then left unknown.
#[tokio::test]
async fn a_frame_from_tiles_with_one_missing() {
    let mem = MemoryRemote::new();
    let now = Target {
        base: t(20, 30),
        valid: t(20, 30),
    };
    let before = Target {
        base: t(20, 25),
        valid: t(20, 25),
    };
    mem.put(&times_url(TEST_BASE, "N1"), times(&[now, before]));
    let set = tiles();
    publish(&mem, &set, now, (56, 25), Some((54, 25)));
    let (engine, store) = testing::engine(&mem);
    let mut task = ObservedTask::new(
        engine.clone(),
        Arc::new(JmaRadar::at(TEST_BASE)),
        radar(),
        reg::PRODUCT,
    );
    assert_eq!(task.name, "jma_radar.radar");
    // 20:30 lacks a tile: it waits. 20:25 has no tiles at all: not there,
    // not an error (it is not the newest).
    let r = task.cycle(t(20, 31)).await.unwrap();
    assert_eq!((r.tally.produced, r.tally.failed), (0, 0), "{:?}", r.tally);
    assert!(engine.catalog.latest("radar", "jma_radar").is_none());
    mem.clear_log();
    let r = task
        .cycle(t(20, 30) + HOLES_AFTER + Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!((r.tally.produced, r.tally.failed), (1, 0), "{:?}", r.tally);
    let part = engine.catalog.latest("radar", "jma_radar").unwrap();
    assert_eq!(part.id, "jma_radar-2611062030");
    // The coverage polygon's reach (120.5-149.0 E, 21.0-46.7 N).
    let b = part.bounds;
    assert!(
        b.west < 121.0 && b.east > 148.9 && b.south < 21.1 && b.north > 46.6,
        "{b:?}"
    );
    let grid = testing::stored_grid(store.as_ref(), "radar", &part.id).await;
    assert_eq!(grid.dimensions(), (6 * 256, 7 * 256));
    let at = |x, y| {
        let (lon, lat) = tile_centre(6, x, y);
        grid.coverage_at(lon, lat)
    };
    assert_eq!(at(56, 25), 1.0, "the rain tile");
    assert_eq!(at(55, 26), 1.0, "a dry tile");
    assert_eq!(at(54, 25), 0.0, "the missing tile");
    assert_eq!(at(53, 22), 0.0, "a tile not fetched, beyond the coverage");
    // Every tile of 20:30, the first of 20:25.
    let fetched = mem
        .requests()
        .iter()
        .filter(|r| r.ends_with(".png"))
        .count();
    assert_eq!(fetched, 24 + 1);

    // Nothing new: one conditional request, answered 304, and 20:25 looked
    // for again (the listing overlaps the last ten minutes) at one tile.
    mem.clear_log();
    let bytes = engine.remote.traffic()[0].bytes;
    let r = task.cycle(t(20, 42)).await.unwrap();
    assert_eq!(r.tally, Default::default());
    let (x, y) = (set.tiles()[0].1, set.tiles()[0].2);
    assert_eq!(
        mem.requests(),
        [
            times_url(TEST_BASE, "N1"),
            tile_url(TEST_BASE, before, 6, x, y)
        ]
    );
    assert_eq!(
        engine.remote.traffic()[0].bytes,
        bytes,
        "a 304 and a 404 have no body"
    );
}

fn nowcast_group() -> Vec<&'static str> {
    vec![reg::PRODUCT]
}

fn run_targets(run: DateTime<Utc>) -> Vec<Target> {
    (1..=12)
        .rev()
        .map(|k| Target {
            base: run,
            valid: run + Duration::minutes(5 * k),
        })
        .collect()
}

/// A run's 12 leads become 12 parts; the next run followed replaces every
/// part of the previous one for the times it covers.
#[tokio::test]
async fn a_new_nowcast_run_replaces_the_previous_one() {
    let mem = MemoryRemote::new();
    let set = tiles();
    let first = t(20, 20);
    mem.put(&times_url(TEST_BASE, "N2"), times(&run_targets(first)));
    mem.put(
        &times_url(TEST_BASE, "N1"),
        times(&analyses(&[t(20, 30), t(20, 25), t(20, 20)])),
    );
    for target in run_targets(first) {
        publish(&mem, &set, target, (56, 25), None);
    }
    let (engine, _) = testing::engine(&mem);
    let mut task = ModelTask::new(
        engine.clone(),
        Arc::new(JmaNowcast::at(TEST_BASE)),
        nowcast(),
        &nowcast_group(),
    );
    let r = task.cycle(t(20, 23), &|_, _| {}).await.unwrap();
    assert_eq!((r.tally.produced, r.tally.failed), (12, 0), "{:?}", r.tally);
    let parts = |engine: &crate::engine::Engine| {
        let mut p: Vec<(DateTime<Utc>, DateTime<Utc>)> = engine
            .catalog
            .timeline("radar")
            .parts
            .into_iter()
            .filter(|p| p.source == "jma_nowcast")
            .map(|p| (p.valid, p.run.unwrap()))
            .collect();
        p.sort();
        p
    };
    assert_eq!(parts(&engine).len(), 12);
    assert_eq!(
        engine.catalog.latest("radar", "jma_nowcast").unwrap().id,
        "jma_nowcast-2611062020-60"
    );
    let tiles = |base: DateTime<Utc>| {
        let run = format!("/{}/none/", stamp(base));
        mem.requests()
            .iter()
            .filter(|r| r.contains(&run) && r.ends_with(".png"))
            .count()
    };
    assert_eq!(tiles(first), 12 * 24);
    // The 20:10 run is not read: the analyses here do not include it.
    assert_eq!(tiles(t(20, 10)), 0);
    engine.catalog.take_dropped();

    // 20:25 is a run not followed; 20:30 is.
    let second = t(20, 30);
    mem.put(&times_url(TEST_BASE, "N2"), times(&run_targets(second)));
    for target in run_targets(second) {
        publish(&mem, &set, target, (56, 25), None);
    }
    let r = task.cycle(t(20, 33), &|_, _| {}).await.unwrap();
    assert_eq!(r.tally.produced, 12, "{:?}", r.tally);
    let now = parts(&engine);
    assert_eq!(now.len(), 14, "{now:?}");
    for (valid, run) in &now {
        let want = if *valid <= t(20, 30) { first } else { second };
        assert_eq!(*run, want, "{valid}");
    }
    let dropped = engine.catalog.take_dropped();
    assert_eq!(
        dropped
            .iter()
            .filter(|d| d.part.source == "jma_nowcast")
            .count(),
        10,
        "{dropped:?}"
    );
}

fn analyses(at: &[DateTime<Utc>]) -> Vec<Target> {
    at.iter().map(|&v| Target { base: v, valid: v }).collect()
}

/// In a forecast a tile that is not there is dry (JMA answers 404 for dry
/// forecast tiles); an older run is read only when its analysis exists,
/// and not at all beyond `OLDEST_RUN`.
#[tokio::test]
async fn forecast_tiles_not_there_are_dry() {
    let mem = MemoryRemote::new();
    let set = tiles();
    let run = t(20, 20);
    mem.put(&times_url(TEST_BASE, "N2"), times(&run_targets(run)));
    mem.put(
        &times_url(TEST_BASE, "N1"),
        times(&analyses(&[t(20, 20), t(20, 10), t(20, 5)])),
    );
    // Only the rain tile is there, at every lead; the 20:10 run has no
    // forecast tiles, and its analysis is dry.
    for target in run_targets(run) {
        mem.put(&tile_url(TEST_BASE, target, set.z, 56, 25), rain_tile());
    }
    publish_dry(
        &mem,
        &set,
        Target {
            base: t(20, 10),
            valid: t(20, 10),
        },
    );
    let (engine, store) = testing::engine(&mem);
    let mut task = ModelTask::new(
        engine.clone(),
        Arc::new(JmaNowcast::at(TEST_BASE)),
        nowcast(),
        &nowcast_group(),
    );
    let r = task.cycle(t(20, 23), &|_, _| {}).await.unwrap();
    // The run's 12 and the 20:10 run's two leads before it, all dry there.
    assert_eq!((r.tally.produced, r.tally.failed), (14, 0), "{:?}", r.tally);
    let part = engine.catalog.latest("radar", "jma_nowcast").unwrap();
    let grid = testing::stored_grid(store.as_ref(), "radar", &part.id).await;
    let at = |x, y| {
        let (lon, lat) = tile_centre(6, x, y);
        grid.coverage_at(lon, lat)
    };
    assert_eq!(
        (at(56, 25), at(57, 23), at(53, 22)),
        (1.0, 1.0, 0.0),
        "rain, dry, not fetched"
    );
    let requested = |base: DateTime<Utc>| {
        let run = format!("/{}/none/", stamp(base));
        mem.requests()
            .iter()
            .filter(|r| r.contains(&run) && r.ends_with(".png"))
            .count()
    };
    // The two leads and the analysis, once.
    assert_eq!((requested(t(20, 10)), requested(t(20, 0))), (3 * 24, 0));
}

#[tokio::test]
#[ignore = "network"]
async fn live_jma_time_lists_and_a_frame() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let j = JmaRadar::default();
    let files = j
        .list(&cx, radar(), reg::PRODUCT, now - Duration::hours(1))
        .await
        .unwrap();
    let newest = files
        .iter()
        .max_by_key(|f| f.valid)
        .expect("a frame in the last hour")
        .clone();
    eprintln!("N1: {} frames, newest {}", files.len(), newest.valid);
    assert!(now - newest.valid < Duration::minutes(15));
    // Asked again: answered 304.
    let again = j
        .list(&cx, radar(), reg::PRODUCT, now - Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(again.len(), files.len());
    eprintln!("{:?}", remote.traffic());
}

/// The stored `jma_radar` grid of a live run (zoom-6 tiles assembled)
/// against JMA's own zoom-8 tiles of the same time where it rains most,
/// both through the same derive and grid: a shift shows as a best match
/// away from no shift, in zoom-8 samples (about 0.5 km).
#[tokio::test]
#[ignore = "needs a live run's store and the network"]
async fn live_jma_grid_matches_its_own_zoom_8_tiles() {
    use crate::families::opera::tests::{agreement, live_store, mosaic, stored};
    let Some(dir) = live_store() else { return };
    let (time, ours) = stored(&dir, "jma_radar", &[]).expect("a stored jma_radar grid");
    let valid = NaiveDateTime::parse_from_str(&time, "%y%m%d%H%M")
        .unwrap()
        .and_utc();
    let set = tiles();
    let mut best = (0usize, 0u32, 0u32);
    for (_, x, y) in set.tiles() {
        for (x8, y8) in (0..16).map(|k| (4 * x + k % 4, 4 * y + k / 4)) {
            let m = mosaic(&ours, 8, x8..=x8, y8..=y8);
            let n = m.v.iter().filter(|v| **v >= 15.0).count();
            if n > best.0 {
                best = (n, x8, y8);
            }
        }
    }
    let (n, bx, by) = best;
    assert!(n > 0, "no rain over Japan at {time}");
    let remote = crate::testing::live::remote();
    let cx = Cx::new(&remote, "live", Utc::now());
    let target = Target { base: valid, valid };
    let mut tiles = Vec::new();
    for y in by - 1..=by + 1 {
        for x in bx - 1..=bx + 1 {
            tiles.push(
                cx.get(tile_url(BASE, target, 8, x, y))
                    .bytes()
                    .await
                    .unwrap(),
            );
        }
    }
    let refs: Vec<Option<&[u8]>> = tiles.iter().map(|t| t.as_deref()).collect();
    let field = wx_formats::jma::decode_tiles(8, bx - 1, by - 1, 3, 3, &refs).unwrap();
    let layer = wx_core::layer::layer("radar").unwrap();
    let raster = wx_pipeline::derive(layer, layer.input_for(radar()).unwrap(), &[field]).unwrap();
    let theirs = wx_pipeline::FrameGrid::build(layer, &raster);
    let a = mosaic(&ours, 8, bx - 1..=bx + 1, by - 1..=by + 1);
    let b = mosaic(&theirs, 8, bx - 1..=bx + 1, by - 1..=by + 1);
    for echo in [15.0, 30.0] {
        let (both, echoes, at0, shift, best) = agreement(&a, &b, 6, echo);
        eprintln!(
            "JMA zoom-6 grid vs zoom-8 tiles 8/{}..{}/{}..{} at {time}, >= {echo} dBZ: {both} samples, {echoes} echoes; \
             overlap {at0:.3} unshifted, best {best:.3} at {shift:?}",
            bx - 1,
            bx + 1,
            by - 1,
            by + 1
        );
    }
}

/// CWA and JMA (whose Yonaguni and Ishigaki radars reach Taiwan's east
/// coast) over the sea between them, at the newest time both have.
#[test]
#[ignore = "needs a live run's store"]
fn live_cwa_and_jma_agree_where_they_overlap() {
    use crate::families::opera::tests::{agreement, live_store, mosaic, stored, stored_times};
    let Some(dir) = live_store() else { return };
    let common: Vec<String> = stored_times(&dir, "cwa")
        .into_iter()
        .filter(|t| stored_times(&dir, "jma_radar").contains(t))
        .collect();
    let Some((time, cwa)) = stored(&dir, "cwa", &common) else {
        eprintln!("no time in common");
        return;
    };
    let (_, jma) = stored(&dir, "jma_radar", std::slice::from_ref(&time)).unwrap();
    let a = mosaic(&cwa, 8, 213..=216, 108..=112);
    let b = mosaic(&jma, 8, 213..=216, 108..=112);
    for echo in [15.0, 25.0, 35.0] {
        let (both, echoes, at0, shift, best) = agreement(&a, &b, 8, echo);
        eprintln!(
            "CWA vs JMA at {time}, >= {echo} dBZ: {both} samples seen by both, {echoes} CWA echoes; \
             overlap {at0:.3} unshifted, best {best:.3} at {shift:?}"
        );
    }
}

fn polygon() -> Vec<u8> {
    let gz = testing::fixture("formats/jma_hrpns_nd_20261106T2030.geojson.gz");
    crate::remote::codec::inflate(&gz, 64 << 20)
        .unwrap()
        .into_owned()
}

/// JMA's own polygon of 20:30 on 2026-11-06: Japan, Busan and Taipei are
/// observed; Seoul, the Chinese coast, Vladivostok and the open Pacific are
/// not, though the tiles fetched reach them.
#[test]
fn the_coverage_polygon_says_what_jma_observes() {
    let set = tiles();
    let observed = observed_pixels(&polygon(), &set).unwrap();
    let (nx, world) = (set.cols as usize * 256, 64.0 * 256.0);
    let at = |lon: f64, lat: f64| {
        let i = (wx_core::proj::lon_to_x(lon) * world) as usize - set.x0 as usize * 256;
        let j = (wx_core::proj::lat_to_y(lat) * world) as usize - set.y0 as usize * 256;
        observed[j * nx + i]
    };
    for (place, lon, lat) in [
        ("Tokyo", 139.69, 35.69),
        ("Naha", 127.68, 26.21),
        ("Sapporo", 141.35, 43.06),
        ("Busan", 129.08, 35.18),
        ("Taipei", 121.56, 25.03),
    ] {
        assert!(at(lon, lat), "{place} is observed");
    }
    for (place, lon, lat) in [
        ("Seoul", 126.98, 37.57),
        ("Pyongyang", 125.76, 39.03),
        ("Fuzhou", 119.30, 26.08),
        ("Vladivostok", 131.89, 43.12),
        ("Shanghai", 121.47, 31.23),
        ("the Pacific", 145.0, 30.0),
    ] {
        assert!(!at(lon, lat), "{place} is not observed");
    }
    let share = observed.iter().filter(|o| **o).count() as f64 / observed.len() as f64;
    assert!(share > 0.25 && share < 0.6, "{share}");
    // The hole alone, read as the area without data, would blank Japan.
    let v: serde_json::Value = serde_json::from_slice(&polygon()).unwrap();
    let hole = &v["features"][0]["geometry"]["coordinates"][1];
    let inverted = format!(
        "{{\"features\":[{{\"geometry\":{{\"type\":\"Polygon\",\"coordinates\":[{hole}]}}}}]}}"
    );
    assert!(observed_pixels(inverted.as_bytes(), &set).is_err());
    assert!(observed_pixels(b"{\"type\":\"FeatureCollection\"}", &set).is_err());
}

/// With the polygon served, a frame is unknown outside it; the polygon is
/// asked for once in the hour.
#[tokio::test]
async fn frames_are_masked_by_the_coverage_polygon() {
    let mem = MemoryRemote::new();
    let set = tiles();
    let frames = analyses(&[t(20, 30), t(20, 25)]);
    mem.put(&times_url(TEST_BASE, "N1"), times(&frames));
    for f in &frames {
        publish(&mem, &set, *f, (56, 25), None);
    }
    let gz = testing::fixture("formats/jma_hrpns_nd_20261106T2030.geojson.gz");
    for f in &frames {
        mem.put(&coverage_url(TEST_BASE, f.valid), gz.clone());
    }
    let (engine, store) = testing::engine(&mem);
    let mut task = ObservedTask::new(
        engine.clone(),
        Arc::new(JmaRadar::at(TEST_BASE)),
        radar(),
        reg::PRODUCT,
    );
    let r = task.cycle(t(20, 31)).await.unwrap();
    assert_eq!(r.tally.produced, 2, "{:?}", r.tally);
    let polygons = mem
        .requests()
        .iter()
        .filter(|r| r.contains("hrpns_nd"))
        .count();
    assert_eq!(polygons, 1);
    for id in ["jma_radar-2611062030", "jma_radar-2611062025"] {
        let grid = testing::stored_grid(store.as_ref(), "radar", id).await;
        assert_eq!(grid.coverage_at(139.69, 35.69), 1.0, "{id}: Tokyo");
        assert_eq!(grid.coverage_at(126.98, 37.57), 0.0, "{id}: Seoul");
        assert_eq!(grid.coverage_at(121.47, 31.23), 0.0, "{id}: Shanghai");
    }
}

/// A failed request for the polygon does not unmask frames: they use the
/// last polygon read (or the built-in one), and it is asked for again
/// minutes later.
#[tokio::test]
async fn a_failed_polygon_request_does_not_unmask_frames() {
    let mem = MemoryRemote::new();
    let set = tiles();
    let first = Target {
        base: t(20, 30),
        valid: t(20, 30),
    };
    mem.put(&times_url(TEST_BASE, "N1"), times(&[first]));
    publish(&mem, &set, first, (56, 25), None);
    mem.fail(&coverage_url(TEST_BASE, t(20, 30)), 503, 10);
    let (engine, store) = testing::engine(&mem);
    let mut task = ObservedTask::new(
        engine.clone(),
        Arc::new(JmaRadar::at(TEST_BASE)),
        radar(),
        reg::PRODUCT,
    );
    let r = task.cycle(t(20, 31)).await.unwrap();
    assert_eq!(r.tally.produced, 1, "{:?}", r.tally);
    let grid = testing::stored_grid(store.as_ref(), "radar", "jma_radar-2611062030").await;
    assert_eq!(grid.coverage_at(139.69, 35.69), 1.0, "Tokyo");
    assert_eq!(
        grid.coverage_at(126.98, 37.57),
        0.0,
        "Seoul is not observed, polygon or not"
    );

    let next = Target {
        base: t(20, 35),
        valid: t(20, 35),
    };
    mem.put(&times_url(TEST_BASE, "N1"), times(&[next, first]));
    publish(&mem, &set, next, (56, 25), None);
    mem.put(
        &coverage_url(TEST_BASE, t(20, 35)),
        testing::fixture("formats/jma_hrpns_nd_20261106T2030.geojson.gz"),
    );
    mem.clear_log();
    task.cycle(t(20, 36)).await.unwrap();
    let asked: Vec<String> = mem
        .requests()
        .into_iter()
        .filter(|r| r.contains("hrpns_nd"))
        .collect();
    assert_eq!(
        asked,
        [coverage_url(TEST_BASE, t(20, 35))],
        "asked again within minutes"
    );
}

#[test]
fn the_built_in_polygon_reads() {
    let mask = observed_pixels(
        &crate::remote::codec::inflate(BUILT_IN_COVERAGE, 64 << 20).unwrap(),
        &tiles(),
    )
    .unwrap();
    assert_eq!(mask, observed_pixels(&polygon(), &tiles()).unwrap());
}

/// A lead with no tile at all is dry only when the run's analysis is dry
/// too; with rain in the analysis it is waited for, and with no analysis
/// tiles either the task fails, rather than showing a dry Japan.
#[tokio::test]
async fn a_nowcast_with_no_tiles_is_not_a_dry_one() {
    let set = tiles();
    let run = t(20, 20);
    let analysis = Target {
        base: run,
        valid: run,
    };
    let setup = |analysis_tiles: Option<bool>| {
        let mem = MemoryRemote::new();
        mem.put(&times_url(TEST_BASE, "N2"), times(&run_targets(run)));
        mem.put(&times_url(TEST_BASE, "N1"), times(&analyses(&[run])));
        match analysis_tiles {
            Some(true) => publish(&mem, &set, analysis, (56, 25), None),
            Some(false) => publish_dry(&mem, &set, analysis),
            None => {}
        }
        mem
    };
    // Rain in the analysis: every lead waits.
    let mem = setup(Some(true));
    let (engine, _) = testing::engine(&mem);
    let mut task = ModelTask::new(
        engine.clone(),
        Arc::new(JmaNowcast::at(TEST_BASE)),
        nowcast(),
        &nowcast_group(),
    );
    let r = task.cycle(t(20, 23), &|_, _| {}).await.unwrap();
    assert_eq!(r.tally.produced, 0, "{:?}", r.tally);
    assert!(engine.catalog.latest("radar", "jma_nowcast").is_none());
    // No analysis tiles either: the paths have changed.
    let mem = setup(None);
    let (engine, _) = testing::engine(&mem);
    let mut task = ModelTask::new(
        engine.clone(),
        Arc::new(JmaNowcast::at(TEST_BASE)),
        nowcast(),
        &nowcast_group(),
    );
    let e = task
        .cycle(t(20, 23), &|_, _| {})
        .await
        .expect_err("a failing task");
    assert!(format!("{e:#}").contains("paths changed"), "{e:#}");
    // A dry analysis: a dry forecast.
    let mem = setup(Some(false));
    let (engine, _) = testing::engine(&mem);
    let mut task = ModelTask::new(
        engine.clone(),
        Arc::new(JmaNowcast::at(TEST_BASE)),
        nowcast(),
        &nowcast_group(),
    );
    let r = task.cycle(t(20, 23), &|_, _| {}).await.unwrap();
    assert_eq!(r.tally.produced, 12, "{:?}", r.tally);
}

/// The newest analysis listed must have tiles, and a list that has stopped
/// is a failing source.
#[tokio::test]
async fn missing_newest_tiles_and_a_stopped_list_fail_the_task() {
    let mem = MemoryRemote::new();
    mem.put(
        &times_url(TEST_BASE, "N1"),
        times(&analyses(&[t(20, 30), t(20, 25)])),
    );
    publish(
        &mem,
        &tiles(),
        Target {
            base: t(20, 25),
            valid: t(20, 25),
        },
        (56, 25),
        None,
    );
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(
        engine.clone(),
        Arc::new(JmaRadar::at(TEST_BASE)),
        radar(),
        reg::PRODUCT,
    );
    // The fetch fails, or (where the driver keeps such a file for another
    // try) the next listing does.
    let mut failed = None;
    for now in [t(20, 31), t(20, 32)] {
        if let Err(e) = task.cycle(now).await {
            failed = Some(e);
            break;
        }
    }
    let e = failed.expect("the newest has no tiles");
    assert!(format!("{e:#}").contains("paths changed"), "{e:#}");
    let e = task
        .cycle(t(21, 15))
        .await
        .err()
        .expect("nothing for 45 minutes");
    assert!(format!("{e:#}").contains("stopped"), "{e:#}");
}
