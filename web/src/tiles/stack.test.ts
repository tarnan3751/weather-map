import { describe, expect, it } from 'vitest';
import type { Part } from '../api/types.ts';
import { box, part, WORLD } from '../testing.ts';
import { boxContains, boxContainsPoint, boxesIntersect, stackMaxZoom, tileBounds, tileParts, topPartAt } from './stack.ts';

// The same fixtures as crates/wx-core/src/frame/tests.rs, so the client's
// answers can be compared with the server's line by line.
const CONUS = box(-130, 20, -60, 55);
const CONUS_SOLID = box(-120, 25, -70, 50);
/** Roughly Alaska: from 170E eastward to 130W. */
const ALASKA = box(170, 50, -130, 72);

const mrms = part('mrms-2611061200', CONUS, null, 7);
const rtma = part('rtma-2611061200', CONUS, CONUS_SOLID, 6);
const hrrr = part('hrrr-26110612-0', CONUS, CONUS_SOLID, 6, '2026-11-06T12:00:00Z');
const gfs = part('gfs-26110612-0', WORLD, WORLD, 4, '2026-11-06T12:00:00Z');

const ids = (ps: Part[]) => ps.map((p) => p.id);
const names = (ps: Part[], z: number, x: number, y: number) => ids(tileParts(ps, tileBounds(z, x, y)));
/** The tile at zoom `z` holding a point. */
function tileAt(z: number, lon: number, lat: number): [number, number, number] {
  const n = 2 ** z;
  const s = Math.sin((lat * Math.PI) / 180);
  return [z, Math.floor(((lon + 180) / 360) * n), Math.floor((0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI)) * n)];
}

describe('lon/lat boxes', () => {
  it('intersect only when they share area, edges excepted', () => {
    const a = box(0, 0, 10, 10);
    expect(boxesIntersect(a, box(10, 0, 20, 10))).toBe(false);
    expect(boxesIntersect(a, box(0, 10, 10, 20))).toBe(false);
    expect(boxesIntersect(a, box(9.999, 0, 20, 10))).toBe(true);
  });

  it('cross the antimeridian when west is east of east', () => {
    expect(boxesIntersect(ALASKA, box(175, 55, 180, 60))).toBe(true);
    expect(boxesIntersect(ALASKA, box(-180, 55, -175, 60))).toBe(true);
    expect(boxesIntersect(ALASKA, box(-129, 55, -100, 60))).toBe(false);
    expect(boxesIntersect(ALASKA, box(100, 55, 169.9, 60))).toBe(false);
    expect(boxesIntersect(box(170, 50, -170, 60), box(160, 40, -175, 55))).toBe(true);
    expect(boxContains(WORLD, ALASKA)).toBe(true);
    expect(boxContains(ALASKA, box(175, 55, -170, 60))).toBe(true);
    expect(boxContains(ALASKA, box(-160, 55, -140, 70))).toBe(true);
    expect(boxContains(ALASKA, box(175, 55, 180, 60))).toBe(true);
    expect(boxContains(ALASKA, box(-180, 55, -175, 60))).toBe(true);
    // An ordinary box never holds a wrapping one, however wide.
    expect(boxContains(box(-179, 50, 179, 72), box(175, 55, -175, 60))).toBe(false);
    expect(boxContains(ALASKA, box(160, 55, 175, 60))).toBe(false);
    expect(boxContainsPoint(ALASKA, 179.9, 60)).toBe(true);
    expect(boxContainsPoint(ALASKA, -179.9, 60)).toBe(true);
    expect(boxContainsPoint(ALASKA, -100, 60)).toBe(false);
  });

  it('give tiles the server’s bounds', () => {
    expect(tileBounds(0, 0, 0)).toEqual({ west: -180, east: 180, north: expect.closeTo(85.0511287798, 9), south: expect.closeTo(-85.0511287798, 9) });
    const t = tileBounds(3, 7, 2);
    expect(t.east).toBe(180);
    expect(tileBounds(3, 0, 2).west).toBe(-180);
  });
});

describe('the tile stack', () => {
  it('lets a solid part hide what is below', () => {
    const p = [rtma, hrrr, gfs];
    const inside = tileAt(6, -98, 38);
    expect(names(p, ...inside)).toEqual(['rtma-2611061200']);
    // Without the solid box the lower parts show through, down to the next one.
    expect(names([{ ...rtma, solid: null }, hrrr, gfs], ...inside)).toEqual(['rtma-2611061200', 'hrrr-26110612-0']);
    // Across CONUS's edge all three show; over Europe, GFS alone.
    expect(names(p, ...tileAt(2, -120, 40))).toEqual(ids(p));
    expect(names(p, ...tileAt(3, 10, 50))).toEqual(['gfs-26110612-0']);
  });

  it('leaves out a part that only touches the tile’s edge', () => {
    // Tile 1/1/0 spans 0..180 E; a box ending at 0 touches it, one ending
    // just past 0 overlaps it.
    const touching = part('hrrr-26110612-60', box(-10, 10, 0, 50), null, 6, '2026-11-06T12:00:00Z');
    const overlapping = { ...touching, bounds: box(-10, 10, 5e-7, 50) };
    expect(names([touching, gfs], 1, 1, 0)).toEqual(['gfs-26110612-0']);
    expect(names([overlapping, gfs], 1, 1, 0)).toEqual(['hrrr-26110612-60', 'gfs-26110612-0']);
    // Along a latitude edge too: tile 1/0/0's south edge is the equator.
    const south = part('rtma-2611061200', box(-100, -20, -50, 0), null, 6);
    expect(names([south, gfs], 1, 0, 0)).toEqual(['gfs-26110612-0']);
    // A solid box exactly as wide as the tile hides what is below.
    const edge = box(0, -1, 180, 89);
    expect(names([{ ...touching, bounds: edge, solid: edge }, gfs], 1, 1, 0)).toEqual(['hrrr-26110612-60']);
  });

  it('works across the antimeridian, with a wrapping solid box', () => {
    const alaska = part('mrms-2611061200', ALASKA, box(165, 50, -140, 70), 7);
    const p = [alaska, gfs];
    const both = ['mrms-2611061200', 'gfs-26110612-0'];
    // Either side of the line, and the column the box ends in.
    expect(names(p, 3, 7, 2)).toEqual(both);
    expect(names(p, 3, 0, 2)).toEqual(both);
    expect(names(p, 3, 1, 2)).toEqual(both);
    expect(names(p, 3, 4, 2)).toEqual(['gfs-26110612-0']);
    // The solid box, which crosses the line too, hides GFS on both sides.
    expect(names(p, 5, 31, 9)).toEqual(['mrms-2611061200']);
    expect(names(p, 5, 0, 9)).toEqual(['mrms-2611061200']);
  });

  it('names every part that reaches the z0 tile, unless a world-wide solid part is on top', () => {
    const alaska = part('rtma-2611061200', ALASKA, null, 6);
    const p = [mrms, alaska, hrrr, gfs];
    expect(names(p, 0, 0, 0)).toEqual(ids(p));
    expect(names([gfs, mrms], 0, 0, 0)).toEqual(['gfs-26110612-0']);
  });

  it('offers zooms as deep as its deepest part, and names the top part at a point', () => {
    expect(stackMaxZoom([mrms, gfs])).toBe(7);
    expect(stackMaxZoom([])).toBe(-1);
    expect(topPartAt([mrms, gfs], -98, 38)?.id).toBe('mrms-2611061200');
    expect(topPartAt([mrms, gfs], 10, 50)?.id).toBe('gfs-26110612-0');
    expect(topPartAt([part('a-1', ALASKA, null, 6), gfs], -179.5, 60)?.id).toBe('a-1');
  });
});
