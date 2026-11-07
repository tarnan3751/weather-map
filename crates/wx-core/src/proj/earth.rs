//! The figure of the earth a projection is defined on, and the ellipsoid
//! functions the projections share. Formulas and their numbers are from
//! Snyder, "Map Projections: A Working Manual" (USGS Professional Paper 1395,
//! 1987).

use serde::{Deserialize, Serialize};
use std::f64::consts::FRAC_PI_2;

/// An ellipsoid of revolution, or a sphere when `a == b`. Projections use
/// their spherical formulas for a sphere and the ellipsoidal ones otherwise,
/// so a model grid defined on a sphere (every NOAA model) and a radar or
/// satellite product defined on WGS84 both land where their producers put
/// them. At 1 km resolution the difference is several cells at the edge of a
/// continental grid.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Earth {
    /// Semi-major (equatorial) axis, metres.
    pub a: f64,
    /// Semi-minor (polar) axis, metres.
    pub b: f64,
}

impl Earth {
    pub const WGS84: Earth = Earth {
        a: 6_378_137.0,
        b: 6_356_752.314_245_179,
    };
    /// GRS 1980, the ellipsoid of the GOES-R fixed grid.
    pub const GRS80: Earth = Earth {
        a: 6_378_137.0,
        b: 6_356_752.314_140_356,
    };

    pub const fn sphere(radius: f64) -> Earth {
        Earth {
            a: radius,
            b: radius,
        }
    }

    /// From the semi-major axis and the inverse flattening `1 / f`, the way
    /// CF metadata and PROJ strings state an ellipsoid.
    pub fn from_inverse_flattening(a: f64, inverse_flattening: f64) -> Earth {
        Earth {
            a,
            b: a * (1.0 - 1.0 / inverse_flattening),
        }
    }

    pub fn is_sphere(&self) -> bool {
        self.a == self.b
    }

    /// First eccentricity squared.
    pub fn e2(&self) -> f64 {
        let r = self.b / self.a;
        (1.0 - r * r).max(0.0)
    }

    pub fn e(&self) -> f64 {
        self.e2().sqrt()
    }

    /// Spherical or oblate and the size of the earth: what every real earth
    /// model is. Constructors of stored projections require it, so damaged
    /// parameters are refused instead of producing a projection of nothing.
    pub fn is_plausible(&self) -> bool {
        (6.0e6..=6.5e6).contains(&self.a) && self.b <= self.a && self.b >= 0.99 * self.a
    }
}

/// Snyder (15-9): `tan(pi/4 - phi/2) / ((1 - e sin phi) / (1 + e sin phi))^(e/2)`,
/// the quantity stereographic and Mercator radii are built from.
#[inline]
pub(crate) fn tsfn(phi: f64, e: f64) -> f64 {
    let t = (0.5 * (FRAC_PI_2 - phi)).tan();
    if e == 0.0 {
        return t;
    }
    let es = e * phi.sin();
    t / ((1.0 - es) / (1.0 + es)).powf(0.5 * e)
}

/// The latitude whose [`tsfn`] is `ts` (Snyder 7-9), iterated to full
/// precision. The fixed iteration count keeps it deterministic.
pub(crate) fn phi_from_ts(ts: f64, e: f64) -> f64 {
    let mut phi = FRAC_PI_2 - 2.0 * ts.atan();
    if e == 0.0 {
        return phi;
    }
    for _ in 0..15 {
        let es = e * phi.sin();
        let next = FRAC_PI_2 - 2.0 * (ts * ((1.0 - es) / (1.0 + es)).powf(0.5 * e)).atan();
        if (next - phi).abs() < 1e-15 {
            return next;
        }
        phi = next;
    }
    phi
}

/// Snyder (14-15): `cos phi / sqrt(1 - e^2 sin^2 phi)`.
#[inline]
pub(crate) fn msfn(phi: f64, e2: f64) -> f64 {
    let s = phi.sin();
    phi.cos() / (1.0 - e2 * s * s).sqrt()
}

/// Snyder (3-12): the authalic function `q` of `sin phi`.
#[inline]
pub(crate) fn qsfn(sin_phi: f64, e: f64) -> f64 {
    if e < 1e-12 {
        return 2.0 * sin_phi;
    }
    let es = e * sin_phi;
    (1.0 - e * e) * (sin_phi / (1.0 - es * es) - (0.5 / e) * ((1.0 - es) / (1.0 + es)).ln())
}

/// Coefficients of Snyder (3-18), geodetic latitude from authalic latitude,
/// truncated after e^6 (a few tenths of a millimetre on WGS84): the
/// starting point of [`phi_from_q`].
pub(crate) fn authalic_coefficients(e2: f64) -> [f64; 3] {
    let (e4, e6) = (e2 * e2, e2 * e2 * e2);
    [
        e2 / 3.0 + 31.0 * e4 / 180.0 + 517.0 * e6 / 5040.0,
        23.0 * e4 / 360.0 + 251.0 * e6 / 3780.0,
        761.0 * e6 / 45360.0,
    ]
}

/// The latitude whose [`qsfn`] is `q` (`qp` is `qsfn(1)`): the series
/// (3-18) on the authalic latitude, then Newton's method on (3-12), whose
/// derivative is `2 (1 - e^2) cos phi / (1 - e^2 sin^2 phi)^2`, to full
/// precision. A fixed step limit keeps it deterministic.
pub(crate) fn phi_from_q(q: f64, e: f64, qp: f64, c: &[f64; 3]) -> f64 {
    let beta = (q / qp).clamp(-1.0, 1.0).asin();
    let mut phi =
        beta + c[0] * (2.0 * beta).sin() + c[1] * (4.0 * beta).sin() + c[2] * (6.0 * beta).sin();
    if e < 1e-12 || q.abs() >= qp {
        return phi;
    }
    let e2 = e * e;
    for _ in 0..4 {
        let (s, cos) = phi.sin_cos();
        if cos < 1e-12 {
            break;
        }
        let w = 1.0 - e2 * s * s;
        let step = (qsfn(s, e) - q) * w * w / (2.0 * (1.0 - e2) * cos);
        phi -= step;
        if step.abs() < 1e-15 {
            break;
        }
    }
    phi
}

/// `(cos a, sin a)` of the angle from true north clockwise to the plane's +y
/// axis at a point, measured with the projection itself: the direction in
/// which a small step north moves the point on the plane. Exact for
/// conformal projections; for others it is the rotation that maps true north
/// onto grid north.
pub(crate) fn plane_rotation(
    forward: impl Fn(f64, f64) -> (f64, f64),
    lon: f64,
    lat: f64,
) -> (f64, f64) {
    let d = 1e-5;
    let (lat_a, lat_b) = if lat + d > 90.0 {
        (lat - d, lat)
    } else {
        (lat, lat + d)
    };
    let (x0, y0) = forward(lon, lat_a);
    let (x1, y1) = forward(lon, lat_b);
    let (nx, ny) = (x1 - x0, y1 - y0);
    let r = nx.hypot(ny);
    if !(r.is_finite() && r > 0.0) {
        return (1.0, 0.0);
    }
    (ny / r, -nx / r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipsoid_constants() {
        let wgs = Earth::from_inverse_flattening(6_378_137.0, 298.257_223_563);
        assert!((wgs.b - Earth::WGS84.b).abs() < 1e-6);
        let grs = Earth::from_inverse_flattening(6_378_137.0, 298.257_222_101);
        assert!((grs.b - Earth::GRS80.b).abs() < 1e-6);
        assert!((Earth::WGS84.e() - 0.081_819_190_842_622).abs() < 1e-12);
        assert!(Earth::sphere(6_371_229.0).is_sphere() && Earth::sphere(6e6).e2() == 0.0);
        assert!(Earth::WGS84.is_plausible() && !Earth::sphere(1.0).is_plausible());
        assert!(!Earth { a: 6.3e6, b: 6.4e6 }.is_plausible(), "prolate");
    }

    #[test]
    fn conformal_and_authalic_latitudes_invert() {
        let e2 = Earth::WGS84.e2();
        let e = e2.sqrt();
        let c = authalic_coefficients(e2);
        for deg in [-89.9, -60.0, -12.3, 0.0, 33.3, 55.0, 75.5, 89.99] {
            let phi = f64::to_radians(deg);
            assert!((phi_from_ts(tsfn(phi, e), e) - phi).abs() < 1e-14, "{deg}");
            let qp = qsfn(1.0, e);
            // q flattens towards the poles, so its last bits weigh more there.
            assert!(
                (phi_from_q(qsfn(phi.sin(), e), e, qp, &c) - phi).abs() < 1e-12,
                "{deg}"
            );
        }
        // On a sphere the authalic latitude is the latitude.
        assert_eq!(qsfn(0.5, 0.0), 1.0);
    }
}
