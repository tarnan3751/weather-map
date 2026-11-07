// Zoom-dependent thinning of point overlays (METAR stations, anywhere on the
// globe): each point gets the lowest whole zoom level at which it is at least
// `spacingPx` from every point of higher priority already shown, so a zoomed
// out map shows an even spread of the most useful stations and zooming in
// fills the gaps. Computed once per data set; the style filters on it.

import { MAP_TILE_PX } from '../tiles/tileMath.ts';

export interface ThinPoint {
  /** Web-Mercator x and y, 0..1. */
  x: number;
  y: number;
  /** Higher is shown first. */
  priority: number;
}

/**
 * The zoom (minZoom..maxZoom) from which each point is shown. Every point
 * left at `maxZoom` is shown there regardless of spacing.
 */
export function thinByZoom(points: ThinPoint[], spacingPx: number, minZoom: number, maxZoom: number): Uint8Array {
  const order = points.map((_, i) => i).sort((a, b) => points[b].priority - points[a].priority || a - b);
  const zoom = new Uint8Array(points.length).fill(maxZoom);
  const placed: number[] = [];
  for (let z = minZoom; z < maxZoom; z++) {
    // Grid cells one spacing wide: a closer neighbour is in the 3 x 3 block.
    // Columns wrap round the world, so stations either side of the
    // antimeridian are neighbours too.
    const cell = spacingPx / (MAP_TILE_PX * 2 ** z);
    const cols = Math.max(1, Math.ceil(1 / cell));
    const col = (x: number) => ((Math.floor(x / cell) % cols) + cols) % cols;
    const grid = new Map<number, number[]>();
    const key = (cx: number, cy: number) => cx * 1_000_003 + cy;
    const add = (i: number) => {
      const k = key(col(points[i].x), Math.floor(points[i].y / cell));
      const list = grid.get(k);
      if (list) list.push(i);
      else grid.set(k, [i]);
    };
    for (const i of placed) add(i);
    const crowded = (i: number) => {
      const p = points[i];
      const cx = col(p.x);
      const cy = Math.floor(p.y / cell);
      for (let dx = -1; dx <= 1; dx++) {
        for (let dy = -1; dy <= 1; dy++) {
          for (const j of grid.get(key((cx + dx + cols) % cols, cy + dy)) ?? []) {
            const q = points[j];
            let ddx = Math.abs(p.x - q.x);
            ddx = Math.min(ddx, 1 - ddx);
            if (ddx ** 2 + (p.y - q.y) ** 2 < cell * cell) return true;
          }
        }
      }
      return false;
    };
    for (const i of order) {
      if (zoom[i] < maxZoom || crowded(i)) continue;
      zoom[i] = z;
      placed.push(i);
      add(i);
    }
  }
  return zoom;
}

/** A stable 0..1 value per id, so ties between stations break the same way
 *  on every load and every update. */
export function stableFraction(id: string): number {
  let h = 2166136261;
  for (let i = 0; i < id.length; i++) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return (h >>> 0) / 4294967296;
}

/** Main airports: the stations people look for first (US ones; elsewhere
 *  every airport counts the same, ahead of buoys and small stations by how
 *  much they report). */
const HUBS = new Set(
  (
    'KATL KORD KDFW KDEN KLAX KJFK KSFO KSEA KLAS KMCO KCLT KPHX KMIA KIAH KEWR KMSP KBOS KDTW KPHL KLGA KFLL KBWI ' +
    'KDCA KSLC KSAN KTPA KMDW KPDX KSTL KBNA KAUS KMCI KRDU KSMF KSAT KCLE KPIT KIND KCMH KCVG KMKE KMSY KOKC KABQ ' +
    'KTUL KMEM KJAX KBUF KOMA KBOI KELP KTUS KRNO KSDF KRIC KBDL KPVD KBIL KFAR KFSD KBIS KCYS KGEG KICT KLIT KBHM ' +
    'KJAN KDSM KMSN KGRR KALB KBTV KPWM KSYR KCHS KSAV KDAL KHOU KORF KGSO KCRW KLEX KSGF KLBB KAMA KMAF KRAP KGTF ' +
    'KMSO KFAT KSBA KMFR KEUG KBZN KIDA KGJT KFLG KPUB KDLH KGRB KFNT KTOL KDAY KTYS KCHA KAVL KCAE KMOB KPNS KTLH ' +
    'KBTR KLFT KSHV KJLN KLNK KABR KMOT KINL KMQT KBGR KCAR KACY KABE KIPT KSCH KEYW KRSW KPBI KDAB PANC PAFA PAJN ' +
    'PHNL PHOG PHTO PHLI CYYZ CYUL CYVR CYYC CYEG CYWG CYOW CYQB CYHZ'
  ).split(' '),
);

type Maybe<T> = T | null | undefined;

/** Which stations to keep when there is room for only some: main airports,
 *  then those with a temperature (the label), then the most complete reports.
 *  Unreported values are absent (or null from older servers). */
export function stationPriority(p: {
  id: string;
  temp_c?: Maybe<number>;
  dewpoint_c?: Maybe<number>;
  wind_speed_ms?: Maybe<number>;
  visibility_m?: Maybe<number>;
  altimeter_hpa?: Maybe<number>;
  flight_category?: Maybe<string>;
}): number {
  const fields = [p.temp_c, p.dewpoint_c, p.wind_speed_ms, p.visibility_m, p.altimeter_hpa, p.flight_category].filter((v) => v !== null && v !== undefined).length;
  return (HUBS.has(p.id) ? 100 : 0) + (p.temp_c !== null && p.temp_c !== undefined ? 20 : 0) + fields + stableFraction(p.id);
}
