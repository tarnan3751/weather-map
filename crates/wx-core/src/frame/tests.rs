//! Catalog tests.
//!
//! Every test but one builds its catalog over the small fixed registry below,
//! so the real registries can change freely. The one exception is a smoke
//! test that holds whatever rows the real registries have.

use super::catalog::State;
use super::*;
use crate::layer::InputSpec;
use crate::proj::{lat_to_y, lon_to_x};
use crate::source::Family;
use chrono::{Duration, TimeZone};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// The test registry
// ---------------------------------------------------------------------------

/// The catalog reads only a source's id, family, kind and rank, and a layer's
/// id, inputs (family, hold and `fill_past`), retention, tick and `anchored`.
/// Those are spelled out here; every other field is copied from the first
/// real row, so these rows keep compiling as the real registry changes.
fn source_row(id: &'static str, family: Family, kind: FrameKind, rank: u16) -> SourceDef {
    SourceDef {
        id,
        family,
        kind,
        rank,
        ..SOURCES[0]
    }
}

/// `inputs` are `(family, hold minutes, fill_past)`.
fn layer_row(
    id: &'static str,
    inputs: &[(Family, u32, bool)],
    retention_minutes: u32,
    observed_tick_minutes: u32,
    anchored: bool,
) -> LayerSpec {
    let inputs = inputs
        .iter()
        .map(|&(family, hold_minutes, fill_past)| InputSpec {
            family,
            hold_minutes,
            fill_past,
            ..LAYERS[0].inputs[0]
        })
        .collect();
    LayerSpec {
        id,
        inputs: leak(inputs),
        retention_minutes,
        observed_tick_minutes,
        anchored,
        ..LAYERS[0]
    }
}

struct Registry {
    layers: &'static [LayerSpec],
    sources: &'static [SourceDef],
}

/// Sources, top first: `obs` (radar-like observations), `ana` (an hourly
/// analysis), `nowcast` (runs every 5 minutes; it shares the observations'
/// family, so its input on a layer is theirs), `fine` (a regional model) and
/// `coarse` (a global model).
static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    use Family::{Gfs as Coarse, Hrrr as Fine, Mrms as Obs, Rtma as Ana};
    let sources = vec![
        source_row("obs", Obs, FrameKind::Observed, 900),
        source_row("ana", Ana, FrameKind::Observed, 800),
        source_row("nowcast", Obs, FrameKind::Forecast, 700),
        source_row("fine", Fine, FrameKind::Forecast, 600),
        source_row("coarse", Coarse, FrameKind::Forecast, 100),
    ];
    let layers = vec![
        // Two-minute observations; models only ahead of them.
        layer_row(
            "radar",
            &[(Obs, 6, false), (Fine, 15, false), (Coarse, 180, false)],
            120,
            2,
            false,
        ),
        // Hourly analyses; models fill the past.
        layer_row(
            "temp",
            &[(Ana, 60, false), (Fine, 60, true), (Coarse, 180, true)],
            720,
            60,
            false,
        ),
        // Models only.
        layer_row(
            "clouds",
            &[(Fine, 60, false), (Coarse, 180, false)],
            0,
            60,
            false,
        ),
        // Accumulations from one model, and from two.
        layer_row("snow", &[(Fine, 60, false)], 0, 60, true),
        layer_row(
            "snowmix",
            &[(Fine, 60, false), (Coarse, 180, false)],
            0,
            60,
            true,
        ),
        // A hold shorter than the rounding to a tick.
        layer_row("sparse", &[(Obs, 1, false)], 120, 10, false),
        // Two-minute observations over a slow, long-held source (a satellite
        // estimate every half hour).
        layer_row(
            "mosaic",
            &[(Obs, 6, false), (Ana, 150, false)],
            120,
            2,
            false,
        ),
    ];
    Registry {
        layers: leak(layers),
        sources: leak(sources),
    }
});

fn cat() -> State {
    State::with_registry(REGISTRY.layers, REGISTRY.sources)
}

fn catalog(hub: Hub) -> FrameCatalog {
    FrameCatalog::with_registry(hub, REGISTRY.layers, REGISTRY.sources)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// `h` hours and `m` minutes after 2026-11-06 00:00 UTC.
fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 6, 0, 0, 0).unwrap() + Duration::hours(h as i64) + mins(m as i64)
}

fn mins(m: i64) -> Duration {
    Duration::minutes(m)
}

fn secs(s: i64) -> Duration {
    Duration::seconds(s)
}

fn hm(t: DateTime<Utc>) -> String {
    t.format("%H:%M").to_string()
}

const CONUS: LonLatBox = LonLatBox::new(-130.0, 20.0, -60.0, 55.0);
const CONUS_SOLID: LonLatBox = LonLatBox::new(-120.0, 25.0, -70.0, 50.0);
/// Roughly Alaska: from 170E eastward to 130W.
const ALASKA: LonLatBox = LonLatBox::new(170.0, 50.0, -130.0, 72.0);

fn obs(h: u32, m: u32) -> Part {
    Part::observed("obs", at(h, m), CONUS, None, 7)
}

fn ana(h: u32) -> Part {
    Part::observed("ana", at(h, 0), CONUS, Some(CONUS_SOLID), 6)
}

fn nowcast(run: DateTime<Utc>, lead: u32) -> Part {
    Part::forecast("nowcast", run, lead, CONUS, None, 7)
}

fn fine(run_h: u32, lead: u32) -> Part {
    Part::forecast("fine", at(run_h, 0), lead, CONUS, Some(CONUS_SOLID), 6)
}

fn coarse(run_h: u32, lead: u32) -> Part {
    Part::forecast(
        "coarse",
        at(run_h, 0),
        lead,
        LonLatBox::WORLD,
        Some(LonLatBox::WORLD),
        4,
    )
}

fn with_bounds(mut p: Part, bounds: LonLatBox, solid: Option<LonLatBox>) -> Part {
    p.bounds = bounds;
    p.solid = solid;
    p
}

/// Insert a part that must be taken, returning the event.
fn put(s: &mut State, layer: &str, p: Part, now: DateTime<Utc>) -> Option<WsEvent> {
    let id = p.id.clone();
    let (r, e) = s.insert(layer, p, now).unwrap();
    assert!(matches!(r, Insert::Added { .. }), "{id} was refused");
    e
}

fn refused(s: &mut State, layer: &str, p: Part, now: DateTime<Utc>) -> bool {
    let (r, e) = s.insert(layer, p, now).unwrap();
    assert!(e.is_none(), "a refused part announced something");
    r == Insert::Refused
}

fn show_frame(f: &Frame) -> String {
    let kind = match f.kind {
        FrameKind::Observed => "obs",
        FrameKind::Forecast => "fc",
    };
    format!("{} {kind} {}", hm(f.valid), f.parts.join(" "))
}

/// The layer's frames as `"HH:MM obs|fc id id ..."`, after checking that the
/// timeline's parts are exactly the ones its frames name.
fn show(s: &State, layer: &str) -> Vec<String> {
    let tl = s.timeline(layer);
    let named: HashSet<&str> = tl
        .frames
        .iter()
        .flat_map(|f| f.parts.iter().map(String::as_str))
        .collect();
    let listed: HashSet<&str> = tl.parts.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        named, listed,
        "{layer}: timeline parts differ from the parts its frames name"
    );
    assert_eq!(
        listed.len(),
        tl.parts.len(),
        "{layer}: a part is listed twice"
    );
    assert!(
        tl.frames.windows(2).all(|w| w[0].valid < w[1].valid),
        "{layer}: frames out of order"
    );
    tl.frames.iter().map(show_frame).collect()
}

#[derive(Debug, PartialEq)]
struct Ev {
    layer: String,
    parts: Vec<String>,
    frames: Vec<String>,
    removed: Vec<String>,
}

fn evt(layer: &str, parts: &[&str], frames: &[&str], removed: &[&str]) -> Ev {
    let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
    Ev {
        layer: layer.to_string(),
        parts: own(parts),
        frames: own(frames),
        removed: own(removed),
    }
}

fn ev(e: &WsEvent) -> Ev {
    let WsEvent::Timeline {
        layer,
        parts,
        frames,
        removed,
    } = e
    else {
        panic!("not a timeline event: {e:?}");
    };
    Ev {
        layer: layer.clone(),
        parts: parts.iter().map(|p| p.id.clone()).collect(),
        frames: frames.iter().map(show_frame).collect(),
        removed: removed.iter().map(|t| hm(*t)).collect(),
    }
}

fn ids(parts: &[Part]) -> Vec<&str> {
    parts.iter().map(|p| p.id.as_str()).collect()
}

fn refs<'a>(parts: &[&'a Part]) -> Vec<&'a str> {
    parts.iter().map(|p| p.id.as_str()).collect()
}

fn tile_at(z: u8, lon: f64, lat: f64) -> TileCoord {
    let n = (1u32 << z) as f64;
    TileCoord::new(z, (lon_to_x(lon) * n) as u32, (lat_to_y(lat) * n) as u32).unwrap()
}

fn leak<T>(v: Vec<T>) -> &'static [T] {
    Box::leak(v.into_boxed_slice())
}

fn leak_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// The runs of the parts the layer's timeline shows.
fn shown_runs(s: &State, layer: &str) -> Vec<DateTime<Utc>> {
    let mut runs: Vec<DateTime<Utc>> = s
        .timeline(layer)
        .parts
        .iter()
        .filter_map(|p| p.run)
        .collect();
    runs.sort_unstable();
    runs.dedup();
    runs
}

fn fine_ids(run_h: u32, leads: impl IntoIterator<Item = u32>) -> Vec<String> {
    leads.into_iter().map(|l| fine(run_h, l).id).collect()
}

// ---------------------------------------------------------------------------
// Part ids
// ---------------------------------------------------------------------------

#[test]
fn forecast_ids_write_runs_off_the_hour_to_the_minute() {
    assert_eq!(Part::forecast_id("fine", at(11, 0), 30), "fine-26110611-30");
    assert_eq!(
        Part::forecast_id("nowcast", at(11, 5), 30),
        "nowcast-2611061105-30"
    );
    let p = nowcast(at(11, 5), 30);
    assert_eq!(
        (p.id.as_str(), p.valid, p.run),
        ("nowcast-2611061105-30", at(11, 35), Some(at(11, 5)))
    );
    assert!(Part::is_valid_id(&p.id));
    // Runs every five minutes for a day with two hours of leads, and an
    // observation every minute, all from one source: no two ids alike.
    let mut seen = HashSet::new();
    for run in (0..24 * 60).step_by(5) {
        for lead in (0..=120).step_by(5) {
            assert!(seen.insert(Part::forecast_id("x", at(0, run), lead)));
        }
    }
    for m in 0..24 * 60 {
        assert!(seen.insert(Part::observed_id("x", at(0, m))));
    }
}

#[test]
#[should_panic(expected = "whole minute")]
fn forecast_runs_are_whole_minutes() {
    nowcast(at(11, 5) + secs(30), 30);
}

// ---------------------------------------------------------------------------
// Rule 1
// ---------------------------------------------------------------------------

#[test]
fn newer_run_replaces_and_older_run_is_refused() {
    let mut s = cat();
    let now = at(12, 0);
    put(&mut s, "clouds", coarse(6, 360), now);
    let (r, e) = s.insert("clouds", coarse(12, 0), now).unwrap();
    assert_eq!(
        r,
        Insert::Added {
            replaced: Some(coarse(6, 360))
        }
    );
    assert_eq!(show(&s, "clouds"), ["12:00 fc coarse-26110612-0"]);
    assert_eq!(
        ev(&e.unwrap()),
        evt(
            "clouds",
            &["coarse-26110612-0"],
            &["12:00 fc coarse-26110612-0"],
            &[]
        )
    );
    // Older and same runs for that valid time.
    assert!(refused(&mut s, "clouds", coarse(6, 360), now));
    assert!(refused(&mut s, "clouds", coarse(0, 720), now));
    assert!(refused(&mut s, "clouds", coarse(12, 0), now));
    assert!(!s.wants("clouds", "coarse", at(12, 0), Some(at(6, 0)), now));
    assert!(!s.wants("clouds", "coarse", at(12, 0), Some(at(12, 0)), now));
    assert!(s.wants("clouds", "coarse", at(12, 0), Some(at(18, 0)), now));
    assert_eq!(
        s.take_dropped(),
        [Dropped {
            layer: "clouds",
            part: coarse(6, 360),
            at: now
        }]
    );
    assert_eq!(s.parts(), [("clouds", coarse(12, 0))]);
}

#[test]
fn the_same_observation_twice_is_refused() {
    let mut s = cat();
    put(&mut s, "radar", obs(12, 0), at(12, 1));
    assert!(!s.wants("radar", "obs", at(12, 0), None, at(12, 1)));
    assert!(refused(&mut s, "radar", obs(12, 0), at(12, 1)));
    assert!(s.take_dropped().is_empty());
}

#[test]
fn an_id_already_taken_is_refused() {
    let mut s = cat();
    put(&mut s, "clouds", coarse(12, 0), at(12, 0));
    // Same id, different valid time: a broken feed, not a new part.
    let mut odd = coarse(12, 60);
    odd.id = coarse(12, 0).id;
    assert!(refused(&mut s, "clouds", odd, at(12, 0)));
}

#[test]
fn observations_dated_ahead_are_refused() {
    let mut s = cat();
    let now = at(12, 0);
    assert!(s.wants("radar", "obs", at(12, 5), None, now));
    put(&mut s, "radar", obs(12, 5), now);
    assert!(!s.wants("radar", "obs", at(12, 6), None, now));
    assert!(refused(&mut s, "radar", obs(12, 6), now));
    assert!(refused(&mut s, "temp", ana(13), now));
    // Forecasts are dated ahead by nature.
    put(&mut s, "radar", fine(12, 600), now);
    // A minute later the same observation is fine.
    put(&mut s, "radar", obs(12, 6), now + mins(1));
}

#[test]
fn nowcast_runs_replace_each_other() {
    let mut s = cat();
    let mut next_obs = 0;
    for k in 0..6u32 {
        let run = at(12, 5 * k);
        let now = run + mins(2);
        // Observations every two minutes, up to now.
        while next_obs <= 5 * k + 2 {
            put(&mut s, "radar", obs(12, next_obs), now);
            next_obs += 2;
        }
        for lead in (5..=120).step_by(5) {
            let (r, _) = s.insert("radar", nowcast(run, lead), now).unwrap();
            // Every lead but the last replaces the previous run's part for
            // the same valid time.
            let replaced = (k > 0 && lead < 120).then(|| nowcast(run - mins(5), lead + 5));
            assert_eq!(r, Insert::Added { replaced }, "run {k} lead {lead}");
        }
        s.sweep(now);

        // Ahead of the observations, only the newest run shows.
        let tl = s.timeline("radar");
        let runs: HashMap<&str, Option<DateTime<Utc>>> =
            tl.parts.iter().map(|p| (p.id.as_str(), p.run)).collect();
        let ahead: Vec<&Frame> = tl
            .frames
            .iter()
            .filter(|f| f.kind == FrameKind::Forecast)
            .collect();
        assert_eq!(ahead.len(), 24);
        assert!(
            ahead
                .iter()
                .all(|f| f.parts.iter().all(|id| runs[id.as_str()] == Some(run)))
        );

        // The previous run's lead 5 is not replaced, and goes once the
        // observations pass it; the one before that has gone.
        let held = s
            .parts()
            .into_iter()
            .filter(|(_, p)| p.source == "nowcast")
            .count();
        assert_eq!(held, 24 + (k > 0) as usize, "run {k}");
        let gone: Vec<String> = s.take_dropped().into_iter().map(|d| d.part.id).collect();
        assert_eq!(
            gone.len(),
            if k == 0 { 0 } else { 23 + (k > 1) as usize },
            "run {k}"
        );
        if k > 1 {
            assert!(gone.contains(&nowcast(run - mins(10), 5).id));
        }
    }
    assert_eq!(nowcast(at(12, 0), 5).id, "nowcast-26110612-5");
    assert_eq!(nowcast(at(12, 5), 5).id, "nowcast-2611061205-5");
    // Anchored layers take none of this.
    assert!(!s.wants("snow", "fine", at(13, 5), Some(at(12, 5)), at(12, 30)));
}

// ---------------------------------------------------------------------------
// Rules 2 to 6
// ---------------------------------------------------------------------------

#[test]
fn observed_parts_tick_on_the_next_multiple() {
    let mut s = cat();
    let now = at(12, 10);
    let e = put(&mut s, "radar", obs(12, 5), now).unwrap();
    assert_eq!(ev(&e).frames, ["12:06 obs obs-2611061205"]);
    // A part at 12:06 lands on the same tick and, being newer, takes it.
    put(&mut s, "radar", obs(12, 6), now);
    assert_eq!(show(&s, "radar"), ["12:06 obs obs-2611061206"]);
    put(&mut s, "radar", obs(12, 7), now);
    assert_eq!(
        show(&s, "radar"),
        ["12:06 obs obs-2611061206", "12:08 obs obs-2611061207"]
    );
    // Hourly ticks on an hourly layer.
    put(
        &mut s,
        "temp",
        Part::observed("ana", at(11, 30), CONUS, None, 6),
        now,
    );
    assert_eq!(show(&s, "temp"), ["12:00 obs ana-2611061130"]);
}

#[test]
fn forecast_parts_tick_at_their_valid_time() {
    let mut s = cat();
    put(&mut s, "clouds", fine(12, 75), at(12, 0));
    assert_eq!(show(&s, "clouds"), ["13:15 fc fine-26110612-75"]);
}

#[test]
fn fill_past_decides_who_appears_up_to_latest_observed() {
    // Models that do not fill the past.
    let mut s = cat();
    let now = at(12, 20);
    put(&mut s, "radar", obs(12, 0), now);
    put(&mut s, "radar", fine(11, 60), now);
    put(&mut s, "radar", fine(11, 75), now);
    put(&mut s, "radar", coarse(6, 360), now);
    put(&mut s, "radar", coarse(6, 420), now);
    assert_eq!(
        show(&s, "radar"),
        [
            // The models' 12:00 are at the latest observation: they make no
            // time step and join none there.
            "12:00 obs obs-2611061200",
            // After it, the coarse 12:00 is held into the first forecast step.
            "12:15 fc fine-26110611-75 coarse-26110606-360",
            "13:00 fc coarse-26110606-420",
        ]
    );
    assert_eq!(s.parts().len(), 5, "the hidden fine part is still held");

    // Models that fill the past, stacked by rank.
    let mut s = cat();
    let now = at(12, 30);
    put(&mut s, "temp", ana(12), now);
    for lead in [0, 60, 120] {
        put(&mut s, "temp", fine(11, lead), now);
    }
    for lead in [300, 360, 420] {
        put(&mut s, "temp", coarse(6, lead), now);
    }
    assert_eq!(
        show(&s, "temp"),
        [
            "11:00 obs fine-26110611-0 coarse-26110606-300",
            "12:00 obs ana-2611061200 fine-26110611-60 coarse-26110606-360",
            "13:00 fc fine-26110611-120 coarse-26110606-420",
        ]
    );
}

#[test]
fn observations_stop_at_the_latest_observation() {
    // Within its hold, but after the latest observation: left out.
    let mut s = cat();
    put(&mut s, "radar", obs(12, 10), at(12, 12));
    put(&mut s, "radar", fine(11, 75), at(12, 12));
    assert_eq!(
        show(&s, "radar"),
        ["12:10 obs obs-2611061210", "12:15 fc fine-26110611-75"]
    );

    // Holds still apply on each side.
    let mut s = cat();
    let now = at(12, 40);
    put(&mut s, "temp", ana(10), now);
    put(&mut s, "temp", ana(12), now);
    put(&mut s, "temp", fine(10, 30), now);
    put(&mut s, "temp", fine(12, 30), now);
    assert_eq!(
        show(&s, "temp"),
        [
            "10:00 obs ana-2611061000",
            "10:30 obs ana-2611061000 fine-26110610-30",
            "12:00 obs ana-2611061200",
            "12:30 fc fine-26110612-30",
        ]
    );
}

#[test]
fn parts_are_held_until_their_hold_runs_out() {
    let mut s = cat();
    let now = at(12, 0);
    put(&mut s, "clouds", coarse(12, 0), now);
    for lead in [0, 60, 120, 180] {
        put(&mut s, "clouds", fine(12, lead), now);
    }
    assert_eq!(
        show(&s, "clouds"),
        [
            "12:00 fc fine-26110612-0 coarse-26110612-0",
            "13:00 fc fine-26110612-60 coarse-26110612-0",
            "14:00 fc fine-26110612-120 coarse-26110612-0",
            // 180 minutes after the coarse part: its hold has run out.
            "15:00 fc fine-26110612-180",
        ]
    );
    put(&mut s, "clouds", coarse(12, 195), now);
    put(&mut s, "clouds", coarse(12, 300), now);
    assert_eq!(
        show(&s, "clouds")[3..],
        [
            "15:00 fc fine-26110612-180",
            "15:15 fc fine-26110612-180 coarse-26110612-195",
            "17:00 fc coarse-26110612-300",
        ]
    );
}

#[test]
fn empty_frames_are_not_time_steps() {
    let mut s = cat();
    // 12:05 rounds up to 12:10, five minutes past its one-minute hold.
    let (r, e) = s.insert("sparse", obs(12, 5), at(12, 6)).unwrap();
    assert!(matches!(r, Insert::Added { .. }) && e.is_none());
    assert!(show(&s, "sparse").is_empty());
    put(&mut s, "sparse", obs(12, 10), at(12, 11));
    assert_eq!(show(&s, "sparse"), ["12:10 obs obs-2611061210"]);
}

// ---------------------------------------------------------------------------
// Rule 7
// ---------------------------------------------------------------------------

#[test]
fn observed_parts_expire_once_their_hold_has_left_the_window() {
    let mut s = cat();
    put(&mut s, "radar", obs(10, 0), at(11, 0));
    put(&mut s, "radar", obs(11, 0), at(11, 0));
    assert!(s.sweep(at(12, 0)).is_empty());
    // Its time step leaves the window first (rule 4)...
    let events = s.sweep(at(12, 1));
    assert_eq!(events.len(), 1);
    assert_eq!(ev(&events[0]), evt("radar", &[], &[], &["10:00"]));
    assert_eq!(show(&s, "radar"), ["11:00 obs obs-2611061100"]);
    assert!(s.take_dropped().is_empty());
    // ...and the part itself once its six-minute hold has too.
    assert!(s.sweep(at(12, 5)).is_empty());
    assert!(s.take_dropped().is_empty());
    assert!(s.sweep(at(12, 6)).is_empty());
    assert_eq!(
        s.take_dropped(),
        [Dropped {
            layer: "radar",
            part: obs(10, 0),
            at: at(12, 6)
        }]
    );
    assert!(!s.wants("radar", "obs", at(10, 0), None, at(12, 6)));
    assert!(refused(&mut s, "radar", obs(10, 0), at(12, 6)));
    assert!(s.wants("radar", "obs", at(10, 1), None, at(12, 6)));
}

#[test]
fn a_long_held_source_stays_to_the_edge_of_the_window() {
    let mut s = cat();
    // The slow source stops after 08:00; two-minute observations run from
    // 09:00 to 12:00. Each arrives on time.
    let slow = |h, m| Part::observed("ana", at(h, m), LonLatBox::WORLD, None, 6);
    for (h, m) in [(7, 30), (8, 0)] {
        put(&mut s, "mosaic", slow(h, m), at(h, m));
    }
    for m in (0..=180).step_by(2) {
        put(&mut s, "mosaic", obs(9, m), at(9, m));
    }
    s.sweep(at(12, 0));
    let frames = show(&s, "mosaic");
    // The oldest time step is the window's start, and the slow part from two
    // hours before it still stands in there, until its hold runs out.
    assert_eq!(frames.len(), 61);
    assert_eq!(frames[0], "10:00 obs obs-2611061000 ana-2611060800");
    assert_eq!(frames[14], "10:28 obs obs-2611061028 ana-2611060800");
    assert_eq!(frames[15], "10:30 obs obs-2611061030");
    // The 07:30 part's hold ended at the window's start; the 08:00 one's has
    // not. Observations whose hold still reaches into the window are kept,
    // but make no frames before it.
    let gone: Vec<String> = s.take_dropped().into_iter().map(|d| d.part.id).collect();
    assert_eq!(gone.len(), 28 + 1);
    assert!(gone.contains(&"ana-2611060730".to_string()));
    assert!(s.parts().iter().any(|(_, p)| p.id == "obs-2611060958"));
    // The window moves on; the slow part goes once 08:00 + 150 minutes is at
    // its start.
    s.sweep(at(12, 29));
    assert_eq!(show(&s, "mosaic")[0], "10:30 obs obs-2611061030");
    assert!(s.take_dropped().iter().all(|d| d.part.source == "obs"));
    s.sweep(at(12, 30));
    assert!(
        s.take_dropped()
            .iter()
            .any(|d| d.part.id == "ana-2611060800")
    );
    // A late caller's earlier `now` does not move the window back.
    put(&mut s, "mosaic", obs(12, 2), at(11, 57));
    assert_eq!(show(&s, "mosaic")[0], "10:30 obs obs-2611061030");
}

#[test]
fn fill_past_forecasts_expire_after_the_retention_window() {
    let mut s = cat();
    put(&mut s, "temp", fine(0, 0), at(1, 0));
    put(&mut s, "temp", coarse(0, 0), at(1, 0));
    put(&mut s, "temp", fine(0, 60), at(1, 0));
    assert_eq!(
        show(&s, "temp"),
        [
            "00:00 fc fine-26110600-0 coarse-26110600-0",
            "01:00 fc fine-26110600-60 coarse-26110600-0",
        ]
    );
    // Twelve hours of retention, well past the forecast grace. Each time step
    // leaves with the window (rule 4), each part once its hold has too.
    let dropped = |s: &mut State| {
        s.take_dropped()
            .into_iter()
            .map(|d| d.part)
            .collect::<Vec<_>>()
    };
    assert!(s.sweep(at(12, 0)).is_empty());
    assert_eq!(
        ev(&s.sweep(at(12, 1))[0]),
        evt("temp", &[], &[], &["00:00"])
    );
    assert!(dropped(&mut s).is_empty());
    assert!(s.sweep(at(13, 0)).is_empty());
    assert_eq!(dropped(&mut s), [fine(0, 0)]);
    assert_eq!(
        ev(&s.sweep(at(14, 0))[0]),
        evt("temp", &[], &[], &["01:00"])
    );
    assert_eq!(dropped(&mut s), [fine(0, 60)]);
    assert!(s.sweep(at(15, 0)).is_empty());
    assert_eq!(dropped(&mut s), [coarse(0, 0)]);
    assert!(refused(&mut s, "temp", coarse(0, 0), at(15, 0)));
}

#[test]
fn layers_that_fill_the_past_stop_at_the_window_without_observations() {
    // Hourly runs of a model filling the past, no analysis at all.
    let mut s = cat();
    for h in 7..=22 {
        put(&mut s, "temp", coarse(h, 0), at(h, 0));
    }
    s.sweep(at(22, 0));
    let frames = show(&s, "temp");
    assert_eq!(frames.len(), 13);
    assert_eq!(frames[0], "10:00 fc coarse-26110610-0");
    // 08:00 and 09:00 are kept for their three-hour hold, off the timeline.
    assert!(s.parts().iter().any(|(_, p)| p.id == "coarse-26110608-0"));

    // A layer with neither observations nor models filling the past keeps
    // its past for the forecast grace, whatever its window.
    let mut s = cat();
    put(&mut s, "clouds", coarse(12, 0), at(12, 0));
    s.sweep(at(13, 0));
    assert_eq!(show(&s, "clouds"), ["12:00 fc coarse-26110612-0"]);
}

#[test]
fn wants_matches_expiry_at_the_window_edge() {
    // A six-minute hold and a two-hour window: 09:54 can no longer appear at
    // noon, 09:55 can, and is kept.
    let mut s = cat();
    let now = at(12, 0);
    assert!(!s.wants("radar", "obs", at(9, 54), None, now));
    assert!(refused(&mut s, "radar", obs(9, 54), now));
    assert!(s.wants("radar", "obs", at(9, 55), None, now));
    put(&mut s, "radar", obs(9, 55), now);
    s.sweep(now);
    assert!(s.take_dropped().is_empty());

    // A model filling the past, with a three-hour hold and a twelve-hour
    // window.
    let mut s = cat();
    let now = at(15, 0);
    assert!(!s.wants("temp", "coarse", at(0, 0), Some(at(0, 0)), now));
    assert!(refused(&mut s, "temp", coarse(0, 0), now));
    assert!(s.wants("temp", "coarse", at(0, 1), Some(at(0, 0)), now));
    put(&mut s, "temp", coarse(0, 1), now);
    s.sweep(now);
    assert!(s.take_dropped().is_empty());

    // A caller with a slower clock gets the answer the next sweep would
    // give, not one that costs a download the sweep then drops.
    let mut s = cat();
    s.sweep(at(12, 10));
    assert!(!s.wants("radar", "obs", at(10, 3), None, at(12, 0)));
    assert!(refused(&mut s, "radar", obs(10, 3), at(12, 0)));
    assert!(s.wants("radar", "obs", at(10, 5), None, at(12, 0)));
}

#[test]
fn other_forecasts_expire_after_the_grace() {
    let mut s = cat();
    put(&mut s, "clouds", coarse(12, 0), at(12, 0));
    put(&mut s, "clouds", coarse(12, 60), at(12, 0));
    assert!(s.sweep(at(13, 30)).is_empty());
    let events = s.sweep(at(13, 31));
    assert_eq!(ev(&events[0]).removed, ["12:00"]);
    assert_eq!(show(&s, "clouds"), ["13:00 fc coarse-26110612-60"]);
    assert_eq!(s.take_dropped()[0].part, coarse(12, 0));
    assert!(!s.wants("clouds", "coarse", at(12, 0), Some(at(18, 0)), at(13, 31)));
}

#[test]
fn forecasts_that_can_no_longer_appear_expire() {
    let mut s = cat();
    let now = at(12, 15);
    put(&mut s, "radar", fine(11, 60), now);
    put(&mut s, "radar", fine(11, 75), now);
    put(&mut s, "radar", coarse(6, 360), now);
    put(&mut s, "radar", obs(12, 14), now);
    // The fine 12:00 could still appear at a step before 12:15.
    assert!(s.sweep(at(12, 17)).is_empty());
    assert!(s.take_dropped().is_empty());
    // An observation that ticks at 12:16 ends that.
    put(&mut s, "radar", obs(12, 15), at(12, 17));
    let before = show(&s, "radar");
    assert!(s.sweep(at(12, 17)).is_empty(), "nothing visible went");
    assert_eq!(show(&s, "radar"), before);
    let gone: Vec<String> = s.take_dropped().into_iter().map(|d| d.part.id).collect();
    assert_eq!(gone, ["fine-26110611-60"]);
    // Anything else ending by 12:16 is refused up front; the coarse model's
    // long hold keeps its 12:00 wanted.
    assert!(!s.wants("radar", "fine", at(11, 45), Some(at(11, 0)), at(12, 17)));
    assert!(refused(&mut s, "radar", fine(11, 45), at(12, 17)));
    assert!(s.wants("radar", "coarse", at(12, 0), Some(at(12, 0)), at(12, 17)));
}

// ---------------------------------------------------------------------------
// Anchored layers
// ---------------------------------------------------------------------------

#[test]
fn anchored_life_cycle() {
    let mut s = cat();
    let t0 = at(1, 0);
    // Too few parts for an anchor: nothing to show, nothing to announce.
    for lead in (60..=300).step_by(60) {
        assert!(s.wants("snow", "fine", at(0, lead), Some(at(0, 0)), t0));
        assert!(put(&mut s, "snow", fine(0, lead), t0).is_none());
    }
    assert!(show(&s, "snow").is_empty());
    assert_eq!(s.parts().len(), 5);

    // The sixth makes the run the anchor.
    let e = ev(&put(&mut s, "snow", fine(0, 360), t0).unwrap());
    assert_eq!(e.parts, fine_ids(0, (60..=360).step_by(60)));
    assert_eq!(e.frames.len(), 6);
    assert_eq!(e.frames[0], "01:00 fc fine-26110600-60");
    assert!(e.removed.is_empty());
    for lead in (420..=1080).step_by(60) {
        put(&mut s, "snow", fine(0, lead), t0);
    }
    assert_eq!(show(&s, "snow").len(), 18);

    // The next run waits off the timeline. Its furthest part alone does not
    // move the anchor: every valid time the anchor has from 13:00 to 18:00
    // must be covered.
    let t1 = at(13, 0);
    let before = show(&s, "snow");
    assert!(put(&mut s, "snow", fine(12, 360), t1).is_none());
    for lead in (60..=240).step_by(60) {
        assert!(put(&mut s, "snow", fine(12, lead), t1).is_none());
    }
    assert_eq!(show(&s, "snow"), before);
    assert_eq!(s.parts().len(), 23);

    // The last one missing moves it.
    let e = ev(&put(&mut s, "snow", fine(12, 300), t1).unwrap());
    assert_eq!(e.parts, fine_ids(12, (60..=360).step_by(60)));
    assert_eq!(
        e.frames,
        [
            "13:00 fc fine-26110612-60",
            "14:00 fc fine-26110612-120",
            "15:00 fc fine-26110612-180",
            "16:00 fc fine-26110612-240",
            "17:00 fc fine-26110612-300",
            "18:00 fc fine-26110612-360",
        ]
    );
    let removed: Vec<String> = (1..=12).map(|h| hm(at(h, 0))).collect();
    assert_eq!(e.removed, removed);

    // The old run is dropped, refused and no longer wanted.
    let gone = s.take_dropped();
    assert_eq!(gone.len(), 18);
    assert!(
        gone.iter()
            .all(|d| d.layer == "snow" && d.at == t1 && d.part.run == Some(at(0, 0)))
    );
    assert!(!s.wants("snow", "fine", at(19, 0), Some(at(0, 0)), t1));
    assert!(refused(&mut s, "snow", fine(0, 1140), t1));
    assert!(s.wants("snow", "fine", at(25, 0), Some(at(24, 0)), t1));
    assert!(!s.wants("snow", "fine", at(25, 0), None, t1));
}

#[test]
fn anchored_layers_take_only_runs_on_the_cycle() {
    let mut s = cat();
    let now = at(13, 0);
    for run in [1, 6, 13, 18] {
        assert!(
            !s.wants("snow", "fine", at(run + 1, 0), Some(at(run, 0)), now),
            "{run}Z"
        );
        assert!(refused(&mut s, "snow", fine(run, 60), now), "{run}Z");
    }
    assert!(s.wants("snow", "fine", at(13, 0), Some(at(12, 0)), now));
    put(&mut s, "snow", fine(12, 60), now);
    // Other layers take any run.
    put(&mut s, "clouds", fine(13, 60), now);
}

#[test]
fn reach_needs_every_valid_time_the_anchor_has() {
    let mut s = cat();
    // The anchor has no part at 15:00.
    for lead in [780, 840, 960, 1020, 1080, 1140] {
        put(&mut s, "snow", fine(0, lead), at(12, 0));
    }
    assert_eq!(show(&s, "snow").len(), 6);
    // Nor does the next run; it needs 13:00, 14:00 and 16:00 to 19:00.
    for lead in [60, 120, 240, 300, 360] {
        assert!(put(&mut s, "snow", fine(12, lead), at(13, 0)).is_none());
    }
    let e = ev(&put(&mut s, "snow", fine(12, 420), at(13, 0)).unwrap());
    assert_eq!(e.parts, fine_ids(12, [60, 120, 240, 300, 360, 420]));
    assert!(e.removed.is_empty());

    // However far the anchor reaches: three days, every three hours.
    let mut s = cat();
    for k in 0..=20 {
        put(&mut s, "snow", fine(0, 780 + 180 * k), at(12, 0));
    }
    for k in 0..20 {
        assert!(
            put(&mut s, "snow", fine(12, 60 + 180 * k), at(13, 0)).is_none(),
            "{k}"
        );
    }
    // The last valid time, 61 hours into the run.
    assert!(put(&mut s, "snow", fine(12, 60 + 180 * 20), at(13, 0)).is_some());
    assert_eq!(shown_runs(&s, "snow"), [at(12, 0)]);
}

#[test]
fn anchor_advances_by_patience() {
    let anchor_far_ahead = |s: &mut State| {
        // 16:00 to 00:00 tomorrow.
        for lead in (960..=1440).step_by(60) {
            put(s, "snow", fine(0, lead), at(1, 0));
        }
    };

    let mut s = cat();
    anchor_far_ahead(&mut s);
    // Six parts that will never cover 22:00 to 00:00, the first at 13:10.
    for (k, lead) in (240..=540).step_by(60).enumerate() {
        assert!(put(&mut s, "snow", fine(12, lead), at(13, 10) + mins(k as i64)).is_none());
    }
    assert!(s.sweep(at(16, 9)).is_empty());
    let events = s.sweep(at(16, 10));
    assert_eq!(events.len(), 1);
    let e = ev(&events[0]);
    assert_eq!(e.frames.len(), 6);
    assert_eq!(e.removed, ["22:00", "23:00", "00:00"]);
    assert_eq!(show(&s, "snow")[0], "16:00 fc fine-26110612-240");
    assert_eq!(s.take_dropped().len(), 9);

    // Patience needs MIN_ANCHOR_PARTS too.
    let mut s = cat();
    anchor_far_ahead(&mut s);
    for lead in (240..=480).step_by(60) {
        put(&mut s, "snow", fine(12, lead), at(13, 10));
    }
    assert!(s.sweep(at(16, 30)).is_empty());
    assert_eq!(show(&s, "snow")[0], "16:00 fc fine-26110600-960");
    // The sixth, long after the first, moves it at once.
    let e = ev(&put(&mut s, "snow", fine(12, 540), at(16, 31)).unwrap());
    assert_eq!(e.frames.len(), 6);
    assert_eq!(e.removed.len(), 3);
}

#[test]
fn first_anchor_is_the_newest_run_with_enough_parts() {
    let mut s = cat();
    let now = at(13, 0);
    for k in 0..5 {
        assert!(put(&mut s, "snow", fine(0, 780 + 60 * k), now).is_none());
        assert!(put(&mut s, "snow", fine(12, 60 + 60 * k), now).is_none());
    }
    let e = ev(&put(&mut s, "snow", fine(12, 360), now).unwrap());
    assert_eq!(e.parts, fine_ids(12, (60..=360).step_by(60)));
    let gone = s.take_dropped();
    assert_eq!(gone.len(), 5);
    assert!(gone.iter().all(|d| d.part.run == Some(at(0, 0))));
    assert!(refused(&mut s, "snow", fine(0, 1080), now));
}

#[test]
fn the_anchor_never_goes_away_or_back() {
    let mut s = cat();
    for lead in (60..=360).step_by(60) {
        put(&mut s, "snow", fine(0, lead), at(1, 0));
    }
    put(&mut s, "snow", fine(12, 60), at(2, 0));
    put(&mut s, "snow", fine(12, 120), at(2, 0));
    // Every part of the anchor expires; the anchor stays.
    assert_eq!(ev(&s.sweep(at(8, 0))[0]).removed.len(), 6);
    assert!(show(&s, "snow").is_empty());
    put(&mut s, "snow", fine(0, 420), at(8, 0));
    assert_eq!(show(&s, "snow"), ["07:00 fc fine-26110600-420"]);
    let older = Part::forecast("fine", at(0, 0) - Duration::hours(12), 1200, CONUS, None, 6);
    assert!(refused(&mut s, "snow", older, at(8, 0)));
    // Nothing to compare the next run against: only patience moves it, and
    // only once it has enough parts.
    s.sweep(at(9, 0));
    assert!(show(&s, "snow").is_empty());
    for lead in (180..=300).step_by(60) {
        assert!(put(&mut s, "snow", fine(12, lead), at(9, 0)).is_none());
    }
    assert!(put(&mut s, "snow", fine(12, 360), at(9, 0)).is_some());
    assert_eq!(show(&s, "snow")[0], "13:00 fc fine-26110612-60");
}

/// On a layer with two models the coarse one, lowest in rank, is the base
/// as soon as it holds anything; until then the fine one stands in, once it
/// has given the coarse one its patience.
#[test]
fn the_registry_base_takes_over_from_a_stand_in() {
    let mut s = cat();
    let patience = mins(ANCHOR_PATIENCE_MINUTES);
    let t0 = at(12, 0);
    for lead in (600..=900).step_by(60) {
        assert!(put(&mut s, "snowmix", fine(12, lead), t0).is_none());
    }
    assert!(s.sweep(t0 + patience - mins(1)).is_empty());
    let t1 = t0 + patience;
    assert_eq!(s.sweep(t1).len(), 1);
    assert_eq!(show(&s, "snowmix").len(), 6, "the stand-in set an anchor");
    // Runs older than the anchor stay refused, whoever set it.
    assert!(refused(&mut s, "snowmix", coarse(0, 1320), t1));
    // The base joins the anchor's run; the anchor stays.
    put(&mut s, "snowmix", coarse(12, 600), t1);
    assert_eq!(
        show(&s, "snowmix"),
        [
            "22:00 fc fine-26110612-600 coarse-26110612-600",
            "23:00 fc fine-26110612-660 coarse-26110612-600",
            "00:00 fc fine-26110612-720 coarse-26110612-600",
            "01:00 fc fine-26110612-780",
            "02:00 fc fine-26110612-840",
            "03:00 fc fine-26110612-900",
        ]
    );
    // The fine model's next run cannot move it, however long it waits.
    for lead in (60..=360).step_by(60) {
        assert!(put(&mut s, "snowmix", fine(24, lead), t1 + mins(10)).is_none());
    }
    assert!(s.sweep(t1 + mins(40)).is_empty());
    // The base's next run has nothing to cover (the anchor's base parts end
    // before it starts), so it takes over by patience.
    let t2 = t1 + mins(40);
    for lead in (60..=360).step_by(60) {
        assert!(put(&mut s, "snowmix", coarse(24, lead), t2).is_none());
    }
    assert!(s.sweep(t2 + patience - mins(1)).is_empty());
    let e = ev(&s.sweep(t2 + patience)[0]);
    assert_eq!(e.frames.len(), 6);
    assert_eq!(e.frames[0], "01:00 fc fine-26110700-60 coarse-26110700-60");
    assert_eq!(e.removed.len(), 3);
    assert_eq!(s.take_dropped().len(), 7);
}

/// On a cold start a fast stand-in must not set the first anchor before the
/// registry base has had its patience, or the base's newest run (a cycle
/// older) would be refused and most of the world left without the layer.
#[test]
fn a_stand_in_waits_for_the_registry_base_on_a_cold_start() {
    let patience = mins(ANCHOR_PATIENCE_MINUTES);
    let t0 = at(13, 30);
    let stand_in = |s: &mut State| {
        for lead in (240..=540).step_by(60) {
            assert!(put(s, "snowmix", fine(12, lead), t0).is_none());
        }
        assert!(show(s, "snowmix").is_empty());
    };

    // The base's previous run is still wanted, and sets the anchor when it
    // arrives; the stand-in's newer run waits as any newer run does.
    let mut s = cat();
    stand_in(&mut s);
    assert!(s.wants("snowmix", "coarse", at(15, 0), Some(at(0, 0)), t0));
    for lead in (900..=1200).step_by(60) {
        put(&mut s, "snowmix", coarse(0, lead), at(14, 0));
    }
    assert_eq!(shown_runs(&s, "snowmix"), [at(0, 0)]);
    assert_eq!(s.parts().len(), 12);

    // When the base sends nothing, the stand-in sets it after the wait.
    let mut s = cat();
    stand_in(&mut s);
    assert!(s.sweep(t0 + patience - mins(1)).is_empty());
    assert_eq!(s.sweep(t0 + patience).len(), 1);
    assert_eq!(shown_runs(&s, "snowmix"), [at(12, 0)]);

    // A restore starts the wait again, from the first sweep after it.
    let mut s = cat();
    s.restore(
        (240..=540)
            .step_by(60)
            .map(|lead| ("snowmix".to_string(), fine(12, lead))),
    );
    assert!(show(&s, "snowmix").is_empty());
    let t = at(14, 0);
    assert!(s.sweep(t).is_empty());
    assert!(s.sweep(t + patience - mins(1)).is_empty());
    assert_eq!(s.sweep(t + patience).len(), 1);
}

#[test]
fn only_enabled_sources_count_for_the_anchor() {
    // Only the fine model runs: it is the base, and sets the first anchor at
    // once.
    let c = catalog(Hub::new());
    c.set_enabled_sources(&["fine"]);
    for lead in (240..=540).step_by(60) {
        c.insert("snowmix", fine(12, lead), at(13, 30)).unwrap();
    }
    assert_eq!(c.timeline("snowmix").frames.len(), 6);

    // Both run: the coarse model is waited for, as on any cold start.
    let mut s = cat();
    s.set_enabled_sources(&["fine", "coarse"]);
    for lead in (240..=540).step_by(60) {
        assert!(put(&mut s, "snowmix", fine(12, lead), at(13, 30)).is_none());
    }
    assert!(show(&s, "snowmix").is_empty());

    // A part left by the disabled coarse model restores and shows, but is
    // neither the base nor waited for when the anchor moves.
    let mut s = cat();
    s.set_enabled_sources(&["fine"]);
    let left = std::iter::once(coarse(12, 600));
    s.restore(
        left.chain((600..=900).step_by(60).map(|l| fine(12, l)))
            .map(|p| ("snowmix".to_string(), p)),
    );
    assert_eq!(
        show(&s, "snowmix")[0],
        "22:00 fc fine-26110612-600 coarse-26110612-600"
    );
    for lead in [60, 120] {
        assert!(put(&mut s, "snowmix", fine(24, lead), at(13, 0)).is_none());
    }
    assert!(put(&mut s, "snowmix", fine(24, 180), at(13, 0)).is_some());
    assert_eq!(shown_runs(&s, "snowmix"), [at(24, 0)]);
}

/// When the base's next run covers the anchor before a finer model's next
/// run is out, the anchor waits for it rather than dropping it.
#[test]
fn the_anchor_waits_for_every_model_on_it() {
    let patience = mins(ANCHOR_PATIENCE_MINUTES);
    let base_next = at(25, 0);
    let setup = || {
        let mut s = cat();
        // Both models' 12Z runs, a day and a half ahead.
        for lead in (60..=2160).step_by(60) {
            put(&mut s, "snowmix", coarse(12, lead), at(13, 0));
            put(&mut s, "snowmix", fine(12, lead), at(13, 0));
        }
        // The base's 24Z covers its 12Z, but the fine model has none of it.
        for lead in (60..=1440).step_by(60) {
            assert!(
                put(&mut s, "snowmix", coarse(24, lead), base_next).is_none(),
                "{lead}"
            );
        }
        assert_eq!(shown_runs(&s, "snowmix"), [at(12, 0)]);
        s
    };

    // The fine model's first part of 24Z lets it move.
    let mut s = setup();
    assert!(put(&mut s, "snowmix", fine(24, 360), at(25, 30)).is_some());
    assert_eq!(shown_runs(&s, "snowmix"), [at(24, 0)]);
    assert!(
        show(&s, "snowmix").contains(&"06:00 fc fine-26110700-360 coarse-26110700-360".to_string())
    );

    // Without it, patience moves it, counted from the base's first part.
    let mut s = setup();
    s.sweep(base_next + patience - mins(1));
    assert_eq!(shown_runs(&s, "snowmix"), [at(12, 0)]);
    s.sweep(base_next + patience);
    assert_eq!(shown_runs(&s, "snowmix"), [at(24, 0)]);
}

/// A base that stops producing runs hands over to the stand-in once it is
/// more than two cycles behind, and takes over again when it comes back.
#[test]
fn a_stalled_base_hands_over_and_takes_back() {
    let mut s = cat();
    let day = || (60..=1440).step_by(60);
    // One run from both models.
    for lead in day() {
        put(&mut s, "snowmix", coarse(0, lead), at(1, 0));
        put(&mut s, "snowmix", fine(0, lead), at(1, 0));
    }
    assert_eq!(shown_runs(&s, "snowmix"), [at(0, 0)]);
    // The coarse model stops. One and two cycles on it is still the base, so
    // the fine model's runs wait.
    for run in [12, 24] {
        for lead in day() {
            assert!(put(&mut s, "snowmix", fine(run, lead), at(run + 1, 0)).is_none());
        }
        assert_eq!(shown_runs(&s, "snowmix"), [at(0, 0)], "{run}Z");
    }
    // Three cycles behind, it is not: the fine model stands in. Its 24Z has
    // waited long enough to take over at once; its 36Z follows once it
    // covers 24Z's valid times after 36Z.
    assert!(put(&mut s, "snowmix", fine(36, 60), at(37, 0)).is_some());
    assert_eq!(shown_runs(&s, "snowmix"), [at(24, 0)]);
    assert_eq!(s.take_dropped().len(), 3 * 24);
    for lead in (120..=660).step_by(60) {
        assert!(put(&mut s, "snowmix", fine(36, lead), at(37, 0)).is_none());
    }
    assert!(put(&mut s, "snowmix", fine(36, 720), at(37, 0)).is_some());
    assert_eq!(shown_runs(&s, "snowmix"), [at(36, 0)]);
    for lead in (780..=1440).step_by(60) {
        put(&mut s, "snowmix", fine(36, lead), at(37, 0));
    }
    // The coarse model comes back and is the base again: the fine model's
    // next run covers the anchor but no longer moves it; the coarse model's
    // does, by patience.
    for lead in (180..=480).step_by(60) {
        assert!(put(&mut s, "snowmix", coarse(48, lead), at(49, 0)).is_none());
    }
    for lead in (60..=720).step_by(60) {
        assert!(put(&mut s, "snowmix", fine(48, lead), at(49, 0)).is_none());
    }
    s.sweep(at(49, 0) + mins(ANCHOR_PATIENCE_MINUTES - 1));
    assert_eq!(shown_runs(&s, "snowmix"), [at(36, 0)]);
    s.sweep(at(49, 0) + mins(ANCHOR_PATIENCE_MINUTES));
    assert_eq!(shown_runs(&s, "snowmix"), [at(48, 0)]);
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[test]
fn events_name_new_frames_removed_steps_and_new_parts() {
    let mut s = cat();
    let now = at(12, 0);
    let e = put(&mut s, "clouds", fine(12, 60), now).unwrap();
    let WsEvent::Timeline { parts, .. } = &e else {
        unreachable!()
    };
    assert_eq!(parts, &[fine(12, 60)]);
    assert_eq!(
        ev(&e),
        evt(
            "clouds",
            &["fine-26110612-60"],
            &["13:00 fc fine-26110612-60"],
            &[]
        )
    );
    // A changed frame names a part the timeline already had: only the new
    // one is described.
    let e = put(&mut s, "clouds", coarse(12, 60), now).unwrap();
    assert_eq!(
        ev(&e),
        evt(
            "clouds",
            &["coarse-26110612-60"],
            &["13:00 fc fine-26110612-60 coarse-26110612-60"],
            &[]
        )
    );

    // An observation moves the latest observation past a forecast step,
    // which goes, while the step after it stays as it was.
    let mut s = cat();
    let now = at(12, 12);
    put(&mut s, "radar", fine(11, 75), now);
    put(&mut s, "radar", fine(11, 90), now);
    put(&mut s, "radar", coarse(6, 360), now);
    put(&mut s, "radar", obs(12, 10), now);
    assert_eq!(
        show(&s, "radar"),
        [
            "12:10 obs obs-2611061210",
            "12:15 fc fine-26110611-75 coarse-26110606-360",
            "12:30 fc fine-26110611-90 coarse-26110606-360",
        ]
    );
    let e = put(&mut s, "radar", obs(12, 15), now).unwrap();
    assert_eq!(
        ev(&e),
        evt(
            "radar",
            &["obs-2611061215"],
            &["12:16 obs obs-2611061215"],
            &["12:15"]
        )
    );
}

#[test]
fn no_event_when_nothing_visible_changes() {
    let mut s = cat();
    let now = at(12, 10);
    put(&mut s, "radar", obs(12, 6), now);
    // 12:05 ticks at 12:06, where 12:06 is newer: taken but not shown.
    let (r, e) = s.insert("radar", obs(12, 5), now).unwrap();
    assert_eq!(r, Insert::Added { replaced: None });
    assert!(e.is_none());
    // Refused parts.
    assert!(refused(&mut s, "radar", obs(12, 6), now));
    // A part waiting for an anchor.
    assert!(put(&mut s, "snow", fine(12, 60), now).is_none());
    // A sweep with nothing to do.
    assert!(s.sweep(now).is_empty());
    assert!(s.sweep(now + mins(1)).is_empty());
}

#[test]
fn sweep_announces_each_changed_layer_once() {
    let mut s = cat();
    put(&mut s, "radar", obs(10, 0), at(11, 0));
    put(&mut s, "radar", obs(10, 2), at(11, 0));
    put(&mut s, "radar", obs(11, 0), at(11, 0));
    put(&mut s, "clouds", coarse(10, 0), at(11, 0));
    put(&mut s, "clouds", coarse(10, 60), at(11, 0));
    put(&mut s, "clouds", coarse(10, 180), at(11, 0));
    let events: Vec<Ev> = s.sweep(at(12, 31)).iter().map(ev).collect();
    assert_eq!(
        events,
        [
            evt("radar", &[], &[], &["10:00", "10:02"]),
            evt("clouds", &[], &[], &["10:00", "11:00"]),
        ]
    );
}

#[test]
fn the_catalog_announces_on_the_hub() {
    let hub = Hub::new();
    let mut rx = hub.subscribe();
    let cat = catalog(hub);
    cat.insert("clouds", coarse(12, 0), at(12, 0)).unwrap();
    assert_eq!(
        ev(&rx.try_recv().unwrap()).frames,
        ["12:00 fc coarse-26110612-0"]
    );
    assert_eq!(
        cat.insert("clouds", coarse(12, 0), at(12, 0)),
        Ok(Insert::Refused)
    );
    assert!(rx.try_recv().is_err());
    cat.sweep(at(13, 31));
    assert_eq!(ev(&rx.try_recv().unwrap()).removed, ["12:00"]);
    assert!(rx.try_recv().is_err());
    assert_eq!(cat.take_dropped().len(), 1);
}

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

#[test]
fn resolve_accepts_the_canonical_combo_and_checks_zoom() {
    let mut s = cat();
    // The frame's earlier forms, as its parts arrived one by one, have
    // served their grace by `now`.
    let built = at(12, 10);
    let now = built + secs(SERVE_GRACE_SECONDS);
    put(&mut s, "temp", ana(12), built);
    put(&mut s, "temp", fine(11, 60), built);
    put(&mut s, "temp", coarse(6, 360), built);
    let p = s.frame_parts("temp", at(12, 0)).unwrap();
    assert_eq!(
        ids(&p),
        ["ana-2611061200", "fine-26110611-60", "coarse-26110606-360"]
    );

    let check = |coord: TileCoord, expect: &[&str]| {
        assert_eq!(refs(&tile_parts(&p, coord)), expect, "{coord:?}");
        let got = s
            .resolve("temp", coord, expect, now)
            .unwrap_or_else(|| panic!("{coord:?} refused"));
        assert_eq!(ids(&got), expect);
    };
    // Inside the analysis' solid box it hides everything below.
    let kansas = tile_at(5, -98.0, 38.0);
    check(kansas, &["ana-2611061200"]);
    assert!(s.resolve("temp", kansas, &ids(&p), now).is_none());
    // Across CONUS's edge all three show.
    check(tile_at(2, -120.0, 40.0), &ids(&p));
    // Europe: the coarse model only, to its own max_zoom.
    check(tile_at(3, 10.0, 50.0), &["coarse-26110606-360"]);
    check(tile_at(4, 10.0, 50.0), &["coarse-26110606-360"]);
    assert!(
        s.resolve(
            "temp",
            tile_at(5, 10.0, 50.0),
            &["coarse-26110606-360"],
            now
        )
        .is_none()
    );
    check(tile_at(6, -98.0, 38.0), &["ana-2611061200"]);
    assert!(
        s.resolve("temp", tile_at(7, -98.0, 38.0), &["ana-2611061200"], now)
            .is_none()
    );
    assert_eq!(
        s.resolve("temp", kansas, &["ana-2611061200"], now).unwrap(),
        [ana(12)]
    );
}

#[test]
fn resolve_accepts_must_and_may_variants_at_an_edge() {
    let tile = TileCoord::new(1, 1, 0).unwrap();
    let b = coarse(12, 60);
    let check = |a: Part, ok: &[&[&str]], bad: &[&[&str]]| {
        let mut s = cat();
        put(&mut s, "clouds", a, at(12, 0));
        put(&mut s, "clouds", b.clone(), at(12, 0));
        // Past the grace of the frame's form before B arrived.
        let now = at(12, 0) + secs(SERVE_GRACE_SECONDS);
        for combo in ok {
            assert!(
                s.resolve("clouds", tile, combo, now).is_some(),
                "{combo:?} refused"
            );
        }
        for combo in bad {
            assert!(
                s.resolve("clouds", tile, combo, now).is_none(),
                "{combo:?} accepted"
            );
        }
    };
    let a_id = "fine-26110612-60";
    let b_id = "coarse-26110612-60";

    // Overlaps the tile by less than the tolerance: either answer will do.
    let a = with_bounds(fine(12, 60), LonLatBox::new(-10.0, 10.0, 5e-7, 50.0), None);
    let p = [a.clone(), b.clone()];
    assert_eq!(refs(&tile_parts(&p, tile)), [a_id, b_id]);
    assert_eq!(refs(&tile_parts_must(&p, tile)), [b_id]);
    assert_eq!(refs(&tile_parts_may(&p, tile)), [a_id, b_id]);
    check(a, &[&[a_id, b_id], &[b_id]], &[&[a_id], &[b_id, a_id]]);

    // Touches the tile: the exact rule leaves it out, but naming it is fine.
    let a = with_bounds(fine(12, 60), LonLatBox::new(-10.0, 10.0, 0.0, 50.0), None);
    let p = [a.clone(), b.clone()];
    assert_eq!(refs(&tile_parts(&p, tile)), [b_id]);
    check(a, &[&[b_id], &[a_id, b_id]], &[&[a_id]]);

    // A solid box exactly as wide as the tile may or may not hide B.
    let edge = LonLatBox::new(0.0, -1.0, 180.0, 89.0);
    let a = with_bounds(fine(12, 60), edge, Some(edge));
    let p = [a.clone(), b.clone()];
    assert_eq!(refs(&tile_parts(&p, tile)), [a_id]);
    assert_eq!(refs(&tile_parts_must(&p, tile)), [a_id]);
    assert_eq!(refs(&tile_parts_may(&p, tile)), [a_id, b_id]);
    check(a, &[&[a_id], &[a_id, b_id]], &[&[b_id]]);
}

#[test]
fn resolve_rejects_what_never_was_a_tile() {
    let mut s = cat();
    let built = at(12, 0);
    put(&mut s, "clouds", fine(12, 60), built);
    put(&mut s, "clouds", coarse(12, 60), built);
    put(&mut s, "clouds", coarse(12, 120), built);
    let tile = TileCoord::new(2, 0, 1).unwrap();
    let ok = ["fine-26110612-60", "coarse-26110612-60"];
    // While the frame's fine-only form is in its grace, the fine part alone
    // is still an answer; after it, it is not.
    assert!(
        s.resolve("clouds", tile, &["fine-26110612-60"], built)
            .is_some()
    );
    let now = built + secs(SERVE_GRACE_SECONDS);
    assert!(s.resolve("clouds", tile, &ok, now).is_some());
    assert!(
        s.resolve(
            "clouds",
            TileCoord::new(2, 2, 1).unwrap(),
            &["coarse-26110612-120"],
            now
        )
        .is_some()
    );
    // Parts of two different frames.
    assert!(
        s.resolve(
            "clouds",
            tile,
            &["fine-26110612-60", "coarse-26110612-120"],
            now
        )
        .is_none()
    );
    // Out of order, twice, or missing a part the tile needs.
    assert!(
        s.resolve(
            "clouds",
            tile,
            &["coarse-26110612-60", "fine-26110612-60"],
            now
        )
        .is_none()
    );
    assert!(
        s.resolve(
            "clouds",
            tile,
            &["coarse-26110612-60", "coarse-26110612-60"],
            now
        )
        .is_none()
    );
    assert!(
        s.resolve("clouds", tile, &["fine-26110612-60"], now)
            .is_none()
    );
    // Unknown ids and layers, nothing at all, too much.
    assert!(
        s.resolve("clouds", tile, &["coarse-26110612-999"], now)
            .is_none()
    );
    assert!(
        s.resolve("clouds", tile, &["fine-26110612-60", "nope"], now)
            .is_none()
    );
    assert!(s.resolve("nope", tile, &ok, now).is_none());
    assert!(s.resolve("temp", tile, &ok, now).is_none());
    assert!(s.resolve("clouds", tile, &[], now).is_none());
    assert!(
        s.resolve(
            "clouds",
            tile,
            &["coarse-26110612-60"; MAX_COMBO_PARTS + 1],
            now
        )
        .is_none()
    );
}

#[test]
fn resolve_serves_changed_frames_during_the_grace() {
    let mut s = cat();
    let tile = TileCoord::new(2, 0, 1).unwrap();
    put(&mut s, "clouds", fine(11, 120), at(12, 0));
    put(&mut s, "clouds", coarse(12, 60), at(12, 0));
    let old = ["fine-26110611-120", "coarse-26110612-60"];
    let new = ["fine-26110612-60", "coarse-26110612-60"];
    assert!(s.resolve("clouds", tile, &old, at(12, 0)).is_some());
    assert!(s.resolve("clouds", tile, &new, at(12, 0)).is_none());

    // A newer run changes the frame and drops the part it replaced.
    let t1 = at(12, 10);
    put(&mut s, "clouds", fine(12, 60), t1);
    assert_eq!(s.take_dropped()[0].part, fine(11, 120));
    assert_eq!(
        s.resolve("clouds", tile, &old, t1 + secs(SERVE_GRACE_SECONDS - 1)),
        Some(vec![fine(11, 120), coarse(12, 60)])
    );
    assert!(
        s.resolve("clouds", tile, &old, t1 + secs(SERVE_GRACE_SECONDS))
            .is_none()
    );
    assert!(
        s.resolve("clouds", tile, &new, t1 + secs(SERVE_GRACE_SECONDS))
            .is_some()
    );

    // Changing it again retires the second form too, each for its own grace.
    let t2 = t1 + secs(60);
    put(&mut s, "clouds", fine(13, 0), t2);
    let newest = ["fine-26110613-0", "coarse-26110612-60"];
    assert!(s.resolve("clouds", tile, &newest, t2).is_some());
    assert!(s.resolve("clouds", tile, &old, t2 + secs(10)).is_some());
    assert!(s.resolve("clouds", tile, &new, t2 + secs(10)).is_some());
    // A sweep after the first grace prunes the first form only.
    let t3 = t1 + secs(SERVE_GRACE_SECONDS + 1);
    s.sweep(t3);
    assert!(s.resolve("clouds", tile, &old, t3).is_none());
    assert!(s.resolve("clouds", tile, &new, t3).is_some());
    assert!(
        s.resolve("clouds", tile, &new, t2 + secs(SERVE_GRACE_SECONDS))
            .is_none()
    );
    assert!(
        s.resolve("clouds", tile, &newest, t2 + secs(SERVE_GRACE_SECONDS))
            .is_some()
    );
}

#[test]
fn resolve_serves_removed_frames_during_the_grace() {
    let mut s = cat();
    put(&mut s, "radar", obs(10, 0), at(11, 0));
    put(&mut s, "radar", obs(11, 0), at(11, 0));
    let tile = tile_at(4, -98.0, 38.0);
    let t = at(12, 1);
    assert_eq!(s.sweep(t).len(), 1);
    assert!(s.take_dropped().is_empty(), "the part stays for its hold");
    assert_eq!(
        s.resolve(
            "radar",
            tile,
            &["obs-2611061000"],
            t + secs(SERVE_GRACE_SECONDS - 1)
        ),
        Some(vec![obs(10, 0)])
    );
    assert!(
        s.resolve(
            "radar",
            tile,
            &["obs-2611061000"],
            t + secs(SERVE_GRACE_SECONDS)
        )
        .is_none()
    );
    assert!(
        s.resolve(
            "radar",
            tile,
            &["obs-2611061100"],
            t + secs(SERVE_GRACE_SECONDS)
        )
        .is_some()
    );
}

// ---------------------------------------------------------------------------
// take_dropped, restore and the rest of the surface
// ---------------------------------------------------------------------------

#[test]
fn frame_parts_gives_one_live_frame() {
    let cat = catalog(Hub::new());
    cat.insert("clouds", fine(11, 120), at(12, 0)).unwrap();
    cat.insert("clouds", coarse(12, 60), at(12, 0)).unwrap();
    assert_eq!(
        cat.frame_parts("clouds", at(13, 0)),
        Some(vec![fine(11, 120), coarse(12, 60)])
    );
    // After a change, the frame as it is now, not its retired form.
    cat.insert("clouds", fine(12, 60), at(12, 10)).unwrap();
    assert_eq!(
        cat.frame_parts("clouds", at(13, 0)),
        Some(vec![fine(12, 60), coarse(12, 60)])
    );
    assert_eq!(cat.frame_parts("clouds", at(13, 30)), None);
    assert_eq!(cat.frame_parts("nope", at(13, 0)), None);
}

#[test]
fn take_dropped_reports_each_departure_once() {
    let mut s = cat();
    put(&mut s, "clouds", coarse(6, 360), at(12, 0));
    put(&mut s, "clouds", coarse(12, 60), at(12, 0));
    put(&mut s, "clouds", coarse(12, 0), at(12, 5));
    s.sweep(at(13, 31));
    assert_eq!(
        s.take_dropped(),
        [
            Dropped {
                layer: "clouds",
                part: coarse(6, 360),
                at: at(12, 5)
            },
            Dropped {
                layer: "clouds",
                part: coarse(12, 0),
                at: at(13, 31)
            },
        ]
    );
    assert!(s.take_dropped().is_empty());
    s.sweep(at(13, 32));
    assert!(s.take_dropped().is_empty());
}

#[test]
fn restore_is_silent_and_applies_rule_one() {
    let hub = Hub::new();
    let mut rx = hub.subscribe();
    let cat = catalog(hub);
    cat.restore(
        [
            ("clouds", coarse(6, 360)),
            ("clouds", coarse(12, 0)),
            ("clouds", coarse(0, 720)),
            ("clouds", fine(12, 0)),
            ("clouds", fine(12, 0)),
            ("radar", obs(12, 0)),
        ]
        .map(|(l, p)| (l.to_string(), p)),
    );
    assert!(rx.try_recv().is_err());
    assert!(cat.take_dropped().is_empty());
    let tl = cat.timeline("clouds");
    assert_eq!(
        tl.frames.iter().map(show_frame).collect::<Vec<_>>(),
        ["12:00 fc fine-26110612-0 coarse-26110612-0"]
    );
    assert_eq!(cat.parts().len(), 3);
    assert_eq!(cat.timeline("radar").frames.len(), 1);
    // The restored timeline is live: resolve and later changes work on it.
    let tile = TileCoord::new(2, 0, 1).unwrap();
    assert!(
        cat.resolve(
            "clouds",
            tile,
            &["fine-26110612-0", "coarse-26110612-0"],
            at(12, 0)
        )
        .is_some()
    );
    assert_eq!(
        cat.insert("clouds", coarse(6, 360), at(12, 0)),
        Ok(Insert::Refused)
    );
    cat.insert("clouds", coarse(12, 60), at(12, 0)).unwrap();
    assert_eq!(ev(&rx.try_recv().unwrap()).parts, ["coarse-26110612-60"]);
}

#[test]
fn restore_ignores_unknown_layers_sources_inputs_and_runs() {
    let mut s = cat();
    let mut stray = obs(12, 0);
    stray.source = "nope".into();
    stray.id = "nope-2611061200".into();
    s.restore(
        [
            ("nope", obs(12, 0)),
            ("radar", stray),
            ("clouds", ana(12)),
            ("snow", obs(12, 0)),
            ("snow", fine(13, 60)),
        ]
        .map(|(l, p)| (l.to_string(), p)),
    );
    assert!(s.parts().is_empty());
    assert!(s.timelines().values().all(|t| t.frames.is_empty()));
}

#[test]
fn restore_picks_an_anchor() {
    // `(run, first lead, last lead)`, hourly.
    let restore = |runs: &[(u32, u32, u32)]| {
        let mut s = cat();
        s.restore(runs.iter().flat_map(|&(run, first, last)| {
            (first..=last)
                .step_by(60)
                .map(move |lead| ("snow".to_string(), fine(run, lead)))
        }));
        s
    };

    // The oldest run with enough parts, the one they were saved under, stays;
    // a newer one with nothing to cover waits for its patience, which starts
    // at the first sweep.
    let mut s = restore(&[(0, 240, 720), (12, 60, 360), (24, 60, 120)]);
    assert_eq!(show(&s, "snow").len(), 9);
    assert_eq!(show(&s, "snow")[0], "04:00 fc fine-26110600-240");
    let t = at(0, 0);
    let patience = mins(ANCHOR_PATIENCE_MINUTES);
    assert!(s.sweep(t).is_empty());
    assert!(s.sweep(t + patience - mins(1)).is_empty());
    assert_eq!(s.sweep(t + patience).len(), 1);
    assert_eq!(show(&s, "snow")[0], "13:00 fc fine-26110612-60");
    let gone = s.take_dropped();
    assert_eq!(gone.len(), 9);
    assert!(gone.iter().all(|d| d.at == t + patience));

    // A newer run that covers the anchor takes over straight away; the old
    // run is gone without being reported.
    let mut s = restore(&[(0, 60, 1080), (12, 60, 360), (24, 60, 120)]);
    assert_eq!(show(&s, "snow")[0], "13:00 fc fine-26110612-60");
    assert_eq!(s.parts().len(), 8);
    assert!(s.take_dropped().is_empty());

    // No run with enough parts: no anchor, everything waits.
    let s = restore(&[(0, 60, 300), (12, 60, 300)]);
    assert!(show(&s, "snow").is_empty());
    assert_eq!(s.parts().len(), 10);
}

#[test]
fn wants_agrees_with_insert() {
    let mut s = cat();
    let snow = |lead| ("snow", fine(12, lead), at(13, 0));
    let script = [
        ("radar", obs(12, 0), at(12, 1)),
        ("radar", obs(12, 0), at(12, 1)),
        ("radar", obs(9, 0), at(12, 1)),
        ("radar", obs(12, 7), at(12, 1)),
        ("radar", obs(12, 6), at(12, 1)),
        ("radar", fine(11, 45), at(12, 1)),
        ("radar", fine(11, 60), at(12, 1)),
        ("radar", fine(10, 120), at(12, 1)),
        ("radar", fine(12, 0), at(12, 1)),
        ("radar", nowcast(at(12, 5), 5), at(12, 1)),
        ("radar", nowcast(at(12, 0), 10), at(12, 1)),
        ("clouds", coarse(12, 0), at(13, 31)),
        ("clouds", coarse(12, 0), at(13, 30)),
        ("temp", ana(0), at(12, 1)),
        ("temp", ana(1), at(12, 1)),
        ("temp", ana(13), at(12, 1)),
        snow(60),
        snow(120),
        snow(180),
        snow(240),
        snow(300),
        snow(360),
        ("snow", fine(13, 360), at(13, 0)),
        ("snow", fine(0, 780), at(13, 0)),
        ("snow", fine(24, 60), at(13, 0)),
        snow(360),
    ];
    let mut outcomes = HashSet::new();
    for (layer, p, now) in script {
        let id = p.id.clone();
        let wanted = s.wants(layer, &p.source, p.valid, p.run, now);
        let taken = matches!(s.insert(layer, p, now).unwrap().0, Insert::Added { .. });
        assert_eq!(wanted, taken, "{layer} {id}");
        outcomes.insert(taken);
    }
    assert_eq!(outcomes.len(), 2);
}

#[test]
fn unknown_layers_and_sources_are_errors() {
    let mut s = cat();
    let now = at(12, 0);
    assert_eq!(
        s.insert("nope", obs(12, 0), now).unwrap_err(),
        CatalogError::UnknownLayer("nope".into())
    );
    assert_eq!(
        s.insert("clouds", ana(12), now).unwrap_err(),
        CatalogError::NoSuchInput {
            layer: "clouds".into(),
            source_id: "ana".into()
        }
    );
    let mut stray = obs(12, 0);
    stray.source = "nope".into();
    assert_eq!(
        s.insert("radar", stray, now).unwrap_err(),
        CatalogError::NoSuchInput {
            layer: "radar".into(),
            source_id: "nope".into()
        }
    );
    assert!(!s.wants("nope", "obs", at(12, 0), None, now));
    assert!(!s.wants("clouds", "ana", at(12, 0), None, now));
    assert!(!s.wants("radar", "nope", at(12, 0), None, now));
    assert!(s.parts().is_empty());
}

#[test]
fn latest_parts_and_timelines() {
    let mut s = cat();
    let now = at(13, 0);
    put(&mut s, "clouds", coarse(6, 360), now);
    put(&mut s, "clouds", coarse(6, 420), now);
    put(&mut s, "clouds", coarse(12, 0), now);
    assert_eq!(s.latest("clouds", "coarse"), Some(coarse(6, 420)));
    assert_eq!(s.latest("clouds", "fine"), None);
    assert_eq!(s.latest("nope", "coarse"), None);
    for lead in (60..=360).step_by(60) {
        put(&mut s, "snow", fine(12, lead), now);
    }
    for lead in (60..=240).step_by(60) {
        put(&mut s, "snow", fine(24, lead), now);
    }
    // Waiting parts count.
    assert_eq!(s.latest("snow", "fine"), Some(fine(24, 240)));
    assert_eq!(s.parts().len(), 2 + 6 + 4);
    let keys: Vec<&str> = s.timelines().into_keys().collect();
    let mut all: Vec<&str> = REGISTRY.layers.iter().map(|l| l.id).collect();
    all.sort_unstable();
    assert_eq!(keys, all);
    assert_eq!(*s.timeline("nope"), Timeline::default());
}

/// The one test against the real registries, written to hold whatever rows
/// they gain: every layer takes a part from each source with an input on it,
/// and shows every one of them.
#[test]
fn every_real_layer_shows_a_part_from_each_of_its_sources() {
    for spec in LAYERS {
        let cat = FrameCatalog::new(Hub::new());
        // A moment on one of the layer's ticks, and a run before it.
        let step = spec.observed_tick_minutes.max(1) as i64 * 60;
        let t = at(12, 0).timestamp();
        let now = DateTime::from_timestamp((t + step - 1).div_euclid(step) * step, 0).unwrap();
        let cycle = if spec.anchored {
            ANCHOR_CYCLE_HOURS * 3600
        } else {
            3600
        };
        let run = DateTime::from_timestamp(now.timestamp().div_euclid(cycle) * cycle, 0).unwrap();
        let ahead = (now - run).num_minutes() as u32 + 60;
        let count = if spec.anchored {
            MIN_ANCHOR_PARTS as u32
        } else {
            1
        };
        let mut taken = Vec::new();
        for src in SOURCES.iter().filter(|s| spec.input_for(s).is_some()) {
            let parts: Vec<Part> = match src.kind {
                FrameKind::Observed => vec![Part::observed(
                    src.id,
                    now,
                    LonLatBox::WORLD,
                    None,
                    src.max_zoom,
                )],
                FrameKind::Forecast => (0..count)
                    .map(|k| {
                        Part::forecast(
                            src.id,
                            run,
                            ahead + 60 * k,
                            LonLatBox::WORLD,
                            None,
                            src.max_zoom,
                        )
                    })
                    .collect(),
            };
            for p in parts {
                let id = p.id.clone();
                let r = cat
                    .insert(spec.id, p, now)
                    .map(|r| matches!(r, Insert::Added { .. }));
                assert_eq!(r, Ok(true), "{}: {id}", spec.id);
                taken.push(id);
            }
        }
        let tl = cat.timeline(spec.id);
        for id in &taken {
            assert!(
                tl.parts.iter().any(|p| &p.id == id),
                "{}: {id} is on no frame",
                spec.id
            );
        }
        assert_eq!(taken.is_empty(), tl.frames.is_empty(), "{}", spec.id);
    }
}

// ---------------------------------------------------------------------------
// tile_parts and publish_boxes
// ---------------------------------------------------------------------------

#[test]
fn tile_parts_across_the_antimeridian() {
    let alaska = Part::observed(
        "obs",
        at(12, 0),
        ALASKA,
        Some(LonLatBox::new(165.0, 50.0, -140.0, 70.0)),
        7,
    );
    let p = [alaska, coarse(12, 0)];
    let names = |z, x, y| {
        let t = TileCoord::new(z, x, y).unwrap();
        let exact = refs(&tile_parts(&p, t));
        assert_eq!(refs(&tile_parts_must(&p, t)), exact, "{t:?}");
        assert_eq!(refs(&tile_parts_may(&p, t)), exact, "{t:?}");
        exact
    };
    let both = ["obs-2611061200", "coarse-26110612-0"];
    // Either side of the line, and the column the box ends in.
    assert_eq!(names(3, 7, 2), both);
    assert_eq!(names(3, 0, 2), both);
    assert_eq!(names(3, 1, 2), both);
    assert_eq!(names(3, 4, 2), ["coarse-26110612-0"]);
    // The solid box, which crosses the line too, hides the coarse model on
    // both sides.
    assert_eq!(names(5, 31, 9), ["obs-2611061200"]);
    assert_eq!(names(5, 0, 9), ["obs-2611061200"]);
}

#[test]
fn a_solid_part_hides_what_is_below() {
    let p = [ana(12), fine(12, 0), coarse(12, 0)];
    let inside = tile_at(6, -98.0, 38.0);
    assert_eq!(refs(&tile_parts(&p, inside)), ["ana-2611061200"]);
    assert_eq!(refs(&tile_parts_must(&p, inside)), ["ana-2611061200"]);
    assert_eq!(refs(&tile_parts_may(&p, inside)), ["ana-2611061200"]);
    // Without the solid box the lower parts show through.
    let p = [
        with_bounds(ana(12), CONUS, None),
        fine(12, 0),
        coarse(12, 0),
    ];
    assert_eq!(
        refs(&tile_parts(&p, inside)),
        ["ana-2611061200", "fine-26110612-0"]
    );
}

#[test]
fn the_z0_tile() {
    let z0 = TileCoord::new(0, 0, 0).unwrap();
    let alaska = Part::observed("ana", at(12, 0), ALASKA, None, 6);
    let p = [obs(12, 0), alaska, fine(12, 0), coarse(12, 0)];
    let all = ids(&p);
    assert_eq!(refs(&tile_parts(&p, z0)), all);
    assert_eq!(refs(&tile_parts_must(&p, z0)), all);
    assert_eq!(refs(&tile_parts_may(&p, z0)), all);
    // A world-covering solid part on top hides everything else.
    let p = [coarse(12, 0), obs(12, 0)];
    assert_eq!(refs(&tile_parts(&p, z0)), ["coarse-26110612-0"]);
    assert_eq!(refs(&tile_parts_must(&p, z0)), ["coarse-26110612-0"]);
    assert_eq!(refs(&tile_parts_may(&p, z0)), ["coarse-26110612-0"]);
}

#[test]
fn publish_boxes_round_bounds_out_and_solid_boxes_in() {
    let b = LonLatBox::new(-97.123, 30.456, -80.001, 45.999);
    let (bounds, solid) = publish_boxes(b, Some(b));
    assert_eq!(bounds, LonLatBox::new(-97.13, 30.45, -80.0, 46.0));
    assert_eq!(solid, Some(LonLatBox::new(-97.12, 30.46, -80.01, 45.99)));
    // Kept on the globe.
    let (bounds, _) = publish_boxes(LonLatBox::new(-180.004, -90.5, 180.003, 90.2), None);
    assert_eq!(bounds, LonLatBox::new(-180.0, -90.0, 180.0, 90.0));
}

#[test]
fn publish_boxes_keep_the_antimeridian() {
    let (bounds, solid) = publish_boxes(
        LonLatBox::new(170.004, 50.0, -130.006, 72.0),
        Some(LonLatBox::new(170.004, 51.0, -130.006, 71.0)),
    );
    assert_eq!(bounds, LonLatBox::new(170.0, 50.0, -130.0, 72.0));
    assert_eq!(solid, Some(LonLatBox::new(170.01, 51.0, -130.01, 71.0)));
    assert!(bounds.wraps() && solid.unwrap().wraps());
    // A box that wraps nearly all the way round becomes the whole circle
    // rather than a sliver.
    let (bounds, _) = publish_boxes(LonLatBox::new(0.004, -80.0, 0.001, 80.0), None);
    assert_eq!(bounds, LonLatBox::new(-180.0, -80.0, 180.0, 80.0));
    let (bounds, _) = publish_boxes(LonLatBox::new(0.004, -80.0, -0.004, 80.0), None);
    assert_eq!((bounds.west, bounds.east), (-180.0, 180.0));
}

#[test]
fn publish_boxes_drop_solid_slivers() {
    let b = LonLatBox::new(0.0, 0.0, 20.0, 20.0);
    assert_eq!(
        publish_boxes(b, Some(LonLatBox::new(10.001, 0.0, 10.009, 5.0))).1,
        None
    );
    assert_eq!(
        publish_boxes(b, Some(LonLatBox::new(0.0, 10.001, 5.0, 10.009))).1,
        None
    );
    assert_eq!(publish_boxes(b, None).1, None);
}

// ---------------------------------------------------------------------------
// Partial rebuilds
// ---------------------------------------------------------------------------

/// Random inserts and sweeps on every test layer. After every rebuild, of
/// whatever span, the catalog checks its timeline against a whole build
/// (`check_against_a_full_build`), so this holds partial rebuilds to the
/// rules over far more situations than the tests above spell out.
#[test]
fn partial_rebuilds_match_whole_ones() {
    for seed in [1u64, 7, 42, 1234] {
        let mut rng = seed;
        let mut next = |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        let mut s = cat();
        let mut now = at(12, 0);
        let mut taken = 0;
        for step in 0..3000 {
            now += mins(next(3) as i64);
            let layer = REGISTRY.layers[next(REGISTRY.layers.len() as u64) as usize];
            let src = &REGISTRY.sources[next(REGISTRY.sources.len() as u64) as usize];
            let hour = |t: DateTime<Utc>| t - secs(t.timestamp().rem_euclid(3600));
            let part = match src.id {
                "obs" => Part::observed("obs", now - mins(next(200) as i64 - 3), CONUS, None, 7),
                "ana" => Part::observed(
                    "ana",
                    hour(now) - Duration::hours(next(8) as i64),
                    CONUS,
                    Some(CONUS_SOLID),
                    6,
                ),
                "nowcast" => {
                    let run =
                        now - secs(now.timestamp().rem_euclid(300)) - mins(5 * next(4) as i64);
                    nowcast(run, 5 * next(13) as u32)
                }
                id => {
                    let run = if layer.anchored {
                        let cycle = 12 * 3600;
                        now - secs(now.timestamp().rem_euclid(cycle))
                            - Duration::hours(12 * next(2) as i64)
                    } else {
                        hour(now) - Duration::hours(next(7) as i64)
                    };
                    let world = id == "coarse";
                    let (b, solid) = if world {
                        (LonLatBox::WORLD, Some(LonLatBox::WORLD))
                    } else {
                        (CONUS, None)
                    };
                    Part::forecast(id, run, 60 * next(30) as u32, b, solid, 6)
                }
            };
            if let Ok((Insert::Added { .. }, _)) = s.insert(layer.id, part, now) {
                taken += 1;
            }
            if step % 17 == 0 {
                s.sweep(now);
            }
        }
        assert!(taken > 1000, "seed {seed}: only {taken} parts taken");
    }
}

// ---------------------------------------------------------------------------
// Scale
// ---------------------------------------------------------------------------

/// 25 sources with 300 valid times each on one layer. Prints the costs; the
/// bounds only catch something gone badly wrong, even in an unoptimized build.
#[test]
fn rebuild_and_resolve_stay_fast_at_scale() {
    let families = [
        (Family::Mrms, FrameKind::Observed),
        (Family::Hrrr, FrameKind::Forecast),
        (Family::Gfs, FrameKind::Forecast),
    ];
    let sources = leak(
        (0..25u16)
            .map(|k| {
                let (family, kind) = families[k as usize % 3];
                source_row(leak_str(format!("s{k:02}")), family, kind, 1000 + k)
            })
            .collect(),
    );
    let spec = layer_row(
        "big",
        &[
            (Family::Mrms, 6, false),
            (Family::Hrrr, 15, false),
            (Family::Gfs, 180, false),
        ],
        24 * 60,
        2,
        false,
    );
    let mut s = State::with_registry(leak(vec![spec]), sources);
    let now = at(12, 0);
    // Overlapping regions round the globe; the last ones cross the
    // antimeridian.
    let region = |k: usize| {
        let w = -180.0 + 14.0 * k as f64;
        let e = if w + 30.0 > 180.0 {
            w + 30.0 - 360.0
        } else {
            w + 30.0
        };
        LonLatBox::new(w, -60.0 + k as f64, e, 60.0 - k as f64)
    };
    let mut parts = Vec::new();
    for (k, src) in sources.iter().enumerate() {
        let solid = region(k).grown(-2.0);
        for i in 0..300u32 {
            let p = match src.family {
                Family::Mrms => {
                    Part::observed(src.id, now - mins(600 - 2 * i as i64), region(k), solid, 7)
                }
                Family::Hrrr => Part::forecast(src.id, now, 15 * i, region(k), solid, 6),
                _ => Part::forecast(src.id, now, 60 * i, region(k), solid, 4),
            };
            parts.push(("big".to_string(), p));
        }
    }
    let n = parts.len();
    let start = std::time::Instant::now();
    s.restore(parts);
    let restore = start.elapsed();
    let frames = s.timeline("big").frames.len();
    assert_eq!(s.parts().len(), n);

    // A newer run of one fine model replaces its parts one by one.
    let fine = sources.iter().find(|d| d.family == Family::Hrrr).unwrap();
    let start = std::time::Instant::now();
    let mut announced = 0;
    for i in 0..300u32 {
        let p = Part::forecast(fine.id, now + mins(60), 15 * i, region(1), None, 6);
        announced += s.insert("big", p, now).unwrap().1.is_some() as usize;
    }
    let insert = start.elapsed() / 300;
    assert!(announced > 250);

    let tl = s.timeline("big");
    let lookup: HashMap<&str, &Part> = tl.parts.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut requests = Vec::new();
    for (k, f) in tl.frames.iter().enumerate().step_by(3) {
        let p: Vec<Part> = f
            .parts
            .iter()
            .map(|id| lookup[id.as_str()].clone())
            .collect();
        let tile = tile_at(4, -170.0 + (k % 300) as f64, 0.0);
        let combo: Vec<String> = tile_parts(&p, tile).iter().map(|p| p.id.clone()).collect();
        if !combo.is_empty() {
            requests.push((tile, combo));
        }
    }
    let start = std::time::Instant::now();
    for (tile, combo) in &requests {
        let combo: Vec<&str> = combo.iter().map(String::as_str).collect();
        assert!(s.resolve("big", *tile, &combo, now).is_some());
    }
    let resolve = start.elapsed() / requests.len() as u32;
    eprintln!(
        "{n} parts, {frames} frames: restore {restore:?}, insert {insert:?} each, \
         resolve {resolve:?} each over {} requests",
        requests.len()
    );
    assert!(
        insert < std::time::Duration::from_millis(50),
        "insert {insert:?}"
    );
    assert!(
        resolve < std::time::Duration::from_millis(1),
        "resolve {resolve:?}"
    );
}
