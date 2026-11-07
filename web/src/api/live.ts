// The `/ws` event stream, kept open: reconnects with capped, jittered backoff,
// notices a connection that has gone quiet (the server sends a heartbeat every
// 25 seconds), and tells the app when it has reconnected so it can refetch
// what it may have missed (the server does not replay events).

import type { WsEvent } from './types.ts';

export type LiveState = 'connecting' | 'open' | 'closed';

export interface LiveHandlers {
  onEvent(ev: WsEvent): void;
  /** Called on every successful (re)connection after the first. */
  onResync(): void;
  onState?(s: LiveState): void;
  /** Every successful connection, the first included. */
  onOpen?(): void;
}

/** The part of a WebSocket this uses, so tests can supply their own. */
export interface SocketLike {
  readonly readyState: number;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: unknown) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  close(code?: number, reason?: string): void;
}

interface EventTargetLike {
  addEventListener(type: string, fn: () => void): void;
  removeEventListener(type: string, fn: () => void): void;
}

interface LiveOptions {
  url?: string;
  createSocket?: (url: string) => SocketLike;
  now?: () => number;
  /** Silence after which an open connection is presumed dead. */
  staleMs?: number;
  /** A connection must stay up this long before backoff starts over, so a
   *  server that keeps closing (1013 under load) is not hammered. */
  stableMs?: number;
  document?: EventTargetLike & { visibilityState: string };
  window?: EventTargetLike;
}

const CONNECTING = 0;
const OPEN = 1;

export class LiveSocket {
  private ws: SocketLike | null = null;
  private attempts = 0;
  private everOpened = false;
  private openedAt = 0;
  private lastMessageAt = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private watchdog: ReturnType<typeof setInterval> | null = null;
  private stopped = true;
  private hiddenAt = 0;
  private readonly handlers: LiveHandlers;
  private readonly url: string;
  private readonly createSocket: (url: string) => SocketLike;
  private readonly now: () => number;
  private readonly staleMs: number;
  private readonly stableMs: number;
  private readonly doc: LiveOptions['document'];
  private readonly win: LiveOptions['window'];
  /** For the status of the connection in debugging and tests. */
  readonly stats = { opens: 0, messages: 0, heartbeats: 0, staleCloses: 0, lastMessageAt: 0 };

  constructor(handlers: LiveHandlers, opts: LiveOptions = {}) {
    this.handlers = handlers;
    if (opts.url) this.url = opts.url;
    else {
      const u = new URL('/ws', location.href);
      u.protocol = u.protocol === 'https:' ? 'wss:' : 'ws:';
      this.url = u.href;
    }
    this.createSocket = opts.createSocket ?? ((url) => new WebSocket(url) as unknown as SocketLike);
    this.now = opts.now ?? (() => Date.now());
    this.staleMs = opts.staleMs ?? 60_000;
    this.stableMs = opts.stableMs ?? 30_000;
    this.doc = opts.document ?? (typeof document === 'undefined' ? undefined : document);
    this.win = opts.window ?? (typeof window === 'undefined' ? undefined : window);
  }

  start() {
    if (!this.stopped) return;
    this.stopped = false;
    this.doc?.addEventListener('visibilitychange', this.onVisibility);
    this.win?.addEventListener('online', this.onOnline);
    this.watchdog = setInterval(() => this.checkAlive(), Math.min(10_000, this.staleMs / 4));
    this.connect();
  }

  stop() {
    this.stopped = true;
    if (this.timer) clearTimeout(this.timer);
    if (this.watchdog) clearInterval(this.watchdog);
    this.timer = null;
    this.watchdog = null;
    this.discard();
    this.doc?.removeEventListener('visibilitychange', this.onVisibility);
    this.win?.removeEventListener('online', this.onOnline);
  }

  /** Detach and close the current socket, if any, so its late events are ignored. */
  private discard() {
    const ws = this.ws;
    if (!ws) return;
    this.ws = null;
    ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
    try {
      ws.close();
    } catch {
      // Closing a socket that never opened can throw; it is gone either way.
    }
  }

  private connect() {
    if (this.stopped) return;
    // Waking a device fires `visibilitychange` and `online` together: one
    // socket per client, never two.
    if (this.ws && (this.ws.readyState === CONNECTING || this.ws.readyState === OPEN)) return;
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    this.discard();
    this.handlers.onState?.('connecting');
    let ws: SocketLike;
    try {
      ws = this.createSocket(this.url);
    } catch {
      this.schedule();
      return;
    }
    this.ws = ws;
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.openedAt = this.lastMessageAt = this.stats.lastMessageAt = this.now();
      this.stats.opens++;
      this.handlers.onState?.('open');
      this.handlers.onOpen?.();
      if (this.everOpened) this.handlers.onResync();
      this.everOpened = true;
    };
    ws.onmessage = (m) => {
      if (this.ws !== ws) return;
      this.lastMessageAt = this.stats.lastMessageAt = this.now();
      this.stats.messages++;
      if (typeof m.data !== 'string') return;
      let ev: WsEvent;
      try {
        ev = JSON.parse(m.data) as WsEvent;
      } catch {
        return;
      }
      if (!ev || typeof ev.type !== 'string') return;
      if (ev.type === 'heartbeat') this.stats.heartbeats++;
      this.handlers.onEvent(ev);
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      if (this.openedAt && this.now() - this.openedAt >= this.stableMs) this.attempts = 0;
      this.openedAt = 0;
      this.handlers.onState?.('closed');
      this.schedule();
    };
    // `onclose` follows every error and does the reconnecting.
    ws.onerror = () => {};
  }

  private schedule() {
    if (this.stopped || this.timer) return;
    this.attempts++;
    const base = Math.min(30_000, 1000 * 2 ** Math.min(this.attempts - 1, 5));
    this.timer = setTimeout(() => {
      this.timer = null;
      this.connect();
    }, base * (0.75 + Math.random() * 0.5));
  }

  /** An open socket that has heard nothing, not even a heartbeat, for
   *  `staleMs` is dead in all but name (a proxy or NAT dropped it). */
  private checkAlive() {
    const ws = this.ws;
    if (!ws || ws.readyState !== OPEN) return;
    if (this.now() - this.lastMessageAt < this.staleMs) return;
    this.stats.staleCloses++;
    this.discard();
    this.handlers.onState?.('closed');
    this.connect();
  }

  private onVisibility = () => {
    if (this.doc?.visibilityState === 'hidden') {
      this.hiddenAt = this.now();
      return;
    }
    // Phones freeze background tabs; a socket that looks open may be dead.
    this.checkAlive();
    if (!this.ws) this.connect();
    else if (this.hiddenAt && this.now() - this.hiddenAt > 60_000) this.handlers.onResync();
    this.hiddenAt = 0;
  };

  private onOnline = () => {
    this.checkAlive();
    this.connect();
  };
}
