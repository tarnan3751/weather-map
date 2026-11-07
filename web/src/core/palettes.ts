// Palettes live in the client (the server only names them). Stops are in the
// band's stored unit so a palette never depends on the display unit: switching
// °F/°C or mph/kt only relabels the legend. Colours were picked to read on a
// near-black basemap: no dark blues where the background is dark, and
// transparency where there is nothing worth drawing, which a palette says
// with an explicit transparent stop: past either end of its stops a value
// takes the end colour, so a reading colder or windier than any stop is still
// drawn. Ranges are the world's: Antarctic cold to desert heat, hurricanes.

import { speedUnit, tempUnit } from './units.ts';

export type Transfer = 'linear' | 'sqrt' | 'log';

export interface Stop {
  /** Value in the band's stored unit. */
  v: number;
  c: string;
  /** Opacity, 0..1. Defaults to 1. */
  a?: number;
}

export interface LegendSpec {
  /** Stored-unit range the legend bar spans. */
  range: [number, number];
  /** Labelled ticks, in display units, keyed by the display unit's label. */
  ticks: Record<string, number[]>;
  /** Words under the bar, at stored-unit positions. */
  words?: { v: number; text: string }[];
}

interface Base {
  id: string;
  title: string;
  /** Stored-unit range the lookup texture covers. */
  domain: [number, number];
  transfer: Transfer;
  legend: LegendSpec;
}

export interface ScalarPalette extends Base {
  kind: 'scalar';
  stops: Stop[];
}

export interface PrecipTypePalette extends Base {
  kind: 'precip_type';
  /** One ramp per precipitation type code (1 rain .. 5 mix), by rate. */
  ramps: Record<number, Stop[]>;
}

export type Palette = ScalarPalette | PrecipTypePalette;

export const PTYPE_NAMES: Record<number, string> = { 0: 'None', 1: 'Rain', 2: 'Snow', 3: 'Sleet', 4: 'Freezing rain', 5: 'Mix' };

const fToC = tempUnit('F').from;
const mphToMs = speedUnit('mph').from;
const inToMm = (i: number) => i * 25.4;

const PALETTES: Palette[] = [
  {
    id: 'reflectivity',
    title: 'Reflectivity',
    kind: 'scalar',
    domain: [-30, 80],
    transfer: 'linear',
    // Transparent below 10 dBZ (drizzle, clutter) and at the −30 dBZ "no echo"
    // floor; classic green-yellow-red progression, magenta and white for hail cores.
    stops: [
      { v: 9.9, c: '#2a7f62', a: 0 },
      { v: 10, c: '#2a7f62', a: 0.45 },
      { v: 15, c: '#38a169', a: 0.7 },
      { v: 20, c: '#48bb5a', a: 0.85 },
      { v: 25, c: '#2f9a3e' },
      { v: 30, c: '#1f7a32' },
      { v: 35, c: '#f2e33a' },
      { v: 40, c: '#f2b632' },
      { v: 45, c: '#f27b22' },
      { v: 50, c: '#e8341c' },
      { v: 55, c: '#b5121b' },
      { v: 60, c: '#e040d0' },
      { v: 65, c: '#9b4dff' },
      { v: 70, c: '#f3ecff' },
    ],
    legend: {
      range: [10, 72],
      ticks: { dBZ: [10, 20, 30, 40, 50, 60, 70] },
      words: [
        { v: 15, text: 'Light' },
        { v: 40, text: 'Heavy' },
        { v: 62, text: 'Hail' },
      ],
    },
  },
  {
    id: 'precip_type',
    title: 'Precipitation type',
    kind: 'precip_type',
    domain: [0.05, 100],
    transfer: 'log',
    ramps: {
      1: [
        { v: 0.0499, c: '#2f8f5b', a: 0 },
        { v: 0.05, c: '#2f8f5b', a: 0.45 },
        { v: 0.5, c: '#45b45c', a: 0.85 },
        { v: 2.5, c: '#2b8a3c' },
        { v: 6, c: '#f2e33a' },
        { v: 12, c: '#f2992e' },
        { v: 25, c: '#e8341c' },
        { v: 60, c: '#c21b6a' },
      ],
      2: [
        { v: 0.0499, c: '#3d6fa8', a: 0 },
        { v: 0.05, c: '#3d6fa8', a: 0.5 },
        { v: 0.5, c: '#4f8fd8', a: 0.85 },
        { v: 1.5, c: '#6cb0f0' },
        { v: 3, c: '#a3d2ff' },
        { v: 6, c: '#d9eeff' },
        { v: 12, c: '#ffffff' },
      ],
      3: [
        { v: 0.0499, c: '#9a6234', a: 0 },
        { v: 0.05, c: '#9a6234', a: 0.5 },
        { v: 0.5, c: '#d4843f', a: 0.85 },
        { v: 2, c: '#f0a05a' },
        { v: 6, c: '#ffc58f' },
      ],
      4: [
        { v: 0.0499, c: '#9c3f74', a: 0 },
        { v: 0.05, c: '#9c3f74', a: 0.5 },
        { v: 0.5, c: '#d6559f', a: 0.85 },
        { v: 2, c: '#f07cc0' },
        { v: 6, c: '#ffb0e0' },
      ],
      5: [
        { v: 0.0499, c: '#6a4aa3', a: 0 },
        { v: 0.05, c: '#6a4aa3', a: 0.5 },
        { v: 0.5, c: '#9168d8', a: 0.85 },
        { v: 2, c: '#b394f0' },
        { v: 6, c: '#dac8ff' },
      ],
    },
    legend: { range: [0.1, 50], ticks: { 'in/h': [0.01, 0.1, 0.5, 1], 'mm/h': [0.25, 2.5, 10, 25] } },
  },
  {
    id: 'rainfall',
    title: 'Rainfall',
    kind: 'scalar',
    // Monsoons and typhoons bring a metre in three days.
    domain: [0, 1000],
    transfer: 'sqrt',
    stops: [
      { v: inToMm(0.009), c: '#2c6f4a', a: 0 },
      { v: inToMm(0.01), c: '#2c6f4a', a: 0.55 },
      { v: inToMm(0.1), c: '#3fa35b', a: 0.85 },
      { v: inToMm(0.25), c: '#2d8a3e' },
      { v: inToMm(0.5), c: '#2f7fc8' },
      { v: inToMm(1), c: '#5b5fe0' },
      { v: inToMm(1.5), c: '#9b4fd8' },
      { v: inToMm(2), c: '#d64fb8' },
      { v: inToMm(3), c: '#e8341c' },
      { v: inToMm(4), c: '#f27b22' },
      { v: inToMm(6), c: '#f2d43a' },
      { v: inToMm(10), c: '#fff7d6' },
      { v: inToMm(20), c: '#ffffff' },
    ],
    legend: { range: [0, inToMm(20)], ticks: { in: [0.5, 1, 2, 4, 8, 16], mm: [10, 25, 50, 100, 200, 400] } },
  },
  {
    id: 'snowfall',
    title: 'Snowfall',
    kind: 'scalar',
    domain: [0, 2500],
    transfer: 'sqrt',
    stops: [
      { v: inToMm(0.09), c: '#8fb8e0', a: 0 },
      { v: inToMm(0.1), c: '#8fb8e0', a: 0.5 },
      { v: inToMm(1), c: '#9fc6e8', a: 0.85 },
      { v: inToMm(2), c: '#6fa8dc' },
      { v: inToMm(4), c: '#3d85c6' },
      { v: inToMm(6), c: '#4a5fd0' },
      { v: inToMm(8), c: '#7444c9' },
      { v: inToMm(12), c: '#a83dc6' },
      { v: inToMm(18), c: '#d63dab' },
      { v: inToMm(24), c: '#ea5a8a' },
      { v: inToMm(36), c: '#f5a8bd' },
      { v: inToMm(48), c: '#ffffff' },
    ],
    legend: { range: [0, inToMm(40)], ticks: { in: [1, 3, 6, 12, 24, 36], cm: [2, 5, 10, 25, 50, 100] } },
  },
  {
    id: 'temperature',
    title: 'Temperature',
    kind: 'scalar',
    domain: [-75, 60],
    transfer: 'linear',
    // Bands every 10 °F as on US broadcast maps; 32 °F sits on the blue-teal
    // break. Colder than any US map needs, it fades to white: Siberian and
    // Antarctic winters are -60 °C.
    stops: [
      { v: fToC(-80), c: '#ffffff' },
      { v: fToC(-60), c: '#f6f2ff' },
      { v: fToC(-40), c: '#e6d9ff' },
      { v: fToC(-20), c: '#c8b0ff' },
      { v: fToC(-10), c: '#9d7ee8' },
      { v: fToC(0), c: '#7660d6' },
      { v: fToC(10), c: '#5566d0' },
      { v: fToC(20), c: '#3f86d8' },
      { v: fToC(32), c: '#43b4e6' },
      { v: fToC(40), c: '#3fbfa6' },
      { v: fToC(50), c: '#55bb5e' },
      { v: fToC(60), c: '#a9d047' },
      { v: fToC(70), c: '#f0d23a' },
      { v: fToC(80), c: '#f29a2e' },
      { v: fToC(90), c: '#e8551d' },
      { v: fToC(100), c: '#c41f1f' },
      { v: fToC(110), c: '#8e1240' },
      { v: fToC(120), c: '#ff9ad5' },
    ],
    legend: { range: [-60, 50], ticks: { '°F': [-60, -30, 0, 30, 60, 90, 120], '°C': [-60, -40, -20, 0, 20, 40] } },
  },
  {
    id: 'dew_point',
    title: 'Dew point',
    kind: 'scalar',
    domain: [-70, 40],
    transfer: 'linear',
    // Dry browns, comfortable greens, then muggy blues and tropical purples.
    stops: [
      { v: fToC(-60), c: '#6b5a4a' },
      { v: fToC(-20), c: '#5b4128' },
      { v: fToC(10), c: '#80603a' },
      { v: fToC(25), c: '#a88f55' },
      { v: fToC(35), c: '#c4bd78' },
      { v: fToC(45), c: '#8fc76a' },
      { v: fToC(50), c: '#5bb35b' },
      { v: fToC(55), c: '#2f9e5a' },
      { v: fToC(60), c: '#1f8a7a' },
      { v: fToC(65), c: '#2a72c0' },
      { v: fToC(70), c: '#4b52cc' },
      { v: fToC(75), c: '#7a3dc9' },
      { v: fToC(80), c: '#b33db3' },
    ],
    legend: {
      range: [fToC(-40), fToC(80)],
      ticks: { '°F': [-40, -10, 20, 50, 70], '°C': [-40, -20, 0, 10, 20] },
      words: [
        { v: fToC(15), text: 'Dry' },
        { v: fToC(52), text: 'Comfortable' },
        { v: fToC(70), text: 'Muggy' },
      ],
    },
  },
  {
    id: 'wind',
    title: 'Wind speed',
    kind: 'scalar',
    // Up to the strongest hurricanes; the square root keeps the everyday
    // breezes spread out.
    domain: [0, 85],
    transfer: 'sqrt',
    stops: [
      { v: 0, c: '#2b3f66', a: 0.35 },
      { v: mphToMs(5), c: '#2f5f92', a: 0.7 },
      { v: mphToMs(10), c: '#3a8fb0' },
      { v: mphToMs(15), c: '#3bb39a' },
      { v: mphToMs(20), c: '#6cc25a' },
      { v: mphToMs(25), c: '#c7d43a' },
      { v: mphToMs(30), c: '#f2c12e' },
      { v: mphToMs(40), c: '#f2802e' },
      { v: mphToMs(50), c: '#e8401c' },
      { v: mphToMs(60), c: '#c41a5a' },
      { v: mphToMs(75), c: '#9b2bb3' },
      { v: mphToMs(100), c: '#eadcff' },
      { v: mphToMs(150), c: '#ffffff' },
    ],
    legend: {
      range: [0, mphToMs(160)],
      ticks: {
        mph: [0, 10, 25, 50, 75, 110, 150],
        kt: [0, 10, 25, 50, 75, 100, 130],
        'm/s': [0, 5, 10, 20, 35, 50, 70],
        'km/h': [0, 20, 50, 100, 150, 250],
      },
      words: [
        { v: 17.2, text: 'Gale' },
        { v: 32.7, text: 'Hurricane' },
      ],
    },
  },
  {
    id: 'clouds',
    title: 'Cloud cover',
    kind: 'scalar',
    domain: [0, 100],
    transfer: 'linear',
    stops: [
      { v: 5, c: '#8a96a8', a: 0 },
      { v: 20, c: '#8a96a8', a: 0.18 },
      { v: 50, c: '#b8c2d0', a: 0.45 },
      { v: 80, c: '#dfe5ee', a: 0.68 },
      { v: 100, c: '#f4f7fb', a: 0.82 },
    ],
    legend: {
      range: [0, 100],
      ticks: { '%': [0, 25, 50, 75, 100] },
      words: [
        { v: 10, text: 'Clear' },
        { v: 90, text: 'Overcast' },
      ],
    },
  },
  {
    // Enhanced infrared, as forecasters read it: the warm ground and low
    // cloud in fading greys (the map shows through where it is clear), cloud
    // whiter as it rises, and colour only for the coldest, highest tops,
    // where thunderstorms are.
    id: 'infrared',
    title: 'Cloud-top temperature',
    kind: 'scalar',
    domain: [-92, 52],
    transfer: 'linear',
    stops: [
      { v: -90, c: '#ffffff' },
      { v: -84, c: '#ff5ad2' },
      { v: -78, c: '#e8262e' },
      { v: -71, c: '#ff8a1c' },
      { v: -64, c: '#f5e23c' },
      { v: -57, c: '#3cc85c' },
      { v: -49, c: '#2f80d8' },
      { v: -40.5, c: '#5ab8ea' },
      { v: -40, c: '#f1f4f8' },
      { v: -25, c: '#d3d8df' },
      { v: -5, c: '#a7aeb8' },
      { v: 12, c: '#6f7782', a: 0.75 },
      { v: 28, c: '#3c424b', a: 0.4 },
      { v: 50, c: '#1b1e23', a: 0.12 },
    ],
    legend: {
      // Warm on the left, cold on the right: colder reads as higher cloud.
      range: [45, -90],
      ticks: { '°C': [40, 20, 0, -20, -40, -60, -80], '°F': [100, 50, 0, -50, -100] },
      words: [
        { v: 25, text: 'Low cloud' },
        { v: -32, text: 'High cloud' },
        { v: -74, text: 'Storm tops' },
      ],
    },
  },
];

const FALLBACK: ScalarPalette = {
  id: 'generic',
  title: 'Value',
  kind: 'scalar',
  domain: [0, 100],
  transfer: 'linear',
  stops: [
    { v: 0, c: '#30123b' },
    { v: 25, c: '#2a7ab9' },
    { v: 50, c: '#3bc47a' },
    { v: 75, c: '#e6c43a' },
    { v: 100, c: '#f06a2b' },
  ],
  legend: { range: [0, 100], ticks: {} },
};

export function getPalette(id: string): Palette {
  return PALETTES.find((p) => p.id === id) ?? FALLBACK;
}

// ---------------------------------------------------------------------------
// Colour math
// ---------------------------------------------------------------------------

export type RGBA = [number, number, number, number];

function parseHex(hex: string): [number, number, number] {
  const h = hex.replace('#', '');
  return [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)];
}

/** A stop's own colour, unblended. */
const solid = (s: Stop): RGBA => [...parseHex(s.c), Math.round((s.a ?? 1) * 255)];

const toLinear = (c: number) => {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
};
const fromLinear = (l: number) => {
  const s = l <= 0.0031308 ? 12.92 * l : 1.055 * l ** (1 / 2.4) - 0.055;
  return Math.max(0, Math.min(255, Math.round(s * 255)));
};

function rgbToOklab([r, g, b]: [number, number, number]): [number, number, number] {
  const lr = toLinear(r);
  const lg = toLinear(g);
  const lb = toLinear(b);
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

function oklabToRgb([L, a, b]: [number, number, number]): [number, number, number] {
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    fromLinear(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
    fromLinear(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
    fromLinear(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
  ];
}

/** Colour of a ramp at stored value `v`. Smooth ramps blend in OKLab so
 *  gradients stay even in lightness; stepped ramps hold each stop's colour. */
export function rampColor(stops: Stop[], v: number, stepped = false): RGBA {
  if (!stops.length || Number.isNaN(v)) return [0, 0, 0, 0];
  if (v < stops[0].v) return solid(stops[0]);
  const last = stops[stops.length - 1];
  if (v >= last.v) return solid(last);
  let i = 0;
  while (i < stops.length - 2 && v >= stops[i + 1].v) i++;
  const a = stops[i];
  const b = stops[i + 1];
  if (stepped) return solid(a);
  const t = (v - a.v) / (b.v - a.v);
  const la = rgbToOklab(parseHex(a.c));
  const lb = rgbToOklab(parseHex(b.c));
  const rgb = oklabToRgb([la[0] + (lb[0] - la[0]) * t, la[1] + (lb[1] - la[1]) * t, la[2] + (lb[2] - la[2]) * t]);
  const alpha = (a.a ?? 1) + ((b.a ?? 1) - (a.a ?? 1)) * t;
  return [rgb[0], rgb[1], rgb[2], Math.round(alpha * 255)];
}

// ---------------------------------------------------------------------------
// Lookup textures
// ---------------------------------------------------------------------------

export const LUT_SIZE = 1024;
/** Rows of a precipitation-type lookup texture: one per type code 0..5. */
export const PTYPE_ROWS = 6;

/** Each transfer function and its inverse. */
const TRANSFERS: Record<Transfer, [(v: number) => number, (x: number) => number]> = {
  linear: [(v) => v, (x) => x],
  sqrt: [(v) => Math.sqrt(Math.max(0, v)), (x) => x * x],
  log: [(v) => Math.log10(Math.max(1e-6, v)), (x) => 10 ** x],
};

export const transferFn = (t: Transfer): ((v: number) => number) => TRANSFERS[t][0];

export const TRANSFER_CODE: Record<Transfer, number> = { linear: 0, sqrt: 1, log: 2 };

/** Where `v` falls between the ends of `range`, 0..1, in transfer space. */
export function transferPos(t: Transfer, [lo, hi]: readonly [number, number], v: number): number {
  const f = transferFn(t);
  return Math.max(0, Math.min(1, (f(v) - f(lo)) / (f(hi) - f(lo))));
}

/** The value at fraction `x` along `range`, in transfer space. */
export function transferAt(t: Transfer, [lo, hi]: readonly [number, number], x: number): number {
  const [f, inv] = TRANSFERS[t];
  return inv(f(lo) + x * (f(hi) - f(lo)));
}

/** Where value `v` falls in the lookup texture, 0..1. The shader computes
 *  exactly this. */
export const lutCoord = (p: Palette, v: number) => transferPos(p.transfer, p.domain, v);

/** RGBA8 lookup texture, straight (not premultiplied) alpha. Scalar palettes
 *  are one row; precipitation-type palettes have one row per type code. */
export function buildLut(p: Palette, stepped = false, size = LUT_SIZE): { data: Uint8Array; width: number; height: number } {
  const rows = p.kind === 'scalar' ? 1 : PTYPE_ROWS;
  const data = new Uint8Array(size * rows * 4);
  for (let r = 0; r < rows; r++) {
    const stops = p.kind === 'scalar' ? p.stops : (p.ramps[r] ?? []);
    for (let i = 0; i < size; i++) {
      // The value at the centre of texel `i`.
      const c = rampColor(stops, transferAt(p.transfer, p.domain, (i + 0.5) / size), stepped);
      data.set(c, (r * size + i) * 4);
    }
  }
  return { data, width: size, height: rows };
}

/** CPU twin of the shader's lookup: the colour a value is drawn with. */
export function lookupLut(lut: { data: Uint8Array; width: number }, p: Palette, v: number, row = 0): RGBA {
  const i = Math.min(lut.width - 1, Math.max(0, Math.floor(lutCoord(p, v) * lut.width)));
  const o = (row * lut.width + i) * 4;
  return [lut.data[o], lut.data[o + 1], lut.data[o + 2], lut.data[o + 3]];
}

export function cssColor([r, g, b, a]: RGBA): string {
  return `rgba(${r}, ${g}, ${b}, ${(a / 255).toFixed(3)})`;
}
