use super::*;
use crate::driver::model::ModelTask;
use crate::driver::{Decoded, Steps};
use crate::engine::{Engine, PartJob};
use crate::remote::memory::MemoryRemote;
use crate::remote::{Failure, Remote, RemoteConfig, RetryPolicy};
use crate::testing;
use object_store::ObjectStoreExt;
use object_store::memory::InMemory;
use object_store::path::Path;
use std::time::Duration as StdDuration;
use wx_core::layer::{FieldSel, layer};
use wx_core::proj::{TILE_CELLS, TILE_SAMPLES, lat_to_y, lon_to_x};
use wx_core::{FrameCatalog, Hub, Part, TileCoord, source};

pub(super) use crate::testing::on as t;

/// Every fixture is from the 6 Nov 12 UTC run.
fn run() -> DateTime<Utc> {
    t(6, 12, 0)
}

fn src(id: &str) -> &'static SourceDef {
    source::source(id).unwrap()
}

fn bucket() -> S3Bucket {
    S3Bucket::at(HOST)
}

fn mem() -> Arc<MemoryRemote> {
    let mem = MemoryRemote::new();
    mem.serve_s3(&bucket(), 1000);
    mem
}

/// An engine reading from `mem`, which does not wait between requests to
/// the bucket.
fn engine(mem: &Arc<MemoryRemote>) -> (Arc<Engine>, Arc<InMemory>) {
    let fast = crate::HostPolicy {
        concurrency: 8,
        min_interval: StdDuration::ZERO,
    };
    let remote = Remote::with_transport(
        mem.clone(),
        RemoteConfig {
            hosts: vec![("ovh.net".into(), fast)],
            retry: RetryPolicy {
                retries: 1,
                base: StdDuration::from_millis(1),
                max: StdDuration::from_millis(5),
            },
            ..RemoteConfig::default()
        },
    );
    let store = Arc::new(InMemory::new());
    let catalog = Arc::new(FrameCatalog::new(Hub::new()));
    let engine = Engine::new(
        store.clone(),
        catalog,
        crate::IngestConfig::default(),
        remote,
        LAYERS,
    );
    (Arc::new(engine), store)
}

fn task(engine: Arc<Engine>, adapter: Arome, id: &str) -> ModelTask {
    let group = vec![PRODUCT];
    ModelTask::new(engine, Arc::new(adapter), src(id), &group)
}

fn key(id: &str, hour: u32) -> String {
    file_key(
        &layout(src(id).region).unwrap(),
        run(),
        PRODUCT,
        "SP1",
        hour,
    )
}

fn publish(mem: &MemoryRemote, id: &str, hour: u32, bytes: Vec<u8>) -> String {
    let k = key(id, hour);
    mem.put_s3(&bucket(), &k, bytes);
    k
}

/// GETs of the file at `key`, as `URL [range]`.
fn gets(mem: &MemoryRemote, key: &str) -> Vec<String> {
    let u = bucket().url(key);
    mem.requests()
        .into_iter()
        .filter(|r| r.starts_with(&u))
        .collect()
}

fn ranges(mem: &MemoryRemote, key: &str) -> Vec<String> {
    gets(mem, key)
        .into_iter()
        .map(|r| {
            r.rsplit_once(" [").map_or(String::new(), |(_, range)| {
                range.trim_end_matches(']').to_string()
            })
        })
        .collect()
}

fn listings(mem: &MemoryRemote) -> Vec<String> {
    mem.listings()
}

fn fixture(name: &str) -> Vec<u8> {
    testing::fixture(&format!("arome/{name}"))
}

fn guyane(hour: u32) -> Vec<u8> {
    fixture(&format!("aromeom_guyane_t12z_SP1_{hour:03}H.grib2"))
}

/// The France 00H06H group, cut down to `tp` at steps 1, 2 and 6, then
/// `tsnowp` and `tgrp` at step 6: parameter after parameter, as the real
/// groups are.
fn france_00h06h() -> Vec<u8> {
    let mut b = testing::fixture("arome0025_t12z_SP1_00H06H__tp_0-1_0-2.grib2");
    b.extend(fixture(
        "arome0025_t12z_SP1_00H06H__tp_tsnowp_tgrp_0-6.grib2",
    ));
    b
}

/// The 07H12H group cut down to `tp`, total cloud, `tsnowp` and `tgrp` at
/// step 7.
fn france_07h12h() -> Vec<u8> {
    fixture("arome0025_t12z_SP1_07H12H__tp_tcc_tsnowp_tgrp_7.grib2")
}

/// The ids of the parts the catalog holds on `layer`, sorted.
fn held(engine: &Engine, layer: &str) -> Vec<String> {
    let mut ids: Vec<String> = engine
        .catalog
        .parts()
        .into_iter()
        .filter(|(l, _)| *l == layer)
        .map(|(_, p)| p.id)
        .collect();
    ids.sort();
    ids
}

/// The stored bands of a part at a point, as a tile at zoom 12 gives them
/// (within a 25th of a grid cell of the point).
async fn stored(store: &InMemory, layer_id: &str, part: &str, lon: f64, lat: f64) -> Vec<f32> {
    let grid = testing::stored_grid(store, layer_id, part).await;
    let z = 12;
    let n = f64::from(1u32 << z);
    let (x, y) = (lon_to_x(lon) * n, lat_to_y(lat) * n);
    let coord = TileCoord::new(z, x.floor() as u32, y.floor() as u32).unwrap();
    let tile = wx_tile::decode(&grid.render(coord).unwrap()).unwrap();
    let c = ((x - x.floor()) * TILE_CELLS as f64).round() as usize;
    let r = ((y - y.floor()) * TILE_CELLS as f64).round() as usize;
    let l = layer(layer_id).unwrap();
    l.bands
        .iter()
        .enumerate()
        .map(|(b, spec)| {
            spec.dequantize(tile.bands[b][r * TILE_SAMPLES + c])
                .unwrap_or(f32::NAN)
        })
        .collect()
}

fn need(layer_id: &str, source_id: &str, field: usize, stat: Stat, lead: u32) -> Need {
    let input = layer(layer_id).unwrap().input_for(src(source_id)).unwrap();
    Need {
        input,
        field: Some(FieldSel {
            stat,
            ..input.fields[field]
        }),
        lead,
    }
}

// ---------------------------------------------------------------------------
// Names, leads, times
// ---------------------------------------------------------------------------

#[test]
fn file_names_by_package_and_lead() {
    let fr = layout("EURW1S").unwrap();
    let r = run();
    assert_eq!(
        file_key(&fr, r, PRODUCT, "SP1", 0),
        "pnt/2026-11-06T12:00:00Z/arome/0025/SP1/arome__0025__SP1__00H06H__2026-11-06T12:00:00Z.grib2"
    );
    assert_eq!(
        file_key(&fr, r, PRODUCT, "SP2", 7),
        "pnt/2026-11-06T12:00:00Z/arome/0025/SP2/arome__0025__SP2__07H12H__2026-11-06T12:00:00Z.grib2"
    );
    let groups: Vec<(u32, u32)> = [0, 6, 7, 12, 13, 24, 25, 48, 49, 51]
        .map(|h| (group(h).0, group(h).1))
        .to_vec();
    assert_eq!(
        groups,
        [
            (0, 6),
            (0, 6),
            (7, 12),
            (7, 12),
            (13, 18),
            (19, 24),
            (25, 30),
            (43, 48),
            (49, 51),
            (49, 51)
        ]
    );
    assert_eq!(group(51).2, 8, "nine groups");
    let om = layout("GUYANE").unwrap();
    assert_eq!(
        file_key(&om, r, PRODUCT, "SP1", 7),
        "pnt/2026-11-06T12:00:00Z/arome-om/GUYANE/0025/SP1/arome-om-GUYANE__0025__SP1__007H__2026-11-06T12:00:00Z.grib2"
    );
    assert_eq!(
        package_prefix(&om, r, PRODUCT, "SP1"),
        "pnt/2026-11-06T12:00:00Z/arome-om/GUYANE/0025/SP1/"
    );
    // The URL escapes the colons, which the bucket takes as they are.
    assert!(
        bucket()
            .url(&key("arome_guyane", 7))
            .contains("/pnt/2026-11-06T12%3A00%3A00Z/arome-om/")
    );
    // The file of a need comes from its field's package and lead.
    let a = Arome::default();
    let temp = need("temp", "arome_fr", 0, Stat::Instant, 13 * 60);
    assert!(
        a.file(src("arome_fr"), r, &temp)
            .ends_with("SP1__13H18H__2026-11-06T12:00:00Z.grib2")
    );
    let temp = need("temp", "arome_polyn", 0, Stat::Instant, 48 * 60);
    assert!(
        a.file(src("arome_polyn"), r, &temp)
            .ends_with("arome-om-POLYN__0025__SP1__048H__2026-11-06T12:00:00Z.grib2")
    );
}

#[test]
fn leads_cycles_and_regions() {
    let a = Arome::default();
    let fr = src("arome_fr");
    let leads = a.leads(fr, PRODUCT, t(6, 15, 0));
    assert_eq!(
        (leads.len(), leads[7], *leads.last().unwrap()),
        (52, 420, 51 * 60)
    );
    assert!(a.leads(fr, PRODUCT, t(6, 16, 0)).is_empty());
    assert_eq!(a.cycle(fr).every, Duration::hours(3));
    for id in [
        "arome_antilles",
        "arome_guyane",
        "arome_indien",
        "arome_ncaled",
        "arome_polyn",
    ] {
        let s = src(id);
        assert!(a.check(s).is_ok(), "{id}");
        assert_eq!(a.cycle(s).every, Duration::hours(6), "{id}");
        assert_eq!(a.leads(s, PRODUCT, run()).len(), 49, "{id}");
        assert!(
            a.leads(s, PRODUCT, t(6, 15, 0)).is_empty(),
            "{id}: no 15 UTC run"
        );
    }
    assert!(a.check(fr).is_ok());
    let mars = SourceDef {
        id: "arome_mars",
        region: "MARS",
        ..wx_core::families::arome::AROME_FR
    };
    assert!(a.check(&mars).is_err());
    assert!(a.leads(&mars, PRODUCT, run()).is_empty());
    // Runs looked at: for the overseas domains, at 19:30 the 18 UTC run
    // (not out until after midnight) back past the 12-hour window and two
    // spares.
    let runs = a
        .cycle(src("arome_guyane"))
        .runs(t(6, 19, 30), Duration::hours(12));
    assert_eq!(
        runs,
        [
            t(6, 18, 0),
            t(6, 12, 0),
            t(6, 6, 0),
            t(6, 0, 0),
            t(5, 18, 0)
        ]
    );
    let runs = a.cycle(fr).runs(t(6, 19, 30), Duration::hours(12));
    assert_eq!(
        (runs.first(), runs.last(), runs.len()),
        (Some(&t(6, 18, 0)), Some(&t(6, 0, 0)), 7)
    );
}

#[test]
fn arrival_follows_each_tree() {
    let fr = layout("EURW1S").unwrap();
    // 12 UTC: the first group at 3 h 08, the last at 4 h 02.
    assert_eq!(arrival(&fr, run(), 0), t(6, 15, 8));
    assert_eq!(arrival(&fr, run(), 6), t(6, 15, 8));
    assert_eq!(arrival(&fr, run(), 51), t(6, 16, 2));
    assert_eq!(
        arrival(&fr, run(), 7),
        run() + Duration::seconds(194 * 60 + 45)
    );
    // The 03 UTC run lands first, the 06 and 18 UTC runs last.
    assert_eq!(arrival(&fr, t(6, 3, 0), 0), t(6, 4, 53));
    assert_eq!(arrival(&fr, t(6, 6, 0), 0), t(6, 10, 23));
    assert_eq!(arrival(&fr, t(6, 18, 0), 51), t(6, 23, 8));
    // Overseas: six and a half hours and more.
    let gy = layout("GUYANE").unwrap();
    assert_eq!(arrival(&gy, run(), 0), t(6, 18, 26));
    assert_eq!(arrival(&gy, run(), 24), t(6, 18, 47));
    assert_eq!(arrival(&gy, run(), 48), t(6, 19, 8));
    let ind = layout("INDIEN").unwrap();
    assert_eq!(arrival(&ind, run(), 0), t(6, 18, 39));
    assert_eq!(arrival(&ind, run(), 48), t(6, 19, 57));
}

/// Every input applies to every source: all six domains carry the same
/// fields, so none is limited with `InputSpec::only`.
#[test]
fn every_domain_feeds_every_layer() {
    let ids = [
        "temp",
        "feels_like",
        "dew_point",
        "wind",
        "clouds",
        "radar",
        "precip",
        "snow",
    ];
    for s in wx_core::source::sources_of(wx_core::Family::Arome) {
        let fed: Vec<&str> = crate::driver::inputs_of(LAYERS, s)
            .iter()
            .map(|(l, _)| l.id)
            .collect();
        assert_eq!(fed.len(), ids.len(), "{}: {fed:?}", s.id);
        for id in ids {
            assert!(fed.contains(&id), "{}: {id}", s.id);
        }
    }
}

// ---------------------------------------------------------------------------
// Inventories and selection
// ---------------------------------------------------------------------------

#[test]
fn inventories_name_messages_by_short_name_and_time() {
    let fr = france_00h06h();
    let infos = wx_pipeline::scan(&fr).unwrap();
    let inv = inventory(&infos, "SP1", run(), "test").unwrap();
    let names: Vec<(&str, IdxTime)> = inv.iter().map(|e| (e.var.as_str(), e.time)).collect();
    let acc = |to| IdxTime::Acc { from: 0, to };
    assert_eq!(
        names,
        [
            ("tp", acc(60)),
            ("tp", acc(120)),
            ("tp", acc(360)),
            ("tsnowp", acc(360)),
            ("tgrp", acc(360))
        ]
    );
    assert_eq!(inv[1].range, ByteRange::new(261_949, 533_922));
    assert!(inv.iter().all(|e| e.level == "SP1"));
    // Of the three run totals of `tp`, the one ending at the lead.
    let tp = FieldSel {
        var: "tp",
        level: "SP1",
        stat: Stat::RunTotal,
    };
    assert_eq!(idx::find(&inv, &tp, 120).unwrap().range.start, 261_949);
    assert!(idx::find(&inv, &tp, 180).is_none());
    assert!(
        idx::find(
            &inv,
            &FieldSel {
                stat: Stat::Instant,
                ..tp
            },
            120
        )
        .is_none()
    );
    assert!(idx::find(&inv, &FieldSel { level: "SP2", ..tp }, 120).is_none());
    // Another run's file is refused.
    assert!(inventory(&infos, "SP1", t(6, 9, 0), "test").is_err());

    let gy = guyane(12);
    let inv = inventory(&wx_pipeline::scan(&gy).unwrap(), "SP1", run(), "test").unwrap();
    let names: Vec<&str> = inv.iter().map(|e| e.var.as_str()).collect();
    assert_eq!(
        names,
        [
            "0.2.0",
            "0.2.1",
            "max_i10fg",
            "0.3.1",
            "10u",
            "10v",
            "0.2.23",
            "0.2.24",
            "2t",
            "2r",
            "tp",
            "0.6.1",
            "tsnowp",
            "0.4.7",
            "tgrp"
        ]
    );
    assert_eq!(inv[2].time, IdxTime::Max { from: 660, to: 720 });
    assert_eq!(inv[8].time, IdxTime::At { minutes: 720 });
    assert_eq!(inv[10].time, acc(720));
    // Step 0 holds only the instantaneous fields, cloud excepted.
    let inv = inventory(
        &wx_pipeline::scan(&guyane(0)).unwrap(),
        "SP1",
        run(),
        "test",
    )
    .unwrap();
    let names: Vec<&str> = inv.iter().map(|e| e.var.as_str()).collect();
    assert_eq!(names, ["0.2.0", "0.2.1", "0.3.1", "10u", "10v", "2t", "2r"]);
}

/// A whole overseas file: one GET, each need its own message, decoded to
/// what ecCodes reads.
#[tokio::test]
async fn whole_files_are_cut_into_messages() {
    let mem = mem();
    let k = publish(&mem, "arome_guyane", 12, guyane(12));
    let (engine, _) = engine(&mem);
    let cx = Cx::new(&engine.remote, "test", t(6, 20, 0));
    let a = Arome::default();
    let needs = [
        need("temp", "arome_guyane", 0, Stat::Instant, 720),
        need("precip", "arome_guyane", 0, Stat::RunTotal, 720),
        need("wind", "arome_guyane", 2, Stat::StepMax, 720),
        need("temp", "arome_guyane", 0, Stat::Instant, 660),
    ];
    let file = ModelFile {
        name: k.clone(),
        size: Some(guyane(12).len() as u64),
    };
    let Fetched::Got(p) = a
        .fetch(&cx, src("arome_guyane"), run(), &file, &needs)
        .await
        .unwrap()
    else {
        panic!("missing");
    };
    assert_eq!(gets(&mem, &k), [bucket().url(&k)], "one whole GET");
    assert!(p[3].is_none(), "the file holds hour 12 only");
    let decoded: Vec<Decoded> = p[..3]
        .iter()
        .zip(&needs)
        .map(|(p, n)| decode(p.as_ref().unwrap(), n).unwrap())
        .collect();
    for d in &decoded {
        assert_eq!(d.valid, Some(t(7, 0, 0)));
        assert_eq!((d.field.nx, d.field.ny), (419, 317));
    }
    // ecCodes: 2t 300.2537 K near Cayenne, tp 0-12 2.2889 kg m-2 at the
    // wettest hour's cell.
    assert!((decoded[0].field.values[69_311] - 300.253_66).abs() < 1e-3);
    assert!((decoded[1].field.values[29_720] - 2.288_94).abs() < 1e-4);
    assert!(
        decoded[2]
            .field
            .values
            .iter()
            .all(|v| (0.0..30.0).contains(v))
    );
    // A need decodes only from its own message.
    assert!(decode(p[0].as_ref().unwrap(), &needs[1]).is_err());
    // Known now: the next need from this file is one range, not the file.
    mem.clear_log();
    let Fetched::Got(p) = a
        .fetch(&cx, src("arome_guyane"), run(), &file, &needs[1..2])
        .await
        .unwrap()
    else {
        panic!("missing");
    };
    assert!(p[0].is_some());
    assert_eq!(ranges(&mem, &k), ["bytes=1224919-1272718"]);
}

/// Step 0 never holds an accumulation, a gust or the total cloud: nothing
/// is fetched for them.
#[tokio::test]
async fn step_zero_needs_are_not_fetched() {
    let mem = mem();
    let k = publish(&mem, "arome_guyane", 0, guyane(0));
    let (engine, _) = engine(&mem);
    let cx = Cx::new(&engine.remote, "test", t(6, 20, 0));
    let needs = [
        need("wind", "arome_guyane", 2, Stat::StepMax, 0),
        need("clouds", "arome_guyane", 0, Stat::Instant, 0),
        need("radar", "arome_guyane", 0, Stat::RunTotal, 0),
    ];
    assert!(needs.iter().all(at_step_zero));
    assert!(!at_step_zero(&need(
        "temp",
        "arome_guyane",
        0,
        Stat::Instant,
        0
    )));
    assert!(!at_step_zero(&need(
        "clouds",
        "arome_guyane",
        0,
        Stat::Instant,
        60
    )));
    let file = ModelFile {
        name: k,
        size: None,
    };
    let got = Arome::default()
        .fetch(&cx, src("arome_guyane"), run(), &file, &needs)
        .await
        .unwrap();
    assert!(matches!(got, Fetched::Got(p) if p.iter().all(Option::is_none)));
    assert!(mem.requests().is_empty(), "{:?}", mem.requests());
}

/// The France groups are walked, one small range per message, then only
/// the wanted messages are fetched.
#[tokio::test]
async fn a_walk_reads_each_head_once() {
    let mem = mem();
    let bytes = france_07h12h();
    let size = bytes.len() as u64;
    let k = publish(&mem, "arome_fr", 7, bytes);
    let (engine, _) = engine(&mem);
    let cx = Cx::new(&engine.remote, "test", t(6, 16, 0));
    let a = Arome::walking();
    let file = ModelFile {
        name: k.clone(),
        size: Some(size),
    };
    let needs = [
        need("clouds", "arome_fr", 0, Stat::Instant, 420),
        need("snow", "arome_fr", 0, Stat::RunTotal, 420),
    ];
    let Fetched::Got(p) = a
        .fetch(&cx, src("arome_fr"), run(), &file, &needs)
        .await
        .unwrap()
    else {
        panic!("missing");
    };
    // Four heads, then cloud and snow, which sit together.
    assert_eq!(
        ranges(&mem, &k),
        [
            "bytes=0-511",
            "bytes=371554-372065",
            "bytes=746182-746693",
            "bytes=868043-868554",
            "bytes=371554-868042"
        ]
    );
    let cloud = decode(p[0].as_ref().unwrap(), &needs[0]).unwrap();
    assert_eq!((cloud.field.nx, cloud.field.ny), (1121, 717));
    assert_eq!(cloud.valid, Some(t(6, 19, 0)));
    let present: Vec<f32> = cloud
        .field
        .values
        .iter()
        .copied()
        .filter(|v| !v.is_nan())
        .collect();
    assert_eq!(
        present.len(),
        1121 * 717 - 138_076,
        "the trapezoid's corners are missing"
    );
    let (lo, hi) = present
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    assert!(
        lo >= 0.0 && hi <= 100.01 && hi > 99.0,
        "percent: {lo}..{hi}"
    );
    assert_eq!(a.inventories.len(), 1);

    // Re-published with the same size but other bytes (the four messages in
    // another order): the kept inventory no longer matches. The fetch fails
    // and the inventory is forgotten, so the next one walks again.
    let infos = wx_pipeline::scan(&france_07h12h()).unwrap();
    let all = france_07h12h();
    let reordered: Vec<u8> = infos
        .iter()
        .rev()
        .flat_map(|m| all[m.offset..m.offset + m.length].to_vec())
        .collect();
    assert_eq!(reordered.len() as u64, size);
    mem.put_s3(&bucket(), &k, reordered);
    let e = a
        .fetch(&cx, src("arome_fr"), run(), &file, &needs)
        .await
        .unwrap_err();
    assert_eq!(e.failure, Failure::Object, "{e}");
    assert_eq!(a.inventories.len(), 0);
    mem.clear_log();
    let Fetched::Got(p) = a
        .fetch(&cx, src("arome_fr"), run(), &file, &needs)
        .await
        .unwrap()
    else {
        panic!("missing");
    };
    assert_eq!(
        gets(&mem, &k).len(),
        4 + 1,
        "walked again: four heads, then the two messages, together now"
    );
    assert_eq!(
        decode(p[0].as_ref().unwrap(), &needs[0]).unwrap().valid,
        Some(t(6, 19, 0))
    );

    // A file that is not GRIB where a message should start is broken.
    let mut shifted = vec![0u8; 7];
    shifted.extend(france_07h12h());
    mem.put_s3(&bucket(), &k, shifted);
    let file = ModelFile {
        name: k.clone(),
        size: Some(size + 7),
    };
    let e = a
        .fetch(&cx, src("arome_fr"), run(), &file, &needs)
        .await
        .unwrap_err();
    assert_eq!(e.failure, Failure::Object, "{e}");
}

#[tokio::test]
async fn listing_reads_the_runs_packages() {
    let mem = mem();
    let sp1 = publish(&mem, "arome_fr", 0, france_00h06h());
    publish(&mem, "arome_fr", 7, france_07h12h());
    // Another package, and another run, are not this run's SP1.
    let fr = layout("EURW1S").unwrap();
    mem.put_s3(
        &bucket(),
        &file_key(&fr, run(), PRODUCT, "SP2", 0),
        vec![0u8; 3],
    );
    mem.put_s3(
        &bucket(),
        &file_key(&fr, t(6, 9, 0), PRODUCT, "SP1", 0),
        vec![0u8; 3],
    );
    let (engine, _) = engine(&mem);
    let cx = Cx::new(&engine.remote, "test", t(6, 16, 0));
    let listing = Arome::default()
        .list(&cx, src("arome_fr"), run(), &[PRODUCT])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listing.len(), 2);
    assert_eq!(listing.size(&sp1), Some(Some(france_00h06h().len() as u64)));
    assert_eq!(listings(&mem).len(), 1, "one prefix, SP1's");
    assert!(
        listings(&mem)[0].contains("prefix=pnt%2F2026-11-06T12%3A00%3A00Z%2Farome%2F0025%2FSP1%2F")
    );
}

// ---------------------------------------------------------------------------
// Whole runs through the driver
// ---------------------------------------------------------------------------

/// Half past seven, an hour after the 12 UTC Guyane run began to land: the
/// files for hours 0, 11 and 12 are out, 10 is not. Each is downloaded once
/// and feeds every layer reading it; the steps ending at 11 wait for 10.
#[tokio::test]
async fn an_overseas_run_feeds_every_layer() {
    let mem = mem();
    let keys: Vec<String> = [0, 11, 12]
        .into_iter()
        .map(|h| publish(&mem, "arome_guyane", h, guyane(h)))
        .collect();
    let (engine, store) = engine(&mem);
    let mut task = task(engine.clone(), Arome::default(), "arome_guyane");
    let now = t(6, 19, 30);
    let r = task.cycle(now, &|_, _| {}).await.unwrap();
    // Hour 0: temperature, feels-like and dew point; the wind has no gust
    // there and is not asked for. Hour 11: everything without a step.
    // Hour 12: all eight.
    assert_eq!(
        (r.tally.produced, r.tally.unavailable, r.tally.failed),
        (17, 0, 0),
        "{:?}",
        r.tally
    );
    for k in &keys {
        assert_eq!(gets(&mem, k), [bucket().url(k)], "one whole GET each");
    }
    let id = |h: u32| Part::forecast_id("arome_guyane", run(), h * 60);
    for l in ["temp", "feels_like", "dew_point"] {
        assert_eq!(held(&engine, l), [id(0), id(11), id(12)], "{l}");
    }
    for l in ["wind", "clouds", "snow"] {
        assert_eq!(held(&engine, l), [id(11), id(12)], "{l}");
    }
    for l in ["radar", "precip"] {
        assert_eq!(held(&engine, l), [id(12)], "{l}");
    }
    // The grid's box: Guyane, half a cell beyond the outer samples, with
    // its solid box inside.
    let part = engine.catalog.latest("temp", "arome_guyane").unwrap();
    let b = part.bounds;
    assert!(
        (b.west + 56.7625).abs() < 0.01 && (b.east + 46.2875).abs() < 0.01,
        "{b:?}"
    );
    assert!(
        (b.south - 1.0375).abs() < 0.01 && (b.north - 8.9625).abs() < 0.01,
        "{b:?}"
    );
    let s = part.solid.expect("a model grid has a solid box");
    assert!(
        s.west > b.west && s.east < b.east && s.south > b.south && s.north < b.north,
        "{s:?}"
    );
    // Stored values: 27.10 C near Cayenne at 00 UTC; the hour to 00 UTC at
    // the wettest cell is the difference of the run totals, 2.286 mm, rain.
    let temp = stored(&store, "temp", &id(12), -52.35, 4.825).await;
    assert!((temp[0] - 27.104).abs() < 0.05, "{temp:?}");
    let precip = stored(&store, "precip", &id(12), -47.0, 7.2).await;
    assert!((precip[0] - 2.286).abs() < 0.05, "{precip:?}");
    assert_eq!(precip[1], wx_core::layer::ptype::RAIN);

    // Hour 10 is still to come: the steps ending at 11 wait, and so does
    // every hour not out yet. Five minutes on the run is listed again,
    // nothing is fetched.
    let wanted = task.wanted(run(), now);
    assert!(
        wanted
            .iter()
            .any(|j| j.layer.id == "radar" && j.lead == 660)
    );
    assert!(wanted.iter().any(|j| j.layer.id == "temp" && j.lead == 600));
    mem.clear_log();
    let r = task
        .cycle(now + Duration::minutes(6), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(r.tally.produced, 0);
    assert!(
        keys.iter().all(|k| gets(&mem, k).is_empty()),
        "{:?}",
        mem.requests()
    );
    assert!(
        listings(&mem)
            .iter()
            .any(|l| l.contains("2026-11-06T12%3A00%3A00Z"))
    );
}

/// A file that lacks a field gives up the parts needing it, and only them.
#[tokio::test]
async fn a_file_missing_a_field_gives_up_only_its_parts() {
    let full = guyane(12);
    let infos = wx_pipeline::scan(&full).unwrap();
    let no_cloud: Vec<u8> = infos
        .iter()
        .filter(|m| !m.is(0, 6, 1))
        .flat_map(|m| full[m.offset..m.offset + m.length].to_vec())
        .collect();
    let mem = mem();
    publish(&mem, "arome_guyane", 11, guyane(11));
    publish(&mem, "arome_guyane", 12, no_cloud);
    let (engine, _) = engine(&mem);
    let r = task(engine.clone(), Arome::default(), "arome_guyane")
        .cycle(t(6, 19, 30), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(
        (r.tally.produced, r.tally.unavailable, r.tally.failed),
        (13, 1, 0),
        "{:?}",
        r.tally
    );
    let id = |h: u32| Part::forecast_id("arome_guyane", run(), h * 60);
    assert_eq!(held(&engine, "clouds"), [id(11)]);
    assert_eq!(held(&engine, "wind"), [id(11), id(12)]);
}

/// The 12 UTC France run, its groups landing in two cycles. The first
/// cycle has 07H12H only: what needs nothing else is made, the steps
/// starting at hour 6 wait. The second has 00H06H too: it is walked, and the
/// steps ending at 7 take hour 6's totals from it and hour 7's from 07H12H's
/// kept inventory, without walking it again.
#[tokio::test]
async fn a_france_run_is_walked_once_per_group() {
    let mem = mem();
    let g1 = publish(&mem, "arome_fr", 7, france_07h12h());
    let (engine, store) = engine(&mem);
    let mut task = task(engine.clone(), Arome::walking(), "arome_fr");
    let id = |h: u32| Part::forecast_id("arome_fr", run(), h * 60);

    let now = t(6, 15, 20);
    let r = task.cycle(now, &|_, _| {}).await.unwrap();
    // Cloud and snowfall at 7; everything else in the cut-down group lacks
    // its fields (7 to 12, 44 parts).
    assert_eq!(
        (r.tally.produced, r.tally.unavailable, r.tally.failed),
        (2, 44, 0),
        "{:?}",
        r.tally
    );
    assert_eq!(held(&engine, "clouds"), [id(7)]);
    assert_eq!(held(&engine, "snow"), [id(7)]);
    assert!(
        held(&engine, "radar").is_empty(),
        "the step to 7 starts in 00H06H"
    );
    // Four heads, then `tp`, cloud and `tsnowp` at 7 in one range.
    assert_eq!(
        ranges(&mem, &g1),
        [
            "bytes=0-511",
            "bytes=371554-372065",
            "bytes=746182-746693",
            "bytes=868043-868554",
            "bytes=0-868042"
        ]
    );

    mem.clear_log();
    let g0 = publish(&mem, "arome_fr", 0, france_00h06h());
    let now = t(6, 15, 26);
    let r = task.cycle(now, &|_, _| {}).await.unwrap();
    // Snowfall at 6, radar at 2 (tp 0-2 less tp 0-1), radar and
    // precipitation at 7 across the two groups. Step 0 is never asked for
    // the fields it lacks, so it is not among the unavailable.
    assert_eq!(
        (r.tally.produced, r.tally.unavailable, r.tally.failed),
        (4, 45, 0),
        "{:?}",
        r.tally
    );
    assert_eq!(held(&engine, "radar"), [id(2), id(7)]);
    assert_eq!(held(&engine, "precip"), [id(7)]);
    assert_eq!(held(&engine, "snow"), [id(6), id(7)]);
    // 00H06H: five heads, then the four wanted messages together (tgrp is
    // not wanted).
    assert_eq!(
        ranges(&mem, &g0),
        [
            "bytes=0-511",
            "bytes=261949-262460",
            "bytes=533922-534433",
            "bytes=889680-890191",
            "bytes=1010624-1011135",
            "bytes=0-1010623"
        ]
    );
    // 07H12H: no walk, its `tp` and `tsnowp` at 7, the cloud between them
    // too big a gap to fetch for nothing.
    assert_eq!(ranges(&mem, &g1), ["bytes=0-371553", "bytes=746182-868042"]);

    // ecCodes: tp 0-7 less tp 0-6 is 13.819 mm at 4.35 E 38.475 N, 41.26
    // dBZ by Marshall-Palmer; 5.701 mm, 2.782 of it snow, at 7.375 E 46 N.
    let radar = stored(&store, "radar", &id(7), 4.35, 38.475).await;
    let want = 23.0103 + 16.0 * 13.8193f32.log10();
    assert!((radar[0] - want).abs() < 0.2, "{radar:?} vs {want}");
    let precip = stored(&store, "precip", &id(7), 7.375, 46.0).await;
    assert!((precip[0] - 5.701).abs() < 0.1, "{precip:?}");
    assert_eq!(precip[1], wx_core::layer::ptype::MIX, "half of it snow");
    // The France grid's box.
    let b = engine.catalog.latest("clouds", "arome_fr").unwrap().bounds;
    assert!(
        (b.west + 12.0125).abs() < 0.01 && (b.east - 16.0125).abs() < 0.01,
        "{b:?}"
    );
    assert!(
        (b.south - 37.4875).abs() < 0.01 && (b.north - 55.4125).abs() < 0.01,
        "{b:?}"
    );

    // Nothing is left to fetch from either group.
    mem.clear_log();
    task.cycle(now, &|_, _| {}).await.unwrap();
    assert!(
        gets(&mem, &g0).is_empty() && gets(&mem, &g1).is_empty(),
        "{:?}",
        mem.requests()
    );
}

/// A run is not looked for before it can be out: the overseas domains
/// land six and a half hours after their run, France two to four and a
/// half depending on the run.
#[tokio::test]
async fn runs_are_not_probed_before_they_are_due() {
    let mem = mem();
    let (engine, _) = engine(&mem);
    let mut gy = task(engine.clone(), Arome::default(), "arome_guyane");
    let twelve = "2026-11-06T12%3A00%3A00Z";
    // Due 18:26, looked for from 18:16.
    gy.cycle(t(6, 18, 15), &|_, _| {}).await.unwrap();
    assert!(
        !mem.requests().iter().any(|r| r.contains(twelve)),
        "{:?}",
        mem.requests()
    );
    gy.cycle(t(6, 18, 17), &|_, _| {}).await.unwrap();
    assert!(listings(&mem).iter().any(|r| r.contains(twelve)));

    let six = "2026-11-06T06%3A00%3A00Z";
    let mut fr = task(engine, Arome::default(), "arome_fr");
    mem.clear_log();
    // The 06 UTC France run is due at 10:23.
    fr.cycle(t(6, 10, 12), &|_, _| {}).await.unwrap();
    assert!(
        !mem.requests().iter().any(|r| r.contains(six)),
        "{:?}",
        mem.requests()
    );
    fr.cycle(t(6, 10, 14), &|_, _| {}).await.unwrap();
    assert!(listings(&mem).iter().any(|r| r.contains(six)));
}

/// When a late run lands, its past hours are still wanted by the inputs
/// that fill the past, and only the recent ones by the others.
#[tokio::test]
async fn past_hours_of_a_late_run_are_wanted() {
    let mem = mem();
    let (engine, _) = engine(&mem);
    let task = task(engine, Arome::default(), "arome_indien");
    // 19:00: the INDIEN 12 UTC run has been landing for twenty minutes.
    let now = t(6, 19, 0);
    let wanted = task.wanted(run(), now);
    let leads = |l: &str| -> Vec<u32> {
        wanted
            .iter()
            .filter(|j| j.layer.id == l)
            .map(|j| j.lead / 60)
            .collect()
    };
    // The wind's gust does not exist at step 0, so that part is never wanted.
    for l in ["temp", "feels_like", "dew_point"] {
        assert_eq!(leads(l).first(), Some(&0), "{l}");
        assert_eq!(leads(l).len(), 49, "{l}");
    }
    assert_eq!(leads("wind").first(), Some(&1));
    assert_eq!(leads("wind").len(), 48);
    // Not filling the past: from an hour and a half ago.
    assert_eq!(leads("radar").first(), Some(&6), "{:?}", leads("radar"));
    assert_eq!(leads("clouds").first(), Some(&6));
    // And the step ending at 6 starts at 5.
    let radar6 = wanted
        .iter()
        .find(|j| j.layer.id == "radar" && j.lead == 360)
        .unwrap();
    assert_eq!(
        radar6.needs().iter().map(|n| n.lead).collect::<Vec<_>>(),
        [360, 300]
    );
    let job = PartJob::forecast(
        layer("radar").unwrap(),
        radar6.input,
        src("arome_indien"),
        run(),
        60,
        Some(0),
        Steps::RunTotals,
    )
    .unwrap();
    assert_eq!(job.needs().len(), 1, "the first step starts from nothing");
}

/// Decode a payload as the build step does.
pub(super) fn decode(payload: &Payload, need: &Need) -> anyhow::Result<Decoded> {
    match payload {
        Payload::Custom { bytes, decode } => decode(bytes, need),
        Payload::Grib(_) => anyhow::bail!("AROME payloads carry their decoder"),
    }
}

/// Kept inventories are bounded, the least recently used going first.
#[test]
fn inventories_are_bounded() {
    let inv = Inventories::default();
    for k in 0..KEPT_INVENTORIES + 5 {
        inv.keep(&format!("f{k}"), Some(1), Vec::new());
        // f0 stays in use.
        assert!(inv.get("f0", Some(1)).is_some(), "{k}");
    }
    assert_eq!(inv.len(), KEPT_INVENTORIES);
    assert!(inv.get("f1", Some(1)).is_none());
    assert!(
        inv.get(&format!("f{}", KEPT_INVENTORIES + 4), Some(1))
            .is_some()
    );
    // Another size is another file.
    assert!(inv.get("f0", Some(2)).is_none());
    assert!(inv.get("f0", None).is_some(), "unlisted, the name alone");
}

/// The family is registered: a selection naming it starts one task per
/// domain.
#[test]
fn the_family_is_registered() {
    let mem = mem();
    let config = crate::IngestConfig {
        feeds: vec!["arome".into()],
        ..crate::IngestConfig::default()
    };
    let ingest = crate::Ingest::with_remote(
        Arc::new(InMemory::new()),
        Arc::new(FrameCatalog::new(Hub::new())),
        config,
        Remote::with_transport(mem, RemoteConfig::default()),
    )
    .unwrap();
    assert_eq!(
        ingest.task_names(),
        [
            "arome_fr",
            "arome_antilles",
            "arome_guyane",
            "arome_indien",
            "arome_ncaled",
            "arome_polyn",
            "janitor"
        ]
    );
}

mod live;
