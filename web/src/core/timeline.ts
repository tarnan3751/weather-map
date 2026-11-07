// A layer's timeline as the client sees it: frames sorted by valid time, each
// with its parts resolved from the layer's dictionary of parts, the edits the
// WebSocket asks for, and the slider's time scale.

import type { Frame, Iso, Part, Timeline } from '../api/types.ts';

export const MIN = 60_000;
export const HOUR = 60 * MIN;

export interface TFrame extends Frame {
  /** `valid` in epoch milliseconds: the frame's identity. */
  t: number;
  /** `parts` resolved, top first. Ids the dictionary does not know are left
   *  out (see `LayerTimeline.unknown`). */
  stack: readonly Part[];
}

/** A layer's frames and the dictionary of every part they name. Frame
 *  objects are never changed in place: a changed time step is a new object,
 *  so anything keyed by frame identity stays valid. */
export interface LayerTimeline {
  frames: TFrame[];
  parts: ReadonlyMap<string, Part>;
  /** Part ids some frame names that no message described: a sign the client
   *  lost track and should fetch the timeline again. */
  unknown: number;
}

/** RFC 3339 to epoch ms. The server may send microseconds, which some
 *  engines refuse to parse; milliseconds are all that matter here. */
export function parseTime(iso: string): number {
  return Date.parse(iso.replace(/(\.\d{3})\d+/, '$1'));
}

function toTFrame(f: Frame, parts: ReadonlyMap<string, Part>): TFrame {
  const stack: Part[] = [];
  for (const id of f.parts) {
    const p = parts.get(id);
    if (p) stack.push(p);
  }
  return { valid: f.valid, kind: f.kind, parts: f.parts, t: parseTime(f.valid), stack };
}

function finish(byT: Map<number, TFrame>, dict: Map<string, Part>): LayerTimeline {
  const frames = [...byT.values()].sort((a, b) => a.t - b.t);
  // Parts no frame names any more are forgotten; their tiles stay in the
  // cache (keyed by URL) until evicted.
  const named = new Set<string>();
  let unknown = 0;
  for (const f of frames) {
    for (const id of f.parts) named.add(id);
    unknown += f.parts.length - f.stack.length;
  }
  for (const id of [...dict.keys()]) if (!named.has(id)) dict.delete(id);
  return { frames, parts: dict, unknown };
}

/** A timeline from `/api/frames`. */
export function parseTimeline(tl: Timeline | undefined): LayerTimeline {
  const dict = new Map((tl?.parts ?? []).map((p) => [p.id, p] as const));
  const byT = new Map<number, TFrame>();
  for (const f of tl?.frames ?? []) {
    const tf = toTFrame(f, dict);
    if (Number.isFinite(tf.t)) byT.set(tf.t, tf);
  }
  return finish(byT, dict);
}

/**
 * Apply a `timeline` event: learn its parts, drop the `removed` time steps,
 * then add or replace `frames` by valid time. Unchanged frames keep their
 * objects. Returns a new timeline; `tl` is not modified.
 */
export function applyTimeline(tl: LayerTimeline, ev: { parts: readonly Part[]; frames: readonly Frame[]; removed: readonly Iso[] }): LayerTimeline {
  const dict = new Map(tl.parts);
  for (const p of ev.parts) dict.set(p.id, p);
  const byT = new Map(tl.frames.map((f) => [f.t, f] as const));
  for (const v of ev.removed) byT.delete(parseTime(v));
  for (const f of ev.frames) {
    const tf = toTFrame(f, dict);
    if (!Number.isFinite(tf.t)) continue;
    const old = byT.get(tf.t);
    // A repeated frame (a replayed event) keeps its object, so nothing that
    // keys on it starts over.
    const same = old && old.kind === tf.kind && old.parts.length === tf.parts.length && old.parts.every((id, i) => id === tf.parts[i]) && old.stack.length === tf.stack.length;
    byT.set(tf.t, same ? old : tf);
  }
  return finish(byT, dict);
}

/** Index of the frame closest in time to `t` (the earlier one on a tie), or
 *  -1 for an empty timeline. */
export function nearestIndex(frames: readonly TFrame[], t: number): number {
  if (!frames.length) return -1;
  let lo = 0;
  let hi = frames.length - 1;
  if (t <= frames[lo].t) return lo;
  if (t >= frames[hi].t) return hi;
  while (hi - lo > 1) {
    const mid = (lo + hi) >> 1;
    if (frames[mid].t <= t) lo = mid;
    else hi = mid;
  }
  return t - frames[lo].t <= frames[hi].t - t ? lo : hi;
}

/** Newest observed frame, or -1. */
export function latestObservedIndex(frames: readonly TFrame[]): number {
  for (let i = frames.length - 1; i >= 0; i--) if (frames[i].kind === 'observed') return i;
  return -1;
}

/** An observation this recent is "now" whatever comes after it: a radar
 *  scan minutes old, rather than a nowcast or a model a minute off the clock. */
const FRESH_OBSERVATION_MS = 15 * MIN;

/**
 * The frame "now" means for a layer: a fresh newest observation; otherwise
 * the frame nearest the clock from the newest observation on (never an older
 * one), the observation on a tie. On an hourly layer at 22:36 that is the
 * 23:00 forecast rather than the 22:00 analysis.
 */
export function nowIndex(frames: readonly TFrame[], now: number): number {
  const obs = latestObservedIndex(frames);
  if (obs >= 0 && now - frames[obs].t <= FRESH_OBSERVATION_MS) return obs;
  const from = Math.max(0, obs);
  return from + nearestIndex(frames.slice(from), now);
}

// ---------------------------------------------------------------------------
// Slider scale
// ---------------------------------------------------------------------------

/**
 * Frames arrive 2 minutes apart for radar, 15 minutes or an hour apart for the
 * forecast, and 3 hours apart in the long range. A linear time axis would
 * squeeze two hours of radar into a sliver of a week, so the slider gives every
 * frame step the same width instead, which follows the data's own density: the
 * radar loop gets about a third of the track and day 7 is still reachable.
 * A step that is much longer than its neighbours (a missing hour) is widened
 * in proportion so gaps still look like gaps.
 */
export interface SliderScale {
  /** Fraction 0..1 along the track for each frame. */
  pos: number[];
  times: number[];
  /** Track fraction of a time; clamps outside the timeline. */
  x(t: number): number;
  /** Time at a track fraction. */
  t(x: number): number;
  /** Frame index nearest a track fraction. */
  indexAt(x: number): number;
}

const MAX_GAP_WEIGHT = 4;

function median(xs: number[]): number {
  if (!xs.length) return NaN;
  const s = [...xs].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
}

export function stepWeights(times: readonly number[]): number[] {
  const steps: number[] = [];
  for (let i = 1; i < times.length; i++) steps.push(Math.max(0, times[i] - times[i - 1]));
  return steps.map((dt, i) => {
    const before = median(steps.slice(Math.max(0, i - 3), i));
    const after = median(steps.slice(i + 1, i + 4));
    const typical = Math.max(Number.isNaN(before) ? 0 : before, Number.isNaN(after) ? 0 : after);
    if (!(typical > 0) || dt <= typical * 1.75) return 1;
    return Math.min(MAX_GAP_WEIGHT, dt / typical);
  });
}

export function buildScale(frames: readonly { t: number }[]): SliderScale {
  const times = frames.map((f) => f.t);
  const n = times.length;
  const pos: number[] = new Array(n).fill(0);
  if (n > 1) {
    const w = stepWeights(times);
    const total = w.reduce((a, b) => a + b, 0);
    let acc = 0;
    for (let i = 1; i < n; i++) {
      acc += w[i - 1];
      pos[i] = acc / total;
    }
  } else if (n === 1) {
    pos[0] = 0.5;
  }

  // Time to track fraction and back: piecewise linear between the frames.
  const interpolate = (from: number[], to: number[], v: number): number => {
    if (n === 0) return 0;
    if (n === 1 || v <= from[0]) return to[0];
    if (v >= from[n - 1]) return to[n - 1];
    let lo = 0;
    let hi = n - 1;
    while (hi - lo > 1) {
      const mid = (lo + hi) >> 1;
      if (from[mid] <= v) lo = mid;
      else hi = mid;
    }
    const span = from[hi] - from[lo];
    const f = span > 0 ? (v - from[lo]) / span : 0;
    return to[lo] + f * (to[hi] - to[lo]);
  };

  const indexAt = (xf: number): number => {
    if (n === 0) return -1;
    let best = 0;
    let bestD = Infinity;
    // Frames are sorted along the track too, so a binary search would do; the
    // linear scan is fine for a few hundred frames and simpler to trust.
    for (let i = 0; i < n; i++) {
      const d = Math.abs(pos[i] - xf);
      if (d < bestD) {
        bestD = d;
        best = i;
      }
    }
    return best;
  };

  return { pos, times, x: (t) => interpolate(times, pos, t), t: (xf) => interpolate(pos, times, xf), indexAt };
}

// ---------------------------------------------------------------------------
// Ticks
// ---------------------------------------------------------------------------

export interface Tick {
  x: number;
  t: number;
  label: string;
  /** Midnight in the viewer's time zone. */
  day: boolean;
}

const partFormats = new Map<string, Intl.DateTimeFormat>();

/** The numeric parts of a moment in a zone (undefined: the browser's), by a
 *  formatter made once per zone; with the date only when asked, as it costs. */
function partsIn(t: number, timeZone: string | undefined, withDate: boolean): Record<string, string> {
  const key = `${timeZone ?? ''}|${withDate}`;
  let f = partFormats.get(key);
  if (!f) {
    const date = withDate ? ({ year: 'numeric', month: '2-digit', day: '2-digit' } as const) : {};
    f = new Intl.DateTimeFormat('en-US', { timeZone, ...date, hour: 'numeric', minute: '2-digit', hourCycle: 'h23' });
    partFormats.set(key, f);
  }
  const p: Record<string, string> = {};
  for (const x of f.formatToParts(t)) p[x.type] = x.value;
  return p;
}

/** Local date (`YYYY-MM-DD`), hour and minute of a moment in a zone. */
export function clockIn(t: number, timeZone: string | undefined): { date: string; hour: number; minute: number } {
  const p = partsIn(t, timeZone, true);
  return { date: `${p.year}-${p.month}-${p.day}`, hour: Number(p.hour) % 24, minute: Number(p.minute) };
}

export const sameDay = (a: number, b: number, timeZone?: string) => clockIn(a, timeZone).date === clockIn(b, timeZone).date;

/** Every local whole hour from t0 to t1 in a zone, with its hour of the day.
 *  Steps by quarter hours, because some zones sit 30 or 45 minutes off UTC. */
export function localHours(t0: number, t1: number, timeZone: string | undefined): { t: number; hour: number }[] {
  const out: { t: number; hour: number }[] = [];
  const step = 15 * MIN;
  for (let t = Math.ceil(t0 / step) * step; t <= t1; t += step) {
    const p = partsIn(t, timeZone, false);
    if (Number(p.minute) === 0) out.push({ t, hour: Number(p.hour) % 24 });
  }
  return out;
}

/**
 * Labelled ticks for a track `widthPx` wide. Candidates are the local hour
 * boundaries; midnights win, then noon, 6-hourly, 3-hourly and hourly marks,
 * each placed only if it keeps clear of the labels already placed.
 */
export function buildTicks(scale: SliderScale, widthPx: number, opts: { timeZone?: string; locale?: string; minGapPx?: number } = {}): Tick[] {
  const n = scale.times.length;
  if (n < 2 || widthPx <= 0) return [];
  const minGap = opts.minGapPx ?? 64;
  const dayParts = new Intl.DateTimeFormat(opts.locale, { timeZone: opts.timeZone, weekday: 'short', day: 'numeric' });
  // "Sat 7" in every locale: some put the day number first, which reads
  // oddly on a time axis.
  const dayLabel = (t: number) => {
    const p = dayParts.formatToParts(t);
    return `${p.find((x) => x.type === 'weekday')?.value ?? ''} ${p.find((x) => x.type === 'day')?.value ?? ''}`.trim();
  };
  const hourFmt = new Intl.DateTimeFormat(opts.locale, { timeZone: opts.timeZone, hour: 'numeric' });
  const rank = (hour: number) => (hour === 0 ? 0 : hour === 12 ? 1 : hour % 6 === 0 ? 2 : hour % 3 === 0 ? 3 : 4);
  const cands = localHours(scale.times[0], scale.times[n - 1], opts.timeZone).map(({ t, hour }) => ({ t, x: scale.x(t), rank: rank(hour), day: hour === 0 }));
  cands.sort((a, b) => a.rank - b.rank || a.t - b.t);

  const placed: typeof cands = [];
  for (const c of cands) {
    const px = c.x * widthPx;
    if (px < 8 || px > widthPx - 8) continue;
    if (placed.every((p) => Math.abs(p.x * widthPx - px) >= minGap)) placed.push(c);
  }
  placed.sort((a, b) => a.t - b.t);
  return placed.map((c) => ({ x: c.x, t: c.t, day: c.day, label: c.day ? dayLabel(c.t) : hourFmt.format(c.t) }));
}

/** Frames to play through: the stretch around `index` that is dense enough to
 *  animate (the radar loop and near-term forecast), not a week of 3-hourly steps. */
export function loopRange(frames: readonly TFrame[], index: number, now: number): [number, number] {
  const n = frames.length;
  if (n === 0) return [0, -1];
  const horizon = now + 12 * HOUR;
  const nearEnd = (() => {
    let e = n - 1;
    while (e > 0 && frames[e].t > horizon) e--;
    return e;
  })();
  if (index <= nearEnd || nearEnd === n - 1) return [0, nearEnd];
  return [Math.min(nearEnd + 1, n - 1), n - 1];
}
