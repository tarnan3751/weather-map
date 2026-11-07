//! Fixtures (`fixtures/eccc/hrdps_*__x4`) are real files of the 2026-11-06
//! 12 UTC run cut to every fourth point of every fourth row (0.09 degree,
//! the same rotated grid), keeping their JPEG 2000 packing;
//! `fixtures/hrdps_t12z_PT003H__PRATE_Sfc.grib2` is a whole one.

use super::*;
use crate::driver::model::ModelTask;
use crate::engine::PartJob;
use crate::families::eccc::testing::FakeDatamart;
use crate::testing::{self, fixture};
use std::sync::Arc;
use wx_core::LAYERS;
use wx_core::frame::FORECAST_GRACE_MINUTES;
use wx_core::layer::{self, ptype};

use crate::testing::on as t;

fn hrdps() -> &'static SourceDef {
    crate::testing::source("hrdps")
}

fn run() -> DateTime<Utc> {
    t(6, 12, 0)
}

/// Places on both sides of the border, `(name, lon, lat, blanked)`. The
/// polygons are within 20 km of the border, so the places are well away
/// from it.
const PLACES: [(&str, f64, f64, bool); 12] = [
    ("Lewistown, Montana", -109.43, 47.06, true),
    ("Dauphin, Manitoba", -100.05, 51.15, false),
    ("Winnipeg, Manitoba", -97.14, 49.9, false),
    ("Fairbanks, Alaska", -147.72, 64.84, true),
    ("Whitehorse, Yukon", -135.06, 60.72, false),
    ("Seattle", -122.33, 47.61, true),
    ("Kamloops, British Columbia", -120.33, 50.67, false),
    ("Detroit", -83.05, 42.33, true),
    ("Toronto", -79.38, 43.65, false),
    ("Bangor, Maine", -68.77, 44.8, true),
    ("Fredericton, New Brunswick", -66.64, 45.96, false),
    ("Lake Superior, US side", -87.5, 47.4, true),
];

fn url(var: &str, level: &str, hour: u32) -> String {
    let r = run();
    format!(
        "{}{hour:03}/{}",
        LAYOUT.run_dir(eccc::HOST, r.date_naive(), r),
        LAYOUT.file_name(r, var, level, hour)
    )
}

fn cut(var: &str, level: &str, hour: u32) -> Vec<u8> {
    fixture(&format!(
        "eccc/hrdps_t12z_PT{hour:03}H__{var}_{level}__x4.grib2"
    ))
}

const STEP_3: [(&str, &str); 9] = [
    ("TMP", "AGL-2m"),
    ("DPT", "AGL-2m"),
    ("UGRD", "AGL-10m"),
    ("VGRD", "AGL-10m"),
    ("GUST", "AGL-10m"),
    ("PRATE", "Sfc"),
    ("PTYPE", "Sfc"),
    ("WEASN", "Sfc"),
    ("TCDC", "Sfc"),
];

fn need(layer_id: &str, field: usize, lead: u32) -> Need {
    let input = layer::layer(layer_id).unwrap().input_for(hrdps()).unwrap();
    Need {
        input,
        field: Some(input.fields[field]),
        lead,
    }
}

/// Decode with the adapter's decoder (which blanks the US).
fn decode(h: &Hrdps, bytes: &[u8], need: &Need) -> Field {
    let blanking = h.blanking.clone();
    let d = eccc::decoder_then(run(), move |f| blanking.apply(f));
    d(bytes, need).unwrap().field
}

fn at(f: &Field, lon: f64, lat: f64) -> f32 {
    let (i, j) = f.proj.lonlat_to_ij(lon, lat);
    let (i, j) = (i.round() as usize, j.round() as usize);
    assert!(i < f.nx && j < f.ny, "({lon}, {lat}) is off the grid");
    f.values[j * f.nx + i]
}

#[test]
fn steps_names_and_arrival() {
    let h = Hrdps::default();
    let leads = h.leads(hrdps(), reg::PRODUCT, run());
    assert_eq!((leads.len(), *leads.last().unwrap()), (49, 48 * 60));
    assert!(h.leads(hrdps(), reg::PRODUCT, t(6, 13, 0)).is_empty());
    assert_eq!(h.leads(hrdps(), reg::PRODUCT, t(6, 18, 0)).len(), 49);
    assert_eq!(arrival(run(), 0), t(6, 14, 48));
    assert_eq!(arrival(run(), 1), t(6, 14, 58));
    assert_eq!(arrival(run(), 48), t(6, 15, 16));
    assert_eq!(
        h.file(hrdps(), run(), &need("radar", 0, 180)),
        "20261106T12Z_MSC_HRDPS_PRATE_Sfc_RLatLon0.0225_PT003H.grib2"
    );
    assert_eq!(h.cycle(hrdps()).every, Duration::hours(6));
}

/// What step 0 lacks is never asked for there: no input reading it fills
/// the past or has a step 0, and step 0 lands after its grace has run out.
#[tokio::test]
async fn what_step_0_lacks_is_never_wanted() {
    let lacking = |i: &wx_core::InputSpec| {
        i.fields.iter().any(|f| {
            reg::FIRST_STEP_LACKS
                .iter()
                .any(|l| (l.var, l.level) == (f.var, f.level))
        })
    };
    let mut seen = 0;
    for (l, input) in crate::driver::inputs_of(LAYERS, hrdps()) {
        if !lacking(input) {
            continue;
        }
        seen += 1;
        let job = PartJob::forecast(l, input, hrdps(), run(), 0, None, Default::default());
        assert!(job.is_none() || !input.fill_past, "{}", l.id);
    }
    assert!(seen >= 4);
    assert!(arrival(run(), 0) - run() > Duration::minutes(FORECAST_GRACE_MINUTES));
    // So a cycle at step 0's arrival wants none of it.
    let fake = FakeDatamart::new();
    let (engine, _) = fake.engine();
    let group = vec![reg::PRODUCT];
    let task = ModelTask::new(engine, Arc::new(Hrdps::default()), hrdps(), &group);
    for job in task
        .wanted(run(), arrival(run(), 0))
        .iter()
        .filter(|j| j.lead == 0)
    {
        assert!(!lacking(job.input), "{} at step 0", job.layer.id);
    }
}

/// The US is blanked, Canada is not; the cells are worked out once per grid.
#[test]
fn the_us_is_blanked() {
    let h = Hrdps::default();
    let tmp = decode(&h, &cut("TMP", "AGL-2m", 3), &need("temp", 0, 180));
    for (name, lon, lat, blanked) in PLACES {
        let v = at(&tmp, lon, lat);
        assert_eq!(v.is_nan(), blanked, "{name}: {v}");
    }
    // The sea south of the US is blanked too, the Atlantic off Nova Scotia
    // is not.
    assert!(at(&tmp, -70.0, 38.0).is_nan());
    assert!(at(&tmp, -62.0, 43.0).is_finite());
    let share = tmp.values.iter().filter(|v| v.is_nan()).count() as f64 / tmp.values.len() as f64;
    assert!(
        (0.25..0.5).contains(&share),
        "{share} of the domain blanked"
    );
    // Another field on the same grid reuses the cells.
    let dpt = decode(&h, &cut("DPT", "AGL-2m", 3), &need("dew_point", 0, 180));
    assert!(at(&dpt, -109.43, 47.06).is_nan() && at(&dpt, -97.14, 49.9).is_finite());
    assert_eq!(h.blanking().computed(), 1);
    let cells = h.blanking().cells(tmp.nx, tmp.ny, &tmp.proj);
    assert!(
        cells.len() < 2 * tmp.ny,
        "{} runs for {} rows",
        cells.len(),
        tmp.ny
    );

    // The whole grid: worked out once more, the same places.
    let started = std::time::Instant::now();
    let prate = decode(
        &h,
        &fixture("hrdps_t12z_PT003H__PRATE_Sfc.grib2"),
        &need("radar", 0, 180),
    );
    assert_eq!((prate.nx, prate.ny), (2540, 1290));
    for (name, lon, lat, blanked) in PLACES {
        assert_eq!(at(&prate, lon, lat).is_nan(), blanked, "{name}");
    }
    assert_eq!(h.blanking().computed(), 2);
    eprintln!(
        "decoding and blanking the whole grid took {:?}",
        started.elapsed()
    );
    let started = std::time::Instant::now();
    let cells = Blanking::new(reg::MASK_OUT).cells(prate.nx, prate.ny, &prate.proj);
    eprintln!(
        "working out its blanked cells took {:?} ({} runs)",
        started.elapsed(),
        cells.len()
    );
}

/// Grid-relative winds come out earth-relative: their direction is ECCC's
/// own `WDIR` (degrees true), and GDPS's at the same places.
#[test]
fn winds_come_out_earth_relative() {
    let h = Hrdps::default();
    let wind = layer::layer("wind").unwrap();
    let input = wind.input_for(hrdps()).unwrap();
    let fields: Vec<Field> = [("UGRD", 0), ("VGRD", 1), ("GUST", 2)]
        .iter()
        .map(|&(v, k)| decode(&h, &cut(v, "AGL-10m", 3), &need("wind", k, 180)))
        .collect();
    assert!(fields[0].grid_relative_winds && fields[0].proj.rotates_winds());
    let raster = wx_pipeline::derive(wind, input, &fields).unwrap();
    let wdir = wx_pipeline::decode_messages(&cut("WDIR", "AGL-10m", 3), &[0])
        .unwrap()
        .remove(0)
        .field;
    let from = |u: f32, v: f32| (-u).atan2(-v).to_degrees().rem_euclid(360.0);
    let diff = |a: f32, b: f32| ((a - b + 540.0).rem_euclid(360.0) - 180.0).abs();
    let (mut worst, mut unrotated, mut n) = (0f32, 0f32, 0);
    for k in 0..raster.len() {
        let (u, v) = (raster.bands[0][k], raster.bands[1][k]);
        // Calm, or blanked.
        if u.hypot(v) < 3.0 || u.is_nan() {
            continue;
        }
        n += 1;
        worst = worst.max(diff(from(u, v), wdir.values[k]));
        unrotated = unrotated.max(diff(
            from(fields[0].values[k], fields[1].values[k]),
            wdir.values[k],
        ));
    }
    assert!(n > 20_000, "{n} cells compared");
    assert!(worst < 3.0, "{worst} degrees off ECCC's direction");
    assert!(
        unrotated > 30.0,
        "unrotated, the east of the domain is off by {unrotated} degrees"
    );

    // GDPS at the same places and time, a different model: close.
    let g = eccc::decoder(run());
    let gwind = wind.input_for(crate::testing::source("gdps")).unwrap();
    let gneed = |k| Need {
        input: gwind,
        field: Some(gwind.fields[k]),
        lead: 180,
    };
    let gu = g(
        &fixture("eccc/gdps_t12z_PT003H__WindU_AGL-10m__x4.grib2"),
        &gneed(0),
    )
    .unwrap()
    .field;
    let gv = g(
        &fixture("eccc/gdps_t12z_PT003H__WindV_AGL-10m__x4.grib2"),
        &gneed(1),
    )
    .unwrap()
    .field;
    // Around the Canadian places (the US ones are blanked).
    let mut diffs = Vec::new();
    for &(_, lon, lat, _) in PLACES.iter().filter(|p| !p.3) {
        for (dlon, dlat) in [
            (0.0, 0.0),
            (2.0, 1.0),
            (-2.0, -1.0),
            (1.0, -1.0),
            (-1.0, 1.0),
        ] {
            let (lon, lat) = (lon + dlon, lat + dlat);
            let (i, j) = fields[0].proj.lonlat_to_ij(lon, lat);
            let k = j.round() as usize * fields[0].nx + i.round() as usize;
            let (u, v) = (raster.bands[0][k], raster.bands[1][k]);
            let (gu, gv) = (at(&gu, lon, lat), at(&gv, lon, lat));
            if u.hypot(v) > 4.0 && gu.hypot(gv) > 4.0 {
                diffs.push(diff(from(u, v), from(gu, gv)));
            }
        }
    }
    diffs.sort_by(f32::total_cmp);
    assert!(
        diffs.len() >= 5 && diffs[diffs.len() / 2] < 20.0,
        "{diffs:?}"
    );
}

/// The precipitation type codes, through ECCC's table: snow where it is
/// cold, rain where it is warm.
#[test]
fn precipitation_types_follow_the_temperature() {
    let h = Hrdps::default();
    let precip = layer::layer("precip").unwrap();
    let input = precip.input_for(hrdps()).unwrap();
    let fields = [
        decode(&h, &cut("PRATE", "Sfc", 3), &need("precip", 0, 180)),
        decode(&h, &cut("PTYPE", "Sfc", 3), &need("precip", 1, 180)),
    ];
    let raster = wx_pipeline::derive(precip, input, &fields).unwrap();
    let tmp = decode(&h, &cut("TMP", "AGL-2m", 3), &need("temp", 0, 180));
    let mut by_type: std::collections::BTreeMap<i32, Vec<f32>> = Default::default();
    for k in 0..raster.len() {
        if raster.bands[0][k] >= 0.1 {
            by_type
                .entry(raster.bands[1][k] as i32)
                .or_default()
                .push(tmp.values[k] - 273.15);
        }
    }
    let mut median = |code: f32| {
        let v = by_type.get_mut(&(code as i32)).unwrap();
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    let (snow, rain, freezing) = (
        median(ptype::SNOW),
        median(ptype::RAIN),
        median(ptype::FREEZING_RAIN),
    );
    assert!(
        snow < -1.0 && rain > 4.0 && (-4.0..0.5).contains(&freezing),
        "snow {snow}, rain {rain}, freezing rain {freezing}"
    );
}

/// One cycle: every layer's part from step 3, with the US blanked in the
/// stored grids and the solid box in Canada.
#[tokio::test]
async fn every_layer_from_one_step_with_the_us_blanked() {
    let fake = FakeDatamart::new();
    for (var, level) in STEP_3 {
        fake.publish(&url(var, level, 3), cut(var, level, 3), t(6, 14, 58));
    }
    let (engine, store) = fake.engine();
    let group = vec![reg::PRODUCT];
    let mut task = ModelTask::new(engine.clone(), Arc::new(Hrdps::default()), hrdps(), &group);
    let report = task.cycle(t(6, 15, 5), &|_, _| {}).await.unwrap();
    assert_eq!(report.tally.produced, 8, "{:?}", report.tally);
    assert_eq!(
        report.tally.failed + report.tally.unavailable,
        0,
        "{:?}",
        report.tally
    );
    for l in [
        "radar",
        "precip",
        "snow",
        "temp",
        "feels_like",
        "dew_point",
        "wind",
        "clouds",
    ] {
        let part = engine
            .catalog
            .latest(l, "hrdps")
            .unwrap_or_else(|| panic!("{l}"));
        assert_eq!(part.id, "hrdps-26110612-180");
        let grid = testing::stored_grid(store.as_ref(), l, &part.id).await;
        for (name, lon, lat, blanked) in PLACES {
            let w = grid.coverage_at(lon, lat);
            if blanked {
                assert_eq!(w, 0.0, "{l}: {name}");
            }
        }
        assert_eq!(grid.coverage_at(-100.05, 51.15), 1.0, "{l}: Manitoba");
        let solid = part.solid.unwrap_or_else(|| panic!("{l}: no solid box"));
        for (name, lon, lat, blanked) in PLACES {
            assert!(
                !(blanked && solid.contains(lon, lat)),
                "{l}: the solid box {solid:?} holds {name}"
            );
        }
        // The box is the data's: it reaches the Pacific corner of the grid
        // but not the blanked United States south of it.
        assert!(
            part.bounds.west < -142.0 && part.bounds.south > 30.0,
            "{l}: {:?}",
            part.bounds
        );
    }
    assert!(testing::keys(store.as_ref()).await.len() >= 8);
}
