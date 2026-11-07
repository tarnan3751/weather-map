use super::testing::{FakeDatamart, glob};
use super::*;
use crate::driver::Cx;
use crate::testing::fixture;
use wx_core::layer::{self, LayerSpec};
use wx_core::source;

use crate::testing::on as t;

fn eccc_fixture(name: &str) -> Vec<u8> {
    fixture(&format!("eccc/{name}"))
}

const GDPS: Layout = crate::families::gdps::LAYOUT;
const HRDPS: Layout = crate::families::hrdps::LAYOUT;

fn need(layer_id: &str, source_id: &str, field: usize, lead: u32) -> Need {
    let l: &'static LayerSpec = layer::layer(layer_id).unwrap();
    let input = l.input_for(source::source(source_id).unwrap()).unwrap();
    Need {
        input,
        field: Some(input.fields[field]),
        lead,
    }
}

#[test]
fn names_and_directories() {
    let run = t(6, 12, 0);
    assert_eq!(
        GDPS.file_name(run, "AirTemp", "AGL-2m", 3),
        "20261106T12Z_MSC_GDPS_AirTemp_AGL-2m_LatLon0.15_PT003H.grib2"
    );
    assert_eq!(
        HRDPS.file_name(run, "TMP", "AGL-2m", 48),
        "20261106T12Z_MSC_HRDPS_TMP_AGL-2m_RLatLon0.0225_PT048H.grib2"
    );
    assert_eq!(
        GDPS.run_dir(HOST, run.date_naive(), run),
        "https://dd.weather.gc.ca/20261106/WXO-DD/model_gdps/15km/12/"
    );
    let mart = Datamart::new(HOST, HRDPS, registry_levels(Family::Hrdps));
    assert_eq!(
        mart.file(run, &need("wind", "hrdps", 0, 180)),
        "20261106T12Z_MSC_HRDPS_UGRD_AGL-10m_RLatLon0.0225_PT003H.grib2"
    );
    assert_eq!(
        mart.step_index_url(run.date_naive(), run, 7),
        "https://dd.weather.gc.ca/20261106/WXO-DD/model_hrdps/continental/2.5km/12/007/?P=20261106T12Z_MSC_HRDPS_*_%5BAS%5D%5BGf%5D%5BLc%5D*"
    );
    // Not listed yet: the run's own day.
    assert_eq!(
        mart.url(run, 7, "x.grib2"),
        "https://dd.weather.gc.ca/20261106/WXO-DD/model_hrdps/continental/2.5km/12/007/x.grib2"
    );
}

#[test]
fn a_run_is_looked_for_in_the_next_days_tree_once_it_has_begun() {
    let run = t(6, 18, 0);
    let day = |d: u32| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
    assert_eq!(trees(run, t(6, 23, 59)), [day(6)]);
    assert_eq!(trees(run, t(7, 0, 1)), [day(6), day(7)]);
}

/// The pattern keeps every file the registry reads and drops the
/// pressure levels, which are most of a step.
#[test]
fn the_index_pattern_keeps_what_the_registry_reads() {
    let levels = registry_levels(Family::Gdps);
    assert_eq!(levels.len(), 3, "{levels:?}");
    let p = index_pattern("20261106T12Z_MSC_GDPS", &levels).unwrap();
    assert_eq!(p, "20261106T12Z_MSC_GDPS_*_[AS][Gf][Lc]*");
    let run = t(6, 12, 0);
    for (_, input) in wx_core::layer::layers_for(Family::Gdps) {
        for f in input.fields {
            let name = GDPS.file_name(run, f.var, f.level, 3);
            assert!(glob(&p, &name), "{name}");
        }
    }
    assert!(!glob(&p, &GDPS.file_name(run, "AirTemp", "IsbL-0500", 3)));
    assert!(!glob(
        &p,
        "20261106T12Z_MSC_GDPS_HighLowPressure_MSL_LatLon0.15_PT003H.json"
    ));
    let h = index_pattern("20261106T12Z_MSC_HRDPS", &registry_levels(Family::Hrdps)).unwrap();
    assert!(!glob(
        &h,
        "20261106T12Z_MSC_HRDPS-WEonG_PCPNTYPE_Sfc_RLatLon0.0225_PT003H.grib2"
    ));
    assert!(glob(&h, &HRDPS.file_name(run, "PTYPE", "Sfc", 3)));
    // Pattern syntax in a level, or no levels: no pattern.
    assert_eq!(index_pattern("x", &["A*B"]), None);
    assert_eq!(index_pattern("x", &[]), None);
    assert_eq!(
        index_pattern("x", &["Sfc", "M"]).as_deref(),
        Some("x_*_[MS]*")
    );
}

#[test]
fn a_step_index_is_kept_while_its_directory_has_not_changed() {
    let at = t(6, 15, 58);
    let index = |listed_at| StepIndex {
        modified: Some(at),
        listed_at,
        files: Vec::new(),
    };
    // Read two minutes after the time it shows: nothing can have changed
    // unseen.
    assert!(index(t(6, 16, 0)).holds(Some(at)));
    assert!(!index(t(6, 16, 0)).holds(Some(t(6, 16, 1))));
    // Read in the same minute: a later change in that minute would not show.
    assert!(!index(t(6, 15, 58) + Duration::seconds(30)).holds(Some(at)));
    assert!(!index(t(6, 16, 0)).holds(None));
}

/// Times are what the selector asks for; a file whose name says one thing
/// and holds another is refused.
#[test]
fn messages_are_taken_by_their_times() {
    let run = t(6, 12, 0);
    let scan = |name: &str| wx_pipeline::scan(&eccc_fixture(name)).unwrap().remove(0);
    let tmp = scan("hrdps_t12z_PT003H__TMP_AGL-2m__x4.grib2");
    let weasn = scan("hrdps_t12z_PT003H__WEASN_Sfc__x4.grib2");
    let hour = wx_pipeline::scan(&fixture("hrdps_t12z_PT003H__WEASN-Accum1h_Sfc.grib2"))
        .unwrap()
        .remove(0);
    let temp = need("temp", "hrdps", 0, 180);
    assert!(holds(&tmp, &temp, run));
    assert!(
        !holds(&tmp, &need("temp", "hrdps", 0, 120), run),
        "another step"
    );
    assert!(!holds(&tmp, &temp, t(6, 6, 0)), "another run");
    let snow = need("snow", "hrdps", 0, 180);
    assert!(holds(&weasn, &snow, run));
    assert!(
        !holds(&hour, &snow, run),
        "the last hour is not a run total"
    );
    assert!(!holds(&tmp, &snow, run), "an instant is not a run total");
    // The hour before the step, for a step amount (GDPS's precipitation).
    let precip = need("precip", "gdps", 0, 180);
    assert_eq!(precip.field.unwrap().stat, Stat::StepPerHour);
    assert!(holds(&hour, &precip, run));
    assert!(!holds(&weasn, &precip, run));
    let p1h = scan("gdps_t12z_PT003H__Precip-Accum1h_Sfc__x4.grib2");
    assert!(holds(&p1h, &precip, run));
}

async fn listed(
    fake: &Arc<FakeDatamart>,
    mart: &Datamart,
    run: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Listing {
    let remote = fake.remote();
    let cx = Cx::new(&remote, "test", now);
    mart.list(&cx, run, &(0..=48).collect::<Vec<_>>())
        .await
        .unwrap()
}

/// A run is listed from its index and its steps' indexes; a step is read
/// again only when it changed.
#[tokio::test]
async fn listing_reads_only_steps_that_changed() {
    let fake = FakeDatamart::new();
    let mart = Datamart::new(HOST, HRDPS, registry_levels(Family::Hrdps));
    let run = t(6, 12, 0);
    let day = run.date_naive();
    let dir = |h: u32| format!("{}{h:03}/", HRDPS.run_dir(HOST, day, run));
    let publish = |h: u32, var: &str, level: &str, at| {
        fake.publish(
            &format!("{}{}", dir(h), HRDPS.file_name(run, var, level, h)),
            vec![0u8; 3000],
            at,
        )
    };
    // Step 0 has its temperature; step 1 only its JSON so far; a pressure
    // level the pattern leaves out.
    publish(0, "TMP", "AGL-2m", t(6, 14, 48));
    publish(0, "TMP", "IsbL-0500", t(6, 14, 48));
    fake.publish(
        &format!(
            "{}20261106T12Z_MSC_HRDPS_HighLowPressure_MSL_RLatLon0.0225_PT001H.json",
            dir(1)
        ),
        vec![1],
        t(6, 14, 40),
    );
    let l = listed(&fake, &mart, run, t(6, 14, 55)).await;
    assert!(l.contains(&HRDPS.file_name(run, "TMP", "AGL-2m", 0)));
    assert_eq!(l.len(), 1, "the pressure level and the JSON are not ours");
    // Sizes as the index abbreviates them (`2.9K`).
    assert_eq!(
        l.size(&HRDPS.file_name(run, "TMP", "AGL-2m", 0)),
        Some(Some(2970))
    );
    let requests = fake.requests();
    assert_eq!(requests.len(), 3, "{requests:#?}");
    assert_eq!(requests[0], HRDPS.run_dir(HOST, day, run));
    assert_eq!(requests[1], mart.step_index_url(day, run, 0));

    // Unchanged steps read long enough after their last change are kept.
    fake.clear_log();
    publish(1, "TMP", "AGL-2m", t(6, 14, 58));
    let l = listed(&fake, &mart, run, t(6, 15, 1)).await;
    assert!(l.contains(&HRDPS.file_name(run, "TMP", "AGL-2m", 1)));
    assert_eq!(
        fake.requests(),
        [
            HRDPS.run_dir(HOST, day, run),
            mart.step_index_url(day, run, 1)
        ]
    );
    fake.clear_log();
    let l = listed(&fake, &mart, run, t(6, 15, 4)).await;
    assert_eq!(l.len(), 2);
    assert_eq!(fake.requests(), [HRDPS.run_dir(HOST, day, run)]);
}

/// A run still publishing at midnight continues in the next day's tree,
/// and files are fetched from where they were listed.
#[tokio::test]
async fn a_run_spans_two_date_trees() {
    let fake = FakeDatamart::new();
    let mart = Datamart::new(HOST, HRDPS, registry_levels(Family::Hrdps));
    let run = t(6, 18, 0);
    let (d6, d7) = (run.date_naive(), t(7, 0, 0).date_naive());
    let name = |h| HRDPS.file_name(run, "TMP", "AGL-2m", h);
    fake.publish(
        &format!("{}047/{}", HRDPS.run_dir(HOST, d6, run), name(47)),
        vec![0u8; 10],
        t(6, 23, 58),
    );
    fake.publish(
        &format!("{}048/{}", HRDPS.run_dir(HOST, d7, run), name(48)),
        vec![0u8; 10],
        t(7, 0, 2),
    );
    let before = listed(&fake, &mart, run, t(6, 23, 59)).await;
    assert!(before.contains(&name(47)) && !before.contains(&name(48)));
    let after = listed(&fake, &mart, run, t(7, 0, 5)).await;
    assert!(after.contains(&name(47)) && after.contains(&name(48)));
    assert_eq!(
        mart.url(run, 48, &name(48)),
        format!("{}048/{}", HRDPS.run_dir(HOST, d7, run), name(48))
    );
    assert_eq!(
        mart.url(run, 47, &name(47)),
        format!("{}047/{}", HRDPS.run_dir(HOST, d6, run), name(47))
    );
}
