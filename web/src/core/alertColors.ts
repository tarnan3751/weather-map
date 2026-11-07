// Alert colours: US alerts follow the NWS hazard map, so a Tornado Warning is
// red and a Severe Thunderstorm Warning orange as people there know them (a
// few too dark for a dark basemap are lifted); everywhere else follows
// MeteoAlarm's levels, red for the most severe down to green, as most
// national services colour their own warnings.

const ALERT_COLORS: Record<string, string> = {
  'Tornado Warning': '#ff2d2d',
  'Extreme Wind Warning': '#ff8c00',
  'Severe Thunderstorm Warning': '#ffa500',
  'Flash Flood Warning': '#e0204a',
  'Flash Flood Emergency': '#ff3fa0',
  'Tornado Watch': '#ffee33',
  'Severe Thunderstorm Watch': '#db7093',
  'Flood Warning': '#22d65e',
  'Flood Watch': '#2e8b57',
  'Flood Advisory': '#00e0a0',
  'Hurricane Warning': '#ff1744',
  'Hurricane Watch': '#ff44ff',
  'Tropical Storm Warning': '#e8473b',
  'Tropical Storm Watch': '#f08080',
  'Storm Surge Warning': '#b524f7',
  'Blizzard Warning': '#ff4500',
  'Winter Storm Warning': '#ff69b4',
  'Winter Storm Watch': '#5f9ee0',
  'Ice Storm Warning': '#c03ac0',
  'Lake Effect Snow Warning': '#14b8b8',
  'Winter Weather Advisory': '#8b7bee',
  'High Wind Warning': '#daa520',
  'High Wind Watch': '#b8860b',
  'Wind Advisory': '#d2b48c',
  'Red Flag Warning': '#ff1493',
  'Fire Weather Watch': '#ffdead',
  'Extreme Cold Warning': '#5a8ce6',
  'Cold Weather Advisory': '#afeeee',
  'Wind Chill Warning': '#b0c4de',
  'Freeze Warning': '#6a5acd',
  'Frost Advisory': '#6495ed',
  'Excessive Heat Warning': '#d6178a',
  'Extreme Heat Warning': '#d6178a',
  'Heat Advisory': '#ff7f50',
  'Dense Fog Advisory': '#8796a8',
  'Special Weather Statement': '#ffe4b5',
  'Gale Warning': '#dda0dd',
  'Small Craft Advisory': '#d8bfd8',
  'Dust Storm Warning': '#ffe4c4',
  'Air Quality Alert': '#a0a0a0',
};

const SEVERITY_RANK: Record<string, number> = { Extreme: 4, Severe: 3, Moderate: 2, Minor: 1, Unknown: 0 };

/** The vector tiles' `level`, from a severity: 4 Extreme down to 0 Unknown. */
export const severityLevel = (severity: string | null | undefined) => SEVERITY_RANK[severity ?? 'Unknown'] ?? 0;

/** MeteoAlarm's colours by level, 0 to 4. */
const LEVEL_COLORS = ['#a0a8b8', '#5fc36b', '#f2d24b', '#ff9a3c', '#ff3b3b'];

/** `source` is the alert's feed: NWS event colours apply to `us-nws` (and to
 *  an alert whose feed is not known, such as a saved place's summary). */
export function alertColor(event: string, severity?: string | null, source?: string): string {
  const nws = source === undefined || source === 'us-nws' ? ALERT_COLORS[event] : undefined;
  return nws ?? LEVEL_COLORS[severityLevel(severity)];
}

const isWarning = (event: string) => /Warning$|Emergency$/.test(event);
/** Warnings above watches above advisories and the rest. */
const kindRank = (event: string) => (isWarning(event) ? 30 : /Watch$/.test(event) ? 20 : 10);

/** Warnings above watches above advisories, then by severity: the order to
 *  draw (last on top) and to list. */
export function alertPriority(event: string, severity: string): number {
  return kindRank(event) + (SEVERITY_RANK[severity] ?? 0);
}

/** Which of the alerts under a click opens first (higher first): the most
 *  severe, by the tiles' `level` where a feature has one, and within a level
 *  warnings before watches before the rest. */
export function alertClickRank(p: { event: string; severity?: string | null; level?: number | null }): number {
  const level = typeof p.level === 'number' && Number.isFinite(p.level) ? p.level : severityLevel(p.severity);
  return level * 100 + kindRank(p.event);
}

/** MapLibre expression colouring an alert tile feature: by `event` for the
 *  NWS, by `level` for everyone else. */
export function alertColorExpression(): unknown[] {
  const level: unknown[] = ['match', ['to-number', ['get', 'level'], 0], 4, LEVEL_COLORS[4], 3, LEVEL_COLORS[3], 2, LEVEL_COLORS[2], 1, LEVEL_COLORS[1], LEVEL_COLORS[0]];
  const byEvent: unknown[] = ['match', ['get', 'event']];
  for (const [k, v] of Object.entries(ALERT_COLORS)) byEvent.push(k, v);
  byEvent.push(level);
  return ['case', ['==', ['get', 'source'], 'us-nws'], byEvent, level];
}

/** Worth interrupting someone for, by the server's notification rule: an NWS
 *  warning, or elsewhere a Severe or Extreme alert that is not already past. */
export function isSerious(a: { event: string; severity: string | null; urgency?: string | null; source?: string }): boolean {
  if (a.source === undefined || a.source === 'us-nws') return isWarning(a.event);
  return (a.severity === 'Severe' || a.severity === 'Extreme') && a.urgency !== 'Past';
}
