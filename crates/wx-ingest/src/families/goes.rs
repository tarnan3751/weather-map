//! GOES-19 (East) and GOES-18 (West): ABI band 13 full disks every ten
//! minutes, the satellite layer over the Americas and the Pacific. Everything
//! about the product (buckets, layout, crops) is in the registry module
//! `wx_core::families::goes`.
//!
//! `SourceDef::region` is the bucket (`noaa-goes19`, `noaa-goes18`); the
//! satellite's tag in file names, its longitude and its crop come from the
//! small table here. An hour prefix holds every band of every scan with an
//! unpredictable creation stamp in the name, so the listing is of the hour
//! and the band-13 files are picked out of it; the valid time is the scan
//! start in the name.
//!
//! Each file is cropped before it is built: cells beyond the registry's
//! viewing limit, and for the satellite on top the cells past the split
//! meridian, become missing, and the grid shrinks to the rectangle that is
//! left. The cropped disk is then averaged onto a grid [`FACTOR`] times
//! coarser, because a full-resolution disk is heavier than the layer needs
//! (see [`FACTOR`]).
//!
//! The grid's bounds are those of the cropped rectangle, which reaches the
//! limb at its corners (and, for GOES-18, east of the split at high
//! latitudes): wider than the data, which only costs a tile request that
//! finds nothing there. The solid box is the data's.

use crate::clock;
use crate::driver::{Cadence, Cx, Decoded, Fetched, Need, ObsFile, Observed, Payload};
use crate::families::common;
use crate::remote::{FetchError, S3Bucket};
use anyhow::{Context, ensure};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use std::sync::Arc;
use wx_core::families::goes as reg;
use wx_core::proj::{GeostationaryGrid, GridProjection, wrap_lon};
use wx_core::{Field, SourceDef};

/// A band-13 file is about 25 MB; anything near this is not one.
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// Cells averaged along each axis of the stored grid: 2, so 4 km at the
/// sub-satellite point. Measured on the 2026-11-06 18:10 disks: cropped at
/// 2 km, GOES-19 keeps 5314 x 5296 cells, 77 MB in memory with overviews
/// and coverage and 26 MB stored (GOES-18 61 and 21 MB), so the two newest
/// parts alone would take more than half of a 256 MB grid cache and the
/// three-hour loop 0.9 GB of storage; building one peaks at about 470 MB.
/// At 4 km a part is 19 MB in memory and 7.5 MB stored, building peaks at
/// about 350 MB (the decoder's full-disk arrays), and it is still twice as
/// fine as GMGSI's 8 km below it. The cells are larger than a zoom-6 tile's
/// samples (2.4 km at the equator).
pub const FACTOR: usize = 2;
/// The decoder gives the middle of the scan; it must lie this close after
/// the scan start the file is named after.
const SCAN_SLACK: Duration = Duration::minutes(15);

/// What differs by satellite: the bucket, the tag in file names, the
/// longitude, and which side of the split it keeps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Satellite {
    pub bucket: &'static str,
    pub tag: &'static str,
    pub sub_lon: f32,
    /// Keep only cells west of this meridian (the satellite ranked on top).
    pub west_of: Option<f32>,
}

pub const SATELLITES: &[Satellite] = &[
    Satellite {
        bucket: "noaa-goes19",
        tag: "G19",
        sub_lon: reg::GOES19_LON,
        west_of: None,
    },
    Satellite {
        bucket: "noaa-goes18",
        tag: "G18",
        sub_lon: reg::GOES18_LON,
        west_of: Some(reg::SPLIT_LON),
    },
];

pub fn satellite(region: &str) -> Option<&'static Satellite> {
    SATELLITES.iter().find(|s| s.bucket == region)
}

/// `ABI-L2-CMIPF/C13` as the product directory and the band.
pub fn product_parts(product: &str) -> Option<(&str, &str)> {
    let (dir, band) = product.split_once('/')?;
    let ok = !dir.is_empty()
        && band.len() == 3
        && band.starts_with('C')
        && band[1..].bytes().all(|b| b.is_ascii_digit());
    ok.then_some((dir, band))
}

/// The prefix every file of `dir` from the hour starting at `hour` sorts
/// under, whatever its scan mode and band.
pub fn hour_prefix(dir: &str, hour: DateTime<Utc>) -> String {
    format!(
        "{dir}/{}/{:03}/{:02}/OR_{dir}-M",
        hour.year(),
        hour.ordinal(),
        hour.hour()
    )
}

/// Scan start of a file of `dir`, `band` and satellite `tag` from its key:
/// `.../OR_{dir}-M{mode}{band}_{tag}_s{YYYYDDDHHMMSS}{tenth}_e..._c....nc`.
pub fn parse_key(dir: &str, band: &str, tag: &str, key: &str) -> Option<DateTime<Utc>> {
    let name = key.rsplit('/').next()?;
    let rest = name
        .strip_prefix("OR_")?
        .strip_prefix(dir)?
        .strip_prefix("-M")?;
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest
        .strip_prefix(band)?
        .strip_prefix('_')?
        .strip_prefix(tag)?
        .strip_prefix("_s")?;
    let digits = rest.split('_').next()?;
    if !name.ends_with(".nc") || digits.len() != 14 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let num = |a: usize, b: usize| rest[a..b].parse::<u32>().ok();
    let day = NaiveDate::from_yo_opt(rest[..4].parse().ok()?, num(4, 7)?)?;
    let t = day.and_hms_milli_opt(num(7, 9)?, num(9, 11)?, num(11, 13)?, num(13, 14)? * 100)?;
    Some(t.and_utc())
}

/// Which cells of a full disk a satellite keeps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crop {
    pub sub_lon: f64,
    /// Cells further than this from the sub-satellite point, in degrees of
    /// great circle, are dropped.
    pub max_view_deg: f64,
    pub west_of: Option<f64>,
}

impl Crop {
    pub fn of(sat: &Satellite) -> Self {
        Crop {
            sub_lon: f64::from(sat.sub_lon),
            max_view_deg: f64::from(reg::MAX_VIEW_DEG),
            west_of: sat.west_of.map(f64::from),
        }
    }

    /// Within the viewing limit (a point off the disk is not).
    pub fn in_view(&self, lon: f64, lat: f64) -> bool {
        lon.is_finite()
            && lat.is_finite()
            && lat.to_radians().cos() * wrap_lon(lon - self.sub_lon).to_radians().cos()
                >= self.max_view_deg.to_radians().cos()
    }

    /// On this satellite's side of the split.
    pub fn on_side(&self, lon: f64) -> bool {
        self.west_of.is_none_or(|w| wrap_lon(lon - w) < 0.0)
    }

    #[cfg(test)]
    pub fn keeps(&self, lon: f64, lat: f64) -> bool {
        self.in_view(lon, lat) && self.on_side(lon)
    }
}

/// The last index in `[lo, hi]` where `p` holds, for a `p` that holds up to
/// some index and not after it, given that it holds at `lo`.
fn last_true(mut lo: usize, mut hi: usize, p: impl Fn(usize) -> bool) -> usize {
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if p(mid) { lo = mid } else { hi = mid - 1 }
    }
    lo
}

/// The first index in `[lo, hi]` where `p` holds, for a `p` that holds from
/// some index on, given that it holds at `hi`.
fn first_true(mut lo: usize, mut hi: usize, p: impl Fn(usize) -> bool) -> usize {
    while lo < hi {
        let mid = (lo + hi) / 2;
        if p(mid) { hi = mid } else { lo = mid + 1 }
    }
    lo
}

/// The columns each row keeps, `None` for a row that keeps nothing.
///
/// Along a row of a geostationary grid the distance from the sub-satellite
/// point falls towards the column below the satellite and rises past it, so
/// the cells in view are one run around the grid column nearest to it (none
/// when that one is out of view); longitudes increase eastward, so the cells
/// on a side of a meridian are a run from one end. Each end is found by
/// bisection, a few dozen positions per row.
fn runs(g: &GeostationaryGrid, nx: usize, ny: usize, crop: &Crop) -> Vec<Option<(usize, usize)>> {
    if nx == 0 {
        return vec![None; ny];
    }
    let below = (-g.x_offset / g.x_scale)
        .round()
        .clamp(0.0, (nx - 1) as f64) as usize;
    (0..ny)
        .map(|j| {
            let at = |i: usize| {
                g.inverse(
                    g.x_offset + i as f64 * g.x_scale,
                    g.y_offset + j as f64 * g.y_scale,
                )
            };
            let in_view = |i: usize| {
                let (lon, lat) = at(i);
                crop.in_view(lon, lat)
            };
            if !in_view(below) {
                return None;
            }
            let first = first_true(0, below, in_view);
            let last = last_true(below, nx - 1, in_view);
            if crop.west_of.is_none() {
                return Some((first, last));
            }
            let on_side = |i: usize| crop.on_side(at(i).0);
            if !on_side(first) {
                return None;
            }
            Some((first, last_true(first, last, on_side)))
        })
        .collect()
}

/// `field`, a full disk (or part of one) on its fixed grid, with the cells
/// `crop` drops made missing, cut to the rectangle that keeps anything, and
/// averaged over blocks of `factor` by `factor` cells (missing cells left
/// out; a block with none is missing).
pub fn crop(field: Field, crop: &Crop, factor: usize) -> anyhow::Result<Field> {
    let GridProjection::Geostationary(g) = &field.proj else {
        anyhow::bail!("not a geostationary grid");
    };
    ensure!(factor >= 1, "a factor of {factor}");
    let (nx, ny) = (field.nx, field.ny);
    let runs = runs(g, nx, ny, crop);
    let j0 = runs
        .iter()
        .position(Option::is_some)
        .context("the crop keeps nothing of this grid")?;
    let j1 = runs.iter().rposition(Option::is_some).unwrap_or(j0);
    let i0 = runs.iter().flatten().map(|r| r.0).min().unwrap_or(0);
    let i1 = runs.iter().flatten().map(|r| r.1).max().unwrap_or(0);
    let (w, h) = (
        (i1 - i0 + 1).div_ceil(factor),
        (j1 - j0 + 1).div_ceil(factor),
    );
    let mut sum = vec![0f32; w * h];
    let mut n = vec![0u16; w * h];
    for (j, run) in runs.iter().enumerate().take(j1 + 1).skip(j0) {
        let Some((a, b)) = *run else { continue };
        let row = &field.values[j * nx..(j + 1) * nx];
        let out = (j - j0) / factor * w;
        for (i, &v) in row.iter().enumerate().take(b + 1).skip(a) {
            if !v.is_nan() {
                let k = out + (i - i0) / factor;
                sum[k] += v;
                n[k] += 1;
            }
        }
    }
    let values = sum
        .into_iter()
        .zip(n)
        .map(|(s, n)| if n == 0 { f32::NAN } else { s / f32::from(n) })
        .collect();
    // A block's centre is the mean of its cells' centres.
    let half = (factor as f64 - 1.0) / 2.0;
    let grid = GeostationaryGrid::new(
        g.earth,
        g.height,
        g.sub_lon,
        g.sweep,
        g.x_scale * factor as f64,
        g.x_offset + (i0 as f64 + half) * g.x_scale,
        g.y_scale * factor as f64,
        g.y_offset + (j0 as f64 + half) * g.y_scale,
    );
    Ok(Field {
        nx: w,
        ny: h,
        proj: GridProjection::Geostationary(grid),
        values,
        grid_relative_winds: false,
    })
}

#[derive(Default)]
pub struct Goes;

fn satellite_of(source: &SourceDef) -> Result<&'static Satellite, FetchError> {
    common::region("GOES", source, satellite(source.region))
}

fn product_of(product: &str) -> Result<(&str, &str), FetchError> {
    product_parts(product).ok_or_else(|| {
        FetchError::object(
            product,
            "not a product directory and band such as ABI-L2-CMIPF/C13",
        )
    })
}

/// The decoder for one file: `wx_formats::goes`, the crop, the coarser grid.
/// The valid time stays the scan start the file is named after.
fn decoder(sat: Satellite, scan_start: DateTime<Utc>) -> crate::driver::Decoder {
    Arc::new(move |bytes: &[u8], _: &Need| {
        let d = wx_formats::goes::decode(bytes)?;
        ensure!(
            d.valid >= scan_start && d.valid - scan_start <= SCAN_SLACK,
            "the scan is centred at {} but the file is named after {scan_start}",
            d.valid
        );
        if let GridProjection::Geostationary(g) = &d.field.proj {
            ensure!(
                (wrap_lon(g.sub_lon - f64::from(sat.sub_lon))).abs() < 1.0,
                "the file looks down from {} but {} is at {}",
                g.sub_lon,
                sat.tag,
                sat.sub_lon
            );
        }
        let field = crop(d.field, &Crop::of(&sat), FACTOR)?;
        Ok(Decoded {
            field,
            valid: Some(scan_start),
        })
    })
}

#[async_trait::async_trait]
impl Observed for Goes {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        satellite_of(source)?;
        Ok(())
    }

    fn cadence(&self, _: &SourceDef, _: &str) -> Cadence {
        // Written about ten minutes after the scan starts (S3 LastModified,
        // 2026-11-06), when the scan ends.
        let latency = Duration::seconds(reg::LATENCY_MINUTES as i64 * 60 + 30);
        Cadence::every(
            Duration::minutes(reg::EVERY_MINUTES as i64),
            latency,
            Duration::seconds(30),
        )
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        product: &str,
        from: DateTime<Utc>,
    ) -> Result<Vec<ObsFile>, FetchError> {
        let sat = satellite_of(source)?;
        let (dir, band) = product_of(product)?;
        let bucket = S3Bucket::aws(sat.bucket);
        let mut out = Vec::new();
        let mut hour = clock::floor(from, Duration::hours(1));
        let latency = self.cadence(source, product).latency;
        // At most a day of hour prefixes, whatever `from` is.
        for _ in 0..24 {
            if hour > cx.now {
                break;
            }
            let prefix = hour_prefix(dir, hour);
            let listed = cx.list_s3(&bucket, &prefix, None).await?;
            let before = out.len();
            for l in &listed {
                if let Some(valid) = parse_key(dir, band, sat.tag, &l.key) {
                    out.push(ObsFile {
                        valid,
                        name: l.key.clone(),
                        size: Some(l.size),
                    });
                }
            }
            // A whole hour of other files and none of ours: the names have
            // changed, and the task would otherwise look healthy while it
            // finds nothing. (An hour with no files at all is an outage.)
            if out.len() == before
                && !listed.is_empty()
                && hour + Duration::hours(1) + latency <= cx.now
            {
                return Err(FetchError::object(
                    format!("{}/{prefix}", bucket.base()),
                    format!(
                        "{} files but no {band} file of {}: has the naming changed?",
                        listed.len(),
                        sat.tag
                    ),
                ));
            }
            hour += Duration::hours(1);
        }
        Ok(out)
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        _: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let sat = *satellite_of(source)?;
        let url = S3Bucket::aws(sat.bucket).url(&file.name);
        cx.whole(
            &url,
            needs,
            MAX_FILE,
            Payload::decoded_by(decoder(sat, file.valid)),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use chrono::TimeZone;

    const PRODUCT: &str = "ABI-L2-CMIPF/C13";
    const GULF: &str = "formats/goes19_CMIPF_C13_s20263101810214_rows1200-1800_cols1800-2400.nc";
    const LIMB: &str = "formats/goes19_CMIPF_C13_s20263101810214_rows2600-2800_cols0-300_limb.nc";

    use crate::testing::hms as t;

    fn key(band: &str, tag: &str, start: &str, created: &str) -> String {
        let hh = &start[7..9];
        format!(
            "ABI-L2-CMIPF/2026/310/{hh}/OR_ABI-L2-CMIPF-M6{band}_{tag}_s{start}_e{start}_c{created}.nc"
        )
    }

    #[test]
    fn keys_and_prefixes() {
        assert_eq!(product_parts(PRODUCT), Some(("ABI-L2-CMIPF", "C13")));
        assert_eq!(product_parts("ABI-L2-CMIPF"), None);
        assert_eq!(product_parts("ABI-L2-CMIPF/13"), None);
        assert_eq!(wx_core::families::goes::SATELLITE.product, PRODUCT);
        let k = "ABI-L2-CMIPF/2026/310/18/OR_ABI-L2-CMIPF-M6C13_G19_s20263101810214_e20263101819534_c20263101820006.nc";
        assert_eq!(
            parse_key("ABI-L2-CMIPF", "C13", "G19", k),
            Some(t(18, 10, 21) + Duration::milliseconds(400))
        );
        // The creation stamp is anything; other bands, satellites and
        // products are not this file.
        let other = k.replace("c20263101820006", "c20263101820999");
        assert_eq!(
            parse_key("ABI-L2-CMIPF", "C13", "G19", &other),
            Some(t(18, 10, 21) + Duration::milliseconds(400))
        );
        assert_eq!(parse_key("ABI-L2-CMIPF", "C13", "G18", k), None);
        assert_eq!(parse_key("ABI-L2-CMIPF", "C02", "G19", k), None);
        assert_eq!(
            parse_key("ABI-L2-CMIPF", "C13", "G19", &k.replace("C13", "C130")),
            None
        );
        assert_eq!(parse_key("ABI-L2-MCMIPF", "C13", "G19", k), None);
        assert_eq!(
            parse_key("ABI-L2-CMIPF", "C13", "G19", &k.replace(".nc", ".nc.tmp")),
            None
        );
        // Another scan mode is the same product.
        assert!(parse_key("ABI-L2-CMIPF", "C13", "G19", &k.replace("-M6C13", "-M3C13")).is_some());
        // Day of year 310 is 6 November; the prefix holds every band.
        assert_eq!(
            hour_prefix("ABI-L2-CMIPF", t(18, 25, 0)),
            "ABI-L2-CMIPF/2026/310/18/OR_ABI-L2-CMIPF-M"
        );
        assert!(k.starts_with(&hour_prefix("ABI-L2-CMIPF", t(18, 0, 0))));
        let jan = Utc.with_ymd_and_hms(2027, 1, 1, 0, 5, 0).unwrap();
        assert_eq!(
            hour_prefix("ABI-L2-CMIPF", jan),
            "ABI-L2-CMIPF/2027/001/00/OR_ABI-L2-CMIPF-M"
        );
        for s in wx_core::source::sources_of(wx_core::Family::Goes) {
            assert!(Goes.check(s).is_ok(), "{}", s.id);
        }
        let mut odd = *crate::testing::source("goes19");
        odd.region = "noaa-goes16";
        assert!(Goes.check(&odd).is_err());
    }

    fn decoded(name: &str) -> Field {
        wx_formats::goes::decode(&testing::fixture(name))
            .unwrap()
            .field
    }

    #[test]
    fn the_crop_keeps_the_view_and_the_side() {
        let east = Crop::of(satellite("noaa-goes19").unwrap());
        let west = Crop::of(satellite("noaa-goes18").unwrap());
        assert!(east.keeps(-75.0, 0.0) && east.keeps(-75.0, 69.9) && !east.keeps(-75.0, 70.1));
        assert!(east.keeps(-144.9, 0.0) && !east.keeps(-145.1, 0.0));
        // GOES-18 keeps its side of the split, across the antimeridian too.
        assert!(west.keeps(-137.0, 0.0) && west.keeps(-106.1, 10.0) && !west.keeps(-105.9, 10.0));
        assert!(west.keeps(160.0, 0.0) && !west.keeps(150.0, 0.0));
        assert!(!east.keeps(f64::NAN, 0.0));

        // The western limb at the equator: the outer columns, off the disk or
        // seen at a slant, go; the rest stays as it is.
        let limb = decoded(LIMB);
        let g = match &limb.proj {
            GridProjection::Geostationary(g) => g.clone(),
            _ => unreachable!(),
        };
        let cut = crop(limb.clone(), &east, 1).unwrap();
        assert_eq!(cut.ny, limb.ny);
        let dropped = limb.nx - cut.nx;
        assert!((40..70).contains(&dropped), "{dropped} columns dropped");
        let (mut kept, mut gone) = (0, 0);
        for j in 0..limb.ny {
            for i in 0..limb.nx {
                let (lon, lat) = g.inverse(
                    g.x_offset + i as f64 * g.x_scale,
                    g.y_offset + j as f64 * g.y_scale,
                );
                let was = limb.values[j * limb.nx + i];
                let now = (i >= dropped).then(|| cut.values[j * cut.nx + i - dropped]);
                if east.keeps(lon, lat) {
                    assert_eq!(now.map(f32::to_bits), Some(was.to_bits()), "({i}, {j})");
                    kept += 1;
                } else {
                    assert!(now.is_none_or(f32::is_nan), "({i}, {j}) at ({lon}, {lat})");
                    gone += usize::from(!was.is_nan());
                }
            }
        }
        assert!(kept > 40_000 && gone > 5_000, "{kept} kept, {gone} dropped");
        // The cut grid's cells are where they were.
        let (lon, lat) = cut.proj.ij_to_lonlat(10.0, 100.0);
        let (a, b) = limb.proj.ij_to_lonlat(10.0 + dropped as f64, 100.0);
        assert!((lon - a).abs() < 1e-9 && (lat - b).abs() < 1e-9);
    }

    #[test]
    fn the_split_and_the_coarser_grid() {
        let gulf = decoded(GULF);
        // Wholly inside GOES-19's view: nothing changes at full resolution.
        let east = Crop::of(satellite("noaa-goes19").unwrap());
        let same = crop(gulf.clone(), &east, 1).unwrap();
        assert_eq!((same.nx, same.ny), (gulf.nx, gulf.ny));
        assert_eq!(same.proj, gulf.proj);
        assert!(
            same.values
                .iter()
                .zip(&gulf.values)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        // A split at 90 W: the western cells only.
        let split = Crop {
            west_of: Some(-90.0),
            ..east
        };
        let west = crop(gulf.clone(), &split, 1).unwrap();
        assert!(
            west.nx < gulf.nx && west.ny == gulf.ny,
            "{} x {}",
            west.nx,
            west.ny
        );
        for j in 0..west.ny {
            for i in 0..west.nx {
                let (lon, _) = west.proj.ij_to_lonlat(i as f64, j as f64);
                assert_eq!(
                    !west.values[j * west.nx + i].is_nan(),
                    lon < -90.0,
                    "({i}, {j}) at {lon}"
                );
            }
        }
        // Halved: a quarter of the cells, each the mean of its block, centred
        // on the block.
        let half = crop(gulf.clone(), &east, 2).unwrap();
        assert_eq!((half.nx, half.ny), (300, 300));
        let mean = (gulf.values[0] + gulf.values[1] + gulf.values[600] + gulf.values[601]) / 4.0;
        assert!((half.values[0] - mean).abs() < 1e-4);
        let (lon, lat) = half.proj.ij_to_lonlat(0.0, 0.0);
        let (a, b) = gulf.proj.ij_to_lonlat(0.5, 0.5);
        assert!((lon - a).abs() < 1e-9 && (lat - b).abs() < 1e-9);
        // A crop that keeps nothing is an error, not an empty part.
        let none = Crop {
            west_of: Some(-120.0),
            ..east
        };
        assert!(crop(gulf, &none, 1).is_err());
    }

    /// A listing with every band, a second satellite and an earlier hour: the
    /// band-13 files of this satellite are fetched, and the real one becomes
    /// a part, cropped and on the coarser grid, valid at its scan start.
    #[tokio::test]
    async fn listed_scans_become_parts() {
        let mem = testing::remote(&["noaa-goes19", "noaa-goes18"]);
        let east = S3Bucket::aws("noaa-goes19");
        let gulf = testing::fixture(GULF);
        mem.put_s3(
            &east,
            &key("C13", "G19", "20263101810214", "20263101820006"),
            gulf.clone(),
        );
        mem.put_s3(
            &east,
            &key("C02", "G19", "20263101810214", "20263101820117"),
            &b"visible"[..],
        );
        mem.put_s3(
            &east,
            &key("C13", "G19", "20263101750214", "20263101800013"),
            gulf.clone(),
        );
        mem.put_s3(
            &S3Bucket::aws("noaa-goes18"),
            &key("C13", "G18", "20263101810223", "20263101820018"),
            &b"x"[..],
        );
        let (engine, store) = testing::engine(&mem);
        let mut task = crate::testing::observed_task(&engine, Goes, "goes19", PRODUCT);
        assert_eq!(task.name, "goes19.satellite");
        let report = task.cycle(t(18, 22, 0)).await.unwrap();
        // The 17:50 file holds the 18:10 scan: refused as mislabelled.
        assert_eq!(
            (report.tally.produced, report.tally.failed),
            (1, 1),
            "{:?}",
            report.tally
        );
        let parts = engine.catalog.timeline("satellite").parts.clone();
        let ids: Vec<&str> = parts.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["goes19-2611061810"]);
        let part = engine.catalog.latest("satellite", "goes19").unwrap();
        assert_eq!(part.valid, t(18, 10, 0));
        // The Gulf of Mexico cut: about 95 to 81 W, 17 to 30 N.
        let b = part.bounds;
        assert!(
            b.west > -96.0 && b.west < -94.0 && b.east > -81.0 && b.east < -79.0,
            "{b:?}"
        );
        assert!(
            b.south > 15.0 && b.south < 17.5 && b.north > 29.0 && b.north < 31.0,
            "{b:?}"
        );
        let solid = part
            .solid
            .expect("a cloud image with no gaps has a solid box");
        assert!(b.contains_box(&solid));
        // Stored on the coarser grid.
        let grid = testing::stored_grid(store.as_ref(), "satellite", &part.id).await;
        assert_eq!(grid.dimensions(), (300, 300));
        // Only the band-13 files of this satellite were fetched.
        let gets: Vec<String> = mem.fetches();
        assert_eq!(gets.len(), 2, "{gets:?}");
        assert!(gets.iter().all(|g| g.contains("C13_G19")));
        let lists: Vec<String> = mem.listings();
        assert!(lists.iter().all(|l| l.contains("noaa-goes19")), "{lists:?}");
    }

    /// A whole past hour of files that are not band 13 of this satellite as
    /// the adapter knows them stops the task with an error instead of
    /// letting it look healthy while it finds nothing; the current hour,
    /// still filling, does not.
    #[tokio::test]
    async fn renamed_files_are_an_error_not_silence() {
        let mem = testing::remote(&["noaa-goes19"]);
        let east = S3Bucket::aws("noaa-goes19");
        for (start, created) in [
            ("20263101800214", "20263101810006"),
            ("20263101810214", "20263101820006"),
        ] {
            let renamed = key("C13", "G19", start, created).replace("C13_G19", "C13-G19");
            mem.put_s3(&east, &renamed, &b"x"[..]);
            mem.put_s3(&east, &key("C02", "G19", start, created), &b"x"[..]);
        }
        let (engine, _) = testing::engine(&mem);
        let mut task = crate::testing::observed_task(&engine, Goes, "goes19", PRODUCT);
        // 18:40: hour 18 is still filling.
        assert_eq!(
            task.cycle(t(18, 40, 0)).await.unwrap().tally,
            Default::default()
        );
        let e = task.cycle(t(19, 15, 0)).await.err().expect("an error");
        assert!(format!("{e:#}").contains("no C13 file of G19"), "{e:#}");
    }

    /// GOES-18's task keeps the West's side: a GOES-East file put where
    /// GOES-18's should be is refused for looking down from the wrong place.
    #[tokio::test]
    async fn a_file_from_the_wrong_satellite_is_refused() {
        let mem = testing::remote(&["noaa-goes18"]);
        let west = S3Bucket::aws("noaa-goes18");
        mem.put_s3(
            &west,
            &key("C13", "G18", "20263101810223", "20263101820018"),
            testing::fixture(GULF),
        );
        let (engine, _) = testing::engine(&mem);
        let mut task = crate::testing::observed_task(&engine, Goes, "goes18", PRODUCT);
        let report = task.cycle(t(18, 22, 0)).await.unwrap();
        assert_eq!(
            (report.tally.produced, report.tally.failed),
            (0, 1),
            "{:?}",
            report.tally
        );
        assert!(engine.catalog.latest("satellite", "goes18").is_none());
    }
}

/// Checks against the real hosts, ignored by default:
/// `cargo test -p wx-ingest --features satellite -- --ignored live_satellite`.
///
/// `live_satellite_run` follows every satellite source (CMORPH2, GMGSI,
/// GOES-19, GOES-18) for `WX_LIVE_MINUTES` (default 10) into a local store
/// at `WX_LIVE_STORE` with an in-memory catalog, then prints the parts per
/// layer and source, one part's boxes per source and the traffic.
/// `live_satellite_orientation` then reads that store and checks that the
/// grids are where they should be (see its docs). Run each once; never loop.
#[cfg(test)]
mod live {
    use super::*;
    use crate::{Ingest, IngestConfig};
    use object_store::local::LocalFileSystem;
    use object_store::path::Path;
    use object_store::{ObjectStore, ObjectStoreExt};
    use std::collections::BTreeMap;
    use tokio_util::sync::CancellationToken;
    use wx_core::{FrameCatalog, HealthRegistry, Hub};
    use wx_pipeline::FrameGrid;

    const USER_AGENT: &str = "weather-map-dev";

    fn store_dir() -> Option<std::path::PathBuf> {
        let dir = std::env::var_os("WX_LIVE_STORE").map(std::path::PathBuf::from);
        if dir.is_none() {
            eprintln!("WX_LIVE_STORE not set: skipped");
        }
        dir
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "network: ten minutes against NOAA's hosts"]
    async fn live_satellite_run() {
        let Some(dir) = store_dir() else { return };
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "wx_ingest=info".into()),
            )
            .with_writer(std::io::stderr)
            .try_init();
        std::fs::create_dir_all(&dir).unwrap();
        let minutes: u64 = std::env::var("WX_LIVE_MINUTES")
            .ok()
            .and_then(|m| m.parse().ok())
            .unwrap_or(10);
        let store: Arc<dyn ObjectStore> = Arc::new(LocalFileSystem::new_with_prefix(&dir).unwrap());
        let catalog = Arc::new(FrameCatalog::new(Hub::new()));
        let config = IngestConfig {
            feeds: vec!["cmorph".into(), "gmgsi".into(), "goes".into()],
            user_agent: USER_AGENT.into(),
            ..IngestConfig::default()
        };
        let ingest = Ingest::new(store.clone(), catalog.clone(), config).unwrap();
        eprintln!("tasks: {:?}", ingest.task_names());
        let health = HealthRegistry::new(Hub::new());
        let cancel = CancellationToken::new();
        let started = std::time::Instant::now();
        let handles = ingest.spawn(&health, cancel.clone());
        for m in 1..=minutes {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let mut line = format!("{m} min:");
            for name in ingest.task_names() {
                if let Some(h) = health.get(&name) {
                    line.push_str(&format!(" [{name}: {:?} {}]", h.state, h.detail));
                }
            }
            eprintln!("{line}");
        }
        cancel.cancel();
        for h in handles {
            let _ = h.await;
        }
        let elapsed = started.elapsed().as_secs_f64();
        let mut counts: BTreeMap<(&str, String), usize> = BTreeMap::new();
        let mut newest: BTreeMap<String, (&str, wx_core::Part)> = BTreeMap::new();
        for (layer, part) in catalog.parts() {
            *counts.entry((layer, part.source.clone())).or_default() += 1;
            let e = newest
                .entry(part.source.clone())
                .or_insert((layer, part.clone()));
            if part.valid > e.1.valid {
                *e = (layer, part);
            }
        }
        for ((layer, source), n) in &counts {
            eprintln!("parts: {layer} {source} {n}");
        }
        for (source, (layer, p)) in &newest {
            eprintln!(
                "newest {source} on {layer}: {} valid {} bounds {:?} solid {:?}",
                p.id, p.valid, p.bounds, p.solid
            );
        }
        for t in ingest.traffic() {
            eprintln!(
                "traffic {}: {} lists, {} gets, {} not found, {} retries, {:.1} MB ({:.1} MB/h over the run)",
                t.host,
                t.list_requests,
                t.get_requests,
                t.not_found,
                t.retries,
                t.bytes as f64 / 1e6,
                t.bytes as f64 / 1e6 * 3600.0 / elapsed
            );
        }
        for name in ingest.task_names() {
            if let Some(h) = health.get(&name) {
                eprintln!(
                    "health {name}: {:?} {:?} {} error {:?}",
                    h.state, h.last_data_time, h.detail, h.last_error
                );
            }
        }
    }

    /// Stored parts: `(layer, part id)` to the grid.
    async fn grids(store: &dyn ObjectStore, layer: &str) -> BTreeMap<String, FrameGrid> {
        let mut out = BTreeMap::new();
        let listed: Vec<_> = futures::TryStreamExt::try_collect(
            store.list(Some(&Path::from(format!("grids/{layer}")))),
        )
        .await
        .unwrap();
        for meta in listed {
            let name = meta
                .location
                .filename()
                .unwrap()
                .trim_end_matches(".wxg")
                .to_string();
            let bytes = store
                .get(&meta.location)
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            out.insert(name, FrameGrid::from_bytes(&bytes).unwrap());
        }
        out
    }

    /// One band of a grid at points, as the map shows it.
    struct Sampler<'a>(crate::testing::live::Sampler<'a>);

    impl<'a> Sampler<'a> {
        fn new(grid: &'a FrameGrid, layer_id: &str, z: u8) -> Self {
            Self(crate::testing::live::Sampler::new(grid, layer_id, z))
        }

        fn at(&mut self, lon: f64, lat: f64) -> Option<f32> {
            self.0.at(lon, lat).map(|v| v[0]).filter(|v| !v.is_nan())
        }
    }

    fn correlation(pairs: &[(f64, f64)]) -> f64 {
        let n = pairs.len() as f64;
        let (ma, mb) = (
            pairs.iter().map(|p| p.0).sum::<f64>() / n,
            pairs.iter().map(|p| p.1).sum::<f64>() / n,
        );
        let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
        for (a, b) in pairs {
            sab += (a - ma) * (b - mb);
            saa += (a - ma) * (a - ma);
            sbb += (b - mb) * (b - mb);
        }
        sab / (saa * sbb).sqrt()
    }

    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    }

    /// Pairs of `a` at a point and `b` at the point moved by `(dlon, dlat)`
    /// (and mirrored north to south when `flip`), over a lattice.
    #[allow(clippy::too_many_arguments)]
    fn pairs(
        a: &mut Sampler,
        b: &mut Sampler,
        lons: (f64, f64),
        lats: (f64, f64),
        step: f64,
        dlon: f64,
        dlat: f64,
        flip: bool,
    ) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let mut lat = lats.0;
        while lat <= lats.1 {
            let mut lon = lons.0;
            while lon <= lons.1 {
                let blat = if flip { -lat } else { lat } + dlat;
                if let (Some(x), Some(y)) = (a.at(lon, lat), b.at(wrap_lon(lon + dlon), blat)) {
                    out.push((f64::from(x), f64::from(y)));
                }
                lon += step;
            }
            lat += step;
        }
        out
    }

    /// Where the temperature along a parallel rises most steeply (from sea
    /// to warmer land going east, or the reverse with `west`), and by how
    /// much, over a few cells.
    fn steepest(s: &mut Sampler, lat: f64, lons: (f64, f64), west: bool) -> Option<(f64, f64)> {
        let step = 0.05;
        let mut v = Vec::new();
        let mut lon = lons.0;
        while lon <= lons.1 {
            v.push((lon, s.at(lon, lat)?));
            lon += step;
        }
        (3..v.len().saturating_sub(3))
            .map(|k| {
                let rise = f64::from(v[k + 3].1 - v[k - 3].1);
                (v[k].0, if west { -rise } else { rise })
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// GFS's instantaneous precipitation rate (kg m-2 s-1) and land mask at
    /// the step of the newest run that has one nearest `at`.
    async fn gfs_rate_and_land(at: DateTime<Utc>) -> (Field, Field) {
        let remote = crate::testing::live::remote();
        let cx = Cx::new(&remote, "live", Utc::now());
        let input = &wx_core::families::gfs::PRECIP;
        let land = wx_core::layer::FieldSel {
            var: "LAND",
            level: "surface",
            stat: wx_core::layer::Stat::Instant,
        };
        for back in [6, 12, 18] {
            let run = clock::floor(at - Duration::hours(back), Duration::hours(6));
            let hour = ((at - run).num_minutes() as f64 / 60.0).round() as u32;
            let key = crate::families::gfs::file_key(run, wx_core::families::gfs::PRODUCT, hour);
            let url = S3Bucket::aws(crate::families::gfs::BUCKET).url(&key);
            let needs = [
                Need {
                    input,
                    field: input.fields.first().copied(),
                    lead: hour * 60,
                },
                Need {
                    input,
                    field: Some(land),
                    lead: hour * 60,
                },
            ];
            let Ok(Fetched::Got(p)) = cx
                .grib_with_idx(&url, &format!("{url}.idx"), None, &needs)
                .await
            else {
                continue;
            };
            let mut fields = Vec::new();
            for (payload, need) in p.iter().zip(&needs) {
                // Each payload is the one message.
                if let Some(Payload::Grib(b)) = payload {
                    let m = wx_pipeline::decode_messages(b, &[0]).unwrap().remove(0);
                    eprintln!(
                        "GFS {key} {} valid {}",
                        need.field.unwrap().var,
                        m.time.valid()
                    );
                    fields.push(m.field);
                }
            }
            if fields.len() == 2 {
                let land = fields.pop().unwrap();
                return (fields.pop().unwrap(), land);
            }
        }
        panic!("no GFS run has the precipitation rate and land mask");
    }

    /// Orientation and position, from the parts the run stored:
    ///
    /// * GOES-19 and GOES-18 against GMGSI at the same hour, over the
    ///   satellite's own side: the brightness temperatures agree (median
    ///   difference, correlation), and they agree best with no shift, not
    ///   with a degree's shift either way or mirrored north to south.
    /// * Coastlines in the afternoon: along 23.5 S (the Atacama, GOES-19) and
    ///   31 N (northern Baja California, GOES-18) the land is warmer
    ///   than the Pacific, so the steepest eastward rise in temperature
    ///   within a degree of the coast should sit on it. The coast is where
    ///   GFS's 0.25 degree land mask turns from sea to land. Reported, not
    ///   asserted: clouds can hide a coast.
    /// * CMORPH2's newest hourly total against a GFS precipitation rate from
    ///   NOAA's bucket at about the same time: zonal means agree on where the
    ///   tropical rain belt is, and the two correlate on a 2.5 degree lattice
    ///   with no shift far better than shifted by 180 degrees.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "network and a store written by live_satellite_run"]
    async fn live_satellite_orientation() {
        let Some(dir) = store_dir() else { return };
        let store = LocalFileSystem::new_with_prefix(&dir).unwrap();
        let rain = grids(&store, "rain_1h").await;
        let (rain_id, cm) = rain.iter().next_back().expect("an hourly CMORPH2 total");
        let end = chrono::NaiveDateTime::parse_from_str(
            rain_id.trim_start_matches("cmorph-"),
            "%y%m%d%H%M",
        )
        .unwrap()
        .and_utc();
        // GFS's rate near the middle of the total's newest half hour, and its
        // land mask.
        let (gfs, land) = gfs_rate_and_land(end - Duration::minutes(15)).await;
        let at = |f: &Field, lon: f64, lat: f64| {
            let (i, j) = f.proj.lonlat_to_ij(lon, lat);
            let (i, j) = (i.round() as i64, j.round() as i64);
            (i >= 0 && j >= 0 && (i as usize) < f.nx && (j as usize) < f.ny)
                .then(|| f64::from(f.values[j as usize * f.nx + i as usize]))
        };
        // Where the mask first turns from sea to land going east.
        let coast = |lat: f64, from: f64, to: f64| {
            let mut lon = from;
            while lon < to {
                if at(&land, lon, lat) == Some(0.0) && at(&land, lon + 0.25, lat) == Some(1.0) {
                    return Some(lon + 0.125);
                }
                lon += 0.25;
            }
            None
        };
        let sat = grids(&store, "satellite").await;
        let ids: Vec<&String> = sat.keys().collect();
        eprintln!("satellite parts: {ids:?}");
        // The newest GMGSI hour with a GOES scan at the same time.
        let hours: Vec<&String> = sat.keys().filter(|k| k.starts_with("gmgsi-")).collect();
        let mut checked = false;
        for hour in hours.iter().rev() {
            let stamp = hour.trim_start_matches("gmgsi-");
            for (goes, lons, lats) in [
                ("goes19", (-110.0, -40.0), (-50.0, 50.0)),
                ("goes18", (-175.0, -110.0), (-50.0, 50.0)),
            ] {
                let Some(g) = sat.get(&format!("{goes}-{stamp}")) else {
                    eprintln!("no {goes} part at {stamp}");
                    continue;
                };
                let m = &sat[*hour];
                let (mut a, mut b) = (
                    Sampler::new(g, "satellite", 5),
                    Sampler::new(m, "satellite", 5),
                );
                let same = pairs(&mut a, &mut b, lons, lats, 0.5, 0.0, 0.0, false);
                let diffs: Vec<f64> = same.iter().map(|(x, y)| y - x).collect();
                let warm: Vec<f64> = same
                    .iter()
                    .filter(|(x, _)| *x > -23.0)
                    .map(|(x, y)| y - x)
                    .collect();
                let cold: Vec<f64> = same
                    .iter()
                    .filter(|(x, _)| *x < -43.0)
                    .map(|(x, y)| y - x)
                    .collect();
                let r0 = correlation(&same);
                eprintln!(
                    "{goes} vs GMGSI at {stamp}: {} pairs, r {r0:.3}, median GMGSI - GOES {:.2} K (warm, {} pairs: {:.2} K; cold, {} pairs: {:.2} K), within 5 K {:.0} %",
                    same.len(),
                    median(diffs.clone()),
                    warm.len(),
                    median(warm.clone()),
                    cold.len(),
                    if cold.is_empty() {
                        f64::NAN
                    } else {
                        median(cold.clone())
                    },
                    100.0 * diffs.iter().filter(|d| d.abs() <= 5.0).count() as f64
                        / diffs.len() as f64
                );
                for (dlon, dlat, flip) in [
                    (1.0, 0.0, false),
                    (-1.0, 0.0, false),
                    (0.0, 1.0, false),
                    (0.0, -1.0, false),
                    (0.0, 0.0, true),
                ] {
                    let r = correlation(&pairs(&mut a, &mut b, lons, lats, 0.5, dlon, dlat, flip));
                    eprintln!(
                        "  shifted ({dlon}, {dlat}){}: r {r:.3}",
                        if flip { " and mirrored" } else { "" }
                    );
                    assert!(
                        r < r0,
                        "{goes}: a shift of ({dlon}, {dlat}) flip {flip} matches GMGSI better ({r:.3} against {r0:.3})"
                    );
                }
                assert!(
                    r0 > 0.8 && median(warm).abs() < 2.0,
                    "{goes} disagrees with GMGSI"
                );
                let (coast_lat, from) = if goes == "goes19" {
                    (-23.5, -74.0)
                } else {
                    (31.0, -119.0)
                };
                let Some(shore) = coast(coast_lat, from, from + 6.0) else {
                    eprintln!("  no coast in the land mask along {coast_lat}");
                    continue;
                };
                for (name, s) in [(goes, &mut a), ("gmgsi", &mut b)] {
                    match steepest(s, coast_lat, (shore - 1.0, shore + 1.0), false) {
                        Some((lon, rise)) => eprintln!(
                            "  coast along {coast_lat} at {shore:.3} (GFS land mask): {name} rises most ({rise:.1} K over 0.3 degrees) at {lon:.2}"
                        ),
                        None => eprintln!("  coast along {coast_lat}: {name} has no data"),
                    }
                }
                checked = true;
            }
            if checked {
                break;
            }
        }
        assert!(checked, "no GMGSI hour with a GOES scan at the same time");
        // Each part's boxes.
        for (id, g) in &sat {
            if id.ends_with("00") || sat.len() < 6 {
                eprintln!(
                    "{id}: {:?} cells, bounds {:?}, solid {:?}",
                    g.dimensions(),
                    g.bounds(),
                    g.solid()
                );
            }
        }

        // CMORPH2 against GFS.
        eprintln!(
            "{rain_id}: {:?} cells, bounds {:?}, solid {:?}",
            cm.dimensions(),
            cm.bounds(),
            cm.solid()
        );
        let gfs_at = |lon: f64, lat: f64| at(&gfs, lon, lat).map(|v| v * 3600.0);
        let mut s = Sampler::new(cm, "rain_1h", 3);
        // Zonal means over 5 degree bands, 60 S to 60 N, both in mm/h.
        let mut zonal = Vec::new();
        for band in -12..12 {
            let (mut c, mut g, mut n) = (0.0, 0.0, 0);
            for k in 0..5 {
                let lat = band as f64 * 5.0 + k as f64 + 0.5;
                let mut lon = -179.5;
                while lon < 180.0 {
                    if let (Some(x), Some(y)) = (s.at(lon, lat), gfs_at(lon, lat)) {
                        c += f64::from(x);
                        g += y;
                        n += 1;
                    }
                    lon += 1.0;
                }
            }
            zonal.push((
                band as f64 * 5.0 + 2.5,
                c / n.max(1) as f64,
                g / n.max(1) as f64,
            ));
        }
        for (lat, c, g) in &zonal {
            eprintln!("  zonal {lat:+5.1}: CMORPH2 {c:.3} mm/h, GFS {g:.3} mm/h");
        }
        let tropics: Vec<&(f64, f64, f64)> = zonal.iter().filter(|z| z.0.abs() < 20.0).collect();
        let wettest = |pick: fn(&(f64, f64, f64)) -> f64| {
            tropics
                .iter()
                .max_by(|a, b| pick(a).total_cmp(&pick(b)))
                .unwrap()
                .0
        };
        let (c_belt, g_belt) = (wettest(|z| z.1), wettest(|z| z.2));
        eprintln!("rain belt: CMORPH2 at {c_belt}, GFS at {g_belt}");
        let zr = correlation(&zonal.iter().map(|z| (z.1, z.2)).collect::<Vec<_>>());
        // On a 2.5 degree lattice, as it is and shifted by 180 degrees.
        let lattice = |shift: f64, s: &mut Sampler| {
            let mut out = Vec::new();
            let mut lat = -58.75;
            while lat < 60.0 {
                let mut lon = -178.75;
                while lon < 180.0 {
                    if let (Some(x), Some(y)) = (s.at(lon, lat), gfs_at(wrap_lon(lon + shift), lat))
                    {
                        out.push((f64::from(x).min(10.0), y.min(10.0)));
                    }
                    lon += 2.5;
                }
                lat += 2.5;
            }
            out
        };
        let (r0, r180) = (
            correlation(&lattice(0.0, &mut s)),
            correlation(&lattice(180.0, &mut s)),
        );
        eprintln!(
            "CMORPH2 against GFS: zonal r {zr:.2}; 2.5 degree points r {r0:.2}, shifted 180 degrees r {r180:.2}"
        );
        assert!(zr > 0.6 && r0 > 2.0 * r180.max(0.05) && (c_belt - g_belt).abs() <= 5.0);
    }
}
