//! Lambert azimuthal equal-area grids (PROJ `+proj=laea`), oblique or
//! polar, on a sphere or an ellipsoid: the pan-European OPERA radar
//! composite uses one centred at 55 N 10 E on WGS84. Snyder 24-13 to 24-21
//! and 3-18.

use serde::{Deserialize, Serialize};

use super::earth::{authalic_coefficients, msfn, phi_from_q, plane_rotation, qsfn};
use super::{Earth, Projection, Terms, wrap_lon};

super::defined_by!(LaeaGrid, LaeaParams, LaeaGrid::from_projected, {
    earth: Earth,
    lat0: f64,
    lon0: f64,
    x0: f64,
    y0: f64,
    dx: f64,
    dy: f64,
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "LaeaParams", into = "LaeaParams")]
pub struct LaeaGrid {
    pub earth: Earth,
    /// Centre of the projection (PROJ `lat_0`, `lon_0`), degrees.
    pub lat0: f64,
    pub lon0: f64,
    /// Plane coordinates of sample `(0, 0)`, metres from the centre.
    pub x0: f64,
    pub y0: f64,
    pub dx: f64,
    pub dy: f64,
    e: f64,
    qp: f64,
    /// Radius of the sphere of equal area, `a sqrt(qp / 2)`.
    rq: f64,
    d: f64,
    sin_b1: f64,
    cos_b1: f64,
    authalic: [f64; 3],
}

impl LaeaGrid {
    /// From the plane coordinates of sample `(0, 0)` (PROJ's coordinates
    /// with false easting and northing removed).
    pub fn from_projected(
        earth: Earth,
        lat0: f64,
        lon0: f64,
        x0: f64,
        y0: f64,
        dx: f64,
        dy: f64,
    ) -> Self {
        let e = earth.e();
        let qp = qsfn(1.0, e);
        let rq = earth.a * (0.5 * qp).sqrt();
        let phi1 = lat0.to_radians();
        let beta1 = (qsfn(phi1.sin(), e) / qp).clamp(-1.0, 1.0).asin();
        let (sin_b1, cos_b1) = beta1.sin_cos();
        // D (24-20) is 0/0 at the poles, where its limit is 1.
        let d = if cos_b1 < 1e-10 {
            1.0
        } else {
            earth.a * msfn(phi1, e * e) / (rq * cos_b1)
        };
        LaeaGrid {
            earth,
            lat0,
            lon0,
            x0,
            y0,
            dx,
            dy,
            e,
            qp,
            rq,
            d,
            sin_b1,
            cos_b1,
            authalic: authalic_coefficients(e * e),
        }
    }

    pub fn from_first_point(
        earth: Earth,
        lat0: f64,
        lon0: f64,
        first_lat: f64,
        first_lon: f64,
        dx: f64,
        dy: f64,
    ) -> Self {
        let probe = LaeaGrid::from_projected(earth, lat0, lon0, 0.0, 0.0, 1.0, 1.0);
        let (x0, y0) = probe.forward(first_lon, first_lat);
        LaeaGrid::from_projected(earth, lat0, lon0, x0, y0, dx, dy)
    }

    pub fn forward(&self, lon: f64, lat: f64) -> (f64, f64) {
        self.plane(
            &Projection::lon_terms(self, lon),
            &Projection::lat_terms(self, lat),
        )
    }

    /// Snyder (24-17) to (24-19). The antipode of the centre has no image.
    #[inline]
    fn plane(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        let (sin_d, cos_d) = (lon[0], lon[1]);
        let (sin_b, cos_b) = (lat[0], lat[1]);
        let k = 1.0 + self.sin_b1 * sin_b + self.cos_b1 * cos_b * cos_d;
        if k <= 1e-12 {
            return (f64::NAN, f64::NAN);
        }
        let b = self.rq * (2.0 / k).sqrt();
        (
            b * self.d * cos_b * sin_d,
            (b / self.d) * (self.cos_b1 * sin_b - self.sin_b1 * cos_b * cos_d),
        )
    }

    /// Snyder (24-28) to (24-31).
    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (xd, yd) = (x / self.d, y * self.d);
        let rho = xd.hypot(yd);
        if rho < 1e-9 {
            return (wrap_lon(self.lon0), self.lat0);
        }
        let s = rho / (2.0 * self.rq);
        if s > 1.0 {
            return (f64::NAN, f64::NAN);
        }
        let ce = 2.0 * s.asin();
        let (sin_ce, cos_ce) = ce.sin_cos();
        let q = self.qp * (cos_ce * self.sin_b1 + yd * sin_ce * self.cos_b1 / rho);
        let lat = phi_from_q(q, self.e, self.qp, &self.authalic);
        let dl = (x * sin_ce)
            .atan2(self.d * rho * self.cos_b1 * cos_ce - self.d * yd * self.sin_b1 * sin_ce);
        (wrap_lon(self.lon0 + dl.to_degrees()), lat.to_degrees())
    }

    pub(crate) fn params(&self) -> Vec<f64> {
        vec![
            self.earth.a,
            self.earth.b,
            self.lat0,
            self.lon0,
            self.x0,
            self.y0,
            self.dx,
            self.dy,
        ]
    }

    pub(crate) fn from_params(p: &[f64]) -> Option<Self> {
        let &[a, b, lat0, lon0, x0, y0, dx, dy] = p else {
            return None;
        };
        let earth = Earth { a, b };
        let ok = p.iter().all(|v| v.is_finite())
            && earth.is_plausible()
            && lat0.abs() <= 90.0
            && dx != 0.0
            && dy != 0.0;
        ok.then(|| LaeaGrid::from_projected(earth, lat0, lon0, x0, y0, dx, dy))
    }
}

impl Projection for LaeaGrid {
    #[inline]
    fn lon_terms(&self, lon: f64) -> Terms {
        let d = wrap_lon(lon - self.lon0).to_radians();
        [d.sin(), d.cos()]
    }

    /// Sine and cosine of the authalic latitude.
    #[inline]
    fn lat_terms(&self, lat: f64) -> Terms {
        let q = qsfn(lat.to_radians().sin(), self.e);
        let sin_b = (q / self.qp).clamp(-1.0, 1.0);
        [sin_b, (1.0 - sin_b * sin_b).max(0.0).sqrt()]
    }

    #[inline]
    fn ij_from_terms(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        let (x, y) = self.plane(lon, lat);
        ((x - self.x0) / self.dx, (y - self.y0) / self.dy)
    }

    fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64) {
        self.inverse(self.x0 + i * self.dx, self.y0 + j * self.dy)
    }

    fn wind_rotation(&self, lon: f64, lat: f64) -> (f64, f64) {
        plane_rotation(|lon, lat| self.forward(lon, lat), lon, lat)
    }

    fn nominal_spacing_m(&self) -> f64 {
        self.dx.abs().min(self.dy.abs())
    }
}

#[cfg(test)]
mod tests {
    use super::super::GridProjection;
    use super::super::close;
    use super::*;

    /// OPERA's 1 km composite: `+proj=laea +lat_0=55 +lon_0=10
    /// +x_0=1950000 +y_0=-2100000 +ellps=WGS84`, 3800 x 4400 cells, upper-left
    /// outer corner at projected (0, 0), rows north to south.
    fn opera() -> LaeaGrid {
        LaeaGrid::from_projected(
            Earth::WGS84,
            55.0,
            10.0,
            -1_950_000.0 + 500.0,
            2_100_000.0 - 500.0,
            1000.0,
            -1000.0,
        )
    }

    #[test]
    fn opera_corners_match_pyproj() {
        let g = opera();
        for (i, j, lon, lat) in [
            (-0.5, -0.5, -39.535_786_412_503, 67.022_832_763_513),
            (3799.5, -0.5, 57.811_964_750_150, 67.621_037_108_211),
            (-0.5, 4399.5, -10.434_576_838_640, 31.746_215_329_282),
            (3799.5, 4399.5, 29.421_038_635_578, 31.987_650_287_630),
            (1900.0, 2200.0, 9.243_423_343_842, 54.094_763_123_593),
        ] {
            let (glon, glat) = g.ij_to_lonlat(i, j);
            assert!(
                close(glon, lon, 1e-8) && close(glat, lat, 1e-8),
                "({i}, {j}): {glon}, {glat}"
            );
            let (gi, gj) = g.lonlat_to_ij(lon, lat);
            assert!(
                close(gi, i, 1e-5) && close(gj, j, 1e-5),
                "({i}, {j}) -> ({gi}, {gj})"
            );
        }
    }

    #[test]
    fn forward_matches_pyproj() {
        let g = LaeaGrid::from_projected(Earth::WGS84, 55.0, 10.0, 0.0, 0.0, 1.0, 1.0);
        for (lon, lat, x, y) in [
            (10.0, 55.0, 0.0, 0.0),
            (-20.0, 40.0, -2_504_738.190_155, -1_140_227.466_669),
            (40.0, 70.0, 1_111_220.095_641, 1_921_443.439_830),
            (10.0, -30.0, 0.0, -8_586_201.259_189),
        ] {
            let (gx, gy) = g.forward(lon, lat);
            assert!(
                close(gx, x, 1e-4) && close(gy, y, 1e-4),
                "({lon}, {lat}): {gx}, {gy}"
            );
            let (glon, glat) = g.inverse(x, y);
            assert!(
                close(wrap_lon(glon - lon), 0.0, 1e-8) && close(glat, lat, 1e-8),
                "({lon}, {lat})"
            );
        }
        // The polar aspect on a sphere (EASE-Grid style).
        let polar =
            LaeaGrid::from_projected(Earth::sphere(6_371_228.0), 90.0, 0.0, 0.0, 0.0, 1.0, 1.0);
        for (lon, lat, x, y) in [
            (45.0, 60.0, 2_332_031.301_303, -2_332_031.301_303),
            (-120.0, 10.0, -7_093_348.068_645, 4_095_346.416_888),
        ] {
            let (gx, gy) = polar.forward(lon, lat);
            assert!(
                close(gx, x, 1e-4) && close(gy, y, 1e-4),
                "({lon}, {lat}): {gx}, {gy}"
            );
        }
        // The antipode of the centre maps nowhere, without panicking.
        let (i, j) = g.lonlat_to_ij(-170.0, -55.0);
        assert!(!i.is_finite() || !j.is_finite() || i.abs() > 1e6);
    }

    #[test]
    fn serde_and_params_round_trip() {
        let g = GridProjection::LambertAzimuthal(opera());
        let back: GridProjection =
            serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
        let (kind, p) = g.to_params();
        assert_eq!(GridProjection::from_params(kind, &p), Some(g));
        let b = GridProjection::LambertAzimuthal(opera()).bounds(3800, 4400);
        assert!(
            close(b.west, -39.54, 0.01) && close(b.east, 57.82, 0.01),
            "{b:?}"
        );
        assert!(
            b.north > 67.62 && b.north < 75.0 && close(b.south, 31.75, 0.01),
            "{b:?}"
        );
    }
}
