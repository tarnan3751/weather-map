// Words for values, frames and times: the read-out under the cursor, the
// slider's frame label, relative times.

import type { FrameKind, LayerSpec, Medium, Part, SourceDef } from '../api/types.ts';
import { getPalette, lookupLut, buildLut, cssColor, PTYPE_NAMES, type Palette } from '../core/palettes.ts';
import { HOUR, MIN, type TFrame } from '../core/timeline.ts';
import { compass, displayUnit, type DisplayUnit, formatValue, speedUnit, type Units, windFrom } from '../core/units.ts';
import type { Sample } from '../render/weatherLayer.ts';

const lutCache = new Map<string, ReturnType<typeof buildLut>>();
function lutFor(p: Palette, stepped: boolean) {
  const k = `${p.id}|${stepped}`;
  let l = lutCache.get(k);
  if (!l) lutCache.set(k, (l = buildLut(p, stepped)));
  return l;
}

/** "Moderate" etc. for a reflectivity, by the usual radar thresholds. */
function reflectivityWord(dbz: number): string {
  if (dbz < 20) return 'Very light';
  if (dbz < 35) return 'Light';
  if (dbz < 45) return 'Moderate';
  if (dbz < 55) return 'Heavy';
  if (dbz < 60) return 'Very heavy';
  return 'Intense, possible hail';
}

/** What an infrared brightness temperature (°C) says about the cloud: the
 *  colder, the higher. Warm readings cannot tell low cloud from the ground. */
function cloudTopWord(c: number): string {
  if (c <= -60) return 'Very high cloud tops, storms likely';
  if (c <= -40) return 'High cloud';
  if (c <= -15) return 'Mid-level cloud';
  if (c <= 5) return 'Low cloud or cold ground';
  return 'Low cloud or clear';
}

function rateWord(mmh: number): string {
  if (mmh < 0.25) return 'Very light';
  if (mmh < 2.5) return 'Light';
  if (mmh < 7.6) return 'Moderate';
  return 'Heavy';
}

interface Described {
  text: string;
  detail?: string;
  swatch?: string;
}

/** A small amount in display units; "trace" where it would round to zero
 *  but is still drawn on the map. */
function amount(stored: number, du: DisplayUnit): string {
  const shown = du.to(stored);
  if (stored > 0 && Math.abs(shown) < 0.5 * 10 ** -du.decimals) return `trace (under ${formatValue(du.from(10 ** -du.decimals), du)})`;
  return formatValue(stored, du);
}

/** What the read-out says for a sample of the current layer. `byGust`: the
 *  wind layer is coloured by gusts, so the swatch should be too. */
export function describeSample(layer: LayerSpec, s: Sample | null, units: Units, stepped: boolean, byGust: boolean): Described {
  if (!s || s.status === 'loading') return { text: 'Loading…' };
  const palette = getPalette(layer.palette);
  const lut = lutFor(palette, stepped);
  const r = layer.render;
  const radar = layer.bands[0]?.quantity === 'reflectivity';
  if (s.status === 'empty' || s.values.every((v) => v === null)) {
    return { text: radar ? 'No radar coverage' : 'No data here' };
  }
  if (r.kind === 'scalar') {
    const v = s.values[0];
    const band = layer.bands[r.band];
    if (v === null) return { text: radar ? 'No radar coverage' : 'No data here' };
    const du = displayUnit(band.quantity, units);
    const swatch = cssColor(lookupLut(lut, palette, v));
    if (radar) {
      // −30 dBZ is the "radar looked and saw nothing" floor, not a measurement.
      if (v <= -29.95) return { text: 'No echo' };
      if (v < 10) return { text: `${Math.round(v)} dBZ`, detail: 'Below the weak-echo threshold' };
      return { text: `${Math.round(v)} dBZ`, detail: reflectivityWord(v), swatch };
    }
    if (palette.id === 'infrared') return { text: amount(v, du), detail: cloudTopWord(v), swatch };
    if ((band.quantity === 'precip_depth' || band.quantity === 'snow_depth') && v < 0.05) {
      return { text: band.quantity === 'snow_depth' ? 'No snow' : 'No rain', detail: formatValue(0, du) };
    }
    return { text: amount(v, du), swatch };
  }
  if (r.kind === 'wind') {
    const [u, v, g] = s.values;
    if (u === null || v === null) return { text: 'No data here' };
    const su = speedUnit(units.speed);
    const speed = Math.hypot(u, v);
    const dir = windFrom(u, v);
    const gust = g ?? null;
    const swatch = cssColor(lookupLut(lut, palette, byGust && gust !== null ? gust : speed));
    const calm = su.to(speed) < 1;
    return {
      text: calm ? 'Calm' : `${compass(dir)} ${formatValue(speed, su)}`,
      detail: gust !== null ? `Gusts ${formatValue(gust, su)}${calm ? '' : ` · from ${Math.round(dir)}°`}` : calm ? undefined : `From ${Math.round(dir)}°`,
      swatch,
    };
  }
  const [rate, code] = s.values;
  if (rate === null || code === null || code < 0.5 || rate < 0.01) return { text: 'No precipitation' };
  const c = Math.round(code);
  const du = displayUnit('precip_rate', units);
  return {
    text: PTYPE_NAMES[c] ?? 'Precipitation',
    detail: `${rateWord(rate)} · ${amount(rate, du)}`,
    swatch: cssColor(lookupLut(lut, palette, rate, c)),
  };
}

// ---------------------------------------------------------------------------
// Frames and times
// ---------------------------------------------------------------------------

/** "HRRR" for an observation, "HRRR 11Z run" for a model. */
export function partLabel(p: Pick<Part, 'source' | 'run'>, sources: Readonly<Record<string, SourceDef>>): string {
  const title = sources[p.source]?.title ?? p.source.toUpperCase();
  if (!p.run) return title;
  const run = new Date(p.run);
  return Number.isNaN(run.getTime()) ? title : `${title} ${String(run.getUTCHours()).padStart(2, '0')}Z run`;
}

/**
 * The time bar's badge for a frame: its kind and what is on top at the centre
 * of the view ("Forecast · HRRR 11Z run"), and how many other sources the
 * view spans ("and 2 others"). Frames are mosaics: the read-out names the
 * source at a point, the credits every source in view. `centre` is the
 * source on top at the centre, when known. The kind is the named part's own:
 * a frame up to the newest observation is "observed", but where a model fills
 * it (Africa, in the temperature "now" frame) the value shown is a forecast.
 */
export function frameBadge(f: TFrame, sources: Readonly<Record<string, SourceDef>>, inView: readonly string[], centre: string | null): { kind: FrameKind; text: string } {
  const word = (k: FrameKind) => (k === 'observed' ? 'Observed' : 'Forecast');
  // What kind of looking it is, where the name does not say: "MRMS radar".
  const named = (p: Part) => {
    const label = partLabel(p, sources);
    const medium = sources[p.source]?.medium;
    const word = medium === 'satellite' ? 'satellite' : medium === 'radar' ? 'radar' : medium === 'analysis' ? 'analysis' : null;
    return word && !label.toLowerCase().includes(word) ? `${label} ${word}` : label;
  };
  const at = centre ? f.stack.find((p) => p.source === centre) : undefined;
  if (at) {
    const others = inView.filter((id) => id !== centre).length;
    return { kind: at.kind, text: `${word(at.kind)} · ${named(at)}${others ? ` and ${others} other${others > 1 ? 's' : ''}` : ''}` };
  }
  const only = inView.length === 1 ? f.stack.find((p) => p.source === inView[0]) : undefined;
  if (only) return { kind: only.kind, text: `${word(only.kind)} · ${named(only)}` };
  // Several sources and none named: one kind if they all are.
  const kinds = new Set(f.stack.filter((p) => inView.includes(p.source)).map((p) => p.kind));
  const kind = kinds.size === 1 ? [...kinds][0] : f.kind;
  return { kind, text: inView.length > 1 ? `${word(kind)} · ${inView.length} sources` : word(kind) };
}

/** A resolution as a person would say it: "2.2 km", "13 km". */
export function aboutKm(km: number): string {
  if (!Number.isFinite(km) || km <= 0) return '';
  return `${km >= 10 ? Math.round(km) : Number(km.toPrecision(2))} km`;
}

const MEDIUM_WORDS: Record<Medium, string> = {
  radar: 'radar observations',
  satellite: 'satellite estimates',
  analysis: 'analyses of observations',
  nowcast: 'nowcasts',
  model: 'model forecasts',
};

/** What a source's values are, in a few words. */
export function mediumWords(d: Pick<SourceDef, 'medium' | 'kind'>): string {
  return MEDIUM_WORDS[d.medium] ?? (d.kind === 'observed' ? 'observations' : 'model forecasts');
}

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const dayTimeFmt = new Intl.DateTimeFormat(undefined, { weekday: 'short', hour: 'numeric', minute: '2-digit' });
const dateFmt = new Intl.DateTimeFormat(undefined, { weekday: 'short', month: 'short', day: 'numeric' });

// With Date rather than Intl (`sameDay`): the time bar asks on every frame of playback.
function sameLocalDay(a: number, b: number) {
  const da = new Date(a);
  const db = new Date(b);
  return da.getFullYear() === db.getFullYear() && da.getMonth() === db.getMonth() && da.getDate() === db.getDate();
}

/** Local time of a frame, with the weekday when it is not today. */
export function frameTime(t: number, now: number): string {
  return sameLocalDay(t, now) ? timeFmt.format(t) : dayTimeFmt.format(t);
}

export function formatDateTime(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return '—';
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return '—';
  return sameLocalDay(t, now) ? timeFmt.format(t) : `${dateFmt.format(t)}, ${timeFmt.format(t)}`;
}

/** "now", "12 min ago", "in 3 h", "in 2 days". */
export function relative(t: number, now: number): string {
  const d = t - now;
  const a = Math.abs(d);
  if (a < 1.5 * MIN) return 'now';
  let s: string;
  if (a < HOUR) s = `${Math.round(a / MIN)} min`;
  else if (a < 36 * HOUR) {
    const h = a / HOUR;
    s = `${h < 10 ? Math.round(h * 2) / 2 : Math.round(h)} h`;
  } else s = `${Math.round(a / (24 * HOUR))} days`;
  return d < 0 ? `${s} ago` : `in ${s}`;
}
