// Renders one WXT1 tile from a stack of parts, sampling on the tile's
// 257 x 257 lattice exactly as the Rust tiler does: at each sample the top
// part with data there wins, fading into what is beneath across the edge of
// its coverage (the soft blend). Every part contributes its own source's
// synthetic weather at its own valid time and run, so the bytes depend on the
// whole stack.
// Erasable syntax only: worker threads load this directly.

import type { LayerSpec, Part, Provenance } from '../src/api/types.ts';
import { contains } from './boxes.ts';
import { encodeWxt } from './encode.ts';
import { feelsLike, prepareInfrared, prepareRadar, prepareSurface, ptypeFor, rainRate, type Quality } from './fields.ts';
import { xToLon, yToLat } from './geo.ts';
import { quantize } from './registry.ts';
import { sourceById } from './sources.ts';
import { HOUR } from './util.ts';

const SAMPLES = 257;

type Sampler = (lon: number, lat: number) => number[] | null;

/**
 * Evaluate `fn` on every `step`-th lattice point and fill the rest by
 * bilinear interpolation. Only for smooth, continuous fields: it is what makes
 * multi-day rainfall totals cheap enough to generate on request.
 */
function evaluate(lons: Float64Array, lats: Float64Array, bands: number, step: number, fn: Sampler): Float32Array[] {
  const N = SAMPLES;
  const out = Array.from({ length: bands }, () => new Float32Array(N * N).fill(Number.NaN));
  if (step <= 1) {
    for (let row = 0; row < N; row++) {
      for (let col = 0; col < N; col++) {
        const v = fn(lons[col], lats[row]);
        if (!v) continue;
        for (let b = 0; b < bands; b++) out[b][row * N + col] = v[b];
      }
    }
    return out;
  }
  const M = Math.floor((N - 1) / step) + 1;
  const coarse = Array.from({ length: bands }, () => new Float32Array(M * M).fill(Number.NaN));
  for (let j = 0; j < M; j++) {
    for (let i = 0; i < M; i++) {
      const v = fn(lons[i * step], lats[j * step]);
      if (!v) continue;
      for (let b = 0; b < bands; b++) coarse[b][j * M + i] = v[b];
    }
  }
  for (let row = 0; row < N; row++) {
    const fy = row / step;
    const j0 = Math.min(M - 2, Math.floor(fy));
    const ty = fy - j0;
    for (let col = 0; col < N; col++) {
      const fx = col / step;
      const i0 = Math.min(M - 2, Math.floor(fx));
      const tx = fx - i0;
      for (let b = 0; b < bands; b++) {
        const c = coarse[b];
        const a = c[j0 * M + i0];
        const bb = c[j0 * M + i0 + 1];
        const cc = c[(j0 + 1) * M + i0];
        const d = c[(j0 + 1) * M + i0 + 1];
        if (Number.isNaN(a) || Number.isNaN(bb) || Number.isNaN(cc) || Number.isNaN(d)) {
          // Near a coverage edge take the nearest coarse sample.
          const ni = Math.round(fx);
          const nj = Math.round(fy);
          out[b][row * N + col] = c[Math.min(M - 1, nj) * M + Math.min(M - 1, ni)];
          continue;
        }
        out[b][row * N + col] = (a * (1 - tx) + bb * tx) * (1 - ty) + (cc * (1 - tx) + d * tx) * ty;
      }
    }
  }
  return out;
}

/** Rain over the `hours` before `ms`, from radar every `stepHours`. */
function accumulation(ms: number, hours: number, stepHours: number, source: Quality) {
  const fields: ReturnType<typeof prepareRadar>[] = [];
  for (let k = 0; k < hours / stepHours; k++) fields.push(prepareRadar(ms - (k + 0.5) * stepHours * HOUR, source, { skipGeoref: true }));
  return (lon: number, lat: number) => {
    let mm = 0;
    for (const f of fields) mm += rainRate(f.dbz(lon, lat)) * stepHours;
    // The synthetic systems rain harder for longer than real ones; keep
    // multi-day totals in a believable range.
    return mm * 0.45;
  };
}

/** Rainfall totals: hours, hours per radar step, sampling step. */
const RAIN: Record<string, [number, number, number]> = { rain_1h: [1, 0.25, 2], rain_24h: [24, 1, 4], rain_72h: [72, 2, 4] };

const scalar = (step: number, f: (lon: number, lat: number) => number) => ({ step, fn: (lon: number, lat: number) => [f(lon, lat)] });

/** One part's weather: lon/lat to the layer's band values in stored units.
 *  `step` is how sparsely it may be evaluated (it is smooth enough to
 *  interpolate between). */
function sampler(layer: LayerSpec, part: Part, q: Quality): { fn: Sampler; step: number } {
  const ms = Date.parse(part.valid);
  const runMs = part.run ? Date.parse(part.run) : undefined;
  const rain = RAIN[layer.id];
  if (rain) return scalar(rain[2], accumulation(ms, rain[0], rain[1], q));

  switch (layer.id) {
    case 'radar':
      return scalar(1, prepareRadar(ms, q, { runMs }).dbz);
    case 'precip': {
      const radar = prepareRadar(ms, q, { runMs });
      const sfc = prepareSurface(ms, q);
      return {
        step: 1,
        fn: (lon, lat) => {
          const rate = rainRate(radar.dbz(lon, lat));
          return [rate, ptypeFor(sfc.temp(lon, lat), rate)];
        },
      };
    }
    case 'snow': {
      // Anchored: the run is when the accumulation starts.
      const start = runMs ?? ms - 6 * HOUR;
      const hours = Math.max(1, Math.round((ms - start) / HOUR));
      // At most a couple of dozen steps, so a week of snowfall stays cheap.
      const stepH = Math.max(1, Math.ceil(hours / 24));
      const steps: { radar: ReturnType<typeof prepareRadar>; sfc: ReturnType<typeof prepareSurface>; h: number }[] = [];
      for (let k = 0; k * stepH < hours; k++) {
        const h = Math.min(stepH, hours - k * stepH);
        const t = start + (k * stepH + h / 2) * HOUR;
        steps.push({ radar: prepareRadar(t, q, { skipGeoref: true, runMs }), sfc: prepareSurface(t, q), h });
      }
      return {
        step: 4,
        fn: (lon, lat) => {
          let mm = 0;
          for (const s of steps) {
            const rate = rainRate(s.radar.dbz(lon, lat));
            if (rate <= 0) continue;
            const type = ptypeFor(s.sfc.temp(lon, lat), rate);
            // About ten millimetres of snow per millimetre of water; a mix counts for less.
            mm += (type === 2 ? rate * 10 : type === 5 ? rate * 4 : type === 3 ? rate * 2 : 0) * 0.5 * s.h;
          }
          return [mm];
        },
      };
    }
    case 'temp':
      return scalar(2, prepareSurface(ms, q).temp);
    case 'dew_point':
      return scalar(2, prepareSurface(ms, q).dewpoint);
    case 'feels_like': {
      const sfc = prepareSurface(ms, q);
      return {
        step: 2,
        fn: (lon, lat) => {
          const w = sfc.wind(lon, lat);
          return [feelsLike(sfc.temp(lon, lat), sfc.dewpoint(lon, lat), Math.hypot(w.u, w.v))];
        },
      };
    }
    case 'wind': {
      const sfc = prepareSurface(ms, q);
      return {
        step: 2,
        fn: (lon, lat) => {
          const w = sfc.wind(lon, lat);
          return [w.u, w.v, w.gust];
        },
      };
    }
    case 'satellite':
      return scalar(2, prepareInfrared(ms, q, { runMs }).tb);
    case 'clouds': {
      const sfc = prepareSurface(ms, q);
      const radar = prepareRadar(ms, q, { runMs, skipGeoref: true });
      return { step: 2, fn: (lon, lat) => [sfc.clouds(lon, lat, radar.dbz(lon, lat))] };
    }
    default:
      return { step: 1, fn: () => null };
  }
}

interface Layered {
  part: Part;
  fn: Sampler;
  weight: (lon: number, lat: number) => number;
}

/**
 * The stack's value at a point: the top part with data there, faded into
 * what lies beneath where its weight is below one. Categorical bands take
 * whichever side weighs more instead of blending.
 */
function composer(layer: LayerSpec, stack: Layered[]): Sampler {
  const categorical = layer.bands.map((b) => b.kind === 'categorical');
  const at = (i: number, lon: number, lat: number): number[] | null => {
    for (; i < stack.length; i++) {
      const s = stack[i];
      const w = s.weight(lon, lat);
      if (w <= 0) continue;
      const v = s.fn(lon, lat);
      if (!v) continue;
      if (w >= 1) return v;
      const below = at(i + 1, lon, lat);
      if (!below) return v;
      return v.map((x, b) => (categorical[b] ? (w >= 0.5 ? x : below[b]) : w * x + (1 - w) * below[b]));
    }
    return null;
  };
  return (lon, lat) => at(0, lon, lat);
}

/** How much of a source shows at a point, as the tile composer weighs it. */
function weightOf(part: Part): (lon: number, lat: number) => number {
  const src = sourceById(part.source);
  const { bounds, solid } = part;
  return (lon, lat) => (solid && contains(solid, lon, lat) ? 1 : src && contains(bounds, lon, lat) ? src.weight(lon, lat) : 0);
}

/**
 * The parts that make the value at a point, top first, with each one's share
 * of the blend (what `/api/provenance` answers): the top part with data
 * takes its weight, the rest of the value comes from below, and so on. The
 * synthetic weather is defined everywhere, so a part has data wherever its
 * weight is above zero.
 */
export function provenanceAt(parts: readonly Part[], lon: number, lat: number): Provenance['parts'] {
  const out: Provenance['parts'] = [];
  let rest = 1;
  for (const p of parts) {
    if (rest <= 1e-6) break;
    const w = weightOf(p)(lon, lat);
    if (w <= 0) continue;
    out.push({ id: p.id, source: p.source, share: rest * w });
    rest *= 1 - w;
  }
  // Nothing beneath the last one: it makes up the rest on its own.
  const total = out.reduce((a, x) => a + x.share, 0);
  return total > 0 ? out.map((x) => ({ ...x, share: Math.round((x.share / total) * 1000) / 1000 })) : [];
}

/** Encoded tile, or null for a tile with no data at all (the server's 204). */
export function renderTile(layer: LayerSpec, parts: readonly Part[], z: number, x: number, y: number): Buffer | null {
  const n = 2 ** z;
  const lons = new Float64Array(SAMPLES);
  const lats = new Float64Array(SAMPLES);
  for (let i = 0; i < SAMPLES; i++) {
    lons[i] = xToLon((x + i / (SAMPLES - 1)) / n);
    lats[i] = yToLat((y + i / (SAMPLES - 1)) / n);
  }
  let step = Infinity;
  const stack: Layered[] = [];
  for (const part of parts) {
    const src = sourceById(part.source);
    if (!src) continue;
    const s = sampler(layer, part, src.quality);
    step = Math.min(step, s.step);
    stack.push({ part, fn: s.fn, weight: weightOf(part) });
  }
  if (!stack.length) return null;
  const values = evaluate(lons, lats, layer.bands.length, step, composer(layer, stack));
  let any = false;
  const bands = values.map((vals, b) => {
    const q = new Uint16Array(vals.length);
    for (let i = 0; i < vals.length; i++) {
      if (Number.isNaN(vals[i])) continue;
      q[i] = quantize(layer.bands[b], vals[i]);
      any = true;
    }
    return q;
  });
  return any ? encodeWxt(SAMPLES, SAMPLES, bands) : null;
}
