import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { alertEnd } from '../app/alertText.ts';
import { places as placesApi } from '../api/device.ts';
import { ApiError, api } from '../api/http.ts';
import type { AlertProps, PointForecast } from '../api/types.ts';
import {
  conditionGlyph,
  conditionWord,
  coords,
  currentSteps,
  dayLabel,
  dayPrecip,
  displayZone,
  forecastDays,
  forecastError,
  formatClock,
  formatTemp,
  issuedText,
  nowDetails,
  pointToday,
  precipMode,
  rangeLabel,
  splitPeriods,
  stepsInRange,
  type Glyph,
  type Range,
} from '../app/forecast.ts';
import { alertColor, alertPriority } from '../core/alertColors.ts';
import { useApp, useAppState } from './context.ts';
import { ForecastChart } from './ForecastChart.tsx';
import { Sheet } from './Sheet.tsx';
import { IconStar } from './icons.tsx';
import '../styles/point.css';

type Load = { state: 'loading' } | { state: 'ok'; f: PointForecast; at: number } | { state: 'error'; code: string; status: number };

const CLOUD = 'M7 19a4 4 0 0 1-.6-7.96 6 6 0 0 1 11.48-1.5A4.5 4.5 0 0 1 17.5 19Z';
const CLOUD_HIGH = 'M7 15a3.5 3.5 0 0 1-.5-6.96 5.5 5.5 0 0 1 10.52-1.32A4 4 0 0 1 17 15Z';
const CLOUD_LOW = 'M9 20a3.5 3.5 0 0 1-.4-6.98 5 5 0 0 1 9.6-1.2A3.8 3.8 0 0 1 18 20Z';
const MOON = 'M20 13.5A8 8 0 1 1 10.5 4a6.2 6.2 0 0 0 9.5 9.5Z';
const dot = (cx: number, cy: number) => <circle cx={cx} cy={cy} r={1} fill="currentColor" stroke="none" />;

const GLYPHS: Record<Glyph, ReactNode> = {
  sun: (
    <>
      <circle cx={12} cy={12} r={4} />
      <path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4" />
    </>
  ),
  moon: <path d={MOON} />,
  partsun: (
    <>
      <path d="M5.4 9.5A3 3 0 1 1 10.9 7.2M8 2v1.6M2 8h1.6M3.8 3.8l1.1 1.1M12.2 3.8l-1.1 1.1" />
      <path d={CLOUD_LOW} />
    </>
  ),
  partmoon: (
    <>
      <path d={MOON} transform="translate(0.5 0.5) scale(0.55)" vectorEffect="non-scaling-stroke" />
      <path d={CLOUD_LOW} />
    </>
  ),
  cloud: <path d={CLOUD} />,
  fog: <path d="M4 8h16M3 12h18M5 16h14M8 20h8" />,
  rain: (
    <>
      <path d={CLOUD_HIGH} />
      <path d="M8.5 17.5l-1 2.5M12.5 17.5l-1 2.5M16.5 17.5l-1 2.5" />
    </>
  ),
  sleet: (
    <>
      <path d={CLOUD_HIGH} />
      <path d="M8.5 17.5l-1 2.5M15.5 17.5l-1 2.5" />
      {dot(11.5, 19.5)}
    </>
  ),
  snow: (
    <>
      <path d={CLOUD_HIGH} />
      {dot(8, 18.5)}
      {dot(12, 20.5)}
      {dot(16, 18.5)}
    </>
  ),
  thunder: (
    <>
      <path d={CLOUD_HIGH} />
      <path d="M12.5 15l-2 3.5h3l-2 3.5" />
    </>
  ),
};

/** A small line drawing for a symbol code; nothing for a code it does not know. */
function CondGlyph({ code, size = 18 }: { code: string | null | undefined; size?: number }) {
  const g = conditionGlyph(code);
  if (!g) return null;
  return (
    <svg className="cond-glyph" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.7} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
      {GLYPHS[g]}
    </svg>
  );
}

export function PointPanel({ lat, lon }: { lat: number; lon: number }) {
  const { store } = useApp();
  const units = useAppState((s) => s.units);
  const places = useAppState((s) => s.places);
  const [load, setLoad] = useState<Load>({ state: 'loading' });
  const [alerts, setAlerts] = useState<AlertProps[]>([]);
  const [range, setRange] = useState<Range>(48);
  const [saving, setSaving] = useState<{ name: string; busy: boolean; error?: string } | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [openDay, setOpenDay] = useState<string | null>(null);

  useEffect(() => {
    const ac = new AbortController();
    setLoad({ state: 'loading' });
    setSaving(null);
    setOpenDay(null);
    api
      .point(lat, lon, ac.signal)
      .then((f) => setLoad({ state: 'ok', f, at: Date.now() }))
      .catch((e) => {
        if (ac.signal.aborted) return;
        const err = e instanceof ApiError ? e : new ApiError(0, 'network', String(e));
        setLoad({ state: 'error', code: err.code, status: err.status });
      });
    api
      .alertsAt(lat, lon)
      .then((r) => !ac.signal.aborted && setAlerts([...r.alerts].sort((a, b) => alertPriority(b.event, b.severity) - alertPriority(a.event, a.severity))))
      .catch(() => setAlerts([]));
    return () => ac.abort();
  }, [lat, lon, attempt]);

  const f = load.state === 'ok' ? load.f : null;
  const at = load.state === 'ok' ? load.at : 0;
  const zone = useMemo(() => displayZone(f?.time_zone), [f]);
  const steps = useMemo(() => (f ? currentSteps(f.hourly, at) : []), [f, at]);
  const shown = useMemo(() => stepsInRange(steps, range), [steps, range]);
  const mode = useMemo(() => precipMode(steps), [steps]);
  const days = forecastDays(steps);
  const today = f ? pointToday(at, f.time_zone, Number.isFinite(f.lon) ? f.lon : lon) : '';
  const daily = f?.daily ?? [];

  const here = coords(lat, lon);
  const title = f?.place ?? (load.state === 'loading' ? 'Loading forecast…' : here);
  const saved = places.find((p) => Math.abs(p.lat - lat) < 0.002 && Math.abs(p.lon - lon) < 0.002);
  const issued = f ? issuedText(f.generated_at, zone.timeZone, Date.now()) : null;
  const error = load.state === 'error' ? forecastError(load.code, load.status) : null;

  const save = async () => {
    if (!saving?.name.trim()) return;
    setSaving({ ...saving, busy: true });
    try {
      const p = await placesApi.add({ name: saving.name.trim(), lat, lon, notify: true });
      store.set((s) => ({ places: [...s.places, p] }));
      setSaving(null);
    } catch (e) {
      const msg = e instanceof ApiError && e.status === 409 ? 'You already have 20 saved places. Remove one first.' : 'Could not save this place.';
      setSaving({ ...saving, busy: false, error: msg });
    }
  };

  const now = steps[0];
  const first = daily[0];
  const todayRange =
    first && first.date === today && (first.high_c !== null || first.low_c !== null)
      ? [first.high_c !== null ? `High ${formatTemp(first.high_c, units)}` : null, first.low_c !== null ? `Low ${formatTemp(first.low_c, units)}` : null].filter(Boolean).join(' · ')
      : null;
  const nowWord = now ? conditionWord(now.condition, now.text) : null;

  return (
    <Sheet title={title} subtitle={title !== here ? <span>{here}</span> : undefined} labelId="point-title">
      {alerts.length > 0 && (
        <ul className="point-alerts" role="list">
          {alerts.map((a) => {
            const ends = alertEnd(a);
            return (
              <li key={a.id}>
                <button className="alert-row" style={{ ['--alert-color' as string]: alertColor(a.event, a.severity, a.source) }} onClick={() => store.set({ panel: { kind: 'alert', id: a.id } })}>
                  <span className="alert-chip" aria-hidden="true" />
                  <span>
                    <strong>{a.event}</strong>
                    {ends && (
                      <small>
                        {' '}
                        until {ends.about ? 'about ' : ''}
                        {formatClock(ends.at, zone.timeZone, Date.now())}
                      </small>
                    )}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      )}

      <div className="point-actions">
        {saved ? (
          <button className="btn small" onClick={() => store.set({ panel: { kind: 'places' } })}>
            <IconStar size={16} /> Saved as “{saved.name}”
          </button>
        ) : saving ? (
          <form
            className="save-form"
            onSubmit={(e) => {
              e.preventDefault();
              void save();
            }}
          >
            <label className="sr-only" htmlFor="place-name">
              Name for this place
            </label>
            <input id="place-name" data-autofocus autoFocus value={saving.name} maxLength={80} onChange={(e) => setSaving({ ...saving, name: e.target.value, error: undefined })} placeholder="Home, campus…" />
            <button className="btn small primary" disabled={saving.busy || !saving.name.trim()}>
              Save
            </button>
            <button type="button" className="btn small ghost" onClick={() => setSaving(null)}>
              Cancel
            </button>
            {saving.error && <p className="form-error">{saving.error}</p>}
          </form>
        ) : (
          <button className="btn small" onClick={() => setSaving({ name: f?.place?.replace(/^\d+ mi from /, '').split(',')[0] ?? '', busy: false })}>
            <IconStar size={16} /> Save place
          </button>
        )}
      </div>

      {load.state === 'loading' && <div className="skeleton" aria-busy="true" aria-label="Loading forecast" />}
      {error && (
        <div className="notice">
          <p>{error.text}</p>
          {error.retry && (
            <button className="btn small" onClick={() => setAttempt((k) => k + 1)}>
              Try again
            </button>
          )}
        </div>
      )}
      {f && (
        <>
          {now && (
            <div className="now-row">
              <span className="now-temp">{formatTemp(now.temp_c, units)}</span>
              <span className="now-text">
                {nowWord && (
                  <strong className="now-cond">
                    <CondGlyph code={now.condition} size={20} />
                    {nowWord}
                  </strong>
                )}
                <span className="muted">{nowDetails(now, units).join(' · ')}</span>
                {todayRange && <span className="muted">{todayRange}</span>}
              </span>
            </div>
          )}
          <p className="point-meta">
            {f.attribution && <span className="point-attribution">{f.attribution}</span>}
            {issued && <span className="point-issued">{issued}</span>}
          </p>

          <div className="chart-head">
            <h3>{days <= 2.2 ? `Next ${Math.max(1, Math.round(days * 24))} hours` : range === 48 ? 'Next 48 hours' : `Next ${rangeLabel(days)}`}</h3>
            {days > 2.2 && (
              <div className="seg-control tiny" role="group" aria-label="Forecast range">
                <button aria-pressed={range === 48} onClick={() => setRange(48)}>
                  48 h
                </button>
                <button aria-pressed={range === 'all'} onClick={() => setRange('all')}>
                  {rangeLabel(days)}
                </button>
              </div>
            )}
          </div>
          <ForecastChart steps={shown} mode={mode} units={units} timeZone={zone.timeZone} lat={lat} lon={lon} />
          {zone.note && <p className="muted small">{zone.note}</p>}

          {daily.length > 0 && (
            <>
              <h3 className="section-title">Daily</h3>
              <ul className="days" role="list">
                {daily.map((d, i) => {
                  const key = `${d.date}-${i}`;
                  const day = dayLabel(d.date, today);
                  const periods = splitPeriods(d.text);
                  const open = openDay === key;
                  const word = conditionWord(d.condition, null);
                  const precip = dayPrecip(d.pop, d.precip_mm, units);
                  const row = (
                    <>
                      <span className="day-name">
                        <strong>{day.name}</strong>
                        {day.sub && <small>{day.sub}</small>}
                      </span>
                      <span className="day-cond">
                        <CondGlyph code={d.condition} />
                        {word && <span>{word}</span>}
                      </span>
                      <span className="day-precip">{precip ?? ''}</span>
                      <span className="day-temps">
                        <span className="hi">
                          <span className="sr-only">High </span>
                          {formatTemp(d.high_c, units)}
                        </span>
                        <span className="lo">
                          <span className="sr-only">, low </span>
                          {formatTemp(d.low_c, units)}
                        </span>
                      </span>
                    </>
                  );
                  return (
                    <li key={key}>
                      {periods.length > 0 ? (
                        <button className="day-row" aria-expanded={open} onClick={() => setOpenDay(open ? null : key)}>
                          {row}
                        </button>
                      ) : (
                        <div className="day-row">{row}</div>
                      )}
                      {open && periods.length > 0 && (
                        <div className="day-text">
                          <p className="day-text-note">In the forecaster's words and units:</p>
                          {periods.map((p, k) => (
                            <p key={k}>
                              {p.name && <strong>{p.name}: </strong>}
                              {p.text}
                            </p>
                          ))}
                        </div>
                      )}
                    </li>
                  );
                })}
              </ul>
            </>
          )}
        </>
      )}
    </Sheet>
  );
}
