use super::*;
use crate::driver::model::ModelTask;
use crate::driver::observed::ObservedTask;
use crate::remote::memory::MemoryRemote;
use crate::testing;
use std::sync::Arc;
use wx_core::source;

/// A host the test remote does not space requests on.
const TEST_BASE: &str = "https://dwd.example.org/weather/radar/composite";
const HX: &str = "hx";
const WN: &str = "wn";

use crate::testing::hms as t;

fn radar() -> &'static SourceDef {
    crate::testing::source("dwd_radar")
}

fn nowcast() -> &'static SourceDef {
    crate::testing::source("dwd_nowcast")
}

fn obs_url(v: DateTime<Utc>) -> String {
    format!("{}{}", dir_url(TEST_BASE, HX), obs_name(HX, v))
}

/// An nginx index of the `hx` directory listing `times` and the alias of
/// the last.
fn index(times: &[DateTime<Utc>]) -> String {
    let mut html = String::from(
        "<html>\r\n<head><title>Index of /weather/radar/composite/hx/</title></head>\r\n<body>\r\n\
         <h1>Index of /weather/radar/composite/hx/</h1><hr><pre><a href=\"../\">../</a>\r\n",
    );
    let latest = times
        .last()
        .map(|v| ("composite_hx_LATEST-hd5".to_string(), *v));
    for (name, v) in times.iter().map(|v| (obs_name(HX, *v), *v)).chain(latest) {
        html.push_str(&format!(
            "<a href=\"{name}\">{name}</a>                     {}              990978\r\n",
            (v + Duration::seconds(94)).format("%d-%b-%Y %H:%M:%S")
        ));
    }
    html.push_str("</pre><hr></body>\r\n</html>\r\n");
    html
}

#[test]
fn names() {
    assert_eq!(obs_name(HX, t(18, 15, 0)), "composite_hx_20261106_1815-hd5");
    assert_eq!(
        parse_obs_name(HX, "composite_hx_20261106_1815-hd5"),
        Some(t(18, 15, 0))
    );
    assert_eq!(parse_obs_name(HX, "composite_hx_LATEST-hd5"), None);
    assert_eq!(parse_obs_name(HX, "composite_dmax_20261106_1815-hd5"), None);
    assert_eq!(parse_obs_name(HX, "composite_hx_20261106_1815.h5"), None);
    assert_eq!(tar_name(WN, t(18, 15, 0)), "composite_wn_20261106_1815.tar");
    let run = t(18, 15, 0);
    assert_eq!(
        member_lead(WN, run, "composite_wn_20261106_1815_060-hd5"),
        Some(60)
    );
    assert_eq!(
        member_lead(WN, run, "x/composite_wn_20261106_1815_000-hd5"),
        Some(0)
    );
    assert_eq!(
        member_lead(WN, run, "composite_wn_20261106_1810_060-hd5"),
        None,
        "another run"
    );
    assert_eq!(
        member_lead(WN, run, "composite_rv_20261106_1815_060-hd5"),
        None
    );
    assert_eq!(
        member_lead(WN, run, "composite_wn_20261106_1815_60-hd5"),
        None
    );
    assert!(index(&[t(18, 10, 0), t(18, 15, 0)]).contains("composite_hx_LATEST-hd5"));
}

#[test]
fn nowcast_runs_and_leads() {
    let n = DwdNowcast::default();
    let c = n.cycle(nowcast());
    assert_eq!(c.every, Duration::minutes(10));
    assert_eq!(
        c.runs(t(18, 27, 0), Duration::minutes(20))[..3],
        [t(18, 20, 0), t(18, 10, 0), t(18, 0, 0)]
    );
    let leads = n.leads(nowcast(), WN, t(18, 20, 0));
    assert_eq!(
        (leads.len(), leads[0], leads[23]),
        (24, 5, 120),
        "lead 0 is the observation's"
    );
    assert_eq!(n.due(nowcast(), WN, t(18, 20, 0), 120), t(18, 23, 20));
    assert_eq!(
        DwdNowcast::default()
            .following_every(3)
            .cycle(nowcast())
            .every,
        Duration::minutes(5)
    );
    assert_eq!(
        DwdNowcast::default()
            .following_every(12)
            .cycle(nowcast())
            .every,
        Duration::minutes(15)
    );
    assert!(n.check(nowcast()).is_ok() && DwdRadar::default().check(radar()).is_ok());
}

/// The archive is split as the `tar` crate reads it, without copying.
#[test]
fn tar_members_are_the_archive_s_files() {
    let tar = Bytes::from(testing::fixture(
        "formats/dwd_composite_wn_20261106_1815_cut4of25.tar",
    ));
    let Tar { members, complete } = tar_members(&tar).unwrap();
    assert!(complete);
    let run = t(18, 15, 0);
    let leads: Vec<Option<u32>> = members
        .iter()
        .map(|(n, _)| member_lead(WN, run, n))
        .collect();
    assert_eq!(leads, [Some(0), Some(5), Some(60), Some(120)]);
    let steps = wx_formats::dwd::decode_tar(&tar).unwrap();
    for ((_, m), s) in members.iter().zip(&steps) {
        let one = wx_formats::dwd::decode(m).unwrap();
        assert_eq!((one.valid, one.lead), (s.valid, s.lead));
        assert!(
            m.as_ptr() >= tar.as_ptr() && m.as_ptr() < tar.as_ptr().wrapping_add(tar.len()),
            "a slice of the tar"
        );
    }
    assert!(tar_members(&Bytes::new()).unwrap().members.is_empty());
    // Cut between two members, as a tar being written is: whole members,
    // but not complete.
    let cut = members[1].1.as_ptr() as usize - tar.as_ptr() as usize + members[1].1.len();
    let cut = tar.slice(..cut.div_ceil(512) * 512);
    let part = tar_members(&cut).unwrap();
    assert_eq!((part.members.len(), part.complete), (2, false));
    assert!(tar_members(&Bytes::from_static(b"definitely not a tar")).is_err());
    let mut bad = tar.to_vec();
    bad[3] ^= 0x20;
    assert!(
        tar_members(&Bytes::from(bad)).is_err(),
        "a damaged header fails its checksum"
    );
    assert!(
        tar_members(&tar.slice(..tar.len() / 2)).is_err(),
        "a member cut short"
    );
}

/// The first cycle reads the index; the real 18:15 composite becomes
/// `dwd_radar-2611061815`.
#[tokio::test]
async fn an_observation_becomes_a_part() {
    let mem = MemoryRemote::new();
    mem.put(&dir_url(TEST_BASE, HX), index(&[t(18, 15, 0)]));
    mem.put(
        &obs_url(t(18, 15, 0)),
        testing::fixture("formats/dwd_composite_hx_20261106_1815-hd5"),
    );
    let (engine, store) = testing::engine(&mem);
    let mut task = ObservedTask::new(
        engine.clone(),
        Arc::new(DwdRadar::at(TEST_BASE)),
        radar(),
        HX,
    );
    assert_eq!(task.name, "dwd_radar.radar");
    let report = task.cycle(t(18, 17, 0)).await.unwrap();
    assert_eq!(report.tally.produced, 1, "{:?}", report.tally);
    let part = engine.catalog.latest("radar", "dwd_radar").unwrap();
    assert_eq!(part.id, "dwd_radar-2611061815");
    assert_eq!(part.max_zoom, radar().max_zoom);
    assert!(
        part.bounds.west < 5.0 && part.bounds.east > 16.0 && part.bounds.north > 55.5,
        "{:?}",
        part.bounds
    );
    assert!(
        testing::keys(store.as_ref())
            .await
            .contains(&wx_core::keys::grid("radar", &part.id))
    );
    assert_eq!(
        mem.requests(),
        [dir_url(TEST_BASE, HX), obs_url(t(18, 15, 0))]
    );
}

/// Once the recent times are accounted for, a look asks for the first byte
/// of the next file expected, nothing before it can exist, and the file
/// itself once that byte is there. After an outage, more times are missing
/// than are worth asking after one by one: the index is read instead.
#[tokio::test]
async fn new_observations_are_found_by_probing() {
    let mem = MemoryRemote::new();
    let times: Vec<_> = (1..5)
        .map(|k| t(18, 0, 0) + Duration::minutes(5 * k))
        .collect();
    mem.put(&dir_url(TEST_BASE, HX), index(&times));
    let remote = testing::quick_remote(&mem);
    let d = DwdRadar::at(TEST_BASE);
    let look = |h, m, s, from| {
        let (d, remote) = (&d, &remote);
        async move {
            let cx = Cx::new(remote, "test", t(h, m, s));
            let files = d.list(&cx, radar(), HX, from).await.unwrap();
            files.into_iter().map(|f| f.valid).collect::<Vec<_>>()
        }
    };
    assert_eq!(look(18, 22, 0, t(18, 5, 0)).await, times);
    assert_eq!(mem.requests(), [dir_url(TEST_BASE, HX)]);
    // The index's sizes and links are kept.
    let cx = Cx::new(&remote, "test", t(18, 22, 0));
    let files = d.list(&cx, radar(), HX, t(18, 5, 0)).await.unwrap();
    assert_eq!(
        (files[0].name.as_str(), files[0].size),
        (obs_url(t(18, 5, 0)).as_str(), Some(990978))
    );

    // 18:25 cannot be there yet: nothing is asked.
    mem.clear_log();
    assert_eq!(look(18, 25, 50, t(18, 10, 0)).await.len(), 3);
    assert!(mem.requests().is_empty(), "{:?}", mem.requests());
    // Then one byte of it, not written yet, and again.
    look(18, 26, 10, t(18, 10, 0)).await;
    look(18, 26, 30, t(18, 10, 0)).await;
    let probe = format!("{} [bytes=0-0]", obs_url(t(18, 25, 0)));
    assert_eq!(mem.requests(), [probe.clone(), probe.clone()]);
    // Written: one more byte, and it is listed from then on.
    mem.put(&obs_url(t(18, 25, 0)), &b"not a composite"[..]);
    mem.clear_log();
    assert_eq!(
        look(18, 26, 50, t(18, 10, 0)).await.last(),
        Some(&t(18, 25, 0))
    );
    assert_eq!(
        look(18, 27, 10, t(18, 10, 0)).await.last(),
        Some(&t(18, 25, 0))
    );
    assert_eq!(mem.requests(), [probe]);

    // Twenty minutes unaccounted for: the index again.
    mem.clear_log();
    look(18, 51, 0, t(18, 25, 0)).await;
    assert_eq!(mem.requests(), [dir_url(TEST_BASE, HX)]);
}

fn nowcast_group() -> Vec<&'static str> {
    vec![WN]
}

/// One tar of the real 18:15 run (its leads 0, 5, 60 and 120) feeds every
/// lead it holds; the analysis at lead 0 is left to `hx`. The cut-down tar
/// is not whole, so it is taken only once the run is `WHOLE_BY` old, and
/// the leads it lacks are given up then.
#[tokio::test]
async fn one_tar_feeds_every_nowcast_lead() {
    let mem = MemoryRemote::new();
    let run = t(18, 15, 0);
    let url = format!("{}{}", dir_url(TEST_BASE, WN), tar_name(WN, run));
    mem.put(
        &url,
        testing::fixture("formats/dwd_composite_wn_20261106_1815_cut4of25.tar"),
    );
    let (engine, _) = testing::engine(&mem);
    let model = Arc::new(DwdNowcast::at(TEST_BASE).following_every(5));
    let mut task = ModelTask::new(engine.clone(), model, nowcast(), &nowcast_group());
    let r = task
        .cycle(run + WHOLE_BY + Duration::minutes(1), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(
        (r.tally.produced, r.tally.unavailable, r.tally.failed),
        (3, 21, 0),
        "{:?}",
        r.tally
    );
    let mut ids: Vec<String> = engine
        .catalog
        .timeline("radar")
        .parts
        .into_iter()
        .filter(|p| p.source == "dwd_nowcast")
        .map(|p| p.id)
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "dwd_nowcast-2611061815-120",
            "dwd_nowcast-2611061815-5",
            "dwd_nowcast-2611061815-60"
        ]
    );
    // One download for all of them, or one for each of the driver's two
    // passes (valid times up to now, then ahead).
    let gets = mem.requests().iter().filter(|r| r.as_str() == url).count();
    assert!((1..=2).contains(&gets), "{:?}", mem.requests());
    let p = engine.catalog.latest("radar", "dwd_nowcast").unwrap();
    assert_eq!(
        (p.run, p.valid, p.max_zoom),
        (Some(run), t(20, 15, 0), nowcast().max_zoom)
    );
}

/// A tar not there yet is looked for by its first byte until it is; runs
/// older than `OLDEST_RUN` are not looked for at all; a tar whose members
/// belong to another run is refused; a tar not whole yet is waited for.
#[tokio::test]
async fn a_late_tar_is_looked_for_again() {
    let mem = MemoryRemote::new();
    let (engine, _) = testing::engine(&mem);
    let model = Arc::new(DwdNowcast::at(TEST_BASE).following_every(5));
    let mut task = ModelTask::new(engine.clone(), model, nowcast(), &nowcast_group());
    let run = t(18, 15, 0);
    let url = format!("{}{}", dir_url(TEST_BASE, WN), tar_name(WN, run));
    // No tar at all for the last 17 minutes: the nowcast has stopped, which
    // is the task failing, not a quiet one.
    let e = task
        .cycle(t(18, 17, 0), &|_, _| {})
        .await
        .expect_err("a stopped nowcast");
    assert!(format!("{e:#}").contains("stopped"), "{e:#}");
    let probes: Vec<String> = mem.requests();
    let probed = |r: DateTime<Utc>| probes.iter().any(|p| p.contains(&tar_name(WN, r)));
    assert!(probed(t(18, 0, 0)) && probed(t(18, 10, 0)), "{probes:?}");
    // Nothing of the 17:55 run or before.
    assert!(!probed(t(17, 55, 0)), "{probes:?}");
    // The 18:15 tar published as 18:05's is caught by its members' run; the
    // real one, not whole, waits.
    let wrong = format!("{}{}", dir_url(TEST_BASE, WN), tar_name(WN, t(18, 5, 0)));
    mem.put(
        &wrong,
        testing::fixture("formats/dwd_composite_wn_20261106_1815_cut4of25.tar"),
    );
    mem.put(
        &url,
        testing::fixture("formats/dwd_composite_wn_20261106_1815_cut4of25.tar"),
    );
    let r = task.cycle(t(18, 23, 30), &|_, _| {}).await.unwrap();
    assert_eq!(
        (r.tally.produced, r.tally.failed, r.tally.unavailable),
        (0, 24, 0),
        "{:?}",
        r.tally
    );
    let r = task
        .cycle(run + WHOLE_BY + Duration::minutes(1), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(r.tally.produced, 3, "{:?}", r.tally);
    assert!(
        engine
            .catalog
            .timeline("radar")
            .parts
            .iter()
            .all(|p| p.run == Some(run))
    );
}

/// Both halves start, each under its own name, from one registration.
#[test]
fn both_halves_get_their_own_tasks() {
    let store: Arc<dyn object_store::ObjectStore> = Arc::new(object_store::memory::InMemory::new());
    let catalog = Arc::new(wx_core::FrameCatalog::new(wx_core::Hub::new()));
    let config = crate::IngestConfig {
        feeds: ["opera", "dwd_radar", "dpc", "jma", "cwa"]
            .map(String::from)
            .to_vec(),
        ..crate::IngestConfig::default()
    };
    let mut names = crate::Ingest::new(store, catalog, config)
        .unwrap()
        .task_names();
    names.sort();
    assert_eq!(
        names,
        [
            "cwa.radar",
            "dpc.radar",
            "dwd_nowcast",
            "dwd_radar.radar",
            "janitor",
            "jma_nowcast",
            "jma_radar.radar",
            "opera.radar",
            "opera.rain_1h",
        ]
    );
    let config = crate::IngestConfig::default();
    let adapter = |id| crate::families::adapter(source::source(id).unwrap(), &config);
    assert!(matches!(
        adapter("dwd_radar"),
        Some(crate::families::Adapter::Observed(_))
    ));
    assert!(matches!(
        adapter("dwd_nowcast"),
        Some(crate::families::Adapter::Model(_))
    ));
}

#[tokio::test]
#[ignore = "network"]
async fn live_dwd_newest_files() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let d = DwdRadar::default();
    let files = d
        .list(&cx, radar(), HX, now - Duration::minutes(30))
        .await
        .unwrap();
    let newest = files
        .iter()
        .max_by_key(|f| f.valid)
        .expect("an hx file in 30 minutes");
    eprintln!("hx: {} files, newest {}", files.len(), newest.valid);
    assert!(now - newest.valid < Duration::minutes(15));
    let again = d
        .list(&cx, radar(), HX, newest.valid - Duration::minutes(10))
        .await
        .unwrap();
    assert!(again.iter().any(|f| f.valid == newest.valid));
    eprintln!("{:?}", remote.traffic());
}

/// The time and memory the 21-million-cell build takes: run in release,
/// alone, under `/usr/bin/time -l`.
#[test]
#[ignore = "timing"]
fn timing_the_250_m_composite_build() {
    let bytes = testing::fixture("formats/dwd_composite_hx_20261106_1815-hd5");
    let layer = wx_core::layer::layer("radar").unwrap();
    let input = layer.input_for(radar()).unwrap();
    let need = Need {
        input,
        field: None,
        lead: 0,
    };
    let start = std::time::Instant::now();
    let d = decoder(None)(&bytes, &need).unwrap();
    let decoded = start.elapsed();
    let raster = wx_pipeline::derive(layer, input, &[d.field]).unwrap();
    let derived = start.elapsed();
    let grid = wx_pipeline::FrameGrid::build(layer, &raster);
    drop(raster);
    let built = start.elapsed();
    let stored = grid.to_bytes();
    let solid = grid.solid();
    eprintln!(
        "DWD hx: decode {:.2} s, derive {:.2} s, grid {:.2} s, store {:.2} s; {} levels, {:.1} MB stored, solid {solid:?}",
        decoded.as_secs_f64(),
        (derived - decoded).as_secs_f64(),
        (built - derived).as_secs_f64(),
        (start.elapsed() - built).as_secs_f64(),
        grid.level_count(),
        stored.len() as f64 / 1e6
    );
}

/// A tar cut off between two members (being written, or a short read) is
/// looked for again until it is whole, rather than giving up the leads it
/// lacks.
#[tokio::test]
async fn a_tar_cut_between_members_is_not_taken() {
    let mem = MemoryRemote::new();
    let run = t(18, 15, 0);
    let url = format!("{}{}", dir_url(TEST_BASE, WN), tar_name(WN, run));
    let tar = Bytes::from(testing::fixture(
        "formats/dwd_composite_wn_20261106_1815_cut4of25.tar",
    ));
    let second = tar_members(&tar).unwrap().members[1].1.clone();
    let end = (second.as_ptr() as usize - tar.as_ptr() as usize + second.len()).div_ceil(512) * 512;
    mem.put(&url, tar.slice(..end));
    let (engine, _) = testing::engine(&mem);
    let model = Arc::new(DwdNowcast::at(TEST_BASE).following_every(5));
    let mut task = ModelTask::new(engine.clone(), model, nowcast(), &nowcast_group());
    let r = task.cycle(t(18, 19, 0), &|_, _| {}).await.unwrap();
    assert_eq!(
        r.tally,
        Default::default(),
        "nothing made, nothing given up"
    );
    assert!(r.wake <= t(18, 21, 0), "{}", r.wake);
    // Whole now (as whole as the fixture gets): every lead it has is made.
    mem.put(&url, tar);
    let r = task
        .cycle(run + WHOLE_BY + Duration::minutes(1), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(r.tally.produced, 3, "{:?}", r.tally);
}

/// An observed product with nothing new for half an hour is a failing
/// task, not a quiet one.
#[tokio::test]
async fn a_stopped_composite_fails_the_task() {
    let mem = MemoryRemote::new();
    mem.put(
        &dir_url(TEST_BASE, HX),
        index(&[t(18, 0, 0), t(18, 5, 0), t(18, 10, 0), t(18, 15, 0)]),
    );
    let remote = testing::quick_remote(&mem);
    let d = DwdRadar::at(TEST_BASE);
    let cx = Cx::new(&remote, "test", t(18, 30, 0));
    assert_eq!(
        d.list(&cx, radar(), HX, t(18, 0, 0)).await.unwrap().len(),
        4
    );
    let cx = Cx::new(&remote, "test", t(18, 50, 0));
    let e = d.list(&cx, radar(), HX, t(18, 5, 0)).await.unwrap_err();
    assert!(e.to_string().contains("stopped"), "{e}");
}
