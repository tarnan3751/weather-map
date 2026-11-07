import { describe, expect, it } from 'vitest';
import { lowCentres, NO_ECHO, prepareInfrared, prepareRadar, prepareSurface, setEpoch, type Quality } from './fields.ts';
import { xToLon, yToLat } from './geo.ts';
import { landWeight } from './land.ts';
import { GEOREF_CITY } from './places.ts';

const HOUR = 3_600_000;
const EPOCH = Date.UTC(2026, 10, 6, 12);
setEpoch(EPOCH);
const at = (h: number) => EPOCH + h * HOUR;

const QUALITIES: Quality[] = ['obs', 'sat', 'hrrr', 'gfs'];
const LATS = Array.from({ length: 341 }, (_, i) => -85 + i * 0.5);
const EAST = 179.9999;
const WEST = -179.9999;
/** Echo below the drawing threshold counts as none on both sides. */
const shown = (dbz: number) => Math.max(5, dbz);

/** Point `km` east (negative: west) and `northKm` north of a place. */
function moved(lon: number, lat: number, km: number, northKm = 0): [number, number] {
  return [lon + km / (111.32 * Math.cos((lat * Math.PI) / 180)), lat + northKm / 111.32];
}

describe('synthetic fields across the antimeridian', () => {
  it('reflectivity meets itself at 180 degrees', () => {
    for (const q of QUALITIES) {
      for (const h of [-12, 0, 6, 20, 72, 160]) {
        const r = prepareRadar(at(h), q, { runMs: q === 'obs' || q === 'sat' ? undefined : at(Math.floor(h / 6) * 6 - 6) });
        for (const lat of LATS) expect(Math.abs(shown(r.dbz(EAST, lat)) - shown(r.dbz(WEST, lat))), `${q} h${h} lat ${lat}`).toBeLessThan(0.5);
      }
    }
  });

  it('the Aleutian low rains on both sides of the dateline in the first day', () => {
    for (const h of [0, 12, 24]) {
      const r = prepareRadar(at(h), 'obs');
      let east = 0;
      let west = 0;
      for (let lat = 40; lat <= 60; lat += 0.25) {
        for (let d = 0.25; d <= 6; d += 0.25) {
          if (r.dbz(180 - d, lat) > 15) east++;
          if (r.dbz(-180 + d, lat) > 15) west++;
        }
      }
      expect(east, `h${h} east`).toBeGreaterThan(50);
      expect(west, `h${h} west`).toBeGreaterThan(50);
    }
  });

  it('surface fields meet themselves at 180 degrees', () => {
    for (const q of ['obs', 'gfs'] as Quality[]) {
      for (const h of [-12, 0, 9, 30, 100, 168]) {
        const s = prepareSurface(at(h), q);
        const r = prepareRadar(at(h), q);
        for (const lat of LATS) {
          const where = `${q} h${h} lat ${lat}`;
          expect(Math.abs(s.temp(EAST, lat) - s.temp(WEST, lat)), `temp ${where}`).toBeLessThan(0.05);
          expect(Math.abs(s.dewpoint(EAST, lat) - s.dewpoint(WEST, lat)), `dew ${where}`).toBeLessThan(0.05);
          const a = s.wind(EAST, lat);
          const b = s.wind(WEST, lat);
          expect(Math.abs(a.u - b.u) + Math.abs(a.v - b.v), `wind ${where}`).toBeLessThan(0.05);
          expect(Math.abs(a.gust - b.gust), `gust ${where}`).toBeLessThan(0.1);
          const ca = s.clouds(EAST, lat, r.dbz(EAST, lat));
          const cb = s.clouds(WEST, lat, r.dbz(WEST, lat));
          expect(Math.abs(ca - cb), `clouds ${where}`).toBeLessThan(0.5);
        }
      }
    }
  });

  it('takes any longitude, the same one a turn apart', () => {
    const r = prepareRadar(at(3), 'obs');
    const s = prepareSurface(at(3), 'obs');
    for (const [lon, lat] of [[-150, 7], [178.5, 52], [12, 40], [-95, 39]]) {
      expect(r.dbz(lon + 360, lat)).toBeCloseTo(r.dbz(lon, lat), 6);
      expect(s.temp(lon - 360, lat)).toBeCloseTo(s.temp(lon, lat), 6);
    }
  });
});

describe('the georeferencing storm', () => {
  it('has a crisp core over Kansas City and nothing 160 km east', () => {
    for (const h of [0, 0.5, 2]) {
      for (const q of ['obs', 'hrrr'] as Quality[]) {
        const r = prepareRadar(at(h), q);
        expect(r.dbz(GEOREF_CITY.lon, GEOREF_CITY.lat)).toBeGreaterThan(60);
        // The core falls off within a few km.
        const [lon5, lat5] = moved(GEOREF_CITY.lon, GEOREF_CITY.lat, 15);
        expect(r.dbz(lon5, lat5)).toBeLessThan(r.dbz(GEOREF_CITY.lon, GEOREF_CITY.lat) - 8);
        for (let k = -12; k <= 12; k += 3) {
          const [lon, lat] = moved(GEOREF_CITY.lon, GEOREF_CITY.lat, 160 + k, k);
          expect(r.dbz(lon, lat), `${q} h${h}`).toBeLessThan(15);
        }
      }
    }
  });

  it('stays exactly on the city in the satellite estimate', () => {
    const r = prepareRadar(at(0), 'sat');
    const centre = r.dbz(GEOREF_CITY.lon, GEOREF_CITY.lat);
    expect(centre).toBeGreaterThan(40);
    for (const [dx, dy] of [[10, 0], [-10, 0], [0, 10], [0, -10]]) {
      const [lon, lat] = moved(GEOREF_CITY.lon, GEOREF_CITY.lat, dx, dy);
      expect(r.dbz(lon, lat)).toBeLessThanOrEqual(centre);
    }
  });
});

describe('land mask', () => {
  it('knows land from sea and wraps in longitude', () => {
    expect(landWeight(10, 23)).toBeGreaterThan(0.95); // Sahara
    expect(landWeight(-97, 38)).toBeGreaterThan(0.95); // Kansas
    expect(landWeight(0, -85)).toBeGreaterThan(0.95); // Antarctica
    expect(landWeight(-150, 0)).toBe(0); // mid-Pacific
    expect(landWeight(-30, 40)).toBe(0); // mid-Atlantic
    expect(landWeight(51, 42)).toBeLessThan(0.05); // the Caspian is water
    for (const lat of [-80, -17, 0, 66, 71.2]) {
      expect(landWeight(190, lat)).toBeCloseTo(landWeight(-170, lat), 9);
      expect(Math.abs(landWeight(EAST, lat) - landWeight(WEST, lat))).toBeLessThan(0.01);
    }
    for (let lat = -90; lat <= 90; lat += 7.5) {
      const v = landWeight(33, lat);
      expect(v).toBeGreaterThanOrEqual(0);
      expect(v).toBeLessThanOrEqual(1);
    }
  });
});

describe('plausibility', () => {
  it('keeps temperature, dew point and wind in range everywhere', () => {
    for (const q of ['obs', 'gfs'] as Quality[]) {
      for (const h of [-12, 0, 7, 19, 50, 90, 130, 168]) {
        const s = prepareSurface(at(h), q);
        for (let lat = -90; lat <= 90; lat += 2.5) {
          for (let lon = -180; lon < 180; lon += 5) {
            const t = s.temp(lon, lat);
            const d = s.dewpoint(lon, lat);
            const w = s.wind(lon, lat);
            expect(t).toBeGreaterThan(-60);
            expect(t).toBeLessThan(50);
            expect(d).toBeLessThanOrEqual(t);
            expect(d).toBeGreaterThan(-75);
            expect(Math.hypot(w.u, w.v)).toBeLessThan(80);
            expect(w.gust).toBeLessThan(80);
            expect(Number.isFinite(w.u + w.v + w.gust + t + d)).toBe(true);
            const c = s.clouds(lon, lat, NO_ECHO);
            expect(c).toBeGreaterThanOrEqual(0);
            expect(c).toBeLessThanOrEqual(100);
          }
        }
      }
    }
  });

  it('looks like November', () => {
    const s = prepareSurface(at(0), 'obs');
    // Daily means, from noon and midnight local time.
    const mean = (lon: number, lat: number) => {
      const a = prepareSurface(at(12 - lon / 15), 'obs').temp(lon, lat);
      const b = prepareSurface(at(24 - lon / 15), 'obs').temp(lon, lat);
      return (a + b) / 2;
    };
    expect(mean(-160, 0)).toBeGreaterThan(24); // equatorial Pacific
    expect(mean(-160, 0)).toBeLessThan(30);
    expect(mean(125, 63)).toBeLessThan(-12); // Siberia
    expect(mean(-100, 62)).toBeLessThan(-5); // northern Canada
    expect(mean(-40, 73)).toBeLessThan(-20); // Greenland
    expect(mean(0, -85)).toBeLessThan(-25); // Antarctic plateau
    expect(mean(133, -24)).toBeGreaterThan(22); // interior Australia in spring
    expect(mean(2, 48.8)).toBeGreaterThan(mean(-97, 49)); // maritime Europe milder than the prairies
    expect(mean(88, 33)).toBeLessThan(mean(80, 25) - 12); // Tibet
    // Deserts dry, the tropics humid.
    const spread = (lon: number, lat: number) => s.temp(lon, lat) - s.dewpoint(lon, lat);
    expect(spread(12, 24)).toBeGreaterThan(12);
    expect(spread(-150, 5)).toBeLessThan(6);
  });

  it('has trades and westerlies', () => {
    let trades = 0;
    let westerlies = 0;
    const s = prepareSurface(at(0), 'gfs');
    for (let lon = -180; lon < 180; lon += 5) {
      trades += s.wind(lon, 14).u + s.wind(lon, -14).u;
      westerlies += s.wind(lon, -50).u;
    }
    expect(trades / 144).toBeLessThan(-2);
    expect(westerlies / 72).toBeGreaterThan(4);
  });

  it('turns lows anticlockwise in the north and clockwise in the south', () => {
    const ms = at(24);
    const s = prepareSurface(ms, 'obs');
    const lows = lowCentres(ms).filter((c) => c.kind === 'low' && c.strength > 12);
    expect(lows.some((c) => c.hemi < 0)).toBe(true);
    expect(lows.some((c) => c.hemi > 0)).toBe(true);
    for (const c of lows) {
      const [ex, ey] = moved(c.lon, c.lat, 400);
      const [wx, wy] = moved(c.lon, c.lat, -400);
      // East of a low the wind blows north in the northern hemisphere
      // (anticlockwise) and south in the southern (clockwise); west of it,
      // the other way.
      const dv = s.wind(ex, ey).v - s.wind(wx, wy).v;
      if (c.hemi > 0) expect(dv, `low at ${c.lon},${c.lat}`).toBeGreaterThan(8);
      else expect(dv, `low at ${c.lon},${c.lat}`).toBeLessThan(-8);
    }
    const [typhoon] = lowCentres(ms).filter((c) => c.kind === 'cyclone');
    const [nx, ny] = moved(typhoon.lon, typhoon.lat, 0, 120);
    expect(s.wind(nx, ny).u).toBeLessThan(-15); // easterly north of the eye
  });

  it('puts the typhoon in the Philippine Sea heading north-west', () => {
    const a = lowCentres(at(0)).find((c) => c.kind === 'cyclone')!;
    const b = lowCentres(at(48)).find((c) => c.kind === 'cyclone')!;
    expect(a.lon).toBeGreaterThan(125);
    expect(a.lon).toBeLessThan(135);
    expect(a.lat).toBeGreaterThan(13);
    expect(a.lat).toBeLessThan(18);
    expect(b.lon).toBeLessThan(a.lon);
    expect(b.lat).toBeGreaterThan(a.lat);
    const r = prepareRadar(at(0), 'obs');
    expect(r.dbz(a.lon, a.lat)).toBeLessThan(20); // the eye
    let wall = -Infinity;
    for (let k = 0; k < 16; k++) {
      const ang = (k * Math.PI) / 8;
      const [lon, lat] = moved(a.lon, a.lat, 38 * Math.cos(ang), 38 * Math.sin(ang));
      wall = Math.max(wall, r.dbz(lon, lat));
    }
    expect(wall).toBeGreaterThan(40);
  });
});

describe('sources', () => {
  it('caps the satellite estimate and the global model', () => {
    const sat = prepareRadar(at(0), 'sat', { skipGeoref: true });
    const obs = prepareRadar(at(0), 'obs', { skipGeoref: true });
    let satMax = -Infinity;
    let obsMax = -Infinity;
    let satWet = 0;
    for (let lat = -60; lat <= 70; lat += 0.5) {
      for (let lon = -180; lon < 180; lon += 0.5) {
        const a = sat.dbz(lon, lat);
        satMax = Math.max(satMax, a);
        obsMax = Math.max(obsMax, obs.dbz(lon, lat));
        if (a > 15) satWet++;
      }
    }
    expect(satMax).toBeLessThanOrEqual(50);
    expect(obsMax).toBeGreaterThan(satMax);
    expect(satWet).toBeGreaterThan(1000);
  });

  it('is deterministic, and different runs give different forecasts', () => {
    const ms = at(30);
    const a = prepareRadar(ms, 'hrrr', { runMs: at(6) });
    const b = prepareRadar(ms, 'hrrr', { runMs: at(6) });
    const c = prepareRadar(ms, 'hrrr', { runMs: at(7) });
    let same = true;
    let differ = 0;
    for (let lat = -10; lat <= 20; lat += 0.25) {
      for (let lon = 100; lon <= 180; lon += 0.25) {
        if (a.dbz(lon, lat) !== b.dbz(lon, lat)) same = false;
        if (Math.abs(shown(a.dbz(lon, lat)) - shown(c.dbz(lon, lat))) > 3) differ++;
      }
    }
    expect(same).toBe(true);
    expect(differ).toBeGreaterThan(100);
  });

  it('honours MOCK_WIND', () => {
    const before = process.env.MOCK_WIND;
    try {
      process.env.MOCK_WIND = 'westerly';
      expect(prepareSurface(at(0), 'obs').wind(150, -40)).toEqual({ u: 10, v: 0, gust: 14 });
      process.env.MOCK_WIND = 'vortex';
      const [lon, lat] = moved(-98, 38.5, 600);
      const w = prepareSurface(at(0), 'obs').wind(lon, lat);
      expect(w.v).toBeCloseTo(15, 0);
    } finally {
      if (before === undefined) delete process.env.MOCK_WIND;
      else process.env.MOCK_WIND = before;
    }
  });
});

describe('infrared', () => {
  const box = (tb: (lon: number, lat: number) => number, lon0: number, lon1: number, lat0: number, lat1: number, step = 0.5) => {
    const out: number[] = [];
    for (let lat = lat0; lat <= lat1; lat += step) for (let lon = lon0; lon <= lon1; lon += step) out.push(tb(lon, lat));
    return out.sort((a, b) => a - b);
  };
  const median = (v: number[]) => v[Math.floor(v.length / 2)];

  it('meets itself at 180 degrees', () => {
    for (const q of ['obs', 'hrrr', 'gfs'] as Quality[]) {
      for (const h of [-12, 0, 20, 100, 168]) {
        const ir = prepareInfrared(at(h), q, { runMs: q === 'obs' ? undefined : at(Math.floor(h / 6) * 6 - 6) });
        for (const lat of LATS) expect(Math.abs(ir.tb(EAST, lat) - ir.tb(WEST, lat)), `${q} h${h} lat ${lat}`).toBeLessThan(0.5);
      }
    }
  });

  it('stays between -90 and +55', () => {
    for (const q of ['obs', 'gfs'] as Quality[]) {
      for (const h of [0, 9, 40, 130]) {
        const ir = prepareInfrared(at(h), q);
        for (let lat = -90; lat <= 90; lat += 2.5) {
          for (let lon = -180; lon < 180; lon += 2.5) {
            const t = ir.tb(lon, lat);
            expect(Number.isFinite(t)).toBe(true);
            expect(t).toBeGreaterThanOrEqual(-90);
            expect(t).toBeLessThanOrEqual(55);
          }
        }
      }
    }
  });

  it('is coldest over deep convection, where the radar has strong echo', () => {
    const ms = at(0);
    const ir = prepareInfrared(ms, 'obs');
    const radar = prepareRadar(ms, 'obs');
    expect(box((lon, lat) => ir.tb(lon, lat), -180, 179.5, -20, 20)[0]).toBeLessThan(-65);
    let strong = 0;
    for (let lat = -25; lat <= 25; lat += 0.25) {
      for (let lon = -180; lon < 180; lon += 0.25) {
        if (radar.dbz(lon, lat) > 45) {
          strong++;
          expect(ir.tb(lon, lat), `core at ${lon},${lat}`).toBeLessThan(-60);
        }
      }
    }
    expect(strong).toBeGreaterThan(20);
  });

  it('shows the typhoon: a warm eye inside a very cold eyewall and overcast', () => {
    for (const h of [0, 24]) {
      const ir = prepareInfrared(at(h), 'obs');
      const c = lowCentres(at(h)).find((x) => x.kind === 'cyclone')!;
      const ring = (km: number) =>
        Array.from({ length: 32 }, (_, k) => ir.tb(...moved(c.lon, c.lat, km * Math.cos((k * Math.PI) / 16), km * Math.sin((k * Math.PI) / 16)))).sort((a, b) => a - b);
      expect(median(ring(45)), `h${h} eyewall`).toBeLessThan(-70);
      expect(median(ring(180)), `h${h} overcast`).toBeLessThan(-55);
      expect(ir.tb(c.lon, c.lat), `h${h} eye`).toBeGreaterThan(median(ring(45)) + 40);
    }
  });

  it('sees the ground where clear: hot deserts by day, cold polar night, stratocumulus near the sea surface', () => {
    // 13 UTC is early afternoon over the Sahara and night in eastern Siberia.
    const day = prepareInfrared(at(1), 'obs');
    expect(median(box((lon, lat) => day.tb(lon, lat), 0, 25, 18, 28))).toBeGreaterThan(35);
    expect(median(box((lon, lat) => day.tb(lon, lat), 100, 140, 62, 72))).toBeLessThan(-30);
    // 05 UTC is early afternoon over interior Australia.
    const oz = prepareInfrared(at(-7), 'obs');
    expect(median(box((lon, lat) => oz.tb(lon, lat), 125, 140, -30, -20))).toBeGreaterThan(35);
    const now = prepareInfrared(at(0), 'obs');
    const deck = median(box((lon, lat) => now.tb(lon, lat), -85, -78, -20, -12));
    expect(deck).toBeGreaterThan(5);
    expect(deck).toBeLessThan(22);
    expect(median(box((lon, lat) => now.tb(lon, lat), -45, -35, 70, 76))).toBeLessThan(-30); // Greenland
    // The Iceland low's frontal shield.
    const shield = box((lon, lat) => now.tb(lon, lat), -30, 10, 45, 65);
    expect(shield[0]).toBeLessThan(-40);
    expect(shield[0]).toBeGreaterThan(-65);
  });

  it('is deterministic, and smoother for models', () => {
    const a = prepareInfrared(at(5), 'obs');
    const b = prepareInfrared(at(5), 'obs');
    const g = prepareInfrared(at(5), 'gfs');
    let roughObs = 0;
    let roughGfs = 0;
    for (let lat = -10; lat <= 20; lat += 0.05) {
      expect(a.tb(130, lat)).toBe(b.tb(130, lat));
      roughObs += Math.abs(a.tb(130, lat + 0.05) - a.tb(130, lat));
      roughGfs += Math.abs(g.tb(130, lat + 0.05) - g.tb(130, lat));
    }
    expect(roughGfs).toBeLessThan(roughObs);
  });
});

describe('cost', () => {
  it('fills a world tile lattice quickly', () => {
    const N = 257;
    const lons = Array.from({ length: N }, (_, i) => xToLon(i / (N - 1)));
    const lats = Array.from({ length: N }, (_, j) => yToLat(j / (N - 1)));
    // Warm up, then time a fresh frame.
    prepareRadar(at(1), 'obs').dbz(0, 0);
    const t0 = performance.now();
    const r = prepareRadar(at(2.25), 'obs');
    let sum = 0;
    for (const lat of lats) for (const lon of lons) sum += r.dbz(lon, lat);
    const radarMs = performance.now() - t0;
    const s = prepareSurface(at(2.25), 'obs');
    const t1 = performance.now();
    for (let j = 0; j < N; j += 2) for (let i = 0; i < N; i += 2) sum += s.temp(lons[i], lats[j]) + s.wind(lons[i], lats[j]).u;
    const surfaceMs = performance.now() - t1;
    expect(Number.isFinite(sum)).toBe(true);
    expect(radarMs).toBeLessThan(1500);
    expect(surfaceMs).toBeLessThan(1500);
    const t2 = performance.now();
    const ir = prepareInfrared(at(2.25), 'obs');
    for (let j = 0; j < N; j += 2) for (let i = 0; i < N; i += 2) sum += ir.tb(lons[i], lats[j]);
    expect(performance.now() - t2).toBeLessThan(1500);
    expect(Number.isFinite(sum)).toBe(true);
  });
});
