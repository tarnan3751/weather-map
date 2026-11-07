import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LiveHandlers } from '../api/live.ts';
import type { AlertCollection, Config, Frame, LayerSpec, Part, Timeline, WsEvent } from '../api/types.ts';
import { LAYERS } from '../../mock/registry.ts';
import { sourceDefs } from '../../mock/sources.ts';
import { MIN } from '../core/timeline.ts';
import { stubStorage, T0, WORLD } from '../testing.ts';
import { DataController, type DataDeps } from './data.ts';
import { createAppStore } from './state.ts';

const iso = (ms: number) => new Date(ms).toISOString();
const part = (ms: number): Part => ({
  id: `mrms-${ms / MIN}`,
  source: 'mrms',
  valid: iso(ms),
  run: null,
  kind: 'observed',
  bounds: { west: -130, south: 20, east: -60, north: 55 },
  solid: null,
  max_zoom: 7,
});
/** The satellite estimate every radar frame here stands on. */
const SAT: Part = { ...part(T0 - 30 * MIN), id: 'cmorph2-1', source: 'cmorph2', bounds: WORLD, solid: WORLD, max_zoom: 4 };
const radar = (ms: number): Frame => ({ valid: iso(ms), kind: 'observed', parts: [part(ms).id, SAT.id] });
const partsOf = (frames: Frame[]) => [SAT, ...frames.map((f) => part(Date.parse(f.valid)))];
const event = (frames: Frame[], removed: Frame[] = [], parts = frames.map((f) => part(Date.parse(f.valid)))): WsEvent => ({
  type: 'timeline',
  layer: 'radar',
  parts,
  frames,
  removed: removed.map((f) => f.valid),
});
const config: Config = {
  tile_url: '/tiles/{layer}/{z}/{x}/{y}/{parts}.wxt',
  tile_cells: 256,
  basemap_url: null,
  vapid_public_key: null,
  server_time: iso(T0),
};

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const noAlerts = (revision = 1): AlertCollection => ({ type: 'FeatureCollection', revision, truncated: false, features: [] });

function harness(framesAnswers: (() => Promise<Timeline>)[], configAnswers?: (() => Promise<Config>)[]) {
  stubStorage();
  const store = createAppStore();
  let handlers: LiveHandlers | null = null;
  let f = 0;
  let c = 0;
  const fetched: string[] = [];
  const alertQueries: unknown[] = [];
  const deps: DataDeps = {
    api: {
      config: () => (configAnswers ? configAnswers[Math.min(c++, configAnswers.length - 1)]() : Promise.resolve(config)),
      layers: () => Promise.resolve(LAYERS as LayerSpec[]),
      sources: () => Promise.resolve(sourceDefs()),
      layerFrames: (layer: string) => {
        fetched.push(layer);
        return layer === 'radar' ? framesAnswers[Math.min(f++, framesAnswers.length - 1)]().then((t) => ({ layer, ...t })) : Promise.resolve({ layer, frames: [], parts: [] });
      },
      alerts: (q) => {
        alertQueries.push(q);
        return Promise.resolve(noAlerts(3));
      },
      alertTiles: () => Promise.resolve({ tilejson: '3.0.0', name: 'alerts', revision: 2, tiles: [], minzoom: 0, maxzoom: 12, attribution: 'credit', vector_layers: [] }),
      alertDetail: () => Promise.reject(new Error('not used')),
      stations: () => Promise.resolve({ type: 'FeatureCollection', sources: {}, features: [] }),
    },
    listPlaces: () => Promise.resolve([]),
    hasDevice: () => false,
    createLive: (h) => {
      handlers = h;
      return { start() {}, stop() {} };
    },
  };
  const data = new DataController(store, {}, deps);
  return { store, data, fetched, alertQueries, live: () => handlers!, radarIds: () => (store.get().timelines.radar?.frames ?? []).map((x) => x.valid) };
}

const snapshot = (frames: Frame[]): Timeline => ({ frames, parts: partsOf(frames) });
const settle = async () => {
  for (let i = 0; i < 20; i++) await Promise.resolve();
};

describe('data controller', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('keeps events that arrive while a resync snapshot is on its way', async () => {
    const a = radar(T0 - 4 * MIN);
    const b = radar(T0 - 2 * MIN);
    const c = radar(T0);
    const resync = deferred<Timeline>();
    const h = harness([() => Promise.resolve(snapshot([a, b])), () => resync.promise]);
    h.data.start();
    await settle();
    expect(h.radarIds()).toEqual([a.valid, b.valid]);
    expect(h.store.get().sources.mrms?.attribution).toBe('NOAA/NSSL MRMS');

    h.live().onResync();
    // While the request is out: a new frame, and the oldest goes.
    h.live().onEvent(event([c]));
    h.live().onEvent(event([], [a], []));
    // The snapshot was taken before both.
    resync.resolve(snapshot([a, b]));
    await settle();
    expect(h.radarIds()).toEqual([b.valid, c.valid]);
    const tl = h.store.get().timelines.radar;
    expect(tl.frames[1].stack.map((p) => p.id)).toEqual([part(T0).id, SAT.id]);
    // The part only the removed frame named is forgotten.
    expect(tl.parts.has(part(T0 - 4 * MIN).id)).toBe(false);
  });

  it('applies a source update to the time steps it changes only', async () => {
    const a = radar(T0 - 2 * MIN);
    const b = radar(T0);
    const h = harness([() => Promise.resolve(snapshot([a, b]))]);
    h.data.start();
    await settle();
    const before = h.store.get().timelines.radar.frames;
    // A newer satellite estimate replaces the one under the newest frame.
    const sat2 = { ...SAT, id: 'cmorph2-2' };
    h.live().onEvent(event([{ ...b, parts: [part(T0).id, sat2.id] }], [], [sat2]));
    const after = h.store.get().timelines.radar.frames;
    expect(after[0]).toBe(before[0]);
    expect(after[1]).not.toBe(before[1]);
    expect(after[1].stack.map((p) => p.id)).toEqual([part(T0).id, 'cmorph2-2']);
  });

  it('fetches everything again when a frame names a part nobody described', async () => {
    const h = harness([() => Promise.resolve(snapshot([radar(T0)])), () => Promise.resolve(snapshot([radar(T0), radar(T0 + 2 * MIN)]))]);
    h.data.start();
    await settle();
    h.live().onEvent(event([radar(T0 + 2 * MIN)], [], []));
    await settle();
    expect(h.radarIds()).toEqual([radar(T0).valid, radar(T0 + 2 * MIN).valid]);
    expect(h.store.get().timelines.radar.unknown).toBe(0);
  });

  it('replays events held during the first load', async () => {
    const a = radar(T0 - 2 * MIN);
    const b = radar(T0);
    const first = deferred<Timeline>();
    const h = harness([() => first.promise]);
    h.data.start();
    h.live().onEvent(event([b]));
    first.resolve(snapshot([a]));
    await settle();
    expect(h.radarIds()).toEqual([a.valid, b.valid]);
  });

  it('retries a failed first load by itself, sooner when the socket opens', async () => {
    const fail = () => Promise.reject(Object.assign(new Error('down'), { status: 0 }));
    const h = harness([() => Promise.resolve(snapshot([radar(T0)]))], [fail, fail, () => Promise.resolve(config)]);
    h.data.start();
    await settle();
    expect(h.store.get().error).toMatch(/could not be reached/);
    expect(h.data.isBooted).toBe(false);
    // Backoff: nothing for a moment, then a second try that fails too.
    await vi.advanceTimersByTimeAsync(2500);
    await settle();
    expect(h.data.isBooted).toBe(false);
    // The socket opening is a sign the server is back: try straight away.
    h.live().onOpen?.();
    await settle();
    expect(h.data.isBooted).toBe(true);
    expect(h.store.get().error).toBeNull();
    expect(h.radarIds()).toHaveLength(1);
  });

  it('does not let the held events grow without bound', async () => {
    const never = deferred<Timeline>();
    const h = harness([() => never.promise]);
    h.data.start();
    for (let i = 0; i < 12_000; i++) h.live().onEvent({ type: 'stations', updated: iso(T0), count: i } as WsEvent);
    // Private, but this is the point of the test.
    const held = (h.data as unknown as { buffer: WsEvent[] | null }).buffer;
    expect(held!.length).toBeLessThanOrEqual(5000);
  });

  it('keeps the clock offset fresh from heartbeats', async () => {
    const h = harness([() => Promise.resolve(snapshot([radar(T0)]))]);
    h.data.start();
    await settle();
    const now = Date.now();
    h.live().onEvent({ type: 'heartbeat', server_time: new Date(now + 90_000).toISOString().replace('Z', '123Z') });
    expect(Math.abs(h.store.get().clockOffset - 90_000)).toBeLessThan(1000);
  });

  it('asks for the shown layer’s timeline alongside the registries, not after them', async () => {
    const answer = deferred<Config>();
    const h = harness([() => Promise.resolve(snapshot([radar(T0)]))], [() => answer.promise]);
    h.data.start();
    await settle();
    expect(h.fetched).toEqual(['radar']);
    expect(h.data.isBooted).toBe(false);
    answer.resolve(config);
    await settle();
    expect(h.data.isBooted).toBe(true);
    expect(h.fetched).toEqual(['radar']);
    expect(h.radarIds()).toHaveLength(1);
  });

  it('loads the first layer offered when the one shown last is gone', async () => {
    const h = harness([() => Promise.resolve(snapshot([radar(T0)]))]);
    h.store.set({ layerId: 'retired' });
    h.data.start();
    await settle();
    expect(h.store.get().layerId).toBe(LAYERS[0].id);
    expect(h.fetched).toEqual(['retired', LAYERS[0].id]);
    expect(Object.keys(h.store.get().timelines)).toEqual([LAYERS[0].id]);
  });

  it('loads the active layer first and the others only when shown', async () => {
    const h = harness([() => Promise.resolve(snapshot([radar(T0)]))]);
    h.data.start();
    await settle();
    expect(h.fetched).toEqual(['radar']);
    expect(Object.keys(h.store.get().timelines)).toEqual(['radar']);
    // A layer never loaded ignores its events.
    h.live().onEvent({ type: 'timeline', layer: 'temp', parts: [], frames: [{ valid: iso(T0), kind: 'observed', parts: [] }], removed: [] });
    expect(h.store.get().timelines.temp).toBeUndefined();
    h.store.set({ layerId: 'temp' });
    await settle();
    expect(h.fetched).toEqual(['radar', 'temp']);
    expect(h.store.get().timelines.temp?.frames).toEqual([]);
    // Back again: nothing is fetched twice.
    h.store.set({ layerId: 'radar' });
    h.store.set({ layerId: 'temp' });
    await settle();
    expect(h.fetched).toEqual(['radar', 'temp']);
    // A reconnect refreshes every layer held, and only those.
    h.live().onResync();
    await settle();
    expect(h.fetched.slice(2).sort()).toEqual(['radar', 'temp']);
  });

  it('follows the alert set’s revision and asks for the alerts in view', async () => {
    const h = harness([() => Promise.resolve(snapshot([radar(T0)]))]);
    h.data.start();
    await settle();
    // From tiles.json at boot.
    expect(h.store.get().alertsRevision).toBe(2);
    expect(h.store.get().alertsAttribution).toBe('credit');
    // No view yet, no list; then the view's box, debounced.
    expect(h.alertQueries).toHaveLength(0);
    h.store.set({ viewBox: { west: 170, south: 40, east: -150, north: 70 } });
    await vi.advanceTimersByTimeAsync(400);
    expect(h.alertQueries).toEqual([{ bbox: { west: 170, south: 40, east: -150, north: 70 }, unshaped: true, limit: 500 }]);
    expect(h.store.get().alertsRevision).toBe(3);
    // A newer revision announced live is taken at once, and the list fetched
    // again: with no ids, too, which means a shapeless alert came or went.
    h.live().onEvent({ type: 'alerts', revision: 9, added: [], updated: [], removed: [] });
    await settle();
    expect(h.store.get().alertsRevision).toBe(9);
    expect(h.alertQueries).toHaveLength(2);
    // An event not newer than what is held is old news: nothing is refetched.
    h.live().onEvent({ type: 'alerts', revision: 9, added: ['x'], updated: [], removed: [] });
    h.live().onEvent({ type: 'alerts', revision: 5, added: [], updated: [], removed: [] });
    await settle();
    expect(h.store.get().alertsRevision).toBe(9);
    expect(h.alertQueries).toHaveLength(2);
    // Revisions are large and not consecutive.
    h.live().onEvent({ type: 'alerts', revision: 1_793_990_045_123, added: [], updated: [], removed: [] });
    await settle();
    expect(h.store.get().alertsRevision).toBe(1_793_990_045_123);
    expect(h.alertQueries).toHaveLength(3);
  });
});
