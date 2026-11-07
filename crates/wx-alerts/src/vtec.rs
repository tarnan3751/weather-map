//! P-VTEC strings (NWS Directive 10-1703), e.g.
//! `/O.NEW.KOUN.TO.W.0042.261106T2300Z-261106T2345Z/`.

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Vtec {
    /// `O` operational, `T` test, `E`/`X` experimental.
    pub class: char,
    /// NEW, CON, EXT, EXA, EXB, UPG, CAN, EXP, COR, ROU.
    pub action: String,
    pub office: String,
    pub phenomenon: String,
    pub significance: String,
    /// Event tracking number, as written (four digits).
    pub etn: String,
    /// `None` for `000000T0000Z`: already in effect, or until further notice.
    pub begin: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

impl Vtec {
    pub fn parse(s: &str) -> Option<Self> {
        let body = s.trim().strip_prefix('/')?.strip_suffix('/')?;
        let parts: Vec<&str> = body.split('.').collect();
        let [class, action, office, phen, sig, etn, times] = parts.as_slice() else {
            return None;
        };
        let mut class_chars = class.chars();
        let class = class_chars.next().filter(|c| "OTEX".contains(*c))?;
        if class_chars.next().is_some() {
            return None;
        }
        let upper =
            |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_uppercase());
        let upper_alnum = |s: &str, len: usize| {
            s.len() == len
                && s.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        };
        if !upper(action, 3)
            || !upper_alnum(office, 4)
            || !upper_alnum(phen, 2)
            || !upper(sig, 1)
            || !(etn.len() == 4 && etn.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        let (begin, end) = times.split_once('-')?;
        Some(Self {
            class,
            action: action.to_string(),
            office: office.to_string(),
            phenomenon: phen.to_string(),
            significance: sig.to_string(),
            etn: etn.to_string(),
            begin: vtec_time(begin)?,
            end: vtec_time(end)?,
        })
    }

    /// `CAN` and `EXP` announce that the hazard is over; `UPG` that it is
    /// replaced by a more serious one (a watch upgraded to a warning).
    pub fn is_ending(&self) -> bool {
        matches!(self.action.as_str(), "CAN" | "EXP" | "UPG")
    }

    /// Everything but `NEW` continues an event issued earlier.
    pub fn continues(&self) -> bool {
        self.action != "NEW"
    }

    /// The part of the event key that names the event, without the year.
    pub fn key_prefix(&self) -> String {
        format!(
            "{}.{}.{}.{}.",
            self.office, self.phenomenon, self.significance, self.etn
        )
    }

    /// The key that ties every message about one hazard together.
    ///
    /// Event numbers restart every January 1 (UTC), so the key carries the
    /// year the event was first issued. Continuations write `000000T0000Z`
    /// as their begin time, so the year comes from `issued`: the earliest
    /// send time known for the message chain (the message itself and the
    /// messages it references), which stays put when an event issued on
    /// December 31 is continued in January.
    pub fn event_key(&self, issued: Option<DateTime<Utc>>) -> String {
        let year = issued
            .or(self.begin)
            .or(self.end)
            .map(|t| t.year().to_string())
            .unwrap_or_else(|| "0000".to_string());
        format!("{}{year}", self.key_prefix())
    }
}

/// `Ok(None)` for the all-zero "no time" marker, `None` when malformed.
fn vtec_time(s: &str) -> Option<Option<DateTime<Utc>>> {
    if s == "000000T0000Z" {
        return Some(None);
    }
    let b = s.as_bytes();
    if b.len() != 12 || b[6] != b'T' || b[11] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> { s.get(r)?.parse().ok() };
    let date = NaiveDate::from_ymd_opt(2000 + num(0..2)? as i32, num(2..4)?, num(4..6)?)?;
    let time = NaiveTime::from_hms_opt(num(7..9)?, num(9..11)?, 0)?;
    Some(Some(NaiveDateTime::new(date, time).and_utc()))
}

/// Every well-formed VTEC string of an alert, in order.
pub(crate) fn parse_all(strings: &[String]) -> Vec<Vtec> {
    strings.iter().filter_map(|s| Vtec::parse(s)).collect()
}

/// The event an alert is about. With `[EXP advisory, NEW warning]` or
/// `[UPG watch, NEW warning]` that is the warning: the first event that is
/// not ending. A pure cancellation is about the event it cancels.
pub(crate) fn primary(vtecs: &[Vtec]) -> Option<&Vtec> {
    vtecs
        .iter()
        .find(|v| !v.is_ending())
        .or_else(|| vtecs.first())
}

/// Ended only when every event it names is ending.
pub(crate) fn all_ending(vtecs: &[Vtec]) -> bool {
    !vtecs.is_empty() && vtecs.iter().all(Vtec::is_ending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn parses_the_documented_example() {
        let v = Vtec::parse("/O.NEW.KOUN.TO.W.0042.261106T2300Z-261106T2345Z/").unwrap();
        assert_eq!(v.class, 'O');
        assert_eq!(v.action, "NEW");
        assert_eq!(v.office, "KOUN");
        assert_eq!(v.phenomenon, "TO");
        assert_eq!(v.significance, "W");
        assert_eq!(v.etn, "0042");
        assert_eq!(v.begin, Some(utc(2026, 11, 6, 23, 0)));
        assert_eq!(v.end, Some(utc(2026, 11, 6, 23, 45)));
        assert!(!v.is_ending());
    }

    #[test]
    fn zero_times_mean_unknown() {
        let v = Vtec::parse("/O.CON.KMLB.FL.W.0006.000000T0000Z-000000T0000Z/").unwrap();
        assert_eq!(v.begin, None);
        assert_eq!(v.end, None);
    }

    #[test]
    fn rejects_malformed_strings() {
        for s in [
            "",
            "/O.NEW.KOUN.TO.W.0042/",
            "O.NEW.KOUN.TO.W.0042.261106T2300Z-261106T2345Z",
            "/Q.NEW.KOUN.TO.W.0042.261106T2300Z-261106T2345Z/",
            "/O.new.KOUN.TO.W.0042.261106T2300Z-261106T2345Z/",
            "/O.NEW.KOUN.TO.W.42.261106T2300Z-261106T2345Z/",
            "/O.NEW.KOUN.TO.W.0042.261306T2300Z-261106T2345Z/",
            "/O.NEW.KOUN.TO.W.0042.261106T2300Z/",
            "/O.NEW.KOUN.TO.W.0042.261106T2300Z-261106T2345Z.EXTRA/",
            // H-VTEC lines are not P-VTEC.
            "/00000.N.ER.000000T0000Z.000000T0000Z.000000T0000Z.OO/",
        ] {
            assert_eq!(Vtec::parse(s), None, "{s:?}");
        }
    }

    #[test]
    fn continuations_keep_the_key() {
        let issued = utc(2026, 11, 6, 22, 58);
        let new = Vtec::parse("/O.NEW.KOUN.TO.W.0042.261106T2300Z-261106T2345Z/").unwrap();
        let con = Vtec::parse("/O.CON.KOUN.TO.W.0042.000000T0000Z-261106T2345Z/").unwrap();
        let ext = Vtec::parse("/O.EXT.KOUN.TO.W.0042.000000T0000Z-261107T0015Z/").unwrap();
        let can = Vtec::parse("/O.CAN.KOUN.TO.W.0042.000000T0000Z-261106T2345Z/").unwrap();
        let key = new.event_key(Some(issued));
        assert_eq!(key, "KOUN.TO.W.0042.2026");
        // The continuations were sent later but reference the original.
        assert_eq!(con.event_key(Some(issued)), key);
        assert_eq!(ext.event_key(Some(issued)), key);
        assert_eq!(can.event_key(Some(issued)), key);
        assert!(can.is_ending());
    }

    #[test]
    fn year_comes_from_first_issue_not_from_the_continuation() {
        // Issued on New Year's Eve, continued on January 1: the event
        // number belongs to the old year.
        let issued = utc(2026, 12, 31, 23, 50);
        let new = Vtec::parse("/O.NEW.KOUN.TO.W.0123.261231T2350Z-270101T0030Z/").unwrap();
        let con = Vtec::parse("/O.CON.KOUN.TO.W.0123.000000T0000Z-270101T0030Z/").unwrap();
        assert_eq!(new.event_key(Some(issued)), "KOUN.TO.W.0123.2026");
        assert_eq!(con.event_key(Some(issued)), "KOUN.TO.W.0123.2026");
    }

    #[test]
    fn different_events_get_different_keys() {
        let t = Some(utc(2026, 11, 6, 22, 0));
        let a = Vtec::parse("/O.NEW.KOUN.TO.W.0042.261106T2300Z-261106T2345Z/").unwrap();
        let b = Vtec::parse("/O.NEW.KOUN.TO.W.0043.261106T2300Z-261106T2345Z/").unwrap();
        let c = Vtec::parse("/O.NEW.KTSA.TO.W.0042.261106T2300Z-261106T2345Z/").unwrap();
        let d = Vtec::parse("/O.NEW.KOUN.SV.W.0042.261106T2300Z-261106T2345Z/").unwrap();
        let keys = [
            a.event_key(t),
            b.event_key(t),
            c.event_key(t),
            d.event_key(t),
        ];
        for i in 0..keys.len() {
            for j in i + 1..keys.len() {
                assert_ne!(keys[i], keys[j]);
            }
        }
        // Same number, next year: a different event.
        assert_ne!(a.event_key(t), a.event_key(Some(utc(2027, 3, 1, 0, 0))));
    }

    #[test]
    fn year_falls_back_to_vtec_times() {
        let v = Vtec::parse("/O.NEW.KOUN.TO.W.0042.261106T2300Z-261106T2345Z/").unwrap();
        assert_eq!(v.event_key(None), "KOUN.TO.W.0042.2026");
        let v = Vtec::parse("/O.CON.KMLB.FL.W.0006.000000T0000Z-000000T0000Z/").unwrap();
        assert_eq!(v.event_key(None), "KMLB.FL.W.0006.0000");
    }

    #[test]
    fn parse_all_skips_garbage() {
        let all = parse_all(&[
            "nonsense".to_string(),
            "/O.EXT.KPSR.FA.W.0014.000000T0000Z-261106T2100Z/".to_string(),
        ]);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].etn, "0014");
        assert!(parse_all(&[]).is_empty());
        assert!(primary(&[]).is_none());
        assert!(!all_ending(&[]));
    }

    fn many(strings: &[&str]) -> Vec<Vtec> {
        parse_all(&strings.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn the_event_an_alert_is_about_is_its_first_live_one() {
        // An advisory expiring as a warning starts: about the warning.
        let v = many(&[
            "/O.EXP.KBOU.WW.Y.0010.000000T0000Z-261106T1800Z/",
            "/O.NEW.KBOU.WS.W.0005.261106T1800Z-261107T1200Z/",
        ]);
        assert_eq!(primary(&v).unwrap().event_key(None), "KBOU.WS.W.0005.2026");
        assert!(!all_ending(&v), "the warning is live");

        // A watch upgraded to a warning: the warning's key, not the watch's.
        let v = many(&[
            "/O.UPG.KBOU.WS.A.0003.261107T0000Z-261107T1200Z/",
            "/O.NEW.KBOU.WS.W.0005.261106T1800Z-261107T1200Z/",
        ]);
        assert_eq!(primary(&v).unwrap().key_prefix(), "KBOU.WS.W.0005.");
        assert!(!all_ending(&v));

        // Only ending events: ended, and about the first of them.
        for ending in [
            &["/O.UPG.KBOU.WS.A.0003.261107T0000Z-261107T1200Z/"][..],
            &[
                "/O.CAN.KOUN.TO.W.0042.000000T0000Z-261106T2345Z/",
                "/O.EXP.KOUN.SV.W.0100.000000T0000Z-261106T2345Z/",
            ][..],
        ] {
            let v = many(ending);
            assert!(all_ending(&v));
            assert_eq!(primary(&v), v.first());
        }
        let con = many(&["/O.CON.KOUN.TO.W.0042.000000T0000Z-261106T2345Z/"]);
        assert!(primary(&con).unwrap().continues());
        assert!(!many(&["/O.NEW.KOUN.TO.W.0042.261106T2300Z-261106T2345Z/"])[0].continues());
    }
}
