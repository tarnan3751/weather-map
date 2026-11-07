// The forecast as three small multiples sharing one time axis (temperature,
// precipitation, wind) rather than one chart with several y-scales. The axis
// is time, not step number: steps further out that cover six hours are drawn
// six hours wide. A crosshair reads every panel at once; the same values are
// in the table below for anyone not using a pointer.

import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react';
import type { HourlyPoint } from '../api/types.ts';
import { HOUR } from '../core/timeline.ts';
import { formatNumber, precipUnit, speedUnit, tempUnit, type Units } from '../core/units.ts';
import {
  NONE,
  conditionWord,
  firstLongStep,
  formatPop,
  formatPrecip,
  formatTemp,
  formatWind,
  isNum,
  niceCeil,
  nightSpans,
  precipValue,
  rateTop,
  stepEnd,
  stepHours,
  stepStart,
  timeTicks,
  type PrecipMode,
} from '../app/forecast.ts';

// Validated against the panel surface (dark mode, CVD-separated, >= 3:1).
const C_TEMP = '#d95926';
const C_PRECIP = '#3987e5';
const C_WIND = '#199e70';

const PAD_L = 34;
const PAD_R = 10;
const H_TEMP = 92;
const H_PRECIP = 46;
const H_WIND = 52;
const GAP = 16;
const H_AXIS = 20;

interface Props {
  /** In time order, none already over. */
  steps: HourlyPoint[];
  /** Decided for the whole forecast, so switching range keeps the axis. */
  mode: PrecipMode;
  units: Units;
  /** For Intl; undefined is the browser's zone. */
  timeZone: string | undefined;
  lat: number;
  lon: number;
}

/** A bar with rounded top corners standing on `base`. */
function bar(x0: number, w: number, top: number, base: number): string {
  const r = Math.min(3, w / 2, (base - top) / 2);
  return `M${x0} ${base}V${top + r}Q${x0} ${top} ${x0 + r} ${top}H${x0 + w - r}Q${x0 + w} ${top} ${x0 + w} ${top + r}V${base}Z`;
}

export function ForecastChart({ steps, mode, units, timeZone, lat, lon }: Props) {
  const wrap = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(340);
  const [hover, setHover] = useState<number | null>(null);
  useEffect(() => {
    const el = wrap.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWidth(Math.max(240, el.clientWidth)));
    ro.observe(el);
    setWidth(Math.max(240, el.clientWidth));
    return () => ro.disconnect();
  }, []);
  useEffect(() => setHover(null), [steps]);

  const n = steps.length;
  const starts = useMemo(() => steps.map(stepStart), [steps]);
  const ends = useMemo(() => steps.map(stepEnd), [steps]);
  const t0 = n ? starts[0] : 0;
  const t1 = n ? ends[n - 1] : HOUR;
  const spanH = (t1 - t0) / HOUR;
  const everyH = spanH > 72 ? 0 : width < 420 ? 12 : 6;
  const fmt = useMemo(
    () => ({
      hour: new Intl.DateTimeFormat(undefined, { hour: 'numeric', timeZone }),
      day: new Intl.DateTimeFormat(undefined, { weekday: 'short', timeZone }),
      full: new Intl.DateTimeFormat(undefined, { weekday: 'short', hour: 'numeric', minute: '2-digit', timeZone }),
      clock: new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit', timeZone }),
    }),
    [timeZone],
  );
  const ticks = useMemo(() => timeTicks(t0, t1, timeZone, everyH), [t0, t1, timeZone, everyH]);
  const nights = useMemo(() => nightSpans(lat, lon, t0, t1), [lat, lon, t0, t1]);

  if (n === 0) return <p className="muted">No hourly forecast available.</p>;

  const tu = tempUnit(units.temp);
  const su = speedUnit(units.speed);
  const pu = precipUnit(units.precip);
  const plotW = width - PAD_L - PAD_R;
  const x = (t: number) => PAD_L + ((t - t0) / (t1 - t0)) * plotW;

  const temps = steps.map((h) => (isNum(h.temp_c) ? tu.to(h.temp_c) : null));
  const tv = temps.filter((v): v is number => v !== null);
  const hasTemp = tv.length > 0;
  const tMin = hasTemp ? Math.min(...tv) : 0;
  const tMax = hasTemp ? Math.max(...tv) : 10;
  const tStep = niceCeil(Math.max(4, tMax - tMin) / 3);
  const t0v = Math.floor(tMin / tStep) * tStep;
  const t1v = Math.ceil(tMax / tStep) * tStep + (Math.ceil(tMax / tStep) * tStep === tMax ? tStep : 0);
  const yT = (v: number) => 6 + (1 - (v - t0v) / (t1v - t0v)) * (H_TEMP - 12);

  const topPrecip = H_TEMP + GAP;
  const precip = steps.map((h) => precipValue(h, mode, units));
  const pMax = Math.max(0, ...precip.filter((v): v is number => v !== null));
  const pTop = mode === 'pop' ? 100 : rateTop(pMax, units);
  const yP = (v: number) => topPrecip + (1 - v / pTop) * H_PRECIP;
  const pLabel = (v: number) => (mode === 'pop' ? `${formatNumber(v, 0)}%` : formatNumber(v, Number.isInteger(v) ? 0 : pu.decimals));
  const dry = precip.every((v) => v === null || v <= 0);

  const winds = steps.map((h) => (isNum(h.wind_ms) ? su.to(Math.max(0, h.wind_ms)) : null));
  // Headroom, so the line and its arrows stay clear of the panel's name.
  const wMax = Math.max(10, 1.25 * Math.max(0, ...winds.filter((v): v is number => v !== null)));
  const wStep = niceCeil(wMax / 2);
  const w1 = Math.ceil(wMax / wStep) * wStep;
  const topWind = topPrecip + H_PRECIP + GAP;
  const yW = (v: number) => topWind + (1 - v / w1) * H_WIND;
  const plotBottom = topWind + H_WIND;
  const height = plotBottom + H_AXIS;
  // A grid line across the plot, and a value on the axis at its left.
  const grid = (y: number) => <line className="fchart-grid" x1={PAD_L} x2={width - PAD_R} y1={y} y2={y} />;
  const axisValue = (y: number, text: string) => (
    <text className="fchart-axis" x={PAD_L - 6} y={y} dy="0.32em" textAnchor="end">
      {text}
    </text>
  );

  const linePath = (vals: (number | null)[], y: (v: number) => number) => {
    let d = '';
    let pen = false;
    vals.forEach((v, i) => {
      if (v === null) {
        pen = false;
        return;
      }
      d += `${pen ? 'L' : 'M'}${x(starts[i]).toFixed(1)} ${y(v).toFixed(1)}`;
      pen = true;
    });
    return d;
  };

  const long = firstLongStep(steps);
  const iMax = hasTemp ? temps.indexOf(tMax) : -1;
  const iMin = hasTemp ? temps.indexOf(tMin) : -1;

  const label = (i: number) =>
    stepHours(steps[i]) > 1 ? `${fmt.full.format(starts[i])} to ${fmt.clock.format(ends[i])}` : fmt.full.format(starts[i]);

  const pick = (clientX: number) => {
    const r = wrap.current?.getBoundingClientRect();
    if (!r) return;
    const t = t0 + ((clientX - r.left - PAD_L) / plotW) * (t1 - t0);
    let i = 0;
    while (i < n - 1 && starts[i + 1] <= t) i++;
    setHover(i);
  };
  const onPointer = (e: PointerEvent<SVGSVGElement>) => pick(e.clientX);
  const onKey = (e: KeyboardEvent<SVGSVGElement>) => {
    if (e.key === 'ArrowRight') setHover((h) => Math.min(n - 1, (h ?? -1) + 1));
    else if (e.key === 'ArrowLeft') setHover((h) => Math.max(0, (h ?? 1) - 1));
    else if (e.key === 'Escape') setHover(null);
    else return;
    e.preventDefault();
  };

  // The read-out sits in a fixed row above the panels rather than floating
  // over them, so it never hides the line being read.
  const shown = hover === null ? 0 : Math.min(hover, n - 1);
  const hv = steps[shown];
  const now = Date.now();
  const idleLabel = starts[0] <= now && now < ends[0] ? 'Now' : label(0);
  const wind = formatWind(hv.wind_ms, hv.wind_dir_deg, units, false);
  const spanText = (h: HourlyPoint) => `in ${stepHours(h)} h`;
  const precipCell = (h: HourlyPoint) => {
    const parts = [isNum(h.pop) ? formatPop(h.pop) : null, isNum(h.precip_mm) ? formatPrecip(h.precip_mm, units) : null].filter(Boolean);
    return parts.length ? parts.join(' · ') : NONE;
  };
  const panelName = mode === 'pop' ? 'Chance of precipitation' : mode === 'amount' ? `Precipitation, ${pu.label} per hour` : 'Precipitation';
  const reach = spanH <= 72 ? `${Math.round(spanH)} hours` : `${Math.round(spanH / 24)} days`;

  return (
    <div className="fchart" ref={wrap}>
      <div className={`fchart-readout ${hover === null ? 'idle' : ''}`} role="status" aria-live="off">
        <span className="tip-time">{hover === null ? idleLabel : label(shown)}</span>
        <span className="tip-item">
          <span className="key" style={{ background: C_TEMP }} />
          <strong>{formatTemp(hv.temp_c, units, true)}</strong>
        </span>
        <span className="tip-item">
          <span className="key" style={{ background: C_PRECIP }} />
          {mode === 'pop' ? (
            <>
              <strong>{formatPop(hv.pop)}</strong>
              <span className="muted">chance</span>
              {isNum(hv.precip_mm) && hv.precip_mm > 0 && <span className="muted">· {formatPrecip(hv.precip_mm, units)}</span>}
            </>
          ) : mode === 'amount' ? (
            <>
              <strong>{formatPrecip(hv.precip_mm, units)}</strong>
              <span className="muted">{spanText(hv)}</span>
            </>
          ) : (
            <>
              <strong>{NONE}</strong>
              <span className="muted">precip</span>
            </>
          )}
        </span>
        <span className="tip-item">
          <span className="key" style={{ background: C_WIND }} />
          <strong>{wind}</strong>
          {wind !== NONE && wind !== 'Calm' && <span className="muted">{su.label}</span>}
        </span>
        <span className="tip-short">{conditionWord(hv.condition, hv.text) ?? ''}</span>
      </div>
      <svg
        width={width}
        height={height}
        role="img"
        aria-label={`Forecast for the next ${reach}: ${hasTemp ? `temperature from ${formatNumber(tMin, 0)} to ${formatNumber(tMax, 0)}${tu.label}` : 'no temperature'}. Use the arrow keys to read each step.`}
        tabIndex={0}
        onPointerMove={onPointer}
        onPointerDown={onPointer}
        onPointerLeave={(e) => e.pointerType === 'mouse' && setHover(null)}
        onKeyDown={onKey}
        onBlur={() => setHover(null)}
      >
        {nights.map(([a, b]) => (
          <rect key={a} className="fchart-night" x={x(a)} width={Math.max(0, x(b) - x(a))} y={0} height={plotBottom} />
        ))}
        {ticks.days.map((t) => (
          <line key={`d${t}`} className="fchart-day" x1={x(t)} x2={x(t)} y1={0} y2={plotBottom} />
        ))}
        {hover !== null && <rect className="fchart-hl" x={x(starts[shown])} width={Math.max(1, x(ends[shown]) - x(starts[shown]))} y={0} height={plotBottom} pointerEvents="none" />}
        {long > 0 && <line className="fchart-six" x1={x(starts[long])} x2={x(starts[long])} y1={0} y2={plotBottom} />}

        {/* Temperature */}
        {hasTemp &&
          Array.from({ length: Math.round((t1v - t0v) / tStep) + 1 }, (_, k) => t0v + k * tStep).map((v) => (
            <g key={`t${v}`}>
              {grid(yT(v))}
              {axisValue(yT(v), `${formatNumber(v, 0)}°`)}
            </g>
          ))}
        {hasTemp ? (
          <path d={linePath(temps, yT)} stroke={C_TEMP} strokeWidth={2} fill="none" strokeLinejoin="round" strokeLinecap="round" />
        ) : (
          <text className="fchart-axis" x={PAD_L + plotW / 2} y={H_TEMP / 2} textAnchor="middle">
            No temperature forecast
          </text>
        )}
        {/* Six-hour steps are samples six hours apart: mark each one. */}
        {steps.map((h, i) =>
          stepHours(h) > 1 && temps[i] !== null ? <circle key={`s${i}`} cx={x(starts[i])} cy={yT(temps[i]!)} r={2.5} fill={C_TEMP} className="fchart-dot" /> : null,
        )}
        {[iMax, iMin].map((i, k) => {
          if (i < 0 || temps[i] === null) return null;
          const cx = x(starts[i]);
          const cy = yT(temps[i]!);
          // The high goes above its point unless the panel's name is there.
          const above = k === 0 && !(cx < PAD_L + 110 && cy < 24);
          return (
            <g key={`m${k}`}>
              <circle cx={cx} cy={cy} r={4} fill={C_TEMP} className="fchart-dot" />
              <text className="fchart-label" x={cx} y={above ? cy - 9 : cy + 15} textAnchor="middle">
                {formatNumber(temps[i]!, 0)}°
              </text>
            </g>
          );
        })}

        {/* Precipitation: the chance, or the average rate over each step */}
        {grid(yP(0))}
        {mode !== 'none' && (
          <>
            {grid(yP(pTop / 2))}
            {axisValue(yP(pTop / 2), pLabel(pTop / 2))}
            {axisValue(yP(pTop), pLabel(pTop))}
          </>
        )}
        {(dry || mode === 'none') && (
          <text className="fchart-axis" x={PAD_L + plotW / 2} y={yP(pTop * 0.3)} textAnchor="middle">
            {mode === 'none' ? 'No precipitation forecast' : 'No precipitation expected'}
          </text>
        )}
        {steps.map((_, i) => {
          const v = precip[i];
          if (v === null || v <= 0) return null;
          const xa = x(starts[i]);
          const w = x(ends[i]) - xa;
          // A gap between neighbours where there is room for one.
          const gap = w >= 6 ? 2 : w >= 3 ? 1 : 0;
          if (w - gap <= 0.3) return null;
          // At least a pixel, so a trace still shows.
          const top = Math.min(yP(Math.min(v, pTop)), yP(0) - 1);
          return <path key={`p${i}`} fill={C_PRECIP} opacity={hover === i ? 1 : 0.85} d={bar(xa + gap / 2, w - gap, top, yP(0))} />;
        })}

        {/* Wind */}
        {grid(yW(0))}
        {grid(yW(w1 / 2))}
        {axisValue(yW(w1 / 2), formatNumber(w1 / 2, 0))}
        {axisValue(yW(w1), formatNumber(w1, 0))}
        <path d={linePath(winds, yW)} stroke={C_WIND} strokeWidth={2} fill="none" strokeLinejoin="round" strokeLinecap="round" />
        {(() => {
          // Spaced by distance, not by step count, since steps vary in
          // length; the first few sit clear of the panel's name.
          let last = -Infinity;
          return steps.map((h, i) => {
            const xi = x(starts[i]);
            const w = winds[i];
            if (w === null || !isNum(h.wind_dir_deg) || xi < PAD_L + 84 || xi - last < 24) return null;
            last = xi;
            // Points downwind, the way the air is moving.
            return (
              <path key={`a${i}`} className="fchart-arrow" transform={`translate(${xi} ${yW(w) - 10}) rotate(${(h.wind_dir_deg + 180) % 360})`} d="M0 4V-4M-2.6 -1.4 0 -4.4 2.6 -1.4" />
            );
          });
        })()}

        {/* Panel names, top-left of each panel */}
        <text className="fchart-panel" x={PAD_L + 4} y={10}>
          Temperature, {tu.label}
        </text>
        <text className="fchart-panel" x={PAD_L + 4} y={topPrecip + 8}>
          {panelName}
        </text>
        <text className="fchart-panel" x={PAD_L + 4} y={topWind + 8}>
          Wind, {su.label}
        </text>

        {/* Time axis */}
        {ticks.hours.map((t) => (
          <text key={`h${t}`} className="fchart-axis" x={x(t)} y={height - 6} textAnchor="middle">
            {fmt.hour.format(t)}
          </text>
        ))}
        {ticks.days.map((t) =>
          x(t) + 22 <= width ? (
            <text key={`dl${t}`} className="fchart-axis strong" x={x(t) + 3} y={height - 6} textAnchor="start">
              {fmt.day.format(t)}
            </text>
          ) : null,
        )}

        {hover !== null && (
          <g className="fchart-cross" pointerEvents="none">
            <line x1={x(starts[shown])} x2={x(starts[shown])} y1={0} y2={plotBottom} />
            {temps[shown] !== null && <circle cx={x(starts[shown])} cy={yT(temps[shown]!)} r={4.5} fill={C_TEMP} className="fchart-dot" />}
            {winds[shown] !== null && <circle cx={x(starts[shown])} cy={yW(winds[shown]!)} r={4.5} fill={C_WIND} className="fchart-dot" />}
          </g>
        )}
      </svg>
      {long > 0 && (
        <p className="fchart-note muted small">
          Hourly until {fmt.full.format(starts[long])}, then in 6-hour steps (right of the dashed line)
          {mode === 'amount' ? ', each bar at its average rate per hour.' : '.'}
        </p>
      )}
      <details className="fchart-table">
        <summary>Forecast table</summary>
        <table>
          <thead>
            <tr>
              <th scope="col">Time</th>
              <th scope="col">Temp</th>
              <th scope="col">Precip</th>
              <th scope="col">Wind</th>
              <th scope="col">Sky</th>
            </tr>
          </thead>
          <tbody>
            {steps.map((h, i) => (
              <tr key={h.time}>
                <td>{label(i)}</td>
                <td>{formatTemp(h.temp_c, units)}</td>
                <td>{precipCell(h)}</td>
                <td>{formatWind(h.wind_ms, h.wind_dir_deg, units)}</td>
                <td>{conditionWord(h.condition, h.text) ?? ''}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </details>
    </div>
  );
}
