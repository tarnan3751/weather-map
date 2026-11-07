import { useEffect, useState } from 'react';
import { places as placesApi } from '../api/device.ts';
import type { Place } from '../api/types.ts';
import { alertColor } from '../core/alertColors.ts';
import { useApp, useAppState } from './context.ts';
import { IconBell, IconEdit, IconPin, IconTrash } from './icons.tsx';
import { Sheet } from './Sheet.tsx';

function PlaceRow({ p }: { p: Place }) {
  const { store, map } = useApp();
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(p.name);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const replace = (next: Place) => store.set((s) => ({ places: s.places.map((x) => (x.id === next.id ? next : x)) }));

  const rename = async () => {
    const n = name.trim();
    if (!n || n === p.name) {
      setEditing(false);
      setName(p.name);
      return;
    }
    setBusy(true);
    try {
      replace(await placesApi.update(p.id, { name: n }));
      setEditing(false);
    } catch {
      setError('Could not rename.');
    } finally {
      setBusy(false);
    }
  };

  const toggleNotify = async () => {
    setBusy(true);
    replace({ ...p, notify: !p.notify });
    try {
      replace(await placesApi.update(p.id, { notify: !p.notify }));
    } catch {
      replace(p);
      setError('Could not change notifications.');
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    try {
      await placesApi.remove(p.id);
      store.set((s) => ({ places: s.places.filter((x) => x.id !== p.id) }));
    } catch {
      setError('Could not delete.');
      setBusy(false);
    }
  };

  return (
    <li className="place">
      <div className="place-main">
        {editing ? (
          <form
            className="save-form"
            onSubmit={(e) => {
              e.preventDefault();
              void rename();
            }}
          >
            <label className="sr-only" htmlFor={`rename-${p.id}`}>
              New name
            </label>
            <input id={`rename-${p.id}`} autoFocus value={name} maxLength={80} onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === 'Escape' && (e.stopPropagation(), setEditing(false))} />
            <button className="btn small primary" disabled={busy}>
              Save
            </button>
          </form>
        ) : (
          <button className="place-name" onClick={() => map()?.flyTo(p.lon, p.lat, 8)} title="Show on map">
            <IconPin size={16} />
            <span>{p.name}</span>
          </button>
        )}
        <div className="place-tools">
          <label className="switch compact" title={p.notify ? 'Notifications on' : 'Notifications off'}>
            <span className="sr-only">Notify me about warnings at {p.name}</span>
            <IconBell size={15} aria-hidden="true" />
            <input type="checkbox" role="switch" checked={p.notify} disabled={busy} onChange={() => void toggleNotify()} />
          </label>
          {!editing && (
            <button className="btn icon ghost small" aria-label={`Rename ${p.name}`} onClick={() => setEditing(true)}>
              <IconEdit size={16} />
            </button>
          )}
          {confirm ? (
            <>
              <button className="btn small danger" onClick={() => void remove()} disabled={busy}>
                Delete
              </button>
              <button className="btn small ghost" onClick={() => setConfirm(false)}>
                Keep
              </button>
            </>
          ) : (
            <button className="btn icon ghost small" aria-label={`Delete ${p.name}`} onClick={() => setConfirm(true)}>
              <IconTrash size={16} />
            </button>
          )}
        </div>
      </div>
      {p.alerts.length > 0 ? (
        <ul className="place-alerts" role="list" aria-label={`Alerts covering ${p.name}`}>
          {p.alerts.map((a) => (
            <li key={a.id}>
              <button className="alert-pill" style={{ ['--alert-color' as string]: alertColor(a.event, a.severity) }} onClick={() => store.set({ panel: { kind: 'alert', id: a.id } })}>
                {a.event}
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p className="muted small place-clear">No alerts here right now</p>
      )}
      {error && <p className="form-error">{error}</p>}
    </li>
  );
}

export function PlacesPanel() {
  const { store, data } = useApp();
  const places = useAppState((s) => s.places);
  useEffect(() => {
    void data.loadPlaces();
  }, [data]);
  const notifying = places.filter((p) => p.notify).length;

  return (
    <Sheet title="Saved places" subtitle={places.length ? `${places.length} of 20` : undefined} labelId="places-title">
      {places.length === 0 ? (
        <div className="empty">
          <p>Tap anywhere on the map, then choose Save place. Saved places show the alerts that cover them, and can notify you when a warning is issued there.</p>
        </div>
      ) : (
        <ul className="places" role="list">
          {places.map((p) => (
            <PlaceRow key={p.id} p={p} />
          ))}
        </ul>
      )}
      <div className="notice subtle">
        <p>
          {notifying
            ? `Warnings for ${notifying} ${notifying === 1 ? 'place' : 'places'} can be sent to this device.`
            : 'Turn on the bell for a place to be told about warnings there.'}
        </p>
        <button className="btn small" onClick={() => store.set({ panel: { kind: 'notify' } })}>
          Notification settings
        </button>
      </div>
    </Sheet>
  );
}
