// Loading and live-updating everything the map shows: the layer and source
// registries, the timelines of the layers looked at (the active one first,
// the others when the user switches to them), the alerts and stations in
// view, and saved places. Applies WebSocket events as they arrive and
// refetches after a reconnect, since the server does not replay what was
// missed.

import { hasDevice, places as placesApi } from '../api/device.ts';
import { api } from '../api/http.ts';
import { type LiveHandlers, LiveSocket } from '../api/live.ts';
import type { AlertCollection, AlertProps, Config, Place, WsEvent } from '../api/types.ts';
import { isSerious } from '../core/alertColors.ts';
import type { Store } from '../core/store.ts';
import { applyTimeline, parseTime, parseTimeline } from '../core/timeline.ts';
import type { AppState, Toast } from './state.ts';

let toastId = 1;

export interface DataHooks {
  /** The first load succeeded (after any number of retries). */
  booted?(config: Config): void;
}

/** What the controller talks to, replaceable in tests. */
export interface DataDeps {
  api: Pick<typeof api, 'config' | 'layers' | 'sources' | 'layerFrames' | 'alerts' | 'alertTiles' | 'alertDetail' | 'stations'>;
  listPlaces(): Promise<Place[]>;
  hasDevice(): boolean;
  createLive(h: LiveHandlers): { start(): void; stop(): void; readonly stats?: LiveSocket['stats'] };
}

export const defaultDeps: DataDeps = {
  api,
  listPlaces: () => placesApi.list(),
  hasDevice,
  createLive: (h) => new LiveSocket(h),
};

/** Events held while a snapshot is fetched, at most. Beyond this the snapshot
 *  is fetched again instead, which is cheaper than replaying a flood. */
const MAX_BUFFERED = 5000;

/** Fetch a layer's timeline again at most this often when its frames name
 *  parts no message described (the client lost track of something). */
const UNKNOWN_RESYNC_MS = 30_000;

/** Lists fetched by area wait this long after the view stops changing. */
const VIEW_DEBOUNCE_MS = 350;

/** Alerts asked for per view: enough for a busy continent. `?alertlimit=N`
 *  asks for fewer, to see what a capped list looks like. */
const ALERT_LIMIT = (() => {
  const forced = typeof location === 'undefined' ? NaN : Number(new URLSearchParams(location.search).get('alertlimit'));
  return forced >= 1 && forced <= 5000 ? Math.floor(forced) : 500;
})();

export class DataController {
  private readonly store: Store<AppState>;
  private readonly hooks: DataHooks;
  private readonly deps: DataDeps;
  private live: ReturnType<DataDeps['createLive']> | null = null;
  private unsub: (() => void) | null = null;
  /** Alert events are handled one at a time, in order: each refetches, and
   *  overlapping refetches would make an alert look old before it is announced. */
  private alertQueue: Promise<void> = Promise.resolve();
  /**
   * Events that arrive while the timelines are being fetched (the first load,
   * or a resync after reconnecting) are held and replayed onto them once they
   * are in, so a change announced during the request is neither lost nor
   * undone by the older snapshot.
   */
  private buffer: WsEvent[] | null = [];
  private overflowed = false;
  /** The same, per layer, while one layer's timeline loads on its own. */
  private pending = new Map<string, WsEvent[]>();
  private booted = false;
  private booting = false;
  private bootFailures = 0;
  private bootTimer: ReturnType<typeof setTimeout> | null = null;
  private resyncing = false;
  /** The newest alert revision whose news has been taken in (a snapshot or an
   *  event). Revisions are large and not consecutive; an event at or below
   *  this one is old news, from a replay or a race with a snapshot. */
  private alertNews = 0;
  private resyncAgain = false;
  private freshTimer: ReturnType<typeof setTimeout> | null = null;
  private lastUnknownResync = new Map<string, number>();
  private viewTimer: ReturnType<typeof setTimeout> | null = null;
  private alertsAbort: AbortController | null = null;
  private stationsAbort: AbortController | null = null;

  constructor(store: Store<AppState>, hooks: DataHooks, deps: DataDeps = defaultDeps) {
    this.store = store;
    this.hooks = hooks;
    this.deps = deps;
  }

  get liveStats() {
    return this.live?.stats;
  }

  get isBooted() {
    return this.booted;
  }

  /** Open the socket and load everything, retrying until it works. */
  start() {
    this.startLive();
    this.unsub ??= this.store.subscribe((s, prev) => {
      if (s.layerId !== prev.layerId) void this.ensureLayer(s.layerId);
      if (s.viewBox !== prev.viewBox || (s.overlays.stations && !prev.overlays.stations)) this.viewChanged();
    });
    void this.bootWithRetry();
  }

  /** Try the first load now (the "Try again" button, or the socket opened). */
  retryNow() {
    if (this.booted || this.booting) return;
    if (this.bootTimer) clearTimeout(this.bootTimer);
    this.bootTimer = null;
    void this.bootWithRetry();
  }

  private async bootWithRetry() {
    if (this.booted || this.booting) return;
    this.booting = true;
    try {
      const config = await this.boot();
      this.booted = true;
      this.bootFailures = 0;
      this.store.set({ error: null });
      this.hooks.booted?.(config);
      // The user may have switched layers while the first one loaded.
      void this.ensureLayer(this.store.get().layerId);
      this.viewChanged();
    } catch (e) {
      this.bootFailures++;
      const status = (e as { status?: number }).status;
      const delay = Math.min(30_000, 2000 * 2 ** (this.bootFailures - 1));
      const what = status === 0 || status === undefined ? 'The weather server could not be reached.' : 'The weather server returned an error.';
      this.store.set({ error: `${what} Trying again in ${Math.round(delay / 1000)} s…` });
      this.bootTimer = setTimeout(() => {
        this.bootTimer = null;
        void this.bootWithRetry();
      }, delay);
    } finally {
      this.booting = false;
    }
  }

  /** The registries, the alert set's revision, and the active layer's
   *  timeline only: the others wait until someone looks at them. */
  async boot(): Promise<Config> {
    // Events from before this request are covered by its answer.
    this.buffer = [];
    this.overflowed = false;
    let config: Config;
    try {
      // The layer shown last time is almost always still offered: its
      // timeline is asked for alongside the registries, not after them.
      const guessed = this.store.get().layerId;
      const early = this.deps.api.layerFrames(guessed).then(
        (tl) => () => tl,
        (e: unknown) => () => {
          throw e;
        },
      );
      const [c, layers, sources, tiles] = await Promise.all([
        this.deps.api.config(),
        this.deps.api.layers(),
        this.deps.api.sources(),
        this.deps.api.alertTiles().catch(() => null),
      ]);
      config = c;
      const s = this.store.get();
      const layerId = layers.some((l) => l.id === s.layerId) ? s.layerId : (layers[0]?.id ?? s.layerId);
      const tl = !layers.length ? null : layerId === guessed ? (await early)() : await this.deps.api.layerFrames(layerId);
      this.alertNews = Math.max(this.alertNews, tiles?.revision ?? 0);
      this.store.set({
        config,
        layers,
        sources: Object.fromEntries(sources.map((d) => [d.id, d])),
        timelines: tl ? { [layerId]: parseTimeline(tl) } : {},
        clockOffset: this.offsetFrom(config.server_time) ?? s.clockOffset,
        layerId,
        alertsRevision: Math.max(s.alertsRevision, tiles?.revision ?? 0),
        alertsAttribution: tiles?.attribution ?? null,
      });
    } catch (e) {
      // Keep holding events, but drop these: the next attempt starts afresh.
      this.buffer = [];
      throw e;
    }
    this.flush();
    if (this.deps.hasDevice()) void this.loadPlaces();
    return config;
  }

  /** Replay what was held during a snapshot fetch, then stop holding. */
  private flush() {
    const held = this.buffer ?? [];
    const again = this.overflowed;
    this.buffer = null;
    this.overflowed = false;
    for (const ev of held) this.apply(ev);
    if (again) void this.resync();
  }

  /** Load a layer's timeline the first time it is shown. */
  async ensureLayer(id: string) {
    const s = this.store.get();
    if (!this.booted || s.timelines[id] || this.pending.has(id) || !s.layers.some((l) => l.id === id)) return;
    await this.loadLayer(id);
  }

  /** Fetch one layer's timeline, holding its events meanwhile and replaying
   *  them onto it after. */
  private async loadLayer(id: string) {
    if (this.pending.has(id)) return;
    this.pending.set(id, []);
    try {
      const tl = parseTimeline(await this.deps.api.layerFrames(id));
      this.store.set((s) => ({ timelines: { ...s.timelines, [id]: tl } }));
    } catch {
      // Tried again the next time the layer is chosen, or on a resync.
    } finally {
      const held = this.pending.get(id) ?? [];
      this.pending.delete(id);
      for (const ev of held) this.apply(ev);
    }
  }

  /** Open the socket; safe to call before `boot`, and only once. */
  startLive() {
    if (this.live) return;
    this.live = this.deps.createLive({
      onEvent: (ev) => this.onEvent(ev),
      onResync: () => void this.resync(),
      onState: (live) => this.store.set({ live }),
      // The server is answering again: no need to wait out the retry timer.
      onOpen: () => this.retryNow(),
    });
    this.live.start();
  }

  stop() {
    this.live?.stop();
    this.unsub?.();
    this.unsub = null;
    if (this.bootTimer) clearTimeout(this.bootTimer);
    if (this.viewTimer) clearTimeout(this.viewTimer);
  }

  /** The view moved: ask again for what is in it, once it settles. */
  private viewChanged() {
    if (this.viewTimer) clearTimeout(this.viewTimer);
    this.viewTimer = setTimeout(() => {
      this.viewTimer = null;
      void this.loadAlerts();
      if (this.store.get().overlays.stations) void this.loadStations();
    }, VIEW_DEBOUNCE_MS);
  }

  /** The alerts in view, shaped or not. Null when there is no view yet or
   *  the request failed (what was shown stays). */
  async loadAlerts(): Promise<AlertCollection | null> {
    const box = this.store.get().viewBox;
    if (!box || !this.booted) return null;
    this.alertsAbort?.abort();
    const ac = new AbortController();
    this.alertsAbort = ac;
    try {
      const alerts = await this.deps.api.alerts({ bbox: box, unshaped: true, limit: ALERT_LIMIT }, ac.signal);
      if (ac.signal.aborted) return null;
      this.store.set((s) => ({ alerts, alertsRevision: Math.max(s.alertsRevision, alerts.revision ?? 0) }));
      return alerts;
    } catch {
      return null;
    }
  }

  async loadStations() {
    const box = this.store.get().viewBox;
    if (!box || !this.booted) return;
    this.stationsAbort?.abort();
    const ac = new AbortController();
    this.stationsAbort = ac;
    try {
      const stations = await this.deps.api.stations(box, ac.signal);
      if (!ac.signal.aborted) this.store.set({ stations });
    } catch {
      // What is shown stays; the next move asks again.
    }
  }

  /** The stations overlay was switched on. */
  ensureStations() {
    void this.loadStations();
  }

  async loadPlaces() {
    // Listing places would register a device; wait until there is a reason.
    if (!this.deps.hasDevice()) return;
    try {
      this.store.set({ places: await this.deps.listPlaces() });
    } catch {
      // Places stay as they were; the panel shows its own errors.
    }
  }

  private offsetFrom(serverTime: string | undefined): number | null {
    const t = serverTime ? parseTime(serverTime) : Number.NaN;
    return Number.isFinite(t) ? t - Date.now() : null;
  }

  /** After a reconnect: every timeline held, the alert set, the lists in
   *  view and the places, as the server has them now. */
  async resync() {
    // Before the first load there is nothing to bring up to date.
    if (!this.booted) return;
    if (this.resyncing) {
      this.resyncAgain = true;
      return;
    }
    this.resyncing = true;
    this.buffer = [];
    this.overflowed = false;
    try {
      const ids = Object.keys(this.store.get().timelines);
      const fresh = await Promise.all(ids.map(async (id) => [id, parseTimeline(await this.deps.api.layerFrames(id))] as const));
      this.store.set((s) => ({ timelines: { ...s.timelines, ...Object.fromEntries(fresh) } }));
      // Events announced while the snapshots were on their way go on top.
      this.flush();
    } catch {
      // No snapshot: apply what was held to what we have. The socket will
      // drop and resync again if the server is really gone.
      this.flush();
    } finally {
      this.resyncing = false;
    }
    const tiles = await this.deps.api.alertTiles().catch(() => null);
    if (tiles) {
      this.alertNews = Math.max(this.alertNews, tiles.revision);
      this.store.set((s) => ({ alertsRevision: Math.max(s.alertsRevision, tiles.revision), alertsAttribution: tiles.attribution ?? null }));
    }
    await this.loadAlerts();
    if (this.store.get().overlays.stations) await this.loadStations();
    if (this.deps.hasDevice()) await this.loadPlaces();
    if (this.resyncAgain) {
      this.resyncAgain = false;
      void this.resync();
    }
  }

  private onEvent(ev: WsEvent) {
    // The clock is not state the snapshot replaces: keep it fresh always.
    if (ev.type === 'hello' || ev.type === 'heartbeat') {
      const off = this.offsetFrom(ev.server_time);
      if (off !== null) this.store.set({ clockOffset: off });
      return;
    }
    if (this.buffer) {
      if (this.buffer.length >= MAX_BUFFERED) {
        this.buffer = [];
        this.overflowed = true;
      } else this.buffer.push(ev);
      return;
    }
    this.apply(ev);
  }

  private apply(ev: WsEvent) {
    switch (ev.type) {
      case 'timeline':
        this.onTimeline(ev);
        break;
      case 'alerts':
        this.alertQueue = this.alertQueue.then(() => this.onAlerts(ev)).catch(() => {});
        break;
      case 'stations':
        if (this.store.get().overlays.stations) void this.loadStations();
        break;
      default:
        break;
    }
  }

  private onTimeline(ev: Extract<WsEvent, { type: 'timeline' }>) {
    const held = this.pending.get(ev.layer);
    if (held) {
      held.push(ev);
      return;
    }
    const s = this.store.get();
    const cur = s.timelines[ev.layer];
    // A layer never loaded ignores its events: it is fetched whole when shown.
    if (!cur) return;
    const next = applyTimeline(cur, ev);
    this.store.set({ timelines: { ...s.timelines, [ev.layer]: next } });
    // A frame naming a part nobody described cannot be drawn right; the
    // layer's timeline describes every part, so fetch it (but not in a loop
    // against a server that keeps doing it).
    const last = this.lastUnknownResync.get(ev.layer) ?? -Infinity;
    if (next.unknown > 0 && performance.now() - last > UNKNOWN_RESYNC_MS) {
      this.lastUnknownResync.set(ev.layer, performance.now());
      void this.loadLayer(ev.layer);
    }
  }

  private async onAlerts(ev: Extract<WsEvent, { type: 'alerts' }>) {
    const { added, updated, removed } = ev;
    if (!(ev.revision > this.alertNews)) return;
    this.alertNews = ev.revision;
    const before = this.store.get();
    if (ev.revision > before.alertsRevision) this.store.set({ alertsRevision: ev.revision });
    const list = await this.loadAlerts();
    if (this.deps.hasDevice() && (added.length || updated.length || removed.length)) await this.loadPlaces();
    const s = this.store.get();
    const byId = new Map((list ?? s.alerts)?.features.map((f) => [f.properties.id, f.properties]) ?? []);
    const covering = new Set(s.places.flatMap((p) => p.alerts.map((a) => a.id)));
    // New alerts not in view are only news when they cover a saved place:
    // fetch those few in full to judge them.
    const known = new Map<string, AlertProps>();
    for (const id of added) {
      const a = byId.get(id) ?? (covering.has(id) ? await this.deps.api.alertDetail(id).catch(() => null) : null);
      if (a) known.set(id, a);
    }

    // An update arrives as a new id: follow the alert being read to it.
    if (s.panel?.kind === 'alert' && removed.includes(s.panel.id) && !byId.has(s.panel.id)) {
      const old = before.alerts?.features.find((f) => f.properties.id === (s.panel as { id: string }).id)?.properties;
      const next = old && [...known.values()].find((a) => a.source === old.source && (a.event === old.event || a.area_desc === old.area_desc));
      this.store.set({ panel: next ? { ...s.panel, id: next.id } : null });
    }
    const fresh = added.filter((id) => known.has(id));
    if (!fresh.length) return;

    const toasts: Toast[] = [];
    for (const id of fresh) {
      const a = known.get(id)!;
      if (!isSerious(a)) continue;
      // A new warning over a saved place is the headline; others are news.
      const place = s.places.find((p) => p.alerts.some((x) => x.id === id));
      toasts.push({
        id: toastId++,
        title: place ? `${a.event} · ${place.name}` : a.event,
        body: place ? `Now covers ${place.name}.` : (a.area_desc ?? a.headline ?? ''),
        severity: a.severity,
        action: { label: 'View', alertId: id },
      });
    }
    this.store.set((st) => ({ freshAlerts: fresh, toasts: [...st.toasts.filter((t) => !fresh.includes(t.action?.alertId ?? '')), ...toasts].slice(-4) }));
    if (this.freshTimer) clearTimeout(this.freshTimer);
    this.freshTimer = setTimeout(() => this.store.set({ freshAlerts: [] }), 6000);
  }
}
