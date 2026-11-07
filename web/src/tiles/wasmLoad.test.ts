import { existsSync, readFileSync } from 'node:fs';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { loadWasmModule, wasmDecoderFrom } from './wasmDecoder.ts';

const WASM = new URL('../../public/wasm/wx_tile_wasm.wasm', import.meta.url);

function serve(body: BodyInit | null, status = 200) {
  vi.stubGlobal('fetch', async () => new Response(body, { status }));
}

afterEach(() => vi.unstubAllGlobals());

/** What a decode worker ends up with for a module fetched from `url`. */
const loadWasmDecoder = async (url: string) => wasmDecoderFrom(await loadWasmModule(url));

describe('the WebAssembly decoder falls back (null) rather than failing', () => {
  it('when the file is missing', async () => {
    serve('not found', 404);
    expect(await loadWasmDecoder('/wasm/x.wasm')).toBeNull();
  });

  it('when a server answers with a page instead of the module', async () => {
    serve('<!doctype html><title>app</title>');
    expect(await loadWasmDecoder('/wasm/x.wasm')).toBeNull();
  });

  it('when the module is damaged', async () => {
    serve(new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0, 0xff, 0xff, 0xff]));
    expect(await loadWasmDecoder('/wasm/x.wasm')).toBeNull();
  });

  it('when it is some other WebAssembly module', async () => {
    // The smallest valid module: no exports at all.
    serve(new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]));
    expect(await loadWasmDecoder('/wasm/x.wasm')).toBeNull();
  });

  it('when the network fails', async () => {
    vi.stubGlobal('fetch', async () => {
      throw new TypeError('offline');
    });
    expect(await loadWasmDecoder('/wasm/x.wasm')).toBeNull();
  });

  it('when WebAssembly is not available', async () => {
    const fetch = vi.fn(async () => new Response(new Uint8Array(8)));
    vi.stubGlobal('fetch', fetch);
    vi.stubGlobal('WebAssembly', undefined);
    expect(await loadWasmDecoder('/wasm/x.wasm')).toBeNull();
    expect(fetch).not.toHaveBeenCalled();
  });

  it.skipIf(!existsSync(WASM))('and loads the real module when it is there', async () => {
    serve(readFileSync(WASM));
    expect(await loadWasmDecoder('/wasm/wx_tile_wasm.wasm')).not.toBeNull();
  });
});
