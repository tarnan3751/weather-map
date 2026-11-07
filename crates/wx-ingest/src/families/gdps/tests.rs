//! Fixtures (`fixtures/eccc/gdps_*`) are real files of the 2026-11-06 12 UTC
//! run cut to every fourth point of every fourth row (0.6 degree), keeping
//! their JPEG 2000 packing.

use super::*;
use crate::driver::model::ModelTask;
use crate::families::eccc::testing::FakeDatamart;
use crate::testing::{self, fixture};
use std::sync::Arc;
use wx_core::layer::{self, Stat};

use crate::testing::on as t;

fn gdps() -> &'static SourceDef {
    crate::testing::source("gdps")
}

const RUN: (u32, u32) = (6, 12);

fn run() -> DateTime<Utc> {
    t(RUN.0, RUN.1, 0)
}

fn url(var: &str, level: &str, hour: u32) -> String {
    let r = run();
    format!(
        "{}{hour:03}/{}",
        LAYOUT.run_dir(eccc::HOST, r.date_naive(), r),
        LAYOUT.file_name(r, var, level, hour)
    )
}

/// Publish the cut fixture of `var` at `level` and step `hour`.
fn publish(fake: &FakeDatamart, var: &str, level: &str, hour: u32, at: DateTime<Utc>) {
    let name = format!("eccc/gdps_t12z_PT{hour:03}H__{var}_{level}__x4.grib2");
    fake.publish(&url(var, level, hour), fixture(&name), at);
}

/// Every field step 3 has in the fixtures (the wind direction is the
/// oracle, not something the registry reads).
const STEP_3: [(&str, &str); 9] = [
    ("AirTemp", "AGL-2m"),
    ("DewPoint", "AGL-2m"),
    ("WindU", "AGL-10m"),
    ("WindV", "AGL-10m"),
    ("WindGust", "AGL-10m"),
    ("TotalCloudCover", "Sfc"),
    ("Precip-Accum1h", "Sfc"),
    ("Snow-Accum1h", "Sfc"),
    ("Snow-Accum", "Sfc"),
];

fn task(engine: Arc<crate::engine::Engine>) -> ModelTask {
    let group = vec![reg::PRODUCT];
    ModelTask::new(engine, Arc::new(Gdps::default()), gdps(), &group)
}

#[test]
fn steps_names_and_arrival() {
    let g = Gdps::default();
    let leads = g.leads(gdps(), reg::PRODUCT, run());
    assert_eq!(leads.len(), 85);
    assert_eq!((leads[1], *leads.last().unwrap()), (60, 84 * 60));
    assert!(
        g.leads(gdps(), reg::PRODUCT, t(6, 6, 0)).is_empty(),
        "no 06 UTC run"
    );
    assert_eq!(g.steps(gdps(), reg::PRODUCT), Steps::Windows);
    assert_eq!(arrival(run(), 0), t(6, 15, 50));
    assert_eq!(arrival(run(), 120), t(6, 16, 40));
    let temp = layer::layer("temp").unwrap().input_for(gdps()).unwrap();
    let need = Need {
        input: temp,
        field: Some(temp.fields[0]),
        lead: 180,
    };
    assert_eq!(
        g.file(gdps(), run(), &need),
        "20261106T12Z_MSC_GDPS_AirTemp_AGL-2m_LatLon0.15_PT003H.grib2"
    );
    // At 16:00 the newest run that has started is 12 UTC; the past window
    // (12 hours) and a spare reach back to 12 UTC the day before.
    let runs = g.cycle(gdps()).runs(t(6, 16, 0), Duration::hours(12));
    assert_eq!(runs, [t(6, 12, 0), t(6, 0, 0), t(5, 12, 0)]);
    assert!(g.check(gdps()).is_ok());
}

/// Precipitation is each hour's own amounts: asked for at the step only,
/// never at the step before.
#[test]
fn precipitation_needs_the_hours_own_amounts() {
    let l = layer::layer("precip").unwrap();
    let job = crate::engine::PartJob::forecast(
        l,
        l.input_for(gdps()).unwrap(),
        gdps(),
        run(),
        180,
        Some(120),
        Steps::Windows,
    )
    .unwrap();
    let needs = job.needs();
    assert_eq!(needs.len(), 2);
    assert!(
        needs
            .iter()
            .all(|n| n.lead == 180 && n.field.unwrap().stat == Stat::StepPerHour)
    );
    let g = Gdps::default();
    assert_eq!(
        g.file(gdps(), run(), &needs[0]),
        "20261106T12Z_MSC_GDPS_Precip-Accum1h_Sfc_LatLon0.15_PT003H.grib2"
    );
    assert_eq!(
        g.file(gdps(), run(), &needs[1]),
        "20261106T12Z_MSC_GDPS_Snow-Accum1h_Sfc_LatLon0.15_PT003H.grib2"
    );
    // No step 0.
    assert!(
        crate::engine::PartJob::forecast(
            l,
            l.input_for(gdps()).unwrap(),
            gdps(),
            run(),
            0,
            None,
            Steps::Windows
        )
        .is_none()
    );
}

/// Every layer GDPS feeds gets its part from one step, and the global grid
/// spans the world.
#[tokio::test]
async fn every_layer_from_one_step() {
    let fake = FakeDatamart::new();
    for (var, level) in STEP_3 {
        publish(&fake, var, level, 3, t(6, 15, 51));
    }
    let (engine, store) = fake.engine();
    let report = task(engine.clone())
        .cycle(t(6, 15, 55), &|_, _| {})
        .await
        .unwrap();
    assert_eq!(report.tally.produced, 7, "{:?}", report.tally);
    assert_eq!(
        (report.tally.failed, report.tally.unavailable),
        (0, 0),
        "{:?}",
        report.tally
    );
    let keys = testing::keys(store.as_ref()).await;
    for l in [
        "temp",
        "dew_point",
        "feels_like",
        "wind",
        "clouds",
        "precip",
        "snow",
    ] {
        let part = engine
            .catalog
            .latest(l, "gdps")
            .unwrap_or_else(|| panic!("{l}"));
        assert_eq!(part.id, "gdps-26110612-180", "{l}");
        assert!(keys.contains(&wx_core::keys::grid(l, &part.id)), "{l}");
        assert!(
            part.bounds.east - part.bounds.west > 359.0
                && part.bounds.north - part.bounds.south > 179.0,
            "{l}: {:?}",
            part.bounds
        );
    }
    assert!(engine.catalog.latest("radar", "gdps").is_none());
    // Each file was fetched once, whole, though several layers read it.
    let gets: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|r| r.ends_with(".grib2"))
        .collect();
    assert_eq!(gets.len(), STEP_3.len(), "{gets:#?}");
}

/// A run that is only partly out: what is there is used, the rest waits
/// (no failures), and a later look reads only the steps that changed.
#[tokio::test]
async fn a_partly_published_run() {
    let fake = FakeDatamart::new();
    for h in 0..=2 {
        publish(&fake, "AirTemp", "AGL-2m", h, t(6, 15, 52));
    }
    // Step 3 is only its JSON so far.
    let json = url("HighLowPressure", "MSL", 3).replace(".grib2", ".json");
    fake.publish(&json, vec![b'{'], t(6, 15, 50));
    let (engine, _) = fake.engine();
    let mut task = task(engine.clone());
    let report = task.cycle(t(6, 15, 56), &|_, _| {}).await.unwrap();
    assert_eq!(report.tally.produced, 3, "{:?}", report.tally);
    assert_eq!(report.tally.failed + report.tally.unavailable, 0);
    let temp: Vec<String> = engine
        .catalog
        .timeline("temp")
        .parts
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(
        temp,
        ["gdps-26110612-0", "gdps-26110612-60", "gdps-26110612-120"]
    );
    assert_eq!(task.newest_listed(), Some((run(), 3)));
    // Overdue files are looked for again after the relist interval.
    assert!(
        report.wake <= t(6, 15, 56) + Duration::minutes(5),
        "{}",
        report.wake
    );

    // Step 3 lands: the next look reads the run's index and step 3's only.
    publish(&fake, "AirTemp", "AGL-2m", 3, t(6, 15, 59));
    fake.clear_log();
    let report = task.cycle(t(6, 16, 2), &|_, _| {}).await.unwrap();
    assert_eq!(report.tally.produced, 1, "{:?}", report.tally);
    let g = Gdps::default();
    let day = run().date_naive();
    let lists: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|r| !r.ends_with(".grib2"))
        .collect();
    let run_dir = LAYOUT.run_dir(eccc::HOST, day, run());
    // The older runs, not there at all, are looked for too.
    let others: Vec<String> = [t(6, 0, 0), t(5, 12, 0)]
        .iter()
        .map(|r| LAYOUT.run_dir(eccc::HOST, r.date_naive(), *r))
        .collect();
    assert_eq!(
        lists[..2],
        [run_dir, g.datamart().step_index_url(day, run(), 3)],
        "{lists:#?}"
    );
    assert!(lists[2..].iter().all(|l| others.contains(l)), "{lists:#?}");
    assert!(
        engine
            .catalog
            .latest("temp", "gdps")
            .is_some_and(|p| p.id == "gdps-26110612-180")
    );
}

/// GDPS's winds are earth-relative already: decoded as they are, they give
/// GDPS's own wind direction.
#[test]
fn winds_match_the_models_own_direction() {
    let decode = eccc::decoder(run());
    let wind = layer::layer("wind").unwrap();
    let input = wind.input_for(gdps()).unwrap();
    let field = |var: &str, k: usize| {
        let need = Need {
            input,
            field: Some(input.fields[k]),
            lead: 180,
        };
        decode(
            &fixture(&format!("eccc/gdps_t12z_PT003H__{var}_AGL-10m__x4.grib2")),
            &need,
        )
        .unwrap()
        .field
    };
    let fields = [field("WindU", 0), field("WindV", 1), field("WindGust", 2)];
    assert!(!fields[0].grid_relative_winds);
    let raster = wx_pipeline::derive(wind, input, &fields).unwrap();
    let dir = &wx_pipeline::decode_messages(
        &fixture("eccc/gdps_t12z_PT003H__WindDir_AGL-10m__x4.grib2"),
        &[0],
    )
    .unwrap()[0];
    assert_eq!(
        dir.info.grid,
        Some((fields[0].nx, fields[0].ny, fields[0].proj.clone()))
    );
    let mut worst: f32 = 0.0;
    for k in 0..raster.len() {
        let (u, v) = (raster.bands[0][k], raster.bands[1][k]);
        if u.hypot(v) < 2.0 {
            continue;
        }
        let from = (-u).atan2(-v).to_degrees().rem_euclid(360.0);
        let d = (from - dir.field.values[k] + 540.0).rem_euclid(360.0) - 180.0;
        worst = worst.max(d.abs());
    }
    assert!(
        worst < 2.0,
        "the direction from u and v is {worst} degrees off GDPS's own"
    );
}

/// The precipitation part from the hour's own amounts: the rate is the
/// hour's amount, and the type follows the snow share, so snow is where it
/// is cold. (Under about 0.1 mm the two amounts' packing makes the share
/// noisy, and some light snow comes out as a mix.)
#[test]
fn precipitation_from_the_hours_own_amounts() {
    use wx_core::layer::ptype;
    let decode = eccc::decoder(run());
    let precip = layer::layer("precip").unwrap();
    let input = precip.input_for(gdps()).unwrap();
    let field = |name: &str, k: usize| {
        let need = Need {
            input,
            field: Some(input.fields[k]),
            lead: 180,
        };
        decode(
            &fixture(&format!("eccc/gdps_t12z_PT003H__{name}__x4.grib2")),
            &need,
        )
        .unwrap()
        .field
    };
    let fields = [field("Precip-Accum1h_Sfc", 0), field("Snow-Accum1h_Sfc", 1)];
    let raster = wx_pipeline::derive(precip, input, &fields).unwrap();
    let temp = layer::layer("temp").unwrap().input_for(gdps()).unwrap();
    let t2m = decode(
        &fixture("eccc/gdps_t12z_PT003H__AirTemp_AGL-2m__x4.grib2"),
        &Need {
            input: temp,
            field: Some(temp.fields[0]),
            lead: 180,
        },
    )
    .unwrap()
    .field;
    let mut by_type: std::collections::BTreeMap<i32, Vec<f32>> = Default::default();
    for k in 0..raster.len() {
        let (rate, kind) = (raster.bands[0][k], raster.bands[1][k]);
        assert!(
            (rate - fields[0].values[k].max(0.0)).abs() < 1e-6,
            "the rate is the hour's amount"
        );
        if rate >= 0.2 {
            by_type
                .entry(kind as i32)
                .or_default()
                .push(t2m.values[k] - 273.15);
        }
    }
    let median = |v: &mut Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    let snow = median(by_type.get_mut(&(ptype::SNOW as i32)).unwrap());
    let rain = median(by_type.get_mut(&(ptype::RAIN as i32)).unwrap());
    let mix = median(by_type.get_mut(&(ptype::MIX as i32)).unwrap());
    assert!(
        snow < -1.0 && rain > 5.0 && (-1.0..4.0).contains(&mix),
        "median 2 m temperatures: snow {snow}, rain {rain}, mix {mix}"
    );
    assert!(
        !by_type.contains_key(&(ptype::FREEZING_RAIN as i32))
            && !by_type.contains_key(&(ptype::SLEET as i32))
    );
}
