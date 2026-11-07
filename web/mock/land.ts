// A global land/sea mask for the synthetic weather, rasterized once per
// process from Natural Earth's 1:110m land polygons (world-atlas TopoJSON).
// Plain TypeScript with only erasable syntax: tile workers load it directly.

import { readFileSync } from 'node:fs';

/** Grid spacing, degrees. Cell centres sit at -180 + (i + 0.5) * RES. */
const RES = 0.25;
const W = Math.round(360 / RES);
const H = Math.round(180 / RES);

interface Topology {
  transform: { scale: [number, number]; translate: [number, number] };
  arcs: [number, number][][];
  objects: { land: { geometries: { type: string; arcs: number[][] | number[][][] }[] } };
}

/** Rings as flat [x0, y0, x1, y1, ...] arrays, unwrapped so consecutive
 *  points never jump across the antimeridian, each moved by a whole turn so
 *  its middle lies within [-180, 180) (keeping holes in the same copy of the
 *  world as the shores around them). A ring that winds once round the pole
 *  (Antarctica) is closed along the pole. */
function rings(topo: Topology): number[][] {
  const { scale, translate } = topo.transform;
  const arcs = topo.arcs.map((arc) => {
    let x = 0;
    let y = 0;
    const out: number[] = [];
    for (const [dx, dy] of arc) {
      x += dx;
      y += dy;
      out.push(x * scale[0] + translate[0], y * scale[1] + translate[1]);
    }
    return out;
  });
  const out: number[][] = [];
  const addRing = (idx: number[]) => {
    const raw: number[] = [];
    for (const i of idx) {
      const a = arcs[i < 0 ? ~i : i];
      const n = a.length / 2;
      // Consecutive arcs share their joining point.
      for (let k = raw.length ? 1 : 0; k < n; k++) {
        const p = i < 0 ? n - 1 - k : k;
        raw.push(a[2 * p], a[2 * p + 1]);
      }
    }
    const pts: number[] = [];
    let shift = 0;
    let lo = Infinity;
    let hi = -Infinity;
    let latSum = 0;
    for (let k = 0; k < raw.length; k += 2) {
      if (k > 0) {
        const d = raw[k] - raw[k - 2];
        if (d > 180) shift -= 360;
        else if (d < -180) shift += 360;
      }
      const x = raw[k] + shift;
      pts.push(x, raw[k + 1]);
      lo = Math.min(lo, x);
      hi = Math.max(hi, x);
      latSum += raw[k + 1];
    }
    if (shift !== 0) {
      const pole = latSum < 0 ? -90 : 90;
      pts.push(pts[pts.length - 2], pole, pts[0], pole);
    }
    const turn = 360 * Math.round((lo + hi) / 2 / 360);
    if (turn !== 0) for (let k = 0; k < pts.length; k += 2) pts[k] -= turn;
    out.push(pts);
  };
  for (const g of topo.objects.land.geometries) {
    if (g.type === 'Polygon') for (const r of g.arcs as number[][]) addRing(r);
    else if (g.type === 'MultiPolygon') for (const p of g.arcs as number[][][]) for (const r of p) addRing(r);
  }
  return out;
}

/** Even-odd scanline fill at cell centres: 1 for land. Spans that run past
 *  +/-180 fold back round the world. */
function rasterize(rs: number[][]): Uint8Array {
  // Edges bucketed by the rows whose centre line they cross.
  const ex1: number[] = [];
  const ey1: number[] = [];
  const ex2: number[] = [];
  const ey2: number[] = [];
  const rowOf = (lat: number) => (lat + 90) / RES - 0.5;
  const count = new Int32Array(H + 1);
  const span: [number, number][] = [];
  for (const r of rs) {
    const n = r.length / 2;
    for (let k = 0; k < n; k++) {
      const x1 = r[2 * k];
      const y1 = r[2 * k + 1];
      const x2 = r[2 * ((k + 1) % n)];
      const y2 = r[2 * ((k + 1) % n) + 1];
      if (y1 === y2) continue;
      const j0 = Math.max(0, Math.ceil(rowOf(Math.min(y1, y2))));
      const j1 = Math.min(H - 1, Math.floor(rowOf(Math.max(y1, y2))));
      if (j1 < j0) continue;
      ex1.push(x1);
      ey1.push(y1);
      ex2.push(x2);
      ey2.push(y2);
      span.push([j0, j1]);
      for (let j = j0; j <= j1; j++) count[j + 1]++;
    }
  }
  for (let j = 0; j < H; j++) count[j + 1] += count[j];
  const fill = count.slice(0, H);
  const byRow = new Int32Array(count[H]);
  for (let e = 0; e < span.length; e++) for (let j = span[e][0]; j <= span[e][1]; j++) byRow[fill[j]++] = e;

  const grid = new Uint8Array(W * H);
  const xs: number[] = [];
  for (let j = 0; j < H; j++) {
    const lat = -90 + (j + 0.5) * RES;
    xs.length = 0;
    for (let q = count[j]; q < count[j + 1]; q++) {
      const e = byRow[q];
      const y1 = ey1[e];
      const y2 = ey2[e];
      if (y1 > lat === y2 > lat) continue;
      xs.push(ex1[e] + ((lat - y1) * (ex2[e] - ex1[e])) / (y2 - y1));
    }
    xs.sort((a, b) => a - b);
    const row = j * W;
    for (let k = 0; k + 1 < xs.length; k += 2) {
      const i0 = Math.ceil((xs[k] + 180) / RES - 0.5);
      const i1 = Math.ceil((xs[k + 1] + 180) / RES - 0.5) - 1;
      if (i1 - i0 + 1 >= W) {
        grid.fill(1, row, row + W);
        continue;
      }
      for (let i = i0; i <= i1; i++) grid[row + (((i % W) + W) % W)] = 1;
    }
  }
  return grid;
}

/** Two passes of a 3 x 3 box blur (wrapping in longitude), so coasts are a
 *  ramp about 100 km wide instead of a step along the cells. */
function blur(mask: Uint8Array): Float32Array {
  const a = Float32Array.from(mask);
  const b = new Float32Array(W * H);
  for (let pass = 0; pass < 2; pass++) {
    for (let j = 0; j < H; j++) {
      const row = j * W;
      for (let i = 0; i < W; i++) {
        const l = i === 0 ? W - 1 : i - 1;
        const r = i === W - 1 ? 0 : i + 1;
        b[row + i] = (a[row + l] + a[row + i] + a[row + r]) / 3;
      }
    }
    for (let j = 0; j < H; j++) {
      const up = Math.max(0, j - 1) * W;
      const row = j * W;
      const dn = Math.min(H - 1, j + 1) * W;
      for (let i = 0; i < W; i++) a[row + i] = (b[up + i] + b[row + i] + b[dn + i]) / 3;
    }
  }
  return a;
}

function build(): Float32Array {
  const url = new URL('../node_modules/world-atlas/land-110m.json', import.meta.url);
  const topo = JSON.parse(readFileSync(url, 'utf8')) as Topology;
  return blur(rasterize(rings(topo)));
}

const LAND = build();

/** 0 over open sea, 1 well inland, smooth across coasts; bilinear between
 *  cell centres, wrapping in longitude. Any longitude, latitude clamped. */
export function landWeight(lon: number, lat: number): number {
  let fx = (lon + 180) / RES - 0.5;
  fx -= W * Math.floor(fx / W);
  if (fx >= W) fx = 0;
  let fy = (lat + 90) / RES - 0.5;
  fy = fy < 0 ? 0 : fy > H - 1 ? H - 1 : fy;
  const i0 = Math.floor(fx);
  const i1 = i0 + 1 === W ? 0 : i0 + 1;
  const j0 = Math.min(H - 2, Math.floor(fy));
  const tx = fx - i0;
  const ty = fy - j0;
  const r0 = j0 * W;
  const r1 = r0 + W;
  const top = LAND[r0 + i0] + (LAND[r0 + i1] - LAND[r0 + i0]) * tx;
  const bot = LAND[r1 + i0] + (LAND[r1 + i1] - LAND[r1 + i0]) * tx;
  return top + (bot - top) * ty;
}
