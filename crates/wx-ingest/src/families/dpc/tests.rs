use super::*;
use crate::driver::observed::ObservedTask;
use crate::remote::memory::MemoryRemote;
use crate::testing;

/// Hosts the test remote does not space requests on.
const TEST_API: &str = "https://dpc.example.org";
const TEST_S3: &str = "https://s3-prod-dpc-radar.s3.eu-south-1.amazonaws.com";
const VMI: &str = "VMI";

use crate::testing::at as t;

fn dpc() -> &'static SourceDef {
    crate::testing::source("dpc")
}

fn last(v: DateTime<Utc>) -> String {
    format!(
        "{{\"total\":1,\"lastProducts\":[{{\"productType\":\"VMI\",\"time\":{},\"period\":\"PT5M\"}}]}}",
        v.timestamp_millis()
    )
}

/// Where the test serves the frame of `v`; real links carry a signature.
fn link(v: DateTime<Utc>) -> String {
    format!("{TEST_S3}/{}", key_for(VMI, v))
}

fn answer(v: DateTime<Utc>) -> String {
    format!(
        "{{\"bucket\":\"s3-prod-dpc-radar\",\"key\":\"{}\",\"url\":\"{}\"}}",
        key_for(VMI, v),
        link(v)
    )
}

const NO_PRODUCT: &str = "{\"error\":\"S3 error: An error occurred (403) when calling the HeadObject operation: Forbidden\"}";

/// The API announcing `newest`, with links for `frames` and, as DPC does,
/// a 500 for every other time of the two hours before.
fn serve(mem: &MemoryRemote, d: &Dpc, newest: DateTime<Utc>, frames: &[DateTime<Utc>]) {
    mem.put(&d.last_url(VMI), last(newest));
    for k in 0..=25 {
        let v = newest - Duration::minutes(5 * k);
        let (status, body) = if frames.contains(&v) {
            (200, answer(v))
        } else {
            (500, NO_PRODUCT.to_string())
        };
        mem.put_post(&d.download_url(), &download_request(VMI, v), status, body);
    }
}

/// POSTs asking for the frame of `v`.
fn posts(mem: &MemoryRemote, d: &Dpc, v: DateTime<Utc>) -> usize {
    let p = format!("POST {} {}", d.download_url(), download_request(VMI, v));
    mem.requests().iter().filter(|r| **r == p).count()
}

#[test]
fn the_api_s_answers() {
    let (time, period) = parse_last(last(t(20, 20)).as_bytes(), VMI).unwrap();
    assert_eq!((time, period), (t(20, 20), Duration::minutes(5)));
    assert_eq!(
        t(20, 20).timestamp_millis(),
        1_793_996_400_000,
        "as the API wrote it on 2026-11-06"
    );
    for changed in [
        "<html>maintenance</html>",
        "{\"total\":0,\"lastProducts\":[]}",
        "{\"lastProducts\":[{\"productType\":\"SRI\",\"time\":1793996400000}]}",
        "{\"lastProducts\":[{\"productType\":\"VMI\",\"time\":\"20:20\"}]}",
        "{\"lastProducts\":[{\"productType\":\"VMI\",\"time\":1793996400000,\"period\":\"P1D\"}]}",
    ] {
        assert!(parse_last(changed.as_bytes(), VMI).is_err(), "{changed}");
    }
    assert_eq!(
        download_request(VMI, t(20, 20)),
        "{\"productType\":\"VMI\",\"productDate\":1793996400000}"
    );
    assert_eq!(key_for(VMI, t(20, 20)), "VMI/06-11-2026-20-20.tif");
    let (key, url) = parse_link(answer(t(20, 20)).as_bytes()).unwrap();
    assert_eq!(
        (key.as_str(), url),
        ("VMI/06-11-2026-20-20.tif", link(t(20, 20)))
    );
    for changed in [
        "{\"bucket\":\"b\",\"key\":\"VMI/x.tif\"}",
        "{\"key\":\"VMI/x.tif\",\"url\":\"http://s3-prod-dpc-radar.s3.eu-south-1.amazonaws.com/x\"}",
        "{\"key\":\"VMI/x.tif\",\"url\":\"https://example.com/x.tif\"}",
        "{\"error\":\"S3 error\"}",
    ] {
        assert!(parse_link(changed.as_bytes()).is_err(), "{changed}");
    }
}

/// The newest time and the five-minute steps before it, back to the
/// oldest wanted.
#[tokio::test]
async fn frames_up_to_the_newest_announced() {
    let mem = MemoryRemote::new();
    let d = Dpc::at(TEST_API);
    serve(&mem, &d, t(20, 20), &[]);
    let remote = testing::quick_remote(&mem);
    let cx = Cx::new(&remote, "test", t(20, 31));
    let files = d.list(&cx, dpc(), VMI, t(20, 2)).await.unwrap();
    let times: Vec<_> = files.iter().map(|f| f.valid).collect();
    assert_eq!(times, [t(20, 5), t(20, 10), t(20, 15), t(20, 20)]);
    assert_eq!(files[3].name, "VMI/06-11-2026-20-20.tif");
    assert_eq!(mem.requests(), [d.last_url(VMI)]);
}

/// Two requests a frame: the link, then the file, built into `dpc-...`.
#[tokio::test]
async fn a_frame_is_a_link_then_a_download() {
    let mem = MemoryRemote::new();
    let d = Dpc::at(TEST_API);
    serve(&mem, &d, t(18, 20), &[t(18, 20)]);
    mem.put(
        &link(t(18, 20)),
        testing::fixture("formats/dpc_VMI_20261106_1820.tif"),
    );
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(Dpc::at(TEST_API)), dpc(), VMI);
    assert_eq!(task.name, "dpc.radar");
    let r = task.cycle(t(18, 28)).await.unwrap();
    assert_eq!(r.tally.produced, 1, "{:?}", r.tally);
    let part = engine.catalog.latest("radar", "dpc").unwrap();
    assert_eq!(part.id, "dpc-2611061820");
    let b = part.bounds;
    assert!(
        b.west < 6.0 && b.east > 18.5 && b.south < 36.0 && b.north > 47.5,
        "{b:?}"
    );
    let r = mem.requests();
    assert_eq!(r[0], d.last_url(VMI));
    assert_eq!(posts(&mem, &d, t(18, 20)), 1);
    assert_eq!(
        r.iter().filter(|r| **r == link(t(18, 20))).count(),
        1,
        "{r:?}"
    );
}

/// The link expired while the download waited: one fresh link.
#[tokio::test]
async fn an_expired_link_is_asked_for_again() {
    let mem = MemoryRemote::new();
    let d = Dpc::at(TEST_API);
    serve(&mem, &d, t(18, 20), &[t(18, 20)]);
    mem.put(
        &link(t(18, 20)),
        testing::fixture("formats/dpc_VMI_20261106_1820.tif"),
    );
    mem.fail(&link(t(18, 20)), 403, 1);
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(Dpc::at(TEST_API)), dpc(), VMI);
    let r = task.cycle(t(18, 28)).await.unwrap();
    assert_eq!(r.tally.produced, 1, "{:?}", r.tally);
    assert_eq!(posts(&mem, &d, t(18, 20)), 2);
    // Refused twice: the frame is charged, the task goes on.
    mem.fail(&link(t(18, 20)), 403, 2);
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(Dpc::at(TEST_API)), dpc(), VMI);
    let r = task.cycle(t(18, 28)).await.unwrap();
    assert_eq!((r.tally.produced, r.tally.failed), (0, 1), "{:?}", r.tally);
}

/// An older time the server has no product for (answered 500) is skipped;
/// the newest, which it has just announced, is made.
#[tokio::test]
async fn a_missing_older_frame_is_skipped() {
    let mem = MemoryRemote::new();
    let d = Dpc::at(TEST_API);
    serve(&mem, &d, t(18, 20), &[t(18, 20)]);
    mem.put(
        &link(t(18, 20)),
        testing::fixture("formats/dpc_VMI_20261106_1820.tif"),
    );
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(Dpc::at(TEST_API)), dpc(), VMI);
    let r = task
        .cycle(t(18, 28))
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!((r.tally.produced, r.tally.failed), (1, 0), "{:?}", r.tally);
    assert!(posts(&mem, &d, t(18, 15)) > 0);
    // Not asked for again.
    mem.clear_log();
    task.cycle(t(18, 29)).await.unwrap();
    assert_eq!(posts(&mem, &d, t(18, 15)), 0, "{:?}", mem.requests());

    // The newest having no file is the API failing: the next listing says
    // so.
    let mem = MemoryRemote::new();
    serve(&mem, &d, t(18, 20), &[]);
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(Dpc::at(TEST_API)), dpc(), VMI);
    let e = fails_within(&mut task, t(18, 28), 2).await;
    assert!(format!("{e:#}").contains("has no file for"), "{e:#}");
}

/// Answers in another shape stop the task with a message naming the API,
/// and nothing reaches the catalog.
#[tokio::test]
async fn a_changed_api_is_a_failing_source() {
    let d = Dpc::at(TEST_API);
    let cases: [(&str, Option<&str>); 4] = [
        ("{\"items\":[]}", None),
        ("<html>Service temporarily unavailable</html>", None),
        ("", None),
        (
            &last(t(18, 20)),
            Some("{\"href\":\"https://s3-prod-dpc-radar.s3.eu-south-1.amazonaws.com/x\"}"),
        ),
    ];
    for (time_list, link_answer) in cases {
        let mem = MemoryRemote::new();
        mem.put(&d.last_url(VMI), time_list.to_string());
        if let Some(a) = link_answer {
            mem.put_post(
                &d.download_url(),
                &download_request(VMI, t(18, 20)),
                200,
                a.to_string(),
            );
        }
        let (engine, _) = testing::engine(&mem);
        let mut task = ObservedTask::new(engine.clone(), Arc::new(Dpc::at(TEST_API)), dpc(), VMI);
        // The time list failing stops the task at once; link requests
        // failing do once a few files in a row have.
        let e = fails_within(&mut task, t(18, 28), 3).await;
        let e = format!("{e:#}");
        assert!(
            e.contains("DPC radar API") || e.contains("in a row unreachable"),
            "{time_list}: {e}"
        );
        assert!(engine.catalog.latest("radar", "dpc").is_none());
    }
    // The endpoints gone altogether.
    let mem = MemoryRemote::new();
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(Dpc::at(TEST_API)), dpc(), VMI);
    assert!(task.cycle(t(18, 28)).await.is_err());
}

/// Supervised, a changed API shows as a task backing off with the reason,
/// not a crash: the supervisor's backoff spaces the retries.
#[tokio::test]
async fn the_status_page_says_the_source_is_unavailable() {
    let mem = MemoryRemote::new();
    mem.put(&Dpc::default().last_url(VMI), "{\"moved\":true}");
    let store: Arc<dyn object_store::ObjectStore> = Arc::new(object_store::memory::InMemory::new());
    let catalog = Arc::new(wx_core::FrameCatalog::new(wx_core::Hub::new()));
    let config = crate::IngestConfig {
        feeds: vec!["dpc".into()],
        ..crate::IngestConfig::default()
    };
    let ingest =
        crate::Ingest::with_remote(store, catalog, config, testing::quick_remote(&mem)).unwrap();
    let health = wx_core::HealthRegistry::new(wx_core::Hub::new());
    let cancel = tokio_util::sync::CancellationToken::new();
    let handles = ingest.spawn(&health, cancel.clone());
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let h = health.get("dpc.radar").unwrap();
    assert_eq!(h.state, wx_core::FeedState::BackingOff, "{h:?}");
    assert_eq!(h.consecutive_failures, 1);
    assert!(
        h.last_error
            .as_deref()
            .is_some_and(|e| e.contains("undocumented")),
        "{h:?}"
    );
    cancel.cancel();
    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test]
#[ignore = "network"]
async fn live_dpc_newest_frame() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let d = Dpc::default();
    let files = d
        .list(&cx, dpc(), VMI, now - Duration::minutes(20))
        .await
        .unwrap();
    let newest = files.last().expect("a frame in 20 minutes").clone();
    eprintln!("DPC: {} frames, newest {}", files.len(), newest.valid);
    let layer = wx_core::layer::layer("radar").unwrap();
    let need = Need {
        input: layer.input_for(dpc()).unwrap(),
        field: None,
        lead: 0,
    };
    let Fetched::Got(p) = d.fetch(&cx, dpc(), VMI, &newest, &[need]).await.unwrap() else {
        panic!("the newest frame is missing");
    };
    let Some(Payload::Custom { bytes, decode }) = &p[0] else {
        panic!()
    };
    let decoded = decode(bytes, &need).unwrap();
    assert_eq!(decoded.valid, Some(newest.valid));
    eprintln!("{} bytes; {:?}", bytes.len(), remote.traffic());
}

/// The first error of up to `n` cycles a minute apart from `at`.
async fn fails_within(task: &mut ObservedTask, at: DateTime<Utc>, n: i64) -> anyhow::Error {
    for k in 0..n {
        if let Err(e) = task.cycle(at + Duration::minutes(k)).await {
            return e;
        }
    }
    panic!("the task went on for {n} cycles");
}

/// The real 18:20 frame, its `DocumentName` restamped to `v`.
fn frame_at(v: DateTime<Utc>) -> Vec<u8> {
    let mut tif = testing::fixture("formats/dpc_VMI_20261106_1820.tif");
    let (from, to) = (
        "_18-20.tif".to_string(),
        format!("_{}.tif", v.format("%H-%M")),
    );
    let at = tif
        .windows(from.len())
        .position(|w| w == from.as_bytes())
        .expect("the DocumentName");
    tif[at..at + to.len()].copy_from_slice(to.as_bytes());
    tif
}

/// Only DPC's own answer makes an older frame missing: any other failure of
/// the link request (a timeout, a 503) stops the cycle, and the frame is
/// made when the API answers again.
#[tokio::test]
async fn a_blip_on_an_older_frame_does_not_lose_it() {
    let mem = MemoryRemote::new();
    let d = Dpc::at(TEST_API);
    serve(&mem, &d, t(18, 20), &[t(18, 20), t(18, 15)]);
    mem.put_post(
        &d.download_url(),
        &download_request(VMI, t(18, 15)),
        503,
        "Service Unavailable",
    );
    mem.put(&link(t(18, 20)), frame_at(t(18, 20)));
    mem.put(&link(t(18, 15)), frame_at(t(18, 15)));
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(Dpc::at(TEST_API)), dpc(), VMI);
    // The driver may fail the cycle or keep the frame for the next: either
    // way it is not taken for missing.
    let _ = task.cycle(t(18, 28)).await;
    assert!(
        !engine
            .catalog
            .timeline("radar")
            .parts
            .iter()
            .any(|p| p.id == "dpc-2611061815")
    );
    mem.put_post(
        &d.download_url(),
        &download_request(VMI, t(18, 15)),
        200,
        answer(t(18, 15)),
    );
    task.cycle(t(18, 29)).await.unwrap();
    let ids: Vec<String> = engine
        .catalog
        .timeline("radar")
        .parts
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert!(ids.contains(&"dpc-2611061815".to_string()), "{ids:?}");
}

/// A frame DPC says it has no file for is left alone for a few minutes,
/// then asked for again while the driver still lists it.
#[tokio::test]
async fn a_missing_frame_is_asked_for_again_later() {
    let mem = MemoryRemote::new();
    let d = Dpc::at(TEST_API);
    serve(&mem, &d, t(18, 20), &[t(18, 20)]);
    mem.put(&link(t(18, 20)), frame_at(t(18, 20)));
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(Dpc::at(TEST_API)), dpc(), VMI);
    task.cycle(t(18, 28)).await.unwrap();
    assert!(posts(&mem, &d, t(18, 15)) > 0);
    mem.clear_log();
    task.cycle(t(18, 29)).await.unwrap();
    assert_eq!(posts(&mem, &d, t(18, 15)), 0, "not again at once");
    // Written late after all.
    mem.put_post(
        &d.download_url(),
        &download_request(VMI, t(18, 15)),
        200,
        answer(t(18, 15)),
    );
    mem.put(&link(t(18, 15)), frame_at(t(18, 15)));
    task.cycle(t(18, 28) + MISSING_RETRY + Duration::seconds(30))
        .await
        .unwrap();
    assert_eq!(posts(&mem, &d, t(18, 15)), 1);
    assert!(
        engine
            .catalog
            .timeline("radar")
            .parts
            .iter()
            .any(|p| p.id == "dpc-2611061815")
    );
}

/// An API that keeps announcing an old frame is a failing source, not a
/// quiet one.
#[tokio::test]
async fn a_stalled_api_is_a_failing_source() {
    let mem = MemoryRemote::new();
    let d = Dpc::at(TEST_API);
    serve(&mem, &d, t(17, 0), &[]);
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(Dpc::at(TEST_API)), dpc(), VMI);
    let e = task.cycle(t(18, 28)).await.err().expect("stale");
    assert!(format!("{e:#}").contains("nothing newer than"), "{e:#}");
    assert!(posts(&mem, &d, t(17, 0)) == 0);
}
