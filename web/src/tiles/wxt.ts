// WXT1 raw-value tile decoder. See docs/protocol.md ("WXT1 tile format") and
// crates/wx-tile/src/lib.rs, which this mirrors byte for byte.

import { unzlibSync } from 'fflate';

export const HEADER_LEN = 16;
export const MAX_BANDS = 8;

export type WxtErrorCode = 'truncated' | 'bad_magic' | 'unsupported' | 'bad_header' | 'corrupt';

export class WxtError extends Error {
  readonly code: WxtErrorCode;
  constructor(code: WxtErrorCode, message: string) {
    super(message);
    this.code = code;
    this.name = 'WxtError';
  }
}

export interface WxtHeader {
  width: number;
  height: number;
  bands: number;
  compression: number;
  predictor: number;
  rawLen: number;
}

export interface WxtTile {
  width: number;
  height: number;
  /** `bands[b][row * width + col]`, rows north to south. */
  bands: Uint16Array[];
}

export type Inflate = (compressed: Uint8Array, expectedLen: number) => Promise<Uint8Array>;

export function parseHeader(bytes: Uint8Array): WxtHeader {
  if (bytes.length < HEADER_LEN) throw new WxtError('truncated', 'tile is shorter than its header');
  if (bytes[0] !== 0x57 || bytes[1] !== 0x58 || bytes[2] !== 0x54 || bytes[3] !== 0x31) {
    throw new WxtError('bad_magic', 'not a WXT1 tile');
  }
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const h: WxtHeader = {
    width: dv.getUint16(4, true),
    height: dv.getUint16(6, true),
    bands: bytes[8],
    compression: bytes[9],
    predictor: bytes[10],
    rawLen: dv.getUint32(12, true),
  };
  if (h.bands === 0 || h.bands > MAX_BANDS) throw new WxtError('bad_header', 'band count');
  if (h.rawLen !== h.bands * h.width * h.height * 2) throw new WxtError('bad_header', 'payload length');
  if (h.compression > 1) throw new WxtError('unsupported', `compression code ${h.compression}`);
  if (h.predictor > 1) throw new WxtError('unsupported', `predictor code ${h.predictor}`);
  return h;
}

/** Undo the predictor on a decompressed payload. */
export function decodePayload(h: WxtHeader, raw: Uint8Array): Uint16Array[] {
  const n = h.width * h.height;
  if (raw.length !== h.rawLen) throw new WxtError('corrupt', 'tile payload is corrupt');
  const out: Uint16Array[] = [];
  for (let b = 0; b < h.bands; b++) {
    const base = b * n * 2;
    const band = new Uint16Array(n);
    if (h.predictor === 0) {
      for (let i = 0; i < n; i++) band[i] = raw[base + 2 * i] | (raw[base + 2 * i + 1] << 8);
    } else {
      const lo = base;
      const hi = base + n;
      const w = h.width;
      let above = 0;
      for (let row = 0; row < h.height; row++) {
        let prev = above;
        const r = row * w;
        for (let col = 0; col < w; col++) {
          const i = r + col;
          prev = (prev + (raw[lo + i] | (raw[hi + i] << 8))) & 0xffff;
          band[i] = prev;
        }
        above = band[r];
      }
    }
    out.push(band);
  }
  return out;
}

/** Inflate RFC 1950 zlib data with the platform's DecompressionStream,
 *  refusing to produce more than `expectedLen` bytes. */
export const inflateZlib: Inflate = async (compressed, expectedLen) => {
  const ds = new DecompressionStream('deflate');
  const writer = ds.writable.getWriter();
  // The write promise rejects alongside the reader on corrupt input; the
  // reader's rejection is the one reported.
  writer.write(compressed as Uint8Array<ArrayBuffer>).catch(() => {});
  writer.close().catch(() => {});
  const reader = ds.readable.getReader();
  const out = new Uint8Array(expectedLen);
  let len = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (len + value.length > expectedLen) {
        await reader.cancel().catch(() => {});
        throw new WxtError('corrupt', 'tile payload inflates past its declared length');
      }
      out.set(value, len);
      len += value.length;
    }
  } catch (e) {
    if (e instanceof WxtError) throw e;
    throw new WxtError('corrupt', 'tile payload is corrupt');
  }
  if (len !== expectedLen) throw new WxtError('corrupt', 'tile payload is corrupt');
  return out;
};

/** Pure-JavaScript inflate for browsers without DecompressionStream (Safari
 *  and iOS before 16.4). One spare byte of output space shows a payload that
 *  inflates past its declared length. */
export const inflateJs: Inflate = async (compressed, expectedLen) => {
  let out: Uint8Array;
  try {
    out = unzlibSync(compressed, { out: new Uint8Array(expectedLen + 1) });
  } catch {
    throw new WxtError('corrupt', 'tile payload is corrupt');
  }
  if (out.length !== expectedLen) throw new WxtError('corrupt', 'tile payload is corrupt');
  return out;
};

/** The platform's DecompressionStream where there is one, else `inflateJs`. */
export const inflateAuto: Inflate = (compressed, expectedLen) =>
  typeof DecompressionStream === 'function' ? inflateZlib(compressed, expectedLen) : inflateJs(compressed, expectedLen);

export async function decodeWxt(bytes: Uint8Array, inflate: Inflate = inflateAuto): Promise<WxtTile> {
  const h = parseHeader(bytes);
  const payload = bytes.subarray(HEADER_LEN);
  let raw: Uint8Array;
  if (h.compression === 0) {
    if (payload.length !== h.rawLen) throw new WxtError('corrupt', 'tile payload is corrupt');
    raw = payload;
  } else {
    raw = await inflate(payload, h.rawLen);
  }
  return { width: h.width, height: h.height, bands: decodePayload(h, raw) };
}

/** Texture channels for `n` bands: three are padded to four, as GPUs have
 *  no three-channel 16-bit integer format and drivers would otherwise
 *  convert every upload. */
export const textureChannels = (n: number) => (n === 3 ? 4 : n);

/**
 * Pack the chosen bands into one channel-interleaved array, the layout an
 * RGBA16UI-style integer texture upload wants (`textureChannels` wide; a
 * padding channel is no-data). Missing band indices become no-data so a
 * malformed tile cannot index out of range. A single band that owns its
 * buffer is returned as it is.
 */
export function interleave(tile: WxtTile, pick: readonly number[]): Uint16Array {
  const n = tile.width * tile.height;
  const c = textureChannels(pick.length);
  const one = c === 1 ? tile.bands[pick[0]] : undefined;
  if (one && one.length === n && one.byteOffset === 0 && one.buffer.byteLength === one.byteLength) return one;
  const out = new Uint16Array(n * c);
  for (let k = 0; k < pick.length; k++) {
    const src = tile.bands[pick[k]];
    if (!src) continue;
    for (let i = 0; i < n; i++) out[i * c + k] = src[i];
  }
  return out;
}
