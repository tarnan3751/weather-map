//! The fixtures under `fixtures/dmi/` are cut from DINI's 2026-11-06 18 UTC
//! run with ecCodes: steps 2 and 3, rows 1260-1409 and columns 340-539 of
//! the Lambert grid (south-west Iceland, 62.9-66.2 N, 26.3-17.0 W, where the
//! winds turn about 12 degrees), each message re-encoded on that sub-grid
//! with its own packing. Step 3 keeps twelve of the file's messages in the
//! file's order, with fields the layers do not read between them; each has
//! an index in DINI's own format. Expected values are ecCodes'.

use super::*;
use crate::driver::model::ModelTask;
use crate::remote::memory::MemoryRemote;
use crate::testing;
use object_store::memory::InMemory;
use std::sync::Arc;
use wx_core::layer::{FieldSel, Stat};
use wx_core::{TileCoord, layer};

use crate::testing::at as t;

fn dini() -> &'static SourceDef {
    crate::testing::source("dini")
}

fn ig() -> &'static SourceDef {
    crate::testing::source("ig")
}

fn fixture(name: &str) -> Vec<u8> {
    testing::fixture(&format!("dmi/{name}"))
}

fn bucket() -> S3Bucket {
    S3Bucket::at(BUCKET_URL)
}

/// Serve `file` (and its index, when given) as step `hour` of the 18 UTC run.
fn publish(mem: &MemoryRemote, hour: u32, file: Vec<u8>, index: Option<Vec<u8>>) -> String {
    let key = file_key(layout("DINI").unwrap(), t(18, 0), hour);
    mem.put_s3(&bucket(), &key, file);
    if let Some(index) = index {
        mem.put_s3(&bucket(), &format!("{key}.index"), index);
    }
    bucket().url(&key)
}

/// Band `band` of a stored part at `lon`, `lat`, read back through a tile
/// rendered at zoom 10.
async fn stored_value(
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
    let x = (wx_core::proj::lon_to_x(lon) * n).floor() as u32;
    let y = (wx_core::proj::lat_to_y(lat) * n).floor() as u32;
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

/// Meteorological direction (where the wind comes from, clockwise from
/// north) of components `u`, `v`.
fn direction(u: f32, v: f32) -> f32 {
    (-u).atan2(-v).to_degrees().rem_euclid(360.0)
}

fn angle_between(a: f32, b: f32) -> f32 {
    ((a - b + 540.0).rem_euclid(360.0) - 180.0).abs()
}

#[test]
fn keys_leads_and_cycle() {
    let dl = layout("DINI").unwrap();
    // As on the server (research, 2026-11-06).
    assert_eq!(
        file_key(dl, t(15, 0), 3),
        "forecastdata/HARMONIE_DINI_SF/HARMONIE_DINI_SF_2026-11-06T150000Z_2026-11-06T180000Z.grib"
    );
    assert_eq!(
        file_key(layout("IG").unwrap(), t(18, 0), 30),
        "forecastdata/HARMONIE_IG_SF/HARMONIE_IG_SF_2026-11-06T180000Z_2026-11-08T000000Z.grib"
    );
    let d = Dmi;
    assert_eq!(d.leads(dini(), PRODUCT_SF, t(18, 0)).len(), 61);
    assert_eq!(d.leads(ig(), PRODUCT_SF, t(18, 0)).len(), 73);
    // Only every second run is followed.
    assert!(d.leads(dini(), PRODUCT_SF, t(15, 0)).is_empty());
    assert_eq!(d.cycle(dini()).every, Duration::hours(6));
    let runs = d.cycle(dini()).runs(t(20, 30), Duration::hours(12));
    assert_eq!(runs.first(), Some(&t(18, 0)));
    assert!(runs.iter().all(|r| r.hour() % 6 == 0));
    // Arrival estimates sit at the early end of what was measured.
    assert_eq!(arrival(dl, t(18, 0), 0), t(19, 25));
    assert_eq!(arrival(dl, t(18, 0), 60), t(20, 30));
    assert_eq!(d.due(ig(), PRODUCT_SF, t(18, 0), 72 * 60), t(20, 35));
    let need = Need {
        input: layer::layer("temp").unwrap().input_for(dini()).unwrap(),
        field: None,
        lead: 180,
    };
    assert_eq!(d.file(dini(), t(18, 0), &need), file_key(dl, t(18, 0), 3));
    let mars = SourceDef {
        region: "mars",
        ..*dini()
    };
    assert!(d.check(&mars).is_err() && d.check(dini()).is_ok() && d.check(ig()).is_ok());
}

const PRODUCT_SF: &str = wx_core::families::dmi::PRODUCT;

fn group() -> Vec<&'static str> {
    vec![PRODUCT_SF]
}

/// Only the needed messages are fetched, by the ranges the index gives,
/// neighbours in one request; a file whose index is not written yet is not
/// touched; every part the two steps can make is made.
#[tokio::test]
async fn ranges_from_the_index_make_every_part() {
    let mem = testing::remote(&[]);
    mem.serve_s3(&bucket(), 1000);
    let f3 = publish(
        &mem,
        3,
        fixture("dini_t18z_03.grib"),
        Some(fixture("dini_t18z_03.grib.index")),
    );
    let f2 = publish(
        &mem,
        2,
        fixture("dini_t18z_02.grib"),
        Some(fixture("dini_t18z_02.grib.index")),
    );
    let f4 = publish(&mem, 4, fixture("dini_t18z_03.grib"), None);
    let (engine, store) = testing::engine(&mem);
    let mut task = ModelTask::new(engine.clone(), Arc::new(Dmi), dini(), &group());
    let r = task.cycle(t(21, 20), &|_, _| {}).await.unwrap();
    // Step 3 makes every layer but snowfall, which takes the 00 and 12 UTC
    // runs only (its 12-hour anchor). Step 2 holds two fields: its other
    // layers are given up for want of theirs. Steps 0, 1 and 4 are not out,
    // so step 2's hour of precipitation waits for step 1.
    assert_eq!(
        (r.tally.produced, r.tally.unavailable, r.tally.failed),
        (7, 5, 0),
        "{:?}",
        r.tally
    );
    for l in [
        "temp",
        "feels_like",
        "dew_point",
        "wind",
        "clouds",
        "precip",
        "radar",
    ] {
        let ids: Vec<String> = engine
            .catalog
            .timeline(l)
            .parts
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["dini-26110618-180"], "{l}");
    }
    assert!(engine.catalog.timeline("snow").parts.is_empty());

    // The step 3 inventory, then two ranges: cloud cover to the v wind, and
    // the two run totals with the gust (the wind speed and direction lie
    // between and are not read); the cloud layers and the unnamed field are
    // never fetched.
    let gets = |url: &str| -> Vec<String> {
        mem.requests()
            .into_iter()
            .filter(|r| r.starts_with(url))
            .collect()
    };
    assert_eq!(
        gets(&f3),
        [
            format!("{f3}.index"),
            format!("{f3} [bytes=60919-395513]"),
            format!("{f3} [bytes=547352-820180]")
        ]
    );
    assert_eq!(
        gets(&f2),
        [format!("{f2}.index"), format!("{f2} [bytes=0-181885]")]
    );
    assert!(gets(&f4).is_empty(), "{:?}", gets(&f4));

    // 63.50 N, 22.82 W at 21:00 (ecCodes): 281.7836 K; wind from 206.5
    // degrees true at 9.25 m/s by DMI's own direction field, from 218.7 on
    // the grid; gust 11.32 m/s; overcast.
    let (lon, lat) = (-22.8191, 63.4978);
    let id = "dini-26110618-180";
    let temp = stored_value(&store, "temp", id, 0, lon, lat).await.unwrap();
    assert!((temp - 8.6336).abs() < 0.05, "{temp}");
    let u = stored_value(&store, "wind", id, 0, lon, lat).await.unwrap();
    let v = stored_value(&store, "wind", id, 1, lon, lat).await.unwrap();
    assert!(
        angle_between(direction(u, v), 206.511) < 1.0,
        "{u} {v}: {}",
        direction(u, v)
    );
    assert!((u.hypot(v) - 9.247).abs() < 0.1, "{u} {v}");
    let gust = stored_value(&store, "wind", id, 2, lon, lat).await.unwrap();
    assert!((gust - 11.3157).abs() < 0.1, "{gust}");
    let cloud = stored_value(&store, "clouds", id, 0, lon, lat)
        .await
        .unwrap();
    assert!(cloud > 99.0, "{cloud}");

    // The hour to 21:00 at 63.66 N, 23.60 W: 0.6667 mm, none of it solid.
    let (lon, lat) = (-23.5954, 63.6579);
    let rate = stored_value(&store, "precip", id, 0, lon, lat)
        .await
        .unwrap();
    assert!((rate - 0.6667).abs() < 0.07, "{rate}");
    let kind = stored_value(&store, "precip", id, 1, lon, lat)
        .await
        .unwrap();
    assert_eq!(kind, wx_core::layer::ptype::RAIN);

    // A Lambert grid's box, and a solid box inside it.
    let part = engine.catalog.latest("temp", "dini").unwrap();
    assert!(
        part.bounds.west < -26.0 && part.bounds.east > -17.2 && part.bounds.north > 66.1,
        "{:?}",
        part.bounds
    );
    let solid = part.solid.unwrap();
    assert!(
        solid.west > part.bounds.west && solid.north < part.bounds.north,
        "{solid:?}"
    );

    // What was made or given up is not fetched again, and step 4 still
    // waits for its index.
    mem.clear_log();
    let r = task.cycle(t(21, 31), &|_, _| {}).await.unwrap();
    assert_eq!(r.tally.produced, 0, "{:?}", r.tally);
    assert!(
        gets(&f3).is_empty() && gets(&f2).is_empty() && gets(&f4).is_empty(),
        "{:?}",
        mem.requests()
    );
}

/// DMI's winds are grid-relative. Rotated by the decoder's projection, every
/// point's direction is DMI's own `WDIR` (true north) to within half a
/// degree, while the unrotated grid direction is some 12 degrees off.
#[test]
fn winds_are_rotated_to_true_north() {
    let grib = fixture("dini_t18z_03.grib");
    let wind = layer::layer("wind").unwrap();
    let input = wind.input_for(dini()).unwrap();
    let set = wx_pipeline::decode(&grib, input.fields).unwrap();
    assert!(set.fields[0].grid_relative_winds && set.fields[1].grid_relative_winds);
    let raster = wx_pipeline::derive(wind, input, &set.fields).unwrap();
    let inst = |var| FieldSel {
        var,
        level: "10 m above ground",
        stat: Stat::Instant,
    };
    let own = wx_pipeline::decode(&grib, &[inst("WDIR"), inst("WIND")]).unwrap();
    let (wdir, speed) = (&own.fields[0].values, &own.fields[1].values);
    let (mut worst, mut unrotated, mut n) = (0f32, 0f32, 0);
    for k in 0..wdir.len() {
        if speed[k] < 2.0 {
            continue;
        }
        let (u, v) = (raster.bands[0][k], raster.bands[1][k]);
        worst = worst.max(angle_between(direction(u, v), wdir[k]));
        unrotated += angle_between(
            direction(set.fields[0].values[k], set.fields[1].values[k]),
            wdir[k],
        );
        n += 1;
    }
    assert!(n > 20_000, "{n}");
    assert!(worst < 0.5, "{worst}");
    let unrotated = unrotated / n as f32;
    assert!((10.0..14.0).contains(&unrotated), "{unrotated}");
}

/// Airports (temperature and wind against their observations) and open
/// sea (wind against a global model), where the grids turn the winds by
/// -14 (Kangerlussuaq) to +49 degrees (Longyearbyen).
const PLACES: &[crate::families::live_check::Place] = &[
    ("EKCH", 12.656, 55.618),
    ("EIDW", -6.270, 53.421),
    ("BIKF", -22.605, 63.985),
    ("ENBR", 5.218, 60.294),
    ("EGPH", -3.373, 55.950),
    ("BGGH", -51.678, 64.191),
    ("BGSF", -50.711, 67.012),
    ("ENSB", 15.466, 78.246),
    ("denmark_strait", -28.0, 66.0),
    ("norwegian_sea", 5.0, 68.0),
    ("greenland_sea", -5.0, 75.0),
    ("barents_sea", 30.0, 76.0),
    ("north_sea", 3.0, 56.0),
];

/// One cycle of each DMI source against the bucket: see `live_check`.
/// `cargo test -p wx-ingest --features dmi -- --ignored live_dmi
/// --nocapture`.
#[tokio::test]
#[ignore = "network"]
async fn live_dmi_into_a_store() {
    crate::families::live_check::run(&["dini", "ig"], PLACES).await;
}

/// How long a wind part takes from full-size DINI messages (decode three,
/// rotate, grid), from `WX_FULL_DIR/DINI_03_{UGRD,VGRD,GUST}*.grib`. Run
/// with `--release`.
#[test]
#[ignore = "needs full-size files"]
fn time_a_full_size_wind_part() {
    let Ok(dir) = std::env::var("WX_FULL_DIR") else {
        return;
    };
    let mut grib = Vec::new();
    for name in [
        "UGRD__10_m_above_ground",
        "VGRD__10_m_above_ground",
        "GUST__10_m_above_ground",
    ] {
        grib.extend(std::fs::read(format!("{dir}/DINI_03_{name}.grib")).unwrap());
    }
    let wind = layer::layer("wind").unwrap();
    let input = wind.input_for(dini()).unwrap();
    let started = std::time::Instant::now();
    let set = wx_pipeline::decode(&grib, input.fields).unwrap();
    let decoded_ms = started.elapsed().as_secs_f64() * 1e3;
    let raster = wx_pipeline::derive(wind, input, &set.fields).unwrap();
    let stored = wx_pipeline::FrameGrid::build(wind, &raster).to_bytes();
    let total_ms = started.elapsed().as_secs_f64() * 1e3;
    println!(
        "wind dini: {} MB in, decode {decoded_ms:.0} ms, part {total_ms:.0} ms, {} kB stored",
        grib.len() / 1_000_000,
        stored.len() / 1000
    );
}
