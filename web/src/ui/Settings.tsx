import { useId } from 'react';
import { setPrefs } from '../app/state.ts';
import { saveUnits, SPEED_LABEL, type Units } from '../core/units.ts';
import { useApp, useAppState } from './context.ts';

/** A setting with one button per choice; `options` maps each value to its words, in order. */
export function Segmented<T extends string>({ label, value, options, onChange }: { label: string; value: T; options: Record<T, string>; onChange: (v: T) => void }) {
  // Not derived from the label: aria-labelledby splits ids at spaces.
  const id = useId();
  return (
    <div className="setting">
      <span className="setting-label" id={id}>
        {label}
      </span>
      <div className="seg-control" role="group" aria-labelledby={id}>
        {(Object.entries(options) as [T, string][]).map(([v, text]) => (
          <button key={v} aria-pressed={value === v} onClick={() => onChange(v)}>
            {text}
          </button>
        ))}
      </div>
    </div>
  );
}

export function Settings() {
  const { store } = useApp();
  const units = useAppState((s) => s.units);
  const stepped = useAppState((s) => s.stepped);
  const basemap = useAppState((s) => s.basemap);
  const live = useAppState((s) => s.live);
  const decoder = useAppState((s) => s.decoder);
  const sources = useAppState((s) => s.sources);

  const setUnits = (patch: Partial<Units>) => {
    const u = { ...store.get().units, ...patch };
    store.set({ units: u });
    saveUnits(u);
  };

  return (
    <div className="settings">
      <Segmented label="Temperature" value={units.temp} options={{ F: '°F', C: '°C' }} onChange={(temp) => setUnits({ temp })} />
      <Segmented label="Wind" value={units.speed} options={SPEED_LABEL} onChange={(speed) => setUnits({ speed })} />
      <Segmented label="Rain and snow" value={units.precip} options={{ in: 'in', mm: 'mm' }} onChange={(precip) => setUnits({ precip })} />
      <Segmented
        label="Colours"
        value={stepped ? 'stepped' : 'smooth'}
        options={{ smooth: 'Smooth', stepped: 'Banded' }}
        onChange={(v) => setPrefs(store, { stepped: v === 'stepped' })}
      />

      <div className="settings-foot">
        <p>
          <span className={`status-dot ${live === 'open' ? 'ok' : 'warn'}`} aria-hidden="true" />
          {live === 'open' ? 'Live updates connected' : 'Reconnecting to live updates…'}
        </p>
        {basemap === 'fallback' && <p className="muted">The detailed basemap is unavailable, so simplified outlines are shown.</p>}
        <p>
          <a href="/status.html">Data feed status</a>
        </p>
        <details className="shortcuts">
          <summary>Keyboard shortcuts</summary>
          <dl>
            <dt>Space</dt>
            <dd>Play or pause</dd>
            <dt>[ and ]</dt>
            <dd>Previous or next frame</dd>
            <dt>N</dt>
            <dd>Jump to now</dd>
            <dt>L</dt>
            <dd>Layers</dd>
            <dt>Esc</dt>
            <dd>Close the panel</dd>
            <dt>Arrows, + and −</dt>
            <dd>Pan and zoom the map (when it has focus)</dd>
          </dl>
        </details>
        {Object.keys(sources).length > 0 && (
          <p className="muted small">
            Weather data from{' '}
            {Object.values(sources)
              .sort((a, b) => b.rank - a.rank)
              .map((d) => `${d.title} (${d.provider})`)
              .join(', ')}
            ; the map credits each one while it is in view. Alerts come from the US National Weather Service and the national services in the
            WMO register; point forecasts from the National Weather Service in the US and from MET Norway elsewhere.
          </p>
        )}
        {decoder && (
          <p className="muted small">Tiles are decoded {decoder === 'wasm' ? 'by the Rust decoder, compiled to WebAssembly' : 'in TypeScript'}.</p>
        )}
      </div>
    </div>
  );
}
