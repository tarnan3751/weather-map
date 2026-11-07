// The browser's device identity. There are no accounts: the app registers
// once, keeps the bearer token in localStorage, and registers again if the
// server stops recognising it. The token is a credential; it is never logged
// or shown.

import { ApiError, request } from './http.ts';
import type { Place } from './types.ts';

const KEY = 'wx.device';

interface Stored {
  device_id: string;
  token: string;
}

function load(): Stored | null {
  try {
    const raw = localStorage.getItem(KEY);
    const v = raw ? (JSON.parse(raw) as Stored) : null;
    return v && typeof v.token === 'string' ? v : null;
  } catch {
    return null;
  }
}

function save(v: Stored | null) {
  try {
    if (v) localStorage.setItem(KEY, JSON.stringify(v));
    else localStorage.removeItem(KEY);
  } catch {
    // Without storage the token lives for this page only.
  }
}

let current: Stored | null = load();
let registering: Promise<Stored> | null = null;
const listeners = new Set<() => void>();

/** Called after this browser gets a new device identity, so anything the
 *  server keeps per device (the push subscription) can be sent again. */
export function onDeviceRegistered(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** One registration at a time, however many requests ask for it. */
async function register(): Promise<Stored> {
  registering ??= request<Stored>('/api/device', { method: 'POST' })
    .then((v) => {
      current = v;
      save(v);
      for (const fn of listeners) fn();
      return v;
    })
    .finally(() => {
      registering = null;
    });
  return registering;
}

/** The server rejected `stale`. Replace it, unless another request already
 *  did: a second 401 for the same old token must not throw away the new one
 *  (and register a third device). */
async function replaceToken(stale: string): Promise<Stored> {
  if (current && current.token !== stale) return current;
  if (current) {
    current = null;
    save(null);
  }
  return register();
}

export function hasDevice(): boolean {
  return current !== null;
}

/** Call an authenticated endpoint, registering first if needed and once more
 *  if the server answers 401. */
async function authed<T>(path: string, init: RequestInit = {}): Promise<T> {
  const dev = current ?? (await register());
  const call = (token: string) =>
    request<T>(path, {
      ...init,
      headers: {
        ...init.headers,
        authorization: `Bearer ${token}`,
        ...(init.body ? { 'content-type': 'application/json' } : {}),
      },
    });
  try {
    return await call(dev.token);
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) {
      const fresh = await replaceToken(dev.token);
      return call(fresh.token);
    }
    throw e;
  }
}

export const places = {
  list: () => authed<{ places: Place[] }>('/api/places').then((r) => r.places),
  add: (p: { name: string; lat: number; lon: number; notify?: boolean }) => authed<Place>('/api/places', { method: 'POST', body: JSON.stringify(p) }),
  update: (id: string, patch: { name?: string; notify?: boolean }) =>
    authed<Place>(`/api/places/${encodeURIComponent(id)}`, { method: 'PATCH', body: JSON.stringify(patch) }),
  remove: (id: string) => authed<void>(`/api/places/${encodeURIComponent(id)}`, { method: 'DELETE' }),
};

export const push = {
  subscribe: (sub: PushSubscriptionJSON) => authed<void>('/api/push/subscription', { method: 'POST', body: JSON.stringify(sub) }),
  unsubscribe: (endpoint: string) => authed<void>('/api/push/subscription', { method: 'DELETE', body: JSON.stringify({ endpoint }) }),
  test: () => authed<{ sent: number; failed: number }>('/api/push/test', { method: 'POST' }),
};
