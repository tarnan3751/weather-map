import { useEffect, useRef } from 'react';
import { currentLayer } from '../app/state.ts';
import { buildLut, getPalette, lookupLut, type Palette, PTYPE_NAMES, transferAt, transferPos } from '../core/palettes.ts';
import { displayUnit, type DisplayUnit, formatNumber } from '../core/units.ts';
import { useApp, useAppState } from './context.ts';

/** Position of stored value `v` along a legend bar, 0..1, in the palette's
 *  own transfer space so ticks spread the way colours do. */
const legendPos = (p: Palette, v: number) => transferPos(p.transfer, p.legend.range, v);

function Bar({ palette, stepped, row = 0, height = 10 }: { palette: Palette; stepped: boolean; row?: number; height?: number }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const c = ref.current;
    if (!c) return;
    const w = 256;
    c.width = w;
    c.height = 1;
    const g = c.getContext('2d');
    if (!g) return;
    const lut = buildLut(palette, stepped);
    const img = g.createImageData(w, 1);
    for (let i = 0; i < w; i++) {
      const v = transferAt(palette.transfer, palette.legend.range, (i + 0.5) / w);
      const [r, gg, b, a] = lookupLut(lut, palette, v, row);
      // Show transparent stretches as they look over the map: dark.
      const k = a / 255;
      img.data.set([r * k + 23 * (1 - k), gg * k + 27 * (1 - k), b * k + 34 * (1 - k), 255], i * 4);
    }
    g.putImageData(img, 0, 0);
  }, [palette, stepped, row]);
  return <canvas ref={ref} className="legend-bar" style={{ height }} aria-hidden="true" />;
}

function Ticks({ palette, du }: { palette: Palette; du: DisplayUnit }) {
  const ticks = palette.legend.ticks[du.label] ?? [];
  return (
    <div className="legend-ticks" aria-hidden="true">
      {ticks.map((t) => {
        const x = legendPos(palette, du.from(t));
        return (
          <span key={t} style={{ left: `${x * 100}%` }} className={x < 0.04 ? 'first' : x > 0.96 ? 'last' : ''}>
            {formatNumber(t, Number.isInteger(t) ? 0 : t < 0.1 ? 2 : 1)}
          </span>
        );
      })}
    </div>
  );
}

export function Legend() {
  const { store } = useApp();
  const layer = useAppState(currentLayer);
  const units = useAppState((s) => s.units);
  const stepped = useAppState((s) => s.stepped);
  const byGust = useAppState((s) => s.windByGust);
  if (!layer) return null;
  const palette = getPalette(layer.palette);
  const r = layer.render;
  // The band the colours stand for: a rate, a speed, the value itself.
  const du = displayUnit(layer.bands[r.kind === 'precip_type' ? r.rate : r.kind === 'wind' ? r.u : r.band].quantity, units);
  // Infrared shows a temperature, but of cloud tops, not of the air: say so.
  const title =
    r.kind === 'wind' ? (byGust ? 'Wind gusts' : 'Wind speed') : r.kind === 'precip_type' ? 'Precipitation rate' : palette.id === 'infrared' ? palette.title : layer.title;
  const words = palette.legend.words ?? [];
  const summary =
    r.kind === 'precip_type'
      ? 'Colour shows the type of precipitation, deeper for heavier rates.'
      : `${title}, from ${formatNumber(du.to(palette.legend.range[0]), 0)} to ${formatNumber(du.to(palette.legend.range[1]), 0)} ${du.label}.`;

  return (
    <section className="legend card" aria-label={`Legend: ${title}`}>
      <div className="legend-head">
        <span className="legend-title">{title}</span>
        <span className="legend-unit">{du.label}</span>
        {r.kind === 'wind' && r.gust !== null && (
          <div className="seg-control tiny" role="group" aria-label="Colour wind by">
            <button aria-pressed={!byGust} onClick={() => store.set({ windByGust: false })}>
              Speed
            </button>
            <button aria-pressed={byGust} onClick={() => store.set({ windByGust: true })}>
              Gusts
            </button>
          </div>
        )}
      </div>
      <p className="sr-only">{summary}</p>
      {r.kind === 'precip_type' ? (
        <div className="legend-ptype">
          {[1, 2, 3, 4, 5].map((code) => (
            <div className="ptype-row" key={code}>
              <span className="ptype-name">{PTYPE_NAMES[code]}</span>
              <Bar palette={palette} stepped={stepped} row={code} height={7} />
            </div>
          ))}
          <div className="ptype-row">
            <span className="ptype-name" />
            <Ticks palette={palette} du={du} />
          </div>
        </div>
      ) : (
        <>
          <Bar palette={palette} stepped={stepped} />
          <Ticks palette={palette} du={du} />
          {words.length > 0 && (
            <div className="legend-words" aria-hidden="true">
              {words.map((w) => (
                <span key={w.text} style={{ left: `${legendPos(palette, w.v) * 100}%` }}>
                  {w.text}
                </span>
              ))}
            </div>
          )}
        </>
      )}
    </section>
  );
}
