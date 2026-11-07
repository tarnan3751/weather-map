#!/usr/bin/env node
// Startup: time from navigation to the first weather tile drawn, in headless
// Chromium against the mock, with the requests made until then (how many,
// how many bytes, and in what order: config, sources, layers, a timeline,
// basemap, tiles). Each run is a fresh browser context: no cache, no saved
// view, so the first view is guessed from the time zone (America/Chicago:
// the lower 48).
//
//   node scripts/perf/startup.mjs                 # 5 runs against the production build
//   node scripts/perf/startup.mjs --runs 9 --mode dev
//   node scripts/perf/startup.mjs --latency 40    # the mock delays every answer 20-60 ms
//   node scripts/perf/startup.mjs --verbose       # print one run's request sequence (--code: with scripts)
//   node scripts/perf/startup.mjs --json FILE

import { writeFileSync } from 'node:fs';
import { flag, kb, launch, median, ms, opt, startApp } from './lib.mjs';

const runs = Number(opt('--runs', 5));
const latency = opt('--latency', '0');
const app = await startApp({ env: { MOCK_LATENCY: latency } });
const browser = await launch();

/** What a request is for, from its path. */
function kind(u) {
  const p = new URL(u).pathname;
  if (p === '/' || p.endsWith('.html')) return 'document';
  if (/^\/(assets|src|node_modules|@vite|@react-refresh|@id|@fs)\//.test(p) || /\.(m?js|tsx?|css)$/.test(p)) return 'code';
  if (p.startsWith('/api/frames/')) return `timeline ${p.slice('/api/frames/'.length)}`;
  if (p.startsWith('/api/alerts/tiles/')) return 'alert tiles';
  if (p.startsWith('/api/')) return p.slice(1);
  if (p.startsWith('/tiles/')) return 'weather tile';
  if (p.startsWith('/basemap/')) return 'basemap archive';
  if (p.startsWith('/basemap-assets/')) return 'basemap fonts/sprites';
  if (p.endsWith('.wasm')) return 'wasm';
  if (p.startsWith('/ws')) return 'websocket';
  return p;
}

async function once() {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Chicago', locale: 'en-US' });
  // The moment the weather layer first draws a cell from a tile, and the
  // moment every tile of the first view is in.
  await ctx.addInitScript(() => {
    const marks = (window.__startup = { longTasks: [] });
    // Main-thread tasks over 50 ms: where the page could not respond.
    new PerformanceObserver((l) => {
      for (const e of l.getEntries()) marks.longTasks.push([Math.round(e.startTime), Math.round(e.duration)]);
    }).observe({ type: 'longtask', buffered: true });
    const tick = () => {
      const m = window.__wx?.map;
      const st = m?.layer?.stats;
      if (st && marks.firstTile === undefined && st.exact + st.ancestor > 0) marks.firstTile = performance.now();
      const d = m?.layer?.getDisplay();
      if (marks.firstTile !== undefined && d?.a && m.layer.isReady(d.a) && st.missing === 0) {
        marks.viewReady = performance.now();
        marks.viewUrls = m.layer.urlsInView(d.a).length;
        return;
      }
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  const page = await ctx.newPage();
  const reqs = [];
  const t0 = Date.now();
  ctx.on('request', (r) => reqs.push({ r, url: r.url(), kind: kind(r.url()), start: Date.now() - t0, end: null, bytes: 0 }));
  const done = async (r) => {
    const e = reqs.find((x) => x.r === r);
    if (!e) return;
    e.end = Date.now() - t0;
    try {
      const s = await r.sizes();
      e.bytes = s.responseBodySize + s.responseHeadersSize;
    } catch {
      // Gone with its page.
    }
  };
  ctx.on('requestfinished', done);
  ctx.on('requestfailed', done);
  await page.goto(`${app.base}/?debug`, { waitUntil: 'commit' });
  await page.waitForFunction(() => window.__startup?.viewReady !== undefined, null, { timeout: 60_000, polling: 100 });
  const marks = await page.evaluate(() => ({ ...window.__startup, origin: performance.timeOrigin }));
  // Request times are taken from Node's clock; the marks from the page's.
  const skew = marks.origin - t0;
  const firstTile = marks.firstTile + skew;
  const viewReady = marks.viewReady + skew;
  await page.waitForTimeout(200);
  const before = (t) => reqs.filter((x) => x.start <= t);
  const firstOf = (k) => reqs.find((x) => x.kind === k || x.kind.startsWith(k));
  const sum = (xs) => xs.reduce((n, x) => n + x.bytes, 0);
  const tilesBeforeReady = before(viewReady).filter((x) => x.kind === 'weather tile').length;
  const result = {
    firstTile,
    viewReady,
    requestsToFirstTile: before(firstTile).length,
    bytesToFirstTile: sum(before(firstTile)),
    requestsToViewReady: before(viewReady).length,
    bytesToViewReady: sum(before(viewReady)),
    tilesBeforeReady,
    viewUrls: marks.viewUrls,
    timelines: [...new Set(before(viewReady).filter((x) => x.kind.startsWith('timeline')).map((x) => x.kind))],
    milestones: Object.fromEntries(
      ['document', 'config', 'api/layers', 'api/sources', 'api/alerts/tiles.json', 'timeline', 'basemap archive', 'basemap fonts/sprites', 'weather tile', 'wasm'].map((k) => {
        const e = k === 'config' ? firstOf('api/config') : firstOf(k);
        return [k, e ? { start: e.start, end: e.end } : null];
      }),
    ),
    sequence: reqs
      .filter((x) => x.start <= viewReady && (flag('--code') || x.kind !== 'code'))
      .map((x) => `${String(x.start).padStart(5)}-${String(x.end ?? '?').padStart(5)} ms  ${x.kind.padEnd(24)} ${new URL(x.url).pathname.slice(0, 80)}`),
    longTasks: marks.longTasks.filter(([t]) => t + skew <= viewReady).map(([t, d]) => [Math.round(t + skew), d]),
    codeRequests: before(viewReady).filter((x) => x.kind === 'code').length,
    codeBytes: sum(before(viewReady).filter((x) => x.kind === 'code')),
  };
  await ctx.close();
  return result;
}

// The first page load warms the mock's world and the browser's caches of
// compiled code; it is not counted.
for (let i = 0; i < Number(opt('--warmup', 1)); i++) await once();
const results = [];
for (let i = 0; i < runs; i++) {
  const r = await once();
  results.push(r);
  console.log(`run ${i + 1}: first tile ${ms(r.firstTile)}, view complete ${ms(r.viewReady)}, ${r.requestsToViewReady} requests, ${kb(r.bytesToViewReady)}`);
}
const med = (f) => median(results.map(f));
console.log(`\nmedian of ${runs} (${app.mode ?? 'external'} build, mock latency ${latency} ms):`);
console.log(`  first weather tile drawn   ${ms(med((r) => r.firstTile))}  (${med((r) => r.requestsToFirstTile)} requests, ${kb(med((r) => r.bytesToFirstTile))})`);
console.log(`  first view complete        ${ms(med((r) => r.viewReady))}  (${med((r) => r.requestsToViewReady)} requests, ${kb(med((r) => r.bytesToViewReady))}; code ${med((r) => r.codeRequests)} requests, ${kb(med((r) => r.codeBytes))})`);
console.log(`  weather tiles asked for before the view was complete: ${med((r) => r.tilesBeforeReady)} (the view needs ${med((r) => r.viewUrls)})`);
console.log(`  timelines fetched: ${results[0].timelines.join(', ')}`);
console.log('  first request of each kind (start-end, median):');
for (const k of Object.keys(results[0].milestones)) {
  const xs = results.map((r) => r.milestones[k]).filter(Boolean);
  if (!xs.length) continue;
  console.log(`    ${k.padEnd(24)} ${String(median(xs.map((x) => x.start))).padStart(5)} - ${String(median(xs.map((x) => x.end ?? NaN))).padStart(5)} ms`);
}
if (flag('--verbose')) console.log(`\nlong main-thread tasks of the last run (start ms, duration ms): ${JSON.stringify(results.at(-1).longTasks)}`);
if (flag('--verbose')) console.log(`\nsequence of the last run (non-code requests until the view was complete):\n${results.at(-1).sequence.join('\n')}`);
const json = opt('--json');
if (json) writeFileSync(json, JSON.stringify(results.map(({ sequence, ...r }) => r), null, 2));
await browser.close();
await app.close();
