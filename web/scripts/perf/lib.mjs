// Shared by the measurement scripts in this folder: a server with the mock
// backend (the production build by default, or the dev server), headless
// Chromium with software WebGL as the smoke test runs it, and statistics.

import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

export const webDir = fileURLToPath(new URL('../..', import.meta.url));

const argv = process.argv.slice(2);
export const opt = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : fallback;
};
export const flag = (name) => argv.includes(name);

export const median = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  if (!s.length) return NaN;
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};
export const pct = (xs, p) => {
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
};
export const ms = (x) => (Number.isFinite(x) ? `${x.toFixed(x < 10 ? 2 : 0)} ms` : '-');
export const kb = (n) => `${(n / 1024).toFixed(1)} KB`;

/**
 * The app with the mock backend: `--url` for one already running, else a
 * server started here on `--port`: `vite preview` of dist/ (`--mode preview`,
 * the default; `--build` rebuilds dist/ first; `--dist DIR` serves another
 * build, such as an older one to compare with) or the dev server (`--mode dev`).
 */
export async function startApp({ port = Number(opt('--port', 5187)), env = {} } = {}) {
  const url = opt('--url');
  if (url) return { base: url, close: async () => {} };
  const mode = opt('--mode', 'preview');
  Object.assign(process.env, { VITE_MOCK: '1' }, env);
  const vite = await import('vite');
  const common = { root: webDir, configFile: join(webDir, 'vite.config.ts'), logLevel: 'warn' };
  if (mode === 'dev') {
    const server = await vite.createServer({ ...common, server: { port, strictPort: true, host: '127.0.0.1' } });
    await server.listen();
    return { base: `http://127.0.0.1:${port}`, mode, close: () => server.close() };
  }
  const outDir = opt('--dist', join(webDir, 'dist'));
  if (flag('--build') || !existsSync(join(outDir, 'index.html'))) await vite.build({ ...common, build: { outDir, emptyOutDir: true } });
  const server = await vite.preview({ ...common, build: { outDir }, preview: { port, strictPort: true, host: '127.0.0.1' } });
  return { base: `http://127.0.0.1:${port}`, mode, close: () => new Promise((r) => server.httpServer.close(r)) };
}

/** Headless Chromium with software WebGL, as in the smoke test. */
export function launch() {
  return chromium.launch({
    channel: 'chromium',
    args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--enable-precise-memory-info'],
  });
}

/** Wait until the map exists and the displayed frame's tiles are in. */
export async function waitWeather(page, timeout = 60_000) {
  await page.waitForFunction(
    () => {
      const m = window.__wx?.map;
      if (!m || !m.map.isStyleLoaded()) return false;
      const d = m.layer.getDisplay();
      return d.a && m.layer.isReady(d.a) && m.layer.stats.slots > 0;
    },
    null,
    { timeout, polling: 50 },
  );
}

/** Move the map and wait for the move and the tiles to settle. */
export async function jumpTo(page, center, zoom) {
  await page.evaluate(([center, zoom]) => window.__wx.map.map.jumpTo({ center, zoom }), [center, zoom]);
  await page.waitForTimeout(300);
  await waitWeather(page);
}
