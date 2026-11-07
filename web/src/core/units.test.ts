import { describe, expect, it } from 'vitest';
import { memoryStorage } from '../testing.ts';
import { browserRegion, compass, DEFAULT_UNITS, displayUnit, formatValue, loadUnits, METRIC_UNITS, saveUnits, unitsForRegion, windFrom } from './units.ts';

const close = (a: number, b: number, eps = 1e-6) => expect(Math.abs(a - b)).toBeLessThan(eps);

describe('unit conversions', () => {
  it('defaults to US units', () => {
    expect(DEFAULT_UNITS).toEqual({ temp: 'F', speed: 'mph', precip: 'in' });
  });

  it('converts temperature both ways', () => {
    const f = displayUnit('temperature', { ...DEFAULT_UNITS, temp: 'F' });
    close(f.to(0), 32);
    close(f.to(100), 212);
    close(f.to(-40), -40);
    close(f.from(f.to(12.34)), 12.34);
    expect(displayUnit('temperature', { ...DEFAULT_UNITS, temp: 'C' }).to(21.5)).toBe(21.5);
  });

  it('converts wind speed to every unit', () => {
    close(displayUnit('speed', { ...DEFAULT_UNITS, speed: 'mph' }).to(10), 22.369362920544, 1e-9);
    close(displayUnit('speed', { ...DEFAULT_UNITS, speed: 'kt' }).to(10), 19.438444924406, 1e-9);
    close(displayUnit('speed', { ...DEFAULT_UNITS, speed: 'kmh' }).to(10), 36);
    close(displayUnit('speed', { ...DEFAULT_UNITS, speed: 'ms' }).to(10), 10);
    const kt = displayUnit('speed', { ...DEFAULT_UNITS, speed: 'kt' });
    close(kt.from(kt.to(7.7)), 7.7);
  });

  it('converts precipitation, and snow to centimetres in metric', () => {
    close(displayUnit('precip_depth', DEFAULT_UNITS).to(25.4), 1);
    close(displayUnit('precip_rate', DEFAULT_UNITS).to(50.8), 2);
    expect(displayUnit('precip_depth', { ...DEFAULT_UNITS, precip: 'mm' }).label).toBe('mm');
    const cm = displayUnit('snow_depth', { ...DEFAULT_UNITS, precip: 'mm' });
    expect(cm.label).toBe('cm');
    close(cm.to(254), 25.4);
    expect(displayUnit('reflectivity', DEFAULT_UNITS).label).toBe('dBZ');
  });

  it('formats without negative zero and with the unit', () => {
    const c = displayUnit('temperature', { ...DEFAULT_UNITS, temp: 'C' });
    expect(formatValue(-0.2, c)).toBe('0°C');
    expect(formatValue(25.4, displayUnit('precip_depth', DEFAULT_UNITS))).toBe('1.00 in');
    expect(formatValue(4.47, displayUnit('speed', DEFAULT_UNITS))).toBe('10 mph');
  });

  it('names compass points and wind direction from u/v', () => {
    expect(compass(0)).toBe('N');
    expect(compass(359)).toBe('N');
    expect(compass(225)).toBe('SW');
    expect(compass(-90)).toBe('W');
    // Wind blowing toward the north-east comes from the south-west.
    close(windFrom(1, 1), 225);
    close(windFrom(0, -5), 0);
    close(windFrom(-3, 0), 90);
  });

  it('remembers choices and survives junk in storage', () => {
    const s = memoryStorage();
    saveUnits({ temp: 'C', speed: 'kt', precip: 'mm' }, s);
    expect(loadUnits(s, 'US')).toEqual({ temp: 'C', speed: 'kt', precip: 'mm' });
    s.setItem('wx.units', '{"temp":"K","speed":"furlongs"}');
    expect(loadUnits(s, 'US')).toEqual(DEFAULT_UNITS);
    s.setItem('wx.units', 'not json');
    expect(loadUnits(s, 'US')).toEqual(DEFAULT_UNITS);
    expect(loadUnits(null, 'US')).toEqual(DEFAULT_UNITS);
    // A choice made is kept whatever the region.
    saveUnits({ temp: 'F', speed: 'mph', precip: 'in' }, s);
    expect(loadUnits(s, 'DE')).toEqual(DEFAULT_UNITS);
  });

  it('defaults to what people use where the browser is set up for', () => {
    expect(loadUnits(null, 'DE')).toEqual(METRIC_UNITS);
    expect(METRIC_UNITS).toEqual({ temp: 'C', speed: 'kmh', precip: 'mm' });
    expect(unitsForRegion('US')).toEqual(DEFAULT_UNITS);
    expect(unitsForRegion('PR')).toEqual(DEFAULT_UNITS);
    expect(unitsForRegion('LR')).toEqual(DEFAULT_UNITS);
    expect(unitsForRegion('GB')).toEqual({ temp: 'C', speed: 'mph', precip: 'mm' });
    expect(unitsForRegion('CA')).toEqual(METRIC_UNITS);
    expect(unitsForRegion('AU')).toEqual(METRIC_UNITS);
    expect(unitsForRegion(null)).toEqual(METRIC_UNITS);
  });

  it('reads the region from the browser’s languages', () => {
    expect(browserRegion(['en-US'])).toBe('US');
    expect(browserRegion(['de-AT', 'de'])).toBe('AT');
    expect(browserRegion(['en-GB'])).toBe('GB');
    // A bare language means its most likely region.
    expect(browserRegion(['fr'])).toBe('FR');
    expect(browserRegion(['ja'])).toBe('JP');
    expect(browserRegion(['not a tag', 'pt-BR'])).toBe('BR');
    expect(browserRegion([])).toBeNull();
  });
});
