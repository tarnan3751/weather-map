// The layer registry as wx-server serializes crates/wx-core/src/layer.rs
// (`LAYERS`), so the client is developed against the real shape: same ids,
// order, bands and quantization. The inputs are the mock's worldwide set of
// families (see sources.ts), which is more than the Rust table has today.

import type { BandSpec, Derive, FieldSel, InputSpec, LayerSpec } from '../src/api/types.ts';
import { pad } from './util.ts';

const continuous = (name: string, quantity: BandSpec['quantity'], unit: string, scale: number, offset: number): BandSpec => ({
  name,
  quantity,
  unit,
  scale,
  offset,
  kind: 'continuous',
  aggregate: 'mean',
});

const DBZ: BandSpec = { ...continuous('dbz', 'reflectivity', 'dBZ', 0.1, -40.0), aggregate: 'max' };
const TEMP_C = continuous('t', 'temperature', 'degC', 0.01, -100.0);
const WIND_U = continuous('u', 'speed', 'm/s', 0.01, -150.0);
const WIND_V = continuous('v', 'speed', 'm/s', 0.01, -150.0);
const WIND_GUST = continuous('gust', 'speed', 'm/s', 0.01, 0.0);
const RATE_MMH = continuous('rate', 'precip_rate', 'mm/h', 0.01, 0.0);
const DEPTH_MM = continuous('depth', 'precip_depth', 'mm', 0.05, 0.0);
const SNOW_MM = continuous('depth', 'snow_depth', 'mm', 0.1, 0.0);
const CLOUD_PCT = continuous('cover', 'percent', '%', 0.01, 0.0);
/** Infrared brightness temperature: about -90 for the coldest storm tops to
 *  +50 for a desert afternoon. */
const IR_TB = continuous('tb', 'temperature', 'degC', 0.01, -150.0);
const PTYPE: BandSpec = { name: 'ptype', quantity: 'category', unit: '', scale: 1.0, offset: 0.0, kind: 'categorical', aggregate: 'nearest' };

const f = (v: string, level: string, stat: FieldSel['stat'] = 'instant'): FieldSel => ({ var: v, level, stat });
const K_TO_C: Derive = { op: 'linear', mul: 1.0, add: -273.15 };
const IDENTITY: Derive = { op: 'linear', mul: 1.0, add: 0.0 };
const SNOW_MM_FROM_M: Derive = { op: 'linear', mul: 1000.0, add: 0.0 };
const REFLECTIVITY: Derive = { op: 'reflectivity' };
const PRECIP_DEPTH: Derive = { op: 'precip_depth' };

/** What one family contributes to a layer, as `wx_core::layer::InputSpec`. */
const input = (family: string, product: string, fields: FieldSel[], derive: Derive, hold_minutes: number, fill_past = false): InputSpec => ({
  family,
  product,
  fields,
  derive,
  hold_minutes,
  fill_past,
});
const mrms = (product: string, derive: Derive, hold: number) => input('mrms', product, [], derive, hold);
const cmorph = (product: string, derive: Derive = PRECIP_DEPTH) => input('cmorph', product, [], derive, 120);
const imager = (family: string) => input(family, 'ir_10.4um', [], K_TO_C, 30);
const rtma = (fields: FieldSel[], derive: Derive) => input('rtma', '2dvaranl_ndfd', fields, derive, 60);
const hrrr = (fields: FieldSel[], derive: Derive, fillPast = false, product = 'wrfsfc', hold = 60) => input('hrrr', product, fields, derive, hold, fillPast);
const icon = (fields: FieldSel[], derive: Derive, fillPast = false) => input('icon', 'single-level', fields, derive, 60, fillPast);
const gfs = (fields: FieldSel[], derive: Derive, fillPast = false) => input('gfs', 'pgrb2.0p25', fields, derive, 180, fillPast);
/** The three models, each from the same fields. */
const models = (fields: FieldSel[], derive: Derive, fillPast = false) => [hrrr(fields, derive, fillPast), icon(fields, derive, fillPast), gfs(fields, derive, fillPast)];

const TMP_2M = f('TMP', '2 m above ground');
const DPT_2M = f('DPT', '2 m above ground');
const UGRD_10M = f('UGRD', '10 m above ground');
const VGRD_10M = f('VGRD', '10 m above ground');
const REFC = f('REFC', 'entire atmosphere');
const ASNOW = f('ASNOW', 'surface', 'run_total');
const FEELS_FIELDS = [TMP_2M, DPT_2M, UGRD_10M, VGRD_10M];
const PRECIP_FIELDS = [f('PRATE', 'surface'), f('CRAIN', 'surface'), f('CSNOW', 'surface'), f('CICEP', 'surface'), f('CFRZR', 'surface')];
const PRECIP_TYPE: Derive = { op: 'precip_type_rate' };

const RADAR_LOOP_MIN = 120;
const HOURLY_LOOP_MIN = 12 * 60;

/** A layer drawn from its first band, on hourly ticks, unless it says otherwise. */
function layer(l: Omit<LayerSpec, 'render' | 'observed_tick_minutes' | 'anchored'> & Partial<LayerSpec>): LayerSpec {
  const { id, title, group, description, bands, render = { kind: 'scalar', band: 0 }, palette, inputs, retention_minutes } = l;
  return { id, title, group, description, bands, render, palette, inputs, retention_minutes, observed_tick_minutes: l.observed_tick_minutes ?? 60, anchored: l.anchored ?? false };
}

/** Rainfall over the past `hours`, from MRMS and the satellite estimate. */
function rain(hours: number, hold: number, tick: number): LayerSpec {
  const span = hours === 1 ? 'hour' : `${hours} hours`;
  return layer({
    id: `rain_${hours}h`, title: `Rain, past ${span}`, group: 'precipitation', palette: 'rainfall', bands: [DEPTH_MM], retention_minutes: 180, observed_tick_minutes: tick,
    description: `Rainfall over the past ${span}, from radar and satellites`,
    inputs: [mrms(`RadarOnly_QPE_${pad(hours)}H_00.00`, PRECIP_DEPTH, hold), cmorph(`cmorph2_${hours}h`)],
  });
}

export const LAYERS: LayerSpec[] = [
  layer({
    id: 'radar', title: 'Radar', group: 'precipitation', palette: 'reflectivity', bands: [DBZ], retention_minutes: RADAR_LOOP_MIN, observed_tick_minutes: 2,
    description: 'Ground radar where there is some, satellite estimates elsewhere, continuing into simulated future radar',
    inputs: [
      mrms('MergedReflectivityQCComposite_00.50', REFLECTIVITY, 4),
      input('opera', 'composite_dbzh', [], REFLECTIVITY, 10),
      cmorph('cmorph2_30min', REFLECTIVITY),
      input('nowcast', 'reflectivity_extrapolation', [], REFLECTIVITY, 10),
      hrrr([REFC], REFLECTIVITY, false, 'wrfsubh', 15),
      icon([f('DBZ_CMAX', 'entire atmosphere')], REFLECTIVITY),
      gfs([REFC], REFLECTIVITY),
    ],
  }),
  layer({
    id: 'precip', title: 'Precipitation type', group: 'precipitation', palette: 'precip_type', bands: [RATE_MMH, PTYPE], retention_minutes: 0,
    render: { kind: 'precip_type', rate: 0, ptype: 1 },
    description: 'Forecast rain, snow, sleet and freezing rain, with intensity',
    inputs: [hrrr(PRECIP_FIELDS, PRECIP_TYPE), icon([f('TOT_PREC', 'surface'), f('WW', 'surface')], PRECIP_TYPE), gfs(PRECIP_FIELDS, PRECIP_TYPE)],
  }),
  rain(1, 10, 10),
  rain(24, 60, 60),
  rain(72, 60, 60),
  layer({
    id: 'snow', title: 'Snowfall', group: 'precipitation', palette: 'snowfall', bands: [SNOW_MM], retention_minutes: 0, anchored: true,
    description: 'Forecast snowfall accumulating from the start of the latest model runs',
    inputs: [hrrr([ASNOW], SNOW_MM_FROM_M), icon([f('TOT_SNOW', 'surface', 'run_total')], SNOW_MM_FROM_M), gfs([ASNOW], SNOW_MM_FROM_M)],
  }),
  layer({
    id: 'temp', title: 'Temperature', group: 'temperature', palette: 'temperature', bands: [TEMP_C], retention_minutes: HOURLY_LOOP_MIN,
    description: 'Air temperature two metres above the ground',
    inputs: [rtma([TMP_2M], K_TO_C), ...models([TMP_2M], K_TO_C, true)],
  }),
  layer({
    id: 'feels_like', title: 'Feels like', group: 'temperature', palette: 'temperature', bands: [TEMP_C], retention_minutes: HOURLY_LOOP_MIN,
    description: 'Wind chill when it is cold, heat index when it is hot',
    inputs: [rtma(FEELS_FIELDS, { op: 'feels_like' }), ...models(FEELS_FIELDS, { op: 'feels_like' }, true)],
  }),
  layer({
    id: 'dew_point', title: 'Dew point', group: 'temperature', palette: 'dew_point', bands: [TEMP_C], retention_minutes: HOURLY_LOOP_MIN,
    description: 'How humid the air is: the temperature at which dew forms',
    inputs: [rtma([DPT_2M], K_TO_C), ...models([DPT_2M], K_TO_C, true)],
  }),
  layer({
    id: 'wind', title: 'Wind and gusts', group: 'wind', palette: 'wind', bands: [WIND_U, WIND_V, WIND_GUST], retention_minutes: HOURLY_LOOP_MIN,
    render: { kind: 'wind', u: 0, v: 1, gust: 2 },
    description: 'Wind ten metres above the ground, and peak gusts',
    inputs: [
      rtma([UGRD_10M, VGRD_10M, f('GUST', '10 m above ground')], { op: 'wind' }),
      ...models([UGRD_10M, VGRD_10M, f('GUST', 'surface')], { op: 'wind' }, true),
    ],
  }),
  layer({
    id: 'clouds', title: 'Cloud cover', group: 'sky', palette: 'clouds', bands: [CLOUD_PCT], retention_minutes: 0,
    description: 'Forecast share of the sky covered by cloud',
    inputs: models([f('TCDC', 'entire atmosphere')], IDENTITY),
  }),
  layer({
    id: 'satellite', title: 'Satellite', group: 'sky', palette: 'infrared', bands: [IR_TB], retention_minutes: 180, observed_tick_minutes: 10,
    description: 'Infrared from geostationary satellites: the colder the cloud tops, the higher the cloud',
    inputs: [imager('goes'), imager('meteosat'), imager('himawari')],
  }),
];

export const layerById = (id: string) => LAYERS.find((l) => l.id === id);

/** `value = offset + (q - 1) * scale`, clamped like `BandSpec::quantize`. */
export function quantize(band: BandSpec, v: number): number {
  if (Number.isNaN(v)) return 0;
  const q = Math.round((v - band.offset) / band.scale) + 1;
  return Math.min(65535, Math.max(1, q));
}
