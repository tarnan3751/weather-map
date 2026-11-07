// Which parts a tile is rendered from: the client's half of
// `wx_core::frame::tile_parts`, with the lon/lat box arithmetic of
// `wx_core::proj::LonLatBox`. The tile URL names this stack, so the client and
// the server must agree on it; the server tolerates differences only within a
// millionth of a degree of a tile edge.
//
// Longitudes are in [-180, 180]. `west > east` means a box crosses the
// antimeridian (it runs east from `west` to 180 and on from -180 to `east`);
// `west = -180, east = 180` is every longitude.

import type { LonLatBox, Part } from '../api/types.ts';
import { xToLon, yToLat } from './tileMath.ts';

export const wraps = (b: LonLatBox) => b.west > b.east;

/** A box's longitudes as one or two ordinary `[west, east]` intervals. */
function spans(b: LonLatBox): [number, number][] {
  return wraps(b)
    ? [
        [b.west, 180],
        [-180, b.east],
      ]
    : [[b.west, b.east]];
}

/** Do the boxes share any area? Touching along an edge does not count. */
export function boxesIntersect(a: LonLatBox, b: LonLatBox): boolean {
  if (!(a.south < b.north && b.south < a.north)) return false;
  for (const [aw, ae] of spans(a)) for (const [bw, be] of spans(b)) if (aw < be && bw < ae) return true;
  return false;
}

/** Is `inner` entirely inside `outer` (edges may coincide)? */
export function boxContains(outer: LonLatBox, inner: LonLatBox): boolean {
  if (!(outer.south <= inner.south && inner.north <= outer.north)) return false;
  if (outer.west === -180 && outer.east === 180) return true;
  const ow = wraps(outer);
  const iw = wraps(inner);
  if (ow === iw) return outer.west <= inner.west && inner.east <= outer.east;
  // A wrapping box holds an ordinary one that lies in either of its halves;
  // an ordinary box can never hold one that wraps.
  if (ow) return inner.west >= outer.west || inner.east <= outer.east;
  return false;
}

export function boxContainsPoint(b: LonLatBox, lon: number, lat: number): boolean {
  if (!(lat >= b.south && lat <= b.north)) return false;
  return wraps(b) ? lon >= b.west || lon <= b.east : lon >= b.west && lon <= b.east;
}

/** Lon/lat bounds of an XYZ tile, computed as `TileCoord::bounds` does. */
export function tileBounds(z: number, x: number, y: number): LonLatBox {
  const n = 2 ** z;
  return { west: xToLon(x / n), east: xToLon((x + 1) / n), north: yToLat(y / n), south: yToLat((y + 1) / n) };
}

type Stackable = Pick<Part, 'bounds' | 'solid'>;

/**
 * The parts of a frame (top first) that a tile with bounds `tile` is rendered
 * from: each whose bounds intersect it, stopping after the first whose solid
 * box contains it, because nothing beneath that part shows.
 */
export function tileParts<P extends Stackable>(parts: readonly P[], tile: LonLatBox): P[] {
  const out: P[] = [];
  for (const p of parts) {
    if (!boxesIntersect(p.bounds, tile)) continue;
    out.push(p);
    if (p.solid && boxContains(p.solid, tile)) break;
  }
  return out;
}

/** A stack offers tiles down to the deepest zoom any of its parts has. */
export function stackMaxZoom(parts: readonly Pick<Part, 'max_zoom'>[]): number {
  let z = -1;
  for (const p of parts) z = Math.max(z, p.max_zoom);
  return z;
}

/** The top part whose bounds hold a point: who the value there comes from. */
export function topPartAt<P extends Pick<Part, 'bounds'>>(parts: readonly P[], lon: number, lat: number): P | null {
  for (const p of parts) if (boxContainsPoint(p.bounds, lon, lat)) return p;
  return null;
}
