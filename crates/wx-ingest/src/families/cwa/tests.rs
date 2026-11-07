use super::*;
use crate::driver::observed::ObservedTask;
use crate::remote::memory::MemoryRemote;
use crate::testing;

const PRODUCT: &str = "O-A0059-001";

use crate::testing::at as t;

fn cwa() -> &'static SourceDef {
    crate::testing::source("cwa")
}

/// The real 18:20 frame (a cut of it), optionally restamped.
fn frame(at: Option<DateTime<Utc>>) -> Vec<u8> {
    let real = testing::fixture("formats/cwa_O-A0059-001_20261106T1820_cols420-660_rows0-300.json");
    match at {
        None => real,
        Some(t) => {
            let local = (t + Duration::hours(8))
                .format("%Y-%m-%dT%H:%M:%S+08:00")
                .to_string();
            String::from_utf8(real)
                .unwrap()
                .replace("2026-11-07T02:20:00+08:00", &local)
                .into_bytes()
        }
    }
}

#[test]
fn the_frame_time_is_in_the_head() {
    let head = &frame(None)[..HEAD_BYTES as usize];
    assert_eq!(date_time(head), Some(t(18, 20)));
    // Cut inside a Chinese character after it, as a ranged read may be.
    let whole = frame(None);
    let end = (750..2000)
        .find(|&n| std::str::from_utf8(&whole[..n]).is_err())
        .unwrap();
    assert_eq!(date_time(&whole[..end]), Some(t(18, 20)));
    assert_eq!(
        date_time(b"{\"DateTime\" : \"2026-11-07T04:30:00+08:00\"}"),
        Some(t(20, 30))
    );
    assert_eq!(date_time(b"{\"DateTime\": \"yesterday\"}"), None);
    assert_eq!(
        date_time(b"{\"sent\": \"2026-11-07T04:30:00+08:00\"}"),
        None
    );
}

/// One ranged read finds the frame, one download makes it; while nothing
/// changes each poll is a 304; a new frame is found by the next poll.
#[tokio::test]
async fn polls_are_conditional_and_frames_whole() {
    let mem = MemoryRemote::new();
    let c = Cwa::default();
    let url = c.url(PRODUCT);
    mem.put(&url, frame(None));
    let (engine, _) = testing::engine(&mem);
    let mut task = ObservedTask::new(engine.clone(), Arc::new(Cwa::default()), cwa(), PRODUCT);
    assert_eq!(task.name, "cwa.radar");
    let r = task.cycle(t(18, 27)).await.unwrap();
    assert_eq!(r.tally.produced, 1, "{:?}", r.tally);
    let part = engine.catalog.latest("radar", "cwa").unwrap();
    assert_eq!(part.id, "cwa-2611061820");
    assert!(
        part.bounds.west > 120.2 && part.bounds.east < 123.3,
        "{:?}",
        part.bounds
    );
    assert_eq!(
        mem.requests(),
        [format!("{url} [bytes=0-4095]"), url.clone()]
    );

    mem.clear_log();
    let bytes = engine.remote.traffic()[0].bytes;
    let r = task.cycle(t(18, 37)).await.unwrap();
    assert_eq!(r.tally, Default::default());
    assert_eq!(mem.requests(), [format!("{url} [bytes=0-4095]")]);
    assert_eq!(engine.remote.traffic()[0].bytes, bytes, "answered 304");

    mem.put(&url, frame(Some(t(18, 30))));
    mem.clear_log();
    let r = task.cycle(t(18, 37) + Duration::seconds(30)).await.unwrap();
    assert_eq!(r.tally.produced, 1, "{:?}", r.tally);
    assert_eq!(
        engine.catalog.latest("radar", "cwa").unwrap().id,
        "cwa-2611061830"
    );
    assert_eq!(mem.requests().len(), 2);
}

/// Overwritten between the poll and the download: that frame is gone, and
/// the next poll finds the new one.
#[tokio::test]
async fn a_frame_overwritten_mid_cycle_is_not_mislabelled() {
    let mem = MemoryRemote::new();
    let c = Cwa::default();
    let url = c.url(PRODUCT);
    mem.put(&url, frame(None));
    let remote = testing::quick_remote(&mem);
    let cx = Cx::new(&remote, "test", t(18, 37));
    let files = c.list(&cx, cwa(), PRODUCT, t(16, 0)).await.unwrap();
    assert_eq!(files[0].valid, t(18, 20));
    mem.put(&url, frame(Some(t(18, 30))));
    let layer = wx_core::layer::layer("radar").unwrap();
    let need = Need {
        input: layer.input_for(cwa()).unwrap(),
        field: None,
        lead: 0,
    };
    assert!(matches!(
        c.fetch(&cx, cwa(), PRODUCT, &files[0], &[need])
            .await
            .unwrap(),
        Fetched::Missing
    ));
    let files = c.list(&cx, cwa(), PRODUCT, t(16, 0)).await.unwrap();
    assert_eq!(files[0].valid, t(18, 30));
    // A document that no longer says when it is from stops the task.
    mem.put(&url, &b"{\"cwaopendata\": {}}"[..]);
    assert!(c.list(&cx, cwa(), PRODUCT, t(16, 0)).await.is_err());
    // Gone, or stuck on an old frame: a failing source, not a quiet one.
    mem.remove(&url);
    assert!(
        c.list(&cx, cwa(), PRODUCT, t(16, 0))
            .await
            .unwrap_err()
            .to_string()
            .contains("gone")
    );
    mem.put(&url, frame(Some(t(17, 30))));
    let e = c.list(&cx, cwa(), PRODUCT, t(16, 0)).await.unwrap_err();
    assert!(e.to_string().contains("stopped"), "{e}");
    // Also while every poll is answered 304.
    let c = Cwa::default();
    mem.put(&url, frame(None));
    let cx = Cx::new(&remote, "test", t(18, 37));
    assert!(c.list(&cx, cwa(), PRODUCT, t(16, 0)).await.is_ok());
    let cx = Cx::new(&remote, "test", t(19, 30));
    assert!(c.list(&cx, cwa(), PRODUCT, t(16, 0)).await.is_err());
}

#[tokio::test]
#[ignore = "network"]
async fn live_cwa_head_and_304() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let c = Cwa::default();
    let files = c
        .list(&cx, cwa(), PRODUCT, now - Duration::hours(1))
        .await
        .unwrap();
    eprintln!("CWA: newest {}", files[0].valid);
    assert!(now - files[0].valid < Duration::minutes(30));
    let again = c
        .list(&cx, cwa(), PRODUCT, now - Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(again, files);
    eprintln!("{:?}", remote.traffic());
}
