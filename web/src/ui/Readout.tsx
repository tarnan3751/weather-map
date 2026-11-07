import { useAppState } from './context.ts';

/** The value under the cursor (desktop) or under the crosshair (touch). */
export function Readout() {
  const r = useAppState((s) => s.readout);
  const probe = useAppState((s) => s.probe);
  if (!r) return probe ? <div className="probe-cross" aria-hidden="true" /> : null;
  const style = r.centre
    ? { left: r.x, top: r.y + 26, transform: 'translateX(-50%)' }
    : { left: Math.min(r.x + 16, window.innerWidth - 220), top: Math.min(r.y + 18, window.innerHeight - 90) };
  return (
    <>
      {probe && <div className="probe-cross" aria-hidden="true" />}
      <div className="readout" style={style} role="status" aria-live="off">
        {r.swatch && <span className="swatch" style={{ background: r.swatch }} aria-hidden="true" />}
        <span className="readout-text">{r.text}</span>
        {r.detail && <span className="readout-detail">{r.detail}</span>}
        {r.source && <span className="readout-source">{r.source}</span>}
      </div>
    </>
  );
}
