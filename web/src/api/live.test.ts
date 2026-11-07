import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { LiveSocket, type SocketLike } from './live.ts';

class FakeSocket implements SocketLike {
  readyState = 0;
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  closed = false;
  open() {
    this.readyState = 1;
    this.onopen?.({});
  }
  send(ev: object) {
    this.onmessage?.({ data: JSON.stringify(ev) });
  }
  /** The server hangs up. */
  drop() {
    this.readyState = 3;
    this.onclose?.({});
  }
  close() {
    this.closed = true;
    this.readyState = 3;
  }
}

class Target {
  visibilityState = 'visible';
  private fns = new Map<string, Set<() => void>>();
  addEventListener(t: string, fn: () => void) {
    if (!this.fns.has(t)) this.fns.set(t, new Set());
    this.fns.get(t)!.add(fn);
  }
  removeEventListener(t: string, fn: () => void) {
    this.fns.get(t)?.delete(fn);
  }
  fire(t: string) {
    for (const fn of this.fns.get(t) ?? []) fn();
  }
}

function setup() {
  const sockets: FakeSocket[] = [];
  const events: string[] = [];
  let resyncs = 0;
  const doc = new Target();
  const win = new Target();
  const live = new LiveSocket(
    { onEvent: (e) => events.push(e.type), onResync: () => resyncs++ },
    {
      url: 'ws://test/ws',
      createSocket: () => {
        const s = new FakeSocket();
        sockets.push(s);
        return s;
      },
      document: doc,
      window: win,
    },
  );
  return { live, sockets, events, doc, win, resyncs: () => resyncs, alive: () => sockets.filter((s) => !s.closed && s.readyState < 2) };
}

describe('live socket', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('reconnects when an open connection goes silent', () => {
    const t = setup();
    t.live.start();
    t.sockets[0].open();
    vi.advanceTimersByTime(55_000);
    expect(t.sockets).toHaveLength(1);
    vi.advanceTimersByTime(15_000);
    expect(t.sockets[0].closed).toBe(true);
    expect(t.sockets).toHaveLength(2);
    t.sockets[1].open();
    expect(t.resyncs()).toBe(1);
    expect(t.live.stats.staleCloses).toBe(1);
    t.live.stop();
  });

  it('stays on one connection while heartbeats arrive', () => {
    const t = setup();
    t.live.start();
    t.sockets[0].open();
    for (let i = 0; i < 12; i++) {
      vi.advanceTimersByTime(25_000);
      t.sockets[0].send({ type: 'heartbeat', server_time: new Date().toISOString() });
    }
    expect(t.sockets).toHaveLength(1);
    expect(t.live.stats.heartbeats).toBe(12);
    expect(t.events.filter((e) => e === 'heartbeat')).toHaveLength(12);
    t.live.stop();
  });

  it('checks a silent connection as soon as the page is shown again', () => {
    const t = setup();
    t.live.start();
    t.sockets[0].open();
    t.doc.visibilityState = 'hidden';
    t.doc.fire('visibilitychange');
    // A frozen tab: timers do not run, the clock does.
    vi.setSystemTime(Date.now() + 5 * 60_000);
    t.doc.visibilityState = 'visible';
    t.doc.fire('visibilitychange');
    expect(t.sockets[0].closed).toBe(true);
    expect(t.sockets).toHaveLength(2);
    t.live.stop();
  });

  it('opens one socket, not two, when waking fires visibility and online together', () => {
    const t = setup();
    t.live.start();
    t.sockets[0].open();
    t.sockets[0].drop();
    t.doc.fire('visibilitychange');
    t.win.fire('online');
    t.win.fire('online');
    expect(t.alive()).toHaveLength(1);
    // The first connection plus exactly one replacement.
    expect(t.sockets).toHaveLength(2);
    t.sockets.at(-1)!.open();
    t.sockets.at(-1)!.send({ type: 'alerts', added: [], updated: [], removed: [] });
    // Every event is delivered once.
    expect(t.events.filter((e) => e === 'alerts')).toHaveLength(1);
    t.live.stop();
  });

  it('ignores a replaced socket’s late events', () => {
    const t = setup();
    t.live.start();
    const first = t.sockets[0];
    first.open();
    vi.advanceTimersByTime(70_000);
    first.send({ type: 'stations', updated: '', count: 1 });
    expect(t.events).not.toContain('stations');
    t.live.stop();
  });

  it('backs off against a server that keeps closing (1013), resetting only after a stable connection', () => {
    const t = setup();
    t.live.start();
    const delays: number[] = [];
    // Each connection opens and is closed by the server straight away.
    for (let i = 0; i < 8; i++) {
      const s = t.sockets.at(-1)!;
      s.open();
      s.drop();
      const before = Date.now();
      vi.advanceTimersToNextTimer();
      // The watchdog interval also fires; step until a new socket exists.
      while (t.sockets.at(-1) === s) vi.advanceTimersToNextTimer();
      delays.push(Date.now() - before);
    }
    expect(delays[0]).toBeLessThan(2_000);
    expect(delays.at(-1)!).toBeGreaterThan(20_000);
    // A connection that stays up resets the backoff.
    const s = t.sockets.at(-1)!;
    s.open();
    for (let i = 0; i < 2; i++) {
      vi.advanceTimersByTime(20_000);
      s.send({ type: 'heartbeat', server_time: '' });
    }
    s.drop();
    const before = Date.now();
    while (t.sockets.at(-1) === s) vi.advanceTimersToNextTimer();
    expect(Date.now() - before).toBeLessThan(2_000);
    t.live.stop();
  });
});
