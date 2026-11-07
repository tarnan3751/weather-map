import { existsSync, readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { WasmDecoder } from './wasmDecoder.ts';
import { decodeWxt, interleave, WxtError } from './wxt.ts';

const WASM = new URL('../../public/wasm/wx_tile_wasm.wasm', import.meta.url);
const built = existsSync(WASM);
const fixture = (name: string) => new Uint8Array(readFileSync(new URL(`../../../fixtures/tiles/${name}`, import.meta.url)));

/** The error code a decode reports, or "ok". Anything other than a WxtError
 *  (a trap surfaces as WebAssembly.RuntimeError) fails the test. */
async function outcome(f: () => unknown): Promise<string> {
  try {
    await f();
    return 'ok';
  } catch (e) {
    if (e instanceof WxtError) return e.code;
    throw e;
  }
}

describe.skipIf(!built)(`WASM decoder${built ? '' : ' (skipped: public/wasm/wx_tile_wasm.wasm missing; run `npm run build:wasm`)'}`, () => {
  const load = async () => (await WasmDecoder.fromBytes(readFileSync(WASM)))!;

  it('decodes the three golden tiles exactly as the TypeScript decoder does', async () => {
    const wasm = await load();
    expect(wasm).not.toBeNull();
    for (const name of ['pattern_1band.wxt', 'pattern_3band.wxt', 'tiny.wxt']) {
      const bytes = fixture(name);
      const a = wasm.decode(bytes);
      const b = await decodeWxt(bytes);
      expect(a.width, name).toBe(b.width);
      expect(a.height, name).toBe(b.height);
      expect(a.bands.length, name).toBe(b.bands.length);
      a.bands.forEach((band, i) => expect(band, `${name} band ${i}`).toEqual(b.bands[i]));
    }
  });

  it('interleaves the picked bands straight from its memory as the TypeScript path does', async () => {
    const wasm = await load();
    for (const pick of [[0], [2, 0], [0, 1, 2]]) {
      const bytes = fixture('pattern_3band.wxt');
      const a = wasm.decodePicked(bytes, pick);
      const b = interleave(await decodeWxt(bytes), pick);
      expect(a.data, `pick ${pick}`).toEqual(b);
      expect(a.data.buffer).not.toBe(wasm.decodePicked(fixture('tiny.wxt'), [0]).data.buffer);
    }
  });

  it('reports the same errors as the TypeScript decoder, without trapping', async () => {
    const wasm = await load();
    const cut = fixture('pattern_1band.wxt').slice(0, 200);
    const badLen = fixture('tiny.wxt').slice();
    badLen[12] = 3;
    const unsupported = fixture('tiny.wxt').slice();
    unsupported[9] = 7;
    const cases: [string, Uint8Array][] = [
      ['truncated', new Uint8Array([0x57, 0x58])],
      ['bad magic', new Uint8Array(32)],
      ['cut payload', cut],
      ['bad length', badLen],
      ['unsupported', unsupported],
      ['empty', new Uint8Array(0)],
    ];
    for (const [label, bytes] of cases) {
      expect(await outcome(() => wasm.decode(bytes)), label).toBe(await outcome(() => decodeWxt(bytes)));
    }
  });

  it('refuses an absurd header instead of trying to allocate it', async () => {
    const wasm = await load();
    const huge = fixture('tiny.wxt').slice();
    huge.set([0xff, 0xff, 0xff, 0xff], 4);
    huge[8] = 8;
    expect(await outcome(() => wasm.decode(huge))).toBe('bad_header');
  });

  it('survives a few thousand random inputs and keeps decoding afterwards', async () => {
    const wasm = await load();
    let seed = 0x9e3779b9;
    const next = () => {
      seed ^= seed << 13;
      seed ^= seed >>> 17;
      seed ^= seed << 5;
      return seed >>> 0;
    };
    const golden = fixture('pattern_3band.wxt');
    for (let i = 0; i < 3000; i++) {
      // Random bytes, or a real tile with a few bytes flipped.
      const bytes = i % 2 ? new Uint8Array(next() % 400).map(() => next() & 0xff) : golden.slice(0, 16 + (next() % golden.length));
      if (i % 2 === 0) for (let k = 0; k < 4; k++) bytes[next() % bytes.length] = next() & 0xff;
      await outcome(() => wasm.decode(bytes));
    }
    expect((await load()).decode(fixture('tiny.wxt')).bands[0][1]).toBe(65535);
    expect(wasm.decode(fixture('tiny.wxt')).bands[0][1]).toBe(65535);
  });
});
