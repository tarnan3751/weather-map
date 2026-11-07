//! Reading CAP 1.1 and 1.2 alert messages (OASIS Common Alerting Protocol).
//!
//! A message is third-party content: everything is checked and bounded
//! here, and a field that makes no sense is dropped rather than allowed to
//! fail the message. A document that is not a CAP alert at all, or lacks
//! the three fields that identify a message (`sender`, `identifier`,
//! `sent`), is refused.

use super::text::{line, plain, token, web_url};
use super::xml::{self, Element};
use chrono::{DateTime, NaiveDateTime, Utc};

const MAX_INFOS: usize = 16;
const MAX_AREAS: usize = 1000;
const MAX_SHAPES_PER_AREA: usize = 500;
const MAX_VERTICES_PER_POLYGON: usize = 100_000;
const MAX_VERTICES_PER_MESSAGE: usize = 1_000_000;
const MAX_GEOCODES_PER_AREA: usize = 2000;
const MAX_REFERENCES: usize = 500;
const MAX_CIRCLE_KM: f64 = 5000.0;

/// Longest identifier, sender or code kept. CAP sets no limit; real ones are
/// under a hundred characters.
const MAX_ID_LEN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MsgType {
    Alert,
    Update,
    Cancel,
    /// `Ack` and `Error`: about other messages, never shown.
    Other,
}

/// The message types shown, as CAP spells them.
const MSG_TYPES: [(MsgType, &str); 3] = [
    (MsgType::Alert, "Alert"),
    (MsgType::Update, "Update"),
    (MsgType::Cancel, "Cancel"),
];
const URGENCIES: &[&str] = &["Immediate", "Expected", "Future", "Past", "Unknown"];
/// Most severe first.
const SEVERITIES: &[&str] = &["Extreme", "Severe", "Moderate", "Minor", "Unknown"];
const CERTAINTIES: &[&str] = &[
    "Observed",
    "Likely",
    "Possible",
    "Unlikely",
    "Unknown",
    "Very Likely",
];

impl MsgType {
    pub fn as_str(self) -> &'static str {
        MSG_TYPES
            .iter()
            .find(|(t, _)| *t == self)
            .map_or("Other", |(_, s)| s)
    }

    fn parse(raw: &str) -> Self {
        MSG_TYPES
            .iter()
            .find(|(_, s)| s.eq_ignore_ascii_case(raw))
            .map_or(Self::Other, |(t, _)| *t)
    }
}

/// One entry of `<references>`: `sender,identifier,sent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reference {
    pub sender: String,
    pub identifier: String,
}

impl Reference {
    /// The key a message is known by: `sender,identifier`.
    pub fn key(&self) -> String {
        message_key(&self.sender, &self.identifier)
    }
}

fn message_key(sender: &str, identifier: &str) -> String {
    format!("{sender},{identifier}")
}

#[derive(Debug, Clone)]
pub(crate) struct Message {
    pub sender: String,
    pub identifier: String,
    pub sent: DateTime<Utc>,
    pub status: String,
    pub msg_type: MsgType,
    /// `Public`, `Restricted` or `Private`; absent counts as public.
    pub scope: Option<String>,
    pub references: Vec<Reference>,
    pub infos: Vec<Info>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Info {
    /// RFC 3066 tag as given; CAP's default is `en-US`.
    pub language: String,
    pub categories: Vec<String>,
    pub event: Option<String>,
    pub urgency: Option<String>,
    pub severity: Option<String>,
    pub certainty: Option<String>,
    pub effective: Option<DateTime<Utc>>,
    pub onset: Option<DateTime<Utc>>,
    pub expires: Option<DateTime<Utc>>,
    pub sender_name: Option<String>,
    pub headline: Option<String>,
    pub description: Option<String>,
    pub instruction: Option<String>,
    pub web: Option<String>,
    pub areas: Vec<Area>,
    /// The block says the hazard is over where it applies (see
    /// [`block_is_over`]).
    pub over: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Area {
    pub desc: Option<String>,
    /// Rings as `(lat, lon)` pairs, closed.
    pub polygons: Vec<Vec<(f64, f64)>>,
    pub circles: Vec<Circle>,
    /// `(valueName, value)`.
    pub geocodes: Vec<(String, String)>,
    /// Parts to cut out of the area (DWD's `EXCLUDE_POLYGON` geocode).
    pub excludes: Vec<Vec<(f64, f64)>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Circle {
    pub lat: f64,
    pub lon: f64,
    pub radius_km: f64,
}

impl Message {
    pub fn key(&self) -> String {
        message_key(&self.sender, &self.identifier)
    }

    /// The blocks in the language shown: English (CAP's default when none
    /// is given) if the message has it, else the language of its first
    /// block. A message often has several blocks in one language, one per
    /// area or time step, some of them saying the hazard is over there.
    pub fn in_language(&self) -> Vec<&Info> {
        let Some(first) = self
            .infos
            .iter()
            .find(|i| is_english(&i.language))
            .or_else(|| self.infos.first())
        else {
            return Vec::new();
        };
        let english = is_english(&first.language);
        self.infos
            .iter()
            .filter(|i| {
                if english {
                    is_english(&i.language)
                } else {
                    i.language.eq_ignore_ascii_case(&first.language)
                }
            })
            .collect()
    }

    /// The blocks in the language shown that are in effect. What is drawn,
    /// listed and notified is built from these.
    pub fn shown(&self) -> Vec<&Info> {
        self.in_language().into_iter().filter(|i| !i.over).collect()
    }

    /// Some block in the language shown is in effect. A message whose
    /// blocks all say the hazard is over ends it, like a Cancel.
    pub fn in_effect(&self) -> bool {
        !self.shown().is_empty()
    }

    /// The block whose text is shown: the first in effect, else the first in
    /// the language shown.
    pub fn primary(&self) -> Option<&Info> {
        self.shown()
            .first()
            .copied()
            .or_else(|| self.in_language().first().copied())
    }

    /// The most severe block in effect (the first among equals): its
    /// severity, urgency and certainty describe the message.
    pub fn most_severe(&self) -> Option<&Info> {
        // 4 for Extreme down to 0 for Unknown.
        let rank = |i: &Info| {
            SEVERITIES
                .iter()
                .rev()
                .position(|s| i.severity.as_deref() == Some(s))
                .unwrap_or(0)
        };
        let mut best: Option<&Info> = None;
        for i in self.shown() {
            if best.is_none_or(|b| rank(i) > rank(b)) {
                best = Some(i);
            }
        }
        best.or_else(|| self.primary())
    }

    /// Every area of every block in effect; for a message with none in
    /// effect, those of its primary block (kept for the record).
    pub fn areas(&self) -> Vec<&Area> {
        let shown = self.shown();
        let blocks = if shown.is_empty() {
            self.primary().into_iter().collect()
        } else {
            shown
        };
        blocks.into_iter().flat_map(|i| i.areas.iter()).collect()
    }

    /// When the message stops being in effect: the latest `expires` of its
    /// blocks in effect, else of any block.
    pub fn expires(&self) -> Option<DateTime<Utc>> {
        self.shown()
            .iter()
            .filter_map(|i| i.expires)
            .max()
            .or_else(|| self.infos.iter().filter_map(|i| i.expires).max())
    }

    /// When it starts: the earliest onset (else effective) of its blocks in
    /// effect, else the time it was sent.
    pub fn start(&self) -> DateTime<Utc> {
        self.shown()
            .iter()
            .filter_map(|i| i.onset.or(i.effective))
            .min()
            .unwrap_or(self.sent)
    }

    pub fn is_public(&self) -> bool {
        self.scope
            .as_deref()
            .is_none_or(|s| s.eq_ignore_ascii_case("public"))
    }
}

fn is_english(language: &str) -> bool {
    let l = language.trim().to_ascii_lowercase();
    l.is_empty() || l == "en" || l.starts_with("en-") || l.starts_with("en_")
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CapError {
    #[error(transparent)]
    Xml(#[from] xml::XmlError),
    #[error("not a CAP alert (root element <{0}>)")]
    NotCap(String),
    #[error("CAP message without {0}")]
    Missing(&'static str),
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Message, CapError> {
    let root = xml::parse(bytes)?;
    parse_element(&root)
}

/// A CAP `<alert>` element (also used for alerts embedded in other documents).
pub(crate) fn parse_element(root: &Element) -> Result<Message, CapError> {
    if !root.name.eq_ignore_ascii_case("alert") {
        return Err(CapError::NotCap(root.name.chars().take(40).collect()));
    }
    let id = |name, what| {
        root.child_text(name)
            .and_then(|s| token(s, MAX_ID_LEN))
            .ok_or(CapError::Missing(what))
    };
    let sender = id("sender", "a sender")?;
    let identifier = id("identifier", "an identifier")?;
    let sent = root
        .child_text("sent")
        .and_then(time)
        .ok_or(CapError::Missing("a sent time"))?;
    let status = root
        .child_text("status")
        .and_then(|s| token(s, 32))
        .unwrap_or_default();
    let msg_type = root
        .child_text("msgType")
        .map_or(MsgType::Other, MsgType::parse);
    let scope = root.child_text("scope").and_then(|s| token(s, 32));
    let references = root
        .child_text("references")
        .map(parse_references)
        .unwrap_or_default();

    let mut budget = MAX_VERTICES_PER_MESSAGE;
    let infos = root
        .children_named("info")
        .take(MAX_INFOS)
        .map(|i| parse_info(i, &mut budget))
        .collect();
    Ok(Message {
        sender,
        identifier,
        sent,
        status,
        msg_type,
        scope,
        references,
        infos,
    })
}

/// Space-separated `sender,identifier,sent` triples. Malformed entries are
/// skipped; a missing `sent` is tolerated.
fn parse_references(raw: &str) -> Vec<Reference> {
    let mut out: Vec<Reference> = Vec::new();
    for entry in raw.split_whitespace().take(MAX_REFERENCES) {
        let parts: Vec<&str> = entry.split(',').collect();
        let (sender, identifier) = match parts.len() {
            0 | 1 => continue,
            2 => (parts[0], parts[1]),
            // An identifier with commas in it breaks the format; the last
            // field is the time, the first the sender.
            n => (
                parts[0],
                &entry[parts[0].len() + 1..entry.len() - parts[n - 1].len() - 1],
            ),
        };
        let (Some(sender), Some(identifier)) =
            (token(sender, MAX_ID_LEN), token(identifier, MAX_ID_LEN))
        else {
            continue;
        };
        let r = Reference { sender, identifier };
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

fn parse_info(el: &Element, budget: &mut usize) -> Info {
    let text = |name: &str, max: usize| el.child_text(name).and_then(|s| plain(s, max));
    let one_line = |name: &str, max: usize| el.child_text(name).and_then(|s| line(s, max));
    let one_of = |name: &str, allowed| el.child_text(name).and_then(|s| enumerated(s, allowed));
    let at = |name: &str| el.child_text(name).and_then(time);
    let responses: Vec<&str> = el
        .children_named("responseType")
        .filter_map(Element::text)
        .collect();
    let ended_type = el.children_named("parameter").any(|p| {
        p.child_text("valueName")
            .is_some_and(|n| n.to_ascii_lowercase().ends_with("alert_type"))
            && p.child_text("value")
                .is_some_and(|v| v.eq_ignore_ascii_case("ended"))
    });
    let mut info = Info {
        language: el
            .child_text("language")
            .and_then(|s| token(s, 35))
            .unwrap_or_default(),
        categories: el
            .children_named("category")
            .filter_map(Element::text)
            .filter_map(|c| token(c, 16))
            .take(12)
            .collect(),
        event: one_line("event", 200),
        urgency: one_of("urgency", URGENCIES),
        severity: one_of("severity", SEVERITIES),
        certainty: one_of("certainty", CERTAINTIES),
        effective: at("effective"),
        onset: at("onset"),
        expires: at("expires"),
        sender_name: one_line("senderName", 200),
        headline: one_line("headline", 300),
        description: text("description", 20_000),
        instruction: text("instruction", 10_000),
        web: el.child_text("web").and_then(web_url),
        areas: el
            .children_named("area")
            .take(MAX_AREAS)
            .map(|a| parse_area(a, budget))
            .collect(),
        over: false,
    };
    info.over = block_is_over(&info, &responses, ended_type);
    info
}

/// A block that says the hazard is over where it applies, rather than
/// warning of it: urgency `Past`, an `AllClear` response (ECCC sends both
/// for an ended warning), an "ended" alert type (ECCC's own parameter), or
/// a cancellation headline (the Bureau of Meteorology cancels part of a
/// warning with a block headed "Cancellation of ...", which says so in no
/// other way).
fn block_is_over(info: &Info, responses: &[&str], ended_type: bool) -> bool {
    let cancelled = |s: &Option<String>| {
        s.as_deref().is_some_and(|t| {
            let t = t.trim_start().to_lowercase();
            t.starts_with("cancellation") || t.starts_with("cancelled") || t.starts_with("canceled")
        })
    };
    info.urgency.as_deref() == Some("Past")
        || responses.iter().any(|r| r.eq_ignore_ascii_case("AllClear"))
        || ended_type
        || cancelled(&info.headline)
        || cancelled(&info.event)
}

fn parse_area(el: &Element, budget: &mut usize) -> Area {
    let mut area = Area {
        desc: el.child_text("areaDesc").and_then(|s| line(s, 2000)),
        ..Area::default()
    };
    for p in el.children_named("polygon").take(MAX_SHAPES_PER_AREA) {
        if let Some(ring) = p.text().and_then(|t| parse_polygon(t, budget)) {
            area.polygons.push(ring);
        }
    }
    for c in el.children_named("circle").take(MAX_SHAPES_PER_AREA) {
        if let Some(circle) = c.text().and_then(parse_circle) {
            area.circles.push(circle);
        }
    }
    for g in el.children_named("geocode").take(MAX_GEOCODES_PER_AREA) {
        let (Some(name), Some(value)) = (g.child_text("valueName"), g.child_text("value")) else {
            continue;
        };
        if name.eq_ignore_ascii_case("EXCLUDE_POLYGON") {
            if let Some(ring) = parse_polygon(value, budget) {
                area.excludes.push(ring);
            }
            continue;
        }
        if let (Some(name), Some(value)) = (token(name, 64), token(value, 128)) {
            area.geocodes.push((name, value));
        }
    }
    area
}

/// `lat,lon lat,lon ...`, closed. Separators are read loosely (some feeds
/// use commas throughout, or commas followed by spaces). A ring is refused
/// when it has too few points, a coordinate out of range, or more vertices
/// than the budget left for the message. A ring written `lon,lat` (which
/// some senders do) is recognised when its first number cannot be a
/// latitude, and turned round.
fn parse_polygon(raw: &str, budget: &mut usize) -> Option<Vec<(f64, f64)>> {
    let mut numbers = Vec::new();
    for tok in raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        let v: f64 = tok.parse().ok()?;
        if !v.is_finite() {
            return None;
        }
        numbers.push(v);
        if numbers.len() > 2 * MAX_VERTICES_PER_POLYGON.min(*budget) + 2 {
            return None;
        }
    }
    if numbers.len() % 2 != 0 {
        return None;
    }
    let mut ring: Vec<(f64, f64)> = numbers
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[a, b]| (a, b))
        .collect();
    let fits = |r: &[(f64, f64)]| {
        r.iter()
            .all(|&(lat, lon)| lat.abs() <= 90.0 && lon.abs() <= 180.0)
    };
    if !fits(&ring) {
        let swapped: Vec<(f64, f64)> = ring.iter().map(|&(a, b)| (b, a)).collect();
        if !fits(&swapped) {
            return None;
        }
        ring = swapped;
    }
    ring.dedup();
    if ring.first() != ring.last() {
        ring.push(ring[0]);
    }
    let distinct = {
        let mut pts: Vec<(i64, i64)> = ring
            .iter()
            .map(|&(a, b)| ((a * 1e7).round() as i64, (b * 1e7).round() as i64))
            .collect();
        pts.sort_unstable();
        pts.dedup();
        pts.len()
    };
    if distinct < 3 || ring.len() > *budget {
        return None;
    }
    *budget -= ring.len();
    Some(ring)
}

/// `lat,lon radius` with the radius in kilometres. A zero radius (a point)
/// covers nothing and is dropped.
fn parse_circle(raw: &str) -> Option<Circle> {
    let nums: Vec<f64> = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [lat, lon, radius_km] = nums.as_slice() else {
        return None;
    };
    let (lat, lon, radius_km) = (*lat, *lon, *radius_km);
    let ok = lat.is_finite()
        && lon.is_finite()
        && radius_km.is_finite()
        && lat.abs() <= 90.0
        && lon.abs() <= 180.0
        && radius_km > 0.0
        && radius_km <= MAX_CIRCLE_KM;
    ok.then_some(Circle {
        lat,
        lon,
        radius_km,
    })
}

/// CAP times are RFC 3339 with an offset. Some senders leave the offset
/// out; those are read as UTC.
pub(crate) fn time(raw: &str) -> Option<DateTime<Utc>> {
    let s = raw.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    // "+0100" instead of "+01:00".
    if s.len() > 5 {
        let (head, tail) = s.split_at(s.len() - 5);
        if (tail.starts_with('+') || tail.starts_with('-'))
            && tail[1..].bytes().all(|b| b.is_ascii_digit())
        {
            let fixed = format!("{head}{}:{}", &tail[..3], &tail[3..]);
            if let Ok(t) = DateTime::parse_from_rfc3339(&fixed) {
                return Some(t.with_timezone(&Utc));
            }
        }
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(t.and_utc());
        }
    }
    None
}

/// One of CAP's enumerated values, in its canonical spelling.
fn enumerated(raw: &str, allowed: &[&str]) -> Option<String> {
    let raw = raw.trim();
    allowed
        .iter()
        .find(|a| a.eq_ignore_ascii_case(raw))
        .map(|a| a.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <alert xmlns="urn:oasis:names:tc:emergency:cap:1.2">
              <identifier>id-1</identifier><sender>met@example.org</sender>
              <sent>2026-11-06T10:00:00+01:00</sent><status>Actual</status>
              <msgType>Alert</msgType><scope>Public</scope>{body}
            </alert>"#
        )
    }

    #[test]
    fn identifies_the_message() {
        let m = parse(cap("").as_bytes()).unwrap();
        assert_eq!(m.key(), "met@example.org,id-1");
        assert_eq!(m.sent.to_rfc3339(), "2026-11-06T09:00:00+00:00");
        assert_eq!(m.msg_type, MsgType::Alert);
        assert!(m.is_public());
        assert!(m.primary().is_none());

        assert!(matches!(parse(b"<rss/>"), Err(CapError::NotCap(_))));
        assert!(matches!(
            parse(b"<alert><sender>a</sender><sent>2026-11-06T10:00:00Z</sent></alert>"),
            Err(CapError::Missing("an identifier"))
        ));
        assert!(matches!(
            parse(b"<alert><sender>a</sender><identifier>b</identifier><sent>soon</sent></alert>"),
            Err(CapError::Missing("a sent time"))
        ));
    }

    #[test]
    fn primary_info_is_english_or_the_first() {
        let m = parse(
            cap("<info><language>fr-CA</language><event>vent</event></info>
                 <info><language>en-CA</language><event>wind</event></info>")
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(m.primary().unwrap().event.as_deref(), Some("wind"));
        let m = parse(cap("<info><language>hr</language><event>vjetar</event></info>").as_bytes())
            .unwrap();
        assert_eq!(m.primary().unwrap().event.as_deref(), Some("vjetar"));
        // No language: CAP's default, en-US.
        let m = parse(
            cap("<info><language>es</language><event>viento</event></info><info><event>wind</event></info>").as_bytes(),
        )
        .unwrap();
        assert_eq!(m.primary().unwrap().event.as_deref(), Some("wind"));
    }

    #[test]
    fn references_are_sender_identifier_pairs() {
        let refs = parse_references(
            "a@x,ID-1,2026-11-05T12:00:00+00:00  a@x,ID-2,2026-11-05T13:00:00+00:00 broken a@x,ID-1,2026-11-05T12:00:00Z b,c",
        );
        let keys: Vec<String> = refs.iter().map(Reference::key).collect();
        assert_eq!(keys, ["a@x,ID-1", "a@x,ID-2", "b,c"]);
        assert!(parse_references("").is_empty());
    }

    #[test]
    fn polygons_are_checked_and_closed() {
        let mut budget = 1000;
        let ring = parse_polygon("10,20 10,21 11,21", &mut budget).unwrap();
        assert_eq!(
            ring,
            [(10.0, 20.0), (10.0, 21.0), (11.0, 21.0), (10.0, 20.0)]
        );
        assert_eq!(budget, 996);
        assert_eq!(
            parse_polygon("10, 20, 10, 21, 11, 21, 10, 20", &mut budget)
                .unwrap()
                .len(),
            4
        );
        // Written lon,lat: the first number cannot be a latitude.
        let swapped = parse_polygon("120,10 121,10 121,11 120,10", &mut budget).unwrap();
        assert_eq!(swapped[0], (10.0, 120.0));
        for bad in [
            "",
            "10,20",
            "10,20 10,21",
            "10,20 10,21 x,y",
            "10,20 10,21 11",
            "95,200 96,200 97,201",
            "NaN,1 2,3 4,5",
        ] {
            assert!(parse_polygon(bad, &mut 1000).is_none(), "{bad:?}");
        }
        // Repeated points are not enough for an area.
        assert!(parse_polygon("1,1 1,1 1,1 1,1", &mut 1000).is_none());
        // The per-message budget bounds the work.
        let big: String = (0..2000)
            .map(|i| format!("{},{} ", (i % 90) as f64 * 0.5, i as f64 * 0.001))
            .collect();
        let mut small = 100;
        assert!(parse_polygon(&big, &mut small).is_none());
    }

    #[test]
    fn circles() {
        assert_eq!(
            parse_circle("-19.0487,-169.8619 14"),
            Some(Circle {
                lat: -19.0487,
                lon: -169.8619,
                radius_km: 14.0
            })
        );
        assert!(parse_circle("1,2 0").is_none(), "a point is not an area");
        assert!(parse_circle("1,2 -5").is_none());
        assert!(parse_circle("91,2 5").is_none());
        assert!(parse_circle("1,2 50000").is_none());
        assert!(parse_circle("1,2").is_none());
    }

    #[test]
    fn times() {
        assert!(time("2026-11-06T10:00:00-00:00").is_some());
        assert_eq!(
            time("2026-11-06T10:00:00+0100"),
            time("2026-11-06T09:00:00Z")
        );
        assert_eq!(time("2026-11-06T10:00:00"), time("2026-11-06T10:00:00Z"));
        assert!(time("tomorrow").is_none());
    }

    #[test]
    fn hostile_fields_are_bounded_or_dropped() {
        let long = "x".repeat(50_000);
        let m = parse(
            cap(&format!(
                "<info><language>en</language><category>Met</category>
                 <event>{long}</event><severity>EXTREME</severity><urgency>soon</urgency>
                 <headline>&lt;b&gt;Bold&lt;/b&gt; headline</headline>
                 <description>{long}</description><web>javascript:alert(1)</web>
                 <area><areaDesc>A</areaDesc><polygon>1,1 1,2 2,2 1,1</polygon><polygon>bad</polygon>
                   <circle>1,1 0</circle><geocode><valueName>NUTS3</valueName><value>BG332</value></geocode>
                   <geocode><valueName>EXCLUDE_POLYGON</valueName><value>1.2,1.2 1.2,1.3 1.3,1.3 1.2,1.2</value></geocode>
                 </area></info>"
            ))
            .as_bytes(),
        )
        .unwrap();
        let i = m.primary().unwrap();
        assert_eq!(i.event.as_ref().unwrap().chars().count(), 200);
        assert_eq!(i.severity.as_deref(), Some("Extreme"));
        assert_eq!(i.urgency, None);
        assert_eq!(i.headline.as_deref(), Some("Bold headline"));
        assert_eq!(i.description.as_ref().unwrap().chars().count(), 20_000);
        assert_eq!(i.web, None);
        let a = &i.areas[0];
        assert_eq!(
            (a.polygons.len(), a.circles.len(), a.excludes.len()),
            (1, 0, 1)
        );
        assert_eq!(a.geocodes, [("NUTS3".to_string(), "BG332".to_string())]);
    }

    fn fixture(name: &str) -> Message {
        let path = format!("{}/tests/fixtures/world/{name}", env!("CARGO_MANIFEST_DIR"));
        parse(&std::fs::read(&path).unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    #[test]
    fn real_messages_from_several_countries() {
        // Canada: bilingual, ten areas with a polygon each, a cumulative
        // reference chain.
        let m = fixture("ca_cwhx_update_2.cap");
        assert_eq!(m.msg_type, MsgType::Update);
        assert_eq!(m.sender, "cap-pac@canada.ca");
        let keys: Vec<String> = m.references.iter().map(Reference::key).collect();
        assert_eq!(
            keys,
            [
                "cap-pac@canada.ca,urn:oid:2.49.0.1.124.4123941209.2026",
                "cap-pac@canada.ca,urn:oid:2.49.0.1.124.0381057386.2026"
            ]
        );
        assert_eq!(m.infos.len(), 2);
        assert_eq!(m.primary().unwrap().language, "en-CA");
        assert_eq!(m.areas().len(), 10);
        assert!(m.areas().iter().all(|a| a.polygons.len() == 1));

        // Niue: a circle, 14 km.
        let m = fixture("nu_drought_circle.xml");
        let c = m.areas()[0].circles[0];
        assert_eq!((c.lat, c.lon, c.radius_km), (-19.0487, -169.8619, 14.0));
        assert_eq!(m.primary().unwrap().severity.as_deref(), Some("Severe"));

        // MeteoAlarm: region codes only, the English block is the primary.
        let m = fixture("hr_meteoalarm_nuts.xml");
        let p = m.primary().unwrap();
        assert_eq!(p.language, "en-GB");
        assert_eq!(p.event.as_deref(), Some("Yellow wind warning"));
        assert!(m.areas()[0].polygons.is_empty());
        assert!(
            m.areas()[0]
                .geocodes
                .contains(&("NUTS3".to_string(), "HR033".to_string()))
        );
        let m = fixture("bg_meteoalarm_nuts.xml");
        assert_eq!(m.primary().unwrap().language, "en-GB");

        // Germany: hundreds of named areas, one polygon, and parts cut out.
        let m = fixture("de_dwd_exclude.xml");
        let areas = m.areas();
        assert_eq!(areas.len(), 317);
        assert_eq!(areas.iter().map(|a| a.polygons.len()).sum::<usize>(), 1);
        assert!(areas.iter().any(|a| !a.excludes.is_empty()));

        // Algeria: French, English and Arabic.
        let m = fixture("dz_three_languages.xml");
        let langs: Vec<&str> = m.infos.iter().map(|i| i.language.as_str()).collect();
        assert_eq!(langs, ["fr-FR", "en-US", "ar-AR"]);
        assert_eq!(m.primary().unwrap().event.as_deref(), Some("Rain"));

        // Macao lists the message itself among its references.
        let m = fixture("mo_self_reference_past.xml");
        assert_eq!(m.references[0].key(), m.key());
        assert_eq!(m.primary().unwrap().language, "en-US");
        assert_eq!(m.primary().unwrap().urgency.as_deref(), Some("Past"));

        // Argentina (Spanish only) and Sri Lanka (CAP 1.1).
        let m = fixture("ar_spanish_update.xml");
        assert_eq!(m.primary().unwrap().language, "es-AR");
        assert_eq!(m.references.len(), 1);
        let m = fixture("lk_cap11.xml");
        assert_eq!(m.msg_type, MsgType::Alert);
        assert_eq!(m.areas()[0].polygons.len(), 1);

        // Italy: an update naming EMMA regions only.
        let m = fixture("it_meteoalarm_emma_update.xml");
        assert!(
            m.areas()
                .iter()
                .all(|a| a.polygons.is_empty() && a.circles.is_empty())
        );
        assert!(m.areas()[0].geocodes.iter().any(|(n, _)| n == "EMMA_ID"));
    }
}
