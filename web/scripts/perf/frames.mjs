#!/usr/bin/env node
// Frame cost of the weather layer. Scenarios: radar playing a 60-frame loop
// at a world view and at a city view (also with the pointer resting on the
// map, so the read-out follows the loop), and wind with particles at both.
//
// For each, while the map runs on its own for a few seconds (after a full
// loop, so the steady state is measured rather than the downloads):
//   - WebGL calls made inside the layers per frame: draws, uniforms, texture
//     uploads, texture binds, program switches;
//   - bytes allocated inside the layers' entry points and the app store's
//     fan-out per frame, from Chromium's sampling heap profiler (which also
//     counts objects already collected);
// then, because `performance.now()` is coarsened to 0.1 ms here, the time per
// call of the colour layer's prerender (planning, requests, uploads) and
// render, the particle step and composite, and a playback tick's store update
// (the map controller's subscription and React), each called 200 times in a
// row with the arguments the map last used.
// Software WebGL in headless Chromium is slow and the machine may be shared:
// compare runs of this script, not absolute numbers.
//
//   node scripts/perf/frames.mjs                    # 3 runs, production build
//   node scripts/perf/frames.mjs --runs 5 --seconds 8 --only radar-city
//   node scripts/perf/frames.mjs --json FILE
//   node scripts/perf/frames.mjs --profile 20      # also list the 20 functions with the most self time

import { writeFileSync } from 'node:fs';
import { jumpTo, launch, median, opt, startApp, waitWeather } from './lib.mjs';

const runs = Number(opt('--runs', 3));
const seconds = Number(opt('--seconds', 5));
const only = opt('--only')?.split(',');
const profileTop = Number(opt('--profile', 0));

const KC = [-94.5786, 39.0997];
const SCENARIOS = [
  { name: 'radar-world', layer: 'radar', center: [-40, 30], zoom: 1.5, play: true },
  { name: 'radar-city', layer: 'radar', center: KC, zoom: 9, play: true },
  { name: 'radar-city-hover', layer: 'radar', center: KC, zoom: 9, play: true, hover: true },
  { name: 'wind-world', layer: 'wind', center: [-40, 30], zoom: 1.5, particles: true },
  { name: 'wind-city', layer: 'wind', center: KC, zoom: 9, particles: true },
].filter((s) => !only || only.includes(s.name));

const app = await startApp();
const browser = await launch();

/** Wrap the layers' entry points, the store, and the GL calls made inside the layers. */
function instrument() {
  const wx = window.__wx;
  const L = wx.map.layer;
  const P = wx.map.particles;
  const gl = wx.map.map.painter.context.gl;
  const perf = (window.__perf = { frames: [], cur: null, inside: 0, on: false, last: {}, orig: {} });
  const blank = () => ({ draws: 0, uniforms: 0, uploads: 0, binds: 0, programs: 0 });
  const wrap = (obj, name, slot, counts) => {
    const orig = obj[name];
    perf.orig[slot] = (...args) => orig.apply(obj, args);
    const body = (self, args) => {
      // A colour-layer prerender starts the map's frame.
      if (slot === 'layer_prerender' && perf.on) perf.frames.push((perf.cur = blank()));
      perf.last[slot] = args;
      if (counts) perf.inside++;
      try {
        return orig.apply(self, args);
      } finally {
        if (counts) perf.inside--;
      }
    };
    // A named function, so the heap profile's stacks show whose it is.
    obj[name] = new Function('body', `return function __perf_${slot}(...args) { return body(this, args); }`)(body);
  };
  wrap(L, 'prerender', 'layer_prerender', true);
  wrap(L, 'render', 'layer_render', true);
  wrap(P, 'prerender', 'particles_step', true);
  wrap(P, 'render', 'particles_render', true);
  wrap(wx.store, 'set', 'store_set', false);
  const count = (name, slot) => {
    const orig = gl[name].bind(gl);
    gl[name] = (...args) => {
      if (perf.inside && perf.cur && perf.on) perf.cur[slot]++;
      return orig(...args);
    };
  };
  for (const k of ['drawArrays', 'drawElements']) count(k, 'draws');
  for (const k of ['texImage2D', 'texSubImage2D', 'texStorage2D']) count(k, 'uploads');
  count('bindTexture', 'binds');
  count('useProgram', 'programs');
  for (const k of Object.getOwnPropertyNames(WebGL2RenderingContext.prototype)) if (/^uniform(\d|Matrix)/.test(k)) count(k, 'uniforms');
}

/** Microseconds per call of each entry point, called `k` times in a row. */
function bench(k) {
  const wx = window.__wx;
  const perf = window.__perf;
  const time = (f) => {
    const t = performance.now();
    for (let i = 0; i < k; i++) f(i);
    return ((performance.now() - t) / k) * 1000;
  };
  const out = {};
  const call = (slot) => {
    if (perf.last[slot]) out[slot] = time(() => perf.orig[slot](...perf.last[slot]));
  };
  call('layer_prerender');
  call('layer_render');
  if (wx.map.particles.stats.running) {
    call('particles_step');
    call('particles_render');
  }
  // A playback tick: the playhead moves a little within the same frame.
  const s = wx.store.get();
  if (s.playing && s.playhead !== null) {
    const p = s.playhead;
    out.store_tick = time((i) => wx.store.set({ playhead: p + (i % 2) * 1e-6 }));
  }
  // MapLibre caches GL state; calls made outside its render pass changed it.
  wx.map.map.painter.context.setDirty();
  wx.map.map.triggerRepaint();
  return out;
}

/** Bytes allocated inside the wrapped functions, by wrapper, from the
 *  profile's samples. */
function allocations(profile) {
  const owner = new Map();
  const walk = (node, under) => {
    const name = node.callFrame.functionName;
    const here = name.startsWith('__perf_') ? name.slice(7) : under;
    owner.set(node.id, here);
    for (const c of node.children) walk(c, here);
  };
  walk(profile.head, null);
  const out = {};
  for (const s of profile.samples) {
    const k = owner.get(s.nodeId);
    if (k) out[k] = (out[k] ?? 0) + s.size;
  }
  return out;
}

/** The functions with the most self time in a CPU profile. */
function printProfile(name, p, n) {
  const self = new Map();
  const dt = new Map();
  p.samples.forEach((id, i) => dt.set(id, (dt.get(id) ?? 0) + (p.timeDeltas[i] ?? 0)));
  let total = 0;
  for (const node of p.nodes) {
    const t = (dt.get(node.id) ?? 0) / 1000;
    total += t;
    const f = node.callFrame;
    const k = `${f.functionName || '(anonymous)'} ${f.url.split('/').pop()}:${f.lineNumber + 1}`;
    self.set(k, (self.get(k) ?? 0) + t);
  }
  console.log(`  ${name}: CPU profile, ${total.toFixed(0)} ms sampled; most self time:`);
  for (const [k, t] of [...self].sort((a, b) => b[1] - a[1]).slice(0, n)) console.log(`    ${t.toFixed(1).padStart(8)} ms  ${k}`);
}

/** Until nothing is loading or waiting to be uploaded (or `ms` pass). */
async function settle(page, ms) {
  await page
    .waitForFunction(
      () => {
        const t = window.__wx.map.tiles;
        return t.inFlight === 0 && t.pendingUploads().next().done;
      },
      null,
      { timeout: ms, polling: 250 },
    )
    .catch(() => {});
}

async function scenario(page, cdp, s) {
  await page.evaluate((s) => {
    const wx = window.__wx;
    wx.playback.pause();
    wx.store.set({ layerId: s.layer, windMarks: s.particles ? 'particles' : 'arrows', following: true, readout: null });
  }, s);
  await jumpTo(page, s.center, s.zoom);
  await page.mouse.move(s.hover ? 512 : 5, s.hover ? 320 : 5);
  if (s.play) {
    // The 60 frames before now (or as many as there are), played once
    // through first so their tiles are loaded and uploaded.
    const loopS = await page.evaluate(() => {
      const wx = window.__wx;
      wx.playback.play();
      const hi = wx.playback.loop[1];
      wx.playback.loop = [Math.max(0, hi - 59), hi];
      return (wx.playback.loop[1] - wx.playback.loop[0]) / 5 + 1.5;
    });
    await page.waitForTimeout(loopS * 1000);
  }
  await settle(page, 30_000);
  await page.waitForTimeout(500);
  await cdp.send('HeapProfiler.collectGarbage');
  await cdp.send('HeapProfiler.startSampling', { samplingInterval: 256, includeObjectsCollectedByMajorGC: true, includeObjectsCollectedByMinorGC: true });
  if (profileTop) await cdp.send('Profiler.start');
  const live = await page.evaluate(async (seconds) => {
    const perf = window.__perf;
    perf.frames = [];
    perf.cur = null;
    perf.on = true;
    const t0 = performance.now();
    await new Promise((r) => setTimeout(r, seconds * 1000));
    perf.on = false;
    return { frames: perf.frames, wall: performance.now() - t0 };
  }, seconds);
  const { profile } = await cdp.send('HeapProfiler.stopSampling');
  if (profileTop) printProfile(s.name, (await cdp.send('Profiler.stop')).profile, profileTop);
  const us = await page.evaluate(bench, 200);
  await page.evaluate(() => window.__wx.playback.pause());
  const n = Math.max(1, live.frames.length);
  const mean = (k) => live.frames.reduce((a, f) => a + f[k], 0) / n;
  return {
    fps: (live.frames.length / live.wall) * 1000,
    us,
    draws: mean('draws'),
    uniforms: mean('uniforms'),
    uploads: mean('uploads'),
    binds: mean('binds'),
    programs: mean('programs'),
    allocPerFrame: Object.fromEntries(Object.entries(allocations(profile)).map(([k, v]) => [k, v / n])),
  };
}

const all = {};
for (let run = 0; run < runs; run++) {
  const ctx = await browser.newContext({ viewport: { width: 1024, height: 640 }, timezoneId: 'America/Chicago', locale: 'en-US' });
  const page = await ctx.newPage();
  await page.goto(`${app.base}/?debug`);
  await waitWeather(page);
  await page.evaluate(instrument);
  const cdp = await ctx.newCDPSession(page);
  await cdp.send('HeapProfiler.enable');
  if (profileTop) await cdp.send('Profiler.enable');
  for (const s of SCENARIOS) {
    const r = await scenario(page, cdp, s);
    (all[s.name] ??= []).push(r);
    console.log(`run ${run + 1} ${s.name}: ${JSON.stringify(r, (k, v) => (typeof v === 'number' ? +v.toFixed(2) : v))}`);
  }
  await ctx.close();
}

const f1 = (x) => (Number.isFinite(x) ? x.toFixed(1) : '-');
console.log(`\nmedians over ${runs} runs. Time: µs per call. GL calls and KB allocated: per frame.`);
console.log(
  `${'scenario'.padEnd(17)} ${'fps'.padStart(5)} ${'prerender'.padStart(9)} ${'render'.padStart(7)} ${'p.step'.padStart(7)} ${'p.draw'.padStart(7)} ${'tick'.padStart(7)}` +
    ` ${'draws'.padStart(6)} ${'unif'.padStart(5)} ${'upl'.padStart(5)} ${'binds'.padStart(5)}  KB: layer particles store`,
);
for (const [name, all_] of Object.entries(all)) {
  // A run in which the map drew nothing (the machine starved the page) has
  // no per-frame numbers; its timings per call still count.
  const drew = all_.filter((r) => r.fps > 0);
  const rs = drew.length ? drew : all_;
  const m = (f) => median(rs.map(f));
  const a = (k) => m((r) => r.allocPerFrame[k] ?? 0) / 1024;
  const t = (k) => f1(median(all_.map((r) => r.us[k] ?? NaN)));
  console.log(
    `${name.padEnd(17)} ${f1(m((r) => r.fps)).padStart(5)} ${t('layer_prerender').padStart(9)} ${t('layer_render').padStart(7)} ${t('particles_step').padStart(7)} ${t('particles_render').padStart(7)} ${t('store_tick').padStart(7)}` +
      ` ${f1(m((r) => r.draws)).padStart(6)} ${f1(m((r) => r.uniforms)).padStart(5)} ${m((r) => r.uploads).toFixed(2).padStart(5)} ${f1(m((r) => r.binds)).padStart(5)}` +
      `  ${f1(a('layer_prerender') + a('layer_render')).padStart(5)} ${f1(a('particles_step') + a('particles_render')).padStart(9)} ${f1(a('store_set')).padStart(5)}`,
  );
}
const json = opt('--json');
if (json) writeFileSync(json, JSON.stringify(all, null, 2));
await browser.close();
await app.close();
