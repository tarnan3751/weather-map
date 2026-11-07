// The status page: every feed's health and when it last delivered, kept
// fresh over the WebSocket. Plain DOM, no map and no framework, so it loads
// fast on the bad day it is needed.

import '../styles/status.css';
import { api } from '../api/http.ts';
import { LiveSocket, type LiveState } from '../api/live.ts';
import type { FeedHealth, LayerSpec, SourceDef, SourceStatus, StatusReply } from '../api/types.ts';
import { aboutKm, mediumWords, relative } from '../app/describe.ts';
import { parseTime } from '../core/timeline.ts';

type Feed = FeedHealth & { stale: boolean };

const FEED_INFO: Record<string, string> = {
  mrms: 'Radar mosaics and rainfall estimates (MRMS), every 2 minutes',
  rtma: 'Hourly surface analyses (RTMA)',
  hrrr: 'HRRR model, hourly runs to 18–48 hours',
  gfs: 'GFS model, four runs a day out to day 7',
  alerts: 'National Weather Service alerts',
  stations: 'Airport weather reports (METAR)',
};

let status: StatusReply | null = null;
let layers: LayerSpec[] = [];
let sources: SourceDef[] = [];
let clockOffset = 0;
let live: LiveState = 'connecting';
let loadError: string | null = null;
const root = document.getElementById('status')!;

const now = () => Date.now() + clockOffset;

function h(tag: string, attrs: Record<string, string | undefined> = {}, ...children: (Node | string | null | undefined | false)[]): HTMLElement {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) if (v !== undefined) el.setAttribute(k, v);
  for (const c of children) if (c !== null && c !== undefined && c !== false) el.append(c);
  return el;
}

/** A coloured dot and its words: a feed's or a source's state, the connection's. */
const dotted = (cls: string, text: string) => h('span', { class: cls }, h('span', { class: 'dot', 'aria-hidden': 'true' }), text);
const tableHead = (...cols: string[]) => h('thead', {}, h('tr', {}, ...cols.map((c) => h('th', { scope: 'col' }, c))));

const fmt = new Intl.DateTimeFormat(undefined, { weekday: 'short', hour: 'numeric', minute: '2-digit', second: '2-digit' });

function when(iso: string | null, empty = 'never'): HTMLElement {
  if (!iso) return h('span', { class: 'muted' }, empty);
  const t = Date.parse(iso);
  return h('time', { datetime: iso, title: fmt.format(t) }, relative(t, now()));
}

/** Same rule as `FeedHealth::is_stale`: quiet for three expected intervals. */
function isStale(f: FeedHealth): boolean {
  const ref = f.last_success ?? f.started_at;
  if (!ref) return false;
  return now() - Date.parse(ref) > Math.max(30, f.expected_interval_secs) * 3 * 1000;
}

function stateChip(f: Feed) {
  const stale = f.stale || isStale(f);
  const [cls, text] =
    f.state === 'backing_off'
      ? ['bad', 'Backing off']
      : f.state === 'stopped'
        ? ['off', 'Stopped']
        : stale
          ? ['warn', 'Stale']
          : f.state === 'starting'
            ? ['info', 'Starting']
            : ['ok', 'Healthy'];
  return dotted(`chip ${cls}`, text);
}

/** A feed named after a source the registry knows: who publishes it. */
function sourceInfo(name: string): string {
  const d = sources.find((x) => x.id === name);
  return d ? `${d.title} from ${d.provider}, ${mediumWords(d)}${aboutKm(d.res_km) ? ` at about ${aboutKm(d.res_km)}` : ''}` : '';
}

function feedCard(f: Feed): HTMLElement {
  const retry = f.next_retry_at ? Date.parse(f.next_retry_at) : null;
  return h(
    'article',
    { class: `feed ${f.state}`, 'aria-labelledby': `feed-${f.name}` },
    h('header', {}, h('h2', { id: `feed-${f.name}` }, f.name.toUpperCase()), stateChip(f)),
    h('p', { class: 'feed-info' }, FEED_INFO[f.name] ?? sourceInfo(f.name)),
    h(
      'dl',
      {},
      h('dt', {}, 'Newest data'),
      h('dd', {}, when(f.last_data_time, 'none yet')),
      h('dt', {}, 'Last success'),
      h('dd', {}, when(f.last_success)),
      h('dt', {}, 'Expected every'),
      h('dd', {}, f.expected_interval_secs >= 3600 ? `${Math.round(f.expected_interval_secs / 3600)} h` : f.expected_interval_secs >= 60 ? `${Math.round(f.expected_interval_secs / 60)} min` : `${f.expected_interval_secs} s`),
      f.restarts > 0 && h('dt', {}, 'Restarts'),
      f.restarts > 0 && h('dd', {}, String(f.restarts)),
    ),
    f.detail && h('p', { class: 'feed-detail' }, f.detail),
    f.last_error &&
      h(
        'div',
        { class: `feed-error ${f.state === 'backing_off' ? 'active' : ''}` },
        h('strong', {}, f.state === 'backing_off' ? 'Failing: ' : 'Last error: '),
        f.last_error,
        h('span', { class: 'muted' }, ' · ', when(f.last_error_at)),
        f.consecutive_failures > 0 && h('span', { class: 'muted' }, ` · ${f.consecutive_failures} in a row`),
        retry !== null && retry > now() && h('span', { class: 'muted' }, ` · retrying ${relative(retry, now())}`),
      ),
  );
}

function render() {
  const s = status;
  const title = h('div', { class: 'status-title' }, h('h1', {}, 'Data feeds'), h('a', { href: '/', class: 'back' }, '← Back to the map'));
  const meta = h(
    'p',
    { class: 'status-meta' },
    dotted(`live ${live === 'open' ? 'ok' : 'warn'}`, live === 'open' ? 'Live' : 'Reconnecting…'),
    s ? ` · Server time ${fmt.format(Date.parse(s.server_time) + (Date.now() - fetchedAt))}` : '',
    s ? ` · Up for ${relative(Date.parse(s.started_at), now()).replace(' ago', '').replace('now', 'a minute')}` : '',
    s ? ` · ${s.clients} ${s.clients === 1 ? 'viewer' : 'viewers'} connected` : '',
  );
  const nodes: Node[] = [title, meta];
  if (loadError) nodes.push(h('div', { class: 'banner', role: 'alert' }, loadError));
  if (s) {
    const unhealthy = s.feeds.filter((f) => f.state === 'backing_off' || f.stale || isStale(f));
    nodes.push(
      h(
        'p',
        { class: `summary ${unhealthy.length ? 'warn' : 'ok'}` },
        unhealthy.length ? `${unhealthy.length} of ${s.feeds.length} feeds need attention.` : `All ${s.feeds.length} feeds are healthy.`,
      ),
    );
    nodes.push(h('section', { class: 'feeds', 'aria-label': 'Feeds' }, ...s.feeds.map(feedCard)));
    const title = (id: string) => layers.find((l) => l.id === id)?.title ?? id;
    nodes.push(
      h(
        'section',
        { class: 'layers', 'aria-labelledby': 'layers-h' },
        h('h2', { id: 'layers-h' }, 'Layers'),
        h(
          'table',
          {},
          tableHead('Layer', 'Frames', 'Oldest', 'Newest'),
          h(
            'tbody',
            {},
            ...s.layers.map((l) =>
              h('tr', {}, h('th', { scope: 'row' }, title(l.id)), h('td', {}, String(l.frames)), h('td', {}, when(l.oldest_valid, '—')), h('td', {}, when(l.newest_valid, '—'))),
            ),
          ),
        ),
      ),
    );
    if (s.sources?.length) nodes.push(sourcesTable(s.sources));
  } else if (!loadError) {
    nodes.push(h('p', { class: 'muted' }, 'Loading…'));
  }
  root.replaceChildren(...nodes);
}

/** Observations this old, or model runs this old, mean a source has stopped. */
const OBS_BEHIND_MS = 3 * 3600_000;
const RUN_BEHIND_MS = 30 * 3600_000;

/** Every source in the registry: what it holds and how new it is, with the
 *  silent and lagging ones marked so they stand out among dozens. */
function sourcesTable(rows: SourceStatus[]): HTMLElement {
  const state = (r: SourceStatus): ['ok' | 'warn' | 'off', string] => {
    if (!r.parts) return ['off', 'Nothing held'];
    const kind = sources.find((d) => d.id === r.id)?.kind;
    const newest = Date.parse((kind === 'forecast' ? r.newest_run : r.newest_valid) ?? r.newest_valid ?? '');
    if (Number.isFinite(newest) && now() - newest > (kind === 'forecast' ? RUN_BEHIND_MS : OBS_BEHIND_MS)) return ['warn', 'Behind'];
    return ['ok', 'Delivering'];
  };
  const delivering = rows.filter((r) => state(r)[0] === 'ok').length;
  return h(
    'section',
    { class: 'layers sources', 'aria-labelledby': 'sources-h' },
    h('h2', { id: 'sources-h' }, 'Sources'),
    h('p', { class: 'muted' }, `${delivering} of ${rows.length} sources delivering.`),
    h(
      'table',
      {},
      tableHead('Source', 'Kind', 'State', 'Parts', 'Layers', 'Newest data', 'Newest run'),
      h(
        'tbody',
        {},
        ...rows.map((r) => {
          const [cls, text] = state(r);
          return h(
            'tr',
            { class: `source-${cls}` },
            h('th', { scope: 'row' }, r.title, h('span', { class: 'muted' }, ` · ${r.provider}`)),
            h('td', {}, sources.find((d) => d.id === r.id)?.medium ?? ''),
            h('td', {}, dotted(`chip ${cls}`, text)),
            h('td', {}, String(r.parts)),
            h('td', {}, String(r.layers)),
            h('td', {}, when(r.newest_valid, '—')),
            h('td', {}, r.newest_run ? when(r.newest_run) : h('span', { class: 'muted' }, 'observations')),
          );
        }),
      ),
    ),
  );
}

let fetchedAt = Date.now();
async function load() {
  try {
    const [s, l, d] = await Promise.all([
      api.status(),
      layers.length ? Promise.resolve(layers) : api.layers().catch(() => []),
      sources.length ? Promise.resolve(sources) : api.sources().catch(() => []),
    ]);
    status = s;
    layers = l;
    sources = d;
    fetchedAt = Date.now();
    clockOffset = Date.parse(s.server_time) - Date.now();
    loadError = null;
  } catch {
    loadError = 'The server is not answering. Showing the last known status.';
  }
  render();
}

let pending: ReturnType<typeof setTimeout> | null = null;
function reloadSoon() {
  if (pending) return;
  pending = setTimeout(() => {
    pending = null;
    void load();
  }, 1500);
}

new LiveSocket({
  onEvent(ev) {
    if (ev.type === 'hello' || ev.type === 'heartbeat') {
      const t = parseTime(ev.server_time);
      if (Number.isFinite(t)) clockOffset = t - Date.now();
    } else if (ev.type === 'feed' && status) {
      const i = status.feeds.findIndex((f) => f.name === ev.feed.name);
      const next = { ...ev.feed, stale: false };
      next.stale = isStale(next);
      if (i >= 0) status.feeds[i] = next;
      else status.feeds.push(next);
      render();
    } else if (ev.type === 'timeline' || ev.type === 'stations' || ev.type === 'alerts') reloadSoon();
  },
  onResync: () => void load(),
  onState(s) {
    live = s;
    render();
  },
}).start();

void load();
setInterval(render, 5000);
setInterval(() => void load(), 60_000);
