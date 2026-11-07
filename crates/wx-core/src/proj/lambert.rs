//! Lambert conformal conic grids on a sphere (GRIB2 template 3.30).
//!
//! Spherical only: every producer we take Lambert grids from (NOAA's
//! models and analyses, the Taiwan and Argentina WRF runs) defines them on a
//! sphere. The ellipsoidal form (Snyder 15-7 to 15-11) would slot in the
//! same way as [`super::PolarStereoGrid`]'s if a source needs it.

use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

use super::{Projection, Terms, wrap_lon};

super::defined_by!(LambertGrid, LambertParams, LambertGrid::new, {
    lat_std1: f64,
    lat_std2: f64,
    lon_orient: f64,
    first_lat: f64,
    first_lon: f64,
    dx: f64,
    dy: f64,
    radius: f64,
});

/// Lambert conformal conic grid on a sphere. The cone's apex is over the
/// North Pole for standard parallels in the northern hemisphere and over the
/// South Pole for southern ones, where the cone constant `n` is negative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "LambertParams", into = "LambertParams")]
pub struct LambertGrid {
    pub lat_std1: f64,
    pub lat_std2: f64,
    /// Longitude that points straight up on the map (GRIB "LoV"), degrees east.
    pub lon_orient: f64,
    pub first_lat: f64,
    pub first_lon: f64,
    /// Metres between columns, measured on the projection plane.
    pub dx: f64,
    /// Metres between rows on the projection plane; negative when rows run
    /// north to south.
    pub dy: f64,
    pub radius: f64,
    // Derived constants, kept so the hot path does no trigonometry it can avoid.
    pub(crate) n: f64,
    rf: f64,
    x0: f64,
    y0: f64,
}

impl LambertGrid {
    /// The GRIB form: projection parameters and the position of sample `(0, 0)`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        lat_std1: f64,
        lat_std2: f64,
        lon_orient: f64,
        first_lat: f64,
        first_lon: f64,
        dx: f64,
        dy: f64,
        radius: f64,
    ) -> Self {
        let p1 = lat_std1.to_radians();
        let p2 = lat_std2.to_radians();
        let n = if (p1 - p2).abs() < 1e-10 {
            p1.sin()
        } else {
            (p1.cos() / p2.cos()).ln()
                / ((PI / 4.0 + p2 / 2.0).tan() / (PI / 4.0 + p1 / 2.0).tan()).ln()
        };
        let rf = radius * p1.cos() * (PI / 4.0 + p1 / 2.0).tan().powf(n) / n;
        let mut g = Self {
            lat_std1,
            lat_std2,
            lon_orient,
            first_lat,
            first_lon,
            dx,
            dy,
            radius,
            n,
            rf,
            x0: 0.0,
            y0: 0.0,
        };
        let (x0, y0) = g.forward(first_lon, first_lat);
        g.x0 = x0;
        g.y0 = y0;
        g
    }

    /// The form PROJ and CF use: `+proj=lcc +lat_1 +lat_2 +lat_0 +lon_0` on a
    /// sphere, with `(x_first, y_first)` the projected coordinates of sample
    /// `(0, 0)` measured from the natural origin `(lat_origin, lon_orient)`
    /// (false easting and northing removed).
    #[allow(clippy::too_many_arguments)]
    pub fn from_projected(
        lat_std1: f64,
        lat_std2: f64,
        lon_orient: f64,
        lat_origin: f64,
        x_first: f64,
        y_first: f64,
        dx: f64,
        dy: f64,
        radius: f64,
    ) -> Self {
        let probe = LambertGrid::new(
            lat_std1, lat_std2, lon_orient, lat_origin, lon_orient, 1.0, 1.0, radius,
        );
        // `forward` puts the cone's apex at the origin, so the natural
        // origin sits at `probe.y0` (straight below the apex).
        let (lon, lat) = probe.inverse(x_first, y_first + probe.y0);
        LambertGrid::new(lat_std1, lat_std2, lon_orient, lat, lon, dx, dy, radius)
    }

    /// Projected metres, origin at the cone's apex.
    fn forward(&self, lon: f64, lat: f64) -> (f64, f64) {
        let theta = self.n * wrap_lon(lon - self.lon_orient).to_radians();
        let rho = self.rho(lat);
        (rho * theta.sin(), -rho * theta.cos())
    }

    #[inline]
    fn rho(&self, lat: f64) -> f64 {
        self.rf / (PI / 4.0 + lat.to_radians() / 2.0).tan().powf(self.n)
    }

    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let sign = self.n.signum();
        let rho = sign * x.hypot(y);
        let theta = (sign * x).atan2(-sign * y);
        let lat = 2.0 * (self.rf / rho).powf(1.0 / self.n).atan() - PI / 2.0;
        (
            wrap_lon(self.lon_orient + (theta / self.n).to_degrees()),
            lat.to_degrees(),
        )
    }

    pub fn lonlat_to_ij(&self, lon: f64, lat: f64) -> (f64, f64) {
        Projection::lonlat_to_ij(self, lon, lat)
    }

    pub fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64) {
        self.inverse(self.x0 + i * self.dx, self.y0 + j * self.dy)
    }

    pub(crate) fn params(&self) -> Vec<f64> {
        vec![
            self.lat_std1,
            self.lat_std2,
            self.lon_orient,
            self.first_lat,
            self.first_lon,
            self.dx,
            self.dy,
            self.radius,
        ]
    }

    pub(crate) fn from_params(p: &[f64]) -> Option<Self> {
        let &[lat1, lat2, lov, flat, flon, dx, dy, radius] = p else {
            return None;
        };
        let ok = p.iter().all(|v| v.is_finite())
            && lat1.abs() < 90.0
            && lat2.abs() < 90.0
            && flat.abs() < 90.0
            && dx != 0.0
            && dy != 0.0
            && (6.0e6..=6.5e6).contains(&radius);
        let g = LambertGrid::new(lat1, lat2, lov, flat, flon, dx, dy, radius);
        // A southern cone has a negative cone constant.
        (ok && g.n.is_finite() && g.n.abs() > 1e-6 && g.x0.is_finite() && g.y0.is_finite())
            .then_some(g)
    }
}

impl Projection for LambertGrid {
    #[inline]
    fn lon_terms(&self, lon: f64) -> Terms {
        let theta = self.n * wrap_lon(lon - self.lon_orient).to_radians();
        [theta.sin(), theta.cos()]
    }

    #[inline]
    fn lat_terms(&self, lat: f64) -> Terms {
        [self.rho(lat), 0.0]
    }

    #[inline]
    fn ij_from_terms(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        let rho = lat[0];
        let (x, y) = (rho * lon[0], -rho * lon[1]);
        ((x - self.x0) / self.dx, (y - self.y0) / self.dy)
    }

    fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64) {
        LambertGrid::ij_to_lonlat(self, i, j)
    }

    fn wind_rotation(&self, lon: f64, _lat: f64) -> (f64, f64) {
        let a = self.n * wrap_lon(lon - self.lon_orient).to_radians();
        (a.cos(), a.sin())
    }

    fn nominal_spacing_m(&self) -> f64 {
        self.dx.abs().min(self.dy.abs())
    }
}

#[cfg(test)]
mod tests {
    use super::super::close;
    use super::super::{EARTH_RADIUS_NCEP, GridProjection};
    use super::*;

    /// HRRR's CONUS grid, as written in its GRIB2 grid definition section.
    fn hrrr() -> LambertGrid {
        LambertGrid::new(
            38.5,
            38.5,
            262.5,
            21.138123,
            237.280472,
            3000.0,
            3000.0,
            EARTH_RADIUS_NCEP,
        )
    }

    #[test]
    fn lambert_matches_hrrr_corners() {
        let g = hrrr();
        let (i, j) = g.lonlat_to_ij(237.280472, 21.138123);
        assert!(close(i, 0.0, 1e-6) && close(j, 0.0, 1e-6));
        // Published corner points of the 1799 x 1059 HRRR grid.
        let (lon, lat) = g.ij_to_lonlat(1798.0, 1058.0);
        assert!(close(lon, -60.917, 0.01), "ne lon {lon}");
        assert!(close(lat, 47.842, 0.01), "ne lat {lat}");
        let (lon, lat) = g.ij_to_lonlat(0.0, 1058.0);
        assert!(close(lon, -134.095, 0.01), "nw lon {lon}");
        assert!(close(lat, 47.838, 0.01), "nw lat {lat}");
        let (lon, lat) = g.ij_to_lonlat(1798.0, 0.0);
        assert!(close(lon, -72.29, 0.01), "se lon {lon}");
        assert!(close(lat, 21.14, 0.01), "se lat {lat}");
    }

    #[test]
    fn lambert_round_trip() {
        let g = GridProjection::Lambert(hrrr());
        for (i, j) in [
            (0.0, 0.0),
            (900.5, 500.25),
            (1798.0, 1058.0),
            (12.0, 1000.0),
        ] {
            let (lon, lat) = g.ij_to_lonlat(i, j);
            let (i2, j2) = g.lonlat_to_ij(lon, lat);
            assert!(
                close(i, i2, 1e-6) && close(j, j2, 1e-6),
                "{i},{j} -> {i2},{j2}"
            );
        }
        // Adjacent samples are one grid length apart near the standard parallel.
        let (lon_a, lat_a) = g.ij_to_lonlat(900.0, 600.0);
        let (lon_b, lat_b) = g.ij_to_lonlat(901.0, 600.0);
        let d = ((lon_b - lon_a).to_radians() * lat_a.to_radians().cos())
            .hypot((lat_b - lat_a).to_radians())
            * EARTH_RADIUS_NCEP;
        assert!(close(d, 3000.0, 30.0), "spacing {d}");
    }

    #[test]
    fn lambert_serde_recomputes_derived_constants() {
        let g = GridProjection::Lambert(hrrr());
        let json = serde_json::to_string(&g).unwrap();
        assert!(
            !json.contains("\"rf\""),
            "derived constants must not be serialized: {json}"
        );
        let back: GridProjection = serde_json::from_str(&json).unwrap();
        assert_eq!(back, g);
        assert_eq!(back.lonlat_to_ij(-97.5, 38.5), g.lonlat_to_ij(-97.5, 38.5));
    }

    #[test]
    fn lambert_wind_rotation() {
        let g = GridProjection::Lambert(hrrr());
        let (c, s) = g.wind_rotation(-97.5, 38.5);
        assert!(close(c, 1.0, 1e-12) && close(s, 0.0, 1e-12));
        // East of the orientation longitude, true north leans toward grid west.
        let (_, s) = g.wind_rotation(-75.0, 40.0);
        assert!(s > 0.2 && s < 0.3, "sin {s}");
        let (c2, s2) =
            super::super::earth::plane_rotation(|lon, lat| hrrr().forward(lon, lat), -75.0, 40.0);
        let (c, s) = g.wind_rotation(-75.0, 40.0);
        assert!(close(c, c2, 1e-6) && close(s, s2, 1e-6));
    }

    #[test]
    fn projected_form_matches_the_grib_form() {
        // NBM CONUS: Latin1 = Latin2 = LaD = 25, LoV 265, first point
        // 19.229 N 233.7234 E. With lat_0 = 25, PROJ puts (25 N, 265 E) at
        // the origin.
        let grib = LambertGrid::new(
            25.0,
            25.0,
            265.0,
            19.229,
            233.7234,
            2539.703,
            2539.703,
            6_371_200.0,
        );
        let origin = LambertGrid::new(25.0, 25.0, 265.0, 25.0, 265.0, 1.0, 1.0, 6_371_200.0);
        let (x, y) = grib.forward(233.7234, 19.229);
        let (x0, y0) = origin.forward(265.0, 25.0);
        let p = LambertGrid::from_projected(
            25.0,
            25.0,
            265.0,
            25.0,
            x - x0,
            y - y0,
            2539.703,
            2539.703,
            6_371_200.0,
        );
        assert!(
            close(p.first_lat, 19.229, 1e-9) && close(p.first_lon, -126.2766, 1e-9),
            "{p:?}"
        );
    }

    /// A southern cone: the HRRR grid mirrored to standard parallels at
    /// 38.5 S, against pyproj (`+proj=lcc +lat_1=-38.5 +lat_2=-38.5
    /// +lat_0=-38.5 +lon_0=262.5 +R=6371229`, which eccodes matches), and
    /// Argentina's WRF cone at 35 S, which must survive a trip through its
    /// stored parameters.
    #[test]
    fn southern_cones() {
        let g = LambertGrid::new(
            -38.5,
            -38.5,
            262.5,
            -21.138123,
            237.280472,
            3000.0,
            3000.0,
            6_371_229.0,
        );
        assert!(g.n < 0.0);
        for ((i, j), (lat, lon)) in [
            ((0.0, 0.0), (-21.138123, -122.719528)),
            ((1798.0, 0.0), (-21.140547, -72.289718)),
            ((0.0, 1058.0), (3.171065, -116.659072)),
            ((1798.0, 1058.0), (3.169552, -78.348105)),
            ((900.0, 600.0), (-9.271745, -97.479973)),
        ] {
            let (glon, glat) = g.ij_to_lonlat(i, j);
            assert!(
                close(glat, lat, 2e-6) && close(glon, lon, 2e-6),
                "({i}, {j}): {glat} {glon}"
            );
            let (bi, bj) = g.lonlat_to_ij(lon, lat);
            assert!(
                close(bi, i, 1e-3) && close(bj, j, 1e-3),
                "({i}, {j}): {bi} {bj}"
            );
        }
        let smn = LambertGrid::from_projected(
            -35.0,
            -35.0,
            -65.0,
            -35.0,
            -1_996_000.0,
            -2_496_000.0,
            4000.0,
            4000.0,
            6_370_000.0,
        );
        let p = GridProjection::Lambert(smn.clone());
        let (kind, params) = p.to_params();
        assert_eq!(GridProjection::from_params(kind, &params), Some(p));
        let back = LambertGrid::from_params(&smn.params()).expect("a southern cone round-trips");
        assert_eq!(
            back.lonlat_to_ij(-60.0, -30.0),
            smn.lonlat_to_ij(-60.0, -30.0)
        );
    }
}
