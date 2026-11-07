import { describe, expect, it } from 'vitest';
import { assemble } from '../../mock/catalog.ts';
import { layerById } from '../../mock/registry.ts';
import type { Frame, Part, Timeline } from '../api/types.ts';
import { box, T0, WORLD } from '../testing.ts';
import {
  applyTimeline,
  buildScale,
  buildTicks,
  HOUR,
  type LayerTimeline,
  loopRange,
  MIN,
  nearestIndex,
  nowIndex,
  parseTimeline,
  stepWeights,
  type TFrame,
} from './timeline.ts';

const iso = (ms: number) => new Date(ms).toISOString().replace('.000Z', 'Z');
const CONUS = box(-130, 20, -60, 55);

const p = (id: string, source: string, valid: number, run: number | null, solid = false): Part => ({
  id,
  source,
  valid: iso(valid),
  run: run === null ? null : iso(run),
  kind: run === null ? 'observed' : 'forecast',
  bounds: source === 'gfs' || source === 'cmorph2' ? WORLD : CONUS,
  solid: solid ? WORLD : null,
  max_zoom: source === 'gfs' ? 4 : 7,
});
const mrms = (ms: number) => p(`mrms-${ms / MIN}`, 'mrms', ms, null);
const sat = (ms: number) => p(`cmorph2-${ms / MIN}`, 'cmorph2', ms, null, true);
const hrrr = (ms: number, run = T0 - 2 * HOUR) => p(`hrrr-${run / HOUR}-${(ms - run) / MIN}`, 'hrrr', ms, run);
const gfs = (ms: number, run = T0 - 6 * HOUR) => p(`gfs-${run / HOUR}-${(ms - run) / MIN}`, 'gfs', ms, run, true);
const frame = (ms: number, kind: Frame['kind'], parts: Part[]): Frame => ({ valid: iso(ms), kind, parts: parts.map((x) => x.id) });

/** Radar's shape: 2 h of 2-minute mosaics over a half-hourly satellite
 *  estimate, 18 h of 15-minute HRRR, then 3-hourly GFS to day 7. */
function radarTimeline(): Timeline {
  const frames: Frame[] = [];
  const parts = new Map<string, Part>();
  const add = (t: number, kind: Frame['kind'], ps: Part[]) => {
    for (const x of ps) parts.set(x.id, x);
    frames.push(frame(t, kind, ps));
  };
  for (let t = T0 - 120 * MIN; t <= T0; t += 2 * MIN) add(t, 'observed', [mrms(t), sat(Math.floor(t / (30 * MIN)) * 30 * MIN)]);
  for (let t = T0 + 15 * MIN; t <= T0 + 18 * HOUR; t += 15 * MIN) add(t, 'forecast', [hrrr(t), gfs(Math.floor(t / (3 * HOUR)) * 3 * HOUR)]);
  for (let t = T0 + 21 * HOUR; t <= T0 + 168 * HOUR; t += 3 * HOUR) add(t, 'forecast', [gfs(t)]);
  return { frames, parts: [...parts.values()] };
}
const radarFrames = (): TFrame[] => parseTimeline(radarTimeline()).frames;
const tl = (frames: Frame[], parts: Part[]): LayerTimeline => parseTimeline({ frames, parts });

describe('nearest frame', () => {
  const frames = radarFrames();

  it('snaps to the closest valid time, earlier on a tie', () => {
    expect(nearestIndex([], T0)).toBe(-1);
    expect(frames[nearestIndex(frames, T0 - 3 * MIN)].t).toBe(T0 - 4 * MIN);
    expect(frames[nearestIndex(frames, T0 - 2.9 * MIN)].t).toBe(T0 - 2 * MIN);
    expect(frames[nearestIndex(frames, T0 + 7 * MIN)].t).toBe(T0);
    expect(frames[nearestIndex(frames, T0 + 9 * MIN)].t).toBe(T0 + 15 * MIN);
    expect(nearestIndex(frames, T0 - 10 * HOUR)).toBe(0);
    expect(nearestIndex(frames, T0 + 30 * 24 * HOUR)).toBe(frames.length - 1);
  });

  it('carries a chosen time across layers with different spacing', () => {
    const hourly = tl(
      [frame(T0 - HOUR, 'observed', [mrms(T0 - HOUR)]), frame(T0, 'observed', [mrms(T0)]), frame(T0 + HOUR, 'forecast', [hrrr(T0 + HOUR)]), frame(T0 + 2 * HOUR, 'forecast', [hrrr(T0 + 2 * HOUR)]), frame(T0 + 6 * HOUR, 'forecast', [gfs(T0 + 6 * HOUR)])],
      [mrms(T0 - HOUR), mrms(T0), hrrr(T0 + HOUR), hrrr(T0 + 2 * HOUR), gfs(T0 + 6 * HOUR)],
    ).frames;
    const chosen = frames[nearestIndex(frames, T0 + 75 * MIN)].t;
    expect(hourly[nearestIndex(hourly, chosen)].t).toBe(T0 + HOUR);
    expect(hourly[nearestIndex(hourly, T0 + 4.2 * HOUR)].t).toBe(T0 + 6 * HOUR);
  });

  it('means the frame nearest the clock from the newest observation on by "now"', () => {
    expect(frames[nowIndex(frames, T0 + 60_000)].t).toBe(T0);
    // Hourly: at 36 past, the next hour's forecast is nearer than the analysis.
    const hourly = tl(
      [frame(T0 - HOUR, 'observed', [mrms(T0 - HOUR)]), frame(T0, 'observed', [mrms(T0)]), frame(T0 + HOUR, 'forecast', [hrrr(T0 + HOUR)])],
      [mrms(T0 - HOUR), mrms(T0), hrrr(T0 + HOUR)],
    ).frames;
    expect(hourly[nowIndex(hourly, T0 + 36 * MIN)].t).toBe(T0 + HOUR);
    expect(hourly[nowIndex(hourly, T0 + 24 * MIN)].t).toBe(T0);
    expect(hourly[nowIndex(hourly, T0 + 30 * MIN)].t).toBe(T0);
    // Observations running ahead of a slow clock: still never before the newest.
    expect(hourly[nowIndex(hourly, T0 - 50 * MIN)].t).toBe(T0);
    // A radar scan minutes old is now, though a nowcast step is nearer.
    const radar = tl([frame(T0, 'observed', [mrms(T0)]), frame(T0 + 10 * MIN, 'forecast', [hrrr(T0 + 10 * MIN)])], [mrms(T0), hrrr(T0 + 10 * MIN)]).frames;
    expect(radar[nowIndex(radar, T0 + 9 * MIN)].t).toBe(T0);
    // Not once the scans have stopped.
    expect(radar[nowIndex(radar, T0 + 16 * MIN)].t).toBe(T0 + 10 * MIN);
    const parts = [hrrr(T0 - HOUR, T0 - 3 * HOUR), hrrr(T0, T0 - 3 * HOUR), hrrr(T0 + HOUR, T0 - 3 * HOUR)];
    const forecastOnly = tl(parts.map((x) => frame(Date.parse(x.valid), 'forecast', [x])), parts).frames;
    expect(nowIndex(forecastOnly, T0 + 20 * MIN)).toBe(1);
  });
});

describe('timeline events', () => {
  const base = () => tl([frame(T0 - 2 * MIN, 'observed', [mrms(T0 - 2 * MIN), sat(T0 - 30 * MIN)]), frame(T0, 'observed', [mrms(T0), sat(T0 - 30 * MIN)]), frame(T0 + 15 * MIN, 'forecast', [hrrr(T0 + 15 * MIN), gfs(T0)])], [mrms(T0 - 2 * MIN), mrms(T0), sat(T0 - 30 * MIN), hrrr(T0 + 15 * MIN), gfs(T0)]);

  it('resolves every frame’s parts, top first, from one dictionary', () => {
    const b = base();
    expect(b.frames.map((f) => f.stack.map((x) => x.id))).toEqual([
      [mrms(T0 - 2 * MIN).id, sat(T0 - 30 * MIN).id],
      [mrms(T0).id, sat(T0 - 30 * MIN).id],
      [hrrr(T0 + 15 * MIN).id, gfs(T0).id],
    ]);
    // A part held across ticks is one object, shared by both frames.
    expect(b.frames[0].stack[1]).toBe(b.frames[1].stack[1]);
    expect(b.parts.size).toBe(5);
    expect(b.unknown).toBe(0);
  });

  it('adds a new time step with its new parts, in order', () => {
    const b = base();
    const next = applyTimeline(b, { parts: [mrms(T0 + 2 * MIN)], frames: [frame(T0 + 2 * MIN, 'observed', [mrms(T0 + 2 * MIN), sat(T0 - 30 * MIN)])], removed: [] });
    expect(next.frames.map((f) => f.t)).toEqual([T0 - 2 * MIN, T0, T0 + 2 * MIN, T0 + 15 * MIN]);
    // The new frame reuses a part the timeline already had.
    expect(next.frames[2].stack.map((x) => x.id)).toEqual([mrms(T0 + 2 * MIN).id, sat(T0 - 30 * MIN).id]);
    expect(next.unknown).toBe(0);
    // Untouched frames keep their objects; the old timeline is unchanged.
    expect(next.frames[0]).toBe(b.frames[0]);
    expect(b.frames).toHaveLength(3);
  });

  it('replaces a changed time step by valid time, and drops removed ones', () => {
    const b = base();
    const newer = hrrr(T0 + 15 * MIN, T0 - HOUR);
    const next = applyTimeline(b, { parts: [newer], frames: [frame(T0 + 15 * MIN, 'forecast', [newer, gfs(T0)])], removed: [iso(T0 - 2 * MIN)] });
    expect(next.frames.map((f) => f.t)).toEqual([T0, T0 + 15 * MIN]);
    expect(next.frames[1].stack.map((x) => x.id)).toEqual([newer.id, gfs(T0).id]);
    expect(next.frames[1]).not.toBe(b.frames[2]);
    // Parts no frame names any more are forgotten.
    expect(next.parts.has(hrrr(T0 + 15 * MIN).id)).toBe(false);
    expect(next.parts.has(mrms(T0 - 2 * MIN).id)).toBe(false);
    expect([...next.parts.keys()].sort()).toEqual([mrms(T0).id, sat(T0 - 30 * MIN).id, newer.id, gfs(T0).id].sort());
  });

  it('matches valid times however the server spells them', () => {
    const b = base();
    const next = applyTimeline(b, { parts: [], frames: [], removed: [new Date(T0).toISOString().replace('Z', '000Z')] });
    expect(next.frames.map((f) => f.t)).toEqual([T0 - 2 * MIN, T0 + 15 * MIN]);
  });

  it('is unchanged by an event replayed onto a timeline that already has it', () => {
    const ev = { parts: [mrms(T0 + 2 * MIN)], frames: [frame(T0 + 2 * MIN, 'observed', [mrms(T0 + 2 * MIN), sat(T0 - 30 * MIN)])], removed: [iso(T0 - 2 * MIN)] };
    const once = applyTimeline(base(), ev);
    const twice = applyTimeline(once, ev);
    expect(twice.frames).toEqual(once.frames);
    expect(twice.frames[1]).toBe(once.frames[1]);
  });

  it('counts parts no message described, so the client can resync', () => {
    const next = applyTimeline(base(), { parts: [], frames: [frame(T0 + 2 * MIN, 'observed', ['mrms-unknown'].map((id) => ({ ...mrms(T0), id })))], removed: [] });
    expect(next.unknown).toBe(1);
    expect(next.frames.find((f) => f.t === T0 + 2 * MIN)!.stack).toEqual([]);
  });
});

describe('slider scale with uneven spacing', () => {
  const frames = radarFrames();
  const s = buildScale(frames);

  it('is monotonic and spans the track', () => {
    expect(s.pos[0]).toBe(0);
    expect(s.pos[s.pos.length - 1]).toBeCloseTo(1, 12);
    for (let i = 1; i < s.pos.length; i++) expect(s.pos[i]).toBeGreaterThan(s.pos[i - 1]);
  });

  it('does not squeeze the radar loop into a sliver, and still reaches day 7', () => {
    const lastObs = frames.findIndex((f) => f.t === T0);
    expect(s.pos[lastObs]).toBeGreaterThan(0.25);
    expect(s.pos[lastObs]).toBeLessThan(0.5);
    const day2 = nearestIndex(frames, T0 + 48 * HOUR);
    expect(1 - s.pos[day2]).toBeGreaterThan(0.15);
    expect(frames[frames.length - 1].t - T0).toBeGreaterThanOrEqual(167 * HOUR);
  });

  it('maps time and track position back and forth', () => {
    for (const t of [T0 - 90 * MIN, T0 + 5 * HOUR, T0 + 100 * HOUR]) {
      expect(s.t(s.x(t))).toBeCloseTo(t, -3);
    }
    expect(s.x(T0 - 99 * HOUR)).toBe(0);
    expect(s.indexAt(s.pos[10] + 1e-4)).toBe(10);
  });

  it('widens a missing frame so the gap shows', () => {
    const times = [0, 1, 2, 3, 4, 6, 7, 8, 9].map((h) => T0 + h * HOUR);
    const w = stepWeights(times);
    expect(w[4]).toBe(2);
    expect(w.filter((x) => x === 1)).toHaveLength(7);
    // A change of cadence (2-minute radar to 15-minute forecast) is not a gap.
    expect(stepWeights([0, 2, 4, 6, 8, 23, 38, 53].map((m) => T0 + m * MIN)).every((x) => x === 1)).toBe(true);
  });

  it('labels days and hours in the viewer’s time zone without crowding', () => {
    const ticks = buildTicks(s, 1200, { timeZone: 'America/Chicago', locale: 'en-US', minGapPx: 64 });
    expect(ticks.length).toBeGreaterThan(5);
    for (let i = 1; i < ticks.length; i++) expect((ticks[i].x - ticks[i - 1].x) * 1200).toBeGreaterThanOrEqual(64);
    const days = ticks.filter((t) => t.day);
    expect(days.length).toBeGreaterThan(2);
    expect(days[0].label).toMatch(/^(Sat|Sun|Mon|Tue|Wed|Thu|Fri) \d+$/);
    expect(ticks.find((t) => !t.day)?.label).toMatch(/\d+ (AM|PM)/);
  });

  it('labels hours in zones offset by 45 minutes (Nepal, Chatham Islands)', () => {
    for (const timeZone of ['Asia/Kathmandu', 'Pacific/Chatham', 'Asia/Kolkata']) {
      const ticks = buildTicks(s, 1200, { timeZone, locale: 'en-US', minGapPx: 64 });
      expect(ticks.length, timeZone).toBeGreaterThan(5);
    }
  });

  it('plays the dense near-term stretch, or the long range from beyond it', () => {
    const [lo, hi] = loopRange(frames, 10, T0);
    expect(lo).toBe(0);
    expect(frames[hi].t).toBeLessThanOrEqual(T0 + 12 * HOUR);
    expect(frames[hi + 1].t).toBeGreaterThan(T0 + 12 * HOUR);
    const late = loopRange(frames, frames.length - 3, T0);
    expect(late[0]).toBe(hi + 1);
    expect(late[1]).toBe(frames.length - 1);
  });
});

describe('the mock catalog follows the server’s rules', () => {
  const now = Date.UTC(2026, 10, 6, 12, 7);

  it('keeps two hours of radar, then forecasts to day 7, one frame per valid time', () => {
    const t = parseTimeline(assemble(layerById('radar')!, now));
    const f = t.frames;
    expect(f[0].kind).toBe('observed');
    expect(f[0].t).toBeGreaterThanOrEqual(now - 120 * MIN);
    expect(new Set(f.map((x) => x.t)).size).toBe(f.length);
    const lastObs = f.filter((x) => x.kind === 'observed').at(-1)!;
    expect(f.filter((x) => x.kind === 'forecast').every((x) => x.t > lastObs.t)).toBe(true);
    expect(f.at(-1)!.t - now).toBeGreaterThan(6 * 24 * HOUR);
    expect(t.unknown).toBe(0);
    // Observed frames: ground radar over a satellite estimate held across ticks.
    expect(lastObs.stack.map((x) => x.source)).toEqual(['mrms', 'opera', 'cmorph2']);
    const sats = new Set(f.filter((x) => x.kind === 'observed').map((x) => x.stack.find((s) => s.source === 'cmorph2')?.id));
    expect(sats.size).toBeLessThan(10);
    // Forecast frames: fine models over the global one.
    const fc = f.find((x) => x.t === lastObs.t + 8 * MIN || x.kind === 'forecast')!;
    expect(fc.stack.at(-1)!.source).toBe('gfs');
  });

  it('uses part ids in the Rust formats', () => {
    const t = assemble(layerById('radar')!, now);
    expect(t.parts.find((x) => x.source === 'mrms')!.id).toMatch(/^mrms-\d{10}$/);
    expect(t.parts.find((x) => x.source === 'hrrr')!.id).toMatch(/^hrrr-\d{8}-\d+$/);
    expect(assemble(layerById('temp')!, now).parts.find((x) => x.source === 'gfs')!.id).toMatch(/^gfs-\d{8}-\d+$/);
  });
});
