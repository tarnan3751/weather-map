// Every layer's timeline at a given moment, assembled from parts the way
// `wx_core::frame::FrameCatalog` does it (see the rules at the top of
// crates/wx-core/src/frame.rs): one part per source and valid time from the
// newest run, observed parts on rounded-up ticks up to the newest observation,
// forecasts after it (or before it too when they fill the past), each
// source's newest part standing in for later ticks within its hold, parts
// stacked by rank, expiry once a part can no longer reach the window, and
// anchored accumulations on the 12-hourly runs. When the clock moves,
// diffing two moments gives the `timeline` events the server would have sent.
// It also answers the question the tile server asks: is this stack one some
// frame has (or had in the last four minutes) for this tile?

import type { Frame, LayerSpec, Part, Timeline, WsEvent } from '../src/api/types.ts';
import { MAX_COMBO_PARTS, stackAcceptable } from './boxes.ts';
import { LAYERS } from './registry.ts';
import { type MockSource, SOURCES } from './sources.ts';
import { HOUR, MIN, iso, pad } from './util.ts';

const FORECAST_GRACE_MIN = 90;
/** Stacks stay fetchable this long after their frame changes or goes. */
export const SERVE_GRACE_MS = 240_000;
/** Observations stamped further ahead of now than this are refused. */
const MAX_OBSERVED_AHEAD_MIN = 5;
/** Anchored layers take only runs starting on a multiple of this. */
export const ANCHOR_CYCLE_H = 12;

const yymmddHH = (d: Date) => `${pad(d.getUTCFullYear() % 100)}${pad(d.getUTCMonth() + 1)}${pad(d.getUTCDate())}${pad(d.getUTCHours())}`;
const floorTo = (ms: number, step: number) => Math.floor(ms / step) * step;

function part(src: MockSource, stamp: string, valid: number, run: number | null): Part {
  return {
    id: `${src.id}-${stamp}`,
    source: src.id,
    valid: iso(valid),
    run: run === null ? null : iso(run),
    kind: run === null ? 'observed' : 'forecast',
    bounds: src.bounds,
    solid: src.solid,
    max_zoom: src.max_zoom,
  };
}

const observedPart = (src: MockSource, valid: number) => part(src, `${yymmddHH(new Date(valid))}${pad(new Date(valid).getUTCMinutes())}`, valid, null);

export function forecastPart(src: MockSource, run: number, leadMin: number): Part {
  const d = new Date(run);
  // A run off the hour (a nowcast) is written with its minute.
  const stamp = run % HOUR === 0 ? yymmddHH(d) : `${yymmddHH(d)}${pad(d.getUTCMinutes())}`;
  return part(src, `${stamp}-${leadMin}`, run + leadMin * MIN, run);
}

// ---------------------------------------------------------------------------
// Feeds: when each source's data exists
// ---------------------------------------------------------------------------

/** New data every `everyMin` minutes (for a model, a run), `latencyMin` late. */
type Feed =
  | { kind: 'observed'; everyMin: number; latencyMin: number }
  | { kind: 'forecast'; everyMin: number; latencyMin: number; runs: number; leads: (runHour: number) => number[] };

const hourly = (toH: number) => Array.from({ length: toH }, (_, i) => (i + 1) * 60);
const every = (stepMin: number, toMin: number) => Array.from({ length: Math.floor(toMin / stepMin) }, (_, i) => (i + 1) * stepMin);
const synoptic = (h: number) => h % 6 === 0;
const observed = (everyMin: number, latencyMin: number): Feed => ({ kind: 'observed', everyMin, latencyMin });
const forecast = (everyMin: number, latencyMin: number, runs: number, leads: (runHour: number) => number[]): Feed => ({ kind: 'forecast', everyMin, latencyMin, runs, leads });

/** How a source feeds a layer in the mock; null if it does not. */
export function feedFor(layer: LayerSpec, src: MockSource): Feed | null {
  if (!layer.inputs.some((i) => i.family === src.family)) return null;
  const subhourly = layer.inputs.find((i) => i.family === 'hrrr')?.product === 'wrfsubh';
  const hrrrLeads = (h: number) => (subhourly ? every(15, 18 * 60) : hourly(synoptic(h) ? 48 : 18));
  switch (src.id) {
    case 'mrms':
      return layer.id === 'radar' ? observed(2, 1) : layer.id === 'rain_1h' ? observed(10, 2) : observed(60, 6);
    case 'opera':
      return observed(5, 3);
    case 'cmorph2':
      // Half-hourly, about an hour behind.
      return observed(30, 60);
    case 'nowcast':
      return forecast(10, 3, 3, () => every(10, 120));
    case 'rtma':
      return observed(60, 35);
    case 'goes19':
    case 'goes18':
    case 'himawari9':
      return observed(10, 12);
    case 'mtg_i1':
    case 'msg_iodc':
      // Fifteen-minute scans on a ten-minute layer: held across ticks.
      return observed(15, 14);
    case 'hrrr':
      return forecast(60, 85, layer.retention_minutes > 0 || layer.anchored ? 14 : 7, hrrrLeads);
    case 'hrrr_ak':
      return forecast(180, 110, 6, hrrrLeads);
    case 'icon_eu':
      return forecast(180, 240, 6, (h) => hourly(synoptic(h) ? 78 : 30));
    case 'gfs':
      return forecast(360, 280, 4, () => [...hourly(120), ...every(180, 168 * 60).filter((m) => m > 120 * 60)]);
    default:
      return null;
  }
}

/** The newest observation, or the newest run whose output is in, at `now`. */
export const newestData = (feed: Feed, now: number) => floorTo(now - feed.latencyMin * MIN, feed.everyMin * MIN);

// ---------------------------------------------------------------------------
// Assembling a timeline
// ---------------------------------------------------------------------------

interface Held {
  part: Part;
  v: number;
  tick: number;
  rank: number;
  hold: number;
  fillPast: boolean;
}

export function assemble(layer: LayerSpec, now: number): Timeline {
  const window = layer.retention_minutes * MIN;
  const tickMs = layer.observed_tick_minutes * MIN;
  const bySource = new Map<string, Held[]>();
  let anchor: number | null = null;
  if (layer.anchored) {
    // The base source (lowest rank with an input here) sets the anchor. A
    // run's parts all arrive at once in the mock, so its newest 12-hourly
    // run covers every lead the previous one had as soon as it is in: the
    // anchor is that run, and every other source joins with its run of the
    // same moment.
    const base = [...SOURCES].reverse().find((s) => feedFor(layer, s)?.kind === 'forecast');
    if (base) anchor = floorTo(now - feedFor(layer, base)!.latencyMin * MIN, ANCHOR_CYCLE_H * HOUR);
  }
  for (const src of SOURCES) {
    const feed = feedFor(layer, src);
    const input = layer.inputs.find((i) => i.family === src.family);
    if (!feed || !input) continue;
    const parts = new Map<number, Held>();
    const hold = input.hold_minutes * MIN;
    if (feed.kind === 'observed') {
      const step = feed.everyMin * MIN;
      // Kept while it can still stand in for a tick inside the window.
      const latest = Math.min(newestData(feed, now), now + MAX_OBSERVED_AHEAD_MIN * MIN);
      for (let v = latest; v + hold > now - window; v -= step) {
        parts.set(v, { part: observedPart(src, v), v, tick: Math.ceil(v / tickMs) * tickMs, rank: src.rank, hold, fillPast: false });
      }
    } else {
      const latest = anchor ?? newestData(feed, now);
      const every = anchor !== null ? ANCHOR_CYCLE_H * 60 : feed.everyMin;
      // Newest run first: one part per valid time, from the newest run that has it.
      for (let k = 0; k < feed.runs; k++) {
        const run = latest - k * every * MIN;
        if (anchor !== null && (run !== anchor || run > newestData(feed, now))) continue;
        for (const lead of feed.leads(new Date(run).getUTCHours())) {
          const v = run + lead * MIN;
          if (parts.has(v)) continue;
          if (input.fill_past ? v + hold <= now - window : v < now - FORECAST_GRACE_MIN * MIN) continue;
          parts.set(v, { part: forecastPart(src, run, lead), v, tick: v, rank: src.rank, hold, fillPast: input.fill_past });
        }
      }
    }
    bySource.set(src.id, [...parts.values()].sort((a, b) => a.v - b.v));
  }
  let latestObs = -Infinity;
  for (const list of bySource.values()) for (const h of list) if (h.part.kind === 'observed') latestObs = Math.max(latestObs, h.tick);
  // Forecasts that can no longer appear in any frame.
  for (const [id, list] of bySource) bySource.set(id, list.filter((h) => h.part.kind === 'observed' || h.fillPast || h.v + h.hold > latestObs));
  // Observations up to the newest one, forecasts after it (or before it too
  // when they fill the past): every frame after it is forecasts only.
  const takesPart = (h: Held, t: number) => (h.part.kind === 'observed' ? t <= latestObs : t > latestObs || h.fillPast);
  const ticks = new Set<number>();
  // Ticks up to the newest observation must be inside the window: parts
  // kept past it only stand in for its first ticks. Later ticks (and every
  // tick of a layer with no observations) are bounded by the forecast grace
  // when parts are picked, not by the window.
  const inWindow = (t: number) => t > latestObs || t >= now - window;
  for (const list of bySource.values()) for (const h of list) if (takesPart(h, h.tick) && inWindow(h.tick)) ticks.add(h.tick);
  const frames: Frame[] = [];
  const named = new Map<string, Part>();
  for (const t of [...ticks].sort((a, b) => a - b)) {
    const stack: Held[] = [];
    for (const list of bySource.values()) {
      let best: Held | null = null;
      for (const h of list) {
        if (h.v > t) break;
        if (takesPart(h, t)) best = h;
      }
      if (best && t - best.v < best.hold) stack.push(best);
    }
    if (!stack.length) continue;
    stack.sort((a, b) => b.rank - a.rank || (a.part.source < b.part.source ? -1 : 1));
    for (const h of stack) named.set(h.part.id, h.part);
    frames.push({ valid: iso(t), kind: t <= latestObs ? 'observed' : 'forecast', parts: stack.map((h) => h.part.id) });
  }
  return { frames, parts: [...named.values()] };
}

// ---------------------------------------------------------------------------
// The catalog
// ---------------------------------------------------------------------------

interface LayerState {
  frames: Frame[];
  parts: Map<string, Part>;
}

function layerState(layer: LayerSpec, now: number): LayerState {
  const tl = assemble(layer, now);
  return { frames: tl.frames, parts: new Map(tl.parts.map((p) => [p.id, p])) };
}

export class Catalog {
  private timelines = new Map<string, LayerState>();
  /** Part lists of frames that changed or left their timeline, and when. */
  private retired: { layer: string; at: number; parts: Part[] }[] = [];

  constructor(now: number) {
    for (const l of LAYERS) this.timelines.set(l.id, layerState(l, now));
  }

  timeline(layer: string): Timeline {
    const t = this.timelines.get(layer);
    return { frames: t?.frames ?? [], parts: t ? [...t.parts.values()] : [] };
  }

  all(): Record<string, Timeline> {
    return Object.fromEntries(LAYERS.map((l) => [l.id, this.timeline(l.id)]));
  }

  frames(layer: string): Frame[] {
    return this.timelines.get(layer)?.frames ?? [];
  }

  /** Move to `now`, returning one `timeline` event per layer that changed. */
  advance(now: number): WsEvent[] {
    this.retired = this.retired.filter((r) => now - r.at < SERVE_GRACE_MS);
    const events: WsEvent[] = [];
    for (const layer of LAYERS) {
      const before = this.timelines.get(layer.id)!;
      const after = layerState(layer, now);
      this.timelines.set(layer.id, after);
      const retire = (f: Frame) => this.retired.push({ layer: layer.id, at: now, parts: f.parts.map((id) => before.parts.get(id)!).filter(Boolean) });
      const old = new Map(before.frames.map((f) => [f.valid, f]));
      const changed: Frame[] = [];
      for (const f of after.frames) {
        const o = old.get(f.valid);
        old.delete(f.valid);
        if (o && o.kind === f.kind && o.parts.join(',') === f.parts.join(',')) continue;
        changed.push(f);
        if (o) retire(o);
      }
      const removed = [...old.keys()];
      for (const f of old.values()) retire(f);
      if (!changed.length && !removed.length) continue;
      const newParts = new Map<string, Part>();
      for (const f of changed) for (const id of f.parts) if (!before.parts.has(id)) newParts.set(id, after.parts.get(id)!);
      events.push({ type: 'timeline', layer: layer.id, parts: [...newParts.values()], frames: changed, removed });
    }
    return events;
  }

  /** Would the server render this tile? Mirrors the 404 rules. */
  checkStack(layer: string, z: number, x: number, y: number, ids: string[], now: number): 'ok' | 'unknown_part' | 'no_frame' | 'too_deep' {
    const tl = this.timelines.get(layer);
    if (!tl || !ids.length || ids.length > MAX_COMBO_PARTS) return 'no_frame';
    const parts: Part[] = [];
    for (const id of ids) {
      const p = this.part(layer, id, now);
      if (!p) return 'unknown_part';
      parts.push(p);
    }
    const candidates = tl.frames.filter((f) => f.parts.includes(ids[0])).map((f) => f.parts.map((id) => tl.parts.get(id)!));
    for (const r of this.retiredAt(layer, now)) if (r.parts.some((p) => p.id === ids[0])) candidates.push(r.parts);
    if (!candidates.some((c) => stackAcceptable(c, z, x, y, ids))) return 'no_frame';
    if (z > Math.max(...parts.map((p) => p.max_zoom))) return 'too_deep';
    return 'ok';
  }

  private retiredAt(layer: string, now: number) {
    return this.retired.filter((r) => r.layer === layer && now - r.at < SERVE_GRACE_MS);
  }

  /** A part by id, current or recently retired. */
  part(layer: string, id: string, now: number): Part | undefined {
    return this.timelines.get(layer)?.parts.get(id) ?? this.retiredAt(layer, now).flatMap((r) => r.parts).find((p) => p.id === id);
  }
}
