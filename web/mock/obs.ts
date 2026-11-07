// Station observations and point forecasts derived from the synthetic fields,
// shaped like the server serves them: stations worldwide from three feeds
// (airport METARs, NOAA buoys and coastal stations, Japan's AMeDAS), and the
// provider-neutral point forecast, NWS-style inside the US and MET
// Norway-style everywhere else.

import type { DailyPoint, Feature, HourlyPoint, LonLatBox, PointForecast, StationCollection, StationProps } from '../src/api/types.ts';
import { hoursSinceEpoch, lowCentres, prepareRadar, prepareSurface, ptypeFor, rainRate, type Quality, type RadarField, type SurfaceField } from './fields.ts';
import { distKm, hash1, nearConus, wrapLon } from './geo.ts';
import { AIRPORTS, nearestAirport } from './places.ts';
import { AMEDAS, BUOYS, COASTAL, WORLD_AIRPORTS } from './stationsWorld.ts';
import { DAY, HOUR, MIN, iso, pad, round } from './util.ts';

const round1 = (v: number) => round(v, 1);
/** Where an earth-relative wind blows from, degrees clockwise from north. */
const windFrom = (u: number, v: number) => ((Math.atan2(-u, -v) * 180) / Math.PI + 360) % 360;

// ---------------------------------------------------------------------------
// Prepared fields, shared between requests: a forecast for a second point
// in the same hour reuses the first one's.
// ---------------------------------------------------------------------------

const FIELD_CACHE_MAX = 600;
const surfaceCache = new Map<string, SurfaceField>();
const radarCache = new Map<string, RadarField>();

function remember<T>(cache: Map<string, T>, key: string, make: () => T): T {
  let v = cache.get(key);
  if (v === undefined) {
    v = make();
    if (cache.size >= FIELD_CACHE_MAX) cache.delete(cache.keys().next().value as string);
    cache.set(key, v);
  }
  return v;
}

// The fields depend on the time of day and on the hours since the epoch,
// which tests move, so both are in the key.
const fieldKey = (ms: number, q: Quality) => `${ms}|${hoursSinceEpoch(ms)}|${q}`;
const surfaceAt = (ms: number, q: Quality) => remember(surfaceCache, fieldKey(ms, q), () => prepareSurface(ms, q));
const radarAt = (ms: number, q: Quality) => remember(radarCache, fieldKey(ms, q), () => prepareRadar(ms, q, { skipGeoref: true }));

// ---------------------------------------------------------------------------
// The sun
// ---------------------------------------------------------------------------

const RAD = Math.PI / 180;

/** Solar elevation in degrees (the usual low-precision almanac formulas). */
function sunElevation(lat: number, lon: number, ms: number): number {
  const d = ms / DAY - 10957.5;
  const g = (357.529 + 0.98560028 * d) * RAD;
  const q = 280.459 + 0.98564736 * d;
  const L = (q + 1.915 * Math.sin(g) + 0.02 * Math.sin(2 * g)) * RAD;
  const e = (23.439 - 0.00000036 * d) * RAD;
  const ra = Math.atan2(Math.cos(e) * Math.sin(L), Math.cos(L)) / RAD;
  const dec = Math.asin(Math.sin(e) * Math.sin(L));
  const gmst = 280.46061837 + 360.98564736629 * d;
  const ha = (gmst + lon - ra) * RAD;
  const el = Math.asin(Math.sin(lat * RAD) * Math.sin(dec) + Math.cos(lat * RAD) * Math.cos(dec) * Math.cos(ha));
  return el / RAD;
}

// ---------------------------------------------------------------------------
// Stations
// ---------------------------------------------------------------------------

/** Routine METARs go out a few minutes before the hour. */
export function metarTime(now: number): number {
  const t = Math.floor(now / HOUR) * HOUR - 7 * MIN;
  return t <= now - 5 * MIN ? t : t - HOUR;
}

/** Buoys report at ten to the hour and take a while to come through. */
function ndbcTime(now: number): number {
  const t = Math.floor((now + 10 * MIN) / HOUR) * HOUR - 10 * MIN;
  return now - t >= 25 * MIN ? t : t - HOUR;
}

/** AMeDAS: every ten minutes, ten minutes late. */
const amedasTime = (now: number) => Math.floor((now - 10 * MIN) / (10 * MIN)) * 10 * MIN;

const SOURCES: Record<string, { name: string; attribution: string }> = {
  metar: { name: 'Airport weather reports (METAR)', attribution: 'Source: Aviation Weather Center (NOAA/NWS), aviationweather.gov' },
  ndbc: { name: 'NOAA National Data Buoy Center', attribution: 'Source: NOAA National Data Buoy Center, ndbc.noaa.gov' },
  amedas: { name: 'Japan Meteorological Agency AMeDAS', attribution: 'Source: Japan Meteorological Agency website (jma.go.jp)' },
};

const fieldsAt = (t: number) => ({ t, sfc: prepareSurface(t, 'obs'), radar: prepareRadar(t, 'obs'), lows: lowCentres(t) });
type Fields = ReturnType<typeof fieldsAt>;

/** Sea-level pressure: subtropical highs, the equatorial trough, and a dip
 *  under every low and the typhoon. */
function pressureAt(lon: number, lat: number, lows: Fields['lows']): number {
  let p = 1012 + 9 * Math.exp(-(((Math.abs(lat) - 32) / 11) ** 2)) - 3 * Math.exp(-((lat / 10) ** 2));
  for (const L of lows) {
    const r = L.kind === 'cyclone' ? 350 : 900;
    p -= L.strength * (L.kind === 'cyclone' ? 1.6 : 1.3) * Math.exp(-((distKm(lon, lat, L.lon, L.lat) / r) ** 2));
  }
  return p;
}

function sample(f: Fields, lon: number, lat: number) {
  const temp = f.sfc.temp(lon, lat);
  const dew = f.sfc.dewpoint(lon, lat);
  const w = f.sfc.wind(lon, lat);
  const dbz = f.radar.dbz(lon, lat);
  return {
    temp,
    dew,
    speed: Math.hypot(w.u, w.v),
    gust: w.gust,
    dir: windFrom(w.u, w.v),
    dbz,
    clouds: f.sfc.clouds(lon, lat, dbz),
    pressure: pressureAt(lon, lat, f.lows),
  };
}

function metarTemp(c: number): string {
  const r = Math.round(c);
  return r < 0 ? `M${pad(-r)}` : pad(r);
}

interface AirportSite {
  icao: string;
  name: string;
  lat: number;
  lon: number;
  /** Reports in statute miles and inches of mercury, temperatures in tenths. */
  us: boolean;
}

const AIRPORT_SITES: AirportSite[] = [
  ...AIRPORTS.map((a) => ({ icao: a.icao, name: `${a.city}, ${a.state}`, lat: a.lat, lon: a.lon, us: true })),
  ...WORLD_AIRPORTS.map(([icao, name, cc, lat, lon]) => ({ icao, name: `${name}, ${cc}`, lat, lon, us: false })),
];

function airport(a: AirportSite, i: number, f: Fields): Feature<StationProps> {
  const s = sample(f, a.lon, a.lat);
  // Salts chosen so the airports the docs and checks name report in full.
  const missingWind = hash1(i * 17 + 5) < 0.04;
  const missingTemp = hash1(i * 23 + 6) < 0.03;
  const missingDew = hash1(i * 29 + 11) < 0.03;
  const missingAlt = hash1(i * 31 + 5) < 0.02;
  const calm = s.speed < 1.3;
  const variable = !calm && s.speed < 3 && hash1(i * 41 + 13) < 0.3;
  const dir = Math.round(s.dir / 10) * 10 || 360;
  const gust = !calm && s.gust - s.speed > 4.5 ? s.gust : null;
  const fog = s.temp - s.dew < 1.2;
  const vis = a.us
    ? s.dbz > 45 ? 2414 : s.dbz > 30 ? 6437 : fog ? 1207 : 16093
    : s.dbz > 45 ? 2400 : s.dbz > 30 ? 6000 : fog ? 1200 : 10000;
  const ceiling = s.dbz > 35 ? 700 : s.dbz > 20 ? 1500 : s.clouds > 85 ? 2500 : s.clouds > 60 ? 4500 : null;
  const cat =
    (ceiling !== null && ceiling < 500) || vis < 1609
      ? 'LIFR'
      : (ceiling !== null && ceiling < 1000) || vis < 4828
        ? 'IFR'
        : (ceiling !== null && ceiling <= 3000) || vis <= 8047
          ? 'MVFR'
          : 'VFR';
  const kt = (ms: number) => Math.round(ms * 1.9438444924406);
  const inHg = Math.round(s.pressure * 0.02953 * 100);
  const d = new Date(f.t);
  const stamp = `${pad(d.getUTCDate())}${pad(d.getUTCHours())}${pad(d.getUTCMinutes())}Z`;
  const windStr = missingWind ? '' : calm ? '00000KT' : `${variable ? 'VRB' : pad(dir, 3)}${pad(kt(s.speed))}${gust ? `G${pad(kt(gust))}` : ''}KT`;
  const wx = s.dbz > 40 ? (s.temp < 0 ? '+SN' : '+TSRA') : s.dbz > 25 ? (s.temp < 0 ? 'SN' : 'RA') : s.dbz > 15 ? (s.temp < 0 ? '-SN' : '-RA') : fog ? 'BR' : '';
  const sky = ceiling === null ? (s.clouds > 30 ? 'SCT090' : a.us ? 'CLR' : 'NSC') : `${s.clouds > 85 ? 'OVC' : 'BKN'}${pad(Math.round(ceiling / 100), 3)}`;
  const visStr = a.us ? (vis >= 16093 ? '10SM' : vis >= 6437 ? '4SM' : vis >= 2414 ? '1 1/2SM' : '3/4SM') : vis >= 10000 ? '9999' : pad(vis, 4);
  const tempStr = missingTemp ? '' : `${metarTemp(s.temp)}/${missingDew ? '' : metarTemp(s.dew)}`;
  const altStr = missingAlt ? '' : a.us ? `A${pad(inHg, 4)}` : `Q${pad(Math.round(s.pressure), 4)}`;
  const raw = [a.icao, stamp, a.us ? 'AUTO' : '', windStr, visStr, wx, sky, tempStr, altStr, a.us ? 'RMK AO2' : 'NOSIG'].filter(Boolean).join(' ');

  const p: StationProps = { id: a.icao, name: a.name, kind: 'airport', source: 'metar', observed: iso(f.t) };
  // International METARs give whole degrees; US ones carry tenths in the remarks.
  const temp = (c: number) => (a.us ? round1(c) : Math.round(c));
  if (!missingTemp) {
    p.temp_c = temp(s.temp);
    if (!missingDew) p.dewpoint_c = temp(s.dew);
  }
  if (!missingWind) {
    if (!calm && !variable) p.wind_dir_deg = dir;
    p.wind_speed_ms = calm ? 0 : round1(kt(s.speed) * 0.514444);
    if (gust !== null) p.wind_gust_ms = round1(kt(gust) * 0.514444);
  }
  p.visibility_m = vis;
  if (!missingAlt) p.altimeter_hpa = a.us ? round1((inHg / 100) * 33.8639) : Math.round(s.pressure);
  p.flight_category = cat;
  p.raw = raw;
  return point(a.icao, a.lon, a.lat, p);
}

function waterTemp(lon: number, lat: number, air: number, lake: boolean): number {
  let sst = lake ? 9 : 28.5 - 0.0072 * lat * lat;
  // The California Current keeps the West Coast cold.
  if (!lake && lon > -135 && lon < -115 && lat > 30 && lat < 50) sst -= 5;
  return Math.max(-1.8, 0.8 * sst + 0.2 * air);
}

function buoy([code, lat, lon, lake]: (typeof BUOYS)[number], i: number, f: Fields): Feature<StationProps> {
  const s = sample(f, lon, lat);
  const id = `ndbc:${code}`;
  const p: StationProps = { id, name: `Buoy ${code}`, kind: 'buoy', source: 'ndbc', observed: iso(f.t) };
  if (hash1(i * 13 + 101) >= 0.05) p.temp_c = round1(s.temp);
  if (p.temp_c !== undefined && hash1(i * 19 + 103) < 0.5) p.dewpoint_c = round1(s.dew);
  if (p.temp_c !== undefined && p.dewpoint_c !== undefined && p.dewpoint_c > p.temp_c) p.dewpoint_c = p.temp_c;
  if (hash1(i * 23 + 107) >= 0.03) {
    if (s.speed >= 0.3) p.wind_dir_deg = Math.round(s.dir) || 360;
    p.wind_speed_ms = round1(s.speed);
    p.wind_gust_ms = round1(Math.max(s.speed, s.gust));
  }
  if (hash1(i * 29 + 109) >= 0.04) p.altimeter_hpa = round1(s.pressure);
  if (hash1(i * 31 + 113) >= 0.05) p.water_temp_c = round1(waterTemp(lon, lat, s.temp, !!lake));
  if (hash1(i * 37 + 127) >= 0.1) {
    const swell = lake ? 0.2 : 0.6 + 1.2 * hash1(i * 53 + 2);
    p.wave_height_m = round1(swell + 0.021 * s.speed * s.speed * (lake ? 0.6 : 1));
  }
  return point(id, lon, lat, p);
}

function coastal([code, name, lat, lon]: (typeof COASTAL)[number], i: number, f: Fields): Feature<StationProps> {
  const s = sample(f, lon, lat);
  const id = `ndbc:${code}`;
  const p: StationProps = { id, name: name || `Station ${code}`, kind: 'station', source: 'ndbc', observed: iso(f.t) };
  if (hash1(i * 11 + 201) >= 0.05) p.temp_c = round1(s.temp);
  if (p.temp_c !== undefined && hash1(i * 17 + 203) < 0.4) p.dewpoint_c = round1(Math.min(s.dew, s.temp));
  if (hash1(i * 23 + 207) >= 0.03) {
    if (s.speed >= 0.3) p.wind_dir_deg = Math.round(s.dir) || 360;
    p.wind_speed_ms = round1(s.speed);
    if (s.gust - s.speed > 2) p.wind_gust_ms = round1(s.gust);
  }
  if (hash1(i * 29 + 209) < 0.7) p.altimeter_hpa = round1(s.pressure);
  return point(id, lon, lat, p);
}

function amedas([code, name, lat, lon, manned]: (typeof AMEDAS)[number], i: number, f: Fields): Feature<StationProps> {
  const s = sample(f, lon, lat);
  const id = `amedas:${code}`;
  const p: StationProps = { id, name, kind: 'station', source: 'amedas', observed: iso(f.t) };
  if (hash1(i * 7 + 301) >= 0.02) p.temp_c = round1(s.temp);
  if (manned && p.temp_c !== undefined && hash1(i * 11 + 303) >= 0.1) p.dewpoint_c = round1(Math.min(s.dew, s.temp));
  if (hash1(i * 13 + 307) >= 0.02) {
    // Sixteen compass points, as JMA reports them; calm has none.
    if (s.speed >= 0.3) p.wind_dir_deg = Math.round((Math.round(s.dir / 22.5) % 16) * 22.5) || 360;
    p.wind_speed_ms = round1(s.speed);
  }
  if (manned && hash1(i * 17 + 311) < 0.3) {
    const fog = s.temp - s.dew < 1;
    p.visibility_m = s.dbz > 40 ? 280 + Math.round(hash1(i) * 500) : s.dbz > 25 ? 3000 : fog ? 800 : 20000;
  }
  if (manned) p.altimeter_hpa = round1(s.pressure);
  return point(id, lon, lat, p);
}

function point(id: string, lon: number, lat: number, properties: StationProps): Feature<StationProps> {
  return { type: 'Feature', id, geometry: { type: 'Point', coordinates: [round(lon, 3), round(lat, 3)] }, properties };
}

let stationCache: { key: string; features: Feature<StationProps>[] } | null = null;

/** Every station, raw METARs included; rebuilt when any feed has a newer report. */
function allStations(now: number): Feature<StationProps>[] {
  const tm = metarTime(now);
  const tn = ndbcTime(now);
  const ta = amedasTime(now);
  const key = `${tm}|${tn}|${ta}|${hoursSinceEpoch(tm)}`;
  if (stationCache?.key === key) return stationCache.features;
  const fm = fieldsAt(tm);
  // A few airports are an hour late, as some always are.
  const late = fieldsAt(tm - HOUR);
  const fn = fieldsAt(tn);
  const fa = fieldsAt(ta);
  const features = [
    ...AIRPORT_SITES.map((a, i) => airport(a, i, hash1(i * 43 + 17) < 0.04 ? late : fm)),
    ...BUOYS.map((b, i) => buoy(b, i, fn)),
    ...COASTAL.map((c, i) => coastal(c, i, fn)),
    ...AMEDAS.map((a, i) => amedas(a, i, fa)),
  ];
  stationCache = { key, features };
  return features;
}

/** Whether a point is in a box that may cross the antimeridian or have
 *  longitudes beyond +-180. */
function boxTest(b: LonLatBox): (lon: number, lat: number) => boolean {
  const width = b.east >= b.west ? b.east - b.west : b.east - b.west + 360;
  const latOk = (lat: number) => lat >= b.south && lat <= b.north;
  if (width >= 360) return (_lon, lat) => latOk(lat);
  const w = wrapLon(b.west);
  const e = wrapLon(b.east);
  // East exactly on 180 wraps to -180: the box then reaches the antimeridian.
  if (w <= e) return (lon, lat) => latOk(lat) && lon >= w && lon <= e;
  return (lon, lat) => latOk(lat) && (lon >= w || lon <= e);
}

export function stations(now: number, bbox: LonLatBox | null, opts: { raw?: boolean } = {}): StationCollection {
  const inside = bbox ? boxTest(bbox) : null;
  const features: Feature<StationProps>[] = [];
  for (const f of allStations(now)) {
    const [lon, lat] = f.geometry.coordinates as [number, number];
    if (inside && !inside(lon, lat)) continue;
    if (opts.raw || f.properties.raw === undefined) {
      features.push(f);
    } else {
      const properties = { ...f.properties };
      delete properties.raw;
      features.push({ ...f, properties });
    }
  }
  const sources: StationCollection['sources'] = {};
  for (const key of Object.keys(SOURCES)) if (features.some((f) => f.properties.source === key)) sources[key] = SOURCES[key];
  return { type: 'FeatureCollection', sources, features };
}

// ---------------------------------------------------------------------------
// Point forecast: shared pieces
// ---------------------------------------------------------------------------

/** Chance and intensity of precipitation around a point: the fraction of
 *  nearby samples with echo, so a storm passing just to the side still
 *  raises the chance. */
function wetness(radar: RadarField, lon: number, lat: number): { centre: number; max: number; wet: number } {
  let wet = 0;
  let max = -30;
  let centre = -30;
  const offsets = [[0, 0], [0.25, 0], [-0.25, 0], [0, 0.2], [0, -0.2], [0.5, 0.4], [-0.5, -0.4]];
  for (const [dx, dy] of offsets) {
    const v = radar.dbz(lon + dx, lat + dy);
    if (dx === 0 && dy === 0) centre = v;
    max = Math.max(max, v);
    if (v > 18) wet++;
  }
  return { centre, max, wet: wet / offsets.length };
}

const SKY_TEXT: Record<string, string> = { clearsky: 'Clear sky', fair: 'Fair', partlycloudy: 'Partly cloudy', cloudy: 'Cloudy', fog: 'Fog' };
const PRECIP_CODE = /^(light|heavy)?(rain|sleet|snow)(showers)?(andthunder)?$/;

/** MET Norway's codes for clear, fair and partly cloudy skies and for showers
 *  have a day, night or polar twilight form; the others have none. */
const hasVariant = (base: string) => base === 'clearsky' || base === 'fair' || base === 'partlycloudy' || base.includes('showers');
const baseCode = (code: string | null) => (code === null ? null : code.replace(/_(day|night|polartwilight)$/, ''));

function withVariant(base: string, lat: number, elevation: number): string {
  if (!hasVariant(base)) return base;
  if (elevation > 0) return `${base}_day`;
  return elevation > -6 && Math.abs(lat) >= 60 ? `${base}_polartwilight` : `${base}_night`;
}

/** The condition in English, as MET Norway's legend words it. */
function conditionText(code: string): string {
  const base = baseCode(code) ?? '';
  if (SKY_TEXT[base]) return SKY_TEXT[base];
  const m = PRECIP_CODE.exec(base);
  if (!m) return '';
  const words = [m[1], m[2], m[3] ? 'showers' : '', m[4] ? 'and thunder' : ''].filter(Boolean).join(' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

const US_BOXES: [number, number, number, number][] = [
  // west, south, east, north
  [-180, 51, -141, 62], // Alaska south of 62 N and the Aleutians east of 180
  [-172, 62, -141, 64],
  [-169, 64, -141, 71.6],
  [-141, 54.5, -129.9, 60.5], // the panhandle
  [172, 51, 180, 53.5], // the western Aleutians
  [-160.6, 18.5, -154.5, 22.6], // Hawaii
  [-67.5, 17.6, -64.5, 18.6], // Puerto Rico and the US Virgin Islands
  [144.5, 13, 146, 20.6], // Guam and the Northern Marianas
  [-171.2, -14.6, -169.3, -14], // American Samoa
];

function inUs(lon: number, lat: number): boolean {
  return nearConus(lon, lat, 0.25) || US_BOXES.some(([w, s, e, n]) => lon >= w && lon <= e && lat >= s && lat <= n);
}

export function pointForecast(lat: number, lon: number, now: number): { status: 200; body: PointForecast } | { status: 502; body: { error: 'upstream'; message: string } } {
  // Somewhere for the error path to be seen: the poles (off the map) and
  // Point Nemo, as far from land as the ocean gets.
  if (Math.abs(lat) > 89.5 || distKm(lon, lat, -123.39, -48.88) < 150) {
    return { status: 502, body: { error: 'upstream', message: 'No forecast provider answered for this point.' } };
  }
  return { status: 200, body: inUs(lon, lat) ? nwsForecast(lat, lon, now) : metNoForecast(lat, lon, now) };
}

// ---------------------------------------------------------------------------
// NWS style
// ---------------------------------------------------------------------------

function sky(clouds: number, day: boolean): string {
  if (clouds < 15) return day ? 'Sunny' : 'Clear';
  if (clouds < 40) return day ? 'Mostly Sunny' : 'Mostly Clear';
  if (clouds < 65) return 'Partly Cloudy';
  if (clouds < 88) return 'Mostly Cloudy';
  return 'Cloudy';
}

function precipWords(type: number, pop: number, heavy: boolean): string {
  const noun = type === 2 ? 'Snow' : type === 3 ? 'Sleet' : type === 4 ? 'Freezing Rain' : type === 5 ? 'Rain And Snow' : heavy ? 'Showers And Thunderstorms' : 'Rain Showers';
  if (pop >= 80) return noun === 'Rain Showers' ? 'Rain' : noun;
  if (pop >= 55) return `${noun} Likely`;
  if (pop >= 30) return `Chance ${noun}`;
  return `Slight Chance ${noun}`;
}

/** NWS wording to the nearest MET Norway code; freezing rain has none. */
const NWS_CODES: Record<string, string | null> = {
  Sunny: 'clearsky', Clear: 'clearsky', 'Mostly Sunny': 'fair', 'Mostly Clear': 'fair', 'Partly Cloudy': 'partlycloudy', 'Mostly Cloudy': 'cloudy',
  Cloudy: 'cloudy', 'Patchy Fog': 'fog', Rain: 'rain', 'Rain Showers': 'rainshowers', 'Showers And Thunderstorms': 'rainshowersandthunder',
  Snow: 'snow', Sleet: 'sleet', 'Rain And Snow': 'sleet', 'Freezing Rain': null,
};

function nwsCode(text: string, day: boolean): string | null {
  const base = NWS_CODES[text.replace(/^(Slight Chance|Chance) /, '').replace(/ Likely$/, '')] ?? null;
  return base === null ? null : hasVariant(base) ? `${base}_${day ? 'day' : 'night'}` : base;
}

const COMPASS8 = ['North', 'Northeast', 'East', 'Southeast', 'South', 'Southwest', 'West', 'Northwest'];

function localClock(tz: string): (ms: number) => { date: string; hour: number } {
  const f = new Intl.DateTimeFormat('en-US', { timeZone: tz, year: 'numeric', month: '2-digit', day: '2-digit', hour: 'numeric', hourCycle: 'h23' });
  return (ms) => {
    const p: Record<string, string> = {};
    for (const x of f.formatToParts(ms)) p[x.type] = x.value;
    return { date: `${p.year}-${p.month}-${p.day}`, hour: Number(p.hour) % 24 };
  };
}

function shiftDate(date: string, days: number): string {
  return new Date(Date.parse(`${date}T00:00:00Z`) + days * DAY).toISOString().slice(0, 10);
}

const WEEKDAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
const weekday = (date: string) => WEEKDAYS[new Date(`${date}T12:00:00Z`).getUTCDay()];
const toC = (f: number) => round1(((f - 32) * 5) / 9);

function nwsForecast(lat: number, lon: number, now: number): PointForecast {
  const { a, km } = nearestAirport(lon, lat);
  const local = localClock(a.tz);
  const start = Math.floor(now / HOUR) * HOUR;
  const hours = Array.from({ length: 156 }, (_, k) => {
    const t = start + k * HOUR;
    const q: Quality = k < 18 ? 'hrrr' : 'gfs';
    const sfc = surfaceAt(t, q);
    const temp = sfc.temp(lon, lat);
    const dew = sfc.dewpoint(lon, lat);
    const w = sfc.wind(lon, lat);
    const wx = wetness(radarAt(t, q), lon, lat);
    const pop = Math.min(100, Math.round((wx.wet * 100 + (wx.max > 12 ? 15 : 0)) / 10) * 10);
    const day = sunElevation(lat, lon, t + 30 * MIN) > 0;
    const speed = Math.hypot(w.u, w.v);
    const type = ptypeFor(temp, Math.max(0.1, rainRate(wx.max)));
    const fog = temp - dew < 1 && speed < 3;
    const text = pop >= 30 ? precipWords(type, pop, wx.max > 42) : fog ? 'Patchy Fog' : sky(sfc.clouds(lon, lat, wx.max), day);
    return {
      t,
      ...local(t),
      tempF: Math.round((temp * 9) / 5 + 32),
      mph: Math.round(speed * 2.2369362920544),
      dir16: Math.round(windFrom(w.u, w.v) / 22.5) % 16,
      pop: pop >= 10 ? pop : pop > 0 ? 5 : 0,
      text,
      condition: nwsCode(text, day),
    };
  });

  const hourly: HourlyPoint[] = hours.map((h) => ({
    time: iso(h.t),
    span_h: 1,
    temp_c: toC(h.tempF),
    pop: h.pop,
    precip_mm: null,
    wind_ms: round1(h.mph * 0.44704),
    wind_dir_deg: h.mph === 0 ? null : Math.round(h.dir16 * 22.5) % 360,
    condition: h.condition,
    text: h.text,
  }));

  // Half-day periods, 6 am to 6 pm and 6 pm to 6 am local, as the NWS words
  // its forecast; each belongs to the local date it starts on.
  const periods: { key: string; date: string; day: boolean; hours: typeof hours; name: string }[] = [];
  for (const h of hours) {
    const day = h.hour >= 6 && h.hour < 18;
    const key = day ? `${h.date}D` : `${h.hour >= 18 ? h.date : shiftDate(h.date, -1)}N`;
    const last = periods.at(-1);
    if (last?.key === key) last.hours.push(h);
    else periods.push({ key, date: h.date, day, hours: [h], name: '' });
  }
  if (periods.length > 1 && periods.at(-1)!.hours.length < 6) periods.pop();
  periods.forEach((p, i) => {
    const h0 = p.hours[0].hour;
    if (i === 0) p.name = p.day ? (h0 < 12 ? 'Today' : 'This Afternoon') : h0 < 6 ? 'Overnight' : 'Tonight';
    else if (i === 1 && periods[0].day) p.name = 'Tonight';
    else p.name = `${weekday(p.date)}${p.day ? '' : ' Night'}`;
  });

  const daily: DailyPoint[] = [];
  for (const p of periods) {
    const temps = p.hours.map((h) => h.tempF);
    const tf = p.day ? Math.max(...temps) : Math.min(...temps);
    const pop = Math.max(...p.hours.map((h) => h.pop));
    const lead = pop >= 30 ? p.hours.reduce((m, h) => (h.pop > m.pop ? h : m)) : p.hours[Math.floor(p.hours.length / 2)];
    const winds = p.hours.map((h) => Math.round(h.mph / 5) * 5);
    const wmin = Math.min(...winds);
    const wmax = Math.max(...winds);
    const dirName = COMPASS8[Math.round(lead.dir16 / 2) % 8];
    const wind = wmax === 0 ? 'Calm wind.' : `${dirName} wind ${wmin === wmax || wmin === 0 ? `around ${Math.max(5, wmax)}` : `${wmin} to ${wmax}`} mph.`;
    const short = lead.text.charAt(0) + lead.text.slice(1).toLowerCase();
    const detail = `${short}, with a ${p.day ? 'high near' : 'low around'} ${tf}. ${wind}${pop >= 20 ? ` Chance of precipitation is ${pop}%.` : ''}`;
    let d = daily.at(-1);
    if (d?.date !== p.date) {
      d = { date: p.date, low_c: null, high_c: null, pop: null, precip_mm: null, condition: null, text: null };
      daily.push(d);
    }
    if (p.day) d.high_c = d.high_c === null ? toC(tf) : Math.max(d.high_c, toC(tf));
    else d.low_c = d.low_c === null ? toC(tf) : Math.min(d.low_c, toC(tf));
    d.pop = Math.max(d.pop ?? 0, pop);
    // The daytime period's condition stands for the day.
    if (p.day || d.condition === null) d.condition = baseCode(lead.condition);
    d.text = `${d.text ? `${d.text} ` : ''}${p.name}: ${detail}`;
  }

  return {
    lat: round(lat, 4),
    lon: round(lon, 4),
    source: 'nws',
    attribution: 'Forecast from the US National Weather Service',
    place: km < 40 ? `${a.city}, ${a.state}` : `${Math.round(km * 0.621)} mi from ${a.city}, ${a.state}`,
    time_zone: a.tz,
    // Issued at a quarter past every hour.
    generated_at: iso(Math.floor((now - 735_000) / HOUR) * HOUR + 735_000),
    hourly,
    daily,
  };
}

// ---------------------------------------------------------------------------
// MET Norway style
// ---------------------------------------------------------------------------

function metCondition(rate: number, maxDbz: number, wet: number, temp: number, dew: number, speed: number, clouds: number, lat: number, elevation: number): string {
  let base: string;
  if (rate >= 0.1) {
    const type = ptypeFor(temp, rate);
    const kind = type === 2 ? 'snow' : type === 1 ? 'rain' : 'sleet';
    const intensity = rate < 1 ? 'light' : rate >= 4 ? 'heavy' : '';
    // Scattered echo around the point reads as showers, a solid shield as
    // steady rain.
    base = `${intensity}${kind}${wet < 0.7 ? 'showers' : ''}${maxDbz > 45 && temp > 2 ? 'andthunder' : ''}`;
  } else if (temp - dew < 0.8 && speed < 3) base = 'fog';
  else base = clouds < 13 ? 'clearsky' : clouds < 38 ? 'fair' : clouds < 75 ? 'partlycloudy' : 'cloudy';
  return withVariant(base, lat, elevation);
}

function metNoForecast(lat: number, lon: number, now: number): PointForecast {
  // MET Norway's pop exists only for its Nordic model domain.
  const nordic = lon >= -4 && lon <= 32 && lat >= 54 && lat <= 72;
  const h0 = Math.floor(now / HOUR) * HOUR;
  // Hourly for about 2.5 days, then six-hour steps on 00, 06, 12 and 18 UTC.
  const switchT = Math.ceil((h0 + 60 * HOUR) / (6 * HOUR)) * 6 * HOUR;
  const endT = h0 + 9 * DAY;
  const quality = (t: number): Quality => (nordic && t - h0 < 18 * HOUR ? 'hrrr' : 'gfs');

  const hourly: HourlyPoint[] = [];
  for (let t = h0; t < endT; ) {
    const span = t < switchT ? 1 : 6;
    const q = quality(t);
    const sfc = surfaceAt(t, q);
    const temp = sfc.temp(lon, lat);
    const dew = sfc.dewpoint(lon, lat);
    const w = sfc.wind(lon, lat);
    const speed = Math.hypot(w.u, w.v);
    // Precipitation, chance and symbol describe the whole step.
    const mids = span === 1 ? [t + 30 * MIN] : [t + HOUR, t + 3 * HOUR, t + 5 * HOUR];
    let rate = 0;
    let wet = 0;
    let wetMax = 0;
    let maxDbz = -30;
    for (const m of mids) {
      const wx = wetness(radarAt(m, q), lon, lat);
      rate += rainRate(wx.centre) / mids.length;
      wet += wx.wet / mids.length;
      wetMax = Math.max(wetMax, wx.wet);
      maxDbz = Math.max(maxDbz, wx.max);
    }
    const elevation = sunElevation(lat, lon, t + (span * HOUR) / 2);
    const condition = metCondition(rate, maxDbz, wet, temp, dew, speed, sfc.clouds(lon, lat, maxDbz), lat, elevation);
    hourly.push({
      time: iso(t),
      span_h: span,
      temp_c: round1(temp),
      pop: nordic ? Math.min(100, Math.round(wetMax * 100 + (maxDbz > 12 ? 10 : 0))) : null,
      precip_mm: round1(rate * span),
      wind_ms: round1(speed),
      wind_dir_deg: Math.round(windFrom(w.u, w.v)) % 360,
      condition,
      text: conditionText(condition),
    });
    t += span * HOUR;
  }

  // Days by local solar time: MET Norway gives no time zone.
  const offset = (lon / 15) * HOUR;
  const solarDate = (ms: number) => new Date(ms + offset).toISOString().slice(0, 10);
  const days = new Map<string, { temps: number[]; steps: number; precip: number; pops: number[]; noon: string | null; first: string | null }>();
  const day = (date: string) => {
    let d = days.get(date);
    if (!d) days.set(date, (d = { temps: [], steps: 0, precip: 0, pops: [], noon: null, first: null }));
    return d;
  };
  const last = hourly.at(-1)!;
  const stepsEnd = Date.parse(last.time) + last.span_h * HOUR;
  // Highs and lows from every hour, the six-hour stretch included, as MET
  // Norway's own six-hour maxima and minima are.
  for (let t = h0; t < stepsEnd; t += HOUR) day(solarDate(t)).temps.push(surfaceAt(t, quality(t)).temp(lon, lat));
  for (const h of hourly) {
    const t = Date.parse(h.time);
    const end = t + h.span_h * HOUR;
    const date = solarDate((t + end) / 2);
    const d = day(date);
    d.steps++;
    d.precip += h.precip_mm ?? 0;
    if (h.pop !== null) d.pops.push(h.pop);
    const noon = Date.parse(`${date}T12:00:00Z`) - offset;
    if (t <= noon && noon < end) d.noon = baseCode(h.condition);
    d.first ??= baseCode(h.condition);
  }
  const daily: DailyPoint[] = [];
  const dates = [...days.keys()];
  const today = solarDate(now);
  dates.forEach((date, i) => {
    const d = days.get(date)!;
    // Days already over go, as do the last minutes of today when no step
    // falls in them, and a last day the forecast barely reaches.
    if (date < today || d.steps === 0 || (i === dates.length - 1 && d.temps.length < 12 && i > 0)) return;
    const enough = d.temps.length >= 6;
    daily.push({
      date,
      low_c: enough ? round1(Math.min(...d.temps)) : null,
      high_c: enough ? round1(Math.max(...d.temps)) : null,
      pop: d.pops.length ? Math.max(...d.pops) : null,
      precip_mm: round1(d.precip),
      condition: d.noon ?? d.first,
      text: null,
    });
  });

  return {
    lat: round(lat, 4),
    lon: round(lon, 4),
    source: 'met-no',
    attribution: 'Based on data from MET Norway',
    place: null,
    time_zone: null,
    generated_at: iso(Math.floor((now - 50 * MIN) / HOUR) * HOUR + 18 * MIN + 29_000),
    hourly,
    daily: daily.slice(0, 10),
  };
}
