// Synthetic alerts: NWS alerts placed on the synthetic weather, the rest of
// the world's CAP alerts (alertsWorld.ts), and a Tornado Warning over the
// Kansas City storm that appears a few seconds after a page loads, to
// exercise the live `alerts` path and place notifications. Answers what the
// server's alert endpoints answer: the list, point queries, details (ended
// alerts too, for a day), TileJSON and vector tiles, all keyed to a revision
// that rises whenever the drawn set changes.

import type { AlertCategory, AlertCollection, AlertDetail, AlertFeature, AlertInfo, AlertProps, AlertTileJson, Geometry, LonLatBox, Severity } from '../src/api/types.ts';
import { QUIET_SOURCES, UNSHAPED_SEED, UPDATED_SEED, WORLD, WORLD_SOURCES, type WorldTemplate } from './alertsWorld.ts';
import { hash1, hash3, pointInPolygon, wrapLon } from './geo.ts';
import { clipAxis, encodeTile, ringBox, type MvtFeature, type Polygon, type Ring } from './mvt.ts';
import { DAY, HOUR, iso, round } from './util.ts';

interface MockAlert {
  props: AlertDetail;
  /** The full stored shape, rings closed; empty for an alert without one. */
  polygons: Polygon[];
  /** One per polygon: a shape split at the antimeridian keeps two boxes
   *  rather than one spanning the world. */
  boxes: LonLatBox[];
  /** The light shape `/api/alerts` lists. */
  display: Geometry | null;
  /** Template seed: an Update keeps it, so it names the hazard's thread. */
  thread: number;
}

export const EUROGEOGRAPHICS = '© EuroGeographics for the administrative boundaries';
const TILE_MAX_ZOOM = 16;
/** Tiles stop gaining detail here; clients overzoom past it. */
const TILE_DETAIL_ZOOM = 12;

const CATEGORIES = new Set<AlertCategory>(['met', 'geo', 'safety', 'security', 'rescue', 'fire', 'health', 'env', 'transport', 'infra', 'cbrne', 'other']);
/** As the server normalises it: lower case, `other` for none or unknown. */
const category = (c: string | null | undefined): AlertCategory => {
  const k = (c ?? '').toLowerCase() as AlertCategory;
  return CATEGORIES.has(k) ? k : 'other';
};

function alertId(n: number): string {
  const hex = Math.floor(hash1(n) * 0xffffffff).toString(16).padStart(8, '0');
  const hex2 = Math.floor(hash1(n + 99) * 0xffffffff).toString(16).padStart(8, '0');
  return `urn:oid:2.49.0.1.840.0.${hex}${hex2}.001.1`;
}

/** `cap:` and 20 hex digits, fixed for a seed. */
export function capId(seed: number): string {
  let s = '';
  for (let i = 0; s.length < 20; i++) s += Math.floor(hash3(seed, i, 0x5eed) * 0x100000000).toString(16).padStart(8, '0');
  return `cap:${s.slice(0, 20)}`;
}

const close = (r: Ring): Ring => (r[0][0] === r[r.length - 1][0] && r[0][1] === r[r.length - 1][1] ? r : [...r, r[0]]);

const SEVERITY_LEVEL: Record<string, number> = { Extreme: 4, Severe: 3, Moderate: 2, Minor: 1, Unknown: 0 };
const level = (s: string) => SEVERITY_LEVEL[s] ?? 0;
const sentMs = (a: MockAlert) => (a.props.sent ? Date.parse(a.props.sent) : -Infinity);
/** By level, then by time sent, `dir` 1 ascending and -1 descending; ties by id. */
const bySeverity = (dir: 1 | -1) => (a: MockAlert, b: MockAlert) =>
  dir * (level(a.props.severity) - level(b.props.severity) || sentMs(a) - sentMs(b)) || (a.props.id < b.props.id ? -1 : 1);
/** Most severe first, then newest: the order of every list. */
const listOrder = bySeverity(-1);
/** Least severe first, then oldest: the order features are painted in. */
const paintOrder = bySeverity(1);

// ---------------------------------------------------------------- shapes

const meets = (a: LonLatBox, b: LonLatBox) => a.west <= b.east && a.east >= b.west && a.south <= b.north && a.north >= b.south;

/** A query box as one or two boxes that do not cross the antimeridian. */
function splitBox(b: LonLatBox): LonLatBox[] {
  if (b.west <= b.east) return [b];
  return [
    { west: b.west, south: b.south, east: 180, north: b.north },
    { west: -180, south: b.south, east: b.east, north: b.north },
  ];
}

/** A shape drawn with longitudes past 180 cut there, the far side moved back
 *  by 360, so no ring jumps across the map. */
function splitAntimeridian(polys: Polygon[]): Polygon[] {
  if (!polys.some((p) => p.some((r) => r.some(([lon]) => lon > 180)))) return polys;
  const out: Polygon[] = [];
  for (const p of polys) {
    for (const east of [false, true]) {
      const piece: Polygon = [];
      for (const r of p) {
        const cut = clipAxis(r, 0, 180, east);
        if (cut.length < 3) {
          if (piece.length === 0) break;
          continue;
        }
        piece.push(east ? cut.map(([lon, lat]): [number, number] => [lon - 360, lat]) : cut);
      }
      if (piece.length > 0) out.push(piece);
    }
  }
  return out;
}

function covers(polys: readonly Polygon[], lon: number, lat: number): boolean {
  return polys.some((p) => pointInPolygon(lon, lat, p[0]) && !p.slice(1).some((h) => pointInPolygon(lon, lat, h)));
}

/** A point on the shape: the middle of the widest stretch inside the largest
 *  polygon along a line of latitude through its middle. */
function pointOnShape(polys: readonly Polygon[]): [number, number] | null {
  let best: Polygon | null = null;
  let bestArea = -1;
  for (const p of polys) {
    const b = ringBox(p[0]);
    const area = (b.east - b.west) * (b.north - b.south);
    if (area > bestArea) [best, bestArea] = [p, area];
  }
  if (!best) return null;
  const b = ringBox(best[0]);
  // Off the exact middle, so the line is unlikely to pass through a vertex.
  const lat = b.south + (b.north - b.south) * 0.5013;
  const xs: number[] = [];
  for (const r of best) {
    for (let i = 0, j = r.length - 1; i < r.length; j = i++) {
      const [xi, yi] = r[i];
      const [xj, yj] = r[j];
      if (yi > lat !== yj > lat) xs.push(xi + ((lat - yi) * (xj - xi)) / (yj - yi));
    }
  }
  xs.sort((p, q) => p - q);
  let pick: number | null = null;
  let width = -1;
  for (let i = 0; i + 1 < xs.length; i += 2) {
    if (xs[i + 1] - xs[i] > width) [width, pick] = [xs[i + 1] - xs[i], (xs[i] + xs[i + 1]) / 2];
  }
  return pick === null ? null : [round(pick, 6), round(lat, 6)];
}

/** Rounded to three decimals with repeats dropped, rings still closed. */
function displayGeometry(polys: readonly Polygon[]): Geometry | null {
  if (polys.length === 0) return null;
  const light = polys.map((p) =>
    p.map((r) => {
      const out: [number, number][] = [];
      for (const [lon, lat] of r) {
        const q: [number, number] = [round(lon, 3), round(lat, 3)];
        const last = out[out.length - 1];
        if (!last || last[0] !== q[0] || last[1] !== q[1]) out.push(q);
      }
      return close(out);
    }),
  );
  return light.length === 1 ? { type: 'Polygon', coordinates: light[0] } : { type: 'MultiPolygon', coordinates: light };
}

function shaped(props: AlertDetail, polygons: Polygon[], thread: number, anchor?: [number, number]): MockAlert {
  const closed = polygons.map((p) => p.map(close));
  props.anchor = closed.length > 0 ? pointOnShape(closed) : (anchor ?? null);
  return { props, polygons: closed, boxes: closed.map((p) => ringBox(p[0])), display: displayGeometry(closed), thread };
}

/** An alert's detail: the shown block's text and the message's own fields. */
function detail(
  shown: AlertInfo,
  infos: AlertInfo[],
  m: Pick<AlertDetail, 'id' | 'sent' | 'ends' | 'expires' | 'expires_assumed' | 'geom_source' | 'source' | 'country' | 'message_type' | 'source_name' | 'country_name'>,
): AlertDetail {
  return {
    id: m.id,
    event: shown.event,
    severity: shown.severity!,
    urgency: shown.urgency,
    certainty: shown.certainty,
    headline: shown.headline,
    area_desc: shown.area_desc,
    sent: m.sent,
    onset: shown.onset,
    ends: m.ends,
    expires: m.expires,
    expires_assumed: m.expires_assumed,
    geom_source: m.geom_source,
    source: m.source,
    country: m.country,
    language: shown.language,
    category: shown.category,
    anchor: null,
    description: shown.description,
    instruction: shown.instruction,
    sender_name: shown.sender_name,
    message_type: m.message_type,
    status: 'Actual',
    active: true,
    source_name: m.source_name,
    country_name: m.country_name,
    infos,
  };
}

// ------------------------------------------------------------- the NWS

interface Template {
  n: number;
  event: string;
  severity: Severity;
  urgency: string;
  certainty: string;
  area: string;
  sender: string;
  geom: 'polygon' | 'zones';
  ring: Ring;
  /** Hours relative to now. */
  sent: number;
  ends: number;
  headline: string;
  what: string;
  impacts: string;
  instruction: string;
}

const TEMPLATES: Template[] = [
  { n: 1, event: 'Winter Storm Warning', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', geom: 'zones', sent: -3, ends: 30,
    sender: 'NWS Sioux Falls SD', area: 'Knox, NE; Cedar, NE; Dixon, NE; Yankton, SD; Clay, SD; Union, SD; Plymouth, IA',
    ring: [[-100, 45], [-96.5, 45], [-96.5, 44.5], [-95.2, 44.5], [-95.2, 43.5], [-96.4, 43.5], [-96.4, 42.5], [-98.3, 42.5], [-98.3, 42.9], [-100, 42.9]],
    headline: 'Winter Storm Warning issued until tomorrow evening by NWS Sioux Falls SD',
    what: 'Heavy snow. Total snow accumulations of 6 to 10 inches. Winds gusting as high as 40 mph.',
    impacts: 'Travel could be very difficult. Patchy blowing snow could significantly reduce visibility.',
    instruction: 'If you must travel, keep an extra flashlight, food, and water in your vehicle in case of an emergency.' },
  { n: 2, event: 'Winter Weather Advisory', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', geom: 'zones', sent: -2, ends: 26,
    sender: 'NWS Aberdeen SD', area: 'Brown, SD; Spink, SD; Beadle, SD; Kingsbury, SD; Brookings, SD; Lincoln, MN',
    ring: [[-101.5, 46.2], [-95.5, 46.2], [-95.5, 45], [-96.5, 45], [-100, 45], [-100, 44.2], [-101.5, 44.2]],
    headline: 'Winter Weather Advisory issued until tomorrow afternoon by NWS Aberdeen SD',
    what: 'Snow. Additional snow accumulations of 2 to 4 inches.',
    impacts: 'Plan on slippery road conditions.',
    instruction: 'Slow down and use caution while traveling.' },
  { n: 3, event: 'Ice Storm Warning', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', geom: 'zones', sent: -1.5, ends: 18,
    sender: 'NWS Des Moines IA', area: 'Woodbury, IA; Ida, IA; Sac, IA; Calhoun, IA; Webster, IA; Hamilton, IA',
    ring: [[-96.2, 42.9], [-93.4, 42.9], [-93.4, 42.2], [-94.6, 42.2], [-94.6, 41.9], [-96.2, 41.9]],
    headline: 'Ice Storm Warning issued until tomorrow morning by NWS Des Moines IA',
    what: 'Significant icing. Additional ice accumulations of one quarter to one half inch.',
    impacts: 'Power outages and tree damage are likely due to the ice. Travel could be nearly impossible.',
    instruction: 'Travel is strongly discouraged. If you must travel, keep an extra flashlight, food, and water in your vehicle.' },
  { n: 4, event: 'Tornado Watch', severity: 'Severe', urgency: 'Future', certainty: 'Possible', geom: 'zones', sent: -1, ends: 6,
    sender: 'NWS Storm Prediction Center Norman OK', area: 'Tornado Watch 612: Osage, OK; Pawnee, OK; Creek, OK; Tulsa, OK; Okmulgee, OK; Muskogee, OK; Sebastian, AR; Crawford, AR',
    ring: [[-98, 37], [-94, 37], [-94, 34.6], [-95, 34.6], [-95, 33.6], [-97.4, 33.6], [-97.4, 34.8], [-98, 34.8]],
    headline: 'Tornado Watch issued until this evening by NWS Storm Prediction Center',
    what: 'Conditions are favorable for tornadoes and severe thunderstorms in and close to the watch area.',
    impacts: 'A few tornadoes, scattered large hail up to 2 inches and damaging wind gusts to 70 mph are possible.',
    instruction: 'Review your severe weather plan and be ready to act quickly if a warning is issued.' },
  { n: 5, event: 'Severe Thunderstorm Warning', severity: 'Severe', urgency: 'Immediate', certainty: 'Observed', geom: 'polygon', sent: -0.3, ends: 0.6,
    sender: 'NWS Norman OK', area: 'Logan, OK; Payne, OK; Lincoln, OK',
    ring: [[-97.65, 35.62], [-97.42, 36.18], [-96.78, 36.05], [-96.95, 35.5]],
    headline: 'Severe Thunderstorm Warning issued until later this morning by NWS Norman OK',
    what: 'Severe thunderstorm moving east at 35 mph. 70 mph wind gusts and quarter size hail.',
    impacts: 'Expect considerable tree damage. Damage is likely to mobile homes, roofs, and outbuildings.',
    instruction: 'For your protection move to an interior room on the lowest floor of a building.' },
  { n: 6, event: 'Flash Flood Warning', severity: 'Severe', urgency: 'Immediate', certainty: 'Likely', geom: 'polygon', sent: -0.8, ends: 2.5,
    sender: 'NWS Fort Worth TX', area: 'Tarrant, TX; Dallas, TX; Johnson, TX',
    ring: [[-97.55, 32.55], [-97.45, 32.98], [-96.75, 32.95], [-96.85, 32.48]],
    headline: 'Flash Flood Warning issued until this afternoon by NWS Fort Worth TX',
    what: 'Flash flooding caused by thunderstorms. Between 2 and 3 inches of rain have fallen.',
    impacts: 'Flash flooding of small creeks and streams, urban areas, highways, streets and underpasses.',
    instruction: 'Turn around, don\'t drown when encountering flooded roads. Most flood deaths occur in vehicles.' },
  { n: 7, event: 'Wind Advisory', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', geom: 'zones', sent: -2, ends: 10,
    sender: 'NWS Amarillo TX', area: 'Texas Panhandle; Oklahoma Panhandle; Southwest Kansas',
    ring: [[-103, 38], [-100, 38], [-100, 36.5], [-99.5, 36.5], [-99.5, 34.3], [-103, 34.3]],
    headline: 'Wind Advisory issued until this evening by NWS Amarillo TX',
    what: 'Northwest winds 25 to 35 mph with gusts up to 50 mph.',
    impacts: 'Gusty winds will blow around unsecured objects. Tree limbs could be blown down.',
    instruction: 'Use extra caution when driving, especially if operating a high profile vehicle.' },
  { n: 8, event: 'Red Flag Warning', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', geom: 'zones', sent: -5, ends: 20,
    sender: 'NWS Los Angeles/Oxnard CA', area: 'Ventura County Mountains; Los Angeles County Mountains; Santa Clarita Valley',
    ring: [[-119.4, 34.9], [-117.6, 34.9], [-117.6, 34.2], [-118.4, 34.2], [-118.4, 34.05], [-119.4, 34.05]],
    headline: 'Red Flag Warning issued until tomorrow morning by NWS Los Angeles/Oxnard CA',
    what: 'Gusty Santa Ana winds and low humidity. Northeast winds 20 to 30 mph with gusts to 55 mph.',
    impacts: 'Any fires that develop will likely spread rapidly.',
    instruction: 'Avoid outdoor burning and any activity that can produce sparks.' },
  { n: 9, event: 'Lake Effect Snow Warning', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', geom: 'zones', sent: -4, ends: 36,
    sender: 'NWS Buffalo NY', area: 'Southern Erie; Wyoming; Chautauqua; Cattaraugus',
    ring: [[-79.8, 42.9], [-78, 42.9], [-78, 42], [-79.8, 42]],
    headline: 'Lake Effect Snow Warning issued until Sunday morning by NWS Buffalo NY',
    what: 'Lake effect snow. Additional snow accumulations of 1 to 2 feet in the most persistent bands.',
    impacts: 'Travel will be very difficult to impossible. Snowfall rates of 2 to 3 inches per hour.',
    instruction: 'Persons should consider delaying all travel.' },
  { n: 10, event: 'Dense Fog Advisory', severity: 'Minor', urgency: 'Expected', certainty: 'Likely', geom: 'zones', sent: -6, ends: 3,
    sender: 'NWS Sacramento CA', area: 'Sacramento Valley; Northern San Joaquin Valley',
    ring: [[-122.4, 40.2], [-121.6, 40.2], [-120.4, 37.6], [-119.6, 36.8], [-120.3, 36.5], [-121.4, 37.4], [-122.3, 38.6]],
    headline: 'Dense Fog Advisory issued until late morning by NWS Sacramento CA',
    what: 'Visibility one quarter mile or less in dense fog.',
    impacts: 'Low visibility could make driving conditions hazardous.',
    instruction: 'If driving, slow down, use your headlights, and leave plenty of distance ahead of you.' },
  { n: 11, event: 'Special Weather Statement', severity: 'Moderate', urgency: 'Expected', certainty: 'Observed', geom: 'polygon', sent: -0.5, ends: 1,
    sender: 'NWS Melbourne FL', area: 'Orange, FL; Seminole, FL; Osceola, FL',
    ring: [[-81.6, 28.2], [-81.5, 28.8], [-81, 28.75], [-81.1, 28.15]],
    headline: 'Strong thunderstorms will impact portions of Orange, Seminole and Osceola Counties',
    what: 'Strong thunderstorm with wind gusts up to 45 mph and pea size hail.',
    impacts: 'Gusty winds could knock down tree limbs and blow around unsecured objects.',
    instruction: 'If outdoors, consider seeking shelter inside a building.' },
  { n: 12, event: 'High Wind Warning', severity: 'Severe', urgency: 'Expected', certainty: 'Likely', geom: 'zones', sent: -3, ends: 15,
    sender: 'NWS Cheyenne WY', area: 'Laramie Range; South Laramie Range Foothills; Larimer County Mountains',
    ring: [[-106.4, 42.6], [-104.6, 42.6], [-104.6, 41], [-105.2, 41], [-105.2, 40.4], [-106.4, 40.4]],
    headline: 'High Wind Warning issued until tomorrow morning by NWS Cheyenne WY',
    what: 'West winds 35 to 45 mph with gusts up to 75 mph.',
    impacts: 'Travel will be difficult, especially for high profile vehicles.',
    instruction: 'People should avoid being outside in forested areas and around trees and branches.' },
  { n: 13, event: 'Flood Watch', severity: 'Severe', urgency: 'Future', certainty: 'Possible', geom: 'zones', sent: -4, ends: 28,
    sender: 'NWS Shreveport LA', area: 'Southern Arkansas; Northern Louisiana; Northeast Texas',
    ring: [[-95, 34], [-91.2, 34], [-91.2, 31.6], [-93, 31.6], [-93, 31], [-95, 31]],
    headline: 'Flood Watch issued through Saturday morning by NWS Shreveport LA',
    what: 'Flooding caused by excessive rainfall is possible. 2 to 4 inches of rain, locally higher.',
    impacts: 'Excessive runoff may result in flooding of rivers, creeks, streams, and other low-lying areas.',
    instruction: 'Monitor later forecasts and be alert for possible Flood Warnings.' },
  { n: 14, event: 'Freeze Warning', severity: 'Moderate', urgency: 'Expected', certainty: 'Likely', geom: 'zones', sent: -6, ends: 22,
    sender: 'NWS Albuquerque NM', area: 'Eastern New Mexico; West Texas',
    ring: [[-106, 35.2], [-103, 35.2], [-103, 32.2], [-104.6, 32.2], [-104.6, 33], [-106, 33]],
    headline: 'Freeze Warning issued for tonight by NWS Albuquerque NM',
    what: 'Sub-freezing temperatures as low as 25 degrees.',
    impacts: 'Frost and freeze conditions could kill crops and other sensitive vegetation.',
    instruction: 'Take steps now to protect tender plants from the cold.' },
];

/** The warning that arrives after load: a radar-confirmed tornado on the
 *  stationary storm over downtown Kansas City. */
const LIVE_TEMPLATE: Template = { n: 100, event: 'Tornado Warning', severity: 'Extreme', urgency: 'Immediate', certainty: 'Observed', geom: 'polygon', sent: 0, ends: 0.75,
  sender: 'NWS Kansas City/Pleasant Hill MO', area: 'Jackson, MO; Wyandotte, KS; Johnson, KS; Clay, MO',
  ring: [[-94.98, 38.93], [-94.8, 39.33], [-94.22, 39.27], [-94.3, 38.9]],
  headline: 'Tornado Warning issued until later this morning by NWS Kansas City/Pleasant Hill MO',
  what: 'Radar confirmed tornado moving east at 25 mph.',
  impacts: 'Flying debris will be dangerous to those caught without shelter. Mobile homes will be damaged or destroyed.',
  instruction: 'TAKE COVER NOW! Move to a basement or an interior room on the lowest floor of a sturdy building. Avoid windows. If you are outdoors, in a mobile home, or in a vehicle, move to the closest substantial shelter and protect yourself from flying debris.' };

function buildNws(t: Template, now: number): MockAlert {
  const sent = now + t.sent * HOUR;
  const ends = now + t.ends * HOUR;
  const fmt = new Intl.DateTimeFormat('en-US', { hour: 'numeric', minute: '2-digit', timeZone: 'America/Chicago', timeZoneName: 'short' });
  const description =
    t.geom === 'polygon'
      ? `At ${fmt.format(sent)}, ${t.what.charAt(0).toLowerCase()}${t.what.slice(1)}\n\nHAZARD...${t.what}\n\nIMPACT...${t.impacts}\n\nLocations impacted include ${t.area.replace(/, [A-Z]{2}/g, '').replace(/;/g, ',')}.`
      : `* WHAT...${t.what}\n\n* WHERE...${t.area}.\n\n* WHEN...Until ${fmt.format(ends)}.\n\n* IMPACTS...${t.impacts}`;
  // The server builds the one block from the message's own fields.
  const info: AlertInfo = {
    language: 'en-US', event: t.event, headline: t.headline, description, instruction: t.instruction, area_desc: t.area, sender_name: t.sender, web: null,
    category: 'met', severity: t.severity, urgency: t.urgency, certainty: t.certainty, onset: iso(sent), expires: iso(Math.min(ends, sent + 6 * HOUR)), in_effect: true,
  };
  const props = detail(info, [info], {
    id: alertId(t.n), sent: iso(sent), ends: iso(ends), expires: info.expires, expires_assumed: false, geom_source: t.geom, source: 'us-nws', country: 'us',
    message_type: 'Alert', source_name: 'United States of America: National Weather Service', country_name: 'United States of America',
  });
  return shaped(props, [[t.ring]], t.n);
}

// ----------------------------------------------------------- the world

const worldSource = new Map(WORLD_SOURCES.map((s) => [s.id, s]));

/** The block an alert is shown in: English when the message has it, else its first. */
const shownInfo = (t: WorldTemplate) => t.infos.find((i) => /^en(-|$)/i.test(i.language)) ?? t.infos[0];

const updateNote = (language: string) => (language.startsWith('de') ? 'Aktualisierung: Die Warnung wurde verlängert.' : 'Update: the warning has been extended.');

function buildWorld(t: WorldTemplate, base: number, update?: { id: string; sent: number; expires: number }): MockAlert {
  const src = worldSource.get(t.source);
  const onset = base + t.onset * HOUR;
  // A sender with no expiry gets a day after onset, and the blocks keep null.
  const given = update?.expires ?? (t.expires === null ? null : base + t.expires * HOUR);
  const infos = t.infos.map(
    (i): AlertInfo => ({
      language: i.language,
      event: i.event,
      headline: i.headline ?? null,
      description: update && i.description ? `${updateNote(i.language)} ${i.description}` : (i.description ?? null),
      instruction: i.instruction ?? null,
      area_desc: i.area ?? null,
      sender_name: i.sender ?? null,
      web: i.web ?? null,
      category: category(t.category),
      severity: t.severity,
      urgency: t.urgency,
      certainty: t.certainty,
      onset: iso(onset),
      expires: given === null ? null : iso(given),
      in_effect: !i.past,
    }),
  );
  const props = detail(infos[t.infos.indexOf(shownInfo(t))], infos, {
    id: update?.id ?? capId(t.n), sent: iso(update?.sent ?? base + t.sent * HOUR), ends: null, expires: iso(given ?? onset + DAY), expires_assumed: given === null,
    geom_source: t.geom, source: t.source, country: src?.country ?? null, message_type: update ? 'Update' : 'Alert',
    source_name: src?.name ?? null, country_name: src?.country_name ?? null,
  });
  return shaped(props, splitAntimeridian(t.polygons), t.n, t.anchor);
}

/** What `/api/alerts`, `/api/alerts/at` and place summaries carry: the detail
 *  without its texts and blocks. */
function summary(a: AlertDetail): AlertProps {
  const { description, instruction, sender_name, message_type, status, active, source_name, country_name, infos, ...props } = a;
  return props;
}

/** At most `n` characters, counting characters rather than UTF-16 units. */
const truncate = (s: string, n: number) => {
  const cs = Array.from(s);
  return cs.length <= n ? s : cs.slice(0, n).join('');
};

/** The vector tiles' fields, and their types for TileJSON. */
const TILE_FIELDS = {
  id: 'String', event: 'String', severity: 'String', level: 'Number', category: 'String', headline: 'String',
  expires: 'Number', expires_assumed: 'Boolean', source: 'String', country: 'String', geom_source: 'String',
};

function tileFeature(a: MockAlert): MvtFeature {
  const p = a.props;
  const end = p.ends ?? p.expires;
  const properties: Record<keyof typeof TILE_FIELDS, string | number | boolean | undefined> = {
    id: p.id,
    event: p.event,
    severity: p.severity,
    level: level(p.severity),
    category: p.category,
    headline: truncate(p.headline ?? p.event, 120),
    expires: end ? Math.floor(Date.parse(end) / 1000) : undefined,
    expires_assumed: !!p.expires_assumed,
    source: p.source,
    country: p.country ?? undefined,
    geom_source: p.geom_source,
  };
  return { polygons: a.polygons, properties };
}

// --------------------------------------------------------------- Alerts

export class Alerts {
  private alerts: MockAlert[];
  private live: MockAlert | null = null;
  /** Ended or replaced alerts, kept a day for `detail`. */
  private ended = new Map<string, { props: AlertDetail; at: number }>();
  private rev: number;
  private updates = 0;
  private base: number;
  private clock: number;
  private painted: MvtFeature[] | null = null;
  private tiles = new Map<string, Buffer>();

  constructor(now: number) {
    this.base = now;
    this.clock = now;
    // At least the time of the change in milliseconds, as the server's are,
    // so a restarted mock never goes back to a revision a browser holds.
    this.rev = Math.floor(now);
    this.alerts = [...TEMPLATES.map((t) => buildNws(t, now)), ...WORLD.map((t) => buildWorld(t, now))];
  }

  /** Rises whenever the drawn set changes; never goes back. */
  get revision(): number {
    return this.rev;
  }

  get liveId(): string {
    return alertId(LIVE_TEMPLATE.n);
  }

  private active(): MockAlert[] {
    return this.live ? [...this.alerts, this.live] : this.alerts;
  }

  private hasNuts(): boolean {
    return this.active().some((a) => a.props.geom_source === 'nuts' && a.polygons.length > 0);
  }

  private changed(now?: number) {
    if (now !== undefined && now > this.clock) this.clock = now;
    this.rev = Math.max(this.rev + 1, Math.floor(this.clock));
    this.painted = null;
    this.tiles.clear();
    for (const [id, e] of this.ended) if (this.clock - e.at > DAY) this.ended.delete(id);
  }

  private end(a: MockAlert) {
    this.ended.set(a.props.id, { props: { ...a.props, active: false }, at: this.clock });
  }

  list(q: { bbox: LonLatBox | null; country: string | null; unshaped: boolean; limit: number }): AlertCollection {
    const boxes = q.bbox ? splitBox(q.bbox) : null;
    const country = q.country?.toLowerCase() ?? null;
    const matched = this.active()
      .filter((a) => {
        if (country && a.props.country !== country) return false;
        if (a.polygons.length === 0) {
          if (!q.unshaped) return false;
          if (!boxes) return true;
          const anchor = a.props.anchor;
          return !!anchor && boxes.some((b) => anchor[0] >= b.west && anchor[0] <= b.east && anchor[1] >= b.south && anchor[1] <= b.north);
        }
        return !boxes || a.boxes.some((ab) => boxes.some((b) => meets(ab, b)));
      })
      .sort(listOrder);
    const limit = Math.max(0, Math.floor(q.limit));
    return {
      type: 'FeatureCollection',
      revision: this.rev,
      truncated: matched.length > limit,
      features: matched.slice(0, limit).map((a): AlertFeature => ({ type: 'Feature', id: a.props.id, geometry: a.display, properties: summary(a.props) })),
    };
  }

  /** Alerts whose full shape covers the point, most severe first. */
  at(lon: number, lat: number): AlertProps[] {
    const x = wrapLon(lon);
    return this.active()
      .filter((a) => covers(a.polygons, x, lat))
      .sort(listOrder)
      .map((a) => summary(a.props));
  }

  detail(id: string): AlertDetail | undefined {
    const props = this.active().find((x) => x.props.id === id)?.props ?? this.ended.get(id)?.props;
    return props && { ...props };
  }

  tileJson(): AlertTileJson {
    return {
      tilejson: '3.0.0',
      name: 'alerts',
      revision: this.rev,
      tiles: [`/api/alerts/tiles/${this.rev}/{z}/{x}/{y}.mvt`],
      minzoom: 0,
      maxzoom: TILE_DETAIL_ZOOM,
      ...(this.hasNuts() ? { attribution: EUROGEOGRAPHICS } : {}),
      vector_layers: [{ id: 'alerts', minzoom: 0, maxzoom: TILE_DETAIL_ZOOM, fields: { ...TILE_FIELDS } }],
    };
  }

  /** An older revision gets the current shapes, not cacheable; a newer one,
   *  or an address off the zoom's grid, is a 404. */
  tile(revision: number, z: number, x: number, y: number): { status: 200; body: Buffer; immutable: boolean } | { status: 404 } {
    if (!Number.isInteger(revision) || revision > this.rev) return { status: 404 };
    if (!Number.isInteger(z) || z < 0 || z > TILE_MAX_ZOOM) return { status: 404 };
    const n = 2 ** z;
    if (!Number.isInteger(x) || !Number.isInteger(y) || x < 0 || y < 0 || x >= n || y >= n) return { status: 404 };
    const key = `${z}/${x}/${y}`;
    let body = this.tiles.get(key);
    if (!body) {
      this.painted ??= this.active()
        .filter((a) => a.polygons.length > 0)
        .sort(paintOrder)
        .map(tileFeature);
      const bytes = encodeTile('alerts', this.painted, z, x, y);
      body = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      // The mock is long-lived; a pan at zoom 12 should not grow it for ever.
      if (this.tiles.size >= 4000) this.tiles.clear();
      this.tiles.set(key, body);
    }
    return { status: 200, body, immutable: revision === this.rev };
  }

  /** `/api/alerts/sources`: the world path's health, failing sources first. */
  sources(): unknown {
    const now = this.clock;
    const counts = new Map<string, { active: number; drawn: number }>();
    for (const a of this.active()) {
      if (a.props.source === 'us-nws') continue;
      const c = counts.get(a.props.source) ?? { active: 0, drawn: 0 };
      c.active++;
      if (a.polygons.length > 0) c.drawn++;
      counts.set(a.props.source, c);
    }
    const stateRank: Record<string, number> = { failing: 0, starting: 1, healthy: 2, empty: 3 };
    const rows = [
      ...WORLD_SOURCES.map((s, i) => {
        const c = counts.get(s.id) ?? { active: 0, drawn: 0 };
        const polled = iso(now - (20 + ((i * 37) % 200)) * 1000);
        return {
          ...s,
          state: c.active > 0 ? 'healthy' : 'empty',
          items: c.active,
          backlog: 0,
          active_alerts: c.active,
          drawn_alerts: c.drawn,
          last_ok: polled,
          last_attempt: polled,
          last_error: null as string | null,
          consecutive_failures: 0,
        };
      }),
      ...QUIET_SOURCES.map(({ state, error, ...s }, i) => {
        const polled = iso(now - (40 + i * 23) * 1000);
        return {
          ...s,
          state,
          items: 0,
          backlog: state === 'starting' ? 3 : 0,
          active_alerts: 0,
          drawn_alerts: 0,
          last_ok: state === 'empty' ? polled : state === 'failing' ? iso(now - 3 * HOUR) : null,
          last_attempt: state === 'starting' ? null : polled,
          last_error: error ?? null,
          consecutive_failures: state === 'failing' ? 3 + i : 0,
        };
      }),
    ].sort((a, b) => stateRank[a.state] - stateRank[b.state] || a.name.localeCompare(b.name));
    const tally = (st: string) => rows.filter((r) => r.state === st).length;
    return {
      enabled: true,
      register: { loaded_from: 'live', fetched_at: iso(this.base - 2 * HOUR), last_error: null },
      summary: { sources: rows.length, healthy: tally('healthy'), empty: tally('empty'), failing: tally('failing'), starting: tally('starting') },
      attributions: this.hasNuts() ? [EUROGEOGRAPHICS] : [],
      sources: rows,
    };
  }

  /** Withdraw the live warning; null when it was not showing. */
  clearLive(): { removed: string[]; revision: number } | null {
    if (!this.live) return null;
    this.end(this.live);
    this.live = null;
    this.changed();
    return { removed: [this.liveId], revision: this.rev };
  }

  /** Issue the live warning, under the same id every time. */
  issueLive(now: number): { added: string[]; revision: number; alert: AlertDetail } {
    this.live = buildNws(LIVE_TEMPLATE, now);
    this.ended.delete(this.liveId);
    this.changed(now);
    return { added: [this.liveId], revision: this.rev, alert: { ...this.live.props } };
  }

  /** The Kenyan alert, which has no shape, ends or comes back: the list
   *  changes, the map does not, so the event names no ids. */
  toggleUnshaped(now: number): { revision: number; active: boolean } {
    const i = this.alerts.findIndex((a) => a.thread === UNSHAPED_SEED);
    if (i >= 0) {
      this.end(this.alerts[i]);
      this.alerts.splice(i, 1);
    } else {
      this.ended.delete(capId(UNSHAPED_SEED));
      this.alerts.push(buildWorld(WORLD.find((w) => w.n === UNSHAPED_SEED)!, this.base));
    }
    this.changed(now);
    return { revision: this.rev, active: i < 0 };
  }

  /** A CAP Update to the German alert: a new message with a new id replaces
   *  it, and the old one stays readable in `detail` as inactive. */
  updateOne(now: number): { added: string[]; removed: string[]; revision: number } {
    const i = this.alerts.findIndex((a) => a.thread === UPDATED_SEED);
    const t = WORLD.find((w) => w.n === UPDATED_SEED);
    if (i < 0 || !t) return { added: [], removed: [], revision: this.rev };
    const old = this.alerts[i];
    if (now > this.clock) this.clock = now;
    this.updates++;
    const oldExpires = old.props.expires ? Date.parse(old.props.expires) : now;
    const next = buildWorld(t, this.base, { id: capId(900_000 + this.updates), sent: now, expires: Math.max(oldExpires, now + 12 * HOUR) });
    this.end(old);
    this.alerts[i] = next;
    this.changed(now);
    return { added: [next.props.id], removed: [old.props.id], revision: this.rev };
  }
}
