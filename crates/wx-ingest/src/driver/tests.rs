//! A fake family driven end to end against the in-memory network: listing,
//! inventories and byte ranges, a family decoder, step differencing across
//! files, one file feeding many parts, insertion, expiry and deletion.

use super::model::{Cycle, Listing, Model, ModelFile, ModelTask};
use super::*;
use crate::idx;
use crate::janitor;
use crate::remote::memory::MemoryRemote;
use crate::testing::{self, leak};
use chrono::{Duration, TimeZone};
use object_store::ObjectStore;
use object_store::memory::InMemory;
use std::sync::LazyLock;
use wx_core::layer::{Derive, FrameKind, layer};
use wx_core::proj::LatLonGrid;
use wx_core::{Family, GridProjection, InputSpec, LayerSpec, SOURCES, SourceDef, keys};

const BASE: &str = "https://example.org/fake";
const IDENTITY: Derive = Derive::Linear { mul: 1.0, add: 0.0 };

/// `t`: an hourly field that fills the past. `p`: an hourly accumulation per
/// hour, made from run totals. `r`: a half-hourly field, every lead of a run
/// in one file.
struct Registry {
    layers: &'static [LayerSpec],
    sources: &'static [SourceDef],
}

fn input(product: &'static str, var: &'static str, stat: Stat, hold: u32, past: bool) -> InputSpec {
    InputSpec {
        family: Family::Gfs,
        sources: &[],
        product,
        fields: leak(vec![FieldSel {
            var,
            level: "sfc",
            stat,
        }]),
        derive: IDENTITY,
        hold_minutes: hold,
        fill_past: past,
    }
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let row = |id: &'static str, like: &str, input: InputSpec| LayerSpec {
        id,
        inputs: leak(vec![input]),
        anchored: false,
        ..*layer(like).unwrap()
    };
    Registry {
        layers: leak(vec![
            row("t", "temp", input("hourly", "TMP", Stat::Instant, 60, true)),
            row(
                "p",
                "rain_1h",
                input("hourly", "APCP", Stat::StepPerHour, 60, false),
            ),
            row("r", "radar", input("pack", "REF", Stat::Instant, 30, false)),
            LayerSpec {
                retention_minutes: 120,
                observed_tick_minutes: 10,
                ..row(
                    "s",
                    "radar",
                    InputSpec {
                        family: Family::Mrms,
                        ..input("obs", "SAT", Stat::Instant, 150, false)
                    },
                )
            },
        ]),
        sources: leak(vec![
            SourceDef {
                id: "fake",
                family: Family::Gfs,
                region: "",
                kind: FrameKind::Forecast,
                rank: 100,
                max_zoom: 4,
                ..SOURCES[0]
            },
            SourceDef {
                id: "obs",
                family: Family::Mrms,
                region: "",
                kind: FrameKind::Observed,
                rank: 900,
                max_zoom: 4,
                ..SOURCES[0]
            },
        ]),
    }
});

fn run_label(run: DateTime<Utc>) -> String {
    run.format("%Y%m%d%H").to_string()
}

/// A constant field on a 20 x 15 degree grid over Europe.
fn constant(v: f32) -> Field {
    let (nx, ny) = (41, 31);
    Field {
        nx,
        ny,
        proj: GridProjection::LatLon(LatLonGrid {
            lon0: -10.0,
            lat0: 55.0,
            dlon: 0.5,
            dlat: -0.5,
            periodic: false,
        }),
        values: vec![v; nx * ny],
        grid_relative_winds: false,
    }
}

/// Records are plain numbers; the pack file is `VAR LEAD VALUE` lines.
fn number(bytes: &[u8], _: &Need) -> anyhow::Result<Decoded> {
    let v: f32 = std::str::from_utf8(bytes)?.trim().parse()?;
    Ok(Decoded {
        field: constant(v),
        valid: None,
    })
}

fn pack_line(bytes: &[u8], need: &Need) -> anyhow::Result<Decoded> {
    let var = need.field.map_or("", |f| f.var);
    let text = std::str::from_utf8(bytes)?;
    let line = text
        .lines()
        .find(|l| {
            l.split_whitespace()
                .take(2)
                .eq([var, need.lead.to_string().as_str()])
        })
        .ok_or_else(|| anyhow::anyhow!("no {var} at {}", need.lead))?;
    let v: f32 = line.split_whitespace().nth(2).unwrap_or("").parse()?;
    Ok(Decoded {
        field: constant(v),
        valid: None,
    })
}

/// Runs every six hours; `hourly` has one file per lead with an `.idx`
/// beside it and is fetched by range, `pack` one file per run fetched whole.
struct Fake {
    bucket: S3Bucket,
    /// When a run's first file is due after its run time.
    lag: Duration,
    /// Files whose name ends so take this long to fetch.
    slow: Option<(&'static str, std::time::Duration)>,
    /// Temperature at lead 0 is known not to exist (and is not served).
    no_temperature_at_0: bool,
}

impl Fake {
    fn new(bucket: S3Bucket) -> Self {
        Self {
            bucket,
            lag: Duration::hours(1),
            slow: None,
            no_temperature_at_0: false,
        }
    }

    fn lacks(&self, need: &Need) -> bool {
        self.no_temperature_at_0 && need.lead == 0 && need.field.is_some_and(|f| f.var == "TMP")
    }
}

#[async_trait::async_trait]
impl Model for Fake {
    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::hours(6),
            spare: 1,
            relist: Duration::minutes(1),
            give_up: Duration::minutes(30),
        }
    }

    fn leads(&self, _: &SourceDef, product: &str, _: DateTime<Utc>) -> Vec<u32> {
        match product {
            "pack" => (0..=360).step_by(30).collect(),
            _ => (0..=360).step_by(60).collect(),
        }
    }

    fn due(&self, _: &SourceDef, _: &str, run: DateTime<Utc>, lead: u32) -> DateTime<Utc> {
        run + self.lag + Duration::minutes(lead as i64 / 60 * 5)
    }

    fn absent(&self, _: &SourceDef, _: DateTime<Utc>, need: &Need) -> bool {
        self.lacks(need)
    }

    fn file(&self, _: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        match need.product() {
            "pack" => format!("{}/pack.txt", run_label(run)),
            _ => format!("{}/f{:03}.dat", run_label(run), need.lead / 60),
        }
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        run: DateTime<Utc>,
        _: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let listed = cx
            .list_s3(&self.bucket, &format!("{}/", run_label(run)), None)
            .await?;
        let mut listing = Listing::with_sidecars(&listed, ".idx");
        for l in listed.iter().filter(|l| l.key.ends_with("pack.txt")) {
            listing.add(l.key.clone(), Some(l.size));
        }
        Ok(Some(listing))
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: DateTime<Utc>,
        file: &ModelFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let url = self.bucket.url(&file.name);
        if let Some((suffix, d)) = self.slow
            && file.name.ends_with(suffix)
        {
            tokio::time::sleep(d).await;
        }
        if file.name.ends_with("pack.txt") {
            return cx
                .whole(
                    &url,
                    needs,
                    1 << 20,
                    Payload::decoded_by(Arc::new(pack_line)),
                )
                .await;
        }
        let Some(text) = cx.get(format!("{url}.idx")).bytes().await? else {
            return Ok(Fetched::Missing);
        };
        let inventory = idx::parse(&String::from_utf8_lossy(&text));
        let decode: Decoder = Arc::new(number);
        let mut out = Vec::new();
        for n in needs {
            let Some(e) = n
                .field
                .and_then(|f| idx::find(&inventory, &f, n.lead))
                .filter(|_| !self.lacks(n))
            else {
                out.push(None);
                continue;
            };
            let Some(bytes) = cx
                .get(&url)
                .range(e.range.bounded(file.size))
                .bytes()
                .await?
            else {
                return Ok(Fetched::Missing);
            };
            out.push(Some(Payload::Custom {
                bytes,
                decode: decode.clone(),
            }));
        }
        Ok(Fetched::Got(out))
    }
}

const OBS_BASE: &str = "https://example.org/obs";

/// One file per half hour, `obs/{YYYYMMDDHHMM}.txt`, holding a number.
struct FakeObs {
    bucket: S3Bucket,
}

#[async_trait::async_trait]
impl Observed for FakeObs {
    fn cadence(&self, _: &SourceDef, _: &str) -> Cadence {
        Cadence::every(
            Duration::minutes(30),
            Duration::minutes(5),
            Duration::minutes(1),
        )
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: &str,
        from: DateTime<Utc>,
    ) -> Result<Vec<ObsFile>, FetchError> {
        let listed = cx.list_s3(&self.bucket, "obs/", None).await?;
        Ok(listed
            .into_iter()
            .filter_map(|l| {
                let stamp = l.key.strip_prefix("obs/")?.strip_suffix(".txt")?;
                let valid = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M")
                    .ok()?
                    .and_utc();
                (valid >= from).then(|| ObsFile {
                    valid,
                    name: l.key.clone(),
                    size: Some(l.size),
                })
            })
            .collect())
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        cx.whole(
            &self.bucket.url(&file.name),
            needs,
            1 << 20,
            Payload::decoded_by(Arc::new(number)),
        )
        .await
    }
}

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 6, 0, 0, 0).unwrap()
}

fn temp_at(lead: u32) -> f32 {
    10.0 + lead as f32 / 60.0
}

/// Run totals: 2 mm in the first hour, 3 in the second, then 1 an hour, and
/// a small decrease (packing noise) in the last.
fn total_at(lead: u32) -> f32 {
    match lead / 60 {
        0 => 0.0,
        1 => 2.0,
        2 => 5.0,
        h if h < 6 => 5.0 + (h - 2) as f32,
        _ => 7.9,
    }
}

/// Publish hourly file `hour` of the run at `t0`.
fn publish_hour(mem: &MemoryRemote, bucket: &S3Bucket, hour: u32) {
    publish_hour_of(mem, bucket, t0(), hour);
}

/// Publish hourly file `hour` of `run`.
fn publish_hour_of(mem: &MemoryRemote, bucket: &S3Bucket, run: DateTime<Utc>, hour: u32) {
    let lead = hour * 60;
    let time = if hour == 0 {
        "anl".to_string()
    } else {
        format!("{hour} hour fcst")
    };
    let tmp = format!("{}", temp_at(lead));
    let apcp = format!("{}", total_at(lead));
    let idx = format!(
        "1:0:d=2026110600:TMP:sfc:{time}:\n2:{}:d=2026110600:APCP:sfc:0-{hour} hour acc fcst:\n",
        tmp.len()
    );
    let key = format!("{}/f{hour:03}.dat", run_label(run));
    mem.put_s3(bucket, &key, format!("{tmp}{apcp}"));
    mem.put_s3(bucket, &format!("{key}.idx"), idx);
}

fn publish_pack(mem: &MemoryRemote, bucket: &S3Bucket) {
    let text: String = (0..=360)
        .step_by(30)
        .map(|l| format!("REF {l} {}\n", 20 + l / 30))
        .collect();
    mem.put_s3(bucket, &format!("{}/pack.txt", run_label(t0())), text);
}

/// The value a stored part shows, read back through a rendered tile.
async fn stored_value(store: &dyn ObjectStore, layer: &str, id: &str) -> f32 {
    let grid = testing::stored_grid(store, layer, id).await;
    let tile = wx_tile::decode(
        &grid
            .render(wx_core::TileCoord::new(0, 0, 0).unwrap())
            .unwrap(),
    )
    .unwrap();
    let q = *tile.bands[0]
        .iter()
        .find(|&&q| q != wx_tile::NODATA)
        .unwrap();
    layer_of(layer).bands[0].dequantize(q).unwrap()
}

/// The fake family's bucket, in memory.
fn fake_host() -> (Arc<MemoryRemote>, S3Bucket) {
    let mem = MemoryRemote::new();
    let bucket = S3Bucket::at(BASE);
    mem.serve_s3(&bucket, 1000);
    (mem, bucket)
}

/// An engine over the test registry reading from `mem`.
fn fake_engine(mem: &Arc<MemoryRemote>) -> Arc<crate::engine::Engine> {
    testing::engine_with(
        mem,
        Arc::new(InMemory::new()),
        REGISTRY.layers,
        REGISTRY.sources,
    )
}

fn layer_of(id: &str) -> &'static LayerSpec {
    REGISTRY.layers.iter().find(|l| l.id == id).unwrap()
}

fn part_ids(engine: &crate::engine::Engine, layer: &str) -> Vec<String> {
    let mut ids: Vec<String> = engine
        .catalog
        .timeline(layer)
        .parts
        .into_iter()
        .map(|p| p.id)
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn a_fake_family_end_to_end() {
    let (mem, bucket) = fake_host();
    for h in 0..=4 {
        publish_hour(&mem, &bucket, h);
    }
    publish_pack(&mem, &bucket);
    let engine = fake_engine(&mem);
    let store = engine.store.clone();
    let source = &REGISTRY.sources[0];
    let mut task = ModelTask::new(
        engine.clone(),
        Arc::new(Fake::new(bucket.clone())),
        source,
        &["hourly", "pack"],
    );

    // 01:30: f000-f004 and the pack are out; f005 and f006 are not.
    let now = t0() + Duration::minutes(90);
    let report = task.cycle(now, &|_, _| {}).await.unwrap();
    // t: leads 0-4 h (the past is filled); p: 1-4 h (no step at 0); r: the
    // half hours from 00:30 (00:00 is more than 90 minutes old by the time
    // it would be stored).
    assert_eq!(report.tally.produced, 5 + 4 + 12, "{:?}", report.tally);
    assert_eq!(report.tally.failed + report.tally.unavailable, 0);
    let requests = mem.requests();
    // The run is listed once. (So are the two before it, whose past hours
    // temperature still wants; their files are not there.)
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.contains("prefix=2026110600"))
            .count(),
        1,
        "{requests:#?}"
    );
    assert_eq!(
        requests.iter().filter(|r| r.ends_with("pack.txt")).count(),
        1,
        "one fetch feeds twelve parts"
    );
    // f002 holds t at 2 h and p's total at 2 h, which is also where p's
    // step to 3 h starts: one range each (this family does not merge them),
    // the total asked for once.
    assert_eq!(
        requests.iter().filter(|r| r.contains("f002.dat [")).count(),
        2,
        "{requests:#?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.ends_with("f002.dat.idx"))
            .count(),
        1
    );
    assert_eq!(task.newest_listed(), Some((t0(), 6)));
    // The next look is the relist for f005 and f006, which are due.
    assert!(
        report.wake <= now + Duration::minutes(1) + Duration::seconds(1),
        "{}",
        report.wake
    );

    assert_eq!(part_ids(&engine, "t").len(), 5);
    assert_eq!(
        stored_value(store.as_ref(), "t", "fake-26110600-180").await,
        temp_at(180)
    );
    // Per hour over each step: from zero at the first, then differences.
    assert_eq!(
        stored_value(store.as_ref(), "p", "fake-26110600-60").await,
        2.0
    );
    assert_eq!(
        stored_value(store.as_ref(), "p", "fake-26110600-120").await,
        3.0
    );
    assert_eq!(
        stored_value(store.as_ref(), "p", "fake-26110600-240").await,
        1.0
    );
    assert_eq!(
        stored_value(store.as_ref(), "r", "fake-26110600-90").await,
        23.0
    );
    assert!(
        engine
            .catalog
            .latest("p", "fake")
            .is_some_and(|x| x.id == "fake-26110600-240")
    );

    // f005 and f006 arrive. The step at f005 starts at f004, whose total
    // was kept from the last cycle: f004 is not fetched again.
    assert_eq!(task.kept_totals(), 1, "f004's total");
    publish_hour(&mem, &bucket, 5);
    publish_hour(&mem, &bucket, 6);
    mem.clear_log();
    let later = now + Duration::minutes(2);
    let report = task.cycle(later, &|_, _| {}).await.unwrap();
    assert_eq!(report.tally.produced, 2 + 2, "{:?}", report.tally);
    let requests = mem.requests();
    assert!(
        !requests.iter().any(|r| r.contains("f004.dat")),
        "{requests:#?}"
    );
    assert_eq!(
        stored_value(store.as_ref(), "p", "fake-26110600-300").await,
        1.0
    );
    // A decrease is passed on and stored as the band's floor.
    assert_eq!(
        stored_value(store.as_ref(), "p", "fake-26110600-360").await,
        0.0
    );

    // Nothing is left to do: nothing is asked for, and no total is kept.
    mem.clear_log();
    let report = task
        .cycle(later + Duration::minutes(2), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(report.tally, Default::default());
    assert!(mem.requests().is_empty(), "{:#?}", mem.requests());
    assert_eq!(task.kept_totals(), 0);

    // Expiry and deletion: half a day on, only temperature's last hours are
    // still in its window, and everything else goes.
    let stored = testing::keys(store.as_ref()).await;
    assert_eq!(stored.len(), 7 + 6 + 12);
    let next_day = t0() + Duration::hours(17) + Duration::minutes(30);
    let (dropped, deleted) =
        janitor::pass(store.as_ref(), &engine.catalog, &engine.deletions, next_day)
            .await
            .unwrap();
    assert!(dropped >= 6 + 12, "{dropped}");
    assert_eq!(deleted, 0, "not before the grace period");
    let after = next_day + janitor::DELETE_GRACE;
    let (_, deleted) = janitor::pass(store.as_ref(), &engine.catalog, &engine.deletions, after)
        .await
        .unwrap();
    assert_eq!(deleted, dropped);
    // Temperature at 05:00 and 06:00 still shows within the 12 hours (each
    // part stands for the hour after it).
    let left = testing::keys(store.as_ref()).await;
    assert_eq!(
        left,
        vec![
            keys::grid("t", "fake-26110600-300"),
            keys::grid("t", "fake-26110600-360")
        ]
    );
    let mut held: Vec<String> = engine
        .catalog
        .parts()
        .into_iter()
        .map(|(_, p)| p.id)
        .collect();
    held.sort();
    assert_eq!(held, ["fake-26110600-300", "fake-26110600-360"]);
    assert!(part_ids(&engine, "p").is_empty() && part_ids(&engine, "r").is_empty());
}

#[tokio::test]
async fn a_broken_file_is_charged_until_given_up() {
    let (mem, bucket) = fake_host();
    publish_hour(&mem, &bucket, 0);
    publish_hour(&mem, &bucket, 1);
    let engine = fake_engine(&mem);
    let mut task = hourly_task(engine.clone(), Fake::new(bucket.clone()));
    // f001's record does not parse: its parts are charged, f000's are made.
    let key = format!("{}/f001.dat", run_label(t0()));
    mem.put_s3(&bucket, &key, "eleven!".to_string());
    let now = t0() + Duration::minutes(70);
    let report = task.cycle(now, &|_, _| {}).await.unwrap();
    assert_eq!(report.tally.produced, 1, "{:?}", report.tally);
    assert_eq!(report.tally.failed, 2, "t and p at f001");
    for attempt in 2..=crate::engine::MAX_ATTEMPTS {
        let r = task
            .cycle(now + Duration::minutes(attempt as i64), &|_, _| {})
            .await
            .unwrap();
        assert_eq!(r.tally.failed, 2, "attempt {attempt}");
    }
    mem.clear_log();
    let r = task
        .cycle(now + Duration::minutes(10), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(r.tally, Default::default(), "given up");
    assert!(!mem.requests().iter().any(|r| r.contains("f001")));
}

fn hourly_task(engine: Arc<crate::engine::Engine>, fake: Fake) -> ModelTask {
    ModelTask::new(engine, Arc::new(fake), &REGISTRY.sources[0], &["hourly"])
}

fn requests_for(mem: &MemoryRemote, pattern: &str) -> usize {
    mem.requests()
        .iter()
        .filter(|r| r.contains(pattern))
        .count()
}

/// One object answering 503 is passed over for now: the cycle goes on to
/// the files after it, and nothing is charged.
#[tokio::test]
async fn one_unreachable_file_is_passed_over() {
    let (mem, bucket) = fake_host();
    for h in 0..=3 {
        publish_hour(&mem, &bucket, h);
    }
    mem.fail(&format!("{BASE}/{}/f001.dat", run_label(t0())), 503, 100);
    let engine = fake_engine(&mem);
    let mut task = hourly_task(engine.clone(), Fake::new(bucket.clone()));
    let now = t0() + Duration::minutes(90);
    let r = task
        .cycle(now, &|_, _| {})
        .await
        .expect("one file is not the provider");
    // t at 0, 2 and 3 h; p at 3 h (its step starts at f002). Not what needs
    // f001.
    assert_eq!(r.tally.produced, 4, "{:?}", r.tally);
    assert_eq!(r.tally.failed, 0, "not charged");
    let ids = part_ids(&engine, "t");
    assert!(
        !ids.contains(&"fake-26110600-60".to_string())
            && ids.contains(&"fake-26110600-180".to_string())
    );
    // Tried again after the relist interval.
    assert!(
        r.wake <= now + Duration::minutes(1) + Duration::seconds(1),
        "{}",
        r.wake
    );
}

#[tokio::test]
async fn a_provider_that_is_down_stops_the_task_without_charging() {
    let (mem, bucket) = fake_host();
    for h in 0..=6 {
        publish_hour(&mem, &bucket, h);
    }
    mem.fail(&format!("{BASE}/{}/f", run_label(t0())), 503, 1000);
    let engine = fake_engine(&mem);
    let mut task = hourly_task(engine.clone(), Fake::new(bucket.clone()));
    let now = t0() + Duration::minutes(90);
    let e = task.cycle(now, &|_, _| {}).await.unwrap_err();
    assert!(format!("{e:#}").contains("in a row unreachable"), "{e:#}");
    // Not the data's fault: the parts are still wanted.
    assert!(!task.wanted(t0(), now).is_empty());
    assert!(engine.catalog.latest("t", "fake").is_none());
}

/// A restart keeps the task's state, so the runs it had listed are not
/// listed again.
#[tokio::test(start_paused = true)]
async fn a_restart_keeps_what_the_task_knew() {
    let (mem, bucket) = fake_host();
    let run = crate::clock::floor(Utc::now(), Duration::hours(6)) - Duration::hours(6);
    // Its last three hours never come.
    for h in 0..=3 {
        publish_hour_of(&mem, &bucket, run, h);
    }
    // The run before cannot be listed, once: the task fails and restarts.
    let older = run - Duration::hours(6);
    mem.fail(
        &format!("{BASE}?list-type=2&prefix={}", run_label(older)),
        503,
        2,
    );
    let engine = fake_engine(&mem);
    let state = Arc::new(tokio::sync::Mutex::new(hourly_task(
        engine,
        Fake::new(bucket.clone()),
    )));
    let health = wx_core::HealthRegistry::new(wx_core::Hub::new());
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = wx_core::spawn_feed(
        "fake",
        std::time::Duration::from_secs(3600),
        &health,
        cancel.clone(),
        wx_core::Backoff::default(),
        move |ctx| super::model::run(state.clone(), ctx),
    );
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    cancel.cancel();
    handle.await.unwrap();
    assert_eq!(health.get("fake").unwrap().restarts, 1);
    assert_eq!(
        requests_for(&mem, &format!("prefix={}", run_label(older))),
        3,
        "the failed listing, then one more"
    );
    assert_eq!(
        requests_for(&mem, &format!("prefix={}", run_label(run))),
        1,
        "not listed again after the restart"
    );
}

/// A run later than its `give_up` window is still listed now and then, and
/// its files fetched when they come.
#[tokio::test]
async fn a_run_later_than_its_give_up_is_still_picked_up() {
    let (mem, bucket) = fake_host();
    let engine = fake_engine(&mem);
    let mut task = hourly_task(engine.clone(), Fake::new(bucket.clone()));
    // Due from 01:00 to 01:30, give-up 30 minutes: listed empty at 01:10
    // and 01:40.
    for minutes in [70, 100] {
        let r = task
            .cycle(t0() + Duration::minutes(minutes), &|_, _| {})
            .await
            .unwrap();
        assert_eq!(r.tally.produced, 0);
    }
    let late = t0() + Duration::minutes(100);
    let r = task
        .cycle(late + Duration::minutes(5), &|_, _| {})
        .await
        .unwrap();
    assert!(
        r.wake <= late + super::model::LATE_RELIST + Duration::seconds(1),
        "a late run is looked at again: {}",
        r.wake
    );
    // Every file is past its give-up by 02:00; the run lands at 02:01 and
    // is found by the slow relisting (01:45, then at least 20 minutes on).
    for h in 0..=6 {
        publish_hour(&mem, &bucket, h);
    }
    let r = task
        .cycle(t0() + Duration::minutes(130), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(r.tally.produced, 7 + 6, "{:?}", r.tally);
}

/// The health line says which run is in and which is awaited, and the data
/// time is the run delivered, so a run that never comes shows.
#[tokio::test]
async fn health_says_which_run_is_in_and_which_is_awaited() {
    let (mem, bucket) = fake_host();
    let engine = fake_engine(&mem);
    let mut task = hourly_task(engine.clone(), Fake::new(bucket.clone()));
    let now = t0() + Duration::minutes(70);
    // A cycle, a run lag of an hour (to its first file) and the give-up.
    assert_eq!(
        task.max_data_age(now),
        Duration::hours(6) + Duration::hours(1) + Duration::minutes(30)
    );
    let r = task.cycle(now, &|_, _| {}).await.unwrap();
    let (data, detail) = task.summary(&r, now);
    assert_eq!(data, None);
    assert!(detail.contains("waiting for run 06 00Z"), "{detail}");
    for h in 0..=6 {
        publish_hour(&mem, &bucket, h);
    }
    let later = now + Duration::minutes(2);
    let r = task.cycle(later, &|_, _| {}).await.unwrap();
    let (data, detail) = task.summary(&r, later);
    assert_eq!(data, Some(t0()));
    assert!(detail.starts_with("run 06 00Z, 7 of 7 leads"), "{detail}");
}

/// Each file is judged by the clock when it comes up: after an hour-long
/// download, parts that expired meanwhile are neither fetched nor built.
#[tokio::test(start_paused = true)]
async fn a_long_cycle_does_not_fetch_what_expired_meanwhile() {
    let (mem, bucket) = fake_host();
    for h in 0..=6 {
        publish_hour(&mem, &bucket, h);
    }
    let noon = t0() - Duration::hours(12);
    for h in 1..=6 {
        publish_hour_of(&mem, &bucket, noon, h);
    }
    let engine = fake_engine(&mem);
    let fake = Fake {
        slow: Some(("2026110600/f000.dat", std::time::Duration::from_secs(3600))),
        ..Fake::new(bucket.clone())
    };
    let mut task = hourly_task(engine.clone(), fake);
    let r = task
        .cycle(t0() + Duration::minutes(90), &|_, _| {})
        .await
        .unwrap();
    // Temperature at 13:00 the day before stands for 13:00 to 14:00, inside
    // the 12 hours back from 01:30 but not from 02:30.
    assert_eq!(
        requests_for(&mem, "2026110512/f001.dat"),
        0,
        "{:#?}",
        mem.requests()
    );
    assert!(requests_for(&mem, "2026110512/f002.dat") > 0);
    assert_eq!(r.tally.superseded + r.tally.unwanted, 0, "{:?}", r.tally);
}

/// On a cold start the past part of a timeline is filled from the older
/// runs before the newest run's far leads are fetched.
#[tokio::test]
async fn a_cold_start_fills_the_past_before_the_far_leads() {
    let (mem, bucket) = fake_host();
    let evening = t0() - Duration::hours(6);
    for h in 0..=6 {
        publish_hour(&mem, &bucket, h);
        publish_hour_of(&mem, &bucket, evening, h);
    }
    let engine = fake_engine(&mem);
    let mut task = hourly_task(engine.clone(), Fake::new(bucket.clone()));
    task.cycle(t0() + Duration::minutes(90), &|_, _| {})
        .await
        .unwrap();
    let requests = mem.requests();
    let first = |p: &str| {
        requests
            .iter()
            .position(|r| r.contains(p))
            .unwrap_or_else(|| panic!("{p}"))
    };
    assert!(
        first("2026110518/f000.dat") < first("2026110600/f002.dat"),
        "{requests:#?}"
    );
    // The evening run fills temperature's past, each hour from the newest
    // run holding it; the accumulation, which does not fill the past, takes
    // nothing from it.
    let t: Vec<String> = part_ids(&engine, "t")
        .into_iter()
        .filter(|id| id.contains("26110518"))
        .collect();
    assert_eq!(t.len(), 6, "{t:?}");
    assert!(
        !part_ids(&engine, "t").contains(&"fake-26110518-360".to_string()),
        "00:00 is the newer run's"
    );
    assert!(
        !part_ids(&engine, "p")
            .iter()
            .any(|id| id.contains("26110518"))
    );
}

/// A field a step never has is not asked for, and its absence is not
/// logged as a surprise.
#[tokio::test]
async fn a_field_known_absent_is_never_asked_for() {
    let (mem, bucket) = fake_host();
    for h in 0..=2 {
        publish_hour(&mem, &bucket, h);
    }
    let engine = fake_engine(&mem);
    let fake = Fake {
        no_temperature_at_0: true,
        ..Fake::new(bucket.clone())
    };
    let mut task = hourly_task(engine.clone(), fake);
    let r = task
        .cycle(t0() + Duration::minutes(90), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(r.tally.unavailable, 0, "{:?}", r.tally);
    assert!(!part_ids(&engine, "t").contains(&"fake-26110600-0".to_string()));
    assert!(part_ids(&engine, "t").contains(&"fake-26110600-60".to_string()));
    // f000 feeds nothing else: it is not fetched at all.
    assert_eq!(requests_for(&mem, "2026110600/f000.dat"), 0);
}

/// A file feeding leads on both sides of now, and a step starting in a file
/// of the first pass and ending in one of the second: each file once.
#[tokio::test]
async fn a_file_across_now_is_fetched_once() {
    let (mem, bucket) = fake_host();
    for h in 0..=6 {
        publish_hour(&mem, &bucket, h);
    }
    publish_pack(&mem, &bucket);
    let engine = fake_engine(&mem);
    let mut task = ModelTask::new(
        engine.clone(),
        Arc::new(Fake::new(bucket.clone())),
        &REGISTRY.sources[0],
        &["hourly", "pack"],
    );
    let r = task
        .cycle(t0() + Duration::minutes(90), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(requests_for(&mem, "pack.txt"), 1);
    for h in 0..=6 {
        assert_eq!(
            requests_for(&mem, &format!("f{h:03}.dat.idx")),
            1,
            "f{h:03}"
        );
    }
    // The step to 02:00 starts in f001 (before now) and ends in f002.
    assert_eq!(
        stored_value(engine.store.as_ref(), "p", "fake-26110600-120").await,
        3.0
    );
    assert_eq!(r.tally.failed + r.tally.unavailable, 0);
}

/// An observed product whose parts hold long backfills that hold, not just
/// the loop window.
#[tokio::test]
async fn observations_backfill_their_hold() {
    let mem = MemoryRemote::new();
    let bucket = S3Bucket::at(OBS_BASE);
    mem.serve_s3(&bucket, 1000);
    let now = t0() + Duration::hours(6);
    for k in 0..=10 {
        let v = now - Duration::minutes(30 * k);
        mem.put_s3(&bucket, &format!("obs/{}.txt", v.format("%Y%m%d%H%M")), "5");
    }
    let engine = fake_engine(&mem);
    let source = REGISTRY.sources.iter().find(|s| s.id == "obs").unwrap();
    let mut task = super::observed::ObservedTask::new(
        engine.clone(),
        Arc::new(FakeObs { bucket }),
        source,
        "obs",
    );
    assert_eq!(task.max_data_age(), Duration::minutes(4 * 30 + 5));
    for _ in 0..5 {
        if task.cycle(now).await.unwrap().remaining == 0 {
            break;
        }
    }
    // The window is two hours and each part holds for two and a half: a
    // part from up to four and a half hours back still shows.
    let mut held: Vec<DateTime<Utc>> = engine
        .catalog
        .parts()
        .into_iter()
        .filter(|(l, _)| *l == "s")
        .map(|(_, p)| p.valid)
        .collect();
    held.sort();
    assert_eq!(held.len(), 9, "{held:?}");
    assert_eq!(held[0], now - Duration::minutes(240));
}

#[test]
fn step_needs() {
    let p = layer_of("p");
    let input = &p.inputs[0];
    let source = &REGISTRY.sources[0];
    let job = |lead, previous, steps| {
        crate::engine::PartJob::forecast(p, input, source, t0(), lead, previous, steps)
    };
    assert!(
        job(0, None, Steps::RunTotals).is_none(),
        "no step at lead 0"
    );
    let first = job(60, Some(0), Steps::RunTotals).unwrap();
    let needs = first.needs();
    assert_eq!(needs.len(), 1, "the total at 0 is zero, not fetched");
    assert_eq!(needs[0].field.unwrap().stat, Stat::RunTotal);
    let later = job(180, Some(120), Steps::RunTotals).unwrap();
    let leads: Vec<(u32, Stat)> = later
        .needs()
        .iter()
        .map(|n| (n.lead, n.field.unwrap().stat))
        .collect();
    assert_eq!(leads, [(180, Stat::RunTotal), (120, Stat::RunTotal)]);
    // A family whose files hold the hour's record is asked for it.
    let window = job(180, Some(120), Steps::Windows).unwrap();
    let leads: Vec<(u32, Stat)> = window
        .needs()
        .iter()
        .map(|n| (n.lead, n.field.unwrap().stat))
        .collect();
    assert_eq!(leads, [(180, Stat::StepPerHour)]);
    // Leads that do not start at 0 (NBM's f001): the first step is from 0.
    assert_eq!(job(60, None, Steps::RunTotals).unwrap().needs().len(), 1);
}

#[test]
fn differences_keep_missing_and_noise() {
    let mut end = constant(5.0);
    end.values[0] = f32::NAN;
    let mut start = constant(2.0);
    start.values[1] = 5.5;
    crate::engine::step_per_hour(&mut end, Some(&start), 3.0).unwrap();
    assert!(end.values[0].is_nan());
    assert!((end.values[1] - (-0.5 / 3.0)).abs() < 1e-6);
    assert!((end.values[2] - 1.0).abs() < 1e-6);
    let mut other = constant(1.0);
    other.nx -= 1;
    assert!(crate::engine::step_per_hour(&mut constant(1.0), Some(&other), 1.0).is_err());
}

#[test]
fn cycles_reach_back_over_the_past() {
    let c = Cycle {
        every: Duration::hours(1),
        spare: 3,
        relist: Duration::seconds(30),
        give_up: Duration::minutes(45),
    };
    let now = t0() + Duration::hours(12) + Duration::minutes(13);
    let runs = c.runs(now, Duration::hours(12));
    assert_eq!(runs.first(), Some(&(t0() + Duration::hours(12))));
    // From 00:13 minus four more hours: 21Z the day before is the oldest.
    assert_eq!(runs.last(), Some(&(t0() - Duration::hours(3))));
    let nowcast = Cycle {
        every: Duration::minutes(5),
        ..c
    };
    // 12:10 back to 10:25: 90 minutes and four spares before 12:13.
    assert_eq!(nowcast.runs(now, Duration::minutes(90)).len(), 22);
}

/// GRIB2 named another way: the decoder picks the message from what `scan`
/// says of it.
#[test]
fn picking_decoder() {
    let grib = testing::fixture("hrrr_t10z_f06__TMP_2mabove.grib2");
    let input = &REGISTRY.layers[0].inputs[0];
    let need = Need {
        input,
        field: Some(FieldSel {
            var: "t_2m",
            level: "",
            stat: Stat::Instant,
        }),
        lead: 360,
    };
    let temperature =
        grib_picking(|m, n| m.is(0, 0, 0) && m.at_level(103, Some(2.0)) && n.lead == 360);
    let d = temperature(&grib, &need).unwrap();
    assert_eq!(d.field.nx * d.field.ny, d.field.values.len());
    assert!(d.valid.is_some());
    let nothing = grib_picking(|m, _| m.is(0, 2, 2));
    assert!(nothing(&grib, &need).is_err());
}
