import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { deflateSync } from 'node:zlib';
import { decodeWxt, inflateJs, interleave, parseHeader, WxtError } from './wxt.ts';

const fixture = (name: string) =>
  new Uint8Array(readFileSync(new URL(`../../../fixtures/tiles/${name}`, import.meta.url)));

// Patterns documented in crates/wx-tile/examples/golden.rs.
const N = 257;

describe('WXT1 decoder against the Rust golden tiles', () => {
  it('pattern_1band.wxt', async () => {
    const bytes = fixture('pattern_1band.wxt');
    const h = parseHeader(bytes);
    expect(h).toMatchObject({ width: 257, height: 257, bands: 1, compression: 1, predictor: 1 });
    const tile = await decodeWxt(bytes);
    expect(tile.bands).toHaveLength(1);
    const band = tile.bands[0];
    let mismatches = 0;
    for (let row = 0; row < N; row++) {
      for (let col = 0; col < N; col++) {
        const want = (col + row) % 11 === 0 ? 0 : (col * 7 + row * 13) % 65536;
        if (band[row * N + col] !== want) mismatches++;
      }
    }
    expect(mismatches).toBe(0);
  });

  it('pattern_3band.wxt', async () => {
    const tile = await decodeWxt(fixture('pattern_3band.wxt'));
    expect(tile.width).toBe(257);
    expect(tile.height).toBe(257);
    expect(tile.bands).toHaveLength(3);
    for (let b = 0; b < 3; b++) {
      let mismatches = 0;
      for (let row = 0; row < N; row++) {
        for (let col = 0; col < N; col++) {
          const want = (col * (b + 1) * 31 + row * 17 + b * 1000) % 65536;
          if (tile.bands[b][row * N + col] !== want) mismatches++;
        }
      }
      expect(mismatches, `band ${b}`).toBe(0);
    }
  });

  it('tiny.wxt keeps the extremes', async () => {
    const tile = await decodeWxt(fixture('tiny.wxt'));
    expect(tile.width).toBe(3);
    expect(tile.height).toBe(2);
    expect(Array.from(tile.bands[0])).toEqual([0, 65535, 1, 32768, 0, 65534]);
  });
});

describe('WXT1 decoder edge cases', () => {
  const header = (w: number, h: number, bands: number, compression: number, predictor: number) => {
    const b = new Uint8Array(16);
    b.set([0x57, 0x58, 0x54, 0x31]);
    const dv = new DataView(b.buffer);
    dv.setUint16(4, w, true);
    dv.setUint16(6, h, true);
    b[8] = bands;
    b[9] = compression;
    b[10] = predictor;
    dv.setUint32(12, bands * w * h * 2, true);
    return b;
  };

  it('reads stored, unpredicted samples', async () => {
    const samples = new Uint16Array([5, 9, 65535, 0]);
    const body = new Uint8Array(samples.buffer);
    const bytes = new Uint8Array([...header(2, 2, 1, 0, 0), ...body]);
    const tile = await decodeWxt(bytes);
    expect(Array.from(tile.bands[0])).toEqual([5, 9, 65535, 0]);
  });

  it('reads zlib without the predictor', async () => {
    const samples = new Uint16Array([1, 2, 3, 4, 5, 6]);
    const z = deflateSync(Buffer.from(samples.buffer));
    const bytes = new Uint8Array([...header(3, 2, 1, 1, 0), ...z]);
    expect(Array.from((await decodeWxt(bytes)).bands[0])).toEqual([1, 2, 3, 4, 5, 6]);
  });

  it('rejects garbage', async () => {
    await expect(decodeWxt(new Uint8Array([0x57, 0x58]))).rejects.toMatchObject({ code: 'truncated' });
    await expect(decodeWxt(new Uint8Array(32))).rejects.toMatchObject({ code: 'bad_magic' });
    const bad = fixture('pattern_1band.wxt').slice();
    bad[12] = 3;
    await expect(decodeWxt(bad)).rejects.toMatchObject({ code: 'bad_header' });
    const cut = fixture('pattern_1band.wxt').slice(0, 200);
    await expect(decodeWxt(cut)).rejects.toBeInstanceOf(WxtError);
    const unknown = fixture('tiny.wxt').slice();
    unknown[9] = 7;
    await expect(decodeWxt(unknown)).rejects.toMatchObject({ code: 'unsupported' });
  });

  it('refuses a payload that inflates past its declared length', async () => {
    const z = deflateSync(Buffer.alloc(10_000));
    const bytes = new Uint8Array([...header(2, 2, 1, 1, 0), ...z]);
    await expect(decodeWxt(bytes)).rejects.toMatchObject({ code: 'corrupt' });
  });

  it('interleaves the chosen bands for texture upload', async () => {
    const tile = await decodeWxt(fixture('pattern_3band.wxt'));
    const packed = interleave(tile, [2, 0]);
    expect(packed.length).toBe(N * N * 2);
    const i = 10 * N + 7;
    expect(packed[i * 2]).toBe(tile.bands[2][i]);
    expect(packed[i * 2 + 1]).toBe(tile.bands[0][i]);
    const missing = interleave(tile, [5]);
    expect(missing.every((v) => v === 0)).toBe(true);
  });

  it('pads three bands to four channels, the widest integer texture there is', async () => {
    const tile = await decodeWxt(fixture('pattern_3band.wxt'));
    const packed = interleave(tile, [0, 1, 2]);
    expect(packed.length).toBe(N * N * 4);
    const i = 33 * N + 5;
    expect([...packed.subarray(i * 4, i * 4 + 4)]).toEqual([tile.bands[0][i], tile.bands[1][i], tile.bands[2][i], 0]);
  });

  it('hands over a single band without copying it, unless it is a view', async () => {
    const tile = await decodeWxt(fixture('pattern_3band.wxt'));
    expect(interleave(tile, [1])).toBe(tile.bands[1]);
    const view = { width: 3, height: 2, bands: [new Uint16Array([9, 9, 1, 2, 3, 4, 5, 6]).subarray(2)] };
    const copy = interleave(view, [0]);
    expect(copy).not.toBe(view.bands[0]);
    expect([...copy]).toEqual([1, 2, 3, 4, 5, 6]);
  });
});

describe('without DecompressionStream (Safari before 16.4)', () => {
  it('decodes the golden tiles with the JavaScript inflate', async () => {
    const one = await decodeWxt(fixture('pattern_1band.wxt'), inflateJs);
    const viaStream = await decodeWxt(fixture('pattern_1band.wxt'));
    expect(one.bands[0]).toEqual(viaStream.bands[0]);
    const three = await decodeWxt(fixture('pattern_3band.wxt'), inflateJs);
    expect(three.bands[2][256]).toBe((256 * 3 * 31 + 2000) % 65536);
  });

  it('falls back automatically when the platform lacks it', async () => {
    const saved = globalThis.DecompressionStream;
    // @ts-expect-error simulating an older browser
    delete globalThis.DecompressionStream;
    try {
      const tile = await decodeWxt(fixture('tiny.wxt'));
      expect(Array.from(tile.bands[0])).toEqual([0, 65535, 1, 32768, 0, 65534]);
    } finally {
      globalThis.DecompressionStream = saved;
    }
  });

  it('refuses corrupt or oversized payloads the same way', async () => {
    const cut = fixture('pattern_1band.wxt').slice(0, 200);
    await expect(decodeWxt(cut, inflateJs)).rejects.toMatchObject({ code: 'corrupt' });
    const big = new Uint8Array([0x57, 0x58, 0x54, 0x31, 2, 0, 2, 0, 1, 1, 0, 0, 8, 0, 0, 0, ...deflateSync(Buffer.alloc(10_000))]);
    await expect(decodeWxt(big, inflateJs)).rejects.toMatchObject({ code: 'corrupt' });
  });
});
