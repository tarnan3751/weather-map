//! Regular latitude/longitude grids (GRIB2 template 3.0).

use serde::{Deserialize, Serialize};

use super::{EARTH_RADIUS_NCEP, Projection, Terms, rem_turn, wrap_lon};

/// Regular latitude/longitude grid: sample `(i, j)` is at
/// `(lon0 + i * dlon, lat0 + j * dlat)`. Latitude and longitude are taken as
/// they come, whatever earth the producer had in mind: the grid is defined in
/// those coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatLonGrid {
    pub lon0: f64,
    pub lat0: f64,
    /// Must be positive: columns run west to east. Decoders flip rows that
    /// were stored east to west.
    pub dlon: f64,
    /// Negative when rows run north to south.
    pub dlat: f64,
    /// True for global grids whose columns wrap around the date line.
    pub periodic: bool,
}

impl LatLonGrid {
    /// Column index of a longitude.
    ///
    /// Longitude is measured eastward from the first column so grids written
    /// with 0..360 longitudes and callers using -180..180 agree. A regional
    /// grid's longitudes are cut one column west of its first: every
    /// longitude on the grid (from half a cell west of the first column to
    /// half a cell east of the last) then has its own index, however wide the
    /// grid, since a grid that is not periodic leaves at least one and a half
    /// cells uncovered. A longitude off the grid indexes off it, below -0.5
    /// within a cell of the first column and beyond the last column farther
    /// west.
    #[inline]
    pub fn i_of(&self, lon: f64) -> f64 {
        let dl = if self.periodic {
            rem_turn(lon - self.lon0)
        } else {
            rem_turn(lon - self.lon0 + self.dlon) - self.dlon
        };
        dl / self.dlon
    }

    #[inline]
    pub fn j_of(&self, lat: f64) -> f64 {
        (lat - self.lat0) / self.dlat
    }

    pub fn lonlat_to_ij(&self, lon: f64, lat: f64) -> (f64, f64) {
        (self.i_of(lon), self.j_of(lat))
    }

    pub fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64) {
        (
            wrap_lon(self.lon0 + i * self.dlon),
            self.lat0 + j * self.dlat,
        )
    }

    pub(crate) fn params(&self) -> Vec<f64> {
        vec![
            self.lon0,
            self.lat0,
            self.dlon,
            self.dlat,
            f64::from(u8::from(self.periodic)),
        ]
    }

    pub(crate) fn from_params(p: &[f64]) -> Option<Self> {
        let &[lon0, lat0, dlon, dlat, periodic] = p else {
            return None;
        };
        let ok = [lon0, lat0, dlon, dlat].iter().all(|v| v.is_finite())
            && dlon > 0.0
            && dlat != 0.0
            && (periodic == 0.0 || periodic == 1.0);
        ok.then_some(LatLonGrid {
            lon0,
            lat0,
            dlon,
            dlat,
            periodic: periodic == 1.0,
        })
    }
}

impl Projection for LatLonGrid {
    #[inline]
    fn lon_terms(&self, lon: f64) -> Terms {
        [self.i_of(lon), 0.0]
    }

    #[inline]
    fn lat_terms(&self, lat: f64) -> Terms {
        [self.j_of(lat), 0.0]
    }

    #[inline]
    fn ij_from_terms(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        (lon[0], lat[0])
    }

    fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64) {
        LatLonGrid::ij_to_lonlat(self, i, j)
    }

    fn wind_rotation(&self, _lon: f64, _lat: f64) -> (f64, f64) {
        (1.0, 0.0)
    }

    fn nominal_spacing_m(&self) -> f64 {
        self.dlat.abs().to_radians() * EARTH_RADIUS_NCEP
    }
}

#[cfg(test)]
mod tests {
    use super::super::GridProjection;
    use super::super::close;
    use super::*;

    #[test]
    fn latlon_grid_north_to_south() {
        // MRMS: 7000 x 3500, 0.01 degree, first point in the north-west.
        let g = LatLonGrid {
            lon0: 230.005,
            lat0: 54.995,
            dlon: 0.01,
            dlat: -0.01,
            periodic: false,
        };
        let (i, j) = g.lonlat_to_ij(-129.995, 54.995);
        assert!(close(i, 0.0, 1e-6) && close(j, 0.0, 1e-6));
        let (i, j) = g.lonlat_to_ij(-60.005, 20.005);
        assert!(close(i, 6999.0, 1e-6) && close(j, 3499.0, 1e-6));
        // Off the grid to the west: off it, whichever side it lands.
        for lon in [-129.995 - 0.0051, -131.0, -175.0, 100.0] {
            let (i, _) = g.lonlat_to_ij(lon, 40.0);
            assert!(!(-0.5..=6999.5).contains(&i), "{lon}: {i}");
        }
        let (i, _) = g.lonlat_to_ij(-129.995 - 0.0049, 40.0);
        assert!(close(i, -0.49, 1e-6), "half a cell west is still on: {i}");
        let (lon, lat) = g.ij_to_lonlat(6999.0, 3499.0);
        assert!(close(lon, -60.005, 1e-6) && close(lat, 20.005, 1e-6));
        // The box reaches half a cell beyond the outer samples.
        let b = GridProjection::LatLon(g).bounds(7000, 3500);
        assert!(
            close(b.west, -130.0, 2e-4) && close(b.north, 55.0, 2e-4),
            "{b:?}"
        );
        assert!(
            close(b.east, -60.0, 2e-4) && close(b.south, 20.0, 2e-4),
            "{b:?}"
        );
    }

    #[test]
    fn latlon_grid_global_wraps() {
        // GFS 0.25 degree: 1440 x 721, 0E eastward, 90N southward.
        let g = LatLonGrid {
            lon0: 0.0,
            lat0: 90.0,
            dlon: 0.25,
            dlat: -0.25,
            periodic: true,
        };
        let (i, j) = g.lonlat_to_ij(-97.5, 35.0);
        assert!(close(i, 1050.0, 1e-9) && close(j, 220.0, 1e-9));
        let (i, _) = g.lonlat_to_ij(-0.1, 0.0);
        assert!(close(i, 1439.6, 1e-9));
        let b = GridProjection::LatLon(g).bounds(1440, 721);
        assert_eq!(
            (b.west, b.south, b.east, b.north),
            (-180.0, -90.0, 180.0, 90.0)
        );
    }

    #[test]
    fn regional_grid_across_the_antimeridian() {
        // MRMS Alaska: 5000 x 2200 at 0.01 degree from 184.005 E.
        let g = GridProjection::LatLon(LatLonGrid {
            lon0: 184.005,
            lat0: 71.995,
            dlon: 0.01,
            dlat: -0.01,
            periodic: false,
        });
        let b = g.bounds(5000, 2200);
        assert!(
            !b.wraps(),
            "184 E to 234 E is all west of the antimeridian: {b:?}"
        );
        assert!(
            close(b.west, -176.0, 2e-4) && close(b.east, -126.0, 2e-4),
            "{b:?}"
        );
        // The same grid started 10 degrees further west crosses it.
        let g = GridProjection::LatLon(LatLonGrid {
            lon0: 174.005,
            lat0: 71.995,
            dlon: 0.01,
            dlat: -0.01,
            periodic: false,
        });
        let b = g.bounds(5000, 2200);
        assert!(
            b.wraps() && close(b.west, 174.0, 2e-4) && close(b.east, -136.0, 2e-4),
            "{b:?}"
        );
        let (i, _) = g.lonlat_to_ij(-179.0, 60.0);
        assert!(close(i, 699.5, 1e-6), "{i}");
    }

    /// A regional grid wider than 270 degrees, and one that is a column
    /// short of going round: every longitude on them has its own column.
    #[test]
    fn wide_regional_grids_keep_their_east_part() {
        let g = LatLonGrid {
            lon0: -100.0,
            lat0: 60.0,
            dlon: 0.25,
            dlat: -0.25,
            periodic: false,
        };
        // 1120 columns: -100 to 179.75.
        for (lon, want) in [
            (-100.0, 0.0),
            (0.0, 400.0),
            (179.0, 1116.0),
            (179.75, 1119.0),
        ] {
            assert!(close(g.i_of(lon), want, 1e-9), "{lon}: {}", g.i_of(lon));
        }
        assert!(close(g.i_of(179.85), 1119.4, 1e-9), "within the last cell");
        let off = g.i_of(-100.3);
        assert!(off < -0.5 || off > 1119.5, "{off}");
        let b = GridProjection::LatLon(g.clone()).bounds(1120, 100);
        let (i, _) = g.lonlat_to_ij(b.east - 0.01, 50.0);
        assert!(
            i > 1118.0 && i <= 1119.5,
            "east edge of the box is the grid's: {i}"
        );
        // 3599 columns of 0.1 degree, not periodic.
        let g = LatLonGrid {
            lon0: 0.0,
            lat0: 90.0,
            dlon: 0.1,
            dlat: -0.1,
            periodic: false,
        };
        assert!(close(g.i_of(-75.0), 2850.0, 1e-9), "{}", g.i_of(-75.0));
        assert!(close(g.i_of(359.8), 3598.0, 1e-9));
        assert!(close(g.i_of(-0.04), -0.4, 1e-9));
        let gap = g.i_of(-0.12);
        assert!(gap < -0.5 || gap > 3598.5, "{gap}");
    }
}
