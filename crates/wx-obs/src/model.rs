//! The provider-neutral point forecast, the hook through which the server can
//! supply its own, and the shared condition vocabulary.

use chrono::{DateTime, NaiveDate, Utc};
use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::Value;

/// One step of the hourly series. `span_h` is how long the step lasts (1, or
/// 6 where a provider only has coarser data further out); `precip_mm`,
/// `pop` and `condition` describe that span.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HourlyPoint {
    pub time: DateTime<Utc>,
    pub span_h: u8,
    pub temp_c: Option<f64>,
    /// Chance of precipitation, percent, when the provider has one.
    pub pop: Option<u8>,
    pub precip_mm: Option<f64>,
    pub wind_ms: Option<f64>,
    pub wind_dir_deg: Option<u16>,
    /// A MET Norway symbol code, with a `_day`/`_night` suffix where one exists.
    pub condition: Option<String>,
    /// The same condition in English words.
    pub text: Option<String>,
}

/// One local calendar day.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DailyPoint {
    pub date: NaiveDate,
    pub low_c: Option<f64>,
    pub high_c: Option<f64>,
    pub pop: Option<u8>,
    pub precip_mm: Option<f64>,
    /// A symbol code without the day/night suffix.
    pub condition: Option<String>,
    /// The provider's own written forecast, when it has one.
    pub text: Option<String>,
    /// When the day is over, to drop it from cached forecasts; not sent.
    #[serde(skip)]
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PointForecast {
    /// Which provider made this: `nws`, `met-no`, or whatever a [`PointModel`] says.
    pub source: String,
    pub attribution: String,
    pub place: Option<String>,
    pub time_zone: Option<String>,
    pub generated_at: DateTime<Utc>,
    pub hourly: Vec<HourlyPoint>,
    pub daily: Vec<DailyPoint>,
}

/// A provider's probability as a whole percentage.
pub(crate) fn percent(p: f64) -> u8 {
    p.round().clamp(0.0, 100.0) as u8
}

/// Fold `v` into an accumulated value: `f64::min`, `f64::max` or a sum.
pub(crate) fn fold(acc: &mut Option<f64>, v: f64, with: fn(f64, f64) -> f64) {
    *acc = Some(acc.map_or(v, |a| with(a, v)));
}

/// A timestamp from a JSON string.
pub(crate) fn time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()?.parse().ok()
}

/// A forecast the server computes itself, used when the outside providers
/// fail for a point. Return `None` when the point is not covered. The
/// forecast's `source` and `attribution` are sent as given; an empty `source`
/// becomes `model`.
pub trait PointModel: Send + Sync {
    fn forecast(&self, lat: f64, lon: f64) -> BoxFuture<'_, Option<PointForecast>>;
}

/// Codes that have separate day and night symbols.
fn has_daypart(base: &str) -> bool {
    base.starts_with("clearsky")
        || base.starts_with("fair")
        || base.starts_with("partlycloudy")
        || base.contains("showers")
}

/// "partlycloudy_night" -> "partlycloudy".
pub fn strip_daypart(code: &str) -> &str {
    ["_day", "_night", "_polartwilight"]
        .iter()
        .find_map(|s| code.strip_suffix(s))
        .unwrap_or(code)
}

/// "lightrainshowersandthunder_day" -> "Light rain showers and thunder".
pub fn condition_text(code: &str) -> String {
    let base = strip_daypart(code);
    let thunder = base.contains("andthunder");
    let base = base.replace("andthunder", "");
    let (intensity, rest) = match (base.strip_prefix("light"), base.strip_prefix("heavy")) {
        (Some(r), _) => ("light ", r),
        (_, Some(r)) => ("heavy ", r),
        _ => ("", base.as_str()),
    };
    let noun = match rest {
        "clearsky" => "clear sky",
        "partlycloudy" => "partly cloudy",
        "rainshowers" => "rain showers",
        "sleetshowers" => "sleet showers",
        "snowshowers" => "snow showers",
        other => other,
    };
    let mut s = format!("{intensity}{noun}");
    if thunder {
        s.push_str(" and thunder");
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => s,
    }
}

/// The nearest symbol code to an NWS `shortForecast` ("Chance Showers And
/// Thunderstorms"), or `None` for wording not recognised. Where the text
/// changes ("Fog then Sunny") the first part is the one in effect now.
pub fn code_from_nws(short: &str, is_day: bool) -> Option<String> {
    let lower = short.to_lowercase();
    let s = lower.split(" then ").next().unwrap_or("");
    let has = |w: &str| s.contains(w);
    let thunder = if has("thunder") { "andthunder" } else { "" };
    let intensity = if has("light") || has("slight") || has("drizzle") {
        "light"
    } else if has("heavy") {
        "heavy"
    } else {
        ""
    };
    let showers = if has("showers") { "showers" } else { "" };
    let code = if has("freezing") || has("sleet") || has("wintry") || has("ice pellet") {
        format!("{intensity}sleet{showers}{thunder}")
    } else if has("snow") || has("flurries") || has("blizzard") {
        let intensity = if has("flurries") {
            "light"
        } else if has("blizzard") {
            "heavy"
        } else {
            intensity
        };
        format!("{intensity}snow{showers}{thunder}")
    } else if has("rain") || has("showers") || has("drizzle") || has("thunder") {
        format!("{intensity}rain{showers}{thunder}")
    } else if has("fog") || has("haze") || has("smoke") || has("mist") || has("dust") {
        "fog".to_string()
    } else if has("mostly cloudy") || has("cloudy") && !has("partly") || has("overcast") {
        "cloudy".to_string()
    } else if has("partly") {
        "partlycloudy".to_string()
    } else if has("mostly sunny") || has("mostly clear") {
        "fair".to_string()
    } else if has("sunny") || has("clear") {
        "clearsky".to_string()
    } else {
        return None;
    };
    let part = if is_day { "_day" } else { "_night" };
    Some(if has_daypart(&code) {
        code + part
    } else {
        code
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_from_codes() {
        assert_eq!(condition_text("partlycloudy_night"), "Partly cloudy");
        assert_eq!(condition_text("clearsky_day"), "Clear sky");
        assert_eq!(
            condition_text("lightrainshowersandthunder_day"),
            "Light rain showers and thunder"
        );
        assert_eq!(condition_text("heavysnow"), "Heavy snow");
        assert_eq!(condition_text("fog"), "Fog");
        assert_eq!(strip_daypart("rainshowers_polartwilight"), "rainshowers");
        assert_eq!(strip_daypart("cloudy"), "cloudy");
    }

    #[test]
    fn nws_wording_maps_to_codes() {
        let c = |s: &str, day| code_from_nws(s, day).unwrap_or_default();
        assert_eq!(c("Sunny", true), "clearsky_day");
        assert_eq!(c("Clear", false), "clearsky_night");
        assert_eq!(c("Mostly Sunny", true), "fair_day");
        assert_eq!(c("Partly Cloudy", false), "partlycloudy_night");
        assert_eq!(c("Partly Sunny", true), "partlycloudy_day");
        assert_eq!(c("Mostly Cloudy", true), "cloudy");
        assert_eq!(c("Patchy Fog then Partly Sunny", true), "fog");
        assert_eq!(c("Chance Rain Showers", true), "rainshowers_day");
        assert_eq!(c("Light Rain", true), "lightrain");
        assert_eq!(
            c("Slight Chance Showers And Thunderstorms", false),
            "lightrainshowersandthunder_night"
        );
        assert_eq!(c("Thunderstorms", true), "rainandthunder");
        assert_eq!(c("Heavy Snow", true), "heavysnow");
        assert_eq!(c("Snow Showers", false), "snowshowers_night");
        assert_eq!(c("Freezing Rain", true), "sleet");
        assert_eq!(c("Areas Of Frost", true), "", "not recognised");
    }
}
