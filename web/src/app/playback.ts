// Animation through a layer's frames. The playhead moves at a steady number
// of frames per second (the slider gives every frame the same width, so the
// thumb moves steadily too). Each step holds its frame, then cross-fades into
// the next; it waits, briefly, for the next frame's tiles rather than fading
// into a half-loaded frame.

import type { Store } from '../core/store.ts';
import { loopRange, nearestIndex, type TFrame } from '../core/timeline.ts';
import { currentFrames, selectedIndex, serverNow, type AppState } from './state.ts';

/** Fraction of a step spent holding the frame before the fade starts. */
const FADE_START = 0.55;
const BASE_FPS = 5;
const END_DWELL_S = 1.1;
const MAX_WAIT_S = 2.5;

interface ReadinessProbe {
  isReadyIndex(i: number): boolean;
}

export class Playback {
  private raf = 0;
  private last = 0;
  private dwell = 0;
  private waited = 0;
  private readonly store: Store<AppState>;
  private readonly probe: () => ReadinessProbe | null;
  private readonly reducedMotion: () => boolean;
  loop: [number, number] = [0, -1];
  /** The frames the playhead and the loop are indices into: when a live
   *  update inserts or drops frames, both are moved to the same times. */
  private frames: readonly TFrame[] = [];

  constructor(store: Store<AppState>, probe: () => ReadinessProbe | null, reducedMotion: () => boolean) {
    this.store = store;
    this.probe = probe;
    this.reducedMotion = reducedMotion;
  }

  play() {
    const s = this.store.get();
    const frames = currentFrames(s);
    if (frames.length < 2) return;
    const idx = selectedIndex(s);
    this.loop = loopRange(frames, idx, serverNow(s));
    const [lo, hi] = this.loop;
    const start = idx >= hi || idx < lo ? lo : idx;
    this.dwell = 0;
    this.waited = 0;
    this.frames = frames;
    this.store.set({ playing: true, following: false, playhead: start });
    this.last = performance.now();
    cancelAnimationFrame(this.raf);
    this.raf = requestAnimationFrame(this.tick);
  }

  pause() {
    cancelAnimationFrame(this.raf);
    const s = this.store.get();
    if (!s.playing) return;
    const frames = currentFrames(s);
    const i = Math.max(0, Math.min(frames.length - 1, Math.round(s.playhead ?? 0)));
    this.store.set({ playing: false, playhead: null, targetTime: frames[i]?.t ?? s.targetTime });
  }

  toggle() {
    if (this.store.get().playing) this.pause();
    else this.play();
  }

  private tick = (now: number) => {
    const s = this.store.get();
    if (!s.playing) return;
    const frames = currentFrames(s);
    if (frames.length < 2) {
      this.pause();
      return;
    }
    const dt = Math.min(0.1, (now - this.last) / 1000);
    this.last = now;
    let p = s.playhead ?? this.loop[0];
    // Frames come and go while playing (a new observation, an old one
    // expiring): the playhead and the loop's ends keep their times, not their
    // indices, so a frame inserted behind the playhead does not step it back.
    if (frames !== this.frames) {
      p = this.reanchor(p, frames);
      const [l, h] = this.loop.map((i) => this.reanchor(i, frames));
      this.loop = [Math.round(l), Math.round(h)];
      this.frames = frames;
    }
    let [lo, hi] = this.loop;
    hi = Math.min(hi, frames.length - 1);
    lo = Math.min(lo, hi);
    this.loop = [lo, hi];
    const reduced = this.reducedMotion();
    const fps = (reduced ? 2 : BASE_FPS) * s.speed;

    if (this.dwell > 0) {
      this.dwell -= dt;
      if (this.dwell <= 0) p = lo;
    } else {
      let next = p + dt * fps;
      if (next >= hi) {
        next = hi;
        this.dwell = END_DWELL_S;
      } else {
        const i = Math.floor(next);
        const f = next - i;
        const probe = this.probe();
        if (!reduced && f > FADE_START && probe && !probe.isReadyIndex(i + 1) && this.waited < MAX_WAIT_S) {
          next = i + FADE_START;
          this.waited += dt;
        } else if (Math.floor(next) !== Math.floor(p)) {
          this.waited = 0;
        }
        if (reduced) next = Math.min(hi, next);
      }
      p = next;
    }
    this.store.set({ playhead: p });
    this.raf = requestAnimationFrame(this.tick);
  };

  /** Position `p` in the old frames, as the same time in `next`. */
  private reanchor(p: number, next: readonly TFrame[]): number {
    const old = this.frames;
    const i = Math.max(0, Math.min(old.length - 1, Math.floor(p)));
    if (!old[i] || !next.length) return p;
    const j = next[i]?.t === old[i].t ? i : nearestIndex(next, old[i].t);
    return Math.min(next.length - 1, j + (p - Math.floor(p)));
  }

  /** Cross-fade weight between frame floor(p) and the next. */
  static mix(p: number, reduced: boolean): number {
    if (reduced) return 0;
    const f = p - Math.floor(p);
    const t = Math.max(0, Math.min(1, (f - FADE_START) / (1 - FADE_START)));
    return t * t * (3 - 2 * t);
  }
}
