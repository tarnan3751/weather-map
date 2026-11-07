// A Vite dev-server plugin that implements docs/protocol.md with synthetic
// data, so the client can be built without the Rust server. Enabled with
// VITE_MOCK=1 (see vite.config.ts). Also works under `vite preview`, which
// lets the production build be checked against it.

import { createReadStream, existsSync, statSync } from 'node:fs';
import type { IncomingMessage, Server, ServerResponse } from 'node:http';
import { basename, join } from 'node:path';
import type { Duplex } from 'node:stream';
import type { Connect, Plugin } from 'vite';
import { WebSocketServer } from 'ws';
import type { Config, LonLatBox, Place } from '../src/api/types.ts';
import type { StoredPlace } from './devices.ts';
import { pointForecast, stations } from './obs.ts';
import { LAYERS, layerById } from './registry.ts';
import { sourceDefs } from './sources.ts';
import { provenanceAt } from './tiles.ts';
import { TilePool } from './tilePool.ts';
import { World } from './world.ts';

const TILE_CACHE_MAX = 4000;
const IMMUTABLE = 'public, max-age=31536000, immutable';

function json(res: ServerResponse, status: number, body: unknown) {
  res.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
  res.end(JSON.stringify(body));
}

const error = (res: ServerResponse, status: number, code: string, message: string) => json(res, status, { error: code, message });

function noContent(res: ServerResponse, headers?: Record<string, string>) {
  res.writeHead(204, headers);
  res.end();
}

/**
 * `west,south,east,north` as both list endpoints take it: longitudes beyond
 * +-180 (a map panned across the date line) are wrapped, `west > east` then
 * crosses the antimeridian, and a box 360 degrees wide or more is the world.
 * Undefined when absent, null when malformed.
 */
function parseBbox(raw: string | null): LonLatBox | null | undefined {
  if (raw === null) return undefined;
  const v = raw.split(',').map(Number);
  if (v.length !== 4 || !v.every(Number.isFinite)) return null;
  const [w, south, e, north] = v;
  if (south > north || Math.abs(south) > 90 || Math.abs(north) > 90) return null;
  if (e - w >= 360) return { west: -180, south, east: 180, north };
  const wrap = (lon: number) => (lon >= -180 && lon <= 180 ? lon : ((((lon + 180) % 360) + 360) % 360) - 180);
  return { west: wrap(w), south, east: wrap(e), north };
}

/** `lat` and `lon` from the query; null when not numbers or, with
 *  `onGlobe`, off it. */
function latLon(q: URLSearchParams, onGlobe: boolean): [number, number] | null {
  const lat = Number(q.get('lat'));
  const lon = Number(q.get('lon'));
  if (!Number.isFinite(lat) || !Number.isFinite(lon)) return null;
  return onGlobe && (Math.abs(lat) > 90 || Math.abs(lon) > 180) ? null : [lat, lon];
}

async function readJson(req: IncomingMessage): Promise<Record<string, unknown> | null> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const c of req) {
    size += (c as Buffer).length;
    if (size > 64 * 1024) return null;
    chunks.push(c as Buffer);
  }
  if (!chunks.length) return {};
  try {
    const v = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    return v && typeof v === 'object' ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

function serveBasemap(file: string, req: IncomingMessage, res: ServerResponse) {
  const st = statSync(file);
  const size = st.size;
  const headers: Record<string, string | number> = {
    'content-type': 'application/octet-stream',
    'accept-ranges': 'bytes',
    etag: `"${size.toString(16)}-${Math.floor(st.mtimeMs).toString(16)}"`,
    'cache-control': 'no-cache',
  };
  const range = /^bytes=(\d*)-(\d*)$/.exec(req.headers.range ?? '');
  if (!range) {
    res.writeHead(200, { ...headers, 'content-length': size });
    if (req.method === 'HEAD') return void res.end();
    return void createReadStream(file).pipe(res);
  }
  const start = Math.max(0, range[1] === '' ? size - Number(range[2]) : Number(range[1]));
  const end = Math.min(size - 1, range[1] === '' || range[2] === '' ? size - 1 : Number(range[2]));
  if (start > end || start >= size) {
    res.writeHead(416, { 'content-range': `bytes */${size}` });
    return void res.end();
  }
  res.writeHead(206, { ...headers, 'content-range': `bytes ${start}-${end}/${size}`, 'content-length': end - start + 1 });
  if (req.method === 'HEAD') return void res.end();
  createReadStream(file, { start, end }).pipe(res);
}

export function mockBackend(opts: { repoRoot: string }): Plugin {
  let world: World | null = null;
  let pool: TilePool | null = null;
  const tileCache = new Map<string, Promise<Buffer | null>>();
  // The world extract when there is one, else the older US archive.
  const worldFile = join(opts.repoRoot, 'data/basemap/world.pmtiles');
  const basemapFile = process.env.MOCK_BASEMAP_FILE || (existsSync(worldFile) ? worldFile : join(opts.repoRoot, 'data/basemap/us.pmtiles'));
  const basemapPath = `/basemap/${basename(basemapFile)}`;
  const basemapOn = process.env.MOCK_BASEMAP !== '0';
  const latency = Number(process.env.MOCK_LATENCY || 0);

  const setup = (httpServer: Server | null, middlewares: Connect.Server, log: (m: string) => void) => {
    world?.stop();
    pool?.close();
    tileCache.clear();
    const w = new World(log);
    world = w;
    w.start();
    pool = new TilePool(join(opts.repoRoot, 'web/mock/tileWorker.ts'), w.startedAt, log);
    const serverTime = () => new Date(w.now()).toISOString();

    const wss = new WebSocketServer({ noServer: true });
    httpServer?.on('upgrade', (req: IncomingMessage, socket: Duplex, head: Buffer) => {
      if (new URL(req.url ?? '/', 'http://x').pathname !== '/ws') return;
      wss.handleUpgrade(req, socket, head, (ws) => {
        w.clients.add(ws);
        ws.send(JSON.stringify({ type: 'hello', server_time: serverTime() }));
        // Like the server: a heartbeat every 25 seconds, so clients can tell
        // a quiet connection from a dead one.
        const ping = setInterval(() => {
          ws.ping();
          if (!w.silenced.has(ws)) ws.send(JSON.stringify({ type: 'heartbeat', server_time: serverTime() }));
        }, 25_000);
        ws.on('close', () => {
          clearInterval(ping);
          w.clients.delete(ws);
        });
        ws.on('error', () => ws.terminate());
      });
    });
    httpServer?.on('close', () => {
      w.stop();
      pool?.close();
    });

    /** Routes that answer JSON with a 200, by `METHOD path`. */
    const simple: Record<string, (q: URLSearchParams) => unknown> = {
      'POST /__mock/advance': (q) => {
        w.advance(Number(q.get('minutes') ?? 2));
        return { server_time: serverTime() };
      },
      // Stop talking on every open connection without closing it: what a
      // proxy that black-holes a socket looks like to the client.
      'POST /__mock/silence': () => {
        for (const c of w.clients) w.silenced.add(c);
        return { silenced: w.clients.size };
      },
      // Tile requests answered 404, for the smoke test: the client's stacks
      // should always be ones the server accepts.
      'GET /__mock/misses': () => ({ misses: w.tileMisses }),
      'POST /__mock/alert': () => {
        w.clearLiveAlert();
        w.issueLiveAlert();
        return { id: w.alerts.liveId, revision: w.alerts.revision };
      },
      // A shapeless alert ends or returns: a new revision and no ids.
      'POST /__mock/alert-unshaped': () => {
        const r = w.alerts.toggleUnshaped(w.now());
        w.broadcast({ type: 'alerts', revision: r.revision, added: [], updated: [], removed: [] });
        return r;
      },
      // A CAP Update: the alert comes back under a new id.
      'POST /__mock/alert-update': () => {
        const r = w.alerts.updateOne(w.now());
        w.broadcast({ type: 'alerts', revision: r.revision, added: r.added, updated: [], removed: r.removed });
        return r;
      },
      'GET /api/config': (): Config => {
        w.scheduleLiveAlert();
        return {
          tile_url: '/tiles/{layer}/{z}/{x}/{y}/{parts}.wxt',
          tile_cells: 256,
          basemap_url: basemapOn && existsSync(basemapFile) ? basemapPath : null,
          vapid_public_key: w.vapidKey,
          server_time: serverTime(),
        };
      },
      'GET /api/layers': () => ({ layers: LAYERS }),
      'GET /api/sources': () => ({ sources: sourceDefs() }),
      'GET /api/frames': () => ({ server_time: serverTime(), timelines: w.catalog.all() }),
      'GET /api/alerts/tiles.json': () => w.alerts.tileJson(),
      'GET /api/alerts/sources': () => w.alerts.sources(),
      'GET /api/status': () => w.status(),
      'POST /api/device': () => w.devices.register(),
    };

    async function route(url: URL, method: string, req: IncomingMessage, res: ServerResponse) {
      const path = url.pathname;
      const q = url.searchParams;

      if (path === '/healthz') {
        res.writeHead(200, { 'content-type': 'text/plain' });
        return void res.end('ok');
      }

      // ------------------------------------------------------------- tiles
      const tm = /^\/tiles\/([a-z0-9_]+)\/(\d+)\/(\d+)\/(\d+)\/([a-z0-9_,-]+)\.wxt$/.exec(path);
      if (tm) {
        const [, layerId, zs, xs, ys, stack] = tm;
        const layer = layerById(layerId);
        const [z, x, y] = [Number(zs), Number(xs), Number(ys)];
        const ids = stack.split(',');
        if (!layer || z > 14 || x >= 2 ** z || y >= 2 ** z) return error(res, 404, 'not_found', 'unknown layer or tile');
        // The server's rules: every part known, a stack some frame has (or
        // had in the last four minutes) for this tile, and no deeper than it.
        const check = w.catalog.checkStack(layerId, z, x, y, ids, w.now());
        if (check !== 'ok') {
          w.tileMisses.push({ path, reason: check });
          if (w.tileMisses.length > 200) w.tileMisses.splice(0, 100);
          return error(res, 404, check, `no such tile: ${check.replace('_', ' ')}`);
        }
        const key = `${layerId}/${z}/${x}/${y}/${stack}`;
        let job = tileCache.get(key);
        if (!job) {
          job = pool!.render(layer, ids.map((id) => w.catalog.part(layerId, id, w.now())!), z, x, y);
          tileCache.set(key, job);
          job.catch(() => tileCache.delete(key));
          if (tileCache.size > TILE_CACHE_MAX) tileCache.delete(tileCache.keys().next().value as string);
        }
        const tile = await job;
        if (!tile) return noContent(res, { 'cache-control': IMMUTABLE });
        res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-length': tile.length, 'cache-control': IMMUTABLE });
        return void res.end(tile);
      }

      if (path === basemapPath && (method === 'GET' || method === 'HEAD')) {
        if (!basemapOn || !existsSync(basemapFile)) return error(res, 404, 'not_found', 'no basemap archive');
        return serveBasemap(basemapFile, req, res);
      }

      const handler = simple[`${method} ${path}`];
      if (handler) return json(res, 200, handler(q));

      // --------------------------------------------------------------- api
      const fm = /^\/api\/frames\/([a-z0-9_]+)$/.exec(path);
      if (method === 'GET' && fm) {
        if (!layerById(fm[1])) return error(res, 404, 'unknown_layer', 'no such layer');
        return json(res, 200, { layer: fm[1], ...w.catalog.timeline(fm[1]) });
      }
      if (method === 'GET' && path === '/api/alerts') {
        const bbox = parseBbox(q.get('bbox'));
        if (bbox === null) return error(res, 400, 'invalid_bbox', 'bbox is west,south,east,north');
        const country = q.get('country');
        if (country !== null && !/^[a-z]{2}$/.test(country)) return error(res, 400, 'invalid_country', 'country is ISO 3166-1 alpha-2, lower case');
        const limit = q.has('limit') ? Number(q.get('limit')) : 1000;
        if (!Number.isInteger(limit) || limit < 1 || limit > 5000) return error(res, 400, 'invalid_limit', 'limit is 1 to 5000');
        const unshapedRaw = q.get('unshaped');
        if (unshapedRaw !== null && unshapedRaw !== 'true' && unshapedRaw !== 'false') return error(res, 400, 'invalid_query', 'unshaped is true or false');
        // With a country, alerts without a shape are listed unless asked not to.
        const unshaped = unshapedRaw === null ? country !== null : unshapedRaw === 'true';
        return json(res, 200, w.alerts.list({ bbox: bbox ?? null, country, unshaped, limit }));
      }
      const am = /^\/api\/alerts\/tiles\/(\d+)\/(\d+)\/(\d+)\/(\d+)\.mvt$/.exec(path);
      if (method === 'GET' && am) {
        const t = w.alerts.tile(Number(am[1]), Number(am[2]), Number(am[3]), Number(am[4]));
        if (t.status === 404) return error(res, 404, 'not_found', 'no such alert tile');
        res.writeHead(200, {
          'content-type': 'application/vnd.mapbox-vector-tile',
          'content-length': t.body.length,
          'cache-control': t.immutable ? IMMUTABLE : 'no-cache',
        });
        return void res.end(t.body);
      }
      if (method === 'GET' && path === '/api/alerts/detail') {
        const a = w.alerts.detail(q.get('id') ?? '');
        return a ? json(res, 200, a) : error(res, 404, 'not_found', 'unknown alert');
      }
      if (method === 'GET' && path === '/api/alerts/at') {
        const at = latLon(q, false);
        if (!at) return error(res, 400, 'bad_coordinates', 'lat and lon are required');
        return json(res, 200, { alerts: w.alerts.at(at[1], at[0]) });
      }
      if (method === 'GET' && path === '/api/stations') {
        const bbox = parseBbox(q.get('bbox'));
        if (bbox === null) return error(res, 400, 'bad_request', 'bbox is west,south,east,north');
        return json(res, 200, stations(w.now(), bbox ?? null, { raw: q.get('raw') === '1' }));
      }
      if (method === 'GET' && path === '/api/provenance') {
        const layer = layerById(q.get('layer') ?? '');
        const valid = Date.parse(q.get('valid') ?? '');
        const at = latLon(q, true);
        if (!at) return error(res, 400, 'bad_request', 'lon and lat are required');
        const tl = layer ? w.catalog.timeline(layer.id) : null;
        const frame = tl?.frames.find((f) => Date.parse(f.valid) === valid);
        if (!layer || !tl || !frame) return error(res, 404, 'not_found', 'unknown layer, or not a frame');
        const parts = frame.parts.map((id) => tl.parts.find((p) => p.id === id)!).filter(Boolean);
        return json(res, 200, { layer: layer.id, valid: frame.valid, parts: provenanceAt(parts, at[1], at[0]) });
      }
      if (method === 'GET' && path === '/api/point') {
        const at = latLon(q, true);
        if (!at) return error(res, 400, 'bad_request', 'lat and lon are required');
        // The NWS API is not quick; neither is this.
        await new Promise((r) => setTimeout(r, 250));
        const r = pointForecast(at[0], at[1], w.now());
        return json(res, r.status, r.body);
      }

      // ------------------------------------------------- devices and places
      if (path === '/api/places' || path.startsWith('/api/places/') || path.startsWith('/api/push/')) {
        const d = w.devices.device(req.headers.authorization);
        if (!d) return error(res, 401, 'unauthorized', 'unknown device token');
        const view = (p: StoredPlace): Place => ({
          ...p,
          alerts: w.alerts.at(p.lon, p.lat).map(({ id, event, severity, headline, ends }) => ({ id, event, severity, headline, ends })),
        });
        if (path === '/api/places' && method === 'GET') return json(res, 200, { places: [...d.places.values()].map(view) });
        if (path === '/api/places' && method === 'POST') {
          const body = await readJson(req);
          if (!body) return error(res, 400, 'bad_request', 'expected a JSON body');
          const p = w.devices.addPlace(d, body);
          if (p === 'full') return error(res, 409, 'too_many_places', 'at most 20 places per device');
          if (p === 'invalid') return error(res, 400, 'bad_request', 'name, lat and lon are required');
          return json(res, 201, view(p));
        }
        const pm = /^\/api\/places\/([0-9a-f-]+)$/.exec(path);
        if (pm) {
          const p = d.places.get(pm[1]);
          if (!p) return error(res, 404, 'not_found', 'unknown place');
          if (method === 'DELETE') {
            d.places.delete(p.id);
            return noContent(res);
          }
          if (method === 'PATCH') {
            const body = await readJson(req);
            if (!body) return error(res, 400, 'bad_request', 'expected a JSON body');
            if (typeof body.name === 'string' && body.name.trim()) p.name = body.name.trim().slice(0, 80);
            if (typeof body.notify === 'boolean') p.notify = body.notify;
            return json(res, 200, view(p));
          }
        }
        if (path === '/api/push/subscription' && method === 'POST') {
          const body = await readJson(req);
          const keys = body?.keys as { p256dh?: unknown; auth?: unknown } | undefined;
          if (!body || typeof body.endpoint !== 'string' || typeof keys?.p256dh !== 'string' || typeof keys?.auth !== 'string') {
            return error(res, 400, 'bad_request', 'expected a PushSubscription');
          }
          d.subscriptions.set(body.endpoint, { endpoint: body.endpoint, keys: { p256dh: keys.p256dh, auth: keys.auth } });
          return noContent(res);
        }
        if (path === '/api/push/subscription' && method === 'DELETE') {
          const body = await readJson(req);
          if (body && typeof body.endpoint === 'string') d.subscriptions.delete(body.endpoint);
          return noContent(res);
        }
        // Nothing is delivered: the mock has no route to a push service.
        if (path === '/api/push/test' && method === 'POST') return json(res, 200, { sent: d.subscriptions.size, failed: 0 });
      }

      return error(res, 404, 'not_found', `no route for ${method} ${path}`);
    }

    middlewares.use(async (req, res, next) => {
      const url = new URL(req.url ?? '/', 'http://mock');
      const path = url.pathname;
      const method = req.method ?? 'GET';
      if (!['/api/', '/tiles/', '/basemap/', '/__mock/'].some((p) => path.startsWith(p)) && path !== '/healthz') return next();
      if (latency) await new Promise((r) => setTimeout(r, latency * (0.5 + Math.random())));
      try {
        await route(url, method, req, res);
      } catch (e) {
        log(`mock error on ${method} ${path}: ${(e as Error).stack ?? e}`);
        if (!res.headersSent) error(res, 500, 'internal', 'mock backend error');
      }
    });
  };

  const logTo = (server: { config: { logger: { info(m: string): void } } }) => (m: string) => server.config.logger.info(`[mock] ${m}`);
  return {
    name: 'wx-mock-backend',
    configureServer(server) {
      setup(server.httpServer as Server | null, server.middlewares, logTo(server));
    },
    configurePreviewServer(server) {
      setup(server.httpServer as Server, server.middlewares, logTo(server));
    },
  };
}
