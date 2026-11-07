import { describe, expect, it } from 'vitest';
import { MAP_TILE_PX } from '../tiles/tileMath.ts';
import { stableFraction, stationPriority, thinByZoom, type ThinPoint } from './thinning.ts';

function randomPoints(n: number, seed = 7): ThinPoint[] {
  let s = seed;
  const r = () => ((s = (s * 1103515245 + 12345) % 2147483648) / 2147483648);
  // Clustered like airports: most in a few dense areas.
  return Array.from({ length: n }, (_, i) => {
    const c = i % 5;
    return { x: 0.2 + c * 0.03 + r() * 0.02, y: 0.37 + r() * 0.02, priority: r() };
  });
}

describe('thinByZoom', () => {
  const pts = randomPoints(3000);
  const zoom = thinByZoom(pts, 26, 3, 9);

  it('keeps every point shown at a zoom at least the spacing apart', () => {
    for (let z = 3; z < 9; z++) {
      const shown = pts.filter((_, i) => zoom[i] <= z);
      const px = MAP_TILE_PX * 2 ** z;
      let closest = Infinity;
      for (let i = 0; i < shown.length; i++) {
        for (let j = i + 1; j < shown.length; j++) {
          closest = Math.min(closest, Math.hypot((shown[i].x - shown[j].x) * px, (shown[i].y - shown[j].y) * px));
        }
      }
      expect(closest, `zoom ${z}, ${shown.length} shown`).toBeGreaterThanOrEqual(26);
    }
  });

  it('fills in as the zoom grows and shows everything at the last level', () => {
    const counts = [3, 4, 5, 6, 7, 8, 9].map((z) => pts.filter((_, i) => zoom[i] <= z).length);
    for (let k = 1; k < counts.length; k++) expect(counts[k]).toBeGreaterThan(counts[k - 1]);
    expect(counts.at(-1)).toBe(pts.length);
  });

  it('shows the higher-priority point of a close pair first', () => {
    const z = thinByZoom(
      [
        { x: 0.5, y: 0.5, priority: 1 },
        { x: 0.5 + 1e-5, y: 0.5, priority: 2 },
        { x: 0.9, y: 0.9, priority: 0 },
      ],
      26,
      3,
      9,
    );
    expect(z[1]).toBe(3);
    expect(z[0]).toBeGreaterThan(3);
    expect(z[2]).toBe(3);
  });

  it('gives the same answer every time', () => {
    expect(thinByZoom(pts, 26, 3, 9)).toEqual(zoom);
  });
});

describe('stationPriority', () => {
  const base = { temp_c: 10, dewpoint_c: 5, wind_speed_ms: 3, visibility_m: 16000, altimeter_hpa: 1013, flight_category: 'VFR' };
  it('puts main airports first, then stations with a temperature', () => {
    expect(stationPriority({ id: 'KORD', ...base, visibility_m: null })).toBeGreaterThan(stationPriority({ id: 'KPWK', ...base }));
    expect(stationPriority({ id: 'KPWK', ...base, flight_category: null, visibility_m: null })).toBeGreaterThan(stationPriority({ id: 'KUGN', ...base, temp_c: null }));
  });
  it('breaks ties the same way for the same station', () => {
    expect(stableFraction('KPWK')).toBe(stableFraction('KPWK'));
    expect(stableFraction('KPWK')).not.toBe(stableFraction('KUGN'));
    expect(stableFraction('KPWK')).toBeGreaterThanOrEqual(0);
    expect(stableFraction('KPWK')).toBeLessThan(1);
  });
});

describe('thinByZoom across the antimeridian', () => {
  it('treats stations either side of 180 degrees as neighbours', () => {
    // 0.0001 of the world apart, across the line: too close to show both early.
    const pts: ThinPoint[] = [
      { x: 0.99995, y: 0.3, priority: 2 },
      { x: 0.00005, y: 0.3, priority: 1 },
    ];
    const z = thinByZoom(pts, 26, 3, 9);
    expect(z[0]).toBe(3);
    expect(z[1]).toBeGreaterThan(3);
  });
});
