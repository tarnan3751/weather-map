//! Matching GRIB2 messages against `.idx`-style selectors (`TMP:2 m above
//! ground`): the inventory vocabulary of NOAA's files and of those that copy
//! wgrib2's (DMI's `.index`, CPTEC's `.inv`).

use anyhow::{Result, anyhow};
use wx_core::layer::{FieldSel, Stat};

/// GRIB2 code table 4.10: accumulation.
pub(crate) const STAT_ACCUMULATION: u8 = 1;
/// GRIB2 code table 4.10: maximum.
pub(crate) const STAT_MAXIMUM: u8 = 2;
/// [`Stat::StepPerHour`] takes a record whose window is the registry's
/// ingest step, an hour: its amount is then already per hour.
pub(crate) const STEP_SECONDS: i64 = 3600;
/// GRIB2 code table 4.5: missing / no second surface.
const NO_SURFACE: u8 = 255;

/// How a message treats time, as far as selection cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimeKind {
    /// Product template 4.0 (or 4.1 for a control member): a value at one
    /// instant.
    Instant,
    /// Product template 4.8 (or 4.11 for a control member): statistically
    /// processed over an interval ending at the valid time.
    Statistical {
        /// Code table 4.10 of the single time range, `None` when there are several.
        process: Option<u8>,
        /// The interval starts at the reference time (`0-6 hour acc`, not `5-6 hour acc`).
        from_reference: bool,
        /// Length of the single time range in seconds, `None` when there
        /// are several or the unit has no fixed length.
        window: Option<i64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Product {
    pub discipline: u8,
    pub category: u8,
    pub number: u8,
    pub surface: u8,
    /// NaN when the message leaves it missing.
    pub surface_value: f64,
    pub second_surface: u8,
    pub time: TimeKind,
}

/// A selector resolved to GRIB2 numbers.
#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    pub sel: FieldSel,
    codes: Vec<(u8, u8, u8)>,
    surface: u8,
    surface_value: Option<f64>,
}

impl Resolved {
    pub fn new(sel: &FieldSel) -> Result<Self> {
        let codes = param_codes(sel.var)
            .map(<[_]>::to_vec)
            .or_else(|| unnamed_param(sel.var).map(|c| vec![c]))
            .ok_or_else(|| anyhow!("unknown GRIB variable name {:?}", sel.var))?;
        let (surface, surface_value) = parse_level(sel.level)
            .ok_or_else(|| anyhow!("unsupported level {:?} for {}", sel.level, sel.var))?;
        Ok(Self {
            sel: *sel,
            codes,
            surface,
            surface_value,
        })
    }

    pub fn matches(&self, p: &Product) -> bool {
        let param = self.codes.contains(&(p.discipline, p.category, p.number));
        let value_ok = match self.surface_value {
            None => true,
            Some(v) => (p.surface_value - v).abs() <= 1e-6 * v.abs().max(1.0),
        };
        let level = p.surface == self.surface && value_ok && p.second_surface == NO_SURFACE;
        let time = match (self.sel.stat, p.time) {
            (Stat::Instant, TimeKind::Instant) => true,
            (
                Stat::RunTotal,
                TimeKind::Statistical {
                    process,
                    from_reference,
                    ..
                },
            ) => process == Some(STAT_ACCUMULATION) && from_reference,
            // `5-6 hour acc fcst`, and `0-1 hour acc fcst` at step 1. A file
            // with run totals only has none at later steps: the adapter then
            // differences two of them (`crate::step_per_hour`).
            (
                Stat::StepPerHour,
                TimeKind::Statistical {
                    process, window, ..
                },
            ) => process == Some(STAT_ACCUMULATION) && window == Some(STEP_SECONDS),
            // `2-3 hour max fcst`: whatever interval the file uses.
            (Stat::StepMax, TimeKind::Statistical { process, .. }) => process == Some(STAT_MAXIMUM),
            _ => false,
        };
        param && level && time
    }
}

pub(crate) fn describe(sel: &FieldSel) -> String {
    let stat = match sel.stat {
        Stat::Instant => "instantaneous",
        Stat::RunTotal => "accumulated since the run started",
        Stat::StepPerHour => "accumulated over the hour before",
        Stat::StepMax => "maximum over the output interval",
        #[allow(unreachable_patterns)]
        _ => "another statistic",
    };
    format!("{}:{} ({stat})", sel.var, sel.level)
}

/// wgrib2's name for a parameter it has no name for, as DMI's `.index`
/// writes it: `var discipline=0 center=94 local_table=1 parmcat=6 parm=199`.
/// The centre and local table version are not in the message's Section 4
/// and are not checked.
pub(crate) fn unnamed_param(var: &str) -> Option<(u8, u8, u8)> {
    let mut fields = var.strip_prefix("var ")?.split(' ');
    let mut value = |key: &str| -> Option<u8> {
        let (k, v) = fields.next()?.split_once('=')?;
        (k == key).then(|| v.parse().ok()).flatten()
    };
    let discipline = value("discipline")?;
    value("center")?;
    value("local_table")?;
    let (category, number) = (value("parmcat")?, value("parm")?);
    fields
        .next()
        .is_none()
        .then_some((discipline, category, number))
}

/// `(discipline, category, number)` for an NCEP variable abbreviation. Some
/// variables have both a WMO number and an NCEP local one, and the models do
/// not agree on which to use (HRRR writes categorical snow as 0.1.36, GFS as
/// 0.1.195), so both are accepted.
pub(crate) fn param_codes(var: &str) -> Option<&'static [(u8, u8, u8)]> {
    Some(match var {
        "TMP" => &[(0, 0, 0)],
        "DPT" => &[(0, 0, 6)],
        "SPFH" => &[(0, 1, 0)],
        "RH" => &[(0, 1, 1)],
        "PRATE" => &[(0, 1, 7)],
        "APCP" => &[(0, 1, 8)],
        "SNOD" => &[(0, 1, 11)],
        "WEASD" => &[(0, 1, 13)],
        "ASNOW" => &[(0, 1, 29)],
        "CRAIN" => &[(0, 1, 33), (0, 1, 192)],
        "CFRZR" => &[(0, 1, 34), (0, 1, 193)],
        "CICEP" => &[(0, 1, 35), (0, 1, 194)],
        "CSNOW" => &[(0, 1, 36), (0, 1, 195)],
        // Total precipitation rate; DMI writes its accumulated `tp` with
        // this number, and wgrib2 names it so.
        "TPRATE" => &[(0, 1, 52)],
        // Total solid precipitation (DMI's `titspf`, accumulated).
        "TSPRATE" => &[(0, 1, 128)],
        "WDIR" => &[(0, 2, 0)],
        "WIND" => &[(0, 2, 1)],
        "UGRD" => &[(0, 2, 2)],
        "VGRD" => &[(0, 2, 3)],
        "GUST" => &[(0, 2, 22)],
        "PRES" => &[(0, 3, 0)],
        "PRMSL" => &[(0, 3, 1)],
        "HGT" => &[(0, 3, 5)],
        "TCDC" => &[(0, 6, 1)],
        // DMI's total cloud cover, a fraction (ecCodes `cc`).
        "FRACCC" => &[(0, 6, 32)],
        "CAPE" => &[(0, 7, 6)],
        "REFC" => &[(0, 16, 196), (0, 16, 5)],
        "VIS" => &[(0, 19, 0)],
        _ => return None,
    })
}

/// Code table 4.5 surface type and, where the level names one, its value in
/// the table's unit (metres, pascals).
pub(crate) fn parse_level(level: &str) -> Option<(u8, Option<f64>)> {
    let fixed = match level {
        "surface" => Some(1),
        "cloud top" => Some(3),
        "entire atmosphere" => Some(10),
        "mean sea level" => Some(101),
        "entire atmosphere (considered as a single layer)" => Some(200),
        "cloud ceiling" => Some(215),
        _ => None,
    };
    if let Some(t) = fixed {
        return Some((t, None));
    }
    let number = |suffix: &str| -> Option<f64> {
        let n: f64 = level.strip_suffix(suffix)?.trim().parse().ok()?;
        n.is_finite().then_some(n)
    };
    if let Some(n) = number(" m above ground") {
        return Some((103, Some(n)));
    }
    if let Some(n) = number(" m above mean sea level") {
        return Some((102, Some(n)));
    }
    if let Some(n) = number(" mb") {
        return Some((100, Some(n * 100.0)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(d: u8, c: u8, n: u8, surface: u8, value: f64, time: TimeKind) -> Product {
        Product {
            discipline: d,
            category: c,
            number: n,
            surface,
            surface_value: value,
            second_surface: NO_SURFACE,
            time,
        }
    }

    const fn sel(var: &'static str, level: &'static str, stat: Stat) -> FieldSel {
        FieldSel { var, level, stat }
    }

    const ACC_FROM_START: TimeKind = TimeKind::Statistical {
        process: Some(STAT_ACCUMULATION),
        from_reference: true,
        window: Some(6 * 3600),
    };

    #[test]
    fn levels_parse() {
        assert_eq!(parse_level("surface"), Some((1, None)));
        assert_eq!(parse_level("2 m above ground"), Some((103, Some(2.0))));
        assert_eq!(parse_level("10 m above ground"), Some((103, Some(10.0))));
        assert_eq!(parse_level("entire atmosphere"), Some((10, None)));
        assert_eq!(parse_level("500 mb"), Some((100, Some(50_000.0))));
        assert_eq!(
            parse_level("entire atmosphere (considered as a single layer)"),
            Some((200, None))
        );
        assert_eq!(parse_level("nowhere"), None);
        assert_eq!(parse_level("x m above ground"), None);
    }

    #[test]
    fn height_must_match() {
        let tmp = Resolved::new(&sel("TMP", "2 m above ground", Stat::Instant)).unwrap();
        assert!(tmp.matches(&product(0, 0, 0, 103, 2.0, TimeKind::Instant)));
        assert!(!tmp.matches(&product(0, 0, 0, 103, 80.0, TimeKind::Instant)));
        assert!(
            !tmp.matches(&product(0, 0, 0, 1, 0.0, TimeKind::Instant)),
            "surface temperature"
        );
        assert!(
            !tmp.matches(&product(0, 0, 6, 103, 2.0, TimeKind::Instant)),
            "dew point"
        );
        let mut layer = product(0, 0, 0, 103, 2.0, TimeKind::Instant);
        layer.second_surface = 103;
        assert!(
            !tmp.matches(&layer),
            "a layer between two heights is not a level"
        );
    }

    #[test]
    fn categorical_precip_accepts_both_numberings() {
        let csnow = Resolved::new(&sel("CSNOW", "surface", Stat::Instant)).unwrap();
        let hrrr = product(0, 1, 36, 1, 0.0, TimeKind::Instant);
        let gfs = product(0, 1, 195, 1, 0.0, TimeKind::Instant);
        assert!(csnow.matches(&hrrr) && csnow.matches(&gfs));
        let crain = Resolved::new(&sel("CRAIN", "surface", Stat::Instant)).unwrap();
        assert!(!crain.matches(&hrrr) && !crain.matches(&gfs));
        assert!(crain.matches(&product(0, 1, 33, 1, 0.0, TimeKind::Instant)));
        assert!(crain.matches(&product(0, 1, 192, 1, 0.0, TimeKind::Instant)));
    }

    #[test]
    fn instant_rejects_averages_and_accumulations() {
        let u = Resolved::new(&sel("UGRD", "10 m above ground", Stat::Instant)).unwrap();
        assert!(u.matches(&product(0, 2, 2, 103, 10.0, TimeKind::Instant)));
        // HRRR sub-hourly files also carry `10-15 min ave fcst`.
        let average = TimeKind::Statistical {
            process: Some(0),
            from_reference: false,
            window: Some(300),
        };
        assert!(!u.matches(&product(0, 2, 2, 103, 10.0, average)));
        assert!(!u.matches(&product(0, 2, 2, 103, 10.0, ACC_FROM_START)));
    }

    #[test]
    fn run_total_wants_the_accumulation_from_the_start() {
        let asnow = Resolved::new(&sel("ASNOW", "surface", Stat::RunTotal)).unwrap();
        assert!(asnow.matches(&product(0, 1, 29, 1, 0.0, ACC_FROM_START)));
        assert!(!asnow.matches(&product(0, 1, 29, 1, 0.0, TimeKind::Instant)));
        // `5-6 hour acc fcst`: the last hour only.
        let last_hour = TimeKind::Statistical {
            process: Some(STAT_ACCUMULATION),
            from_reference: false,
            window: Some(3600),
        };
        assert!(!asnow.matches(&product(0, 1, 29, 1, 0.0, last_hour)));
        let average = TimeKind::Statistical {
            process: Some(0),
            from_reference: true,
            window: Some(6 * 3600),
        };
        assert!(!asnow.matches(&product(0, 1, 29, 1, 0.0, average)));
        let several_ranges = TimeKind::Statistical {
            process: None,
            from_reference: true,
            window: None,
        };
        assert!(!asnow.matches(&product(0, 1, 29, 1, 0.0, several_ranges)));
    }

    #[test]
    fn step_per_hour_wants_a_one_hour_window() {
        let apcp = Resolved::new(&sel("APCP", "surface", Stat::StepPerHour)).unwrap();
        let acc = |from_reference, hours: i64| TimeKind::Statistical {
            process: Some(STAT_ACCUMULATION),
            from_reference,
            window: Some(hours * 3600),
        };
        // NBM's `5-6 hour acc fcst`, and `0-1 hour acc fcst` at step 1.
        assert!(apcp.matches(&product(0, 1, 8, 1, 0.0, acc(false, 1))));
        assert!(apcp.matches(&product(0, 1, 8, 1, 0.0, acc(true, 1))));
        // A run total over 3 hours is not the last hour's amount.
        assert!(!apcp.matches(&product(0, 1, 8, 1, 0.0, acc(true, 3))));
        assert!(!apcp.matches(&product(0, 1, 8, 1, 0.0, acc(false, 6))));
        assert!(!apcp.matches(&product(0, 1, 8, 1, 0.0, TimeKind::Instant)));
        let average = TimeKind::Statistical {
            process: Some(0),
            from_reference: false,
            window: Some(3600),
        };
        assert!(!apcp.matches(&product(0, 1, 8, 1, 0.0, average)));
    }

    #[test]
    fn step_max_wants_a_maximum() {
        let gust = Resolved::new(&sel("GUST", "10 m above ground", Stat::StepMax)).unwrap();
        let max = |from_reference, hours: i64| TimeKind::Statistical {
            process: Some(STAT_MAXIMUM),
            from_reference,
            window: Some(hours * 3600),
        };
        // DMI's `2-3 hour max fcst`, `0-1 hour max fcst` at step 1, and a
        // longer interval if that is what the file uses.
        assert!(gust.matches(&product(0, 2, 22, 103, 10.0, max(false, 1))));
        assert!(gust.matches(&product(0, 2, 22, 103, 10.0, max(true, 1))));
        assert!(gust.matches(&product(0, 2, 22, 103, 10.0, max(false, 3))));
        assert!(!gust.matches(&product(0, 2, 22, 103, 10.0, TimeKind::Instant)));
        let min = TimeKind::Statistical {
            process: Some(3),
            from_reference: false,
            window: Some(3600),
        };
        assert!(!gust.matches(&product(0, 2, 22, 103, 10.0, min)));
    }

    #[test]
    fn wgrib2_names_for_unnamed_parameters() {
        assert_eq!(
            unnamed_param("var discipline=0 center=94 local_table=1 parmcat=6 parm=199"),
            Some((0, 6, 199))
        );
        assert_eq!(unnamed_param("var discipline=0 parmcat=6 parm=199"), None);
        assert_eq!(
            unnamed_param("var discipline=0 center=94 local_table=1 parmcat=6 parm=999"),
            None
        );
        assert_eq!(unnamed_param("TMP"), None);
        let cloud = Resolved::new(&sel(
            "var discipline=0 center=94 local_table=1 parmcat=6 parm=199",
            "0 m above ground",
            Stat::Instant,
        ))
        .unwrap();
        assert!(cloud.matches(&product(0, 6, 199, 103, 0.0, TimeKind::Instant)));
        assert!(!cloud.matches(&product(0, 6, 1, 103, 0.0, TimeKind::Instant)));
        // DMI's total cloud cover is a named field.
        let total = Resolved::new(&sel("FRACCC", "0 m above ground", Stat::Instant)).unwrap();
        assert!(total.matches(&product(0, 6, 32, 103, 0.0, TimeKind::Instant)));
        assert!(!total.matches(&product(0, 6, 32, 103, 2.0, TimeKind::Instant)));
    }

    /// The families whose selectors are inventory strings (see
    /// `wx_core::families`); the others name fields in their own vocabulary,
    /// which their adapters map to GRIB numbers (or to datasets).
    const INVENTORY_FAMILIES: &[wx_core::Family] = &[
        wx_core::Family::Mrms,
        wx_core::Family::Rtma,
        wx_core::Family::Hrrr,
        wx_core::Family::Gfs,
        wx_core::Family::Nbm,
        wx_core::Family::Dmi,
        wx_core::Family::Cptec,
    ];

    #[test]
    fn every_inventory_selector_resolves() {
        let mut checked = 0;
        for layer in wx_core::LAYERS {
            for source in layer.inputs {
                if !INVENTORY_FAMILIES.contains(&source.family) {
                    continue;
                }
                for s in source.fields {
                    Resolved::new(s).unwrap_or_else(|e| panic!("{}: {e}", layer.id));
                    checked += 1;
                }
            }
        }
        assert!(checked > 20, "{checked}");
        assert!(Resolved::new(&sel("NOPE", "surface", Stat::Instant)).is_err());
        assert!(Resolved::new(&sel("TMP", "the moon", Stat::Instant)).is_err());
    }
}
