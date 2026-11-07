//! The lon/lat box that encloses a grid's cells, for any projection.
//!
//! Each sample stands for the cell around it, so the box reaches half a
//! sample beyond the outer samples. Without a pole inside, a region's
//! latitudes reach their extremes on its boundary and its longitudes are the
//! boundary's, so walking the boundary is enough: the grid's edge, plus for
//! a geostationary grid the part of the limb that lies inside it (edge
//! points that look past the earth have no position). Longitudes are an arc:
//! the complement of the widest gap between the boundary's longitudes, which
//! crosses the antimeridian when the gap does not. A gap no wider than the
//! largest step between consecutive boundary points may be bridged by the
//! boundary itself, so then every longitude is in. A grid holding a pole
//! spans every longitude.

use super::{GridProjection, LonLatBox, wrap_lon};

/// Room for the edge bulging between the points walked.
const MARGIN_DEG: f64 = 1e-4;
/// Edge points walked per side at most (one per cell below this).
const MAX_EDGE_POINTS: usize = 8192;

pub(crate) fn grid_bounds(p: &GridProjection, nx: usize, ny: usize) -> LonLatBox {
    if nx == 0 || ny == 0 {
        return LonLatBox::new(0.0, 0.0, 0.0, 0.0);
    }
    let (w, h) = (nx as f64, ny as f64);
    if let GridProjection::LatLon(g) = p {
        let a = g.lat0 - 0.5 * g.dlat;
        let b = g.lat0 + (h - 0.5) * g.dlat;
        let (south, north) = (
            (a.min(b) - MARGIN_DEG).max(-90.0),
            (a.max(b) + MARGIN_DEG).min(90.0),
        );
        let width = w * g.dlon + 2.0 * MARGIN_DEG;
        if g.periodic || width >= 360.0 {
            return LonLatBox::new(-180.0, south, 180.0, north);
        }
        let west = wrap_lon(g.lon0 - 0.5 * g.dlon - MARGIN_DEG);
        return LonLatBox::new(west, south, fold(west + width), north);
    }

    let mut lons = Vec::with_capacity(2 * (nx.min(MAX_EDGE_POINTS) + ny.min(MAX_EDGE_POINTS) + 2));
    let (mut south, mut north) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut max_step: f64 = 0.0;
    // Each run of points is walked in order along the boundary, so the
    // steps between neighbours bound what the boundary can bridge.
    let mut walk = |points: &mut dyn Iterator<Item = (f64, f64)>| {
        let mut prev: Option<f64> = None;
        for (lon, lat) in points {
            if !(lon.is_finite() && lat.is_finite()) {
                prev = None;
                continue;
            }
            let lon = wrap_lon(lon);
            if let Some(q) = prev {
                let d = (lon - q).rem_euclid(360.0);
                max_step = max_step.max(d.min(360.0 - d));
            }
            prev = Some(lon);
            lons.push(lon);
            south = south.min(lat);
            north = north.max(lat);
        }
    };
    let si = nx.min(MAX_EDGE_POINTS);
    let sj = ny.min(MAX_EDGE_POINTS);
    let along = |k: usize, n: usize, len: f64| -0.5 + len * k as f64 / n as f64;
    walk(&mut (0..=si).map(|k| p.ij_to_lonlat(along(k, si, w), -0.5)));
    walk(&mut (0..=si).map(|k| p.ij_to_lonlat(along(k, si, w), h - 0.5)));
    walk(&mut (0..=sj).map(|k| p.ij_to_lonlat(-0.5, along(k, sj, h))));
    walk(&mut (0..=sj).map(|k| p.ij_to_lonlat(w - 0.5, along(k, sj, h))));
    if let GridProjection::Geostationary(g) = p {
        walk(&mut g.limb_points(nx, ny).into_iter());
    }
    if lons.is_empty() {
        return LonLatBox::new(0.0, 0.0, 0.0, 0.0);
    }
    let inside = |(i, j): (f64, f64)| i >= -0.5 && i <= w - 0.5 && j >= -0.5 && j <= h - 0.5;
    let north_pole = inside(p.lonlat_to_ij(0.0, 90.0));
    let south_pole = inside(p.lonlat_to_ij(0.0, -90.0));
    if north_pole {
        north = 90.0;
    }
    if south_pole {
        south = -90.0;
    }
    let (south, north) = (
        (south - MARGIN_DEG).max(-90.0),
        (north + MARGIN_DEG).min(90.0),
    );
    if north_pole || south_pole {
        return LonLatBox::new(-180.0, south, 180.0, north);
    }
    let (west, east) = lon_arc(&mut lons, max_step);
    LonLatBox::new(west, south, east, north)
}

/// Back into [-180, 180] from just beyond it.
fn fold(lon: f64) -> f64 {
    if lon > 180.0 {
        lon - 360.0
    } else if lon < -180.0 {
        lon + 360.0
    } else {
        lon
    }
}

/// The smallest arc holding every longitude, grown by the margin; every
/// longitude when no gap is wider than `bridged`.
fn lon_arc(lons: &mut [f64], bridged: f64) -> (f64, f64) {
    lons.sort_by(f64::total_cmp);
    let n = lons.len();
    // The gap after lons[k]; the last one wraps round to the first.
    let gap = |k: usize| {
        if k + 1 < n {
            lons[k + 1] - lons[k]
        } else {
            lons[0] + 360.0 - lons[n - 1]
        }
    };
    let widest = (0..n)
        .max_by(|&a, &b| gap(a).total_cmp(&gap(b)))
        .expect("not empty");
    if gap(widest) <= 1.5 * bridged {
        return (-180.0, 180.0);
    }
    let west = lons[(widest + 1) % n];
    let east = lons[widest];
    let width = (east - west).rem_euclid(360.0) + 2.0 * MARGIN_DEG;
    if width >= 360.0 {
        return (-180.0, 180.0);
    }
    (fold(west - MARGIN_DEG), fold(east + MARGIN_DEG))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arcs() {
        assert_eq!(
            lon_arc(&mut [10.0, -20.0, 5.0], 1.0),
            (-20.0 - MARGIN_DEG, 10.0 + MARGIN_DEG)
        );
        let (w, e) = lon_arc(&mut [170.0, -170.0, 179.0, -175.0], 1.0);
        assert!(
            (w - (170.0 - MARGIN_DEG)).abs() < 1e-9 && (e - (-170.0 + MARGIN_DEG)).abs() < 1e-9
        );
        let mut all: Vec<f64> = (0..360).map(|d| d as f64 - 180.0).collect();
        assert_eq!(lon_arc(&mut all, 1.0), (-180.0, 180.0));
        // A real gap wider than the boundary's steps is kept out.
        let mut band: Vec<f64> = (0..350).map(|d| d as f64 - 180.0).collect();
        assert_eq!(
            lon_arc(&mut band, 1.0),
            (fold(-180.0 - MARGIN_DEG), 169.0 + MARGIN_DEG)
        );
    }
}
