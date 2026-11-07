//! The `.wxg` object: a [`FrameGrid`] at rest.
//!
//! ```text
//! header (little-endian):
//!   magic "WXG1", version u16 (2), reserved u16 (0)
//!   nx u32, ny u32, bands u8, stored_levels u8, projection u8, parameters u8
//!   that many projection parameters, f64 each, as
//!     `GridProjection::to_params` gives them (flags as 0.0 or 1.0)
//!   per band: kind u8, aggregate u8, scale f32, offset f32
//!   crc32 of everything above, u32
//! body, for each stored level then each band:
//!   rows_per_chunk u32, chunks u32, compressed length u32 per chunk,
//!   then the chunks
//! ```
//!
//! A chunk is a run of whole rows: every sample replaced by its wrapping
//! difference from its left neighbour (a row's first sample from the first
//! sample of the row above), split into a plane of low bytes and a plane of
//! high bytes, then zstd-compressed with a checksum. Chunks compress and
//! decompress in parallel. With `stored_levels == 1` only the full-resolution
//! grid is stored and the overviews are rebuilt on load. Measured on an MRMS
//! frame (10 cores): 1.35 MB and 5.9 ms to load that way, against 1.99 MB and
//! 4.1 ms with the overviews stored; fetching the extra 0.64 MB costs more
//! than the 1.8 ms it saves, so `to_bytes` rebuilds. Both forms load. The
//! coverage weights are always rebuilt from level 0, which is what makes a
//! loaded grid's weights those of the grid that was stored.
//!
//! Version 1 (two projections, parameters in a fixed layout) is refused.

use anyhow::{Context, Result, anyhow, bail, ensure};
use rayon::prelude::*;
use wx_core::GridProjection;
use wx_core::layer::{Aggregate, BandKind};

use crate::FrameGrid;
use crate::grid::{self, BandMeta, Level};

const MAGIC: &[u8; 4] = b"WXG1";
const VERSION: u16 = 2;
/// Raw bytes per chunk: small enough to spread over every core, large
/// enough that zstd's ratio does not suffer.
const CHUNK_BYTES: usize = 1 << 20;
pub(crate) const ZSTD_LEVEL: i32 = 3;

pub(crate) fn to_bytes(g: &FrameGrid, store_overviews: bool, zstd_level: i32) -> Vec<u8> {
    let stored = if store_overviews { g.levels.len() } else { 1 };
    let base = &g.levels[0];
    let mut w = Writer(Vec::new());
    w.0.extend_from_slice(MAGIC);
    w.u16(VERSION);
    w.u16(0);
    w.u32(base.nx as u32);
    w.u32(base.ny as u32);
    w.u8(g.bands.len() as u8);
    w.u8(stored as u8);
    let (kind, params) = g.proj.to_params();
    w.u8(kind);
    w.u8(u8::try_from(params.len()).expect("a projection has few parameters"));
    for v in params {
        w.f64(v);
    }
    for b in &g.bands {
        w.u8(match b.kind {
            BandKind::Continuous => 0,
            BandKind::Categorical => 1,
        });
        w.u8(match b.aggregate {
            Aggregate::Mean => 0,
            Aggregate::Max => 1,
            Aggregate::Nearest => 2,
        });
        w.f32(b.scale);
        w.f32(b.offset);
    }
    let mut crc = flate2::Crc::new();
    crc.update(&w.0);
    w.u32(crc.sum());

    struct Job<'a> {
        plane: &'a [u16],
        nx: usize,
        rows: std::ops::Range<usize>,
    }
    let mut jobs = Vec::new();
    let mut layout = Vec::new();
    for level in &g.levels[..stored] {
        let rows = rows_per_chunk(level.nx);
        for plane in &level.bands {
            let chunks = chunk_count(level.nx, level.ny, rows);
            layout.push((rows, chunks));
            for c in 0..chunks {
                jobs.push(Job {
                    plane,
                    nx: level.nx,
                    rows: c * rows..((c + 1) * rows).min(level.ny),
                });
            }
        }
    }
    let packed: Vec<Vec<u8>> = jobs
        .par_iter()
        .map_init(
            || {
                let mut c = zstd::bulk::Compressor::new(zstd_level).expect("zstd context");
                c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))
                    .expect("zstd checksum flag");
                c
            },
            |c, job| {
                let raw = pack(job.plane, job.nx, job.rows.clone());
                c.compress(&raw)
                    .expect("zstd compression of a buffer in memory")
            },
        )
        .collect();
    let mut chunks = packed.iter();
    for (rows, n) in layout {
        w.u32(rows as u32);
        w.u32(n as u32);
        let these: Vec<&Vec<u8>> = chunks.by_ref().take(n).collect();
        for c in &these {
            w.u32(c.len() as u32);
        }
        for c in these {
            w.0.extend_from_slice(c);
        }
    }
    w.0
}

pub(crate) fn from_bytes(bytes: &[u8]) -> Result<FrameGrid> {
    let mut r = Reader { b: bytes, pos: 0 };
    ensure!(r.take(4)? == MAGIC, "not a .wxg grid (bad magic)");
    let version = r.u16()?;
    ensure!(version == VERSION, "unsupported .wxg version {version}");
    ensure!(r.u16()? == 0, "unknown .wxg flags");
    let nx = r.u32()? as usize;
    let ny = r.u32()? as usize;
    // Checked before anything is sized from them; the checksum only proves
    // the header is intact, not that it is ours.
    ensure!(
        nx as u64 * ny as u64 <= crate::MAX_GRID_POINTS as u64,
        "grid of {nx} x {ny} points is larger than any feed produces"
    );
    let nb = r.u8()? as usize;
    let stored = r.u8()? as usize;
    let kind = r.u8()?;
    let count = r.u8()?;
    let params = (0..count).map(|_| r.f64()).collect::<Result<Vec<_>>>()?;
    let proj = GridProjection::from_params(kind, &params).ok_or_else(|| {
        anyhow!("projection {kind} with parameters {params:?} is not one we write")
    })?;
    ensure!(
        (1..=wx_tile::MAX_BANDS).contains(&nb),
        "bad band count {nb}"
    );
    let mut bands = Vec::with_capacity(nb);
    for _ in 0..nb {
        let kind = match r.u8()? {
            0 => BandKind::Continuous,
            1 => BandKind::Categorical,
            v => bail!("bad band kind {v}"),
        };
        let aggregate = match r.u8()? {
            0 => Aggregate::Mean,
            1 => Aggregate::Max,
            2 => Aggregate::Nearest,
            v => bail!("bad aggregate {v}"),
        };
        let (scale, offset) = (r.f32()?, r.f32()?);
        ensure!(
            scale.is_finite() && scale > 0.0 && offset.is_finite(),
            "bad band quantization"
        );
        bands.push(BandMeta {
            kind,
            aggregate,
            scale,
            offset,
        });
    }
    let header_len = r.pos;
    let mut crc = flate2::Crc::new();
    crc.update(&bytes[..header_len]);
    ensure!(
        r.u32()? == crc.sum(),
        ".wxg header is corrupt (checksum mismatch)"
    );

    let periodic = proj.is_periodic();
    let sizes = grid::level_sizes(nx, ny, periodic);
    ensure!(
        stored == 1 || stored == sizes.len(),
        "{stored} stored levels, expected 1 or {}",
        sizes.len()
    );

    // Read every chunk table first, so nothing is allocated for a file that
    // is cut short. Planes come level by level, bands within a level.
    let mut planes: Vec<(usize, Vec<&[u8]>)> = Vec::with_capacity(stored * nb);
    for (k, &(w, h)) in sizes[..stored].iter().enumerate() {
        for b in 0..nb {
            let rows = r.u32()? as usize;
            let n = r.u32()? as usize;
            ensure!(
                rows >= 1 && n == chunk_count(w, h, rows),
                "level {k} band {b}: bad chunk layout"
            );
            let lens = (0..n).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
            let chunks = lens
                .into_iter()
                .map(|len| r.take(len as usize))
                .collect::<Result<Vec<_>>>()?;
            planes.push((rows, chunks));
        }
    }
    ensure!(
        r.pos == bytes.len(),
        "{} unexpected bytes after the grid",
        bytes.len() - r.pos
    );

    let mut levels: Vec<Level> = sizes[..stored]
        .iter()
        .map(|&(w, h)| Level {
            nx: w,
            ny: h,
            bands: vec![vec![0u16; w * h]; nb],
        })
        .collect();
    // Each chunk fills its own disjoint run of rows.
    let mut targets: Vec<(&mut [u16], usize, &[u8])> = Vec::new();
    let mut planes = planes.into_iter();
    for level in levels.iter_mut() {
        let w = level.nx;
        for plane in level.bands.iter_mut() {
            let (rows, chunks) = planes.next().expect("one table per plane");
            if chunks.is_empty() {
                continue;
            }
            for (out, data) in plane.chunks_mut(rows * w).zip(chunks) {
                targets.push((out, w, data));
            }
        }
    }
    targets
        .into_par_iter()
        .map_init(
            || {
                let d = zstd::bulk::Decompressor::new().map_err(|e| anyhow!("zstd context: {e}"));
                (d, Vec::new())
            },
            |(d, buf), (out, w, data)| {
                let d = d.as_mut().map_err(|e| anyhow!("{e}"))?;
                let len = out.len() * 2;
                if buf.len() < len {
                    buf.resize(len, 0);
                }
                let raw = &mut buf[..len];
                let n = d
                    .decompress_to_buffer(data, &mut *raw)
                    .context(".wxg chunk is corrupt")?;
                ensure!(n == len, ".wxg chunk holds {n} bytes, expected {len}");
                unpack(raw, w, out);
                Ok(())
            },
        )
        .collect::<Result<()>>()?;

    if stored == 1 {
        let base = levels.pop().expect("one level");
        return Ok(FrameGrid::from_base(proj, bands, base));
    }
    let coverage = crate::coverage::Coverage::build(&proj, &levels[0]);
    Ok(FrameGrid::assemble(proj, bands, levels, coverage))
}

/// Chunks of `rows` rows in a `w` x `h` plane; an empty plane has none.
fn chunk_count(w: usize, h: usize, rows: usize) -> usize {
    if w == 0 { 0 } else { h.div_ceil(rows) }
}

fn rows_per_chunk(nx: usize) -> usize {
    (CHUNK_BYTES / (2 * nx.max(1))).max(1)
}

fn pack(plane: &[u16], nx: usize, rows: std::ops::Range<usize>) -> Vec<u8> {
    let n = rows.len() * nx;
    let mut buf = vec![0u8; 2 * n];
    let (lo, hi) = buf.split_at_mut(n);
    let mut above = 0u16;
    let lines = plane[rows.start * nx..rows.end * nx].chunks_exact(nx);
    for ((row, lo), hi) in lines
        .zip(lo.chunks_exact_mut(nx))
        .zip(hi.chunks_exact_mut(nx))
    {
        let mut prev = above;
        above = row[0];
        for ((&v, lo), hi) in row.iter().zip(lo).zip(hi) {
            let [l, h] = v.wrapping_sub(prev).to_le_bytes();
            (*lo, *hi) = (l, h);
            prev = v;
        }
    }
    buf
}

fn unpack(raw: &[u8], nx: usize, out: &mut [u16]) {
    let (lo, hi) = raw.split_at(out.len());
    let mut above = 0u16;
    for ((row, lo), hi) in out
        .chunks_exact_mut(nx)
        .zip(lo.chunks_exact(nx))
        .zip(hi.chunks_exact(nx))
    {
        let mut prev = above;
        for ((v, &l), &h) in row.iter_mut().zip(lo).zip(hi) {
            prev = prev.wrapping_add(u16::from_le_bytes([l, h]));
            *v = prev;
        }
        above = row[0];
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| anyhow!(".wxg grid is truncated at byte {}", self.pos))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn array<const K: usize>(&mut self) -> Result<[u8; K]> {
        Ok(self.take(K)?.try_into().expect("length checked"))
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.array()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.array()?))
    }
}
