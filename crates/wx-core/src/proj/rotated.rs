//! Rotated latitude/longitude grids (GRIB2 template 3.1): a regular
//! lat/lon grid on a sphere whose poles have been moved, as Canada's RDPS
//! and HRDPS and many European regional models use so their domain sits on
//! the rotated equator, where cells are nearly square.
//!
//! The rotation follows the GRIB2 definition: rotate the sphere by the
//! south pole's longitude about the polar axis, then by `90 + south pole
//! latitude` degrees about the new y axis, so the southern pole of the
//! rotated system lands where the template says. An angle of rotation about
//! the new polar axis is not supported (no producer we use sets one, and
//! implementations disagree on its sign); decoders refuse it.
//!
//! Latitudes and longitudes are treated as coordinates on a sphere whatever
//! earth the producer names: the rotation is defined in those coordinates.

use serde::{Deserialize, Serialize};

use super::{EARTH_RADIUS_NCEP, LatLonGrid, Projection, Terms, wrap_lon};

super::defined_by!(RotatedLatLonGrid, RotatedParams, RotatedLatLonGrid::new, {
    south_pole_lat: f64,
    south_pole_lon: f64,
    grid: LatLonGrid,
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "RotatedParams", into = "RotatedParams")]
pub struct RotatedLatLonGrid {
    /// Geographic latitude of the rotated system's southern pole, degrees.
    pub south_pole_lat: f64,
    /// Geographic longitude of the rotated system's southern pole, degrees east.
    pub south_pole_lon: f64,
    /// The grid in rotated coordinates (`lon0`/`lat0` are rotated
    /// coordinates of sample `(0, 0)`). Never periodic.
    pub grid: LatLonGrid,
    /// cos and sin of `90 + south_pole_lat`.
    c: f64,
    s: f64,
    /// sin and cos of the geographic latitude of the rotated north pole.
    np_sin: f64,
    np_cos: f64,
}

impl RotatedLatLonGrid {
    pub fn new(south_pole_lat: f64, south_pole_lon: f64, mut grid: LatLonGrid) -> Self {
        grid.periodic = false;
        let theta = (90.0 + south_pole_lat).to_radians();
        let np = (-south_pole_lat).to_radians();
        RotatedLatLonGrid {
            south_pole_lat,
            south_pole_lon,
            grid,
            c: theta.cos(),
            s: theta.sin(),
            np_sin: np.sin(),
            np_cos: np.cos(),
        }
    }

    /// Geographic to rotated coordinates, degrees.
    pub fn to_rotated(&self, lon: f64, lat: f64) -> (f64, f64) {
        self.rotate(
            &Projection::lon_terms(self, lon),
            &Projection::lat_terms(self, lat),
        )
    }

    #[inline]
    fn rotate(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        let (sin_d, cos_d) = (lon[0], lon[1]);
        let (sin_p, cos_p) = (lat[0], lat[1]);
        let (x, y, z) = (cos_p * cos_d, cos_p * sin_d, sin_p);
        let xr = self.c * x + self.s * z;
        let zr = self.c * z - self.s * x;
        (
            y.atan2(xr).to_degrees(),
            zr.clamp(-1.0, 1.0).asin().to_degrees(),
        )
    }

    /// Rotated to geographic coordinates, degrees.
    pub fn from_rotated(&self, lon_r: f64, lat_r: f64) -> (f64, f64) {
        let (lr, pr) = (lon_r.to_radians(), lat_r.to_radians());
        let (xr, yr, zr) = (pr.cos() * lr.cos(), pr.cos() * lr.sin(), pr.sin());
        let x = self.c * xr - self.s * zr;
        let z = self.s * xr + self.c * zr;
        (
            wrap_lon(self.south_pole_lon + yr.atan2(x).to_degrees()),
            z.clamp(-1.0, 1.0).asin().to_degrees(),
        )
    }

    pub(crate) fn params(&self) -> Vec<f64> {
        let mut p = vec![self.south_pole_lat, self.south_pole_lon];
        p.extend(self.grid.params());
        p
    }

    pub(crate) fn from_params(p: &[f64]) -> Option<Self> {
        let (&[lat, lon], rest) = p.split_first_chunk::<2>()?;
        let grid = LatLonGrid::from_params(rest)?;
        (lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && !grid.periodic)
            .then(|| RotatedLatLonGrid::new(lat, lon, grid))
    }
}

impl Projection for RotatedLatLonGrid {
    #[inline]
    fn lon_terms(&self, lon: f64) -> Terms {
        let d = (lon - self.south_pole_lon).to_radians();
        [d.sin(), d.cos()]
    }

    #[inline]
    fn lat_terms(&self, lat: f64) -> Terms {
        let p = lat.to_radians();
        [p.sin(), p.cos()]
    }

    #[inline]
    fn ij_from_terms(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        let (lon_r, lat_r) = self.rotate(lon, lat);
        self.grid.lonlat_to_ij(lon_r, lat_r)
    }

    fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64) {
        let (lon_r, lat_r) = self.grid.ij_to_lonlat(i, j);
        self.from_rotated(lon_r, lat_r)
    }

    /// Grid north is the direction of the rotated north pole, which is at
    /// geographic `(south_pole_lon + 180, -south_pole_lat)`: the azimuth
    /// of the great circle from the point to it.
    fn wind_rotation(&self, lon: f64, lat: f64) -> (f64, f64) {
        let d = (self.south_pole_lon + 180.0 - lon).to_radians();
        let p = lat.to_radians();
        let (east, north) = (
            d.sin() * self.np_cos,
            p.cos() * self.np_sin - p.sin() * self.np_cos * d.cos(),
        );
        let r = east.hypot(north);
        if r > 0.0 {
            (north / r, east / r)
        } else {
            (1.0, 0.0)
        }
    }

    fn nominal_spacing_m(&self) -> f64 {
        self.grid.dlat.abs().to_radians() * EARTH_RADIUS_NCEP
    }
}

#[cfg(test)]
mod tests {
    use super::super::GridProjection;
    use super::super::close;
    use super::*;

    /// HRDPS as its GRIB2 template 3.1 states it.
    fn hrdps() -> RotatedLatLonGrid {
        RotatedLatLonGrid::new(
            -36.08852,
            245.305142,
            LatLonGrid {
                lon0: 345.17878,
                lat0: -12.302501,
                dlon: 0.0225,
                dlat: 0.0225,
                periodic: false,
            },
        )
    }

    #[test]
    fn hrdps_corners_match_eccodes() {
        let g = hrdps();
        // pyproj's `ob_tran` with the CF form of the same pole. eccodes 2.50
        // gives the same corners to within the float rounding it applies
        // (2e-5 degree), and the research notes list them.
        for (i, j, lat, lon) in [
            (0.0, 0.0, 39.626_033_829_7, -133.629_519_519_4),
            (2539.0, 0.0, 27.284_599_258_0, -66.966_424_705_0),
            (0.0, 1289.0, 66.568_539_172_6, -152.730_663_456_6),
            (2539.0, 1289.0, 47.876_458_657_8, -40.708_565_707_1),
            (0.0, 1.0, 39.647_708_074_8, -133.637_362_143_6),
        ] {
            let (glon, glat) = Projection::ij_to_lonlat(&g, i, j);
            assert!(
                close(glat, lat, 1e-9) && close(glon, lon, 1e-9),
                "({i}, {j}): {glon}, {glat}"
            );
            let (gi, gj) = g.lonlat_to_ij(lon, lat);
            assert!(
                close(gi, i, 1e-3) && close(gj, j, 1e-3),
                "({i}, {j}) -> ({gi}, {gj})"
            );
        }
    }

    #[test]
    fn rdps_reaches_over_the_pole_and_across_the_antimeridian() {
        let g = GridProjection::RotatedLatLon(RotatedLatLonGrid::new(
            -31.758312,
            267.597031,
            LatLonGrid {
                lon0: 306.141412,
                lat0: -48.805954,
                dlon: 0.090298,
                dlat: 0.090298,
                periodic: false,
            },
        ));
        // pyproj, as above.
        for (i, j, lat, lon) in [
            (0.0, 0.0, -3.771_776_324_9, -124.613_050_821_6),
            (1139.0, 0.0, -1.638_799_219_9, -62.588_408_663_4),
            (0.0, 1044.0, 46.626_503_472_8, 143.157_374_216_6),
            (1139.0, 1044.0, 50.039_624_386_4, 32.107_236_853_0),
        ] {
            let (glon, glat) = g.ij_to_lonlat(i, j);
            assert!(
                close(glat, lat, 1e-9) && close(glon, lon, 1e-9),
                "({i}, {j}): {glon}, {glat}"
            );
        }
        let b = g.bounds(1140, 1045);
        assert_eq!((b.west, b.east, b.north), (-180.0, 180.0, 90.0), "{b:?}");
        assert!(b.south < -3.77 && b.south > -4.0, "{b:?}");
    }

    #[test]
    fn rotation_round_trips() {
        let g = hrdps();
        for (lon, lat) in [(-100.0, 50.0), (-60.0, 45.0), (179.0, 80.0), (10.0, -30.0)] {
            let (lr, pr) = g.to_rotated(lon, lat);
            let (lon2, lat2) = g.from_rotated(lr, pr);
            assert!(
                close(wrap_lon(lon2 - lon), 0.0, 1e-9) && close(lat2, lat, 1e-9),
                "({lon}, {lat})"
            );
        }
        // The rotated south pole is where the template says.
        let (_, pr) = g.to_rotated(245.305142, -36.08852);
        assert!(close(pr, -90.0, 1e-6));
    }

    /// Grid north measured with the projection itself, by stepping along
    /// the rotated meridian.
    #[test]
    fn wind_rotation_points_along_rotated_meridians() {
        let g = hrdps();
        for (i, j) in [
            (10.0, 10.0),
            (1270.0, 645.0),
            (2500.0, 1200.0),
            (100.0, 1250.0),
        ] {
            let (lon, lat) = Projection::ij_to_lonlat(&g, i, j);
            let (lon2, lat2) = Projection::ij_to_lonlat(&g, i, j + 1e-3);
            let east = wrap_lon(lon2 - lon).to_radians() * lat.to_radians().cos();
            let north = (lat2 - lat).to_radians();
            let az = east.atan2(north);
            let (c, s) = g.wind_rotation(lon, lat);
            assert!(
                close(c, az.cos(), 1e-5) && close(s, az.sin(), 1e-5),
                "({i}, {j}): {c} {s} vs {az}"
            );
        }
    }

    #[test]
    fn serde_and_params_round_trip() {
        let g = GridProjection::RotatedLatLon(hrdps());
        let back: GridProjection =
            serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back, g);
        let (kind, p) = g.to_params();
        assert_eq!(GridProjection::from_params(kind, &p), Some(g));
    }
}
