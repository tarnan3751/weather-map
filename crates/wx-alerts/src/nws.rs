//! Reading api.weather.gov GeoJSON: the active-alerts feed and zone
//! boundaries. Everything here is defensive: a feature that does not make
//! sense is skipped, never allowed to fail the whole feed.

use crate::vtec::{self, Vtec};
use chrono::{DateTime, FixedOffset, Utc};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// A zone an alert names, with the API collection it lives in
/// (`forecast`, `county`, `fire`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ZoneRef {
    pub kind: String,
    pub id: String,
}

/// Serialized, an alert is a row of `alerts` (its fields are named after
/// the columns) and its own `polygon`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FeedAlert {
    pub id: String,
    /// The key derived from this message alone. The feed step prefers the
    /// key of a stored message this one continues (see `feed::resolve_keys`).
    pub event_key: String,
    /// `OFFICE.PH.S.ETN.` of the event the alert is about, if it has VTEC.
    #[serde(skip)]
    pub vtec_prefix: Option<String>,
    /// That event's VTEC action is not NEW: it continues an earlier one.
    #[serde(skip)]
    pub continues: bool,
    pub event: String,
    pub severity: Option<String>,
    pub urgency: Option<String>,
    pub certainty: Option<String>,
    pub status: Option<String>,
    pub message_type: Option<String>,
    pub headline: Option<String>,
    pub description: Option<String>,
    pub instruction: Option<String>,
    pub area_desc: Option<String>,
    pub sender_name: Option<String>,
    pub sent: Option<DateTime<Utc>>,
    pub effective: Option<DateTime<Utc>>,
    pub onset: Option<DateTime<Utc>>,
    pub expires: Option<DateTime<Utc>>,
    pub ends: Option<DateTime<Utc>>,
    /// Sorted, unique UGC codes of every zone the alert names. With the
    /// `affectedZones` in `raw` they give the zones to fetch.
    pub ugc: Vec<String>,
    /// The alert's own shape as clean GeoJSON (a MultiPolygon), if any.
    pub polygon: Option<String>,
    /// Cancelled, or past its end; stored but neither drawn nor notified.
    #[serde(skip)]
    pub ended: bool,
    /// Ids of the earlier messages this one replaces.
    #[serde(skip)]
    pub references: Vec<String>,
    pub raw: Value,
    /// Fingerprint of `raw`, to skip rewriting alerts that did not change.
    #[serde(skip)]
    pub hash: [u8; 32],
}

#[derive(Debug, Default)]
pub(crate) struct ParsedFeed {
    pub alerts: Vec<FeedAlert>,
    /// Features that were not `status: Actual` (tests, exercises, drafts).
    pub not_actual: usize,
    /// Features that could not be understood.
    pub skipped: usize,
}

impl ParsedFeed {
    pub fn newest_sent(&self) -> Option<DateTime<Utc>> {
        self.alerts.iter().filter_map(|a| a.sent).max()
    }
}

pub(crate) fn parse_feed(body: &[u8], now: DateTime<Utc>) -> anyhow::Result<ParsedFeed> {
    let doc: Value = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("alerts feed is not JSON: {e}"))?;
    let features = doc
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("alerts feed has no features array"))?;
    let mut out = ParsedFeed::default();
    let mut seen = BTreeSet::new();
    for feature in features {
        match parse_feature(feature, now) {
            Feature::Alert(a) => {
                // Duplicate ids would make the batch upsert ambiguous.
                if seen.insert(a.id.clone()) {
                    out.alerts.push(*a);
                }
            }
            Feature::NotActual => out.not_actual += 1,
            Feature::Invalid(why) => {
                out.skipped += 1;
                tracing::debug!("skipping alert feature: {why}");
            }
        }
    }
    Ok(out)
}

enum Feature {
    Alert(Box<FeedAlert>),
    NotActual,
    Invalid(&'static str),
}

fn parse_feature(feature: &Value, now: DateTime<Utc>) -> Feature {
    let mut raw = feature.clone();
    strip_nul(&mut raw);
    let Some(p) = raw.get("properties").filter(|p| p.is_object()) else {
        return Feature::Invalid("no properties");
    };
    if text(p, "status").as_deref() != Some("Actual") {
        return Feature::NotActual;
    }
    let Some(id) = text(p, "id").filter(|id| id.len() <= 256) else {
        return Feature::Invalid("no id");
    };
    let Some(event) = text(p, "event").filter(|e| e.len() <= 256) else {
        return Feature::Invalid("no event");
    };

    let sent = time(p, "sent");
    let vtecs = vtec::parse_all(&strings(
        p.get("parameters").unwrap_or(&Value::Null),
        "VTEC",
    ));
    let vtec = vtec::primary(&vtecs);
    let refs: Vec<&Value> = p
        .get("references")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let issued = refs
        .iter()
        .filter_map(|r| time(r, "sent"))
        .chain(sent)
        .min();
    let references: Vec<String> = refs.iter().filter_map(|r| text(r, "identifier")).collect();
    let event_key = match vtec {
        Some(v) => v.event_key(issued),
        None => id.clone(),
    };

    let affected = strings(p, "affectedZones");
    let geocode_ugc = strings(p.get("geocode").unwrap_or(&Value::Null), "UGC");
    let ugc: Vec<String> = zone_refs(&affected, &geocode_ugc)
        .into_iter()
        .map(|z| z.id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let ends = time(p, "ends");
    let expires = time(p, "expires");
    let message_type = text(p, "messageType");
    let ended = message_type.as_deref() == Some("Cancel")
        || vtec::all_ending(&vtecs)
        || ends.or(expires).is_some_and(|t| t <= now);
    let vtec_prefix = vtec.map(Vtec::key_prefix);
    let continues = vtec.is_some_and(Vtec::continues);

    let polygon = raw.get("geometry").and_then(clean_polygons);
    let hash = Sha256::digest(serde_json::to_vec(&raw).unwrap_or_default()).into();

    Feature::Alert(Box::new(FeedAlert {
        event_key,
        vtec_prefix,
        continues,
        event,
        severity: text(p, "severity"),
        urgency: text(p, "urgency"),
        certainty: text(p, "certainty"),
        status: text(p, "status"),
        message_type,
        headline: text(p, "headline"),
        description: text(p, "description"),
        instruction: text(p, "instruction"),
        area_desc: text(p, "areaDesc"),
        sender_name: text(p, "senderName"),
        sent,
        effective: time(p, "effective"),
        onset: time(p, "onset"),
        expires,
        ends,
        ugc,
        polygon,
        ended,
        references,
        id,
        hash,
        raw,
    }))
}

/// Zones named by an alert. `affectedZones` URLs say which collection a
/// zone is in; UGC codes missing from them are guessed from the code.
pub(crate) fn zone_refs(affected_zones: &[String], ugc: &[String]) -> Vec<ZoneRef> {
    let mut out: Vec<ZoneRef> = Vec::new();
    let mut seen = BTreeSet::new();
    for url in affected_zones {
        if let Some(z) = zone_ref_from_url(url)
            && seen.insert(z.id.clone())
        {
            out.push(z);
        }
    }
    for code in ugc {
        if is_ugc(code) && seen.insert(code.clone()) {
            let kind = if code.as_bytes()[2] == b'C' {
                "county"
            } else {
                "forecast"
            };
            out.push(ZoneRef {
                kind: kind.to_string(),
                id: code.clone(),
            });
        }
    }
    out
}

/// `https://api.weather.gov/zones/forecast/TXZ123` -> (forecast, TXZ123).
/// Only the path is used: zones are always fetched from the configured
/// API base, never from a URL found in the feed.
fn zone_ref_from_url(url: &str) -> Option<ZoneRef> {
    let parsed = url::Url::parse(url).ok()?;
    let segments: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    let at = segments.iter().rposition(|s| *s == "zones")?;
    let (kind, id) = (segments.get(at + 1)?, segments.get(at + 2)?);
    let kind_ok = (1..=16).contains(&kind.len()) && kind.bytes().all(|b| b.is_ascii_lowercase());
    (kind_ok && is_ugc(id) && segments.len() == at + 3).then(|| ZoneRef {
        kind: kind.to_string(),
        id: id.to_string(),
    })
}

/// `SSZNNN` / `SSCNNN`: two letters, `Z` (zone) or `C` (county), three digits.
pub(crate) fn is_ugc(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 6
        && b[0].is_ascii_uppercase()
        && b[1].is_ascii_uppercase()
        && (b[2] == b'Z' || b[2] == b'C')
        && b[3..].iter().all(u8::is_ascii_digit)
}

/// A zone boundary from `/zones/{kind}/{id}`.
#[derive(Debug, Clone)]
pub(crate) struct ZoneData {
    pub id: String,
    pub kind: String,
    pub name: Option<String>,
    pub state: Option<String>,
    pub geometry: String,
}

/// `Ok(None)` when the zone exists but has no usable boundary.
pub(crate) fn parse_zone(body: &[u8], requested: &ZoneRef) -> anyhow::Result<Option<ZoneData>> {
    let mut doc: Value =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("zone is not JSON: {e}"))?;
    strip_nul(&mut doc);
    let props = doc.get("properties").unwrap_or(&Value::Null);
    if let Some(id) = text(props, "id")
        && id != requested.id
    {
        anyhow::bail!("asked for zone {} but got {id}", requested.id);
    }
    let Some(geometry) = doc.get("geometry").and_then(clean_polygons) else {
        return Ok(None);
    };
    Ok(Some(ZoneData {
        id: requested.id.clone(),
        // The API collection, which is what a later refresh fetches from
        // (marine zones live in `forecast`).
        kind: requested.kind.clone(),
        name: text(props, "name").map(|n| truncate(&n, 200)),
        state: text(props, "state").filter(|s| s.len() <= 8),
        geometry,
    }))
}

/// Keep only polygonal parts with sane coordinates and closed rings, as a
/// GeoJSON MultiPolygon. PostGIS then never sees input it would reject,
/// and `ST_MakeValid` takes care of self-intersections.
pub(crate) fn clean_polygons(geometry: &Value) -> Option<String> {
    let mut polygons = Vec::new();
    collect_polygons(geometry, &mut polygons, 0);
    (!polygons.is_empty())
        .then(|| json!({ "type": "MultiPolygon", "coordinates": polygons }).to_string())
}

fn collect_polygons(g: &Value, out: &mut Vec<Value>, depth: usize) {
    if depth > 4 {
        return;
    }
    let coords = g.get("coordinates");
    match g.get("type").and_then(Value::as_str) {
        Some("Polygon") => {
            if let Some(p) = coords.and_then(clean_polygon) {
                out.push(p);
            }
        }
        Some("MultiPolygon") => {
            for p in coords.and_then(Value::as_array).into_iter().flatten() {
                if let Some(p) = clean_polygon(p) {
                    out.push(p);
                }
            }
        }
        Some("GeometryCollection") => {
            for member in g
                .get("geometries")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                collect_polygons(member, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn clean_polygon(rings: &Value) -> Option<Value> {
    let rings = rings.as_array()?;
    let mut out = Vec::with_capacity(rings.len());
    for (i, ring) in rings.iter().enumerate() {
        match clean_ring(ring) {
            Some(r) => out.push(r),
            // Without its outer ring a polygon is nothing; a broken hole
            // is just dropped.
            None if i == 0 => return None,
            None => {}
        }
    }
    Some(Value::Array(out))
}

fn clean_ring(ring: &Value) -> Option<Value> {
    let mut points: Vec<[f64; 2]> = Vec::new();
    for p in ring.as_array()? {
        let p = p.as_array()?;
        let (x, y) = (p.first()?.as_f64()?, p.get(1)?.as_f64()?);
        // Longitudes past +/-180 occur on zones that straddle the date line.
        if !x.is_finite() || !y.is_finite() || x.abs() > 540.0 || y.abs() > 90.0 {
            return None;
        }
        points.push([x, y]);
    }
    if points.first() != points.last() {
        points.push(*points.first()?);
    }
    (points.len() >= 4).then(|| json!(points))
}

/// "until 6:45 PM CST" (or "until November 7 at 6:00 AM PST" when the
/// end falls on another day), taken from the NWS headline, which spells
/// the local time zone out, or else from `ends` in the offset the NWS gave
/// it. `ends_raw` is the alert's `ends` only: `expires` is when the next
/// statement is due, not when the hazard ends, so an alert without `ends`
/// (a river flood warning, typically) is "until further notice".
pub(crate) fn until_phrase(headline: Option<&str>, ends_raw: Option<&str>) -> Option<String> {
    let Some(ends_raw) = ends_raw else {
        return Some("until further notice".to_string());
    };
    if let Some(phrase) = headline.and_then(headline_until) {
        return Some(phrase);
    }
    let end = DateTime::<FixedOffset>::parse_from_rfc3339(ends_raw).ok()?;
    let offset = end.offset().local_minus_utc();
    let (sign, abs) = if offset < 0 {
        ('-', -offset)
    } else {
        ('+', offset)
    };
    let zone = match abs % 3600 {
        0 => format!("UTC{sign}{}", abs / 3600),
        _ => format!("UTC{sign}{}:{:02}", abs / 3600, abs % 3600 / 60),
    };
    Some(format!("until {} {zone}", end.format("%-I:%M %p")))
}

fn headline_until(headline: &str) -> Option<String> {
    let after_issued = headline.split_once(" issued ")?.1;
    let (issued, rest) = after_issued.split_once(" until ")?;
    let until = rest.rsplit_once(" by ").map_or(rest, |(u, _)| u);
    let (issued_day, _) = split_when(issued)?;
    let (until_day, (clock, zone)) = split_when(until)?;
    let clock = clock_with_space(clock)?;
    Some(if issued_day == until_day {
        format!("until {clock} {zone}")
    } else {
        format!("until {until_day} at {clock} {zone}")
    })
}

/// "November 6 at 6:45PM CST" -> ("November 6", ("6:45PM", "CST")).
fn split_when(s: &str) -> Option<(&str, (&str, &str))> {
    let (day, rest) = s.split_once(" at ")?;
    let (clock, zone) = rest.trim().split_once(' ')?;
    let zone_ok = (2..=5).contains(&zone.len()) && zone.bytes().all(|b| b.is_ascii_alphabetic());
    (zone_ok && !day.is_empty() && day.len() <= 24).then_some((day, (clock, zone)))
}

/// "6:45PM" -> "6:45 PM".
fn clock_with_space(clock: &str) -> Option<String> {
    // `get`, not `split_at`: headline text is remote and may not be ASCII.
    let cut = clock.len().checked_sub(2)?;
    let (digits, half) = (clock.get(..cut)?, clock.get(cut..)?);
    let ok = matches!(half, "AM" | "PM")
        && (1..=5).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit() || b == b':');
    ok.then(|| format!("{digits} {half}"))
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn time(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    let s = v.get(key)?.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// PostgreSQL rejects NUL in `text` and `\u0000` in `jsonb`.
fn strip_nul(v: &mut Value) {
    match v {
        Value::String(s) if s.contains('\0') => s.retain(|c| c != '\0'),
        Value::Array(items) => items.iter_mut().for_each(strip_nul),
        Value::Object(map) => {
            if map.keys().any(|k| k.contains('\0')) {
                let old = std::mem::take(map);
                *map = old
                    .into_iter()
                    .map(|(k, v)| (k.replace('\0', ""), v))
                    .collect::<Map<String, Value>>();
            }
            map.values_mut().for_each(strip_nul);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const FEED: &[u8] = include_bytes!("../tests/fixtures/alerts_active.json");
    const ZONE_369: &[u8] = include_bytes!("../tests/fixtures/zone_fire_CAZ369.json");

    fn fixture_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 6, 12, 30, 0).unwrap()
    }

    fn find<'a>(feed: &'a ParsedFeed, suffix: &str) -> &'a FeedAlert {
        feed.alerts.iter().find(|a| a.id.ends_with(suffix)).unwrap()
    }

    /// The zones the shape step will look up for an alert.
    fn zones_of(a: &FeedAlert) -> Vec<ZoneRef> {
        zone_refs(&strings(&a.raw["properties"], "affectedZones"), &a.ugc)
    }

    #[test]
    fn parses_the_recorded_feed() {
        let feed = parse_feed(FEED, fixture_now()).unwrap();
        assert_eq!(feed.alerts.len(), 6);
        assert_eq!(feed.not_actual, 1, "the KEEPALIVE test message is dropped");
        assert_eq!(feed.skipped, 0);
        assert_eq!(
            feed.alerts.iter().filter(|a| a.polygon.is_some()).count(),
            3
        );
        assert_eq!(
            feed.newest_sent(),
            Some(Utc.with_ymd_and_hms(2026, 11, 6, 12, 16, 0).unwrap())
        );
    }

    #[test]
    fn polygon_warning() {
        let feed = parse_feed(FEED, fixture_now()).unwrap();
        let a = find(&feed, "07cec9d89e97566ed511c13e.001.1");
        assert_eq!(a.event, "Flash Flood Warning");
        assert_eq!(a.event_key, "KPHI.FF.W.0158.2026");
        assert_eq!(a.severity.as_deref(), Some("Severe"));
        assert_eq!(a.sender_name.as_deref(), Some("NWS Mount Holly NJ"));
        assert_eq!(
            a.sent,
            Some(Utc.with_ymd_and_hms(2026, 11, 6, 11, 12, 0).unwrap())
        );
        assert_eq!(
            a.ends,
            Some(Utc.with_ymd_and_hms(2026, 11, 6, 15, 15, 0).unwrap())
        );
        assert!(!a.ended);
        let poly: Value = serde_json::from_str(a.polygon.as_ref().unwrap()).unwrap();
        assert_eq!(poly["type"], "MultiPolygon");
        let ring = poly["coordinates"][0][0].as_array().unwrap();
        assert_eq!(ring.len(), 7);
        assert_eq!(ring.first(), ring.last());
        assert_eq!(a.ugc, ["NJC005", "NJC007", "NJC015"]);
    }

    #[test]
    fn zone_alert_names_fire_zones() {
        let feed = parse_feed(FEED, fixture_now()).unwrap();
        let a = find(&feed, "e058e5e892776c071e50760f.001.1");
        assert_eq!(a.event, "Red Flag Warning");
        assert!(a.polygon.is_none());
        let zones = zones_of(a);
        assert_eq!(zones.len(), 5);
        assert!(zones.iter().all(|z| z.kind == "fire"));
        assert_eq!(a.ugc, ["CAZ288", "CAZ369", "CAZ370", "CAZ375", "CAZ379"]);
        assert_eq!(a.event_key, "KLOX.FW.W.0006.2026");
    }

    #[test]
    fn expired_message_with_future_end_is_still_in_effect() {
        // The Juneau Storm Warning's message expired the day before but the
        // hazard ends on November 12; the NWS keeps it active, and so do we.
        let feed = parse_feed(FEED, fixture_now()).unwrap();
        let a = find(&feed, "9f057f4bfe4d02c3eb322b25.012.1");
        assert!(a.expires.unwrap() < fixture_now());
        assert!(!a.ended);
        assert_eq!(
            zones_of(a),
            [ZoneRef {
                kind: "forecast".into(),
                id: "PKZ036".into()
            }]
        );
    }

    #[test]
    fn continuation_year_uses_the_referenced_originals() {
        let feed = parse_feed(FEED, fixture_now()).unwrap();
        let a = find(&feed, "4a7c938733f37d326216ca69.006.1");
        assert_eq!(a.message_type.as_deref(), Some("Update"));
        assert_eq!(a.event_key, "KMQT.SC.Y.0169.2026");
        assert_eq!(a.references.len(), 4);
        assert!(
            a.references
                .iter()
                .all(|r| r.starts_with("urn:oid:2.49.0.1.840.0."))
        );
    }

    #[test]
    fn alert_without_vtec_keys_on_its_id() {
        let feed = parse_feed(FEED, fixture_now()).unwrap();
        let a = find(&feed, "2794dde317608956a3bbecff.001.1");
        assert_eq!(a.event, "Marine Weather Statement");
        assert_eq!(a.event_key, a.id);
    }

    #[test]
    fn end_time_decides_ended() {
        let later = Utc.with_ymd_and_hms(2026, 11, 6, 16, 0, 0).unwrap();
        let feed = parse_feed(FEED, later).unwrap();
        assert!(find(&feed, "07cec9d89e97566ed511c13e.001.1").ended);
        // No `ends`: `expires` (12:45Z) decides.
        assert!(find(&feed, "2794dde317608956a3bbecff.001.1").ended);
        assert!(!find(&feed, "e058e5e892776c071e50760f.001.1").ended);
    }

    /// The alert a feature without geometry makes.
    fn alert(props: Value) -> FeedAlert {
        let feature = json!({ "type": "Feature", "geometry": null, "properties": props });
        let Feature::Alert(a) = parse_feature(&feature, fixture_now()) else {
            panic!("{feature}")
        };
        *a
    }

    #[test]
    fn cancellations_are_ended() {
        let mut base = json!({
            "id": "urn:x:1", "status": "Actual", "event": "Tornado Warning",
            "messageType": "Update", "sent": "2026-11-06T12:00:00Z",
            "ends": "2026-11-06T13:00:00Z",
            "parameters": { "VTEC": ["/O.CAN.KOUN.TO.W.0042.000000T0000Z-261106T1300Z/"] }
        });
        assert!(alert(base.clone()).ended);
        base["messageType"] = json!("Cancel");
        base["parameters"] = json!({});
        assert!(alert(base).ended);
    }

    #[test]
    fn several_vtec_strings_are_about_the_live_event() {
        let with_vtec = |vtec: &[&str]| {
            alert(json!({
                "id": "urn:x:2", "status": "Actual", "event": "Winter Storm Warning",
                "messageType": "Alert", "sent": "2026-11-06T12:00:00Z", "ends": "2026-11-07T12:00:00Z",
                "parameters": { "VTEC": vtec }
            }))
        };
        let warning = "/O.NEW.KBOU.WS.W.0005.261106T1800Z-261107T1200Z/";
        let upgraded_watch = "/O.UPG.KBOU.WS.A.0003.261107T0000Z-261107T1200Z/";
        // An advisory expiring as the warning starts.
        let a = with_vtec(&["/O.EXP.KBOU.WW.Y.0010.000000T0000Z-261106T1800Z/", warning]);
        assert!(!a.ended, "the warning is in effect");
        assert_eq!(a.event_key, "KBOU.WS.W.0005.2026");
        assert!(!a.continues);

        // A watch upgraded to the warning.
        let a = with_vtec(&[upgraded_watch, warning]);
        assert!(!a.ended);
        assert_eq!(
            a.event_key, "KBOU.WS.W.0005.2026",
            "the warning's key, not the watch's"
        );
        assert_eq!(a.vtec_prefix.as_deref(), Some("KBOU.WS.W.0005."));

        // The watch's own segment, upgraded away: ended.
        assert!(with_vtec(&[upgraded_watch]).ended);
    }

    #[test]
    fn hostile_features_are_skipped_not_fatal() {
        let now = fixture_now();
        let doc = json!({ "features": [
            "not an object",
            { "properties": null },
            { "properties": { "status": "Actual" } },
            { "properties": { "status": "Actual", "id": 7, "event": "X" } },
            { "properties": { "status": "Actual", "id": "a", "event": "Tornado Warning",
                              "sent": "yesterday", "affectedZones": [1, null, "ftp://x/zones/a/b"],
                              "geocode": { "UGC": "TXZ001" } },
              "geometry": { "type": "Polygon", "coordinates": [[[1.0, 2.0], ["x", 3.0]]] } },
            { "properties": { "status": "Actual", "id": "b\u{0}c", "event": "Gale Warning" },
              "geometry": { "type": "Polygon", "coordinates": [[[0, 0], [1, 0], [1, 1]]] } }
        ]});
        let feed = parse_feed(doc.to_string().as_bytes(), now).unwrap();
        assert_eq!(feed.skipped, 4);
        assert_eq!(feed.alerts.len(), 2);
        assert!(feed.alerts[0].polygon.is_none());
        assert!(feed.alerts[0].ugc.is_empty());
        assert_eq!(feed.alerts[0].sent, None);
        // NUL removed; open ring closed.
        assert_eq!(feed.alerts[1].id, "bc");
        let p: Value = serde_json::from_str(feed.alerts[1].polygon.as_ref().unwrap()).unwrap();
        assert_eq!(p["coordinates"][0][0].as_array().unwrap().len(), 4);

        assert!(parse_feed(b"<html>", now).is_err());
        assert!(parse_feed(b"{\"type\":\"FeatureCollection\"}", now).is_err());
    }

    #[test]
    fn zone_refs_from_urls_and_codes() {
        let refs = zone_refs(
            &[
                "https://api.weather.gov/zones/forecast/TXZ123".into(),
                "https://api.weather.gov/zones/county/TXC201".into(),
                "https://api.weather.gov/zones/fire/TXZ123".into(),
                "https://api.weather.gov/zones/forecast/../../etc".into(),
                "https://api.weather.gov/zones/forecast/TXZ12".into(),
                "https://api.weather.gov/zones/forecast/TXZ124/extra".into(),
            ],
            &[
                "TXZ123".into(),
                "OKC027".into(),
                "GMZ555".into(),
                "bogus".into(),
            ],
        );
        let pairs: Vec<(&str, &str)> = refs
            .iter()
            .map(|z| (z.kind.as_str(), z.id.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("forecast", "TXZ123"),
                ("county", "TXC201"),
                ("county", "OKC027"),
                ("forecast", "GMZ555")
            ]
        );
    }

    #[test]
    fn parses_a_recorded_zone() {
        let req = ZoneRef {
            kind: "fire".into(),
            id: "CAZ369".into(),
        };
        let z = parse_zone(ZONE_369, &req).unwrap().unwrap();
        assert_eq!(z.kind, "fire");
        assert_eq!(z.state.as_deref(), Some("CA"));
        assert_eq!(
            z.name.as_deref(),
            Some("Western Santa Monica Mountains Recreational Area")
        );
        let g: Value = serde_json::from_str(&z.geometry).unwrap();
        assert_eq!(g["type"], "MultiPolygon");

        let wrong = ZoneRef {
            kind: "fire".into(),
            id: "CAZ370".into(),
        };
        assert!(parse_zone(ZONE_369, &wrong).is_err());
        let empty = json!({ "properties": { "id": "CAZ370" }, "geometry": null });
        assert!(
            parse_zone(empty.to_string().as_bytes(), &wrong)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn until_phrases() {
        let ends = Some("2026-11-06T18:45:00-06:00");
        assert_eq!(
            until_phrase(
                Some("Tornado Warning issued November 6 at 5:45PM CST until November 6 at 6:45PM CST by NWS Norman OK"),
                ends
            )
            .as_deref(),
            Some("until 6:45 PM CST")
        );
        assert_eq!(
            until_phrase(
                Some("Red Flag Warning issued November 5 at 1:36PM PST until November 7 at 6:00PM PST by NWS Los Angeles/Oxnard CA"),
                Some("2026-11-07T18:00:00-08:00")
            )
            .as_deref(),
            Some("until November 7 at 6:00 PM PST")
        );
        assert_eq!(
            until_phrase(
                Some("Flood Warning issued November 6 at 7:16AM EST by NWS Upton NY"),
                ends
            )
            .as_deref(),
            Some("until 6:45 PM UTC-6")
        );
        assert_eq!(
            until_phrase(None, Some("2026-11-06T18:45:00+05:30")).as_deref(),
            Some("until 6:45 PM UTC+5:30")
        );
        assert_eq!(
            until_phrase(Some("garbage until"), Some("not a time")),
            None
        );
        assert_eq!(
            // "€" is three bytes: the last two bytes start mid-character.
            until_phrase(
                Some("X issued May 1 at 1:00AM CST until May 1 at 2:00€ CST by NWS Y"),
                Some("bad")
            ),
            None,
            "non-ASCII clock text is rejected, not a panic"
        );
    }

    #[test]
    fn an_alert_without_an_end_is_until_further_notice() {
        // River flood warnings: no `ends`, no "until" in the headline. Their
        // `expires` is only when the next statement is due.
        let headline = "Flood Warning issued November 6 at 10:28AM CST by NWS Fort Worth TX";
        assert_eq!(
            until_phrase(Some(headline), None).as_deref(),
            Some("until further notice")
        );
        assert_eq!(
            until_phrase(None, None).as_deref(),
            Some("until further notice")
        );
    }
}
