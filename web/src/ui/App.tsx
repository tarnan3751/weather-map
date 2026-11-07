import { lazy, Suspense, useCallback, useEffect, useRef, useState } from 'react';
import { currentFrames, currentLayer, selectedIndex, serverNow } from '../app/state.ts';
import { nowIndex } from '../core/timeline.ts';
import type { MapController } from '../map/mapController.ts';
import { useApp, useAppState } from './context.ts';
import { IconBell, IconChevron, IconCrosshair, IconGear, IconLayers, IconStar } from './icons.tsx';
import { Legend } from './Legend.tsx';
import { Readout } from './Readout.tsx';
import { Sheet } from './Sheet.tsx';
import { TimeBar } from './TimeBar.tsx';
import { Toasts } from './Toasts.tsx';

// MapLibre, the tile store and the weather layer are most of the code: their
// chunk is fetched now, alongside the first API requests, not before them.
const mapModule = import('../map/mapController.ts');

// The panels load on first use; once the map is up they are fetched in the
// background, so opening one does not wait for the network.
const panels = {
  alert: () => import('./AlertPanel.tsx'),
  layers: () => import('./LayerMenu.tsx'),
  notify: () => import('./NotifyPanel.tsx'),
  places: () => import('./PlacesPanel.tsx'),
  point: () => import('./PointPanel.tsx'),
  settings: () => import('./Settings.tsx'),
};
const AlertPanel = lazy(() => panels.alert().then((m) => ({ default: m.AlertPanel })));
const LayerMenu = lazy(() => panels.layers().then((m) => ({ default: m.LayerMenu })));
const NotifyPanel = lazy(() => panels.notify().then((m) => ({ default: m.NotifyPanel })));
const PlacesPanel = lazy(() => panels.places().then((m) => ({ default: m.PlacesPanel })));
const PointPanel = lazy(() => panels.point().then((m) => ({ default: m.PointPanel })));
const Settings = lazy(() => panels.settings().then((m) => ({ default: m.Settings })));
const preloadPanels = () => {
  for (const load of Object.values(panels)) void load().catch(() => {});
};

export interface AppProps {
  setMap(m: MapController | null): void;
  retry(): void;
}

function TopBar() {
  const { store } = useApp();
  const layer = useAppState(currentLayer);
  const panel = useAppState((s) => s.panel);
  const live = useAppState((s) => s.live);
  const probe = useAppState((s) => s.probe);
  const placeAlerts = useAppState((s) => s.places.reduce((n, p) => n + p.alerts.length, 0));
  const coarse = matchMedia('(pointer: coarse)').matches;
  const toggle = (kind: 'layers' | 'places' | 'notify' | 'settings') =>
    store.set((s) => ({ panel: s.panel?.kind === kind ? null : { kind } }));

  return (
    <header className="topbar">
      <div className="topbar-left">
        <span className="brand" aria-label="Weather map">
          <svg viewBox="0 0 32 32" width="26" height="26" aria-hidden="true">
            <circle cx="16" cy="16" r="15" fill="#10151d" stroke="#2b3442" />
            <path d="M8 19a5 5 0 0 1 4.6-5 6.5 6.5 0 0 1 12.4 2.4A3.6 3.6 0 0 1 24.4 23H11a3 3 0 0 1-3-4Z" fill="#6cc4ff" />
            <path d="M12 25.5 11 28M16.5 25.5 15.5 28M21 25.5 20 28" stroke="#5fd4b0" strokeWidth="1.8" strokeLinecap="round" />
          </svg>
        </span>
        <button className={`btn layer-btn ${panel?.kind === 'layers' ? 'on' : ''}`} onClick={() => toggle('layers')} aria-expanded={panel?.kind === 'layers'} aria-keyshortcuts="L">
          <IconLayers size={18} />
          <span className="layer-btn-text">{layer?.title ?? 'Layers'}</span>
          <IconChevron size={16} />
        </button>
        <span className={`live-pill ${live === 'open' ? 'ok' : 'warn'}`} title={live === 'open' ? 'Live updates connected' : 'Reconnecting…'}>
          <span className="status-dot" aria-hidden="true" />
          <span className="live-text">{live === 'open' ? 'Live' : 'Reconnecting'}</span>
        </span>
      </div>
      <div className="topbar-right">
        {coarse && (
          <button className={`btn icon ${probe ? 'on' : ''}`} aria-pressed={probe} aria-label="Read values under a crosshair" onClick={() => store.set({ probe: !probe, readout: null })}>
            <IconCrosshair />
          </button>
        )}
        <button className={`btn icon ${panel?.kind === 'places' ? 'on' : ''}`} aria-label="Saved places" aria-expanded={panel?.kind === 'places'} onClick={() => toggle('places')}>
          <IconStar />
          {placeAlerts > 0 && <span className="badge" aria-label={`${placeAlerts} alerts at saved places`}>{placeAlerts}</span>}
        </button>
        <button className={`btn icon ${panel?.kind === 'notify' ? 'on' : ''}`} aria-label="Notifications" aria-expanded={panel?.kind === 'notify'} onClick={() => toggle('notify')}>
          <IconBell />
        </button>
        <button className={`btn icon ${panel?.kind === 'settings' ? 'on' : ''}`} aria-label="Settings" aria-expanded={panel?.kind === 'settings'} onClick={() => toggle('settings')}>
          <IconGear />
        </button>
      </div>
    </header>
  );
}

/** The view's alert list was capped: say that the markers and news shown
 *  are the most severe ones only. */
function AlertsNotice() {
  const shown = useAppState((s) => (s.overlays.alerts && s.alerts?.truncated ? s.alerts.features.length : 0));
  if (!shown) return null;
  return (
    <p className="alerts-notice" role="status">
      Showing the {shown} most severe alerts here; zoom in for the rest.
    </p>
  );
}

function Panels() {
  const panel = useAppState((s) => s.panel);
  if (!panel) return null;
  switch (panel.kind) {
    case 'layers':
      return (
        <Sheet title="Layers" side="left" labelId="layers-title">
          <LayerMenu />
        </Sheet>
      );
    case 'settings':
      return (
        <Sheet title="Settings" labelId="settings-title">
          <Settings />
        </Sheet>
      );
    case 'point':
      return <PointPanel lat={panel.lat} lon={panel.lon} key={`${panel.lat},${panel.lon}`} />;
    case 'alert':
      return <AlertPanel id={panel.id} others={panel.others} at={panel.at} key={panel.id} />;
    case 'places':
      return <PlacesPanel />;
    case 'notify':
      return <NotifyPanel />;
  }
}

/** Global shortcuts, ignored while typing or when a control has focus. */
function useShortcuts() {
  const { store, playback } = useApp();
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (t?.closest('input, textarea, select, button, a, [role="slider"], [contenteditable="true"], summary')) return;
      const s = store.get();
      const frames = currentFrames(s);
      const step = (d: number) => {
        const i = Math.max(0, Math.min(frames.length - 1, selectedIndex(s) + d));
        if (s.playing) playback.pause();
        if (frames[i]) store.set({ targetTime: frames[i].t, following: i === nowIndex(frames, serverNow(s)) });
      };
      if (e.key === ' ') playback.toggle();
      else if (e.key === '[') step(-1);
      else if (e.key === ']') step(1);
      else if (e.key === 'n' || e.key === 'N') {
        if (s.playing) playback.pause();
        store.set({ following: true });
      } else if (e.key === 'l' || e.key === 'L') store.set({ panel: s.panel?.kind === 'layers' ? null : { kind: 'layers' } });
      else return;
      e.preventDefault();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [store, playback]);
}

export function App({ setMap, retry }: AppProps) {
  const ctx = useApp();
  const mapEl = useRef<HTMLDivElement>(null);
  const config = useAppState((s) => s.config);
  const error = useAppState((s) => s.error);
  const [ready, setReady] = useState(false);
  useShortcuts();
  const bottomObserver = useRef<ResizeObserver | null>(null);
  // Map attribution and controls sit above the legend and time bar.
  const bottomRef = useCallback((el: HTMLDivElement | null) => {
    bottomObserver.current?.disconnect();
    const app = el?.closest('.app') as HTMLElement | null;
    if (!el || !app) return;
    bottomObserver.current = new ResizeObserver(() => app.style.setProperty('--bottom-h', `${el.offsetHeight}px`));
    bottomObserver.current.observe(el);
  }, []);

  useEffect(() => {
    const host = mapEl.current;
    if (!config || !host) return;
    // Each map gets its own element, so one torn down late (React may mount
    // effects twice in development) cannot strip classes from its successor.
    const el = document.createElement('div');
    el.className = 'map-inner';
    host.appendChild(el);
    let ctl: MapController | null = null;
    let cancelled = false;
    void mapModule
      .then(({ MapController }) => MapController.create(el, ctx.store, config, { reducedMotion: ctx.reducedMotion, playback: ctx.playback }))
      .then((c) => {
        if (cancelled) {
          c.destroy();
          el.remove();
          return;
        }
        ctl = c;
        setMap(c);
        // Show the map as soon as the basemap style is drawn; MapLibre's
        // `load` waits for every tile in view, which can take a while.
        const check = () => {
          if (!c.map.isStyleLoaded()) return;
          c.map.off('render', check);
          setReady(true);
          setTimeout(preloadPanels, 1500);
        };
        c.map.on('render', check);
        check();
      })
      .catch((e: Error) => ctx.store.set({ error: `The map could not start: ${e.message}` }));
    return () => {
      cancelled = true;
      if (ctl) {
        ctl.destroy();
        el.remove();
        setMap(null);
      }
    };
    // One map per configuration; the controllers in `ctx` never change.
  }, [config]);

  return (
    <div className="app">
      <div ref={mapEl} className="map" aria-label="Weather map. Click or tap for the forecast at a point." />
      <TopBar />
      <AlertsNotice />
      <Suspense fallback={null}>
        <Panels />
      </Suspense>
      <div className="bottom" ref={bottomRef}>
        <Legend />
        <TimeBar />
      </div>
      <Readout />
      <Toasts />
      {!ready && !error && (
        <div className="splash" role="status">
          <span className="spinner" aria-hidden="true" />
          <span>Loading weather…</span>
        </div>
      )}
      {error && (
        <div className="splash error" role="alert">
          <p>{error}</p>
          <button className="btn primary" onClick={retry}>
            Try again now
          </button>
        </div>
      )}
    </div>
  );
}
