import { describe, expect, it } from 'vitest';
import type { Part } from '../api/types.ts';
import { box, part, WORLD } from '../testing.ts';
import { type Cell, FrameTiles, leafAt, planCells } from './plan.ts';
import { tileBounds } from './stack.ts';
import { latToY, lonToX } from './tileMath.ts';

const gfs = part('gfs-1', WORLD, WORLD, 4);
const gfs2 = part('gfs-2', WORLD, WORLD, 4);
/** A ragged radar over the US: deep, no solid box. */
const mrms = part('mrms-1', box(-130, 20, -60, 55), null, 7);
const europe = part('icon-1', box(-23.5, 29.5, 62.5, 70.5), box(-22.5, 30.5, 61.5, 69.5), 5);
const alaska = part('ak-1', box(160, 41.5, -116.5, 76.3), box(165, 50, -140, 70), 6);

const url = (z: number, x: number, y: number, parts: string) => `/tiles/radar/${z}/${x}/${y}/${parts}.wxt`;
const frame = (...parts: Part[]) => new FrameTiles(parts, url);

/** The mercator view of a lon/lat window (x unwrapped). */
const view = (west: number, south: number, east: number, north: number) => ({ x0: (west + 180) / 360, x1: (east + 180) / 360, y0: latToY(north), y1: latToY(south) });

/** Cells must tile the view exactly once over: no gaps, no overlaps. */
function coveredArea(cells: Cell[], v: ReturnType<typeof view>) {
  let area = 0;
  for (const c of cells) {
    const n = 2 ** c.z;
    const x0 = Math.max(c.x / n + c.wrap, v.x0);
    const x1 = Math.min((c.x + 1) / n + c.wrap, v.x1);
    const y0 = Math.max(c.y / n, Math.max(0, v.y0));
    const y1 = Math.min((c.y + 1) / n, Math.min(1, v.y1));
    area += Math.max(0, x1 - x0) * Math.max(0, y1 - y0);
  }
  return area;
}

describe('the quadtree walk', () => {
  it('stops at the global model’s zoom over the ocean and goes deep over the radar', () => {
    const v = view(-140, 10, -40, 60);
    const cells = planCells(v, 7, [frame(mrms, gfs)]);
    const zooms = new Set(cells.map((c) => c.z));
    expect(zooms.has(4)).toBe(true);
    expect(zooms.has(7)).toBe(true);
    for (const c of cells) {
      const leaf = c.leaves[0]!;
      const b = tileBounds(c.z, c.x, c.y);
      // Where the radar reaches, the tile goes as deep as the view wants;
      // elsewhere GFS's zoom is as deep as it goes.
      if (leaf.z === c.z) expect(c.z).toBe(leaf.parts.some((p) => p.id === 'mrms-1') ? 7 : 4);
      else expect(leaf.parts.some((p) => p.id === 'mrms-1')).toBe(true);
      // Never deeper than the tile's own stack offers: the server would 404.
      expect(leaf.z).toBeLessThanOrEqual(leaf.maxZoom);
      expect(leaf.url).toBe(url(leaf.z, leaf.x, leaf.y, leaf.parts.map((p) => p.id).join(',')));
      expect(b.west).toBeLessThan(-40);
    }
    // Just off the radar's box, a deep cell is drawn from its parent's tile.
    expect(cells.some((c) => c.leaves[0]!.z < c.z)).toBe(true);
    expect(coveredArea(cells, v)).toBeCloseTo((v.x1 - v.x0) * (v.y1 - v.y0), 12);
  });

  it('goes no deeper than the view wants, even where the data does', () => {
    const cells = planCells(view(-100, 30, -90, 40), 3, [frame(mrms, gfs)]);
    expect(cells.every((c) => c.z === 3)).toBe(true);
  });

  it('leaves an empty stack alone: nothing to request', () => {
    const cells = planCells(view(-10, 35, 20, 60), 6, [frame(mrms)]);
    expect(cells.length).toBeGreaterThan(0);
    // The walk stops as soon as no part reaches a tile.
    expect(cells.every((c) => c.leaves[0]!.url === null && c.leaves[0]!.parts.length === 0 && c.z <= 3)).toBe(true);
  });

  it('walks every copy of the world in view, in canonical columns', () => {
    // From 30 degrees west of the antimeridian, across it, into the next world.
    const v = { x0: lonToX(150), x1: lonToX(-150) + 1, y0: latToY(70), y1: latToY(45) };
    const cells = planCells(v, 6, [frame(alaska, gfs)]);
    const wraps = new Set(cells.map((c) => c.wrap));
    expect([...wraps].sort()).toEqual([0, 1]);
    for (const c of cells) {
      expect(c.x).toBeGreaterThanOrEqual(0);
      expect(c.x).toBeLessThan(2 ** c.z);
    }
    // Alaska's grid is named on both sides of the line.
    const east = cells.filter((c) => c.wrap === 0 && c.leaves[0]!.parts.some((p) => p.id === 'ak-1'));
    const west = cells.filter((c) => c.wrap === 1 && c.leaves[0]!.parts.some((p) => p.id === 'ak-1'));
    expect(east.length).toBeGreaterThan(0);
    expect(west.length).toBeGreaterThan(0);
    // Inside its solid box (which also wraps) GFS is hidden on both sides.
    const hidden = cells.filter((c) => c.leaves[0]!.parts.length === 1 && c.leaves[0]!.parts[0].id === 'ak-1');
    expect(hidden.some((c) => c.wrap === 0) && hidden.some((c) => c.wrap === 1)).toBe(true);
    expect(coveredArea(cells, v)).toBeCloseTo((v.x1 - v.x0) * (v.y1 - v.y0), 12);
  });

  it('plans two frames together as the finer of their tile sets', () => {
    const v = view(-140, 10, 40, 65);
    const a = frame(mrms, gfs);
    const b = frame(europe, gfs2);
    const cells = planCells(v, 6, [a, b]);
    expect(coveredArea(cells, v)).toBeCloseTo((v.x1 - v.x0) * (v.y1 - v.y0), 12);
    for (const c of cells) {
      for (const leaf of c.leaves) {
        // Each frame's tile is the cell or an ancestor of it.
        expect(leaf!.z).toBeLessThanOrEqual(c.z);
        expect(c.x >> (c.z - leaf!.z)).toBe(leaf!.x);
        expect(c.y >> (c.z - leaf!.z)).toBe(leaf!.y);
      }
    }
    // Over the US the first frame is the deep one, over Europe the second.
    const us = cells.find((c) => c.leaves[0]!.parts[0].id === 'mrms-1' && c.z === 6)!;
    expect(us.leaves[1]!.z).toBe(4);
    const eu = cells.find((c) => c.leaves[1]!.parts[0].id === 'icon-1' && c.z === 5)!;
    expect(eu.leaves[0]!.z).toBe(4);
  });

  it('skips frames that are not planned', () => {
    const cells = planCells(view(-100, 30, -90, 40), 5, [null, frame(gfs), null]);
    expect(cells.every((c) => c.leaves[0] === null && c.leaves[2] === null && c.leaves[1]!.z === 4)).toBe(true);
  });

  it('finds the same tile for a point as the plan draws there', () => {
    const f = frame(mrms, gfs);
    const v = view(-140, 10, -40, 60);
    const cells = planCells(v, 7, [f]);
    for (const [lon, lat] of [
      [-98, 38],
      [-50, 30],
      [-125.5, 49.9],
    ]) {
      const mx = lonToX(lon);
      const my = latToY(lat);
      const leaf = leafAt(f, mx, my, 7);
      const cell = cells.find((c) => {
        const n = 2 ** c.z;
        return mx >= c.x / n && mx < (c.x + 1) / n && my >= c.y / n && my < (c.y + 1) / n;
      })!;
      const drawn = cell.leaves[0]!;
      expect([leaf.z, leaf.x, leaf.y]).toEqual([drawn.z, drawn.x, drawn.y]);
    }
  });

  it('works out each tile’s stack once per frame', () => {
    const f = frame(mrms, gfs);
    expect(f.at(5, 7, 12)).toBe(f.at(5, 7, 12));
  });
});
