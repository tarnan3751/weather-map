import { describe, expect, it } from 'vitest';
import type { HourlyPoint } from '../api/types.ts';
import { HOUR, MIN } from '../core/timeline.ts';
import { DEFAULT_UNITS, METRIC_UNITS, type Units } from '../core/units.ts';
import { T0 } from '../testing.ts';
import {
  NONE,
  conditionGlyph,
  conditionWord,
  coords,
  currentSteps,
  dayLabel,
  dayPrecip,
  displayZone,
  firstLongStep,
  forecastDays,
  forecastError,
  formatPop,
  formatPrecip,
  formatTemp,
  formatWind,
  issuedText,
  nightSpans,
  nowDetails,
  parseCondition,
  pointToday,
  precipMode,
  precipValue,
  rangeLabel,
  rateTop,
  splitPeriods,
  stepsInRange,
  sunElevation,
  timeTicks,
} from './forecast.ts';

const US: Units = DEFAULT_UNITS;
const SI: Units = METRIC_UNITS;
const MS: Units = { temp: 'C', speed: 'ms', precip: 'mm' };

function step(h: number, over: Partial<HourlyPoint> = {}): HourlyPoint {
  return {
    time: new Date(T0 + h * HOUR).toISOString(),
    span_h: 1,
    temp_c: 10,
    pop: null,
    precip_mm: null,
    wind_ms: 3,
    wind_dir_deg: 90,
    condition: 'cloudy',
    text: 'Cloudy',
    ...over,
  };
}

const bad = /null|undefined|NaN|Infinity/;

describe('values in the user units', () => {
  it('never prints a missing number', () => {
    for (const u of [US, SI, MS]) {
      for (const v of [null, undefined, Number.NaN, Infinity]) {
        for (const s of [formatTemp(v, u), formatTemp(v, u, true), formatWind(v, 10, u), formatPrecip(v, u), formatPop(v)]) {
          expect(s).toBe(NONE);
        }
      }
      expect(formatWind(5, null, u)).not.toMatch(bad);
      expect(formatWind(5, Number.NaN, u)).not.toMatch(bad);
      expect(nowDetails(step(0, { temp_c: null, wind_ms: null, wind_dir_deg: null, pop: null, precip_mm: null }), u)).toEqual([]);
      expect(dayPrecip(null, null, u)).toBeNull();
    }
  });

  it('converts from SI', () => {
    expect(formatTemp(0, US)).toBe('32°');
    expect(formatTemp(-0.2, SI)).toBe('0°');
    expect(formatTemp(21.4, US, true)).toBe('71°F');
    expect(formatWind(5, 22.5, US)).toBe('NNE 11 mph');
    expect(formatWind(5, 22.5, SI)).toBe('NNE 18 km/h');
    expect(formatWind(5, 270, MS, false)).toBe('W 5.0');
    expect(formatWind(0.1, 270, US)).toBe('Calm');
    expect(formatPrecip(25.4, US)).toBe('1.00 in');
    expect(formatPrecip(1.44, SI)).toBe('1.4 mm');
    expect(formatPrecip(0, SI)).toBe('0 mm');
    expect(formatPrecip(0.1, US)).toBe('under 0.01 in');
    expect(formatPop(42.4)).toBe('42%');
    expect(formatPop(130)).toBe('100%');
  });

  it('describes a day by its chance, else its amount, else nothing', () => {
    expect(dayPrecip(40, 2.6, SI)).toBe('40%');
    expect(dayPrecip(null, 2.6, SI)).toBe('2.6 mm');
    expect(dayPrecip(0, 0, SI)).toBeNull();
    expect(dayPrecip(null, 0, SI)).toBeNull();
  });

  it('sums up the step now', () => {
    expect(nowDetails(step(0, { pop: 40 }), US)).toEqual(['Wind E 7 mph', '40% chance of precipitation']);
    expect(nowDetails(step(0, { precip_mm: 1.4 }), SI)).toEqual(['Wind E 11 km/h', '1.4 mm of precipitation this hour']);
    expect(nowDetails(step(0, { wind_ms: 0 }), SI)).toEqual(['Calm']);
  });

  it('writes coordinates with hemispheres', () => {
    expect(coords(-1.2921, 36.822)).toBe('1.29°S, 36.82°E');
    expect(coords(35.2, -97.44)).toBe('35.20°N, 97.44°W');
  });
});

describe('conditions', () => {
  it('words known codes', () => {
    expect(conditionWord('clearsky_day')).toBe('Clear sky');
    expect(conditionWord('partlycloudy_polartwilight')).toBe('Partly cloudy');
    expect(conditionWord('lightrainshowers_day')).toBe('Light rain showers');
    expect(conditionWord('heavysnow')).toBe('Heavy snow');
    expect(conditionWord('rainandthunder')).toBe('Rain and thunder');
    expect(conditionWord('heavysleetshowersandthunder_night')).toBe('Heavy sleet showers and thunder');
    // MET Norway's own spelling of two codes.
    expect(conditionWord('lightssnowshowersandthunder_day')).toBe('Light snow showers and thunder');
    expect(conditionWord('fog')).toBe('Fog');
  });

  it('falls back to the text, then to nothing', () => {
    expect(conditionWord('volcanicash', 'Ash fall')).toBe('Ash fall');
    expect(conditionWord(null, 'Patchy Fog')).toBe('Patchy Fog');
    expect(conditionWord('cloudy_sunset', null)).toBeNull();
    expect(conditionWord(null, '  ')).toBeNull();
    expect(conditionWord(undefined)).toBeNull();
    expect(parseCondition('rain_day_night')).toBeNull();
  });

  it('parses the parts', () => {
    expect(parseCondition('lightrainshowersandthunder_night')).toEqual({ kind: 'rain', intensity: 'light', showers: true, thunder: true, variant: 'night' });
    expect(parseCondition('sleet')).toEqual({ kind: 'sleet', intensity: null, showers: false, thunder: false, variant: null });
  });

  it('picks a glyph, with night versions', () => {
    expect(conditionGlyph('clearsky_day')).toBe('sun');
    expect(conditionGlyph('clearsky_night')).toBe('moon');
    expect(conditionGlyph('fair_polartwilight')).toBe('partmoon');
    expect(conditionGlyph('partlycloudy_day')).toBe('partsun');
    expect(conditionGlyph('cloudy')).toBe('cloud');
    expect(conditionGlyph('heavyrainshowersandthunder_day')).toBe('thunder');
    expect(conditionGlyph('lightsnow')).toBe('snow');
    expect(conditionGlyph('sleetshowers_night')).toBe('sleet');
    expect(conditionGlyph('somethingnew')).toBeNull();
  });
});

describe('time zones and dates', () => {
  it('uses the point zone, or says the times are the browser zone', () => {
    expect(displayZone('America/Chicago', 'America/Chicago')).toEqual({ timeZone: 'America/Chicago', note: null });
    expect(displayZone('America/Chicago', 'Europe/Oslo', T0, 'en-US')).toEqual({ timeZone: 'America/Chicago', note: 'Times in CST' });
    expect(displayZone(null, 'Europe/Oslo')).toEqual({ timeZone: undefined, note: 'Times in your time zone' });
    expect(displayZone('Not/A_Zone', 'Europe/Oslo')).toEqual({ timeZone: undefined, note: 'Times in your time zone' });
  });

  it('labels a day from its own date parts', () => {
    expect(dayLabel('2026-11-06', '2026-11-06', 'en-US')).toEqual({ name: 'Today', sub: 'Nov 6' });
    expect(dayLabel('2026-11-07', '2026-11-06', 'en-US')).toEqual({ name: 'Sat', sub: 'Nov 7' });
    // A date is never shifted: 1 January stays 1 January whatever the zone.
    expect(dayLabel('2027-01-01', '2026-12-31', 'en-US')).toEqual({ name: 'Fri', sub: 'Jan 1' });
    expect(dayLabel('soon', '2026-11-06')).toEqual({ name: 'soon', sub: '' });
  });

  it("finds today's date at the point", () => {
    // 12:00 UTC is 6 am in Chicago and 11:49 pm by the sun at 177.4 E.
    expect(pointToday(T0, 'America/Chicago', -97)).toBe('2026-11-06');
    expect(pointToday(T0, 'Pacific/Auckland', 174.8)).toBe('2026-11-07');
    expect(pointToday(T0, null, 177.4)).toBe('2026-11-06');
    expect(pointToday(T0 + 20 * MIN, null, 177.4)).toBe('2026-11-07');
    expect(pointToday(T0, null, -150)).toBe('2026-11-06');
  });

  it('says when the forecast was issued', () => {
    expect(issuedText('2026-11-06T11:35:00Z', 'UTC', T0, 'en-US')).toBe('Issued 11:35 AM (25 min ago)');
    expect(issuedText('2026-11-05T09:00:00Z', 'UTC', T0, 'en-US')).toBe('Issued Thu 9:00 AM (27 h ago)');
    expect(issuedText('2026-11-06T12:05:00Z', 'UTC', T0, 'en-US')).toBe('Issued 12:05 PM');
    expect(issuedText(null, 'UTC', T0)).toBeNull();
    expect(issuedText('yesterday', 'UTC', T0)).toBeNull();
  });

  it('puts ticks on local midnights and hours, also in half-hour zones', () => {
    const kolkata = timeTicks(T0, T0 + 48 * HOUR, 'Asia/Kolkata', 6);
    // Midnight in India is 18:30 UTC.
    expect(kolkata.days.map((t) => new Date(t).toISOString())).toEqual(['2026-11-06T18:30:00.000Z', '2026-11-07T18:30:00.000Z']);
    expect(kolkata.hours.length).toBe(6);
    const chicago = timeTicks(T0, T0 + 6 * 24 * HOUR, 'America/Chicago', 0);
    expect(chicago.hours).toEqual([]);
    expect(chicago.days.length).toBe(6);
  });
});

describe('steps', () => {
  const metNo = [...Array.from({ length: 60 }, (_, h) => step(h, { precip_mm: 0.2 })), ...Array.from({ length: 26 }, (_, k) => step(60 + 6 * k, { span_h: 6, precip_mm: 3 }))];
  const nws = Array.from({ length: 156 }, (_, h) => step(h, { pop: 20 }));

  it('drops steps already over and sorts the rest', () => {
    const steps = currentSteps([step(2), step(0), step(1), step(-1), step(3, { time: 'later' })], T0 + 30 * MIN);
    expect(steps.map((h) => h.time)).toEqual([step(0).time, step(1).time, step(2).time]);
    expect(currentSteps(null, T0)).toEqual([]);
  });

  it('cuts the range at 48 hours and knows how far the forecast reaches', () => {
    expect(stepsInRange(metNo, 48).length).toBe(48);
    expect(stepsInRange(metNo, 'all')).toBe(metNo);
    expect(forecastDays(metNo)).toBeCloseTo((60 + 26 * 6) / 24);
    expect(rangeLabel(forecastDays(nws))).toBe('7 days');
    expect(rangeLabel(forecastDays(metNo))).toBe('9 days');
    expect(firstLongStep(metNo)).toBe(60);
    expect(firstLongStep(nws)).toBe(-1);
  });

  it('charts the chance where there is one, else the amount', () => {
    expect(precipMode(nws)).toBe('pop');
    expect(precipMode(metNo)).toBe('amount');
    expect(precipMode([step(0, { pop: 30, precip_mm: 1 })])).toBe('pop');
    expect(precipMode([step(0)])).toBe('none');
  });

  it('draws six-hour amounts as their average rate', () => {
    expect(precipValue(step(0, { span_h: 6, precip_mm: 3 }), 'amount', SI)).toBeCloseTo(0.5);
    expect(precipValue(step(0, { precip_mm: 25.4 }), 'amount', US)).toBeCloseTo(1);
    expect(precipValue(step(0, { pop: 140 }), 'pop', SI)).toBe(100);
    expect(precipValue(step(0, { pop: null }), 'pop', SI)).toBeNull();
    expect(rateTop(0.2, SI)).toBe(1);
    expect(rateTop(3.3, SI)).toBe(5);
    expect(rateTop(0.01, US)).toBe(0.05);
  });
});

describe('day and night', () => {
  it('finds the sun', () => {
    // Noon at the equator on the equinox: nearly overhead; midnight: below.
    const equinox = Date.UTC(2026, 2, 20, 12, 0);
    expect(sunElevation(0, 0, equinox)).toBeGreaterThan(85);
    expect(sunElevation(0, 0, equinox + 12 * HOUR)).toBeLessThan(-80);
    // Norman, OK at 6 am CST in November: before sunrise.
    expect(sunElevation(35.2, -97.4, T0)).toBeLessThan(0);
    expect(sunElevation(35.2, -97.4, T0 + 6 * HOUR)).toBeGreaterThan(30);
  });

  it('shades about twelve hours of night a day near the equator', () => {
    const spans = nightSpans(-1.3, 36.8, T0, T0 + 48 * HOUR);
    const total = spans.reduce((s, [a, b]) => s + b - a, 0) / HOUR;
    expect(total).toBeGreaterThan(22);
    expect(total).toBeLessThan(25);
    // Polar night: the whole span.
    expect(nightSpans(80, 15, Date.UTC(2026, 11, 21), Date.UTC(2026, 11, 22))).toHaveLength(1);
  });
});

describe('the provider text', () => {
  it('splits NWS periods by name', () => {
    expect(splitPeriods('Overnight: Patchy fog. Mostly clear, with a low around 45. Friday: Sunny, with a high near 58. Friday Night: Clear.')).toEqual([
      { name: 'Overnight', text: 'Patchy fog. Mostly clear, with a low around 45.' },
      { name: 'Friday', text: 'Sunny, with a high near 58.' },
      { name: 'Friday Night', text: 'Clear.' },
    ]);
    expect(splitPeriods("Washington's Birthday: Sunny. This Afternoon: Rain. Chance of precipitation is 80%. New rainfall amounts: little.")).toEqual([
      { name: "Washington's Birthday", text: 'Sunny.' },
      { name: 'This Afternoon', text: 'Rain. Chance of precipitation is 80%. New rainfall amounts: little.' },
    ]);
    expect(splitPeriods('Light rain later.')).toEqual([{ name: null, text: 'Light rain later.' }]);
    expect(splitPeriods(null)).toEqual([]);
    expect(splitPeriods('')).toEqual([]);
  });
});

describe('errors', () => {
  it('words the protocol errors', () => {
    expect(forecastError('bad_request', 400).retry).toBe(false);
    expect(forecastError('upstream', 502)).toMatchObject({ retry: true });
    expect(forecastError('upstream', 502).text).not.toMatch(/coverage|National Weather Service/);
    expect(forecastError('network', 0).text).toBe('The server could not be reached.');
    expect(forecastError('teapot', 418).retry).toBe(true);
  });
});
