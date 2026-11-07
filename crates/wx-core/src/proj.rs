//! Coordinate systems: Web-Mercator tiles on one side, model grids on the other.
//!
//! Everything a tiler needs is `GridProjection::lonlat_to_ij`: for each output
//! sample it asks "where in the source grid does this lon/lat fall?".

use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Sphere radius NCEP uses for its Lambert grids (GRIB2 earth shape 6), metres.
pub const EARTH_RADIUS_NCEP: f64 = 6_371_229.0;
/// Latitude where Web-Mercator's unit square ends.
pub const MAX_MERCATOR_LAT: f64 = 85.051_128_779_806_6;
/// Cells per tile edge. A tile stores `TILE_CELLS + 1` samples per edge, see [`TileCoord::sample_lonlat`].
pub const TILE_CELLS: usize = 256;
/// Samples per tile edge (lattice points, shared with neighbouring tiles).
pub const TILE_SAMPLES: usize = TILE_CELLS + 1;
pub const MAX_ZOOM: u8 = 14;

/// Web-Mercator x in the unit square, 0 at 180°W, 1 at 180°E.
pub fn lon_to_x(lon: f64) -> f64 {
    (lon + 180.0) / 360.0
}

/// Web-Mercator y in the unit square, 0 at the north edge, 1 at the south edge.
pub fn lat_to_y(lat: f64) -> f64 {
    let s = lat
        .clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT)
        .to_radians()
        .sin();
    0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * PI)
}

pub fn x_to_lon(x: f64) -> f64 {
    x * 360.0 - 180.0
}

pub fn y_to_lat(y: f64) -> f64 {
    (PI * (1.0 - 2.0 * y)).sinh().atan().to_degrees()
}

/// Wrap a longitude into `[-180, 180)`.
pub fn wrap_lon(lon: f64) -> f64 {
    rem_turn(lon + 180.0) - 180.0
}

/// `x.rem_euclid(360.0)`, bit for bit, without the division for the
/// arguments projections mostly give it, within a turn either side: there
/// one subtraction or addition of 360 is exact, or is what `rem_euclid`
/// itself does.
#[inline]
pub(crate) fn rem_turn(x: f64) -> f64 {
    if (0.0..360.0).contains(&x) {
        x
    } else if (360.0..720.0).contains(&x) {
        x - 360.0
    } else if -360.0 < x && x < 0.0 {
        x + 360.0
    } else {
        x.rem_euclid(360.0)
    }
}

/// A box in longitude and latitude.
///
/// Longitudes are in `[-180, 180]`. `west > east` means the box crosses the
/// antimeridian: it runs east from `west` to 180 and on from -180 to `east`
/// (Alaska's grids do this). `west == -180 && east == 180` is every longitude.
/// Serialized as `{"west":..,"south":..,"east":..,"north":..}`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LonLatBox {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl LonLatBox {
    /// Everything Web-Mercator tiles can show, and then some.
    pub const WORLD: LonLatBox = LonLatBox::new(-180.0, -90.0, 180.0, 90.0);

    pub const fn new(west: f64, south: f64, east: f64, north: f64) -> Self {
        Self {
            west,
            south,
            east,
            north,
        }
    }

    /// True when the box crosses the antimeridian.
    pub fn wraps(&self) -> bool {
        self.west > self.east
    }

    /// The box's longitudes as one or two ordinary `(west, east)` intervals.
    fn lon_spans(&self) -> ([(f64, f64); 2], usize) {
        if self.wraps() {
            ([(self.west, 180.0), (-180.0, self.east)], 2)
        } else {
            ([(self.west, self.east), (0.0, 0.0)], 1)
        }
    }

    /// Do the two boxes share any area? Boxes that only touch along an edge
    /// do not intersect.
    pub fn intersects(&self, other: &LonLatBox) -> bool {
        if !(self.south < other.north && other.south < self.north) {
            return false;
        }
        let (a, na) = self.lon_spans();
        let (b, nb) = other.lon_spans();
        a[..na]
            .iter()
            .any(|&(aw, ae)| b[..nb].iter().any(|&(bw, be)| aw < be && bw < ae))
    }

    /// Is `other` entirely inside this box (edges may coincide)?
    pub fn contains_box(&self, other: &LonLatBox) -> bool {
        if !(self.south <= other.south && other.north <= self.north) {
            return false;
        }
        if self.west == -180.0 && self.east == 180.0 {
            return true;
        }
        match (self.wraps(), other.wraps()) {
            (false, false) => self.west <= other.west && other.east <= self.east,
            (true, true) => self.west <= other.west && other.east <= self.east,
            (true, false) => other.west >= self.west || other.east <= self.east,
            (false, true) => false,
        }
    }

    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        if !(lat >= self.south && lat <= self.north) {
            return false;
        }
        if self.wraps() {
            lon >= self.west || lon <= self.east
        } else {
            lon >= self.west && lon <= self.east
        }
    }

    /// The box with every edge moved outward by `deg` degrees (inward when
    /// negative), kept within the globe. `None` when shrinking leaves nothing.
    pub fn grown(&self, deg: f64) -> Option<LonLatBox> {
        let south = (self.south - deg).max(-90.0);
        let north = (self.north + deg).min(90.0);
        if south >= north {
            return None;
        }
        let width = if self.wraps() {
            self.east - self.west + 360.0
        } else {
            self.east - self.west
        };
        let new_width = width + 2.0 * deg;
        if new_width <= 0.0 {
            return None;
        }
        if new_width >= 360.0 {
            return Some(LonLatBox::new(-180.0, south, 180.0, north));
        }
        let fold = |lon: f64| {
            if lon > 180.0 {
                lon - 360.0
            } else if lon < -180.0 {
                lon + 360.0
            } else {
                lon
            }
        };
        Some(LonLatBox::new(
            fold(self.west - deg),
            south,
            fold(self.east + deg),
            north,
        ))
    }

    /// Tiles at zoom `z` that touch this box, as inclusive
    /// `(x_min, y_min, x_max, y_max)` ranges: one, or two for a box that
    /// crosses the antimeridian.
    pub fn tile_ranges(&self, z: u8) -> Vec<(u32, u32, u32, u32)> {
        let n = (1u64 << z) as f64;
        let max = (1u32 << z) - 1;
        let clamp = |v: f64| (v.floor().max(0.0) as u32).min(max);
        // Nudge the far edges inward so a box ending exactly on a tile
        // boundary does not pull in the next tile.
        let eps = 1e-9;
        let y0 = clamp(lat_to_y(self.north) * n);
        let y1 = clamp(lat_to_y(self.south) * n - eps);
        let (spans, count) = self.lon_spans();
        spans[..count]
            .iter()
            .map(|&(w, e)| (clamp(lon_to_x(w) * n), y0, clamp(lon_to_x(e) * n - eps), y1))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TileCoord {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileCoord {
    pub fn new(z: u8, x: u32, y: u32) -> Option<Self> {
        if z > MAX_ZOOM {
            return None;
        }
        let n = 1u32 << z;
        (x < n && y < n).then_some(Self { z, x, y })
    }

    pub fn bounds(&self) -> LonLatBox {
        let n = (1u64 << self.z) as f64;
        LonLatBox {
            west: x_to_lon(self.x as f64 / n),
            east: x_to_lon((self.x + 1) as f64 / n),
            north: y_to_lat(self.y as f64 / n),
            south: y_to_lat((self.y + 1) as f64 / n),
        }
    }

    /// Longitude/latitude of sample `(col, row)`, both in `0..=TILE_CELLS`.
    ///
    /// Samples sit on the tile's lattice points, not at cell centres: sample 0
    /// is on the tile's west/north edge and sample `TILE_CELLS` on its
    /// east/south edge, so neighbouring tiles repeat each other's edge samples.
    /// That lets the client interpolate inside one tile without ever needing a
    /// neighbour, and the map stays seamless.
    pub fn sample_lonlat(&self, col: usize, row: usize) -> (f64, f64) {
        let n = (1u64 << self.z) as f64;
        let x = (self.x as f64 + col as f64 / TILE_CELLS as f64) / n;
        let y = (self.y as f64 + row as f64 / TILE_CELLS as f64) / n;
        (x_to_lon(x), y_to_lat(y))
    }
}

/// Serialize a projection by its defining fields only, so that a
/// deserialized grid rebuilds its derived constants and is always
/// self-consistent: `$params` holds the fields and `$build` takes them in
/// order. The grid itself carries `#[serde(from = "$params", into =
/// "$params")]`.
macro_rules! defined_by {
    ($grid:ident, $params:ident, $build:path, { $($field:ident: $ty:ty),+ $(,)? }) => {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct $params {
            $($field: $ty),+
        }

        impl From<$params> for $grid {
            fn from(p: $params) -> Self {
                $build($(p.$field),+)
            }
        }

        impl From<$grid> for $params {
            fn from(g: $grid) -> Self {
                $params { $($field: g.$field),+ }
            }
        }
    };
}
use defined_by;

mod bounds;
mod earth;
mod geos;
mod laea;
mod lambert;
mod latlon;
mod mercator;
mod rotated;
mod stereo;
mod tmerc;

pub use earth::Earth;
pub use geos::{GeostationaryGrid, Sweep};
pub use laea::LaeaGrid;
pub use lambert::LambertGrid;
pub use latlon::LatLonGrid;
pub use mercator::MercatorGrid;
pub use rotated::RotatedLatLonGrid;
pub use stereo::PolarStereoGrid;
pub use tmerc::TransverseMercatorGrid;

/// What a projection precomputes for one longitude or one latitude; see
/// [`Projection`].
pub type Terms = [f64; 2];

/// What each grid projection provides.
///
/// The forward map is split into a part that depends only on longitude, a
/// part that depends only on latitude, and a cheap combination of the two.
/// A tile's samples lie on a lattice whose longitudes depend only on the
/// column and latitudes only on the row, so the tiler computes the two
/// parts once per column and once per row and only combines them per
/// sample: for most projections that leaves a few multiplications per
/// sample instead of a dozen transcendental functions. `lonlat_to_ij` is
/// defined as exactly that composition, so both paths give bit-identical
/// results.
///
/// Adding a projection: a struct implementing this trait, a variant of
/// [`GridProjection`], an arm in its dispatch and in `to_params` /
/// `from_params`.
pub trait Projection {
    fn lon_terms(&self, lon: f64) -> Terms;
    fn lat_terms(&self, lat: f64) -> Terms;
    fn ij_from_terms(&self, lon: &Terms, lat: &Terms) -> (f64, f64);

    /// Fractional grid index of a point; NaN where the projection cannot
    /// place it (behind a geostationary satellite's limb, say).
    fn lonlat_to_ij(&self, lon: f64, lat: f64) -> (f64, f64) {
        self.ij_from_terms(&self.lon_terms(lon), &self.lat_terms(lat))
    }

    fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64);

    /// See [`GridProjection::wind_rotation`].
    fn wind_rotation(&self, lon: f64, lat: f64) -> (f64, f64);

    fn nominal_spacing_m(&self) -> f64;
}

/// How a source grid's `(i, j)` indices relate to the earth.
///
/// `i` counts along a row, `j` counts rows, and sample `(i, j)` lives at index
/// `j * nx + i` of the decoded data in the order the GRIB message stored it
/// (after rows stored east to west, or alternate rows of a boustrophedon
/// scan, have been reversed). A grid scanned north-to-south simply has a
/// negative `dlat` / `dy`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GridProjection {
    /// GRIB2 template 3.0.
    LatLon(LatLonGrid),
    /// GRIB2 template 3.30, spherical.
    Lambert(LambertGrid),
    /// GRIB2 template 3.20; PROJ `stere` with `lat_0 = +-90`.
    PolarStereographic(PolarStereoGrid),
    /// GRIB2 template 3.10; PROJ `merc`.
    Mercator(MercatorGrid),
    /// GRIB2 template 3.1.
    RotatedLatLon(RotatedLatLonGrid),
    /// PROJ `laea`.
    LambertAzimuthal(LaeaGrid),
    /// PROJ `tmerc`.
    TransverseMercator(TransverseMercatorGrid),
    /// The GOES-R / Himawari fixed grid; PROJ `geos`.
    Geostationary(GeostationaryGrid),
}

macro_rules! each {
    ($self:expr, $g:ident => $e:expr) => {
        match $self {
            GridProjection::LatLon($g) => $e,
            GridProjection::Lambert($g) => $e,
            GridProjection::PolarStereographic($g) => $e,
            GridProjection::Mercator($g) => $e,
            GridProjection::RotatedLatLon($g) => $e,
            GridProjection::LambertAzimuthal($g) => $e,
            GridProjection::TransverseMercator($g) => $e,
            GridProjection::Geostationary($g) => $e,
        }
    };
}

impl GridProjection {
    /// Fractional grid index of a point. The result can lie outside the grid,
    /// or be NaN where the projection cannot place the point; callers
    /// bounds-check against `nx`/`ny` (NaN fails every check).
    #[inline]
    pub fn lonlat_to_ij(&self, lon: f64, lat: f64) -> (f64, f64) {
        each!(self, g => Projection::lonlat_to_ij(g, lon, lat))
    }

    pub fn ij_to_lonlat(&self, i: f64, j: f64) -> (f64, f64) {
        each!(self, g => Projection::ij_to_lonlat(g, i, j))
    }

    /// The longitude-only part of [`GridProjection::lonlat_to_ij`].
    #[inline]
    pub fn lon_terms(&self, lon: f64) -> Terms {
        each!(self, g => g.lon_terms(lon))
    }

    /// The latitude-only part of [`GridProjection::lonlat_to_ij`].
    #[inline]
    pub fn lat_terms(&self, lat: f64) -> Terms {
        each!(self, g => g.lat_terms(lat))
    }

    /// `lonlat_to_ij(lon, lat)` from `lon_terms(lon)` and `lat_terms(lat)`,
    /// bit for bit.
    #[inline]
    pub fn ij_from_terms(&self, lon: &Terms, lat: &Terms) -> (f64, f64) {
        each!(self, g => g.ij_from_terms(lon, lat))
    }

    /// `(cos a, sin a)` of the angle that turns grid-relative wind components
    /// into earth-relative ones at `(lon, lat)`:
    ///
    /// ```text
    /// u_earth =  cos a * u_grid + sin a * v_grid
    /// v_earth = -sin a * u_grid + cos a * v_grid
    /// ```
    ///
    /// `a` is the angle from true north clockwise to grid north (the
    /// direction of increasing projected y, or rotated latitude). It depends
    /// on longitude alone for conic and polar grids and on both coordinates
    /// for rotated ones. Only needed when the GRIB grid definition flags its
    /// vectors as grid-relative (resolution-and-component flag bit 0x08).
    pub fn wind_rotation(&self, lon: f64, lat: f64) -> (f64, f64) {
        each!(self, g => g.wind_rotation(lon, lat))
    }

    /// False when grid north is true north everywhere, so grid-relative
    /// winds need no rotation.
    pub fn rotates_winds(&self) -> bool {
        !matches!(
            self,
            GridProjection::LatLon(_) | GridProjection::Mercator(_)
        )
    }

    /// Approximate ground spacing of the grid, metres.
    pub fn nominal_spacing_m(&self) -> f64 {
        each!(self, g => g.nominal_spacing_m())
    }

    /// True for global lat/lon grids whose columns wrap around.
    pub fn is_periodic(&self) -> bool {
        matches!(self, GridProjection::LatLon(g) if g.periodic)
    }

    /// Lon/lat box that encloses the cells of an `nx` by `ny` grid: each
    /// sample stands for the cell around it, so the box reaches half a
    /// sample beyond the outer samples. It crosses the antimeridian
    /// (`west > east`) when the grid does, and spans every longitude when
    /// the grid holds a pole or wraps.
    pub fn bounds(&self, nx: usize, ny: usize) -> LonLatBox {
        bounds::grid_bounds(self, nx, ny)
    }

    /// A compact form for binary stores: a kind number and the defining
    /// parameters (flags as 0 or 1). [`GridProjection::from_params`] inverts
    /// it exactly.
    pub fn to_params(&self) -> (u8, Vec<f64>) {
        match self {
            GridProjection::LatLon(g) => (1, g.params()),
            GridProjection::Lambert(g) => (2, g.params()),
            GridProjection::PolarStereographic(g) => (3, g.params()),
            GridProjection::Mercator(g) => (4, g.params()),
            GridProjection::RotatedLatLon(g) => (5, g.params()),
            GridProjection::LambertAzimuthal(g) => (6, g.params()),
            GridProjection::TransverseMercator(g) => (7, g.params()),
            GridProjection::Geostationary(g) => (8, g.params()),
        }
    }

    /// `None` for an unknown kind, the wrong number of parameters, or
    /// parameters no real grid has (non-finite, zero spacing, an implausible
    /// earth).
    pub fn from_params(kind: u8, p: &[f64]) -> Option<GridProjection> {
        Some(match kind {
            1 => GridProjection::LatLon(LatLonGrid::from_params(p)?),
            2 => GridProjection::Lambert(LambertGrid::from_params(p)?),
            3 => GridProjection::PolarStereographic(PolarStereoGrid::from_params(p)?),
            4 => GridProjection::Mercator(MercatorGrid::from_params(p)?),
            5 => GridProjection::RotatedLatLon(RotatedLatLonGrid::from_params(p)?),
            6 => GridProjection::LambertAzimuthal(LaeaGrid::from_params(p)?),
            7 => GridProjection::TransverseMercator(TransverseMercatorGrid::from_params(p)?),
            8 => GridProjection::Geostationary(GeostationaryGrid::from_params(p)?),
            _ => return None,
        })
    }
}

/// Whether `a` and `b` agree to `tol`, for the projections' tests.
#[cfg(test)]
pub(crate) fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rem_turn_is_rem_euclid() {
        let mut v = vec![
            0.0,
            -0.0,
            360.0,
            -360.0,
            720.0,
            -720.0,
            1e-300,
            -1e-300,
            359.999_999_999_999_94,
            -359.999_999_999_999_94,
            719.999_999_999_999_9,
            f64::NAN,
            f64::INFINITY,
            1e20,
        ];
        let mut x = 0.123_456_789f64;
        for _ in 0..100_000 {
            x = (x * 7.3 + 0.61).fract();
            v.push((x - 0.5) * 1600.0);
            v.push((x - 0.5) * 1e-3);
        }
        for x in v {
            assert_eq!(rem_turn(x).to_bits(), x.rem_euclid(360.0).to_bits(), "{x}");
        }
    }

    #[test]
    fn mercator_round_trip() {
        for lat in [-80.0, -33.3, 0.0, 21.1, 38.5, 54.9, 84.0] {
            assert!(close(y_to_lat(lat_to_y(lat)), lat, 1e-9));
        }
        assert!(close(lat_to_y(0.0), 0.5, 1e-12));
        assert!(close(lat_to_y(MAX_MERCATOR_LAT), 0.0, 1e-9));
        assert!(close(x_to_lon(lon_to_x(-97.5)), -97.5, 1e-12));
    }

    #[test]
    fn tile_bounds_and_samples() {
        let t = TileCoord::new(0, 0, 0).unwrap();
        let b = t.bounds();
        assert!(close(b.west, -180.0, 1e-9) && close(b.east, 180.0, 1e-9));
        assert!(close(b.north, MAX_MERCATOR_LAT, 1e-6));
        let t = TileCoord::new(4, 3, 6).unwrap();
        let b = t.bounds();
        let (lon, lat) = t.sample_lonlat(0, 0);
        assert!(close(lon, b.west, 1e-9) && close(lat, b.north, 1e-9));
        let (lon, lat) = t.sample_lonlat(TILE_CELLS, TILE_CELLS);
        assert!(close(lon, b.east, 1e-9) && close(lat, b.south, 1e-9));
        // The east edge of one tile is the west edge of the next.
        let east = TileCoord::new(4, 4, 6).unwrap();
        assert_eq!(t.sample_lonlat(TILE_CELLS, 40), east.sample_lonlat(0, 40));
        assert!(TileCoord::new(3, 8, 0).is_none());
    }

    #[test]
    fn tile_ranges_cover_conus_and_the_world() {
        let conus = LonLatBox::new(-138.0, 15.0, -52.0, 60.0);
        assert_eq!(conus.tile_ranges(0), vec![(0, 0, 0, 0)]);
        let (x0, y0, x1, y1) = conus.tile_ranges(5)[0];
        assert!(x0 <= 4 && x1 >= 10 && y0 <= 10 && y1 >= 13);
        assert_eq!(LonLatBox::WORLD.tile_ranges(2), vec![(0, 0, 3, 3)]);
    }

    #[test]
    fn boxes_that_cross_the_antimeridian() {
        // Roughly HRRR Alaska: from 170E eastward to 115W.
        let alaska = LonLatBox::new(170.0, 40.0, -115.0, 76.0);
        assert!(alaska.wraps());
        assert!(alaska.contains(-150.0, 61.0) && alaska.contains(175.0, 52.0));
        assert!(!alaska.contains(0.0, 60.0) && !alaska.contains(-150.0, 30.0));
        let conus = LonLatBox::new(-138.0, 15.0, -52.0, 60.0);
        let europe = LonLatBox::new(-25.0, 30.0, 45.0, 72.0);
        assert!(alaska.intersects(&conus) && conus.intersects(&alaska));
        assert!(!alaska.intersects(&europe) && !europe.intersects(&alaska));
        assert!(LonLatBox::WORLD.intersects(&alaska) && LonLatBox::WORLD.contains_box(&alaska));
        assert!(alaska.contains_box(&LonLatBox::new(175.0, 50.0, -170.0, 60.0)));
        assert!(alaska.contains_box(&LonLatBox::new(-160.0, 55.0, -140.0, 70.0)));
        assert!(!alaska.contains_box(&conus) && !conus.contains_box(&alaska));
        // Two ranges at zoom 3: the columns east of 170E and those west of 115W.
        let ranges = alaska.tile_ranges(3);
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].0, ranges[0].2), (7, 7));
        assert_eq!((ranges[1].0, ranges[1].2), (0, 1));
        // Touching is not intersecting.
        let a = LonLatBox::new(0.0, 0.0, 10.0, 10.0);
        assert!(!a.intersects(&LonLatBox::new(10.0, 0.0, 20.0, 10.0)));
    }

    #[test]
    fn growing_and_shrinking_boxes() {
        let a = LonLatBox::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(a.grown(1.0), Some(LonLatBox::new(-1.0, -1.0, 11.0, 11.0)));
        assert_eq!(a.grown(-1.0), Some(LonLatBox::new(1.0, 1.0, 9.0, 9.0)));
        assert_eq!(a.grown(-5.0), None);
        let edge = LonLatBox::new(175.0, 0.0, 179.5, 10.0).grown(1.0).unwrap();
        assert!(edge.wraps() && edge.west == 174.0 && edge.east == -179.5);
        assert_eq!(LonLatBox::WORLD.grown(1.0), Some(LonLatBox::WORLD));
        let wide = LonLatBox::new(-179.0, -10.0, 179.5, 10.0)
            .grown(1.0)
            .unwrap();
        assert_eq!((wide.west, wide.east), (-180.0, 180.0));
    }
}
