import { describe, expect, it } from 'vitest';
import { bilinearSample, dequantize, nearestPositiveSample, nearestSample } from './sampling.ts';

// 3 x 3 lattice, one channel.
const grid = (v: number[]) => new Uint16Array(v);

describe('lattice sampling', () => {
  const g = grid([10, 20, 30, 40, 50, 60, 70, 80, 90]);

  it('hits samples exactly on lattice points, including the far edge', () => {
    expect(bilinearSample(g, 3, 3, 1, 0, 0, 0)).toBe(10);
    expect(bilinearSample(g, 3, 3, 1, 0, 1, 1)).toBe(90);
    expect(bilinearSample(g, 3, 3, 1, 0, 0.5, 0.5)).toBe(50);
    expect(bilinearSample(g, 3, 3, 1, 0, 1, 0)).toBe(30);
  });

  it('interpolates between them', () => {
    expect(bilinearSample(g, 3, 3, 1, 0, 0.25, 0)).toBeCloseTo(15, 10);
    expect(bilinearSample(g, 3, 3, 1, 0, 0.25, 0.25)).toBeCloseTo(30, 10);
  });

  it('skips no-data and renormalises, but gives up when most weight is missing', () => {
    const holes = grid([10, 0, 30, 40, 50, 60, 70, 80, 90]);
    // Between 10, 0(no data), 40 and 50 at the cell centre: average of the three.
    expect(bilinearSample(holes, 3, 3, 1, 0, 0.25, 0.25)).toBeCloseTo((10 + 40 + 50) / 3, 10);
    expect(bilinearSample(holes, 3, 3, 1, 0, 0.5, 0)).toBeNull();
    expect(bilinearSample(holes, 3, 3, 1, 0, 0.45, 0.05)).toBeNull();
    expect(bilinearSample(holes, 3, 3, 1, 0, 0.2, 0.05)).not.toBeNull();
  });

  it('reads one channel of interleaved data', () => {
    const two = new Uint16Array([1, 101, 2, 102, 3, 103, 4, 104]);
    expect(bilinearSample(two, 2, 2, 2, 1, 1, 1)).toBe(104);
    expect(nearestSample(two, 2, 2, 2, 0, 0.9, 0.1)).toBe(2);
  });

  it('takes the nearest sample for categories', () => {
    expect(nearestSample(g, 3, 3, 1, 0, 0.3, 0.3)).toBe(50);
    expect(nearestSample(g, 3, 3, 1, 0, 0.3, 0.2)).toBe(20);
    expect(nearestSample(grid([0, 1, 1, 1]), 2, 2, 1, 0, 0.1, 0.1)).toBeNull();
  });

  it('takes the nearest positive type at the edge of a rain area', () => {
    const band = { scale: 1, offset: 0 };
    // Type codes (sample = code + 1): none, rain / none, snow.
    const types = grid([1, 2, 1, 3]);
    expect(nearestPositiveSample(types, 2, 2, 1, 0, 0.1, 0.1, band)).toBe(2);
    expect(nearestPositiveSample(types, 2, 2, 1, 0, 0.2, 0.9, band)).toBe(3);
    expect(nearestPositiveSample(grid([1, 1, 1, 1]), 2, 2, 1, 0, 0.5, 0.5, band)).toBeNull();
  });

  it('dequantizes like BandSpec::dequantize', () => {
    const dbz = { scale: 0.1, offset: -40 };
    expect(dequantize(dbz, 1)).toBe(-40);
    expect(dequantize(dbz, 101)).toBeCloseTo(-30, 10);
    expect(dequantize({ scale: 0.01, offset: -100 }, 10001)).toBeCloseTo(0, 10);
  });
});
