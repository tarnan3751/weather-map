// Devices, saved places and push subscriptions, held in memory the way
// wx-alerts keeps them in Postgres: bearer tokens, at most 20 places each.

import { randomBytes, randomUUID } from 'node:crypto';
import type { Place } from '../src/api/types.ts';

/** A place as stored; its alerts are looked up when it is shown. */
export type StoredPlace = Omit<Place, 'alerts'>;

interface Device {
  id: string;
  places: Map<string, StoredPlace>;
  subscriptions: Map<string, { endpoint: string; keys: { p256dh: string; auth: string } }>;
  /** Hazards already notified, per place: once per event per place. */
  notified: Set<string>;
}

const MAX_PLACES = 20;

export class Devices {
  private byToken = new Map<string, Device>();

  register(): { device_id: string; token: string } {
    const token = randomBytes(32).toString('base64url');
    const device: Device = { id: randomUUID(), places: new Map(), subscriptions: new Map(), notified: new Set() };
    this.byToken.set(token, device);
    return { device_id: device.id, token };
  }

  device(authHeader: string | undefined): Device | null {
    const m = /^Bearer\s+(\S+)$/i.exec(authHeader ?? '');
    return m ? (this.byToken.get(m[1]) ?? null) : null;
  }

  addPlace(d: Device, body: { name?: unknown; lat?: unknown; lon?: unknown; notify?: unknown }): StoredPlace | 'full' | 'invalid' {
    const name = typeof body.name === 'string' ? body.name.trim().slice(0, 80) : '';
    const lat = Number(body.lat);
    const lon = Number(body.lon);
    if (!name || !Number.isFinite(lat) || !Number.isFinite(lon) || Math.abs(lat) > 90 || Math.abs(lon) > 180) return 'invalid';
    if (d.places.size >= MAX_PLACES) return 'full';
    const p: StoredPlace = { id: randomUUID(), name, lat, lon, notify: body.notify === undefined ? true : Boolean(body.notify), created_at: new Date().toISOString() };
    d.places.set(p.id, p);
    return p;
  }

  all(): Device[] {
    return [...this.byToken.values()];
  }
}
