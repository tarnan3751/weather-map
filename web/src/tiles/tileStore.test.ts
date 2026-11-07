import { describe, expect, it } from 'vitest';
import { box, part, WORLD } from '../testing.ts';
import { FrameTiles, planCells } from './plan.ts';
import { latToY } from './tileMath.ts';
import { type LoadResult, type TileRequest, TileStore } from './tileStore.ts';

function harness(maxInFlight = 2, budgetBytes = 10_000, estimateBytes = 1000) {
  const calls: { url: string; resolve: (r: LoadResult) => void; signal: AbortSignal }[] = [];
  const store = new TileStore(
    (url, _pick, signal) =>
      new Promise<LoadResult>((resolve) => {
        calls.push({ url, resolve, signal });
        signal.addEventListener('abort', () => resolve({ status: 'cancelled' }));
      }),
    { budgetBytes, maxInFlight, estimateBytes: () => estimateBytes },
  );
  let updates = 0;
  store.onUpdate = () => updates++;
  const url = (x: number, stack = 'f1') => `/tiles/radar/4/${x}/6/${stack}.wxt`;
  const req = (x: number, stack = 'f1'): TileRequest => ({ url: url(x, stack), tile: { z: 4, x, y: 6 }, pick: [0] });
  const ok = (bytes = 1000): LoadResult => ({ status: 'ok', width: 2, height: 2, channels: 1, data: new Uint16Array(bytes / 2) });
  const flush = () => new Promise((r) => setTimeout(r, 0));
  return { store, calls, req, url, ok, flush, updates: () => updates };
}

describe('tile store', () => {
  it('loads in priority order up to the concurrency limit', () => {
    const h = harness(2);
    h.store.want([h.req(1), h.req(2), h.req(3)]);
    expect(h.calls.map((c) => c.url)).toEqual([h.url(1), h.url(2)]);
    expect(h.store.inFlight).toBe(2);
  });

  it('cancels loads that are no longer wanted', async () => {
    const h = harness(2);
    h.store.want([h.req(1), h.req(2)]);
    h.store.want([h.req(3)]);
    expect(h.calls[0].signal.aborted).toBe(true);
    expect(h.calls[1].signal.aborted).toBe(true);
    expect(h.calls[2].url).toContain('/4/3/6/');
    await h.flush();
    expect(h.store.state(h.url(1))).toBeUndefined();
  });

  it('records ready, empty (204) and missing (404) tiles quietly', async () => {
    const h = harness(3);
    h.store.want([h.req(1), h.req(2), h.req(3)]);
    h.calls[0].resolve(h.ok());
    h.calls[1].resolve({ status: 'empty' });
    h.calls[2].resolve({ status: 'missing' });
    await h.flush();
    expect(h.store.state(h.url(1))).toBe('ready');
    expect(h.store.get(h.url(1))?.data?.length).toBe(500);
    expect(h.store.state(h.url(2))).toBe('empty');
    expect(h.store.state(h.url(3))).toBe('missing');
    expect(h.updates()).toBe(3);
    // Settled tiles are not fetched again.
    h.store.want([h.req(1), h.req(2), h.req(3)]);
    expect(h.calls).toHaveLength(3);
  });

  it('retries a failed load later, not immediately', async () => {
    const h = harness(1);
    h.store.want([h.req(1)]);
    h.calls[0].resolve({ status: 'error', retry: true });
    await h.flush();
    expect(h.store.state(h.url(1))).toBe('error');
    h.store.want([h.req(1)]);
    expect(h.calls).toHaveLength(1);
    h.store.want([h.req(1)], performance.now() + 5000);
    expect(h.calls).toHaveLength(2);
  });

  it('evicts least recently used tiles over budget once unwanted, never protected ones', async () => {
    const h = harness(10, 2500);
    h.store.want([h.req(1), h.req(2), h.req(3)], 1);
    h.calls.forEach((c) => c.resolve(h.ok(1000)));
    await h.flush();
    expect(h.store.usedBytes).toBe(3000);
    // The view moved on: nothing is wanted now.
    h.store.want([], 2);
    h.store.touch([h.url(1)], 5);
    h.store.evict(new Set([h.url(2)]));
    expect(h.store.usedBytes).toBeLessThanOrEqual(2500);
    expect(h.store.state(h.url(3))).toBeUndefined();
    expect(h.store.state(h.url(2))).toBe('ready');
  });

  it('fetches a tile once however many frames name its stack', async () => {
    const h = harness(4);
    // Two frames want the same stack at x = 1 and different ones at x = 2.
    h.store.want([h.req(1, 'mrms-1,cmorph2-1'), h.req(2, 'mrms-1,cmorph2-1'), h.req(1, 'mrms-1,cmorph2-1'), h.req(2, 'mrms-2,cmorph2-1')]);
    expect(h.calls.map((c) => c.url)).toEqual([h.url(1, 'mrms-1,cmorph2-1'), h.url(2, 'mrms-1,cmorph2-1'), h.url(2, 'mrms-2,cmorph2-1')]);
    h.calls.forEach((c) => c.resolve(h.ok()));
    await h.flush();
    expect(h.store.size).toBe(3);
    expect(h.store.stats.started).toBe(3);
  });

  it('settles on an idle view that does not fit the budget: no refetching', async () => {
    // Budget for three 1000-byte tiles; the view and its prefetch want eight.
    const h = harness(8, 3500);
    const reqs = [1, 2, 3, 4, 5, 6, 7, 8].map((x) => h.req(x));
    const drawn = new Set([h.url(1), h.url(2)]);
    const settle = async (round: number) => {
      h.store.want(reqs, round, { required: 2 });
      for (const c of h.calls) if (!c.signal.aborted) c.resolve(h.ok(1000));
      await h.flush();
      h.store.evict(drawn);
    };
    for (let round = 0; round < 10; round++) await settle(round);
    const settled = h.calls.length;
    for (let round = 10; round < 30; round++) await settle(round);
    expect(h.calls.length - settled).toBe(0);
    expect(settled).toBeLessThanOrEqual(4);
    expect(h.store.state(h.url(1))).toBe('ready');
    expect(h.store.state(h.url(2))).toBe('ready');
  });

  it('keeps what it can of a playing loop too big for the budget', async () => {
    // Twelve frames of one tile each, room for eight, the next three wanted.
    const loads = async (ahead: boolean) => {
      const h = harness(8, 8500);
      const frames = Array.from({ length: 12 }, (_, x) => h.req(x));
      let n = 0;
      for (let step = 0; step < 36; step++) {
        const i = step % 12;
        const before = h.calls.length;
        h.store.want([0, 1, 2].map((k) => frames[(i + k) % 12]), step, { required: 1 });
        for (const c of h.calls) c.resolve(h.ok(1000));
        await h.flush();
        h.store.evict(new Set([frames[i].url]), ahead ? () => new Map(frames.map((r, j) => [r.url, (j - i + 12) % 12])) : undefined);
        // The third time round.
        if (step >= 24) n += h.calls.length - before;
      }
      return n;
    };
    // Least recently used is exactly wrong for a loop: every frame is gone
    // by the time it comes round again.
    expect(await loads(false)).toBe(12);
    expect(await loads(true)).toBeLessThanOrEqual(8);
  });

  it('never evicts a tile that is still wanted', async () => {
    const h = harness(4, 1500);
    h.store.want([h.req(1), h.req(2)], 1, { required: 2 });
    h.calls.forEach((c) => c.resolve(h.ok(1000)));
    await h.flush();
    h.store.evict(new Set());
    expect(h.store.size).toBe(2);
  });

  it('retries a 404 later while the frame is on its timeline', async () => {
    const h = harness(1);
    h.store.want([h.req(1)], 0);
    h.calls[0].resolve({ status: 'missing' });
    await h.flush();
    expect(h.store.state(h.url(1))).toBe('missing');
    h.store.want([h.req(1)], 1000);
    expect(h.calls).toHaveLength(1);
    h.store.want([h.req(1)], performance.now() + 60_000);
    expect(h.calls).toHaveLength(2);
  });

  it('waits as long as a 503 asks before retrying', async () => {
    const h = harness(1);
    h.store.want([h.req(1)]);
    h.calls[0].resolve({ status: 'error', retry: true, retryAfterMs: 30_000 });
    await h.flush();
    h.store.want([h.req(1)], performance.now() + 10_000);
    expect(h.calls).toHaveLength(1);
    h.store.want([h.req(1)], performance.now() + 31_000);
    expect(h.calls).toHaveLength(2);
  });

  it('never retries a tile that cannot be decoded', async () => {
    const h = harness(1);
    h.store.want([h.req(1)]);
    h.calls[0].resolve({ status: 'error', retry: false });
    await h.flush();
    h.store.want([h.req(1)], performance.now() + 24 * 3600_000);
    expect(h.calls).toHaveLength(1);
  });

  it('asks again for an empty tile after an hour', async () => {
    const h = harness(1);
    h.store.want([h.req(1)]);
    h.calls[0].resolve({ status: 'empty' });
    await h.flush();
    h.store.want([h.req(1)], performance.now() + 10 * 60_000);
    expect(h.calls).toHaveLength(1);
    h.store.want([h.req(1)], performance.now() + 61 * 60_000);
    expect(h.calls).toHaveLength(2);
  });

  it('asked for the same tiles again, changes nothing until a retry is due', async () => {
    const h = harness(4);
    h.store.want([h.req(1), h.req(2)], 0);
    h.calls[0].resolve(h.ok());
    h.calls[1].resolve({ status: 'error', retry: true, retryAfterMs: 30_000 });
    await h.flush();
    const version = h.store.version;
    for (let t = 1; t < 50; t++) h.store.want([h.req(1), h.req(2)], performance.now() + t * 100);
    expect(h.store.version).toBe(version);
    expect(h.calls).toHaveLength(2);
    h.store.want([h.req(1), h.req(2)], performance.now() + 31_000);
    expect(h.calls).toHaveLength(3);
    expect(h.store.version).toBeGreaterThan(version);
  });

  it('starts a wanted load again after the fetcher gave up on it', async () => {
    const h = harness(4);
    h.store.want([h.req(1)]);
    h.calls[0].resolve({ status: 'cancelled' });
    await h.flush();
    expect(h.store.state(h.url(1))).toBeUndefined();
    h.store.want([h.req(1)]);
    expect(h.calls).toHaveLength(2);
  });

  it('lists the tiles waiting for a texture, and only those', async () => {
    const h = harness(4);
    h.store.want([h.req(1), h.req(2), h.req(3)]);
    h.calls[0].resolve(h.ok());
    h.calls[1].resolve(h.ok());
    h.calls[2].resolve({ status: 'empty' });
    await h.flush();
    const pending = () => [...h.store.pendingUploads()].map((e) => e.key);
    expect(pending()).toEqual([h.url(1), h.url(2)]);
    const version = h.store.version;
    h.store.setTexture(h.store.get(h.url(1))!, {} as WebGLTexture);
    expect(pending()).toEqual([h.url(2)]);
    // Textures are the renderer's: attaching one is not a change of state.
    expect(h.store.version).toBe(version);
    expect(h.store.usedBytes).toBe(3000);
    // A lost context takes every texture with it.
    h.store.forgetTextures();
    expect(pending()).toEqual([h.url(2), h.url(1)]);
    expect(h.store.usedBytes).toBe(2000);
  });

  it('starts the next queued load as soon as one finishes, without waiting for a frame', async () => {
    const h = harness(1);
    h.store.want([h.req(1), h.req(2), h.req(3)]);
    expect(h.calls).toHaveLength(1);
    h.calls[0].resolve(h.ok());
    await h.flush();
    expect(h.calls).toHaveLength(2);
    h.calls[1].resolve({ status: 'empty' });
    await h.flush();
    expect(h.calls).toHaveLength(3);
  });
});

describe('tiles shared across frames', () => {
  const US = box(-130, 20, -60, 55);
  // Two radar frames two minutes apart: new ground radar, the same satellite
  // estimate held across both ticks.
  const sat = part('cmorph2-2611061130', WORLD, WORLD, 4);
  const f1 = new FrameTiles([part('mrms-2611061204', US, null, 7), sat], (z, x, y, p) => `/tiles/radar/${z}/${x}/${y}/${p}.wxt`);
  const f2 = new FrameTiles([part('mrms-2611061206', US, null, 7), sat], (z, x, y, p) => `/tiles/radar/${z}/${x}/${y}/${p}.wxt`);
  const world = { x0: 0, x1: 1, y0: latToY(80), y1: latToY(-80) };

  it('names the same URL wherever only the held part reaches, and a new one where the radar is', () => {
    const urls = (f: FrameTiles) => planCells(world, 3, [f]).map((c) => c.leaves[0]!.url!);
    const a = urls(f1);
    const b = urls(f2);
    expect(a).toHaveLength(b.length);
    const same = a.filter((u, i) => u === b[i]);
    expect(same.length).toBeGreaterThan(0);
    expect(same.length).toBeLessThan(a.length);
    for (const u of same) expect(u).toMatch(/\/cmorph2-2611061130\.wxt$/);
    for (const u of a.filter((x, i) => x !== b[i])) expect(u).toContain('mrms-2611061204');
  });

  it('fetches a tile the next frame shares only once', async () => {
    const calls: string[] = [];
    const store = new TileStore(
      async (url) => {
        calls.push(url);
        return { status: 'ok', width: 2, height: 2, channels: 1, data: new Uint16Array(4) };
      },
      { budgetBytes: 1e9, maxInFlight: 100 },
    );
    const want = (f: FrameTiles) => store.want(planCells(world, 3, [f]).map((c) => ({ url: c.leaves[0]!.url!, tile: c.leaves[0]!, pick: [0] })));
    want(f1);
    await new Promise((r) => setTimeout(r, 0));
    const first = calls.length;
    want(f2);
    await new Promise((r) => setTimeout(r, 0));
    const again = calls.slice(first);
    // Only the tiles the new radar touches are fetched for the second frame.
    expect(again.length).toBeGreaterThan(0);
    expect(again.length).toBeLessThan(first);
    expect(again.every((u) => u.includes('mrms-2611061206'))).toBe(true);
    expect(new Set(calls).size).toBe(calls.length);
  });
});
