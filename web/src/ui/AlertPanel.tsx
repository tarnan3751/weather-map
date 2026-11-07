import { useEffect, useState } from 'react';
import { ApiError, api } from '../api/http.ts';
import type { AlertDetail, AlertProps } from '../api/types.ts';
import { alertEnd, alertText, browserLanguages, inEffect, issuer, languageLabels, linkHost, uiLanguage, type LangText } from '../app/alertText.ts';
import { formatDateTime, relative } from '../app/describe.ts';
import { alertColor } from '../core/alertColors.ts';
import '../styles/alert.css';
import { useApp, useAppState } from './context.ts';
import { Sheet } from './Sheet.tsx';

/** Alert text as text, in its own language and direction: an Arabic or
 *  Hebrew block lays out right to left inside the left-to-right panel. */
function Said({ t, as: Tag = 'span', className }: { t: LangText; as?: 'span' | 'p'; className?: string }) {
  return (
    <Tag lang={t.lang} dir="auto" className={className ? `alert-text ${className}` : 'alert-text'}>
      {t.text}
    </Tag>
  );
}

export function AlertPanel({ id, others = [], at }: { id: string; others?: string[]; at?: { lat: number; lon: number } }) {
  const { store, map } = useApp();
  const listed = useAppState((s) => s.alerts?.features.find((f) => f.properties.id === id) ?? null);
  const all = useAppState((s) => s.alerts);
  const also = others.map((o) => all?.features.find((f) => f.properties.id === o)?.properties).filter((x): x is AlertProps => !!x);
  const clockOffset = useAppState((s) => s.clockOffset);
  const [fetched, setFetched] = useState<AlertDetail | null>(null);
  const [error, setError] = useState<'gone' | 'failed' | null>(null);
  // The reader's own pick of language, carried to the next alert they open.
  const [picked, setPicked] = useState<string | null>(null);
  // And the block picked in this alert, which may share its language with
  // another (one block per area or time step).
  const [pickedBlock, setPickedBlock] = useState<{ id: string; index: number } | null>(null);

  useEffect(() => {
    let live = true;
    setFetched(null);
    setError(null);
    api
      .alertDetail(id)
      .then((d) => live && setFetched(d))
      .catch((e) => live && setError(e instanceof ApiError && e.status === 404 ? 'gone' : 'failed'));
    return () => {
      live = false;
    };
  }, [id]);

  // Until the effect has run, `fetched` may still be the previous alert's.
  const detail = fetched?.id === id ? fetched : null;
  const a = detail ?? listed?.properties ?? null;
  const now = Date.now() + clockOffset;
  if (!a) {
    return (
      <Sheet title="Alert" labelId="alert-title">
        {error ? (
          <p className="muted">{error === 'gone' ? 'This alert is no longer available.' : 'This alert could not be loaded.'}</p>
        ) : (
          <div className="skeleton short" aria-busy="true" aria-label="Loading alert" />
        )}
      </Sheet>
    );
  }

  const browser = browserLanguages();
  const t = alertText(a, picked ? [picked, ...browser] : browser, pickedBlock?.id === id ? pickedBlock.index : null);
  const infos = detail?.infos ?? [];
  // A block of the message that no longer counts is still readable, greyed.
  const shownPast = !!infos[t.index] && !inEffect(infos[t.index]);
  const past = shownPast ? ' past' : '';
  const labels = infos.length > 1 ? languageLabels(infos.map((b) => b.language), uiLanguage()) : [];
  const color = alertColor(a.event, a.severity, a.source);
  const ends = alertEnd(a);
  const onset = a.onset ? Date.parse(a.onset) : null;
  const active = detail ? detail.active : true;
  const unshaped = a.geom_source === 'none' || (listed !== null && !listed.geometry);
  const from = detail ? issuer(detail) : null;
  const region: LangText | null = t.area ?? (detail?.country_name ? { text: detail.country_name, lang: 'en' } : null);
  const chips = [
    a.severity === 'Unknown' ? 'Severity unknown' : a.severity,
    a.urgency && a.urgency !== 'Unknown' ? a.urgency : null,
    a.certainty && a.certainty !== 'Unknown' ? a.certainty : null,
    detail?.status && detail.status !== 'Actual' ? detail.status : null,
  ].filter((c): c is string => !!c);

  return (
    <Sheet title={<Said t={t.event} />} subtitle={t.area && <Said t={t.area} />} labelId="alert-title" accent={color}>
      {labels.length > 0 && (
        <div className="alert-langs" role="group" aria-label="Language of the alert text">
          {infos.map((b, i) => (
            <button
              key={`${b.language}-${i}`}
              type="button"
              className={`alert-lang${inEffect(b) ? '' : ' past'}`}
              aria-pressed={i === t.index}
              title={inEffect(b) ? undefined : 'No longer in effect'}
              onClick={() => {
                setPicked(b.language);
                setPickedBlock({ id, index: i });
              }}
            >
              {labels[i]}
              {!inEffect(b) && <span className="sr-only">, no longer in effect</span>}
            </button>
          ))}
        </div>
      )}
      <div className="alert-meta">
        {chips.map((c) => (
          <span key={c} className="chip">
            {c}
          </span>
        ))}
      </div>
      {!active && <p className="notice alert-ended">This alert is no longer in effect: it has ended or was replaced by a newer message.</p>}
      {active && shownPast && (
        <p className="notice alert-past-note">This part of the message is no longer in effect{infos.some(inEffect) ? '; its other parts are' : ''}.</p>
      )}
      <dl className="alert-times">
        {onset !== null && onset > now ? (
          <>
            <dt>Begins</dt>
            <dd>
              {formatDateTime(a.onset, now)} <span className="muted">({relative(onset, now)})</span>
            </dd>
          </>
        ) : (
          a.sent && (
            <>
              <dt>{detail?.message_type === 'Update' ? 'Updated' : 'Issued'}</dt>
              <dd>
                {formatDateTime(a.sent, now)}
                {t.sender && (
                  <span className="muted">
                    {' by '}
                    <Said t={t.sender} />
                  </span>
                )}
              </dd>
            </>
          )
        )}
        {ends && (
          <>
            <dt>Until</dt>
            <dd>
              {ends.about ? 'About ' : ''}
              {formatDateTime(new Date(ends.at).toISOString(), now)} <span className="muted">({relative(ends.at, now)})</span>
              {ends.about && <div className="muted small">The issuer gave no end time; alerts like this are assumed to last a day.</div>}
            </dd>
          </>
        )}
        {from && (
          <>
            <dt>Source</dt>
            <dd className="alert-text">{from}</dd>
          </>
        )}
      </dl>
      {t.headline && <Said t={t.headline} as="p" className={`alert-headline${past}`} />}
      {unshaped && (
        <p className="notice alert-unshaped">
          {region ? (
            <>
              No map shape was published for this alert; it covers the region named: <Said t={region} />.
            </>
          ) : (
            'No map shape was published for this alert, so it is not drawn on the map.'
          )}
        </p>
      )}
      {t.instruction && (
        <div className={`alert-instruction${past}`} style={{ ['--alert-color' as string]: color }}>
          <h3>What to do</h3>
          <Said t={t.instruction} as="p" />
        </div>
      )}
      {t.description ? (
        <div className={`alert-description${past}`}>
          <h3>Details</h3>
          <Said t={t.description} as="p" />
        </div>
      ) : !detail && !error ? (
        <div className="skeleton short" aria-busy="true" aria-label="Loading alert text" />
      ) : error ? (
        <p className="muted">The full text of this alert could not be loaded.</p>
      ) : null}
      {t.web && (
        <p className="alert-web">
          <a href={t.web} target="_blank" rel="noopener noreferrer">
            Full alert at {linkHost(t.web)}
          </a>
        </p>
      )}
      <div className="point-actions">
        {!unshaped && active && (
          <button className="btn small" onClick={() => map()?.fitAlert(id)}>
            Show the whole area
          </button>
        )}
        {at && (
          <button className="btn small" onClick={() => store.set({ panel: { kind: 'point', lat: at.lat, lon: at.lon } })}>
            Forecast for this spot
          </button>
        )}
      </div>
      {also.length > 0 && (
        <>
          <h3>Also in effect here</h3>
          <ul className="point-alerts" role="list">
            {also.map((o) => (
              <li key={o.id}>
                <button
                  className="alert-row"
                  style={{ ['--alert-color' as string]: alertColor(o.event, o.severity, o.source) }}
                  onClick={() => store.set({ panel: { kind: 'alert', id: o.id, others: [id, ...others.filter((x) => x !== o.id)], at } })}
                >
                  <span className="alert-chip" aria-hidden="true" />
                  <strong lang={o.language} dir="auto" className="alert-text">
                    {o.event}
                  </strong>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
      {a.geom_source === 'nuts' && (
        <p className="muted small">Area drawn from the administrative regions the alert names. Boundaries © EuroGeographics.</p>
      )}
      {a.geom_source === 'zones' && <p className="muted small">Area drawn from the forecast zones and counties the alert names.</p>}
    </Sheet>
  );
}
