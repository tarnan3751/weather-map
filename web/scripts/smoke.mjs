#!/usr/bin/env node
// Smoke test: starts the dev server against the mock backend, drives the app
// in headless Chromium at desktop and phone sizes, and fails if the weather
// layer draws nothing, the georeferencing storm is not over Kansas City, a
// live frame does not arrive, the world view, the dateline or a mixed-zoom
// view look wrong, a source update refetches tiles it did not change, the
// credits or the read-out do not name the sources, the client asks for a
// stack the server would refuse, or the console logs an error.
//
//   npm run smoke                      # checks only
//   npm run smoke -- --shots DIR       # also save screenshots to DIR
//   npm run smoke -- --url http://…    # use a server that is already running
//                                      # (e.g. `VITE_MOCK=1 vite preview` for the build)
//   npm run smoke -- --only particles  # run only the named steps (comma-separated)

import { existsSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const webDir = fileURLToPath(new URL('..', import.meta.url));
const argv = process.argv.slice(2);
const opt = (name) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : undefined;
};
const shotsDir = opt('--shots');
const port = Number(opt('--port') ?? 5183);
let base = opt('--url');
if (shotsDir) mkdirSync(shotsDir, { recursive: true });

// Downtown Kansas City, where the mock parks a stationary 63 dBZ storm.
const KC = { lon: -94.5786, lat: 39.0997 };

const failures = [];
const check = (ok, msg) => {
  console.log(`${ok ? '  ok  ' : ' FAIL '} ${msg}`);
  if (!ok) failures.push(msg);
};

let server = null;
if (!base) {
  process.env.VITE_MOCK = '1';
  const { createServer } = await import('vite');
  server = await createServer({
    root: webDir,
    configFile: join(webDir, 'vite.config.ts'),
    server: { port, strictPort: true, host: '127.0.0.1' },
    logLevel: 'warn',
  });
  await server.listen();
  base = `http://127.0.0.1:${port}`;
}

const browser = await chromium.launch({
  // The full Chromium in new headless mode (the default headless shell
  // refuses notification permission), with software WebGL: there is no GPU.
  channel: 'chromium',
  args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'],
});

// Every request any page makes, by host: the page must need no other host
// than its own (the basemap's glyphs and sprites are served with it).
const outsideHosts = new Set();
const plainNewContext = browser.newContext.bind(browser);
browser.newContext = async (opts) => {
  const c = await plainNewContext(opts);
  c.on('request', (r) => {
    const u = new URL(r.url());
    if (/^(https?|wss?):$/.test(u.protocol) && u.host !== new URL(base).host) outsideHosts.add(u.host);
  });
  return c;
};

function watchConsole(page, label, allow = []) {
  const errors = [];
  page.on('console', (m) => {
    if (m.type() !== 'error') return;
    const text = m.text();
    if (allow.some((re) => re.test(text))) return;
    errors.push(text);
  });
  page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
  return () => check(errors.length === 0, `${label}: no console errors${errors.length ? ` (${errors.slice(0, 3).join(' | ')})` : ''}`);
}

async function shot(page, name) {
  if (!shotsDir) return;
  await page.screenshot({ path: join(shotsDir, `${name}.png`) });
}

/** Wait until the map exists and the displayed frame's tiles are in. */
async function waitWeather(page, timeout = 30_000) {
  await page.waitForFunction(
    () => {
      const m = window.__wx?.map;
      if (!m || !m.map.isStyleLoaded()) return false;
      const d = m.layer.getDisplay();
      return d.a && m.layer.isReady(d.a) && m.layer.stats.slots > 0;
    },
    null,
    { timeout },
  );
  // Let the basemap's own tiles settle too, if they will.
  await page.waitForFunction(() => window.__wx.map.map.areTilesLoaded(), null, { timeout: 15_000 }).catch(() => {});
  await page.waitForTimeout(400);
}

/** Fraction of pixels that differ between two screenshots. */
async function diffFraction(page, a, b) {
  return page.evaluate(
    async ([a64, b64]) => {
      const load = async (s) => createImageBitmap(await (await fetch(`data:image/png;base64,${s}`)).blob());
      const [ia, ib] = await Promise.all([load(a64), load(b64)]);
      const c = new OffscreenCanvas(ia.width, ia.height);
      const g = c.getContext('2d');
      g.drawImage(ia, 0, 0);
      const da = g.getImageData(0, 0, ia.width, ia.height).data;
      g.drawImage(ib, 0, 0);
      const db = g.getImageData(0, 0, ia.width, ia.height).data;
      let n = 0;
      for (let i = 0; i < da.length; i += 4) {
        if (Math.abs(da[i] - db[i]) + Math.abs(da[i + 1] - db[i + 1]) + Math.abs(da[i + 2] - db[i + 2]) > 40) n++;
      }
      return n / (da.length / 4);
    },
    [a.toString('base64'), b.toString('base64')],
  );
}

/** Fraction of pixels that differ between two screenshots inside a box. */
async function diffIn(page, a, b, [x0, y0, x1, y1]) {
  return page.evaluate(
    async ([a64, b64, x0, y0, x1, y1]) => {
      const load = async (s) => createImageBitmap(await (await fetch(`data:image/png;base64,${s}`)).blob());
      const [ia, ib] = await Promise.all([load(a64), load(b64)]);
      const c = new OffscreenCanvas(ia.width, ia.height);
      const g = c.getContext('2d');
      const w = Math.max(1, Math.round(x1 - x0));
      const h = Math.max(1, Math.round(y1 - y0));
      g.drawImage(ia, 0, 0);
      const da = g.getImageData(Math.round(x0), Math.round(y0), w, h).data;
      g.drawImage(ib, 0, 0);
      const db = g.getImageData(Math.round(x0), Math.round(y0), w, h).data;
      let n = 0;
      for (let i = 0; i < da.length; i += 4) {
        if (Math.abs(da[i] - db[i]) + Math.abs(da[i + 1] - db[i + 1]) + Math.abs(da[i + 2] - db[i + 2]) > 40) n++;
      }
      return n / (da.length / 4);
    },
    [a.toString('base64'), b.toString('base64'), x0, y0, x1, y1],
  );
}

/** Screenshots with and without the weather layer. */
async function withAndWithout(page) {
  const on = await page.screenshot();
  await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'none'));
  await page.waitForTimeout(300);
  const off = await page.screenshot();
  await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'visible'));
  await page.waitForTimeout(200);
  return { on, off };
}

/** Tile requests the mock refused since it started. */
async function refused(page) {
  return (await (await page.request.get(`${base}/__mock/misses`)).json()).misses;
}

/** Hover a lon/lat and return the read-out's text and source line. */
async function hoverAt(page, lon, lat) {
  // The copy of that longitude nearest the centre, which may be past 180.
  const p = await page.evaluate(([lon, lat]) => {
    const m = window.__wx.map.map;
    const c = m.getCenter().lng;
    return m.project([lon + 360 * Math.round((c - lon) / 360), lat]);
  }, [lon, lat]);
  await page.mouse.move(p.x + 1, p.y + 1);
  await page.waitForTimeout(150);
  await page.mouse.move(p.x, p.y);
  await page.waitForFunction(() => document.querySelector('.readout'), null, { timeout: 5000 }).catch(() => {});
  // Outside a solid box the source line waits for the server's answer,
  // asked once the pointer has rested: wait for a read-out of this very
  // point (an earlier one may still be on screen) with its source.
  await page
    .waitForFunction(
      ([lon, lat]) => {
        const r = window.__wx.store.get().readout;
        return r && Math.abs(r.lat - lat) < 0.1 && Math.abs(((r.lon - lon + 540) % 360) - 180) < 0.1 && r.source;
      },
      [lon, lat],
      { timeout: 10_000 },
    )
    .catch(() => {});
  await page.waitForTimeout(150);
  // The read-out of this point, not whatever was last on screen.
  return page.evaluate(([lon, lat]) => {
    const r = window.__wx.store.get().readout;
    const here = r && Math.abs(r.lat - lat) < 0.1 && Math.abs(((r.lon - lon + 540) % 360) - 180) < 0.1;
    return here ? { text: r.text, source: r.source ?? null } : { text: null, source: null };
  }, [lon, lat]);
}

/** Strongest radar-like colour in a small ring of pixels around (x, y). */
async function ringColor(page, png, x, y, r0, r1) {
  return page.evaluate(
    async ([s, x, y, r0, r1]) => {
      const img = await createImageBitmap(await (await fetch(`data:image/png;base64,${s}`)).blob());
      const c = new OffscreenCanvas(img.width, img.height);
      const g = c.getContext('2d');
      g.drawImage(img, 0, 0);
      let best = { score: -1, rgb: [0, 0, 0] };
      for (let a = 0; a < 360; a += 15) {
        for (let r = r0; r <= r1; r += 2) {
          const px = Math.round(x + r * Math.cos((a * Math.PI) / 180));
          const py = Math.round(y + r * Math.sin((a * Math.PI) / 180));
          const [R, G, B] = g.getImageData(px, py, 1, 1).data;
          const score = Math.max(R, B) - G;
          if (score > best.score) best = { score, rgb: [R, G, B] };
        }
      }
      return best;
    },
    [png.toString('base64'), x, y, r0, r1],
  );
}

async function desktop() {
  console.log('desktop 1440x900');
  // Notifications allowed, so the panel shows its "enable" step rather than "blocked".
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Chicago', locale: 'en-US', permissions: ['notifications'] });
  const page = await ctx.newPage();
  const done = watchConsole(page, 'desktop');
  await page.goto(`${base}/?debug`);
  await waitWeather(page);
  await shot(page, '01-desktop-radar');

  // The weather layer must actually draw: compare with it hidden.
  const withWx = await page.screenshot();
  await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'none'));
  await page.waitForTimeout(300);
  const without = await page.screenshot();
  await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'visible'));
  const frac = await diffFraction(page, withWx, without);
  check(frac > 0.01, `radar layer draws (${(frac * 100).toFixed(1)}% of pixels change when it is hidden)`);

  // Labels sit above the weather layer.
  const order = await page.evaluate(() => {
    // getStyle() leaves custom layers out; the render order includes them.
    const ids = window.__wx.map.map.getLayersOrder();
    const w = ids.indexOf('weather');
    return { w, firstLabel: ids.findIndex((id, i) => i > 0 && /places_|label/.test(id)), roads: ids.findIndex((id) => id.startsWith('roads_tunnels')), boundaries: ids.indexOf('boundaries') };
  });
  check(order.w >= 0 && order.w < order.firstLabel && (order.boundaries < 0 || order.w < order.boundaries), 'weather layer is below borders and labels');

  // A new radar frame over the WebSocket reaches the timeline, and a map that
  // follows "now" shows the frame nearest the clock from the newest
  // observation on (a nowcast step may be nearer than the newest scan).
  const newestObs = () => page.evaluate(() => window.__wx.frames().filter((f) => f.kind === 'observed').at(-1)?.valid);
  const beforeObs = await newestObs();
  await page.request.post(`${base}/__mock/advance?minutes=2`);
  await page.waitForFunction((v) => window.__wx.frames().filter((f) => f.kind === 'observed').at(-1)?.valid !== v, beforeObs, { timeout: 10_000 }).catch(() => {});
  await page.waitForTimeout(500);
  const followed = await page.evaluate(() => {
    const s = window.__wx.state();
    return { shown: window.__wx.map.layer.getDisplay().a?.valid, now: window.__wx.frames()[s.index]?.valid, following: window.__wx.store.get().following };
  });
  const afterObs = await newestObs();
  check(afterObs !== beforeObs && followed.following && followed.shown === followed.now, `live frame followed: newest scan ${beforeObs} -> ${afterObs}, showing ${followed.shown} as now`);
  await waitWeather(page);
  await page.waitForFunction(() => /MRMS/.test(document.querySelector('.kind-badge')?.textContent ?? ''), null, { timeout: 8000 }).catch(() => {});
  const label = await page.textContent('.kind-badge');
  check(/^\s*Observed · MRMS radar( and \d+ others?)?\s*$/.test(label ?? ''), `over the lower 48 the time bar names the radar at the centre ("${label}")`);
  const credits = await page.textContent('.maplibregl-ctrl-attrib-inner');
  check(/OpenStreetMap|Natural Earth/.test(credits ?? '') && /Weather: .*NOAA\/NSSL MRMS/.test(credits ?? ''), `credits name the basemap and the radar in view ("${credits}")`);

  // Georeferencing: the storm centred on downtown Kansas City.
  await page.evaluate(([lon, lat]) => {
    window.__wx.store.set({ overlays: { ...window.__wx.store.get().overlays, alerts: false } });
    window.__wx.map.map.jumpTo({ center: [lon, lat], zoom: 7.5 });
  }, [KC.lon, KC.lat]);
  await waitWeather(page);
  await shot(page, '02-georef-kansas-city');
  const kcPng = await page.screenshot();
  const pt = await page.evaluate(([lon, lat]) => window.__wx.map.map.project([lon, lat]), [KC.lon, KC.lat]);
  const sample = await page.evaluate(([lon, lat]) => window.__wx.map.layer.sample(lon, lat), [KC.lon, KC.lat]);
  check(sample?.values?.[0] > 55, `read-out at Kansas City is a storm core (${sample?.values?.[0]?.toFixed(1)} dBZ)`);
  const core = await ringColor(page, kcPng, pt.x, pt.y, 4, 12);
  const away = await ringColor(page, kcPng, pt.x + 330, pt.y, 4, 12);
  check(core.score > 90 && away.score < 40, `storm core drawn on Kansas City (centre ${core.rgb}, 160 km east ${away.rgb})`);

  // Hover read-out, and where the value comes from.
  await page.mouse.move(pt.x + 2, pt.y + 2);
  await page.waitForSelector('.readout', { timeout: 5000 });
  const readout = await page.textContent('.readout');
  check(/dBZ/.test(readout ?? ''), `hover read-out shows reflectivity ("${readout}")`);
  // Inside the radar's box but not a solid one: named once the server says.
  await page.waitForSelector('.readout-source', { timeout: 4000 }).catch(() => {});
  const from = await page.textContent('.readout-source').catch(() => null);
  check(from === 'From MRMS', `hover read-out names the source ("${from}")`);
  await shot(page, '03-hover-readout');
  await page.mouse.move(5, 450);

  // Back to the national view for the animation.
  await page.evaluate(() => {
    window.__wx.store.set({ overlays: { ...window.__wx.store.get().overlays, alerts: true } });
    window.__wx.map.map.jumpTo({ center: [-95.5, 38.5], zoom: 4.3 });
  });
  await waitWeather(page);

  // Mid cross-fade while playing.
  await page.click('button[aria-label="Play"]');
  // Software GL on a busy machine may draw only a few frames a second, so a
  // fade can span one or two of them: poll often and accept any mid-fade.
  await page.waitForFunction(() => {
    const d = window.__wx.map.layer.getDisplay();
    return d.b && d.mix > 0.15 && d.mix < 0.85;
  }, null, { timeout: 40_000, polling: 30 }).catch(() => {});
  const mix = await page.evaluate(() => window.__wx.map.layer.getDisplay().mix);
  await shot(page, '04-animation-crossfade');
  check(mix > 0.1 && mix < 0.9, `captured a cross-fade between frames (mix ${mix.toFixed(2)})`);
  await page.click('button[aria-label="Pause"]');

  // Drag the time slider from this morning's radar to tonight's forecast.
  const track = await page.locator('.track').boundingBox();
  const target = await page.evaluate(() => {
    const frames = window.__wx.frames();
    const tonight = new Date();
    tonight.setHours(21, 0, 0, 0);
    let best = 0;
    frames.forEach((f, i) => {
      if (Math.abs(f.t - tonight) < Math.abs(frames[best].t - tonight)) best = i;
    });
    return { i: best, x: window.__wx.positions()[best], id: frames[best].valid };
  });
  await page.mouse.move(track.x + 6, track.y + track.height / 2);
  await page.mouse.down();
  for (let k = 1; k <= 12; k++) await page.mouse.move(track.x + track.width * target.x * (k / 12), track.y + track.height / 2);
  await page.mouse.up();
  await waitWeather(page);
  const dragged = await page.evaluate(() => window.__wx.map.layer.getDisplay().a);
  check(dragged?.kind === 'forecast', `slider drag reached tonight's forecast (${dragged?.valid}, wanted ${target.id})`);
  await shot(page, '05-forecast-tonight');

  // Switch to temperature: the time carries over.
  const tRadar = dragged?.t ?? 0;
  await page.click('.layer-btn');
  await page.locator('.layer-item', { has: page.locator('.layer-name', { hasText: /^Temperature$/ }) }).click();
  await page.keyboard.press('Escape');
  await waitWeather(page);
  const temp = await page.evaluate(() => ({ s: window.__wx.state(), a: window.__wx.map.layer.getDisplay().a }));
  check(temp.s.layer === 'temp' && Math.abs(temp.a.t - tRadar) <= 90 * 60_000, `temperature shows the frame nearest the chosen time (${temp.a?.valid})`);
  await shot(page, '06-temperature');
  const tempMove = await page.screenshot();
  await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'none'));
  await page.waitForTimeout(250);
  const tempOff = await page.screenshot();
  await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'visible'));
  check((await diffFraction(page, tempMove, tempOff)) > 0.2, 'temperature layer draws');

  for (const [name, title] of [
    ['07-wind', 'Wind and gusts'],
    ['08-precip-type', 'Precipitation type'],
    ['09-rain-24h', 'Rain, past 24 hours'],
    ['10-snowfall', 'Snowfall'],
    ['11-clouds', 'Cloud cover'],
  ]) {
    await page.click('.layer-btn');
    await page.locator('.layer-item', { has: page.locator('.layer-name', { hasText: new RegExp(`^${title.replace(/[,]/g, '.')}$`) }) }).click();
    await page.keyboard.press('Escape');
    await waitWeather(page);
    await shot(page, name);
  }

  // Back to radar now, then the point forecast for a tapped spot (Denver).
  await page.click('.layer-btn');
  await page.locator('.layer-item', { has: page.locator('.layer-name', { hasText: /^Radar$/ }) }).click();
  await page.keyboard.press('Escape');
  await page.click('.now-btn');
  await waitWeather(page);
  const denver = await page.evaluate(() => window.__wx.map.map.project([-104.99, 39.74]));
  await page.mouse.click(denver.x, denver.y);
  await page.waitForSelector('.fchart svg', { timeout: 10_000 });
  const chart = await page.locator('.fchart svg').boundingBox();
  await page.mouse.move(chart.x + chart.width * 0.4, chart.y + 60);
  await page.waitForTimeout(200);
  check(!(await page.locator('.fchart-readout').getAttribute('class')).includes('idle') && (await page.isVisible('.fchart-cross')), 'forecast chart shows a crosshair read-out');
  await shot(page, '12-point-forecast');

  // Save Kansas City as "Home", then a new warning there arrives live.
  await page.keyboard.press('Escape');
  const kc = await page.evaluate(([lon, lat]) => {
    window.__wx.map.map.jumpTo({ center: [lon, lat], zoom: 6 });
    return window.__wx.map.map.project([lon, lat]);
  }, [KC.lon, KC.lat]);
  await page.evaluate(() => window.__wx.store.set({ overlays: { ...window.__wx.store.get().overlays, alerts: false } }));
  await page.mouse.click(kc.x, kc.y);
  await page.waitForSelector('#point-title');
  await page.click('button:has-text("Save place")');
  await page.fill('#place-name', 'Home');
  await page.click('.save-form button:has-text("Save")');
  await page.waitForSelector('button:has-text("Saved as")');
  await page.evaluate(() => window.__wx.store.set({ overlays: { ...window.__wx.store.get().overlays, alerts: true }, panel: null }));
  await page.request.post(`${base}/__mock/alert`);
  const toast = await page.waitForSelector('.toast:has-text("Home")', { timeout: 10_000 }).catch(() => null);
  check(!!toast, 'a new Tornado Warning over a saved place raises a toast');
  await page.waitForTimeout(700);
  await shot(page, '13-live-warning');
  await page.click('.toast button:has-text("View")');
  await page.waitForSelector('.alert-instruction', { timeout: 10_000 });
  await page.waitForTimeout(1500);
  await shot(page, '14-alert-details');

  await page.click('button[aria-label="Saved places"]');
  await page.waitForSelector('.place');
  const covered = await page.textContent('.place');
  check(/Tornado Warning/.test(covered ?? ''), 'saved place lists the warning covering it');
  // Panels fade in; let them finish before the picture.
  await page.waitForTimeout(400);
  await shot(page, '15-saved-places');

  await page.click('button[aria-label="Notifications"]');
  await page.waitForSelector('#notify-title');
  await page.waitForTimeout(300);
  await shot(page, '16-notifications');

  await page.click('button[aria-label="Settings"]');
  await page.click('.seg-control button:has-text("°C")');
  await page.click('.seg-control button:has-text("km/h")');
  await page.waitForTimeout(400);
  await shot(page, '17-settings-metric');
  await page.click('.seg-control button:has-text("°F")');
  await page.click('.seg-control button:has-text("mph")');

  done();
  await ctx.close();
}

/** Count tile requests over a quiet spell. */
async function tileRequestsDuring(page, ms) {
  let n = 0;
  const on = (r) => {
    if (r.url().includes('/tiles/')) n++;
  };
  page.on('request', on);
  await page.waitForTimeout(ms);
  page.off('request', on);
  return n;
}

/** Things that go wrong mid-session: lost GPU context, a failing basemap,
 *  an idle map under memory pressure, a frame replaced under the viewer, a
 *  click past the antimeridian. */
async function resilience() {
  console.log('resilience');
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'America/Chicago' });
  const page = await ctx.newPage();
  // MapLibre warns (not errors) that it cannot restore custom layers itself.
  const done = watchConsole(page, 'resilience');
  await page.goto(`${base}/?debug`);
  await waitWeather(page);
  await page.waitForTimeout(800);

  // WebGL context loss and restore.
  await page.evaluate(() => window.__wx.map.map.jumpTo({ center: [-95.5, 38.5], zoom: 4.6 }));
  await waitWeather(page);
  const before = await page.screenshot();
  await page.evaluate(async () => {
    const gl = window.__wx.map.map.getCanvas().getContext('webgl2');
    const ext = gl.getExtension('WEBGL_lose_context');
    ext.loseContext();
    await new Promise((r) => setTimeout(r, 600));
    ext.restoreContext();
  });
  await page.waitForFunction(() => window.__wx.map.map.getLayer('weather'), null, { timeout: 15_000 }).catch(() => {});
  await waitWeather(page).catch(() => {});
  await page.waitForTimeout(800);
  const after = await page.screenshot();
  await shot(page, '50-after-context-restore');
  const changed = await diffFraction(page, before, after);
  check(changed < 0.03, `context loss and restore: map looks as it did (${(changed * 100).toFixed(1)}% of pixels differ)`);
  // Through the layer menu, which also fetches the stations.
  await page.click('.layer-btn');
  await page.locator('.switch-row', { hasText: 'Stations' }).locator('input').click();
  await page.keyboard.press('Escape');
  await page.waitForFunction(() => window.__wx.map.map.getLayer('stations-dot') && window.__wx.map.map.queryRenderedFeatures({ layers: ['stations-dot'] }).length > 0, null, { timeout: 15_000 }).catch(() => {});
  const overlaysAfter = await page.evaluate(() => {
    const m = window.__wx.map.map;
    return {
      stations: m.getLayer('stations-dot') ? m.queryRenderedFeatures({ layers: ['stations-dot'] }).length : -1,
      alerts: m.getLayer('alerts-fill') ? m.queryRenderedFeatures({ layers: ['alerts-fill'] }).length : -1,
    };
  });
  check(overlaysAfter.stations > 0 && overlaysAfter.alerts > 0, `context loss and restore: overlays still update (${JSON.stringify(overlaysAfter)})`);
  // Stations are thinned: at zoom 5 dots stand 26 px apart and labels twice that.
  await page.evaluate(() => window.__wx.map.map.jumpTo({ center: [-95.5, 38.5], zoom: 5 }));
  await page.waitForTimeout(1500);
  const spacing = await page.evaluate(() => {
    const m = window.__wx.map.map;
    const closest = (id) => {
      const pts = [...new Map(m.queryRenderedFeatures({ layers: [id] }).map((f) => [f.properties.id, m.project(f.geometry.coordinates)])).values()];
      let d = Infinity;
      for (let i = 0; i < pts.length; i++) for (let j = i + 1; j < pts.length; j++) d = Math.min(d, Math.hypot(pts[i].x - pts[j].x, pts[i].y - pts[j].y));
      return { n: pts.length, d: Math.round(d) };
    };
    return { dots: closest('stations-dot'), labels: closest('stations-label') };
  });
  check(
    spacing.dots.n > 20 && spacing.labels.n > 5 && spacing.dots.d >= 25 && spacing.labels.d >= 50,
    `stations thinned at zoom 5: ${spacing.dots.n} dots at least ${spacing.dots.d} px apart, ${spacing.labels.n} labels at least ${spacing.labels.d} px`,
  );
  await page.click('.layer-btn');
  await page.locator('.switch-row', { hasText: 'Stations' }).locator('input').click();
  await page.keyboard.press('Escape');

  // The basemap archive starts failing mid-session.
  await page.evaluate(() => {
    for (let i = 0; i < 6; i++) window.__wx.map.map.fire('error', { error: new Error('simulated tile failure'), sourceId: 'protomaps' });
  });
  await page.waitForFunction(() => window.__wx.state().basemap === 'fallback' && window.__wx.map.map.getLayer('fallback-land') && window.__wx.map.map.getLayer('weather'), null, { timeout: 15_000 }).catch(() => {});
  await waitWeather(page).catch(() => {});
  await page.waitForTimeout(800);
  const fb = await page.evaluate(() => {
    const m = window.__wx.map.map;
    const order = m.getLayersOrder();
    return {
      basemap: window.__wx.state().basemap,
      weather: order.indexOf('weather'),
      land: order.indexOf('fallback-land'),
      coast: order.indexOf('fallback-coast'),
      overlays: ['alerts-fill', 'alerts-line', 'stations-dot', 'places-pin'].filter((id) => !m.getLayer(id)),
      alerts: m.getLayer('alerts-fill') ? m.queryRenderedFeatures({ layers: ['alerts-fill'] }).length : -1,
    };
  });
  check(fb.basemap === 'fallback' && fb.weather > fb.land && fb.weather < fb.coast, `basemap failure: weather sits above land and below borders in the fallback (${JSON.stringify(fb)})`);
  check(fb.overlays.length === 0 && fb.alerts > 0, 'basemap failure: alerts, stations and places survive the switch');
  const fbOn = await page.screenshot();
  await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'none'));
  await page.waitForTimeout(300);
  const fbOff = await page.screenshot();
  await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'visible'));
  check((await diffFraction(page, fbOn, fbOff)) > 0.01, 'basemap failure: weather still draws over the fallback');
  await shot(page, '51-after-basemap-failure');

  // Clicking after panning a world copy east sends a wrapped longitude.
  await page.evaluate(() => window.__wx.map.map.jumpTo({ center: [-104.99 + 360, 39.74], zoom: 6 }));
  await page.waitForTimeout(500);
  await page.mouse.click(720, 450);
  await page.waitForSelector('#point-title', { timeout: 10_000 }).catch(() => {});
  await page.waitForTimeout(1500);
  const p = await page.evaluate(() => ({ panel: window.__wx.state().panel, title: document.querySelector('#point-title')?.textContent, err: document.querySelector('.sheet .notice')?.textContent ?? null }));
  check(p.panel?.kind === 'point' && p.panel.lon >= -180 && p.panel.lon <= 180 && !p.err, `antimeridian: click gives a wrapped longitude and a forecast (${p.panel?.lon?.toFixed(2)}, "${p.title}")`);
  await page.keyboard.press('Escape');
  done();
  await ctx.close();
}

/** Where `count` particles moved in the last step, against the wind the
 *  read-out samples there: angle error (degrees) and speed ratio, as medians. */
async function particleMotion(page) {
  return page.evaluate(() => {
    const m = window.__wx.map;
    const dt = m.particles.stats.dt;
    const errs = [];
    const ratios = [];
    for (const { from, to } of m.particles.debugSteps(1500)) {
      const ll = m.map.unproject(from);
      const s = m.layer.sample(ll.lng, ll.lat);
      const [u, v] = s?.values ?? [];
      if (u == null || v == null || Math.hypot(u, v) < 2 || !dt) continue;
      const dx = to[0] - from[0];
      const dy = to[1] - from[1];
      // u is east and v north; screen y points down.
      let d = ((Math.atan2(-dy, dx) - Math.atan2(v, u)) * 180) / Math.PI;
      d = ((d + 540) % 360) - 180;
      errs.push(Math.abs(d));
      // 3 px/s per m/s, stretched by 1/cos(latitude) as Mercator stretches distance.
      ratios.push(Math.hypot(dx, dy) / (3 * Math.hypot(u, v) * (1 / Math.cos((ll.lat * Math.PI) / 180)) * dt));
    }
    const med = (a) => a.sort((x, y) => x - y)[a.length >> 1];
    return { n: errs.length, angle: med(errs), speed: med(ratios) };
  });
}

/** Wind particles: drawn, moving, going with the wind, and resetting or
 *  stopping when they should. */
async function particles() {
  console.log('wind particles');
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  const page = await ctx.newPage();
  const done = watchConsole(page, 'particles');
  await page.goto(`${base}/?debug`);
  await waitWeather(page);
  await page.evaluate(() => {
    window.__wx.store.set({ layerId: 'wind', windMarks: 'particles' });
    window.__wx.map.map.jumpTo({ center: [-95.5, 38.5], zoom: 4.6 });
  });
  await waitWeather(page);
  const stats = () => page.evaluate(() => ({ ...window.__wx.map.particles.stats }));
  const stepsPast = (n, timeout = 20_000) =>
    page.waitForFunction((n) => window.__wx.map.particles.stats.steps > n, n, { timeout }).then(
      () => true,
      () => false,
    );
  const vis = (v) => page.evaluate((v) => window.__wx.map.map.setLayoutProperty('wind-particles', 'visibility', v), v);
  /** Two captures of the particles 0.9 s apart, and one without them, once
   *  nothing else on the map is changing (a new alert's pulse, a late tile). */
  const captures = async () => {
    let noise = 1;
    for (let i = 0; i < 12 && noise > 0.003; i++) {
      await vis('none');
      await page.waitForTimeout(300);
      const h1 = await page.screenshot();
      await page.waitForTimeout(900);
      noise = await diffFraction(page, h1, await page.screenshot());
    }
    await vis('visible');
    await stepsPast((await stats()).steps + 8);
    const a = await page.screenshot();
    await page.waitForTimeout(900);
    const b = await page.screenshot();
    await vis('none');
    await page.waitForTimeout(400);
    const off = await page.screenshot();
    await vis('visible');
    return { a, drawn: await diffFraction(page, a, off), moving: await diffFraction(page, a, b), noise, off };
  };
  await stepsPast(12);
  const cap = await captures();
  await shot(page, '60-wind-particles');
  const st = await stats();
  check(
    st.running && st.particles > 1000 && st.particles <= 16384 && cap.drawn > 0.02,
    `particles draw (${st.particles} particles, ${(cap.drawn * 100).toFixed(1)}% of pixels; ${(cap.noise * 100).toFixed(2)}% change without them)`,
  );
  check(cap.moving > 0.01, `particles move: two captures differ (${(cap.moving * 100).toFixed(1)}%)`);
  // Nothing drawn for direction, then arrows; particle mode draws no arrows.
  // Compared over the map only (the credits line may change as overlays
  // settle), and again if something else on the map changed in between.
  let off;
  let arrowsInParticles = 1;
  for (let i = 0; i < 3 && arrowsInParticles >= 0.005; i++) {
    await page.evaluate(() => window.__wx.store.set({ windMarks: 'particles' }));
    await vis('none');
    await page.waitForTimeout(500);
    const hidden = await page.screenshot();
    await vis('visible');
    await page.evaluate(() => window.__wx.store.set({ windMarks: 'off' }));
    await page.waitForTimeout(600);
    off = await page.screenshot();
    arrowsInParticles = await diffIn(page, hidden, off, [0, 64, 1280, 640]);
  }
  check(arrowsInParticles < 0.005, `particle mode draws no arrows (${(arrowsInParticles * 100).toFixed(2)}% differs from no marks)`);
  await page.evaluate(() => window.__wx.store.set({ windMarks: 'arrows' }));
  await page.waitForTimeout(600);
  const arrows = await page.screenshot();
  await shot(page, '61-wind-arrows');
  check((await diffFraction(page, arrows, off)) > 0.01, 'arrows draw in arrow mode');

  await page.evaluate(() => window.__wx.store.set({ windMarks: 'particles' }));
  await stepsPast((await stats()).steps + 6);
  const motion = await particleMotion(page);
  check(
    motion.n > 200 && motion.angle < 3 && Math.abs(motion.speed - 1) < 0.05,
    `particles go with the wind: median direction error ${motion.angle?.toFixed(2)}°, speed ratio ${motion.speed?.toFixed(3)} (${motion.n} particles)`,
  );

  // Across the dateline, where Alaska's grid meets the global model's tiles
  // at another zoom, particles still follow the wind they sample.
  await page.evaluate(() => window.__wx.map.map.jumpTo({ center: [176, 57], zoom: 5.2 }));
  await waitWeather(page);
  await stepsPast((await stats()).steps + 10);
  const across = await particleMotion(page);
  const acrossZooms = await page.evaluate(() => window.__wx.map.layer.stats.zooms);
  await shot(page, '62-wind-dateline');
  check(
    across.n > 200 && across.angle < 3 && Math.abs(across.speed - 1) < 0.05 && acrossZooms.length >= 2,
    `particles across the dateline: median direction error ${across.angle?.toFixed(2)}°, speed ratio ${across.speed?.toFixed(3)} (${across.n} particles, tile zooms ${acrossZooms})`,
  );

  // A pan and a resize start the streaks afresh; then they run again.
  let s0 = await stats();
  await page.evaluate(() => window.__wx.map.map.jumpTo({ center: [-88, 36], zoom: 5.2 }));
  await waitWeather(page);
  await stepsPast(s0.steps + 6);
  let s1 = await stats();
  check(s1.resets > s0.resets && s1.running, `pan: particles reset and run again (${s0.resets} → ${s1.resets} resets)`);
  s0 = s1;
  await page.setViewportSize({ width: 1000, height: 700 });
  await stepsPast(s0.steps + 6);
  s1 = await stats();
  check(s1.resets > s0.resets && s1.particles < s0.particles && s1.running, `resize: particles reset, fewer for the smaller view (${s0.particles} → ${s1.particles})`);

  // Playback cross-fades the velocities; stepping frames does not restart them.
  s0 = await stats();
  await page.evaluate(() => window.__wx.playback.play());
  await page.waitForTimeout(3000);
  await page.evaluate(() => window.__wx.playback.pause());
  s1 = await stats();
  check(s1.steps > s0.steps + 5 && s1.resets - s0.resets <= 2, `playback: particles keep running (${s1.steps - s0.steps} steps, ${s1.resets - s0.resets} resets)`);
  // A different frame picked by hand does.
  s0 = await stats();
  await page.keyboard.press(']');
  await stepsPast(s0.steps + 3);
  s1 = await stats();
  check(s1.resets > s0.resets, 'frame change: particles reset');

  // Stopped when not on wind, or hidden.
  await page.evaluate(() => window.__wx.store.set({ layerId: 'temp' }));
  await page.waitForTimeout(500);
  s0 = await stats();
  await page.waitForTimeout(1000);
  s1 = await stats();
  check(s1.steps === s0.steps && !s1.running, 'other layers: particles stop');
  await page.evaluate(() => window.__wx.store.set({ layerId: 'wind' }));
  await waitWeather(page);
  await page.evaluate(() => {
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await page.waitForTimeout(500);
  s0 = await stats();
  await page.waitForTimeout(1000);
  s1 = await stats();
  check(s1.steps === s0.steps, 'hidden page: particles stop');
  await page.evaluate(() => {
    delete document.visibilityState;
    document.dispatchEvent(new Event('visibilitychange'));
  });

  // Context loss: the layer comes back and moves again.
  await stepsPast(s1.steps + 3);
  s0 = await stats();
  await page.evaluate(async () => {
    const ext = window.__wx.map.map.getCanvas().getContext('webgl2').getExtension('WEBGL_lose_context');
    ext.loseContext();
    await new Promise((r) => setTimeout(r, 600));
    ext.restoreContext();
  });
  await page.waitForFunction(() => window.__wx.map.map.getLayer('wind-particles'), null, { timeout: 15_000 }).catch(() => {});
  await waitWeather(page).catch(() => {});
  const back = await stepsPast(s0.steps + 10);
  const again = await captures();
  check(
    back && again.drawn > 0.02 && again.moving > 0.01,
    `context loss and restore: particles draw and move again (${(again.drawn * 100).toFixed(1)}% drawn, ${(again.moving * 100).toFixed(1)}% moved)`,
  );
  done();
  await ctx.close();

  // Reduced motion, and a device without float render targets: arrows.
  for (const [label, opts, q] of [
    ['reduced motion', { reducedMotion: 'reduce' }, ''],
    ['no float textures', {}, '&particles=nofloat'],
  ]) {
    const c = await browser.newContext({ viewport: { width: 1280, height: 800 }, ...opts });
    const p = await c.newPage();
    const fin = watchConsole(p, label);
    await p.goto(`${base}/?debug${q}`);
    await waitWeather(p);
    await p.evaluate(() => window.__wx.store.set({ layerId: 'wind', windMarks: 'particles' }));
    await waitWeather(p);
    await p.waitForTimeout(800);
    const on = await p.screenshot();
    const info = await p.evaluate(() => ({ ...window.__wx.state(), steps: window.__wx.map.particles.stats.steps }));
    await p.evaluate(() => window.__wx.store.set({ windMarks: 'off' }));
    await p.waitForTimeout(600);
    const none = await p.screenshot();
    await p.click('.layer-btn');
    const note = await p.locator('.menu-note').textContent({ timeout: 3000 }).catch(() => null);
    check(
      info.windMarksShown === 'arrows' && info.steps === 0 && (await diffFraction(p, on, none)) > 0.01 && note === null,
      `${label}: particles chosen, arrows drawn instead (${info.windMarksShown}, ${info.steps} steps)`,
    );
    await p.keyboard.press('Escape');
    await p.evaluate(() => window.__wx.store.set({ windMarks: 'particles' }));
    await p.click('.layer-btn');
    const note2 = await p.locator('.menu-note').textContent({ timeout: 3000 }).catch(() => null);
    check(!!note2, `${label}: the layer menu says why ("${note2}")`);
    fin();
    await c.close();
  }
}

/** The Rust decoder runs as WebAssembly when built; without it (or when it is
 *  switched off or broken) the TypeScript decoder draws the same map. */
async function decoders() {
  console.log('tile decoders');
  const built = existsSync(join(webDir, 'public/wasm/wx_tile_wasm.wasm'));
  const ctx = await browser.newContext({ viewport: { width: 1024, height: 700 } });
  for (const [q, want] of [
    ['', built ? 'wasm' : 'typescript'],
    ['&wasm=off', 'typescript'],
    ['&wasm=/wasm/missing.wasm', 'typescript'],
  ]) {
    const page = await ctx.newPage();
    const done = watchConsole(page, `decoder${q || ' (default)'}`);
    await page.goto(`${base}/?debug${q}`);
    await waitWeather(page);
    await page.waitForFunction(() => window.__wx.decoder(), null, { timeout: 10_000 }).catch(() => {});
    const got = await page.evaluate(() => window.__wx.decoder());
    const on = await page.screenshot();
    await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'none'));
    await page.waitForTimeout(300);
    const off = await page.screenshot();
    const f = await diffFraction(page, on, off);
    check(got === want && f > 0.005, `${q || 'default'}: ${got} decoder draws the radar (${(f * 100).toFixed(1)}%)${!built && !q ? ' [module not built]' : ''}`);
    done();
    await page.close();
  }
  await ctx.close();
}

/** A connection that stays open but goes silent is noticed and replaced. */
async function silentSocket() {
  console.log('silent connection');
  const ctx = await browser.newContext({ viewport: { width: 1024, height: 700 } });
  const page = await ctx.newPage();
  const done = watchConsole(page, 'silent connection');
  await page.goto(`${base}/?debug&livestale=6000`);
  await waitWeather(page);
  await page.waitForFunction(() => window.__wx.live()?.opens >= 1, null, { timeout: 10_000 });
  const before = await page.evaluate(() => ({ ...window.__wx.live() }));
  await page.request.post(`${base}/__mock/silence`);
  await page.waitForFunction((n) => window.__wx.live().opens > n && window.__wx.state().live === 'open', before.opens, { timeout: 25_000 }).catch(() => {});
  const after = await page.evaluate(() => ({ ...window.__wx.live(), state: window.__wx.state().live }));
  check(after.staleCloses > before.staleCloses && after.opens > before.opens && after.state === 'open', `a silent connection is dropped and replaced (${JSON.stringify({ staleCloses: after.staleCloses, opens: after.opens })})`);
  done();
  await ctx.close();
}

/** An idle map under a small memory budget makes no requests once settled,
 *  and a frame replaced by a newer run does not blank the map. */
async function caching() {
  console.log('tile cache');
  const ctx = await browser.newContext({ viewport: { width: 1920, height: 1080 }, timezoneId: 'America/Chicago' });
  const page = await ctx.newPage();
  const done = watchConsole(page, 'tile cache');
  await page.goto(`${base}/?debug&tilebudget=48`);
  await waitWeather(page);
  await page.evaluate(() => window.__wx.store.set({ layerId: 'wind' }));
  await waitWeather(page);
  // Held on the frame shown: following "now", an hourly layer moves on to
  // the next hour's frame at half past, which is a real change, not churn.
  await page.evaluate(() => window.__wx.store.set({ following: false, targetTime: window.__wx.map.layer.getDisplay().a.t }));
  await waitWeather(page);
  // Let prefetching finish.
  await page.waitForFunction(() => window.__wx.map.tiles.inFlight === 0, null, { timeout: 30_000, polling: 500 }).catch(() => {});
  await page.waitForTimeout(3000);
  const idle = await tileRequestsDuring(page, 10_000);
  const used = await page.evaluate(() => ({ bytes: window.__wx.map.tiles.usedBytes, budget: window.__wx.map.tiles.opts.budgetBytes }));
  check(idle === 0, `idle map under a 48 MB budget makes no tile requests (${idle} in 10 s; ${(used.bytes / 1048576).toFixed(0)} MB held)`);

  // Park on a forecast frame over the US, then let a new HRRR run replace it.
  await page.evaluate(() => {
    window.__wx.store.set({ layerId: 'temp' });
    window.__wx.map.map.jumpTo({ center: [-96, 38.5], zoom: 4.3 });
  });
  // The layer's timeline loads when it is first shown.
  await page.waitForFunction(() => window.__wx.frames().length > 0, null, { timeout: 15_000 });
  await page.evaluate(() => {
    const t = window.__wx.frames().find((x) => x.stack.some((p) => p.source === 'hrrr') && x.t > Date.now() + 4 * 3600e3);
    window.__wx.store.set({ following: false, targetTime: t.t });
  });
  await waitWeather(page);
  await page.waitForTimeout(500);
  const id0 = await page.evaluate(() => window.__wx.map.layer.getDisplay().a.parts.join(','));
  await page.evaluate(() => {
    window.__wxMissing = 0;
    const tick = () => {
      window.__wxMissing = Math.max(window.__wxMissing, window.__wx.map.layer.stats.missing);
      if (window.__wxWatch) requestAnimationFrame(tick);
    };
    window.__wxWatch = true;
    requestAnimationFrame(tick);
  });
  await page.request.post(`${base}/__mock/advance?minutes=60`);
  await page.waitForFunction((id) => window.__wx.map.layer.getDisplay().a.parts.join(',') !== id, id0, { timeout: 15_000 }).catch(() => {});
  await page.waitForTimeout(2500);
  const r = await page.evaluate(() => {
    window.__wxWatch = false;
    return { missing: window.__wxMissing, a: window.__wx.map.layer.getDisplay().a.parts.join(',') };
  });
  check(r.a !== id0 && r.missing === 0, `replaced frame: no blank tiles while the new run loads (${id0} -> ${r.a}, worst ${r.missing} empty slots)`);
  done();
  await ctx.close();
}

/** The service worker shows pushed warnings, and a notification's URL opens
 *  the app on that alert. */
async function notifications() {
  console.log('push and notification links');
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, permissions: ['notifications'], timezoneId: 'America/Chicago' });
  const page = await ctx.newPage();
  const done = watchConsole(page, 'notifications');
  const cdp = await ctx.newCDPSession(page);
  const regs = [];
  cdp.on('ServiceWorker.workerRegistrationUpdated', (e) => regs.push(...e.registrations));
  await cdp.send('ServiceWorker.enable');
  await page.goto(`${base}/?debug`);
  await page.evaluate(() => navigator.serviceWorker.ready.then(() => true));
  await page.waitForTimeout(300);
  const origin = new URL(base).origin;
  const reg = regs.find((r) => r.scopeURL === `${origin}/` && !r.isDeleted);
  check(!!reg, 'service worker registered at /');
  if (reg) {
    const payload = { title: 'Tornado Warning', body: 'Home: Tornado Warning until 8:02 AM CST', tag: 'evt-1', url: '/?alert=x', alert_id: 'x', severity: 'Extreme' };
    await cdp.send('ServiceWorker.deliverPushMessage', { origin, registrationId: reg.registrationId, data: JSON.stringify(payload) });
    await page.waitForTimeout(800);
    const shown = await page.evaluate(async () => (await (await navigator.serviceWorker.ready).getNotifications()).map((n) => ({ title: n.title, body: n.body, url: n.data?.url })));
    check(shown.some((n) => n.title === 'Tornado Warning' && n.body.startsWith('Home') && n.url === '/?alert=x'), `push shows a notification from the payload (${JSON.stringify(shown)})`);
  }
  const alert = (await (await page.request.get(`${base}/api/alerts`)).json()).features.find((f) => f.properties.event === 'Flash Flood Warning');
  await page.goto(`${base}/?debug&alert=${encodeURIComponent(alert.properties.id)}&lat=32.75&lon=-97.1`);
  await page.waitForFunction(() => window.__wx?.state().panel?.kind === 'alert', null, { timeout: 15_000 }).catch(() => {});
  // The camera flies there; wait for it to arrive rather than a fixed time.
  await page.waitForFunction(() => window.__wx.map && !window.__wx.map.map.isMoving() && window.__wx.map.map.getZoom() > 6, null, { timeout: 15_000 }).catch(() => {});
  const after = await page.evaluate(() => ({ panel: window.__wx.state().panel, c: window.__wx.map.map.getCenter(), z: window.__wx.map.map.getZoom() }));
  check(after.panel?.id === alert.properties.id && Math.abs(after.c.lat - 32.75) < 1 && Math.abs(after.c.lng + 97.1) < 1.5 && after.z > 6, 'a notification link opens the app at its alert');
  await shot(page, '18-notification-link');
  done();
  await ctx.close();
}

async function phone() {
  console.log('phone 390x844');
  const ctx = await browser.newContext({
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true,
    timezoneId: 'America/Chicago',
    locale: 'en-US',
  });
  const page = await ctx.newPage();
  const done = watchConsole(page, 'phone');
  await page.goto(`${base}/?debug`);
  await waitWeather(page);
  await shot(page, '20-phone-radar');
  const withWx = await page.screenshot();
  await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'none'));
  await page.waitForTimeout(300);
  const without = await page.screenshot();
  await page.evaluate(() => window.__wx.map.map.setLayoutProperty('weather', 'visibility', 'visible'));
  check((await diffFraction(page, withWx, without)) > 0.005, 'phone: radar layer draws');

  await page.tap('.layer-btn');
  await page.waitForSelector('.layer-menu');
  await page.waitForTimeout(500);
  await shot(page, '21-phone-layers');
  await page.tap('button[aria-label="Close"]');

  const okc = await page.evaluate(() => window.__wx.map.map.project([-97.52, 35.47]));
  await page.touchscreen.tap(okc.x, okc.y);
  await page.waitForSelector('.sheet', { timeout: 10_000 });
  await page.waitForTimeout(1200);
  await shot(page, '22-phone-panel');
  await page.tap('button[aria-label="Close"]');

  await page.tap('button[aria-label="Read values under a crosshair"]');
  await page.waitForTimeout(600);
  await shot(page, '23-phone-probe');
  await page.tap('button[aria-label="Read values under a crosshair"]');

  // Long layer titles give way; the buttons on the right stay on screen.
  const clipped = [];
  for (const id of ['rain_24h', 'precip', 'temp']) {
    await page.evaluate((id) => window.__wx.store.set({ layerId: id }), id);
    await page.waitForTimeout(400);
    const r = await page.evaluate(() =>
      ['Saved places', 'Notifications', 'Settings'].map((l) => {
        const b = document.querySelector(`.topbar button[aria-label="${l}"]`)?.getBoundingClientRect();
        return b ? b.left >= 0 && b.right <= innerWidth : false;
      }),
    );
    if (r.includes(false)) clipped.push(id);
  }
  await shot(page, '24-phone-long-title');
  check(clipped.length === 0, `phone: settings and the bell stay on screen with long layer titles (${clipped.join(', ') || 'none clipped'})`);
  await page.tap('button[aria-label="Settings"]');
  await page.waitForSelector('.sheet', { timeout: 5000 }).catch(() => {});
  check((await page.locator('.sheet').count()) > 0, 'phone: settings open with a long layer title');
  done();
  await ctx.close();
}

async function statusPage() {
  console.log('status page');
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, timezoneId: 'America/Chicago' });
  const page = await ctx.newPage();
  const done = watchConsole(page, 'status');
  await page.goto(`${base}/status.html`);
  await page.waitForSelector('.feed', { timeout: 10_000 });
  const n = await page.locator('.feed').count();
  check(n >= 6, `status page lists every feed (${n})`);
  await page.waitForSelector('.sources tbody tr', { timeout: 10_000 }).catch(() => {});
  const rows = await page.locator('.sources tbody tr').count();
  const silent = await page.locator('.sources tbody tr.source-off').count();
  const registry = (await (await page.request.get(`${base}/api/sources`)).json()).sources.length;
  check(rows === registry && silent === 0, `status page shows every source (${rows} of ${registry} rows, ${silent} holding nothing)`);
  const floats = await page.evaluate(() => document.body.innerText.match(/\d+\.\d{3,}/g) ?? []);
  const km = await page.evaluate(() => document.body.innerText.match(/at about [\d.]+ km/g) ?? []);
  check(floats.length === 0 && km.length > 0, `status page rounds resolutions (${km.slice(0, 3).join('; ')}${floats.length ? `; raw: ${floats.slice(0, 3).join(', ')}` : ''})`);
  await page.waitForTimeout(500);
  await shot(page, '30-status');
  if (shotsDir) await page.screenshot({ path: join(shotsDir, '30b-status-full.png'), fullPage: true });
  done();
  await ctx.close();
}

async function fallback() {
  console.log('fallback basemap');
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, timezoneId: 'America/Chicago' });
  for (const mode of ['off', 'broken']) {
    const page = await ctx.newPage();
    // The broken archive's 404 is the point of this check.
    const done = watchConsole(page, `basemap=${mode}`, mode === 'broken' ? [/status of 404/] : []);
    await page.goto(`${base}/?debug&basemap=${mode}`);
    await waitWeather(page);
    const b = await page.evaluate(() => window.__wx.state().basemap);
    check(b === 'fallback', `basemap=${mode} falls back to outlines`);
    if (mode === 'off') await shot(page, '31-fallback-basemap');
    done();
    await page.close();
  }
  await ctx.close();
}

/** The whole world, the antimeridian, a view of mixed tile zooms, a source
 *  update that changes only some tiles, and the credits that go with them. */
async function world() {
  console.log('world 1440x900');
  // Somewhere else: the first view and the units follow the browser.
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'Europe/Berlin', locale: 'de-DE' });
  const page = await ctx.newPage();
  const done = watchConsole(page, 'world');
  const timelines = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    if (u.pathname.startsWith('/api/frames')) timelines.push(u.pathname);
  });
  await page.goto(`${base}/?debug`);
  await waitWeather(page);
  check(timelines.length === 1 && timelines[0] === '/api/frames/radar', `only the shown layer's timeline is loaded at startup (${JSON.stringify(timelines)})`);
  const first = await page.evaluate(() => ({ c: window.__wx.map.map.getCenter(), z: window.__wx.map.map.getZoom(), units: window.__wx.store.get().units }));
  check(
    Math.abs(first.c.lng - 13.4) < 2 && Math.abs(first.c.lat - 52.5) < 2 && first.z > 3 && first.units.temp === 'C' && first.units.speed === 'kmh',
    `Berlin time zone and German locale: opens on Europe in metric units (${first.c.lng.toFixed(1)}, ${first.c.lat.toFixed(1)} z${first.z.toFixed(1)}, ${JSON.stringify(first.units)})`,
  );
  const euCredits = await page.textContent('.maplibregl-ctrl-attrib-inner');
  check(/EUMETNET OPERA/.test(euCredits ?? '') && /NOAA\/CPC CMORPH2/.test(euCredits ?? '') && !/MRMS/.test(euCredits ?? ''), `credits over Europe: OPERA and CMORPH2, not MRMS ("${euCredits}")`);

  // The whole world, more than once across a wide screen.
  for (const [layerId, name] of [
    ['temp', '40-world-temperature'],
    ['radar', '41-world-radar'],
  ]) {
    await page.evaluate((id) => {
      window.__wx.store.set({ layerId: id, following: true });
      window.__wx.map.map.jumpTo({ center: [10, 20], zoom: 1.2 });
    }, layerId);
    await waitWeather(page);
    await shot(page, name);
    const { on, off } = await withAndWithout(page);
    const frac = await diffFraction(page, on, off);
    const info = await page.evaluate(() => ({ zooms: window.__wx.map.layer.stats.zooms, missing: window.__wx.map.layer.stats.missing, credits: window.__wx.map.credits.text }));
    check(frac > (layerId === 'temp' ? 0.5 : 0.03) && info.missing === 0, `world view, ${layerId}: draws (${(frac * 100).toFixed(0)}% of pixels), tile zooms ${info.zooms}`);
    if (layerId === 'temp') check(timelines.includes('/api/frames/temp') && timelines.filter((t) => t === '/api/frames/temp').length === 1, `a layer's timeline loads when it is first shown (${JSON.stringify(timelines)})`);
    if (layerId === 'temp') {
      check(/NOAA\/NCEP GFS/.test(info.credits) && /DWD ICON-EU/.test(info.credits) && /NOAA\/NCEP HRRR/.test(info.credits), `world credits list the models in view ("${info.credits}")`);
      // HRRR and HRRR Alaska share a credit: it is listed once.
      const listed = (/Weather: ([^|]*)/.exec(info.credits)?.[1] ?? '').split(',').map((x) => x.trim());
      check(listed.length === new Set(listed).size, `each credit is listed once (${JSON.stringify(listed)})`);
      // The label names what is at the centre (the Sahara: the global model), not the frame's top part.
      await page.waitForFunction(() => /GFS/.test(document.querySelector('.kind-badge')?.textContent ?? ''), null, { timeout: 8000 }).catch(() => {});
      const label = await page.textContent('.kind-badge');
      check(/GFS .*and \d+ others?/.test(label ?? '') && !/Alaska/.test(label ?? ''), `the time bar names the centre's source and counts the rest ("${label}")`);
    }
    else check(/NOAA\/CPC CMORPH2/.test(info.credits) && /NOAA\/NSSL MRMS/.test(info.credits) && /EUMETNET OPERA/.test(info.credits), `world radar credits ("${info.credits}")`);
  }

  // The infrared mosaic from five geostationary imagers, and its legend.
  await page.evaluate(() => {
    window.__wx.store.set({ layerId: 'satellite', following: true });
    window.__wx.map.map.jumpTo({ center: [10, 15], zoom: 1.2 });
  });
  await waitWeather(page);
  await shot(page, '46-world-satellite');
  {
    const { on, off } = await withAndWithout(page);
    const frac = await diffFraction(page, on, off);
    const info = await page.evaluate(() => ({ legend: document.querySelector('.legend-title')?.textContent, unit: document.querySelector('.legend-unit')?.textContent, words: [...document.querySelectorAll('.legend-words span')].map((e) => e.textContent), credits: window.__wx.map.credits.text }));
    check(frac > 0.2 && info.legend === 'Cloud-top temperature' && info.words.at(-1) === 'Storm tops', `satellite layer draws (${(frac * 100).toFixed(0)}%) with a legend that reads cold as high (${JSON.stringify(info.words)}, ${info.unit})`);
    check(['NOAA GOES-19', 'NOAA GOES-18', 'EUMETSAT Meteosat', 'JMA Himawari-9'].every((c) => info.credits.includes(c)), `satellite credits ("${info.credits}")`);
    await page.click('.layer-btn');
    await page.waitForSelector('.layer-menu');
    const sky = await page.locator('.menu-group', { hasText: 'Sky' }).textContent();
    await page.waitForTimeout(300);
    await shot(page, '47-layer-menu');
    check(/Satellite/.test(sky ?? '') && /Cloud cover/.test(sky ?? ''), 'the layer menu lists Satellite under Sky');
    await page.keyboard.press('Escape');
  }

  // Alaska's grid crosses the antimeridian: drawn and named on both sides,
  // and panning across the line leaves no seam.
  await page.evaluate(() => {
    window.__wx.store.set({ layerId: 'temp', following: true });
    window.__wx.map.map.jumpTo({ center: [180, 60], zoom: 3.8 });
  });
  await waitWeather(page);
  await shot(page, '42-dateline');
  const westSide = await hoverAt(page, 172, 60);
  const eastSide = await hoverAt(page, -170, 62);
  check(/HRRR Alaska/.test(westSide.source ?? '') && /HRRR Alaska/.test(eastSide.source ?? ''), `dateline: read-out names HRRR Alaska on both sides ("${westSide.text} ${westSide.source}" | "${eastSide.text} ${eastSide.source}")`);
  await page.mouse.move(5, 450);
  let seams = [];
  for (let k = 0; k < 4; k++) {
    await waitWeather(page);
    const { on, off } = await withAndWithout(page);
    const line = await page.evaluate(() => {
      const m = window.__wx.map.map;
      // Wherever the line is on screen now: the copy of 180 nearest the centre.
      const c = m.getCenter().lng;
      const lon = 180 + 360 * Math.round((c - 180) / 360);
      return { x: m.project([lon, 60]).x, y0: m.project([lon, 68]).y, y1: m.project([lon, 52]).y, missing: window.__wx.map.layer.stats.missing };
    });
    const strip = await diffIn(page, on, off, [line.x - 3, line.y0, line.x + 3, line.y1]);
    seams.push({ x: Math.round(line.x), strip: Number(strip.toFixed(2)), missing: line.missing });
    await page.evaluate(() => window.__wx.map.map.panBy([170, 0], { duration: 0 }));
  }
  await shot(page, '43-dateline-panned');
  // Labels and coastlines drawn over the weather account for the rest; a
  // seam would leave the strip all but unchanged.
  check(seams.every((s) => s.strip > 0.7 && s.missing === 0), `dateline: no seam while panning across it (${JSON.stringify(seams)})`);

  // The western edge of the US radar's box at radar zoom: deep ground-radar
  // tiles next to coarse satellite ones.
  await page.evaluate(() => {
    window.__wx.store.set({ layerId: 'radar', following: true });
    window.__wx.map.map.jumpTo({ center: [-130, 45], zoom: 6.4 });
  });
  await waitWeather(page);
  await shot(page, '44-mixed-zoom');
  const mixed = await page.evaluate(() => ({ zooms: window.__wx.map.layer.stats.zooms, missing: window.__wx.map.layer.stats.missing }));
  check(mixed.zooms.length >= 2 && mixed.missing === 0, `mixed-zoom view draws tiles of zooms ${mixed.zooms}`);
  // Past the radar's box only the satellite estimate is there, certainly;
  // just off the coast the radar's own answer is needed.
  const sea = await hoverAt(page, -134.5, 45);
  const near = await hoverAt(page, -124.5, 45);
  check(sea.source === 'From CMORPH2' && near.source === 'From MRMS', `read-out names the source either side of the radar's edge ("${sea.source}" | "${near.source}")`);
  // Where only the satellite estimate looks, it saw no rain; it is no radar.
  const dry = sea.text === 'No echo' || /No rain seen/.test(sea.text ?? '');
  check(dry ? /No rain seen \(satellite estimate\)/.test(sea.text ?? '') : /dBZ/.test(sea.text ?? ''), `satellite-only reading worded as such ("${sea.text}")`);
  // In the blend at the radar's edge both sources are named.
  await page.mouse.move(5, 450);
  await page.evaluate(() => window.__wx.map.map.jumpTo({ center: [-125.75, 42], zoom: 7 }));
  await waitWeather(page);
  const blend = await hoverAt(page, -125.75, 42);
  check(/MRMS and CMORPH2/.test(blend.source ?? ''), `a blend zone names both sources ("${blend.source}")`);
  await page.mouse.move(5, 450);

  // A radar update over the Atlantic view: new ground-radar tiles, the
  // satellite-only ones shared with the frame before and not fetched again.
  await page.evaluate(() => window.__wx.map.map.jumpTo({ center: [-35, 35], zoom: 2.6 }));
  await waitWeather(page);
  let update = null;
  for (let attempt = 0; attempt < 3 && !update?.ok; attempt++) {
    await page.waitForFunction(() => window.__wx.map.tiles.inFlight === 0, null, { timeout: 20_000, polling: 200 }).catch(() => {});
    const before = await page.evaluate(() => ({ urls: window.__wx.map.layer.urlsInView(), started: window.__wx.map.tiles.stats.started, t: window.__wx.map.layer.getDisplay().a.t }));
    await page.request.post(`${base}/__mock/advance?minutes=2`);
    await page.waitForFunction((t) => window.__wx.map.layer.getDisplay().a.t !== t, before.t, { timeout: 10_000 }).catch(() => {});
    await waitWeather(page);
    const after = await page.evaluate((started) => {
      const st = window.__wx.map.tiles.stats;
      return { urls: window.__wx.map.layer.urlsInView(), fetched: st.recent.slice(st.recent.length - (st.started - started)) };
    }, before.started);
    const old = new Set(before.urls);
    const fetched = new Set(after.fetched);
    const shared = after.urls.filter((u) => old.has(u));
    const changed = after.urls.filter((u) => !old.has(u));
    const refetched = shared.filter((u) => fetched.has(u));
    update = { ok: shared.length > 0 && changed.length > 0 && refetched.length === 0 && changed.every((u) => fetched.has(u)), shared: shared.length, changed: changed.length, refetched: refetched.length, fetched: fetched.size };
  }
  check(update.ok, `source update: only the tiles it changed are fetched (${JSON.stringify(update)})`);
  await shot(page, '45-after-radar-update');

  const misses = await refused(page);
  check(misses.length === 0, `the server accepted every stack the client asked for (${misses.length} refused${misses.length ? `: ${JSON.stringify(misses.slice(0, 3))}` : ''})`);
  done();
  await ctx.close();
}

/** Worldwide alerts (vector tiles, unshaped markers, languages, a revision
 *  bump), stations by area with their credits and buoys, and a point
 *  forecast outside the US. */
async function worldOverlays() {
  console.log('world overlays 1440x900');
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, timezoneId: 'Europe/Zurich', locale: 'de-CH' });
  const page = await ctx.newPage();
  const done = watchConsole(page, 'world overlays');
  await page.goto(`${base}/?debug`);
  await waitWeather(page);
  await page.evaluate(() => window.__wx.store.set({ toasts: [] }));
  const listed = () => page.evaluate(() => window.__wx.store.get().alerts?.features ?? []);
  await page.waitForFunction(() => (window.__wx.store.get().alerts?.features.length ?? 0) > 0, null, { timeout: 10_000 }).catch(() => {});
  await page.waitForFunction(() => window.__wx.map.map.areTilesLoaded(), null, { timeout: 10_000 }).catch(() => {});
  await page.waitForTimeout(800);
  await shot(page, '70-europe-alerts');
  const eu = await page.evaluate(() => {
    const m = window.__wx.map.map;
    const f = m.queryRenderedFeatures({ layers: ['alerts-fill'] });
    return { n: f.length, nuts: f.filter((x) => x.properties.geom_source === 'nuts').length, credits: window.__wx.map.credits.text, src: m.getSource('alerts').tiles?.[0] };
  });
  check(eu.n > 0 && eu.nuts > 0 && /EuroGeographics/.test(eu.credits) && /\/api\/alerts\/tiles\/\d+\//.test(eu.src ?? ''), `European alerts drawn from vector tiles, NUTS shapes credited (${eu.n} drawn, ${eu.nuts} from NUTS; "${eu.credits}")`);

  // An alert without a shape: a marker, and the detail says so.
  const unshaped = (await page.evaluate(async () => (await (await fetch('/api/alerts?unshaped=true&limit=5000')).json()).features.find((f) => !f.geometry)))?.properties;
  check(!!unshaped?.anchor, `the list has an alert without a shape (${unshaped?.id})`);
  if (unshaped?.anchor) {
    await page.evaluate((a) => window.__wx.map.map.jumpTo({ center: a, zoom: 5 }), unshaped.anchor);
    await page.waitForFunction((id) => window.__wx.store.get().alerts?.features.some((f) => f.properties.id === id), unshaped.id, { timeout: 10_000 }).catch(() => {});
    await page.waitForTimeout(800);
    const pt = await page.evaluate((a) => window.__wx.map.map.project(a), unshaped.anchor);
    const marker = await page.evaluate(() => window.__wx.map.map.queryRenderedFeatures({ layers: ['alerts-unshaped'] }).length);
    await page.mouse.click(pt.x, pt.y);
    await page.waitForSelector('#alert-title', { timeout: 10_000 }).catch(() => {});
    await page.waitForTimeout(1200);
    const panel = await page.evaluate(() => ({ s: window.__wx.state().panel, text: document.querySelector('.sheet')?.textContent ?? '' }));
    await shot(page, '71-unshaped-alert');
    check(marker > 0 && panel.s?.kind === 'alert' && panel.s.id === unshaped.id && /shape|named region|region named/i.test(panel.text), `an unshaped alert shows as a marker and its detail says only the region is known (${marker} marker)`);
    await page.keyboard.press('Escape');
  }

  // Several languages: the browser's (German here) is shown, the others offered.
  const all = (await page.evaluate(async () => (await (await fetch('/api/alerts?unshaped=true&limit=5000')).json()).features.map((f) => f.properties.id)));
  const details = await page.evaluate(async (ids) => Promise.all(ids.map(async (id) => (await fetch(`/api/alerts/detail?id=${encodeURIComponent(id)}`)).json())), all);
  const multi = details.find((d) => d.infos?.length >= 3 && d.infos.some((i) => i.language.startsWith('de')));
  const arabic = details.find((d) => d.infos?.some((i) => i.language.startsWith('ar')));
  check(!!multi && !!arabic, `alerts in several languages, one of them Arabic (${multi?.id}, ${arabic?.id})`);
  if (multi) {
    await page.evaluate((id) => window.__wx.store.set({ panel: { kind: 'alert', id } }), multi.id);
    await page.waitForSelector('#alert-title', { timeout: 10_000 });
    await page.waitForTimeout(1200);
    const lang = await page.evaluate(() => ({ shown: document.querySelector('.sheet .alert-text[lang]')?.getAttribute('lang') ?? null, offers: document.querySelectorAll('.sheet .alert-langs button').length }));
    await shot(page, '72-alert-languages');
    check(/^de/.test(lang.shown ?? '') && lang.offers >= 2, `a multi-language alert opens in the browser's language and offers the others (${JSON.stringify(lang)})`);
  }
  if (arabic) {
    await page.evaluate((id) => window.__wx.store.set({ panel: { kind: 'alert', id } }), arabic.id);
    await page.waitForTimeout(400);
    const ar = await page.evaluate(() => {
      const buttons = [...document.querySelectorAll('.sheet .alert-langs button')].filter((b) => /arab|عرب/i.test(b.textContent ?? ''));
      buttons[0]?.click();
      return buttons.length;
    });
    await page.waitForTimeout(500);
    const rtl = await page.evaluate(() => {
      const el = document.querySelector('.sheet .alert-text[lang^="ar"]');
      const title = document.querySelector('#alert-title .alert-text');
      const h2 = document.querySelector('#alert-title');
      // A right-to-left title lines up on the right of its heading.
      const right = title && h2 ? h2.getBoundingClientRect().right - title.getBoundingClientRect().right < 2 && getComputedStyle(title).direction === 'rtl' : false;
      return { dir: el?.getAttribute('dir') ?? null, direction: el ? getComputedStyle(el).direction : null, titleRight: right, overflow: document.querySelector('.sheet')?.scrollWidth > document.querySelector('.sheet')?.clientWidth + 1 };
    });
    await shot(page, '73-alert-arabic');
    check(ar > 0 && rtl.dir === 'auto' && rtl.direction === 'rtl' && rtl.titleRight && !rtl.overflow, `Arabic text, title included, lays out right to left without breaking the panel (${JSON.stringify(rtl)})`);
    await page.keyboard.press('Escape');
  }
  // An alert whose sender gave no end: the server's assumed day is hedged.
  const assumed = details.find((d) => d.expires_assumed);
  if (assumed) {
    await page.evaluate((id) => window.__wx.store.set({ panel: { kind: 'alert', id } }), assumed.id);
    await page.waitForSelector('#alert-title', { timeout: 10_000 });
    await page.waitForFunction(() => /Until/.test(document.querySelector('.sheet')?.textContent ?? ''), null, { timeout: 5000 }).catch(() => {});
    const until = await page.evaluate(() => [...document.querySelectorAll('.sheet dt')].find((d) => d.textContent === 'Until')?.nextElementSibling?.textContent ?? null);
    await shot(page, '73-alert-assumed-end');
    check(/^About /.test(until ?? '') && /no end time/.test(until ?? ''), `an assumed end reads "until about" (${until?.slice(0, 60)})`);
    await page.keyboard.press('Escape');
  } else check(false, 'an alert with an assumed end exists');

  // A CAP Update: a new revision, and the tiles' URL follows it.
  const rev0 = await page.evaluate(() => window.__wx.store.get().alertsRevision);
  const upd = await (await page.request.post(`${base}/__mock/alert-update`)).json();
  await page.waitForFunction((r) => window.__wx.store.get().alertsRevision > r, rev0, { timeout: 10_000 }).catch(() => {});
  const rev1 = await page.evaluate(() => ({ rev: window.__wx.store.get().alertsRevision, url: window.__wx.map.map.getSource('alerts').tiles?.[0] }));
  check(rev1.rev > rev0 && rev1.url.includes(`/tiles/${rev1.rev}/`) && upd.added?.length === 1 && upd.removed?.length === 1, `an alert update bumps the revision and the tile URL (${rev0} -> ${rev1.rev})`);

  // Stations by area: Japan's network with its credit, buoys at sea.
  await page.evaluate(() => {
    window.__wx.store.set({ overlays: { ...window.__wx.store.get().overlays, stations: true } });
    window.__wx.map.map.jumpTo({ center: [138, 37], zoom: 5.2 });
  });
  await page.waitForFunction(() => window.__wx.store.get().stations?.features.some((f) => f.properties.source === 'amedas'), null, { timeout: 10_000 }).catch(() => {});
  await page.waitForTimeout(1500);
  await shot(page, '74-japan-stations');
  const jp = await page.evaluate(() => ({ dots: window.__wx.map.map.queryRenderedFeatures({ layers: ['stations-dot'] }).length, credits: window.__wx.map.credits.text, box: window.__wx.store.get().viewBox }));
  check(jp.dots > 10 && /Japan Meteorological Agency/.test(jp.credits), `stations in view are fetched by area and credited (${jp.dots} shown; "${jp.credits}")`);
  await page.evaluate(() => window.__wx.map.map.jumpTo({ center: [-88, 26], zoom: 5.2 }));
  await page.waitForFunction(() => window.__wx.store.get().stations?.features.some((f) => f.properties.kind === 'buoy'), null, { timeout: 10_000 }).catch(() => {});
  await page.waitForTimeout(1500);
  await shot(page, '75-gulf-buoys');
  const gulf = await page.evaluate(() => ({ buoys: window.__wx.map.map.queryRenderedFeatures({ layers: ['stations-buoy'] }).length, credits: window.__wx.map.credits.text }));
  check(gulf.buoys > 0 && /Data Buoy Center/.test(gulf.credits), `buoys get their own marker and credit (${gulf.buoys})`);
  await page.evaluate(() => window.__wx.map.map.jumpTo({ center: [180, 55], zoom: 3.6 }));
  await page.waitForFunction(() => {
    const b = window.__wx.store.get().viewBox;
    return b && b.west > b.east && window.__wx.store.get().stations?.features.length > 0;
  }, null, { timeout: 10_000 }).catch(() => {});
  const dl = await page.evaluate(() => {
    const st = window.__wx.store.get();
    const lons = (st.stations?.features ?? []).map((f) => f.geometry.coordinates[0]);
    return { box: st.viewBox, east: lons.filter((x) => x > 0).length, west: lons.filter((x) => x < 0).length };
  });
  check(dl.box.west > dl.box.east && dl.east > 0 && dl.west > 0, `stations are asked for across the antimeridian (${JSON.stringify(dl)})`);
  await page.evaluate(() => window.__wx.store.set({ overlays: { ...window.__wx.store.get().overlays, stations: false } }));

  // A point forecast outside the US, credited to its provider.
  await page.evaluate(() => window.__wx.map.map.jumpTo({ center: [36.82, -1.29], zoom: 6 }));
  await page.waitForTimeout(600);
  const nairobi = await page.evaluate(() => window.__wx.map.map.project([36.82, -1.29]));
  await page.mouse.click(nairobi.x, nairobi.y);
  await page.waitForSelector('.point-attribution', { timeout: 15_000 }).catch(() => {});
  await page.waitForSelector('.fchart svg', { timeout: 10_000 }).catch(() => {});
  await page.waitForTimeout(800);
  const fc = await page.evaluate(() => ({ attribution: document.querySelector('.point-attribution')?.textContent ?? null, text: document.querySelector('.sheet')?.textContent ?? '' }));
  await shot(page, '76-point-nairobi');
  check(/MET Norway/.test(fc.attribution ?? '') && !/no forecast for this spot/i.test(fc.text) && !/null|NaN|undefined/.test(fc.text), `a point forecast in Nairobi, credited to MET Norway ("${fc.attribution}")`);
  await page.keyboard.press('Escape');

  const misses = await refused(page);
  check(misses.length === 0, `no refused tile stacks with the overlays (${misses.length})`);
  done();

  // A capped alert list says so.
  const capped = await ctx.newPage();
  const cappedDone = watchConsole(capped, 'capped alerts');
  await capped.goto(`${base}/?debug&alertlimit=3`);
  await waitWeather(capped);
  // A view with plenty of alerts in it (the context remembers the last view).
  await capped.evaluate(() => window.__wx.map.map.jumpTo({ center: [10, 35], zoom: 1.6 }));
  await capped.waitForSelector('.alerts-notice', { timeout: 10_000 }).catch(() => {});
  const notice = await capped.textContent('.alerts-notice').catch(() => null);
  await shot(capped, '77-alerts-capped');
  check(/3 most severe alerts here; zoom in for the rest/.test(notice ?? ''), `a capped alert list is pointed out ("${notice}")`);
  cappedDone();
  await ctx.close();
}

/** With the provenance endpoint failing, the read-out still reads values,
 *  names no source outside solid boxes, and stops asking for a while. */
async function provenanceDown() {
  console.log('provenance endpoint failing');
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, timezoneId: 'America/Chicago', locale: 'en-US' });
  const page = await ctx.newPage();
  // The failing answers are the point; the browser logs each one.
  const done = watchConsole(page, 'provenance down', [/status of 503/]);
  let asked = 0;
  await page.route('**/api/provenance**', (r) => {
    asked++;
    return r.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"busy","message":"try later"}' });
  });
  await page.goto(`${base}/?debug`);
  await waitWeather(page);
  const pts = [[-98, 38], [-97, 37], [-96, 36], [-95, 35], [-94, 34], [-93, 33]];
  // Once the overlays are in, the pointer gets read-outs.
  await page.waitForFunction(() => window.__wx.map.map.getLayer('alerts-fill'), null, { timeout: 10_000 });
  await page.mouse.move(600, 300);
  await page.waitForTimeout(800);
  const toasts = await page.locator('.toast').count();
  // Out of the pointer's way: a toast over the map takes the pointer off it.
  await page.evaluate(() => window.__wx.store.set({ toasts: [] }));
  const seen = [];
  for (const [lon, lat] of pts) {
    const p = await page.evaluate(([lon, lat]) => window.__wx.map.map.project([lon, lat]), [lon, lat]);
    await page.mouse.move(p.x + 1, p.y + 1);
    await page.mouse.move(p.x, p.y);
    await page
      .waitForFunction(([lon, lat]) => {
        const r = window.__wx.store.get().readout;
        return r && Math.abs(r.lon - lon) < 0.1 && Math.abs(r.lat - lat) < 0.1;
      }, [lon, lat], { timeout: 3000 })
      .catch(() => {});
    // Long enough for a source line to arrive if anything were still asking.
    await page.waitForTimeout(500);
    seen.push(await page.evaluate(() => window.__wx.store.get().readout));
  }
  check(
    asked <= 2 && seen.every((r) => r && /dBZ|echo|rain/i.test(r.text) && !r.source) && toasts <= 1,
    `a failing provenance endpoint degrades quietly (${asked} requests for ${pts.length} points, ${toasts} toasts, the live warning's at most; ${JSON.stringify(seen.map((r) => r && [r.text, r.source ?? null]))})`,
  );
  done();
  await ctx.close();
}


/** The review's details: credits clear of the legend, the read-out during
 *  playback, the badge at a centre only a box covers, failed tiles,
 *  shapeless alerts coming and going, blocks no longer in effect. */
async function details() {
  console.log('details 1280x800');
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, timezoneId: 'America/Chicago', locale: 'en-US' });
  const page = await ctx.newPage();
  const done = watchConsole(page, 'details', [/status of 503/]);
  await page.goto(`${base}/?debug`);
  await waitWeather(page);
  await page.evaluate(() => window.__wx.store.set({ toasts: [] }));

  // Credits never under the legend: several views, two widths.
  const covered = [];
  const views = [
    ['radar', [10, 35], 1.6],
    ['temp', [10, 50], 3.5],
    ['radar', [-95, 38], 4],
  ];
  for (const vp of [{ width: 1280, height: 800 }, { width: 820, height: 700 }]) {
    await page.setViewportSize(vp);
    for (const [layerId, center, zoom] of views) {
      await page.evaluate(([layerId, center, zoom]) => {
        window.__wx.store.set({ layerId });
        window.__wx.map.map.jumpTo({ center, zoom });
      }, [layerId, center, zoom]);
      await waitWeather(page);
      await page.waitForTimeout(1200);
      const o = await page.evaluate(() => {
        const a = document.querySelector('.maplibregl-ctrl-attrib')?.getBoundingClientRect();
        const l = document.querySelector('.legend')?.getBoundingClientRect();
        return !!(a && l && a.left < l.right && a.right > l.left && a.top < l.bottom && a.bottom > l.top);
      });
      if (o) covered.push(`${vp.width} ${layerId} ${center}`);
    }
  }
  await shot(page, '80-credits-beside-legend');
  await page.setViewportSize({ width: 1280, height: 800 });
  check(covered.length === 0, `the legend never covers the credits (${covered.join('; ') || 'clear in 6 views'})`);

  // The read-out follows the frames while playing.
  await page.evaluate(() => {
    window.__wx.store.set({ layerId: 'radar' });
    window.__wx.map.map.jumpTo({ center: [-96, 37], zoom: 4.5 });
  });
  await waitWeather(page);
  const pt = await page.evaluate(() => window.__wx.map.map.project([-96, 37]));
  await page.mouse.move(pt.x, pt.y);
  await page.waitForFunction(() => !!window.__wx.store.get().readout, null, { timeout: 5000 }).catch(() => {});
  await page.evaluate(() => window.__wx.playback.play());
  const updates = await page.evaluate(async () => {
    let last = window.__wx.store.get().readout;
    let n = 0;
    const t0 = performance.now();
    while (performance.now() - t0 < 3000) {
      await new Promise((r) => setTimeout(r, 100));
      const r = window.__wx.store.get().readout;
      if (r && r !== last) n++;
      last = r;
    }
    return n;
  });
  await page.evaluate(() => window.__wx.playback.pause());
  check(updates >= 5, `the read-out updates during playback (${updates} updates in 3 s)`);

  // Off Baja MRMS's box reaches but its data does not: the badge never
  // names it, not even for a moment after a step.
  await page.mouse.move(5, 400);
  await page.evaluate(() => {
    // The newest radar observation, stepping back through older ones.
    const s = window.__wx.store.get();
    const obs = s.timelines.radar.frames.filter((f) => f.kind === 'observed');
    window.__wx.store.set({ following: false, targetTime: obs.at(-1).t });
    window.__wx.map.map.jumpTo({ center: [-125, 25], zoom: 5 });
  });
  await waitWeather(page);
  await page.waitForFunction(() => /CMORPH2/.test(document.querySelector('.kind-badge')?.textContent ?? ''), null, { timeout: 5000 }).catch(() => {});
  const badges = await page.evaluate(async () => {
    const seen = new Set([document.querySelector('.kind-badge')?.textContent ?? '']);
    for (let k = 0; k < 3; k++) {
      document.querySelector('button[aria-label="Previous frame"]')?.click();
      const t0 = performance.now();
      while (performance.now() - t0 < 900) {
        await new Promise((r) => setTimeout(r, 40));
        seen.add(document.querySelector('.kind-badge')?.textContent ?? '');
      }
    }
    return [...seen];
  });
  check(badges.every((b) => !/MRMS/.test(b)) && badges.some((b) => /CMORPH2/.test(b)), `the badge names no source whose box only covers the centre (${badges.join(' | ')})`);

  // A layer whose tiles all fail is not ready: nothing to fade into, and
  // the last good frame stays the fallback.
  await page.route('**/tiles/wind/**', (r) => r.fulfill({ status: 503, body: 'busy' }));
  await page.evaluate(() => window.__wx.store.set({ layerId: 'wind', windMarks: 'off' }));
  await page.waitForFunction(() => window.__wx.map.layer.getDisplay().a?.stack.some((p) => p.source), null, { timeout: 10_000 }).catch(() => {});
  await page.waitForTimeout(3000);
  const failing = await page.evaluate(() => {
    const m = window.__wx.map;
    const a = m.layer.getDisplay().a;
    return { ready: m.layer.isReady(a), fallbackIsIt: m.lastReady === a };
  });
  check(!failing.ready && !failing.fallbackIsIt, `a frame whose tiles failed is not ready (${JSON.stringify(failing)})`);
  await page.unroute('**/tiles/wind/**');
  await page.evaluate(() => window.__wx.store.set({ layerId: 'radar', windMarks: 'particles' }));

  // A shapeless alert ends: the event names no ids, the list is fetched again.
  const kenya = await page.evaluate(async () => (await (await fetch('/api/alerts?unshaped=true&limit=5000')).json()).features.find((f) => !f.geometry && f.properties.source === 'ke-kmd-en')?.properties);
  await page.evaluate((a) => window.__wx.map.map.jumpTo({ center: a, zoom: 4 }), kenya?.anchor ?? [37.9, 0]);
  await page.waitForFunction((id) => window.__wx.store.get().alerts?.features.some((f) => f.properties.id === id), kenya?.id, { timeout: 10_000 }).catch(() => {});
  const listedBefore = await page.evaluate((id) => !!window.__wx.store.get().alerts?.features.some((f) => f.properties.id === id), kenya?.id);
  await page.request.post(`${base}/__mock/alert-unshaped`);
  const gone = await page
    .waitForFunction((id) => !window.__wx.store.get().alerts?.features.some((f) => f.properties.id === id), kenya?.id, { timeout: 10_000 })
    .then(() => true, () => false);
  await page.request.post(`${base}/__mock/alert-unshaped`);
  const back = await page
    .waitForFunction((id) => window.__wx.store.get().alerts?.features.some((f) => f.properties.id === id), kenya?.id, { timeout: 10_000 })
    .then(() => true, () => false);
  check(!!kenya && listedBefore && gone && back, `an alerts event with no ids refetches the list (shapeless alert listed ${listedBefore}, gone ${gone}, back ${back})`);

  // A block of the message no longer in effect: offered, greyed, readable.
  const flood = await page.evaluate(async () => (await (await fetch('/api/alerts?unshaped=true&limit=5000')).json()).features.find((f) => f.properties.event === 'Minor Flood Warning')?.properties.id);
  await page.evaluate((id) => window.__wx.store.set({ panel: { kind: 'alert', id } }), flood);
  await page.waitForSelector('.alert-lang', { timeout: 10_000 }).catch(() => {});
  const blocks = await page.evaluate(() => ({ past: document.querySelectorAll('.alert-lang.past').length, note: !!document.querySelector('.alert-past-note'), headline: document.querySelector('.alert-headline')?.textContent ?? '' }));
  await page.click('.alert-lang.past').catch(() => {});
  await page.waitForTimeout(300);
  const picked = await page.evaluate(() => ({ note: !!document.querySelector('.alert-past-note'), greyed: !!document.querySelector('.alert-description.past'), headline: document.querySelector('.alert-headline')?.textContent ?? '' }));
  await shot(page, '81-alert-block-past');
  check(
    blocks.past === 1 && !blocks.note && /Narrabri/.test(blocks.headline) && picked.note && picked.greyed && /eased/.test(picked.headline),
    `an alert block no longer in effect is offered greyed and shown with a note when picked (${JSON.stringify({ blocks, picked })})`,
  );
  await page.keyboard.press('Escape');
  done();
  await ctx.close();
}

const steps = { desktop, world, worldOverlays, details, provenanceDown, resilience, caching, silentSocket, decoders, particles, notifications, phone, statusPage, fallback };
const only = opt('--only')?.split(',');
try {
  for (const [name, step] of Object.entries(steps)) {
    if (!only || only.includes(name)) await step();
  }
} catch (e) {
  failures.push(`exception: ${e.stack ?? e}`);
  console.error(e);
} finally {
  check(outsideHosts.size === 0, `no request leaves the page's own host (${[...outsideHosts].join(', ') || 'none'})`);
  await browser.close();
  await server?.close();
}

if (failures.length) {
  console.error(`\n${failures.length} check(s) failed`);
  process.exit(1);
}
console.log('\nsmoke test passed');
