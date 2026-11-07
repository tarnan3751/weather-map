//! WXT1: the raw-value tile format served to the browser.
//!
//! A tile carries quantized physical values, never colors. The browser turns
//! values into pixels in a shader, so palettes, units and hover read-outs all
//! work from the same bytes.
//!
//! ```text
//! offset size field
//!      0    4 magic            "WXT1"
//!      4    2 width            u16 LE, samples per row
//!      6    2 height           u16 LE, rows
//!      8    1 bands            1..=8
//!      9    1 compression      0 = stored, 1 = zlib (RFC 1950)
//!     10    1 predictor        0 = none, 1 = delta + byte planes
//!     11    1 reserved         0
//!     12    4 raw_len          u32 LE, payload length once decompressed
//!                              (= bands * width * height * 2)
//!     16    … payload
//! ```
//!
//! Samples are `u16`. `0` means "no data"; every other value `q` maps to a
//! physical value `offset + (q - 1) * scale`, with `scale`/`offset` taken from
//! the layer's band description (they are not repeated in each tile).
//!
//! Payload layout before compression, bands in order, rows top (north) to
//! bottom, samples left (west) to right:
//!
//! * predictor 0: `u16` little-endian samples.
//! * predictor 1: each sample is replaced by its wrapping difference from the
//!   sample to its left; the first sample of a row is differenced against the
//!   first sample of the row above (the very first sample against 0). The
//!   differences of one band are then written as all low bytes followed by all
//!   high bytes.

use std::io::{Read, Write};

pub const MAGIC: [u8; 4] = *b"WXT1";
pub const HEADER_LEN: usize = 16;
/// Sample value reserved for "no data".
pub const NODATA: u16 = 0;
/// Samples per edge of the tiles this project produces: 256 cells plus one,
/// because samples sit on the tile's lattice points and the last row/column
/// repeats the neighbouring tile's first. The format itself allows any size.
pub const TILE_SAMPLES: usize = 257;
pub const MAX_BANDS: usize = 8;
/// Largest width or height [`decode`] accepts. Real tiles are 257; the cap
/// keeps a corrupt header from asking for gigabytes.
pub const MAX_DIMENSION: u16 = 4096;

const COMPRESSION_STORED: u8 = 0;
const COMPRESSION_ZLIB: u8 = 1;
const PREDICTOR_NONE: u8 = 0;
const PREDICTOR_DELTA_PLANES: u8 = 1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TileError {
    #[error("tile is shorter than its header")]
    Truncated,
    #[error("not a WXT1 tile")]
    BadMagic,
    #[error("unsupported {0} code {1}")]
    Unsupported(&'static str, u8),
    #[error("tile header is inconsistent: {0}")]
    BadHeader(&'static str),
    #[error("tile payload is corrupt")]
    Corrupt,
}

/// A decoded tile: `bands[b][row * width + col]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub width: u16,
    pub height: u16,
    pub bands: Vec<Vec<u16>>,
}

impl Tile {
    pub fn new(width: u16, height: u16, bands: Vec<Vec<u16>>) -> Self {
        let n = width as usize * height as usize;
        assert!(n > 0, "a tile has at least one sample");
        assert!(
            !bands.is_empty() && bands.len() <= MAX_BANDS,
            "a tile holds 1..={MAX_BANDS} bands"
        );
        assert!(
            bands.iter().all(|b| b.len() == n),
            "every band must hold width * height samples"
        );
        Self {
            width,
            height,
            bands,
        }
    }

    /// True when every sample of every band is [`NODATA`]. Such tiles are not
    /// stored; the server answers 204 for them.
    pub fn is_empty(&self) -> bool {
        self.bands.iter().all(|b| b.iter().all(|&v| v == NODATA))
    }
}

/// Encode with the default settings (delta predictor, zlib level 3).
///
/// After the delta predictor, level 3 output is within about 5% of level 6
/// on real weather tiles and encodes about twice as fast, which matters
/// because compression is a large share of the time to produce a tile.
pub fn encode(tile: &Tile) -> Vec<u8> {
    encode_with(tile, 3)
}

/// Encode with an explicit zlib level (0..=9).
pub fn encode_with(tile: &Tile, level: u32) -> Vec<u8> {
    let w = tile.width as usize;
    let h = tile.height as usize;
    let n = w * h;
    let mut raw = vec![0u8; tile.bands.len() * n * 2];
    for (band, out) in tile.bands.iter().zip(raw.chunks_exact_mut(n * 2)) {
        let (lo, hi) = out.split_at_mut(n);
        let mut row_start_prev = 0u16;
        for row in 0..h {
            let base = row * w;
            let mut prev = row_start_prev;
            if w > 0 {
                row_start_prev = band[base];
            }
            for col in 0..w {
                let v = band[base + col];
                let d = v.wrapping_sub(prev);
                prev = v;
                lo[base + col] = d as u8;
                hi[base + col] = (d >> 8) as u8;
            }
        }
    }

    let mut out = Vec::with_capacity(HEADER_LEN + raw.len() / 4);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&tile.width.to_le_bytes());
    out.extend_from_slice(&tile.height.to_le_bytes());
    out.push(tile.bands.len() as u8);
    out.push(COMPRESSION_ZLIB);
    out.push(PREDICTOR_DELTA_PLANES);
    out.push(0);
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    let mut enc = flate2::write::ZlibEncoder::new(out, flate2::Compression::new(level.min(9)));
    enc.write_all(&raw).expect("writing to a Vec cannot fail");
    enc.finish().expect("writing to a Vec cannot fail")
}

pub fn decode(bytes: &[u8]) -> Result<Tile, TileError> {
    if bytes.len() < HEADER_LEN {
        return Err(TileError::Truncated);
    }
    if bytes[0..4] != MAGIC {
        return Err(TileError::BadMagic);
    }
    let width = u16::from_le_bytes([bytes[4], bytes[5]]);
    let height = u16::from_le_bytes([bytes[6], bytes[7]]);
    let bands = bytes[8] as usize;
    let compression = bytes[9];
    let predictor = bytes[10];
    let raw_len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    if bands == 0 || bands > MAX_BANDS {
        return Err(TileError::BadHeader("band count"));
    }
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(TileError::BadHeader("dimensions"));
    }
    let n = width as usize * height as usize;
    if raw_len != bands * n * 2 {
        return Err(TileError::BadHeader("payload length"));
    }

    let payload = &bytes[HEADER_LEN..];
    let raw = match compression {
        COMPRESSION_STORED => {
            if payload.len() != raw_len {
                return Err(TileError::Corrupt);
            }
            payload.to_vec()
        }
        COMPRESSION_ZLIB => {
            // Grown as data actually arrives, not reserved from the header.
            let mut raw = Vec::with_capacity(raw_len.min(1 << 20));
            // `take` bounds the output so a hostile tile cannot inflate without limit.
            flate2::read::ZlibDecoder::new(payload)
                .take(raw_len as u64 + 1)
                .read_to_end(&mut raw)
                .map_err(|_| TileError::Corrupt)?;
            if raw.len() != raw_len {
                return Err(TileError::Corrupt);
            }
            raw
        }
        other => return Err(TileError::Unsupported("compression", other)),
    };

    let w = width as usize;
    let h = height as usize;
    let mut out = Vec::with_capacity(bands);
    for chunk in raw.chunks_exact(n * 2) {
        let mut band = vec![0u16; n];
        match predictor {
            PREDICTOR_NONE => {
                for (v, b) in band.iter_mut().zip(chunk.as_chunks::<2>().0) {
                    *v = u16::from_le_bytes(*b);
                }
            }
            PREDICTOR_DELTA_PLANES => {
                let (lo, hi) = chunk.split_at(n);
                let mut row_start_prev = 0u16;
                for row in 0..h {
                    let base = row * w;
                    let mut prev = row_start_prev;
                    for col in 0..w {
                        let d = u16::from_le_bytes([lo[base + col], hi[base + col]]);
                        prev = prev.wrapping_add(d);
                        band[base + col] = prev;
                    }
                    if w > 0 {
                        row_start_prev = band[base];
                    }
                }
            }
            other => return Err(TileError::Unsupported("predictor", other)),
        }
        out.push(band);
    }
    Ok(Tile {
        width,
        height,
        bands: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(bands: usize, w: u16, h: u16) -> Tile {
        let n = w as usize * h as usize;
        let bands = (0..bands)
            .map(|b| {
                (0..n)
                    .map(|i| {
                        let x = (i % w as usize) as f32;
                        let y = (i / w as usize) as f32;
                        if ((x + y) as usize).is_multiple_of(17) {
                            NODATA
                        } else {
                            (30000.0 + 900.0 * ((x * 0.05).sin() + (y * 0.03 + b as f32).cos()))
                                as u16
                        }
                    })
                    .collect()
            })
            .collect();
        Tile::new(w, h, bands)
    }

    #[test]
    fn round_trips() {
        for (bands, w, h) in [(1, 256, 256), (3, 256, 256), (2, 7, 5), (1, 1, 1)] {
            let tile = sample(bands, w, h);
            let bytes = encode(&tile);
            assert_eq!(decode(&bytes).unwrap(), tile);
        }
    }

    #[test]
    fn round_trips_extremes() {
        let tile = Tile::new(4, 2, vec![vec![0, 65535, 0, 65535, 1, 65534, 32768, 0]]);
        assert_eq!(decode(&encode(&tile)).unwrap(), tile);
    }

    #[test]
    fn smooth_fields_compress_well() {
        let tile = sample(1, 256, 256);
        let bytes = encode(&tile);
        assert!(
            bytes.len() < 256 * 256 * 2 / 2,
            "expected at least 2x compression, got {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn empty_tile_is_tiny_and_flagged() {
        let tile = Tile::new(256, 256, vec![vec![NODATA; 256 * 256]]);
        assert!(tile.is_empty());
        assert!(encode(&tile).len() < 400);
    }

    #[test]
    fn header_layout_is_stable() {
        let bytes = encode(&Tile::new(2, 1, vec![vec![5, 9]]));
        assert_eq!(&bytes[0..4], b"WXT1");
        assert_eq!(&bytes[4..12], &[2, 0, 1, 0, 1, 1, 1, 0]);
        assert_eq!(&bytes[12..16], &4u32.to_le_bytes());
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(decode(b"WXT").unwrap_err(), TileError::Truncated);
        assert_eq!(decode(&[0u8; 32]).unwrap_err(), TileError::BadMagic);
        let mut bytes = encode(&sample(1, 8, 8));
        bytes.truncate(bytes.len() - 3);
        assert_eq!(decode(&bytes).unwrap_err(), TileError::Corrupt);
        for (offset, value) in [(4, 0u8), (6, 0), (5, 0x20), (7, 0xff)] {
            let mut bytes = encode(&sample(1, 8, 8));
            bytes[offset] = value;
            if offset == 4 || offset == 6 {
                bytes[offset + 1] = 0;
            }
            assert_eq!(
                decode(&bytes).unwrap_err(),
                TileError::BadHeader("dimensions"),
                "offset {offset}"
            );
        }
        let mut bytes = encode(&sample(1, 8, 8));
        bytes[12] = 3;
        assert_eq!(
            decode(&bytes).unwrap_err(),
            TileError::BadHeader("payload length")
        );
    }
}
