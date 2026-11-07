import { describe, expect, it } from 'vitest';
import { LAYERS } from '../../mock/registry.ts';
import { DEFAULT_UNITS } from '../core/units.ts';
import { sourceDefs } from '../../mock/sources.ts';
import { part as testPart, tframe, WORLD } from '../testing.ts';
import { aboutKm, describeSample, frameBadge, mediumWords, partLabel } from './describe.ts';

const frame = tframe(0);
const top = null;
const layer = (id: string) => LAYERS.find((l) => l.id === id)!;

describe('read-out wording', () => {
  it('colours the wind swatch by what the map is coloured by', () => {
    const calm = { frame, values: [1, 1, 30], status: 'ok' as const, top };
    const bySpeed = describeSample(layer('wind'), calm, DEFAULT_UNITS, false, false);
    const byGust = describeSample(layer('wind'), calm, DEFAULT_UNITS, false, true);
    expect(byGust.swatch).not.toEqual(bySpeed.swatch);
    expect(byGust.text).toBe(bySpeed.text);
  });

  it('says "trace" rather than 0.00 for drizzle that is drawn', () => {
    const drizzle = { frame, values: [0.08, 1], status: 'ok' as const, top };
    const d = describeSample(layer('precip'), drizzle, DEFAULT_UNITS, false, false);
    expect(d.text).toBe('Rain');
    expect(d.detail).not.toMatch(/0\.00/);
    expect(d.detail).toMatch(/trace/);
    const metric = describeSample(layer('precip'), drizzle, { ...DEFAULT_UNITS, precip: 'mm' }, false, false);
    expect(metric.detail).toMatch(/0\.1 mm\/h/);
  });

  it('names no echo, no coverage and below-threshold radar', () => {
    const r = layer('radar');
    expect(describeSample(r, { frame, values: [-30], status: 'ok', top }, DEFAULT_UNITS, false, false).text).toBe('No echo');
    expect(describeSample(r, { frame, values: [null], status: 'ok', top }, DEFAULT_UNITS, false, false).text).toBe('No radar coverage');
    expect(describeSample(r, { frame, values: [7], status: 'ok', top }, DEFAULT_UNITS, false, false).detail).toMatch(/weak-echo/);
  });
});

describe('provenance', () => {
  const sources = Object.fromEntries(sourceDefs().map((d) => [d.id, d]));
  const part = (source: string, run: string | null) => testPart(`${source}-x`, WORLD, null, 4, run);

  it('says resolutions and media in words', () => {
    expect([aboutKm(2.200000047683716), aboutKm(13.000001), aboutKm(0.25), aboutKm(27.75), aboutKm(0)]).toEqual(['2.2 km', '13 km', '0.25 km', '28 km', '']);
    expect(mediumWords({ medium: 'satellite', kind: 'observed' })).toBe('satellite estimates');
    expect(mediumWords({ medium: 'analysis', kind: 'observed' })).toBe('analyses of observations');
  });

  it('names a model with its run, an observation by itself', () => {
    expect(partLabel(part('hrrr', '2026-11-06T11:00:00Z'), sources)).toBe('HRRR 11Z run');
    expect(partLabel(part('mrms', null), sources)).toBe('MRMS');
    expect(partLabel(part('nope', null), sources)).toBe('NOPE');
  });

  it('labels a mosaic frame by what is on top at the centre, and how much else is in view', () => {
    const f = { ...frame, kind: 'forecast' as const, stack: [part('hrrr_ak', '2026-11-06T18:00:00Z'), part('hrrr', '2026-11-06T11:00:00Z'), part('gfs', '2026-11-06T06:00:00Z')] };
    // A world view centred on the Atlantic: the global model at the centre,
    // not the top-ranked part of the frame.
    expect(frameBadge(f, sources, ['hrrr_ak', 'hrrr', 'gfs'], 'gfs').text).toBe('Forecast · GFS 06Z run and 2 others');
    expect(frameBadge(f, sources, ['hrrr', 'gfs'], 'hrrr').text).toBe('Forecast · HRRR 11Z run and 1 other');
    expect(frameBadge(f, sources, ['gfs'], 'gfs').text).toBe('Forecast · GFS 06Z run');
    // Observations say what kind of looking they are.
    const obs = { ...frame, stack: [part('mrms', null), part('cmorph2', null)] };
    expect(frameBadge(obs, sources, ['mrms', 'cmorph2'], 'mrms').text).toBe('Observed · MRMS radar and 1 other');
    expect(frameBadge(obs, sources, ['cmorph2'], 'cmorph2').text).toBe('Observed · CMORPH2 satellite');
    // An observed frame filled by a model where the analysis does not reach:
    // the badge follows the part named, not the frame.
    const filled = { ...frame, kind: 'observed' as const, stack: [part('rtma', null), part('gfs', '2026-11-06T06:00:00Z')] };
    expect(frameBadge(filled, sources, ['rtma', 'gfs'], 'gfs')).toEqual({ kind: 'forecast', text: 'Forecast · GFS 06Z run and 1 other' });
    expect(frameBadge(filled, sources, ['rtma', 'gfs'], 'rtma').kind).toBe('observed');
    // Not known yet: one source in view is named, several are counted.
    expect(frameBadge(f, sources, ['gfs'], null).text).toBe('Forecast · GFS 06Z run');
    expect(frameBadge(f, sources, ['hrrr', 'gfs'], null).text).toBe('Forecast · 2 sources');
    // A frame with nothing at the centre or in view (only regional parts elsewhere).
    expect(frameBadge({ ...frame, stack: [] }, sources, [], null).text).toBe('Observed');
  });
});
