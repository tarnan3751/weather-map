import { describe, expect, it } from 'vitest';
import type { LayerSpec } from '../api/types.ts';
import type { TFrame } from '../core/timeline.ts';
import { part, WORLD } from '../testing.ts';
import { type LoadResult, TileStore } from '../tiles/tileStore.ts';
import { WeatherLayer } from './weatherLayer.ts';

const frame = (t: number, id: string): TFrame => ({ valid: new Date(t).toISOString(), kind: 'observed', parts: [id], t, stack: [part(id, WORLD, WORLD, 4)] });
const LAYER = { id: 'temp', render: { kind: 'scalar', band: 0 }, bands: [{ scale: 0.1, offset: -40 }] } as unknown as LayerSpec;

/** A WebGL context that accepts every call, counting them. */
function fakeGl() {
  const calls: Record<string, number> = {};
  const gl = new Proxy({} as Record<string, unknown>, {
    get: (_, k: string) => {
      if (/^[A-Z0-9_]+$/.test(k)) return 1;
      return (..._args: unknown[]) => {
        calls[k] = (calls[k] ?? 0) + 1;
        return k === 'getProgramParameter' ? 0 : {};
      };
    },
  });
  return { gl: gl as unknown as WebGL2RenderingContext, calls };
}

/** A map that only answers what the layer asks of it. */
function fakeMap(view = { lng: 0, lat: 20, zoom: 2 }) {
  return {
    view,
    getCenter: () => ({ lng: view.lng, lat: view.lat }),
    getZoom: () => view.zoom,
    getCanvas: () => ({ clientWidth: 800, clientHeight: 600 }),
    triggerRepaint: () => {},
    on: () => {},
    off: () => {},
  };
}

function harness() {
  const loads: { url: string; resolve: (r: LoadResult) => void }[] = [];
  const store = new TileStore((url) => new Promise((resolve) => loads.push({ url, resolve })), { budgetBytes: 1e9, maxInFlight: 1000 });
  const layer = new WeatherLayer(store, (l, z, x, y, parts) => `/tiles/${l}/${z}/${x}/${y}/${parts}.wxt`);
  const { gl, calls } = fakeGl();
  const map = fakeMap();
  layer.onAdd(map as never, gl);
  const wants: unknown[] = [];
  const want = store.want.bind(store);
  store.want = (reqs, now, opts) => {
    wants.push(reqs);
    return want(reqs, now, opts);
  };
  const args = { defaultProjectionData: { mainMatrix: new Float32Array(16) } } as never;
  const frameDrawn = () => {
    layer.prerender(gl);
    layer.render(gl, args);
  };
  const loadAll = async () => {
    for (const l of loads.splice(0)) l.resolve({ status: 'ok', width: 2, height: 2, channels: 1, data: new Uint16Array([1, 2, 3, 4]) });
    await new Promise((r) => setTimeout(r, 0));
  };
  return { store, layer, gl, calls, map, wants, loads, frameDrawn, loadAll };
}

describe('weather layer memoization', () => {
  const a = frame(0, 'gfs-1');
  const b = frame(3600_000, 'gfs-2');

  it('asks again for the very same requests while nothing changes', async () => {
    const h = harness();
    h.layer.setDisplay({ layer: LAYER, a, b: null, mix: 0, fallback: null, prefetch: [b] });
    h.frameDrawn();
    await h.loadAll();
    h.frameDrawn();
    h.frameDrawn();
    h.frameDrawn();
    // Rebuilt once the tiles came in (the store changed), then the same list,
    // however many textures were uploaded meanwhile.
    expect(h.wants.length).toBe(4);
    expect(h.wants[1]).not.toBe(h.wants[0]);
    expect(h.wants[2]).toBe(h.wants[1]);
    expect(h.wants[3]).toBe(h.wants[1]);
    // The display object may be new each tick (playback): equal frames and
    // prefetch keep the requests.
    h.layer.setDisplay({ layer: LAYER, a, b: null, mix: 0, fallback: null, prefetch: [b] });
    h.frameDrawn();
    expect(h.wants[4]).toBe(h.wants[3]);
  });

  it('plans again when the view or the frames change', async () => {
    const h = harness();
    h.layer.setDisplay({ layer: LAYER, a, b: null, mix: 0, fallback: null, prefetch: [] });
    h.frameDrawn();
    const first = h.layer.urlsInView();
    h.map.view.lng = 120;
    h.frameDrawn();
    expect(h.wants[1]).not.toBe(h.wants[0]);
    expect(h.layer.urlsInView()).not.toEqual(first);
    h.layer.setDisplay({ layer: LAYER, a: b, b: null, mix: 0, fallback: null, prefetch: [] });
    h.frameDrawn();
    expect(h.layer.urlsInView().every((u) => u.includes('gfs-2'))).toBe(true);
  });

  it('uploads each tile once, and redraws from the same quads until something changes', async () => {
    const h = harness();
    h.layer.setDisplay({ layer: LAYER, a, b, mix: 0, fallback: null, prefetch: [] });
    h.frameDrawn();
    expect(h.layer.stats.missing).toBeGreaterThan(0);
    await h.loadAll();
    // Prefetched tiles (b's, here) are uploaded a few per frame.
    while (!h.store.pendingUploads().next().done) h.frameDrawn();
    const quads = h.layer.quads(h.gl);
    expect(quads.length).toBe(h.layer.stats.slots);
    expect(h.layer.stats.exact).toBe(h.layer.stats.slots);
    // Every tile once, the palette and the placeholder for unused samplers.
    expect(h.calls.texImage2D).toBe(h.store.size + 2);
    h.frameDrawn();
    h.frameDrawn();
    expect(h.calls.texImage2D).toBe(h.store.size + 2);
    expect(h.layer.quads(h.gl)).toBe(quads);
    // A fade starting asks for the next frame's tiles and draws from both.
    h.layer.setDisplay({ layer: LAYER, a, b, mix: 0.3, fallback: null, prefetch: [] });
    h.frameDrawn();
    await h.loadAll();
    h.frameDrawn();
    const fading = h.layer.quads(h.gl);
    expect(fading).not.toBe(quads);
    expect(fading.every((q) => q.texB && q.mix === 0.3)).toBe(true);
    // Further along the fade: the same quads with the display's mix.
    h.layer.setDisplay({ layer: LAYER, a, b, mix: 0.7, fallback: null, prefetch: [] });
    h.frameDrawn();
    expect(h.layer.quads(h.gl)).toBe(fading);
    expect(fading.every((q) => q.mix === 0.7)).toBe(true);
  });
});
