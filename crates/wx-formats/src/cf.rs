//! The bits of the NetCDF CF conventions the NetCDF-4 decoders share:
//! time units, ISO 8601 durations, packed-integer attributes.

use anyhow::{Result, anyhow, bail, ensure};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Utc};

use crate::hdf::H5;

/// A CF time `units` string (`seconds since 1970-01-01 00:00:00`, `hours
/// since 2026-11-06 12:00:00`, `seconds since 2000-01-01 12:00:00`) as the
/// length of one unit and the epoch, UTC.
pub(crate) fn time_units(units: &str) -> Result<(TimeDelta, DateTime<Utc>)> {
    let (unit, rest) = units
        .trim()
        .split_once(" since ")
        .ok_or_else(|| anyhow!("time units {units:?} are not '<unit> since <epoch>'"))?;
    let step = match unit.trim() {
        "seconds" | "second" | "s" => TimeDelta::seconds(1),
        "minutes" | "minute" | "min" => TimeDelta::minutes(1),
        "hours" | "hour" | "h" => TimeDelta::hours(1),
        "days" | "day" | "d" => TimeDelta::days(1),
        other => bail!("time unit {other:?} in {units:?}"),
    };
    let mut parts = rest.trim().splitn(3, [' ', 'T']);
    let date = parts.next().unwrap_or_default();
    let time = parts.next().unwrap_or("0:0:0").trim_end_matches('Z');
    let zone = parts.next().unwrap_or("").trim();
    ensure!(
        matches!(zone, "" | "0:00" | "00:00" | "+0:00" | "+00:00" | "UTC" | "Z"),
        "time zone {zone:?} in {units:?}"
    );
    let ymd: Vec<&str> = date.split('-').collect();
    ensure!(ymd.len() == 3, "epoch date {date:?} in {units:?}");
    let num = |s: &str| -> Result<u32> { s.parse().map_err(|_| anyhow!("epoch {rest:?} in {units:?}")) };
    let day = NaiveDate::from_ymd_opt(
        ymd[0].parse().map_err(|_| anyhow!("epoch year in {units:?}"))?,
        num(ymd[1])?,
        num(ymd[2])?,
    )
    .ok_or_else(|| anyhow!("epoch date {date:?} in {units:?}"))?;
    let hms: Vec<&str> = time.split(':').collect();
    ensure!(hms.len() == 3, "epoch time {time:?} in {units:?}");
    let secs: f64 = hms[2].parse().map_err(|_| anyhow!("epoch seconds in {units:?}"))?;
    ensure!((0.0..60.0).contains(&secs) && secs.fract() == 0.0, "epoch seconds {secs} in {units:?}");
    let tod = NaiveTime::from_hms_opt(num(hms[0])?, num(hms[1])?, secs as u32)
        .ok_or_else(|| anyhow!("epoch time {time:?} in {units:?}"))?;
    Ok((step, NaiveDateTime::new(day, tod).and_utc()))
}

/// `epoch + value * step`, refused when not finite or absurd.
pub(crate) fn time_at(units: &str, value: f64) -> Result<DateTime<Utc>> {
    let (step, epoch) = time_units(units)?;
    ensure!(value.is_finite() && value.abs() < 1e11, "time value {value} with units {units:?}");
    let millis = (value * step.num_milliseconds() as f64).round();
    ensure!(millis.abs() < 1e15, "time value {value} with units {units:?}");
    epoch
        .checked_add_signed(TimeDelta::milliseconds(millis as i64))
        .ok_or_else(|| anyhow!("time value {value} with units {units:?} overflows"))
}

/// An ISO 8601 duration of hours, minutes and seconds: `PT30M`, `PT1H`.
pub(crate) fn iso_duration(s: &str) -> Result<TimeDelta> {
    let body = s.trim().strip_prefix("PT").ok_or_else(|| anyhow!("duration {s:?} is not PT..."))?;
    let mut total = TimeDelta::zero();
    let mut num = String::new();
    for c in body.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: i64 = num.parse().map_err(|_| anyhow!("duration {s:?}"))?;
        ensure!(n < 1_000_000, "duration {s:?}");
        num.clear();
        total += match c {
            'H' => TimeDelta::hours(n),
            'M' => TimeDelta::minutes(n),
            'S' => TimeDelta::seconds(n),
            _ => bail!("duration {s:?}"),
        };
    }
    ensure!(num.is_empty() && total > TimeDelta::zero(), "duration {s:?}");
    Ok(total)
}

/// `scale_factor`, `add_offset` and the fill value of a packed variable,
/// with `_Unsigned = "true"` reported so signed storage can be read as
/// unsigned.
pub(crate) struct Packing {
    pub scale: f64,
    pub offset: f64,
    pub fill: Option<f64>,
    pub unsigned: bool,
}

impl Packing {
    pub fn of(h5: &H5, var: &str) -> Result<Packing> {
        let fill = match h5.attr_f64_opt(var, "_FillValue")? {
            Some(f) => Some(f),
            None => h5.attr_f64_opt(var, "missing_value")?,
        };
        Ok(Packing {
            scale: h5.attr_f64_opt(var, "scale_factor")?.unwrap_or(1.0),
            offset: h5.attr_f64_opt(var, "add_offset")?.unwrap_or(0.0),
            fill,
            unsigned: h5.attr_str_opt(var, "_Unsigned")?.is_some_and(|u| u == "true"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cf_time_units() {
        let t = time_at("seconds since 1970-01-01 00:00:00 0:00", 1_793_984_400.0).unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 6, 17, 0, 0).unwrap());
        let t = time_at("seconds since 1970-1-1 0:0:0", 1_793_984_400.0).unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 6, 17, 0, 0).unwrap());
        let t = time_at("hours since 2026-11-06 12:00:00", 6.0).unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 6, 18, 0, 0).unwrap());
        // GOES: J2000 seconds, 847260907.409681 is 18:15:07.410 on 2026-11-06.
        let t = time_at("seconds since 2000-01-01 12:00:00", 847_260_907.409_681).unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 6, 18, 15, 7).unwrap() + TimeDelta::milliseconds(410));
        for bad in [
            "seconds",
            "fortnights since 1970-01-01",
            "seconds since 1970-13-01",
            "seconds since 1970-01-01 0:0:0 +5:00",
        ] {
            assert!(time_units(bad).is_err(), "{bad}");
        }
        assert!(time_at("seconds since 1970-01-01", f64::NAN).is_err());
    }

    #[test]
    fn iso_durations() {
        assert_eq!(iso_duration("PT30M").unwrap(), TimeDelta::minutes(30));
        assert_eq!(iso_duration("PT1H30M").unwrap(), TimeDelta::minutes(90));
        for bad in ["30M", "PT", "PT5X", "PT0M", "PT5"] {
            assert!(iso_duration(bad).is_err(), "{bad}");
        }
    }
}
