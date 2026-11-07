// Renders mock tiles on a worker thread so several can be generated at once
// and the dev server stays responsive. Loaded directly by Node (type
// stripping), so this file and everything it imports use erasable syntax only.

import { parentPort, workerData } from 'node:worker_threads';
import type { Part } from '../src/api/types.ts';
import { setEpoch } from './fields.ts';
import { layerById } from './registry.ts';
import { renderTile } from './tiles.ts';

interface Job {
  id: number;
  layer: string;
  parts: Part[];
  z: number;
  x: number;
  y: number;
}

setEpoch((workerData as { epochMs: number }).epochMs);

parentPort!.on('message', (job: Job) => {
  const layer = layerById(job.layer);
  let out: Buffer | null = null;
  let error: string | null = null;
  try {
    out = layer ? renderTile(layer, job.parts, job.z, job.x, job.y) : null;
  } catch (e) {
    error = String((e as Error).stack ?? e);
  }
  const bytes = out ? new Uint8Array(out.buffer, out.byteOffset, out.byteLength).slice() : null;
  parentPort!.postMessage({ id: job.id, bytes, error }, bytes ? [bytes.buffer] : []);
});
