//! Inventories of GRIB2 files: where each message is, and what it holds.
//!
//! NOAA's `.idx` files and the wgrib2-style `.index` and `.inv` files other
//! providers publish (DMI, CPTEC) have one line per message,
//! `n:byte_offset:d=YYYYMMDDHH:VAR:level:forecast:[more]`, and parse with
//! [`parse`]; their `VAR` and `level` are what those families' [`FieldSel`]s
//! hold. Other index formats (ECMWF's JSON lines) are read by their family
//! into the same [`IdxEntry`] values, `var` and `level` holding whatever the
//! family's selectors name. Either way an inventory lets a family fetch just
//! the messages a layer needs out of a 100+ MB model file.

use wx_core::layer::{FieldSel, Stat};

/// The forecast column of an idx line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdxTime {
    /// `anl`, `6 hour fcst`, `45 min fcst`: the value this many minutes after
    /// the reference time.
    At { minutes: u32 },
    /// `0-6 hour acc fcst`: accumulated between two offsets, in minutes.
    Acc { from: u32, to: u32 },
    /// `2-3 hour max fcst`: the maximum between two offsets.
    Max { from: u32, to: u32 },
    /// Averages, minima and anything else no layer asks for.
    Other,
}

impl IdxTime {
    pub fn parse(column: &str) -> IdxTime {
        let mut it = column.split_whitespace();
        let words: [Option<&str>; 5] = std::array::from_fn(|_| it.next());
        let parsed = match words {
            [Some("anl"), None, ..] => Some(IdxTime::At { minutes: 0 }),
            [Some(n), Some(unit), Some("fcst"), None, _] => {
                scaled(n, unit).map(|minutes| IdxTime::At { minutes })
            }
            [
                Some(span),
                Some(unit),
                Some(op @ ("acc" | "max")),
                Some("fcst"),
                None,
            ] => span.split_once('-').and_then(|(a, b)| {
                let (from, to) = (scaled(a, unit)?, scaled(b, unit)?);
                Some(match op {
                    "acc" => IdxTime::Acc { from, to },
                    _ => IdxTime::Max { from, to },
                })
            }),
            _ => None,
        };
        parsed.unwrap_or(IdxTime::Other)
    }
}

fn scaled(n: &str, unit: &str) -> Option<u32> {
    let per = match unit {
        "min" => 1,
        "hour" => 60,
        // HRRR labels the empty f00 accumulation `0-0 day acc fcst`.
        "day" => 24 * 60,
        _ => return None,
    };
    n.parse::<u32>().ok()?.checked_mul(per)
}

/// A byte range of a remote file. `end` is exclusive; `None` runs to the end
/// of the file (the last message of an inventory).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteRange {
    pub start: u64,
    pub end: Option<u64>,
}

impl ByteRange {
    /// Bytes `start..end`.
    pub const fn new(start: u64, end: u64) -> Self {
        Self {
            start,
            end: Some(end),
        }
    }

    /// `len` bytes from `offset`, as JSON indexes give them.
    pub const fn at(offset: u64, len: u64) -> Self {
        Self {
            start: offset,
            end: Some(offset.saturating_add(len)),
        }
    }

    /// From `start` to the end of the file.
    #[cfg(test)]
    pub const fn tail(start: u64) -> Self {
        Self { start, end: None }
    }

    /// Length in bytes, when the end is known.
    pub fn size(&self) -> Option<u64> {
        self.end.map(|e| e.saturating_sub(self.start))
    }

    /// Fill in an open end once the file size is known.
    pub fn bounded(self, file_size: Option<u64>) -> ByteRange {
        match (self.end, file_size) {
            (None, Some(size)) if size > self.start => ByteRange {
                start: self.start,
                end: Some(size),
            },
            _ => self,
        }
    }

    fn contains(&self, other: &ByteRange) -> bool {
        other.start >= self.start
            && match (self.end, other.end) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(a), Some(b)) => b <= a,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdxEntry {
    pub range: ByteRange,
    pub var: String,
    pub level: String,
    pub time: IdxTime,
}

impl IdxEntry {
    /// Whether this message is the field `sel` at `minutes` after the
    /// reference time. Variable and level must match exactly: `TCDC:500 mb`
    /// is not `TCDC:entire atmosphere`, and a `ave fcst` line is never an
    /// instantaneous value.
    ///
    /// Levels are compared whole, never by prefix: `UGRD:10 mb` (a pressure
    /// level) is not `UGRD:10 m above ground`. For the statistics:
    ///
    /// * [`Stat::Instant`]: `anl`, `N hour fcst`, `N min fcst` at `minutes`.
    /// * [`Stat::RunTotal`]: an accumulation from 0 to `minutes`.
    /// * [`Stat::StepPerHour`]: an accumulation over the hour ending at
    ///   `minutes` (NBM's `5-6 hour acc fcst`), the registry's ingest step.
    ///   Families that difference run totals never ask for it: the driver
    ///   asks them for the totals instead.
    /// * [`Stat::StepMax`]: a maximum over any interval ending at `minutes`.
    pub fn matches(&self, sel: &FieldSel, minutes: u32) -> bool {
        self.var == sel.var && self.level == sel.level && self.time_matches(sel.stat, minutes)
    }

    #[allow(unreachable_patterns)]
    fn time_matches(&self, stat: Stat, minutes: u32) -> bool {
        match (stat, self.time) {
            (Stat::Instant, IdxTime::At { minutes: m }) => m == minutes,
            // GFS files hold two `APCP:surface` lines from f009 on, `6-9 hour
            // acc` and `0-9 hour acc`: only the one from the start matches.
            (Stat::RunTotal, IdxTime::Acc { from, to }) => from == 0 && to == minutes,
            (Stat::StepPerHour, IdxTime::Acc { from, to }) => to == minutes && from + 60 == to,
            (Stat::StepMax, IdxTime::Max { to, .. }) => to == minutes,
            _ => false,
        }
    }
}

/// Parse an inventory. Lines that do not look like inventory lines are
/// skipped rather than failing the whole file.
pub fn parse(text: &str) -> Vec<IdxEntry> {
    parse_where(text, |_| true)
}

/// [`parse`], keeping only the messages with a line for which `keep` holds
/// of what follows the forecast column (empty on most lines; NBM's
/// `ens std dev`, `prob >0.254`). Message ends are those of the whole
/// inventory, so a message never stretches over the ones left out.
pub fn parse_where(text: &str, keep: impl Fn(&str) -> bool) -> Vec<IdxEntry> {
    let mut entries = Vec::new();
    let mut kept: Vec<u64> = Vec::new();
    for line in text.lines() {
        let mut parts = line.trim().splitn(7, ':');
        let (Some(_), Some(start), Some(_), Some(var), Some(level), Some(time)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        let Ok(start) = start.trim().parse::<u64>() else {
            continue;
        };
        if keep(parts.next().unwrap_or("")) {
            kept.push(start);
        }
        entries.push(IdxEntry {
            range: ByteRange { start, end: None },
            var: var.trim().to_string(),
            level: level.trim().to_string(),
            time: IdxTime::parse(time),
        });
    }
    // A message ends where the next one starts. Lines numbered `n.1`, `n.2`
    // share one message and one offset, so look for the next larger offset
    // rather than the next line.
    let mut offsets: Vec<u64> = entries.iter().map(|e| e.range.start).collect();
    offsets.sort_unstable();
    offsets.dedup();
    for e in &mut entries {
        let next = offsets.partition_point(|&o| o <= e.range.start);
        e.range.end = offsets.get(next).copied();
    }
    if kept.len() < entries.len() {
        kept.sort_unstable();
        entries.retain(|e| kept.binary_search(&e.range.start).is_ok());
    }
    entries
}

/// The message for `sel` at `minutes` after the reference time. Where
/// several lines match (GFS repeats its first six hours' accumulations), the
/// first is taken: they describe the same field.
pub fn find<'a>(entries: &'a [IdxEntry], sel: &FieldSel, minutes: u32) -> Option<&'a IdxEntry> {
    entries.iter().find(|e| e.matches(sel, minutes))
}

/// Merge ranges that overlap or sit within `max_gap` bytes of each other, so
/// neighbouring messages cost one request. Each byte is requested once.
pub fn coalesce(ranges: &[ByteRange], max_gap: u64) -> Vec<ByteRange> {
    let mut sorted = ranges.to_vec();
    sorted.sort();
    sorted.dedup();
    let mut out: Vec<ByteRange> = Vec::new();
    for r in sorted {
        if let Some(last) = out.last_mut() {
            match last.end {
                None => continue,
                Some(end) if r.start <= end.saturating_add(max_gap) => {
                    last.end = r.end.map(|e| e.max(end));
                    continue;
                }
                Some(_) => {}
            }
        }
        out.push(r);
    }
    out
}

/// Cut `want` out of ranges fetched by [`coalesce`]. `None` when no fetched
/// range holds it or the response was shorter than the inventory promised.
pub fn slice(fetched: &[(ByteRange, bytes::Bytes)], want: ByteRange) -> Option<bytes::Bytes> {
    let (range, data) = fetched.iter().find(|(r, _)| r.contains(&want))?;
    let from = usize::try_from(want.start - range.start).ok()?;
    let to = match want.end {
        Some(end) => usize::try_from(end - range.start).ok()?,
        None => data.len(),
    };
    (from <= to && to <= data.len()).then(|| data.slice(from..to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wx_core::layer::FieldSel;

    fn fixture(name: &str) -> Vec<IdxEntry> {
        let path = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        parse(&text)
    }

    const fn inst(var: &'static str, level: &'static str) -> FieldSel {
        FieldSel {
            var,
            level,
            stat: Stat::Instant,
        }
    }

    fn range(start: u64, end: u64) -> ByteRange {
        ByteRange {
            start,
            end: Some(end),
        }
    }

    #[test]
    fn forecast_column() {
        assert_eq!(IdxTime::parse("anl"), IdxTime::At { minutes: 0 });
        assert_eq!(IdxTime::parse("6 hour fcst"), IdxTime::At { minutes: 360 });
        assert_eq!(IdxTime::parse("45 min fcst"), IdxTime::At { minutes: 45 });
        assert_eq!(
            IdxTime::parse("1035 min fcst"),
            IdxTime::At { minutes: 1035 }
        );
        assert_eq!(
            IdxTime::parse("0-6 hour acc fcst"),
            IdxTime::Acc { from: 0, to: 360 }
        );
        assert_eq!(
            IdxTime::parse("5-6 hour acc fcst"),
            IdxTime::Acc { from: 300, to: 360 }
        );
        assert_eq!(
            IdxTime::parse("0-0 day acc fcst"),
            IdxTime::Acc { from: 0, to: 0 }
        );
        assert_eq!(IdxTime::parse("18-24 hour ave fcst"), IdxTime::Other);
        assert_eq!(IdxTime::parse("10-15 min ave fcst"), IdxTime::Other);
        assert_eq!(
            IdxTime::parse("1-2 hour max fcst"),
            IdxTime::Max { from: 60, to: 120 }
        );
        assert_eq!(IdxTime::parse("1-2 hour min fcst"), IdxTime::Other);
        assert_eq!(IdxTime::parse(""), IdxTime::Other);
        assert_eq!(IdxTime::parse("99999999999 hour fcst"), IdxTime::Other);
    }

    #[test]
    fn hrrr_f06_fields_and_ranges() {
        let idx = fixture("hrrr.t10z.wrfsfcf06.grib2.idx");
        assert_eq!(idx.len(), 173);
        let tmp = find(&idx, &inst("TMP", "2 m above ground"), 360).unwrap();
        assert_eq!(tmp.range, range(44147451, 45339972));
        let refc = find(&idx, &inst("REFC", "entire atmosphere"), 360).unwrap();
        assert_eq!(refc.range.start, 0);
        let u = find(&idx, &inst("UGRD", "10 m above ground"), 360).unwrap();
        let v = find(&idx, &inst("VGRD", "10 m above ground"), 360).unwrap();
        assert_eq!(u.range, range(52038559, 54420174));
        assert_eq!(v.range, range(54420174, 56801789));
        // The two wind components are neighbours: one request covers both.
        assert_eq!(
            coalesce(&[u.range, v.range], 0),
            vec![range(52038559, 56801789)]
        );
        // Wrong forecast hour: nothing.
        assert!(find(&idx, &inst("TMP", "2 m above ground"), 300).is_none());
        // Cloud cover: the whole column, not the boundary layer line before it.
        let tcdc = find(&idx, &inst("TCDC", "entire atmosphere"), 360).unwrap();
        assert_eq!(tcdc.range, range(85807405, 86617460));
    }

    #[test]
    fn run_total_matches_only_the_run_accumulation() {
        let idx = fixture("hrrr.t10z.wrfsfcf06.grib2.idx");
        let asnow = FieldSel {
            var: "ASNOW",
            level: "surface",
            stat: Stat::RunTotal,
        };
        assert_eq!(find(&idx, &asnow, 360).unwrap().range.start, 42192603);
        let weasd = FieldSel {
            var: "WEASD",
            level: "surface",
            stat: Stat::RunTotal,
        };
        // 0-6 hour, not the 5-6 hour line after it.
        assert_eq!(find(&idx, &weasd, 360).unwrap().range.start, 61408248);
        // ASNOW is accumulated, so an instantaneous request finds nothing.
        assert!(find(&idx, &inst("ASNOW", "surface"), 360).is_none());
        let f01 = fixture("hrrr.t10z.wrfsfcf01.grib2.idx");
        assert_eq!(find(&f01, &asnow, 60).unwrap().range.start, 40246917);
    }

    #[test]
    fn gfs_instant_is_not_the_average() {
        let idx = fixture("gfs.t06z.pgrb2.0p25.f024.idx");
        let prate = find(&idx, &inst("PRATE", "surface"), 24 * 60).unwrap();
        assert_eq!(prate.range.start, 423777891);
        let csnow = find(&idx, &inst("CSNOW", "surface"), 24 * 60).unwrap();
        assert_eq!(csnow.range.start, 427675687);
        let tcdc = find(&idx, &inst("TCDC", "entire atmosphere"), 24 * 60).unwrap();
        assert_eq!(tcdc.range, range(444274253, 445114245));
        // Pressure-level cloud lines come first in the file and must not match.
        assert!(idx.iter().any(|e| e.var == "TCDC" && e.level == "500 mb"));
        // `TMP:2 mb` is a stratospheric level, not two metres.
        let tmp = find(&idx, &inst("TMP", "2 m above ground"), 24 * 60).unwrap();
        assert_eq!(tmp.range.start, 415004804);

        let f120 = fixture("gfs.t06z.pgrb2.0p25.f120.idx");
        let prate = find(&f120, &inst("PRATE", "surface"), 120 * 60).unwrap();
        assert_eq!(prate.range.start, 423871499);
        let tcdc = find(&f120, &inst("TCDC", "entire atmosphere"), 120 * 60).unwrap();
        assert_eq!(tcdc.range.start, 445178244);
    }

    /// GFS names stratospheric levels `2 mb` and `10 mb`; a match on the
    /// level's first characters would take them for 2 and 10 metres.
    #[test]
    fn gfs_metres_are_not_millibars() {
        let idx = fixture("gfs.t06z.pgrb2.0p25.f024.idx");
        assert!(idx.iter().any(|e| e.var == "UGRD" && e.level == "10 mb"));
        let u = find(&idx, &inst("UGRD", "10 m above ground"), 24 * 60).unwrap();
        assert_eq!(u.range.start, 420628197);
        let rh = find(&idx, &inst("RH", "2 m above ground"), 24 * 60).unwrap();
        assert_eq!(rh.range.start, 417597108);
        assert!(find(&idx, &inst("RH", "2 m"), 24 * 60).is_none());
    }

    /// From f009 GFS has two `APCP:surface` lines: the last six hours, and
    /// the run total (written in days from f024).
    #[test]
    fn gfs_run_total_precipitation_is_the_one_from_zero() {
        let apcp = FieldSel {
            var: "APCP",
            level: "surface",
            stat: Stat::RunTotal,
        };
        let f024 = fixture("gfs.t06z.pgrb2.0p25.f024.idx");
        assert_eq!(f024.iter().filter(|e| e.var == "APCP").count(), 2);
        assert_eq!(find(&f024, &apcp, 24 * 60).unwrap().range.start, 426210655);
        let f120 = fixture("gfs.t06z.pgrb2.0p25.f120.idx");
        let total = find(&f120, &apcp, 120 * 60).unwrap();
        assert_eq!(
            total.time,
            IdxTime::Acc {
                from: 0,
                to: 120 * 60
            }
        );
    }

    #[test]
    fn step_statistics() {
        let text = "1:0:d=2026111017:APCP:surface:5-6 hour acc fcst:\n\
                    2:100:d=2026111017:APCP:surface:0-6 hour acc fcst:\n\
                    3:200:d=2026111017:GUST:10 m above ground:5-6 hour max fcst:\n";
        let idx = parse(text);
        let at = |stat| {
            find(
                &idx,
                &FieldSel {
                    var: "APCP",
                    level: "surface",
                    stat,
                },
                360,
            )
            .map(|e| e.range.start)
        };
        assert_eq!(at(Stat::StepPerHour), Some(0));
        assert_eq!(at(Stat::RunTotal), Some(100));
        assert_eq!(at(Stat::Instant), None);
        let gust = FieldSel {
            var: "GUST",
            level: "10 m above ground",
            stat: Stat::StepMax,
        };
        assert_eq!(find(&idx, &gust, 360).unwrap().range.start, 200);
        assert!(find(&idx, &gust, 300).is_none());
        // A six-hour window is not the hour before.
        let six = parse("1:0:d=2026111017:APCP:surface:43-49 hour acc fcst:\n");
        assert!(
            find(
                &six,
                &FieldSel {
                    var: "APCP",
                    level: "surface",
                    stat: Stat::StepPerHour
                },
                49 * 60
            )
            .is_none()
        );
    }

    /// DMI's wgrib2-style `.index` carries an ensemble column at the end.
    #[test]
    fn wgrib2_index_lines() {
        let text = "6:33661920:d=2026110615:RH:2 m above ground:3 hour fcst:ENS=hi-res ctl\n\
                    7:39798982:d=2026110615:TMP:2 m above ground:3 hour fcst:ENS=hi-res ctl\n\
                    8:45936044:d=2026110615:TPRATE:surface:0-3 hour acc fcst:ENS=hi-res ctl\n";
        let idx = parse(text);
        let tmp = find(&idx, &inst("TMP", "2 m above ground"), 180).unwrap();
        assert_eq!(tmp.range, ByteRange::new(39798982, 45936044));
        assert_eq!(idx[2].time, IdxTime::Acc { from: 0, to: 180 });
        assert_eq!(idx[2].range, ByteRange::tail(45936044));
        assert_eq!(ByteRange::at(10, 5), ByteRange::new(10, 15));
    }

    #[test]
    fn subhourly_minutes() {
        let idx = fixture("hrrr.t10z.wrfsubhf01.grib2.idx");
        let refc = inst("REFC", "entire atmosphere");
        let starts: Vec<u64> = [15, 30, 45, 60]
            .iter()
            .map(|&m| find(&idx, &refc, m).unwrap().range.start)
            .collect();
        assert_eq!(starts, vec![0, 41661330, 83141317, 124900784]);
        assert_eq!(find(&idx, &refc, 15).unwrap().range.end, Some(270926));
        assert!(find(&idx, &refc, 75).is_none());
    }

    #[test]
    fn rtma_fields_and_last_line_to_eof() {
        let idx = fixture("rtma2p5.t11z.2dvaranl_ndfd.grb2_wexp.idx");
        assert_eq!(idx.len(), 13);
        let tmp = find(&idx, &inst("TMP", "2 m above ground"), 0).unwrap();
        assert_eq!(tmp.range, range(14980236, 20597872));
        let gust = find(&idx, &inst("GUST", "10 m above ground"), 0).unwrap();
        assert_eq!(gust.range, range(56644292, 62261928));
        // The last message has no successor: it runs to the end of the file.
        let last = idx.last().unwrap();
        assert_eq!(last.var, "TCDC");
        assert_eq!(last.range.end, None);
        assert_eq!(
            last.range.bounded(Some(84_000_000)),
            range(77242165, 84_000_000)
        );
        assert_eq!(last.range.bounded(None).end, None);
        // RTMA's cloud level text differs from the models'.
        assert!(find(&idx, &inst("TCDC", "entire atmosphere"), 0).is_none());
    }

    #[test]
    fn ends_follow_offsets_not_line_numbers() {
        let text = "1:0:d=2026110610:UGRD:10 m above ground:anl:\n\
                    1.2:0:d=2026110610:VGRD:10 m above ground:anl:\n\
                    garbage line\n\
                    2:500:d=2026110610:TMP:2 m above ground:anl:\n\
                    3:x:d=2026110610:BAD:surface:anl:\n";
        let idx = parse(text);
        assert_eq!(idx.len(), 3);
        assert_eq!(idx[0].range, range(0, 500));
        assert_eq!(idx[1].range, range(0, 500));
        assert_eq!(idx[2].range.end, None);
        assert!(parse("").is_empty());
    }

    #[test]
    fn coalescing_merges_neighbours_once() {
        let rtma = fixture("rtma2p5.t11z.2dvaranl_ndfd.grb2_wexp.idx");
        let wanted: Vec<ByteRange> = [
            inst("TMP", "2 m above ground"),
            inst("DPT", "2 m above ground"),
            inst("UGRD", "10 m above ground"),
            inst("VGRD", "10 m above ground"),
            inst("TMP", "2 m above ground"),
            inst("GUST", "10 m above ground"),
        ]
        .iter()
        .map(|s| find(&rtma, s, 0).unwrap().range)
        .collect();
        let merged = coalesce(&wanted, 0);
        assert_eq!(
            merged,
            vec![range(14980236, 38387022), range(56644292, 62261928)]
        );

        let open = ByteRange {
            start: 900,
            end: None,
        };
        assert_eq!(
            coalesce(&[range(0, 10), range(20, 30), open, range(950, 960)], 10),
            vec![range(0, 30), open]
        );
        assert_eq!(
            coalesce(&[range(0, 10), range(30, 40)], 10),
            vec![range(0, 10), range(30, 40)]
        );
        assert!(coalesce(&[], 0).is_empty());
    }

    #[test]
    fn slicing_fetched_ranges() {
        let data = bytes::Bytes::from_static(b"GRIBaaaaGRIBbbGRIBc");
        let fetched = vec![(range(100, 119), data)];
        assert_eq!(&slice(&fetched, range(100, 108)).unwrap()[..], b"GRIBaaaa");
        assert_eq!(&slice(&fetched, range(108, 114)).unwrap()[..], b"GRIBbb");
        let tail = ByteRange {
            start: 114,
            end: None,
        };
        assert!(slice(&fetched, tail).is_none());
        let open = vec![(tail, bytes::Bytes::from_static(b"GRIBc"))];
        assert_eq!(&slice(&open, tail).unwrap()[..], b"GRIBc");
        // Short response: the inventory promised more than arrived.
        let short = vec![(range(0, 100), bytes::Bytes::from_static(b"GRIB"))];
        assert!(slice(&short, range(0, 50)).is_none());
        assert!(slice(&fetched, range(0, 5)).is_none());
    }
}
