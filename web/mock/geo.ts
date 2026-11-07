// Geography and noise for the synthetic weather. Plain TypeScript with only
// erasable syntax so Node can also run it directly.

export const xToLon = (x: number) => x * 360 - 180;
export const yToLat = (y: number) => (Math.atan(Math.sinh(Math.PI * (1 - 2 * y))) * 180) / Math.PI;

const KM_PER_DEG = 111.32;

/** Longitude (or a longitude difference) folded into [-180, 180). */
export function wrapLon(lon: number): number {
  return lon >= -180 && lon < 180 ? lon : lon - 360 * Math.floor((lon + 180) / 360);
}

/** Local flat-earth offset from (lon0, lat0) to (lon, lat), in kilometres
 *  (east, north). Plenty accurate over the few hundred km a storm spans. The
 *  longitude difference is taken the short way round, so a storm on the
 *  antimeridian is the same storm on both sides of it. */
export function offsetKm(lon: number, lat: number, lon0: number, lat0: number): [number, number] {
  return [wrapLon(lon - lon0) * KM_PER_DEG * Math.cos((lat0 * Math.PI) / 180), (lat - lat0) * KM_PER_DEG];
}

export function distKm(lon: number, lat: number, lon0: number, lat0: number): number {
  const [dx, dy] = offsetKm(lon, lat, lon0, lat0);
  return Math.sqrt(dx * dx + dy * dy);
}

/** Coarse outline of the contiguous United States (lon, lat), clockwise from
 *  the north-west. Good to ~50 km: it decides radar coverage and land/sea. */
export const CONUS: [number, number][] = [
  [-124.7, 48.4], [-123.0, 49.0], [-95.2, 49.0], [-89.6, 48.0], [-84.8, 46.6], [-82.4, 45.3],
  [-82.5, 42.6], [-79.0, 43.4], [-76.3, 44.2], [-74.7, 45.0], [-71.5, 45.0], [-69.2, 47.4],
  [-67.8, 47.1], [-67.0, 44.8], [-70.2, 43.6], [-70.0, 41.8], [-71.9, 41.3], [-74.0, 40.5],
  [-74.9, 38.9], [-76.0, 37.0], [-75.5, 35.2], [-77.9, 33.9], [-81.0, 31.5], [-80.0, 26.8],
  [-80.4, 25.2], [-81.8, 26.0], [-82.8, 28.0], [-84.3, 30.0], [-86.5, 30.4], [-89.6, 30.2],
  [-89.4, 29.0], [-91.5, 29.4], [-94.0, 29.6], [-97.2, 27.8], [-97.4, 25.9], [-99.2, 26.6],
  [-101.4, 29.8], [-103.1, 28.9], [-104.5, 29.7], [-106.5, 31.8], [-108.2, 31.3], [-111.1, 31.3],
  [-114.8, 32.5], [-117.1, 32.5], [-118.5, 34.0], [-120.6, 34.6], [-122.4, 37.2], [-123.8, 39.6],
  [-124.2, 42.0], [-124.0, 46.2],
];

export function pointInPolygon(lon: number, lat: number, ring: readonly [number, number][]): boolean {
  let inside = false;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    const [xi, yi] = ring[i];
    const [xj, yj] = ring[j];
    if (yi > lat !== yj > lat && lon < ((xj - xi) * (lat - yi)) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

/** Distance in degrees (lon scaled by cos lat) from a point to a ring's edges. */
export function distToRingDeg(lon: number, lat: number, ring: readonly [number, number][]): number {
  const k = Math.cos((lat * Math.PI) / 180);
  let best = Infinity;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    const ax = ring[j][0] * k;
    const ay = ring[j][1];
    const bx = ring[i][0] * k;
    const by = ring[i][1];
    const px = lon * k;
    const py = lat;
    const dx = bx - ax;
    const dy = by - ay;
    const L = dx * dx + dy * dy;
    const t = L > 0 ? Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / L)) : 0;
    const ex = px - (ax + t * dx);
    const ey = py - (ay + t * dy);
    const d = Math.sqrt(ex * ex + ey * ey);
    if (d < best) best = d;
  }
  return best;
}

/**
 * Signed distance to the US outline in degrees (negative inside), sampled once
 * on a 0.1 degree grid around the US and interpolated: testing the polygon
 * for every one of a tile's 66,049 samples would make tiles slow to generate.
 * Only round the US: a world-sized grid would take seconds to build.
 */
const SD_BOX = { west: -138.0, south: 15.0, east: -52.0, north: 60.0 };
const SD_RES = 0.1;
const SD_W = Math.round((SD_BOX.east - SD_BOX.west) / SD_RES) + 1;
const SD_H = Math.round((SD_BOX.north - SD_BOX.south) / SD_RES) + 1;
let sdGrid: Float32Array | null = null;

/** Same values as distToRingDeg and pointInPolygon per node, but one row at a
 *  time (the lon scale is constant along a row, and the inside test is a
 *  walk along the row's sorted crossings) so it builds in tens of ms. */
function signedDistanceGrid(): Float32Array {
  if (sdGrid) return sdGrid;
  const g = new Float32Array(SD_W * SD_H);
  const n = CONUS.length;
  const ax = new Float64Array(n);
  const ay = new Float64Array(n);
  const ex = new Float64Array(n);
  const ey = new Float64Array(n);
  const inv = new Float64Array(n);
  const cross: number[] = [];
  for (let j = 0; j < SD_H; j++) {
    const lat = SD_BOX.south + j * SD_RES;
    const k = Math.cos((lat * Math.PI) / 180);
    cross.length = 0;
    for (let i = 0, p = n - 1; i < n; p = i++) {
      const [xi, yi] = CONUS[i];
      const [xp, yp] = CONUS[p];
      ax[i] = xp * k;
      ay[i] = yp;
      ex[i] = (xi - xp) * k;
      ey[i] = yi - yp;
      const L = ex[i] * ex[i] + ey[i] * ey[i];
      inv[i] = L > 0 ? 1 / L : 0;
      if (yi > lat !== yp > lat) cross.push(((xp - xi) * (lat - yi)) / (yp - yi) + xi);
    }
    cross.sort((a, b) => a - b);
    let c = 0;
    for (let i = 0; i < SD_W; i++) {
      const lon = SD_BOX.west + i * SD_RES;
      while (c < cross.length && cross[c] <= lon) c++;
      const px = lon * k;
      let best = Infinity;
      for (let e = 0; e < n; e++) {
        const qx = px - ax[e];
        const qy = lat - ay[e];
        let t = (qx * ex[e] + qy * ey[e]) * inv[e];
        t = t < 0 ? 0 : t > 1 ? 1 : t;
        const dx = qx - t * ex[e];
        const dy = qy - t * ey[e];
        const d2 = dx * dx + dy * dy;
        if (d2 < best) best = d2;
      }
      const d = Math.sqrt(best);
      // An odd number of crossings at or west of lon means inside.
      g[j * SD_W + i] = c % 2 === 1 ? -d : d;
    }
  }
  sdGrid = g;
  return g;
}

export function conusDistance(lon: number, lat: number): number {
  const fx = (lon - SD_BOX.west) / SD_RES;
  const fy = (lat - SD_BOX.south) / SD_RES;
  if (fx < 0 || fy < 0 || fx > SD_W - 1 || fy > SD_H - 1) return 99;
  const g = signedDistanceGrid();
  const i = Math.min(SD_W - 2, Math.floor(fx));
  const j = Math.min(SD_H - 2, Math.floor(fy));
  const tx = fx - i;
  const ty = fy - j;
  const a = g[j * SD_W + i];
  const b = g[j * SD_W + i + 1];
  const c = g[(j + 1) * SD_W + i];
  const d = g[(j + 1) * SD_W + i + 1];
  return (a * (1 - tx) + b * tx) * (1 - ty) + (c * (1 - tx) + d * tx) * ty;
}

/** Inside the US or within `bufferDeg` of its outline: where radars see. */
export function nearConus(lon: number, lat: number, bufferDeg: number): boolean {
  return conusDistance(lon, lat) < bufferDeg;
}

// ---------------------------------------------------------------------------
// Deterministic noise
// ---------------------------------------------------------------------------

/** 32-bit integer hash to [0, 1). */
export function hash3(x: number, y: number, z: number): number {
  let h = Math.imul(x | 0, 0x27d4eb2d) ^ Math.imul(y | 0, 0x165667b1) ^ Math.imul(z | 0, 0x9e3779b1);
  h = Math.imul(h ^ (h >>> 15), 0x85ebca6b);
  h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
  h ^= h >>> 16;
  return (h >>> 0) / 4294967296;
}

export function hash1(n: number): number {
  return hash3(n, n * 7 + 3, n * 13 + 11);
}

const smooth = (t: number) => t * t * (3 - 2 * t);

/** Trilinear value noise in [0, 1). */
export function valueNoise(x: number, y: number, z: number): number {
  const xi = Math.floor(x);
  const yi = Math.floor(y);
  const zi = Math.floor(z);
  const fx = smooth(x - xi);
  const fy = smooth(y - yi);
  const fz = smooth(z - zi);
  const c000 = hash3(xi, yi, zi);
  const c100 = hash3(xi + 1, yi, zi);
  const c010 = hash3(xi, yi + 1, zi);
  const c110 = hash3(xi + 1, yi + 1, zi);
  const c001 = hash3(xi, yi, zi + 1);
  const c101 = hash3(xi + 1, yi, zi + 1);
  const c011 = hash3(xi, yi + 1, zi + 1);
  const c111 = hash3(xi + 1, yi + 1, zi + 1);
  const x00 = c000 + (c100 - c000) * fx;
  const x10 = c010 + (c110 - c010) * fx;
  const x01 = c001 + (c101 - c001) * fx;
  const x11 = c011 + (c111 - c011) * fx;
  const y0 = x00 + (x10 - x00) * fy;
  const y1 = x01 + (x11 - x01) * fy;
  return y0 + (y1 - y0) * fz;
}

/** Fractal noise in roughly [0, 1); one octave is `valueNoise` itself. */
function fbm(x: number, y: number, z: number, octaves = 4): number {
  let sum = 0;
  let amp = 0.5;
  let norm = 0;
  let f = 1;
  for (let o = 0; o < octaves; o++) {
    sum += amp * valueNoise(x * f + o * 17.3, y * f - o * 9.1, z * f + o * 3.7);
    norm += amp;
    amp *= 0.5;
    f *= 2.03;
  }
  return sum / norm;
}

/** Longitudes this close (degrees) to the antimeridian blend noise sampled
 *  on both sides of it. */
const SEAM = 15;

/** Blend of two independent noise samples that keeps the contrast of one:
 *  a plain average of two would look washed out along the dateline. */
function blendNoise(a: number, b: number, w: number): number {
  const k = 1 / Math.sqrt((1 - w) * (1 - w) + w * w);
  return clamp(0.5 + ((1 - w) * (a - 0.5) + w * (b - 0.5)) * k, 0, 1);
}

/**
 * Fractal noise over the globe, `scale` lattice cells per degree, seamless
 * across the antimeridian: within SEAM degrees of it the noise at lon is
 * cross-faded with the noise at lon -/+ 360, half and half exactly at 180, so
 * both sides meet. Elsewhere it is exactly `fbm(lon * scale + ox, lat *
 * scale + oy, z)`.
 */
export function fbmGeo(lon: number, lat: number, z: number, scale: number, octaves = 4, ox = 0, oy = 0): number {
  if (lon < -180 || lon >= 180) lon = wrapLon(lon);
  const y = lat * scale + oy;
  const a = fbm(lon * scale + ox, y, z, octaves);
  const al = lon < 0 ? -lon : lon;
  if (al <= 180 - SEAM) return a;
  const other = lon < 0 ? lon + 360 : lon - 360;
  return blendNoise(a, fbm(other * scale + ox, y, z, octaves), 0.5 * smoothstep(180 - SEAM, 180, al));
}

export const clamp = (v: number, lo: number, hi: number) => (v < lo ? lo : v > hi ? hi : v);
export function smoothstep(e0: number, e1: number, x: number): number {
  const t = clamp((x - e0) / (e1 - e0), 0, 1);
  return t * t * (3 - 2 * t);
}

/** Distance from point p to segment ab, all in km offsets. */
export function segmentDistance(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax;
  const dy = by - ay;
  const L = dx * dx + dy * dy;
  const t = L > 0 ? clamp(((px - ax) * dx + (py - ay) * dy) / L, 0, 1) : 0;
  const ex = px - (ax + t * dx);
  const ey = py - (ay + t * dy);
  return Math.sqrt(ex * ex + ey * ey);
}
