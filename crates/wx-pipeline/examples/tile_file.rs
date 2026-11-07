//! Run GRIB2 files through the whole pipeline and write the tiles to disk.
//!
//! ```text
//! cargo run --release -p wx-pipeline --example tile_file -- \
//!     --layer temp --feed hrrr --out /tmp/tiles fixtures/hrrr_t10z_f06__TMP_2mabove.grib2
//! ```
//!
//! `--feed` names the source family whose input row of the layer is used.
//! Options: `--zoom Z` (tiles are written for zooms 0..=Z, default 4), `--png-zoom Z`
//! (zoom of the quick-look mosaic, default 4), `--repeat N` (run the timed
//! stages N times and report the fastest). Several input files are
//! concatenated; gzip input must be a single file or all files gzipped.
//!
//! Writes `<out>/{z}/{x}/{y}.wxt`, `<out>/grid.wxg` and `<out>/mosaic_z{Z}.png`,
//! a picture of one zoom level with a 10-degree graticule and a few cities
//! marked, to check the data is the right way up and in the right place.

use std::collections::BTreeMap;

use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use wx_core::TileCoord;
use wx_core::layer::{BandSpec, LayerSpec, Quantity, Render, layer};
use wx_core::proj::{TILE_SAMPLES, lat_to_y, lon_to_x};
use wx_pipeline::{FrameGrid, decode, derive};

struct Args {
    layer: String,
    feed: String,
    out: PathBuf,
    zoom: Option<u8>,
    png_zoom: u8,
    repeat: usize,
    files: Vec<PathBuf>,
}

fn args() -> Result<Args> {
    let mut a = Args {
        layer: String::new(),
        feed: String::new(),
        out: PathBuf::new(),
        zoom: None,
        png_zoom: 4,
        repeat: 1,
        files: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| anyhow!("{arg} needs a value"));
        match arg.as_str() {
            "--layer" => a.layer = value()?,
            "--feed" => a.feed = value()?,
            "--out" => a.out = value()?.into(),
            "--zoom" => a.zoom = Some(value()?.parse()?),
            "--png-zoom" => a.png_zoom = value()?.parse()?,
            "--repeat" => a.repeat = value()?.parse::<usize>()?.max(1),
            s if s.starts_with("--") => bail!("unknown option {s}"),
            _ => a.files.push(arg.into()),
        }
    }
    if a.layer.is_empty() || a.feed.is_empty() || a.out.as_os_str().is_empty() || a.files.is_empty()
    {
        bail!(
            "usage: tile_file --layer ID --feed mrms|rtma|hrrr|gfs --out DIR \
             [--zoom Z] [--png-zoom Z] [--repeat N] FILE..."
        );
    }
    Ok(a)
}

/// Runs `f` `n` times, returning the last result and the fastest time.
fn timed<T>(n: usize, mut f: impl FnMut() -> Result<T>) -> Result<(T, Duration)> {
    let mut best = Duration::MAX;
    let mut out = None;
    for _ in 0..n {
        let t = Instant::now();
        let v = f()?;
        best = best.min(t.elapsed());
        out = Some(v);
    }
    Ok((out.expect("n >= 1"), best))
}

fn ms(d: Duration) -> String {
    format!("{:8.1} ms", d.as_secs_f64() * 1e3)
}

fn main() -> Result<()> {
    let a = args()?;
    let l = layer(&a.layer).ok_or_else(|| anyhow!("unknown layer {}", a.layer))?;
    let source = l
        .inputs
        .iter()
        .find(|i| i.family.as_str() == a.feed)
        .ok_or_else(|| anyhow!("layer {} has no {} input", l.id, a.feed))?;
    let max_zoom = wx_core::source::sources_of(source.family)
        .map(|s| s.max_zoom)
        .max()
        .unwrap_or(6);
    let zoom = a.zoom.unwrap_or(4);

    let mut bytes = Vec::new();
    for f in &a.files {
        bytes.extend(std::fs::read(f).with_context(|| format!("reading {}", f.display()))?);
    }
    println!(
        "{} / {}: {} input bytes, {} threads",
        l.id,
        a.feed,
        bytes.len(),
        rayon::current_num_threads()
    );

    let (set, t_decode) = timed(a.repeat, || decode(&bytes, source.fields))?;
    let f = &set.fields[0];
    println!(
        "decoded {} field(s), {} x {} = {} points, reference {}, valid {}",
        set.fields.len(),
        f.nx,
        f.ny,
        f.nx * f.ny,
        set.reference_time,
        set.valid_time
    );
    let (raster, t_derive) = timed(a.repeat, || derive(l, source, &set.fields))?;
    let (grid, t_build) = timed(a.repeat, || Ok(FrameGrid::build(l, &raster)))?;
    let (tiles, t_eager) = timed(a.repeat, || {
        let coords: Vec<TileCoord> = (0..=zoom).flat_map(|z| grid.covering(z)).collect();
        Ok(coords
            .into_par_iter()
            .filter_map(|c| grid.render(c).map(|b| (c, b)))
            .collect::<Vec<_>>())
    })?;
    let (wxg, t_to_bytes) = timed(a.repeat, || Ok(grid.to_bytes()))?;
    let (_, t_from_bytes) = timed(a.repeat, || FrameGrid::from_bytes(&wxg))?;

    // Single-tile renders at the source's top zoom, where on-demand tiles come from.
    let sample: Vec<TileCoord> = grid
        .covering(max_zoom)
        .into_iter()
        .step_by(7)
        .take(40)
        .collect();
    let (mut one, mut encode, mut sampling) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..a.repeat.max(3) {
        for &t in &sample {
            let start = Instant::now();
            let Some(bytes) = grid.render(t) else {
                continue;
            };
            let total = start.elapsed();
            // The WXT1 zlib encode alone, to tell sampling from compression.
            let tile = wx_tile::decode(&bytes)?;
            let start = Instant::now();
            let _ = wx_tile::encode(&tile);
            let enc = start.elapsed();
            one.push(total);
            encode.push(enc);
            sampling.push(total.saturating_sub(enc));
        }
    }
    for v in [&mut one, &mut encode, &mut sampling] {
        v.sort();
    }

    println!();
    println!("decode (gunzip included) {}", ms(t_decode));
    println!("derive                   {}", ms(t_derive));
    println!("build (quantize+pyramid) {}", ms(t_build));
    println!("tiles z0-{zoom:<2}               {}", ms(t_eager));
    println!(
        "end to end               {}",
        ms(t_decode + t_derive + t_build + t_eager)
    );
    println!(
        "to_bytes                 {}  ({:.2} MB .wxg, {} levels rebuilt on load)",
        ms(t_to_bytes),
        wxg.len() as f64 / 1e6,
        grid.level_count()
    );
    println!("from_bytes               {}", ms(t_from_bytes));
    let stat = |v: &[Duration]| {
        format!(
            "median {}, p90 {}",
            ms(v[v.len() / 2]).trim(),
            ms(v[v.len() * 9 / 10]).trim()
        )
    };
    if !one.is_empty() {
        println!(
            "one non-empty tile at z{}, {} renders: {}",
            max_zoom,
            one.len(),
            stat(&one)
        );
        println!("  sampling                {}", stat(&sampling));
        println!("  WXT1 encode (zlib 6)    {}", stat(&encode));
    }
    println!("grid in memory {:.1} MB", grid.approx_bytes() as f64 / 1e6);
    println!("bounds {:?}", grid.bounds());
    let t = Instant::now();
    let solid = grid.solid();
    println!("solid  {solid:?} (found in {})", ms(t.elapsed()).trim());

    let mut per_zoom: BTreeMap<u8, Vec<usize>> = BTreeMap::new();
    for (t, b) in &tiles {
        per_zoom.entry(t.z).or_default().push(b.len());
    }
    println!();
    println!(" z  tiles   total KB   mean KB    max KB");
    for (z, sizes) in &per_zoom {
        let total: usize = sizes.iter().sum();
        println!(
            "{z:2} {:6} {:10.1} {:9.1} {:9.1}",
            sizes.len(),
            total as f64 / 1e3,
            total as f64 / 1e3 / sizes.len() as f64,
            *sizes.iter().max().unwrap() as f64 / 1e3
        );
    }

    for (t, b) in &tiles {
        let dir = a.out.join(t.z.to_string()).join(t.x.to_string());
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(format!("{}.wxt", t.y)), b)?;
    }
    std::fs::write(a.out.join("grid.wxg"), &wxg)?;
    let png = a.out.join(format!("mosaic_z{}.png", a.png_zoom));
    mosaic(&grid, l, a.png_zoom, &png)?;
    println!();
    println!(
        "wrote {} tiles under {}, mosaic {}",
        tiles.len(),
        a.out.display(),
        png.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Quick-look mosaic
// ---------------------------------------------------------------------------

type Rgb = [u8; 3];

const NO_DATA: Rgb = [34, 40, 49];

fn ramp(stops: &[(f32, Rgb)], v: f32) -> Rgb {
    if v <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if v <= b {
            let t = (v - a) / (b - a);
            return [0, 1, 2]
                .map(|k| (f32::from(ca[k]) + t * (f32::from(cb[k]) - f32::from(ca[k]))) as u8);
        }
    }
    stops[stops.len() - 1].1
}

fn colour(quantity: Quantity, v: f32) -> Rgb {
    match quantity {
        Quantity::Temperature => ramp(
            &[
                (-30.0, [120, 40, 160]),
                (-15.0, [40, 70, 200]),
                (0.0, [90, 180, 230]),
                (10.0, [80, 190, 110]),
                (20.0, [240, 220, 60]),
                (30.0, [240, 120, 40]),
                (40.0, [180, 20, 30]),
            ],
            v,
        ),
        Quantity::Reflectivity => {
            if v <= wx_core::layer::NO_ECHO_DBZ + 0.05 {
                [205, 205, 200]
            } else {
                ramp(
                    &[
                        (-10.0, [205, 205, 200]),
                        (5.0, [150, 200, 230]),
                        (20.0, [40, 170, 60]),
                        (35.0, [240, 220, 40]),
                        (50.0, [220, 40, 30]),
                        (65.0, [200, 40, 200]),
                    ],
                    v,
                )
            }
        }
        Quantity::Speed => ramp(
            &[
                (0.0, [230, 240, 250]),
                (10.0, [80, 160, 220]),
                (25.0, [140, 40, 160]),
            ],
            v,
        ),
        Quantity::PrecipRate | Quantity::PrecipDepth => ramp(
            &[
                (0.0, [235, 235, 230]),
                (0.1, [170, 220, 170]),
                (5.0, [40, 140, 70]),
                (25.0, [30, 60, 150]),
            ],
            v,
        ),
        Quantity::SnowDepth => ramp(
            &[
                (0.0, [235, 235, 230]),
                (20.0, [150, 200, 240]),
                (250.0, [80, 40, 160]),
            ],
            v,
        ),
        Quantity::Percent => ramp(&[(0.0, [60, 120, 200]), (100.0, [240, 240, 240])], v),
        Quantity::Category => match v as i32 {
            0 => [235, 235, 230],
            1 => [40, 160, 70],
            2 => [60, 130, 230],
            3 => [230, 140, 40],
            4 => [220, 50, 120],
            _ => [140, 60, 180],
        },
    }
}

/// Colour of one pixel from the tile's decoded samples.
fn pixel(l: &LayerSpec, tile: &wx_tile::Tile, k: usize) -> Rgb {
    let value = |b: u8| -> Option<(BandSpec, f32)> {
        let spec = l.bands[b as usize];
        spec.dequantize(tile.bands[b as usize][k])
            .map(|v| (spec, v))
    };
    match l.render {
        Render::Scalar { band } => value(band).map_or(NO_DATA, |(s, v)| colour(s.quantity, v)),
        Render::Wind { u, v, .. } => match (value(u), value(v)) {
            (Some((s, a)), Some((_, b))) => colour(s.quantity, a.hypot(b)),
            _ => NO_DATA,
        },
        Render::PrecipType { rate, ptype } => match (value(rate), value(ptype)) {
            (Some((_, r)), Some((s, t))) if r >= 0.01 => colour(s.quantity, t),
            (Some(_), Some(_)) => [235, 235, 230],
            _ => NO_DATA,
        },
    }
}

fn mosaic(grid: &FrameGrid, l: &LayerSpec, z: u8, path: &Path) -> Result<()> {
    // The grid's own tiles; of the two runs a box across the antimeridian
    // gives, the wider.
    let (x0, y0, x1, y1) = grid
        .bounds()
        .tile_ranges(z)
        .into_iter()
        .max_by_key(|r| r.2 - r.0)
        .ok_or_else(|| anyhow!("empty grid"))?;
    let cells = TILE_SAMPLES - 1;
    let (w, h) = (
        ((x1 - x0 + 1) as usize) * cells,
        ((y1 - y0 + 1) as usize) * cells,
    );
    let mut img = vec![0u8; w * h * 3];
    for ty in y0..=y1 {
        for tx in x0..=x1 {
            let t = TileCoord::new(z, tx, ty).expect("in range");
            let tile = grid.render(t).map(|b| wx_tile::decode(&b)).transpose()?;
            for r in 0..cells {
                for c in 0..cells {
                    let rgb = match &tile {
                        Some(tile) => pixel(l, tile, r * TILE_SAMPLES + c),
                        None => NO_DATA,
                    };
                    let px = (tx - x0) as usize * cells + c;
                    let py = (ty - y0) as usize * cells + r;
                    img[(py * w + px) * 3..][..3].copy_from_slice(&rgb);
                }
            }
        }
    }
    let scale = (1u64 << z) as f64 * cells as f64;
    let to_px = |lon: f64, lat: f64| {
        (
            lon_to_x(lon) * scale - f64::from(x0) * cells as f64,
            lat_to_y(lat) * scale - f64::from(y0) * cells as f64,
        )
    };
    let mut put = |x: f64, y: f64, rgb: Rgb, alpha: f32| {
        if x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h {
            let k = ((y as usize) * w + x as usize) * 3;
            for c in 0..3 {
                img[k + c] =
                    (f32::from(img[k + c]) * (1.0 - alpha) + f32::from(rgb[c]) * alpha) as u8;
            }
        }
    };
    // Graticule every 10 degrees; 40 N and 100 W drawn brighter.
    for lon in (-180..=180).step_by(10) {
        let a = if lon == -100 { 0.9 } else { 0.4 };
        for py in 0..h {
            let (x, _) = to_px(f64::from(lon), 0.0);
            put(x, py as f64, [255, 255, 255], a);
        }
    }
    for lat in (-80..=80).step_by(10) {
        let a = if lat == 40 { 0.9 } else { 0.4 };
        let (_, y) = to_px(0.0, f64::from(lat));
        for px in 0..w {
            put(px as f64, y, [255, 255, 255], a);
        }
    }
    let cities = [
        (-74.0, 40.71),
        (-87.63, 41.88),
        (-95.37, 29.76),
        (-104.99, 39.74),
        (-122.33, 47.61),
        (-80.19, 25.76),
        (-118.24, 34.05),
    ];
    for (lon, lat) in cities {
        let (x, y) = to_px(lon, lat);
        for d in -6..=6 {
            let d = f64::from(d);
            for (dx, dy) in [(d, 0.0), (0.0, d)] {
                put(x + dx, y + dy, [0, 0, 0], 1.0);
                put(x + dx + 1.0, y + dy + 1.0, [255, 255, 255], 1.0);
            }
        }
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&img)?;
    Ok(())
}
