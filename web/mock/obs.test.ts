import { describe, expect, it } from 'vitest';
import type { HourlyPoint, PointForecast, StationProps } from '../src/api/types.ts';
import { setEpoch } from './fields.ts';
import { metarTime, pointForecast, stations } from './obs.ts';

const MIN = 60_000;
const HOUR = 60 * MIN;
const NOW = Date.UTC(2026, 10, 6, 12, 1);
setEpoch(NOW - 6 * HOUR);

const CODE = /^(clearsky|fair|partlycloudy|cloudy|fog|(light|heavy)?(rain|sleet|snow)(showers)?(andthunder)?)(_(day|night|polartwilight))?$/;
const HAS_VARIANT = /^(clearsky|fair|partlycloudy|.*showers.*)$/;
const DATE = /^\d{4}-\d{2}-\d{2}$/;

function ok(lat: number, lon: number): PointForecast {
  const r = pointForecast(lat, lon, NOW);
  if (r.status !== 200) throw new Error(`expected a forecast at ${lat}, ${lon}`);
  return r.body;
}

/** Steps in time order, back to back, none already over. */
function expectSteps(hourly: HourlyPoint[]) {
  expect(hourly.length).toBeGreaterThan(0);
  let prevEnd: number | null = null;
  for (const h of hourly) {
    const t = Date.parse(h.time);
    expect(Number.isFinite(t)).toBe(true);
    expect(t + h.span_h * HOUR).toBeGreaterThan(NOW);
    if (prevEnd !== null) expect(t).toBe(prevEnd);
    prevEnd = t + h.span_h * HOUR;
  }
  expect(Date.parse(hourly[0].time)).toBeLessThanOrEqual(NOW);
}

function expectWellFormed(f: PointForecast) {
  expect(f.attribution.length).toBeGreaterThan(0);
  expect(Date.parse(f.generated_at!)).toBeLessThanOrEqual(NOW);
  expect(NOW - Date.parse(f.generated_at!)).toBeLessThan(2 * HOUR);
  expectSteps(f.hourly);
  for (const h of f.hourly) {
    if (h.condition !== null) {
      expect(h.condition).toMatch(CODE);
      const base = h.condition.replace(/_(day|night|polartwilight)$/, '');
      expect(h.condition !== base).toBe(HAS_VARIANT.test(base));
    }
    if (h.wind_dir_deg !== null) {
      expect(Number.isInteger(h.wind_dir_deg)).toBe(true);
      expect(h.wind_dir_deg).toBeGreaterThanOrEqual(0);
      expect(h.wind_dir_deg).toBeLessThan(360);
    }
    expect(typeof h.text).toBe('string');
  }
  let prev = '';
  for (const d of f.daily) {
    expect(d.date).toMatch(DATE);
    expect(d.date > prev).toBe(true);
    prev = d.date;
    if (d.condition !== null) expect(d.condition).toMatch(/^(clearsky|fair|partlycloudy|cloudy|fog|(light|heavy)?(rain|sleet|snow)(showers)?(andthunder)?)$/);
    if (d.low_c !== null && d.high_c !== null) expect(d.high_c).toBeGreaterThanOrEqual(d.low_c);
  }
}

describe('mock point forecast', () => {
  it('answers NWS-style for Oklahoma', () => {
    const f = ok(35.2226, -97.4395);
    expect(f).toMatchObject({ lat: 35.2226, lon: -97.4395, source: 'nws', attribution: 'Forecast from the US National Weather Service', place: 'Norman, OK', time_zone: 'America/Chicago' });
    expectWellFormed(f);
    expect(f.hourly.length).toBeGreaterThanOrEqual(150);
    expect(f.hourly.length).toBeLessThanOrEqual(160);
    expect(f.hourly.every((h) => h.span_h === 1 && typeof h.pop === 'number' && h.precip_mm === null)).toBe(true);
    expect(f.daily.length).toBeGreaterThanOrEqual(6);
    expect(f.daily.length).toBeLessThanOrEqual(8);
    // Today's local date in Chicago comes first.
    expect(f.daily[0].date).toBe('2026-11-06');
    for (const d of f.daily) {
      expect(d.precip_mm).toBeNull();
      expect(typeof d.pop).toBe('number');
      expect(d.text).toMatch(/^(Today|This Afternoon|Tonight|Overnight|[A-Z][a-z]+day( Night)?): /);
      // The NWS writes Fahrenheit and miles per hour.
      expect(d.text).toMatch(/(high near|low around) -?\d+\./);
      expect(d.high_c !== null || d.low_c !== null).toBe(true);
    }
  });

  it('names the distance to the nearest town away from one', () => {
    expect(ok(44.0, -105.0).place).toMatch(/^\d+ mi from [A-Z][\w .'-]+, [A-Z]{2}$/);
  });

  it('uses the NWS for Alaska, Hawaii, Puerto Rico and across the antimeridian in the Aleutians', () => {
    expect(ok(61.2, -149.9)).toMatchObject({ source: 'nws', time_zone: 'America/Anchorage' });
    expect(ok(21.3, -157.85)).toMatchObject({ source: 'nws', time_zone: 'Pacific/Honolulu' });
    expect(ok(18.4, -66.1)).toMatchObject({ source: 'nws', time_zone: 'America/Puerto_Rico' });
    expect(ok(52.7, 174.1)).toMatchObject({ source: 'nws', place: 'Shemya, AK' });
  });

  it('answers MET Norway-style elsewhere, with six-hour steps further out', () => {
    const f = ok(-1.2921, 36.822);
    expect(f).toMatchObject({ source: 'met-no', attribution: 'Based on data from MET Norway', place: null, time_zone: null });
    expectWellFormed(f);
    const hourly = f.hourly.filter((h) => h.span_h === 1);
    const six = f.hourly.filter((h) => h.span_h === 6);
    expect(hourly.length).toBeGreaterThanOrEqual(58);
    expect(hourly.length).toBeLessThanOrEqual(66);
    expect(six.length).toBeGreaterThan(20);
    expect(f.hourly.every((h) => h.span_h === 1 || h.span_h === 6)).toBe(true);
    // Hourly first, then only six-hour steps, on 00, 06, 12 and 18 UTC.
    expect(f.hourly.slice(0, hourly.length).every((h) => h.span_h === 1)).toBe(true);
    expect(six.every((h) => new Date(h.time).getUTCHours() % 6 === 0)).toBe(true);
    const last = f.hourly.at(-1)!;
    const reach = (Date.parse(last.time) + last.span_h * HOUR - NOW) / (24 * HOUR);
    expect(reach).toBeGreaterThan(8.5);
    expect(reach).toBeLessThan(9.5);
    // No chance of precipitation outside the Nordic region; amounts everywhere.
    expect(f.hourly.every((h) => h.pop === null && typeof h.precip_mm === 'number' && h.precip_mm >= 0)).toBe(true);
    expect(f.hourly.every((h) => h.condition !== null && h.text!.length > 0)).toBe(true);
    expect(f.daily.length).toBeGreaterThanOrEqual(9);
    expect(f.daily.length).toBeLessThanOrEqual(10);
    expect(f.daily.every((d) => d.text === null && d.pop === null && typeof d.precip_mm === 'number')).toBe(true);
  });

  it('dates MET Norway days by solar time, dropping those already over', () => {
    // 177.4 E is 11 h 50 min ahead of UTC by the sun: at 12:31 UTC it is
    // already the 7th there.
    const fiji = pointForecast(-17.75, 177.44, NOW + 30 * MIN);
    expect(fiji.status === 200 && fiji.body.daily[0].date).toBe('2026-11-07');
    // 150 W is ten hours behind: still the 6th.
    expect(ok(-17.55, -149.6).daily[0].date).toBe('2026-11-06');
  });

  it('gives a chance of precipitation inside the Nordic region only', () => {
    const oslo = ok(59.91, 10.75);
    expect(oslo.source).toBe('met-no');
    expect(oslo.hourly.every((h) => typeof h.pop === 'number' && h.pop >= 0 && h.pop <= 100)).toBe(true);
    expect(oslo.daily.every((d) => typeof d.pop === 'number')).toBe(true);
    expect(ok(48.85, 2.35).hourly.every((h) => h.pop === null)).toBe(true);
  });

  it('fails some points with 502 so the error path can be seen', () => {
    for (const [lat, lon] of [[89.9, 0], [-89.9, 120], [-48.88, -123.39]]) {
      const r = pointForecast(lat, lon, NOW);
      expect(r.status).toBe(502);
      expect(r.body).toMatchObject({ error: 'upstream' });
    }
  });

  it('is deterministic', () => {
    expect(pointForecast(40, -100, NOW)).toEqual(pointForecast(40, -100, NOW));
    expect(pointForecast(-33.9, 18.6, NOW)).toEqual(pointForecast(-33.9, 18.6, NOW));
  });
});

const KEYS: (keyof StationProps)[] = [
  'id', 'name', 'kind', 'source', 'observed', 'temp_c', 'dewpoint_c', 'water_temp_c', 'wind_dir_deg', 'wind_speed_ms',
  'wind_gust_ms', 'visibility_m', 'wave_height_m', 'altimeter_hpa', 'flight_category', 'raw',
];

describe('mock stations', () => {
  const all = stations(NOW, null);
  const props = all.features.map((f) => f.properties);

  it('covers the world with three feeds', () => {
    expect(all.type).toBe('FeatureCollection');
    expect(all.features.length).toBeGreaterThanOrEqual(700);
    expect(all.features.length).toBeLessThanOrEqual(1500);
    const count = (kind: string, source: string) => props.filter((p) => p.kind === kind && p.source === source).length;
    expect(count('airport', 'metar')).toBeGreaterThan(500);
    expect(count('buoy', 'ndbc')).toBeGreaterThan(60);
    expect(count('station', 'ndbc')).toBeGreaterThan(30);
    expect(count('station', 'amedas')).toBeGreaterThanOrEqual(100);
    expect(props.length).toBe(count('airport', 'metar') + count('buoy', 'ndbc') + count('station', 'ndbc') + count('station', 'amedas'));
    // Every continent has airports.
    for (const [w, s, e, n] of [[-10, 35, 40, 70], [60, 0, 150, 55], [-20, -35, 50, 35], [-80, -55, -35, 10], [110, -45, 155, -10], [-130, 25, -65, 50]]) {
      expect(stations(NOW, { west: w, south: s, east: e, north: n }).features.some((f) => f.properties.kind === 'airport')).toBe(true);
    }
  });

  it('shapes features as the protocol does', () => {
    const ids = new Set<string>();
    for (const f of all.features) {
      const p = f.properties;
      expect(f.type).toBe('Feature');
      expect(f.id).toBe(p.id);
      expect(ids.has(p.id)).toBe(false);
      ids.add(p.id);
      expect(f.geometry.type).toBe('Point');
      const [lon, lat] = f.geometry.coordinates as number[];
      expect(Math.abs(lon)).toBeLessThanOrEqual(180);
      expect(Math.abs(lat)).toBeLessThanOrEqual(90);
      expect(Math.round(lon * 1000) / 1000).toBe(lon);
      expect(Math.round(lat * 1000) / 1000).toBe(lat);
      expect(all.sources[p.source]).toBeDefined();
      if (p.source === 'metar') expect(p.id).toMatch(/^[A-Z]{4}$/);
      if (p.source === 'ndbc') expect(p.id).toMatch(/^ndbc:[0-9A-Z]{5}$/);
      if (p.source === 'amedas') expect(p.id).toMatch(/^amedas:\d{5}$/);
      if (p.kind === 'buoy') expect(p.name).toBe(`Buoy ${p.id.slice(5)}`);
      const observed = Date.parse(p.observed);
      expect(observed).toBeLessThanOrEqual(NOW);
      expect(NOW - observed).toBeLessThan(3 * HOUR);
    }
    expect(all.features.find((f) => f.id === 'KOUN')!.properties).toMatchObject({ name: 'Norman, OK', kind: 'airport', source: 'metar' });
    expect(all.features.find((f) => f.id === 'ndbc:41013')!.geometry.coordinates).toEqual([-77.764, 33.436]);
    expect(all.features.find((f) => f.id === 'amedas:11016')!.properties.name).toBe('Wakkanai');
  });

  it('leaves out what a station does not report, rather than sending null', () => {
    for (const p of props) {
      for (const [k, v] of Object.entries(p)) {
        expect(KEYS).toContain(k);
        expect(v === null || v === undefined || (typeof v === 'number' && !Number.isFinite(v))).toBe(false);
      }
      if (p.wind_dir_deg !== undefined) {
        expect(p.wind_dir_deg).toBeGreaterThanOrEqual(1);
        expect(p.wind_dir_deg).toBeLessThanOrEqual(360);
        expect(p.wind_speed_ms).toBeGreaterThan(0);
      }
      if (p.water_temp_c !== undefined || p.wave_height_m !== undefined) expect(p.kind).toBe('buoy');
      if (p.flight_category !== undefined) {
        expect(p.kind).toBe('airport');
        expect(['VFR', 'MVFR', 'IFR', 'LIFR']).toContain(p.flight_category);
      }
      expect(p.raw).toBeUndefined();
    }
    // Some stations miss something: temperature, wind, a buoy's waves, calm wind's direction.
    expect(props.some((p) => p.kind === 'airport' && p.temp_c === undefined)).toBe(true);
    expect(props.some((p) => p.kind === 'airport' && p.wind_speed_ms === undefined)).toBe(true);
    expect(props.some((p) => p.wind_speed_ms !== undefined && p.wind_dir_deg === undefined)).toBe(true);
    expect(props.some((p) => p.kind === 'buoy' && p.wave_height_m === undefined)).toBe(true);
    expect(props.some((p) => p.kind === 'buoy' && p.water_temp_c !== undefined && p.wave_height_m !== undefined)).toBe(true);
    expect(props.some((p) => p.source === 'amedas' && p.altimeter_hpa === undefined)).toBe(true);
  });

  it('adds raw METARs for airports only when asked', () => {
    const withRaw = stations(NOW, null, { raw: true }).features.map((f) => f.properties);
    for (const p of withRaw) {
      if (p.kind === 'airport') expect(p.raw).toMatch(new RegExp(`^${p.id} \\d{6}Z `));
      else expect(p.raw).toBeUndefined();
    }
    // Asking for raw text does not change what the default answer holds.
    expect(stations(NOW, null).features.every((f) => f.properties.raw === undefined)).toBe(true);
  });

  it('filters by a box, across the antimeridian too', () => {
    const ids = (w: number, s: number, e: number, n: number) => stations(NOW, { west: w, south: s, east: e, north: n }).features.map((f) => f.id);
    const bering = ids(170, 50, -170, 66);
    for (const id of ['PASY', 'PADK', 'UHMA', 'UHMD', 'ndbc:46070', 'ndbc:46071', 'ndbc:46035', 'ndbc:46072', 'ndbc:ADKA2']) expect(bering).toContain(id);
    for (const f of stations(NOW, { west: 170, south: 50, east: -170, north: 66 }).features) {
      const [lon, lat] = f.geometry.coordinates as number[];
      expect(lon >= 170 || lon <= -170).toBe(true);
      expect(lat >= 50 && lat <= 66).toBe(true);
    }
    // The same box as a map panned across the date line reports it.
    expect(ids(170, 50, 190, 66)).toEqual(bering);
    expect(ids(-190, 50, -170, 66)).toEqual(bering);
    // Fiji to Tonga, Samoa and Wallis; the Chatham Islands.
    const southPacific = ids(175, -25, -170, -10);
    for (const id of ['NFFN', 'NFNA', 'NFTF', 'NSFA', 'NLWW']) expect(southPacific).toContain(id);
    expect(ids(-177, -44.5, -176, -43.5)).toEqual(['NZCI']);
    // 360 degrees or more is the whole world; a plain box keeps to itself.
    expect(ids(-180, -90, 180, 90).length).toBe(all.features.length);
    expect(ids(-200, -90, 200, 90).length).toBe(all.features.length);
    const plains = stations(NOW, { west: -100, south: 33, east: -95, north: 37 }).features;
    expect(plains.map((f) => f.id)).toContain('KOUN');
    expect(plains.every((f) => (f.geometry.coordinates as number[])[0] >= -100 && (f.geometry.coordinates as number[])[0] <= -95)).toBe(true);
  });

  it('lists only the feeds with a station in the answer', () => {
    expect(all.sources).toEqual({
      metar: { name: 'Airport weather reports (METAR)', attribution: 'Source: Aviation Weather Center (NOAA/NWS), aviationweather.gov' },
      ndbc: { name: 'NOAA National Data Buoy Center', attribution: 'Source: NOAA National Data Buoy Center, ndbc.noaa.gov' },
      amedas: { name: 'Japan Meteorological Agency AMeDAS', attribution: 'Source: Japan Meteorological Agency website (jma.go.jp)' },
    });
    expect(Object.keys(stations(NOW, { west: 128, south: 30, east: 146, north: 46 }).sources)).toEqual(['metar', 'amedas']);
    expect(Object.keys(stations(NOW, { west: -60, south: 20, east: -57, north: 23 }).sources)).toEqual(['ndbc']);
    const empty = stations(NOW, { west: -30, south: -40, east: -20, north: -30 });
    expect(empty.features).toEqual([]);
    expect(empty.sources).toEqual({});
  });

  it('is deterministic and keeps the METAR time', () => {
    expect(stations(NOW, null, { raw: true })).toEqual(stations(NOW, null, { raw: true }));
    const t = metarTime(NOW);
    expect(t).toBeLessThanOrEqual(NOW - 5 * MIN);
    expect(new Date(t).getUTCMinutes()).toBe(53);
    expect(all.features.find((f) => f.id === 'KOUN')!.properties.observed).toBe(new Date(t).toISOString().replace(/\.\d{3}Z$/, 'Z'));
  });
});
