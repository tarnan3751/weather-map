//! How much each point of a part counts when parts are stacked: its
//! coverage weight, and the solid box inside which that weight is 1.
//!
//! # The weight
//!
//! The level-0 grid is cut into square blocks of `block` cells. A block is
//! *full* when every band of every sample in it has a value and it lies
//! wholly inside the grid. Block corners are *nodes*; a node next to a block
//! that is not full is a *seed*. Each node stores its Euclidean distance to
//! the nearest seed, in nodes, as a fraction `s` of the ramp length
//! (quantized to 0..=255, saturating at 1). The weight at a point is
//! `smoothstep(s)` of the bilinear interpolation of its block's four
//! corners, except that four corners at 255 give exactly 1 and four at 0
//! exactly 0.
//!
//! So the weight is exactly 0 throughout any block holding a missing value
//! (all its corners are seeds) and beyond the grid's edge (the edge nodes
//! are seeds), rises continuously and monotonically with distance from
//! them, and is exactly 1 from `ramp` cells further in. The ramp is
//! [`RAMP_CELLS`] source cells, or [`RAMP_MIN_M`] on the ground if that is
//! more cells. Global grids have no east/west edge (blocks wrap) and a
//! lat/lon grid whose outer row is on a pole has no edge there.
//!
//! Blocks of about a third of the ramp keep the map small (an MRMS grid of
//! 24.5 million cells has 1.5 million nodes, one byte each), rebuilt from
//! level 0 wherever the grid is built or loaded, so a grid always has the
//! same weights. Data in a block that also holds missing values gets no
//! weight: coverage ends up to one block short of the last valid sample.
//! The compositor still shows that data where no part has weight, so only
//! where a part below has weight does it give way.

use rayon::prelude::*;
use wx_core::{GridProjection, LonLatBox};
use wx_tile::NODATA;

use crate::grid::Level;

/// The ramp reaches full weight this many source cells in from the edge...
pub(crate) const RAMP_CELLS: f64 = 12.0;
/// ...or this many metres, whichever is more cells.
pub(crate) const RAMP_MIN_M: f64 = 15_000.0;
/// Node values are this fraction of the ramp.
const FULL: u8 = 255;
/// The solid box must be at least this wide and tall to be worth having.
pub(crate) const MIN_SOLID_DEG: f64 = 0.25;
/// Raster cells per side searched for the solid box.
const SOLID_RASTER: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Coverage {
    /// Source cells per block side.
    block: usize,
    /// Blocks per row and block rows. Nodes are `(bx + 1) x (by + 1)`.
    bx: usize,
    by: usize,
    periodic: bool,
    /// The ends of the rows (`j < 0`, `j >= ny`) that are poles.
    open: (bool, bool),
    /// Ramp position per node, row-major; for periodic grids node column
    /// `bx` repeats column 0.
    nodes: Vec<u8>,
    /// The lon/lat box of the blocks holding any value, when that is not
    /// every block (see [`data_bounds`]).
    data_bounds: Option<LonLatBox>,
}

/// Ramp length in source cells for a grid of this spacing.
pub(crate) fn ramp_cells(proj: &GridProjection) -> f64 {
    let spacing = proj.nominal_spacing_m();
    if spacing.is_finite() && spacing > 0.0 {
        RAMP_CELLS.max(RAMP_MIN_M / spacing)
    } else {
        RAMP_CELLS
    }
}

/// Block side for a ramp: about a third of it, dividing the width of a
/// periodic grid so blocks wrap exactly.
fn block_side(ramp: f64, nx: usize, periodic: bool) -> usize {
    let mut b = ((ramp / 3.0).floor() as usize).max(1);
    if periodic {
        while b > 1 && !nx.is_multiple_of(b) {
            b -= 1;
        }
    }
    b
}

/// For each end of the rows (`j < 0`, `j >= ny`), whether it is a pole of a
/// lat/lon grid, where there is nothing further to be missing.
fn open_ends(proj: &GridProjection, ny: usize) -> (bool, bool) {
    match proj {
        GridProjection::LatLon(g) => {
            let at_pole = |edge: f64| edge.abs() >= 90.0 - 1e-9;
            (
                at_pole(g.lat0 - 0.5 * g.dlat),
                at_pole(g.lat0 + (ny as f64 - 0.5) * g.dlat),
            )
        }
        _ => (false, false),
    }
}

impl Coverage {
    pub(crate) fn build(proj: &GridProjection, base: &Level) -> Coverage {
        let (nx, ny) = (base.nx, base.ny);
        let periodic = proj.is_periodic();
        if nx == 0 || ny == 0 {
            return Coverage {
                block: 1,
                bx: 0,
                by: 0,
                periodic,
                open: (false, false),
                nodes: Vec::new(),
                data_bounds: None,
            };
        }
        let ramp = ramp_cells(proj);
        let b = block_side(ramp, nx, periodic);
        let (bx, by) = (nx.div_ceil(b), ny.div_ceil(b));
        let (open_top, open_bottom) = open_ends(proj, ny);

        // Which blocks are full, with a border of one block all round: the
        // ends of the rows are full at a pole, the sides wrap on a periodic
        // grid, and everything else beyond the edge is not full.
        let pw = bx + 2;
        let mut full = vec![0u8; pw * (by + 2)];
        // Blocks holding any value at all, for the data's envelope.
        let mut has = vec![0u8; bx * by];
        full[..pw].fill(u8::from(open_top));
        full[(by + 1) * pw..].fill(u8::from(open_bottom));
        full[pw..(by + 1) * pw]
            .par_chunks_mut(pw)
            .zip(has.par_chunks_mut(bx))
            .enumerate()
            .for_each(|(jb, (row, has_row))| {
                // The least and the greatest sample in each column of the
                // block row, over every band: missing data is 0, so a column
                // misses a value where its least is 0 and has one where its
                // greatest is not.
                const _: () = assert!(NODATA == 0);
                let mut least = vec![u16::MAX; nx];
                let mut greatest = vec![0u16; nx];
                for j in jb * b..((jb + 1) * b).min(ny) {
                    for plane in &base.bands {
                        let line = &plane[j * nx..(j + 1) * nx];
                        for ((lo, hi), &q) in least.iter_mut().zip(greatest.iter_mut()).zip(line) {
                            *lo = (*lo).min(q);
                            *hi = (*hi).max(q);
                        }
                    }
                }
                for (h, chunk) in has_row.iter_mut().zip(greatest.chunks(b)) {
                    *h = u8::from(chunk.iter().any(|&q| q != NODATA));
                }
                let inner = &mut row[1..=bx];
                if (jb + 1) * b > ny && !open_bottom {
                    return;
                }
                for (ok, chunk) in inner.iter_mut().zip(least.chunks(b)) {
                    *ok = u8::from(chunk.iter().all(|&q| q != NODATA));
                }
                if !periodic && !nx.is_multiple_of(b) {
                    inner[bx - 1] = 0;
                }
                if periodic {
                    row[0] = row[bx];
                    row[bx + 1] = row[1];
                }
            });

        // Seeds: nodes next to a block that is not full. Node (i, j) touches
        // bordered blocks (i..=i+1, j..=j+1).
        let (wx, wy) = (bx + 1, by + 1);
        let mut seeds = vec![0u8; wx * wy];
        seeds.par_chunks_mut(wx).enumerate().for_each(|(jn, out)| {
            let (above, below) = (
                &full[jn * pw..(jn + 1) * pw],
                &full[(jn + 1) * pw..(jn + 2) * pw],
            );
            for (i, s) in out.iter_mut().enumerate() {
                *s = u8::from(above[i] & above[i + 1] & below[i] & below[i + 1] == 0);
            }
        });
        let ramp_nodes = ramp / b as f64;
        let cap = ramp_nodes.ceil() as u16 + 1;
        // Distance along each row to the nearest seed, capped.
        let mut g = vec![0u16; wx * wy];
        g.par_chunks_mut(wx)
            .zip(seeds.par_chunks(wx))
            .for_each(|(out, seed)| {
                row_distance(out, periodic, cap, seed);
            });
        // Up the columns: d^2 = min over nearby rows of g^2 + dj^2, both at
        // most `cap`, so the node value comes from a table.
        let cap = usize::from(cap);
        let value: Vec<u8> = (0..=2 * cap * cap)
            .map(|d2| ((d2 as f64).sqrt() / ramp_nodes).min(1.0))
            .map(|s| (s * f64::from(FULL)).round() as u8)
            .collect();
        let mut nodes = vec![0u8; wx * wy];
        nodes.par_chunks_mut(wx).enumerate().for_each(|(jn, out)| {
            // Rows beyond `cap` cannot be nearer than `cap` itself.
            let mut best = vec![(cap * cap) as u16; wx];
            let lo = jn.saturating_sub(cap);
            let hi = (jn + cap).min(wy - 1);
            for jj in lo..=hi {
                let dj2 = (jj.abs_diff(jn) * jj.abs_diff(jn)) as u16;
                let row = &g[jj * wx..(jj + 1) * wx];
                for (d, &gi) in best.iter_mut().zip(row) {
                    *d = (*d).min(gi * gi + dj2);
                }
            }
            for (o, d2) in out.iter_mut().zip(best) {
                *o = value[usize::from(d2)];
            }
        });
        let data_bounds = data_bounds(proj, nx, ny, b, &has);
        Coverage {
            block: b,
            bx,
            by,
            periodic,
            open: (open_top, open_bottom),
            nodes,
            data_bounds,
        }
    }

    /// The lon/lat box of the cells holding any value, `None` when every
    /// block holds one (or none does): then the grid's own box is the
    /// answer.
    pub(crate) fn data_bounds(&self) -> Option<LonLatBox> {
        self.data_bounds
    }

    /// Weight at fractional level-0 index `(i, j)`, which the caller has
    /// checked lies on the grid (`-0.5 <= j <= ny - 0.5`, and the same for
    /// `i` unless periodic).
    #[inline]
    pub(crate) fn weight(&self, i: f64, j: f64) -> f64 {
        if self.bx == 0 {
            return 0.0;
        }
        let b = self.block as f64;
        let u = (i + 0.5) / b;
        let v = (j + 0.5) / b;
        let (ib, tx) = if self.periodic {
            let f = u.floor();
            ((f as i64).rem_euclid(self.bx as i64) as usize, u - f)
        } else {
            let ib = (u.floor().max(0.0) as usize).min(self.bx - 1);
            (ib, (u - ib as f64).clamp(0.0, 1.0))
        };
        let jb = (v.floor().max(0.0) as usize).min(self.by - 1);
        let ty = (v - jb as f64).clamp(0.0, 1.0);
        let w = self.bx + 1;
        let k = jb * w + ib;
        let (q00, q10, q01, q11) = (
            self.nodes[k],
            self.nodes[k + 1],
            self.nodes[k + w],
            self.nodes[k + w + 1],
        );
        if q00 == FULL && q10 == FULL && q01 == FULL && q11 == FULL {
            return 1.0;
        }
        if q00 == 0 && q10 == 0 && q01 == 0 && q11 == 0 {
            return 0.0;
        }
        let top = f64::from(q00) + tx * (f64::from(q10) - f64::from(q00));
        let bottom = f64::from(q01) + tx * (f64::from(q11) - f64::from(q01));
        let s = ((top + ty * (bottom - top)) / f64::from(FULL)).clamp(0.0, 1.0);
        s * s * (3.0 - 2.0 * s)
    }

    pub(crate) fn approx_bytes(&self) -> usize {
        self.nodes.capacity()
    }

    /// A lon/lat box everywhere inside which [`Coverage::weight`] is exactly
    /// 1, as large as a raster search finds; `None` when none is at least
    /// [`MIN_SOLID_DEG`] on each side.
    ///
    /// The grid's bounds are cut into a `SOLID_RASTER`-square raster of
    /// lon/lat cells. A cell is good when the index-space box of nine points
    /// on it (corners, edge midpoints, centre), widened by a margin covering
    /// how far its curved image can stray from them (eight times the largest
    /// second difference of those points, at least a quarter node), holds
    /// only blocks whose four corners are full. The answer is the largest
    /// rectangle of good cells.
    pub(crate) fn solid(
        &self,
        proj: &GridProjection,
        bounds: &LonLatBox,
        nx: usize,
        ny: usize,
    ) -> Option<LonLatBox> {
        if self.bx == 0 || bounds.south >= bounds.north {
            return None;
        }
        let w = self.bx + 1;
        // Blocks with weight exactly 1 throughout, as a summed-area table.
        let solid = |ib: usize, jb: usize| {
            let k = jb * w + ib;
            self.nodes[k] == FULL
                && self.nodes[k + 1] == FULL
                && self.nodes[k + w] == FULL
                && self.nodes[k + w + 1] == FULL
        };
        let (bx, by) = (self.bx, self.by);
        let mut sat = vec![0u32; (bx + 1) * (by + 1)];
        for jb in 0..by {
            let mut run = 0u32;
            for ib in 0..bx {
                run += u32::from(solid(ib, jb));
                sat[(jb + 1) * (bx + 1) + ib + 1] = sat[jb * (bx + 1) + ib + 1] + run;
            }
        }
        let count = |i0: usize, i1: usize, j0: usize, j1: usize| -> u32 {
            // Blocks i0..i1, j0..j1 (exclusive ends).
            let at = |i: usize, j: usize| sat[j * (bx + 1) + i];
            at(i1, j1) + at(i0, j0) - at(i0, j1) - at(i1, j0)
        };
        let all_solid = |u0: f64, u1: f64, v0: f64, v1: f64| -> bool {
            // Nothing lies beyond a pole, so a margin reaching past one
            // reaches nothing.
            let v0 = if self.open.0 { v0.max(0.0) } else { v0 };
            let v1 = if self.open.1 {
                v1.min(by as f64 - 1e-9)
            } else {
                v1
            };
            let (j0, j1) = (v0.floor(), v1.floor());
            if !(j0 >= 0.0 && j1 < by as f64) {
                return false;
            }
            let (j0, j1) = (j0 as usize, j1 as usize + 1);
            let (i0, i1) = (u0.floor(), u1.floor());
            if self.periodic {
                if i1 - i0 + 1.0 >= bx as f64 {
                    return count(0, bx, j0, j1) == (bx * (j1 - j0)) as u32;
                }
                let a = (i0 as i64).rem_euclid(bx as i64) as usize;
                let n = (i1 - i0) as usize + 1;
                let first = n.min(bx - a);
                let ok = count(a, a + first, j0, j1) == (first * (j1 - j0)) as u32;
                ok && (first == n
                    || count(0, n - first, j0, j1) == ((n - first) * (j1 - j0)) as u32)
            } else {
                if !(i0 >= 0.0 && i1 < bx as f64) {
                    return false;
                }
                let (i0, i1) = (i0 as usize, i1 as usize + 1);
                count(i0, i1, j0, j1) == ((i1 - i0) * (j1 - j0)) as u32
            }
        };

        let (west, east) = if bounds.west == -180.0 && bounds.east == 180.0 {
            (-180.0, 180.0)
        } else if bounds.wraps() {
            (bounds.west, bounds.east + 360.0)
        } else {
            (bounds.west, bounds.east)
        };
        let (south, north) = (bounds.south, bounds.north);
        let n = SOLID_RASTER;
        let (dlon, dlat) = ((east - west) / n as f64, (north - south) / n as f64);
        // Points at half-cell steps; row 0 at the south.
        let lon_t: Vec<_> = (0..=2 * n)
            .map(|k| proj.lon_terms(west + k as f64 * 0.5 * dlon))
            .collect();
        let lat_t: Vec<_> = (0..=2 * n)
            .map(|k| proj.lat_terms(south + k as f64 * 0.5 * dlat))
            .collect();
        let b = self.block as f64;
        let (fx, fy) = (nx as f64, ny as f64);
        let periodic = self.periodic;
        let uv: Vec<(f64, f64)> = lat_t
            .par_iter()
            .flat_map_iter(|lt| {
                lon_t.iter().map(move |ln| {
                    let (i, j) = proj.ij_from_terms(ln, lt);
                    let inside = j >= -0.5
                        && j <= fy - 0.5
                        && (if periodic {
                            i.is_finite()
                        } else {
                            i >= -0.5 && i <= fx - 0.5
                        });
                    if inside {
                        ((i + 0.5) / b, (j + 0.5) / b)
                    } else {
                        (f64::NAN, f64::NAN)
                    }
                })
            })
            .collect();
        let row_len = 2 * n + 1;
        let good: Vec<bool> = (0..n * n)
            .into_par_iter()
            .map(|cell| {
                let (c, r) = (cell % n, cell / n);
                let mut p = [(0.0, 0.0); 9];
                for (k, q) in p.iter_mut().enumerate() {
                    *q = uv[(2 * r + k / 3) * row_len + 2 * c + k % 3];
                }
                if p.iter().any(|q| !(q.0.is_finite() && q.1.is_finite())) {
                    return false;
                }
                if periodic {
                    let uc = p[4].0;
                    let span = bx as f64;
                    for q in &mut p {
                        q.0 -= span * ((q.0 - uc) / span).round();
                    }
                }
                let mut sd: f64 = 0.0;
                for k in 0..3 {
                    for (a, m, z) in [
                        (p[3 * k], p[3 * k + 1], p[3 * k + 2]),
                        (p[k], p[k + 3], p[k + 6]),
                    ] {
                        sd = sd
                            .max((a.0 - 2.0 * m.0 + z.0).abs())
                            .max((a.1 - 2.0 * m.1 + z.1).abs());
                    }
                }
                let margin = (8.0 * sd).max(0.25);
                let (mut u0, mut u1, mut v0, mut v1) = (
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                );
                for q in &p {
                    u0 = u0.min(q.0);
                    u1 = u1.max(q.0);
                    v0 = v0.min(q.1);
                    v1 = v1.max(q.1);
                }
                all_solid(u0 - margin, u1 + margin, v0 - margin, v1 + margin)
            })
            .collect();

        let (c0, r0, c1, r1) = largest_rectangle(&good, n, n)?;
        let s = LonLatBox::new(
            west + c0 as f64 * dlon,
            south + r0 as f64 * dlat,
            west + (c1 + 1) as f64 * dlon,
            south + (r1 + 1) as f64 * dlat,
        );
        if s.east - s.west < MIN_SOLID_DEG || s.north - s.south < MIN_SOLID_DEG {
            return None;
        }
        if s.east - s.west >= 360.0 - 1e-9 {
            return Some(LonLatBox::new(-180.0, s.south, 180.0, s.north));
        }
        let fold = |lon: f64| if lon > 180.0 { lon - 360.0 } else { lon };
        Some(LonLatBox::new(fold(s.west), s.south, fold(s.east), s.north))
    }
}

/// Added all round to the data's box, beyond what its sampling allows for.
const DATA_MARGIN_DEG: f64 = 1e-4;

/// The lon/lat box enclosing every block of `has` (`bx` by `by` blocks of
/// `b` cells, row-major) that holds a value; `None` when every block or no
/// block does, or when the data reaches past where the projection has
/// positions (a satellite's limb), so that the grid's own box applies.
///
/// The extremes of a region without a pole are on its boundary: each block
/// with data next to one without (or next to the grid's edge) is sampled at
/// its corners, edge midpoints and centre, and the box grown by half the
/// largest extent of such a block, which is more than the boundary can
/// stray between samples. Longitudes are the complement of the widest gap
/// between the samples (all of them when no gap is wider than one block),
/// so the box crosses the antimeridian when the data does. A pole inside a
/// block with data takes in every longitude.
fn data_bounds(
    proj: &GridProjection,
    nx: usize,
    ny: usize,
    b: usize,
    has: &[u8],
) -> Option<LonLatBox> {
    let (bx, by) = (nx.div_ceil(b), ny.div_ceil(b));
    let with = has.iter().filter(|&&h| h != 0).count();
    if with == 0 || with == has.len() {
        return None;
    }
    let periodic = proj.is_periodic();
    let at = |bi: i64, bj: i64| -> bool {
        if bj < 0 || bj >= by as i64 {
            return false;
        }
        let bi = if periodic {
            bi.rem_euclid(bx as i64)
        } else if bi < 0 || bi >= bx as i64 {
            return false;
        } else {
            bi
        };
        has[bj as usize * bx + bi as usize] != 0
    };
    struct Acc {
        lons: Vec<f64>,
        south: f64,
        north: f64,
        lat_spread: f64,
        lon_spread: f64,
        off_earth: bool,
    }
    let merge = |mut a: Acc, b: Acc| {
        a.lons.extend(b.lons);
        Acc {
            lons: a.lons,
            south: a.south.min(b.south),
            north: a.north.max(b.north),
            lat_spread: a.lat_spread.max(b.lat_spread),
            lon_spread: a.lon_spread.max(b.lon_spread),
            off_earth: a.off_earth || b.off_earth,
        }
    };
    let empty = || Acc {
        lons: Vec::new(),
        south: f64::INFINITY,
        north: f64::NEG_INFINITY,
        lat_spread: 0.0,
        lon_spread: 0.0,
        off_earth: false,
    };
    let acc = (0..by)
        .into_par_iter()
        .fold(empty, |mut acc, bj| {
            for bi in 0..bx {
                let (i, j) = (bi as i64, bj as i64);
                if !at(i, j) || (at(i - 1, j) && at(i + 1, j) && at(i, j - 1) && at(i, j + 1)) {
                    continue;
                }
                // The block's cells, edge to edge.
                let (i0, i1) = (bi * b, ((bi + 1) * b).min(nx));
                let (j0, j1) = (bj * b, ((bj + 1) * b).min(ny));
                let is = [
                    i0 as f64 - 0.5,
                    0.5 * (i0 + i1) as f64 - 0.5,
                    i1 as f64 - 0.5,
                ];
                let js = [
                    j0 as f64 - 0.5,
                    0.5 * (j0 + j1) as f64 - 0.5,
                    j1 as f64 - 0.5,
                ];
                let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
                let (mut dlo, mut dhi) = (0.0f64, 0.0f64);
                let mut first: Option<f64> = None;
                for &ii in &is {
                    for &jj in &js {
                        let (lon, lat) = proj.ij_to_lonlat(ii, jj);
                        if !(lon.is_finite() && lat.is_finite()) {
                            acc.off_earth = true;
                            continue;
                        }
                        let lon = wx_core::proj::wrap_lon(lon);
                        acc.lons.push(lon);
                        lo = lo.min(lat);
                        hi = hi.max(lat);
                        let l0 = *first.get_or_insert(lon);
                        let d = (lon - l0 + 180.0).rem_euclid(360.0) - 180.0;
                        dlo = dlo.min(d);
                        dhi = dhi.max(d);
                    }
                }
                acc.south = acc.south.min(lo);
                acc.north = acc.north.max(hi);
                acc.lat_spread = acc.lat_spread.max(hi - lo);
                acc.lon_spread = acc.lon_spread.max(dhi - dlo);
            }
            acc
        })
        .reduce(empty, merge);
    if acc.off_earth || acc.lons.is_empty() {
        return None;
    }
    let pole = |lat: f64| {
        let (i, j) = proj.lonlat_to_ij(0.0, lat);
        let (ri, rj) = (i.round(), j.round());
        let on = (periodic || (ri >= 0.0 && ri < nx as f64)) && rj >= 0.0 && rj < ny as f64;
        on && i.is_finite() && at((ri as i64).div_euclid(b as i64), rj as i64 / b as i64)
    };
    let (north_pole, south_pole) = (pole(90.0), pole(-90.0));
    let grow_lat = 0.5 * acc.lat_spread + DATA_MARGIN_DEG;
    let south = if south_pole {
        -90.0
    } else {
        (acc.south - grow_lat).max(-90.0)
    };
    let north = if north_pole {
        90.0
    } else {
        (acc.north + grow_lat).min(90.0)
    };
    let grow_lon = 0.5 * acc.lon_spread + DATA_MARGIN_DEG;
    let mut lons = acc.lons;
    lons.sort_by(f64::total_cmp);
    let n = lons.len();
    let (mut widest, mut after) = (lons[0] + 360.0 - lons[n - 1], 0);
    for k in 0..n - 1 {
        let gap = lons[k + 1] - lons[k];
        if gap > widest {
            widest = gap;
            after = k + 1;
        }
    }
    if north_pole || south_pole || widest <= acc.lon_spread + 2.0 * grow_lon {
        return Some(LonLatBox::new(-180.0, south, 180.0, north));
    }
    // The data runs from the sample after the widest gap round to the one
    // before it.
    let west = lons[after] - grow_lon;
    let east = lons[(after + n - 1) % n] + grow_lon;
    // Back into [-180, 180] from just beyond it.
    let fold = |lon: f64| {
        if lon > 180.0 {
            lon - 360.0
        } else if lon < -180.0 {
            lon + 360.0
        } else {
            lon
        }
    };
    Some(LonLatBox::new(fold(west), south, fold(east), north))
}

/// Distances along a row of nodes to the nearest seed (non-zero in
/// `seed`), capped at `cap`; cyclic when `periodic` (the last node is the
/// first again).
fn row_distance(row: &mut [u16], periodic: bool, cap: u16, seed: &[u8]) {
    let n = row.len();
    let m = if periodic { n - 1 } else { n };
    let (d, seed) = (&mut row[..m], &seed[..m]);
    let step = |run: u16, s: u8| if s != 0 { 0 } else { (run + 1).min(cap) };
    // On a cyclic row, once round first carries distances across the seam.
    let mut run = cap;
    if periodic {
        run = seed.iter().fold(run, |r, &s| step(r, s));
    }
    for (di, &s) in d.iter_mut().zip(seed) {
        run = step(run, s);
        *di = run;
    }
    run = cap;
    if periodic {
        run = seed.iter().rev().fold(run, |r, &s| step(r, s));
    }
    for (di, &s) in d.iter_mut().zip(seed).rev() {
        run = step(run, s);
        *di = (*di).min(run);
    }
    if periodic {
        row[n - 1] = row[0];
    }
}

/// Largest-area rectangle of `true` cells in a `w` x `h` row-major grid, as
/// inclusive `(c0, r0, c1, r1)`.
fn largest_rectangle(good: &[bool], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let mut heights = vec![0usize; w];
    let mut best: Option<(usize, (usize, usize, usize, usize))> = None;
    let mut stack: Vec<usize> = Vec::with_capacity(w + 1);
    for r in 0..h {
        for c in 0..w {
            heights[c] = if good[r * w + c] { heights[c] + 1 } else { 0 };
        }
        stack.clear();
        for c in 0..=w {
            let hc = if c < w { heights[c] } else { 0 };
            while let Some(&top) = stack.last() {
                if heights[top] <= hc {
                    break;
                }
                stack.pop();
                let height = heights[top];
                let left = stack.last().map_or(0, |&l| l + 1);
                let area = height * (c - left);
                if best.is_none_or(|(a, _)| area > a) {
                    best = Some((area, (left, r + 1 - height, c - 1, r)));
                }
            }
            stack.push(c);
        }
    }
    best.map(|(_, rect)| rect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wx_core::proj::LatLonGrid;

    fn latlon(nx: usize, ny: usize, periodic: bool) -> GridProjection {
        GridProjection::LatLon(LatLonGrid {
            lon0: if periodic { 0.0 } else { -100.0 },
            lat0: if periodic { 90.0 } else { 50.0 },
            dlon: if periodic { 360.0 / nx as f64 } else { 0.01 },
            dlat: if periodic {
                -180.0 / (ny - 1) as f64
            } else {
                -0.01
            },
            periodic,
        })
    }

    #[test]
    fn row_distances() {
        let mut d = [0u16; 10];
        row_distance(&mut d, false, 4, &[0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(d, [2, 1, 0, 1, 2, 3, 4, 4, 4, 4]);
        // Cyclic: the seed reaches round the seam (node 9 is node 0).
        row_distance(&mut d, true, 6, &[0, 0, 0, 0, 0, 0, 0, 1, 0, 0]);
        assert_eq!(d, [2, 3, 4, 4, 3, 2, 1, 0, 1, 2]);
    }

    #[test]
    fn rectangles() {
        #[rustfmt::skip]
        let g = [
            false, true,  true,  false,
            true,  true,  true,  true,
            false, true,  true,  true,
        ];
        assert_eq!(largest_rectangle(&g, 4, 3), Some((1, 0, 2, 2)));
        assert_eq!(largest_rectangle(&[false; 4], 2, 2), None);
    }

    #[test]
    fn blocks_follow_the_ramp() {
        // 1 km: the 15 km floor wins; 3 km: 12 cells.
        assert_eq!(block_side(15.0, 1000, false), 5);
        assert_eq!(block_side(12.0, 1000, false), 4);
        // A periodic grid needs blocks that divide its width.
        assert_eq!(block_side(12.0, 1442, true), 2);
        assert!(
            (ramp_cells(&latlon(100, 100, false))
                - 15_000.0 / (0.01f64.to_radians() * 6_371_229.0))
                .abs()
                < 1e-9
        );
        let (top, bottom) = open_ends(&latlon(1440, 721, true), 721);
        assert!(top && bottom);
        assert_eq!(open_ends(&latlon(100, 100, false), 100), (false, false));
    }
}
