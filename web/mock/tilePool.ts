// A small pool of tile-rendering worker threads, falling back to rendering
// inline if workers cannot start.

import { availableParallelism } from 'node:os';
import { Worker } from 'node:worker_threads';
import type { LayerSpec, Part } from '../src/api/types.ts';
import { renderTile } from './tiles.ts';

export class TilePool {
  private workers: { w: Worker; busy: number }[] = [];
  private pending = new Map<number, { resolve: (b: Buffer | null) => void; reject: (e: Error) => void }>();
  private nextId = 1;

  constructor(workerFile: string, epochMs: number, log: (m: string) => void) {
    const n = Math.max(1, Math.min(6, availableParallelism() - 1));
    try {
      for (let i = 0; i < n; i++) {
        const w = new Worker(workerFile, { workerData: { epochMs } });
        const slot = { w, busy: 0 };
        w.on('message', (m: { id: number; bytes: Uint8Array | null; error: string | null }) => {
          slot.busy--;
          const p = this.pending.get(m.id);
          if (!p) return;
          this.pending.delete(m.id);
          if (m.error) p.reject(new Error(m.error));
          else p.resolve(m.bytes ? Buffer.from(m.bytes.buffer, m.bytes.byteOffset, m.bytes.byteLength) : null);
        });
        w.on('error', (e: Error) => log(`tile worker failed: ${e.message}`));
        w.unref();
        this.workers.push(slot);
      }
    } catch (e) {
      log(`tile workers unavailable, rendering inline: ${(e as Error).message}`);
      this.workers = [];
    }
  }

  render(layer: LayerSpec, parts: Part[], z: number, x: number, y: number): Promise<Buffer | null> {
    if (!this.workers.length) return Promise.resolve(renderTile(layer, parts, z, x, y));
    const slot = this.workers.reduce((a, b) => (b.busy < a.busy ? b : a));
    slot.busy++;
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      slot.w.postMessage({ id, layer: layer.id, parts, z, x, y });
    });
  }

  close() {
    for (const { w } of this.workers) void w.terminate();
    this.workers = [];
  }
}
