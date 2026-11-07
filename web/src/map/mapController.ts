// Owns the MapLibre map: picks the basemap and the first view, inserts the
// weather layer under roads, borders and labels, draws alerts, stations and
// saved places, turns clicks and hovers into panels and read-outs, credits the
// sources in view, and keeps the weather layer's display in step with the app
// state (layer, time, playback, palette).

import type { ExpressionSpecification } from '@maplibre/maplibre-gl-style-spec';
import { addProtocol, Map as MlMap, NavigationControl, setWorkerUrl, type GeoJSONSource, type MapMouseEvent, type VectorTileSource } from 'maplibre-gl';
// MapLibre finds its worker next to its own module, which bundlers move; point
// it at the file explicitly so dev and production builds both find it.
import maplibreWorker from 'maplibre-gl/dist/maplibre-gl-worker.mjs?url';
import { PMTiles, Protocol } from 'pmtiles';
import { api, tileUrl } from '../api/http.ts';
import type { AlertCollection, Config, Feature, LayerSpec, Part, Place, Provenance, StationCollection, StationProps } from '../api/types.ts';
import { describeSample, formatDateTime, partLabel } from '../app/describe.ts';
import { Playback } from '../app/playback.ts';
import { currentFrames, currentLayer, selectedIndex, serverNow, windMarksShown, type AppState } from '../app/state.ts';
import { alertClickRank, alertColor, alertColorExpression, severityLevel } from '../core/alertColors.ts';
import { buildLut, cssColor, getPalette, lookupLut } from '../core/palettes.ts';
import type { Store } from '../core/store.ts';
import type { TFrame } from '../core/timeline.ts';
import type { Sample } from '../render/weatherLayer.ts';
import { boxContainsPoint, topPartAt } from '../tiles/stack.ts';
import { tempUnit } from '../core/units.ts';
import { WeatherLayer, type WeatherDisplay } from '../render/weatherLayer.ts';
import { WindParticles } from '../render/windParticles.ts';
import { createWorkerFetcher } from '../tiles/loader.ts';
import { latToY, lonToX } from '../tiles/tileMath.ts';
import { TileStore } from '../tiles/tileStore.ts';
import { fallbackStyle, overlayBeforeId, protomapsStyle, weatherBeforeId } from './basemap.ts';
import { CreditsControl } from './credits.ts';
import { creditedSources, type Answer } from './inView.ts';
import { ErrorWindow } from './errorWindow.ts';
import { guessView, savedView, saveView } from './firstView.ts';
import { viewBox } from './viewBox.ts';
import { stationPriority, thinByZoom } from './thinning.ts';

let protocol: Protocol | null = null;

/** The alert tiles of one revision of the alert set. */
const alertTileUrl = (revision: number) => new URL(`/api/alerts/tiles/${revision}/{z}/{x}/{y}.mvt`, location.href).href.replace('%7Bz%7D', '{z}').replace('%7Bx%7D', '{x}').replace('%7By%7D', '{y}');

/** A satellite estimate saw no rain rather than no echo. */
const isSatelliteEstimate = (d: { medium?: string } | undefined) => d?.medium === 'satellite';

/** After the provenance endpoint fails, it is left alone this long (doubling
 *  to the maximum): the read-out just names no source meanwhile. */
const PROVENANCE_BACKOFF_MS = 30_000;
const PROVENANCE_BACKOFF_MAX_MS = 5 * 60_000;

const PROVENANCE_REST_MS = 300;
/** The time bar names the centre's source once the view and time settle. */
const CENTRE_SETTLE_MS = 300;

const EMPTY: GeoJSON.FeatureCollection = { type: 'FeatureCollection', features: [] };

function memoryBudget(): number {
  // `?tilebudget=MB` tries the cache under memory pressure.
  const forced = Number(new URLSearchParams(location.search).get('tilebudget'));
  if (forced > 0) return forced * 1024 * 1024;
  const mem = (navigator as Navigator & { deviceMemory?: number }).deviceMemory ?? 4;
  const coarse = matchMedia('(pointer: coarse)').matches;
  // Decoded samples plus their GPU copies; phones get far less.
  return (coarse || mem <= 2 ? 96 : mem <= 4 ? 192 : 320) * 1024 * 1024;
}

function connectionLimit(): number {
  const nav = performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming | undefined;
  // HTTP/1.1 allows six connections per host, shared with the basemap and API.
  return nav?.nextHopProtocol === 'h2' || nav?.nextHopProtocol === 'h3' ? 12 : 4;
}

/** Whether the archive answers with vector tiles. The archive opened to
 *  ask is the one the map then reads, so its header is fetched once. */
async function basemapUsable(url: string): Promise<boolean> {
  try {
    const p = new PMTiles(new URL(url, location.href).href);
    const header = await Promise.race([p.getHeader(), new Promise<never>((_, rej) => setTimeout(() => rej(new Error('timeout')), 6000))]);
    if (header.tileType !== 1) return false;
    protocol?.add(p);
    return true;
  } catch {
    return false;
  }
}

export interface MapControllerOptions {
  reducedMotion: () => boolean;
  playback: Playback;
}

export class MapController {
  readonly map: MlMap;
  readonly tiles: TileStore;
  readonly layer: WeatherLayer;
  readonly particles: WindParticles;
  private readonly app: Store<AppState>;
  private readonly opts: MapControllerOptions;
  private lastReady: TFrame | null = null;
  private lastLayerId = '';
  /** The playing loop's frames, kept while the loop and the timeline are the same. */
  private loop: { frames: TFrame[]; lo: number; hi: number; list: TFrame[] } | null = null;
  private mouse: { x: number; y: number; lng: number; lat: number } | null = null;
  private selectedAlert: string | null = null;
  private pulse = 0;
  private overlaysReady = false;
  private unsub: (() => void) | null = null;
  readonly credits = new CreditsControl();
  /** Credits for the alerts and stations drawn in view, refreshed when the
   *  map settles. */
  private overlayCredits: { text: string; url?: string }[] = [];
  private alertsRevision = 0;
  /** Provenance answers by layer, frame and rounded position. */
  private provenance = new Map<string, Provenance['parts'] | null>();
  /** The question each slot last asked, and its pending request timer. */
  private provenanceAsk: Record<'pointer' | 'centre', { key: string; timer: ReturnType<typeof setTimeout> | null }> = {
    pointer: { key: '', timer: null },
    centre: { key: '', timer: null },
  };
  private centreTimer: ReturnType<typeof setTimeout> | null = null;
  /** When the view and time last changed, plus the settling time. */
  private centreDue = 0;
  /** Sources whose parts' bounds reach the view, as the layer reports them. */
  private boundsSources: string[] = [];
  /** Recent provenance answers, for telling a source's data from its box. */
  private answers: Answer[] = [];
  /** No provenance questions before this (after failures), and the next pause. */
  private provenanceQuietUntil = 0;
  private provenanceBackoff = PROVENANCE_BACKOFF_MS;
  readonly hover = matchMedia('(hover: hover) and (pointer: fine)');

  private constructor(map: MlMap, tiles: TileStore, app: Store<AppState>, config: Config, opts: MapControllerOptions) {
    this.map = map;
    this.app = app;
    this.opts = opts;
    this.tiles = tiles;
    this.layer = new WeatherLayer(this.tiles, (layer, z, x, y, parts) => tileUrl(config.tile_url, layer, z, x, y, parts));
    this.layer.onViewSources = (found) => {
      this.boundsSources = found;
      this.publishViewSources();
    };
    // Fewer particles where drawing them costs more: phones, small machines.
    const modest = matchMedia('(pointer: coarse)').matches || (navigator.hardwareConcurrency || 8) <= 4 || ((navigator as { deviceMemory?: number }).deviceMemory ?? 8) <= 4;
    this.particles = new WindParticles(this.layer, {
      maxParticles: modest ? 4096 : 16384,
      pixelsPerParticle: 110,
      // `?particles=nofloat` acts out a device without float render targets.
      noFloat: new URLSearchParams(location.search).get('particles') === 'nofloat',
    });
    this.particles.onSupport = (ok) => {
      if (app.get().particlesOk !== ok) app.set({ particlesOk: ok });
    };
    this.tiles.onUpdate = () => map.triggerRepaint();
    this.layer.onError = (e) => console.error('weather layer:', e.message);
  }

  static async create(container: HTMLElement, app: Store<AppState>, config: Config, opts: MapControllerOptions): Promise<MapController> {
    if (!protocol) {
      setWorkerUrl(new URL(maplibreWorker, location.href).href);
      protocol = new Protocol();
      addProtocol('pmtiles', protocol.tile);
    }
    // The decode workers start (and fetch their WebAssembly) while the
    // basemap is checked and the map sets itself up.
    // `?wasm=off` decodes in TypeScript; `?wasm=URL` tries another module.
    const wasmParam = new URLSearchParams(location.search).get('wasm');
    const wasmUrl = wasmParam === 'off' ? null : new URL(wasmParam || '/wasm/wx_tile_wasm.wasm', location.href).href;
    const tiles = new TileStore(createWorkerFetcher({ wasmUrl, onDecoder: (decoder) => app.set({ decoder }) }), {
      budgetBytes: memoryBudget(),
      maxInFlight: connectionLimit(),
    });
    // `?basemap=off` / `?basemap=broken` exercise the fallback without
    // reconfiguring the server.
    const forced = new URLSearchParams(location.search).get('basemap');
    const archive = forced === 'off' ? null : forced === 'broken' ? '/basemap/missing.pmtiles' : config.basemap_url;
    // The first view's guess (a lazily loaded table) is looked up meanwhile.
    const first = savedView() ?? guessView(Intl.DateTimeFormat().resolvedOptions().timeZone, container.clientWidth);
    const useArchive = archive ? await basemapUsable(archive) : false;
    const style = useArchive && archive ? protomapsStyle(archive) : await fallbackStyle();
    app.set({ basemap: useArchive ? 'protomaps' : 'fallback' });
    const narrow = container.clientWidth < 700;
    const view0 = await first;
    // Bounds keep clear of the top bar and the legend plus time bar.
    const view =
      view0.kind === 'bounds'
        ? { bounds: view0.bounds, fitBoundsOptions: { padding: narrow ? { top: 64, bottom: 240, left: 4, right: 4 } : { top: 70, bottom: 140, left: 30, right: 30 } } }
        : { center: view0.center, zoom: view0.zoom };
    const map = new MlMap({
      container,
      style,
      ...view,
      // The whole world, every copy of it: no bounds. The basemap archive
      // stops at a lower zoom than this and is drawn larger beyond it.
      minZoom: 0,
      maxZoom: 13,
      renderWorldCopies: true,
      maxPitch: 0,
      dragRotate: false,
      pitchWithRotate: false,
      touchPitch: false,
      // Replaced by `CreditsControl`, which also credits the weather sources.
      attributionControl: false,
      fadeDuration: 0,
      cancelPendingTileRequestsWhileZooming: true,
    });
    map.touchZoomRotate.disableRotation();
    map.keyboard.disableRotation();
    const c = new MapController(map, tiles, app, config, opts);
    map.addControl(c.credits, 'bottom-right');
    map.addControl(new NavigationControl({ showCompass: false }), 'bottom-right');
    map.on('style.load', () => c.installOverlays());
    map.on('error', (e) => void c.onMapError(e as unknown as { error?: Error; sourceId?: string }));
    c.bind();
    return c;
  }

  /** Swap to the bundled outlines when the basemap archive stops answering:
   *  six failures within 20 seconds with no tile loading in between. */
  private archiveErrors = new ErrorWindow(6, 20_000);
  private switchingBasemap = false;
  private async onMapError(e: { error?: Error; sourceId?: string }) {
    const msg = e.error?.message ?? String(e.error);
    if (e.sourceId === 'protomaps') {
      if (this.app.get().basemap !== 'protomaps' || this.switchingBasemap) return;
      if (!this.archiveErrors.fail(performance.now())) return;
      this.switchingBasemap = true;
      console.warn('basemap archive failing; switching to the built-in outlines');
      try {
        const style = await fallbackStyle();
        this.overlaysReady = false;
        // A full rebuild, not a diff: `style.load` then fires and puts the
        // weather layer and every overlay back in their places.
        this.map.setStyle(style, { diff: false });
        this.app.set({ basemap: 'fallback' });
      } finally {
        this.switchingBasemap = false;
      }
      return;
    }
    console.warn('map:', msg);
  }

  // ----------------------------------------------------------- overlays

  /**
   * Put the weather layer and every overlay into the current style. Runs on
   * each `style.load`: after the first load, after a switch to the fallback
   * basemap, and after MapLibre rebuilds the style from a snapshot when a lost
   * WebGL context comes back (the snapshot keeps GeoJSON sources and plain
   * layers but drops custom ones). So it adds only what is missing.
   */
  private installOverlays() {
    const m = this.map;
    this.overlaysReady = false;
    this.selectedAlert = null;
    this.addImages();
    const order = () => m.getStyle().layers;
    if (!m.getLayer('weather')) m.addLayer(this.layer, weatherBeforeId(order()));
    if (!m.getLayer(this.particles.id)) {
      // Directly above the colours (custom layers are not in getStyle()).
      const all = m.getLayersOrder();
      m.addLayer(this.particles, all[all.indexOf('weather') + 1]);
    }
    const labels = overlayBeforeId(order());
    const color = alertColorExpression() as ExpressionSpecification;
    const source = (id: string, spec: Parameters<MlMap['addSource']>[1]) => {
      if (!m.getSource(id)) m.addSource(id, spec);
    };
    const layer = (spec: Parameters<MlMap['addLayer']>[0], before?: string) => {
      if (!m.getLayer(spec.id)) m.addLayer(spec, before);
    };
    // Every alert with a shape, worldwide, as vector tiles named by the
    // alert set's revision (a new revision is a new URL: `syncOverlays`).
    this.alertsRevision = this.app.get().alertsRevision;
    source('alerts', { type: 'vector', tiles: [alertTileUrl(this.alertsRevision)], maxzoom: 12, promoteId: 'id' });
    const tiles = { source: 'alerts', 'source-layer': 'alerts' } as const;
    // Severe and extreme alerts, and NWS warnings, stand out.
    const strong: ExpressionSpecification = ['any', ['>=', ['to-number', ['get', 'level'], 0], 3], ['all', ['==', ['get', 'source'], 'us-nws'], ['==', ['slice', ['get', 'event'], ['max', 0, ['-', ['length', ['get', 'event']], 7]]], 'Warning']]];
    layer(
      {
        id: 'alerts-fill',
        type: 'fill',
        ...tiles,
        layout: { 'fill-sort-key': ['to-number', ['get', 'level'], 0] },
        paint: {
          'fill-color': color,
          'fill-opacity': ['case', ['boolean', ['feature-state', 'selected'], false], 0.34, strong, 0.2, 0.09],
        },
      },
      labels,
    );
    layer(
      {
        id: 'alerts-line',
        type: 'line',
        ...tiles,
        layout: { 'line-sort-key': ['to-number', ['get', 'level'], 0], 'line-join': 'round' },
        paint: {
          'line-color': color,
          'line-width': ['case', ['boolean', ['feature-state', 'selected'], false], 3.4, strong, 2.2, 1.2],
          'line-opacity': ['case', strong, 1, 0.8],
        },
      },
      labels,
    );
    layer(
      {
        id: 'alerts-fresh',
        type: 'line',
        ...tiles,
        filter: ['in', ['get', 'id'], ['literal', []]],
        layout: { 'line-join': 'round' },
        paint: { 'line-color': color, 'line-width': 2, 'line-opacity': 0, 'line-blur': 2 },
      },
      labels,
    );
    // Alerts that name only a region the server has no shape for: a hollow
    // ring at the region's point, so they do not pass for a drawn area.
    source('alerts-unshaped', { type: 'geojson', data: EMPTY, promoteId: 'id' });
    layer(
      {
        id: 'alerts-unshaped',
        type: 'circle',
        source: 'alerts-unshaped',
        paint: {
          'circle-radius': ['interpolate', ['linear'], ['zoom'], 2, 7, 8, 11],
          'circle-color': ['get', 'color'],
          'circle-opacity': ['case', ['boolean', ['feature-state', 'selected'], false], 0.45, 0.14],
          'circle-stroke-color': ['get', 'color'],
          'circle-stroke-width': ['case', ['boolean', ['feature-state', 'selected'], false], 3.2, 2],
          'circle-stroke-opacity': 0.95,
        },
      },
      labels,
    );
    layer(
      {
        id: 'alerts-unshaped-mark',
        type: 'symbol',
        source: 'alerts-unshaped',
        layout: { 'text-field': '?', 'text-font': ['Noto Sans Medium'], 'text-size': 12, 'text-allow-overlap': true, 'text-ignore-placement': true },
        paint: { 'text-color': ['get', 'color'] },
      },
      labels,
    );
    source('stations', { type: 'geojson', data: EMPTY });
    // Each station from the zoom level where it has room (`stationData`);
    // filters see whole zoom levels, as GeoJSON tiles are cut at those.
    const shownAt: ExpressionSpecification = ['<=', ['get', 'mz'], ['zoom']];
    layer({
      id: 'stations-wind',
      type: 'symbol',
      source: 'stations',
      minzoom: 4.2,
      filter: ['all', ['has', 'rot'], shownAt],
      layout: {
        'icon-image': 'wx-arrow',
        'icon-rotate': ['get', 'rot'],
        'icon-rotation-alignment': 'map',
        'icon-size': ['interpolate', ['linear'], ['get', 'spd'], 0, 0.7, 15, 1.15],
        'icon-allow-overlap': true,
        'icon-ignore-placement': true,
      },
      paint: { 'icon-opacity': 0.9 },
    });
    layer({
      id: 'stations-dot',
      type: 'circle',
      source: 'stations',
      minzoom: 3.2,
      filter: ['all', shownAt, ['!=', ['get', 'kind'], 'buoy']],
      paint: {
        'circle-radius': ['interpolate', ['linear'], ['zoom'], 3, 2.5, 7, 5],
        'circle-color': ['coalesce', ['get', 'color'], '#9aa3b2'],
        'circle-stroke-color': '#07090c',
        'circle-stroke-width': 1.2,
      },
    });
    // Buoys are diamonds, so a sea reading is not taken for a coastal one.
    layer({
      id: 'stations-buoy',
      type: 'symbol',
      source: 'stations',
      minzoom: 3.2,
      filter: ['all', shownAt, ['==', ['get', 'kind'], 'buoy']],
      layout: {
        'icon-image': 'wx-buoy',
        'icon-size': ['interpolate', ['linear'], ['zoom'], 3, 0.55, 7, 0.95],
        'icon-allow-overlap': true,
        'icon-ignore-placement': true,
      },
      paint: { 'icon-color': ['coalesce', ['get', 'color'], '#9aa3b2'], 'icon-halo-color': '#07090c', 'icon-halo-width': 1.4 },
    });
    layer({
      id: 'stations-label',
      type: 'symbol',
      source: 'stations',
      minzoom: 5,
      // Labels only on the sparser half: stations shown a zoom level earlier.
      filter: ['all', ['has', 'label'], ['<=', ['+', ['get', 'mz'], 1], ['zoom']]],
      layout: {
        'text-field': ['get', 'label'],
        'text-font': ['Noto Sans Medium'],
        'text-size': 11.5,
        'text-offset': [0, -1.15],
        'text-padding': 3,
      },
      paint: { 'text-color': '#f1f4f8', 'text-halo-color': '#07090c', 'text-halo-width': 1.6 },
    });
    source('places', { type: 'geojson', data: EMPTY });
    layer({
      id: 'places-pin',
      type: 'symbol',
      source: 'places',
      layout: {
        'icon-image': 'wx-pin',
        'icon-anchor': 'bottom',
        'icon-allow-overlap': true,
        'text-field': ['get', 'name'],
        'text-font': ['Noto Sans Medium'],
        'text-size': 12,
        'text-anchor': 'top',
        'text-offset': [0, 0.35],
        'text-optional': true,
      },
      paint: { 'text-color': '#ffffff', 'text-halo-color': '#07090c', 'text-halo-width': 1.6 },
    });
    this.overlaysReady = true;
    this.syncOverlays(this.app.get(), null);
  }

  private addImages() {
    const m = this.map;
    const make = (w: number, h: number, draw: (g: CanvasRenderingContext2D) => void) => {
      const c = document.createElement('canvas');
      c.width = w;
      c.height = h;
      const g = c.getContext('2d')!;
      draw(g);
      return g.getImageData(0, 0, w, h);
    };
    if (!m.hasImage('wx-arrow')) {
      // Points up (downwind once rotated), with its tail at the centre so it
      // grows out of the station dot. A dark outline keeps it visible over
      // any colour.
      const img = make(72, 72, (g) => {
        const arrow = () => {
          g.beginPath();
          g.moveTo(36, 36);
          g.lineTo(36, 12);
          g.stroke();
          g.beginPath();
          g.moveTo(36, 4);
          g.lineTo(43, 15);
          g.lineTo(29, 15);
          g.closePath();
          g.fill();
        };
        g.lineCap = 'round';
        g.lineJoin = 'round';
        g.strokeStyle = 'rgba(7, 9, 12, 0.9)';
        g.fillStyle = 'rgba(7, 9, 12, 0.9)';
        g.lineWidth = 7;
        arrow();
        g.strokeStyle = '#eef1f5';
        g.fillStyle = '#eef1f5';
        g.lineWidth = 3;
        arrow();
      });
      m.addImage('wx-arrow', img, { pixelRatio: 2 });
    }
    if (!m.hasImage('wx-buoy')) {
      // A signed-distance-field diamond, so its colour can follow the reading.
      const img = make(24, 24, (g) => {
        g.beginPath();
        g.moveTo(12, 3);
        g.lineTo(21, 12);
        g.lineTo(12, 21);
        g.lineTo(3, 12);
        g.closePath();
        g.fillStyle = '#ffffff';
        g.fill();
      });
      m.addImage('wx-buoy', img, { pixelRatio: 2, sdf: true });
    }
    if (!m.hasImage('wx-pin')) {
      const img = make(40, 52, (g) => {
        g.beginPath();
        g.arc(20, 18, 14, Math.PI, 0);
        g.bezierCurveTo(34, 30, 25, 38, 20, 50);
        g.bezierCurveTo(15, 38, 6, 30, 6, 18);
        g.closePath();
        g.fillStyle = '#7cc4ff';
        g.fill();
        g.lineWidth = 2.5;
        g.strokeStyle = '#07090c';
        g.stroke();
        g.beginPath();
        g.arc(20, 18, 5, 0, Math.PI * 2);
        g.fillStyle = '#07090c';
        g.fill();
      });
      m.addImage('wx-pin', img, { pixelRatio: 2 });
    }
  }

  /** Alerts in view that have no shape, as points at their regions. */
  private unshapedData(fc: AlertCollection | null): GeoJSON.FeatureCollection {
    if (!fc) return EMPTY;
    return {
      type: 'FeatureCollection',
      features: fc.features
        .filter((f) => !f.geometry && f.properties.anchor)
        .map((f) => ({
          type: 'Feature',
          id: f.properties.id,
          geometry: { type: 'Point', coordinates: f.properties.anchor! },
          properties: {
            id: f.properties.id,
            event: f.properties.event,
            severity: f.properties.severity,
            level: severityLevel(f.properties.severity),
            color: alertColor(f.properties.event, f.properties.severity, f.properties.source),
          },
        })),
    };
  }

  private stationZooms = new WeakMap<StationCollection, Uint8Array>();

  private stationData(s: AppState): GeoJSON.FeatureCollection {
    if (!s.stations) return EMPTY;
    const tu = tempUnit(s.units.temp);
    const pal = getPalette('temperature');
    const lut = buildLut(pal, false, 256);
    let zooms = this.stationZooms.get(s.stations);
    if (!zooms) {
      const pts = s.stations.features.map((f) => {
        const [lon, lat] = (f.geometry as GeoJSON.Point).coordinates;
        return { x: lonToX(lon), y: latToY(lat), priority: stationPriority(f.properties) };
      });
      // 26 px apart at the zoom a station appears (up to twice that just
      // before the next level fills in); every station from zoom 9.
      zooms = thinByZoom(pts, 26, 3, 9);
      this.stationZooms.set(s.stations, zooms);
    }
    const mz = zooms;
    return {
      type: 'FeatureCollection',
      features: s.stations.features.map((f: Feature<StationProps>, i) => {
        const p = f.properties;
        const props: Record<string, unknown> = { id: p.id, name: p.name, kind: p.kind, mz: mz[i] };
        // A buoy's water is what matters at sea when the air is not reported.
        const t = p.temp_c ?? p.water_temp_c;
        if (t !== undefined && t !== null) {
          props.label = `${Math.round(tu.to(t))}°`;
          props.color = cssColor(lookupLut(lut, pal, t));
        }
        if (p.wind_dir_deg !== undefined && p.wind_speed_ms !== undefined && p.wind_speed_ms > 0.5) {
          props.rot = (p.wind_dir_deg + 180) % 360;
          props.spd = p.wind_speed_ms;
        }
        return { type: 'Feature', id: p.id, geometry: f.geometry as GeoJSON.Geometry, properties: props };
      }),
    };
  }

  private placeData(places: Place[]): GeoJSON.FeatureCollection {
    return {
      type: 'FeatureCollection',
      features: places.map((p) => ({
        type: 'Feature',
        id: p.id,
        geometry: { type: 'Point', coordinates: [p.lon, p.lat] },
        properties: { id: p.id, name: p.name },
      })),
    };
  }

  private syncOverlays(s: AppState, prev: AppState | null) {
    if (!this.overlaysReady) return;
    const m = this.map;
    if (s.alertsRevision !== this.alertsRevision) {
      this.alertsRevision = s.alertsRevision;
      (m.getSource('alerts') as VectorTileSource | undefined)?.setTiles([alertTileUrl(s.alertsRevision)]);
    }
    if (!prev || s.alerts !== prev.alerts) (m.getSource('alerts-unshaped') as GeoJSONSource | undefined)?.setData(this.unshapedData(s.alerts));
    if (!prev || s.stations !== prev.stations || s.units.temp !== prev.units.temp) {
      (m.getSource('stations') as GeoJSONSource | undefined)?.setData(this.stationData(s));
    }
    if (!prev || s.places !== prev.places) (m.getSource('places') as GeoJSONSource | undefined)?.setData(this.placeData(s.places));
    if (!prev || s.overlays !== prev.overlays) {
      const vis = (on: boolean) => (on ? 'visible' : 'none');
      for (const id of ['alerts-fill', 'alerts-line', 'alerts-fresh', 'alerts-unshaped', 'alerts-unshaped-mark']) m.setLayoutProperty(id, 'visibility', vis(s.overlays.alerts));
      for (const id of ['stations-wind', 'stations-dot', 'stations-buoy', 'stations-label']) m.setLayoutProperty(id, 'visibility', vis(s.overlays.stations));
      m.setLayoutProperty('places-pin', 'visibility', vis(s.overlays.places));
    }
    const alertId = s.panel?.kind === 'alert' ? s.panel.id : null;
    if (alertId !== this.selectedAlert) {
      const mark = (id: string, selected: boolean) => {
        m.setFeatureState({ source: 'alerts', sourceLayer: 'alerts', id }, { selected });
        m.setFeatureState({ source: 'alerts-unshaped', id }, { selected });
      };
      if (this.selectedAlert) mark(this.selectedAlert, false);
      if (alertId) mark(alertId, true);
      this.selectedAlert = alertId;
    }
    if (!prev || s.freshAlerts !== prev.freshAlerts) this.pulseFresh(s.freshAlerts);
  }

  /** New alerts pulse their outline a few times as they arrive. */
  private pulseFresh(ids: string[]) {
    const m = this.map;
    cancelAnimationFrame(this.pulse);
    m.setFilter('alerts-fresh', ['in', ['get', 'id'], ['literal', ids]]);
    if (!ids.length) return;
    if (this.opts.reducedMotion()) {
      m.setPaintProperty('alerts-fresh', 'line-opacity', 0.8);
      m.setPaintProperty('alerts-fresh', 'line-width', 5);
      return;
    }
    const start = performance.now();
    const step = (t: number) => {
      // The style may be mid-rebuild (a restored context, a new basemap).
      if (!this.overlaysReady || !m.getLayer('alerts-fresh')) return;
      const k = (t - start) / 1500;
      if (k > 3) {
        m.setPaintProperty('alerts-fresh', 'line-opacity', 0);
        return;
      }
      const f = k % 1;
      m.setPaintProperty('alerts-fresh', 'line-width', 2 + 14 * f);
      m.setPaintProperty('alerts-fresh', 'line-opacity', 0.9 * (1 - f));
      this.pulse = requestAnimationFrame(step);
    };
    this.pulse = requestAnimationFrame(step);
  }

  // ------------------------------------------------------------ display

  private display(s: AppState): WeatherDisplay {
    const layer = currentLayer(s) ?? null;
    const frames = currentFrames(s);
    if (!layer || !frames.length) return { layer, a: null, b: null, mix: 0, fallback: null, prefetch: [] };
    if (layer.id !== this.lastLayerId) {
      this.lastLayerId = layer.id;
      this.lastReady = null;
    }
    const reduced = this.opts.reducedMotion();
    let a: TFrame;
    let b: TFrame | null = null;
    let mix = 0;
    let loop: TFrame[] | undefined;
    const prefetch: TFrame[] = [];
    if (s.playing && s.playhead !== null) {
      const [lo, hi] = this.opts.playback.loop;
      const i = Math.max(0, Math.min(frames.length - 1, Math.floor(s.playhead)));
      a = frames[i];
      if (i + 1 <= hi && frames[i + 1]) {
        b = frames[i + 1];
        mix = Playback.mix(s.playhead, reduced);
      }
      const span = Math.max(1, hi - lo + 1);
      for (let k = 2; k <= 10 && k < span; k++) {
        const j = lo + ((i - lo + k) % span);
        if (frames[j]) prefetch.push(frames[j]);
      }
      const l = this.loop;
      if (l?.frames !== frames || l.lo !== lo || l.hi !== hi) this.loop = { frames, lo, hi, list: frames.slice(lo, hi + 1) };
      loop = this.loop!.list;
    } else {
      const i = selectedIndex(s);
      a = frames[i];
      for (const d of [1, -1, 2, -2, 3, -3, 4, -4, 5, 6]) if (frames[i + d]) prefetch.push(frames[i + d]);
    }
    // By identity: a time step whose parts changed is a new frame object,
    // and the old one is just what should fill in while the new one loads.
    const fallback = this.lastReady && this.lastReady !== a ? this.lastReady : null;
    return { layer, a, b, mix, fallback, prefetch, loop };
  }

  private applyStyle(s: AppState) {
    const layer = currentLayer(s);
    this.layer.setStyle({
      palette: getPalette(layer?.palette ?? 'generic'),
      stepped: s.stepped,
      opacity: s.opacity,
      windByGust: s.windByGust,
      arrows: layer?.render.kind === 'wind' && windMarksShown(s) === 'arrows',
    });
    this.particles.setActive(layer?.render.kind === 'wind' && windMarksShown(s) === 'particles', Math.min(1, 0.45 + 0.55 * s.opacity));
  }

  private bind() {
    const m = this.map;
    const app = this.app;
    this.layer.setDisplay(this.display(app.get()));
    this.applyStyle(app.get());
    this.updateCredits(app.get());
    this.unsub = app.subscribe((s, prev) => {
      if (
        s.layerId !== prev.layerId ||
        s.timelines !== prev.timelines ||
        s.targetTime !== prev.targetTime ||
        s.following !== prev.following ||
        s.playing !== prev.playing ||
        s.playhead !== prev.playhead ||
        s.clockOffset !== prev.clockOffset ||
        s.layers !== prev.layers
      ) {
        this.layer.setDisplay(this.display(s));
        this.refreshReadout();
        this.updateCentre();
        if (s.layerId !== prev.layerId || s.timelines !== prev.timelines) this.publishViewSources();
      }
      if (
        s.layerId !== prev.layerId ||
        s.layers !== prev.layers ||
        s.stepped !== prev.stepped ||
        s.opacity !== prev.opacity ||
        s.windByGust !== prev.windByGust ||
        s.windMarks !== prev.windMarks ||
        s.particlesOk !== prev.particlesOk ||
        s.reducedMotion !== prev.reducedMotion
      ) {
        this.applyStyle(s);
      }
      if (s.units !== prev.units || s.windByGust !== prev.windByGust || s.sources !== prev.sources) this.refreshReadout();
      if (s.viewSources !== prev.viewSources || s.sources !== prev.sources || s.overlays !== prev.overlays) this.updateCredits(s);
      this.syncOverlays(s, prev);
      if (s.probe !== prev.probe) this.refreshReadout();
    });

    m.on('sourcedata', (e: { sourceId?: string; tile?: unknown }) => {
      if (e.sourceId === 'protomaps' && e.tile) this.archiveErrors.succeed();
    });
    // While the context is lost MapLibre has no style at all; overlays come
    // back with the `style.load` that follows the restore.
    m.on('webglcontextlost', () => {
      this.overlaysReady = false;
      cancelAnimationFrame(this.pulse);
    });

    m.on('render', () => {
      const d = this.layer.getDisplay();
      if (d.a && d.a !== this.lastReady && this.layer.isReady(d.a)) this.lastReady = d.a;
    });

    m.on('click', (e) => this.onClick(e));
    m.on('mousemove', (e) => {
      if (!this.hover.matches || !this.overlaysReady) return;
      const ll = e.lngLat.wrap();
      this.mouse = { x: e.point.x, y: e.point.y, lng: ll.lng, lat: ll.lat };
      const over = m.queryRenderedFeatures(e.point, { layers: ['alerts-fill', 'alerts-unshaped', 'places-pin', 'stations-dot', 'stations-buoy'].filter((id) => m.getLayer(id)) });
      m.getCanvas().style.cursor = over.length ? 'pointer' : '';
      this.refreshReadout();
    });
    m.getCanvas().addEventListener('mouseleave', () => {
      this.mouse = null;
      if (!app.get().probe) app.set({ readout: null });
    });
    m.on('move', () => {
      // The map moved under a still pointer: what is under it changed too.
      if (app.get().probe || this.mouse) this.refreshReadout();
    });
    // Keyboard users: while the map has keyboard focus, read the value under
    // a centre crosshair (the arrow keys pan under it), and Enter opens the
    // forecast there, as a tap would.
    const canvas = m.getCanvas();
    let probeFromFocus = false;
    canvas.addEventListener('focus', () => {
      if (!canvas.matches(':focus-visible') || app.get().probe) return;
      probeFromFocus = true;
      app.set({ probe: true });
    });
    canvas.addEventListener('blur', () => {
      if (!probeFromFocus) return;
      probeFromFocus = false;
      app.set({ probe: false, readout: null });
    });
    canvas.addEventListener('keydown', (e) => {
      if (e.key !== 'Enter') return;
      const c = m.getCenter().wrap();
      app.set({ panel: { kind: 'point', lat: c.lat, lon: c.lng } });
    });
    m.on('idle', () => {
      this.refreshReadout();
      this.refreshOverlayCredits();
    });
    // Opened again later, the map comes back here; and the lists fetched by
    // area follow the view.
    const settled = () => {
      const c = m.getCenter().wrap();
      saveView(c.lng, c.lat, m.getZoom());
      const b = m.getBounds();
      app.set({ viewBox: viewBox(b.getWest(), b.getSouth(), b.getEast(), b.getNorth()) });
    };
    m.on('moveend', settled);
    m.on('moveend', () => {
      this.updateCentre();
      this.publishViewSources();
    });
    // The first view's centre, before anything has moved or changed.
    this.updateCentre();
    m.once('idle', () => this.updateCentre());
    if (m.loaded()) settled();
    else m.once('load', settled);
  }

  /** Who to credit for the alerts and stations on screen: the NWS and the
   *  other national services for alerts drawn, EuroGeographics while any
   *  shape drawn from NUTS regions is, and each station feed shown. */
  private refreshOverlayCredits() {
    const m = this.map;
    const s = this.app.get();
    const out: { text: string; url?: string }[] = [];
    if (this.overlaysReady && s.overlays.alerts) {
      const ids = ['alerts-fill', 'alerts-unshaped'].filter((id) => m.getLayer(id));
      const drawn = ids.length ? m.queryRenderedFeatures({ layers: ids }) : [];
      const props = drawn.map((f) => f.properties as { source?: string; geom_source?: string });
      if (props.some((p) => p.source === 'us-nws')) out.push({ text: 'Alerts: NOAA/NWS', url: 'https://www.weather.gov/' });
      if (props.some((p) => p.source && p.source !== 'us-nws')) out.push({ text: 'Alerts: national weather services (WMO register)', url: 'https://severeweather.wmo.int/' });
      if (props.some((p) => p.geom_source === 'nuts')) out.push({ text: s.alertsAttribution ?? '© EuroGeographics for the administrative boundaries' });
    }
    if (this.overlaysReady && s.overlays.stations && s.stations && m.getLayer('stations-dot')) {
      const shown = new Set(m.queryRenderedFeatures({ layers: ['stations-dot', 'stations-buoy'].filter((id) => m.getLayer(id)) }).map((f) => (f.properties as { id?: string }).id));
      const feeds = new Set(s.stations.features.filter((f) => shown.has(f.properties.id)).map((f) => f.properties.source));
      for (const k of feeds) {
        const d = s.stations.sources?.[k];
        if (d) out.push({ text: d.attribution });
      }
    }
    if (out.map((c) => c.text).join('|') === this.overlayCredits.map((c) => c.text).join('|')) return;
    this.overlayCredits = out;
    this.updateCredits(s);
  }

  /** Credit every source with a part in view, next to the basemap's. */
  private updateCredits(s: AppState) {
    // Sources often share a credit (one agency's regions, one model's
    // domains): each is listed once.
    const seen = new Set<string>();
    const weather: { text: string; url?: string }[] = [];
    for (const id of s.viewSources) {
      const d = s.sources[id];
      const text = d?.attribution ?? id;
      if (seen.has(text)) continue;
      seen.add(text);
      weather.push({ text, url: d?.attribution_url || undefined });
    }
    this.credits.setWeather(weather);
    const extra = this.overlayCredits.filter((c) => !seen.has(c.text) && !!seen.add(c.text));
    this.credits.setExtra(extra);
  }

  /**
   * Which parts make the value of frame `f` at a point. Certain at once
   * inside the top part's solid box; elsewhere (a ragged radar, a blend
   * zone) the server says, asked after `wait` ms with no newer question in
   * the same slot (the pointer, the view's centre), and until it has there
   * is no answer rather than a guess. Null: no data there; undefined: not
   * known (yet, or the server is not answering).
   */
  private provenanceAt(slot: 'pointer' | 'centre', layer: LayerSpec, f: TFrame, top: Part | null, lon: number, lat: number, wait: number, then: () => void): Part[] | null | undefined {
    if (!top) return null;
    if (top.solid && boxContainsPoint(top.solid, lon, lat)) return [top];
    const key = `${layer.id}|${f.t}|${Math.round(lon * 50) / 50}|${Math.round(lat * 50) / 50}`;
    if (this.provenance.has(key)) {
      const got = this.provenance.get(key);
      if (!got?.length) return null;
      // Name what makes a real share of the value, top first.
      const parts = got
        .filter((p) => p.share >= 0.05)
        .map((p) => f.stack.find((q) => q.id === p.id) ?? ({ id: p.id, source: p.source, run: null } as Part));
      return parts.length ? parts : null;
    }
    const q = this.provenanceAsk[slot];
    // A server that is failing these is not asked again for a while.
    if (performance.now() < this.provenanceQuietUntil) return undefined;
    if (key !== q.key) {
      q.key = key;
      if (q.timer) clearTimeout(q.timer);
      q.timer = setTimeout(() => {
        q.timer = null;
        api
          .provenance(layer.id, f.valid, lon, lat)
          .then((r) => {
            this.provenanceBackoff = PROVENANCE_BACKOFF_MS;
            return r.parts;
          })
          .catch(() => {
            this.provenanceQuietUntil = performance.now() + this.provenanceBackoff;
            this.provenanceBackoff = Math.min(PROVENANCE_BACKOFF_MAX_MS, this.provenanceBackoff * 2);
            // Asked again once the pause is over, not cached as an answer.
            q.key = '';
            return undefined;
          })
          .then((parts) => {
            if (parts === undefined) return;
            if (this.provenance.size > 400) this.provenance.delete(this.provenance.keys().next().value as string);
            this.provenance.set(key, parts);
            this.answers.push({ layer: layer.id, lon, lat, named: new Set(parts.filter((p) => p.share > 0).map((p) => p.source)), stack: new Set(f.stack.map((p) => p.source)) });
            if (this.answers.length > 200) this.answers.splice(0, this.answers.length - 200);
            this.publishViewSources();
            then();
          });
      }, wait);
    }
    return undefined;
  }

  /** The sources to credit for the view: those the layer found by bounds,
   *  less models that provenance answers in view show have no data here. */
  private publishViewSources() {
    const s = this.app.get();
    const layer = currentLayer(s);
    const f = this.layer.getDisplay().a;
    let found = this.boundsSources;
    if (layer && f) {
      const b = this.map.getBounds();
      found = creditedSources(found, f.stack, s.sources, viewBox(b.getWest(), b.getSouth(), b.getEast(), b.getNorth()), layer.id, this.answers);
    }
    // Top first: the order sources stack in.
    const ids = [...found].sort((a, b) => (s.sources[b]?.rank ?? 0) - (s.sources[a]?.rank ?? 0));
    const cur = s.viewSources;
    if (cur.length !== ids.length || cur.some((id, i) => id !== ids[i])) this.app.set({ viewSources: ids });
  }

  /** Where the value under the pointer comes from, and whether only a
   *  satellite estimate looked there. */
  private provenanceOf(layer: LayerSpec, sample: Sample | null, lon: number, lat: number): { parts: Part[]; satelliteOnly: boolean } | null {
    if (!sample) return null;
    const parts = this.provenanceAt('pointer', layer, sample.frame, sample.top, lon, lat, PROVENANCE_REST_MS, () => this.refreshReadout());
    if (!parts) return null;
    const s = this.app.get();
    return { parts, satelliteOnly: parts.every((p) => isSatelliteEstimate(s.sources[p.source])) };
  }

  /**
   * What is on top at the centre of the view in the frame shown, for the
   * time bar's label (a label may guess; the read-out may not). Settles after the view and the time stop changing (a
   * playing loop keeps what it had while the source is still in the frame).
   */
  private updateCentre() {
    // Asked every tick while playing: one timer, put off when it fires early,
    // rather than a new one each time.
    this.centreDue = performance.now() + CENTRE_SETTLE_MS;
    if (!this.centreTimer) this.centreTimer = setTimeout(this.centreSettled, CENTRE_SETTLE_MS);
  }

  private centreSettled = () => {
    const wait = this.centreDue - performance.now();
    if (wait > 0) {
      this.centreTimer = setTimeout(this.centreSettled, wait);
      return;
    }
    this.centreTimer = null;
    const s = this.app.get();
    const layer = currentLayer(s);
    const f = this.layer.getDisplay().a;
    if (!layer || !f) return;
    const c = this.map.getCenter().wrap();
    // Certain inside the top part's solid box; elsewhere the server says
    // (a box may reach far past its data). Until it has, the source
    // already named stays if this frame has it, so a step does not flash
    // the name of a source whose box merely covers the centre.
    const top = topPartAt(f.stack, c.lng, c.lat);
    const parts = this.provenanceAt('centre', layer, f, top, c.lng, c.lat, 0, () => this.updateCentre());
    const held = s.centreSource && f.stack.some((p) => p.source === s.centreSource) ? s.centreSource : null;
    const centre = parts === undefined ? held : (parts?.[0]?.source ?? null);
    if (centre !== s.centreSource) this.app.set({ centreSource: centre });
  };

  private readoutTimer = 0;
  /** Recompute the value under the cursor (or the probe crosshair), once in
   *  the next animation frame. A request already pending is kept: playback,
   *  the map's renders and the pointer all ask every frame, and cancelling
   *  to ask again would put the read-out off for as long as they keep at it. */
  refreshReadout() {
    if (this.readoutTimer) return;
    this.readoutTimer = requestAnimationFrame(() => {
      this.readoutTimer = 0;
      const s = this.app.get();
      const layer = currentLayer(s);
      let pt: { x: number; y: number; lng: number; lat: number } | null = null;
      if (s.probe) {
        const c = this.map.getCenter().wrap();
        const canvas = this.map.getCanvas();
        pt = { x: canvas.clientWidth / 2, y: canvas.clientHeight / 2, lng: c.lng, lat: c.lat };
      } else if (this.mouse) {
        const ll = this.map.unproject([this.mouse.x, this.mouse.y]).wrap();
        pt = { x: this.mouse.x, y: this.mouse.y, lng: ll.lng, lat: ll.lat };
      }
      if (!pt || !layer) {
        if (s.readout) this.app.set({ readout: null });
        return;
      }
      const sample = this.layer.sample(pt.lng, pt.lat);
      const d = describeSample(layer, sample, s.units, s.stepped, s.windByGust);
      const top = sample?.top ?? null;
      // An accumulation counts from its parts' run: say since when.
      const since = layer.anchored && top?.run ? `Since ${formatDateTime(top.run, serverNow(s))}` : undefined;
      const from = this.provenanceOf(layer, sample, pt.lng, pt.lat);
      const names = from?.parts.map((p) => partLabel(p, s.sources));
      // A satellite estimate sees rain or none; "echo" and its thresholds
      // are a radar's words.
      const v = sample?.values[0];
      const dry = from?.satelliteOnly && layer.bands[0]?.quantity === 'reflectivity' && v !== null && v !== undefined && v < 10;
      const text = dry ? 'No rain seen (satellite estimate)' : d.text;
      this.app.set({
        readout: {
          x: pt.x,
          y: pt.y,
          lat: pt.lat,
          lon: pt.lng,
          text,
          detail: dry ? undefined : (d.detail ?? since),
          source: names?.length ? `From ${names.length > 2 ? `${names.slice(0, 2).join(', ')} and ${names.length - 2} more` : names.join(' and ')}` : undefined,
          swatch: d.swatch,
          centre: s.probe,
        },
      });
    });
  }

  private onClick(e: MapMouseEvent) {
    const m = this.map;
    if (!this.overlaysReady) return;
    const layers = ['places-pin', 'alerts-unshaped', 'alerts-fill'].filter((id) => m.getLayer(id) && m.getLayoutProperty(id, 'visibility') !== 'none');
    const hits = m.queryRenderedFeatures(e.point, { layers });
    const place = hits.find((h) => h.layer.id === 'places-pin');
    if (place) {
      this.app.set({ panel: { kind: 'places' } });
      return;
    }
    // Most severe first; within a level, warnings before watches.
    const seen = new Set<string>();
    const alerts = hits
      .filter((h) => h.layer.id === 'alerts-fill' || h.layer.id === 'alerts-unshaped')
      .map((h) => h.properties as { id: string; event: string; severity?: string; level?: number })
      .filter((p) => !seen.has(p.id) && !!seen.add(p.id))
      .sort((a, b) => alertClickRank(b) - alertClickRank(a));
    // Past the antimeridian MapLibre reports longitudes like 250; the server
    // (rightly) rejects those.
    const at = e.lngLat.wrap();
    if (alerts.length) {
      this.app.set({
        panel: { kind: 'alert', id: alerts[0].id, others: alerts.slice(1).map((a) => a.id), at: { lat: at.lat, lon: at.lng } },
      });
      return;
    }
    this.app.set({ panel: { kind: 'point', lat: at.lat, lon: at.lng } });
  }

  // ------------------------------------------------------------- camera

  flyTo(lon: number, lat: number, zoom?: number) {
    const opts = { center: [lon, lat] as [number, number], zoom: zoom ?? Math.max(this.map.getZoom(), 7) };
    if (this.opts.reducedMotion()) this.map.jumpTo(opts);
    else this.map.flyTo({ ...opts, duration: 1400, essential: false });
  }

  /** Show an alert's whole area. One without a shape (or not in the view's
   *  list) is shown at its anchor, the point the server gives for it. False
   *  when there is nothing to go on. */
  fitAlert(id: string, anchor?: [number, number] | null) {
    const f = this.app.get().alerts?.features.find((x) => x.properties.id === id);
    const at = f?.properties.anchor ?? anchor;
    if (!f?.geometry) {
      if (!at) return false;
      this.flyTo(at[0], at[1], Math.min(Math.max(this.map.getZoom(), 4), 6));
      return true;
    }
    const lons: number[] = [];
    let s = 90;
    let n = -90;
    const walk = (c: unknown) => {
      if (!Array.isArray(c)) return;
      if (typeof c[0] !== 'number') {
        c.forEach(walk);
        return;
      }
      lons.push(c[0]);
      s = Math.min(s, c[1] as number);
      n = Math.max(n, c[1] as number);
    };
    walk(f.geometry.coordinates);
    if (!lons.length) return false;
    const range = (xs: number[]) => xs.reduce(([lo, hi], x) => [Math.min(lo, x), Math.max(hi, x)], [Infinity, -Infinity]);
    let [w, e] = range(lons);
    // A shape across the antimeridian (the Aleutians) spans little once its
    // western longitudes are counted past 180: fit that instead of the world.
    if (e - w > 180) {
      const [sw, se] = range(lons.map((x) => (x < 0 ? x + 360 : x)));
      if (se - sw < e - w) [w, e] = [sw, se];
    }
    const narrow = this.map.getContainer().clientWidth < 700;
    const padding = narrow ? { top: 80, bottom: Math.round(window.innerHeight * 0.55), left: 30, right: 30 } : { top: 90, bottom: 170, left: 60, right: 460 };
    this.map.fitBounds(
      [
        [w, s],
        [e, n],
      ],
      { padding, maxZoom: 9, duration: this.opts.reducedMotion() ? 0 : 1300 },
    );
    return true;
  }

  /** The frame index the weather layer is drawing, and whether it is loaded. */
  isReadyIndex(i: number): boolean {
    const f = currentFrames(this.app.get())[i];
    return f ? this.layer.isReady(f) : true;
  }

  destroy() {
    this.unsub?.();
    if (this.centreTimer) clearTimeout(this.centreTimer);
    for (const q of Object.values(this.provenanceAsk)) if (q.timer) clearTimeout(q.timer);
    cancelAnimationFrame(this.readoutTimer);
    cancelAnimationFrame(this.pulse);
    this.map.remove();
  }
}

