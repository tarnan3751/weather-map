use super::*;
use crate::driver::model::ModelTask;
use crate::driver::{Cx, Fetched, Payload};
use crate::remote::memory::MemoryRemote;
use crate::testing::{self, model_file};
use std::sync::Arc;
use wx_core::families::hrrr::SURFACE;
use wx_core::layer;

use crate::testing::at as t;

fn conus() -> Layout {
    layout("conus").unwrap()
}

fn hrrr() -> &'static SourceDef {
    crate::testing::source("hrrr")
}

const F06: [(&str, &str, &str, &str); 3] = [
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
];

/// Runs on the 12-hour anchor cycle, so snowfall takes its parts.
const RUN: (u32, u32) = (12, 0);
const NOW: (u32, u32) = (14, 0);

fn url(key: &str) -> String {
    S3Bucket::aws(BUCKET).url(key)
}

/// Serve `file` and its inventory as hour `hour` of the 12Z run.
fn publish(
    mem: &MemoryRemote,
    product: &str,
    hour: u32,
    file: Vec<u8>,
    inventory: String,
) -> String {
    let key = file_key(conus(), t(RUN.0, RUN.1), product, hour);
    let bucket = S3Bucket::aws(BUCKET);
    mem.put_s3(&bucket, &key, file);
    mem.put_s3(&bucket, &format!("{key}.idx"), inventory);
    key
}

fn task(engine: Arc<crate::engine::Engine>) -> ModelTask {
    testing::model_task(&engine, Hrrr::new(18), "hrrr")
}

/// GETs of `key` and its inventory (not listings).
fn gets(mem: &MemoryRemote, key: &str) -> Vec<String> {
    mem.requests_for(&url(key))
}

#[test]
fn hours_and_leads() {
    assert_eq!(last_hour(SURFACE, t(11, 0), 18), 18);
    assert_eq!(last_hour(SURFACE, t(11, 0), 48), 18);
    assert_eq!(last_hour(SURFACE, t(12, 0), 48), 48);
    assert_eq!(last_hour(SURFACE, t(12, 0), 18), 18);
    assert_eq!(last_hour(SURFACE, t(12, 0), 6), 6);
    assert_eq!(last_hour(SUBHOURLY, t(12, 0), 48), 18);
    let h = Hrrr::new(18);
    let sfc = h.leads(hrrr(), SURFACE, t(11, 0));
    assert_eq!(sfc.len(), 19);
    assert_eq!(sfc[6], 360);
    let subh = h.leads(hrrr(), SUBHOURLY, t(11, 0));
    assert_eq!(
        (subh.len(), subh[0], subh[3], subh[4], *subh.last().unwrap()),
        (72, 15, 60, 75, 1080)
    );
    // The sub-hourly file of hour h holds the four steps after hour h - 1.
    assert_eq!(
        [15, 60, 75, 120, 1080].map(|l| file_hour(SUBHOURLY, l)),
        [1, 1, 2, 2, 18]
    );
    assert_eq!(file_hour(SURFACE, 360), 6);
    // Alaska runs every three hours; 13Z does not exist there.
    let ak = crate::testing::source("hrrr_ak");
    assert!(h.check(ak).is_ok());
    assert!(h.leads(ak, SURFACE, t(13, 0)).is_empty());
    assert_eq!(h.cycle(ak).every, Duration::hours(3));
    assert_eq!(Hrrr::new(48).leads(ak, SURFACE, t(12, 0)).len(), 49);
}

#[test]
fn arrival_estimates_follow_the_spike() {
    let run = t(10, 0);
    assert_eq!(arrival(run, 0), t(10, 50));
    assert_eq!(arrival(run, 18), t(11, 15));
    assert_eq!(arrival(t(6, 0), 48), t(7, 48));
    let f06 = arrival(run, 6) - run;
    assert!(f06 >= Duration::minutes(57) && f06 <= Duration::minutes(61));
}

#[test]
fn keys() {
    let run = t(11, 0);
    assert_eq!(
        file_key(conus(), run, SURFACE, 6),
        "hrrr.20261106/conus/hrrr.t11z.wrfsfcf06.grib2"
    );
    let ak = layout("alaska").unwrap();
    assert_eq!(
        file_key(ak, run, SURFACE, 1),
        "hrrr.20261106/alaska/hrrr.t11z.wrfsfcf01.ak.grib2"
    );
    assert_eq!(
        run_prefix(conus(), run, &[SURFACE, SUBHOURLY]),
        "hrrr.20261106/conus/hrrr.t11z.wrfs"
    );
    assert_eq!(
        run_prefix(conus(), run, &[SURFACE]),
        "hrrr.20261106/conus/hrrr.t11z.wrfsfc"
    );
    let listed: Vec<crate::remote::Listed> = [
        "hrrr.20261106/conus/hrrr.t11z.wrfsfcf06.grib2",
        "hrrr.20261106/conus/hrrr.t11z.wrfsfcf06.grib2.idx",
        "hrrr.20261106/conus/hrrr.t11z.wrfsfcf07.grib2",
    ]
    .into_iter()
    .map(|k| crate::remote::Listed {
        key: k.to_string(),
        size: 9,
        modified: None,
    })
    .collect();
    let l = crate::driver::Listing::with_sidecars(&listed, ".idx");
    // f07's inventory is not written yet: it is not there.
    assert_eq!(l.len(), 1);
    assert_eq!(
        l.size("hrrr.20261106/conus/hrrr.t11z.wrfsfcf06.grib2"),
        Some(Some(9))
    );
    assert!(layout("mars").is_none());
}

#[tokio::test]
async fn one_file_feeds_every_layer_that_reads_it() {
    let mem = testing::remote(&[BUCKET]);
    let (file, inventory) = model_file(&F06);
    let size = file.len() as u64;
    let key = publish(&mem, SURFACE, 6, file, inventory);
    let (engine, store) = testing::engine(&mem);
    let report = task(engine.clone())
        .cycle(t(NOW.0, NOW.1), &|_, _| {})
        .await
        .unwrap();
    // The test file has temperature, cloud and snowfall; the other layers
    // reading it are given up for this file, not errors.
    assert_eq!(report.tally.produced, 3, "{:?}", report.tally);
    assert_eq!(report.tally.unavailable, 4, "{:?}", report.tally);
    assert_eq!(report.tally.failed, 0);
    // One inventory GET and one ranged GET: the three messages sit together.
    let g = gets(&mem, &key);
    assert_eq!(g.len(), 2, "{g:?}");
    assert_eq!(
        g[1],
        format!("{} [bytes=0-{}]", url(&key), size - 1),
        "each message once"
    );
    for l in ["temp", "clouds", "snow"] {
        let part = engine
            .catalog
            .latest(l, "hrrr")
            .unwrap_or_else(|| panic!("{l}"));
        assert_eq!(part.id, "hrrr-26110612-360");
        assert!(
            testing::keys(store.as_ref())
                .await
                .contains(&wx_core::keys::grid(l, &part.id))
        );
    }
    // Nothing is left to fetch from this file.
    mem.clear_log();
    task(engine)
        .cycle(t(NOW.0, NOW.1), &|_, _| {})
        .await
        .unwrap();
    assert!(gets(&mem, &key).is_empty(), "{:?}", mem.requests());
}

/// A broken file charges every part it feeds once per try, without
/// restarting the feed.
#[tokio::test]
async fn a_broken_file_charges_every_part_once_per_try() {
    let mem = testing::remote(&[BUCKET]);
    let (file, inventory) = model_file(&F06);
    // The inventory is for another file: its offsets land mid-message.
    let mut shifted = vec![0u8; 7];
    shifted.extend(file);
    let key = publish(&mem, SURFACE, 6, shifted, inventory);
    let (engine, _) = testing::engine(&mem);
    let mut task = task(engine.clone());
    for attempt in 1..=crate::engine::MAX_ATTEMPTS {
        let r = task
            .cycle(t(NOW.0, NOW.1), &|_, _| {})
            .await
            .expect("a broken file is not the task's failure");
        assert_eq!(r.tally.failed, 7, "attempt {attempt}: {:?}", r.tally);
    }
    // Every part is given up, so the file is never fetched again.
    mem.clear_log();
    task.cycle(t(NOW.0, NOW.1), &|_, _| {}).await.unwrap();
    assert!(gets(&mem, &key).is_empty());
    assert!(engine.catalog.latest("temp", "hrrr").is_none());
}

/// An inventory naming bytes past the end of its file is a broken file,
/// charged like any other.
#[tokio::test]
async fn an_inventory_past_the_end_of_the_file_is_charged() {
    let mem = testing::remote(&[BUCKET]);
    let (file, inventory) = model_file(&F06);
    let truncated = file[..file.len() / 2].to_vec();
    let key = publish(&mem, SURFACE, 6, truncated, inventory);
    let (engine, _) = testing::engine(&mem);
    let mut task = task(engine.clone());
    for _ in 0..crate::engine::MAX_ATTEMPTS {
        mem.clear_log();
        let r = task.cycle(t(NOW.0, NOW.1), &|_, _| {}).await.unwrap();
        assert_eq!(r.tally.failed, 7, "{:?}", r.tally);
        // The listing gave the size: only the inventory was fetched.
        assert_eq!(gets(&mem, &key), [format!("{}.idx", url(&key))]);
    }
    mem.clear_log();
    task.cycle(t(NOW.0, NOW.1), &|_, _| {}).await.unwrap();
    assert!(gets(&mem, &key).is_empty());

    // Without a known size, the short read is caught the same way.
    let cx = Cx::new(&engine.remote, "test", t(NOW.0, NOW.1));
    let temp = layer::layer("temp").unwrap().input_for(hrrr()).unwrap();
    let need = crate::driver::Need {
        input: temp,
        field: Some(temp.fields[0]),
        lead: 360,
    };
    let u = url(&key);
    let e = cx
        .grib_with_idx(&u, &format!("{u}.idx"), None, &[need])
        .await;
    assert_eq!(e.unwrap_err().failure, crate::remote::Failure::Object);
}

#[tokio::test]
async fn the_destination_store_failing_stops_the_task() {
    let mem = testing::remote(&[BUCKET]);
    let (file, inventory) = model_file(&F06);
    publish(&mem, SURFACE, 6, file, inventory);
    let engine = testing::engine_with(
        &mem,
        Arc::new(testing::stores::Broken),
        wx_core::LAYERS,
        wx_core::SOURCES,
    );
    let mut task = task(engine.clone());
    assert!(task.cycle(t(NOW.0, NOW.1), &|_, _| {}).await.is_err());
    // Not the parts' fault: they are not charged.
    let wanted = task.wanted(t(RUN.0, RUN.1), t(NOW.0, NOW.1));
    assert!(wanted.iter().any(|j| j.layer.id == "temp" && j.lead == 360));
}

#[tokio::test]
async fn a_missing_inventory_is_not_an_error() {
    let mem = testing::remote(&[BUCKET]);
    let (file, inventory) = model_file(&F06);
    let key = publish(&mem, SURFACE, 6, file, inventory);
    let (engine, _) = testing::engine(&mem);
    // Listed with its inventory, which then disappears before it is read.
    let mut task = task(engine.clone());
    mem.fail(&format!("{}.idx", url(&key)), 404, 1);
    let r = task.cycle(t(NOW.0, NOW.1), &|_, _| {}).await.unwrap();
    assert_eq!(r.tally, Default::default());
    // The file is listed again soon, then used.
    assert!(
        r.wake <= t(NOW.0, NOW.1) + Duration::minutes(1),
        "{}",
        r.wake
    );
    let r = task.cycle(t(NOW.0, NOW.1 + 1), &|_, _| {}).await.unwrap();
    assert_eq!(r.tally.produced, 3, "{:?}", r.tally);
}

/// A broken f00 does not stop the run: the files after it are ingested.
#[tokio::test]
async fn a_broken_file_does_not_stop_the_run() {
    let mem = testing::remote(&[BUCKET]);
    let tmp = "hrrr_t10z_f06__TMP_2mabove.grib2";
    let tcdc = "hrrr_t10z_f06__TCDC_entire.grib2";
    let (good, inventory) = model_file(&[
        ("TMP", "2 m above ground", "anl", tmp),
        ("TCDC", "entire atmosphere", "anl", tcdc),
    ]);
    let mut broken = vec![0u8; 7];
    broken.extend(good);
    publish(&mem, SURFACE, 0, broken, inventory);
    let (file, inventory) = model_file(&[
        ("TMP", "2 m above ground", "1 hour fcst", tmp),
        ("TCDC", "entire atmosphere", "1 hour fcst", tcdc),
    ]);
    publish(&mem, SURFACE, 1, file, inventory);
    let (engine, _) = testing::engine(&mem);
    let c = task(engine.clone())
        .cycle(t(NOW.0, NOW.1), &|_, _| {})
        .await
        .expect("a broken file is not the task's failure");
    assert!(c.tally.failed >= 2, "{:?}", c.tally);
    let ids = |l: &str| {
        engine
            .catalog
            .timeline(l)
            .parts
            .into_iter()
            .map(|p| p.id)
            .collect::<Vec<_>>()
    };
    assert!(ids("temp").contains(&"hrrr-26110612-60".to_string()));
    assert!(ids("clouds").contains(&"hrrr-26110612-60".to_string()));
    assert!(!ids("temp").contains(&"hrrr-26110612-0".to_string()));
}

/// One sub-hourly file holds four 15-minute steps: one inventory, four
/// messages, four parts.
#[tokio::test]
async fn a_subhourly_file_feeds_four_parts() {
    let mem = testing::remote(&[BUCKET]);
    let q15 = "hrrr_t10z_subh01__REFC_entireatmosphere_15min.grib2";
    let q45 = "hrrr_t10z_subh01__REFC_entireatmosphere_45min.grib2";
    let (file, inventory) = model_file(&[
        ("REFC", "entire atmosphere", "15 min fcst", q15),
        ("REFC", "entire atmosphere", "30 min fcst", q45),
        ("REFC", "entire atmosphere", "45 min fcst", q45),
        ("REFC", "entire atmosphere", "60 min fcst", q15),
    ]);
    let key = publish(&mem, SUBHOURLY, 1, file, inventory);
    let (engine, _) = testing::engine(&mem);
    let r = task(engine.clone())
        .cycle(t(12, 58), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(r.tally.produced, 4, "{:?}", r.tally);
    let mut ids: Vec<String> = engine
        .catalog
        .timeline("radar")
        .parts
        .into_iter()
        .map(|p| p.id)
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "hrrr-26110612-15",
            "hrrr-26110612-30",
            "hrrr-26110612-45",
            "hrrr-26110612-60"
        ]
    );
    // The inventory once; the four messages sit together, so one range.
    assert_eq!(gets(&mem, &key).len(), 2, "{:?}", gets(&mem, &key));
}

#[tokio::test]
async fn ranges_come_as_grib_payloads() {
    let mem = MemoryRemote::new();
    let (file, inventory) = model_file(&F06);
    let u = "https://example.org/f06.grib2";
    mem.put(u, file);
    mem.put(&format!("{u}.idx"), inventory);
    let remote = testing::quick_remote(&mem);
    let cx = Cx::new(&remote, "test", t(NOW.0, NOW.1));
    let snow = layer::layer("snow").unwrap().input_for(hrrr()).unwrap();
    let temp = layer::layer("temp").unwrap().input_for(hrrr()).unwrap();
    let dew = layer::layer("dew_point")
        .unwrap()
        .input_for(hrrr())
        .unwrap();
    let needs: Vec<crate::driver::Need> = [snow, temp, dew]
        .iter()
        .map(|i| crate::driver::Need {
            input: i,
            field: Some(i.fields[0]),
            lead: 360,
        })
        .collect();
    let Fetched::Got(p) = cx
        .grib_with_idx(u, &format!("{u}.idx"), None, &needs)
        .await
        .unwrap()
    else {
        panic!("missing");
    };
    assert!(
        matches!(&p[0], Some(Payload::Grib(b)) if b.starts_with(b"GRIB") && b.ends_with(b"7777"))
    );
    assert!(matches!(&p[1], Some(Payload::Grib(_))));
    assert!(p[2].is_none(), "no dew point in this file");
    // A missing inventory means the file is not ready.
    assert!(matches!(
        cx.grib_with_idx(u, "https://example.org/none.idx", None, &needs)
            .await
            .unwrap(),
        Fetched::Missing
    ));
}
