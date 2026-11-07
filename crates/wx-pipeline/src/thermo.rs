//! Apparent temperature, the way the National Weather Service computes it.

const MS_TO_MPH: f64 = 3600.0 / 1609.344;

pub(crate) fn c_to_f(c: f64) -> f64 {
    c * 1.8 + 32.0
}

pub(crate) fn f_to_c(f: f64) -> f64 {
    (f - 32.0) / 1.8
}

/// NWS wind chill (2001 formula), air temperature in °F and 10 m wind in mph.
pub(crate) fn wind_chill_f(t: f64, mph: f64) -> f64 {
    let v = mph.powf(0.16);
    35.74 + 0.6215 * t - 35.75 * v + 0.4275 * t * v
}

/// NWS heat index in °F from temperature in °F and relative humidity in
/// percent: the simple Steadman-consistent formula where it gives under 80 °F,
/// otherwise the Rothfusz regression with the NWS low- and high-humidity
/// adjustments.
pub(crate) fn heat_index_f(t: f64, rh: f64) -> f64 {
    let simple = 0.5 * (t + 61.0 + (t - 68.0) * 1.2 + rh * 0.094);
    if (simple + t) / 2.0 < 80.0 {
        return simple;
    }
    let mut hi = -42.379 + 2.049_015_23 * t + 10.143_331_27 * rh
        - 0.224_755_41 * t * rh
        - 0.006_837_83 * t * t
        - 0.054_817_17 * rh * rh
        + 0.001_228_74 * t * t * rh
        + 0.000_852_82 * t * rh * rh
        - 0.000_001_99 * t * t * rh * rh;
    if rh < 13.0 && (80.0..=112.0).contains(&t) {
        hi -= (13.0 - rh) / 4.0 * ((17.0 - (t - 95.0).abs()) / 17.0).sqrt();
    } else if rh > 85.0 && (80.0..=87.0).contains(&t) {
        hi += (rh - 85.0) / 10.0 * ((87.0 - t) / 5.0);
    }
    hi
}

/// Relative humidity in percent from temperature and dew point in °C
/// (Magnus form with Bolton's constants).
pub(crate) fn relative_humidity(t_c: f64, td_c: f64) -> f64 {
    let e = |c: f64| (17.67 * c / (c + 243.5)).exp();
    (100.0 * e(td_c) / e(t_c)).clamp(0.0, 100.0)
}

/// The heat index's simple formula exceeds the air temperature only above
/// this, even at 100 % humidity (`0.5 (T + 61 + 1.2 (T - 68) + 9.4) > T`
/// needs `T > 56`), and the Rothfusz regression takes over only near 80 °F:
/// below it the humidity cannot change the answer and is not needed.
const HEAT_INDEX_CAN_EXCEED_F: f64 = 56.0;

/// Apparent temperature in °C, `wx_core::layer::Derive::FeelsLike`'s
/// procedure: NWS wind chill at or below 50 °F with wind above 3 mph;
/// otherwise the NWS heat index where it exceeds the air temperature;
/// otherwise the air temperature. `speed` (m/s) and `rh` (percent) are only
/// asked for when the branch needs them, and NaN from either, or a NaN
/// temperature, gives NaN.
pub(crate) fn apparent_c(t_k: f32, speed: impl FnOnce() -> f64, rh: impl FnOnce() -> f64) -> f32 {
    let t_c = f64::from(t_k) - 273.15;
    let t_f = c_to_f(t_c);
    if t_f.is_nan() {
        return f32::NAN;
    }
    if t_f <= 50.0 {
        let mph = speed() * MS_TO_MPH;
        if mph.is_nan() {
            return f32::NAN;
        }
        if mph > 3.0 {
            return f_to_c(wind_chill_f(t_f, mph)) as f32;
        }
    }
    if t_f <= HEAT_INDEX_CAN_EXCEED_F {
        return t_c as f32;
    }
    let rh = rh();
    if rh.is_nan() {
        return f32::NAN;
    }
    let hi = heat_index_f(t_f, rh);
    if hi > t_f {
        f_to_c(hi) as f32
    } else {
        t_c as f32
    }
}

/// [`apparent_c`] from temperature and dew point in kelvin and wind
/// components in m/s.
pub(crate) fn feels_like_c(t_k: f32, td_k: f32, u: f32, v: f32) -> f32 {
    apparent_c(
        t_k,
        || f64::from(u).hypot(f64::from(v)),
        || relative_humidity(f64::from(t_k) - 273.15, f64::from(td_k) - 273.15),
    )
}

/// Dew point in °C from temperature in °C and relative humidity in percent,
/// the inverse of [`relative_humidity`]: Magnus with Bolton's constants,
/// the humidity clamped to 1..100.
pub(crate) fn dew_point_c(t_c: f64, rh: f64) -> f64 {
    let g = (rh.clamp(1.0, 100.0) / 100.0).ln() + 17.67 * t_c / (t_c + 243.5);
    243.5 * g / (17.67 - g)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NWS wind chill chart (2001): rows are wind speed 5..=60 mph, columns
    /// air temperature 40..=-45 °F in steps of 5.
    const WIND_CHILL_CHART: [[i32; 18]; 12] = [
        [
            36, 31, 25, 19, 13, 7, 1, -5, -11, -16, -22, -28, -34, -40, -46, -52, -57, -63,
        ],
        [
            34, 27, 21, 15, 9, 3, -4, -10, -16, -22, -28, -35, -41, -47, -53, -59, -66, -72,
        ],
        [
            32, 25, 19, 13, 6, 0, -7, -13, -19, -26, -32, -39, -45, -51, -58, -64, -71, -77,
        ],
        [
            30, 24, 17, 11, 4, -2, -9, -15, -22, -29, -35, -42, -48, -55, -61, -68, -74, -81,
        ],
        [
            29, 23, 16, 9, 3, -4, -11, -17, -24, -31, -37, -44, -51, -58, -64, -71, -78, -84,
        ],
        [
            28, 22, 15, 8, 1, -5, -12, -19, -26, -33, -39, -46, -53, -60, -67, -73, -80, -87,
        ],
        [
            28, 21, 14, 7, 0, -7, -14, -21, -27, -34, -41, -48, -55, -62, -69, -76, -82, -89,
        ],
        [
            27, 20, 13, 6, -1, -8, -15, -22, -29, -36, -43, -50, -57, -64, -71, -78, -84, -91,
        ],
        [
            26, 19, 12, 5, -2, -9, -16, -23, -30, -37, -44, -51, -58, -65, -72, -79, -86, -93,
        ],
        [
            26, 19, 12, 4, -3, -10, -17, -24, -31, -38, -45, -52, -60, -67, -74, -81, -88, -95,
        ],
        [
            25, 18, 11, 4, -3, -11, -18, -25, -32, -39, -46, -54, -61, -68, -75, -82, -89, -97,
        ],
        [
            25, 17, 10, 3, -4, -11, -19, -26, -33, -40, -48, -55, -62, -69, -76, -84, -91, -98,
        ],
    ];

    #[test]
    fn wind_chill_matches_nws_chart() {
        for (r, row) in WIND_CHILL_CHART.iter().enumerate() {
            let mph = 5.0 * (r + 1) as f64;
            for (c, &want) in row.iter().enumerate() {
                let t = 40.0 - 5.0 * c as f64;
                let got = wind_chill_f(t, mph);
                // The chart rounds to whole degrees.
                assert!(
                    (got - f64::from(want)).abs() <= 0.5 + 1e-9,
                    "{t} F, {mph} mph: chart {want}, computed {got:.2}"
                );
            }
        }
    }

    /// NWS heat index chart: rows are air temperature 80..=110 °F in steps of
    /// 2, columns relative humidity 40..=100 % in steps of 5. The chart stops
    /// where the values become extreme, marked 0 here.
    const HEAT_INDEX_CHART: [[i32; 13]; 16] = [
        [80, 80, 81, 81, 82, 82, 83, 84, 84, 85, 86, 86, 87],
        [81, 82, 83, 84, 84, 85, 86, 88, 89, 90, 91, 93, 95],
        [83, 84, 85, 86, 88, 89, 90, 92, 94, 96, 98, 100, 103],
        [85, 87, 88, 89, 91, 93, 95, 97, 100, 102, 105, 108, 112],
        [88, 89, 91, 93, 95, 98, 100, 103, 106, 110, 113, 117, 121],
        [91, 93, 95, 97, 100, 103, 106, 109, 113, 117, 122, 127, 132],
        [94, 96, 99, 101, 105, 108, 112, 116, 121, 126, 131, 0, 0],
        [97, 100, 103, 106, 110, 114, 119, 124, 129, 135, 0, 0, 0],
        [101, 104, 108, 112, 116, 121, 126, 132, 0, 0, 0, 0, 0],
        [105, 109, 113, 117, 123, 128, 134, 0, 0, 0, 0, 0, 0],
        [109, 114, 118, 124, 129, 136, 0, 0, 0, 0, 0, 0, 0],
        [114, 119, 124, 130, 137, 0, 0, 0, 0, 0, 0, 0, 0],
        [119, 124, 131, 137, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [124, 130, 137, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [130, 137, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [136, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    ];

    #[test]
    fn heat_index_matches_nws_chart() {
        let mut checked = 0;
        for (r, row) in HEAT_INDEX_CHART.iter().enumerate() {
            let t = 80.0 + 2.0 * r as f64;
            for (c, &want) in row.iter().enumerate() {
                if want == 0 {
                    continue;
                }
                let rh = 40.0 + 5.0 * c as f64;
                // The chart is the bare regression, rounded. The NWS
                // algorithm adds up to 2.1 F to it when it is humid and
                // under 87 F, which the chart leaves out.
                let humid = if rh > 85.0 && t <= 87.0 {
                    (rh - 85.0) / 10.0 * ((87.0 - t) / 5.0)
                } else {
                    0.0
                };
                let got = heat_index_f(t, rh) - humid;
                assert!(
                    (got - f64::from(want)).abs() <= 0.5 + 0.02,
                    "{t} F, {rh} %: chart {want}, computed {got:.2} without the humid adjustment"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 135);
    }

    #[test]
    fn heat_index_adjustments() {
        // Dry heat: the low-humidity adjustment lowers the index.
        let raw = |t: f64, rh: f64| {
            -42.379 + 2.04901523 * t + 10.14333127 * rh
                - 0.22475541 * t * rh
                - 0.00683783 * t * t
                - 0.05481717 * rh * rh
                + 0.00122874 * t * t * rh
                + 0.00085282 * t * rh * rh
                - 0.00000199 * t * t * rh * rh
        };
        assert!(heat_index_f(100.0, 5.0) < raw(100.0, 5.0));
        assert!(heat_index_f(84.0, 95.0) > raw(84.0, 95.0));
        // Mild: the simple formula, close to the air temperature.
        assert!((heat_index_f(80.0, 20.0) - 78.6).abs() < 0.2);
    }

    #[test]
    fn feels_like_branches() {
        let k = |f: f64| (f_to_c(f) + 273.15) as f32;
        let mph = |m: f64| (m / MS_TO_MPH) as f32;
        // 0 F with 15 mph of wind: -19 F on the chart.
        let wc = feels_like_c(k(0.0), k(-10.0), mph(15.0), 0.0);
        assert!((c_to_f(f64::from(wc)) - -19.0).abs() < 0.6, "{wc}");
        // The wind's direction does not matter, only its speed.
        let wc2 = feels_like_c(k(0.0), k(-10.0), mph(9.0), mph(12.0));
        assert!((wc - wc2).abs() < 1e-4);
        // Calm and cold: just the temperature.
        let calm = feels_like_c(k(20.0), k(10.0), mph(2.0), 0.0);
        assert!((c_to_f(f64::from(calm)) - 20.0).abs() < 1e-3);
        // Mild and dry: the heat index (63.1 F) is below the air temperature.
        let mild = feels_like_c(k(65.0), k(40.0), mph(30.0), 0.0);
        assert!((c_to_f(f64::from(mild)) - 65.0).abs() < 1e-3);
        // Mild and humid: it is above, so it counts (0.5 (65 + 61 - 3.6 +
        // 9.4) = 65.9 F at 100 %).
        let humid = feels_like_c(k(65.0), k(65.0), mph(30.0), 0.0);
        assert!((c_to_f(f64::from(humid)) - 65.9).abs() < 1e-3, "{humid}");
        // Hot and very dry: the NWS dry adjustment takes the index below the
        // air temperature, which then stands.
        let dry = apparent_c(k(100.0), || 0.0, || 5.0);
        assert!(heat_index_f(100.0, 5.0) < 100.0);
        assert!((c_to_f(f64::from(dry)) - 100.0).abs() < 1e-3);
        // 90 F at 60 % humidity: 100 F on the chart. The dew point for 60 %
        // at 90 F is about 74.6 F.
        let td = 74.6;
        let rh = relative_humidity(f_to_c(90.0), f_to_c(td));
        assert!((rh - 60.0).abs() < 1.0, "rh {rh}");
        let hi = feels_like_c(k(90.0), k(td), 0.0, 0.0);
        assert!((c_to_f(f64::from(hi)) - 100.0).abs() < 1.5, "{hi}");
        // Missing inputs.
        assert!(feels_like_c(f32::NAN, k(0.0), 0.0, 0.0).is_nan());
        assert!(feels_like_c(k(10.0), k(0.0), f32::NAN, 0.0).is_nan());
        assert!(feels_like_c(k(95.0), f32::NAN, 0.0, 0.0).is_nan());
        assert!(feels_like_c(k(57.0), f32::NAN, 0.0, 0.0).is_nan());
        // Missing dew point does not matter where it cannot change the
        // answer: windy and cold, or below 56 F.
        assert!(!feels_like_c(k(10.0), f32::NAN, 5.0, 0.0).is_nan());
        let close = |got: f32, f: f64| (f64::from(got) - f_to_c(f)).abs() < 1e-4;
        assert!(close(feels_like_c(k(40.0), f32::NAN, 0.5, 0.0), 40.0));
        assert!(close(feels_like_c(k(55.9), f32::NAN, f32::NAN, 0.0), 55.9));
        // Missing wind does not matter above 50 F.
        assert!(!feels_like_c(k(51.0), k(40.0), f32::NAN, 0.0).is_nan());
    }

    #[test]
    fn heat_index_cannot_exceed_the_temperature_below_56_f() {
        for t in [0.0, 30.0, 50.0, 55.0, 55.99] {
            for rh in [0.0, 50.0, 100.0] {
                assert!(heat_index_f(t, rh) <= t, "{t} F {rh} %");
            }
        }
        assert!(heat_index_f(56.1, 100.0) > 56.1);
    }

    #[test]
    fn dew_point_inverts_humidity() {
        for t in [-30.0, -5.0, 0.0, 12.5, 30.0, 45.0] {
            for td_drop in [0.0, 1.0, 10.0, 25.0] {
                let td = t - td_drop;
                let rh = relative_humidity(t, td);
                if rh < 1.0 {
                    continue;
                }
                assert!((dew_point_c(t, rh) - td).abs() < 1e-9, "{t} {td}");
            }
        }
        // Clamped: no dew point above the temperature, none from 0 %.
        assert!((dew_point_c(20.0, 140.0) - 20.0).abs() < 1e-9);
        assert_eq!(dew_point_c(20.0, 0.0), dew_point_c(20.0, 1.0));
        assert!(dew_point_c(20.0, f64::NAN).is_nan());
    }

    #[test]
    fn humidity() {
        assert!((relative_humidity(20.0, 20.0) - 100.0).abs() < 1e-9);
        let rh = relative_humidity(30.0, 10.0);
        assert!((rh - 28.9).abs() < 0.5, "{rh}");
    }
}
