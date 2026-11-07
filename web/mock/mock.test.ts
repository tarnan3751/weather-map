import { describe, expect, it } from 'vitest';
import type { Part } from '../src/api/types.ts';
import { tileBounds, tileParts } from '../src/tiles/stack.ts';
import { decodeWxt } from '../src/tiles/wxt.ts';
import { stackAcceptable, tileBox, tileParts as serverTileParts } from './boxes.ts';
import { ANCHOR_CYCLE_H, assemble, Catalog, forecastPart, SERVE_GRACE_MS } from './catalog.ts';
import { encodeWxt } from './encode.ts';
import { mercX, mercY } from './mvt.ts';
import { LAYERS, layerById, quantize } from './registry.ts';
import { SOURCES, sourceById, sourceDefs } from './sources.ts';
import { provenanceAt, renderTile } from './tiles.ts';

const MIN = 60_000;
const T0 = Date.UTC(2026, 10, 6, 12, 1);

/** The tile at zoom `z` holding a point. */
const tileAt = (z: number, lon: number, lat: number): [number, number, number] => [z, Math.floor(mercX(lon) * 2 ** z), Math.floor(mercY(lat) * 2 ** z)];

/** The parts of a layer's newest observed frame (or newest frame). */
function newestFrame(layer: string, now: number, kind: 'observed' | 'forecast' = 'observed') {
  const tl = assemble(layerById(layer)!, now);
  const byId = new Map(tl.parts.map((p) => [p.id, p]));
  const f = tl.frames.filter((x) => x.kind === kind).at(kind === 'observed' ? -1 : 0)!;
  return { frame: f, parts: f.parts.map((id) => byId.get(id)!), tl };
}

describe('mock backend', () => {
  it('encodes WXT1 that the client decodes back exactly', async () => {
    const n = 257 * 257;
    const a = new Uint16Array(n).map((_, i) => (i * 37) % 65536);
    const b = new Uint16Array(n).map((_, i) => (i % 13 === 0 ? 0 : 65535 - (i % 1000)));
    const tile = await decodeWxt(new Uint8Array(encodeWxt(257, 257, [a, b])));
    expect(tile.bands[0]).toEqual(a);
    expect(tile.bands[1]).toEqual(b);
  });

  it('quantizes like BandSpec::quantize', () => {
    const dbz = layerById('radar')!.bands[0];
    expect(quantize(dbz, Number.NaN)).toBe(0);
    expect(quantize(dbz, -30)).toBe(101);
    expect(quantize(dbz, -1e9)).toBe(1);
    expect(quantize(dbz, 1e9)).toBe(65535);
  });

  it('mirrors the registry shapes of the Rust tables', () => {
    expect(LAYERS.map((l) => l.id)).toEqual(['radar', 'precip', 'rain_1h', 'rain_24h', 'rain_72h', 'snow', 'temp', 'feels_like', 'dew_point', 'wind', 'clouds', 'satellite']);
    expect(layerById('satellite')).toMatchObject({ group: 'sky', palette: 'infrared', bands: [{ quantity: 'temperature' }] });
    expect(layerById('wind')!.render).toEqual({ kind: 'wind', u: 0, v: 1, gust: 2 });
    expect(layerById('precip')!.bands[1]).toMatchObject({ name: 'ptype', kind: 'categorical', scale: 1, offset: 0 });
    expect(layerById('snow')!.anchored).toBe(true);
    expect(LAYERS.filter((l) => l.anchored).map((l) => l.id)).toEqual(['snow']);
    // A global base under every layer.
    for (const l of LAYERS.filter((x) => x.id !== 'satellite')) expect(l.inputs.some((i) => i.family === 'gfs' || i.family === 'cmorph'), l.id).toBe(true);
    const defs = sourceDefs();
    expect(new Set(defs.map((d) => d.rank)).size).toBe(defs.length);
    expect(Object.keys(defs[0]).sort()).toEqual(['attribution', 'attribution_url', 'family', 'id', 'kind', 'licence', 'max_zoom', 'medium', 'provider', 'rank', 'region', 'res_km', 'title'].sort());
    expect(new Set(defs.map((d) => d.medium))).toEqual(new Set(['radar', 'satellite', 'analysis', 'nowcast', 'model']));
  });

  it('describes a believable world of sources', () => {
    const solid = SOURCES.filter((s) => s.solid && s.bounds.west !== -180);
    expect(solid.length).toBeGreaterThanOrEqual(2);
    // One regional grid crosses the antimeridian, solid box and all.
    const ak = sourceById('hrrr_ak')!;
    expect(ak.bounds.west).toBeGreaterThan(ak.bounds.east);
    expect(ak.solid!.west).toBeGreaterThan(ak.solid!.east);
    // Ragged radar has no solid box, and data only where its radars see.
    const mrms = sourceById('mrms')!;
    expect(mrms.solid).toBeNull();
    expect(mrms.weight(-98, 38)).toBe(1);
    expect(mrms.weight(-128, 52)).toBe(0);
    // Solid boxes are inside their grids, at full weight all the way.
    for (const s of SOURCES.filter((x) => x.solid && x.solid.west !== -180)) {
      const b = s.solid!;
      const east = b.east < b.west ? b.east + 360 : b.east;
      for (let i = 0; i <= 20; i++) {
        for (let j = 0; j <= 20; j++) {
          let lon = b.west + ((east - b.west) * i) / 20;
          if (lon > 180) lon -= 360;
          const lat = b.south + ((b.north - b.south) * j) / 20;
          expect(s.weight(lon, lat), `${s.id} at ${lon}, ${lat}`).toBe(1);
        }
      }
    }
  });
});

describe('the mock catalog', () => {
  it('holds the satellite estimate across radar ticks under fresh ground radar', () => {
    const { tl } = newestFrame('radar', T0);
    const obs = tl.frames.filter((f) => f.kind === 'observed');
    const top = obs.map((f) => f.parts[0]);
    expect(new Set(top).size).toBe(obs.length);
    // A part is kept while it can still stand in for a tick in the window,
    // so every past frame, the oldest included, stands on the estimate.
    const base = obs.map((f) => f.parts.at(-1)!);
    expect(base.every((id) => id.startsWith('cmorph2-'))).toBe(true);
    expect(new Set(base).size).toBeLessThan(obs.length / 5);
    // The window's ticks only: none older than the retention.
    expect(Date.parse(obs[0].valid)).toBeGreaterThanOrEqual(T0 - 120 * MIN);
    // Every part a frame names is described, and none is described twice.
    const ids = new Set(tl.parts.map((p) => p.id));
    expect(ids.size).toBe(tl.parts.length);
    for (const f of tl.frames) for (const id of f.parts) expect(ids.has(id)).toBe(true);
  });

  it('puts observations only up to the newest one, forecasts only after it', () => {
    const tl = assemble(layerById('radar')!, T0);
    const byId = new Map(tl.parts.map((p) => [p.id, p]));
    const lastObs = tl.frames.filter((f) => f.kind === 'observed').at(-1)!;
    for (const f of tl.frames) {
      const kinds = new Set(f.parts.map((id) => byId.get(id)!.kind));
      if (Date.parse(f.valid) > Date.parse(lastObs.valid)) expect([...kinds], f.valid).toEqual(['forecast']);
      else expect([...kinds], f.valid).toEqual(['observed']);
    }
    // The nowcast runs every ten minutes; ids of runs off the hour carry the minute.
    const nowcast = tl.parts.filter((p) => p.source === 'nowcast');
    expect(nowcast.length).toBeGreaterThan(0);
    expect(nowcast.every((p) => (Date.parse(p.run!) % 3600_000 === 0 ? /^nowcast-\d{8}-\d+$/ : /^nowcast-\d{10}-\d+$/).test(p.id))).toBe(true);
    expect(forecastPart(sourceById('nowcast')!, Date.UTC(2026, 10, 6, 11, 5), 30).id).toBe('nowcast-2611061105-30');
    expect(forecastPart(sourceById('hrrr')!, Date.UTC(2026, 10, 6, 11), 360).id).toBe('hrrr-26110611-360');
    // No observation stamped ahead of now.
    expect(tl.parts.filter((p) => p.kind === 'observed').every((p) => Date.parse(p.valid) <= T0 + 5 * MIN)).toBe(true);
  });

  it('keeps fill-past forecasts that still reach the window, and only the window’s ticks', () => {
    const layer = layerById('temp')!;
    const tl = assemble(layer, T0);
    const start = T0 - layer.retention_minutes * MIN;
    expect(Date.parse(tl.frames[0].valid)).toBeGreaterThanOrEqual(start);
    // The first tick has the forecasts standing in for it.
    const first = tl.frames[0].parts.map((id) => tl.parts.find((p) => p.id === id)!.source);
    expect(first).toContain('gfs');
  });

  it('cuts only ticks up to the newest observation at the window; forecast-only layers keep their grace', () => {
    // No observations and no window: the last 90 minutes of forecasts stay.
    const precip = assemble(layerById('precip')!, T0);
    const first = Date.parse(precip.frames[0].valid);
    expect(first).toBeLessThan(T0);
    expect(first).toBeGreaterThanOrEqual(T0 - 90 * MIN);
  });

  it('anchors snowfall on a 12-hourly run that every source joins', () => {
    const tl = assemble(layerById('snow')!, T0);
    const runs = new Set(tl.parts.map((p) => p.run));
    expect(runs.size).toBe(1);
    expect(Date.parse([...runs][0]!) % (ANCHOR_CYCLE_H * 3600_000)).toBe(0);
    expect(tl.frames.length).toBeGreaterThan(20);
    expect(new Set(tl.parts.map((p) => p.source))).toEqual(new Set(['hrrr', 'hrrr_ak', 'icon_eu', 'gfs']));
    // The next 12-hourly run moves the whole timeline on.
    const later = assemble(layerById('snow')!, T0 + 12 * 60 * MIN);
    expect([...new Set(later.parts.map((p) => p.run))]).not.toEqual([...runs]);
  });

  it('sends one timeline event per changed layer as the clock moves', () => {
    const c = new Catalog(T0);
    const events = c.advance(T0 + 2 * MIN);
    const radar = events.filter((e) => e.type === 'timeline' && e.layer === 'radar');
    expect(radar).toHaveLength(1);
    const ev = radar[0];
    if (ev.type !== 'timeline') throw new Error('not a timeline event');
    // A new radar time step, the oldest one gone, and only the new parts described.
    expect(ev.frames.some((f) => f.kind === 'observed')).toBe(true);
    expect(ev.removed.length).toBeGreaterThan(0);
    expect(ev.parts.every((p) => ['mrms', 'opera', 'cmorph2', 'nowcast'].includes(p.source))).toBe(true);
    for (const f of ev.frames) for (const id of f.parts) expect(c.timeline('radar').parts.some((p) => p.id === id)).toBe(true);
  });

  it('changes only the stacks a source update touches', () => {
    const before = newestFrame('radar', T0);
    const after = newestFrame('radar', T0 + 2 * MIN);
    expect(after.frame.valid).not.toBe(before.frame.valid);
    // Over Africa only the satellite estimate is there: the same stack, the
    // same tile. Over Kansas the ground radar is new: a new tile.
    const stack = (parts: Part[], t: [number, number, number]) => serverTileParts(parts, tileBox(...t)).map((p) => p.id).join(',');
    const africa = tileAt(4, 20, 5);
    const kansas = tileAt(6, -98, 38);
    expect(stack(after.parts, africa)).toBe(stack(before.parts, africa));
    expect(stack(after.parts, kansas)).not.toBe(stack(before.parts, kansas));
    // The client computes the same stacks.
    for (const t of [africa, kansas, tileAt(2, 179, 60), tileAt(5, -179.5, 60), [0, 0, 0] as [number, number, number]]) {
      expect(tileParts(after.parts, tileBounds(...t)).map((p) => p.id).join(',')).toBe(stack(after.parts, t));
    }
  });

  it('a new model run changes forecast stacks only where that model is', () => {
    const c = new Catalog(T0);
    const run = c.advance(T0 + 60 * MIN).find((e) => e.type === 'timeline' && e.layer === 'temp');
    if (!run || run.type !== 'timeline') throw new Error('no temp event');
    expect(run.parts.some((p) => p.source === 'hrrr')).toBe(true);
    const f = run.frames.find((x) => x.kind === 'forecast')!;
    const tl = c.timeline('temp');
    const parts = f.parts.map((id) => tl.parts.find((p) => p.id === id)!);
    // Europe's tiles still name only ICON-EU, whose run did not change.
    const europe = serverTileParts(parts, tileBox(...tileAt(5, 10, 50)));
    expect(europe.map((p) => p.source)).toEqual(['icon_eu']);
  });

  it('answers 404 the way the server does', () => {
    const c = new Catalog(T0);
    const now = T0;
    const { parts } = newestFrame('radar', now);
    const t = tileAt(6, -98, 38);
    const ids = serverTileParts(parts, tileBox(...t)).map((p) => p.id);
    expect(c.checkStack('radar', ...t, ids, now)).toBe('ok');
    // A stack no frame has: the parts out of order, or one missing.
    expect(c.checkStack('radar', ...t, [...ids].reverse(), now)).toBe('no_frame');
    expect(c.checkStack('radar', ...t, ids.slice(1), now)).toBe('no_frame');
    expect(c.checkStack('radar', ...t, ['mrms-0000000000'], now)).toBe('unknown_part');
    // Deeper than any of its parts goes.
    const deep = tileAt(8, -98, 38);
    const deepIds = serverTileParts(parts, tileBox(...deep)).map((p) => p.id);
    expect(c.checkStack('radar', ...deep, deepIds, now)).toBe('too_deep');
    // A satellite-only tile goes no deeper than the satellite does.
    const ocean = tileAt(3, -140, 0);
    const oceanIds = serverTileParts(parts, tileBox(...ocean)).map((p) => p.id);
    expect(oceanIds).toHaveLength(1);
    expect(c.checkStack('radar', ...ocean, oceanIds, now)).toBe('ok');
    expect(c.checkStack('radar', ...tileAt(4, -140, 0), oceanIds, now)).toBe('too_deep');
  });

  it('keeps a frame’s stacks fetchable for four minutes after it goes', () => {
    const c = new Catalog(T0);
    const oldest = c.frames('radar')[0];
    const tl = c.timeline('radar');
    const parts = oldest.parts.map((id) => tl.parts.find((p) => p.id === id)!);
    const t = tileAt(6, -98, 38);
    const ids = serverTileParts(parts, tileBox(...t)).map((p) => p.id);
    c.advance(T0 + 4 * MIN);
    expect(c.frames('radar')[0].valid).not.toBe(oldest.valid);
    expect(c.checkStack('radar', ...t, ids, T0 + 5 * MIN)).toBe('ok');
    c.advance(T0 + 4 * MIN + SERVE_GRACE_MS + 1);
    expect(c.checkStack('radar', ...t, ids, T0 + 4 * MIN + SERVE_GRACE_MS + 1)).not.toBe('ok');
  });

  it('accepts either answer for a part within a millionth of a degree of the edge', () => {
    const p = (id: string, west: number, east: number, solid: boolean) => ({
      id,
      bounds: { west, south: 10, east, north: 50 },
      solid: solid ? { west, south: 10, east, north: 50 } : null,
    });
    const gfs = { id: 'gfs', bounds: { west: -180, south: -90, east: 180, north: 90 }, solid: { west: -180, south: -90, east: 180, north: 90 } };
    // Tile 1/1/0 starts at 0 E; this part ends just inside it.
    const parts = [p('a', -10, 5e-7, false), gfs];
    expect(stackAcceptable(parts, 1, 1, 0, ['a', 'gfs'])).toBe(true);
    expect(stackAcceptable(parts, 1, 1, 0, ['gfs'])).toBe(true);
    expect(stackAcceptable(parts, 1, 1, 0, ['a'])).toBe(false);
    expect(stackAcceptable(parts, 1, 1, 0, ['gfs', 'a'])).toBe(false);
  });
});

describe('mock tiles', () => {
  it('renders a storm over Kansas City and no-echo around it', async () => {
    const { parts } = newestFrame('radar', Date.now());
    const layer = layerById('radar')!;
    // Tile 7/30/48 holds downtown Kansas City.
    const stack = serverTileParts(parts, tileBox(7, 30, 48));
    expect(stack[0].source).toBe('mrms');
    const bytes = renderTile(layer, stack, 7, 30, 48);
    expect(bytes).not.toBeNull();
    const t = await decodeWxt(new Uint8Array(bytes!));
    const q = Array.from(t.bands[0]);
    const dbz = (v: number) => -40 + (v - 1) * 0.1;
    expect(Math.max(...q.map(dbz))).toBeGreaterThan(60);
    expect(q.some((v) => Math.abs(dbz(v) + 30) < 1e-6)).toBe(true);
  });

  it('serves different bytes for different stacks of the same tile', () => {
    const layer = layerById('radar')!;
    const a = newestFrame('radar', T0).parts;
    const b = newestFrame('radar', T0 + 2 * MIN).parts;
    // Across the radar's northern edge: ground radar over the satellite estimate.
    const t: [number, number, number] = [4, 3, 5];
    const sa = serverTileParts(a, tileBox(...t));
    const sb = serverTileParts(b, tileBox(...t));
    expect(sa.map((p) => p.id)).not.toEqual(sb.map((p) => p.id));
    expect(renderTile(layer, sa, ...t)!.equals(renderTile(layer, sb, ...t)!)).toBe(false);
    // Composition: the top part alone differs from it over what lies beneath.
    expect(renderTile(layer, sa.slice(0, 1), ...t)!.equals(renderTile(layer, sa, ...t)!)).toBe(false);
  });

  it('draws a part that crosses the antimeridian on both sides of it', async () => {
    const layer = layerById('temp')!;
    const { parts } = newestFrame('temp', T0, 'forecast');
    // West of the line the solid box (from 179.5 E) does not hold the whole
    // tile, so GFS shows through at its edge; east of it, it does.
    for (const [t, sources] of [
      [tileAt(5, 179, 60), ['hrrr_ak', 'gfs']],
      [tileAt(5, -179, 60), ['hrrr_ak']],
    ] as [[number, number, number], string[]][]) {
      const stack = serverTileParts(parts, tileBox(...t));
      expect(stack.map((p) => p.source)).toEqual(sources);
      const tile = await decodeWxt(new Uint8Array(renderTile(layer, stack, ...t)!));
      expect(tile.bands[0].every((v) => v > 0)).toBe(true);
    }
  });

  it('says which parts make the value at a point, and how much of it', () => {
    const { parts } = newestFrame('radar', T0);
    // Deep inside the US radar: the radar alone.
    expect(provenanceAt(parts, -98, 38)).toEqual([{ id: parts[0].id, source: 'mrms', share: 1 }]);
    // Mid-Pacific: only the satellite estimate.
    expect(provenanceAt(parts, -150, 10).map((p) => p.source)).toEqual(['cmorph2']);
    // Across the radar's edge offshore: both, top first, shares summing to one.
    const edge = provenanceAt(parts, -125.75, 42);
    expect(edge.map((p) => p.source)).toEqual(['mrms', 'cmorph2']);
    expect(edge.reduce((a, p) => a + p.share, 0)).toBeCloseTo(1, 2);
    expect(provenanceAt([], 0, 0)).toEqual([]);
  });

  it('builds the satellite layer from the geostationary imagers round the world', () => {
    const { parts } = newestFrame('satellite', T0);
    expect(new Set(parts.map((p) => p.source))).toEqual(new Set(['goes19', 'goes18', 'mtg_i1', 'msg_iodc', 'himawari9']));
    // GOES-West's disc crosses the antimeridian.
    expect(sourceById('goes18')!.bounds.west).toBeGreaterThan(sourceById('goes18')!.bounds.east);
    // Where GOES-West's view gets slanted Himawari shows through; the poles have nothing.
    expect(provenanceAt(parts, 165, 10).map((p) => p.source)).toEqual(['goes18', 'himawari9']);
    expect(provenanceAt(parts, 180, 10).map((p) => p.source)).toEqual(['goes18']);
    expect(provenanceAt(parts, 0, 80)).toEqual([]);
    const tile = renderTile(layerById('satellite')!, serverTileParts(parts, tileBox(2, 1, 1)), 2, 1, 1);
    expect(tile).not.toBeNull();
  });
});
