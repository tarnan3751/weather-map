import type { LayerGroup } from '../api/types.ts';
import { currentLayer, savePrefs, setPrefs, windMarksShown } from '../app/state.ts';
import { useApp, useAppState } from './context.ts';
import { Segmented } from './Settings.tsx';

const GROUPS: { id: LayerGroup; title: string }[] = [
  { id: 'precipitation', title: 'Precipitation' },
  { id: 'temperature', title: 'Temperature' },
  { id: 'wind', title: 'Wind' },
  { id: 'sky', title: 'Sky' },
];

export function LayerMenu() {
  const { store, data, playback } = useApp();
  const layers = useAppState((s) => s.layers);
  const active = useAppState(currentLayer);
  const opacity = useAppState((s) => s.opacity);
  const overlays = useAppState((s) => s.overlays);
  const windMarks = useAppState((s) => s.windMarks);
  const shown = useAppState(windMarksShown);
  const reduced = useAppState((s) => s.reducedMotion);
  const timelines = useAppState((s) => s.timelines);

  const choose = (id: string) => {
    const wasPlaying = store.get().playing;
    if (wasPlaying) playback.pause();
    // The chosen time carries over: each layer shows its frame nearest to it.
    setPrefs(store, { layerId: id });
    if (wasPlaying) playback.play();
  };

  const toggle = (k: keyof typeof overlays) => {
    const next = { ...store.get().overlays, [k]: !store.get().overlays[k] };
    setPrefs(store, { overlays: next });
    if (k === 'stations' && next.stations) data.ensureStations();
  };

  const known = new Set(GROUPS.map((g) => g.id));
  const groups = [...GROUPS, ...[...new Set(layers.map((l) => l.group))].filter((g) => !known.has(g)).map((g) => ({ id: g, title: g }))];

  return (
    <div className="layer-menu">
      {groups.map((g) => {
        const items = layers.filter((l) => l.group === g.id);
        if (!items.length) return null;
        return (
          <section key={g.id} className="menu-group" aria-labelledby={`grp-${g.id}`}>
            <h3 id={`grp-${g.id}`} className="menu-heading">
              {g.title}
            </h3>
            <ul role="list">
              {items.map((l) => {
                // A layer not looked at yet has no timeline here, which is not the same as none.
                const n = timelines[l.id]?.frames.length ?? -1;
                return (
                  <li key={l.id}>
                    <button className={`layer-item ${active?.id === l.id ? 'active' : ''}`} aria-pressed={active?.id === l.id} onClick={() => choose(l.id)}>
                      <span className="layer-name">{l.title}</span>
                      <span className="layer-desc">{n !== 0 ? l.description : 'No frames yet'}</span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </section>
        );
      })}

      <section className="menu-group" aria-labelledby="grp-display">
        <h3 id="grp-display" className="menu-heading">
          Display
        </h3>
        <label className="slider-row">
          <span>Layer opacity</span>
          <input
            type="range"
            min={20}
            max={100}
            step={5}
            value={Math.round(opacity * 100)}
            onChange={(e) => store.set({ opacity: Number(e.target.value) / 100 })}
            onPointerUp={() => savePrefs(store.get())}
            onKeyUp={() => savePrefs(store.get())}
            aria-valuetext={`${Math.round(opacity * 100)} percent`}
          />
          <output>{Math.round(opacity * 100)}%</output>
        </label>
        {active?.render.kind === 'wind' && (
          <>
            <Segmented
              label="Wind direction"
              value={windMarks}
              options={{ particles: 'Particles', arrows: 'Arrows', off: 'Off' }}
              onChange={(v) => setPrefs(store, { windMarks: v })}
            />
            {windMarks !== shown && (
              <p className="muted small menu-note">
                {reduced ? 'Showing arrows: your device asks for reduced motion.' : 'Showing arrows: this device cannot draw particles.'}
              </p>
            )}
          </>
        )}
        <label className="switch-row">
          <span>
            Alerts <small>NWS warnings, watches and advisories</small>
          </span>
          <input type="checkbox" role="switch" checked={overlays.alerts} onChange={() => toggle('alerts')} />
        </label>
        <label className="switch-row">
          <span>
            Stations <small>Airport reports: temperature and wind</small>
          </span>
          <input type="checkbox" role="switch" checked={overlays.stations} onChange={() => toggle('stations')} />
        </label>
        <label className="switch-row">
          <span>Saved places</span>
          <input type="checkbox" role="switch" checked={overlays.places} onChange={() => toggle('places')} />
        </label>
      </section>
    </div>
  );
}
