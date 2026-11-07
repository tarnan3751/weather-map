// A straight port of the server's box arithmetic (`wx_core::proj::LonLatBox`)
// and of the tile-stack rule with its tolerances (`wx_core::frame::tile_parts`,
// `tile_parts_must`, `tile_parts_may`). Kept separate from the client's
// version on purpose: the mock checks the client's stacks the way the server
// would, so a mistake in one shows up as a 404 instead of agreeing with itself.
// Erasable syntax only: worker threads load this directly.

import type { LonLatBox, Part } from '../src/api/types.ts';

const COMBO_EPSILON_DEG = 1e-6;
export const MAX_COMBO_PARTS = 48;
export const WORLD: LonLatBox = { west: -180, south: -90, east: 180, north: 90 };

const wraps = (b: LonLatBox) => b.west > b.east;
/** A longitude up to a turn outside [-180, 180] brought back into it. */
export const fold = (lon: number) => (lon > 180 ? lon - 360 : lon < -180 ? lon + 360 : lon);

const lonSpans = (b: LonLatBox): [number, number][] => (wraps(b) ? [[b.west, 180], [-180, b.east]] : [[b.west, b.east]]);

function intersects(a: LonLatBox, b: LonLatBox): boolean {
  if (!(a.south < b.north && b.south < a.north)) return false;
  return lonSpans(a).some(([aw, ae]) => lonSpans(b).some(([bw, be]) => aw < be && bw < ae));
}

function containsBox(a: LonLatBox, b: LonLatBox): boolean {
  if (!(a.south <= b.south && b.north <= a.north)) return false;
  if (a.west === -180 && a.east === 180) return true;
  if (wraps(a) === wraps(b)) return a.west <= b.west && b.east <= a.east;
  return wraps(a) && (b.west >= a.west || b.east <= a.east);
}

export function contains(b: LonLatBox, lon: number, lat: number): boolean {
  if (!(lat >= b.south && lat <= b.north)) return false;
  return wraps(b) ? lon >= b.west || lon <= b.east : lon >= b.west && lon <= b.east;
}

function grown(b: LonLatBox, deg: number): LonLatBox | null {
  const south = Math.max(-90, b.south - deg);
  const north = Math.min(90, b.north + deg);
  if (south >= north) return null;
  const width = wraps(b) ? b.east - b.west + 360 : b.east - b.west;
  const w = width + 2 * deg;
  if (w <= 0) return null;
  if (w >= 360) return { west: -180, south, east: 180, north };
  return { west: fold(b.west - deg), south, east: fold(b.east + deg), north };
}

const xToLon = (x: number) => x * 360 - 180;
const yToLat = (y: number) => Math.atan(Math.sinh(Math.PI * (1 - 2 * y))) * (180 / Math.PI);

export function tileBox(z: number, x: number, y: number): LonLatBox {
  const n = 2 ** z;
  return { west: xToLon(x / n), east: xToLon((x + 1) / n), north: yToLat(y / n), south: yToLat((y + 1) / n) };
}

type P = Pick<Part, 'id' | 'bounds' | 'solid'>;

export function tileParts<T extends P>(parts: readonly T[], tile: LonLatBox): T[] {
  const out: T[] = [];
  for (const p of parts) {
    if (!intersects(p.bounds, tile)) continue;
    out.push(p);
    if (p.solid && containsBox(p.solid, tile)) break;
  }
  return out;
}

/** What a correct answer must name: every doubt resolved towards leaving out. */
function tilePartsMust<T extends P>(parts: readonly T[], tile: LonLatBox): T[] {
  const inner = grown(tile, -COMBO_EPSILON_DEG);
  if (!inner) return [];
  const outer = grown(tile, COMBO_EPSILON_DEG) ?? tile;
  const out: T[] = [];
  for (const p of parts) {
    if (intersects(p.bounds, inner)) out.push(p);
    if (intersects(p.bounds, outer) && p.solid && containsBox(p.solid, inner)) break;
  }
  return out;
}

/** Is `ids` an acceptable stack for this tile of a frame with `parts`: it
 *  keeps the frame's order, names everything `must` does and nothing `may`
 *  (every doubt resolved towards keeping in) does not. */
export function stackAcceptable(parts: readonly P[], z: number, x: number, y: number, ids: readonly string[]): boolean {
  const tile = tileBox(z, x, y);
  const may = tileParts(parts, grown(tile, COMBO_EPSILON_DEG) ?? tile).map((p) => p.id);
  const must = new Set(tilePartsMust(parts, tile).map((p) => p.id));
  let j = 0;
  for (const id of ids) {
    while (j < may.length && may[j] !== id) {
      if (must.has(may[j])) return false;
      j++;
    }
    if (j === may.length) return false;
    j++;
  }
  for (; j < may.length; j++) if (must.has(may[j])) return false;
  return ids.length > 0;
}
