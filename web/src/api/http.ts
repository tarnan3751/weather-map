// Thin wrappers over the HTTP endpoints. Every URL is relative: in production
// the Rust server serves this app at `/`, in development Vite proxies (or the
// mock answers) the same paths.

import type {
  AlertCollection,
  AlertDetail,
  AlertProps,
  AlertTileJson,
  AllFrames,
  Config,
  LayerSpec,
  LonLatBox,
  PointForecast,
  Provenance,
  SourceDef,
  StationCollection,
  StatusReply,
  Timeline,
} from './types.ts';

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.status = status;
    this.code = code;
    this.name = 'ApiError';
  }
}

export async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  let res: Response;
  try {
    res = await fetch(path, { ...init, headers: { accept: 'application/json', ...init.headers } });
  } catch (e) {
    if ((e as Error).name === 'AbortError') throw e;
    throw new ApiError(0, 'network', 'The server could not be reached.');
  }
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  let body: unknown = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = null;
  }
  if (!res.ok) {
    const b = (body ?? {}) as { error?: string; message?: string };
    throw new ApiError(res.status, b.error ?? `http_${res.status}`, b.message ?? res.statusText ?? 'Request failed');
  }
  return body as T;
}

const qs = (p: Record<string, string | number | undefined>) =>
  Object.entries(p)
    .filter(([, v]) => v !== undefined)
    .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`)
    .join('&');

/** `west,south,east,north`, as both list endpoints take it (`west > east`
 *  crosses the antimeridian). Commas stay literal: they separate values. */
const bboxParam = (b: LonLatBox) => [b.west, b.south, b.east, b.north].map((v) => Number(v.toFixed(3))).join(',');

interface AlertQuery {
  bbox?: LonLatBox;
  country?: string;
  unshaped?: boolean;
  limit?: number;
}

export const api = {
  config: () => request<Config>('/api/config'),
  layers: () => request<{ layers: LayerSpec[] }>('/api/layers').then((r) => r.layers),
  sources: () => request<{ sources: SourceDef[] }>('/api/sources').then((r) => r.sources),
  frames: () => request<AllFrames>('/api/frames'),
  layerFrames: (layer: string) => request<{ layer: string } & Timeline>(`/api/frames/${encodeURIComponent(layer)}`),
  alerts: (q: AlertQuery = {}, signal?: AbortSignal) =>
    request<AlertCollection>(
      `/api/alerts?${[q.bbox ? `bbox=${bboxParam(q.bbox)}` : '', qs({ country: q.country, unshaped: q.unshaped ? 'true' : undefined, limit: q.limit })].filter(Boolean).join('&')}`,
      { signal },
    ),
  alertTiles: () => request<AlertTileJson>('/api/alerts/tiles.json'),
  alertDetail: (id: string) => request<AlertDetail>(`/api/alerts/detail?${qs({ id })}`),
  alertsAt: (lat: number, lon: number) => request<{ alerts: AlertProps[] }>(`/api/alerts/at?${qs({ lat, lon })}`),
  stations: (bbox?: LonLatBox, signal?: AbortSignal) => request<StationCollection>(`/api/stations${bbox ? `?bbox=${bboxParam(bbox)}` : ''}`, { signal }),
  provenance: (layer: string, valid: string, lon: number, lat: number, signal?: AbortSignal) =>
    request<Provenance>(`/api/provenance?${qs({ layer, valid, lon: lon.toFixed(4), lat: lat.toFixed(4) })}`, { signal }),
  point: (lat: number, lon: number, signal?: AbortSignal) =>
    request<PointForecast>(`/api/point?${qs({ lat: lat.toFixed(4), lon: lon.toFixed(4) })}`, { signal }),
  status: () => request<StatusReply>('/api/status'),
};

/** URL of one tile, from the template the server advertises in `/api/config`.
 *  `parts` is the stack, top first, joined by commas; part ids are URL-safe
 *  already, and the commas are part of the name. */
export function tileUrl(template: string, layer: string, z: number, x: number, y: number, parts: string): string {
  return template
    .replace('{layer}', encodeURIComponent(layer))
    .replace('{z}', String(z))
    .replace('{x}', String(x))
    .replace('{y}', String(y))
    .replace('{parts}', parts);
}
