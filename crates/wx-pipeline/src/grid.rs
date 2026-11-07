//! The quantized source grid and its overview pyramid.
//!
//! Level 0 is the source grid. Level `k + 1` halves level `k` in both
//! directions: its sample `(I, J)` summarises level-`k` samples `2I..=2I+1`
//! by `2J..=2J+1` and so sits at their centre, `(2I + 0.5, 2J + 0.5)` in
//! level-`k` indices. Unrolled, level-`k` sample `I` sits at level-0 index
//! `I * 2^k + (2^k - 1) / 2`; see [`Level::from_base`]. Rather than giving each
//! level its own projection (whose origin would have to move by that half
//! sample), every lookup goes through the level-0 projection and this affine
//! map, which is exact.
//!
//! Except at the far end of an axis whose length was odd at some level:
//! there the last sample summarises fewer samples than a full block, so it
//! sits at the mean of its inputs' positions, not at the block's centre
//! ([`Axis`]), and between it and the sample before, the map is linear in
//! the true positions.

use rayon::prelude::*;
use wx_core::layer::{Aggregate, BandKind, BandSpec};
use wx_tile::NODATA;

/// Coarser levels stop once the grid fits in this many samples per side:
/// a zoom-0 tile cell is about 150 km, which even a 1 km grid reaches by then.
pub(crate) const MIN_OVERVIEW_SIDE: usize = 64;
pub(crate) const MAX_LEVELS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BandMeta {
    pub kind: BandKind,
    pub aggregate: Aggregate,
    pub scale: f32,
    pub offset: f32,
}

impl From<&BandSpec> for BandMeta {
    fn from(b: &BandSpec) -> Self {
        Self {
            kind: b.kind,
            aggregate: b.aggregate,
            scale: b.scale,
            offset: b.offset,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Level {
    pub nx: usize,
    pub ny: usize,
    /// One `nx * ny` plane per band, row-major, `NODATA` where missing.
    pub bands: Vec<Vec<u16>>,
}

impl Level {
    /// Level-`k` fractional index of a level-0 index.
    #[inline]
    pub fn from_base(k: usize, i0: f64) -> f64 {
        let s = (1u64 << k) as f64;
        (i0 - 0.5 * (s - 1.0)) / s
    }
}

/// Where one axis's samples sit at each level, in level-0 indices: the
/// affine positions above, but for the last sample of each level, which is
/// the mean of its inputs' positions (a partial block's single input, or a
/// full block whose second input was itself partial). Positions are dyadic
/// fractions, so all of this is exact in f64.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Axis {
    /// Per level: its length and the position of its last sample.
    levels: Vec<(usize, f64)>,
    /// Per level, the constants [`Axis::to_level`] works with.
    maps: Vec<AxisMap>,
}

#[derive(Debug, Clone, PartialEq)]
struct AxisMap {
    /// `0.5 * (2^k - 1)` and `2^-k`: the affine map is `(i0 - half) * inv`,
    /// which is exactly `Level::from_base`, the scaling being by a power of 2.
    half: f64,
    inv: f64,
    n: usize,
    /// When the last sample is off the affine map: the level-0 positions of
    /// the last two samples.
    tail: Option<(f64, f64)>,
}

/// Level-0 position of level-`k` sample `i` when it is a full block.
fn affine_position(k: usize, i: usize) -> f64 {
    let s = (1u64 << k) as f64;
    i as f64 * s + 0.5 * (s - 1.0)
}

impl Axis {
    /// For the level lengths `sizes`, finest (level 0) first.
    pub(crate) fn new(sizes: impl IntoIterator<Item = usize>) -> Axis {
        let mut levels: Vec<(usize, f64)> = Vec::new();
        for n in sizes {
            let last = match levels.last() {
                None => n.saturating_sub(1) as f64,
                Some(&(prev, prev_last)) => {
                    let k = levels.len() - 1;
                    if prev.is_multiple_of(2) && prev >= 2 {
                        0.5 * (affine_position(k, prev - 2) + prev_last)
                    } else {
                        prev_last
                    }
                }
            };
            levels.push((n, last));
        }
        let maps = levels
            .iter()
            .enumerate()
            .map(|(k, &(n, last))| {
                let s = (1u64 << k) as f64;
                let off = n >= 2 && last != affine_position(k, n - 1);
                AxisMap {
                    half: 0.5 * (s - 1.0),
                    inv: 1.0 / s,
                    n,
                    tail: off.then(|| (affine_position(k, n - 2), last)),
                }
            })
            .collect();
        Axis { levels, maps }
    }

    /// Level-`k` fractional index of level-0 index `i0`.
    #[inline]
    pub(crate) fn to_level(&self, k: usize, i0: f64) -> f64 {
        let Some(m) = self.maps.get(k) else {
            return Level::from_base(k, i0);
        };
        let x = (i0 - m.half) * m.inv;
        let Some((before, last)) = m.tail else {
            return x;
        };
        if i0 <= before {
            x
        } else if i0 <= last {
            (m.n - 2) as f64 + (i0 - before) / (last - before)
        } else {
            (m.n - 1) as f64 + (i0 - last) * m.inv
        }
    }
}

pub(crate) fn quantize(spec: &BandSpec, values: &[f32]) -> Vec<u16> {
    const CHUNK: usize = 1 << 14;
    let mut out = vec![NODATA; values.len()];
    out.par_chunks_mut(CHUNK)
        .zip(values.par_chunks(CHUNK))
        .for_each(|(out, values)| {
            for (q, &v) in out.iter_mut().zip(values) {
                *q = spec.quantize(v);
            }
        });
    out
}

/// Whether a level is worth halving again. Periodic grids stop at an odd
/// width, because a halved level must still wrap exactly onto itself.
pub(crate) fn reducible(nx: usize, ny: usize, periodic: bool) -> bool {
    nx.max(ny) > MIN_OVERVIEW_SIDE && nx >= 2 && ny >= 2 && !(periodic && !nx.is_multiple_of(2))
}

/// Sizes of every level, finest first, as [`overviews`] (after level 0) makes them.
pub(crate) fn level_sizes(nx: usize, ny: usize, periodic: bool) -> Vec<(usize, usize)> {
    let mut out = vec![(nx, ny)];
    let (mut w, mut h) = (nx, ny);
    while out.len() < MAX_LEVELS && reducible(w, h, periodic) {
        w = w.div_ceil(2);
        h = h.div_ceil(2);
        out.push((w, h));
    }
    out
}

#[cfg(test)]
pub(crate) fn build_pyramid(base: Level, bands: &[BandMeta], periodic: bool) -> Vec<Level> {
    let overviews = overviews(&base, bands, periodic);
    let mut levels = Vec::with_capacity(1 + overviews.len());
    levels.push(base);
    levels.extend(overviews);
    levels
}

/// Every level below `base`, finest first.
pub(crate) fn overviews(base: &Level, bands: &[BandMeta], periodic: bool) -> Vec<Level> {
    let sizes = level_sizes(base.nx, base.ny, periodic);
    let mut out: Vec<Level> = Vec::with_capacity(sizes.len().saturating_sub(1));
    for (step, &size) in sizes.iter().enumerate().skip(1) {
        let next = reduce(out.last().unwrap_or(base), bands, step - 1);
        debug_assert_eq!((next.nx, next.ny), size);
        out.push(next);
    }
    out
}

/// Halves `src`. `step` is the level being reduced, which alternates the
/// corner `Nearest` prefers so the chosen sample drifts by about a sixth of a
/// cell over many levels instead of half a cell toward the top-left.
pub(crate) fn reduce(src: &Level, bands: &[BandMeta], step: usize) -> Level {
    let (nx, ny) = (src.nx.div_ceil(2), src.ny.div_ceil(2));
    let out = src
        .bands
        .iter()
        .zip(bands)
        .map(|(plane, meta)| {
            let mut dst = vec![NODATA; nx * ny];
            match meta.aggregate {
                Aggregate::Max => halve(src, plane, &mut dst, max4),
                Aggregate::Mean => halve(src, plane, &mut dst, mean4),
                // The four inputs are equidistant from the output's centre.
                Aggregate::Nearest if step.is_multiple_of(2) => {
                    halve(src, plane, &mut dst, |q| first_present(q, [3, 0, 1, 2]))
                }
                Aggregate::Nearest => {
                    halve(src, plane, &mut dst, |q| first_present(q, [0, 3, 2, 1]))
                }
            }
            dst
        })
        .collect();
    Level { nx, ny, bands: out }
}

/// Each sample of `dst` (the halved level, row-major) is `f` of its block of
/// `plane`: top-left, top-right, bottom-left, bottom-right. The inputs a
/// block at an odd far edge lacks are missing data.
fn halve(src: &Level, plane: &[u16], dst: &mut [u16], f: impl Fn([u16; 4]) -> u16 + Sync) {
    let (w, h) = (src.nx, src.ny);
    let pairs = w / 2;
    dst.par_chunks_mut(w.div_ceil(2))
        .enumerate()
        .for_each(|(j, row)| {
            let top = &plane[2 * j * w..(2 * j + 1) * w];
            let bottom = (2 * j + 1 < h).then(|| &plane[(2 * j + 1) * w..(2 * j + 2) * w]);
            match bottom {
                Some(bottom) => {
                    let blocks = top.as_chunks::<2>().0.iter().zip(bottom.as_chunks::<2>().0);
                    for (out, (&[a, b], &[c, d])) in row.iter_mut().zip(blocks) {
                        *out = f([a, b, c, d]);
                    }
                    if w % 2 == 1 {
                        row[pairs] = f([top[w - 1], NODATA, bottom[w - 1], NODATA]);
                    }
                }
                None => {
                    for (out, &[a, b]) in row.iter_mut().zip(top.as_chunks::<2>().0) {
                        *out = f([a, b, NODATA, NODATA]);
                    }
                    if w % 2 == 1 {
                        row[pairs] = f([top[w - 1], NODATA, NODATA, NODATA]);
                    }
                }
            }
        });
}

// `mean4` and `max4` rely on missing data being the smallest sample, 0.
const _: () = assert!(NODATA == 0);

/// The rounded mean of the samples present; missing data adds nothing to the
/// sum.
#[inline]
fn mean4(q: [u16; 4]) -> u16 {
    let sum: u32 = q.iter().map(|&v| u32::from(v)).sum();
    let n = q.iter().filter(|&&v| v != NODATA).count() as u32;
    (sum + n / 2).checked_div(n).map_or(NODATA, |m| m as u16)
}

#[inline]
fn max4(q: [u16; 4]) -> u16 {
    q.into_iter().max().unwrap_or(NODATA)
}

/// The first sample present, taking the inputs in `order`.
#[inline]
fn first_present(q: [u16; 4], order: [usize; 4]) -> u16 {
    order
        .into_iter()
        .map(|k| q[k])
        .find(|&v| v != NODATA)
        .unwrap_or(NODATA)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(aggregate: Aggregate) -> BandMeta {
        BandMeta {
            kind: if aggregate == Aggregate::Nearest {
                BandKind::Categorical
            } else {
                BandKind::Continuous
            },
            aggregate,
            scale: 1.0,
            offset: 0.0,
        }
    }

    fn level(nx: usize, ny: usize, v: Vec<u16>) -> Level {
        Level {
            nx,
            ny,
            bands: vec![v],
        }
    }

    #[test]
    fn mean_ignores_missing() {
        let src = level(4, 2, vec![10, 20, 0, 0, 30, 0, 0, 7]);
        let r = reduce(&src, &[meta(Aggregate::Mean)], 0);
        assert_eq!((r.nx, r.ny), (2, 1));
        assert_eq!(r.bands[0], vec![20, 7]);
        let empty = level(2, 2, vec![0; 4]);
        assert_eq!(
            reduce(&empty, &[meta(Aggregate::Mean)], 0).bands[0],
            vec![NODATA]
        );
    }

    #[test]
    fn max_and_nearest() {
        let src = level(4, 2, vec![10, 20, 0, 0, 30, 0, 0, 7]);
        assert_eq!(
            reduce(&src, &[meta(Aggregate::Max)], 0).bands[0],
            vec![30, 7]
        );
        // Even steps prefer the bottom-right input, odd steps the top-left.
        let src = level(2, 2, vec![1, 2, 3, 4]);
        assert_eq!(
            reduce(&src, &[meta(Aggregate::Nearest)], 0).bands[0],
            vec![4]
        );
        assert_eq!(
            reduce(&src, &[meta(Aggregate::Nearest)], 1).bands[0],
            vec![1]
        );
        let holes = level(2, 2, vec![0, 2, 3, 0]);
        let v = reduce(&holes, &[meta(Aggregate::Nearest)], 0).bands[0][0];
        assert!(
            v == 2 || v == 3,
            "a missing preferred input falls back to a valid one"
        );
        assert_eq!(
            reduce(&level(2, 2, vec![0; 4]), &[meta(Aggregate::Nearest)], 0).bands[0],
            vec![0]
        );
    }

    /// The straightforward reduction `reduce` must agree with: each output
    /// sample from its up to four inputs, missing ones skipped.
    fn reference(src: &Level, how: Aggregate, step: usize) -> Vec<u16> {
        let (nx, ny) = (src.nx.div_ceil(2), src.ny.div_ceil(2));
        let plane = &src.bands[0];
        let at = |i: usize, j: usize| {
            if i < src.nx && j < src.ny {
                plane[j * src.nx + i]
            } else {
                NODATA
            }
        };
        let mut out = Vec::new();
        for j in 0..ny {
            for i in 0..nx {
                let q = [
                    at(2 * i, 2 * j),
                    at(2 * i + 1, 2 * j),
                    at(2 * i, 2 * j + 1),
                    at(2 * i + 1, 2 * j + 1),
                ];
                let present: Vec<u16> = q.iter().copied().filter(|&v| v != NODATA).collect();
                out.push(match how {
                    Aggregate::Max => present.iter().copied().max().unwrap_or(NODATA),
                    Aggregate::Mean if present.is_empty() => NODATA,
                    Aggregate::Mean => {
                        let n = present.len() as u32;
                        ((present.iter().map(|&v| u32::from(v)).sum::<u32>() + n / 2) / n) as u16
                    }
                    Aggregate::Nearest => {
                        let order = if step.is_multiple_of(2) {
                            [3, 0, 1, 2]
                        } else {
                            [0, 3, 2, 1]
                        };
                        order
                            .iter()
                            .map(|&k| q[k])
                            .find(|&v| v != NODATA)
                            .unwrap_or(NODATA)
                    }
                });
            }
        }
        out
    }

    #[test]
    fn reduce_matches_the_reference() {
        let mut seed = 99u64;
        for (nx, ny) in [(1, 1), (2, 3), (5, 4), (7, 7), (64, 33), (131, 70)] {
            let v: Vec<u16> = (0..nx * ny)
                .map(|_| {
                    seed = seed
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    let r = (seed >> 33) as u16;
                    if r.is_multiple_of(5) { NODATA } else { r | 1 }
                })
                .collect();
            let src = level(nx, ny, v);
            for how in [Aggregate::Mean, Aggregate::Max, Aggregate::Nearest] {
                for step in 0..2 {
                    assert_eq!(
                        reduce(&src, &[meta(how)], step).bands[0],
                        reference(&src, how, step),
                        "{nx} x {ny}, {how:?}, step {step}"
                    );
                }
            }
        }
    }

    #[test]
    fn odd_sizes_keep_the_edge() {
        let src = level(3, 3, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let r = reduce(&src, &[meta(Aggregate::Mean)], 0);
        assert_eq!((r.nx, r.ny), (2, 2));
        // (1+2+4+5)/4 = 3, (3+6)/2 = 4.5 rounds up, (7+8)/2 = 7.5, 9.
        assert_eq!(r.bands[0], vec![3, 5, 8, 9]);
    }

    #[test]
    fn pyramid_sizes() {
        assert_eq!(level_sizes(7000, 3500, false).last(), Some(&(55, 28)));
        assert_eq!(level_sizes(7000, 3500, false).len(), 8);
        assert_eq!(level_sizes(1799, 1059, false).len(), 6);
        // GFS halves to 45 columns and stops: 45 does not halve onto itself.
        let gfs = level_sizes(1440, 721, true);
        assert_eq!(gfs.last(), Some(&(45, 23)));
        assert_eq!(level_sizes(10, 10, false), vec![(10, 10)]);
    }

    #[test]
    fn last_samples_sit_at_their_inputs_mean() {
        // 7 columns: level 1 holds (0,1), (2,3), (4,5), (6); level 2
        // (0..3), (4..6).
        let a = Axis::new([7, 4, 2]);
        assert_eq!(a.levels, vec![(7, 6.0), (4, 6.0), (2, 0.5 * (4.5 + 6.0))]);
        // Even lengths all the way: the affine positions.
        let even = Axis::new([8, 4, 2]);
        assert_eq!(even.levels, vec![(8, 7.0), (4, 6.5), (2, 5.5)]);
        for i0 in [0.0, 3.3, 6.5, 7.0, 7.4] {
            for k in 0..3 {
                assert_eq!(even.to_level(k, i0), Level::from_base(k, i0));
            }
        }
        // On level 1 of the 7: sample 2 at 4.5, sample 3 at 6.
        assert_eq!(a.to_level(1, 4.5), 2.0);
        assert_eq!(a.to_level(1, 5.25), 2.5);
        assert_eq!(a.to_level(1, 6.0), 3.0);
        assert_eq!(a.to_level(1, 6.5), 3.25);
        assert_eq!(a.to_level(1, 2.5), Level::from_base(1, 2.5));
    }

    /// A ramp along an odd number of columns: every level, read at the
    /// level-0 position, gives the ramp's value there up to the far edge.
    #[test]
    fn odd_lengths_keep_their_last_samples_in_place() {
        let (nx, ny) = (515, 16);
        let base = level(
            nx,
            ny,
            (0..ny * nx).map(|k| (2 * (k % nx) + 1000) as u16).collect(),
        );
        let levels = build_pyramid(base, &[meta(Aggregate::Mean)], false);
        let axis = Axis::new(levels.iter().map(|l| l.nx));
        assert_eq!(levels.len(), 5);
        for (k, l) in levels.iter().enumerate().skip(1) {
            let row = &l.bands[0][..l.nx];
            let mut i0 = (nx - 1) as f64 - 2.0 * (1u64 << k) as f64;
            while i0 <= (nx - 1) as f64 {
                let x = axis.to_level(k, i0);
                let (x0, t) = (x.floor() as usize, x - x.floor());
                let v = if x0 + 1 < l.nx {
                    f64::from(row[x0]) * (1.0 - t) + f64::from(row[x0 + 1]) * t
                } else {
                    f64::from(row[x0])
                };
                let want = 2.0 * i0 + 1000.0;
                assert!(
                    (v - want).abs() <= 1.0 + 0.5 * k as f64,
                    "level {k} at {i0}: {v} vs {want}"
                );
                i0 += 0.25;
            }
        }
    }

    #[test]
    fn half_sample_shift() {
        // A linear ramp: level-k samples must sit where their mean says.
        let nx = 512;
        let base = level(
            nx,
            8,
            (0..8 * nx).map(|k| (2 * (k % nx) + 1000) as u16).collect(),
        );
        let levels = build_pyramid(base, &[meta(Aggregate::Mean)], false);
        assert_eq!(levels.len(), 4);
        for (k, l) in levels.iter().enumerate().skip(1) {
            for (i, &q) in l.bands[0][..l.nx].iter().enumerate() {
                // Inverse of `from_base`: level-0 index of level-k sample i.
                let s = (1u64 << k) as f64;
                let i0 = i as f64 * s + 0.5 * (s - 1.0);
                let want = 2.0 * i0 + 1000.0;
                assert!(
                    (f64::from(q) - want).abs() <= 1.0,
                    "level {k} sample {i}: {q} vs {want}"
                );
                assert!((Level::from_base(k, i0) - i as f64).abs() < 1e-12);
            }
        }
    }
}
