use super::*;
use crate::driver::Steps;
use crate::driver::model::ModelTask;
use crate::engine::Engine;
use crate::remote::memory::MemoryRemote;
use crate::testing;
use object_store::memory::InMemory;
use std::sync::Arc;
use wx_core::LAYERS;
use wx_core::families::cptec::PRODUCT;
use wx_core::layer::{FieldSel, Stat};

use crate::testing::on as t;

fn run() -> DateTime<Utc> {
    t(6, 0, 0)
}

/// Half past noon: f011 and f012 of the 00Z run were out hours ago.
fn now() -> DateTime<Utc> {
    t(6, 12, 30)
}

fn cptec() -> &'static SourceDef {
    testing::source("cptec")
}

fn dir() -> String {
    run_dir(run())
}

fn engine(mem: &Arc<MemoryRemote>) -> (Arc<Engine>, Arc<InMemory>) {
    testing::engine(mem)
}

fn task(engine: Arc<Engine>) -> ModelTask {
    testing::model_task(&engine, Cptec, "cptec")
}

/// The real f012 messages, by fixture name, with their inventory labels.
const F012: [(&str, &str, &str, &str); 8] = [
    (
        "REFC_entire",
        "REFC",
        "entire atmosphere (considered as a single layer)",
        "12 hour fcst",
    ),
    ("TMP_2mabove", "TMP", "2 m above ground", "12 hour fcst"),
    ("DPT_2mabove", "DPT", "2 m above ground", "12 hour fcst"),
    ("UGRD_10mabove", "UGRD", "10 m above ground", "12 hour fcst"),
    ("VGRD_10mabove", "VGRD", "10 m above ground", "12 hour fcst"),
    ("APCP_surface", "APCP", "surface", "0-12 hour acc fcst"),
    (
        "TCDC_entire",
        "TCDC",
        "entire atmosphere (considered as a single layer)",
        "12 hour fcst",
    ),
    ("GUST_surface", "GUST", "surface", "12 hour fcst"),
];

fn fixture(f: u32, name: &str) -> Vec<u8> {
    testing::fixture(&format!("cptec/cptec_t00z_f{f:03}__{name}.grib2"))
}

/// A file of the real messages given as `(var, level, time, bytes)`, each
/// after a decoy of a different level of the same variable (the ones a loose
/// match would take), and its inventory in the server's format.
fn build(messages: &[(&str, &str, &str, Vec<u8>)]) -> (Vec<u8>, String) {
    let mut file = testing::GribFile::default();
    for (var, level, time, bytes) in messages {
        let other = match *var {
            "TMP" => "surface",
            "UGRD" | "VGRD" => "100 m above ground",
            "DPT" => "50 mb",
            "REFC" | "TCDC" => "cloud top",
            _ => "max wind",
        };
        file.decoy(var, other, time, "");
        file.add(var, level, time, "", bytes);
    }
    (file.bytes, file.inventory)
}

fn publish(mem: &MemoryRemote, hour: u32, file: Vec<u8>, inventory: String) -> String {
    let name = file_name(run(), hour * 60);
    mem.put(&format!("{}{name}", dir()), file);
    mem.put(&format!("{}{}", dir(), inventory_name(&name)), inventory);
    name
}

fn gets(mem: &MemoryRemote, name: &str) -> Vec<String> {
    mem.requests_for(&format!("{}{name}", dir()))
}

#[test]
fn names_leads_and_arrival() {
    assert_eq!(
        dir(),
        "https://dataserver.cptec.inpe.br/dataserver_modelos/wrf/ams_07km/brutos/2026/11/06/00/"
    );
    assert_eq!(
        file_name(run(), 720),
        "WRF_cpt_07KM_2026110600_2026110612.grib2"
    );
    assert_eq!(
        file_name(run(), 25 * 60),
        "WRF_cpt_07KM_2026110600_2026110701.grib2"
    );
    assert_eq!(
        inventory_name("WRF_cpt_07KM_2026110600_2026110612.grib2"),
        "WRF_cpt_07KM_2026110600_2026110612.inv"
    );
    assert_eq!(arrival(run(), 0), t(6, 5, 20));
    assert_eq!(arrival(run(), 12), t(6, 5, 29));
    assert_eq!(arrival(run(), 120), t(6, 6, 50));
    let c = Cptec;
    let leads = c.leads(cptec(), PRODUCT, run());
    assert_eq!(
        (leads.len(), leads[0], leads[1], *leads.last().unwrap()),
        (121, 0, 60, 7200)
    );
    assert!(c.leads(cptec(), PRODUCT, t(6, 12, 0)).is_empty());
    assert_eq!(c.cycle(cptec()).every, Duration::hours(24));
    assert_eq!(c.steps(cptec(), PRODUCT), Steps::RunTotals);
    assert!(c.check(cptec()).is_ok());
    assert!(c.check(testing::source("gfs")).is_err());
}

/// The server's directory page: a file is there once its inventory is.
#[tokio::test]
async fn listing_needs_the_inventory() {
    let mem = MemoryRemote::new();
    mem.put(&dir(), testing::fixture("cptec/cptec_t00z_dir.html"));
    let (engine, _) = engine(&mem);
    let cx = Cx::new(&engine.remote, "cptec", now());
    let listing = Cptec
        .list(&cx, cptec(), run(), &[PRODUCT])
        .await
        .unwrap()
        .unwrap();
    // f000, f011 and f012 have an inventory; f013 has not been written
    // completely; the `.ctl` and `.grib2.idx` files are not data.
    assert_eq!(listing.len(), 3);
    for h in [0, 11, 12] {
        assert!(listing.contains(&file_name(run(), h * 60)), "f{h:03}");
    }
    assert!(!listing.contains(&file_name(run(), 13 * 60)));
    // Rounded sizes are not passed on.
    assert_eq!(listing.size(&file_name(run(), 0)), Some(None));
    // The day before the run exists: nothing is listed, not an error.
    let yesterday = t(5, 0, 0);
    let empty = Cptec
        .list(&cx, cptec(), yesterday, &[PRODUCT])
        .await
        .unwrap()
        .unwrap();
    assert!(empty.is_empty());
}

fn selectors() -> Vec<FieldSel> {
    let mut out: Vec<FieldSel> = Vec::new();
    for (_, i) in crate::driver::inputs_of(LAYERS, cptec()) {
        for f in i.fields {
            let f = crate::driver::fetched_as(*f, Steps::RunTotals);
            if !out.contains(&f) {
                out.push(f);
            }
        }
    }
    out
}

/// The real f012 inventory (the server's, 305 lines): every selector the
/// registry names is found, at the level the file really uses, and the byte
/// range is exactly the message the fixture holds.
#[test]
fn the_real_inventory_has_every_selector() {
    let text = String::from_utf8(testing::fixture("cptec/cptec_t00z_f012.inv")).unwrap();
    let inventory = crate::idx::parse(&text);
    assert_eq!(inventory.len(), 305);
    let sels = selectors();
    assert_eq!(sels.len(), 8, "{sels:?}");
    for sel in &sels {
        let e = crate::idx::find(&inventory, sel, 720)
            .unwrap_or_else(|| panic!("{}:{}", sel.var, sel.level));
        let (name, ..) = F012
            .iter()
            .find(|f| f.1 == sel.var && f.2 == sel.level)
            .unwrap();
        assert_eq!(
            e.range.size(),
            Some(fixture(12, name).len() as u64),
            "{}:{}",
            sel.var,
            sel.level
        );
    }
    // The run total at f011 is the only other one the precipitation step
    // needs; at f000 the total reads `0-0 day`.
    let apcp = FieldSel {
        var: "APCP",
        level: "surface",
        stat: Stat::RunTotal,
    };
    let f011 = crate::idx::parse(
        &String::from_utf8(testing::fixture("cptec/cptec_t00z_f011.inv")).unwrap(),
    );
    assert_eq!(
        crate::idx::find(&f011, &apcp, 660).unwrap().range.size(),
        Some(fixture(11, "APCP_surface").len() as u64)
    );
    let f000 = crate::idx::parse(
        &String::from_utf8(testing::fixture("cptec/cptec_t00z_f000.inv")).unwrap(),
    );
    assert_eq!(
        crate::idx::find(&f000, &apcp, 0).unwrap().range.size(),
        Some(203)
    );
    // The shorter level name is a different surface and is not matched.
    let short = FieldSel {
        var: "TCDC",
        level: "entire atmosphere",
        stat: Stat::Instant,
    };
    assert!(crate::idx::find(&inventory, &short, 720).is_none());
}

/// Run totals: the hour's amount is the difference of two files' totals, and
/// the real totals only grow.
#[test]
fn totals_grow_with_the_lead() {
    let apcp = [FieldSel {
        var: "APCP",
        level: "surface",
        stat: Stat::RunTotal,
    }];
    let decode = |bytes: Vec<u8>| wx_pipeline::decode(&bytes, &apcp).unwrap();
    let (a, b) = (
        decode(fixture(11, "APCP_surface")),
        decode(fixture(12, "APCP_surface")),
    );
    assert_eq!(b.valid_time - a.valid_time, Duration::hours(1));
    let (a, b) = (&a.fields[0].values, &b.fields[0].values);
    let mean = |v: &[f32]| v.iter().filter(|x| !x.is_nan()).sum::<f32>() / v.len() as f32;
    let grew = mean(b) - mean(a);
    assert!((0.0..5.0).contains(&grew), "mean hourly amount {grew}");
    let shrank = a.iter().zip(b).filter(|(x, y)| **y < **x - 0.01).count();
    assert!(
        shrank * 1000 < a.len(),
        "{shrank} of {} points shrank",
        a.len()
    );
}

#[tokio::test]
async fn a_missing_inventory_means_the_file_is_not_ready() {
    let mem = MemoryRemote::new();
    let name = file_name(run(), 720);
    mem.put(&format!("{}{name}", dir()), vec![0u8; 10]);
    let (engine, _) = engine(&mem);
    let cx = Cx::new(&engine.remote, "cptec", now());
    let file = ModelFile { name, size: None };
    let needs: Vec<Need> = Vec::new();
    let got = Cptec
        .fetch(&cx, cptec(), run(), &file, &needs)
        .await
        .unwrap();
    assert!(matches!(got, Fetched::Missing), "{got:?}");
}

/// f011 and f012 are published; the run's hour 12 gets every layer, the
/// precipitation one from the two files' totals, with the messages found by
/// byte range and nothing else downloaded.
#[tokio::test]
async fn hour_twelve_feeds_every_layer() {
    let mem = MemoryRemote::new();
    let messages: Vec<(&str, &str, &str, Vec<u8>)> = F012
        .iter()
        .map(|(name, var, level, time)| (*var, *level, *time, fixture(12, name)))
        .collect();
    let (file, inventory) = build(&messages);
    let name12 = publish(&mem, 12, file, inventory);
    let (file, inventory) = build(&[(
        "APCP",
        "surface",
        "0-11 hour acc fcst",
        fixture(11, "APCP_surface"),
    )]);
    let name11 = publish(&mem, 11, file, inventory);
    let (engine, store) = engine(&mem);
    // The server lists the directory; f011 and f012 are there.
    let mut page = String::from_utf8(testing::fixture("cptec/cptec_t00z_dir.html")).unwrap();
    page = page.replace(
        "WRF_cpt_07KM_2026110600_2026110613",
        "WRF_cpt_07KM_2026110600_2026110699",
    );
    mem.put(&dir(), page);
    let report = task(engine.clone()).cycle(now(), &|_, _| {}).await.unwrap();
    assert_eq!(report.tally.failed, 0, "{:?}", report.tally);
    for l in [
        "radar",
        "precip",
        "temp",
        "feels_like",
        "dew_point",
        "wind",
        "clouds",
    ] {
        let part = engine
            .catalog
            .latest(l, "cptec")
            .unwrap_or_else(|| panic!("{l}: {:?}", report.tally));
        assert_eq!(part.id, "cptec-26110600-720", "{l}");
        assert!(
            testing::keys(store.as_ref())
                .await
                .contains(&wx_core::keys::grid(l, &part.id))
        );
    }
    // Eight ranges from f012 (one per wanted message: decoys sit between
    // them, and the last message has no end), the totals of f011 from its
    // file, and each inventory read once.
    let g12 = gets(&mem, &name12);
    assert_eq!(g12.len(), 8, "{g12:?}");
    assert!(
        g12.iter().all(|r| r.contains("[bytes=")),
        "no whole file: {g12:?}"
    );
    assert_eq!(gets(&mem, &name11).len(), 1);
    for name in [&name11, &name12] {
        let inventory = format!("{}{}", dir(), inventory_name(name));
        assert_eq!(
            mem.requests().iter().filter(|r| **r == inventory).count(),
            1,
            "{name}"
        );
    }
    // South America: 57.9 S to 17.7 N, 90.7 W to 19.4 W.
    let b = engine.catalog.latest("temp", "cptec").unwrap().bounds;
    assert!(
        (b.west + 90.7).abs() < 0.3 && (b.east + 19.4).abs() < 0.3,
        "{b:?}"
    );
    assert!(
        (b.south + 57.9).abs() < 0.3 && (b.north - 17.7).abs() < 0.3,
        "{b:?}"
    );
    // A second pass has nothing left to fetch from f012.
    mem.clear_log();
    task(engine).cycle(now(), &|_, _| {}).await.unwrap();
    assert!(gets(&mem, &name12).is_empty(), "{:?}", mem.requests());
}

/// The first hour's step starts at the run: its total at f000 (an empty
/// `0-0 day` field) is not needed, so f000 is not asked for it.
#[tokio::test]
async fn the_first_hour_needs_no_earlier_total() {
    let mem = MemoryRemote::new();
    let (engine, _) = engine(&mem);
    let jobs = task(engine).wanted(run(), now());
    let precip: Vec<_> = jobs.iter().filter(|j| j.layer.id == "precip").collect();
    assert!(precip.iter().all(|j| j.lead > 0), "no step at the analysis");
    if let Some(one) = precip.iter().find(|j| j.lead == 60) {
        let needs = one.needs();
        assert!(needs.iter().all(|n| n.lead == 60), "{needs:?}");
    }
    // The empty f000 total is a message the decoder reads.
    let apcp = [FieldSel {
        var: "APCP",
        level: "surface",
        stat: Stat::RunTotal,
    }];
    let set = wx_pipeline::decode(&fixture(0, "APCP_surface"), &apcp).unwrap();
    assert_eq!(set.valid_time, run());
    assert_eq!(set.fields[0].values.len(), 1019 * 1081);
}

/// This morning's run: the day's directory, the newest hour's inventory and
/// the eight messages of one hour.
#[tokio::test]
#[ignore = "network"]
async fn live_one_hour_has_every_field() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let runs = Cptec.cycle(cptec()).runs(now, Duration::hours(24));
    let mut found = None;
    for run in runs {
        let listing = Cptec
            .list(&cx, cptec(), run, &[PRODUCT])
            .await
            .unwrap()
            .unwrap();
        let name = file_name(run, 3 * 60);
        if listing.contains(&name) {
            found = Some((run, name, listing.len()));
            break;
        }
    }
    let (run, name, count) = found.expect("no run has f003");
    eprintln!("{name}: {count} files listed");
    let needs: Vec<Need> = crate::driver::inputs_of(LAYERS, cptec())
        .into_iter()
        .flat_map(|(_, i)| {
            i.fields.iter().map(move |f| Need {
                input: i,
                field: Some(crate::driver::fetched_as(*f, Steps::RunTotals)),
                lead: 180,
            })
        })
        .collect();
    let file = ModelFile { name, size: None };
    let Fetched::Got(payloads) = Cptec.fetch(&cx, cptec(), run, &file, &needs).await.unwrap()
    else {
        panic!("missing");
    };
    for (p, n) in payloads.iter().zip(&needs) {
        let Some(crate::driver::Payload::Grib(b)) = p else {
            panic!("no {:?}", n.field)
        };
        assert!(b.starts_with(b"GRIB") && b.ends_with(b"7777"));
    }
}
