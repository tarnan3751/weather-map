import { describe, expect, it } from 'vitest';
import type { Part, SourceDef } from '../api/types.ts';
import { creditedSources, type Answer } from './inView.ts';

const src = (id: string, medium: SourceDef['medium']) => ({ id, medium }) as SourceDef;
const sources = { hrdps: src('hrdps', 'model'), hrrr: src('hrrr', 'model'), gfs: src('gfs', 'model'), mrms: src('mrms', 'radar') };
const part = (source: string, bounds: Part['bounds'], solid: Part['solid'] = null): Part => ({ id: `${source}-1`, source, valid: '2026-11-06T18:00:00Z', run: null, kind: 'forecast', bounds, solid, max_zoom: 6 });
// HRDPS's box reaches 27 N though its data stops near the border.
const stack = [
  part('hrdps', { west: -152, south: 27, east: -40, north: 70 }, { west: -125, south: 45, east: -60, north: 60 }),
  part('hrrr', { west: -134, south: 21, east: -60, north: 53 }, { west: -120, south: 25, east: -72, north: 49 }),
  part('mrms', { west: -130, south: 20, east: -60, north: 55 }),
  part('gfs', { west: -180, south: -90, east: 180, north: 90 }, { west: -180, south: -90, east: 180, north: 90 }),
];
const answer = (lon: number, lat: number, named: string[]): Answer => ({ layer: 'temp', lon, lat, named: new Set(named), stack: new Set(stack.map((p) => p.source)) });
const ALL = ['hrdps', 'hrrr', 'mrms', 'gfs'];

describe('credits for the view', () => {
  const gulf = { west: -98, south: 20, east: -80, north: 31 };

  it('fall back to bounds where nothing has been asked', () => {
    expect(creditedSources(ALL, stack, sources, gulf, 'temp', [])).toEqual(ALL);
  });

  it('drop a model whose box, not its data, reaches the view', () => {
    const asked = [answer(-89, 25, ['hrrr', 'mrms'])];
    expect(creditedSources(ALL, stack, sources, gulf, 'temp', asked)).toEqual(['hrrr', 'mrms', 'gfs']);
    // An answer for another layer, or outside the view, says nothing here.
    expect(creditedSources(ALL, stack, sources, gulf, 'temp', [{ ...asked[0], layer: 'radar' }])).toEqual(ALL);
    expect(creditedSources(ALL, stack, sources, gulf, 'temp', [answer(-100, 45, ['hrrr'])])).toEqual(ALL);
  });

  it('keep what is surely in view, what an answer names, and ragged observations', () => {
    // The view reaches HRDPS's solid box over Canada.
    const north = { west: -100, south: 35, east: -80, north: 50 };
    expect(creditedSources(ALL, stack, sources, north, 'temp', [answer(-90, 40, ['hrrr'])])).toEqual(ALL);
    const asked = [answer(-89, 25, ['hrrr']), answer(-85, 29, ['hrdps', 'hrrr'])];
    expect(creditedSources(ALL, stack, sources, gulf, 'temp', asked)).toEqual(ALL);
  });
});
