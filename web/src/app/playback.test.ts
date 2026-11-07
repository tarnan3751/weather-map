import { afterEach, describe, expect, it, vi } from 'vitest';
import type { LayerSpec } from '../api/types.ts';
import { MIN } from '../core/timeline.ts';
import { T0, tframe as frame } from '../testing.ts';
import { Playback } from './playback.ts';
import { createAppStore } from './state.ts';

afterEach(() => vi.unstubAllGlobals());

describe('playback', () => {
  it('keeps its place by time when a live frame is inserted behind the playhead', () => {
    let step: FrameRequestCallback | null = null;
    vi.stubGlobal('requestAnimationFrame', (f: FrameRequestCallback) => {
      step = f;
      return 1;
    });
    vi.stubGlobal('cancelAnimationFrame', () => {});
    vi.useFakeTimers({ now: T0 + 60 * MIN, toFake: ['Date'] });
    const store = createAppStore();
    const layer = { id: 'radar' } as LayerSpec;
    const frames = Array.from({ length: 10 }, (_, k) => frame(T0 + k * 5 * MIN));
    store.set({ layers: [layer], layerId: 'radar', clockOffset: 0, timelines: { radar: { frames, parts: new Map(), unknown: 0 } } });
    const playback = new Playback(store, () => null, () => false);
    playback.play();
    let now = performance.now();
    const tick = () => step!((now += 50));
    while ((store.get().playhead ?? 0) < 3.2) tick();
    const before = frames[Math.floor(store.get().playhead!)].t;
    expect(before).toBe(T0 + 15 * MIN);
    // A late frame lands between the second and third.
    const inserted = [...frames.slice(0, 2), frame(T0 + 7 * MIN), ...frames.slice(2)];
    store.set({ timelines: { radar: { frames: inserted, parts: new Map(), unknown: 0 } } });
    tick();
    const p = store.get().playhead!;
    expect(inserted[Math.floor(p)].t).toBe(T0 + 15 * MIN);
    expect(p).toBeGreaterThan(4);
    // The loop's end followed the newest frame to its new index.
    expect(playback.loop).toEqual([0, 10]);
    vi.useRealTimers();
  });
});
