import { describe, expect, it } from 'vitest';
import { latToY, lonToX, subRect, type TileID, tileFraction, tileZoomFor, viewRect, xToLon, yToLat } from './tileMath.ts';

describe('mercator', () => {
  it('round-trips and matches tile corners', () => {
    expect(lonToX(-180)).toBe(0);
    expect(lonToX(0)).toBe(0.5);
    expect(latToY(0)).toBeCloseTo(0.5, 12);
    expect(xToLon(lonToX(-94.5786))).toBeCloseTo(-94.5786, 10);
    expect(yToLat(latToY(39.0997))).toBeCloseTo(39.0997, 10);
    // Tile 4/5/6's north edge, as in wx_core::proj.
    expect(yToLat(6 / 16)).toBeCloseTo(40.9799, 3);
  });

  it('places a point inside its tile', () => {
    const x = lonToX(-94.5786);
    const y = latToY(39.0997);
    const t: TileID = { z: 7, x: Math.floor(x * 128), y: Math.floor(y * 128) };
    expect(t).toEqual({ z: 7, x: 30, y: 48 });
    const f = tileFraction(t, x, y);
    expect(f.u).toBeGreaterThanOrEqual(0);
    expect(f.u).toBeLessThan(1);
    expect(f.v).toBeGreaterThanOrEqual(0);
    expect(f.v).toBeLessThan(1);
  });
});

describe('the view', () => {
  it('wants the nearest whole tile zoom, within limits', () => {
    expect(tileZoomFor(4.3, 14)).toBe(4);
    expect(tileZoomFor(4.6, 14)).toBe(5);
    expect(tileZoomFor(9.2, 7)).toBe(7);
    expect(tileZoomFor(-1, 7)).toBe(0);
  });

  it('spans the viewport in mercator units, unwrapped past the antimeridian', () => {
    const r = viewRect({ centerX: lonToX(-96), centerY: latToY(38.5), zoom: 4.3, width: 1440, height: 900 });
    const world = 512 * 2 ** 4.3;
    expect(r.x1 - r.x0).toBeCloseTo(1440 / world, 12);
    expect(r.y1 - r.y0).toBeCloseTo(900 / world, 12);
    // Centred on 175 W panned from Asia: the view keeps going past x = 1.
    const d = viewRect({ centerX: lonToX(185 - 360) + 1, centerY: 0.3, zoom: 2, width: 1200, height: 600 });
    expect(d.x0).toBeLessThan(1);
    expect(d.x1).toBeGreaterThan(1);
  });
});

describe('parent fallback', () => {
  it('finds the texture window inside an ancestor', () => {
    const t: TileID = { z: 6, x: 13, y: 22 };
    expect(subRect(t, { z: 4, x: 3, y: 5 })).toEqual({ ox: 0.25, oy: 0.5, s: 0.25 });
    expect(subRect(t, t)).toEqual({ ox: 0, oy: 0, s: 1 });
  });
});
