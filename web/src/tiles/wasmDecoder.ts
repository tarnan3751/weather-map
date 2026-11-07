// The Rust WXT1 decoder (crates/wx-tile-wasm) as WebAssembly, behind the same
// result shape as the TypeScript decoder. The module has no imports and a
// plain C ABI, so loading it is a few lines; if anything about it is off the
// caller keeps using TypeScript.

import { interleave, WxtError, type WxtErrorCode, type WxtTile } from './wxt.ts';

interface Exports {
  memory: WebAssembly.Memory;
  wx_alloc(len: number): number;
  wx_free(ptr: number, len: number): void;
  wx_decode(ptr: number, len: number): number;
  wx_width(): number;
  wx_height(): number;
  wx_bands(): number;
  wx_samples(): number;
  wx_release(): void;
}

/** Status codes from crates/wx-tile-wasm/src/lib.rs. */
const STATUS: Record<number, [WxtErrorCode, string]> = {
  [-1]: ['truncated', 'tile is shorter than its header'],
  [-2]: ['bad_magic', 'not a WXT1 tile'],
  [-3]: ['unsupported', 'unsupported compression or predictor'],
  [-4]: ['bad_header', 'tile header is inconsistent'],
  [-5]: ['corrupt', 'tile payload is corrupt'],
  [-6]: ['bad_header', 'tile is implausibly large'],
  [-7]: ['corrupt', 'no tile data'],
};

const NEEDED = ['memory', 'wx_alloc', 'wx_free', 'wx_decode', 'wx_width', 'wx_height', 'wx_bands', 'wx_samples', 'wx_release'];

/** fixtures/tiles/tiny.wxt: 3 x 2, samples [0, 65535, 1, 32768, 0, 65534]. */
const SELF_TEST = Uint8Array.from(atob('V1hUMQMAAgABAQEADAAAAHicY/jPxMDwj+E/Q0PDfwAagQT+'), (c) => c.charCodeAt(0));
const SELF_TEST_SAMPLES = [0, 65535, 1, 32768, 0, 65534];

export class WasmDecoder {
  private readonly x: Exports;

  private constructor(x: Exports) {
    this.x = x;
  }

  /** Instantiate from the module's bytes; null if it is not the module
   *  expected or does not decode a known tile correctly. */
  static async fromBytes(bytes: BufferSource): Promise<WasmDecoder | null> {
    if (typeof WebAssembly !== 'object') return null;
    return WasmDecoder.fromModule(await WebAssembly.compile(bytes));
  }

  /** The same, from a compiled module (one compile serves every worker). */
  static async fromModule(module: WebAssembly.Module): Promise<WasmDecoder | null> {
    const instance = await WebAssembly.instantiate(module, {});
    const x = instance.exports as unknown as Exports;
    if (!NEEDED.every((k) => k in x)) return null;
    const d = new WasmDecoder(x);
    const t = d.decode(SELF_TEST);
    if (t.width !== 3 || t.height !== 2 || Array.from(t.bands[0]).join() !== SELF_TEST_SAMPLES.join()) return null;
    return d;
  }

  /** Same result as `decodeWxt`, synchronously; throws WxtError for a bad tile. */
  decode(bytes: Uint8Array): WxtTile {
    return this.run(bytes, (width, height, bands) => ({ width, height, bands: bands.map((b) => b.slice()) }));
  }

  /** The picked bands, interleaved as `interleave` does, copied out of the
   *  module's memory once. */
  decodePicked(bytes: Uint8Array, pick: readonly number[]): { width: number; height: number; data: Uint16Array } {
    return this.run(bytes, (width, height, bands) => ({ width, height, data: interleave({ width, height, bands }, pick) }));
  }

  /** Decode, and hand `take` views of the bands in the module's memory,
   *  which are reused once it returns. */
  private run<T>(bytes: Uint8Array, take: (width: number, height: number, bands: Uint16Array[]) => T): T {
    const x = this.x;
    const len = bytes.length;
    const ptr = len ? x.wx_alloc(len) : 0;
    if (len && !ptr) throw new RangeError('WebAssembly allocation failed');
    let status: number;
    try {
      // Re-read memory.buffer after every call that may grow memory.
      if (len) new Uint8Array(x.memory.buffer, ptr, len).set(bytes);
      status = x.wx_decode(ptr, len);
    } finally {
      if (ptr) x.wx_free(ptr, len);
    }
    if (status !== 0) {
      const [code, message] = STATUS[status] ?? ['corrupt', `decoder status ${status}`];
      throw new WxtError(code, message);
    }
    try {
      const width = x.wx_width();
      const height = x.wx_height();
      const n = width * height;
      const base = x.wx_samples();
      const count = x.wx_bands();
      const bands: Uint16Array[] = [];
      for (let b = 0; b < count; b++) bands.push(new Uint16Array(x.memory.buffer, base + b * n * 2, n));
      return take(width, height, bands);
    } finally {
      x.wx_release();
    }
  }
}

/** Fetch and compile the module; null (never a throw) when it is missing or
 *  is not WebAssembly. */
export async function loadWasmModule(url: string): Promise<WebAssembly.Module | null> {
  try {
    if (typeof WebAssembly !== 'object') return null;
    const res = await fetch(url);
    if (!res.ok) return null;
    const bytes = new Uint8Array(await res.arrayBuffer());
    // "\0asm": a dev server answering a missing file with index.html is not it.
    if (bytes[0] !== 0 || bytes[1] !== 0x61 || bytes[2] !== 0x73 || bytes[3] !== 0x6d) return null;
    return await WebAssembly.compile(bytes);
  } catch {
    return null;
  }
}

/** A decoder from a compiled module; null (never a throw) when there is none
 *  or it fails its self-test. */
export async function wasmDecoderFrom(module: WebAssembly.Module | null): Promise<WasmDecoder | null> {
  try {
    return module ? await WasmDecoder.fromModule(module) : null;
  } catch {
    return null;
  }
}
