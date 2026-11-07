import { useEffect } from 'react';
import { alertColor } from '../core/alertColors.ts';
import { useApp, useAppState } from './context.ts';
import { IconClose, IconWarning } from './icons.tsx';

export function Toasts() {
  const { store, map } = useApp();
  const toasts = useAppState((s) => s.toasts);

  useEffect(() => {
    if (!toasts.length) return;
    const t = setTimeout(() => store.set((s) => ({ toasts: s.toasts.slice(1) })), 14_000);
    return () => clearTimeout(t);
  }, [toasts, store]);

  const dismiss = (id: number) => store.set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));

  return (
    <div className="toasts" aria-live="assertive" aria-relevant="additions">
      {toasts.map((t) => {
        const color = t.title.includes('Warning') ? alertColor(t.title.split(' · ')[0], t.severity) : '#6cc4ff';
        return (
          <div key={t.id} className="toast" role="alert" style={{ ['--toast-color' as string]: color }}>
            <span className="toast-icon" aria-hidden="true">
              <IconWarning size={18} />
            </span>
            <div className="toast-text">
              <strong>{t.title}</strong>
              {t.body && <span>{t.body}</span>}
            </div>
            {t.action && (
              <button
                className="btn small"
                onClick={() => {
                  const a = t.action!;
                  if (a.alertId) {
                    map()?.fitAlert(a.alertId);
                    store.set({ panel: { kind: 'alert', id: a.alertId } });
                  } else if (a.lat !== undefined && a.lon !== undefined) map()?.flyTo(a.lon, a.lat);
                  dismiss(t.id);
                }}
              >
                {t.action.label}
              </button>
            )}
            <button className="btn icon ghost small" aria-label="Dismiss" onClick={() => dismiss(t.id)}>
              <IconClose size={16} />
            </button>
          </div>
        );
      })}
    </div>
  );
}
