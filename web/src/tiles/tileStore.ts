// Decoded tiles, keyed by URL: loading, cancellation, retry, and a memory
// bound with least-recently-used eviction. A tile URL names its layer, place
// and stack of parts, never a frame, so every frame whose stack there is the
// same shares one entry, one download and one texture. The decoded samples
// stay on the CPU side for the value read-out; the GPU texture is attached by
// the renderer and freed through `disposeTexture`.

import type { TileID, TileState } from './tileMath.ts';

export type LoadResult =
  | { status: 'ok'; width: number; height: number; channels: number; data: Uint16Array }
  | { status: 'empty' }
  | { status: 'missing' }
  | { status: 'error'; retry: boolean; retryAfterMs?: number }
  | { status: 'cancelled' };

export type Fetcher = (url: string, pick: number[], signal: AbortSignal) => Promise<LoadResult>;

export interface TileEntry {
  /** The tile's URL. */
  key: string;
  z: number;
  x: number;
  y: number;
  state: TileState;
  data: Uint16Array | null;
  width: number;
  height: number;
  channels: number;
  texture: WebGLTexture | null;
  lastUsed: number;
  abort: AbortController | null;
  /** When an error, a 404 or an empty answer should be asked again. */
  retryAt: number;
  failures: number;
}

export interface TileRequest {
  url: string;
  tile: TileID;
  pick: number[];
}

export interface StoreOptions {
  budgetBytes: number;
  maxInFlight: number;
  /** Expected memory for a tile not loaded yet (samples plus texture). */
  estimateBytes?: (pick: readonly number[]) => number;
}

/** A 257 x 257 tile of `pick.length` u16 channels, held twice (CPU and GPU). */
const defaultEstimate = (pick: readonly number[]) => 257 * 257 * 2 * pick.length * 2;

/** The server says empty tiles may be cached for an hour. */
const EMPTY_TTL_MS = 3600_000;

export class TileStore {
  private entries = new Map<string, TileEntry>();
  private loading = 0;
  private bytes = 0;
  /** Keys admitted by the last `want`: never evicted. */
  private wanted = new Set<string>();
  /** The last `want`'s admitted requests, in order: when a load finishes the
   *  next one starts at once, without waiting for the map to draw a frame
   *  (slow devices draw few). */
  private queue: readonly TileRequest[] = [];
  /** No queued tile is due for another attempt before this. */
  private nextRetry = Infinity;
  /** Settled tiles whose samples have no texture yet. */
  private unuploaded = new Set<TileEntry>();
  /** Bumped whenever a tile is added, removed or changes state: what the
   *  renderer planned and drew from the store is still right while this
   *  stays the same (textures are the renderer's own). */
  version = 0;
  /** Something finished loading: the map should redraw. */
  onUpdate: () => void = () => {};
  disposeTexture: (t: WebGLTexture) => void = () => {};
  private readonly fetcher: Fetcher;
  readonly opts: StoreOptions;
  /** Every load started, and the most recent URLs, for tests and `?debug`. */
  readonly stats = { started: 0, recent: [] as string[] };

  constructor(fetcher: Fetcher, opts: StoreOptions) {
    this.fetcher = fetcher;
    this.opts = opts;
  }

  get(url: string): TileEntry | undefined {
    return this.entries.get(url);
  }

  state(url: string): TileState | undefined {
    return this.entries.get(url)?.state;
  }

  /** Bytes held by decoded samples and their textures. */
  get usedBytes() {
    return this.bytes;
  }

  get inFlight() {
    return this.loading;
  }

  get size() {
    return this.entries.size;
  }

  private weigh(e: TileEntry) {
    if (!e.data) return 0;
    // CPU copy plus a GPU copy of the same size once uploaded.
    return e.data.byteLength * (e.texture ? 2 : 1);
  }

  /** The renderer uploaded (or dropped) a tile's texture. */
  setTexture(e: TileEntry, tex: WebGLTexture | null) {
    this.bytes -= this.weigh(e);
    e.texture = tex;
    this.bytes += this.weigh(e);
    if (tex || !e.data) this.unuploaded.delete(e);
    else this.unuploaded.add(e);
  }

  /**
   * Declare everything wanted right now, most important first. The first
   * `required` requests (the frame on screen) are always admitted; the rest
   * (prefetch) only while their expected memory fits the budget, so wanted
   * tiles never outgrow the cache and get evicted and fetched again in a loop.
   * Loads start in order up to the concurrency limit; loads still running for
   * tiles no longer wanted are cancelled, so a fast pan or slider drag does
   * not leave a queue of stale requests in front of the tiles that matter.
   * Returns how many requests were admitted.
   */
  want(reqs: readonly TileRequest[], now = performance.now(), opts: { required?: number } = {}): number {
    const required = opts.required ?? reqs.length;
    const estimate = this.opts.estimateBytes ?? defaultEstimate;
    const room = this.opts.budgetBytes * 0.9;
    // What fits is always a prefix of the requests.
    let n = 0;
    let planned = 0;
    for (; n < reqs.length; n++) {
      const e = this.entries.get(reqs[n].url);
      const cost = e && e.state !== 'loading' && e.state !== 'error' ? this.weighFull(e) : estimate(reqs[n].pick);
      if (n >= required && planned + cost > room) break;
      planned += cost;
    }
    // The map asks every frame, usually for what it asked for last time: then
    // there is nothing to cancel and no new load to start, only retries.
    const q = this.queue;
    let same = n === q.length;
    for (let i = 0; same && i < n; i++) same = reqs[i].url === q[i].url;
    if (!same) {
      // A tile is used for as long as it is wanted: those leaving now were
      // last used now (eviction frees the least recently used first).
      for (const k of this.wanted) {
        const e = this.entries.get(k);
        if (e) e.lastUsed = now;
      }
      this.queue = reqs.slice(0, n);
      this.wanted = new Set(this.queue.map((r) => r.url));
      for (const e of this.entries.values()) {
        if (e.state === 'loading' && !this.wanted.has(e.key)) {
          e.abort?.abort();
          this.entries.delete(e.key);
          this.loading--;
          this.version++;
        }
      }
      for (const k of this.wanted) {
        const e = this.entries.get(k);
        if (e) e.lastUsed = now;
      }
      this.nextRetry = -Infinity;
    }
    if (now >= this.nextRetry) this.pump(now);
    return n;
  }

  /** Start queued loads, in order, up to the concurrency limit. */
  private pump(now: number) {
    this.nextRetry = Infinity;
    for (const r of this.queue) {
      if (this.loading >= this.opts.maxInFlight) {
        // Picked up again when a load finishes.
        this.nextRetry = -Infinity;
        return;
      }
      const e = this.entries.get(r.url);
      if (e) {
        if (e.state === 'error' || e.state === 'missing' || e.state === 'empty') {
          if (e.retryAt <= now) this.start(e, r.pick);
          else this.nextRetry = Math.min(this.nextRetry, e.retryAt);
        }
        continue;
      }
      const entry: TileEntry = {
        key: r.url,
        z: r.tile.z,
        x: r.tile.x,
        y: r.tile.y,
        state: 'loading',
        data: null,
        width: 0,
        height: 0,
        channels: 0,
        texture: null,
        lastUsed: now,
        abort: null,
        retryAt: 0,
        failures: 0,
      };
      this.entries.set(r.url, entry);
      this.start(entry, r.pick);
    }
  }

  /** Memory a settled tile holds once its texture is uploaded too. */
  private weighFull(e: TileEntry) {
    return e.data ? e.data.byteLength * 2 : 0;
  }

  /** Mark tiles as recently used without requesting anything. */
  touch(keys: Iterable<string>, now = performance.now()) {
    for (const k of keys) {
      const e = this.entries.get(k);
      if (e) e.lastUsed = now;
    }
  }

  private start(e: TileEntry, pick: number[]) {
    const ac = new AbortController();
    e.abort = ac;
    e.state = 'loading';
    this.version++;
    this.loading++;
    this.stats.started++;
    this.stats.recent.push(e.key);
    if (this.stats.recent.length > 2000) this.stats.recent.splice(0, 1000);
    this.fetcher(e.key, pick, ac.signal).then(
      (r) => this.finish(e, ac, r),
      () => this.finish(e, ac, { status: 'error', retry: true }),
    );
  }

  private finish(e: TileEntry, ac: AbortController, r: LoadResult) {
    // Cancelled and forgotten, or superseded by a newer attempt.
    if (this.entries.get(e.key) !== e || e.abort !== ac) return;
    this.loading--;
    e.abort = null;
    this.version++;
    const now = performance.now();
    switch (r.status) {
      case 'ok':
        // A retried tile may still hold the samples of an earlier answer.
        this.bytes -= this.weigh(e);
        if (e.texture) this.disposeTexture(e.texture);
        e.texture = null;
        e.state = 'ready';
        e.data = r.data;
        e.width = r.width;
        e.height = r.height;
        e.channels = r.channels;
        e.failures = 0;
        this.bytes += this.weigh(e);
        this.unuploaded.add(e);
        break;
      case 'empty':
        e.state = 'empty';
        e.failures = 0;
        e.retryAt = now + EMPTY_TTL_MS;
        break;
      case 'missing':
        // 404. While some frame still names this stack it is transient (a
        // server restart, a part not stored yet), so ask again with growing
        // gaps; once nothing names it, eviction forgets it.
        e.failures++;
        e.state = 'missing';
        e.retryAt = now + Math.min(300_000, 10_000 * 2 ** (e.failures - 1));
        break;
      case 'cancelled':
        this.entries.delete(e.key);
        // Started again by the next `want` if it is still wanted.
        this.nextRetry = -Infinity;
        return;
      case 'error':
        e.failures++;
        e.state = 'error';
        e.retryAt = !r.retry
          ? Infinity
          : r.retryAfterMs !== undefined
            ? now + Math.max(500, r.retryAfterMs)
            : now + Math.min(60_000, 2000 * 2 ** (e.failures - 1));
        break;
    }
    this.pump(now);
    this.onUpdate();
  }

  /**
   * Free tiles until under budget, the least recently used first. While a
   * loop plays, `nextUse` says how many frames ahead each of its tiles is
   * needed again: those go after every other tile, the one needed furthest
   * ahead first, so a loop too big for the budget keeps what it can of itself
   * instead of evicting each frame just before it comes round again. Wanted
   * tiles and `protect` (drawn this frame, such as a fallback) are never freed.
   */
  evict(protect: ReadonlySet<string>, nextUse?: () => ReadonlyMap<string, number>) {
    if (this.bytes <= this.opts.budgetBytes && this.entries.size < 20_000) return;
    const ahead = nextUse?.() ?? new Map<string, number>();
    const victims = [...this.entries.values()]
      .filter((e) => e.state !== 'loading' && !protect.has(e.key) && !this.wanted.has(e.key))
      .sort((a, b) => (ahead.get(b.key) ?? Infinity) - (ahead.get(a.key) ?? Infinity) || a.lastUsed - b.lastUsed);
    for (const e of victims) {
      if (this.bytes <= this.opts.budgetBytes * 0.9 && this.entries.size < 18_000) break;
      this.remove(e);
    }
  }

  private remove(e: TileEntry) {
    e.abort?.abort();
    if (e.state === 'loading') this.loading--;
    this.bytes -= this.weigh(e);
    if (e.texture) this.disposeTexture(e.texture);
    e.texture = null;
    e.data = null;
    this.entries.delete(e.key);
    this.unuploaded.delete(e);
    this.nextRetry = -Infinity;
    this.version++;
  }

  /** Forget GPU textures (after a lost context) but keep decoded samples. */
  forgetTextures() {
    for (const e of this.entries.values()) {
      if (e.texture) this.setTexture(e, null);
    }
  }

  /** Tiles with samples but no texture yet. */
  pendingUploads(): IterableIterator<TileEntry> {
    return this.unuploaded.values();
  }
}
