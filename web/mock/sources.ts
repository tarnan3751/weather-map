// The mock's source registry, shaped like `wx_core::source::SOURCES`, plus
// what the mock needs to fake each source: the box its grid covers (and the
// box where it has data at full weight), where inside that it actually has
// data, how good its synthetic weather is, and when its data arrives.
//
// A believable world: a global model, and a satellite precipitation estimate
// under the observations; regional models over North America, Alaska (whose grid
// crosses the antimeridian) and Europe; ragged ground-radar composites over
// the US and Europe; an analysis over the US.
// Erasable syntax only: worker threads load this directly.

import type { LonLatBox, SourceDef } from '../src/api/types.ts';
import { fold, WORLD } from './boxes.ts';
import type { Quality } from './fields.ts';
import { conusDistance, distToRingDeg, offsetKm, pointInPolygon, smoothstep } from './geo.ts';

export interface MockSource extends SourceDef {
  bounds: LonLatBox;
  solid: LonLatBox | null;
  quality: Quality;
  /** How much of this source shows at a point: 1 with data at full weight,
   *  falling to 0 across the edge of its coverage (the soft blend). Only
   *  called inside `bounds`. Longitudes in [-180, 180]. */
  weight(lon: number, lat: number): number;
}

const NOAA = { provider: 'NOAA', attribution_url: 'https://www.noaa.gov/', licence: 'Public domain (US government work)' };
const EUMETSAT = { provider: 'EUMETSAT', attribution_url: 'https://www.eumetsat.int/', licence: 'EUMETSAT data policy' };

/** Width of the soft edge where a regional grid fades into what is below. */
const RAMP_DEG = 1;

/** Weight inside a polygon (lon, lat ring), ramping up over `RAMP_DEG`
 *  from its edge. `shift` adds 360 to western longitudes for a ring drawn
 *  across the antimeridian. */
function polygonWeight(ring: [number, number][], shift: boolean) {
  return (lon: number, lat: number) => {
    const x = shift && lon < 0 ? lon + 360 : lon;
    if (!pointInPolygon(x, lat, ring)) return 0;
    return smoothstep(0, RAMP_DEG, distToRingDeg(x, lat, ring));
  };
}

/** Weight inside a lon/lat box (a regular grid fills its box), in plain
 *  degrees so a solid box `RAMP_DEG` inside the edges is at full weight. */
function boxWeight(b: LonLatBox) {
  return (lon: number, lat: number) => smoothstep(0, RAMP_DEG, Math.min(Math.min(lon - b.west, b.east - lon), Math.min(lat - b.south, b.north - lat)));
}

/** The HRRR (and RTMA) Lambert grid's outline: a trapezoid with a bowed top. */
const HRRR_RING: [number, number][] = [[-122.72, 21.14], [-72.28, 21.14], [-60.92, 47.84], [-80.0, 52.0], [-97.5, 52.63], [-115.0, 52.0], [-134.1, 47.84]];

/** HRRR Alaska's grid, with western longitudes counted past 180. */
const ALASKA_RING: [number, number][] = [[171.5, 50.0], [180.0, 42.0], [205.0, 41.6], [235.0, 46.0], [243.3, 58.0], [236.0, 73.0], [200.0, 76.2], [178.0, 72.5], [171.2, 62.0]];

/** European weather radars (approximate sites) in the OPERA composite. */
const OPERA_SITES: [number, number][] = [
  [-0.8, 51.7], [-0.5, 53.3], [-2.0, 57.4], [-2.6, 52.4], [-5.2, 50.0], [-6.3, 54.5], [-6.25, 53.4], [-8.9, 52.7],
  [-4.4, 58.2], [2.0, 48.8], [1.8, 50.1], [-4.3, 48.4], [-0.7, 44.8], [4.5, 43.8], [1.4, 43.6], [6.2, 48.7],
  [5.0, 45.7], [-1.6, 47.2], [3.0, 46.4], [7.0, 53.3], [10.0, 53.6], [12.1, 54.2], [13.9, 52.6], [6.97, 51.4],
  [9.7, 52.5], [13.8, 51.1], [8.7, 50.1], [11.1, 50.5], [10.6, 48.6], [12.1, 48.2], [8.0, 47.9], [4.8, 52.95],
  [5.1, 51.8], [3.1, 51.2], [5.5, 49.9], [10.0, 55.9], [12.5, 55.5], [5.5, 59.0], [10.8, 60.0], [11.0, 63.5],
  [14.0, 67.0], [18.9, 69.6], [14.0, 57.0], [18.0, 59.6], [17.0, 62.4], [21.0, 64.8], [20.6, 67.7], [24.9, 60.5],
  [27.0, 62.3], [25.4, 64.8], [21.0, 52.4], [17.0, 51.1], [19.0, 50.0], [14.2, 53.8], [22.7, 51.0], [18.5, 54.4],
  [14.4, 49.9], [17.2, 49.5], [16.4, 48.2], [13.0, 47.7], [11.5, 47.3], [8.6, 47.3], [6.0, 46.4], [9.0, 46.0],
  [9.3, 45.5], [11.3, 44.5], [12.5, 41.9], [16.0, 40.6], [14.2, 37.5], [8.9, 39.4], [-3.7, 40.4], [2.2, 41.4],
  [-0.4, 39.5], [-5.9, 37.4], [-8.4, 43.2], [-1.3, 42.8], [-4.0, 36.8], [-8.6, 41.2], [-7.9, 38.9], [-8.0, 37.1],
  [19.0, 47.4], [21.6, 47.5], [17.5, 46.7], [14.9, 46.0], [16.0, 45.8], [15.6, 43.5], [25.0, 59.4], [24.2, 56.9],
  [24.0, 55.0],
];
const OPERA_RANGE_KM = 220;

function operaWeight(lon: number, lat: number): number {
  let best = Infinity;
  for (const [x, y] of OPERA_SITES) {
    if (Math.abs(y - lat) > 2.2) continue;
    const [dx, dy] = offsetKm(lon, lat, x, y);
    best = Math.min(best, dx * dx + dy * dy);
  }
  return 1 - smoothstep(OPERA_RANGE_KM - 50, OPERA_RANGE_KM, Math.sqrt(best));
}

/** MRMS sees the US and a radar's range beyond its borders and coasts. */
const mrmsWeight = (lon: number, lat: number) => 1 - smoothstep(0.9, 1.6, conusDistance(lon, lat));

/** A geostationary imager over `subLon`: the box round its disc, the box
 *  inside it where it is always at full weight (boxes reaching past 180
 *  wrap), and full weight up to 55 degrees (central angle) from the point
 *  under it, none past 68, where the view is too slanted to use. */
function sat(d: Pick<MockSource, 'id' | 'family' | 'title' | 'attribution' | 'rank' | 'provider' | 'attribution_url' | 'licence'> & { subLon: number }): MockSource {
  const { subLon, ...def } = d;
  const k = Math.PI / 180;
  return {
    ...def,
    region: '',
    medium: 'satellite',
    kind: 'observed',
    res_km: 2,
    max_zoom: 5,
    quality: 'obs',
    bounds: { west: fold(subLon - 70), south: -70, east: fold(subLon + 70), north: 70 },
    solid: { west: fold(subLon - 35), south: -40, east: fold(subLon + 35), north: 40 },
    weight: (lon, lat) => {
      const c = Math.cos(lat * k) * Math.cos((lon - subLon) * k);
      return 1 - smoothstep(55, 68, Math.acos(Math.max(-1, Math.min(1, c))) / k);
    },
  };
}

/** A NOAA source unless it says otherwise. */
const def = (d: Omit<MockSource, 'provider' | 'attribution_url' | 'region' | 'licence'> & Partial<MockSource>): MockSource => ({ ...NOAA, region: '', ...d });

const HRRR_BOUNDS: LonLatBox = { west: -134.13, south: 21.12, east: -60.88, north: 52.63 };
const HRRR_SOLID: LonLatBox = { west: -122.0, south: 26.0, east: -72.0, north: 47.0 };
const ICON_BOUNDS: LonLatBox = { west: -23.5, south: 29.5, east: 62.5, north: 70.5 };
const CONUS_BOX: LonLatBox = { west: -130, south: 20, east: -60, north: 55 };

/** Highest rank first, as stacks are ordered. */
export const SOURCES: MockSource[] = [
  def({
    id: 'mrms', medium: 'radar', family: 'mrms', region: 'CONUS', title: 'MRMS', attribution: 'NOAA/NSSL MRMS', kind: 'observed',
    rank: 900, res_km: 1, max_zoom: 7, bounds: CONUS_BOX, solid: null, quality: 'obs', weight: mrmsWeight,
  }),
  def({
    id: 'opera', medium: 'radar', family: 'opera', title: 'OPERA', provider: 'EUMETNET', attribution: 'EUMETNET OPERA',
    licence: 'EUMETNET OPERA data policy', attribution_url: 'https://www.eumetnet.eu/', kind: 'observed',
    rank: 880, res_km: 2, max_zoom: 6, bounds: { west: -12.5, south: 34.5, east: 31.5, north: 72.0 }, solid: null, quality: 'obs', weight: operaWeight,
  }),
  // A radar extrapolation run every ten minutes: runs that start off the
  // hour, so its part ids carry the minute.
  def({
    id: 'nowcast', medium: 'nowcast', family: 'nowcast', region: 'conus', title: 'Nowcast', attribution: 'NOAA radar nowcast', kind: 'forecast',
    rank: 700, res_km: 1, max_zoom: 7, bounds: CONUS_BOX, solid: null, quality: 'obs', weight: mrmsWeight,
  }),
  def({
    id: 'rtma', medium: 'analysis', family: 'rtma', region: 'conus', title: 'RTMA', attribution: 'NOAA/NCEP RTMA', kind: 'observed',
    rank: 800, res_km: 2.5, max_zoom: 6, bounds: HRRR_BOUNDS, solid: HRRR_SOLID, quality: 'obs', weight: polygonWeight(HRRR_RING, false),
  }),
  def({
    id: 'hrrr', medium: 'model', family: 'hrrr', region: 'conus', title: 'HRRR', attribution: 'NOAA/NCEP HRRR', kind: 'forecast',
    rank: 600, res_km: 3, max_zoom: 6, bounds: HRRR_BOUNDS, solid: HRRR_SOLID, quality: 'hrrr', weight: polygonWeight(HRRR_RING, false),
  }),
  // One model, two domains, one credit: the map lists it once. West of
  // east: the grid runs from the western Aleutians across 180.
  def({
    id: 'hrrr_ak', medium: 'model', family: 'hrrr', region: 'alaska', title: 'HRRR Alaska', attribution: 'NOAA/NCEP HRRR', kind: 'forecast',
    rank: 590, res_km: 3, max_zoom: 6, bounds: { west: 171.0, south: 41.5, east: -116.5, north: 76.3 }, solid: { west: 179.5, south: 51.0, east: -135.0, north: 69.0 },
    quality: 'hrrr', weight: polygonWeight(ALASKA_RING, true),
  }),
  def({
    id: 'icon_eu', medium: 'model', family: 'icon', region: 'eu', title: 'ICON-EU', provider: 'DWD', attribution: 'DWD ICON-EU',
    licence: 'CC BY 4.0 (Deutscher Wetterdienst)', attribution_url: 'https://www.dwd.de/', kind: 'forecast',
    rank: 500, res_km: 7, max_zoom: 5, bounds: ICON_BOUNDS, solid: { west: -22.5, south: 30.5, east: 61.5, north: 69.5 }, quality: 'hrrr', weight: boxWeight(ICON_BOUNDS),
  }),
  def({
    id: 'gfs', medium: 'model', family: 'gfs', title: 'GFS', attribution: 'NOAA/NCEP GFS', kind: 'forecast',
    rank: 100, res_km: 27, max_zoom: 4, bounds: WORLD, solid: WORLD, quality: 'gfs', weight: () => 1,
  }),
  // Below every model: with observations only ever on ticks up to the newest
  // one, it is the base of the past frames and never shows ahead of now.
  def({
    id: 'cmorph2', medium: 'satellite', family: 'cmorph', title: 'CMORPH2', attribution: 'NOAA/CPC CMORPH2', attribution_url: 'https://www.cpc.ncep.noaa.gov/', kind: 'observed',
    rank: 50, res_km: 28, max_zoom: 3, bounds: WORLD, solid: WORLD, quality: 'sat', weight: () => 1,
  }),
  // Infrared imagers round the world, each on top nearest the point under it.
  sat({ id: 'goes19', family: 'goes', title: 'GOES-East', attribution: 'NOAA GOES-19', rank: 460, subLon: -75.2, ...NOAA }),
  sat({ id: 'goes18', family: 'goes', title: 'GOES-West', attribution: 'NOAA GOES-18', rank: 450, subLon: -137.0, ...NOAA }),
  sat({ id: 'mtg_i1', family: 'meteosat', title: 'Meteosat', attribution: 'EUMETSAT Meteosat', rank: 440, subLon: 0, ...EUMETSAT }),
  sat({ id: 'msg_iodc', family: 'meteosat', title: 'Meteosat IODC', attribution: 'EUMETSAT Meteosat (Indian Ocean)', rank: 430, subLon: 45.5, ...EUMETSAT }),
  sat({
    id: 'himawari9', family: 'himawari', title: 'Himawari', attribution: 'JMA Himawari-9', rank: 420, subLon: 140.7,
    provider: 'JMA', attribution_url: 'https://www.jma.go.jp/', licence: 'JMA website terms of use',
  }),
].sort((a, b) => b.rank - a.rank);

export const sourceById = (id: string) => SOURCES.find((s) => s.id === id);

const DEF_KEYS = ['id', 'family', 'region', 'title', 'provider', 'attribution', 'attribution_url', 'licence', 'kind', 'medium', 'rank', 'res_km', 'max_zoom'] as const;

/** `SourceDef` exactly as the server serializes it. */
export const sourceDefs = (): SourceDef[] => SOURCES.map((s) => Object.fromEntries(DEF_KEYS.map((k) => [k, s[k]])) as unknown as SourceDef);
