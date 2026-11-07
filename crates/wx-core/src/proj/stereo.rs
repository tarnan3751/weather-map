//! Polar stereographic grids (GRIB2 template 3.20; PROJ `+proj=stere`
//! with `lat_0 = +-90`), on a sphere or an ellipsoid. Snyder 21-33 to 21-38.

use serde::{Deserialize, Serialize};
use std::f64::consts::FRAC_PI_2;

use super::earth::{msfn, phi_from_ts, tsfn};
use super::{Earth, Projection, Terms, wrap_lon};

super::defined_by!(PolarStereoGrid, PolarStereoParams, PolarStereoGrid::from_projected, {
    earth: Earth,
    south: bool,
    lat_ts: f64,
    lon0: f64,
    x0: f64,
    y0: f64,
    dx: f64,
    dy: f64,
});

/// Polar stereographic grid. The plane has the pole at its origin; on a
/// north-polar grid `lon0` points straight down from the pole
/// (`y = -rho cos(lon - lon0)`), on a south-polar one straight up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "PolarStereoParams", into = "PolarStereoParams")]
pub struct PolarStereoGrid {
    pub earth: Earth,
    /// The projection is centred on the south pole.
    pub south: bool,
    /// Latitude where the scale is true (GRIB LaD, PROJ `lat_ts`), degrees,
    /// in the hemisphere of the pole; 90 (or -90) for scale true at the pole.
    pub lat_ts: f64,
    /// Orientation longitude (GRIB LoV, PROJ `lon_0`), degrees east.
    pub lon0: f64,
    /// Plane coordinates of sample `(0, 0)`, metres.
    pub x0: f64,
    pub y0: f64,
    /// Metres between columns and between rows on the plane; `dy` is
    /// negative when rows run from the top of the plane down.
    pub dx: f64,
    pub dy: f64,
    e: f64,
    /// `rho = akm * tsfn(lat)`.
    akm: f64,
    /// -1 for a north-polar plane, +1 for a south-polar one.
    ys: f64,
}

impl PolarStereoGrid {
    /// From the plane coordinates of sample `(0, 0)` (PROJ's coordinates with
    /// false easting and northing removed).
    #[allow(clippy::too_many_arguments)]
    pub fn from_projected(
        earth: Earth,
        south: bool,
        lat_ts: f64,
        lon0: f64,
        x0: f64,
        y0: f64,
        dx: f64,
        dy: f64,
    ) -> Self {
        let e = earth.e();
        let phic = if south { -lat_ts } else { lat_ts }.to_radians();
        let akm = if (phic - FRAC_PI_2).abs() < 1e-10 {
            2.0 * earth.a / ((1.0 + e).powf(1.0 + e) * (1.0 - e).powf(1.0 - e)).sqrt()
        } else {
            earth.a * msfn(phic, e * e) / tsfn(phic, e)
        };
        PolarStereoGrid {
            earth,
            south,
            lat_ts,
            lon0,
            x0,
            y0,
            dx,
            dy,
            e,
            akm,
            ys: if south { 1.0 } else { -1.0 },
        }
    }

    /// The GRIB form: the latitude and longitude of sample `(0, 0)`.
    #[allow(clippy::too_many_arguments)]
    pub fn from_first_point(
        earth: Earth,
        south: bool,
        lat_ts: f64,
        lon0: f64,
        first_lat: f64,
        first_lon: f64,
        dx: f64,
        dy: f64,
    ) -> Self {
        let probe = PolarStereoGrid::from_projected(earth, south, lat_ts, lon0, 0.0, 0.0, 1.0, 1.0);
        let (x0, y0) = probe.forward(first_lon, first_lat);
        PolarStereoGrid::from_projected(earth, south, lat_ts, lon0, x0, y0, dx, dy)
    }

    #[inline]
    fn rho(&self, lat: f64) -> f64 {
        let phi = if self.south { -lat } else { lat }.to_radians();
        self.akm * tsfn(phi, self.e)
    }

    /// Plane metres, the pole at the origin.
    pub fn forward(&self, lon: f64, lat: f64) -> (f64, f64) {
        let d = wrap_lon(lon - self.lon0).to_radians();
        let rho = self.rho(lat);
        (rho * d.sin(), self.ys * rho * d.cos())
    }

    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let rho = x.hypot(y);
        let d = x.atan2(self.ys * y);
        let phi = phi_from_ts(rho / self.akm, self.e).to_degrees();
        let lat = if self.south { -phi } else { phi };
        (wrap_lon(self.lon0 + d.to_degrees()), lat)
    }

    pub(crate) fn params(&self) -> Vec<f64> {
        vec![
            self.earth.a,
            self.earth.b,
            f64::from(u8::from(self.south)),
            self.lat_ts,
            self.lon0,
            self.x0,
            self.y0,
            self.dx,
            self.dy,
        ]
    }

    pub(crate) fn from_params(p: &[f64]) -> Option<Self> {
        let &[a, b, south, lat_ts, lon0, x0, y0, dx, dy] = p else {
            return None;
        };
        let earth = Earth { a, b };
        let hemisphere = if south == 1.0 { -lat_ts } else { lat_ts };
        let ok = p.iter().all(|v| v.is_finite())
            && earth.is_plausible()
            && (south == 0.0 || south == 1.0)
            && hemisphere > 0.0
            && hemisphere <= 90.0
            && dx != 0.0
            && dy != 0.0;
        ok.then(|| {
            PolarStereoGrid::from_projected(earth, south == 1.0, lat_ts, lon0, x0, y0, dx, dy)
        })
    }
}

impl Projection for PolarStereoGrid {
    #[inline]
    fn lon_terms(&self, lon: f64) -> Terms {
        let d = wrap_lon(lon - self.lon0).to_radians();
        [d.sin(), d.cos()]
    }

    #[inline]
    fn lat_terms(&self, lat: f64) -> Terms {
        [self.rho(lat), 0.0]
    }

    #[inline]
    fn ij_from_terms(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        let rho = lat[0];
        let (x, y) = (rho * lon[0], self.ys * rho * lon[1]);
        ((x - self.x0) / self.dx, (y - self.y0) / self.dy)
    }

    fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64) {
        self.inverse(self.x0 + i * self.dx, self.y0 + j * self.dy)
    }

    /// Grid north is the plane's +y axis: on a north-polar grid it turns
    /// clockwise from true north by `lon - lon0`, on a south-polar one
    /// counterclockwise.
    fn wind_rotation(&self, lon: f64, _lat: f64) -> (f64, f64) {
        let a = wrap_lon(lon - self.lon0).to_radians() * -self.ys;
        (a.cos(), a.sin())
    }

    fn nominal_spacing_m(&self) -> f64 {
        self.dx.abs().min(self.dy.abs())
    }
}

#[cfg(test)]
mod tests {
    use super::super::GridProjection;
    use super::super::close;
    use super::super::earth::plane_rotation;
    use super::*;

    /// HRRR Alaska: 1299 x 919, 3 km, LaD 60, LoV 225, first point
    /// 41.612949 N 185.117126 E, sphere 6371229 m.
    fn hrrr_ak() -> PolarStereoGrid {
        PolarStereoGrid::from_first_point(
            Earth::sphere(6_371_229.0),
            false,
            60.0,
            225.0,
            41.612949,
            185.117126,
            3000.0,
            3000.0,
        )
    }

    #[test]
    fn hrrr_alaska_corners_match_eccodes() {
        // Corner coordinates eccodes 2.50 computes for the sample file.
        let g = hrrr_ak();
        for (i, j, lat, lon) in [
            (0.0, 0.0, 41.612949, 185.117126),
            (1298.0, 0.0, 51.72556, 231.526897),
            (0.0, 918.0, 55.605448, 156.436843),
            (1298.0, 918.0, 76.337657, 244.224263),
            (0.0, 1.0, 41.63141, 185.096483),
        ] {
            let (glon, glat) = g.ij_to_lonlat(i, j);
            assert!(close(glat, lat, 2e-6), "({i}, {j}): lat {glat} vs {lat}");
            assert!(
                close(wrap_lon(glon - lon), 0.0, 2e-6),
                "({i}, {j}): lon {glon} vs {lon}"
            );
            let (gi, gj) = g.lonlat_to_ij(lon, lat);
            assert!(
                close(gi, i, 1e-3) && close(gj, j, 1e-3),
                "({i}, {j}) -> ({gi}, {gj})"
            );
        }
    }

    /// DWD's RADOLAN grid: `+proj=stere +lat_0=90 +lat_ts=60 +lon_0=10` on
    /// WGS84, 1100 x 1200 cells of 1 km, x_0 = 543196.83521776402,
    /// y_0 = 3622588.8619310018, upper-left outer corner at projected (0, 0).
    fn radolan() -> PolarStereoGrid {
        PolarStereoGrid::from_projected(
            Earth::WGS84,
            false,
            60.0,
            10.0,
            -543_196.835_217_764 + 500.0,
            -3_622_588.861_931_002 - 500.0,
            1000.0,
            -1000.0,
        )
    }

    #[test]
    fn radolan_corners_match_pyproj() {
        // pyproj 3.8 (PROJ 9) for the outer corners and two cell centres.
        let g = radolan();
        for (i, j, lon, lat) in [
            (-0.5, -0.5, 1.472_196_484_961, 55.858_389_752_368),
            (1099.5, -0.5, 18.738_155_632_754, 55.840_423_392_139),
            (-0.5, 1199.5, 3.573_521_877_759, 45.692_777_761_730),
            (1099.5, 1199.5, 16.586_055_314_912, 45.680_022_243_585),
            (550.0, 600.0, 10.099_083_897_038, 50.996_890_667_061),
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
    fn south_polar_grids() {
        // `+proj=stere +lat_0=-90 +lat_ts=-71 +lon_0=0` on WGS84 (EPSG:3031).
        let g = PolarStereoGrid::from_projected(Earth::WGS84, true, -71.0, 0.0, 0.0, 0.0, 1.0, 1.0);
        // pyproj: (lon 30, lat -70) -> (1_097_247.12, 1_900_487.77) metres.
        for (lon, lat, x, y) in [
            (30.0, -70.0, 1_097_247.123_8, 1_900_487.766_9),
            (-100.0, -80.0, -1_072_632.372_3, -189_134.027_6),
        ] {
            let (gx, gy) = g.forward(lon, lat);
            assert!(close(gx, x, 0.001) && close(gy, y, 0.001), "{gx}, {gy}");
            let (glon, glat) = g.inverse(x, y);
            assert!(
                close(glon, lon, 1e-9) && close(glat, lat, 1e-9),
                "{glon}, {glat}"
            );
        }
        // Grid north turns counterclockwise east of lon0, and agrees with the
        // direction the projection itself gives.
        let (c, s) = g.wind_rotation(30.0, -70.0);
        let (c2, s2) = plane_rotation(|lon, lat| g.forward(lon, lat), 30.0, -70.0);
        assert!(
            close(c, c2, 1e-6) && close(s, s2, 1e-6) && s < 0.0,
            "{c} {s} vs {c2} {s2}"
        );
    }

    #[test]
    fn wind_rotation_follows_the_meridians() {
        let g = hrrr_ak();
        for (lon, lat) in [
            (-150.0, 61.0),
            (170.0, 55.0),
            (-120.0, 70.0),
            (-135.0, 50.0),
        ] {
            let (c, s) = g.wind_rotation(lon, lat);
            let (c2, s2) = plane_rotation(|lon, lat| g.forward(lon, lat), lon, lat);
            assert!(close(c, c2, 1e-6) && close(s, s2, 1e-6), "({lon}, {lat})");
        }
        // On the orientation meridian grid north is true north.
        let (c, s) = g.wind_rotation(-135.0, 60.0);
        assert!(close(c, 1.0, 1e-12) && close(s, 0.0, 1e-12));
    }

    #[test]
    fn serde_and_params_round_trip() {
        let g = GridProjection::PolarStereographic(radolan());
        let json = serde_json::to_string(&g).unwrap();
        assert!(!json.contains("akm"), "{json}");
        let back: GridProjection = serde_json::from_str(&json).unwrap();
        assert_eq!(back, g);
        let (kind, p) = g.to_params();
        assert_eq!(GridProjection::from_params(kind, &p), Some(g));
    }

    #[test]
    fn bounds_cross_the_antimeridian_and_can_hold_the_pole() {
        let ak = GridProjection::PolarStereographic(hrrr_ak()).bounds(1299, 919);
        assert!(ak.wraps(), "{ak:?}");
        // eccodes' corners: 156.44 E in the west to 115.78 W in the east,
        // the north edge bulging to 77.1 N between the corners.
        assert!(ak.west > 155.0 && ak.west < 156.44, "{ak:?}");
        assert!(ak.east > -116.0 && ak.east < -115.0, "{ak:?}");
        assert!(
            ak.south < 41.613 && ak.south > 41.5 && ak.north > 77.0 && ak.north < 78.0,
            "{ak:?}"
        );
        // A grid centred on the pole spans every longitude.
        let arctic = PolarStereoGrid::from_projected(
            Earth::WGS84,
            false,
            70.0,
            -45.0,
            -2e6,
            -2e6,
            25_000.0,
            25_000.0,
        );
        let b = GridProjection::PolarStereographic(arctic).bounds(161, 161);
        assert_eq!((b.west, b.east, b.north), (-180.0, 180.0, 90.0), "{b:?}");
        assert!(b.south > 60.0 && b.south < 65.0, "{b:?}");
    }
}
