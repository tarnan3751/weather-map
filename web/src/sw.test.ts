// Runs public/sw.js in a sandbox with fake clients, to check where a tapped
// notification takes the user.
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { describe, expect, it } from 'vitest';

interface FakeClient {
  url: string;
  focused: boolean;
  messages: unknown[];
  postMessage(m: unknown): void;
  focus(): Promise<FakeClient>;
  navigate(url: string): Promise<FakeClient | null>;
}

function client(url: string, controlled = true): FakeClient {
  const c: FakeClient = {
    url,
    focused: false,
    messages: [],
    postMessage: (m) => void c.messages.push(m),
    focus: async () => {
      c.focused = true;
      return c;
    },
    navigate: async (to) => {
      if (!controlled) throw new TypeError('not controlled');
      c.url = to;
      return c;
    },
  };
  return c;
}

async function click(windows: FakeClient[]) {
  const handlers: Record<string, (e: unknown) => void> = {};
  const opened: string[] = [];
  const self = {
    location: { origin: 'https://wx.example' },
    addEventListener: (t: string, fn: (e: unknown) => void) => (handlers[t] = fn),
    clients: {
      matchAll: async () => windows,
      openWindow: async (u: string) => void opened.push(u),
      claim: async () => {},
    },
    registration: { showNotification: async () => {} },
    skipWaiting: () => {},
  };
  runInNewContext(readFileSync(new URL('../public/sw.js', import.meta.url), 'utf8'), { self, URL });
  let done: Promise<unknown> = Promise.resolve();
  handlers.notificationclick({
    notification: { close() {}, data: { url: '/?alert=urn%3Aa&lat=39.1&lon=-94.6' } },
    waitUntil: (p: Promise<unknown>) => (done = p),
  });
  await done;
  return opened;
}

const TARGET = 'https://wx.example/?alert=urn%3Aa&lat=39.1&lon=-94.6';

describe('tapping a notification', () => {
  it('sends an open map to the alert', async () => {
    const status = client('https://wx.example/status.html');
    const map = client('https://wx.example/?layer=radar');
    const opened = await click([status, map]);
    expect(map.messages).toEqual([{ type: 'open-alert', url: TARGET }]);
    expect(map.focused).toBe(true);
    expect(status.focused).toBe(false);
    expect(opened).toEqual([]);
  });

  it('turns the status page into the map at the alert when no map is open', async () => {
    const status = client('https://wx.example/status.html');
    const opened = await click([status]);
    expect(status.url).toBe(TARGET);
    expect(status.focused).toBe(true);
    expect(opened).toEqual([]);
  });

  it('opens a window when nothing of ours is open, or cannot be moved', async () => {
    expect(await click([])).toEqual([TARGET]);
    expect(await click([client('https://wx.example/status.html', false)])).toEqual([TARGET]);
    expect(await click([client('https://elsewhere.example/')])).toEqual([TARGET]);
  });
});
