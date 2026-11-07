import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { stubStorage } from '../testing.ts';

/** A server that knows only the tokens it issued, and is slow on one path. */
function fakeServer(initialToken: string) {
  const issued = new Set<string>();
  let registrations = 0;
  const storage = stubStorage({ 'wx.device': JSON.stringify({ device_id: 'd0', token: initialToken }) });
  // The stale-token push test is answered only once the places request has
  // re-registered and succeeded: the ordering that can lose the new token.
  let placesOk: () => void = () => {};
  const placesDone = new Promise<void>((r) => (placesOk = r));
  vi.stubGlobal('fetch', async (path: string, init: RequestInit = {}) => {
    const auth0 = new Headers(init.headers).get('authorization') ?? '';
    if (path.includes('/push/test') && auth0 === `Bearer ${initialToken}`) await placesDone;
    await new Promise((r) => setTimeout(r, 1));
    if (path === '/api/device') {
      registrations++;
      const token = `T${registrations}`;
      issued.add(token);
      return new Response(JSON.stringify({ device_id: `d${registrations}`, token }), { status: 200 });
    }
    const auth = new Headers(init.headers).get('authorization') ?? '';
    if (!issued.has(auth.replace('Bearer ', ''))) return new Response('{"error":"unauthorized"}', { status: 401 });
    if (path === '/api/places') setTimeout(placesOk, 0);
    return new Response(path.includes('/push/test') ? '{"sent":1,"failed":0}' : '{"places":[]}', { status: 200 });
  });
  return { registrations: () => registrations, stored: () => JSON.parse(storage.getItem('wx.device') ?? 'null')?.token };
}

describe('device token', () => {
  beforeEach(() => vi.resetModules());
  afterEach(() => vi.unstubAllGlobals());

  it('two 401s in flight at once re-register once and keep the new token', async () => {
    const server = fakeServer('stale');
    const { places, push } = await import('./device.ts');
    // Both requests go out with the stale token; the second 401 lands after
    // the first has already registered again.
    const [list, test] = await Promise.all([places.list(), push.test()]);
    expect(list).toEqual([]);
    expect(test).toEqual({ sent: 1, failed: 0 });
    expect(server.registrations()).toBe(1);
    expect(server.stored()).toBe('T1');
  });

  it('tells listeners when it had to register again', async () => {
    fakeServer('stale');
    const { places, onDeviceRegistered } = await import('./device.ts');
    let told = 0;
    onDeviceRegistered(() => told++);
    await places.list();
    expect(told).toBe(1);
  });
});
