//! ODIM_H5 radar composites (`what/object = COMP`), the EUMETNET exchange
//! format OPERA, DWD, SMHI and most European services publish.
//!
//! # Values
//!
//! Each `datasetN/dataM` holds one quantity as stored integers or floats
//! with `what/gain`, `offset`, `nodata` and `undetect`. Physical value is
//! `raw * gain + offset`. `nodata` marks cells no radar covers and becomes
//! `NaN`; `undetect` marks cells a radar covered without detecting anything
//! and becomes a real value: [`NO_ECHO_DBZ`] for reflectivity, 0 for rates
//! and accumulations.
//!
//! # Grid
//!
//! `where/projdef` is a PROJ string; `xsize` by `ysize` cells of `xscale` by
//! `yscale` metres, rows north to south. `where/UL_lon` and `UL_lat` are the
//! outer corner of the upper-left cell (ODIM_H5: "upper left corner of upper
//! left pixel"): the grid is placed from that corner, so a `projdef` without
//! `x_0`/`y_0` (SMHI) places the same as one with them (DWD, OPERA). The
//! other three corners must agree to within a tenth of a cell.
//!
//! # Times
//!
//! `what/date` + `what/time` is the nominal time. `dataset1/what/start*` and
//! `end*` bound the data; for an accumulation they are its window. A file
//! flagged `how/simulated = True` is a nowcast step: its valid time is
//! `enddate`/`endtime` and the nominal time is the observation it starts from.

use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::{DateTime, NaiveDate, NaiveTime, TimeDelta, Utc};
use wx_core::Field;

use crate::hdf::H5;
use crate::projdef::ProjDef;
use crate::{Decoded, MAX_RAIN_RATE_MMH, NO_ECHO_DBZ, Quantity, checked_points, clamp_rate};

/// One quantity of a composite, decoded, with the metadata products check.
#[derive(Debug, Clone)]
pub struct Composite {
    pub decoded: Decoded,
    /// ODIM quantity name: `DBZH`, `RATE`, `ACRR`, ...
    pub quantity: String,
    /// `what/source`, such as `ORG:247` (OPERA) or `ORG:78,CTY:616,...` (DWD).
    pub source: String,
    /// `dataset1/what/product` (`MAX`, `PPI`, `COMP`) and `prodname`.
    pub product: String,
    pub prodname: String,
    /// `where/projdef` as stored.
    pub projdef: String,
    /// `dataset1/what` start and end of the data.
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Share of cells that are `nodata`, `undetect` and anything else.
    pub coverage: Coverage,
}

/// How the cells of a composite divide up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coverage {
    pub nodata: f64,
    pub undetect: f64,
    pub detected: f64,
}

/// The [`Quantity`] an ODIM quantity name stands for.
fn quantity_of(name: &str) -> Option<Quantity> {
    Some(match name {
        "DBZH" | "DBZV" | "TH" | "TV" => Quantity::Reflectivity,
        "RATE" => Quantity::RainRate,
        "ACRR" => Quantity::Accumulation,
        _ => return None,
    })
}

/// Decode the first quantity of `dataset1` that is one of `wanted` (any
/// supported one when `wanted` is empty).
pub fn decode(bytes: &[u8], wanted: &[&str]) -> Result<Composite> {
    let h5 = H5::open(bytes)?;
    let conventions = h5.attr_str("/", "Conventions").context("not ODIM_H5")?;
    ensure!(conventions.starts_with("ODIM_H5"), "Conventions is {conventions:?}, not ODIM_H5");
    let object = h5.attr_str("/what", "object")?;
    ensure!(object == "COMP", "what/object is {object:?}: only composites (COMP) are supported");

    // The data group holding the quantity.
    let mut found = None;
    for m in 1..=64 {
        let group = format!("/dataset1/data{m}");
        if !h5.has_group(&group) {
            break;
        }
        let name = h5.attr_str(&format!("{group}/what"), "quantity")?;
        let accept = if wanted.is_empty() { quantity_of(&name).is_some() } else { wanted.contains(&name.as_str()) };
        if accept {
            found = Some((group, name));
            break;
        }
    }
    let (group, name) = found.ok_or_else(|| anyhow!("dataset1 holds none of the quantities {wanted:?}"))?;
    let quantity = quantity_of(&name).ok_or_else(|| anyhow!("quantity {name} is not supported"))?;
    let what = format!("{group}/what");
    let gain = h5.attr_f64_opt(&what, "gain")?.unwrap_or(1.0);
    let offset = h5.attr_f64_opt(&what, "offset")?.unwrap_or(0.0);
    let nodata = h5.attr_f64_opt(&what, "nodata")?;
    let undetect = h5.attr_f64_opt(&what, "undetect")?;
    ensure!(gain != 0.0, "{what}: gain is 0");

    // Times.
    let nominal = odim_time(&h5, "/what", "date", "time")?;
    let start = odim_time(&h5, "/dataset1/what", "startdate", "starttime")?;
    let end = odim_time(&h5, "/dataset1/what", "enddate", "endtime")?;
    ensure!(end >= start, "dataset1 ends ({end}) before it starts ({start})");
    let simulated = match h5.attr_str_opt("/how", "simulated")?.as_deref() {
        None | Some("False") | Some("false") => false,
        Some("True") | Some("true") => true,
        Some(other) => bail!("how/simulated is {other:?}"),
    };
    let period = if quantity == Quantity::Accumulation { end - start } else { TimeDelta::zero() };
    if quantity == Quantity::Accumulation {
        ensure!(period > TimeDelta::zero(), "accumulation without a period ({start} to {end})");
    }
    let (valid, run) = if simulated {
        ensure!(end >= nominal, "nowcast step valid {end} before its observation {nominal}");
        (end, Some(nominal))
    } else {
        (nominal, None)
    };

    // Grid.
    let projdef = h5.attr_str("/where", "projdef")?;
    let nx = size_attr(&h5, "xsize")?;
    let ny = size_attr(&h5, "ysize")?;
    checked_points("ODIM composite", nx, ny)?;
    let xscale = h5.attr_f64("/where", "xscale")?;
    let yscale = h5.attr_f64("/where", "yscale")?;
    ensure!(xscale > 0.0 && yscale > 0.0, "cell size {xscale} x {yscale}");
    let def = ProjDef::parse(&projdef)?;
    let corner = |c: &str| -> Result<(f64, f64)> {
        Ok((h5.attr_f64("/where", &format!("{c}_lon"))?, h5.attr_f64("/where", &format!("{c}_lat"))?))
    };
    let (ul_lon, ul_lat) = corner("UL")?;
    let proj = def.grid_from_ul_corner(ul_lon, ul_lat, xscale, yscale)?;
    for (c, i, j) in
        [("UR", nx as f64 - 0.5, -0.5), ("LL", -0.5, ny as f64 - 0.5), ("LR", nx as f64 - 0.5, ny as f64 - 0.5)]
    {
        let (lon, lat) = corner(c)?;
        let (gi, gj) = proj.lonlat_to_ij(lon, lat);
        ensure!(
            (gi - i).abs() < 0.1 && (gj - j).abs() < 0.1,
            "where/{c}_lon,{c}_lat ({lon}, {lat}) is cell ({gi:.2}, {gj:.2}) of the grid, expected ({i}, {j})"
        );
    }

    // Values.
    let (raw, shape) = h5.read(&format!("{group}/data"))?;
    ensure!(shape == [ny, nx], "{group}/data has shape {shape:?}, where says {ny} x {nx}");
    let ceiling = match quantity {
        Quantity::RainRate => MAX_RAIN_RATE_MMH,
        Quantity::Accumulation => MAX_RAIN_RATE_MMH * (period.num_seconds() as f32 / 3600.0),
        _ => f32::INFINITY,
    };
    let no_echo = match quantity {
        Quantity::Reflectivity => NO_ECHO_DBZ,
        _ => 0.0,
    };
    let values = raw.map(|x| {
        if Some(x) == nodata {
            f32::NAN
        } else if Some(x) == undetect {
            no_echo
        } else {
            let v = (x * gain + offset) as f32;
            match quantity {
                Quantity::RainRate => clamp_rate(v),
                Quantity::Accumulation => v.clamp(0.0, ceiling),
                _ => v,
            }
        }
    });
    // Counted on the raw values, so the classes are exact.
    let n = values.len() as f64;
    let nodata_share = nodata.map_or(0.0, |nd| raw.count(|x| x == nd) as f64 / n);
    let undetect_share = undetect.map_or(0.0, |ud| raw.count(|x| x == ud) as f64 / n);

    let field = Field { nx, ny, proj, values, grid_relative_winds: false };
    let decoded = Decoded { field, quantity, valid, run, lead: run.map_or(TimeDelta::zero(), |r| valid - r), period };
    Ok(Composite {
        decoded,
        quantity: name,
        source: h5.attr_str_opt("/what", "source")?.unwrap_or_default(),
        product: h5.attr_str_opt("/dataset1/what", "product")?.unwrap_or_default(),
        prodname: h5.attr_str_opt("/dataset1/what", "prodname")?.unwrap_or_default(),
        projdef,
        start,
        end,
        coverage: Coverage {
            nodata: nodata_share,
            undetect: undetect_share,
            detected: 1.0 - nodata_share - undetect_share,
        },
    })
}

fn size_attr(h5: &H5, name: &str) -> Result<usize> {
    let v = h5.attr_f64("/where", name)?;
    ensure!(v >= 1.0 && v.fract() == 0.0 && v <= 1e8, "where/{name} is {v}");
    Ok(v as usize)
}

/// An ODIM date (`YYYYMMDD`) and time (`HHMMSS`) attribute pair, UTC.
fn odim_time(h5: &H5, path: &str, date: &str, time: &str) -> Result<DateTime<Utc>> {
    let d = h5.attr_str(path, date)?;
    let t = h5.attr_str(path, time)?;
    let day = NaiveDate::parse_from_str(&d, "%Y%m%d").map_err(|_| anyhow!("{path}/{date} is {d:?}, not YYYYMMDD"))?;
    let tod = NaiveTime::parse_from_str(&t, "%H%M%S").map_err(|_| anyhow!("{path}/{time} is {t:?}, not HHMMSS"))?;
    Ok(day.and_time(tod).and_utc())
}
