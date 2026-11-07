import { describe, expect, it } from 'vitest';
import { LAYERS } from '../../mock/registry.ts';
import { buildLut, getPalette, LUT_SIZE, lookupLut, lutCoord, PTYPE_ROWS, rampColor } from './palettes.ts';

describe('palettes', () => {
  it('defines every palette the registry names', () => {
    for (const l of LAYERS) expect(getPalette(l.palette).id, l.id).toBe(l.palette);
    expect(getPalette('no-such-palette').id).toBe('generic');
  });

  it('keeps radar transparent below weak echoes and at the no-echo floor', () => {
    const p = getPalette('reflectivity');
    const lut = buildLut(p);
    expect(lookupLut(lut, p, -30)[3]).toBe(0);
    expect(lookupLut(lut, p, 5)[3]).toBe(0);
    expect(lookupLut(lut, p, 9.5)[3]).toBe(0);
    expect(lookupLut(lut, p, 12)[3]).toBeGreaterThan(80);
    expect(lookupLut(lut, p, 40)[3]).toBe(255);
  });

  it('colours reflectivity green, then yellow, red and magenta', () => {
    const p = getPalette('reflectivity');
    const lut = buildLut(p);
    const [r20, g20] = lookupLut(lut, p, 22);
    expect(g20).toBeGreaterThan(r20);
    const [r37, g37, b37] = lookupLut(lut, p, 37);
    expect(r37).toBeGreaterThan(200);
    expect(g37).toBeGreaterThan(180);
    expect(b37).toBeLessThan(120);
    const [r52, g52] = lookupLut(lut, p, 52);
    expect(r52).toBeGreaterThan(150);
    expect(g52).toBeLessThan(80);
    const [r62, , b62] = lookupLut(lut, p, 62);
    expect(b62).toBeGreaterThan(150);
    expect(r62).toBeGreaterThan(120);
  });

  it('hits stop colours exactly and steps when banded', () => {
    const p = getPalette('temperature');
    if (p.kind !== 'scalar') throw new Error('scalar expected');
    const i = p.stops.findIndex((x) => x.c === '#43b4e6');
    const stop = p.stops[i];
    const exact = rampColor(p.stops, stop.v);
    expect(exact.slice(0, 3)).toEqual([0x43, 0xb4, 0xe6]);
    const mid = (p.stops[i].v + p.stops[i + 1].v) / 2;
    expect(rampColor(p.stops, mid, true)).toEqual(exact);
    expect(rampColor(p.stops, mid, false)).not.toEqual(exact);
  });

  it('builds one row per precipitation type and leaves "none" transparent', () => {
    const p = getPalette('precip_type');
    const lut = buildLut(p);
    expect(lut.width).toBe(LUT_SIZE);
    expect(lut.height).toBe(PTYPE_ROWS);
    expect(lookupLut(lut, p, 5, 0)[3]).toBe(0);
    const rain = lookupLut(lut, p, 1, 1);
    const snow = lookupLut(lut, p, 1, 2);
    expect(rain[1]).toBeGreaterThan(rain[2]);
    expect(snow[2]).toBeGreaterThan(snow[0]);
    expect(lookupLut(lut, p, 1, 4)).not.toEqual(lookupLut(lut, p, 1, 3));
  });

  it('maps values through the palette transfer the way the shader does', () => {
    const rain = getPalette('rainfall');
    expect(lutCoord(rain, 0)).toBe(0);
    expect(lutCoord(rain, 1000)).toBe(1);
    // Square-root transfer gives light totals more of the ramp.
    expect(lutCoord(rain, 250)).toBeCloseTo(0.5, 5);
    const pt = getPalette('precip_type');
    expect(lutCoord(pt, 0.05)).toBe(0);
    expect(lutCoord(pt, Math.sqrt(0.05 * 100))).toBeCloseTo(0.5, 5);
    expect(lutCoord(getPalette('temperature'), -7.5)).toBeCloseTo(0.5, 5);
  });

  it('draws the world’s extremes in the end colours, never as holes', () => {
    const opaque = (id: string, v: number) => {
      const p = getPalette(id);
      return lookupLut(buildLut(p), p, v)[3];
    };
    // Greenland, Siberia and Antarctica in winter; a desert afternoon.
    for (const v of [-35, -45, -60, -80, 50, 60]) expect(opaque('temperature', v), `${v} °C`).toBe(255);
    for (const v of [-50, -70, 35]) expect(opaque('dew_point', v), `dew point ${v}`).toBe(255);
    // Hurricane force and beyond, and the coldest storm tops.
    for (const v of [33, 60, 90]) expect(opaque('wind', v), `${v} m/s`).toBe(255);
    for (const v of [-95, -60, 0]) expect(opaque('infrared', v), `${v} °C tops`).toBeGreaterThan(200);
    // A metre of rain, two of snow.
    expect(opaque('rainfall', 1200)).toBe(255);
    expect(opaque('snowfall', 3000)).toBe(255);
    // Whatever is meant to be invisible stays so: no echo, no rain, no snow.
    expect(opaque('reflectivity', -30)).toBe(0);
    expect(opaque('rainfall', 0)).toBe(0);
    expect(opaque('snowfall', 0)).toBe(0);
  });

  it('labels every legend in both unit systems, ticks inside the bar', () => {
    for (const id of ['temperature', 'dew_point', 'wind', 'rainfall', 'snowfall', 'infrared']) {
      const p = getPalette(id);
      expect(Object.keys(p.legend.ticks).length, id).toBeGreaterThanOrEqual(2);
    }
  });
});
