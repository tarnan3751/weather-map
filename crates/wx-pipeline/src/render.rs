//! Cutting a stack of parts into WXT1 tiles.
//!
//! Every tile sample is a pure function of its own lon/lat, the zoom and the
//! parts with weight there:
//!
//! * Sample longitudes depend only on the column and latitudes only on the
//!   row, and both are computed the same way in every tile that shares the
//!   sample. 180 E is taken as 180 W, so the tiles either side of the
//!   antimeridian see one longitude.
//! * Each part's projection is split into per-column and per-row terms
//!   (`wx_core::proj::Projection`), combined per sample: exact, and the same
//!   arithmetic in every tile.
//! * Which pyramid level a part is read from is chosen per sample row from
//!   the latitude (for lat/lon, Mercator, Lambert and polar stereographic
//!   grids the ground footprint of a tile cell, in source cells, depends on
//!   nothing else; for rotated and geostationary grids the choice made at
//!   the grid's central longitude is used everywhere, a close approximation
//!   for the first and blurrier than necessary towards a satellite's limb),
//!   never per tile.
//! * Parts are visited top first and the blend stops once a part has full
//!   weight, so parts below it, present in one tile's stack and absent from
//!   its neighbour's, never enter a shared sample.
//! * Where no part has weight (the outermost block of the lowest part's
//!   data, which coverage weights leave at 0), the topmost part with a value
//!   there gives the sample. Whether a part has a value at a point depends on
//!   the point alone, and a part that has one is inside its own bounds, so
//!   every tile whose stack could hold it does.
//!
//! * A rate band paired with a type band ([`Pair`]) is reconciled per
//!   sample from what that sample's parts give, nothing else.
//!
//! So the samples two neighbouring tiles share come out bit-identical.

use rayon::prelude::*;
use wx_core::TileCoord;
use wx_core::layer::{BandKind, TYPED_RATE_MIN_MMH, ptype};
use wx_core::proj::{TILE_CELLS, TILE_SAMPLES, Terms, lat_to_y, lon_to_x, x_to_lon, y_to_lat};
use wx_tile::{MAX_BANDS, NODATA, Tile};

use crate::FrameGrid;
use crate::grid::{BandMeta, Level};

const N: usize = TILE_SAMPLES;

/// One longitude for the antimeridian.
#[inline]
fn canonical_lon(lon: f64) -> f64 {
    if lon >= 180.0 { lon - 360.0 } else { lon }
}

/// A part's stored sample in another part's quantization: `q * mul + add`.
#[derive(Debug, Clone, Copy)]
struct Requantize {
    mul: f64,
    add: f64,
}

impl Requantize {
    fn new(from: &BandMeta, to: &BandMeta) -> Self {
        let mul = f64::from(from.scale) / f64::from(to.scale);
        let add = (f64::from(from.offset) - f64::from(to.offset)) / f64::from(to.scale) + 1.0 - mul;
        Requantize { mul, add }
    }

    #[inline]
    fn apply(self, q: f64) -> f64 {
        if self.mul == 1.0 && self.add == 0.0 {
            q
        } else {
            q * self.mul + self.add
        }
    }
}

/// A stored sample of `v` in a band's quantization (`BandSpec::quantize`).
fn stored(meta: &BandMeta, v: f32) -> u16 {
    (((v - meta.offset) / meta.scale).round() + 1.0).clamp(1.0, 65535.0) as u16
}

/// A layer that pairs one continuous band with one categorical band: in the
/// registry those are the precipitation-type layers, a rate in mm/h and a
/// `ptype` code (a test holds the registry to it). Their derive rules give
/// type NONE below [`TYPED_RATE_MIN_MMH`]; interpolating the rate and taking
/// the type from the nearest sample would break that between samples (a rate
/// that reaches the threshold next to a sample of NONE draws nothing), so
/// the compositor applies the same rule to its output.
#[derive(Debug, Clone, Copy)]
struct Pair {
    rate: usize,
    kind: usize,
    /// The threshold and NONE as output samples.
    rate_min: u16,
    none: u16,
}

impl Pair {
    fn of(out: &[BandMeta]) -> Option<Pair> {
        let only = |k: BandKind| {
            let mut it = out.iter().enumerate().filter(|(_, b)| b.kind == k);
            match (it.next(), it.next()) {
                (Some((i, _)), None) => Some(i),
                _ => None,
            }
        };
        let (rate, kind) = (only(BandKind::Continuous)?, only(BandKind::Categorical)?);
        (out.len() == 2).then(|| Pair {
            rate,
            kind,
            rate_min: stored(&out[rate], TYPED_RATE_MIN_MMH),
            none: stored(&out[kind], ptype::NONE),
        })
    }

    /// The type to show with output rate `rate`: NONE below the threshold;
    /// at or above it, `kind`, or `typed` (a nearby sample that has a type)
    /// where `kind` is NONE.
    fn resolve(&self, rate: u16, kind: u16, typed: f64) -> u16 {
        if rate == NODATA || kind == NODATA {
            kind
        } else if rate < self.rate_min {
            self.none
        } else if kind == self.none && !typed.is_nan() {
            to_sample(typed)
        } else {
            kind
        }
    }
}

/// One part as seen by one tile.
struct View<'a> {
    g: &'a FrameGrid,
    lon_t: Vec<Terms>,
    lat_t: Vec<Terms>,
    /// Pyramid level per row, `None` for rows outside the part's bounds.
    level: Vec<Option<usize>>,
    /// Columns inside the part's bounds.
    cols: Vec<bool>,
    requantize: Vec<Requantize>,
    /// The paired type band's NONE in this part's quantization.
    none: u16,
}

impl<'a> View<'a> {
    fn new(
        g: &'a FrameGrid,
        out: &[BandMeta],
        pair: Option<Pair>,
        lons: &[f64],
        lats: &[f64],
        z: u8,
    ) -> Self {
        let b = g.bounds;
        let in_lon = |lon: f64| {
            if b.wraps() {
                lon >= b.west || lon <= b.east
            } else {
                lon >= b.west && lon <= b.east
            }
        };
        View {
            g,
            lon_t: lons.iter().map(|&lon| g.proj.lon_terms(lon)).collect(),
            lat_t: lats.iter().map(|&lat| g.proj.lat_terms(lat)).collect(),
            level: lats
                .iter()
                .map(|&lat| (lat >= b.south && lat <= b.north).then(|| g.level_for(lat, z)))
                .collect(),
            cols: lons.iter().map(|&lon| in_lon(lon)).collect(),
            requantize: g
                .bands
                .iter()
                .zip(out)
                .map(|(from, to)| Requantize::new(from, to))
                .collect(),
            none: pair.map_or(NODATA, |p| stored(&g.bands[p.kind], ptype::NONE)),
        }
    }
}

/// Running sums for one sample, for a layer of `NB` bands.
struct Blend<const NB: usize> {
    acc: [f64; NB],
    wsum: [f64; NB],
    count: [u32; NB],
    last: [f64; NB],
    best_w: [f64; NB],
    best: [f64; NB],
    /// The topmost part's value, weighted or not: the sample where no part
    /// has weight.
    first: [f64; NB],
    /// For a paired type band: the nearby typed sample of the most-weighted
    /// part that has one, and of the part that gave `first`.
    typed_w: f64,
    typed: f64,
    first_typed: f64,
}

impl<const NB: usize> Blend<NB> {
    const EMPTY: Self = Blend {
        acc: [0.0; NB],
        wsum: [0.0; NB],
        count: [0; NB],
        last: [0.0; NB],
        best_w: [0.0; NB],
        best: [f64::NAN; NB],
        first: [f64::NAN; NB],
        typed_w: 0.0,
        typed: f64::NAN,
        first_typed: f64::NAN,
    };

    /// Whether some band has neither weight nor a fallback yet, so a part
    /// without weight still has to be read.
    fn wants_fallback(&self, kinds: &[BandKind; NB]) -> bool {
        (0..NB).any(|b| {
            let weighted = match kinds[b] {
                BandKind::Continuous => self.count[b] > 0,
                BandKind::Categorical => !self.best[b].is_nan(),
            };
            !weighted && self.first[b].is_nan()
        })
    }
}

#[inline]
fn to_sample(q: f64) -> u16 {
    q.round().clamp(1.0, 65535.0) as u16
}

pub(crate) fn render_stack(parts: &[&FrameGrid], coord: TileCoord) -> Option<Tile> {
    let first = parts.first()?;
    let out = &first.bands;
    let nb = out.len();
    for p in parts {
        assert!(
            p.bands.len() == nb && p.bands.iter().zip(out).all(|(a, b)| a.kind == b.kind),
            "parts of one stack must have the same bands"
        );
    }
    let tile_box = coord.bounds();
    let lons: Vec<f64> = (0..N)
        .map(|c| canonical_lon(coord.sample_lonlat(c, 0).0))
        .collect();
    let lats: Vec<f64> = (0..N).map(|r| coord.sample_lonlat(0, r).1).collect();
    let pair = Pair::of(out);
    let views: Vec<View> = parts
        .iter()
        .filter(|g| g.has_data() && g.bounds.intersects(&tile_box))
        .map(|g| View::new(g, out, pair, &lons, &lats, coord.z))
        .collect();
    if views.is_empty() {
        return None;
    }
    let mut bands = vec![vec![NODATA; N * N]; nb];
    // The band count as a constant, so a sample's sums stay in registers.
    match nb {
        1 => blend_rows::<1>(&mut bands, &views, out, pair),
        2 => blend_rows::<2>(&mut bands, &views, out, pair),
        3 => blend_rows::<3>(&mut bands, &views, out, pair),
        4 => blend_rows::<4>(&mut bands, &views, out, pair),
        5 => blend_rows::<5>(&mut bands, &views, out, pair),
        6 => blend_rows::<6>(&mut bands, &views, out, pair),
        7 => blend_rows::<7>(&mut bands, &views, out, pair),
        _ => blend_rows::<MAX_BANDS>(&mut bands, &views, out, pair),
    }
    let tile = Tile::new(N as u16, N as u16, bands);
    (!tile.is_empty()).then_some(tile)
}

/// Every sample of the tile into `bands` (`NB` planes of `N * N`, filled
/// with no-data), rows in parallel.
// A sample's column indexes every view's terms as well as the output row.
#[allow(clippy::needless_range_loop)]
fn blend_rows<const NB: usize>(
    bands: &mut [Vec<u16>],
    views: &[View],
    out: &[BandMeta],
    pair: Option<Pair>,
) {
    let kinds: [BandKind; NB] = std::array::from_fn(|b| out[b].kind);
    let mut planes: Vec<_> = bands.iter_mut().map(|b| b.chunks_mut(N)).collect();
    let rows: Vec<[&mut [u16]; NB]> = (0..N)
        .map(|_| std::array::from_fn(|b| planes[b].next().expect("N rows")))
        .collect();
    rows.into_par_iter().enumerate().for_each(|(r, row)| {
        let active: Vec<(&View, usize)> = views
            .iter()
            .filter_map(|v| v.level[r].map(|l| (v, l)))
            .collect();
        if active.is_empty() {
            return;
        }
        let mut vals = [f64::NAN; NB];
        for c in 0..N {
            let mut s = Blend::<NB>::EMPTY;
            // Share of the sample the parts so far leave to those below.
            let mut left = 1.0f64;
            for &(v, level) in &active {
                if !v.cols[c] {
                    continue;
                }
                let (i, j) = v.g.proj.ij_from_terms(&v.lon_t[c], &v.lat_t[r]);
                if !v.g.inside(i, j) {
                    continue;
                }
                let a = v.g.coverage.weight(i, j);
                if a <= 0.0 && !s.wants_fallback(&kinds) {
                    continue;
                }
                let w = a * left;
                v.g.sample_raw(i, j, level, &mut vals);
                if let Some(p) = pair {
                    let typed = v.g.sample_typed(i, j, level, p.kind, v.none);
                    let typed = if typed.is_nan() {
                        typed
                    } else {
                        v.requantize[p.kind].apply(typed)
                    };
                    if s.first[p.kind].is_nan() && !vals[p.kind].is_nan() {
                        s.first_typed = typed;
                    }
                    if a > 0.0 && !typed.is_nan() && w > s.typed_w {
                        s.typed_w = w;
                        s.typed = typed;
                    }
                }
                for b in 0..NB {
                    let q = vals[b];
                    if q.is_nan() {
                        continue;
                    }
                    let q = v.requantize[b].apply(q);
                    if s.first[b].is_nan() {
                        s.first[b] = q;
                    }
                    if a <= 0.0 {
                        continue;
                    }
                    match kinds[b] {
                        BandKind::Continuous => {
                            s.acc[b] += w * q;
                            s.wsum[b] += w;
                            s.count[b] += 1;
                            s.last[b] = q;
                        }
                        BandKind::Categorical => {
                            if w > s.best_w[b] {
                                s.best_w[b] = w;
                                s.best[b] = q;
                            }
                        }
                    }
                }
                if a > 0.0 {
                    left *= 1.0 - a;
                    if left <= 0.0 {
                        break;
                    }
                }
            }
            for b in 0..NB {
                row[b][c] = match kinds[b] {
                    // One part alone gives exactly its own sample.
                    BandKind::Continuous if s.count[b] == 1 => to_sample(s.last[b]),
                    BandKind::Continuous if s.count[b] > 1 => to_sample(s.acc[b] / s.wsum[b]),
                    BandKind::Categorical if !s.best[b].is_nan() => to_sample(s.best[b]),
                    // No weight anywhere: the topmost value, if any.
                    _ if s.first[b].is_nan() => NODATA,
                    _ => to_sample(s.first[b]),
                };
            }
            if let Some(p) = pair {
                let weighted = !s.best[p.kind].is_nan();
                let typed = if weighted { s.typed } else { s.first_typed };
                row[p.kind][c] = p.resolve(row[p.rate][c], row[p.kind][c], typed);
            }
        }
    });
}

impl FrameGrid {
    pub(crate) fn has_data(&self) -> bool {
        let base = &self.levels[0];
        base.nx > 0 && base.ny > 0
    }

    /// Whether a fractional level-0 index lies on the grid. Each sample
    /// stands for the cell around it, so the grid reaches half a sample past
    /// its outer samples. NaN fails these tests too.
    #[inline]
    pub(crate) fn inside(&self, i: f64, j: f64) -> bool {
        let base = &self.levels[0];
        let inside_j = j >= -0.5 && j <= base.ny as f64 - 0.5;
        let inside_i = if self.periodic {
            i.is_finite()
        } else {
            i >= -0.5 && i <= base.nx as f64 - 0.5
        };
        inside_i && inside_j
    }

    /// Pyramid level whose sample spacing best matches a tile cell at this
    /// latitude and zoom: `round(log2(source cells per tile cell))`.
    pub(crate) fn level_for(&self, lat: f64, z: u8) -> usize {
        let top = self.levels.len().saturating_sub(1);
        if top == 0 {
            return 0;
        }
        let d = 1.0 / ((1u64 << z) as f64 * TILE_CELLS as f64);
        let p = self.proj.lonlat_to_ij(self.lon_ref, lat);
        let east = self
            .proj
            .lonlat_to_ij(x_to_lon(lon_to_x(self.lon_ref) + d), lat);
        let south = self
            .proj
            .lonlat_to_ij(self.lon_ref, y_to_lat(lat_to_y(lat) + d));
        let nx = self.levels[0].nx as f64;
        let wrap = |di: f64| {
            if self.periodic {
                di - nx * (di / nx).round()
            } else {
                di
            }
        };
        let (ax, ay) = (wrap(east.0 - p.0), east.1 - p.1);
        let (bx, by) = (wrap(south.0 - p.0), south.1 - p.1);
        let footprint = (ax * by - ay * bx).abs().sqrt();
        if !footprint.is_finite() || footprint <= 0.0 {
            return 0;
        }
        (footprint.log2().round().max(0.0) as usize).min(top)
    }

    /// Every band's value at level-0 index `(i0, j0)` (on the grid) read
    /// from pyramid level `level`, as an unrounded stored sample: bilinear
    /// over the valid neighbours for continuous bands, the nearest sample
    /// for categorical ones; NaN where there is none.
    pub(crate) fn sample_raw(&self, i0: f64, j0: f64, level: usize, out: &mut [f64]) {
        let lv = &self.levels[level];
        let (x, y) = (
            self.axes.0.to_level(level, i0),
            self.axes.1.to_level(level, j0),
        );
        let mut stencil: Option<Stencil> = None;
        let mut nearest: Option<usize> = None;
        for ((q, meta), plane) in out.iter_mut().zip(&self.bands).zip(&lv.bands) {
            *q = match meta.kind {
                BandKind::Continuous => stencil
                    .get_or_insert_with(|| Stencil::new(lv, self.periodic, x, y))
                    .apply(plane),
                BandKind::Categorical => {
                    match plane
                        [*nearest.get_or_insert_with(|| nearest_index(lv, self.periodic, x, y))]
                    {
                        NODATA => f64::NAN,
                        v => f64::from(v),
                    }
                }
            };
        }
    }

    /// Band `b` (categorical) at the nearest of the bilinear neighbours of
    /// level-0 index `(i0, j0)` on `level` that holds a value other than
    /// `none` (a stored sample, nearest meaning the largest bilinear weight,
    /// the first in the stencil's order on a tie); NaN when none does.
    pub(crate) fn sample_typed(&self, i0: f64, j0: f64, level: usize, b: usize, none: u16) -> f64 {
        let lv = &self.levels[level];
        let (x, y) = (
            self.axes.0.to_level(level, i0),
            self.axes.1.to_level(level, j0),
        );
        let st = Stencil::new(lv, self.periodic, x, y);
        let plane = &lv.bands[b];
        let mut best: Option<(f64, u16)> = None;
        for k in 0..st.n {
            let q = plane[st.idx[k]];
            if q != NODATA && q != none && best.is_none_or(|(w, _)| st.w[k] > w) {
                best = Some((st.w[k], q));
            }
        }
        best.map_or(f64::NAN, |(_, q)| f64::from(q))
    }
}

/// Bilinear weights of the in-grid neighbours of a fractional index.
struct Stencil {
    idx: [usize; 4],
    w: [f64; 4],
    n: usize,
}

impl Stencil {
    fn new(lv: &Level, periodic: bool, x: f64, y: f64) -> Self {
        let (fx, fy) = (x.floor(), y.floor());
        let (tx, ty) = (x - fx, y - fy);
        let (x0, y0) = (fx as i64, fy as i64);
        let (nx, ny) = (lv.nx as i64, lv.ny as i64);
        let mut s = Stencil {
            idx: [0; 4],
            w: [0.0; 4],
            n: 0,
        };
        let corners = [
            (0, 0, (1.0 - tx) * (1.0 - ty)),
            (1, 0, tx * (1.0 - ty)),
            (0, 1, (1.0 - tx) * ty),
            (1, 1, tx * ty),
        ];
        // Away from the level's edges every corner is on it.
        if x0 >= 0 && y0 >= 0 && x0 + 1 < nx && y0 + 1 < ny {
            let at = (y0 * nx + x0) as usize;
            for (dx, dy, w) in corners {
                if w > 0.0 {
                    s.idx[s.n] = at + (dy * nx + dx) as usize;
                    s.w[s.n] = w;
                    s.n += 1;
                }
            }
            return s;
        }
        for (dx, dy, w) in corners {
            let (mut xx, yy) = (x0 + dx, y0 + dy);
            if w <= 0.0 || yy < 0 || yy >= ny {
                continue;
            }
            if periodic {
                xx = xx.rem_euclid(nx);
            } else if xx < 0 || xx >= nx {
                continue;
            }
            s.idx[s.n] = (yy * nx + xx) as usize;
            s.w[s.n] = w;
            s.n += 1;
        }
        s
    }

    /// Weighted mean of the valid neighbours; NaN when none is valid.
    #[inline]
    fn apply(&self, plane: &[u16]) -> f64 {
        let (mut acc, mut wsum) = (0.0f64, 0.0f64);
        for k in 0..self.n {
            let q = plane[self.idx[k]];
            if q != NODATA {
                acc += self.w[k] * f64::from(q);
                wsum += self.w[k];
            }
        }
        if wsum > 0.0 { acc / wsum } else { f64::NAN }
    }
}

fn nearest_index(lv: &Level, periodic: bool, x: f64, y: f64) -> usize {
    let (nx, ny) = (lv.nx as i64, lv.ny as i64);
    let yi = (y.round() as i64).clamp(0, ny - 1);
    let xi = x.round() as i64;
    let xi = if periodic {
        xi.rem_euclid(nx)
    } else {
        xi.clamp(0, nx - 1)
    };
    (yi * nx + xi) as usize
}

#[cfg(test)]
mod tests;
