import { readFileSync } from 'node:fs';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { loadTile, retryAfterMs } from './load.ts';

const tiny = readFileSync(new URL('../../../fixtures/tiles/tiny.wxt', import.meta.url));
const answer = (status: number, body: BodyInit | null = null, headers: Record<string, string> = {}) =>
  vi.stubGlobal('fetch', async () => new Response(body, { status, headers }));
const load = () => loadTile('/t.wxt', [0], new AbortController().signal);

describe('one tile load', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('decodes a tile into its picked bands', async () => {
    answer(200, tiny);
    expect(await load()).toEqual({ status: 'ok', width: 3, height: 2, channels: 1, data: new Uint16Array([0, 65535, 1, 32768, 0, 65534]) });
  });

  it('maps the HTTP answer to what the store does next', async () => {
    answer(204);
    expect(await load()).toEqual({ status: 'empty' });
    answer(404);
    expect(await load()).toEqual({ status: 'missing' });
    answer(503, null, { 'retry-after': '7' });
    expect(await load()).toEqual({ status: 'error', retry: true, retryAfterMs: 7000 });
    answer(500);
    expect(await load()).toEqual({ status: 'error', retry: true });
    answer(400);
    expect(await load()).toEqual({ status: 'error', retry: false });
    answer(200, new Uint8Array([1, 2, 3]));
    expect(await load()).toEqual({ status: 'error', retry: false });
  });

  it('tells a cancelled load from a failed network', async () => {
    vi.stubGlobal('fetch', async () => Promise.reject(new TypeError('offline')));
    expect(await load()).toEqual({ status: 'error', retry: true });
    const ac = new AbortController();
    ac.abort();
    expect(await loadTile('/t.wxt', [0], ac.signal)).toEqual({ status: 'cancelled' });
  });

  it('reads Retry-After as seconds or as a date', () => {
    expect(retryAfterMs(null)).toBeUndefined();
    expect(retryAfterMs('0')).toBe(0);
    const later = retryAfterMs(new Date(Date.now() + 60_000).toUTCString())!;
    expect(later).toBeGreaterThan(55_000);
    expect(later).toBeLessThanOrEqual(60_000);
    expect(retryAfterMs('soon')).toBeUndefined();
  });
});
