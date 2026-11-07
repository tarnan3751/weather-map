// CPU twin of the shader's sampling, used for the value under the cursor.
// Samples sit on lattice points: sample (col, row) is at tile fraction
// (col / (w - 1), row / (h - 1)), so a tile is interpolated without neighbours.

/**
 * Bilinear interpolation of raw samples that skips no-data (0) and renormalises
 * the weights of the samples that remain. Where less than half of the weight
 * is valid the result is no-data, so a coverage edge falls midway between
 * samples rather than smearing outward. Returns the interpolated *sample*
 * (still quantized space, fractional) or null.
 */
export function bilinearSample(
  data: Uint16Array,
  w: number,
  h: number,
  channels: number,
  c: number,
  u: number,
  v: number,
): number | null {
  const px = Math.max(0, Math.min(1, u)) * (w - 1);
  const py = Math.max(0, Math.min(1, v)) * (h - 1);
  const x0 = Math.min(w - 2, Math.floor(px));
  const y0 = Math.min(h - 2, Math.floor(py));
  const fx = px - x0;
  const fy = py - y0;
  const at = (x: number, y: number) => data[(y * w + x) * channels + c];
  const q = [at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1)];
  const wt = [(1 - fx) * (1 - fy), fx * (1 - fy), (1 - fx) * fy, fx * fy];
  let sw = 0;
  let sq = 0;
  for (let i = 0; i < 4; i++) {
    if (q[i] === 0) continue;
    sw += wt[i];
    sq += wt[i] * q[i];
  }
  if (sw < 0.5) return null;
  return sq / sw;
}

/** Nearest lattice sample, for categorical bands. */
export function nearestSample(
  data: Uint16Array,
  w: number,
  h: number,
  channels: number,
  c: number,
  u: number,
  v: number,
): number | null {
  const x = Math.round(Math.max(0, Math.min(1, u)) * (w - 1));
  const y = Math.round(Math.max(0, Math.min(1, v)) * (h - 1));
  const q = data[(y * w + x) * channels + c];
  return q === 0 ? null : q;
}

export const dequantize = (band: { scale: number; offset: number }, q: number) => band.offset + (q - 1) * band.scale;

/**
 * Among the four samples around (u, v), the nearest whose value (after
 * dequantization) is positive; used for precipitation type, so the read-out
 * agrees with the shader's `typeAt` at the edges of a rain area.
 */
export function nearestPositiveSample(
  data: Uint16Array,
  w: number,
  h: number,
  channels: number,
  c: number,
  u: number,
  v: number,
  band: { scale: number; offset: number },
): number | null {
  const px = Math.max(0, Math.min(1, u)) * (w - 1);
  const py = Math.max(0, Math.min(1, v)) * (h - 1);
  const x0 = Math.min(w - 2, Math.floor(px));
  const y0 = Math.min(h - 2, Math.floor(py));
  const fx = px - x0;
  const fy = py - y0;
  const corners: [number, number, number][] = [
    [x0, y0, (1 - fx) * (1 - fy)],
    [x0 + 1, y0, fx * (1 - fy)],
    [x0, y0 + 1, (1 - fx) * fy],
    [x0 + 1, y0 + 1, fx * fy],
  ];
  let best: number | null = null;
  let bestW = -1;
  for (const [x, y, wt] of corners) {
    const q = data[(y * w + x) * channels + c];
    if (q === 0 || Math.round(dequantize(band, q)) <= 0) continue;
    if (wt > bestW) {
      bestW = wt;
      best = q;
    }
  }
  return best;
}
