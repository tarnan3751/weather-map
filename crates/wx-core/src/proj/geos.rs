//! The geostationary fixed grid: scan angles seen from a satellite over the
//! equator (GOES-R ABI, Himawari AHI, GK-2A AMI, Meteosat). Formulas follow
//! the GOES-R Product User Guide (section 4.2.8) and PROJ's `geos`, on the
//! ellipsoid; with `a == b` they are the spherical ones.
//!
//! Points the satellite cannot see map to NaN, which lies outside every grid.

use serde::{Deserialize, Serialize};

use super::earth::plane_rotation;
use super::{Earth, Projection, Terms, wrap_lon};

/// The axis the instrument sweeps around: the order in which the two scan
/// angles are applied. GOES-R scans with the x angle outermost (`X`);
/// Himawari, GK-2A and Meteosat with the y angle outermost (`Y`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sweep {
    X,
    Y,
}

super::defined_by!(GeostationaryGrid, GeosParams, GeostationaryGrid::new, {
    earth: Earth,
    height: f64,
    sub_lon: f64,
    sweep: Sweep,
    x_scale: f64,
    x_offset: f64,
    y_scale: f64,
    y_offset: f64,
});

/// Column `i` looks along scan angle `x = x_offset + i * x_scale` and row
/// `j` along `y = y_offset + j * y_scale`, radians, `x` positive east and
/// `y` positive north (the GOES-R NetCDF convention; `y_scale` is negative
/// for rows that run north to south).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "GeosParams", into = "GeosParams")]
pub struct GeostationaryGrid {
    pub earth: Earth,
    /// Height of the satellite above the ellipsoid (perspective point
    /// height), metres: 35 786 023 for GOES-R.
    pub height: f64,
    /// Longitude of the sub-satellite point, degrees east.
    pub sub_lon: f64,
    pub sweep: Sweep,
    pub x_scale: f64,
    pub x_offset: f64,
    pub y_scale: f64,
    pub y_offset: f64,
    /// Distance of the satellite from the earth's centre, in semi-major axes.
    rg: f64,
    /// `b / a` and its square.
    rp: f64,
    rp2: f64,
}

impl GeostationaryGrid {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        earth: Earth,
        height: f64,
        sub_lon: f64,
        sweep: Sweep,
        x_scale: f64,
        x_offset: f64,
        y_scale: f64,
        y_offset: f64,
    ) -> Self {
        let rp = earth.b / earth.a;
        GeostationaryGrid {
            earth,
            height,
            sub_lon,
            sweep,
            x_scale,
            x_offset,
            y_scale,
            y_offset,
            rg: 1.0 + height / earth.a,
            rp,
            rp2: rp * rp,
        }
    }

    /// Scan angles `(x, y)` of a point, radians; NaN where it is not visible.
    pub fn forward(&self, lon: f64, lat: f64) -> (f64, f64) {
        self.angles(
            &Projection::lon_terms(self, lon),
            &Projection::lat_terms(self, lat),
        )
    }

    #[inline]
    fn angles(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        let (cos_d, sin_d) = (lon[0], lon[1]);
        let (vx, vy, vz) = (lat[0] * cos_d, lat[0] * sin_d, lat[1]);
        // Hidden behind the limb: the line of sight crosses the ellipsoid
        // before reaching the point.
        if (self.rg - vx) * vx - vy * vy - vz * vz / self.rp2 < 0.0 {
            return (f64::NAN, f64::NAN);
        }
        let t = self.rg - vx;
        match self.sweep {
            Sweep::X => ((vy / vz.hypot(t)).atan(), (vz / t).atan()),
            Sweep::Y => ((vy / t).atan(), (vz / vy.hypot(t)).atan()),
        }
    }

    /// The point seen along scan angles `(x, y)`; NaN off the disk.
    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (vy, vz) = match self.sweep {
            Sweep::X => {
                let vz = y.tan();
                (x.tan() * 1f64.hypot(vz), vz)
            }
            Sweep::Y => {
                let vy = x.tan();
                (vy, y.tan() * 1f64.hypot(vy))
            }
        };
        // Where the ray from the satellite along (-1, vy, vz) meets the
        // ellipsoid (in units of `a`).
        let qa = vy * vy + (vz / self.rp) * (vz / self.rp) + 1.0;
        let qb = -2.0 * self.rg;
        let qc = self.rg * self.rg - 1.0;
        let det = qb * qb - 4.0 * qa * qc;
        if det.is_nan() || det < 0.0 {
            return (f64::NAN, f64::NAN);
        }
        let k = (-qb - det.sqrt()) / (2.0 * qa);
        let (px, py, pz) = (self.rg - k, k * vy, k * vz);
        let lon = py.atan2(px).to_degrees();
        let lat = (pz / self.rp2).atan2(px.hypot(py)).to_degrees();
        (wrap_lon(self.sub_lon + lon), lat)
    }

    /// Points on the edge of the visible disk that fall inside an `nx` by
    /// `ny` grid: part of the boundary of what the grid can show.
    pub(crate) fn limb_points(&self, nx: usize, ny: usize) -> Vec<(f64, f64)> {
        let inside = |x: f64, y: f64| {
            let i = (x - self.x_offset) / self.x_scale;
            let j = (y - self.y_offset) / self.y_scale;
            i >= -0.5 && i <= nx as f64 - 0.5 && j >= -0.5 && j <= ny as f64 - 0.5
        };
        // A line of sight (-1, vy, vz) grazes the ellipsoid where the
        // discriminant in `inverse` vanishes: vy^2 + (vz / rp)^2 =
        // 1 / (rg^2 - 1), an ellipse. Walk it a hair inside, every 0.125
        // degree, which keeps the bulge between points under the bounds'
        // margin.
        let k = (1.0 - 1e-9) / (self.rg * self.rg - 1.0).sqrt();
        let mut out = Vec::new();
        for step in 0..2880 {
            let (s, c) = (step as f64 * 0.125).to_radians().sin_cos();
            let (vy, vz) = (k * c, k * self.rp * s);
            let (x, y) = match self.sweep {
                Sweep::X => ((vy / 1f64.hypot(vz)).atan(), vz.atan()),
                Sweep::Y => (vy.atan(), (vz / 1f64.hypot(vy)).atan()),
            };
            if inside(x, y) {
                out.push(self.inverse(x, y));
            }
        }
        out
    }

    pub(crate) fn params(&self) -> Vec<f64> {
        vec![
            self.earth.a,
            self.earth.b,
            self.height,
            self.sub_lon,
            match self.sweep {
                Sweep::X => 0.0,
                Sweep::Y => 1.0,
            },
            self.x_scale,
            self.x_offset,
            self.y_scale,
            self.y_offset,
        ]
    }

    pub(crate) fn from_params(p: &[f64]) -> Option<Self> {
        let &[
            a,
            b,
            height,
            sub_lon,
            sweep,
            x_scale,
            x_offset,
            y_scale,
            y_offset,
        ] = p
        else {
            return None;
        };
        let earth = Earth { a, b };
        let sweep = match sweep {
            0.0 => Sweep::X,
            1.0 => Sweep::Y,
            _ => return None,
        };
        let ok = p.iter().all(|v| v.is_finite())
            && earth.is_plausible()
            && (1.0e7..=1.0e8).contains(&height)
            && x_scale != 0.0
            && y_scale != 0.0;
        ok.then(|| {
            GeostationaryGrid::new(
                earth, height, sub_lon, sweep, x_scale, x_offset, y_scale, y_offset,
            )
        })
    }
}

impl Projection for GeostationaryGrid {
    #[inline]
    fn lon_terms(&self, lon: f64) -> Terms {
        let d = wrap_lon(lon - self.sub_lon).to_radians();
        [d.cos(), d.sin()]
    }

    /// The point's distance from the earth's axis and from the equatorial
    /// plane, in units of `a`, through its geocentric latitude.
    #[inline]
    fn lat_terms(&self, lat: f64) -> Terms {
        let pc = (self.rp2 * lat.to_radians().tan()).atan();
        let (s, c) = pc.sin_cos();
        let r = self.rp / (self.rp * c).hypot(s);
        [r * c, r * s]
    }

    #[inline]
    fn ij_from_terms(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        let (x, y) = self.angles(lon, lat);
        (
            (x - self.x_offset) / self.x_scale,
            (y - self.y_offset) / self.y_scale,
        )
    }

    fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64) {
        self.inverse(
            self.x_offset + i * self.x_scale,
            self.y_offset + j * self.y_scale,
        )
    }

    /// The rotation from true north to the image's "up" (+y scan angle).
    /// No producer issues grid-relative winds on a geostationary grid; this
    /// is here so every projection answers.
    fn wind_rotation(&self, lon: f64, lat: f64) -> (f64, f64) {
        plane_rotation(|lon, lat| self.forward(lon, lat), lon, lat)
    }

    /// Spacing at the sub-satellite point.
    fn nominal_spacing_m(&self) -> f64 {
        self.x_scale.abs().min(self.y_scale.abs()) * self.height
    }
}

#[cfg(test)]
mod tests {
    use super::super::GridProjection;
    use super::super::close;
    use super::*;

    /// GOES-19 ABI full disk at 2 km, as its NetCDF `goes_imager_projection`
    /// and `x`/`y` variables state it.
    fn goes_east() -> GeostationaryGrid {
        GeostationaryGrid::new(
            Earth {
                a: 6_378_137.0,
                b: 6_356_752.314_14,
            },
            35_786_023.0,
            -75.0,
            Sweep::X,
            5.6e-05,
            -0.151_844,
            -5.6e-05,
            0.151_844,
        )
    }

    #[test]
    fn scan_angles_match_pyproj() {
        let goes = goes_east();
        for (lon, lat, x, y) in [
            (-75.0, 0.0, 0.0, 0.0),
            (
                -100.0,
                40.0,
                -0.054_432_847_957_979_945,
                0.107_665_812_603_201_57,
            ),
            (
                -40.0,
                -30.0,
                0.083_758_144_113_492_08,
                -0.084_040_107_852_077_4,
            ),
            (-75.0, 60.0, 0.0, 0.140_230_632_525_084_3),
        ] {
            let (gx, gy) = goes.forward(lon, lat);
            assert!(
                close(gx, x, 1e-12) && close(gy, y, 1e-12),
                "({lon}, {lat}): {gx}, {gy}"
            );
            let (glon, glat) = goes.inverse(x, y);
            assert!(
                close(glon, lon, 1e-9) && close(glat, lat, 1e-9),
                "({lon}, {lat}): {glon}, {glat}"
            );
        }
        let himawari = GeostationaryGrid::new(
            Earth::WGS84,
            35_785_863.0,
            140.7,
            Sweep::Y,
            1.0,
            0.0,
            1.0,
            0.0,
        );
        for (lon, lat, x, y) in [
            (140.7, 0.0, 0.0, 0.0),
            (
                100.0,
                30.0,
                -0.094_651_741_348_169_75,
                0.082_926_236_991_294_95,
            ),
            (
                170.0,
                -40.0,
                0.063_097_682_768_092_66,
                -0.106_982_297_894_493_97,
            ),
            (140.7, 70.0, 0.0, 0.148_269_204_537_108_3),
        ] {
            let (gx, gy) = himawari.forward(lon, lat);
            assert!(
                close(gx, x, 1e-12) && close(gy, y, 1e-12),
                "({lon}, {lat}): {gx}, {gy}"
            );
            let (glon, glat) = himawari.inverse(x, y);
            assert!(
                close(wrap_lon(glon - lon), 0.0, 1e-9) && close(glat, lat, 1e-9),
                "({lon}, {lat})"
            );
        }
    }

    #[test]
    fn off_the_disk_is_outside_the_grid() {
        let g = GridProjection::Geostationary(goes_east());
        for (lon, lat) in [(105.0, 0.0), (-75.0, 89.0), (30.0, 10.0), (180.0, -50.0)] {
            let (i, j) = g.lonlat_to_ij(lon, lat);
            assert!(i.is_nan() && j.is_nan(), "({lon}, {lat}) -> ({i}, {j})");
        }
        // The grid's corners look past the earth.
        let (lon, lat) = g.ij_to_lonlat(0.0, 0.0);
        assert!(lon.is_nan() && lat.is_nan());
        let (lon, lat) = g.ij_to_lonlat(2711.5, 2711.5);
        assert!(
            close(lon, -75.0, 1e-3) && close(lat, 0.0, 1e-3),
            "{lon} {lat}"
        );
        // The bounds come from the limb: about 81.3 degrees each way.
        let b = g.bounds(5424, 5424);
        assert!(
            close(b.west, -156.3, 0.2) && close(b.east, 6.3, 0.2),
            "{b:?}"
        );
        assert!(
            close(b.north, 81.3, 0.2) && close(b.south, -81.3, 0.2),
            "{b:?}"
        );
        let (kind, p) = g.to_params();
        assert_eq!(GridProjection::from_params(kind, &p), Some(g));
    }
}
