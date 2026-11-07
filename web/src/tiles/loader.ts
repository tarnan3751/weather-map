// A small pool of decode workers behind the TileStore's Fetcher interface.
// Falls back to decoding on the main thread where module workers are missing.

import type { DecoderName, DecoderReply, WorkerReply, WorkerRequest } from './decode.worker.ts';
import { loadTile } from './load.ts';
import type { Fetcher, LoadResult } from './tileStore.ts';
import { loadWasmModule } from './wasmDecoder.ts';

export interface FetcherOptions {
  /** The WebAssembly decoder's URL, or null to decode in TypeScript. */
  wasmUrl: string | null;
  /** Which decoder the workers ended up with. */
  onDecoder?(name: DecoderName): void;
}

export function createWorkerFetcher(opts: FetcherOptions = { wasmUrl: null }): Fetcher {
  let workers: Worker[] = [];
  try {
    const n = Math.max(1, Math.min(4, (navigator.hardwareConcurrency || 4) - 1));
    for (let i = 0; i < n; i++) {
      workers.push(new Worker(new URL('./decode.worker.ts', import.meta.url), { type: 'module', name: `wxt-decode-${i}` }));
    }
  } catch {
    workers = [];
  }
  if (!workers.length) {
    opts.onDecoder?.('typescript');
    return (url, pick, signal) => loadTile(url, pick, signal);
  }
  // Fetched and compiled once here, then handed to every worker.
  void (opts.wasmUrl ? loadWasmModule(opts.wasmUrl) : Promise.resolve(null)).then((module) => {
    const init: WorkerRequest = { type: 'init', module };
    for (const w of workers) w.postMessage(init);
  });

  let nextId = 1;
  const pending = new Map<number, (r: LoadResult) => void>();
  /** Loads each worker has not answered yet: the next goes to the least busy. */
  const busy = workers.map(() => 0);
  // Report WebAssembly only once every worker has it.
  const decoders = new Map<Worker, DecoderName>();
  workers.forEach((w, i) => {
    w.onmessage = (e: MessageEvent<WorkerReply | DecoderReply>) => {
      if ('decoder' in e.data) {
        decoders.set(w, e.data.decoder);
        if (decoders.size === workers.length) {
          opts.onDecoder?.([...decoders.values()].every((d) => d === 'wasm') ? 'wasm' : 'typescript');
        }
        return;
      }
      const resolve = pending.get(e.data.id);
      if (!resolve) return;
      pending.delete(e.data.id);
      busy[i]--;
      resolve(e.data.result);
    };
  });

  return (url, pick, signal) =>
    new Promise<LoadResult>((resolve) => {
      const id = nextId++;
      const i = busy.indexOf(Math.min(...busy));
      busy[i]++;
      pending.set(id, resolve);
      const msg: WorkerRequest = { type: 'load', id, url: new URL(url, location.href).href, pick };
      workers[i].postMessage(msg);
      signal.addEventListener(
        'abort',
        () => {
          const cancel: WorkerRequest = { type: 'cancel', id };
          workers[i].postMessage(cancel);
        },
        { once: true },
      );
    });
}
