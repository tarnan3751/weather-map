// Where the map opens: where the user left it, else their part of the world
// as the browser's time zone suggests (no location permission needed), else
// the whole world.

/** Mainland United States, framed as the map always used to open. */
const CONUS: [[number, number], [number, number]] = [
  [-125, 24],
  [-66.5, 49.5],
];
/** Phones are tall and narrow: the middle of the country rather than both
 *  coasts in a strip. */
const CONUS_NARROW: [[number, number], [number, number]] = [
  [-114, 27],
  [-78, 47],
];

export type FirstView =
  | { kind: 'center'; center: [number, number]; zoom: number }
  | { kind: 'bounds'; bounds: [[number, number], [number, number]] };

const VIEW_KEY = 'wx.view';

function storage(): Storage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}

/** The view the user last left the map at, if this browser remembers one. */
export function savedView(s: Pick<Storage, 'getItem'> | null = storage()): FirstView | null {
  try {
    const v = JSON.parse(s?.getItem(VIEW_KEY) ?? 'null') as { lng?: unknown; lat?: unknown; zoom?: unknown } | null;
    if (!v || typeof v.lng !== 'number' || typeof v.lat !== 'number' || typeof v.zoom !== 'number') return null;
    if (![v.lng, v.lat, v.zoom].every(Number.isFinite) || Math.abs(v.lat) > 85.06 || v.zoom < 0 || v.zoom > 22) return null;
    return { kind: 'center', center: [((((v.lng + 180) % 360) + 360) % 360) - 180, v.lat], zoom: v.zoom };
  } catch {
    return null;
  }
}

export function saveView(lng: number, lat: number, zoom: number, s: Pick<Storage, 'setItem'> | null = storage()) {
  try {
    s?.setItem(VIEW_KEY, JSON.stringify({ lng: Math.round(lng * 1e4) / 1e4, lat: Math.round(lat * 1e4) / 1e4, zoom: Math.round(zoom * 100) / 100 }));
  } catch {
    // Not remembered; the map opens on the guess next time.
  }
}

/** The whole world across the viewport. */
function worldView(width: number): FirstView {
  return { kind: 'center', center: [0, 25], zoom: Math.max(0, Math.log2(Math.max(256, width) / 512)) };
}

/**
 * A first view for someone in `timeZone`: the mainland US as before for US
 * zones there, otherwise a continent-sized view centred on the zone's city.
 * Zones that name no place (UTC, Etc/GMT+3) get the world, or a band of the
 * world at the right longitude when the offset says something.
 */
export async function guessView(timeZone: string | undefined, width: number, offsetMinutes = new Date().getTimezoneOffset()): Promise<FirstView> {
  const narrow = width < 700;
  if (!timeZone || /^(UTC|GMT|Etc\/(UTC|GMT|UCT|Universal|Zulu)?)$/i.test(timeZone)) return worldView(width);
  const { zonePlace } = await import('./zones.ts');
  const place = zonePlace(timeZone);
  if (!place) {
    // An unknown or offset-only zone (Etc/GMT+3): its longitude is roughly
    // 15 degrees per hour east of Greenwich.
    if (!Number.isFinite(offsetMinutes)) return worldView(width);
    return { kind: 'center', center: [Math.max(-180, Math.min(180, -offsetMinutes / 4)), 25], zoom: narrow ? 1.6 : 2.2 };
  }
  if (place.country === 'US' && place.lon > -130 && place.lat < 50) return { kind: 'bounds', bounds: narrow ? CONUS_NARROW : CONUS };
  return { kind: 'center', center: [place.lon, place.lat], zoom: narrow ? 3.2 : 4 };
}
