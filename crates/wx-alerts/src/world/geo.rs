//! From CAP areas to geometry PostGIS can store.
//!
//! CAP polygons are single `lat,lon` rings in either orientation, and may
//! cross the antimeridian (a ring from 179 E to 179 W is a few kilometres
//! wide, not a band round the planet). Each ring is unwrapped here so its
//! longitudes are continuous (they may then run past +/-180); PostGIS then
//! repairs the shape, splits it at the antimeridian with `ST_WrapX`, and
//! orients it (see [`SHAPE_SQL`]). Circles become rings of points at the
//! given distance on the sphere, so they are true circles on the ground at
//! any latitude, including ones that cross the antimeridian or contain a
//! pole.

use super::cap::{Area, Circle};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Mean Earth radius (IUGG), in kilometres.
const EARTH_RADIUS_KM: f64 = 6371.0088;
/// Points on a circle's ring.
const CIRCLE_POINTS: usize = 72;

/// The shape of an alert from its areas, as GeoJSON MultiPolygons in
/// unwrapped longitudes: `(shape, exclusions)`. `None` when no area has a
/// polygon or circle.
pub(crate) fn shape_geojson(areas: &[&Area]) -> Option<(String, Option<String>)> {
    let mut parts: Vec<Value> = Vec::new();
    let mut cut: Vec<Value> = Vec::new();
    for area in areas {
        parts.extend(area.polygons.iter().map(|r| json!([unwrap(r)])));
        parts.extend(area.circles.iter().map(|c| json!([circle_ring(c)])));
        cut.extend(area.excludes.iter().map(|r| json!([unwrap(r)])));
    }
    if parts.is_empty() {
        return None;
    }
    let multi = |p: Vec<Value>| json!({ "type": "MultiPolygon", "coordinates": p }).to_string();
    let cut = (!cut.is_empty()).then(|| multi(cut));
    Some((multi(parts), cut))
}

/// SQL that turns `$1` (shape) and `$2` (exclusions, or NULL) from
/// [`shape_geojson`] into a valid, counter-clockwise MultiPolygon within
/// +/-180 degrees, or NULL. The `structure` repair unions overlapping
/// rings instead of treating their overlap as a hole.
pub(crate) const SHAPE_SQL: &str = "
    SELECT CASE WHEN w.g IS NULL OR ST_IsEmpty(w.g) THEN NULL ELSE w.g END
    FROM (SELECT ST_SetSRID(ST_ForcePolygonCCW(ST_Multi(ST_CollectionExtract(ST_MakeValid(
                 ST_WrapX(ST_WrapX(
                     CASE WHEN $2::text IS NULL THEN s.g
                          ELSE ST_Difference(s.g, ST_MakeValid(ST_GeomFromGeoJSON($2::text), 'method=structure')) END,
                     180, -360), -180, 360)), 3))), 4326) AS g
          FROM (SELECT ST_MakeValid(ST_GeomFromGeoJSON($1::text), 'method=structure') AS g) s) w";

/// `(lon, lat)` points of a CAP ring, with each longitude moved by whole
/// turns so that no step between neighbours is longer than half a turn.
pub(crate) fn unwrap(ring: &[(f64, f64)]) -> Vec<[f64; 2]> {
    let mut out: Vec<[f64; 2]> = Vec::with_capacity(ring.len());
    let mut prev: Option<f64> = None;
    for &(lat, lon) in ring {
        let lon = match prev {
            Some(p) => p + wrap180(lon - p),
            None => lon,
        };
        prev = Some(lon);
        out.push([lon, lat]);
    }
    // The ring was closed; after unwrapping its ends may differ by a turn
    // (a ring round a pole). Close it again on the first point.
    if let (Some(first), Some(last)) = (out.first().copied(), out.last_mut())
        && (last[0] - first[0]).abs() > 1e-9
        && (last[1] - first[1]).abs() < 1e-9
    {
        *last = first;
    }
    out
}

/// `x` moved into [-180, 180).
fn wrap180(x: f64) -> f64 {
    (x + 180.0).rem_euclid(360.0) - 180.0
}

/// A circle's ring as `(lon, lat)`, longitudes continuous around the
/// centre. A circle that contains a pole is closed along the pole's
/// latitude, which is what such a cap looks like in longitude/latitude.
pub(crate) fn circle_ring(c: &Circle) -> Vec<[f64; 2]> {
    let (lat1, lon1) = (c.lat.to_radians(), c.lon.to_radians());
    let d = (c.radius_km / EARTH_RADIUS_KM).min(std::f64::consts::PI * 0.999);
    let mut ring: Vec<[f64; 2]> = Vec::with_capacity(CIRCLE_POINTS + 3);
    let mut prev: Option<f64> = None;
    for i in 0..CIRCLE_POINTS {
        // Clockwise from north as bearings go; orientation is fixed later.
        let bearing = std::f64::consts::TAU * i as f64 / CIRCLE_POINTS as f64;
        let lat2 = (lat1.sin() * d.cos() + lat1.cos() * d.sin() * bearing.cos()).asin();
        let dlon = (bearing.sin() * d.sin() * lat1.cos()).atan2(d.cos() - lat1.sin() * lat2.sin());
        let lon = (lon1 + dlon).to_degrees();
        let lon = match prev {
            Some(p) => p + wrap180(lon - p),
            None => lon,
        };
        prev = Some(lon);
        ring.push([lon, lat2.to_degrees()]);
    }
    let contains_pole = d >= (std::f64::consts::FRAC_PI_2 - c.lat.abs().to_radians());
    if contains_pole {
        // The ring winds once round the pole: the longitudes cover a full
        // turn. Close it through the pole's latitude.
        let pole = if c.lat >= 0.0 { 90.0 } else { -90.0 };
        let first = ring[0];
        let last_lon = first[0]
            + if ring[ring.len() - 1][0] > first[0] {
                360.0
            } else {
                -360.0
            };
        ring.push([last_lon, first[1]]);
        ring.push([last_lon, pole]);
        ring.push([first[0], pole]);
    }
    ring.push(ring[0]);
    ring
}

/// A fingerprint of where an alert is, for feeds that never link an update
/// to what it replaces: the area's codes, polygons and circles, rounded to
/// two decimals (about a kilometre) so a re-issue of the same area matches.
pub(crate) fn area_signature(areas: &[&Area]) -> Option<String> {
    let mut items: Vec<String> = Vec::new();
    for a in areas {
        for (name, value) in &a.geocodes {
            items.push(format!("g:{}={}", name.to_ascii_uppercase(), value));
        }
        for ring in &a.polygons {
            let mut pts: Vec<String> = ring
                .iter()
                .map(|(la, lo)| format!("{la:.2},{lo:.2}"))
                .collect();
            pts.sort();
            pts.dedup();
            items.push(format!("p:{}", pts.join(" ")));
        }
        for c in &a.circles {
            items.push(format!("c:{:.2},{:.2},{:.0}", c.lat, c.lon, c.radius_km));
        }
    }
    if items.is_empty() {
        return None;
    }
    items.sort();
    items.dedup();
    let digest = Sha256::digest(items.join("\n").as_bytes());
    Some(hex(&digest[..12]))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Great-circle distance in kilometres.
#[cfg(test)]
fn distance_km(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (lon1, lat1, lon2, lat2) = (
        a[0].to_radians(),
        a[1].to_radians(),
        b[0].to_radians(),
        b[1].to_radians(),
    );
    let h = ((lat2 - lat1) / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * ((lon2 - lon1) / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * h.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antimeridian_rings_are_made_continuous() {
        // Fiji-like: from 179 E across to 179 W.
        let ring = [
            (-17.0, 179.0),
            (-17.0, -179.0),
            (-18.0, -179.0),
            (-18.0, 179.0),
            (-17.0, 179.0),
        ];
        let u = unwrap(&ring);
        assert_eq!(u[1], [181.0, -17.0]);
        assert_eq!(u[2], [181.0, -18.0]);
        assert_eq!(u.first(), u.last());
        let lons: Vec<f64> = u.iter().map(|p| p[0]).collect();
        assert!(
            lons.iter().cloned().fold(f64::MIN, f64::max)
                - lons.iter().cloned().fold(f64::MAX, f64::min)
                <= 2.0
        );
        // An ordinary ring is untouched.
        let plain = [(10.0, 20.0), (10.0, 21.0), (11.0, 21.0), (10.0, 20.0)];
        assert_eq!(
            unwrap(&plain),
            [[20.0, 10.0], [21.0, 10.0], [21.0, 11.0], [20.0, 10.0]]
        );
    }

    #[test]
    fn circles_are_round_on_the_ground() {
        for (lat, lon, r) in [
            (-19.0487, -169.8619, 14.0),
            (60.0, 25.0, 50.0),
            (0.0, 179.9, 30.0),
            (78.0, 15.0, 100.0),
        ] {
            let c = Circle {
                lat,
                lon,
                radius_km: r,
            };
            let ring = circle_ring(&c);
            assert_eq!(ring.first(), ring.last());
            assert_eq!(ring.len(), CIRCLE_POINTS + 1);
            for p in &ring {
                let d = distance_km([lon, lat], *p);
                assert!(
                    (d - r).abs() < r * 1e-6,
                    "{d} km from the centre, wanted {r}"
                );
            }
        }
        // Across the antimeridian the longitudes stay continuous.
        let ring = circle_ring(&Circle {
            lat: 0.0,
            lon: 179.9,
            radius_km: 30.0,
        });
        assert!(ring.iter().any(|p| p[0] > 180.0));
        assert!(ring.iter().all(|p| (p[0] - 179.9).abs() < 1.0));
    }

    #[test]
    fn a_circle_round_a_pole_closes_through_it() {
        let ring = circle_ring(&Circle {
            lat: 89.0,
            lon: 0.0,
            radius_km: 500.0,
        });
        assert_eq!(ring.first(), ring.last());
        assert!(ring.iter().any(|p| p[1] == 90.0));
        let lons: Vec<f64> = ring.iter().map(|p| p[0]).collect();
        let span = lons.iter().cloned().fold(f64::MIN, f64::max)
            - lons.iter().cloned().fold(f64::MAX, f64::min);
        assert!((span - 360.0).abs() < 1e-6, "{span}");
    }

    #[test]
    fn shapes_and_signatures() {
        let area = Area {
            polygons: vec![vec![(10.0, 20.0), (10.0, 21.0), (11.0, 21.0), (10.0, 20.0)]],
            circles: vec![Circle {
                lat: 5.0,
                lon: 5.0,
                radius_km: 10.0,
            }],
            excludes: vec![vec![(10.1, 20.5), (10.1, 20.6), (10.2, 20.6), (10.1, 20.5)]],
            ..Area::default()
        };
        let (shape, cut) = shape_geojson(&[&area]).unwrap();
        let v: Value = serde_json::from_str(&shape).unwrap();
        assert_eq!(v["coordinates"].as_array().unwrap().len(), 2);
        assert!(cut.is_some());
        assert!(shape_geojson(&[&Area::default()]).is_none());

        let sig = area_signature(&[&area]).unwrap();
        let mut nudged = area.clone();
        nudged.polygons[0][1].0 += 0.0001;
        assert_eq!(
            area_signature(&[&nudged]).unwrap(),
            sig,
            "a re-issue of the same area matches"
        );
        let mut moved = area.clone();
        moved.polygons[0][1].0 += 0.5;
        assert_ne!(area_signature(&[&moved]).unwrap(), sig);
        assert!(area_signature(&[&Area::default()]).is_none());
    }
}
