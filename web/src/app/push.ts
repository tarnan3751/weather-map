// Web Push for warnings over saved places: what this browser can do, and the
// register → permission → subscribe → tell the server flow.

import { push as pushApi } from '../api/device.ts';

export type PushBlocker = 'insecure' | 'unsupported' | 'ios-needs-home-screen' | 'ios-too-old' | 'no-server-key' | 'denied';

export function isIos(): boolean {
  const ua = navigator.userAgent;
  return /iPad|iPhone|iPod/.test(ua) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
}

function isStandalone(): boolean {
  return matchMedia('(display-mode: standalone)').matches || (navigator as Navigator & { standalone?: boolean }).standalone === true;
}

function iosVersion(): number | null {
  const m = /OS (\d+)_(\d+)/.exec(navigator.userAgent);
  return m ? Number(m[1]) + Number(m[2]) / 100 : null;
}

/** Why push cannot work here, or null if it can. */
export function pushBlocker(vapidKey: string | null): PushBlocker | null {
  if (!window.isSecureContext) return 'insecure';
  if (isIos()) {
    const v = iosVersion();
    if (v !== null && v < 16.04) return 'ios-too-old';
    // iOS only offers Web Push to sites added to the Home Screen.
    if (!isStandalone()) return 'ios-needs-home-screen';
  }
  if (!('serviceWorker' in navigator) || !('PushManager' in window) || !('Notification' in window)) return 'unsupported';
  if (!vapidKey) return 'no-server-key';
  if (Notification.permission === 'denied') return 'denied';
  return null;
}

export function urlBase64ToBytes(b64: string): Uint8Array<ArrayBuffer> {
  const pad = '='.repeat((4 - (b64.length % 4)) % 4);
  const raw = atob((b64 + pad).replace(/-/g, '+').replace(/_/g, '/'));
  const out = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

export async function registerServiceWorker(): Promise<ServiceWorkerRegistration | null> {
  if (!('serviceWorker' in navigator) || !window.isSecureContext) return null;
  try {
    return await navigator.serviceWorker.register('/sw.js', { scope: '/' });
  } catch {
    return null;
  }
}

/** The parts of the Push API used here, so tests can stand in for them. */
export interface SubscriptionLike {
  readonly options?: { readonly applicationServerKey?: ArrayBuffer | null };
  toJSON(): PushSubscriptionJSON;
  unsubscribe(): Promise<boolean>;
}
interface PushManagerLike {
  getSubscription(): Promise<SubscriptionLike | null>;
  subscribe(opts: { userVisibleOnly: boolean; applicationServerKey: Uint8Array<ArrayBuffer> }): Promise<SubscriptionLike>;
}

export function sameKey(a: ArrayBuffer | null | undefined, b: Uint8Array): boolean {
  if (!a) return false;
  const x = new Uint8Array(a);
  return x.length === b.length && x.every((v, i) => v === b[i]);
}

/**
 * This browser's subscription for `vapidKey`. One made with a different key
 * (the server's key changed) is useless, since every push for it would be
 * rejected, so it is dropped and, when `create` is set or it existed, made
 * again with the current key.
 */
export async function subscriptionFor(pm: PushManagerLike, vapidKey: string, create: boolean): Promise<SubscriptionLike | null> {
  const key = urlBase64ToBytes(vapidKey);
  let sub = await pm.getSubscription();
  if (sub && !sameKey(sub.options?.applicationServerKey, key)) {
    await sub.unsubscribe().catch(() => false);
    sub = null;
    create = true;
  }
  if (!sub && create) sub = await pm.subscribe({ userVisibleOnly: true, applicationServerKey: key });
  return sub;
}

export interface ResyncDeps {
  permission(): NotificationPermission;
  pushManager(): Promise<PushManagerLike | null>;
  post(sub: PushSubscriptionJSON): Promise<void>;
}

/** The app's service worker registration, if there is one. */
const registration = async () => ('serviceWorker' in navigator ? navigator.serviceWorker.getRegistration('/') : undefined);

const browserDeps: ResyncDeps = {
  permission: () => ('Notification' in window ? Notification.permission : 'denied'),
  pushManager: async () => ((await registration())?.pushManager as unknown as PushManagerLike | undefined) ?? null,
  post: (sub) => pushApi.subscribe(sub),
};

let resyncing: Promise<void> | null = null;

/**
 * Tell the server about this browser's subscription again (the endpoint is an
 * upsert). The server can forget it: a 401 makes the app register a new
 * device, or its database was reset, or its key changed. Only acts when
 * notifications are allowed and a subscription exists; it never asks.
 */
export function resyncPush(vapidKey: string | null, deps: ResyncDeps = browserDeps): Promise<void> {
  if (resyncing) return resyncing;
  if (!vapidKey || deps.permission() !== 'granted') return Promise.resolve();
  resyncing = (async () => {
    try {
      const pm = await deps.pushManager();
      if (!pm || !(await pm.getSubscription())) return;
      const sub = await subscriptionFor(pm, vapidKey, false);
      if (sub) await deps.post(sub.toJSON());
    } catch {
      // Best effort; the notifications panel shows the real state.
    } finally {
      resyncing = null;
    }
  })();
  return resyncing;
}

export async function currentSubscription(): Promise<PushSubscription | null> {
  return (await (await registration())?.pushManager.getSubscription()) ?? null;
}

export class PushError extends Error {
  readonly reason: 'denied' | 'dismissed' | 'subscribe' | 'server';
  constructor(reason: PushError['reason'], message: string) {
    super(message);
    this.reason = reason;
  }
}

/** Must run from a user gesture: browsers only show the permission prompt then. */
export async function enablePush(vapidKey: string): Promise<PushSubscription> {
  const reg = (await registerServiceWorker()) ?? (await navigator.serviceWorker.ready);
  const permission = await Notification.requestPermission();
  if (permission === 'denied') throw new PushError('denied', 'Notifications are blocked for this site.');
  if (permission !== 'granted') throw new PushError('dismissed', 'Permission was not granted.');
  await navigator.serviceWorker.ready;
  let sub: PushSubscription;
  try {
    // Reuse a subscription only if it was made with this server's key.
    sub = (await subscriptionFor(reg.pushManager as unknown as PushManagerLike, vapidKey, true)) as unknown as PushSubscription;
  } catch (e) {
    throw new PushError('subscribe', (e as Error).message || 'The browser could not subscribe to push.');
  }
  try {
    await pushApi.subscribe(sub.toJSON());
  } catch (e) {
    throw new PushError('server', (e as Error).message || 'The server did not accept the subscription.');
  }
  return sub;
}

export async function disablePush(): Promise<void> {
  const sub = await currentSubscription();
  if (!sub) return;
  try {
    await pushApi.unsubscribe(sub.endpoint);
  } finally {
    await sub.unsubscribe();
  }
}
