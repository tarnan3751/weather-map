import type { Quantity } from '../api/types.ts';

type TempUnit = 'F' | 'C';
export type SpeedUnit = 'mph' | 'kt' | 'ms' | 'kmh';
export type PrecipUnit = 'in' | 'mm';

export interface Units {
  temp: TempUnit;
  speed: SpeedUnit;
  precip: PrecipUnit;
}

export const DEFAULT_UNITS: Units = { temp: 'F', speed: 'mph', precip: 'in' };
export const METRIC_UNITS: Units = { temp: 'C', speed: 'kmh', precip: 'mm' };

/** Where weather is reported in Fahrenheit, miles per hour and inches: the
 *  United States and its territories, and the few countries that follow it. */
const IMPERIAL = new Set(['US', 'PR', 'GU', 'VI', 'AS', 'MP', 'UM', 'LR', 'BS', 'BZ', 'KY', 'PW', 'FM', 'MH']);
/** Celsius and millimetres, but road-sign miles per hour for the wind. */
const MPH_WIND = new Set(['GB', 'IM', 'JE', 'GG']);

/** The region (ISO 3166 code) the browser's language settings name, or
 *  the one most likely for a bare language ("en" is US English). */
export function browserRegion(languages: readonly string[] = typeof navigator === 'undefined' ? [] : (navigator.languages ?? [navigator.language])): string | null {
  for (const tag of languages) {
    try {
      const loc = new Intl.Locale(tag);
      const region = loc.region ?? loc.maximize().region;
      if (region) return region.toUpperCase();
    } catch {
      // Not a valid tag; try the next.
    }
  }
  return null;
}

/** The units people in a region are used to. */
export function unitsForRegion(region: string | null): Units {
  if (region && IMPERIAL.has(region)) return { ...DEFAULT_UNITS };
  if (region && MPH_WIND.has(region)) return { temp: 'C', speed: 'mph', precip: 'mm' };
  // Most of the world, and a browser that does not say where it is.
  return { ...METRIC_UNITS };
}

const STORAGE_KEY = 'wx.units';

/** Units the user chose, else the norm where the browser is set up for. */
export function loadUnits(storage: Pick<Storage, 'getItem'> | null = safeStorage(), region: string | null = browserRegion()): Units {
  const fallback = unitsForRegion(region);
  try {
    const raw = storage?.getItem(STORAGE_KEY);
    if (!raw) return fallback;
    const u = JSON.parse(raw) as Partial<Units>;
    return {
      temp: u.temp === 'C' || u.temp === 'F' ? u.temp : fallback.temp,
      speed: (['mph', 'kt', 'ms', 'kmh'] as const).includes(u.speed as SpeedUnit) ? (u.speed as SpeedUnit) : fallback.speed,
      precip: u.precip === 'mm' || u.precip === 'in' ? u.precip : fallback.precip,
    };
  } catch {
    return fallback;
  }
}

export function saveUnits(u: Units, storage: Pick<Storage, 'setItem'> | null = safeStorage()) {
  try {
    storage?.setItem(STORAGE_KEY, JSON.stringify(u));
  } catch {
    // Private browsing or a full quota: the choice just is not remembered.
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}

/** How one stored quantity is shown: label, conversion and precision. */
export interface DisplayUnit {
  label: string;
  /** Stored unit to display unit. */
  to: (v: number) => number;
  /** Display unit back to stored unit. */
  from: (v: number) => number;
  decimals: number;
}

const linear = (label: string, k: number, decimals: number): DisplayUnit => ({ label, to: (v) => v * k, from: (v) => v / k, decimals });

export const SPEED_LABEL: Record<SpeedUnit, string> = { mph: 'mph', kt: 'kt', ms: 'm/s', kmh: 'km/h' };
const SPEED_K: Record<SpeedUnit, number> = { mph: 2.2369362920544, kt: 1.9438444924406, ms: 1, kmh: 3.6 };

export function speedUnit(u: SpeedUnit): DisplayUnit {
  return linear(SPEED_LABEL[u], SPEED_K[u], u === 'ms' ? 1 : 0);
}

export function tempUnit(u: TempUnit): DisplayUnit {
  return u === 'F' ? { label: '°F', to: (c) => (c * 9) / 5 + 32, from: (f) => ((f - 32) * 5) / 9, decimals: 0 } : linear('°C', 1, 0);
}

/** Amounts in a point forecast: to the hundredth of an inch, the tenth of a millimetre. */
export function precipUnit(u: PrecipUnit): DisplayUnit {
  return u === 'in' ? linear('in', 1 / 25.4, 2) : linear('mm', 1, 1);
}

export function displayUnit(q: Quantity, units: Units): DisplayUnit {
  switch (q) {
    case 'temperature':
      return tempUnit(units.temp);
    case 'speed':
      return speedUnit(units.speed);
    case 'reflectivity':
      return linear('dBZ', 1, 0);
    case 'precip_rate':
      return units.precip === 'in' ? linear('in/h', 1 / 25.4, 2) : linear('mm/h', 1, 1);
    case 'precip_depth':
      return units.precip === 'in' ? linear('in', 1 / 25.4, 2) : linear('mm', 1, 0);
    case 'snow_depth':
      // Snow is reported in centimetres, not millimetres, where metric is used.
      return units.precip === 'in' ? linear('in', 1 / 25.4, 1) : linear('cm', 0.1, 1);
    case 'percent':
      return linear('%', 1, 0);
    case 'category':
      return linear('', 1, 0);
  }
}

/** Round for display without showing "-0". */
export function formatNumber(v: number, decimals: number): string {
  const s = v.toFixed(decimals);
  return /^-0(\.0+)?$/.test(s) ? s.slice(1) : s;
}

export function formatValue(stored: number, du: DisplayUnit, decimals = du.decimals): string {
  const v = formatNumber(du.to(stored), decimals);
  if (!du.label) return v;
  return du.label.startsWith('°') || du.label === '%' ? `${v}${du.label}` : `${v} ${du.label}`;
}

const POINTS = ['N', 'NNE', 'NE', 'ENE', 'E', 'ESE', 'SE', 'SSE', 'S', 'SSW', 'SW', 'WSW', 'W', 'WNW', 'NW', 'NNW'];

/** Sixteen-point compass name for a direction in degrees. */
export function compass(deg: number): string {
  const i = Math.round((((deg % 360) + 360) % 360) / 22.5) % 16;
  return POINTS[i];
}

/** Meteorological direction (where the wind comes from) of an earth-relative
 *  u/v vector, in degrees clockwise from north. */
export function windFrom(u: number, v: number): number {
  const d = (Math.atan2(-u, -v) * 180) / Math.PI;
  return (d + 360) % 360;
}
