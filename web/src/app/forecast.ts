// Pure helpers for the point forecast panel and its chart: wording, units,
// time zones and which steps to draw. Free of React so they can be tested on
// their own.

import type { HourlyPoint } from '../api/types.ts';
import { clockIn, HOUR, localHours, MIN, sameDay } from '../core/timeline.ts';
import { compass, formatNumber, precipUnit, speedUnit, tempUnit, type Units } from '../core/units.ts';
import { relative } from './describe.ts';

const DAY = 24 * HOUR;

/** Shown in place of a value the forecast does not have. */
export const NONE = '–';

export const isNum = (v: number | null | undefined): v is number => typeof v === 'number' && Number.isFinite(v);
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

export function coords(lat: number, lon: number): string {
  return `${Math.abs(lat).toFixed(2)}°${lat >= 0 ? 'N' : 'S'}, ${Math.abs(lon).toFixed(2)}°${lon >= 0 ? 'E' : 'W'}`;
}

// ---------------------------------------------------------------------------
// Values in the user's units
// ---------------------------------------------------------------------------

/** "14°", or "14°F" with the unit. */
export function formatTemp(c: number | null | undefined, units: Units, withUnit = false): string {
  if (!isNum(c)) return NONE;
  const tu = tempUnit(units.temp);
  return `${formatNumber(tu.to(c), 0)}${withUnit ? tu.label : '°'}`;
}

/** "NNE 11 mph", "Calm"; without the unit where a label beside it says it. */
export function formatWind(ms: number | null | undefined, dir: number | null | undefined, units: Units, withUnit = true): string {
  if (!isNum(ms)) return NONE;
  const su = speedUnit(units.speed);
  const shown = formatNumber(su.to(Math.max(0, ms)), su.decimals);
  if (Number(shown) === 0) return 'Calm';
  return `${isNum(dir) ? `${compass(dir)} ` : ''}${shown}${withUnit ? ` ${su.label}` : ''}`;
}

/** "0.06 in", "1.4 mm"; an amount that would round to nothing says so
 *  rather than showing as zero. */
export function formatPrecip(mm: number | null | undefined, units: Units): string {
  if (!isNum(mm)) return NONE;
  const u = precipUnit(units.precip);
  const v = u.to(Math.max(0, mm));
  if (v === 0) return `0 ${u.label}`;
  const step = 10 ** -u.decimals;
  if (v < step / 2) return `under ${formatNumber(step, u.decimals)} ${u.label}`;
  return `${formatNumber(v, u.decimals)} ${u.label}`;
}

export function formatPop(pop: number | null | undefined): string {
  return isNum(pop) ? `${Math.round(clamp(pop, 0, 100))}%` : NONE;
}

/** What a day's row says about precipitation: the chance where the
 *  forecast has one, else the amount; nothing for a dry day. */
export function dayPrecip(pop: number | null | undefined, mm: number | null | undefined, units: Units): string | null {
  if (isNum(pop) && pop >= 1) return formatPop(pop);
  if (isNum(mm) && mm > 0) return formatPrecip(mm, units);
  return null;
}

/** Wind, then the chance or amount of precipitation, for the step now. */
export function nowDetails(h: HourlyPoint, units: Units): string[] {
  const out: string[] = [];
  const wind = formatWind(h.wind_ms, h.wind_dir_deg, units);
  if (wind !== NONE) out.push(wind === 'Calm' ? 'Calm' : `Wind ${wind}`);
  if (isNum(h.pop) && h.pop >= 1) out.push(`${formatPop(h.pop)} chance of precipitation`);
  else if (isNum(h.precip_mm) && h.precip_mm > 0) out.push(`${formatPrecip(h.precip_mm, units)} of precipitation ${stepHours(h) > 1 ? `in ${stepHours(h)} h` : 'this hour'}`);
  return out;
}

// ---------------------------------------------------------------------------
// Conditions: MET Norway symbol codes
// ---------------------------------------------------------------------------

type ConditionKind = 'clearsky' | 'fair' | 'partlycloudy' | 'cloudy' | 'fog' | 'rain' | 'sleet' | 'snow';

interface Condition {
  kind: ConditionKind;
  intensity: 'light' | 'heavy' | null;
  showers: boolean;
  thunder: boolean;
  variant: 'day' | 'night' | 'polartwilight' | null;
}

/** The codes without precipitation, and their words. */
const SKY: Record<string, string> = { clearsky: 'Clear sky', fair: 'Fair', partlycloudy: 'Partly cloudy', cloudy: 'Cloudy', fog: 'Fog' };
// MET Norway's own list spells two codes "lightssleet..." and "lightssnow...".
const PRECIP = /^(?:(light)s?|(heavy))?(rain|sleet|snow)(showers)?(andthunder)?$/;

export function parseCondition(code: string | null | undefined): Condition | null {
  if (typeof code !== 'string') return null;
  const m = /^([a-z]+?)(?:_(day|night|polartwilight))?$/.exec(code.trim().toLowerCase());
  if (!m) return null;
  const variant = (m[2] as Condition['variant'] | undefined) ?? null;
  if (Object.hasOwn(SKY, m[1])) return { kind: m[1] as ConditionKind, intensity: null, showers: false, thunder: false, variant };
  const p = PRECIP.exec(m[1]);
  if (!p) return null;
  return {
    kind: p[3] as ConditionKind,
    intensity: p[1] ? 'light' : p[2] ? 'heavy' : null,
    showers: !!p[4],
    thunder: !!p[5],
    variant,
  };
}

/** A short phrase for a symbol code, "Light rain showers and thunder"; for
 *  a code it does not know, the provider's text; else null. */
export function conditionWord(code: string | null | undefined, text?: string | null): string | null {
  const c = parseCondition(code);
  if (c) {
    if (Object.hasOwn(SKY, c.kind)) return SKY[c.kind];
    const words = [c.intensity, c.kind, c.showers ? 'showers' : null, c.thunder ? 'and thunder' : null].filter(Boolean).join(' ');
    return words.charAt(0).toUpperCase() + words.slice(1);
  }
  const t = typeof text === 'string' ? text.trim() : '';
  return t || null;
}

export type Glyph = 'sun' | 'moon' | 'partsun' | 'partmoon' | 'cloud' | 'fog' | 'rain' | 'sleet' | 'snow' | 'thunder';

export function conditionGlyph(code: string | null | undefined): Glyph | null {
  const c = parseCondition(code);
  if (!c) return null;
  const night = c.variant === 'night' || c.variant === 'polartwilight';
  if (c.thunder) return 'thunder';
  switch (c.kind) {
    case 'clearsky':
      return night ? 'moon' : 'sun';
    case 'fair':
    case 'partlycloudy':
      return night ? 'partmoon' : 'partsun';
    case 'cloudy':
      return 'cloud';
    default:
      return c.kind;
  }
}

// ---------------------------------------------------------------------------
// Time zones and dates
// ---------------------------------------------------------------------------

function validZone(tz: string | null | undefined): tz is string {
  if (!tz) return false;
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: tz });
    return true;
  } catch {
    return false;
  }
}

function browserZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  } catch {
    return 'UTC';
  }
}

interface Zone {
  /** For Intl; undefined is the browser's own zone. */
  timeZone: string | undefined;
  /** Says which zone the times are in, where that is not obvious. */
  note: string | null;
}

/** The point's own zone when the forecast gives one, else the browser's. */
export function displayZone(tz: string | null | undefined, browser = browserZone(), at = Date.now(), locale?: string): Zone {
  if (validZone(tz)) {
    if (tz === browser) return { timeZone: tz, note: null };
    const name = new Intl.DateTimeFormat(locale, { timeZone: tz, timeZoneName: 'short' }).formatToParts(at).find((p) => p.type === 'timeZoneName')?.value;
    return { timeZone: tz, note: `Times in ${name ?? tz}` };
  }
  return { timeZone: undefined, note: 'Times in your time zone' };
}

/** Today's date at the point: in its zone, or by solar time from its
 *  longitude when the forecast has no zone, as the daily dates then are. */
export function pointToday(now: number, tz: string | null | undefined, lon: number): string {
  if (validZone(tz)) return clockIn(now, tz).date;
  return new Date(now + (lon / 15) * HOUR).toISOString().slice(0, 10);
}

/** "Today" or "Sat" over "Nov 7", from the date's own parts: a daily date is
 *  already local, and must not be shifted through a time zone. */
export function dayLabel(date: string, today: string, locale?: string): { name: string; sub: string } {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!m) return { name: date, sub: '' };
  const ms = Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  const name = date === today ? 'Today' : new Intl.DateTimeFormat(locale, { weekday: 'short', timeZone: 'UTC' }).format(ms);
  return { name, sub: new Intl.DateTimeFormat(locale, { month: 'short', day: 'numeric', timeZone: 'UTC' }).format(ms) };
}

/** "6:12 AM", with the weekday when it is not today in that zone. */
export function formatClock(t: number, timeZone: string | undefined, now: number, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { weekday: sameDay(t, now, timeZone) ? undefined : 'short', hour: 'numeric', minute: '2-digit', timeZone }).format(t);
}

/** "Issued 6:12 AM (25 min ago)"; null without a usable time. */
export function issuedText(generatedAt: string | null | undefined, timeZone: string | undefined, now: number, locale?: string): string | null {
  if (!generatedAt) return null;
  const t = Date.parse(generatedAt);
  if (!Number.isFinite(t)) return null;
  const when = formatClock(t, timeZone, now, locale);
  // A clock running behind the server's would say "in 2 min".
  if (t > now) return `Issued ${when}`;
  const ago = relative(t, now);
  return `Issued ${when} (${ago === 'now' ? 'just now' : ago})`;
}

// ---------------------------------------------------------------------------
// Steps
// ---------------------------------------------------------------------------

export const stepStart = (h: HourlyPoint) => Date.parse(h.time);
export const stepHours = (h: HourlyPoint) => (isNum(h.span_h) && h.span_h > 0 ? h.span_h : 1);
export const stepEnd = (h: HourlyPoint) => stepStart(h) + stepHours(h) * HOUR;

/** Steps with a readable time, in order, none already over. */
export function currentSteps(hourly: readonly HourlyPoint[] | null | undefined, now: number): HourlyPoint[] {
  return (hourly ?? []).filter((h) => Number.isFinite(stepStart(h)) && stepEnd(h) > now).sort((a, b) => stepStart(a) - stepStart(b));
}

export type Range = 48 | 'all';

export function stepsInRange(steps: HourlyPoint[], range: Range): HourlyPoint[] {
  if (range === 'all' || steps.length === 0) return steps;
  const end = stepStart(steps[0]) + 48 * HOUR;
  return steps.filter((h) => stepStart(h) < end);
}

/** How far the forecast reaches, in days. */
export function forecastDays(steps: readonly HourlyPoint[]): number {
  if (steps.length === 0) return 0;
  return (stepEnd(steps[steps.length - 1]) - stepStart(steps[0])) / DAY;
}

export function rangeLabel(days: number): string {
  return `${Math.max(1, Math.round(days))} days`;
}

/** What the precipitation panel shows, for the whole forecast: the chance
 *  where it has one, else the amount. One choice, so the axis means one thing. */
export type PrecipMode = 'pop' | 'amount' | 'none';

export function precipMode(steps: readonly HourlyPoint[]): PrecipMode {
  if (steps.some((h) => isNum(h.pop))) return 'pop';
  if (steps.some((h) => isNum(h.precip_mm))) return 'amount';
  return 'none';
}

/** The first step longer than an hour, or -1. */
export function firstLongStep(steps: readonly HourlyPoint[]): number {
  return steps.findIndex((h) => stepHours(h) > 1);
}

/** The value the precipitation panel draws for a step: percent, or the
 *  average rate over the step in display units per hour (so a six-hour bar,
 *  six times as wide, has an area that is its amount). */
export function precipValue(h: HourlyPoint, mode: PrecipMode, units: Units): number | null {
  if (mode === 'pop') return isNum(h.pop) ? clamp(h.pop, 0, 100) : null;
  if (mode === 'amount') return isNum(h.precip_mm) ? precipUnit(units.precip).to(Math.max(0, h.precip_mm) / stepHours(h)) : null;
  return null;
}

export function niceCeil(v: number): number {
  if (!(v > 0)) return 1;
  const p = 10 ** Math.floor(Math.log10(v));
  for (const m of [1, 2, 2.5, 5, 10]) if (m * p >= v - 1e-12) return m * p;
  return 10 * p;
}

/** Top of the precipitation-rate axis: at least a light shower's worth,
 *  so a trace does not fill the panel. */
export function rateTop(maxRate: number, units: Units): number {
  return niceCeil(Math.max(units.precip === 'in' ? 0.04 : 1, maxRate));
}

// ---------------------------------------------------------------------------
// Day and night, and the time axis
// ---------------------------------------------------------------------------

const RAD = Math.PI / 180;

/** Solar elevation in degrees (the usual low-precision almanac formulas). */
export function sunElevation(lat: number, lon: number, t: number): number {
  const d = t / DAY - 10957.5;
  const g = (357.529 + 0.98560028 * d) * RAD;
  const q = 280.459 + 0.98564736 * d;
  const L = (q + 1.915 * Math.sin(g) + 0.02 * Math.sin(2 * g)) * RAD;
  const e = (23.439 - 0.00000036 * d) * RAD;
  const ra = Math.atan2(Math.cos(e) * Math.sin(L), Math.cos(L)) / RAD;
  const dec = Math.asin(Math.sin(e) * Math.sin(L));
  const ha = (280.46061837 + 360.98564736629 * d + lon - ra) * RAD;
  return Math.asin(Math.sin(lat * RAD) * Math.sin(dec) + Math.cos(lat * RAD) * Math.cos(dec) * Math.cos(ha)) / RAD;
}

/** Stretches between t0 and t1 when the sun is down at the point. */
export function nightSpans(lat: number, lon: number, t0: number, t1: number, stepMs = 10 * MIN): [number, number][] {
  const out: [number, number][] = [];
  let start: number | null = null;
  for (let t = t0; t <= t1; t += stepMs) {
    // Below the horizon allowing for refraction and the sun's radius.
    const night = sunElevation(lat, lon, t) < -0.833;
    if (night && start === null) start = t;
    if (!night && start !== null) {
      out.push([start, t]);
      start = null;
    }
  }
  if (start !== null) out.push([start, t1]);
  return out;
}

/** Local midnights and every `everyH`-th local hour (none for 0) between t0
 *  and t1, in a zone. */
export function timeTicks(t0: number, t1: number, timeZone: string | undefined, everyH: number): { hours: number[]; days: number[] } {
  const hours: number[] = [];
  const days: number[] = [];
  for (const { t, hour } of localHours(t0, t1, timeZone)) {
    if (hour === 0) {
      if (t > t0) days.push(t);
    } else if (everyH > 0 && hour % everyH === 0) hours.push(t);
  }
  return { hours, days };
}

// ---------------------------------------------------------------------------
// The provider's written forecast
// ---------------------------------------------------------------------------

const PERIOD_NAME = "[A-Z][A-Za-z']*(?: [A-Z][A-Za-z']*){0,2}";

/** "Tonight: Clear. Saturday: Sunny." as its named periods; text without
 *  names is one unnamed part. */
export function splitPeriods(text: string | null | undefined): { name: string | null; text: string }[] {
  const s = typeof text === 'string' ? text.trim() : '';
  if (!s) return [];
  const parts: string[] = [];
  let from = 0;
  for (const m of s.matchAll(new RegExp(`[.!?]\\s+(?=${PERIOD_NAME}: )`, 'g'))) {
    parts.push(s.slice(from, m.index + 1));
    from = m.index + m[0].length;
  }
  parts.push(s.slice(from));
  const named = new RegExp(`^(${PERIOD_NAME}): ([\\s\\S]*)$`);
  return parts.map((p) => {
    const m = named.exec(p.trim());
    return m ? { name: m[1], text: m[2].trim() } : { name: null, text: p.trim() };
  });
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

export function forecastError(code: string, status: number): { text: string; retry: boolean } {
  if (status === 400 || code === 'bad_request') return { text: 'There is no forecast for these coordinates.', retry: false };
  if (code === 'upstream' || status === 502) return { text: 'No forecast service answered for this spot. They are sometimes slow; try again in a moment.', retry: true };
  if (code === 'network') return { text: 'The server could not be reached.', retry: true };
  return { text: 'The forecast could not be loaded.', retry: true };
}
