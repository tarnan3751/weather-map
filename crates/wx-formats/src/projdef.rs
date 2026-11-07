//! PROJ strings (ODIM's `where/projdef`, and what GeoTIFF keys amount to)
//! to [`GridProjection`]s.
//!
//! A datum shift of zero (`+towgs84=0,0,0`, as SMHI states its Bessel
//! grid) is taken to leave latitude and longitude unchanged. PROJ instead
//! converts between the two ellipsoids through geocentric coordinates, which
//! moves points by tens of metres (25 m across SMHI's 2 km grid); a
//! non-zero shift is refused.
//!
//! Only projections [`GridProjection`] can represent are accepted:
//! `longlat`, polar `stere`, `laea`, `tmerc`, `merc`, `lcc` on a sphere and
//! `geos`. Anything that would move points and is not understood (an
//! unknown parameter, a datum shift, a prime meridian) is refused rather
//! than ignored, so a grid is either placed right or not at all.

use anyhow::{Result, anyhow, bail, ensure};
use wx_core::GridProjection;
use wx_core::proj::{
    Earth, GeostationaryGrid, LaeaGrid, LambertGrid, LatLonGrid, MercatorGrid, PolarStereoGrid, Sweep,
    TransverseMercatorGrid,
};

/// The figure of the earth a PROJ operation assumes when a string names
/// none. (PROJ's CRS layer assumes WGS84 instead; the two differ by
/// micrometres in these projections.)
pub const PROJ_DEFAULT_EARTH: Earth = Earth::GRS80;

/// A projection and the earth it is defined on, as PROJ states it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjDef {
    pub kind: Kind,
    pub earth: Earth,
    /// False easting and northing, metres (degrees for `longlat`).
    pub x_0: f64,
    pub y_0: f64,
}

/// The projection proper; angles in degrees.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    LatLon,
    PolarStereographic {
        south: bool,
        lat_ts: f64,
        lon_0: f64,
    },
    LambertAzimuthal {
        lat_0: f64,
        lon_0: f64,
    },
    TransverseMercator {
        lat_0: f64,
        lon_0: f64,
        k_0: f64,
    },
    Mercator {
        lat_ts: f64,
        lon_0: f64,
    },
    LambertConformal {
        lat_1: f64,
        lat_2: f64,
        lat_0: f64,
        lon_0: f64,
    },
    /// Plane coordinates are scan angles times `h`, as in PROJ.
    Geostationary {
        h: f64,
        lon_0: f64,
        sweep: Sweep,
    },
}

/// Parameters that never move a point.
const IGNORED: &[&str] = &["no_defs", "type", "wktext", "over"];

impl ProjDef {
    /// Parse a PROJ.4-style string such as
    /// `+proj=stere +lat_0=90 +lat_ts=60 +lon_0=10 +a=6378137 +b=6356752.3142451802 +x_0=543196.8 +y_0=3622588.9 +units=m`.
    pub fn parse(s: &str) -> Result<ProjDef> {
        let mut list: Vec<(&str, Option<&str>)> = Vec::new();
        for token in s.split_whitespace() {
            let token = token.strip_prefix('+').unwrap_or(token);
            let (key, value) = match token.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (token, None),
            };
            if list.iter().any(|(k, _)| *k == key) {
                bail!("projdef {s:?}: {key} given twice");
            }
            list.push((key, value));
        }
        let params = Params { s, list };
        let get = |key: &str| params.get(key);
        let num = |key: &str| params.num(key);
        let deg = |key: &str, default: f64| -> Result<f64> { Ok(num(key)?.unwrap_or(default)) };

        let known = [
            "proj", "ellps", "datum", "a", "b", "rf", "f", "R", "towgs84", "units", "lat_0", "lon_0", "lat_ts",
            "lat_1", "lat_2", "k", "k_0", "x_0", "y_0", "h", "sweep",
        ];
        for (key, _) in &params.list {
            if !known.contains(key) && !IGNORED.contains(key) {
                bail!("projdef {s:?}: unsupported parameter +{key}");
            }
        }
        if let Some(t) = get("towgs84") {
            let t = t.ok_or_else(|| anyhow!("projdef {s:?}: towgs84 has no value"))?;
            let zero = t.split(',').all(|v| v.trim().parse::<f64>().is_ok_and(|x| x == 0.0));
            ensure!(zero, "projdef {s:?}: datum shift towgs84={t} is not supported");
        }
        let unit = match get("units") {
            None | Some(Some("m")) => 1.0,
            Some(Some("km")) => 1000.0,
            Some(u) => bail!("projdef {s:?}: units {u:?} not supported"),
        };
        let earth = parse_earth(&params)?;
        let scale = num("k_0")?.or(num("k")?);
        let x_0 = deg("x_0", 0.0)? * unit;
        let y_0 = deg("y_0", 0.0)? * unit;
        let proj = get("proj").flatten().ok_or_else(|| anyhow!("projdef {s:?}: no +proj"))?;
        let no_scale = |what: &str| -> Result<()> {
            match scale {
                Some(k) if (k - 1.0).abs() > 1e-12 => bail!("projdef {s:?}: scale factor {k} not supported for {what}"),
                _ => Ok(()),
            }
        };
        let kind = match proj {
            "longlat" | "latlong" | "lonlat" | "latlon" => Kind::LatLon,
            "stere" => {
                let lat_0 = deg("lat_0", 0.0)?;
                ensure!(
                    (lat_0.abs() - 90.0).abs() < 1e-9,
                    "projdef {s:?}: only polar stereographic (lat_0 = +-90) is supported"
                );
                let lat_ts = match num("lat_ts")? {
                    Some(t) => t,
                    None => {
                        no_scale("stere without lat_ts")?;
                        lat_0
                    }
                };
                ensure!(lat_ts * lat_0 > 0.0, "projdef {s:?}: lat_ts in the other hemisphere");
                Kind::PolarStereographic { south: lat_0 < 0.0, lat_ts, lon_0: deg("lon_0", 0.0)? }
            }
            "laea" => {
                no_scale("laea")?;
                Kind::LambertAzimuthal { lat_0: deg("lat_0", 0.0)?, lon_0: deg("lon_0", 0.0)? }
            }
            "tmerc" => Kind::TransverseMercator {
                lat_0: deg("lat_0", 0.0)?,
                lon_0: deg("lon_0", 0.0)?,
                k_0: scale.unwrap_or(1.0),
            },
            "merc" => {
                no_scale("merc")?;
                Kind::Mercator { lat_ts: deg("lat_ts", 0.0)?, lon_0: deg("lon_0", 0.0)? }
            }
            "lcc" => {
                no_scale("lcc")?;
                ensure!(earth.is_sphere(), "projdef {s:?}: lcc is supported on a sphere only");
                let lat_1 = num("lat_1")?.ok_or_else(|| anyhow!("projdef {s:?}: lcc needs lat_1"))?;
                // PROJ's lcc: a tangent cone (no lat_2) is centred on its
                // standard parallel unless lat_0 says otherwise; a secant
                // one defaults lat_0 to the equator.
                let lat_2 = num("lat_2")?;
                Kind::LambertConformal {
                    lat_1,
                    lat_2: lat_2.unwrap_or(lat_1),
                    lat_0: deg("lat_0", if lat_2.is_some() { 0.0 } else { lat_1 })?,
                    lon_0: deg("lon_0", 0.0)?,
                }
            }
            "geos" => Kind::Geostationary {
                h: num("h")?.ok_or_else(|| anyhow!("projdef {s:?}: geos needs h"))?,
                lon_0: deg("lon_0", 0.0)?,
                sweep: match get("sweep") {
                    None | Some(Some("y")) => Sweep::Y,
                    Some(Some("x")) => Sweep::X,
                    Some(v) => bail!("projdef {s:?}: sweep {v:?}"),
                },
            },
            other => bail!("projdef {s:?}: projection {other} not supported"),
        };
        if kind == Kind::LatLon {
            ensure!(get("units").is_none() && x_0 == 0.0 && y_0 == 0.0, "projdef {s:?}: offsets on longlat");
        }
        Ok(ProjDef { kind, earth, x_0, y_0 })
    }

    /// The grid whose sample `(0, 0)` is at PROJ coordinates `(x, y)` (false
    /// easting and northing included), with `dx` between columns and `dy`
    /// between rows (negative when rows run north to south): metres, or
    /// degrees for `longlat`.
    pub fn grid(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<GridProjection> {
        ensure!(
            x.is_finite() && y.is_finite() && dx.is_finite() && dy.is_finite() && dx != 0.0 && dy != 0.0,
            "grid origin ({x}, {y}) or spacing ({dx}, {dy}) unusable"
        );
        ensure!(self.earth.is_plausible(), "implausible earth {:?}", self.earth);
        let (x0, y0) = (x - self.x_0, y - self.y_0);
        let e = self.earth;
        Ok(match self.kind {
            Kind::LatLon => {
                ensure!(dx > 0.0, "longitude spacing {dx} must be positive");
                GridProjection::LatLon(LatLonGrid { lon0: x0, lat0: y0, dlon: dx, dlat: dy, periodic: false })
            }
            Kind::PolarStereographic { south, lat_ts, lon_0 } => GridProjection::PolarStereographic(
                PolarStereoGrid::from_projected(e, south, lat_ts, lon_0, x0, y0, dx, dy),
            ),
            Kind::LambertAzimuthal { lat_0, lon_0 } => {
                GridProjection::LambertAzimuthal(LaeaGrid::from_projected(e, lat_0, lon_0, x0, y0, dx, dy))
            }
            Kind::TransverseMercator { lat_0, lon_0, k_0 } => GridProjection::TransverseMercator(
                TransverseMercatorGrid::from_projected(e, lat_0, lon_0, k_0, x0, y0, dx, dy),
            ),
            Kind::Mercator { lat_ts, lon_0 } => {
                GridProjection::Mercator(MercatorGrid::from_projected(e, lat_ts, lon_0, x0, y0, dx, dy))
            }
            Kind::LambertConformal { lat_1, lat_2, lat_0, lon_0 } => {
                GridProjection::Lambert(LambertGrid::from_projected(lat_1, lat_2, lon_0, lat_0, x0, y0, dx, dy, e.a))
            }
            Kind::Geostationary { h, lon_0, sweep } => GridProjection::Geostationary(GeostationaryGrid::new(
                e,
                h,
                lon_0,
                sweep,
                dx / h,
                x0 / h,
                dy / h,
                y0 / h,
            )),
        })
    }

    /// PROJ coordinates (false easting and northing included) of a point.
    pub fn forward(&self, lon: f64, lat: f64) -> Result<(f64, f64)> {
        let probe = self.grid(self.x_0, self.y_0, 1.0, 1.0)?;
        let (x, y) = probe.lonlat_to_ij(lon, lat);
        ensure!(x.is_finite() && y.is_finite(), "({lon}, {lat}) has no image in {:?}", self.kind);
        Ok((x + self.x_0, y + self.y_0))
    }

    /// The grid of `nx` by `ny` cells of `dx` by `dy` whose outer upper-left
    /// corner is the point `(lon, lat)`: ODIM's convention, in which
    /// `where/UL_lon` and `UL_lat` are the corner of the corner cell, not its
    /// centre. Rows run north to south.
    pub fn grid_from_ul_corner(&self, lon: f64, lat: f64, dx: f64, dy: f64) -> Result<GridProjection> {
        ensure!(dx > 0.0 && dy > 0.0, "cell size {dx} x {dy}");
        let (x, y) = self.forward(lon, lat)?;
        self.grid(x + 0.5 * dx, y - 0.5 * dy, dx, -dy)
    }
}

/// The `+key=value` pairs of one PROJ string.
struct Params<'a> {
    s: &'a str,
    list: Vec<(&'a str, Option<&'a str>)>,
}

impl<'a> Params<'a> {
    /// `None` when absent, `Some(None)` for a bare flag.
    fn get(&self, key: &str) -> Option<Option<&'a str>> {
        self.list.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    fn num(&self, key: &str) -> Result<Option<f64>> {
        let s = self.s;
        match self.get(key) {
            None => Ok(None),
            Some(None) => bail!("projdef {s:?}: {key} has no value"),
            Some(Some(v)) => {
                let x: f64 = v.parse().map_err(|_| anyhow!("projdef {s:?}: {key}={v} is not a number"))?;
                ensure!(x.is_finite(), "projdef {s:?}: {key}={v}");
                Ok(Some(x))
            }
        }
    }
}

fn parse_earth(p: &Params) -> Result<Earth> {
    let s = p.s;
    if let Some(r) = p.num("R")? {
        return Ok(Earth::sphere(r));
    }
    if let Some(a) = p.num("a")? {
        return Ok(if let Some(b) = p.num("b")? {
            Earth { a, b }
        } else if let Some(rf) = p.num("rf")? {
            Earth::from_inverse_flattening(a, rf)
        } else if let Some(f) = p.num("f")? {
            Earth { a, b: a * (1.0 - f) }
        } else {
            Earth::sphere(a)
        });
    }
    let name = match (p.get("ellps"), p.get("datum")) {
        (Some(Some(e)), _) => e,
        (None, Some(Some("WGS84"))) => "WGS84",
        (None, Some(d)) => bail!("projdef {s:?}: datum {d:?} not supported"),
        (Some(None), _) => bail!("projdef {s:?}: ellps has no value"),
        (None, None) => return Ok(PROJ_DEFAULT_EARTH),
    };
    ellipsoid(name).ok_or_else(|| anyhow!("projdef {s:?}: ellipsoid {name} not supported"))
}

/// PROJ's ellipsoid table, the entries radar and satellite products use.
pub fn ellipsoid(name: &str) -> Option<Earth> {
    Some(match name {
        "WGS84" => Earth::WGS84,
        "GRS80" => Earth::GRS80,
        "bessel" => Earth::from_inverse_flattening(6_377_397.155, 299.152_812_8),
        "intl" => Earth::from_inverse_flattening(6_378_388.0, 297.0),
        "clrk66" => Earth { a: 6_378_206.4, b: 6_356_583.8 },
        "airy" => Earth::from_inverse_flattening(6_377_563.396, 299.324_964_6),
        "krass" => Earth::from_inverse_flattening(6_378_245.0, 298.3),
        "aust_SA" => Earth::from_inverse_flattening(6_378_160.0, 298.25),
        "sphere" => Earth::sphere(6_370_997.0),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn parses_odim_strings() {
        let d = ProjDef::parse(
            "+proj=stere +lat_0=90 +lat_ts=60 +lon_0=10 +a=6378137 +b=6356752.3142451802 +no_defs +x_0=543196.83521776402 +y_0=3622588.8619310018 +units=m",
        )
        .unwrap();
        assert_eq!(d.kind, Kind::PolarStereographic { south: false, lat_ts: 60.0, lon_0: 10.0 });
        assert!(close(d.earth.b, 6_356_752.314_245_18, 1e-6) && close(d.x_0, 543_196.835_217_764, 1e-9));
        let d =
            ProjDef::parse("+proj=laea +lat_0=55.0 +lon_0=10.0 +x_0=1950000.0 +y_0=-2100000.0 +units=m +ellps=WGS84")
                .unwrap();
        assert_eq!(d.kind, Kind::LambertAzimuthal { lat_0: 55.0, lon_0: 10.0 });
        assert_eq!(d.earth, Earth::WGS84);
        // SMHI: Bessel ellipsoid, no false origin, an all-zero datum shift.
        let d = ProjDef::parse("+proj=stere +ellps=bessel +lat_0=90 +lon_0=14 +lat_ts=60 +towgs84=0,0,0").unwrap();
        assert!(close(d.earth.a, 6_377_397.155, 1e-6) && d.x_0 == 0.0);
    }

    #[test]
    fn refuses_what_it_cannot_place() {
        for s in [
            "+proj=stere +lat_0=52 +lon_0=5 +ellps=WGS84",
            "+proj=laea +lat_0=52 +lon_0=10 +ellps=WGS84 +towgs84=1,2,3",
            "+proj=lcc +lat_1=33 +lat_2=45 +lon_0=-97 +ellps=WGS84",
            "+proj=laea +lat_0=52 +lon_0=10 +pm=paris",
            "+proj=omerc +lat_0=52",
            "+proj=laea +lat_0=abc",
            "+proj=laea +lat_0=52 +lat_0=53",
            "+lat_0=52",
        ] {
            assert!(ProjDef::parse(s).is_err(), "{s}");
        }
    }

    /// Parameters a string leaves out take the defaults of PROJ's
    /// projection operations (what `proj` and `proj_create` without
    /// `+type=crs` run): lat_0, lon_0, x_0, y_0 and lat_ts 0, except the
    /// pole for stere's lat_ts and lat_1 for a tangent lcc's lat_0 and
    /// lat_2; k_0 1; sweep y; the GRS80 ellipsoid. Expected values are
    /// pyproj pipelines of exactly these operations. (PyProj's CRS layer
    /// reads a lone lat_1 as a secant cone with lat_2 = 0, which the
    /// operation does not.)
    #[test]
    fn defaults_match_proj() {
        for (s, lon, lat, x, y) in [
            ("+proj=lcc +lat_1=33 +lat_2=45 +lon_0=-97 +R=6371229", -97.0, 33.0, 0.0, 3_980_652.851_014),
            ("+proj=lcc +lat_1=33 +lat_2=45 +lon_0=-97 +R=6371229", -80.0, 40.0, 1_431_975.434_141, 4_890_354.024_860),
            ("+proj=lcc +lat_1=25 +R=6371229", 10.0, 30.0, 965_864.817_122, 592_346.693_809),
            ("+proj=lcc +lat_1=25 +lat_0=0 +R=6371229", 10.0, 30.0, 965_864.817_122, 3_460_413.292_109),
            ("+proj=laea +ellps=WGS84", 10.0, 50.0, 789_530.149_887, 5_382_308.879_250),
            ("+proj=tmerc +ellps=WGS84", 3.0, 45.0, 236_540.642_358, 4_989_325.234_793),
            ("+proj=merc +ellps=WGS84", 10.0, 50.0, 1_113_194.907_933, 6_413_524.594_164),
            ("+proj=stere +lat_0=90 +ellps=WGS84", 10.0, 70.0, 391_737.320_934, -2_221_652.746_279),
            ("+proj=stere +lat_0=-90 +ellps=WGS84", 10.0, -70.0, 391_737.320_934, 2_221_652.746_279),
            ("+proj=stere +lat_0=90 +lat_ts=60 +ellps=WGS84", 10.0, 70.0, 365_517.978_408, -2_072_955.465_691),
            ("+proj=geos +h=35786023 +ellps=GRS80", 10.0, 20.0, 1_027_290.667_228, 2_135_974.472_787),
            ("+proj=laea +lat_0=52", 10.0, 50.0, 714_485.568_725, -173_694.032_637),
        ] {
            let (gx, gy) = ProjDef::parse(s).unwrap().forward(lon, lat).unwrap();
            assert!(close(gx, x, 1e-3) && close(gy, y, 1e-3), "{s} ({lon}, {lat}): ({gx}, {gy}), PROJ ({x}, {y})");
        }
    }

    #[test]
    fn forward_matches_pyproj() {
        // pyproj 3.8 / PROJ 9.8.
        let d =
            ProjDef::parse("+proj=laea +lat_0=55.0 +lon_0=10.0 +x_0=1950000.0 +y_0=-2100000.0 +units=m +ellps=WGS84")
                .unwrap();
        let (x, y) = d.forward(-39.535_786_412_503_4, 67.022_832_762_437_2).unwrap();
        assert!(close(x, 0.0, 1e-3) && close(y, 0.0, 1e-3), "{x} {y}");
        let (x, y) = d.forward(5.4058, 51.0692).unwrap();
        assert!(close(x, 1_628_062.819_109, 1e-3) && close(y, -2_526_948.631_873, 1e-3), "{x} {y}");
        let d = ProjDef::parse("+proj=tmerc +lat_0=42 +lon_0=12.5 +k=1 +ellps=WGS84").unwrap();
        let (x, y) = d.forward(9.0, 45.0).unwrap();
        assert!(close(x, -275_964.114_658, 1e-3) && close(y, 339_271.322_282, 1e-3), "{x} {y}");
    }

    #[test]
    fn ul_corner_grids_put_the_corner_half_a_cell_out() {
        let d =
            ProjDef::parse("+proj=laea +lat_0=55.0 +lon_0=10.0 +x_0=1950000.0 +y_0=-2100000.0 +units=m +ellps=WGS84")
                .unwrap();
        let g = d.grid_from_ul_corner(-39.535_786_412_503_4, 67.022_832_762_437_2, 1000.0, 1000.0).unwrap();
        let (lon, lat) = g.ij_to_lonlat(-0.5, -0.5);
        assert!(close(lon, -39.535_786_412_5, 1e-8) && close(lat, 67.022_832_762_4, 1e-8));
        let (lon, lat) = g.ij_to_lonlat(3799.5, 4399.5);
        assert!(close(lon, 29.421_038_635_578, 1e-8) && close(lat, 31.987_650_287_630, 1e-8), "{lon} {lat}");
    }
}
