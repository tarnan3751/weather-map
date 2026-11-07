import type { AlertCollection, Config, LayerSpec, LonLatBox, Place, SourceDef, StationCollection } from '../api/types.ts';
import type { LiveState } from '../api/live.ts';
import { Store } from '../core/store.ts';
import { loadUnits, type Units } from '../core/units.ts';
import { type LayerTimeline, nearestIndex, nowIndex, type TFrame } from '../core/timeline.ts';

export type Panel =
  | { kind: 'point'; lat: number; lon: number }
  | { kind: 'alert'; id: string; others?: string[]; at?: { lat: number; lon: number } }
  | { kind: 'places' }
  | { kind: 'notify' }
  | { kind: 'layers' }
  | { kind: 'settings' };

export interface Toast {
  id: number;
  title: string;
  body: string;
  severity?: string;
  action?: { label: string; alertId?: string; lat?: number; lon?: number };
}

export interface Readout {
  x: number;
  y: number;
  lat: number;
  lon: number;
  text: string;
  detail?: string;
  /** Where the value comes from: "HRRR, 11Z run". */
  source?: string;
  swatch?: string;
  /** Pinned to the screen centre (touch read-out mode) rather than the cursor. */
  centre?: boolean;
}

export interface AppState {
  config: Config | null;
  layers: LayerSpec[];
  /** Every data source, by id. */
  sources: Record<string, SourceDef>;
  timelines: Record<string, LayerTimeline>;
  /** Sources with a part in view in the frame shown, top first. */
  viewSources: string[];
  /** The source on top at the centre of the view in the frame shown; null
   *  while not known, or where there is no data. */
  centreSource: string | null;
  /** Server clock minus browser clock, ms. */
  clockOffset: number;
  layerId: string;
  /** The time the user chose; frames snap to the nearest one per layer. */
  targetTime: number;
  /** Stay on the newest observation (or the frame nearest now) as frames arrive. */
  following: boolean;
  playing: boolean;
  /** Fractional frame index while playing. */
  playhead: number | null;
  speed: number;
  units: Units;
  stepped: boolean;
  opacity: number;
  windByGust: boolean;
  /** How wind direction is drawn, as chosen; see `windMarksShown`. */
  windMarks: WindMarks;
  /** Whether this device can draw particles; null until the map has a context. */
  particlesOk: boolean | null;
  reducedMotion: boolean;
  overlays: { alerts: boolean; stations: boolean; places: boolean };
  /** The lon/lat box on screen (`west > east` across the antimeridian), for
   *  the lists that are fetched by area; null before the map exists. */
  viewBox: LonLatBox | null;
  /** Alerts in view, with or without a shape. */
  alerts: AlertCollection | null;
  /** The active alert set's revision: what the alert tiles' URLs name. */
  alertsRevision: number;
  /** Credit the alert tiles carry (EuroGeographics for NUTS shapes). */
  alertsAttribution: string | null;
  /** Stations in view. */
  stations: StationCollection | null;
  places: Place[];
  panel: Panel | null;
  readout: Readout | null;
  /** Touch devices: show the value under a crosshair at the map centre. */
  probe: boolean;
  live: LiveState;
  basemap: 'loading' | 'protomaps' | 'fallback';
  toasts: Toast[];
  error: string | null;
  /** Alert ids that just arrived, for a brief highlight. */
  freshAlerts: string[];
  /** Which tile decoder the workers use. */
  decoder: 'wasm' | 'typescript' | null;
}

export type WindMarks = 'particles' | 'arrows' | 'off';

/** What is drawn: particles fall back to arrows with reduced motion or
 *  where the device cannot draw them. */
export function windMarksShown(s: Pick<AppState, 'windMarks' | 'particlesOk' | 'reducedMotion'>): WindMarks {
  if (s.windMarks === 'particles' && (s.reducedMotion || s.particlesOk === false)) return 'arrows';
  return s.windMarks;
}

const PREFS_KEY = 'wx.prefs';

interface Prefs {
  layerId?: string;
  opacity?: number;
  stepped?: boolean;
  windByGust?: boolean;
  wind?: WindMarks;
  /** Before `wind`: a switch for the arrows. */
  arrows?: boolean;
  overlays?: AppState['overlays'];
}

function loadPrefs(): Prefs {
  try {
    return JSON.parse(localStorage.getItem(PREFS_KEY) ?? '{}') as Prefs;
  } catch {
    return {};
  }
}

export function savePrefs(s: AppState) {
  const p: Prefs = {
    layerId: s.layerId,
    opacity: s.opacity,
    stepped: s.stepped,
    windByGust: s.windByGust,
    wind: s.windMarks,
    overlays: s.overlays,
  };
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(p));
  } catch {
    // Not remembered; nothing else depends on it.
  }
}

/** Change preferences and remember them. */
export function setPrefs(store: Store<AppState>, patch: Partial<AppState>) {
  store.set(patch);
  savePrefs(store.get());
}

export function createAppStore(): Store<AppState> {
  const p = loadPrefs();
  return new Store<AppState>({
    config: null,
    layers: [],
    sources: {},
    timelines: {},
    viewSources: [],
    centreSource: null,
    clockOffset: 0,
    layerId: p.layerId ?? 'radar',
    targetTime: Date.now(),
    following: true,
    playing: false,
    playhead: null,
    speed: 1,
    units: loadUnits(),
    stepped: p.stepped ?? false,
    opacity: typeof p.opacity === 'number' ? Math.min(1, Math.max(0.2, p.opacity)) : 0.85,
    windByGust: p.windByGust ?? false,
    windMarks: p.wind === 'particles' || p.wind === 'arrows' || p.wind === 'off' ? p.wind : p.arrows === false ? 'off' : 'particles',
    particlesOk: null,
    reducedMotion: false,
    overlays: { alerts: true, stations: false, places: true, ...p.overlays },
    viewBox: null,
    alerts: null,
    alertsRevision: 0,
    alertsAttribution: null,
    stations: null,
    places: [],
    panel: null,
    readout: null,
    probe: false,
    live: 'connecting',
    basemap: 'loading',
    toasts: [],
    error: null,
    freshAlerts: [],
    decoder: null,
  });
}

export const serverNow = (s: AppState) => Date.now() + s.clockOffset;

export function currentLayer(s: AppState): LayerSpec | undefined {
  return s.layers.find((l) => l.id === s.layerId) ?? s.layers[0];
}

const NO_FRAMES: TFrame[] = [];

export function currentFrames(s: AppState): TFrame[] {
  const l = currentLayer(s);
  // A shared empty array keeps selectors stable for React.
  return (l && s.timelines[l.id]?.frames) || NO_FRAMES;
}

/** Index of the selected frame in the current layer (snapped). */
export function selectedIndex(s: AppState): number {
  const frames = currentFrames(s);
  if (!frames.length) return -1;
  if (s.playing && s.playhead !== null) return Math.max(0, Math.min(frames.length - 1, Math.floor(s.playhead)));
  if (s.following) return nowIndex(frames, serverNow(s));
  return nearestIndex(frames, s.targetTime);
}
