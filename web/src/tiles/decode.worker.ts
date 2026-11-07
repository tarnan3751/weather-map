// Fetches and decodes WXT1 tiles off the main thread, so inflating a few
// dozen tiles while the user drags the time slider does not stall the map.

import { type Decode, decodeTypeScript, loadTile } from './load.ts';
import type { LoadResult } from './tileStore.ts';
import { type WasmDecoder, wasmDecoderFrom } from './wasmDecoder.ts';
import { WxtError } from './wxt.ts';

export type DecoderName = 'wasm' | 'typescript';

export type WorkerRequest =
  /** The WebAssembly decoder, compiled once for every worker, or null to use
   *  TypeScript. Tiles are fetched before it comes, but decoded after. */
  | { type: 'init'; module: WebAssembly.Module | null }
  | { type: 'load'; id: number; url: string; pick: number[] }
  | { type: 'cancel'; id: number };

/** Sent once the decoder is chosen. */
export interface DecoderReply {
  decoder: DecoderName;
}

export interface WorkerReply {
  id: number;
  result: LoadResult;
}

interface Scope {
  onmessage: ((e: MessageEvent<WorkerRequest>) => void) | null;
  postMessage(msg: WorkerReply | DecoderReply, transfer?: Transferable[]): void;
}

const scope = self as unknown as Scope;
const inflight = new Map<number, AbortController>();

let wasm: WasmDecoder | null = null;
let initialised: () => void;
const ready = new Promise<void>((resolve) => (initialised = resolve));

/** The Rust decoder where it loaded, else TypeScript. A failure that is not
 *  about the tile itself (an engine error) turns WebAssembly off for good. */
const decode: Decode = async (bytes, pick) => {
  await ready;
  if (wasm) {
    try {
      return wasm.decodePicked(bytes, pick);
    } catch (e) {
      if (e instanceof WxtError) throw e;
      wasm = null;
      scope.postMessage({ decoder: 'typescript' });
    }
  }
  return decodeTypeScript(bytes, pick);
};

scope.onmessage = (e) => {
  const m = e.data;
  if (m.type === 'init') {
    void wasmDecoderFrom(m.module).then((d) => {
      wasm = d;
      scope.postMessage({ decoder: wasm ? 'wasm' : 'typescript' });
      initialised();
    });
    return;
  }
  if (m.type === 'cancel') {
    inflight.get(m.id)?.abort();
    return;
  }
  const ac = new AbortController();
  inflight.set(m.id, ac);
  void loadTile(m.url, m.pick, ac.signal, decode).then((result) => {
    inflight.delete(m.id);
    // The samples move to the main thread rather than being copied.
    scope.postMessage({ id: m.id, result }, result.status === 'ok' ? [result.data.buffer] : []);
  });
};
