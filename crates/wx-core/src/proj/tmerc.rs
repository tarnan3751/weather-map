//! Transverse Mercator grids (PROJ `+proj=tmerc`) on an ellipsoid or a
//! sphere, by the Poder/Engsager form of Kruger's series to sixth order in
//! the third flattening (Engsager and Poder, ICC 2007; the algorithm PROJ
//! calls `etmerc`). Accurate to well under a millimetre within 3000 km of
//! the central meridian; with `e = 0` it reduces exactly to the spherical
//! formulas.

use serde::{Deserialize, Serialize};

use super::earth::plane_rotation;
use super::{Earth, Projection, Terms, wrap_lon};

const ORDER: usize = 6;
/// Beyond about 150 degrees of longitude from the central meridian the
/// series is meaningless; such points map nowhere.
const MAX_CE: f64 = 2.623_395_162_778;

super::defined_by!(TransverseMercatorGrid, TmercParams, TransverseMercatorGrid::from_projected, {
    earth: Earth,
    lat0: f64,
    lon0: f64,
    k0: f64,
    x0: f64,
    y0: f64,
    dx: f64,
    dy: f64,
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "TmercParams", into = "TmercParams")]
pub struct TransverseMercatorGrid {
    pub earth: Earth,
    /// Latitude of origin and central meridian (PROJ `lat_0`, `lon_0`), degrees.
    pub lat0: f64,
    pub lon0: f64,
    /// Scale on the central meridian (PROJ `k_0`; 0.9996 for UTM).
    pub k0: f64,
    /// Plane coordinates of sample `(0, 0)`, metres from the natural origin.
    pub x0: f64,
    pub y0: f64,
    pub dx: f64,
    pub dy: f64,
    /// Geodetic to conformal ("Gaussian") latitude, and back.
    cbg: [f64; ORDER],
    cgb: [f64; ORDER],
    /// Conformal sphere to ellipsoid plane, and back.
    gtu: [f64; ORDER],
    utg: [f64; ORDER],
    /// `a k0` times the normalised meridian quadrant.
    qn: f64,
    /// Northing of the latitude of origin, in units of `qn`.
    zb: f64,
}

/// Clenshaw sum of `B + sum c_k sin(2 k B)`, PROJ's `gatg`.
fn gatg(c: &[f64; ORDER], b: f64) -> f64 {
    let two_cos = 2.0 * (2.0 * b).cos();
    let (mut h, mut h1, mut h2) = (0.0, c[ORDER - 1], 0.0);
    for &ck in c[..ORDER - 1].iter().rev() {
        h = -h2 + two_cos * h1 + ck;
        h2 = h1;
        h1 = h;
    }
    b + h * (2.0 * b).sin()
}

/// Real Clenshaw sum `sum c_k sin(k arg)`, PROJ's `clens`.
fn clens(c: &[f64; ORDER], arg: f64) -> f64 {
    let r = 2.0 * arg.cos();
    let (mut hr, mut hr1) = (c[ORDER - 1], 0.0);
    for &ck in c[..ORDER - 1].iter().rev() {
        let hr2 = hr1;
        hr1 = hr;
        hr = -hr2 + r * hr1 + ck;
    }
    arg.sin() * hr
}

/// Complex Clenshaw sum `sum c_k sin(k (re + i im))`, PROJ's `clenS`:
/// returns its real and imaginary parts.
fn clens_complex(c: &[f64; ORDER], re: f64, im: f64) -> (f64, f64) {
    let (sin_r, cos_r) = re.sin_cos();
    let (sinh_i, cosh_i) = (im.sinh(), im.cosh());
    let r = 2.0 * cos_r * cosh_i;
    let i = -2.0 * sin_r * sinh_i;
    let (mut hr, mut hi, mut hr1, mut hi1) = (c[ORDER - 1], 0.0, 0.0, 0.0);
    for &ck in c[..ORDER - 1].iter().rev() {
        let (hr2, hi2) = (hr1, hi1);
        hr1 = hr;
        hi1 = hi;
        hr = -hr2 + r * hr1 - i * hi1 + ck;
        hi = -hi2 + i * hr1 + r * hi1;
    }
    let (r, i) = (sin_r * cosh_i, cos_r * sinh_i);
    (r * hr - i * hi, r * hi + i * hr)
}

impl TransverseMercatorGrid {
    /// From the plane coordinates of sample `(0, 0)` (PROJ's coordinates
    /// with false easting and northing removed).
    #[allow(clippy::too_many_arguments)]
    pub fn from_projected(
        earth: Earth,
        lat0: f64,
        lon0: f64,
        k0: f64,
        x0: f64,
        y0: f64,
        dx: f64,
        dy: f64,
    ) -> Self {
        let f = 1.0 - earth.b / earth.a;
        let n = f / (2.0 - f);
        let n2 = n * n;
        let mut np = n;
        let mut cgb = [0.0; ORDER];
        let mut cbg = [0.0; ORDER];
        cgb[0] = n
            * (2.0
                + n * (-2.0 / 3.0
                    + n * (-2.0 + n * (116.0 / 45.0 + n * (26.0 / 45.0 + n * (-2854.0 / 675.0))))));
        cbg[0] = n
            * (-2.0
                + n * (2.0 / 3.0
                    + n * (4.0 / 3.0
                        + n * (-82.0 / 45.0 + n * (32.0 / 45.0 + n * (4642.0 / 4725.0))))));
        np *= n;
        cgb[1] = np
            * (7.0 / 3.0
                + n * (-8.0 / 5.0
                    + n * (-227.0 / 45.0 + n * (2704.0 / 315.0 + n * (2323.0 / 945.0)))));
        cbg[1] = np
            * (5.0 / 3.0
                + n * (-16.0 / 15.0
                    + n * (-13.0 / 9.0 + n * (904.0 / 315.0 + n * (-1522.0 / 945.0)))));
        np *= n;
        cgb[2] = np
            * (56.0 / 15.0 + n * (-136.0 / 35.0 + n * (-1262.0 / 105.0 + n * (73814.0 / 2835.0))));
        cbg[2] =
            np * (-26.0 / 15.0 + n * (34.0 / 21.0 + n * (8.0 / 5.0 + n * (-12686.0 / 2835.0))));
        np *= n;
        cgb[3] = np * (4279.0 / 630.0 + n * (-332.0 / 35.0 + n * (-399572.0 / 14175.0)));
        cbg[3] = np * (1237.0 / 630.0 + n * (-12.0 / 5.0 + n * (-24832.0 / 14175.0)));
        np *= n;
        cgb[4] = np * (4174.0 / 315.0 + n * (-144838.0 / 6237.0));
        cbg[4] = np * (-734.0 / 315.0 + n * (109598.0 / 31185.0));
        np *= n;
        cgb[5] = np * (601676.0 / 22275.0);
        cbg[5] = np * (444337.0 / 155925.0);

        let qn =
            earth.a * k0 / (1.0 + n) * (1.0 + n2 * (1.0 / 4.0 + n2 * (1.0 / 64.0 + n2 / 256.0)));
        let mut utg = [0.0; ORDER];
        let mut gtu = [0.0; ORDER];
        utg[0] = n
            * (-0.5
                + n * (2.0 / 3.0
                    + n * (-37.0 / 96.0
                        + n * (1.0 / 360.0 + n * (81.0 / 512.0 + n * (-96199.0 / 604800.0))))));
        gtu[0] = n
            * (0.5
                + n * (-2.0 / 3.0
                    + n * (5.0 / 16.0
                        + n * (41.0 / 180.0 + n * (-127.0 / 288.0 + n * (7891.0 / 37800.0))))));
        utg[1] = n2
            * (-1.0 / 48.0
                + n * (-1.0 / 15.0
                    + n * (437.0 / 1440.0 + n * (-46.0 / 105.0 + n * (1118711.0 / 3870720.0)))));
        gtu[1] = n2
            * (13.0 / 48.0
                + n * (-3.0 / 5.0
                    + n * (557.0 / 1440.0 + n * (281.0 / 630.0 + n * (-1983433.0 / 1935360.0)))));
        let n3 = n2 * n;
        utg[2] = n3
            * (-17.0 / 480.0 + n * (37.0 / 840.0 + n * (209.0 / 4480.0 + n * (-5569.0 / 90720.0))));
        gtu[2] = n3
            * (61.0 / 240.0
                + n * (-103.0 / 140.0 + n * (15061.0 / 26880.0 + n * (167603.0 / 181440.0))));
        let n4 = n3 * n;
        utg[3] = n4 * (-4397.0 / 161280.0 + n * (11.0 / 504.0 + n * (830251.0 / 7257600.0)));
        gtu[3] = n4 * (49561.0 / 161280.0 + n * (-179.0 / 168.0 + n * (6601661.0 / 7257600.0)));
        let n5 = n4 * n;
        utg[4] = n5 * (-4583.0 / 161280.0 + n * (108847.0 / 3991680.0));
        gtu[4] = n5 * (34729.0 / 80640.0 + n * (-3418889.0 / 1995840.0));
        let n6 = n5 * n;
        utg[5] = n6 * (-20648693.0 / 638668800.0);
        gtu[5] = n6 * (212378941.0 / 319334400.0);

        let z = gatg(&cbg, lat0.to_radians());
        let zb = -(z + clens(&gtu, 2.0 * z));
        TransverseMercatorGrid {
            earth,
            lat0,
            lon0,
            k0,
            x0,
            y0,
            dx,
            dy,
            cbg,
            cgb,
            gtu,
            utg,
            qn,
            zb,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_first_point(
        earth: Earth,
        lat0: f64,
        lon0: f64,
        k0: f64,
        first_lat: f64,
        first_lon: f64,
        dx: f64,
        dy: f64,
    ) -> Self {
        let probe =
            TransverseMercatorGrid::from_projected(earth, lat0, lon0, k0, 0.0, 0.0, 1.0, 1.0);
        let (x0, y0) = probe.forward(first_lon, first_lat);
        TransverseMercatorGrid::from_projected(earth, lat0, lon0, k0, x0, y0, dx, dy)
    }

    pub fn forward(&self, lon: f64, lat: f64) -> (f64, f64) {
        self.plane(
            &Projection::lon_terms(self, lon),
            &Projection::lat_terms(self, lat),
        )
    }

    #[inline]
    fn plane(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        let (sin_ce, cos_ce) = (lon[0], lon[1]);
        let (sin_cn, cos_cn) = (lat[0], lat[1]);
        // Conformal latitude and longitude to the complementary sphere.
        let cn = sin_cn.atan2(cos_ce * cos_cn);
        let ce = (sin_ce * cos_cn).atan2(sin_cn.hypot(cos_cn * cos_ce));
        let ce = ce.tan().asinh();
        let (dn, de) = clens_complex(&self.gtu, 2.0 * cn, 2.0 * ce);
        let (cn, ce) = (cn + dn, ce + de);
        if ce.abs() > MAX_CE {
            return (f64::NAN, f64::NAN);
        }
        (self.qn * ce, self.qn * (cn + self.zb))
    }

    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (cn, ce) = (y / self.qn - self.zb, x / self.qn);
        if ce.abs() > MAX_CE {
            return (f64::NAN, f64::NAN);
        }
        let (dn, de) = clens_complex(&self.utg, 2.0 * cn, 2.0 * ce);
        let (cn, ce) = (cn + dn, (ce + de).sinh().atan());
        let (sin_cn, cos_cn) = cn.sin_cos();
        let (sin_ce, cos_ce) = ce.sin_cos();
        let lam = sin_ce.atan2(cos_ce * cos_cn);
        let phi = (sin_cn * cos_ce).atan2(sin_ce.hypot(cos_ce * cos_cn));
        let phi = gatg(&self.cgb, phi);
        (wrap_lon(self.lon0 + lam.to_degrees()), phi.to_degrees())
    }

    pub(crate) fn params(&self) -> Vec<f64> {
        vec![
            self.earth.a,
            self.earth.b,
            self.lat0,
            self.lon0,
            self.k0,
            self.x0,
            self.y0,
            self.dx,
            self.dy,
        ]
    }

    pub(crate) fn from_params(p: &[f64]) -> Option<Self> {
        let &[a, b, lat0, lon0, k0, x0, y0, dx, dy] = p else {
            return None;
        };
        let earth = Earth { a, b };
        let ok = p.iter().all(|v| v.is_finite())
            && earth.is_plausible()
            && lat0.abs() < 90.0
            && (0.5..=1.5).contains(&k0)
            && dx != 0.0
            && dy != 0.0;
        ok.then(|| TransverseMercatorGrid::from_projected(earth, lat0, lon0, k0, x0, y0, dx, dy))
    }
}

impl Projection for TransverseMercatorGrid {
    #[inline]
    fn lon_terms(&self, lon: f64) -> Terms {
        let d = wrap_lon(lon - self.lon0).to_radians();
        [d.sin(), d.cos()]
    }

    /// Sine and cosine of the conformal latitude.
    #[inline]
    fn lat_terms(&self, lat: f64) -> Terms {
        let c = gatg(&self.cbg, lat.to_radians());
        [c.sin(), c.cos()]
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

    #[test]
    fn matches_pyproj() {
        // Italy's radar mosaic is described as a transverse Mercator at
        // 42 N 12.5 E on WGS84 (scale 1 assumed); UTM zone 33; a sphere.
        let italy = TransverseMercatorGrid::from_projected(
            Earth::WGS84,
            42.0,
            12.5,
            1.0,
            0.0,
            0.0,
            1.0,
            1.0,
        );
        let utm = TransverseMercatorGrid::from_projected(
            Earth::WGS84,
            0.0,
            15.0,
            0.9996,
            0.0,
            0.0,
            1.0,
            1.0,
        );
        let sphere = TransverseMercatorGrid::from_projected(
            Earth::sphere(6_371_229.0),
            10.0,
            -90.0,
            1.0,
            0.0,
            0.0,
            1.0,
            1.0,
        );
        for (g, lon, lat, x, y) in [
            (&italy, 12.5, 42.0, 0.0, 0.0),
            (&italy, 7.0, 47.0, -418_263.180_831, 570_314.103_721),
            (&italy, 18.5, 36.0, 541_289.520_101, -649_399.740_732),
            (&italy, 13.0, 44.0, 40_103.115_228, 222_307.045_025),
            (&italy, 30.0, 60.0, 968_737.369_549, 2_132_069.162_176),
            (
                &utm,
                12.0,
                48.0,
                276_224.084_603 - 500_000.0,
                5_320_655.789_192,
            ),
            (
                &utm,
                21.0,
                70.0,
                728_706.743_826 - 500_000.0,
                7_777_138.716_948,
            ),
            (&sphere, -85.0, 30.0, 481_810.917_955, 2_234_506.661_373),
            (
                &sphere,
                -100.0,
                -5.0,
                -1_113_337.747_423,
                -1_676_516.480_564,
            ),
        ] {
            let (gx, gy) = g.forward(lon, lat);
            assert!(
                close(gx, x, 1e-4) && close(gy, y, 1e-4),
                "({lon}, {lat}): {gx}, {gy}"
            );
            let (glon, glat) = g.inverse(x, y);
            assert!(
                close(glon, lon, 1e-9) && close(glat, lat, 1e-9),
                "({lon}, {lat}): {glon}, {glat}"
            );
        }
        // East of the central meridian the meridians lean towards it, so
        // grid north is clockwise of true north.
        let (c, s) = italy.wind_rotation(18.0, 40.0);
        assert!(s > 0.05 && c > 0.99, "{c} {s}");
    }

    #[test]
    fn far_points_stay_off_the_grid() {
        // Italy's 1200 x 1400 km mosaic. The far side of the earth maps
        // beyond the poles' northings, never onto the grid.
        let g = TransverseMercatorGrid::from_projected(
            Earth::WGS84,
            42.0,
            12.5,
            1.0,
            -599_500.0,
            649_500.0,
            1000.0,
            -1000.0,
        );
        for (lon, lat) in [(-170.0, 0.0), (-167.5, 42.0), (100.0, -60.0), (12.5, -42.0)] {
            let (i, j) = g.lonlat_to_ij(lon, lat);
            let on_grid = (-0.5..1199.5).contains(&i) && (-0.5..1399.5).contains(&j);
            assert!(!on_grid, "({lon}, {lat}) -> ({i}, {j})");
        }
        let (kind, p) = GridProjection::TransverseMercator(g.clone()).to_params();
        assert_eq!(
            GridProjection::from_params(kind, &p),
            Some(GridProjection::TransverseMercator(g))
        );
    }
}
