// The area the map shows, as the endpoints that list things by area take it.

import type { LonLatBox } from '../api/types.ts';

/** The lon/lat box a map shows, as the list endpoints take it: wrapped into
 *  [-180, 180] (`west > east` when it spans the antimeridian), or the whole
 *  world once it is that wide. */
export function viewBox(west: number, south: number, east: number, north: number): LonLatBox {
  const s = Math.max(-90, south);
  const n = Math.min(90, north);
  if (east - west >= 360) return { west: -180, south: s, east: 180, north: n };
  const wrap = (lon: number) => ((((lon + 180) % 360) + 360) % 360) - 180;
  const w = wrap(west);
  let e = wrap(east);
  // A view ending exactly on 180 would wrap to -180 and read as the world.
  if (e === -180 && east > west) e = 180;
  return { west: w, south: s, east: e, north: n };
}
