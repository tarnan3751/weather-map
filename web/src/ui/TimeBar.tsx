import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react';
import { frameBadge, frameTime, relative } from '../app/describe.ts';
import { currentFrames, currentLayer, selectedIndex, serverNow } from '../app/state.ts';
import { buildScale, buildTicks, HOUR, latestObservedIndex, nearestIndex, nowIndex, type TFrame } from '../core/timeline.ts';
import { useApp, useAppState } from './context.ts';
import { IconBack, IconForward, IconPause, IconPlay } from './icons.tsx';

/** Width of an element that may mount later than the component (a callback
 *  ref re-attaches the observer whenever the element changes). */
function useWidth<T extends HTMLElement>(): [{ current: T | null }, (el: T | null) => void, number] {
  const ref = useRef<T | null>(null);
  const [w, setW] = useState(0);
  const ro = useRef<ResizeObserver | null>(null);
  const attach = useCallback((el: T | null) => {
    ro.current?.disconnect();
    ref.current = el;
    if (!el) return;
    ro.current = new ResizeObserver(() => setW(el.clientWidth));
    ro.current.observe(el);
    setW(el.clientWidth);
  }, []);
  return [ref, attach, w];
}

/** Re-render periodically so "now" and relative times stay true. */
function useTicker(ms: number) {
  const [, set] = useState(0);
  useEffect(() => {
    const id = setInterval(() => set((n) => n + 1), ms);
    return () => clearInterval(id);
  }, [ms]);
}

export function TimeBar() {
  const { store, playback } = useApp();
  const layer = useAppState(currentLayer);
  const frames = useAppState(currentFrames);
  const idx = useAppState(selectedIndex);
  const playing = useAppState((s) => s.playing);
  const playhead = useAppState((s) => s.playhead);
  const following = useAppState((s) => s.following);
  const speed = useAppState((s) => s.speed);
  const clockOffset = useAppState((s) => s.clockOffset);
  const sources = useAppState((s) => s.sources);
  const viewSources = useAppState((s) => s.viewSources);
  const centreSource = useAppState((s) => s.centreSource);
  const loaded = useAppState((s) => !!layer && !!s.timelines[layer.id]);
  useTicker(20_000);
  const now = Date.now() + clockOffset;
  const [trackRef, attachTrack, width] = useWidth<HTMLDivElement>();
  const [dragging, setDragging] = useState(false);
  const barObserver = useRef<ResizeObserver | null>(null);
  // Map controls and side panels keep clear of the bar, whatever its height.
  const barRef = useCallback((el: HTMLElement | null) => {
    barObserver.current?.disconnect();
    const app = el?.closest('.app') as HTMLElement | null;
    if (!el || !app) return;
    barObserver.current = new ResizeObserver(() => app.style.setProperty('--timebar-h', `${el.offsetHeight}px`));
    barObserver.current.observe(el);
  }, []);

  const scale = useMemo(() => buildScale(frames), [frames]);
  const ticks = useMemo(() => buildTicks(scale, width, { minGapPx: width < 500 ? 52 : 70 }), [scale, width]);
  const nowIdx = frames.length ? nowIndex(frames, now) : -1;

  const select = useCallback(
    (i: number) => {
      const s = store.get();
      const fr = currentFrames(s);
      if (!fr.length) return;
      const j = Math.max(0, Math.min(fr.length - 1, i));
      if (s.playing) playback.pause();
      store.set({ targetTime: fr[j].t, following: j === nowIndex(fr, serverNow(s)) });
    },
    [store, playback],
  );

  const goNow = useCallback(() => {
    if (store.get().playing) playback.pause();
    store.set({ following: true, targetTime: serverNow(store.get()) });
  }, [store, playback]);

  const indexAtClientX = (clientX: number) => {
    const el = trackRef.current;
    if (!el) return idx;
    const r = el.getBoundingClientRect();
    return scale.indexAt((clientX - r.left) / Math.max(1, r.width));
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    setDragging(true);
    select(indexAtClientX(e.clientX));
    (e.currentTarget.querySelector('[role="slider"]') as HTMLElement | null)?.focus({ preventScroll: true });
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    if (!dragging) return;
    const i = indexAtClientX(e.clientX);
    if (i !== selectedIndex(store.get())) select(i);
  };
  const onPointerUp = () => setDragging(false);

  const stepBy = (d: number) => select(idx + d);
  const jumpTime = (dt: number) => {
    if (idx < 0) return;
    let j = nearestIndex(frames, frames[idx].t + dt);
    if (j === idx) j = idx + Math.sign(dt);
    select(j);
  };

  const onKey = (e: KeyboardEvent<HTMLElement>) => {
    const k = e.key;
    let handled = true;
    if (k === 'ArrowRight' || k === 'ArrowUp') e.shiftKey ? jumpTime(HOUR) : stepBy(1);
    else if (k === 'ArrowLeft' || k === 'ArrowDown') e.shiftKey ? jumpTime(-HOUR) : stepBy(-1);
    else if (k === 'PageUp') jumpTime(6 * HOUR);
    else if (k === 'PageDown') jumpTime(-6 * HOUR);
    else if (k === 'Home') select(0);
    else if (k === 'End') select(frames.length - 1);
    else if (k === 'n' || k === 'N') goNow();
    else if (k === ' ') playback.toggle();
    else handled = false;
    if (handled) e.preventDefault();
  };

  if (!layer) return null;

  const cur: TFrame | undefined = frames[idx];
  const thumbX = (() => {
    if (playing && playhead !== null && frames.length > 1) {
      const i = Math.floor(playhead);
      const f = playhead - i;
      const a = scale.pos[Math.min(i, frames.length - 1)] ?? 0;
      const b = scale.pos[Math.min(i + 1, frames.length - 1)] ?? a;
      return a + (b - a) * f;
    }
    return scale.pos[idx] ?? 0;
  })();
  const lastObs = latestObservedIndex(frames);
  const obsEndX = lastObs >= 0 ? scale.pos[lastObs] : 0;
  const fcStartX = lastObs >= 0 ? (scale.pos[lastObs + 1] ?? 1) : 0;
  const nowInRange = frames.length > 1 && now >= frames[0].t - HOUR && now <= frames[frames.length - 1].t;
  const nowX = scale.x(now);
  const density = frames.length > 1 ? width / frames.length : 0;
  const marks = density >= 2.5 ? scale.pos.map((p) => `M${(p * width).toFixed(1)} 9V13`).join('') : '';
  const valueText = cur ? `${frameTime(cur.t, now)}, ${cur.kind === 'observed' ? 'observed' : 'forecast'}, ${relative(cur.t, now)}` : 'no frames';
  const isForecast = cur?.kind === 'forecast';
  // The badge says what the value at the centre is; the thumb, the frame's kind.
  const badge = cur ? frameBadge(cur, sources, viewSources, centreSource) : null;

  return (
    <section className="timebar" aria-label="Time" ref={barRef}>
      <div className="timebar-head">
        <div className="timebar-buttons">
          <button className="btn icon primary" onClick={() => playback.toggle()} aria-label={playing ? 'Pause' : 'Play'} aria-keyshortcuts="Space" disabled={frames.length < 2}>
            {playing ? <IconPause /> : <IconPlay />}
          </button>
          <button className="btn icon ghost hide-xs" onClick={() => stepBy(-1)} aria-label="Previous frame" aria-keyshortcuts="[" disabled={idx <= 0}>
            <IconBack />
          </button>
          <button className="btn icon ghost hide-xs" onClick={() => stepBy(1)} aria-label="Next frame" aria-keyshortcuts="]" disabled={idx >= frames.length - 1}>
            <IconForward />
          </button>
        </div>
        <div className="timebar-when" aria-live="polite" aria-atomic="true">
          {cur ? (
            <>
              <span className="when-time">{frameTime(cur.t, now)}</span>
              <span className="when-rel">{relative(cur.t, now)}</span>
              <span className={`kind-badge ${badge?.kind === 'forecast' ? 'forecast' : 'observed'}`}>
                <span className="kind-dot" aria-hidden="true" />
                {badge?.text}
              </span>
            </>
          ) : (
            <span className="when-rel">{loaded ? 'No frames available for this layer yet' : 'Loading the timeline…'}</span>
          )}
        </div>
        <div className="timebar-right">
          <button
            className="btn small ghost speed hide-xs"
            onClick={() => store.set({ speed: speed >= 2 ? 0.5 : speed * 2 })}
            aria-label={`Playback speed ${speed} times`}
            title="Playback speed"
          >
            {speed}×
          </button>
          <button className={`btn small now-btn ${following && !playing ? 'on' : ''}`} onClick={goNow} aria-pressed={following && !playing} aria-keyshortcuts="N">
            <span className="live-dot" aria-hidden="true" />
            Now
          </button>
        </div>
      </div>

      <div
        className={`track ${dragging ? 'dragging' : ''}`}
        ref={attachTrack}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
      >
        <div className="track-rail" aria-hidden="true">
          {lastObs >= 0 && <div className="seg observed" style={{ left: 0, width: `${obsEndX * 100}%` }} />}
          {fcStartX < 1 && <div className="seg forecast" style={{ left: `${fcStartX * 100}%`, right: 0 }} />}
          {lastObs >= 0 && fcStartX < 1 && <div className="seg join" style={{ left: `${obsEndX * 100}%`, width: `${(fcStartX - obsEndX) * 100}%` }} />}
        </div>
        {marks && (
          <svg className="track-marks" width={width} height="22" aria-hidden="true">
            <path d={marks} />
          </svg>
        )}
        {playing && (
          <div
            className="track-loop"
            aria-hidden="true"
            style={{ left: `${(scale.pos[playback.loop[0]] ?? 0) * 100}%`, width: `${((scale.pos[playback.loop[1]] ?? 1) - (scale.pos[playback.loop[0]] ?? 0)) * 100}%` }}
          />
        )}
        {nowInRange && <div className="now-mark" style={{ left: `${nowX * 100}%` }} aria-hidden="true" />}
        <div
          className={`thumb ${isForecast ? 'forecast' : 'observed'}`}
          style={{ left: `${thumbX * 100}%` }}
          role="slider"
          tabIndex={0}
          aria-label="Time"
          aria-valuemin={0}
          aria-valuemax={Math.max(0, frames.length - 1)}
          aria-valuenow={Math.max(0, idx)}
          aria-valuetext={valueText}
          aria-keyshortcuts="ArrowLeft ArrowRight Shift+ArrowLeft Shift+ArrowRight PageUp PageDown Home End N"
          onKeyDown={onKey}
        >
          {dragging && cur && <span className="thumb-bubble">{frameTime(cur.t, now)}</span>}
        </div>
      </div>
      <div className="ticks" aria-hidden="true">
        {nowInRange && (
          <span className="tick now" style={{ left: `${nowX * 100}%` }}>
            Now
          </span>
        )}
        {ticks
          .filter((t) => !nowInRange || Math.abs(t.x - nowX) * width > (width < 500 ? 40 : 46))
          .map((t) => (
            <span key={t.t} className={`tick ${t.day ? 'day' : ''}`} style={{ left: `${t.x * 100}%` }}>
              {t.label}
            </span>
          ))}
      </div>
      {nowIdx >= 0 && <span className="sr-only">Latest data is frame {nowIdx + 1} of {frames.length}.</span>}
    </section>
  );
}
