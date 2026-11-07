// The mock server's state: a clock, the frame catalog, alerts, devices, feed
// health and the open WebSocket clients, kept moving by a reconcile loop that
// emits the same live events the Rust server would.

import { generateKeyPairSync } from 'node:crypto';
import type { WebSocket } from 'ws';
import type { FeedHealth, FeedState, Part, SourceStatus, StatusReply, WsEvent } from '../src/api/types.ts';
import { Alerts } from './alerts.ts';
import { Catalog, feedFor, newestData } from './catalog.ts';
import { Devices } from './devices.ts';
import { setEpoch } from './fields.ts';
import { metarTime, stations } from './obs.ts';
import { LAYERS } from './registry.ts';
import { SOURCES, sourceById } from './sources.ts';
import { HOUR, MIN, iso } from './util.ts';

/** How long after a page loads the live Tornado Warning is issued. */
const LIVE_ALERT_DELAY_MS = 6_000;

function vapidPublicKey(): string {
  const jwk = generateKeyPairSync('ec', { namedCurve: 'prime256v1' }).publicKey.export({ format: 'jwk' });
  return Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x as string, 'base64url'), Buffer.from(jwk.y as string, 'base64url')]).toString('base64url');
}

export class World {
  private offset = 0;
  readonly startedAt: number;
  readonly catalog: Catalog;
  readonly alerts: Alerts;
  readonly devices = new Devices();
  readonly vapidKey = vapidPublicKey();
  readonly clients = new Set<WebSocket>();
  /** Connections told to go quiet (test hook): sent nothing at all. */
  readonly silenced = new WeakSet<WebSocket>();
  /** Tile requests answered 404, most recent last. */
  readonly tileMisses: { path: string; reason: string }[] = [];
  private feeds = new Map<string, FeedHealth>();
  private liveTimer: ReturnType<typeof setTimeout> | null = null;
  private loop: ReturnType<typeof setInterval> | null = null;
  private lastStations = 0;
  private readonly log: (msg: string) => void;

  constructor(log: (msg: string) => void) {
    this.log = log;
    this.startedAt = Date.now();
    setEpoch(this.startedAt);
    this.catalog = new Catalog(this.now());
    this.alerts = new Alerts(this.now());
    this.initFeeds();
  }

  now(): number {
    return Date.now() + this.offset;
  }

  start() {
    this.loop = setInterval(() => this.reconcile(), 5_000);
  }

  stop() {
    if (this.loop) clearInterval(this.loop);
    if (this.liveTimer) clearTimeout(this.liveTimer);
    for (const c of this.clients) c.terminate();
  }

  broadcast(ev: WsEvent) {
    const msg = JSON.stringify(ev);
    for (const c of this.clients) if (c.readyState === c.OPEN && !this.silenced.has(c)) c.send(msg);
  }

  /** Jump the mock clock forward, as if `minutes` had passed. */
  advance(minutes: number) {
    this.offset += minutes * MIN;
    this.reconcile();
  }

  reconcile() {
    const now = this.now();
    const events = this.catalog.advance(now);
    for (const ev of events) this.broadcast(ev);
    // A feed is healthy when its newest data arrives: the newest part each
    // source contributed to any layer.
    const newest = new Map<string, Part>();
    for (const e of events) {
      if (e.type !== 'timeline') continue;
      for (const p of e.parts) {
        const cur = newest.get(p.source);
        if (!cur || Date.parse(p.run ?? p.valid) > Date.parse(cur.run ?? cur.valid)) newest.set(p.source, p);
      }
    }
    for (const [source, p] of newest) {
      this.feedOk(source, Date.parse(p.run ?? p.valid), p.run ? `parts from the ${p.run.slice(11, 13)}Z run stored` : `${sourceById(source)?.title ?? source} data stored`);
    }
    for (const name of [...SOURCES.map((s) => s.id), 'alerts']) this.feedPoll(name, now);
    if (now - this.lastStations > 5 * MIN) {
      this.lastStations = now;
      const updated = metarTime(now);
      const count = stations(now, null).features.length;
      this.broadcast({ type: 'stations', updated: iso(updated), count });
      this.feedOk('stations', updated, `${count} stations reporting`);
    }
  }

  // ---------------------------------------------------------------- alerts

  /** A page just loaded: withdraw the live warning and issue it again shortly,
   *  so every load sees it arrive over the WebSocket. */
  scheduleLiveAlert() {
    if (this.liveTimer) clearTimeout(this.liveTimer);
    this.clearLiveAlert();
    this.liveTimer = setTimeout(() => this.issueLiveAlert(), LIVE_ALERT_DELAY_MS);
  }

  /** Withdraw the live warning, if it is showing, and say so. */
  clearLiveAlert() {
    const cleared = this.alerts.clearLive();
    if (cleared) this.broadcast({ type: 'alerts', revision: cleared.revision, added: [], updated: [], removed: cleared.removed });
  }

  issueLiveAlert() {
    this.liveTimer = null;
    const { added, revision, alert: a } = this.alerts.issueLive(this.now());
    this.broadcast({ type: 'alerts', revision, added, updated: [], removed: [] });
    this.feedOk('alerts', Date.parse(a.sent ?? ''), 'active alerts stored');
    for (const d of this.devices.all()) {
      for (const p of d.places.values()) {
        if (!p.notify || !this.alerts.at(p.lon, p.lat).some((x) => x.id === a.id)) continue;
        const key = `${a.id}|${p.id}`;
        if (d.notified.has(key)) continue;
        d.notified.add(key);
        // The mock cannot reach a push service; it reports what the server would send.
        this.log(`push: "${a.event}" for a saved place to ${d.subscriptions.size} subscription(s) (not delivered by the mock)`);
      }
    }
  }

  // ----------------------------------------------------------------- feeds

  private initFeeds() {
    const now = this.now();
    const mk = (name: string, interval: number, state: FeedState, detail: string, data: number | null): FeedHealth => ({
      name,
      state,
      expected_interval_secs: interval,
      started_at: iso(now - 3 * HOUR),
      last_success: state === 'healthy' ? iso(now - 20_000) : iso(now - 9 * MIN),
      last_data_time: data === null ? null : iso(data),
      detail,
      last_error: null,
      last_error_at: null,
      consecutive_failures: 0,
      restarts: 0,
      next_retry_at: null,
    });
    for (const src of SOURCES) {
      // Each source's newest data on the first layer it feeds.
      const layer = LAYERS.find((l) => feedFor(l, src));
      const feed = layer && feedFor(layer, src);
      if (!feed) continue;
      const interval = feed.kind === 'forecast' ? Math.min(1800, feed.everyMin * 10) : feed.everyMin * 60;
      const detail = feed.kind === 'forecast' ? 'parts from the latest run stored' : `${src.title} data stored`;
      this.feeds.set(src.id, mk(src.id, interval, 'healthy', detail, newestData(feed, now)));
    }
    this.feeds.set('alerts', mk('alerts', 30, 'healthy', '14 active alerts', now - 40 * MIN));
    // One feed starts unhealthy so the status page has something to show; it
    // recovers after a minute.
    this.feeds.set('stations', {
      ...mk('stations', 300, 'backing_off', 'fetching METARs', metarTime(now) - HOUR),
      last_error: 'aviationweather.gov: 503 Service Unavailable',
      last_error_at: iso(now - 40_000),
      consecutive_failures: 3,
      restarts: 3,
      next_retry_at: iso(now + 60_000),
    });
    this.lastStations = now - 4 * MIN;
  }

  private feedPoll(name: string, now: number) {
    const f = this.feeds.get(name);
    if (f && f.state === 'healthy') f.last_success = iso(now);
  }

  private feedOk(name: string, dataTime: number, detail: string) {
    const f = this.feeds.get(name);
    if (!f) return;
    const before = `${f.state}|${f.last_data_time}|${f.consecutive_failures}`;
    f.state = 'healthy';
    f.last_success = iso(this.now());
    f.consecutive_failures = 0;
    f.next_retry_at = null;
    if (Number.isFinite(dataTime)) {
      const prev = f.last_data_time ? Date.parse(f.last_data_time) : -Infinity;
      f.last_data_time = iso(Math.max(prev, dataTime));
    }
    f.detail = detail;
    if (before !== `${f.state}|${f.last_data_time}|${f.consecutive_failures}`) this.broadcast({ type: 'feed', feed: { ...f } });
  }

  status(): StatusReply {
    const now = this.now();
    return {
      server_time: iso(now),
      started_at: iso(this.startedAt),
      clients: this.clients.size,
      feeds: [...this.feeds.values()].map((f) => {
        const ref = f.last_success ?? f.started_at;
        const stale = ref ? now - Date.parse(ref) > Math.max(30, f.expected_interval_secs) * 3 * 1000 : false;
        return { ...f, stale };
      }),
      layers: LAYERS.map((l) => {
        const frames = this.catalog.frames(l.id);
        return {
          id: l.id,
          frames: frames.length,
          oldest_valid: frames[0]?.valid ?? null,
          newest_valid: frames.at(-1)?.valid ?? null,
          newest_observed: frames.filter((f) => f.kind === 'observed').at(-1)?.valid ?? null,
        };
      }),
      sources: this.sourceStatus(),
    };
  }

  /** What each source contributes across every layer, registry order, the
   *  silent ones included (as wx-server reports it). */
  private sourceStatus(): SourceStatus[] {
    const by = new Map<string, { parts: Set<string>; layers: Set<string>; valid: number; run: number }>();
    for (const l of LAYERS) {
      for (const p of this.catalog.timeline(l.id).parts) {
        const e = by.get(p.source) ?? { parts: new Set(), layers: new Set(), valid: -Infinity, run: -Infinity };
        e.parts.add(`${l.id}/${p.id}`);
        e.layers.add(l.id);
        e.valid = Math.max(e.valid, Date.parse(p.valid));
        if (p.run) e.run = Math.max(e.run, Date.parse(p.run));
        by.set(p.source, e);
      }
    }
    return SOURCES.map((s) => {
      const e = by.get(s.id);
      return {
        id: s.id,
        title: s.title,
        provider: s.provider,
        parts: e?.parts.size ?? 0,
        layers: e?.layers.size ?? 0,
        newest_valid: e && Number.isFinite(e.valid) ? iso(e.valid) : null,
        newest_run: e && Number.isFinite(e.run) ? iso(e.run) : null,
      };
    });
  }
}
