//! GeoTIFF rasters: the first image of a TIFF, every band as `f32`, and the
//! grid its GeoKeys and tie point describe.
//!
//! # Placement
//!
//! `ModelTiepoint` ties raster point `(I, J)` to model point `(X, Y)` and
//! `ModelPixelScale` gives the cell size `(Sx, Sy)`, rows running south.
//! With `GTRasterTypeGeoKey = PixelIsArea` (the default) raster point
//! `(i, j)` is the upper-left corner of cell `(i, j)`, whose centre is
//! therefore `X + (i + 0.5 - I) Sx`, `Y - (j + 0.5 - J) Sy`; with
//! `PixelIsPoint` it is the centre itself. Rotated or sheared rasters
//! (`ModelTransformation`) are refused.
//!
//! # Projections
//!
//! Geographic rasters (`GTModelType = 2`) become lat/lon grids. Projected
//! ones are read from the user-defined keys (`ProjCoordTrans` with its
//! parameters) or from a handful of EPSG codes (3857, 3035, UTM on WGS84),
//! then go through [`ProjDef`]. Parameters a transformation does not state
//! take GeoTIFF's defaults: origins and false offsets 0, scale 1.

use std::io::Cursor;

use anyhow::{Context, Result, anyhow, bail, ensure};
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;
use wx_core::GridProjection;
use wx_core::proj::Earth;

use crate::checked_points;
use crate::projdef::{Kind, ProjDef, ellipsoid};

/// TIFF `DocumentName`, which the `tiff` crate has no name for.
const TAG_DOCUMENT_NAME: u16 = 269;

/// A decoded GeoTIFF.
#[derive(Debug, Clone)]
pub struct GeoTiff {
    pub nx: usize,
    pub ny: usize,
    /// One vector of `nx * ny` values per band, row-major from the top row.
    pub bands: Vec<Vec<f32>>,
    /// Cell centres, see the module docs.
    pub proj: GridProjection,
    /// The projection as parsed, for checks by the caller.
    pub def: ProjDef,
    /// `GDAL_NODATA`, when present.
    pub nodata: Option<f64>,
    /// TIFF `DocumentName` and `DateTime`, when present.
    pub document_name: Option<String>,
    pub date_time: Option<String>,
}

pub fn read(bytes: &[u8]) -> Result<GeoTiff> {
    let limits = {
        let mut l = Limits::default();
        l.decoding_buffer_size = crate::MAX_POINTS * 8;
        l
    };
    let mut dec = Decoder::new(Cursor::new(bytes)).context("not a TIFF file")?.with_limits(limits);
    let (w, h) = dec.dimensions().context("TIFF dimensions")?;
    let (nx, ny) = (w as usize, h as usize);
    let samples = dec.find_tag_unsigned::<u16>(Tag::SamplesPerPixel)?.unwrap_or(1) as usize;
    ensure!((1..=16).contains(&samples), "{samples} samples per pixel");
    let n = checked_points("GeoTIFF", nx, ny)?;
    ensure!(n.checked_mul(samples).is_some_and(|t| t <= crate::MAX_POINTS * 4), "{samples} bands of {nx} x {ny}");
    let planar = dec.find_tag_unsigned::<u16>(Tag::PlanarConfiguration)?.unwrap_or(1);
    ensure!(planar == 1 || samples == 1, "planar configuration {planar} with {samples} bands is not supported");

    let ascii = |dec: &mut Decoder<Cursor<&[u8]>>, tag: Tag| -> Result<Option<String>> {
        Ok(match dec.find_tag(tag)? {
            Some(v) => Some(v.into_string().map_err(|e| anyhow!("{tag:?}: {e}"))?.trim_end_matches('\0').to_string()),
            None => None,
        })
    };
    let document_name = ascii(&mut dec, Tag::Unknown(TAG_DOCUMENT_NAME))?;
    let date_time = ascii(&mut dec, Tag::DateTime)?;
    let nodata = match ascii(&mut dec, Tag::GdalNodata)? {
        Some(s) => Some(s.trim().parse::<f64>().map_err(|_| anyhow!("GDAL_NODATA {s:?} is not a number"))?),
        None => None,
    };
    let doubles = |dec: &mut Decoder<Cursor<&[u8]>>, tag: Tag| -> Result<Option<Vec<f64>>> {
        Ok(match dec.find_tag(tag)? {
            Some(v) => Some(v.into_f64_vec().map_err(|e| anyhow!("{tag:?}: {e}"))?),
            None => None,
        })
    };
    ensure!(
        dec.find_tag(Tag::ModelTransformationTag)?.is_none(),
        "rotated rasters (ModelTransformation) are not supported"
    );
    let scale =
        doubles(&mut dec, Tag::ModelPixelScaleTag)?.ok_or_else(|| anyhow!("no ModelPixelScale: not a GeoTIFF"))?;
    let tie = doubles(&mut dec, Tag::ModelTiepointTag)?.ok_or_else(|| anyhow!("no ModelTiepoint: not a GeoTIFF"))?;
    ensure!(scale.len() >= 2 && tie.len() >= 6, "ModelPixelScale {scale:?}, ModelTiepoint {tie:?}");
    ensure!(tie.len() == 6, "{} tie points: only one is supported", tie.len() / 6);
    let dir: Vec<u16> = match dec.find_tag(Tag::GeoKeyDirectoryTag)? {
        Some(v) => v.into_u16_vec().map_err(|e| anyhow!("GeoKeyDirectory: {e}"))?,
        None => bail!("no GeoKeyDirectory: not a GeoTIFF"),
    };
    let dparams = doubles(&mut dec, Tag::GeoDoubleParamsTag)?.unwrap_or_default();
    let keys = GeoKeys::parse(&dir, &dparams)?;
    let def = keys.projdef()?;
    let (sx, sy) = (scale[0], scale[1]);
    ensure!(sx > 0.0 && sy > 0.0 && sx.is_finite() && sy.is_finite(), "pixel scale {sx} x {sy}");
    let centre = if keys.pixel_is_point() { 0.0 } else { 0.5 };
    let x = tie[3] + (centre - tie[0]) * sx;
    let y = tie[4] - (centre - tie[1]) * sy;
    let proj = def.grid(x, y, sx, -sy)?;

    let data = dec.read_image().context("decoding TIFF image data")?;
    let flat: Vec<f32> = match data {
        DecodingResult::F32(v) => v,
        DecodingResult::F64(v) => v.into_iter().map(|x| x as f32).collect(),
        DecodingResult::U8(v) => v.into_iter().map(f32::from).collect(),
        DecodingResult::U16(v) => v.into_iter().map(f32::from).collect(),
        DecodingResult::I8(v) => v.into_iter().map(f32::from).collect(),
        DecodingResult::I16(v) => v.into_iter().map(f32::from).collect(),
        DecodingResult::U32(v) => v.into_iter().map(|x| x as f32).collect(),
        DecodingResult::I32(v) => v.into_iter().map(|x| x as f32).collect(),
        _ => bail!("unsupported TIFF sample type"),
    };
    ensure!(flat.len() == n * samples, "image holds {} samples, expected {}", flat.len(), n * samples);
    let bands = if samples == 1 {
        vec![flat]
    } else {
        (0..samples).map(|b| flat.iter().skip(b).step_by(samples).copied().collect()).collect()
    };
    Ok(GeoTiff { nx, ny, bands, proj, def, nodata, document_name, date_time })
}

/// The GeoKeys that place a raster.
#[derive(Debug, Default)]
struct GeoKeys {
    shorts: Vec<(u16, u16)>,
    doubles: Vec<(u16, f64)>,
}

mod key {
    pub const MODEL_TYPE: u16 = 1024;
    pub const RASTER_TYPE: u16 = 1025;
    pub const GEOGRAPHIC_TYPE: u16 = 2048;
    pub const GEODETIC_DATUM: u16 = 2050;
    pub const PRIME_MERIDIAN: u16 = 2051;
    pub const ANGULAR_UNITS: u16 = 2054;
    pub const ELLIPSOID: u16 = 2056;
    pub const SEMI_MAJOR: u16 = 2057;
    pub const SEMI_MINOR: u16 = 2058;
    pub const INV_FLATTENING: u16 = 2059;
    pub const PRIME_MERIDIAN_LONG: u16 = 2061;
    pub const PROJECTED_CS: u16 = 3072;
    pub const PROJECTION: u16 = 3074;
    pub const COORD_TRANS: u16 = 3075;
    pub const LINEAR_UNITS: u16 = 3076;
    pub const STD_PARALLEL_1: u16 = 3078;
    pub const STD_PARALLEL_2: u16 = 3079;
    pub const NAT_ORIGIN_LONG: u16 = 3080;
    pub const NAT_ORIGIN_LAT: u16 = 3081;
    pub const FALSE_EASTING: u16 = 3082;
    pub const FALSE_NORTHING: u16 = 3083;
    pub const FALSE_ORIGIN_LONG: u16 = 3084;
    pub const FALSE_ORIGIN_LAT: u16 = 3085;
    pub const FALSE_ORIGIN_EASTING: u16 = 3086;
    pub const FALSE_ORIGIN_NORTHING: u16 = 3087;
    pub const CENTER_LONG: u16 = 3088;
    pub const CENTER_LAT: u16 = 3089;
    pub const SCALE_AT_NAT_ORIGIN: u16 = 3092;
    pub const SCALE_AT_CENTER: u16 = 3093;
    pub const STRAIGHT_VERT_POLE_LONG: u16 = 3095;
}

const USER_DEFINED: u16 = 32767;

impl GeoKeys {
    fn parse(dir: &[u16], doubles: &[f64]) -> Result<GeoKeys> {
        ensure!(dir.len() >= 4 && dir[0] == 1, "GeoKeyDirectory header {:?}", &dir[..dir.len().min(4)]);
        let count = dir[3] as usize;
        ensure!(dir.len() >= 4 + 4 * count, "GeoKeyDirectory holds {} entries, says {count}", (dir.len() - 4) / 4);
        let mut keys = GeoKeys::default();
        for e in dir[4..4 + 4 * count].as_chunks::<4>().0 {
            let (id, location, n, value) = (e[0], e[1], e[2] as usize, e[3] as usize);
            match location {
                0 => keys.shorts.push((id, e[3])),
                34736 => {
                    ensure!(n >= 1 && value + n <= doubles.len(), "GeoKey {id}: double {value} of {}", doubles.len());
                    keys.doubles.push((id, doubles[value]));
                }
                // ASCII citations and anything else describe, they do not place.
                _ => {}
            }
        }
        Ok(keys)
    }

    fn short(&self, id: u16) -> Option<u16> {
        self.shorts.iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
    }

    fn double(&self, id: u16) -> Option<f64> {
        self.doubles.iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
    }

    fn pixel_is_point(&self) -> bool {
        self.short(key::RASTER_TYPE) == Some(2)
    }

    fn earth(&self) -> Result<Earth> {
        if let Some(pm) = self.double(key::PRIME_MERIDIAN_LONG) {
            ensure!(pm == 0.0, "prime meridian at {pm}");
        }
        if let Some(pm) = self.short(key::PRIME_MERIDIAN) {
            ensure!(pm == 8901 || pm == USER_DEFINED, "prime meridian code {pm}");
        }
        if let Some(u) = self.short(key::ANGULAR_UNITS) {
            ensure!(u == 9102, "angular unit code {u}: only degrees");
        }
        match self.short(key::GEOGRAPHIC_TYPE) {
            Some(4326) => return Ok(Earth::WGS84),
            Some(4258) | Some(4269) | Some(4283) => return Ok(Earth::GRS80),
            Some(USER_DEFINED) | None => {}
            Some(other) => bail!("geographic CRS EPSG:{other} not supported"),
        }
        if let Some(d) = self.short(key::GEODETIC_DATUM) {
            ensure!(d == 6326 || d == USER_DEFINED, "datum code {d} not supported");
        }
        match self.short(key::ELLIPSOID) {
            Some(7030) => return Ok(Earth::WGS84),
            Some(7019) => return Ok(Earth::GRS80),
            Some(7004) => return Ok(ellipsoid("bessel").expect("table")),
            Some(7022) => return Ok(ellipsoid("intl").expect("table")),
            Some(USER_DEFINED) | None => {}
            Some(other) => bail!("ellipsoid code {other} not supported"),
        }
        let a = self.double(key::SEMI_MAJOR).ok_or_else(|| anyhow!("no ellipsoid in the GeoKeys"))?;
        Ok(match (self.double(key::SEMI_MINOR), self.double(key::INV_FLATTENING)) {
            (Some(b), _) => Earth { a, b },
            (None, Some(rf)) if rf > 0.0 => Earth::from_inverse_flattening(a, rf),
            _ => Earth::sphere(a),
        })
    }

    fn projdef(&self) -> Result<ProjDef> {
        match self.short(key::MODEL_TYPE) {
            Some(2) => return Ok(ProjDef { kind: Kind::LatLon, earth: self.earth()?, x_0: 0.0, y_0: 0.0 }),
            Some(1) => {}
            other => bail!("GTModelType {other:?}: only projected (1) and geographic (2) rasters"),
        }
        if let Some(u) = self.short(key::LINEAR_UNITS) {
            ensure!(u == 9001, "linear unit code {u}: only metres");
        }
        match self.short(key::PROJECTED_CS) {
            Some(USER_DEFINED) | None => {}
            Some(code) => return epsg(code),
        }
        if let Some(p) = self.short(key::PROJECTION) {
            ensure!(p == USER_DEFINED, "projection code {p} not supported");
        }
        let d = |id: u16| self.double(id).unwrap_or(0.0);
        let first = |ids: &[u16]| ids.iter().find_map(|&id| self.double(id)).unwrap_or(0.0);
        let earth = self.earth()?;
        let trans = self.short(key::COORD_TRANS).ok_or_else(|| anyhow!("no ProjCoordTrans in the GeoKeys"))?;
        let kind = match trans {
            1 => Kind::TransverseMercator {
                lat_0: d(key::NAT_ORIGIN_LAT),
                lon_0: d(key::NAT_ORIGIN_LONG),
                k_0: [key::SCALE_AT_NAT_ORIGIN, key::SCALE_AT_CENTER]
                    .iter()
                    .find_map(|&id| self.double(id))
                    .unwrap_or(1.0),
            },
            7 => {
                ensure!(self.double(key::SCALE_AT_NAT_ORIGIN).is_none_or(|k| k == 1.0), "Mercator with a scale factor");
                Kind::Mercator { lat_ts: d(key::STD_PARALLEL_1), lon_0: d(key::NAT_ORIGIN_LONG) }
            }
            8 => {
                ensure!(earth.is_sphere(), "Lambert conformal conic on an ellipsoid is not supported");
                Kind::LambertConformal {
                    lat_1: d(key::STD_PARALLEL_1),
                    lat_2: d(key::STD_PARALLEL_2),
                    lat_0: first(&[key::FALSE_ORIGIN_LAT, key::NAT_ORIGIN_LAT]),
                    lon_0: first(&[key::FALSE_ORIGIN_LONG, key::NAT_ORIGIN_LONG]),
                }
            }
            10 => Kind::LambertAzimuthal {
                lat_0: first(&[key::CENTER_LAT, key::NAT_ORIGIN_LAT]),
                lon_0: first(&[key::CENTER_LONG, key::NAT_ORIGIN_LONG]),
            },
            15 => {
                let lat = first(&[key::NAT_ORIGIN_LAT]);
                ensure!(lat.abs() > 0.0, "polar stereographic without a latitude of true scale");
                ensure!(
                    self.double(key::SCALE_AT_NAT_ORIGIN).is_none_or(|k| k == 1.0),
                    "polar stereographic with a scale factor"
                );
                Kind::PolarStereographic {
                    south: lat < 0.0,
                    lat_ts: lat,
                    lon_0: first(&[key::STRAIGHT_VERT_POLE_LONG, key::NAT_ORIGIN_LONG]),
                }
            }
            other => bail!("ProjCoordTrans {other} not supported"),
        };
        let (x_0, y_0) = if trans == 8 {
            (
                first(&[key::FALSE_ORIGIN_EASTING, key::FALSE_EASTING]),
                first(&[key::FALSE_ORIGIN_NORTHING, key::FALSE_NORTHING]),
            )
        } else {
            (d(key::FALSE_EASTING), d(key::FALSE_NORTHING))
        };
        Ok(ProjDef { kind, earth, x_0, y_0 })
    }
}

/// The projected CRSs given by EPSG code that radar and satellite products use.
fn epsg(code: u16) -> Result<ProjDef> {
    let parse = |s: &str| ProjDef::parse(s);
    match code {
        3857 => parse("+proj=merc +lon_0=0 +lat_ts=0 +R=6378137"),
        3035 => parse("+proj=laea +lat_0=52 +lon_0=10 +x_0=4321000 +y_0=3210000 +ellps=GRS80"),
        32601..=32660 => parse(&format!(
            "+proj=tmerc +lat_0=0 +lon_0={} +k_0=0.9996 +x_0=500000 +ellps=WGS84",
            -183 + 6 * (code - 32600) as i32
        )),
        32701..=32760 => parse(&format!(
            "+proj=tmerc +lat_0=0 +lon_0={} +k_0=0.9996 +x_0=500000 +y_0=10000000 +ellps=WGS84",
            -183 + 6 * (code - 32700) as i32
        )),
        other => bail!("projected CRS EPSG:{other} not supported"),
    }
}
