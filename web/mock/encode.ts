// WXT1 encoder, a port of `wx_tile::encode` (delta predictor, byte planes,
// zlib), so the mock serves the same bytes the Rust server would.

import { deflateSync } from 'node:zlib';

export function encodeWxt(width: number, height: number, bands: Uint16Array[], level = 6): Buffer {
  const n = width * height;
  const raw = Buffer.alloc(bands.length * n * 2);
  bands.forEach((band, b) => {
    const lo = b * n * 2;
    const hi = lo + n;
    let rowStartPrev = 0;
    for (let row = 0; row < height; row++) {
      const base = row * width;
      let prev = rowStartPrev;
      rowStartPrev = band[base];
      for (let col = 0; col < width; col++) {
        const v = band[base + col];
        const d = (v - prev) & 0xffff;
        prev = v;
        raw[lo + base + col] = d & 0xff;
        raw[hi + base + col] = d >> 8;
      }
    }
  });
  const header = Buffer.alloc(16);
  header.write('WXT1', 0, 'ascii');
  header.writeUInt16LE(width, 4);
  header.writeUInt16LE(height, 6);
  header[8] = bands.length;
  header[9] = 1;
  header[10] = 1;
  header[11] = 0;
  header.writeUInt32LE(raw.length, 12);
  return Buffer.concat([header, deflateSync(raw, { level })]);
}
