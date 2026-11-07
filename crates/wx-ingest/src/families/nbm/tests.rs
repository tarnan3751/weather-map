use super::*;
use crate::driver::Payload;
use crate::driver::model::ModelTask;
use crate::remote::memory::MemoryRemote;
use crate::testing;
use std::sync::Arc;
use wx_core::LAYERS;
use wx_core::families::nbm::PRODUCT;
use wx_core::layer::Stat;

use crate::testing::on as t;

/// The 18Z run of 6 November, the one the fixtures were cut from.
const RUN: (u32, u32, u32) = (6, 18, 0);
/// Half an hour after the cycle hour: the files are not due yet, but the
/// driver lists a run once anyway.
const NOW: (u32, u32, u32) = (6, 19, 30);
/// The fixtures are f006, valid at 00Z the next day.
const LEAD: u32 = 360;

fn run() -> DateTime<Utc> {
    t(RUN.0, RUN.1, RUN.2)
}

fn now() -> DateTime<Utc> {
    t(NOW.0, NOW.1, NOW.2)
}

use crate::testing::source as src;

fn task(engine: Arc<crate::engine::Engine>, id: &str) -> ModelTask {
    testing::model_task(&engine, Nbm, id)
}

fn url(key: &str) -> String {
    S3Bucket::aws(BUCKET).url(key)
}

/// Messages of a domain, by the fixture field names, in the order the real
/// file holds them (the order does not matter to the adapter).
fn real(domain: &str, fields: &[&str]) -> Vec<(String, String, String, Vec<u8>)> {
    let labels = [
        ("TMP_2mabove", "TMP", "2 m above ground", "6 hour fcst"),
        ("DPT_2mabove", "DPT", "2 m above ground", "6 hour fcst"),
        ("WIND_10mabove", "WIND", "10 m above ground", "6 hour fcst"),
        ("WDIR_10mabove", "WDIR", "10 m above ground", "6 hour fcst"),
        ("GUST_10mabove", "GUST", "10 m above ground", "6 hour fcst"),
        ("TCDC_surface", "TCDC", "surface", "6 hour fcst"),
        ("APCP_surface_5-6h", "APCP", "surface", "5-6 hour acc fcst"),
    ];
    fields
        .iter()
        .map(|f| {
            let (_, var, level, time) = labels.iter().find(|l| l.0 == *f).unwrap();
            let bytes = testing::fixture(&format!("nbm/nbm_t18z_f006_{domain}__{f}.grib2"));
            (var.to_string(), level.to_string(), time.to_string(), bytes)
        })
        .collect()
}

const ALL: [&str; 7] = [
    "TMP_2mabove",
    "DPT_2mabove",
    "WIND_10mabove",
    "WDIR_10mabove",
    "GUST_10mabove",
    "TCDC_surface",
    "APCP_surface_5-6h",
];

/// A file of the messages `real` gives, each preceded by a decoy the
/// inventory marks with a qualifier (the one that would be picked by mistake
/// if qualifiers were ignored), and its inventory. Also the byte range of
/// each real message.
fn build(messages: Vec<(String, String, String, Vec<u8>)>) -> (Vec<u8>, String, Vec<(u64, u64)>) {
    let mut file = testing::GribFile::default();
    let mut ranges = Vec::new();
    for (var, level, time, bytes) in messages {
        // The probability of precipitation comes before the amount in the
        // real inventories; the spread follows the field.
        let qualifier = if var == "APCP" {
            "prob >0.254:prob fcst 255/255"
        } else {
            "10% level"
        };
        file.decoy(&var, &level, &time, qualifier);
        ranges.push(file.add(&var, &level, &time, "", &bytes));
        file.decoy(&var, &level, &time, "ens std dev");
    }
    (file.bytes, file.inventory, ranges)
}

fn publish(mem: &MemoryRemote, domain: &str, file: Vec<u8>, inventory: String) -> String {
    let key = file_key(run(), domain, LEAD / 60);
    let bucket = S3Bucket::aws(BUCKET);
    mem.put_s3(&bucket, &key, file);
    mem.put_s3(&bucket, &format!("{key}.idx"), inventory);
    key
}

fn gets(mem: &MemoryRemote, key: &str) -> Vec<String> {
    mem.requests_for(&url(key))
}

#[test]
fn keys_leads_and_arrival() {
    assert_eq!(
        file_key(run(), "pr", 6),
        "blend.20261106/18/core/blend.t18z.core.f006.pr.grib2"
    );
    assert_eq!(
        file_key(t(7, 0, 0), "co", 48),
        "blend.20261107/00/core/blend.t00z.core.f048.co.grib2"
    );
    assert_eq!(
        list_prefix(run()),
        "blend.20261106/18/core/blend.t18z.core.f0"
    );
    assert_eq!(arrival(run()), t(6, 19, 5));
    let n = Nbm;
    let s = src("nbm_co");
    // Hourly from f001 to f048, nothing at f000; only the synoptic cycles.
    let leads = n.leads(s, PRODUCT, run());
    assert_eq!(
        (leads.len(), leads[0], leads[1], *leads.last().unwrap()),
        (48, 60, 120, 2880)
    );
    assert!(n.leads(s, PRODUCT, t(6, 17, 0)).is_empty());
    assert!(n.leads(s, PRODUCT, t(6, 19, 0)).is_empty());
    assert_eq!(n.leads(s, PRODUCT, t(7, 0, 0)).len(), 48);
    assert_eq!(n.cycle(s).every, Duration::hours(6));
    // At 19:30 the newest run is 18Z, and 12Z is within reach of the past
    // window, but 17Z is never looked at.
    let runs = n.cycle(s).runs(now(), Duration::hours(6));
    assert_eq!(runs.first(), Some(&run()));
    assert!(runs.iter().all(|r| r.hour().is_multiple_of(6)), "{runs:?}");
}

#[test]
fn every_source_has_its_domain() {
    let n = Nbm;
    let ids: Vec<&str> = wx_core::source::sources_of(wx_core::Family::Nbm)
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, ["nbm_co", "nbm_ak", "nbm_hi", "nbm_pr", "nbm_gu"]);
    for s in wx_core::source::sources_of(wx_core::Family::Nbm) {
        assert!(n.check(s).is_ok(), "{}", s.id);
        let need = Need {
            input: &wx_core::families::nbm::TEMP,
            field: None,
            lead: LEAD,
        };
        let key = n.file(s, run(), &need);
        assert_eq!(
            key,
            format!(
                "blend.20261106/18/core/blend.t18z.core.f006.{}.grib2",
                &s.id[4..]
            )
        );
    }
    assert!(n.check(src("hrrr")).is_err());
}

/// The inventories are the real f006 ones: the deterministic message of a
/// field is the one with nothing after its time, and that is not always the
/// first line (CONUS lists `APCP` probability before the amount).
#[test]
fn real_inventories_give_the_deterministic_messages() {
    use crate::idx::find;
    let s = |var, level, stat| wx_core::layer::FieldSel { var, level, stat };
    let wanted = [
        s("TMP", "2 m above ground", Stat::Instant),
        s("DPT", "2 m above ground", Stat::Instant),
        s("WIND", "10 m above ground", Stat::Instant),
        s("WDIR", "10 m above ground", Stat::Instant),
        s("GUST", "10 m above ground", Stat::Instant),
        s("TCDC", "surface", Stat::Instant),
        s("APCP", "surface", Stat::StepPerHour),
    ];
    for d in DOMAINS {
        let text =
            String::from_utf8(testing::fixture(&format!("nbm/nbm_t18z_f006_{d}.idx"))).unwrap();
        let inventory = deterministic(&text);
        let plain: Vec<&str> = text
            .lines()
            .filter(|l| l.split(':').skip(6).all(|q| q.trim().is_empty()))
            .collect();
        assert_eq!(inventory.len(), plain.len(), "{d}");
        for sel in &wanted {
            let e = find(&inventory, sel, LEAD).unwrap_or_else(|| panic!("{d}: {}", sel.var));
            let line = text
                .lines()
                .find(|l| {
                    l.split(':').nth(1).and_then(|o| o.parse::<u64>().ok()) == Some(e.range.start)
                })
                .unwrap();
            assert!(line.ends_with(':'), "{d}: {line}");
            // The end is where the next message of the whole inventory
            // starts, not the next deterministic one.
            let next = text
                .lines()
                .filter_map(|l| l.split(':').nth(1)?.parse::<u64>().ok())
                .filter(|o| *o > e.range.start)
                .min();
            assert_eq!(e.range.end, next, "{d}: {}", sel.var);
        }
        // The 6-hour total beside the 1-hour record is never the step.
        let apcp = find(&inventory, &wanted[6], LEAD).unwrap();
        let line = text
            .lines()
            .find(|l| l.contains(&format!(":{}:", apcp.range.start)))
            .unwrap();
        assert!(line.contains("5-6 hour acc fcst"), "{line}");
    }
    // CONUS: the first line that reads like the hour's amount is its
    // probability, so an inventory read without regard to qualifiers would
    // fetch the wrong message.
    let text = String::from_utf8(testing::fixture("nbm/nbm_t18z_f006_co.idx")).unwrap();
    let naive = find(&crate::idx::parse(&text), &wanted[6], LEAD)
        .unwrap()
        .range;
    let right = find(&deterministic(&text), &wanted[6], LEAD).unwrap().range;
    assert_ne!(naive, right);
}

#[tokio::test]
async fn listing_keeps_one_domain_and_files_with_inventories() {
    let mem = testing::remote(&[BUCKET]);
    let bucket = S3Bucket::aws(BUCKET);
    for d in ["co", "pr", "gu"] {
        for h in [1, 2] {
            mem.put_s3(&bucket, &file_key(run(), d, h), vec![0u8; 10]);
            // Guam's second file has no inventory yet.
            if !(d == "gu" && h == 2) {
                mem.put_s3(&bucket, &format!("{}.idx", file_key(run(), d, h)), "x");
            }
        }
    }
    // Another run's files are not listed.
    mem.put_s3(&bucket, &file_key(t(6, 12, 0), "pr", 1), vec![0u8; 10]);
    let remote = testing::quick_remote(&mem);
    let cx = Cx::new(&remote, "nbm_pr", now());
    let n = Nbm;
    let pr = n
        .list(&cx, src("nbm_pr"), run(), &[PRODUCT])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pr.len(), 2);
    assert!(pr.contains(&file_key(run(), "pr", 1)) && pr.contains(&file_key(run(), "pr", 2)));
    assert_eq!(pr.size(&file_key(run(), "pr", 1)), Some(Some(10)));
    let gu = n
        .list(&cx, src("nbm_gu"), run(), &[PRODUCT])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        gu.len(),
        1,
        "the file whose inventory is not there yet is not listed"
    );
    assert!(
        n.list(&cx, src("nbm_hi"), run(), &[PRODUCT])
            .await
            .unwrap()
            .unwrap()
            .is_empty()
    );
}

fn needs_of(id: &str, lead: u32) -> Vec<Need> {
    crate::driver::inputs_of(LAYERS, src(id))
        .into_iter()
        .flat_map(|(_, i)| {
            i.fields.iter().map(move |f| Need {
                input: i,
                field: Some(crate::driver::fetched_as(*f, Steps::Windows)),
                lead,
            })
        })
        .collect()
}

#[tokio::test]
async fn a_missing_inventory_means_the_file_is_not_ready() {
    let mem = testing::remote(&[BUCKET]);
    let key = file_key(run(), "pr", 6);
    mem.put_s3(&S3Bucket::aws(BUCKET), &key, vec![0u8; 10]);
    let remote = testing::quick_remote(&mem);
    let cx = Cx::new(&remote, "nbm_pr", now());
    let file = ModelFile {
        name: key,
        size: Some(10),
    };
    let needs = needs_of("nbm_pr", LEAD);
    let got = Nbm
        .fetch(&cx, src("nbm_pr"), run(), &file, &needs)
        .await
        .unwrap();
    assert!(matches!(got, Fetched::Missing), "{got:?}");
}

/// Hourly precipitation windows are asked for as they are: the need keeps
/// its per-hour statistic, and the driver builds the part from the one file.
#[tokio::test]
async fn windows_are_not_differenced() {
    let mem = testing::remote(&[BUCKET]);
    let (engine, _) = testing::engine(&mem);
    let jobs = task(engine, "nbm_pr").wanted(run(), now());
    let radar = jobs
        .iter()
        .find(|j| j.layer.id == "radar" && j.lead == LEAD)
        .unwrap();
    let needs = radar.needs();
    assert_eq!(needs.len(), 1, "no earlier total is asked for");
    assert_eq!(needs[0].field.unwrap().stat, Stat::StepPerHour);
    assert_eq!(Nbm.steps(src("nbm_pr"), PRODUCT), Steps::Windows);
    // Every lead from f001 has a precipitation part; the hour is its own.
    let radar_leads: Vec<u32> = jobs
        .iter()
        .filter(|j| j.layer.id == "radar")
        .map(|j| j.lead / 60)
        .collect();
    assert_eq!(radar_leads, (1..=48).collect::<Vec<_>>());
}

#[tokio::test]
async fn the_right_ranges_feed_every_layer() {
    let mem = testing::remote(&[BUCKET]);
    let (file, inventory, ranges) = build(real("pr", &ALL));
    let key = publish(&mem, "pr", file, inventory);
    let (engine, store) = testing::engine(&mem);
    let report = task(engine.clone(), "nbm_pr")
        .cycle(now(), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(report.tally.produced, 7, "{:?}", report.tally);
    assert_eq!(report.tally.failed, 0, "{:?}", report.tally);
    assert_eq!(report.tally.unavailable, 0, "{:?}", report.tally);
    // The inventory, then exactly the seven deterministic messages (the
    // decoys around them would have been bigger than the messages).
    let g = gets(&mem, &key);
    let mut asked: Vec<String> = g.iter().skip(1).cloned().collect();
    asked.sort();
    let mut expected: Vec<String> = ranges
        .iter()
        .map(|(a, b)| format!("{} [bytes={a}-{}]", url(&key), b - 1))
        .collect();
    expected.sort();
    assert_eq!(g[0], format!("{}.idx", url(&key)));
    assert_eq!(asked, expected);
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
            .latest(l, "nbm_pr")
            .unwrap_or_else(|| panic!("{l}"));
        assert_eq!(part.id, "nbm_pr-26110618-360");
        assert!(
            testing::keys(store.as_ref())
                .await
                .contains(&wx_core::keys::grid(l, &part.id))
        );
    }
    // Puerto Rico: 17-19.5 N, 68-64 W.
    let b = engine.catalog.latest("temp", "nbm_pr").unwrap().bounds;
    assert!(
        b.south > 16.0 && b.north < 21.0 && b.west < -66.0 && b.east > -66.0 && b.east < -62.0,
        "{b:?}"
    );
    // Nothing more is fetched next time.
    mem.clear_log();
    task(engine, "nbm_pr")
        .cycle(now(), &|_, _| {})
        .await
        .unwrap();
    assert!(gets(&mem, &key).is_empty(), "{:?}", mem.requests());
}

/// Guam has no apparent temperature; feels-like comes from the three fields
/// every domain has.
#[tokio::test]
async fn guam_and_hawaii_use_their_own_files() {
    let mem = testing::remote(&[BUCKET]);
    for d in ["gu", "hi"] {
        let (file, inventory, _) = build(real(d, &ALL));
        publish(&mem, d, file, inventory);
    }
    let (engine, _) = testing::engine(&mem);
    for (id, west, east, south, north) in [
        ("nbm_gu", 143.0, 149.0, 12.0, 17.5),
        ("nbm_hi", -166.0, -150.0, 14.0, 27.0),
    ] {
        let report = task(engine.clone(), id)
            .cycle(now(), &|_, _| {})
            .await
            .unwrap();
        assert_eq!(report.tally.produced, 7, "{id}: {:?}", report.tally);
        let b = engine.catalog.latest("feels_like", id).unwrap().bounds;
        assert!(
            b.west > west - 1.0
                && b.east < east + 1.0
                && b.south > south - 1.0
                && b.north < north + 1.0,
            "{id}: {b:?}"
        );
    }
}

/// Alaska's grid is polar stereographic, 1649 x 1105 points: its one
/// temperature message covers 40-77 N across the date line.
#[tokio::test]
async fn alaska_and_conus_are_separate_sources() {
    let mem = testing::remote(&[BUCKET]);
    let (file, inventory, _) = build(real("ak", &["TMP_2mabove"]));
    let key = publish(&mem, "ak", file, inventory);
    // CONUS has the same hour but is another file; its task never touches
    // Alaska's.
    let co = file_key(run(), "co", 6);
    mem.put_s3(&S3Bucket::aws(BUCKET), &co, vec![0u8; 10]);
    let (engine, _) = testing::engine(&mem);
    let report = task(engine.clone(), "nbm_ak")
        .cycle(now(), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(report.tally.produced, 1, "{:?}", report.tally);
    assert!(report.tally.unavailable >= 1);
    let part = engine.catalog.latest("temp", "nbm_ak").unwrap();
    assert!(
        part.bounds.south < 45.0 && part.bounds.north > 70.0,
        "{:?}",
        part.bounds
    );
    assert!(gets(&mem, &co).is_empty());
    assert!(!gets(&mem, &key).is_empty());
    assert!(engine.catalog.latest("temp", "nbm_co").is_none());
}

/// The inventory lists a message the file does not hold at that place.
#[tokio::test]
async fn an_inventory_that_does_not_fit_its_file_is_a_broken_file() {
    let mem = testing::remote(&[BUCKET]);
    let (mut file, inventory, _) = build(real("pr", &ALL));
    file.splice(0..200_000, vec![0u8; 200_000]);
    publish(&mem, "pr", file, inventory);
    let (engine, _) = testing::engine(&mem);
    let report = task(engine, "nbm_pr")
        .cycle(now(), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(report.tally.produced, 0, "{:?}", report.tally);
    assert!(report.tally.failed > 0, "{:?}", report.tally);
}

/// A known northerly wind (from 0 degrees) blows towards the south: v < 0.
/// Easterlies (from 90) have u < 0. Checked on the real Guam and Hawaii
/// speed and direction fields, at every point whose direction is within 20
/// degrees of the compass points.
#[test]
fn wind_from_speed_and_direction_has_the_right_signs() {
    let wind = crate::driver::inputs_of(LAYERS, src("nbm_gu"))
        .into_iter()
        .find(|(l, _)| l.id == "wind")
        .unwrap();
    for d in ["gu", "hi"] {
        let mut grib = testing::fixture(&format!("nbm/nbm_t18z_f006_{d}__WIND_10mabove.grib2"));
        grib.extend(testing::fixture(&format!(
            "nbm/nbm_t18z_f006_{d}__WDIR_10mabove.grib2"
        )));
        grib.extend(testing::fixture(&format!(
            "nbm/nbm_t18z_f006_{d}__GUST_10mabove.grib2"
        )));
        let set = wx_pipeline::decode(&grib, wind.1.fields).unwrap();
        let raster = wx_pipeline::derive(wind.0, wind.1, &set.fields).unwrap();
        let (speed, dir) = (&set.fields[0].values, &set.fields[1].values);
        let (u, v) = (&raster.bands[0], &raster.bands[1]);
        let near = |deg: f32, a: f32| {
            let d = (a - deg).rem_euclid(360.0);
            d.min(360.0 - d) < 20.0
        };
        let mut seen = [0usize; 4];
        for k in 0..speed.len() {
            let (s, a) = (speed[k], dir[k]);
            if s.is_nan() || s <= 3.0 || a.is_nan() {
                continue;
            }
            assert!(
                (u[k].hypot(v[k]) - s).abs() < 0.01 * s.max(1.0),
                "{d}: speed {s} became {}",
                u[k].hypot(v[k])
            );
            if near(0.0, a) {
                assert!(
                    v[k] < 0.0 && u[k].abs() < 0.4 * s,
                    "{d}: from {a} at {s}: u {} v {}",
                    u[k],
                    v[k]
                );
                seen[0] += 1;
            } else if near(90.0, a) {
                assert!(
                    u[k] < 0.0 && v[k].abs() < 0.4 * s,
                    "{d}: from {a} at {s}: u {} v {}",
                    u[k],
                    v[k]
                );
                seen[1] += 1;
            } else if near(180.0, a) {
                assert!(
                    v[k] > 0.0 && u[k].abs() < 0.4 * s,
                    "{d}: from {a} at {s}: u {} v {}",
                    u[k],
                    v[k]
                );
                seen[2] += 1;
            } else if near(270.0, a) {
                assert!(
                    u[k] > 0.0 && v[k].abs() < 0.4 * s,
                    "{d}: from {a} at {s}: u {} v {}",
                    u[k],
                    v[k]
                );
                seen[3] += 1;
            }
        }
        eprintln!("{d}: points from N, E, S, W: {seen:?}");
        assert!(seen[0] > 0 || seen[1] > 0, "{d}: no wind to check");
        if d == "hi" {
            assert!(seen[0] > 0, "Hawaii has northerly wind in the fixture");
        }
    }
}

/// Real hosts: this cycle's Puerto Rico and Guam files, one hour, every
/// field the layers read.
#[tokio::test]
#[ignore = "network"]
async fn live_small_domains_have_every_field() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let n = Nbm;
    for id in ["nbm_pr", "nbm_gu"] {
        let s = src(id);
        let runs = n.cycle(s).runs(now, Duration::hours(6));
        let mut found = None;
        for run in runs {
            let listing = n.list(&cx, s, run, &[PRODUCT]).await.unwrap().unwrap();
            let name = file_key(run, s.region, 12);
            if let Some(size) = listing.size(&name) {
                found = Some((run, ModelFile { name, size }));
                break;
            }
        }
        let (run, file) = found.unwrap_or_else(|| panic!("{id}: no run has f012"));
        let needs = needs_of(id, 720);
        let got = n.fetch(&cx, s, run, &file, &needs).await.unwrap();
        let Fetched::Got(payloads) = got else {
            panic!("{id}: missing")
        };
        for (p, need) in payloads.iter().zip(&needs) {
            let Some(Payload::Grib(b)) = p else {
                panic!("{id}: no {:?}", need.field)
            };
            assert!(b.starts_with(b"GRIB") && b.ends_with(b"7777"));
        }
        eprintln!("{id}: {} needs from {}", needs.len(), file.name);
    }
}

/// Reads a stored part back and prints its value at places, for comparing
/// with observations by hand: `WX_CHECK_STORE=<dir> WX_CHECK_PART=<layer>/<part
/// id> WX_CHECK_PLACES="name,lon,lat;..." cargo test -p wx-ingest --features
/// nbm -- --ignored --nocapture live_values_at_places`.
#[test]
#[ignore = "reads a store left by a live run"]
fn live_values_at_places() {
    let (Ok(dir), Ok(part), Ok(places)) = (
        std::env::var("WX_CHECK_STORE"),
        std::env::var("WX_CHECK_PART"),
        std::env::var("WX_CHECK_PLACES"),
    ) else {
        return;
    };
    let (layer_id, id) = part.split_once('/').unwrap();
    let grid = crate::testing::live::stored(std::path::Path::new(&dir), layer_id, id).unwrap();
    let mut sampler = crate::testing::live::Sampler::new(&grid, layer_id, 9);
    let unit = wx_core::layer::layer(layer_id).unwrap().bands[0].unit;
    for place in places.split(';') {
        let f: Vec<&str> = place.split(',').collect();
        let (name, lon, lat): (&str, f64, f64) =
            (f[0], f[1].parse().unwrap(), f[2].parse().unwrap());
        match sampler.at(lon, lat) {
            Some(v) => println!("{name}: {lon:.3},{lat:.3} -> {:?} {unit}", v[0]),
            None => println!("{name}: outside the part"),
        }
    }
}
