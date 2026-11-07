import { useEffect, useRef, useState, type ReactNode } from 'react';
import { useApp } from './context.ts';
import { IconClose } from './icons.tsx';

/**
 * A panel: a card at the side on wide screens, a bottom sheet on phones. Takes
 * focus when it opens and gives it back when it closes; Escape closes it; on a
 * phone, dragging the handle down dismisses it. Closing it closes the app's
 * open panel unless `onClose` says otherwise.
 */
export function Sheet({
  title,
  subtitle,
  onClose: close,
  children,
  side = 'right',
  accent,
  labelId,
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  onClose?: () => void;
  children: ReactNode;
  side?: 'left' | 'right';
  accent?: string;
  labelId: string;
}) {
  const { store } = useApp();
  const onClose = close ?? (() => store.set({ panel: null }));
  const ref = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<{ start: number; dy: number } | null>(null);

  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    ref.current?.querySelector<HTMLElement>('[data-autofocus], h2')?.focus({ preventScroll: true });
    return () => {
      if (prev && document.contains(prev)) prev.focus({ preventScroll: true });
    };
  }, [labelId]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div
      ref={ref}
      className={`sheet ${side}`}
      role="dialog"
      aria-modal="false"
      aria-labelledby={labelId}
      style={{ ...(drag ? { transform: `translateY(${Math.max(0, drag.dy)}px)`, transition: 'none' } : {}), ...(accent ? { ['--accent-strip' as string]: accent } : {}) }}
    >
      <div
        className="sheet-handle"
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          setDrag({ start: e.clientY, dy: 0 });
        }}
        onPointerMove={(e) => drag && setDrag({ ...drag, dy: e.clientY - drag.start })}
        onPointerUp={() => {
          if (drag && drag.dy > 90) onClose();
          setDrag(null);
        }}
        onPointerCancel={() => setDrag(null)}
        aria-hidden="true"
      >
        <span />
      </div>
      {accent && <div className="sheet-accent" aria-hidden="true" />}
      <header className="sheet-head">
        <div className="sheet-titles">
          <h2 id={labelId} tabIndex={-1}>
            {title}
          </h2>
          {subtitle && <div className="sheet-sub">{subtitle}</div>}
        </div>
        <button className="btn icon ghost" onClick={onClose} aria-label="Close">
          <IconClose />
        </button>
      </header>
      <div className="sheet-body">{children}</div>
    </div>
  );
}
