import { describe, expect, it } from 'vitest';
import { resyncPush, type ResyncDeps, sameKey, subscriptionFor, type SubscriptionLike, urlBase64ToBytes } from './push.ts';

const KEY_A = 'BKiPa_5wm-pFdaFerD4O3x291MWH271UvcleTbNcOecjb7LE6ND1NEpfD0IzwckpFgm4z_Db2hwil96cPCr3P6M';
const KEY_B = 'BCIbkmUDdAQryr6vEf_WsEiCn6z8-0YQLT-1c_BbpePHmu2T8XNQeQ4VNBO0N1j46sSucjOv-ljmyhXL5BZ3oL8';

function fakePush(existingKey: string | null) {
  const log: string[] = [];
  let sub: SubscriptionLike | null = null;
  const make = (key: string): SubscriptionLike => ({
    options: { applicationServerKey: urlBase64ToBytes(key).buffer },
    toJSON: () => ({ endpoint: `https://push.example/${key.slice(0, 6)}`, keys: { p256dh: 'p', auth: 'a' } }),
    unsubscribe: async () => {
      log.push(`unsubscribe ${key.slice(0, 6)}`);
      sub = null;
      return true;
    },
  });
  if (existingKey) sub = make(existingKey);
  const pm = {
    getSubscription: async () => sub,
    subscribe: async (o: { applicationServerKey: Uint8Array }) => {
      const k = sameKey(o.applicationServerKey.buffer as ArrayBuffer, urlBase64ToBytes(KEY_A)) ? KEY_A : KEY_B;
      log.push(`subscribe ${k.slice(0, 6)}`);
      sub = make(k);
      return sub;
    },
  };
  const posted: string[] = [];
  const deps: ResyncDeps = { permission: () => 'granted', pushManager: async () => pm, post: async (s) => void posted.push(s.endpoint ?? '') };
  return { pm, log, posted, deps };
}

describe('push subscription', () => {
  it('compares application server keys', () => {
    expect(sameKey(urlBase64ToBytes(KEY_A).buffer, urlBase64ToBytes(KEY_A))).toBe(true);
    expect(sameKey(urlBase64ToBytes(KEY_B).buffer, urlBase64ToBytes(KEY_A))).toBe(false);
    expect(sameKey(null, urlBase64ToBytes(KEY_A))).toBe(false);
  });

  it('replaces a subscription made with an old server key', async () => {
    const f = fakePush(KEY_B);
    const sub = await subscriptionFor(f.pm, KEY_A, true);
    expect(f.log).toEqual(['unsubscribe BCIbkm', 'subscribe BKiPa_']);
    expect(sameKey(sub!.options!.applicationServerKey, urlBase64ToBytes(KEY_A))).toBe(true);
  });

  it('keeps a subscription made with the current key', async () => {
    const f = fakePush(KEY_A);
    await subscriptionFor(f.pm, KEY_A, true);
    expect(f.log).toEqual([]);
  });

  it('re-sends the subscription so a server that forgot it learns it again', async () => {
    const f = fakePush(KEY_A);
    await resyncPush(KEY_A, f.deps);
    expect(f.posted).toEqual(['https://push.example/BKiPa_']);
  });

  it('renews and re-sends after the server key changed', async () => {
    const f = fakePush(KEY_B);
    await resyncPush(KEY_A, f.deps);
    expect(f.log).toEqual(['unsubscribe BCIbkm', 'subscribe BKiPa_']);
    expect(f.posted).toEqual(['https://push.example/BKiPa_']);
  });

  it('does nothing when notifications are off or were never turned on', async () => {
    const none = fakePush(null);
    await resyncPush(KEY_A, none.deps);
    expect(none.log).toEqual([]);
    expect(none.posted).toEqual([]);
    const denied = fakePush(KEY_A);
    await resyncPush(KEY_A, { ...denied.deps, permission: () => 'denied' });
    expect(denied.posted).toEqual([]);
  });
});
