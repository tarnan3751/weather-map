import 'maplibre-gl/dist/maplibre-gl.css';
import './styles/app.css';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { DataController, defaultDeps } from './app/data.ts';
import { LiveSocket } from './api/live.ts';
import { Playback } from './app/playback.ts';
import { onDeviceRegistered } from './api/device.ts';
import type { Config } from './api/types.ts';
import { createAppStore, currentFrames, currentLayer, selectedIndex, windMarksShown } from './app/state.ts';
import { buildScale } from './core/timeline.ts';
import type { MapController } from './map/mapController.ts';
import { App } from './ui/App.tsx';
import { Ctx, type AppContext } from './ui/context.ts';

/** Push and the service worker: not needed to draw the map, so loaded after it starts. */
const push = () => import('./app/push.ts');

const store = createAppStore();
let map: MapController | null = null;
const motion = matchMedia('(prefers-reduced-motion: reduce)');
const reducedMotion = () => motion.matches;
store.set({ reducedMotion: motion.matches });
motion.addEventListener('change', () => store.set({ reducedMotion: motion.matches }));

const playback = new Playback(store, () => (map ? { isReadyIndex: (i) => map!.isReadyIndex(i) } : null), reducedMotion);
// `?livestale=MS` shortens the dead-connection timeout, for tests.
const staleMs = Number(new URLSearchParams(location.search).get('livestale')) || undefined;
const data = new DataController(
  store,
  { booted: (config) => onBooted(config) },
  { ...defaultDeps, createLive: (h) => new LiveSocket(h, { staleMs }) },
);

/** Open an alert from a notification's `url` (`/?alert=…&lat=…&lon=…`). */
async function openFromUrl(url: string) {
  const u = new URL(url, location.href);
  const id = u.searchParams.get('alert');
  const lat = Number(u.searchParams.get('lat'));
  const lon = Number(u.searchParams.get('lon'));
  if (!id) return;
  if (!store.get().alerts?.features.some((f) => f.properties.id === id)) await data.loadAlerts();
  store.set({ panel: { kind: 'alert', id } });
  const fly = () => {
    if (!map) return;
    if (!map.fitAlert(id) && Number.isFinite(lat) && Number.isFinite(lon)) map.flyTo(lon, lat, 8);
  };
  // Only the style needs to be there; `loaded()` also waits for every tile,
  // which may not happen while frames are being preloaded.
  const ready = () => !!map && map.map.isStyleLoaded() === true;
  if (ready()) fly();
  else {
    const wait = setInterval(() => {
      if (ready()) {
        clearInterval(wait);
        fly();
      }
    }, 200);
    setTimeout(() => clearInterval(wait), 30_000);
  }
}

/** Things to do once the first load has worked, whenever that is. */
function onBooted(config: Config) {
  if (new URLSearchParams(location.search).has('alert')) {
    const url = location.href;
    history.replaceState(null, '', location.pathname);
    void openFromUrl(url);
  }
  // The server may have forgotten this browser's push subscription (a new
  // device after a 401, a reset database, a changed key): tell it again.
  void push().then((p) => p.resyncPush(config.vapid_public_key));
}

const ctx: AppContext = { store, data, playback, map: () => map, reducedMotion };

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <Ctx.Provider value={ctx}>
      <App
        setMap={(m) => {
          map = m;
          if (m && store.get().basemap === 'fallback') {
            store.set((s) => ({
              toasts: [...s.toasts, { id: -1, title: 'Simplified basemap', body: 'The detailed basemap is unavailable; showing outlines instead.' }],
            }));
          }
        }}
        retry={() => data.retryNow()}
      />
    </Ctx.Provider>
  </StrictMode>,
);

data.start();
onDeviceRegistered(() => void push().then((p) => p.resyncPush(store.get().config?.vapid_public_key ?? null)));

window.addEventListener('load', () => void push().then((p) => p.registerServiceWorker()));
navigator.serviceWorker?.addEventListener('message', (e: MessageEvent<{ type?: string; url?: string }>) => {
  if (e.data?.type === 'open-alert' && e.data.url) void openFromUrl(e.data.url);
});

// Hooks for the scripted smoke test; nothing here exposes credentials.
if (import.meta.env.DEV || new URLSearchParams(location.search).has('debug')) {
  Object.assign(window, {
    __wx: {
      store,
      playback,
      get map() {
        return map;
      },
      frames: () => currentFrames(store.get()),
      positions: () => buildScale(currentFrames(store.get())).pos,
      live: () => data.liveStats,
      decoder: () => store.get().decoder,
      state: () => {
        const s = store.get();
        return {
          layer: currentLayer(s)?.id,
          index: selectedIndex(s),
          panel: s.panel,
          basemap: s.basemap,
          live: s.live,
          decoder: s.decoder,
          windMarks: s.windMarks,
          windMarksShown: windMarksShown(s),
          particlesOk: s.particlesOk,
        };
      },
    },
  });
}
