//! Japan Meteorological Agency high-resolution precipitation nowcast tiles
//! (`hrpns`), palette PNG on the Web-Mercator XYZ scheme.
//!
//! `https://www.jma.go.jp/bosai/jmatile/data/nowc/{basetime}/none/{validtime}/surf/hrpns/{z}/{x}/{y}.png`:
//! 256 x 256 tiles, 4-bit palette with `tRNS`. The tile carries a rain-rate
//! class per pixel, not a rate. Transparent pixels (palette entries 0 and 1,
//! both white with alpha 0; entry 1 is drawn in blocks around rain) are
//! dry. Tiles without any rain come as fully transparent 8-bit RGBA PNGs.
//! The times are in the URL, not the tile, so the caller keeps them.
//!
//! # Classes
//!
//! Each colour becomes the middle of its class, in mm/h:
//!
//! | Colour | Class | Value |
//! |---|---|---|
//! | `#F2F2FF` | below 1 | 0.5 |
//! | `#A0D2FF` | 1 to 5 | 3 |
//! | `#218CFF` | 5 to 10 | 7.5 |
//! | `#0041FF` | 10 to 20 | 15 |
//! | `#FAF500` | 20 to 30 | 25 |
//! | `#FF9900` | 30 to 50 | 40 |
//! | `#FF2800` | 50 to 80 | 65 |
//! | `#B40068` | 80 and above | 90 |
//!
//! APPROXIMATE: the colours are the ones in the tiles' palette, but the
//! class boundaries are JMA's published nowcast legend as remembered, not
//! read from any file: JMA's legend definition was not among the samples.
//! The top class is open-ended; 90 stands for it.
//!
//! # Coverage
//!
//! A tile cannot tell dry from unobserved, and the `hrpns_nd` no-data mask
//! was not retrievable. Transparent pixels inside [`JMA_DOMAIN`], the
//! nowcast's analysis domain, are taken as dry (0); everything outside it is
//! `NaN`, as are pixels of tiles the caller did not have. APPROXIMATE: the
//! radars see less than that whole box, so far out at sea "dry" may really
//! be "not observed".

use anyhow::{Context, Result, anyhow, bail, ensure};
use std::f64::consts::PI;
use wx_core::Field;
use wx_core::proj::{Earth, GridProjection, LonLatBox, MercatorGrid, x_to_lon, y_to_lat};

use crate::checked_points;

/// Pixels per tile edge.
pub const TILE_SIZE: usize = 256;

/// The JMA nowcast's analysis domain (its 1 km GPV grid, 118 to 150 E, 20
/// to 48 N), within which an empty pixel is taken as dry.
pub const JMA_DOMAIN: LonLatBox = LonLatBox::new(118.0, 20.0, 150.0, 48.0);

/// Class colours and the rain rate, mm/h, each stands for.
pub const CLASSES: [([u8; 3], f32); 8] = [
    ([242, 242, 255], 0.5),
    ([160, 210, 255], 3.0),
    ([33, 140, 255], 7.5),
    ([0, 65, 255], 15.0),
    ([250, 245, 0], 25.0),
    ([255, 153, 0], 40.0),
    ([255, 40, 0], 65.0),
    ([180, 0, 104], 90.0),
];

/// Web-Mercator sphere radius, metres.
const R: f64 = 6_378_137.0;

/// Assemble a rectangle of tiles at zoom `z` into one rain-rate field.
///
/// `tiles` holds `cols * rows` entries, row by row from the north-west
/// tile `(x0, y0)`; `None` marks a tile the caller does not have. The
/// result is on the Web-Mercator grid of those tiles' pixels, `256 * cols`
/// by `256 * rows`, rows north to south.
pub fn decode_tiles(z: u8, x0: u32, y0: u32, cols: u32, rows: u32, tiles: &[Option<&[u8]>]) -> Result<Field> {
    ensure!(z <= 22, "zoom {z}");
    let n = 1u64 << z;
    ensure!(cols >= 1 && rows >= 1, "empty tile rectangle");
    ensure!(
        x0 as u64 + cols as u64 <= n && y0 as u64 + rows as u64 <= n,
        "tiles {x0}..{} x {y0}..{} beyond zoom {z}",
        x0 as u64 + cols as u64,
        y0 as u64 + rows as u64
    );
    ensure!(tiles.len() == (cols as usize) * (rows as usize), "{} tiles for {cols} x {rows}", tiles.len());
    let nx = cols as usize * TILE_SIZE;
    let ny = rows as usize * TILE_SIZE;
    checked_points("JMA tiles", nx, ny)?;

    let mut values = vec![f32::NAN; nx * ny];
    for (k, tile) in tiles.iter().enumerate() {
        let Some(bytes) = tile else { continue };
        let (tc, tr) = (k % cols as usize, k / cols as usize);
        let rates = tile_rates(bytes).with_context(|| format!("tile {z}/{}/{}", x0 as usize + tc, y0 as usize + tr))?;
        for row in 0..TILE_SIZE {
            let dst = (tr * TILE_SIZE + row) * nx + tc * TILE_SIZE;
            values[dst..dst + TILE_SIZE].copy_from_slice(&rates[row * TILE_SIZE..(row + 1) * TILE_SIZE]);
        }
    }

    // Pixel centres on the Web-Mercator plane, metres, with the central
    // meridian in the middle of the rectangle.
    let pixel = 2.0 * PI * R / (n as f64 * TILE_SIZE as f64);
    let gx = |i: f64| (x0 as f64 * TILE_SIZE as f64 + i) * pixel - PI * R;
    let gy = |j: f64| PI * R - (y0 as f64 * TILE_SIZE as f64 + j) * pixel;
    let lon0 = x_to_lon((x0 as f64 + cols as f64 / 2.0) / n as f64);
    let first_x = gx(0.5) - R * lon0.to_radians();
    let proj = GridProjection::Mercator(MercatorGrid::from_projected(
        Earth::sphere(R),
        0.0,
        lon0,
        first_x,
        gy(0.5),
        pixel,
        -pixel,
    ));

    // Dry only inside the domain; the rest of an empty pixel is unknown.
    for j in 0..ny {
        let lat = y_to_lat((y0 as f64 * TILE_SIZE as f64 + j as f64 + 0.5) / (n as f64 * TILE_SIZE as f64));
        for i in 0..nx {
            let v = &mut values[j * nx + i];
            if *v == 0.0 {
                let lon = x_to_lon((x0 as f64 * TILE_SIZE as f64 + i as f64 + 0.5) / (n as f64 * TILE_SIZE as f64));
                if !JMA_DOMAIN.contains(lon, lat) {
                    *v = f32::NAN;
                }
            }
        }
    }
    Ok(Field { nx, ny, proj, values, grid_relative_winds: false })
}

/// One tile's pixels as rain rates, mm/h: 0 where transparent.
fn tile_rates(bytes: &[u8]) -> Result<Vec<f32>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| anyhow!("not a PNG: {e}"))?;
    let info = reader.info();
    ensure!(
        info.width as usize == TILE_SIZE && info.height as usize == TILE_SIZE,
        "tile is {} x {}, expected {TILE_SIZE} x {TILE_SIZE}",
        info.width,
        info.height
    );
    let size = reader.output_buffer_size().ok_or_else(|| anyhow!("PNG too large"))?;
    ensure!(size <= TILE_SIZE * TILE_SIZE * 8, "PNG output of {size} bytes for a 256 x 256 tile");
    let mut buf = vec![0u8; size];
    let frame = reader.next_frame(&mut buf).map_err(|e| anyhow!("damaged PNG: {e}"))?;
    let channels = match frame.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => bail!("palette was not expanded"),
    };
    ensure!(frame.bit_depth == png::BitDepth::Eight, "bit depth {:?} after expansion", frame.bit_depth);
    let line = frame.line_size;
    let mut out = Vec::with_capacity(TILE_SIZE * TILE_SIZE);
    for row in 0..TILE_SIZE {
        let px = &buf[row * line..row * line + TILE_SIZE * channels];
        for p in px.chunks_exact(channels) {
            let (rgb, alpha) = match channels {
                4 => ([p[0], p[1], p[2]], p[3]),
                3 => ([p[0], p[1], p[2]], 255),
                2 => ([p[0], p[0], p[0]], p[1]),
                _ => ([p[0], p[0], p[0]], 255),
            };
            if alpha == 0 {
                out.push(0.0);
                continue;
            }
            let rate = CLASSES.iter().find(|(c, _)| *c == rgb).map(|(_, r)| *r).ok_or_else(|| {
                anyhow!("pixel colour #{:02X}{:02X}{:02X} is not a JMA rain class", rgb[0], rgb[1], rgb[2])
            })?;
            out.push(rate);
        }
    }
    Ok(out)
}
