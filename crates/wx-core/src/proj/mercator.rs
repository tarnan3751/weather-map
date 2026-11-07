//! Mercator grids (GRIB2 template 3.10; PROJ `+proj=merc`), on a sphere or
//! an ellipsoid. Snyder 7-1 to 7-9.

use serde::{Deserialize, Serialize};

use super::earth::{msfn, phi_from_ts, tsfn};
use super::{Earth, Projection, Terms, wrap_lon};

super::defined_by!(MercatorGrid, MercatorParams, MercatorGrid::from_projected, {
    earth: Earth,
    lat_ts: f64,
    lon0: f64,
    x0: f64,
    y0: f64,
    dx: f64,
    dy: f64,
});

/// Mercator grid. `x = a k0 (lon - lon0)` with the longitude difference
/// taken in [-180, 180), so the grid's cut is the meridian opposite `lon0`:
/// put `lon0` near the middle of the grid (the GRIB constructor does) and
/// points on both sides of the antimeridian map continuously. `lon0` is
/// otherwise free; it only moves the plane's origin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "MercatorParams", into = "MercatorParams")]
pub struct MercatorGrid {
    pub earth: Earth,
    /// Latitude of true scale (GRIB LaD, PROJ `lat_ts`), degrees.
    pub lat_ts: f64,
    /// Central meridian, degrees east.
    pub lon0: f64,
    /// Plane coordinates of sample `(0, 0)`, metres.
    pub x0: f64,
    pub y0: f64,
    /// Metres between columns (positive) and rows (negative when rows run
    /// north to south) on the plane.
    pub dx: f64,
    pub dy: f64,
    e: f64,
    /// `a k0`, metres per radian of longitude on the plane.
    ak0: f64,
}

impl MercatorGrid {
    /// From the plane coordinates of sample `(0, 0)` (PROJ's coordinates
    /// with false easting and northing removed). Web-Mercator tile grids
    /// are `Earth::sphere(6_378_137.0)` with `lat_ts = 0`.
    pub fn from_projected(
        earth: Earth,
        lat_ts: f64,
        lon0: f64,
        x0: f64,
        y0: f64,
        dx: f64,
        dy: f64,
    ) -> Self {
        let e = earth.e();
        MercatorGrid {
            earth,
            lat_ts,
            lon0,
            x0,
            y0,
            dx,
            dy,
            e,
            ak0: earth.a * msfn(lat_ts.to_radians(), e * e),
        }
    }

    /// The GRIB form: the latitude and longitude of sample `(0, 0)`.
    #[allow(clippy::too_many_arguments)]
    pub fn from_first_point(
        earth: Earth,
        lat_ts: f64,
        lon0: f64,
        first_lat: f64,
        first_lon: f64,
        dx: f64,
        dy: f64,
    ) -> Self {
        let probe = MercatorGrid::from_projected(earth, lat_ts, lon0, 0.0, 0.0, 1.0, 1.0);
        let (x0, y0) = probe.forward(first_lon, first_lat);
        MercatorGrid::from_projected(earth, lat_ts, lon0, x0, y0, dx, dy)
    }

    #[inline]
    fn x_of(&self, lon: f64) -> f64 {
        self.ak0 * wrap_lon(lon - self.lon0).to_radians()
    }

    #[inline]
    fn y_of(&self, lat: f64) -> f64 {
        -self.ak0 * tsfn(lat.to_radians(), self.e).ln()
    }

    pub fn forward(&self, lon: f64, lat: f64) -> (f64, f64) {
        (self.x_of(lon), self.y_of(lat))
    }

    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let lat = phi_from_ts((-y / self.ak0).exp(), self.e).to_degrees();
        (wrap_lon(self.lon0 + (x / self.ak0).to_degrees()), lat)
    }

    pub(crate) fn params(&self) -> Vec<f64> {
        vec![
            self.earth.a,
            self.earth.b,
            self.lat_ts,
            self.lon0,
            self.x0,
            self.y0,
            self.dx,
            self.dy,
        ]
    }

    pub(crate) fn from_params(p: &[f64]) -> Option<Self> {
        let &[a, b, lat_ts, lon0, x0, y0, dx, dy] = p else {
            return None;
        };
        let earth = Earth { a, b };
        let ok = p.iter().all(|v| v.is_finite())
            && earth.is_plausible()
            && lat_ts.abs() < 90.0
            && dx != 0.0
            && dy != 0.0;
        ok.then(|| MercatorGrid::from_projected(earth, lat_ts, lon0, x0, y0, dx, dy))
    }
}

impl Projection for MercatorGrid {
    #[inline]
    fn lon_terms(&self, lon: f64) -> Terms {
        [self.x_of(lon), 0.0]
    }

    #[inline]
    fn lat_terms(&self, lat: f64) -> Terms {
        [self.y_of(lat), 0.0]
    }

    #[inline]
    fn ij_from_terms(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        ((lon[0] - self.x0) / self.dx, (lat[0] - self.y0) / self.dy)
    }

    fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64) {
        self.inverse(self.x0 + i * self.dx, self.y0 + j * self.dy)
    }

    /// Rows are parallels and columns meridians.
    fn wind_rotation(&self, _lon: f64, _lat: f64) -> (f64, f64) {
        (1.0, 0.0)
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
    fn spherical_and_ellipsoidal_match_pyproj() {
        let web =
            MercatorGrid::from_projected(Earth::sphere(6_378_137.0), 0.0, 0.0, 0.0, 0.0, 1.0, 1.0);
        let ell = MercatorGrid::from_projected(Earth::WGS84, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0);
        let ts20 = MercatorGrid::from_projected(Earth::WGS84, 20.0, -160.0, 0.0, 0.0, 1.0, 1.0);
        for (g, lon, lat, x, y) in [
            (&web, -97.5, 35.0, -10_853_650.352_344, 4_163_881.144_064),
            (&web, 139.7, 35.7, 15_551_332.863_820, 4_259_419.965_548),
            (&web, 179.0, -60.0, 19_926_188.851_996, -8_399_737.889_818),
            (&ell, -97.5, 35.0, -10_853_650.352_344, 4_139_372.762_247),
            (&ell, 10.0, 72.0, 1_113_194.907_933, 11_712_494.454_461),
            (&ts20, -157.8, 21.3, 230_223.589_882, 2_267_588.262_090),
            (&ts20, -155.0, 19.5, 523_235.431_549, 2_067_791.383_114),
        ] {
            let (gx, gy) = g.forward(lon, lat);
            assert!(
                close(gx, x, 1e-6) && close(gy, y, 1e-6),
                "({lon}, {lat}): {gx}, {gy}"
            );
            let (glon, glat) = g.inverse(x, y);
            assert!(
                close(glon, lon, 1e-10) && close(glat, lat, 1e-10),
                "({lon}, {lat})"
            );
        }
    }

    #[test]
    fn rtma_hawaii_corners() {
        // Template 3.10 as RTMA Hawaii writes it: sphere of 6371200 m, LaD
        // 20, 321 x 225 at 2500 m, first point 18.072699 N 198.474999 E.
        // Expected corners from pyproj (eccodes agrees to 1e-6 degree).
        let g = GridProjection::Mercator(MercatorGrid::from_first_point(
            Earth::sphere(6_371_200.0),
            20.0,
            202.3,
            18.072699,
            198.474999,
            2500.0,
            2500.0,
        ));
        for (i, j, lon, lat) in [
            (320.0, 0.0, -153.868_936_834, 18.072_699_000),
            (0.0, 224.0, -161.525_001_000, 23.087_865_988),
            (320.0, 224.0, -153.868_936_834, 23.087_865_988),
            (0.0, 1.0, -161.525_001_000, 18.095_442_345),
        ] {
            let (glon, glat) = g.ij_to_lonlat(i, j);
            assert!(
                close(glon, lon, 1e-8) && close(glat, lat, 1e-8),
                "({i}, {j}): {glon}, {glat}"
            );
        }
        let b = g.bounds(321, 225);
        assert!(
            close(b.west, -161.53, 0.01) && close(b.east, -153.86, 0.01),
            "{b:?}"
        );
    }

    #[test]
    fn noaa_global_mosaic_rows() {
        // GMGSI: 0.072 degree Mercator on a sphere, row 1499 on the equator
        // and lat = atan(sinh((1499 - j) * 0.072 deg)).
        let r = 6_371_000.0;
        let step = r * 0.072f64.to_radians();
        let g = MercatorGrid::from_projected(
            Earth::sphere(r),
            0.0,
            0.0,
            -2499.0 * step,
            1499.0 * step,
            step,
            -step,
        );
        for j in [0.0f64, 700.0, 1499.0, 2999.0] {
            let want = ((1499.0 - j) * 0.072f64.to_radians())
                .sinh()
                .atan()
                .to_degrees();
            let (_, lat) = g.ij_to_lonlat(0.0, j);
            assert!(close(lat, want, 1e-11), "{j}: {lat} vs {want}");
        }
        // The cut is opposite lon0: both sides of the antimeridian are in.
        let (i_w, _) = g.lonlat_to_ij(-179.9, 0.0);
        let (i_e, _) = g.lonlat_to_ij(179.9, 0.0);
        assert!(
            i_w > -1.0 && i_w < 2.0 && i_e > 4996.0 && i_e < 4999.0,
            "{i_w} {i_e}"
        );
    }

    #[test]
    fn serde_and_params_round_trip() {
        let g = GridProjection::Mercator(MercatorGrid::from_projected(
            Earth::WGS84,
            20.0,
            -160.0,
            1.0,
            2.0,
            2500.0,
            -2500.0,
        ));
        let back: GridProjection =
            serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
        let (kind, p) = g.to_params();
        assert_eq!(GridProjection::from_params(kind, &p), Some(g));
        assert_eq!(GridProjection::from_params(kind, &p[..3]), None);
    }
}
