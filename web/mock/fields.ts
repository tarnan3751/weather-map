// Synthetic but plausible weather over the whole world for one November week,
// as functions of place and time.
//
// Over the US: a low over the central Plains with a squall line on its cold
// front, wrap-around snow and a band of freezing rain and sleet on its cold
// side, a Pacific rain shield coming ashore, lake-effect bands, Florida
// afternoon showers, and a second system later in the week for the long
// range. One stationary storm sits on downtown Kansas City to check
// georeferencing.
//
// Elsewhere: a deep Iceland low running to Norway with a long cold front over
// the UK and France, a Mediterranean low, an Aleutian low on the dateline, a
// weak Siberian low, lows off Japan and Newfoundland later in the week,
// Southern Ocean lows marching east (turning clockwise, fronts mirrored), a
// typhoon heading for Luzon, the ITCZ and SPCZ, tropical convection over
// land with an afternoon peak, and the north-east monsoon over southern India
// and Sri Lanka.
//
// Every field is "prepared" for one time (feature positions computed once)
// and then evaluated per sample. Every field is continuous across the
// antimeridian: longitude differences wrap, noise is cross-faded there (see
// fbmGeo), and terms that depend on longitude are periodic in it.

import { clamp, conusDistance, fbmGeo, hash1, hash3, offsetKm, segmentDistance, smoothstep, valueNoise, wrapLon } from './geo.ts';
import { landWeight } from './land.ts';
import { GEOREF_CITY } from './places.ts';
import { HOUR } from './util.ts';

/** Who made the field: `obs` ground radar or an analysis (sharpest), `sat` a
 *  satellite precipitation estimate (global, smoother, weaker peaks, a little
 *  displaced), `hrrr` a fine regional model, `gfs` a coarse global model. */
export type Quality = 'obs' | 'sat' | 'hrrr' | 'gfs';

const KM = 111.32;
const RAD = Math.PI / 180;

let epochMs = Math.floor(Date.now() / HOUR) * HOUR;
/** Feature positions are laid out relative to this moment (the mock's start). */
export function setEpoch(ms: number) {
  epochMs = Math.floor(ms / HOUR) * HOUR;
}
export const hoursSinceEpoch = (ms: number) => (ms - epochMs) / HOUR;

export const NO_ECHO = -30;

const utcHourOf = (ms: number) => (((ms / HOUR) % 24) + 24) % 24;
/** Hours folded into [-12, 12). */
const wrap24 = (x: number) => x - 24 * Math.floor((x + 12) / 24);
const mix = (a: number, b: number, w: number) => a + (b - a) * w;

// ---------------------------------------------------------------------------
// Geography
// ---------------------------------------------------------------------------

/** Rows of [lon, lat, rx, ry, value]: flat-topped bumps of radius rx, ry degrees. */
function bumps(rows: number[][]): Float64Array {
  return Float64Array.from(rows.flat());
}

/** Flat-topped: 1 inside the ellipse, ~0.37 on it, nothing by twice its
 *  radius. For plateaus and deserts. */
function plateau(lon: number, lat: number, lonC: number, latC: number, rx: number, ry: number): number {
  const dy = (lat - latC) / ry;
  if (dy >= 2 || dy <= -2) return 0;
  const dx = wrapLon(lon - lonC) / rx;
  if (dx >= 2 || dx <= -2) return 0;
  const q = dx * dx + dy * dy;
  return q >= 4 ? 0 : Math.exp(-q * q);
}

/** Gaussian, cut off at three radii. For climate anomalies, which have no edges. */
function blob(lon: number, lat: number, lonC: number, latC: number, rx: number, ry: number): number {
  const dy = (lat - latC) / ry;
  if (dy >= 3 || dy <= -3) return 0;
  const dx = wrapLon(lon - lonC) / rx;
  if (dx >= 3 || dx <= -3) return 0;
  const q = dx * dx + dy * dy;
  return q >= 9 ? 0 : Math.exp(-q);
}

function maxBump(lon: number, lat: number, b: Float64Array, shape = blob): number {
  let best = 0;
  for (let i = 0; i < b.length; i += 5) {
    if (b[i + 4] <= best) continue;
    const v = b[i + 4] * shape(lon, lat, b[i], b[i + 1], b[i + 2], b[i + 3]);
    if (v > best) best = v;
  }
  return best;
}

/** Highest of several ranges, each a crest line of [lon, lat, half-width
 *  (degrees), height (km)] points, flat-topped across. */
function ridges(lon: number, lat: number, crests: Float64Array[]): number {
  const k = Math.cos(lat * RAD);
  let best = 0;
  for (const p of crests) {
    for (let i = 4; i < p.length; i += 4) {
      const reach = 2 * Math.max(p[i - 2], p[i + 2]);
      if (lat < Math.min(p[i - 3], p[i + 1]) - reach || lat > Math.max(p[i - 3], p[i + 1]) + reach) continue;
      const ex = wrapLon(p[i] - p[i - 4]) * k;
      const ey = p[i + 1] - p[i - 3];
      const qx = wrapLon(lon - p[i - 4]) * k;
      const qy = lat - p[i - 3];
      const t = clamp((qx * ex + qy * ey) / (ex * ex + ey * ey), 0, 1);
      const dx = qx - t * ex;
      const dy = qy - t * ey;
      const w = mix(p[i - 2], p[i + 2], t);
      const d2 = (dx * dx + dy * dy) / (w * w);
      if (d2 < 4) best = Math.max(best, mix(p[i - 1], p[i + 3], t) * Math.exp(-d2 * d2));
    }
  }
  return best;
}

/** Mean elevation, km, of the plateaus that shape temperature. */
const PLATEAUS = bumps([
  [88, 33.5, 13, 5, 4.6], // Tibet and the Himalaya
  [73.5, 38, 5, 3, 3.2], // Pamir, Hindu Kush
  [82, 42.5, 8, 2, 2.0], // Tian Shan
  [103, 46, 13, 5, 1.4], // Mongolia
  [55, 32, 10, 5, 1.1], // Iranian plateau
  [35, 39, 6, 2, 1.0], // Anatolia
  [44, 42.5, 3.5, 1.2, 1.2], // Caucasus
  [10, 46.3, 3.8, 1.1, 1.5], // Alps
  [13, 63, 5, 5, 0.6], // Scandinavian mountains
  [39, 9.5, 4.5, 4.5, 2.1], // Ethiopian highlands
  [36, -3, 4, 5.5, 1.4], // East African plateau
  [27, -27, 7, 4.5, 1.3], // Highveld
  [17, -22, 4, 5, 1.0], // Namibian plateau
  [-117, 40, 4, 3.5, 1.5], // Great Basin
  [-110.5, 37, 3.5, 2.5, 1.8], // Colorado Plateau
  [-102, 22.5, 4, 4, 1.8], // Mexican plateau
  [-41, 72.5, 15, 10, 2.6], // Greenland ice sheet
]);

const RANGES = [
  // Andes
  [
    [-72, 10, 1.2, 1.5], [-74.5, 6, 1.6, 2.4], [-77.5, 1, 1.4, 2.6], [-78.5, -3, 1.3, 2.5], [-76.5, -10, 1.8, 3.2],
    [-71, -15, 2.5, 3.8], [-68, -19, 3, 3.9], [-67.5, -24, 2.2, 3.6], [-69.5, -30, 1.4, 3.2], [-70.5, -35, 1.0, 2.4],
    [-71.5, -41, 0.9, 1.5], [-73, -47, 0.9, 1.3], [-73.5, -52, 0.7, 0.8],
  ],
  // Alaska Range, Rockies, Sierra Madre
  [
    [-153, 61, 1.5, 1.0], [-146, 62.5, 1.5, 1.2], [-137, 60, 2.5, 1.1], [-126, 56, 2.5, 1.3], [-117.5, 51, 2.2, 1.6],
    [-113, 47, 2.2, 1.7], [-110, 44, 2.5, 2.0], [-106.5, 40, 2.2, 2.5], [-106, 36, 2.0, 2.0], [-105.5, 32, 1.6, 1.4],
    [-105, 27, 2.0, 1.8], [-102, 21, 2.0, 2.0], [-97, 18.5, 1.5, 1.5],
  ],
  // Cascades and Sierra Nevada
  [[-121.5, 50, 1.0, 1.1], [-121.7, 44, 1.0, 1.3], [-120.3, 39, 1.0, 1.5], [-118.5, 36, 0.8, 1.6]],
].map((r) => Float64Array.from(r.flat()));

function elevationKm(lon: number, lat: number, land: number): number {
  if (land <= 0) return 0;
  // Warp the plateaus' outlines so they are not perfect ellipses.
  const wx = 2.4 * (fbmGeo(lon, lat, 0.5, 0.3, 1, 17.1, 4.2) - 0.5);
  const wy = 1.6 * (fbmGeo(lon, lat, 0.5, 0.3, 1, -3.7, 21.9) - 0.5);
  let e = Math.max(maxBump(lon + wx, lat + wy, PLATEAUS, plateau), ridges(lon, lat, RANGES));
  if (lat < -62) e = Math.max(e, (2.2 + 0.8 * Math.cos((lon - 100) * RAD)) * (1 - smoothstep(-82, -66, lat)));
  return e * land;
}

/** How dry the air is over land (0 humid .. 1 desert). */
const DESERTS = bumps([
  [12, 23, 21, 7, 1.0], // Sahara
  [47, 23, 10, 7, 1.0], // Arabia
  [62, 30, 10, 5, 0.85], // Iran, Afghanistan, Pakistan
  [70, 27, 4, 3, 0.8], // Thar
  [64, 43, 14, 5, 0.65], // Turan and the Kazakh steppe
  [87, 39.5, 8, 3, 0.9], // Taklamakan
  [105, 42, 11, 4, 0.8], // Gobi
  [46, 6, 6, 5, 0.6], // Horn of Africa
  [133, -25, 14, 7, 0.85], // interior Australia
  [20, -24, 6, 6, 0.65], // Kalahari and Namib
  [-70.5, -23, 2.5, 6, 0.9], // Atacama
  [-68, -45, 4, 6, 0.5], // Patagonia
  [-106, 28, 6, 4, 0.55], // northern Mexico
  [-114, 37, 6, 5, 0.7], // the US south-west, where the US terms take over
]);

const dryness = (lon: number, lat: number, land: number) => (land > 0 ? maxBump(lon, lat, DESERTS, plateau) * land : 0);

/** Subtropical stratocumulus decks off the west coasts. */
const DECKS = bumps([
  [-84, -18, 9, 8, 1],
  [4, -18, 8, 7, 1],
  [-124, 27, 8, 6, 0.8],
  [-22, 24, 6, 5, 0.6],
  [100, -25, 8, 6, 0.5],
]);

/** Semi-permanent subtropical highs (and the Siberian high building in
 *  November): anticyclonic winds, dry air, fewer clouds. */
const HIGHS = [
  { lon: -142, lat: 33, R: 1600, vmax: 5 },
  { lon: -33, lat: 35, R: 1500, vmax: 5 },
  { lon: 100, lat: 50, R: 1900, vmax: 4 },
  { lon: -100, lat: -32, R: 1700, vmax: 5 },
  { lon: -8, lat: -30, R: 1500, vmax: 5 },
  { lon: 78, lat: -32, R: 1700, vmax: 5 },
  { lon: 132, lat: -38, R: 1300, vmax: 3.5 },
].map((H) => ({ ...H, kx: KM * Math.cos(H.lat * RAD) }));

function highness(lon: number, lat: number): number {
  let best = 0;
  for (const H of HIGHS) best = Math.max(best, blob(lon, lat, H.lon, H.lat, 25, 9));
  return best;
}

/** Catmull-Rom through evenly spaced values. */
function catmull(p0: number, p1: number, p2: number, p3: number, t: number): number {
  return 0.5 * (2 * p1 + (p2 - p0) * t + (2 * p0 - 5 * p1 + 4 * p2 - p3) * t * t + (3 * p1 - p0 - 3 * p2 + p3) * t * t * t);
}

/** A profile in latitude, one value per 10 degrees from -90 to 90. */
function zonal(table: readonly number[], lat: number): number {
  const f = (clamp(lat, -90, 90) + 90) / 10;
  const i = Math.min(17, Math.floor(f));
  return catmull(table[Math.max(0, i - 1)], table[i], table[i + 1], table[Math.min(18, i + 2)], f - i);
}

/** A periodic profile in longitude, one value per 20 degrees from -180. */
function around(table: readonly number[], lon: number): number {
  const f = (wrapLon(lon) + 180) / 20;
  const i = Math.floor(f);
  const n = table.length;
  return catmull(table[(i + n - 1) % n], table[i % n], table[(i + 1) % n], table[(i + 2) % n], f - i);
}

// ---------------------------------------------------------------------------
// Tropical convection potential
// ---------------------------------------------------------------------------

// ITCZ in November by longitude (nodes every 20 degrees from -180): latitude
// of its axis, half-width and strength. North of the equator over the
// Pacific and Atlantic, south of it over the Indian Ocean.
const ITCZ_LAT = [7, 7, 7.5, 8, 8.5, 7, 5, 5.5, 6, 4, 0, -3, -4, -4, -3, 2, 5, 6];
const ITCZ_W = [2.6, 2.6, 2.5, 2.4, 2.4, 3, 3.5, 2.5, 2.5, 3, 4, 4, 4.5, 5, 6, 7, 5.5, 4];
const ITCZ_S = [0.45, 0.45, 0.5, 0.55, 0.6, 0.5, 0.3, 0.5, 0.45, 0.35, 0.1, 0.25, 0.4, 0.5, 0.5, 0.45, 0.5, 0.5];

const CONVECTIVE_SEA = bumps([
  [82, 9, 5, 4, 0.5], // south-west Bay of Bengal, north-east monsoon
  [128, 9, 14, 7, 0.3], // Philippine Sea
  [115, -3, 18, 8, 0.3], // seas of the Maritime Continent
  [109.5, 14, 2.5, 3.5, 0.3], // off central Vietnam
]);

const CONVECTIVE_LAND = bumps([
  [-62, -6, 13, 9, 0.65], // Amazon
  [-74, 5, 5, 5, 0.5], // Colombia
  [-86, 12, 5, 4, 0.3], // Central America
  [-56, -22, 7, 6, 0.4], // Paraguay and southern Brazil
  [22, -3, 12, 8, 0.6], // Congo basin
  [28, -15, 10, 5, 0.4], // Zambia to Mozambique
  [36.5, -2, 4, 5, 0.4], // East African short rains
  [-5, 7.5, 10, 2.5, 0.3], // Guinea coast
  [47, -18, 3, 6, 0.35], // Madagascar
  [28, -26, 4, 3, 0.25], // Highveld storms
  [115, -2, 25, 9, 0.8], // Maritime Continent
  [141, -5, 8, 4, 0.7], // New Guinea
  [133, -14, 12, 4, 0.2], // the Top End build-up
  [79.2, 9.5, 3, 3.5, 0.8], // Tamil Nadu and Sri Lanka, north-east monsoon
  [125, 11, 3, 5, 0.4], // eastern Philippines
  [108, 14, 2.5, 4, 0.5], // central Vietnam
]);

function staticPotential(lon: number, lat: number): [number, number] {
  const L = landWeight(lon, lat);
  const c = around(ITCZ_LAT, lon);
  const w = around(ITCZ_W, lon);
  const itcz = around(ITCZ_S, lon) * Math.exp(-(((lat - c) / w) ** 2));
  // SPCZ: a diagonal from the Solomon Islands toward French Polynesia.
  let spcz = 0;
  const a = wrapLon(lon - 155) / 55;
  if (a > -0.3 && a < 1.3) {
    const lc = -7 - 18 * a;
    spcz = 0.42 * Math.exp(-(((lat - lc) / 3.5) ** 2)) * smoothstep(-0.2, 0.05, a) * (1 - smoothstep(0.75, 1.1, a));
  }
  // SACZ: from the Amazon out over the South Atlantic.
  let sacz = 0;
  const b = wrapLon(lon + 55) / 25;
  if (b > -0.3 && b < 1.3) {
    const lc = -12 - 16 * b;
    sacz = 0.3 * Math.exp(-(((lat - lc) / 3) ** 2)) * smoothstep(-0.2, 0.05, b) * (1 - smoothstep(0.75, 1.1, b));
  }
  const fade = 1 - smoothstep(30, 38, Math.abs(lat));
  const sea = (1 - L) * (itcz + spcz + sacz + maxBump(lon, lat, CONVECTIVE_SEA)) * fade;
  const land = L * (maxBump(lon, lat, CONVECTIVE_LAND) + 0.6 * itcz + spcz + sacz) * fade;
  return [Math.min(1, sea), Math.min(1, land)];
}

// The potential on a 1.25 degree grid: cell centres are also the slots that
// convective cells are drawn from.
const SLOT = 1.25;
const SLOT_W = Math.round(360 / SLOT);
const SLOT_LAT0 = -40;
const SLOT_H = Math.round(80 / SLOT);
const POT_SEA = new Float32Array(SLOT_W * SLOT_H);
const POT_LAND = new Float32Array(SLOT_W * SLOT_H);
for (let j = 0; j < SLOT_H; j++) {
  for (let i = 0; i < SLOT_W; i++) {
    const [s, l] = staticPotential(-180 + (i + 0.5) * SLOT, SLOT_LAT0 + (j + 0.5) * SLOT);
    POT_SEA[j * SLOT_W + i] = s;
    POT_LAND[j * SLOT_W + i] = l;
  }
}

/** Convection over land peaks in the late afternoon; over the sea, weakly,
 *  before dawn. */
const landDiurnal = (lst: number) => 0.15 + 1.6 * Math.exp(-((wrap24(lst - 16.5) / 3.5) ** 2));
const seaDiurnal = (lst: number) => 0.85 + 0.25 * Math.cos(((lst - 5) * Math.PI) / 12);
/** A slow eastward-moving envelope of enhanced convection (an MJO). */
const mjo = (lon: number, h: number) => 1 + 0.3 * Math.cos((lon - 120 - 0.25 * h) * RAD);

/** Convective potential at a point and time (0 none .. ~1.5 very active). */
function potential(lon: number, lat: number, utcHour: number, h: number): number {
  if (lat <= SLOT_LAT0 + 1 || lat >= -SLOT_LAT0 - 1) return 0;
  let fx = (lon + 180) / SLOT - 0.5;
  fx -= SLOT_W * Math.floor(fx / SLOT_W);
  if (fx >= SLOT_W) fx = 0;
  const fy = clamp((lat - SLOT_LAT0) / SLOT - 0.5, 0, SLOT_H - 1.0001);
  const i0 = Math.floor(fx);
  const i1 = i0 + 1 === SLOT_W ? 0 : i0 + 1;
  const j0 = Math.floor(fy);
  const tx = fx - i0;
  const ty = fy - j0;
  const a = j0 * SLOT_W;
  const b = a + SLOT_W;
  const sea = mix(mix(POT_SEA[a + i0], POT_SEA[a + i1], tx), mix(POT_SEA[b + i0], POT_SEA[b + i1], tx), ty);
  const land = mix(mix(POT_LAND[a + i0], POT_LAND[a + i1], tx), mix(POT_LAND[b + i0], POT_LAND[b + i1], tx), ty);
  if (sea + land < 0.004) return 0;
  const lst = utcHour + lon / 15;
  return (sea * seaDiurnal(lst) + land * landDiurnal(lst)) * mjo(lon, h);
}

/** Where convection organises into clusters a few hundred km across. */
const organisation = (lon: number, lat: number, z: number) => fbmGeo(lon, lat, z, 0.28, 2, 71.3, -19.7);

// ---------------------------------------------------------------------------
// Weather systems
// ---------------------------------------------------------------------------

interface LowSystem {
  /** Centre at epoch hour 0 and its motion in degrees per hour. */
  lon0: number;
  lat0: number;
  dlon: number;
  dlat: number;
  /** Active window, epoch hours. */
  h0: number;
  h1: number;
  /** Cyclone peak tangential wind, m/s. */
  vmax: number;
  seed: number;
  /** Front vertices in degrees from the centre, in a frame mirrored for the
   *  southern hemisphere so that +y is always poleward. */
  cold?: [number, number][];
  warm?: [number, number][];
  /** Scales the circulation and the shield of rain round the centre. */
  size?: number;
  /** dBZ added to the squall line on the cold front. */
  squall?: number;
  /** Showers in the cold air behind the front, over the sea. */
  showers?: boolean;
  /** How strongly the warm and cold sectors show in the temperature. */
  contrast?: number;
  /** Hours to spin up and to fill. */
  ramp?: number;
  /** The two US lows: crisp-edged rain shields, and sharper and stronger
   *  warm and cold sectors than the rest. */
  classic?: boolean;
  /** Turns the whole system (degrees, counter-clockwise in the mirrored
   *  frame) and scales its fronts' length, so lows are not all alike. */
  rot?: number;
  stretch?: number;
}

const COLD: [number, number][] = [[0, 0], [-2.2, -5.0], [-5.0, -11.0]];
const WARM: [number, number][] = [[0, 0], [6.0, -2.2]];
/** Lows that march east round the Southern Ocean, a new one every couple of
 *  days on each of five tracks, each front bent its own way. */
function southernLows(): LowSystem[] {
  const out: LowSystem[] = [];
  for (let k = 0; k < 5; k++) {
    for (let n = -2; n <= 3; n++) {
      const id = 40 + k * 10 + n;
      const r = (m: number) => hash1(id * 13 + m);
      const birth = -96 + 60 * n + 30 * r(1) + 11 * k;
      const life = 96 + 40 * r(2);
      const dlon = 0.45 + 0.2 * r(3);
      const dlat = -0.03 - 0.03 * r(4);
      const lonB = -150 + 72 * k + 40 * r(5) + 23 * n;
      const latB = -47 - 5 * r(6);
      const bend = 4 * (r(7) - 0.5);
      out.push({
        lon0: lonB - dlon * birth,
        lat0: latB - dlat * birth,
        dlon,
        dlat,
        h0: birth,
        h1: birth + life,
        vmax: 15 + 8 * r(8),
        seed: 100 + id,
        cold: [[0, 0], [-1 + 0.5 * bend, -5], [-3.5 + bend, -10.5], [-7.5 + 0.6 * bend, -15.5], [-12 - bend, -19]],
        warm: [[0, 0], [5, -1.5 + r(9)], [9, -3.5 + 1.5 * r(10)], [13, -6 + 2 * r(11)]],
        size: 1.3 + 0.4 * r(12),
        rot: 40 * (r(13) - 0.5),
        stretch: 0.75 + 0.5 * r(14),
        squall: -10,
        showers: true,
        contrast: 0.5,
        ramp: 24,
      });
    }
  }
  return out;
}

const LOWS: LowSystem[] = [
  { lon0: -96.0, lat0: 42.5, dlon: 0.42, dlat: 0.12, h0: -48, h1: 66, vmax: 12, seed: 11, classic: true },
  // Born over Utah at hour 56 (positions are extrapolated back to hour 0).
  { lon0: -112.0 - 0.4 * 56, lat0: 40.5 - 0.06 * 56, dlon: 0.4, dlat: 0.06, h0: 56, h1: 200, vmax: 11, seed: 23, classic: true },
  // Deep Iceland low running to Norway; the long cold front crosses the UK,
  // France and Iberia from the triple point south of Iceland.
  {
    lon0: -19, lat0: 63.5, dlon: 0.42, dlat: 0.05, h0: -48, h1: 90, vmax: 20, seed: 31,
    cold: [[0, 0], [9, -3], [13.5, -9], [15, -15], [13, -20], [8, -25]],
    warm: [[9, -3], [17, -4], [24, -7]],
    size: 1.6, squall: -8, showers: true, contrast: 0.8, ramp: 18,
  },
  // Mediterranean low over the Tyrrhenian Sea drifting toward Greece.
  {
    lon0: 12.5, lat0: 38.5, dlon: 0.12, dlat: 0.02, h0: -24, h1: 100, vmax: 13, seed: 37,
    cold: [[0, 0], [-1.5, -3.5], [-4, -7]],
    warm: [[0, 0], [4.5, -1], [8, -2]],
    size: 0.8, squall: 0, contrast: 0.6, ramp: 18,
  },
  // Aleutian low on the dateline during the first day, then into the Gulf of
  // Alaska; rain and its winds straddle 180 degrees.
  {
    lon0: 178.2, lat0: 51.8, dlon: 0.2, dlat: 0.02, h0: -60, h1: 140, vmax: 19, seed: 41,
    cold: [[0, 0], [6, -3], [7, -9], [4, -15], [-1, -20]],
    warm: [[6, -3], [13, -4], [19, -6.5]],
    size: 1.5, squall: -8, showers: true, contrast: 0.7, ramp: 18,
  },
  // Weak Siberian low: light snow.
  { lon0: 95, lat0: 58, dlon: 0.25, dlat: -0.02, h0: -24, h1: 110, vmax: 8, seed: 43, size: 1.1, squall: -16, contrast: 0.5, ramp: 18 },
  // Off Japan from midweek, deepening toward the Kurils.
  {
    lon0: 140 - 0.38 * 40, lat0: 36.5 - 0.12 * 40, dlon: 0.38, dlat: 0.12, h0: 40, h1: 190, vmax: 16, seed: 47,
    size: 1.2, squall: -6, showers: true, contrast: 0.8, ramp: 24,
  },
  // Off Newfoundland from hour 70, reaching Ireland at the end of the week.
  {
    lon0: -50 - 0.45 * 70, lat0: 46 - 0.06 * 70, dlon: 0.45, dlat: 0.06, h0: 70, h1: 220, vmax: 18, seed: 53,
    cold: [[0, 0], [-1.5, -6], [-5, -13], [-10, -18]],
    warm: [[0, 0], [8, -2.5], [13, -5]],
    size: 1.4, squall: -8, showers: true, contrast: 0.8, ramp: 24,
  },
  ...southernLows(),
];

function envelope(h: number, h0: number, h1: number, ramp = 12) {
  return smoothstep(h0, h0 + ramp, h) * (1 - smoothstep(h1 - ramp, h1, h));
}

/** A front in km in the mirrored frame, with the arc length to each vertex. */
interface Polyline {
  pts: [number, number][];
  len: number[];
  total: number;
}

function polyline(deg: [number, number][], lat: number, stretch: number, rot: number): Polyline {
  const k = KM * Math.cos(lat * RAD);
  const c = Math.cos(rot * RAD) * stretch;
  const s = Math.sin(rot * RAD) * stretch;
  const pts = deg.map(([x, y]) => [x * k * c - y * KM * s, x * k * s + y * KM * c] as [number, number]);
  const len: number[] = [0];
  for (let i = 1; i < pts.length; i++) len.push(len[i - 1] + Math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]));
  return { pts, len, total: len[len.length - 1] };
}

function prepareLows(h: number, jitter: number) {
  return LOWS.flatMap((s) => {
    const env = envelope(h, s.h0, s.h1, s.ramp);
    if (env <= 0) return [];
    const lon = wrapLon(s.lon0 + s.dlon * h + jitter);
    const lat = s.lat0 + s.dlat * h + jitter * 0.4;
    const size = s.size ?? 1;
    const rot = s.rot ?? 0;
    const cold = polyline(s.cold ?? COLD, lat, s.stretch ?? 1, rot);
    const warm = polyline(s.warm ?? WARM, lat, s.stretch ?? 1, rot);
    const xs = [...cold.pts, ...warm.pts].map((p) => p[0]);
    const ys = [...cold.pts, ...warm.pts].map((p) => p[1]);
    const margin = Math.max(300 + 520 * size, s.showers ? 1400 * size : 0);
    return [
      {
        lon,
        lat,
        /** km per degree of longitude at the centre (offsetKm's scale). */
        kx: KM * Math.cos(lat * RAD),
        /** +1 north, -1 south: multiplies y to get the mirrored frame. */
        hemi: lat < 0 ? -1 : 1,
        env,
        vmax: s.vmax,
        seed: s.seed,
        size,
        squall: s.squall ?? 0,
        showers: s.showers ?? false,
        contrast: s.contrast ?? 1,
        classic: s.classic ?? false,
        /** Degrees the system is turned by (see LowSystem). */
        rot,
        cold,
        warm,
        /** Where its rain can be, km in the mirrored frame. */
        x0: Math.min(...xs) - margin,
        x1: Math.max(...xs) + margin,
        y0: Math.min(...ys) - margin,
        y1: Math.max(...ys) + margin,
      },
    ];
  });
}
type PreparedLow = ReturnType<typeof prepareLows>[number];

/** Nearest point of a polyline: distance, arc-length fraction and side
 *  (+1 left). Written into NEAR rather than returned, since it runs for
 *  every sample near every low and garbage would cost more than the maths. */
const NEAR = { d: 0, s: 0, side: 1 };
const SEG_D = new Float64Array(16);
const SEG_S = new Float64Array(16);
function nearest({ pts, len, total }: Polyline, px: number, py: number) {
  let best = Infinity;
  const n = pts.length - 1;
  for (let i = 1; i <= n; i++) {
    const ax = pts[i - 1][0];
    const ay = pts[i - 1][1];
    // Same arithmetic as segmentDistance(), plus the position and side.
    const dx = pts[i][0] - ax;
    const dy = pts[i][1] - ay;
    const L = dx * dx + dy * dy;
    const t = L > 0 ? clamp(((px - ax) * dx + (py - ay) * dy) / L, 0, 1) : 0;
    const ex = px - (ax + t * dx);
    const ey = py - (ay + t * dy);
    const d = Math.sqrt(ex * ex + ey * ey);
    SEG_D[i - 1] = d;
    SEG_S[i - 1] = (len[i - 1] + t * (len[i] - len[i - 1])) / total;
    if (d < best) {
      best = d;
      NEAR.side = dx * (py - ay) - dy * (px - ax) >= 0 ? 1 : -1;
    }
  }
  NEAR.d = best;
  // Inside a bend two segments are equally near and the nearest foot point
  // jumps from one to the other, so anything keyed on the position along the
  // front would step. Blend the positions of the nearly-nearest instead.
  let ws = 0;
  let w = 0;
  for (let k = 0; k < n; k++) {
    const e = SEG_D[k] - best;
    if (e > 250) continue;
    const wk = Math.exp(-e / 25);
    ws += wk * SEG_S[k];
    w += wk;
  }
  NEAR.s = ws / w;
}

/** Signed distance (km, positive ahead/east) to a low's cold front and the
 *  arc-length fraction of the nearest point, plus the signed distance to the
 *  warm front (positive poleward of it) and the fraction along it. Offsets
 *  are in the low's mirrored frame. The result is reused by the next call. */
const FRONTS = { cold: 0, s: 0, warm: 0, warmT: 0 };
function frontDistances(L: PreparedLow, px: number, py: number) {
  nearest(L.cold, px, py);
  FRONTS.cold = NEAR.d * NEAR.side;
  FRONTS.s = NEAR.s;
  nearest(L.warm, px, py);
  FRONTS.warm = NEAR.d * NEAR.side;
  FRONTS.warmT = NEAR.s;
  return FRONTS;
}

/** Signed distance (km, positive to the left) to a front with its two ends
 *  carried on as straight rays. Past the end of a clamped polyline the sign
 *  flips along the front's extension, which would draw a long straight edge
 *  in the temperature; on a ray it passes smoothly through zero instead. */
function sideOf(pts: [number, number][], px: number, py: number): number {
  let best = Infinity;
  let signed = 0;
  const last = pts.length - 1;
  for (let i = 1; i <= last; i++) {
    const [ax, ay] = pts[i - 1];
    const [bx, by] = pts[i];
    const dx = bx - ax;
    const dy = by - ay;
    const L = dx * dx + dy * dy;
    let t = L > 0 ? ((px - ax) * dx + (py - ay) * dy) / L : 0;
    if (i > 1 && t < 0) t = 0;
    if (i < last && t > 1) t = 1;
    const ex = px - (ax + t * dx);
    const ey = py - (ay + t * dy);
    const d = Math.sqrt(ex * ex + ey * ey);
    if (d < best) {
      best = d;
      signed = dx * (py - ay) - dy * (px - ax) >= 0 ? d : -d;
    }
  }
  return signed;
}

/** Typhoon track, [hour, lon, lat]: from the Philippine Sea toward northern
 *  Luzon, then across the South China Sea. */
const TYPHOON: [number, number, number][] = [
  [-48, 138.5, 12.2], [-24, 135.5, 13.0], [0, 132.5, 14.0], [24, 129.2, 15.1], [48, 126.0, 16.4], [72, 122.8, 17.9],
  [96, 119.8, 19.5], [120, 117.0, 21.0], [144, 114.4, 22.3], [168, 112.0, 23.5], [192, 109.8, 24.5],
];

function typhoonAt(h: number): [number, number] {
  const t = clamp((h - TYPHOON[0][0]) / 24, 0, TYPHOON.length - 1.0001);
  const i = Math.floor(t);
  const f = t - i;
  const p = (k: number) => TYPHOON[clamp(k, 0, TYPHOON.length - 1)];
  return [
    catmull(p(i - 1)[1], p(i)[1], p(i + 1)[1], p(i + 2)[1], f),
    catmull(p(i - 1)[2], p(i)[2], p(i + 1)[2], p(i + 2)[2], f),
  ];
}

function prepareCyclone(h: number, jitter: number) {
  const env = envelope(h, -60, 190, 18);
  if (env <= 0) return null;
  const [lon, lat] = typhoonAt(h);
  const [lon2, lat2] = typhoonAt(h + 1);
  // Peaks over the Philippine Sea, weakens over Luzon.
  const vmax = 30 + 17 * smoothstep(-24, 24, h) - 15 * smoothstep(66, 100, h);
  return {
    lon: lon + jitter * 3,
    lat: lat + jitter * 1.5,
    env,
    vmax,
    /** Radius of maximum wind, km. */
    rm: 32,
    /** Motion, m/s east and north. */
    mu: ((lon2 - lon) * KM * 1000 * Math.cos(lat * RAD)) / 3600,
    mv: ((lat2 - lat) * KM * 1000) / 3600,
    /** Turns the spiral bands slowly. */
    spin: 0.15 * h,
  };
}

type Centre = { lon: number; lat: number; hemi: number; strength: number; kind: 'low' | 'cyclone' };

/** Centres of the lows and the typhoon at a time, for tests and read-outs:
 *  `hemi` is -1 for southern-hemisphere lows (which turn clockwise). */
export function lowCentres(ms: number): Centre[] {
  const h = hoursSinceEpoch(ms);
  const out = prepareLows(h, 0).map((L): Centre => ({ lon: L.lon, lat: L.lat, hemi: L.hemi, strength: L.vmax * L.env, kind: 'low' }));
  const c = prepareCyclone(h, 0);
  if (c) out.push({ lon: c.lon, lat: c.lat, hemi: 1, strength: c.vmax * c.env, kind: 'cyclone' });
  return out;
}

// ---------------------------------------------------------------------------
// Convective cells
// ---------------------------------------------------------------------------

interface Cell {
  lon: number;
  lat: number;
  peak: number;
  rKm: number;
  /** Multiplies the cell's reflectivity (life cycle). */
  amp: number;
  /** Long axis over short (1 for round) and the long axis's angle, radians. */
  aspect?: number;
  angle?: number;
}

/** Short-lived pop-up storms in the warm, moist South-east. */
function popupCells(h: number, out: Cell[]) {
  const N = 700;
  for (let i = 0; i < N; i++) {
    const birth = -72 + hash1(i * 3 + 1) * 260;
    const life = 1.2 + hash1(i * 3 + 2) * 2.5;
    const age = h - birth;
    if (age < 0 || age > life) continue;
    const phase = Math.sin((Math.PI * age) / life) ** 0.7;
    const lon = -99 + hash1(i * 5 + 7) * 21 + 0.35 * age;
    const lat = 27.5 + hash1(i * 5 + 9) * 9 + 0.1 * age;
    out.push({ lon, lat, peak: 38 + hash1(i * 7 + 5) * 20 * phase, rKm: 5 + hash1(i * 11 + 3) * 9, amp: 1 });
  }
}

/** Afternoon sea-breeze showers over the Florida peninsula. */
function floridaCells(ms: number, out: Cell[]) {
  const day = Math.floor(ms / (24 * HOUR));
  const localHour = (((ms / HOUR) % 24) - 5 + 24) % 24;
  const diurnal = smoothstep(11, 14, localHour) * (1 - smoothstep(18, 21, localHour));
  if (diurnal <= 0) return;
  for (let i = 0; i < 9; i++) {
    const k = day * 31 + i;
    out.push({
      lon: -81.9 + hash1(k * 3 + 1) * 1.6,
      lat: 26.2 + hash1(k * 3 + 2) * 3.6,
      peak: 35 + 20 * diurnal * hash1(k * 3 + 4),
      rKm: 5 + 6 * hash1(k * 7 + 1),
      amp: 1,
    });
  }
}

/** Cells per slot. Each slot runs a cycle of storms, one after another; a
 *  storm is born where the potential and the organisation say so. */
const SLOT_K = 3;

/** Tropical and subtropical convective cells over the whole world. */
function tropicalCells(h: number, utcHour: number, seed: number, orgShift: number, out: Cell[]) {
  for (let j = 0; j < SLOT_H; j++) {
    const latC = SLOT_LAT0 + (j + 0.5) * SLOT;
    // Steering: drift west with the trades, east in the subtropical westerlies.
    const su = -0.045 + 0.1 * smoothstep(18, 30, Math.abs(latC));
    for (let i = 0; i < SLOT_W; i++) {
      const sea = POT_SEA[j * SLOT_W + i];
      const land = POT_LAND[j * SLOT_W + i];
      if (sea + land < 0.015) continue;
      const lonC = -180 + (i + 0.5) * SLOT;
      const landy = land / (sea + land);
      for (let k = 0; k < SLOT_K; k++) {
        const id = (j * SLOT_W + i) * SLOT_K + k;
        const T = 2.5 + 3 * hash3(id, seed, 1);
        const phase0 = hash3(id, seed, 2) * T;
        const cyc = Math.floor((h - phase0) / T);
        const age = h - phase0 - cyc * T;
        const life = T * (0.55 + 0.35 * hash3(id, cyc, seed + 3));
        if (age > life) continue;
        const lonB = lonC + (hash3(id, cyc, seed + 4) - 0.5) * SLOT;
        const latB = latC + (hash3(id, cyc, seed + 5) - 0.5) * SLOT;
        const lstB = utcHour - age + lonB / 15;
        const P = (sea * seaDiurnal(lstB) + land * landDiurnal(lstB)) * mjo(lonB, h - age);
        const roll = hash3(id, cyc, seed + 6);
        // The organisation noise is the dear part: skip it when no value of it
        // could make this storm.
        if (roll >= P * 1.48) continue;
        const org = organisation(lonB, latB, (h - age) * 0.08 + orgShift);
        if (roll >= clamp(P * (0.08 + 1.4 * smoothstep(0.42, 0.72, org)), 0, 0.92)) continue;
        const grow = Math.sin((Math.PI * age) / life) ** 0.7;
        const strong = hash3(id, cyc, seed + 7);
        out.push({
          lon: lonB + su * age,
          lat: latB,
          peak: 37 + 9 * landy + (11 + 6 * landy) * strong * grow,
          rKm: 4 + 7 * hash3(id, cyc, seed + 8) + 3 * landy,
          amp: grow,
          aspect: 1 + 1.3 * hash3(id, cyc, seed + 9),
          angle: Math.PI * hash3(id, cyc, seed + 10),
        });
      }
    }
  }
}

/** Cells bucketed on a 2 degree grid (wrapping in longitude) so a sample
 *  only looks at the few that can reach it. */
interface CellIndex {
  lon: Float64Array;
  lat: Float64Array;
  /** Peak linear reflectivity. */
  z: Float64Array;
  /** The ellipse as a quadratic form: d^2 = qa x^2 + qb x y + qc y^2, km^-2. */
  qa: Float64Array;
  qb: Float64Array;
  qc: Float64Array;
  /** km per degree of longitude at the cell. */
  kx: Float64Array;
  start: Int32Array;
  items: Int32Array;
}

const BK = 2;
const BW = 360 / BK;
const BLAT0 = -60;
const BH = 120 / BK;
/** exp(-12): beyond this a cell adds nothing that shows. Each cell's
 *  contribution has exp(-12) taken off so it meets zero there instead of
 *  stepping (a 60 dBZ core would still be 10 dBZ at the cut). */
const CELL_CUTOFF = 12;
const CELL_FLOOR = Math.exp(-CELL_CUTOFF);

function indexCells(cells: Cell[], rScale: number, peakAdj: number, cap: number): CellIndex {
  const n = cells.length;
  const ix: CellIndex = {
    lon: new Float64Array(n),
    lat: new Float64Array(n),
    z: new Float64Array(n),
    qa: new Float64Array(n),
    qb: new Float64Array(n),
    qc: new Float64Array(n),
    kx: new Float64Array(n),
    start: new Int32Array(BW * BH + 1),
    items: new Int32Array(0),
  };
  const ranges = new Int32Array(n * 4);
  for (let c = 0; c < n; c++) {
    const cell = cells[c];
    const R = cell.rKm * rScale;
    const aspect = cell.aspect ?? 1;
    const a = R * Math.sqrt(aspect);
    const lon = wrapLon(cell.lon);
    const cos = Math.cos(cell.lat * RAD);
    ix.lon[c] = lon;
    ix.lat[c] = cell.lat;
    ix.z[c] = 10 ** (Math.min(cap, cell.peak + peakAdj) / 10) * cell.amp;
    if (aspect === 1) {
      ix.qa[c] = 1 / (R * R);
      ix.qc[c] = 1 / (R * R);
    } else {
      const b = R / Math.sqrt(aspect);
      const ca = Math.cos(cell.angle ?? 0);
      const sa = Math.sin(cell.angle ?? 0);
      ix.qa[c] = (ca * ca) / (a * a) + (sa * sa) / (b * b);
      ix.qb[c] = 2 * ca * sa * (1 / (a * a) - 1 / (b * b));
      ix.qc[c] = (sa * sa) / (a * a) + (ca * ca) / (b * b);
    }
    ix.kx[c] = KM * cos;
    const reach = (Math.sqrt(CELL_CUTOFF) * a) / KM;
    const reachLon = reach / Math.max(0.05, cos);
    ranges[c * 4] = Math.floor((lon - reachLon + 180) / BK);
    ranges[c * 4 + 1] = Math.floor((lon + reachLon + 180) / BK);
    ranges[c * 4 + 2] = Math.max(0, Math.floor((cell.lat - reach - BLAT0) / BK));
    ranges[c * 4 + 3] = Math.min(BH - 1, Math.floor((cell.lat + reach - BLAT0) / BK));
  }
  const visit = (fn: (b: number, c: number) => void) => {
    for (let c = 0; c < n; c++) {
      const i0 = ranges[c * 4];
      const i1 = Math.min(ranges[c * 4 + 1], i0 + BW - 1);
      for (let j = ranges[c * 4 + 2]; j <= ranges[c * 4 + 3]; j++) {
        for (let i = i0; i <= i1; i++) fn(j * BW + (((i % BW) + BW) % BW), c);
      }
    }
  };
  const count = ix.start;
  visit((b) => count[b + 1]++);
  for (let b = 0; b < BW * BH; b++) count[b + 1] += count[b];
  const fill = count.slice(0, BW * BH);
  ix.items = new Int32Array(count[BW * BH]);
  visit((b, c) => {
    ix.items[fill[b]++] = c;
  });
  return ix;
}

/** Sum of the cells' linear reflectivity at a point (lon in [-180, 180)). */
function cellsAt(ix: CellIndex, lon: number, lat: number): number {
  if (lat < BLAT0 || lat >= BLAT0 + BH * BK) return 0;
  const b = Math.floor((lat - BLAT0) / BK) * BW + Math.min(BW - 1, Math.floor((lon + 180) / BK));
  let Z = 0;
  for (let q = ix.start[b], end = ix.start[b + 1]; q < end; q++) {
    const c = ix.items[q];
    let d = lon - ix.lon[c];
    if (d >= 180) d -= 360;
    else if (d < -180) d += 360;
    const dx = d * ix.kx[c];
    const dy = (lat - ix.lat[c]) * KM;
    const d2 = ix.qa[c] * dx * dx + ix.qb[c] * dx * dy + ix.qc[c] * dy * dy;
    if (d2 < CELL_CUTOFF) Z += ix.z[c] * (Math.exp(-d2) - CELL_FLOOR);
  }
  return Z;
}

/** Prepared cell indexes, by time, run and quality: every tile of a frame
 *  (and every step of a rainfall total) asks for the same ones. */
const cellCache = new Map<string, CellIndex>();

function cellIndex(ms: number, h: number, seed: number, orgShift: number, rScale: number, peakAdj: number, cap: number): CellIndex {
  const key = `${ms}|${seed}|${orgShift}|${rScale}|${peakAdj}|${cap}`;
  const hit = cellCache.get(key);
  if (hit) {
    cellCache.delete(key);
    cellCache.set(key, hit);
    return hit;
  }
  const cells: Cell[] = [];
  popupCells(h, cells);
  floridaCells(ms, cells);
  tropicalCells(h, utcHourOf(ms), seed, orgShift, cells);
  const ix = indexCells(cells, rScale, peakAdj, cap);
  cellCache.set(key, ix);
  if (cellCache.size > 48) cellCache.delete(cellCache.keys().next().value as string);
  return ix;
}

/** Lake-effect snow bands: the two ends of each, lon and lat. */
const LAKE_BANDS: [number, number, number, number][] = [
  [-87.0, 43.3, -85.4, 42.7], [-87.2, 41.9, -86.2, 41.55], [-79.5, 42.75, -78.5, 42.9], [-76.9, 43.62, -75.5, 43.6], [-86.0, 46.65, -85.0, 46.4],
];

// ---------------------------------------------------------------------------
// Reflectivity
// ---------------------------------------------------------------------------

export interface RadarField {
  /** dBZ, NO_ECHO when the radar sees nothing. */
  dbz(lon: number, lat: number): number;
}

interface RadarOptions {
  /** Leave out the georeferencing storm (it would swamp rainfall totals). */
  skipGeoref?: boolean;
  /** Run time, for forecasts: different runs place storms slightly differently. */
  runMs?: number;
}

/** How a model run differs from what happened: systems displaced by
 *  `jitter` degrees, growing with lead time; its own convective cells
 *  (`seed`); its clusters drifting off the observed ones (`orgShift`). All
 *  zero for observations. */
function runOffsets(ms: number, runMs: number | undefined): { jitter: number; seed: number; orgShift: number } {
  if (runMs === undefined) return { jitter: 0, seed: 0, orgShift: 0 };
  const runHour = Math.floor(runMs / HOUR);
  const lead = Math.max(0, (ms - runMs) / HOUR);
  return {
    jitter: (hash1(runHour) - 0.5) * 0.12 * Math.min(1, lead / 12),
    seed: 1 + Math.floor(hash1(runHour * 3 + 1) * 1e6),
    orgShift: Math.round((hash1(runHour * 3 + 2) - 0.5) * 0.8 * Math.min(1, lead / 24) * 1e4) / 1e4,
  };
}

export function prepareRadar(ms: number, quality: Quality, opts: RadarOptions = {}): RadarField {
  const h = hoursSinceEpoch(ms);
  const utcHour = utcHourOf(ms);
  const { jitter, seed, orgShift } = runOffsets(ms, opts.runMs);
  const lows = prepareLows(h, jitter);
  const cyclone = prepareCyclone(h, jitter);
  const lakeEnv = envelope(h, -18, 50);
  const sat = quality === 'sat';
  const coarse = quality === 'gfs';
  const smooth = coarse || sat;
  const octaves = quality === 'obs' ? 4 : quality === 'hrrr' ? 3 : 2;
  const rScale = coarse ? 2.6 : sat ? 1.8 : 1;
  const cap = coarse ? 49 : sat ? 47 : 75;
  const top = sat ? 50 : 78;
  const cells =cellIndex(ms, h, seed, orgShift, rScale, coarse ? -8 : sat ? -3 : 0, cap);
  const orgZ = h * 0.08 + orgShift;
  const pacific = {
    lon: -125.8 + 0.3 * h + jitter,
    lat: 45.5 - 0.02 * h,
    env: envelope(h, -36, 62),
  };
  const Zc = (dbz: number) => 10 ** (Math.min(cap, dbz) / 10);

  // Texture noise is the dearest part of a sample, and most of the world is
  // dry: work it out only when something needs it.
  let sLon = 0;
  let sLat = 0;
  let have = false;
  let n = 0.5;
  let fine = 0.5;
  let texture = 1;
  const noise = () => {
    if (have) return;
    have = true;
    // Two scales of texture: broad bands of heavier rain, and the ragged,
    // cellular detail that keeps echoes from looking like airbrushed blobs.
    n = fbmGeo(sLon, sLat, h * 0.12, 1.25, octaves);
    fine = smooth ? 0.5 : fbmGeo(sLon, sLat, h * 0.45, 4.6, 3, 31.7, -12.3);
    texture = 10 ** ((n - 0.5) * 1.9 + (fine - 0.5) * 1.9);
  };

  /** Linear reflectivity of everything but the georeferencing storm. */
  const weather = (lon: number, lat: number): number => {
    sLon = lon;
    sLat = lat;
    have = false;
    let Z = 0;

    for (const L of lows) {
      const py = (lat - L.lat) * KM * L.hemi;
      if (py < L.y0 || py > L.y1) continue;
      const px = wrapLon(lon - L.lon) * L.kx;
      if (px < L.x0 || px > L.x1) continue;
      const s = L.size;
      const f = frontDistances(L, px, py);
      if (!L.classic) {
        // Ragged band edges: the front's rain wanders a few tens of km.
        noise();
        f.cold += 70 * (n - 0.5);
        f.warm += 90 * s * (n - 0.5);
      }
      // Squall line on the equatorward part of the cold front, with gaps. It
      // fades out before the front's far end: past the end the side of the
      // front is undefined, and the band would stop along a hard line.
      if (f.s > 0.22 && f.s < 1 && f.cold > -160 && f.cold < 90 * rScale) {
        noise();
        const along = valueNoise(f.s * 14 + L.seed, h * 0.2, 3.1);
        const tail = 1 - smoothstep(0.88, 1, f.s);
        const strength = smoothstep(0.22, 0.32, f.s) * smoothstep(0.25, 0.55, along) * tail;
        const core = Zc(44 + 14 * along + L.squall) * Math.exp(-(((f.cold - 18) / (11 * rScale)) ** 2)) * strength * (0.35 + 1.3 * fine);
        const trailing = Zc(31) * smoothstep(-150, -40, f.cold) * (1 - smoothstep(-25, 5, f.cold)) * smoothstep(0.22, 0.4, f.s) * tail;
        Z += (core * (0.6 + 0.8 * n) + trailing * texture) * L.env;
      }
      // Wrap-around precipitation on the cold, poleward side of the low.
      const r = Math.sqrt(px * px + py * py);
      const rOut = 520 * s * (L.classic ? 1 : 1.25);
      if (r > 40 * s && r < rOut) {
        // Angle from east, counter-clockwise, unwrapped in the middle of the
        // dry sector (south-east) so the wet range is one continuous span
        // even after the ragged-edge shift below.
        const raw = (Math.atan2(py, px) * 180) / Math.PI - L.rot;
        let theta = raw < -37.5 ? raw + 360 : raw >= 322.5 ? raw - 360 : raw;
        let edge = 1;
        if (!L.classic) {
          // Ragged edges: the shield's ends and rim wander with the texture.
          noise();
          theta += 50 * (n - 0.5);
          edge = 0.75 + 0.5 * n;
        }
        const ang = smoothstep(35, 75, theta) * (1 - smoothstep(205, 250, theta));
        const radial = smoothstep(40 * s, 140 * s, r) * (1 - smoothstep(330 * s * edge, 520 * s * edge, r));
        if (ang > 0) {
          noise();
          Z += Zc(L.classic ? 30 : 28) * ang * radial * texture * L.env;
        }
      }
      // Overrunning rain poleward of the warm front.
      if (f.warm > 0 && f.warm < 260 * s && f.warmT > 0.03) {
        noise();
        const band = smoothstep(0, 40, f.warm) * (1 - smoothstep(120 * s, 260 * s, f.warm)) * smoothstep(0.03, 0.2, f.warmT);
        Z += Zc(29) * band * texture * L.env * (1 - smoothstep(0.85, 1, f.warmT));
      }
      // Showers in the cold air streaming equatorward behind the front, over
      // open water, in clusters.
      if (L.showers && r < 1400 * s && py < 500 * s) {
        // Measured from the front carried on past its ends, which has no
        // edge where the clamped distance changes side.
        noise();
        const behind = sideOf(L.cold.pts, px, py) + 70 * (n - 0.5);
        const where = smoothstep(-150, -400, behind) * (1 - smoothstep(900 * s, 1400 * s, r)) * (1 - smoothstep(0, 500 * s, py));
        const sea = where > 0 ? 1 - landWeight(lon, lat) : 0;
        if (sea > 0.05) {
          noise();
          // Shower cells 10-30 km across, in clusters, moving with the flow.
          const cells = fbmGeo(lon, lat, h * 0.35, coarse ? 0.9 : 2.3, 2, 13.7 - h * 0.05, 5.3);
          const cellular = smoothstep(0.42, 0.6, n) * smoothstep(coarse ? 0.52 : 0.6, coarse ? 0.7 : 0.74, cells);
          Z += Zc(coarse ? 26 : 34) * cellular * where * sea * L.env * (smooth ? 0.6 : 0.4 + 1.2 * fine);
        }
      }
    }

    if (cyclone) {
      const [px, py] = offsetKm(lon, lat, cyclone.lon, cyclone.lat);
      const r2 = px * px + py * py;
      if (r2 < 650 * 650) {
        noise();
        const r = Math.sqrt(r2);
        const rm = cyclone.rm * (coarse ? 2 : sat ? 1.5 : 1);
        // A clear eye inside the radius of maximum wind, the eyewall just
        // outside it, and two spiral bands trailing out clockwise.
        const eye = smoothstep(0.45 * rm, 0.85 * rm, r);
        const wall = Math.exp(-(((r - 1.2 * rm) / (0.45 * rm * rScale)) ** 2));
        const theta = Math.atan2(py, px);
        const arm = Math.cos(2 * (theta + 2.6 * Math.log(Math.max(r, 1) / rm) - cyclone.spin)) + 0.7 * (n - 0.5);
        const bands = smoothstep(0.2, 0.9, arm) * smoothstep(50, 110, r) * (1 - smoothstep(300, 620, r));
        const inner = Math.exp(-((r / 170) ** 2));
        const k = cyclone.env * smoothstep(20, 40, cyclone.vmax);
        Z += (Zc(47 + 6 * n) * wall * (0.6 + 0.8 * n) + Zc(37) * bands * texture + Zc(30) * inner * texture) * eye * k;
      }
    }

    if (pacific.env > 0) {
      const [px, py] = offsetKm(lon, lat, pacific.lon, pacific.lat);
      const a = 0.34;
      const xr = px * Math.cos(a) - py * Math.sin(a);
      const yr = px * Math.sin(a) + py * Math.cos(a);
      const d2 = (xr / 170) ** 2 + (yr / 360) ** 2;
      if (d2 < 9) {
        noise();
        const orographic = 1 + 1.8 * Math.exp(-(((lon + 121.9) / 0.7) ** 2)) * smoothstep(40, 44, lat);
        Z += Zc(31) * Math.exp(-d2 * 1.4) * texture * orographic * pacific.env;
      }
    }

    // The bands widen with rScale, and so must the cut-off and the box, or a
    // coarse model's bands would end along a line.
    if (lakeEnv > 0 && lat > 39.5 && lat < 49 && lon > -90 && lon < -73) {
      noise();
      const reach = Math.max(60, 4.5 * 13 * rScale);
      for (const [ax, ay, bx, by] of LAKE_BANDS) {
        const [qx, qy] = offsetKm(lon, lat, ax, ay);
        const [ex, ey] = offsetKm(bx, by, ax, ay);
        const d = segmentDistance(qx, qy, 0, 0, ex, ey);
        if (d < reach) Z += Zc(33 + 6 * n) * Math.exp(-((d / (13 * rScale)) ** 2)) * lakeEnv * (0.4 + 1.2 * fine);
      }
    }

    // Stratiform rain of the tropical clusters.
    const P = potential(lon, lat, utcHour, h);
    if (P > 0.03) {
      const org = organisation(lon, lat, orgZ);
      if (org > 0.5) {
        noise();
        // Ragged by the texture noise, so clusters are not smooth outlines.
        const q = P * smoothstep(0.6, 0.85, org + 0.16 * (n - 0.5));
        if (q > 0.06) Z += Zc(24 + 9 * Math.min(1, q)) * smoothstep(0.06, 0.35, q) * texture;
      }
    }

    const Zcells = cellsAt(cells, lon, lat);
    if (Zcells > 0) {
      noise();
      Z += Zcells * (0.5 + fine);
    }
    return Z;
  };

  return {
    dbz(lon0, lat) {
      const lon = wrapLon(lon0);
      let Z: number;
      if (sat) {
        // Satellite estimates land a little off the radar's echoes:
        // parallax, advection between scans, and the retrieval itself.
        const dx = 0.1 + 0.3 * (fbmGeo(lon, lat, h * 0.05, 0.15, 1, 3.3, 7.7) - 0.5);
        const dy = 0.06 + 0.3 * (fbmGeo(lon, lat, h * 0.05, 0.15, 1, -5.1, 2.9) - 0.5);
        Z = weather(wrapLon(lon - dx), clamp(lat - dy, -90, 90));
      } else {
        Z = weather(lon, lat);
      }

      if (!opts.skipGeoref) {
        // Never displaced, so the storm stays on the city in every source.
        const [dx, dy] = offsetKm(lon, lat, GEOREF_CITY.lon, GEOREF_CITY.lat);
        const d2 = dx * dx + dy * dy;
        if (d2 < 120 * 120) {
          if (sat) {
            sLon = lon;
            sLat = lat;
            have = false;
          }
          noise();
          const R = 9 * rScale;
          // A crisp core exactly on the city (the georeferencing check needs a
          // sharp centre) inside a ragged anvil.
          Z += Zc(coarse ? 50 : 63) * Math.exp(-d2 / (R * R)) + Zc(25) * Math.exp(-d2 / (34 * 34)) * texture;
        }
      }

      if (Z < 10 ** 0.6) return NO_ECHO;
      return Math.min(top, 10 * Math.log10(Z));
    },
  };
}

/** Marshall-Palmer Z = 200 R^1.6, capped at 53 dBZ so hail does not count as
 *  rain, and tapered to zero below 16 dBZ: model rates are continuous, and a
 *  rate that jumped from 0 would draw the sample grid along every edge. */
export function rainRate(dbz: number): number {
  if (dbz <= 4) return 0;
  return (10 ** (Math.min(dbz, 53) / 10) / 200) ** (1 / 1.6) * smoothstep(4, 16, dbz);
}

// ---------------------------------------------------------------------------
// Temperature, dew point, wind
// ---------------------------------------------------------------------------

export interface SurfaceField {
  /** Air temperature, °C. */
  temp(lon: number, lat: number): number;
  /** Dew point, °C. */
  dewpoint(lon: number, lat: number): number;
  /** Earth-relative wind, m/s, and gust. */
  wind(lon: number, lat: number): { u: number; v: number; gust: number };
  /** Total cloud cover, %. */
  clouds(lon: number, lat: number, dbz: number): number;
}

/**
 * `MOCK_WIND=westerly` (10 m/s from the west everywhere) or `MOCK_WIND=vortex`
 * (counter-clockwise round Kansas, 15 m/s at 600 km) replace the wind with a
 * field whose drawing is easy to judge by eye.
 */
function syntheticWind(lon: number, lat: number): { u: number; v: number; gust: number } | null {
  const pattern = process.env.MOCK_WIND;
  if (pattern === 'westerly') return { u: 10, v: 0, gust: 14 };
  if (pattern !== 'vortex') return null;
  const [px, py] = offsetKm(lon, lat, -98, 38.5);
  const r = Math.max(1, Math.sqrt(px * px + py * py));
  const R = 600;
  const vt = 15 * (r < R ? r / R : R / r);
  return { u: (-vt * py) / r, v: (vt * px) / r, gust: vt * 1.4 };
}

/** How much the hand-tuned US terms count: 1 over the contiguous US, fading
 *  to 0 a few degrees beyond its borders, where the global terms take over. */
const usWeight = (lon: number, lat: number) => 1 - smoothstep(0.5, 4, conusDistance(lon, lat));

/** Hand-tuned US November climatology, without the daily cycle. */
function usTemp(lon: number, lat: number): number {
  const base = 24.5 - 0.95 * (lat - 25);
  const rockies = -9 * Math.exp(-(((lon + 108.5) / 4.5) ** 2)) * smoothstep(31, 37, lat);
  const basin = -3 * Math.exp(-(((lon + 116) / 3) ** 2)) * smoothstep(36, 42, lat);
  const appalachia = -2 * Math.exp(-(((lon + 80.5) / 1.6) ** 2)) * smoothstep(34, 37, lat) * (1 - smoothstep(41, 44, lat));
  const pacificMild = 5 * Math.exp(-(((lon + 124.5) / 2.5) ** 2)) * smoothstep(38, 47, lat);
  const gulf = 2.5 * Math.exp(-(((lat - 26.5) / 2.2) ** 2));
  const arctic = -7 * smoothstep(43.5, 50, lat) * mix(0.45, 1, 1 - smoothstep(-90, -86, lon)) * mix(0.3, 1, smoothstep(-117, -113, lon));
  return base + rockies + basin + appalachia + pacificMild + gulf + arctic;
}

const usDryness = (lon: number, lat: number) => clamp(Math.exp(-(((lon + 111) / 9) ** 2)) * smoothstep(28, 34, lat), 0, 1);

/** Over sea by latitude, every 10 degrees from -90: November. */
const T_SEA = [-30, -20, -9, -1, 5, 11, 17, 22, 25.5, 27, 27, 25, 21, 15, 8, 2, -7, -18, -22];
/** Land minus sea: continents cold in the north, warm in the southern spring. */
const T_LAND = [0, 0, 0, 0, 1.5, 3, 4, 4, 0.5, 0, -0.5, -1.5, -4, -6, -8, -12, -12, -8, -8];

/** Rows of [lon, lat, rx, ry, °C, over land, over sea]. */
const T_REGIONS = Float64Array.from(
  [
    [12, 51, 28, 12, 8, 1, 0.5], // maritime Europe
    [78, 23, 12, 8, 3, 1, 0], // the Indian plains
    [-12, 62, 25, 10, 4, 0.6, 1], // North Atlantic Drift
    [120, 63, 35, 10, -15, 1, 0.4], // Siberia
    [115, 40, 18, 8, -3, 1, 0.5], // East Asian winter monsoon
    [-105, 63, 30, 9, -5, 1, 0.5], // northern Canada
    [-150, 64, 8, 4, -4, 1, 0.2], // interior Alaska
    [-85, 60, 8, 5, -10, 0.3, 1], // Hudson Bay freezing over
    [133, -25, 14, 8, 1, 1, 0], // Australian spring
    [130, 5, 40, 12, 1, 0, 1], // the warm pool
    [-82, -15, 6, 10, -4, 0, 1], // Humboldt upwelling
    [10, -22, 5, 8, -3, 0, 1], // Benguela upwelling
  ].flat(),
);

/** Daily-mean temperature from latitude, land, elevation and region. */
function globalTemp(lon: number, lat: number, land: number): number {
  let t = zonal(T_SEA, lat) + land * zonal(T_LAND, lat);
  const R = T_REGIONS;
  for (let i = 0; i < R.length; i += 7) {
    const b = blob(lon, lat, R[i], R[i + 1], R[i + 2], R[i + 3]);
    if (b > 0) t += R[i + 4] * b * (R[i + 5] * land + R[i + 6] * (1 - land));
  }
  return t - 5 * elevationKm(lon, lat, land);
}

/** Half the daily range: little at sea or in the polar night, most over
 *  deserts. The sun is over 16 degrees south in early November. */
function globalAmp(lat: number, land: number, dry: number): number {
  const sun = clamp(Math.cos((lat + 16) * RAD) * 1.3, 0, 1);
  return sun * (0.6 + land * (3.6 + 5 * dry));
}

/** Pushes the cloud noise up or down: storm tracks, the ITCZ and the
 *  stratocumulus decks are cloudier, deserts and the subtropical highs clearer. */
function cloudBias(lon: number, lat: number, land: number, deck = land < 1 ? maxBump(lon, lat, DECKS) * (1 - land) : 0): number {
  const a = Math.abs(lat);
  let b = 0.12 * smoothstep(38, 50, a) * (1 - smoothstep(65, 78, a)) * (lat < 0 ? 1.4 : 1);
  b += 0.2 * deck;
  b -= 0.3 * dryness(lon, lat, land);
  b -= 0.1 * highness(lon, lat) * (1 - land);
  b += 0.06 * smoothstep(65, 80, lat);
  b -= 0.15 * (1 - smoothstep(-82, -70, lat));
  return b;
}

/** Zonal-mean surface wind by latitude, every 10 degrees from -90: trades,
 *  westerlies (strongest in the Southern Ocean), polar easterlies. */
const U_ZONAL = [0, -2, -5, 3, 8, 6, 0.5, -4, -4, -2.5, -4.5, -4, 0, 3.5, 4.5, 1, -2, -1, 0];
/** Meridional: the trades converge on the ITCZ. */
const V_ZONAL = [0, 1, 1, -1, -1, -0.5, 0.5, 2, 2.5, 0.5, -2.5, -2, -0.5, 0.5, 1, 0.5, -0.5, 0, 0];

export function prepareSurface(ms: number, quality: Quality): SurfaceField {
  const h = hoursSinceEpoch(ms);
  const lows = prepareLows(h, 0);
  const cyclone = prepareCyclone(h, 0);
  const utcHour = utcHourOf(ms);
  const octaves = quality === 'gfs' ? 2 : 3;

  // Dew point asks for the same point right after temperature does, so the
  // per-point terms are kept for the last point asked.
  let atLon = Number.NaN;
  let atLat = Number.NaN;
  let atLand = 0;
  let atUs = 0;
  let atDry = 0;
  let secAnomaly = 0;
  let secDry = 0;
  const at = (lon: number, lat: number) => {
    if (lon === atLon && lat === atLat) return;
    atLon = lon;
    atLat = lat;
    atLand = landWeight(lon, lat);
    atUs = usWeight(lon, lat);
    atDry = atUs < 1 ? dryness(lon, lat, atLand) : 0;
    sectors(lon, lat);
  };

  const sectors = (lon: number, lat: number) => {
    let anomaly = 0;
    let dry = 0;
    for (const L of lows) {
      const R = (L.classic ? 1500 : 1200) * L.size;
      // exp(-x^2) falls below 1% at x = 2.146.
      const py = (lat - L.lat) * KM * L.hemi;
      if (py > 2.146 * R || py < -2.146 * R) continue;
      const px = wrapLon(lon - L.lon) * L.kx;
      const r = Math.sqrt(px * px + py * py);
      // Less 1%, so the cut-off below meets zero instead of stepping.
      const env = (Math.exp(-((r / R) ** 2)) - 0.01) * L.env;
      if (env <= 0) continue;
      if (L.classic) {
        const warm = smoothstep(-70, 70, sideOf(L.cold.pts, px, py)) * (1 - smoothstep(-90, 90, sideOf(L.warm.pts, px, py)));
        anomaly += (warm * 4 - (1 - warm) * 8.5) * env;
        dry += (1 - warm) * env;
      } else {
        // Warm sector ahead of the cold front, cold air behind it, and only a
        // little cooling poleward of the warm front; fronts a model's width.
        const ahead = smoothstep(-150, 150, sideOf(L.cold.pts, px, py));
        const equatorward = 1 - smoothstep(-150, 150, sideOf(L.warm.pts, px, py));
        const warm = ahead * equatorward;
        anomaly += (4 * warm - 7 * (1 - ahead) - 2 * ahead * (1 - equatorward)) * env * L.contrast;
        dry += (1 - ahead) * env;
      }
    }
    secAnomaly = anomaly;
    secDry = dry;
  };

  const cycloneR = (lon: number, lat: number) => {
    if (!cyclone) return Infinity;
    const [px, py] = offsetKm(lon, lat, cyclone.lon, cyclone.lat);
    return Math.sqrt(px * px + py * py);
  };

  const temp = (lon0: number, lat: number) => {
    const lon = wrapLon(lon0);
    at(lon, lat);
    const land = atLand;
    const w = atUs;
    let base = 0;
    let amp = 0;
    if (w < 1) {
      base = globalTemp(lon, lat, land);
      amp = globalAmp(lat, land, atDry);
    }
    if (w > 0) {
      base = mix(base, usTemp(lon, lat), w);
      amp = mix(amp, 1.2 + land * (3.8 + 4 * usDryness(lon, lat)), w);
    }
    const diurnal = amp * Math.cos(((utcHour + lon / 15 - 15) * Math.PI) / 12);
    const noise = 2.2 * (fbmGeo(lon, lat, h * 0.02, 0.35, octaves) - 0.5);
    const rain = cyclone ? -2 * Math.exp(-((cycloneR(lon, lat) / 250) ** 2)) * cyclone.env : 0;
    return clamp(base + diurnal + secAnomaly + noise + rain, -59, 49);
  };

  const dewpoint = (lon0: number, lat: number) => {
    const lon = wrapLon(lon0);
    const t = temp(lon, lat);
    const land = atLand;
    const w = atUs;
    let dep = w < 1 ? 2.2 + 19 * atDry + 2.5 * highness(lon, lat) * (1 - land) + 1.5 * land : 0;
    if (w > 0) {
      const gulf = Math.exp(-(((lat - 28) / 4) ** 2)) * smoothstep(-102, -95, lon);
      const dry = clamp(
        Math.exp(-(((lon + 112) / 9) ** 2)) * smoothstep(28, 34, lat) + 0.55 * Math.exp(-(((lon + 103) / 3.5) ** 2)),
        0,
        1,
      );
      dep = mix(dep, 2.5 + 15 * dry - 2 * gulf, w);
    }
    const afternoon = Math.max(0, Math.cos(((utcHour + lon / 15 - 15) * Math.PI) / 12));
    dep += 5 * secDry + 3.5 * afternoon * mix(0.25 + 0.75 * land, 1, w) + 2 * (fbmGeo(lon, lat, h * 0.03, 0.5, 2) - 0.5);
    if (cyclone) dep *= 1 - 0.7 * Math.exp(-((cycloneR(lon, lat) / 500) ** 2)) * cyclone.env;
    return t - Math.max(0.4, dep);
  };

  const wind = (lon0: number, lat: number) => {
    const known = syntheticWind(lon0, lat);
    if (known) return known;
    const lon = wrapLon(lon0);
    const w = usWeight(lon, lat);
    let u = zonal(U_ZONAL, lat);
    let v = zonal(V_ZONAL, lat);
    // North-east monsoon over southern Asia.
    const monsoon = blob(lon, lat, 95, 14, 25, 10);
    u -= 2.5 * monsoon;
    v -= 3 * monsoon;
    for (const H of HIGHS) {
      const hemi = H.lat < 0 ? -1 : 1;
      const py = (lat - H.lat) * KM * hemi;
      if (py > 4 * H.R || py < -4 * H.R) continue;
      const px = wrapLon(lon - H.lon) * H.kx;
      const r = Math.sqrt(px * px + py * py);
      if (r < 1e-3 || r > 4 * H.R) continue;
      const vt = H.vmax * (r / H.R) * Math.exp(1 - r / H.R) * (1 - smoothstep(2.5 * H.R, 4 * H.R, r));
      // Anticyclonic (clockwise in the north) and spreading out.
      u += -vt * (-py / r) + 0.25 * vt * (px / r);
      v += (-vt * (px / r) + 0.25 * vt * (py / r)) * hemi;
    }
    u += 4 * (fbmGeo(lon, lat, h * 0.05, 0.12, 2, 5.1, 0) - 0.5);
    v += 4 * (fbmGeo(lon, lat, h * 0.05, 0.12, 2, -8.3, 3.7) - 0.5);
    if (w > 0) {
      // The US terms. Their sines of absolute longitude count only near the
      // US, so they never reach the dateline.
      u = mix(u, 2 + 6 * smoothstep(32, 50, lat) * (1 + 0.3 * Math.sin(lon / 9)), w);
      v = mix(v, 1.5 * Math.sin((lon + 100) / 12), w);
    }
    for (const L of lows) {
      const py = (lat - L.lat) * KM * L.hemi;
      if (py > 2500 * L.size || py < -2500 * L.size) continue;
      const px = wrapLon(lon - L.lon) * L.kx;
      const r = Math.sqrt(px * px + py * py);
      if (r < 1e-3 || r > 2500 * L.size) continue;
      const R = 420 * L.size;
      // Tapered to nothing at the cut-off, where it would still be ~1 m/s.
      const vt = L.vmax * (r / R) * Math.exp(1 - r / R) * L.env * (1 - smoothstep(1700 * L.size, 2500 * L.size, r));
      // Counter-clockwise in the mirrored frame, spiralling inward: clockwise
      // round a southern-hemisphere low once mirrored back.
      u += vt * (-py / r) - 0.3 * vt * (px / r);
      v += (vt * (px / r) - 0.3 * vt * (py / r)) * L.hemi;
    }
    if (cyclone) {
      const [px, py] = offsetKm(lon, lat, cyclone.lon, cyclone.lat);
      const r = Math.sqrt(px * px + py * py);
      if (r > 1e-3 && r < 1100) {
        const rm = cyclone.rm * (quality === 'gfs' ? 2 : 1);
        const vt = cyclone.vmax * (r < rm ? r / rm : Math.sqrt(rm / r)) * (1 - smoothstep(500, 1100, r)) * cyclone.env;
        u += vt * (-py / r) - 0.35 * vt * (px / r) + 0.5 * cyclone.mu * (vt / cyclone.vmax);
        v += vt * (px / r) - 0.35 * vt * (py / r) + 0.5 * cyclone.mv * (vt / cyclone.vmax);
      }
    }
    const land = landWeight(lon, lat);
    // Friction slows the wind over land.
    const k = 1 - 0.3 * land;
    u *= k;
    v *= k;
    const speed = Math.sqrt(u * u + v * v);
    const gust = Math.min(75, speed * (1.3 + 0.4 * fbmGeo(lon, lat, h * 0.5, 2, 1)) + 0.6 + 1.2 * land);
    return { u, v, gust };
  };

  const clouds = (lon0: number, lat: number, dbz: number) => {
    const lon = wrapLon(lon0);
    const w = usWeight(lon, lat);
    const bias = w < 1 ? (1 - w) * cloudBias(lon, lat, landWeight(lon, lat)) : 0;
    let c = 100 * smoothstep(0.42, 0.7, fbmGeo(lon, lat, h * 0.03, 0.22, octaves, -h * 0.02) + bias);
    for (const L of lows) {
      // Beyond 3 radii the cloud shield adds under 0.01%.
      const py = (lat - L.lat) * KM;
      const R = 850 * L.size;
      if (py > 3 * R || py < -3 * R) continue;
      const px = wrapLon(lon - L.lon) * L.kx;
      c += 80 * Math.exp(-((px * px + py * py) / (R * R))) * L.env;
    }
    if (cyclone) c += 110 * Math.exp(-((cycloneR(lon, lat) / 450) ** 2)) * cyclone.env;
    const P = potential(lon, lat, utcHour, h);
    if (P > 0.02) c += 60 * Math.min(1, P) * smoothstep(0.35, 0.7, organisation(lon, lat, h * 0.08));
    c = Math.max(c, smoothstep(0, 25, dbz) * 97);
    return clamp(c, 0, 100);
  };

  return { temp, dewpoint, wind, clouds };
}

// ---------------------------------------------------------------------------
// Infrared
// ---------------------------------------------------------------------------

interface InfraredField {
  /** Brightness temperature, °C: the ground or sea where the sky is clear,
   *  the tops of the clouds where it is not. */
  tb(lon: number, lat: number): number;
}

/** The tropopause, °C: as high (and cold) as convection climbs. */
const tropopause = (lat: number) => -81 + 27 * smoothstep(12, 48, Math.abs(lat));

/** Sea surface temperature by latitude, every 10 degrees from -90, November. */
const SST = [-1.8, -1.8, -1.8, 0, 6, 12.5, 18.5, 23.5, 27, 28, 28.5, 26, 22, 16.5, 10, 5, 0, -1.8, -1.8];

/** Warm currents and the warm pool, cold upwelling: [lon, lat, rx, ry, °C]. */
const SST_REGIONS = bumps([
  [140, 5, 35, 10, 1], // warm pool
  [-82, -15, 6, 10, -4], // Humboldt
  [10, -22, 5, 8, -3], // Benguela
  [-123, 33, 5, 7, -3], // California Current
  [-50, 40, 15, 6, 4], // Gulf Stream
  [-15, 58, 18, 8, 3], // North Atlantic Drift
  [145, 36, 8, 4, 2], // Kuroshio
]);

function seaSkin(lon: number, lat: number, air: number): number {
  let sst = zonal(SST, lat);
  for (let i = 0; i < SST_REGIONS.length; i += 5) {
    sst += SST_REGIONS[i + 4] * blob(lon, lat, SST_REGIONS[i], SST_REGIONS[i + 1], SST_REGIONS[i + 2], SST_REGIONS[i + 3]);
  }
  // Sea ice shows its own surface, near the air temperature.
  const ice = smoothstep(-2, -10, air) * smoothstep(50, 62, Math.abs(lat));
  return Math.max(-1.8, sst) * (1 - ice) + (air - 1) * ice;
}

/**
 * Infrared brightness temperature, as a geostationary imager sees it,
 * consistent with prepareRadar and prepareSurface for the same quality and
 * run: deep convection where the radar has strong echo (coldest over the
 * cores) and its anvils, the typhoon's dense overcast round a warm eye,
 * frontal cloud shields round the lows, cirrus, mid and low cloud where
 * `clouds()` has cover, and the ground or sea where it is clear. `sat` is
 * treated as `obs`; models (`hrrr`, `gfs`) are smoother.
 */
export function prepareInfrared(ms: number, quality: Quality, opts: { runMs?: number } = {}): InfraredField {
  const h = hoursSinceEpoch(ms);
  const utcHour = utcHourOf(ms);
  const q: Quality = quality === 'sat' ? 'obs' : quality;
  const sharp = q === 'obs';
  const coarse = q === 'gfs';
  const radar = prepareRadar(ms, q, { runMs: opts.runMs });
  const sfc = prepareSurface(ms, q);
  const { jitter, seed, orgShift } = runOffsets(ms, opts.runMs);
  const lows = prepareLows(h, jitter);
  const cyclone = prepareCyclone(h, jitter);
  // Each convective cell's anvil spreads several times wider than its echo.
  const anvils = cellIndex(ms, h, seed, orgShift, (coarse ? 2.6 : 1) * 5, coarse ? -8 : 0, coarse ? 49 : 75);
  const orgZ = h * 0.08 + orgShift;
  const octaves = coarse ? 2 : 3;
  const soft = coarse ? 1.6 : sharp ? 1 : 1.25;

  // Layers are laid from the top down: each shows its own temperature over a
  // share `o` of the view and hides that share of everything beneath.
  let sum = 0;
  let seen = 1;
  const layer = (o: number, t: number) => {
    if (o <= 0) return;
    sum += seen * o * t;
    seen *= 1 - o;
  };
  // Asked for at most once a sample, and only when something needs it.
  let pLon = 0;
  let pLat = 0;
  let air = Number.NaN;
  const airTemp = () => {
    if (Number.isNaN(air)) air = sfc.temp(pLon, pLat);
    return air;
  };
  let streak = Number.NaN;
  // Fibrous cirrus: noise stretched along the parallels.
  const streaks = () => {
    if (Number.isNaN(streak)) streak = fbmGeo(pLon, pLat * 2.2, h * 0.04, coarse ? 0.5 : 1.2, 2, 3.1, 9.4);
    return streak;
  };

  return {
    tb(lon0, lat) {
      const lon = wrapLon(lon0);
      pLon = lon;
      pLat = lat;
      air = Number.NaN;
      streak = Number.NaN;
      sum = 0;
      seen = 1;
      const dbz = radar.dbz(lon, lat);
      const tropo = tropopause(lat);
      const tropic = 1 - smoothstep(26, 38, Math.abs(lat));

      // Deep convection: the radar's strong echo, the cells' anvils, the
      // clusters' shared shields and the typhoon's central dense overcast.
      let deep = Math.max(tropic * smoothstep(16, 38, dbz), smoothstep(38, 52, dbz));
      const az = cellsAt(anvils, lon, lat);
      if (az > 0) deep = Math.max(deep, 1 - Math.exp(-az / 3000));
      const P = potential(lon, lat, utcHour, h);
      if (P > 0.03) {
        // The clusters' anvils, frayed along the upper-level flow.
        const org = organisation(lon, lat, orgZ) + 0.18 * (streaks() - 0.5);
        deep = Math.max(deep, smoothstep(0.03, 0.25, P * smoothstep(0.45, 0.72, org)));
      }
      let eye = 1;
      let wall = 0;
      if (cyclone) {
        const py = (lat - cyclone.lat) * KM;
        const px = wrapLon(lon - cyclone.lon) * KM * Math.cos(cyclone.lat * RAD);
        const r = Math.sqrt(px * px + py * py);
        if (r < 850) {
          const rm = cyclone.rm * (coarse ? 2 : 1);
          const k = cyclone.env * smoothstep(20, 40, cyclone.vmax);
          eye = 1 - k * (1 - smoothstep(0.45 * rm, 0.95 * rm, r));
          wall = k * Math.exp(-(((r - 1.3 * rm) / (0.8 * rm)) ** 2));
          const cdo = k * (1 - smoothstep(260, 460 * soft, r));
          // Outflow cirrus fanning out in spiral streaks.
          const theta = Math.atan2(py, px);
          const fan = fbmGeo(lon, lat, h * 0.1, 0.4, 1) * 0.5 + 0.5 * (0.5 + 0.5 * Math.cos(3 * (theta + 1.6 * Math.log(Math.max(r, 1) / 100))));
          const outflow = k * (1 - smoothstep(420, 850, r)) * smoothstep(0.35, 0.7, fan);
          deep = Math.max(deep, cdo, 0.85 * outflow);
        }
      }
      deep *= eye;
      if (deep > 0) {
        // Bubbling tops, and overshooting tops coldest over the cores.
        const bubbles = fbmGeo(lon, lat, h * 0.6, sharp ? 3.2 : 1.4, sharp ? 2 : 1, 7.7, -3.3);
        layer(deep, tropo + 9 - 14 * smoothstep(35, 58, dbz) - 6 * wall + (sharp ? 6 : 3) * (bubbles - 0.5) + 5 * (1 - deep));
      }

      // Frontal cloud shields: the comma head round the centre, the warm
      // conveyor belt's shield poleward of the warm front, and the band along
      // the cold front.
      let fo = 0;
      let ft = 0;
      let fw = 0;
      for (const L of lows) {
        const py = (lat - L.lat) * KM * L.hemi;
        if (py < L.y0 || py > L.y1) continue;
        const px = wrapLon(lon - L.lon) * L.kx;
        if (px < L.x0 || px > L.x1) continue;
        const s = L.size;
        const f = frontDistances(L, px, py);
        const r = Math.sqrt(px * px + py * py);
        const head = 1 - smoothstep(200 * s, 520 * s, r);
        const warm = smoothstep(-120, 60, f.warm) * (1 - smoothstep(300 * s, 700 * s, f.warm)) * smoothstep(0, 0.12, f.warmT) * (1 - smoothstep(0.85, 1, f.warmT));
        const cold = smoothstep(0.03, 0.15, f.s) * (1 - smoothstep(0.88, 1, f.s)) * smoothstep(-160, -60, f.cold) * (1 - smoothstep(180, 360, f.cold));
        const o = Math.max(head, warm, cold) * L.env;
        if (o <= 0) continue;
        const w = head + warm + cold;
        const t = (head * -50 + warm * (-54 + 14 * clamp(f.warm / (700 * s), 0, 1)) + cold * (-52 + 12 * f.s)) / w;
        fo = Math.max(fo, o);
        ft += t * o;
        fw += o;
      }
      // Mid-latitude echo under a shield is the shield's rain; out in the open
      // it is showers, with cumulus tops.
      const rainy = smoothstep(4, 22, dbz) * (1 - tropic);
      const under = smoothstep(0.15, 0.45, fo);
      const frontal = rainy * under;
      if (frontal > fo) {
        ft += -48 * (frontal - fo);
        fw += frontal - fo;
        fo = frontal;
      }
      if (fo > 0) {
        const fib = streaks();
        // Fibrous edges: the noise moves only the shield's margins, by up to
        // ~100 km, and leaves its interior and the clear air alone.
        const o = smoothstep(0.25, 0.7, fo + 1.6 * (fib - 0.5) * Math.min(fo, 1 - fo));
        // Tops rise and fall across the shield, by 10-20 degrees.
        const relief = fbmGeo(lon, lat, h * 0.05, coarse ? 0.4 : 0.8, 2, -12.9, 4.4);
        layer(o, Math.max(tropo + 3, ft / fw + 22 * (relief - 0.5) + 5 * (fib - 0.5)));
      }
      if (rainy * (1 - under) > 0) layer(rainy * (1 - under) * 0.9, airTemp() - 28);

      // Everyday cloud where clouds() has cover: cirrus, mid and low by type.
      if (seen > 1e-4) {
        const land = landWeight(lon, lat);
        const us = usWeight(lon, lat);
        const dry = dryness(lon, lat, land);
        const deck = land < 1 ? maxBump(lon, lat, DECKS) * (1 - land) : 0;
        const bias = us < 1 ? (1 - us) * cloudBias(lon, lat, land, deck) : 0;
        const cover = smoothstep(0.42, 0.7, fbmGeo(lon, lat, h * 0.03, 0.22, octaves, -h * 0.02) + bias);
        if (cover > 0) {
          // Mostly low cloud, some mid-level, a little cirrus; denser cloud
          // reaches higher. Cirrus over the deserts, low cloud over the decks
          // and the polar seas.
          const type =
            fbmGeo(lon, lat, h * 0.02, 0.3, 2, 41.3, 17.9) + 0.3 * (cover - 0.5) + 0.25 * dry - 0.3 * deck - 0.15 * smoothstep(55, 70, Math.abs(lat));
          const hi = smoothstep(0.6, 0.72, type);
          const lo = 1 - smoothstep(0.45, 0.56, type);
          const mid = Math.max(0, 1 - hi - lo);
          if (hi > 0) {
            const fib = streaks();
            layer(cover * hi * (0.3 + 0.6 * fib), tropo + 14 + 4 * (fib - 0.5));
          }
          if (mid > 0) layer(cover * mid * 0.9, clamp(airTemp() - 33, tropo + 20, -10));
          if (lo > 0) layer(cover * lo * 0.85, airTemp() - 4);
        }
        if (deck > 0) {
          // Closed cells of the stratocumulus decks.
          const cells = fbmGeo(lon, lat, h * 0.2, coarse ? 1 : 2.6, 2, -6.1, 11.3);
          layer(deck * (sharp ? 0.45 + 0.5 * smoothstep(0.35, 0.6, cells) : 0.75), airTemp() - 3);
        }

        // The ground or sea where it is clear: land swings harder through
        // the day than the air above it and cools hard in the polar night.
        if (seen > 1e-4) {
          const t = airTemp();
          let skin = t;
          if (land < 1) skin = seaSkin(lon, lat, t);
          if (land > 0) {
            const sun = clamp(Math.cos((lat + 16) * RAD) * 1.3, 0, 1);
            const swing = sun * (3 + 9 * dry) * Math.cos(((utcHour + lon / 15 - 13) * Math.PI) / 12);
            skin = mix(skin, t + swing - 10 * (1 - sun), land);
          }
          sum += seen * skin;
          seen = 0;
        }
      }
      return clamp(sum / (1 - seen), -90, 55);
    },
  };
}

/** Precipitation type code from surface temperature: rain, then a mix, then
 *  freezing rain, sleet and snow as it gets colder. */
export function ptypeFor(tempC: number, rate: number): number {
  if (rate < 0.01) return 0;
  if (tempC >= 1.0) return 1;
  if (tempC >= 0.3) return 5;
  if (tempC >= -0.6) return 4;
  if (tempC >= -1.5) return 3;
  return 2;
}

/** NWS wind chill at or below 50 °F with wind above 3 mph, heat index
 *  (Rothfusz with the standard adjustments) at or above 80 °F, else the air temperature. */
export function feelsLike(tempC: number, dewC: number, windMs: number): number {
  const T = (tempC * 9) / 5 + 32;
  const V = windMs * 2.2369362920544;
  let F = T;
  if (T <= 50 && V > 3) {
    const v16 = V ** 0.16;
    F = 35.74 + 0.6215 * T - 35.75 * v16 + 0.4275 * T * v16;
  } else if (T >= 80) {
    const rh = relativeHumidity(tempC, dewC);
    let hi = 0.5 * (T + 61 + (T - 68) * 1.2 + rh * 0.094);
    if (hi >= 80) {
      hi =
        -42.379 + 2.04901523 * T + 10.14333127 * rh - 0.22475541 * T * rh - 0.00683783 * T * T -
        0.05481717 * rh * rh + 0.00122874 * T * T * rh + 0.00085282 * T * rh * rh - 0.00000199 * T * T * rh * rh;
      if (rh < 13 && T <= 112) hi -= ((13 - rh) / 4) * Math.sqrt((17 - Math.abs(T - 95)) / 17);
      else if (rh > 85 && T <= 87) hi += ((rh - 85) / 10) * ((87 - T) / 5);
    }
    F = hi;
  }
  return ((F - 32) * 5) / 9;
}

/** Magnus formula. */
function relativeHumidity(tempC: number, dewC: number): number {
  const es = (t: number) => Math.exp((17.625 * t) / (243.04 + t));
  return clamp((100 * es(dewC)) / es(tempC), 0, 100);
}
