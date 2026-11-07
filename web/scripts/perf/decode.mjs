#!/usr/bin/env node
// WXT1 decoding: per-tile cost of each decoder on real tiles, and what a load
// costs end to end in the browser.
//
// 1. In the browser (after a pass that warms the mock), a world view and a city view of several layers are
//    loaded with each decoder (`?wasm=off` for TypeScript), timing every load
//    from the tile store's request to its decoded samples arriving on the
//    main thread (fetch, decode in a worker, transfer back). The tiles' URLs
//    are kept.
// 2. In Node, those tiles' bytes are decoded again and again by the
//    WebAssembly decoder and by the TypeScript one (with the platform's
//    DecompressionStream, and with fflate as on old Safari), each followed by
//    the channel interleave the worker does; also the way both were called
//    before, with an extra copy, for comparison.
//
//   node scripts/perf/decode.mjs
//   node scripts/perf/decode.mjs --reps 30 --json FILE

import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { launch, median, opt, pct, startApp, waitWeather, webDir } from './lib.mjs';

const reps = Number(opt('--reps', 20));
const app = await startApp();
const browser = await launch();

const VIEWS = [
  { layer: 'radar', center: [-40, 30], zoom: 1.5 },
  { layer: 'radar', center: [-94.58, 39.1], zoom: 7 },
  { layer: 'temp', center: [10, 50], zoom: 4 },
  { layer: 'wind', center: [-94.58, 39.1], zoom: 5 },
];

async function browserLoads(wasm) {
  const ctx = await browser.newContext({ viewport: { width: 1024, height: 640 }, timezoneId: 'America/Chicago' });
  const page = await ctx.newPage();
  await page.goto(`${app.base}/?debug${wasm ? '' : '&wasm=off'}`);
  await waitWeather(page);
  await page.waitForFunction(() => window.__wx.decoder() !== null);
  const decoder = await page.evaluate(() => window.__wx.decoder());
  await page.evaluate(() => {
    const tiles = window.__wx.map.tiles;
    const orig = tiles.fetcher;
    window.__loads = [];
    tiles.fetcher = (url, pick, signal) => {
      const t = performance.now();
      return orig(url, pick, signal).then((r) => {
        if (r.status === 'ok') window.__loads.push({ url, ms: performance.now() - t, bytes: r.data.byteLength, channels: r.channels });
        return r;
      });
    };
  });
  for (const v of VIEWS) {
    await page.evaluate((v) => {
      window.__wx.store.set({ layerId: v.layer, following: true });
      window.__wx.map.map.jumpTo({ center: v.center, zoom: v.zoom });
    }, v);
    await page.waitForTimeout(300);
    await waitWeather(page);
    await page.waitForFunction(() => window.__wx.map.tiles.inFlight === 0, null, { timeout: 30_000 }).catch(() => {});
  }
  const loads = await page.evaluate(() => window.__loads);
  await ctx.close();
  return { decoder, loads };
}

const results = {};
// A first pass warms the mock, which renders each tile the first time it is
// asked for: the passes after it measure the client, not the mock.
await browserLoads(false);
for (const wasm of [true, false]) {
  const r = await browserLoads(wasm);
  const ms = r.loads.map((l) => l.ms);
  results[r.decoder] = { loads: r.loads.length, median: median(ms), p90: pct(ms, 90) };
  console.log(`browser, ${r.decoder} decoder: ${r.loads.length} tiles, load to samples median ${median(ms).toFixed(1)} ms, p90 ${pct(ms, 90).toFixed(1)} ms`);
  results.urls ??= [...new Set(r.loads.map((l) => l.url))];
}

// Node: the same tiles, decoded here.
const { decodeWxt, inflateJs, inflateZlib, interleave, parseHeader } = await import(join(webDir, 'src/tiles/wxt.ts'));
const { WasmDecoder } = await import(join(webDir, 'src/tiles/wasmDecoder.ts'));
const wasm = await WasmDecoder.fromBytes(readFileSync(join(webDir, 'public/wasm/wx_tile_wasm.wasm')));
const tiles = [];
for (const u of results.urls.slice(0, 120)) {
  const res = await fetch(new URL(u, app.base));
  if (res.status !== 200) continue;
  const bytes = new Uint8Array(await res.arrayBuffer());
  const h = parseHeader(bytes);
  tiles.push({ bytes, pick: Array.from({ length: Math.min(h.bands, 3) }, (_, i) => i) });
}
/** The interleave as it was before it handed single bands over and padded
 *  three to four: a copy, always. */
function copyInterleave(tile, pick) {
  const n = tile.width * tile.height;
  const c = pick.length;
  const out = new Uint16Array(n * c);
  for (let k = 0; k < c; k++) {
    const src = tile.bands[pick[k]];
    if (src) for (let i = 0; i < n; i++) out[i * c + k] = src[i];
  }
  return out;
}
const kinds = {
  'wasm, bands copied out, then interleaved': (t) => copyInterleave(wasm.decode(t.bytes), t.pick),
  'wasm, interleaved from its memory': (t) => wasm.decodePicked(t.bytes, t.pick).data,
  'typescript, always copied': async (t) => copyInterleave(await decodeWxt(t.bytes, inflateZlib), t.pick),
  'typescript, one band handed over': async (t) => interleave(await decodeWxt(t.bytes, inflateZlib), t.pick),
  'typescript with fflate': async (t) => interleave(await decodeWxt(t.bytes, inflateJs), t.pick),
};
const total = tiles.reduce((n, t) => n + t.bytes.length, 0);
console.log(`\nNode, ${tiles.length} tiles (${(total / tiles.length / 1024).toFixed(1)} KB each on average), ${reps} passes:`);
results.node = {};
for (const [name, f] of Object.entries(kinds)) {
  for (const t of tiles) await f(t);
  const per = [];
  for (let r = 0; r < reps; r++) {
    const t0 = performance.now();
    for (const t of tiles) await f(t);
    per.push(((performance.now() - t0) / tiles.length) * 1000);
  }
  results.node[name] = median(per);
  console.log(`  ${name.padEnd(42)} ${median(per).toFixed(0).padStart(6)} µs per tile (median of passes)`);
}

const json = opt('--json');
if (json) writeFileSync(json, JSON.stringify(results, null, 2));
await browser.close();
await app.close();
