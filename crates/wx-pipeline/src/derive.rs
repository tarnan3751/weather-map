//! Decoded fields to a layer's bands (`wx_core::layer::Derive`). Each rule
//! is the formula its doc comment in `wx_core::layer` states, including
//! what a missing (NaN) input gives.

use anyhow::{Result, bail, ensure};
use rayon::prelude::*;
use wx_core::layer::{
    BRIGHTNESS_MAX_K, BRIGHTNESS_MIN_K, Derive, ECHO_RATE_MIN_MMH, NO_ECHO_DBZ, PtypeTable,
    RAIN_AT_OR_ABOVE_C, RAIN_BELOW_SOLID_SHARE, SNOW_ABOVE_SOLID_SHARE, SNOW_AT_OR_BELOW_C,
    TYPED_RATE_MIN_MMH, ZR_A, ZR_B, ptype,
};
use wx_core::{Field, GridProjection, InputSpec, LayerSpec, Raster};

use crate::thermo::{apparent_c, dew_point_c, feels_like_c, relative_humidity};

/// Values at or below this are MRMS's "no coverage" (-999).
const NO_COVERAGE_DBZ: f32 = -900.0;
/// "No echo" is -99 (MRMS), -10 (HRRR) or -20 (GFS). The small allowance
/// keeps a packed -10 that decodes a hair above it from passing as an echo.
const NO_ECHO_AT_OR_BELOW_DBZ: f32 = -10.0 + 1e-3;
/// Categorical flags are 0 or 1; anything from a half up counts as set.
const FLAG_SET: f32 = 0.5;
/// Kelvin to Celsius.
const ZERO_C_K: f64 = 273.15;
/// [`Derive::McidasIrCount`]: the two linear pieces of the McIDAS
/// brightness-count scale meet here (242 K).
const MCIDAS_KNEE: f64 = 176.0;
const MCIDAS_MAX: f64 = 255.0;
/// Enough work per rayon task to amortise the scheduling.
const MIN_CHUNK: usize = 1 << 14;

pub(crate) fn derive(layer: &LayerSpec, source: &InputSpec, fields: &[Field]) -> Result<Raster> {
    let rule = source.derive;
    match rule.field_count() {
        Some(needed) => ensure!(
            fields.len() == needed,
            "{} ({}): {rule:?} takes {needed} fields, got {}",
            layer.id,
            source.family.as_str(),
            fields.len()
        ),
        None => ensure!(
            !fields.is_empty(),
            "{} ({}): {rule:?} takes one or more fields, got none",
            layer.id,
            source.family.as_str()
        ),
    }
    let first = &fields[0];
    for (k, f) in fields.iter().enumerate() {
        ensure!(
            f.values.len() == f.nx * f.ny,
            "field {k} holds {} values for a {} x {} grid",
            f.values.len(),
            f.nx,
            f.ny
        );
        ensure!(
            f.nx == first.nx && f.ny == first.ny && f.proj == first.proj,
            "field {k} is on a different grid from field 0"
        );
    }
    let v = |k: usize| fields[k].values.as_slice();

    let bands = match rule {
        Derive::Linear { mul, add } => vec![map1(v(0), |x| x * mul + add)],
        Derive::Reflectivity => vec![map1(v(0), reflectivity)],
        Derive::PrecipDepth => vec![map1(v(0), precip_depth)],
        Derive::Wind => {
            let (u, w) = (&fields[0], &fields[1]);
            ensure!(
                u.grid_relative_winds == w.grid_relative_winds,
                "UGRD and VGRD disagree on whether they are grid-relative"
            );
            let (eu, ev) = earth_relative(u, u.values.clone(), w.values.clone());
            vec![eu, ev, fields[2].values.clone()]
        }
        Derive::FeelsLike => vec![map4(v(0), v(1), v(2), v(3), feels_like_c)],
        Derive::FeelsLikeSpeed => vec![map3(v(0), v(1), v(2), |t, td, speed| {
            apparent_c(
                t,
                || f64::from(speed),
                || relative_humidity(f64::from(t) - ZERO_C_K, f64::from(td) - ZERO_C_K),
            )
        })],
        Derive::FeelsLikeRh => vec![map4(v(0), v(1), v(2), v(3), |t, rh, u, w| {
            apparent_c(
                t,
                || f64::from(u).hypot(f64::from(w)),
                || f64::from(rh).clamp(0.0, 100.0),
            )
        })],
        Derive::DewPointFromRh => vec![map2(v(0), v(1), |t, rh| {
            dew_point_c(f64::from(t) - ZERO_C_K, f64::from(rh)) as f32
        })],
        Derive::PrecipTypeRate => {
            let rate = map1(v(0), |x| x * 3600.0);
            let flags = [
                (v(1), ptype::RAIN),
                (v(2), ptype::SNOW),
                (v(3), ptype::SLEET),
                (v(4), ptype::FREEZING_RAIN),
            ];
            let kind = (0..rate.len())
                .into_par_iter()
                .with_min_len(MIN_CHUNK)
                .map(|i| precip_type(rate[i], flags.map(|(f, code)| (f[i], code))))
                .collect();
            vec![rate, kind]
        }
        Derive::WindSpeedDir => {
            let (speed, dir) = (&fields[0], &fields[1]);
            let (u, w): (Vec<f32>, Vec<f32>) = (&speed.values, &dir.values)
                .into_par_iter()
                .with_min_len(MIN_CHUNK)
                .map(|(&s, &d)| wind_from_speed_dir(s, d))
                .unzip();
            // The direction carries the reference frame.
            let (eu, ev) = earth_relative(dir, u, w);
            vec![eu, ev, fields[2].values.clone()]
        }
        Derive::ReflectivityFromRate { to_mmh } => {
            vec![map1(v(0), |x| reflectivity_from_rate(x * to_mmh))]
        }
        Derive::PrecipTypeTemp { to_mmh } => typed(v(0), v(1), to_mmh, |t_k| {
            let t = f64::from(t_k) - ZERO_C_K;
            if t.is_nan() {
                f32::NAN
            } else if t <= f64::from(SNOW_AT_OR_BELOW_C) {
                ptype::SNOW
            } else if t >= f64::from(RAIN_AT_OR_ABOVE_C) {
                ptype::RAIN
            } else {
                ptype::MIX
            }
        }),
        Derive::PrecipTypeCode { table, to_mmh } => {
            typed(v(0), v(1), to_mmh, |code| code_type(table, code))
        }
        Derive::PrecipPhases { to_mmh } => {
            let (total, solid) = (v(0), v(1));
            let rate = map1(total, |x| typed_rate(x * to_mmh));
            let kind = (0..rate.len())
                .into_par_iter()
                .with_min_len(MIN_CHUNK)
                .map(|i| phase_type(rate[i], total[i], solid[i]))
                .collect();
            vec![rate, kind]
        }
        Derive::Sum { mul, add } => {
            let n = fields[0].values.len();
            let sum = (0..n)
                .into_par_iter()
                .with_min_len(MIN_CHUNK)
                .map(|i| fields.iter().map(|f| f.values[i]).sum::<f32>() * mul + add)
                .collect();
            vec![sum]
        }
        Derive::BrightnessTemp => vec![map1(v(0), brightness_c)],
        Derive::McidasIrCount => vec![map1(v(0), mcidas_ir_c)],
        #[allow(unreachable_patterns)]
        other => bail!("{}: derive rule {other:?} is not implemented", layer.id),
    };
    ensure!(
        bands.len() == rule.band_count() && bands.len() == layer.bands.len(),
        "{}: {rule:?} makes {} bands but the layer has {}",
        layer.id,
        bands.len(),
        layer.bands.len()
    );
    Ok(Raster {
        nx: first.nx,
        ny: first.ny,
        proj: first.proj.clone(),
        bands,
    })
}

pub(crate) fn step_per_hour(total: &Field, previous: &Field, hours: f64) -> Result<Field> {
    ensure!(
        hours.is_finite() && hours > 0.0,
        "a step of {hours} hours is not usable"
    );
    for f in [total, previous] {
        ensure!(
            f.values.len() == f.nx * f.ny,
            "field holds {} values for a {} x {} grid",
            f.values.len(),
            f.nx,
            f.ny
        );
    }
    ensure!(
        total.nx == previous.nx && total.ny == previous.ny && total.proj == previous.proj,
        "the two run totals are on different grids"
    );
    let per_hour = (1.0 / hours) as f32;
    Ok(Field {
        nx: total.nx,
        ny: total.ny,
        proj: total.proj.clone(),
        values: map2(&total.values, &previous.values, |a, b| (a - b) * per_hour),
        grid_relative_winds: total.grid_relative_winds,
    })
}

fn map1(a: &[f32], f: impl Fn(f32) -> f32 + Sync) -> Vec<f32> {
    a.par_iter()
        .with_min_len(MIN_CHUNK)
        .map(|&x| f(x))
        .collect()
}

fn map2(a: &[f32], b: &[f32], f: impl Fn(f32, f32) -> f32 + Sync) -> Vec<f32> {
    (a, b)
        .into_par_iter()
        .with_min_len(MIN_CHUNK)
        .map(|(&x, &y)| f(x, y))
        .collect()
}

fn map3(a: &[f32], b: &[f32], c: &[f32], f: impl Fn(f32, f32, f32) -> f32 + Sync) -> Vec<f32> {
    (a, b, c)
        .into_par_iter()
        .with_min_len(MIN_CHUNK)
        .map(|(&x, &y, &z)| f(x, y, z))
        .collect()
}

fn map4(
    a: &[f32],
    b: &[f32],
    c: &[f32],
    d: &[f32],
    f: impl Fn(f32, f32, f32, f32) -> f32 + Sync,
) -> Vec<f32> {
    (a, b, c, d)
        .into_par_iter()
        .with_min_len(MIN_CHUNK)
        .map(|(&x, &y, &z, &w)| f(x, y, z, w))
        .collect()
}

pub(crate) fn reflectivity(v: f32) -> f32 {
    if v.is_nan() || v <= NO_COVERAGE_DBZ {
        f32::NAN
    } else if v <= NO_ECHO_AT_OR_BELOW_DBZ {
        NO_ECHO_DBZ
    } else {
        v
    }
}

pub(crate) fn precip_depth(v: f32) -> f32 {
    if v < 0.0 { f32::NAN } else { v }
}

/// `flags` pairs each categorical field's value with the type code it stands for.
pub(crate) fn precip_type(rate_mmh: f32, flags: [(f32, f32); 4]) -> f32 {
    if rate_mmh.is_nan() {
        return f32::NAN;
    }
    if rate_mmh < TYPED_RATE_MIN_MMH {
        return ptype::NONE;
    }
    // Every flag is needed to tell one type from a mix.
    if flags.iter().any(|(v, _)| v.is_nan()) {
        return f32::NAN;
    }
    let mut set = flags.iter().filter(|(v, _)| *v >= FLAG_SET);
    match (set.next(), set.next()) {
        (None, _) => ptype::NONE,
        (Some(&(_, code)), None) => code,
        (Some(_), Some(_)) => ptype::MIX,
    }
}

/// The rate band of the typed-precipitation rules: `max(rate, 0)` in mm/h,
/// missing where the rate is.
fn typed_rate(mmh: f32) -> f32 {
    if mmh.is_nan() { f32::NAN } else { mmh.max(0.0) }
}

/// Bands `[rate, ptype]` for a rule whose type comes from one more field:
/// NONE below the typing threshold, `kind(other)` from it up, both missing
/// where the rate is.
fn typed(
    rate: &[f32],
    other: &[f32],
    to_mmh: f32,
    kind: impl Fn(f32) -> f32 + Sync,
) -> Vec<Vec<f32>> {
    let rate = map1(rate, |x| typed_rate(x * to_mmh));
    let types = map2(&rate, other, |r, o| {
        if r.is_nan() {
            f32::NAN
        } else if r < TYPED_RATE_MIN_MMH {
            ptype::NONE
        } else {
            kind(o)
        }
    });
    vec![rate, types]
}

/// A provider's type code, rounded to the nearest integer, through its table.
fn code_type(table: PtypeTable, code: f32) -> f32 {
    if code.is_nan() {
        f32::NAN
    } else {
        // Saturates for codes beyond i32, which no table lists.
        table.ptype(code.round() as i32)
    }
}

/// [`Derive::PrecipPhases`]' type from the rate band and the two amounts.
fn phase_type(rate_mmh: f32, total: f32, solid: f32) -> f32 {
    if rate_mmh.is_nan() {
        return f32::NAN;
    }
    if rate_mmh < TYPED_RATE_MIN_MMH {
        return ptype::NONE;
    }
    if solid.is_nan() {
        return f32::NAN;
    }
    let share = (solid / total).clamp(0.0, 1.0);
    if share < RAIN_BELOW_SOLID_SHARE {
        ptype::RAIN
    } else if share > SNOW_ABOVE_SOLID_SHARE {
        ptype::SNOW
    } else {
        ptype::MIX
    }
}

/// Marshall-Palmer reflectivity of a rain rate in mm/h (negative counts as
/// 0); [`NO_ECHO_DBZ`] below [`ECHO_RATE_MIN_MMH`].
pub(crate) fn reflectivity_from_rate(mmh: f32) -> f32 {
    if mmh.is_nan() {
        return f32::NAN;
    }
    let r = f64::from(mmh.max(0.0));
    if r < f64::from(ECHO_RATE_MIN_MMH) {
        return NO_ECHO_DBZ;
    }
    (10.0 * (f64::from(ZR_A) * r.powf(f64::from(ZR_B))).log10()) as f32
}

/// `(u, v)` of a wind blowing from `dir` degrees clockwise from north (or
/// from the grid's +y axis) at `speed`; missing for a missing or negative
/// speed or a missing direction.
fn wind_from_speed_dir(speed: f32, dir: f32) -> (f32, f32) {
    if speed.is_nan() || speed < 0.0 || dir.is_nan() {
        return (f32::NAN, f32::NAN);
    }
    let (sin, cos) = f64::from(dir).to_radians().sin_cos();
    let s = f64::from(speed);
    ((-s * sin) as f32, (-s * cos) as f32)
}

pub(crate) fn brightness_c(k: f32) -> f32 {
    if (BRIGHTNESS_MIN_K..=BRIGHTNESS_MAX_K).contains(&k) {
        (f64::from(k) - ZERO_C_K) as f32
    } else {
        f32::NAN
    }
}

pub(crate) fn mcidas_ir_c(count: f32) -> f32 {
    let b = f64::from(count);
    if !(0.0..=MCIDAS_MAX).contains(&b) {
        return f32::NAN;
    }
    let k = if b <= MCIDAS_KNEE {
        330.0 - b / 2.0
    } else {
        418.0 - b
    };
    (k - ZERO_C_K) as f32
}

/// Rotates grid-relative `u`/`v` to east/north where `frame` says they are
/// grid-relative and its grid's north is not true north; otherwise passes
/// them through.
fn earth_relative(frame: &Field, u: Vec<f32>, v: Vec<f32>) -> (Vec<f32>, Vec<f32>) {
    let proj: &GridProjection = &frame.proj;
    if !(frame.grid_relative_winds && proj.rotates_winds()) {
        return (u, v);
    }
    let nx = frame.nx;
    let mut eu = vec![0.0f32; u.len()];
    let mut ev = vec![0.0f32; u.len()];
    eu.par_chunks_mut(nx)
        .zip(ev.par_chunks_mut(nx))
        .enumerate()
        .for_each(|(j, (eu_row, ev_row))| {
            for i in 0..nx {
                let k = j * nx + i;
                let (lon, lat) = proj.ij_to_lonlat(i as f64, j as f64);
                let (c, s) = proj.wind_rotation(lon, lat);
                let (gu, gv) = (f64::from(u[k]), f64::from(v[k]));
                eu_row[i] = (c * gu + s * gv) as f32;
                ev_row[i] = (-s * gu + c * gv) as f32;
            }
        });
    (eu, ev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wx_core::layer::Derive;
    use wx_core::layer::layer;
    use wx_core::proj::{EARTH_RADIUS_NCEP, LambertGrid, LatLonGrid};
    use wx_core::{Family, Field, GridProjection};

    /// An input with the rule under test, independent of the registry's rows.
    fn rule(derive: Derive) -> InputSpec {
        InputSpec {
            family: Family::Hrrr,
            sources: &[],
            product: "",
            fields: &[],
            derive,
            hold_minutes: 60,
            fill_past: false,
        }
    }

    fn latlon_field(values: Vec<f32>) -> Field {
        Field {
            nx: values.len(),
            ny: 1,
            proj: GridProjection::LatLon(LatLonGrid {
                lon0: 0.0,
                lat0: 0.0,
                dlon: 1.0,
                dlat: 1.0,
                periodic: false,
            }),
            values,
            grid_relative_winds: false,
        }
    }

    fn same(a: f32, b: f32) -> bool {
        (a.is_nan() && b.is_nan()) || (a - b).abs() < 1e-6
    }

    #[test]
    fn reflectivity_sentinels() {
        let input = [
            -999.0,
            -950.0,
            -99.0,
            -20.0,
            -20.000004,
            -10.0,
            -9.5,
            0.0,
            31.4,
            f32::NAN,
        ];
        let want = [
            f32::NAN,
            f32::NAN,
            -30.0,
            -30.0,
            -30.0,
            -30.0,
            -9.5,
            0.0,
            31.4,
            f32::NAN,
        ];
        for (v, w) in input.iter().zip(want) {
            assert!(same(reflectivity(*v), w), "{v} -> {}", reflectivity(*v));
        }
        let radar = layer("radar").unwrap();
        let raster = derive(
            radar,
            &rule(Derive::Reflectivity),
            &[latlon_field(input.to_vec())],
        )
        .unwrap();
        for (got, w) in raster.bands[0].iter().zip(want) {
            assert!(same(*got, w));
        }
    }

    #[test]
    fn precip_depth_sentinels() {
        let rain = layer("rain_1h").unwrap();
        let input = vec![-3.0, -0.001, 0.0, 0.7, 60.7, f32::NAN];
        let raster = derive(rain, &rule(Derive::PrecipDepth), &[latlon_field(input)]).unwrap();
        let want = [f32::NAN, f32::NAN, 0.0, 0.7, 60.7, f32::NAN];
        for (got, w) in raster.bands[0].iter().zip(want) {
            assert!(same(*got, w), "{got} vs {w}");
        }
    }

    #[test]
    fn linear() {
        let temp = layer("temp").unwrap();
        let raster = derive(
            temp,
            &rule(Derive::Linear {
                mul: 1.0,
                add: -273.15,
            }),
            &[latlon_field(vec![273.15, 300.0, f32::NAN])],
        )
        .unwrap();
        assert!(same(raster.bands[0][0], 0.0) && (raster.bands[0][1] - 26.85).abs() < 1e-4);
        assert!(raster.bands[0][2].is_nan());
        let snow = layer("snow").unwrap();
        let raster = derive(
            snow,
            &rule(Derive::Linear {
                mul: 1000.0,
                add: 0.0,
            }),
            &[latlon_field(vec![0.25])],
        )
        .unwrap();
        assert!(
            (raster.bands[0][0] - 250.0).abs() < 1e-3,
            "metres of snow to millimetres"
        );
    }

    #[test]
    fn precip_types() {
        let none = [
            (0.0, ptype::RAIN),
            (0.0, ptype::SNOW),
            (0.0, ptype::SLEET),
            (0.0, ptype::FREEZING_RAIN),
        ];
        let with = |k: usize| {
            let mut f = none;
            f[k].0 = 1.0;
            f
        };
        assert_eq!(precip_type(2.0, with(0)), ptype::RAIN);
        assert_eq!(precip_type(2.0, with(1)), ptype::SNOW);
        assert_eq!(precip_type(2.0, with(2)), ptype::SLEET);
        assert_eq!(precip_type(2.0, with(3)), ptype::FREEZING_RAIN);
        let mut mix = with(0);
        mix[1].0 = 1.0;
        assert_eq!(precip_type(2.0, mix), ptype::MIX);
        assert_eq!(
            precip_type(0.009, with(1)),
            ptype::NONE,
            "too light to have a type"
        );
        assert_eq!(precip_type(0.01, with(1)), ptype::SNOW);
        assert_eq!(precip_type(2.0, none), ptype::NONE, "rate without any flag");
        assert!(precip_type(f32::NAN, with(1)).is_nan());

        let precip = layer("precip").unwrap();
        let src = &rule(Derive::PrecipTypeRate);
        // PRATE in kg m-2 s-1: 1 mm/h is 1/3600.
        let fields = [
            latlon_field(vec![1.0 / 3600.0, 0.0]),
            latlon_field(vec![0.0, 0.0]),
            latlon_field(vec![1.0, 1.0]),
            latlon_field(vec![0.0, 0.0]),
            latlon_field(vec![0.0, 0.0]),
        ];
        let r = derive(precip, src, &fields).unwrap();
        assert!((r.bands[0][0] - 1.0).abs() < 1e-5);
        assert_eq!(r.bands[1], vec![ptype::SNOW, ptype::NONE]);
    }

    #[test]
    fn rejects_mismatched_inputs() {
        let wind = layer("wind").unwrap();
        let src = &rule(Derive::Wind);
        let f = latlon_field(vec![1.0, 2.0]);
        assert!(
            derive(wind, src, &[f.clone(), f.clone()]).is_err(),
            "too few fields"
        );
        let mut other = f.clone();
        other.nx = 1;
        other.ny = 2;
        assert!(
            derive(wind, src, &[f.clone(), f.clone(), other]).is_err(),
            "different grids"
        );
        let mut short = f.clone();
        short.values.pop();
        assert!(
            derive(wind, src, &[f.clone(), f, short]).is_err(),
            "wrong value count"
        );
    }

    fn hrrr() -> GridProjection {
        GridProjection::Lambert(LambertGrid::new(
            38.5,
            38.5,
            262.5,
            21.138123,
            237.280472,
            3000.0,
            3000.0,
            EARTH_RADIUS_NCEP,
        ))
    }

    /// The direction of true north and east on the grid, measured with the
    /// projection itself rather than the rotation formula.
    fn grid_directions(proj: &GridProjection, i: f64, j: f64) -> ([f64; 2], [f64; 2]) {
        let (lon, lat) = proj.ij_to_lonlat(i, j);
        let d = 1e-4;
        let unit = |(a, b): (f64, f64)| {
            let (di, dj) = (a - i, b - j);
            let n = di.hypot(dj);
            [di / n, dj / n]
        };
        let north = unit(proj.lonlat_to_ij(lon, lat + d));
        let east = unit(proj.lonlat_to_ij(lon + d / lat.to_radians().cos(), lat));
        (north, east)
    }

    #[test]
    fn grid_relative_winds_become_earth_relative() {
        let proj = hrrr();
        let wind = layer("wind").unwrap();
        let src = &rule(Derive::Wind);
        // Points near New York, Seattle and on the orientation meridian.
        let points = [(1500.0, 600.0), (150.0, 900.0), (899.0, 500.0)];
        for (i, j) in points {
            let (north, east) = grid_directions(&proj, i, j);
            for (dir, want) in [(north, (0.0, 1.0)), (east, (1.0, 0.0))] {
                let field = |c: f64| Field {
                    nx: 1,
                    ny: 1,
                    // A one-point grid placed at (i, j) of the HRRR grid.
                    proj: {
                        let (lon, lat) = proj.ij_to_lonlat(i, j);
                        GridProjection::Lambert(LambertGrid::new(
                            38.5,
                            38.5,
                            262.5,
                            lat,
                            lon,
                            3000.0,
                            3000.0,
                            EARTH_RADIUS_NCEP,
                        ))
                    },
                    values: vec![c as f32],
                    grid_relative_winds: true,
                };
                let r = derive(wind, src, &[field(dir[0]), field(dir[1]), field(0.0)]).unwrap();
                let (u, v) = (f64::from(r.bands[0][0]), f64::from(r.bands[1][0]));
                assert!(
                    (u - want.0).abs() < 1e-3 && (v - want.1).abs() < 1e-3,
                    "at ({i}, {j}) grid vector {dir:?} became ({u}, {v}), want {want:?}"
                );
            }
        }
        // East of the orientation meridian true north leans toward grid west.
        let (north, _) = grid_directions(&proj, 1500.0, 600.0);
        assert!(north[0] < -0.1, "{north:?}");
    }

    #[test]
    fn earth_relative_winds_pass_through() {
        let wind = layer("wind").unwrap();
        let src = &rule(Derive::Wind);
        let mut f = Field {
            nx: 1,
            ny: 1,
            proj: hrrr(),
            values: vec![1.5],
            grid_relative_winds: false,
        };
        let r = derive(wind, src, &[f.clone(), f.clone(), f.clone()]).unwrap();
        assert_eq!(
            (r.bands[0][0], r.bands[1][0], r.bands[2][0]),
            (1.5, 1.5, 1.5)
        );
        f.grid_relative_winds = true;
        let mut v = f.clone();
        v.grid_relative_winds = false;
        assert!(derive(wind, src, &[f.clone(), v, f]).is_err());
    }

    // -----------------------------------------------------------------------
    // The rules added for providers outside NOAA
    // -----------------------------------------------------------------------

    /// One band of a rule's output for `inputs`, one field per input.
    fn run(layer_id: &str, d: Derive, inputs: &[Vec<f32>]) -> Vec<Vec<f32>> {
        let fields: Vec<Field> = inputs.iter().map(|v| latlon_field(v.clone())).collect();
        derive(layer(layer_id).unwrap(), &rule(d), &fields)
            .unwrap()
            .bands
    }

    fn close(a: f32, b: f64, tol: f64) -> bool {
        (a.is_nan() && b.is_nan()) || (f64::from(a) - b).abs() <= tol
    }

    const K: f32 = 273.15;

    #[test]
    fn wind_from_speed_and_direction() {
        let s2 = std::f64::consts::FRAC_1_SQRT_2 * 10.0;
        // From the north, east, south-west, west; then a missing direction,
        // a negative speed and a missing speed.
        let speed = vec![10.0, 10.0, 10.0, 4.0, 10.0, -1.0, f32::NAN];
        let dir = vec![0.0, 90.0, 225.0, 270.0, f32::NAN, 90.0, 90.0];
        let gust = vec![12.0, f32::NAN, 13.0, 5.0, 6.0, 7.0, 8.0];
        let b = run("wind", Derive::WindSpeedDir, &[speed, dir, gust.clone()]);
        let want_u = [0.0, -10.0, s2, 4.0, f64::NAN, f64::NAN, f64::NAN];
        let want_v = [-10.0, 0.0, s2, 0.0, f64::NAN, f64::NAN, f64::NAN];
        for k in 0..7 {
            assert!(close(b[0][k], want_u[k], 1e-5), "u[{k}] {}", b[0][k]);
            assert!(close(b[1][k], want_v[k], 1e-5), "v[{k}] {}", b[1][k]);
            assert!(same(b[2][k], gust[k]), "gust passes through as it is");
        }
    }

    #[test]
    fn grid_relative_speed_and_direction_are_rotated_like_components() {
        let wind = layer("wind").unwrap();
        let on_grid = |values: Vec<f32>, grid_relative: bool| Field {
            nx: values.len(),
            ny: 1,
            proj: hrrr(),
            values,
            grid_relative_winds: grid_relative,
        };
        // Far east of the orientation meridian, where grid north leans.
        let dirs = [0.0f32, 45.0, 200.0, 300.0];
        let n = dirs.len();
        let speed = on_grid(vec![8.0; n], true);
        let dir = on_grid(dirs.to_vec(), true);
        let gust = on_grid(vec![9.0; n], true);
        let sd = derive(
            wind,
            &rule(Derive::WindSpeedDir),
            &[speed, dir, gust.clone()],
        )
        .unwrap();
        // The same wind as grid components, through the component rule.
        let (gu, gv): (Vec<f32>, Vec<f32>) = dirs
            .iter()
            .map(|d| {
                let (s, c) = f64::from(*d).to_radians().sin_cos();
                ((-8.0 * s) as f32, (-8.0 * c) as f32)
            })
            .unzip();
        let uv = derive(
            wind,
            &rule(Derive::Wind),
            &[
                on_grid(gu.clone(), true),
                on_grid(gv.clone(), true),
                gust.clone(),
            ],
        )
        .unwrap();
        for k in 0..n {
            assert!((sd.bands[0][k] - uv.bands[0][k]).abs() < 1e-5);
            assert!((sd.bands[1][k] - uv.bands[1][k]).abs() < 1e-5);
        }
        assert!((sd.bands[0][0] - gu[0]).abs() > 1e-3, "rotated at all");
        // Flagged earth-relative: no rotation.
        let er = derive(
            wind,
            &rule(Derive::WindSpeedDir),
            &[
                on_grid(vec![8.0; n], false),
                on_grid(dirs.to_vec(), false),
                gust,
            ],
        )
        .unwrap();
        assert!((er.bands[0][0] - gu[0]).abs() < 1e-6 && (er.bands[1][0] - gv[0]).abs() < 1e-6);
    }

    #[test]
    fn feels_like_from_a_speed_or_a_humidity() {
        let t = vec![K - 15.0, K + 2.0, K + 18.0, K + 32.0, K - 15.0, K + 32.0];
        let td = vec![K - 20.0, K - 3.0, K + 10.0, K + 24.0, K - 20.0, K + 24.0];
        let speed = vec![6.0, 1.0, 0.0, 2.0, f32::NAN, 2.0];
        let from_uv = run(
            "feels_like",
            Derive::FeelsLike,
            &[t.clone(), td.clone(), speed.clone(), vec![0.0; 6]],
        );
        let from_speed = run(
            "feels_like",
            Derive::FeelsLikeSpeed,
            &[t.clone(), td.clone(), speed.clone()],
        );
        assert!(
            from_uv[0]
                .iter()
                .zip(&from_speed[0])
                .all(|(a, b)| same(*a, *b))
        );
        assert!(
            from_speed[0][4].is_nan(),
            "cold, so the missing wind is needed"
        );
        // The humidity the dew points give, then clamped extremes.
        let rh: Vec<f32> = t
            .iter()
            .zip(&td)
            .map(|(&t, &td)| {
                relative_humidity(f64::from(t) - 273.15, f64::from(td) - 273.15) as f32
            })
            .collect();
        let from_rh = run(
            "feels_like",
            Derive::FeelsLikeRh,
            &[t.clone(), rh, speed.clone(), vec![0.0; 6]],
        );
        for k in 0..6 {
            assert!(close(from_rh[0][k], f64::from(from_uv[0][k]), 1e-3), "{k}");
        }
        let hot = vec![K + 35.0; 3];
        let rh = vec![130.0, 100.0, f32::NAN];
        let b = run(
            "feels_like",
            Derive::FeelsLikeRh,
            &[hot, rh, vec![1.0; 3], vec![0.0; 3]],
        );
        assert_eq!(b[0][0], b[0][1], "humidity is clamped to 100");
        assert!(b[0][0] > 50.0 && b[0][2].is_nan());
        // 31 C (87.8 F) at 70 %: about 100 F on the NWS chart.
        let b = run(
            "feels_like",
            Derive::FeelsLikeRh,
            &[vec![K + 31.0], vec![70.0], vec![0.0], vec![0.0]],
        );
        assert!(close(b[0][0], (100.0 - 32.0) / 1.8, 1.0), "{}", b[0][0]);
    }

    #[test]
    fn dew_point_from_humidity() {
        let t = vec![
            K + 20.0,
            K + 20.0,
            K + 20.0,
            K - 10.0,
            K + 20.0,
            f32::NAN,
            K + 20.0,
        ];
        let rh = vec![50.0, 100.0, 140.0, 80.0, 0.0, 50.0, f32::NAN];
        let b = run("dew_point", Derive::DewPointFromRh, &[t, rh]);
        // g = ln(rh / 100) + 17.67 t / (t + 243.5); td = 243.5 g / (17.67 - g).
        let td = |t: f64, rh: f64| {
            let g = (rh / 100.0).ln() + 17.67 * t / (t + 243.5);
            243.5 * g / (17.67 - g)
        };
        let want = [
            td(20.0, 50.0),
            20.0,
            20.0,
            td(-10.0, 80.0),
            td(20.0, 1.0),
            f64::NAN,
            f64::NAN,
        ];
        for k in 0..want.len() {
            assert!(
                close(b[0][k], want[k], 1e-4),
                "[{k}] {} vs {}",
                b[0][k],
                want[k]
            );
        }
        assert!(
            (want[0] - 9.27).abs() < 0.01,
            "20 C at 50 % is a 9.3 C dew point"
        );
    }

    #[test]
    fn reflectivity_from_rain_rate() {
        // kg m-2 s-1: 3600 per mm/h.
        let per_s = |mmh: f32| mmh / 3600.0;
        let rates = vec![
            per_s(1.0),
            per_s(10.0),
            per_s(100.0),
            per_s(0.1),
            per_s(0.0999),
            0.0,
            per_s(-5.0),
            f32::NAN,
        ];
        let b = run(
            "radar",
            Derive::ReflectivityFromRate { to_mmh: 3600.0 },
            &[rates],
        );
        let want = [
            23.0103,
            39.0103,
            55.0103,
            7.0103,
            -30.0,
            -30.0,
            -30.0,
            f64::NAN,
        ];
        for k in 0..want.len() {
            assert!(close(b[0][k], want[k], 2e-3), "[{k}] {}", b[0][k]);
        }
        // Already in mm/h.
        let b = run(
            "radar",
            Derive::ReflectivityFromRate { to_mmh: 1.0 },
            &[vec![1.0, 0.05]],
        );
        assert!(close(b[0][0], 23.0103, 1e-3) && b[0][1] == NO_ECHO_DBZ);
    }

    #[test]
    fn precip_type_from_temperature() {
        let mmh = |r: f32| r / 3600.0;
        let rate = vec![
            mmh(1.0),
            mmh(1.0),
            mmh(1.0),
            mmh(1.0),
            mmh(1.0),
            mmh(0.009),
            mmh(1.0),
            mmh(0.009),
            f32::NAN,
            mmh(-2.0),
        ];
        let t = vec![
            K - 5.0,
            K + 0.49,
            K + 0.51,
            K + 1.99,
            K + 2.01,
            K - 5.0,
            f32::NAN,
            f32::NAN,
            K - 5.0,
            K - 5.0,
        ];
        let b = run(
            "precip",
            Derive::PrecipTypeTemp { to_mmh: 3600.0 },
            &[rate, t],
        );
        use ptype::*;
        let want_type = [
            SNOW,
            SNOW,
            MIX,
            MIX,
            RAIN,
            NONE,
            f32::NAN,
            NONE,
            f32::NAN,
            NONE,
        ];
        let want_rate = [1.0, 1.0, 1.0, 1.0, 1.0, 0.009, 1.0, 0.009, f64::NAN, 0.0];
        for k in 0..want_type.len() {
            assert!(same(b[1][k], want_type[k]), "type [{k}] {}", b[1][k]);
            assert!(close(b[0][k], want_rate[k], 1e-6), "rate [{k}] {}", b[0][k]);
        }
    }

    #[test]
    fn precip_type_from_provider_codes() {
        use ptype::*;
        let rate = vec![2.0; 12];
        let codes = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 4.6, 4.4, 42.0];
        let b = run(
            "precip",
            Derive::PrecipTypeCode {
                table: PtypeTable::Eccc,
                to_mmh: 1.0,
            },
            &[rate, codes],
        );
        assert_eq!(
            b[1],
            [
                RAIN,
                MIX,
                FREEZING_RAIN,
                SLEET,
                SNOW,
                NONE,
                RAIN,
                FREEZING_RAIN,
                MIX,
                SNOW,
                SLEET,
                NONE
            ]
        );
        // Below the threshold there is no type, whatever the code says;
        // above it a missing code is a missing type; a missing rate is
        // missing in both bands.
        let b = run(
            "precip",
            Derive::PrecipTypeCode {
                table: PtypeTable::WmoWw,
                to_mmh: 3600.0,
            },
            &[
                vec![
                    1.0 / 3600.0,
                    1.0 / 3600.0,
                    0.001 / 3600.0,
                    1.0 / 3600.0,
                    f32::NAN,
                ],
                vec![71.0, 66.0, f32::NAN, f32::NAN, 71.0],
            ],
        );
        assert!(same(b[1][0], SNOW) && same(b[1][1], FREEZING_RAIN) && same(b[1][2], NONE));
        assert!(b[1][3].is_nan() && b[1][4].is_nan() && b[0][4].is_nan());
        let b = run(
            "precip",
            Derive::PrecipTypeCode {
                table: PtypeTable::Ecmwf,
                to_mmh: 1.0,
            },
            &[vec![1.0; 3], vec![8.0, 12.0, 7.0]],
        );
        assert_eq!(b[1], [SLEET, FREEZING_RAIN, MIX]);
    }

    #[test]
    fn precip_type_from_the_solid_share() {
        use ptype::*;
        let total = vec![
            4.0,
            4.0,
            4.0,
            4.0,
            4.0,
            4.0,
            4.0,
            0.005,
            4.0,
            f32::NAN,
            -1.0,
        ];
        let solid = vec![
            0.0,
            0.8,
            1.0,
            3.0,
            3.2,
            9.0,
            -1.0,
            0.005,
            f32::NAN,
            1.0,
            0.0,
        ];
        let b = run(
            "precip",
            Derive::PrecipPhases { to_mmh: 1.0 },
            &[total, solid],
        );
        // Shares 0, 0.2, 0.25, 0.75, 0.8, above 1, below 0.
        let want = [
            RAIN,
            RAIN,
            MIX,
            MIX,
            SNOW,
            SNOW,
            RAIN,
            NONE,
            f32::NAN,
            f32::NAN,
            NONE,
        ];
        for (k, w) in want.iter().enumerate() {
            assert!(same(b[1][k], *w), "type [{k}] {}", b[1][k]);
        }
        assert!(b[0][9].is_nan() && b[0][10] == 0.0 && b[0][0] == 4.0);
        // Amounts per hour in metres (`to_mmh` 1000).
        let b = run(
            "precip",
            Derive::PrecipPhases { to_mmh: 1000.0 },
            &[vec![0.002], vec![0.002]],
        );
        assert!(close(b[0][0], 2.0, 1e-5) && b[1][0] == SNOW);
    }

    #[test]
    fn sums() {
        let b = run(
            "rain_1h",
            Derive::Sum {
                mul: 1000.0,
                add: 0.0,
            },
            &[
                vec![0.001, 0.002, f32::NAN],
                vec![0.003, f32::NAN, 0.0],
                vec![0.0, 0.0, 0.0],
            ],
        );
        assert!(close(b[0][0], 4.0, 1e-4) && b[0][1].is_nan() && b[0][2].is_nan());
        let b = run("temp", Derive::Sum { mul: 2.0, add: 1.0 }, &[vec![3.0]]);
        assert_eq!(b[0][0], 7.0);
        let temp = layer("temp").unwrap();
        assert!(derive(temp, &rule(Derive::Sum { mul: 1.0, add: 0.0 }), &[]).is_err());
    }

    #[test]
    fn brightness_temperatures() {
        let b = run(
            "temp",
            Derive::BrightnessTemp,
            &[vec![150.0, 350.0, 273.15, 149.9, 350.1, f32::NAN]],
        );
        let want = [-123.15, 76.85, 0.0, f64::NAN, f64::NAN, f64::NAN];
        for k in 0..want.len() {
            assert!(close(b[0][k], want[k], 1e-4), "[{k}] {}", b[0][k]);
        }
        // McIDAS counts: two straight pieces meeting at 176 (242 K), not
        // rounded.
        let counts = vec![0.0, 176.0, 177.0, 255.0, 100.5, -0.1, 255.5, f32::NAN];
        let b = run("temp", Derive::McidasIrCount, &[counts]);
        let want = [
            330.0,
            242.0,
            241.0,
            163.0,
            279.75,
            f64::NAN,
            f64::NAN,
            f64::NAN,
        ];
        for k in 0..want.len() {
            assert!(close(b[0][k], want[k] - 273.15, 1e-4), "[{k}] {}", b[0][k]);
        }
    }

    #[test]
    fn missing_flags_leave_the_type_unknown() {
        use ptype::*;
        let set = (1.0, SNOW);
        let unset = (0.0, RAIN);
        let missing = (f32::NAN, SLEET);
        assert!(precip_type(2.0, [set, unset, missing, unset]).is_nan());
        assert_eq!(precip_type(0.001, [set, unset, missing, unset]), NONE);
        assert_eq!(precip_type(2.0, [set, unset, unset, unset]), SNOW);
    }

    #[test]
    fn rules_check_their_field_count() {
        let precip = layer("precip").unwrap();
        let f = latlon_field(vec![1.0]);
        assert!(
            derive(
                precip,
                &rule(Derive::PrecipPhases { to_mmh: 1.0 }),
                std::slice::from_ref(&f)
            )
            .is_err()
        );
        assert!(
            derive(
                precip,
                &rule(Derive::PrecipPhases { to_mmh: 1.0 }),
                &[f.clone(), f.clone()]
            )
            .is_ok()
        );
        let temp = layer("temp").unwrap();
        assert!(
            derive(
                temp,
                &rule(Derive::PrecipPhases { to_mmh: 1.0 }),
                &[f.clone(), f]
            )
            .is_err(),
            "two bands for a one-band layer"
        );
    }

    #[test]
    fn last_hour_from_two_run_totals() {
        let now = latlon_field(vec![5.0, 2.0, f32::NAN, 1.0, 3.0]);
        let before = latlon_field(vec![3.0, 2.0, 1.0, f32::NAN, 3.000_01]);
        let h = step_per_hour(&now, &before, 1.0).unwrap();
        assert_eq!(h.values[..2], [2.0, 0.0]);
        assert!(h.values[2].is_nan() && h.values[3].is_nan());
        assert!(h.values[4] < 0.0, "packing noise is passed on");
        let h = step_per_hour(&now, &before, 3.0).unwrap();
        assert!((h.values[0] - 2.0 / 3.0).abs() < 1e-6);
        assert!(step_per_hour(&now, &before, 0.0).is_err());
        let other = latlon_field(vec![1.0; 4]);
        assert!(step_per_hour(&now, &other, 1.0).is_err());
    }

    /// Every rule the registry uses runs on its own layer, with as many
    /// fields as its input lists (or the one a single-message product
    /// gives), and makes the layer's bands.
    #[test]
    fn every_registry_rule_derives() {
        let mut rules = std::collections::BTreeSet::new();
        for l in wx_core::LAYERS {
            for input in l.inputs {
                let n = input.fields.len().max(1);
                let fields: Vec<Field> = (0..n)
                    .map(|_| latlon_field(vec![280.0, 1.0, f32::NAN]))
                    .collect();
                let r = derive(l, input, &fields)
                    .unwrap_or_else(|e| panic!("{} ({:?}): {e:#}", l.id, input.family));
                assert_eq!(r.bands.len(), l.bands.len(), "{}", l.id);
                rules.insert(
                    format!("{:?}", input.derive)
                        .split([' ', '{'])
                        .next()
                        .unwrap()
                        .to_string(),
                );
            }
        }
        assert!(rules.len() >= 10, "{rules:?}");
    }
}
