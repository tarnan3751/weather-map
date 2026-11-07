// One tile load: fetch, map the HTTP answer to what the tile store does next,
// decode the bands asked for. Shared by the decode workers and the
// main-thread fallback.

import type { LoadResult } from './tileStore.ts';
import { decodeWxt, interleave, textureChannels } from './wxt.ts';

/** The picked bands of a tile's bytes, interleaved for upload. */
export type Decode = (bytes: Uint8Array, pick: readonly number[]) => Promise<{ width: number; height: number; data: Uint16Array }>;

export const decodeTypeScript: Decode = async (bytes, pick) => {
  const tile = await decodeWxt(bytes);
  return { width: tile.width, height: tile.height, data: interleave(tile, pick) };
};

/** Seconds or an HTTP date, as `Retry-After` allows. */
export function retryAfterMs(header: string | null): number | undefined {
  if (!header) return undefined;
  const secs = Number(header);
  if (Number.isFinite(secs)) return secs * 1000;
  const at = Date.parse(header);
  return Number.isNaN(at) ? undefined : Math.max(0, at - Date.now());
}

export async function loadTile(url: string, pick: readonly number[], signal: AbortSignal, decode: Decode = decodeTypeScript): Promise<LoadResult> {
  // The network failed (that can come right), or the load was cancelled.
  const failed = (): LoadResult => (signal.aborted ? { status: 'cancelled' } : { status: 'error', retry: true });
  let res: Response;
  try {
    res = await fetch(url, { signal });
  } catch {
    return failed();
  }
  if (res.status === 204) return { status: 'empty' };
  if (res.status === 404) return { status: 'missing' };
  if (res.status === 503 || res.status === 429) return { status: 'error', retry: true, retryAfterMs: retryAfterMs(res.headers.get('retry-after')) };
  if (!res.ok) return { status: 'error', retry: res.status >= 500 };
  let bytes: Uint8Array;
  try {
    bytes = new Uint8Array(await res.arrayBuffer());
  } catch {
    return failed();
  }
  try {
    const t = await decode(bytes, pick);
    return { status: 'ok', width: t.width, height: t.height, channels: textureChannels(pick.length), data: t.data };
  } catch {
    // Decoding the same bytes again would fail the same way: never retried.
    return signal.aborted ? { status: 'cancelled' } : { status: 'error', retry: false };
  }
}
