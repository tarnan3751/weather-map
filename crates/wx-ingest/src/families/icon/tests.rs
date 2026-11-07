//! The fixtures under `fixtures/icon/` are cut from real files with ecCodes,
//! each message re-encoded on a sub-grid with its own packing and headers:
//!
//! * `d2_t18z_*`: ICON-D2's 2026-11-06 18 UTC run, rows 0-149 and columns
//!   725-924 of the regular grid (43.18-46.16 N, 10.56-14.54 E: northern
//!   Italy and the Adriatic, with rain, snow and freezing rain, and the
//!   missing south-east corner of the domain in the bitmap).
//! * `eu_t15z_003_T_2M`: ICON-EU's 15 UTC run, rows 104-231 and columns
//!   216-439 (36.0-43.94 N, 10.0 W-3.94 E), CCSDS-packed.
//! * `listing_icon-d2_18_t_2m.html`: the server's index of that directory,
//!   as served.
//!
//! Expected values are ecCodes' for the same files.

use super::*;
use crate::driver::model::ModelTask;
use crate::remote::memory::MemoryRemote;
use crate::testing;
use chrono::TimeZone;
use object_store::memory::InMemory;
use std::collections::BTreeMap;
use std::sync::Arc;
use wx_core::layer::FieldSel;
use wx_core::{Field, TileCoord, layer};

use crate::testing::at as t;

fn d2() -> &'static SourceDef {
    crate::testing::source("icon_d2")
}

fn eu() -> &'static SourceDef {
    crate::testing::source("icon_eu")
}

fn fixture(name: &str) -> Vec<u8> {
    testing::fixture(&format!("icon/{name}"))
}

fn need(layer_id: &str, source: &SourceDef, field: usize, lead: u32) -> Need {
    let input = layer::layer(layer_id).unwrap().input_for(source).unwrap();
    Need {
        input,
        field: Some(driver::fetched_as(input.fields[field], Default::default())),
        lead,
    }
}

/// A run's files as opendata.dwd.de serves them: each file at its URL, and
/// each directory's index in the server's own format (nginx, exact sizes).
#[derive(Default)]
struct Server {
    dirs: BTreeMap<String, Vec<(String, usize)>>,
}

impl Server {
    fn file(&mut self, mem: &MemoryRemote, key: &str, body: Vec<u8>) {
        let (dir, name) = key.rsplit_once('/').unwrap();
        self.dirs
            .entry(format!("{dir}/"))
            .or_default()
            .push((name.to_string(), body.len()));
        mem.put(&format!("{BASE}{key}"), body);
    }

    fn index(&self, mem: &MemoryRemote) {
        for (dir, files) in &self.dirs {
            mem.put(&format!("{BASE}{dir}"), index_html(dir, files));
        }
    }
}

fn index_html(dir: &str, files: &[(String, usize)]) -> String {
    let mut html = format!(
        "<html>\n<head><title>Index of /weather/nwp/{dir}</title></head>\n<body>\n<h1>Index of /weather/nwp/{dir}</h1><hr><pre><a href=\"../\">../</a>\n"
    );
    for (name, size) in files {
        let shown: String = name.chars().take(47).collect();
        html.push_str(&format!(
            "<a href=\"{name}\">{shown}..&gt;</a> 06-Nov-2026 18:49:33 {size:>19}\n"
        ));
    }
    html.push_str("</pre><hr></body>\n</html>\n");
    html
}

/// What a decoded field holds nearest `lon`, `lat`.
fn value_near(f: &Field, lon: f64, lat: f64) -> f32 {
    let (i, j) = f.proj.lonlat_to_ij(lon, lat);
    let (i, j) = (i.round() as usize, j.round() as usize);
    assert!(i < f.nx && j < f.ny, "{lon},{lat} is outside the field");
    f.values[j * f.nx + i]
}

fn decode(p: &Option<Payload>, need: &Need) -> crate::driver::Decoded {
    match p {
        Some(Payload::Custom { bytes, decode }) => decode(bytes, need).unwrap(),
        other => panic!("{other:?}"),
    }
}

/// Band `band` of a stored part at `lon`, `lat`, read back through a tile
/// rendered at zoom 10, where a tile sample lies within 0.001 degree of any
/// point.
pub(crate) async fn stored_value(
    store: &InMemory,
    layer_id: &str,
    id: &str,
    band: usize,
    lon: f64,
    lat: f64,
) -> Option<f32> {
    let layer = layer::layer(layer_id).unwrap();
    let grid = testing::stored_grid(store, layer_id, id).await;
    let z = 10u8;
    let n = f64::from(1u32 << z);
    let x = ((wx_core::proj::lon_to_x(lon)) * n).floor() as u32;
    let y = ((wx_core::proj::lat_to_y(lat)) * n).floor() as u32;
    let coord = TileCoord::new(z, x, y).unwrap();
    let tile = wx_tile::decode(&grid.render(coord)?).unwrap();
    let width = tile.width as usize;
    let (mut best, mut at) = (f64::MAX, 0);
    for row in 0..tile.height as usize {
        for col in 0..width {
            let (lo, la) = coord.sample_lonlat(col, row);
            let d = (lo - lon).powi(2) + (la - lat).powi(2);
            if d < best {
                (best, at) = (d, row * width + col);
            }
        }
    }
    layer.bands[band].dequantize(tile.bands[band][at])
}

#[test]
fn names_leads_and_arrival() {
    let run = t(12, 0);
    let (eu_l, d2_l) = (layout("icon-eu").unwrap(), layout("icon-d2").unwrap());
    // As on the server (research, 2026-11-06).
    assert_eq!(
        file_key(eu_l, run, "t_2m", 3),
        "icon-eu/grib/12/t_2m/icon-eu_europe_regular-lat-lon_single-level_2026110612_003_T_2M.grib2.bz2"
    );
    assert_eq!(
        file_key(d2_l, run, "t_2m", 3),
        "icon-d2/grib/12/t_2m/icon-d2_germany_regular-lat-lon_single-level_2026110612_003_2d_t_2m.grib2.bz2"
    );
    assert_eq!(
        file_key(d2_l, t(21, 0), "dbz_cmax", 48),
        "icon-d2/grib/21/dbz_cmax/icon-d2_germany_regular-lat-lon_single-level_2026110621_048_2d_dbz_cmax.grib2.bz2"
    );
    // The file of a variable at a lead: ICON-D2's 15-minute steps sit in
    // the file of their hour.
    let i = Icon;
    let radar = need("radar", d2(), 0, 195);
    assert_eq!(
        i.file(d2(), run, &radar),
        file_key(d2_l, run, "dbz_cmax", 3)
    );
    let precip = need("precip", d2(), 1, 180);
    assert_eq!(i.file(d2(), run, &precip), file_key(d2_l, run, "ww", 3));
    // Leads: the hourly part of each run, and ICON-D2's radar every 15
    // minutes to 48 h (the 048 file holds only 48:00).
    assert_eq!(i.leads(eu(), PRODUCT, t(12, 0)).len(), 79);
    assert_eq!(i.leads(eu(), PRODUCT, t(15, 0)).len(), 31);
    assert!(i.leads(eu(), SUBHOURLY, t(12, 0)).is_empty());
    assert_eq!(i.leads(d2(), PRODUCT, t(15, 0)).len(), 49);
    let q = i.leads(d2(), SUBHOURLY, t(15, 0));
    assert_eq!(
        (q.len(), q[1], q[4], *q.last().unwrap()),
        (193, 15, 60, 48 * 60)
    );
    assert!(
        i.leads(d2(), PRODUCT, t(13, 0)).is_empty(),
        "runs are every three hours"
    );
    // Arrival, as measured.
    assert_eq!(d2_l.arrival(t(18, 0), 0), t(18, 44));
    assert_eq!(d2_l.arrival(t(18, 0), 48), t(19, 20));
    assert_eq!(eu_l.arrival(t(12, 0), 0), t(14, 39));
    assert_eq!(
        i.due(d2(), SUBHOURLY, t(18, 0), 195),
        d2_l.arrival(t(18, 0), 3)
    );
    assert_eq!(i.cycle(d2()).every, Duration::hours(3));
    let mars = SourceDef {
        region: "mars",
        ..*d2()
    };
    assert!(i.check(&mars).is_err() && i.check(d2()).is_ok() && i.check(eu()).is_ok());
}

/// The variable directories a source's layers read.
#[test]
fn directories_follow_the_registry() {
    let d = variables(d2(), &[PRODUCT, SUBHOURLY]);
    for v in [
        "t_2m", "td_2m", "u_10m", "v_10m", "vmax_10m", "clct", "ww", "tot_prec", "snow_gsp",
        "dbz_cmax",
    ] {
        assert!(d.contains(&v), "{v} in {d:?}");
    }
    assert!(
        !d.contains(&"snow_con"),
        "ICON-D2 leaves convective snow out"
    );
    assert_eq!(d.len(), 10);
    assert!(!variables(d2(), &[PRODUCT]).contains(&"dbz_cmax"));
    let e = variables(eu(), &[PRODUCT, SUBHOURLY]);
    assert!(e.contains(&"snow_con") && !e.contains(&"dbz_cmax"));
}

/// The real index of `icon-d2/grib/18/t_2m/`: both grids, all 49 steps.
/// Only the run's regular lat-lon files are taken, with their sizes; a run
/// of another day in the same directory is not this one.
#[tokio::test]
async fn listing_finds_the_run_and_nothing_else() {
    let mem = MemoryRemote::new();
    let remote = testing::quick_remote(&mem);
    mem.put(
        &format!("{BASE}icon-d2/grib/18/t_2m/"),
        fixture("listing_icon-d2_18_t_2m.html"),
    );
    let cx = Cx::new(&remote, "test", t(20, 0));
    let i = Icon;
    let listing = i
        .list(&cx, d2(), t(18, 0), &[PRODUCT])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listing.len(), 49, "{listing:?}");
    let l = layout("icon-d2").unwrap();
    assert_eq!(
        listing.size(&file_key(l, t(18, 0), "t_2m", 3)),
        Some(Some(1_083_628))
    );
    assert!(listing.contains(&file_key(l, t(18, 0), "t_2m", 48)));
    // The other directories are not there (404): nothing from them.
    assert!(!listing.contains(&file_key(l, t(18, 0), "td_2m", 3)));
    // Yesterday's 18 UTC run is not in today's directory.
    let yesterday = Utc.with_ymd_and_hms(2026, 11, 5, 18, 0, 0).unwrap();
    assert!(
        i.list(&cx, d2(), yesterday, &[PRODUCT])
            .await
            .unwrap()
            .unwrap()
            .is_empty()
    );
    // One listing per directory the layers read.
    let listed: Vec<String> = mem
        .requests()
        .into_iter()
        .filter(|r| r.ends_with('/'))
        .collect();
    assert_eq!(listed.len(), 2 * 9, "{listed:?}");
}

/// One bzip2 file, inflated once: the four 15-minute radar steps of
/// ICON-D2's `001` file each get their own message.
#[test]
fn a_subhourly_file_splits_into_its_quarter_hours() {
    let run = t(18, 0);
    let file = fixture("d2_t18z_001_dbz_cmax.grib2.bz2");
    let needs: Vec<Need> = [60, 75, 90, 105, 120]
        .map(|lead| need("radar", d2(), 0, lead))
        .to_vec();
    let payloads = split(&file, run, &needs).unwrap();
    assert!(payloads[4].is_none(), "13:00 is in the next file");
    // ecCodes at 45.18 N, 12.56 E.
    let expected = [15.625, 12.742, 8.672, 13.262];
    for (k, want) in expected.iter().enumerate() {
        let d = decode(&payloads[k], &needs[k]);
        assert_eq!(
            d.valid,
            Some(run + Duration::minutes(i64::from(needs[k].lead)))
        );
        let v = value_near(&d.field, 12.56, 45.18);
        assert!((v - want).abs() < 0.01, "{}: {v} not {want}", needs[k].lead);
    }
    // The payloads share the inflated buffer, one message each.
    let Some(Payload::Custom { bytes, .. }) = &payloads[1] else {
        panic!()
    };
    assert!(bytes.starts_with(b"GRIB") && bytes.ends_with(b"7777"));
    assert_eq!(bytes.len(), 222_264 / 4);
}

/// Run totals are the accumulation from the run's start to the lead, not
/// the quarter-hour totals in the same file; an instantaneous need finds
/// nothing in an accumulation file.
#[test]
fn run_totals_and_statistics() {
    let run = t(18, 0);
    let file = fixture("d2_t18z_003_tot_prec.grib2.bz2");
    let total = need("precip", d2(), 0, 180);
    assert_eq!(total.field.unwrap().stat, Stat::RunTotal);
    let quarter = Need { lead: 195, ..total };
    let instant = Need {
        field: Some(FieldSel {
            stat: Stat::Instant,
            ..total.field.unwrap()
        }),
        ..total
    };
    let p = split(&file, run, &[total, quarter, instant]).unwrap();
    assert!(p[2].is_none());
    // ecCodes at 43.62 N, 12.66 E: 1.8823 mm by 21:00, 2.0986 by 21:15.
    let v = value_near(&decode(&p[0], &total).field, 12.66, 43.62);
    assert!((v - 1.8823).abs() < 0.001, "{v}");
    let v = value_near(&decode(&p[1], &quarter).field, 12.66, 43.62);
    assert!((v - 2.0986).abs() < 0.001, "{v}");
    // A file that claims another run is broken.
    assert!(
        split(&file, t(15, 0), &[total])
            .unwrap_err()
            .to_string()
            .contains("run")
    );
}

/// The bitmap's missing corner decodes as NaN; plain GRIB (not compressed)
/// is taken as well; a broken bzip2 stream is an error.
#[test]
fn bitmaps_and_compression() {
    let run = t(18, 0);
    let bz = fixture("d2_t18z_003_t_2m.grib2.bz2");
    let temp = need("temp", d2(), 0, 180);
    let p = split(&bz, run, &[temp]).unwrap();
    let f = decode(&p[0], &temp).field;
    assert_eq!(f.values.iter().filter(|v| v.is_nan()).count(), 4195);
    assert!(value_near(&f, 10.56, 43.18).is_nan(), "the domain's corner");
    let v = value_near(&f, 12.56, 45.18);
    assert!((v - 286.7266).abs() < 0.001, "{v}");
    let plain = codec::bunzip2(&bz, codec::MAX_INFLATED).unwrap();
    assert!(split(&plain, run, &[temp]).unwrap()[0].is_some());
    let mut broken = bz.clone();
    broken.truncate(bz.len() / 2);
    assert!(split(&broken, run, &[temp]).is_err());
    assert!(split(b"<html>not found</html>", run, &[temp]).is_err());
}

/// Publish the cut ICON-D2 18 UTC files: temperature for steps 0-3, the
/// weather code and precipitation for 1-3, reflectivity for step 1. The
/// rest of the run is not out yet.
fn publish_d2(mem: &MemoryRemote) -> Server {
    let l = layout("icon-d2").unwrap();
    let run = t(18, 0);
    let mut s = Server::default();
    for h in 0..=3 {
        s.file(
            mem,
            &file_key(l, run, "t_2m", h),
            fixture(&format!("d2_t18z_{h:03}_t_2m.grib2.bz2")),
        );
    }
    for h in 1..=3 {
        for var in ["ww", "tot_prec"] {
            s.file(
                mem,
                &file_key(l, run, var, h),
                fixture(&format!("d2_t18z_{h:03}_{var}.grib2.bz2")),
            );
        }
    }
    s.file(
        mem,
        &file_key(l, run, "dbz_cmax", 1),
        fixture("d2_t18z_001_dbz_cmax.grib2.bz2"),
    );
    s.index(mem);
    s
}

fn file_gets(mem: &MemoryRemote) -> Vec<String> {
    mem.requests()
        .into_iter()
        .filter(|r| r.ends_with(".bz2"))
        .collect()
}

/// A run that is partly out: the parts its files make are built, each file
/// is fetched once (the `001` precipitation file serves two parts, the
/// reflectivity file four), and the rest waits for the next listing.
#[tokio::test]
async fn a_partly_published_run_gives_what_it_has() {
    let mem = MemoryRemote::new();
    publish_d2(&mem);
    let (engine, store) = testing::engine(&mem);
    let mut task = ModelTask::new(engine.clone(), Arc::new(Icon), d2(), &[PRODUCT, SUBHOURLY]);
    let now = t(18, 58);
    let r = task.cycle(now, &|_, _| {}).await.unwrap();
    assert_eq!(
        (r.tally.produced, r.tally.failed, r.tally.unavailable),
        (11, 0, 0),
        "{:?}",
        r.tally
    );
    let ids = |l: &str| -> Vec<String> {
        let mut v: Vec<String> = engine
            .catalog
            .timeline(l)
            .parts
            .into_iter()
            .map(|p| p.id)
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        ids("temp"),
        [
            "icon_d2-26110618-0",
            "icon_d2-26110618-120",
            "icon_d2-26110618-180",
            "icon_d2-26110618-60"
        ]
    );
    assert_eq!(
        ids("radar"),
        [
            "icon_d2-26110618-105",
            "icon_d2-26110618-60",
            "icon_d2-26110618-75",
            "icon_d2-26110618-90"
        ]
    );
    assert_eq!(
        ids("precip"),
        [
            "icon_d2-26110618-120",
            "icon_d2-26110618-180",
            "icon_d2-26110618-60"
        ]
    );
    for l in ["dew_point", "wind", "clouds", "snow", "feels_like"] {
        assert!(ids(l).is_empty(), "{l}: its files are not out");
    }
    let gets = file_gets(&mem);
    assert_eq!(gets.len(), 11, "{gets:#?}");
    let mut unique = gets.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 11, "each file once");
    // The rest of the run is due: look again after the relist interval.
    assert!(r.wake <= now + Duration::minutes(2), "{}", r.wake);

    // Temperature at 45.18 N, 12.56 E, 21:00: ecCodes 286.7266 K.
    let v = stored_value(&store, "temp", "icon_d2-26110618-180", 0, 12.56, 45.18)
        .await
        .unwrap();
    assert!((v - 13.5766).abs() < 0.05, "{v}");
    // The hour to 21:00 at 43.62 N, 12.66 E: 1.8823 - 1.2090 mm, rain (ww 61).
    let rate = stored_value(&store, "precip", "icon_d2-26110618-180", 0, 12.66, 43.62)
        .await
        .unwrap();
    assert!((rate - 0.6733).abs() < 0.07, "{rate}");
    let kind = stored_value(&store, "precip", "icon_d2-26110618-180", 1, 12.66, 43.62)
        .await
        .unwrap();
    assert_eq!(kind, wx_core::layer::ptype::RAIN);
    let part = engine.catalog.latest("temp", "icon_d2").unwrap();
    assert!(
        (part.bounds.west - 10.55).abs() < 0.02 && (part.bounds.north - 46.17).abs() < 0.02,
        "{:?}",
        part.bounds
    );
    assert!(part.solid.is_some());

    // Nothing new: nothing fetched again.
    mem.clear_log();
    let r = task.cycle(t(18, 59), &|_, _| {}).await.unwrap();
    assert_eq!(r.tally.produced, 0);
    assert!(file_gets(&mem).is_empty(), "{:?}", mem.requests());
}

/// A file that does not inflate is charged to its parts, and the run goes on.
#[tokio::test]
async fn a_broken_file_is_charged_and_the_run_goes_on() {
    let mem = MemoryRemote::new();
    let mut s = publish_d2(&mem);
    let l = layout("icon-d2").unwrap();
    let mut broken = fixture("d2_t18z_003_t_2m.grib2.bz2");
    broken.truncate(1000);
    s.file(&mem, &file_key(l, t(18, 0), "t_2m", 3), broken);
    s.dirs.get_mut("icon-d2/grib/18/t_2m/").unwrap().pop();
    s.index(&mem);
    let (engine, _) = testing::engine(&mem);
    let mut task = ModelTask::new(engine.clone(), Arc::new(Icon), d2(), &[PRODUCT, SUBHOURLY]);
    let r = task.cycle(t(18, 58), &|_, _| {}).await.unwrap();
    assert_eq!((r.tally.produced, r.tally.failed), (10, 1), "{:?}", r.tally);
    assert!(
        engine
            .catalog
            .timeline("temp")
            .parts
            .iter()
            .all(|p| p.id != "icon_d2-26110618-180")
    );
}

/// ICON-EU's CCSDS packing, through the whole path, on a short run.
#[tokio::test]
async fn icon_eu_ccsds_temperature() {
    let mem = MemoryRemote::new();
    let l = layout("icon-eu").unwrap();
    let mut s = Server::default();
    s.file(
        &mem,
        &file_key(l, t(15, 0), "t_2m", 3),
        fixture("eu_t15z_003_T_2M.grib2.bz2"),
    );
    s.index(&mem);
    let (engine, store) = testing::engine(&mem);
    let mut task = ModelTask::new(engine.clone(), Arc::new(Icon), eu(), &[PRODUCT, SUBHOURLY]);
    let r = task.cycle(t(18, 10), &|_, _| {}).await.unwrap();
    assert_eq!((r.tally.produced, r.tally.failed), (1, 0), "{:?}", r.tally);
    let part = engine.catalog.latest("temp", "icon_eu").unwrap();
    assert_eq!(part.id, "icon_eu-26110615-180");
    // The cut spans 10 W to 3.94 E: west of Greenwich is not wrapped away.
    assert!(
        (part.bounds.west + 10.03).abs() < 0.02 && (part.bounds.east - 3.97).abs() < 0.02,
        "{:?}",
        part.bounds
    );
    assert!(
        (part.bounds.south - 35.97).abs() < 0.02 && (part.bounds.north - 43.97).abs() < 0.02,
        "{:?}",
        part.bounds
    );
    // Near Madrid, 40 N 3 W, 18:00: ecCodes 285.4031 K.
    let v = stored_value(&store, "temp", &part.id, 0, -3.0, 40.0)
        .await
        .unwrap();
    assert!((v - 12.2531).abs() < 0.1, "{v}");
}

/// Airports (temperature and wind against their observations) and open
/// sea (wind against a global model).
const PLACES: &[crate::families::live_check::Place] = &[
    ("EDDF", 8.570, 50.033),
    ("EDDM", 11.786, 48.354),
    ("EDDB", 13.501, 52.362),
    ("EDDH", 9.988, 53.630),
    ("LEMD", -3.561, 40.472),
    ("EGLL", -0.461, 51.477),
    ("EPWA", 20.967, 52.166),
    ("LFPG", 2.548, 49.010),
    ("ESSA", 17.919, 59.652),
    ("LIRF", 12.239, 41.800),
    ("north_sea", 3.0, 56.0),
    ("biscay", -6.0, 46.0),
    ("baltic", 17.0, 55.5),
];

/// One cycle of each ICON source against opendata.dwd.de: see
/// `live_check`. `cargo test -p wx-ingest --features icon -- --ignored
/// live_icon --nocapture`.
#[tokio::test]
#[ignore = "network"]
async fn live_icon_into_a_store() {
    crate::families::live_check::run(&["icon_d2", "icon_eu"], PLACES).await;
}

/// How long whole files take to become parts (inflate, split, decode,
/// derive, grid), from full-size files in `WX_FULL_DIR` named as on the
/// server. Run with `--release`.
#[test]
#[ignore = "needs full-size files"]
fn time_full_size_parts() {
    let Ok(dir) = std::env::var("WX_FULL_DIR") else {
        return;
    };
    let cases: [(&str, &SourceDef, &str, u32, &str); 3] = [
        (
            "temp",
            d2(),
            "2026110618",
            180,
            "icon-d2_germany_regular-lat-lon_single-level_2026110618_003_2d_t_2m.grib2.bz2",
        ),
        (
            "radar",
            d2(),
            "2026110618",
            75,
            "icon-d2_germany_regular-lat-lon_single-level_2026110618_001_2d_dbz_cmax.grib2.bz2",
        ),
        (
            "temp",
            eu(),
            "2026110615",
            180,
            "icon-eu_europe_regular-lat-lon_single-level_2026110615_003_T_2M.grib2.bz2",
        ),
    ];
    for (layer_id, source, run, lead, name) in cases {
        let bytes = std::fs::read(format!("{dir}/{name}")).unwrap();
        let run = chrono::NaiveDateTime::parse_from_str(&format!("{run}0000"), "%Y%m%d%H%M%S")
            .unwrap()
            .and_utc();
        let layer = layer::layer(layer_id).unwrap();
        let input = layer.input_for(source).unwrap();
        let n = need(layer_id, source, 0, lead);
        let started = std::time::Instant::now();
        let p = split(&bytes, run, &[n]).unwrap();
        let split_ms = started.elapsed().as_secs_f64() * 1e3;
        let field = decode(&p[0], &n).field;
        let decoded_ms = started.elapsed().as_secs_f64() * 1e3;
        let raster = wx_pipeline::derive(layer, input, &[field]).unwrap();
        let grid = wx_pipeline::FrameGrid::build(layer, &raster).to_bytes();
        let total_ms = started.elapsed().as_secs_f64() * 1e3;
        println!(
            "{layer_id} {}: {} kB in, inflate and split {split_ms:.0} ms, decode {:.0} ms, part {total_ms:.0} ms, {} kB stored",
            source.id,
            bytes.len() / 1000,
            decoded_ms - split_ms,
            grid.len() / 1000
        );
    }
}
