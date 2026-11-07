#!/usr/bin/env node
// Memory over a long session, and leaks on layer switches.
//
// 1. Play the radar loop for `--minutes` (default 10) at a regional view,
//    sampling every 15 s: the tile cache's entries and accounted bytes, the
//    live GPU textures and their actual bytes (from wrapping the WebGL
//    texture calls), the JS heap, and loads started (a loop that fits in the
//    cache starts none once it has gone round once; one that does not keeps
//    refetching). `--budget MB` sets the cache budget (`?tilebudget=`) to see
//    eviction under pressure.
// 2. Switch layers 40 times and count what is left registered: MapLibre
//    listeners, DOM listeners, pending timeouts and intervals, animation
//    frames. A count that grows with the switches is a leak.
//
//   node scripts/perf/memory.mjs                        # 10 minutes, default budget
//   node scripts/perf/memory.mjs --minutes 3 --budget 64
//   node scripts/perf/memory.mjs --only switches

import { writeFileSync } from 'node:fs';
import { jumpTo, launch, opt, startApp, waitWeather } from './lib.mjs';

const minutes = Number(opt('--minutes', 10));
const budget = opt('--budget');
const only = opt('--only');
const app = await startApp();
const browser = await launch();

/** Installed before the app runs: counts of what is registered and alive. */
function census() {
  // Listeners by the kind of target. An AbortSignal's go with it when its
  // load is done and forgotten, so those are left out.
  const live = { timeouts: new Set(), intervals: new Set(), frames: new Set(), listeners: {}, textures: new Map() };
  const kind = (t) => (t === window ? 'window' : t === document ? 'document' : t?.constructor?.name ?? 'other');
  window.__census = live;
  const st = window.setTimeout;
  const ct = window.clearTimeout;
  window.setTimeout = function (f, ms, ...a) {
    const id = st.call(window, (...b) => {
      live.timeouts.delete(id);
      return typeof f === 'function' ? f(...b) : undefined;
    }, ms, ...a);
    live.timeouts.add(id);
    return id;
  };
  window.clearTimeout = function (id) {
    live.timeouts.delete(id);
    return ct.call(window, id);
  };
  const si = window.setInterval;
  const ci = window.clearInterval;
  window.setInterval = function (...a) {
    const id = si.apply(window, a);
    live.intervals.add(id);
    return id;
  };
  window.clearInterval = function (id) {
    live.intervals.delete(id);
    return ci.call(window, id);
  };
  const raf = window.requestAnimationFrame;
  const caf = window.cancelAnimationFrame;
  window.requestAnimationFrame = function (f) {
    const id = raf.call(window, (t) => {
      live.frames.delete(id);
      f(t);
    });
    live.frames.add(id);
    return id;
  };
  window.cancelAnimationFrame = function (id) {
    live.frames.delete(id);
    return caf.call(window, id);
  };
  const add = EventTarget.prototype.addEventListener;
  const remove = EventTarget.prototype.removeEventListener;
  const registered = new WeakMap();
  EventTarget.prototype.addEventListener = function (type, fn, o) {
    const key = `${type}|${typeof o === 'object' ? !!o?.capture : !!o}`;
    let m = registered.get(this);
    if (!m) registered.set(this, (m = new Map()));
    let s = m.get(key);
    if (!s) m.set(key, (s = new Set()));
    const k = kind(this);
    if (fn && !s.has(fn) && k !== 'AbortSignal') {
      s.add(fn);
      live.listeners[k] = (live.listeners[k] ?? 0) + 1;
      // A `once` listener is counted until it fires.
      if (typeof o === 'object' && o?.once) add.call(this, type, () => s.delete(fn) && live.listeners[k]--, { once: true });
    }
    return add.call(this, type, fn, o);
  };
  EventTarget.prototype.removeEventListener = function (type, fn, o) {
    const key = `${type}|${typeof o === 'object' ? !!o?.capture : !!o}`;
    const s = registered.get(this)?.get(key);
    if (s?.delete(fn)) live.listeners[kind(this)]--;
    return remove.call(this, type, fn, o);
  };
  // GPU textures: created, sized by their uploads, deleted.
  const BYTES = { 0x8234: 2, 0x823a: 4, 0x8d77: 6, 0x8d76: 8, 0x8058: 4, 0x881a: 8, 0x8814: 16 };
  const proto = WebGL2RenderingContext.prototype;
  const create = proto.createTexture;
  const del = proto.deleteTexture;
  const tex2d = proto.texImage2D;
  const bind = proto.bindTexture;
  const bound = new WeakMap();
  proto.createTexture = function () {
    const t = create.call(this);
    if (t) live.textures.set(t, 0);
    return t;
  };
  proto.deleteTexture = function (t) {
    live.textures.delete(t);
    return del.call(this, t);
  };
  proto.bindTexture = function (target, t) {
    if (target === this.TEXTURE_2D) bound.set(this, t);
    return bind.call(this, target, t);
  };
  proto.texImage2D = function (...a) {
    const t = bound.get(this);
    // texImage2D(target, level, internalformat, width, height, border, format, type, pixels)
    if (t && live.textures.has(t) && a.length >= 9 && a[1] === 0) {
      // RGB16UI is stored with a fourth channel by most drivers.
      const fmt = a[2];
      const px = fmt === 0x8d77 ? 8 : (BYTES[fmt] ?? 4);
      live.textures.set(t, a[3] * a[4] * px);
    }
    return tex2d.apply(this, a);
  };
}

function sample() {
  const wx = window.__wx;
  const c = window.__census;
  const tiles = wx.map.tiles;
  let gpu = 0;
  for (const b of c.textures.values()) gpu += b;
  let tileGpu = 0;
  let tileCpu = 0;
  for (const e of tiles.entries.values()) {
    if (e.data) tileCpu += e.data.byteLength;
    if (e.texture) tileGpu += c.textures.get(e.texture) ?? 0;
  }
  const ml = wx.map.map._listeners ?? {};
  const mlOnce = wx.map.map._oneTimeListeners ?? {};
  const count = (o) => Object.values(o).reduce((n, a) => n + a.length, 0);
  return {
    t: Math.round(performance.now() / 1000),
    entries: tiles.size,
    accounted: tiles.usedBytes,
    tileCpu,
    tileGpu,
    textures: c.textures.size,
    gpuAll: gpu,
    heap: performance.memory?.usedJSHeapSize ?? 0,
    started: tiles.stats.started,
    budget: tiles.opts.budgetBytes,
    mapListeners: count(ml) + count(mlOnce),
    domListeners: Object.values(c.listeners).reduce((a, b) => a + b, 0),
    domByKind: { ...c.listeners },
    timeouts: c.timeouts.size,
    intervals: c.intervals.size,
    frames: c.frames.size,
  };
}

const mb = (n) => `${(n / 1048576).toFixed(1)} MB`;
const report = {};
const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, timezoneId: 'America/Chicago', locale: 'en-US' });
await ctx.addInitScript(census);
const page = await ctx.newPage();
await page.goto(`${app.base}/?debug${budget ? `&tilebudget=${budget}` : ''}`);
await waitWeather(page);

if (only !== 'switches') {
  // The central US with the radar's loop playing: many frames, each with its
  // own tiles.
  await jumpTo(page, [-95, 38], 5);
  await page.evaluate(() => window.__wx.playback.play());
  const rows = [];
  const end = Date.now() + minutes * 60_000;
  let loopStarted = null;
  for (;;) {
    const s = await page.evaluate(sample);
    rows.push(s);
    loopStarted ??= s.started;
    console.log(
      `${String(s.t).padStart(5)} s  entries ${String(s.entries).padStart(5)}  accounted ${mb(s.accounted).padStart(9)}  samples ${mb(s.tileCpu).padStart(9)} + textures ${mb(s.tileGpu).padStart(9)}` +
        ` (${s.textures} GPU textures, ${mb(s.gpuAll)})  heap ${mb(s.heap).padStart(9)}  loads ${s.started}`,
    );
    if (Date.now() >= end) break;
    await page.waitForTimeout(15_000);
  }
  await page.evaluate(() => window.__wx.playback.pause());
  const first = rows.find((r) => r.t - rows[0].t >= 60) ?? rows[0];
  const last = rows.at(-1);
  report.session = { rows, budget: last.budget, loadsAfterFirstMinute: last.started - first.started, minutes };
  console.log(
    `\nbudget ${mb(last.budget)}; after the first minute the loop started ${last.started - first.started} more loads; ` +
      `accounted ${mb(last.accounted)} vs actual ${mb(last.tileCpu + last.tileGpu)} (samples + textures)`,
  );
}

if (only !== 'session') {
  const layers = await page.evaluate(() => window.__wx.store.get().layers.map((l) => l.id));
  await jumpTo(page, [-95, 38], 4);
  const switchTo = async (id) => {
    await page.evaluate((id) => window.__wx.store.set({ layerId: id }), id);
    await page.waitForTimeout(150);
  };
  const settle = async () => {
    await switchTo('radar');
    await waitWeather(page);
    await page.waitForTimeout(1500);
    return page.evaluate(sample);
  };
  // Once round every layer first, so first-time costs are not counted.
  for (const id of layers) await switchTo(id);
  const before = await settle();
  for (let i = 0; i < 40; i++) await switchTo(layers[i % layers.length]);
  const after = await settle();
  report.switches = { before, after, layers };
  console.log(`\nafter 40 layer switches (${layers.length} layers), back on radar and settled:`);
  for (const k of ['mapListeners', 'domListeners', 'timeouts', 'intervals', 'frames', 'textures']) {
    console.log(`  ${k.padEnd(13)} ${String(before[k]).padStart(5)} -> ${String(after[k]).padStart(5)}`);
  }
  for (const k of new Set([...Object.keys(before.domByKind), ...Object.keys(after.domByKind)])) {
    if (before.domByKind[k] !== after.domByKind[k]) console.log(`    DOM listeners on ${k}: ${before.domByKind[k] ?? 0} -> ${after.domByKind[k] ?? 0}`);
  }
}

const json = opt('--json');
if (json) writeFileSync(json, JSON.stringify(report, null, 2));
await browser.close();
await app.close();
