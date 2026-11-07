# Weather map: web client

TypeScript, React, MapLibre GL JS and WebGL2. Talks to the server only through
the endpoints in `../docs/protocol.md`, using relative URLs.

## Running

```sh
npm install
npx playwright install chromium   # once, for the smoke test

npm run dev:mock      # against the built-in mock backend, http://127.0.0.1:5180
npm run dev           # against the Rust server on 127.0.0.1:8080 (proxied)
npm run build         # type-check and build to dist/ (served by the Rust server at /)
npm run preview:mock  # serve dist/ with the mock backend, http://127.0.0.1:5181
npm test              # unit tests (Vitest)
npm run smoke         # headless Chromium end-to-end check against the mock
npm run smoke -- --shots DIR   # same, saving screenshots to DIR
npm run smoke -- --only particles,decoders   # just those steps
npm run build:wasm    # optional: the Rust tile decoder as WebAssembly, to public/wasm/
npm run build -- --sourcemap   # with source maps (off by default)
```

Measurements, each against the mock in headless Chromium (software WebGL, so
compare runs rather than read absolute numbers; each takes `--runs` and
`--json FILE`, and serves `dist/` unless given `--mode dev` or `--url`):

```sh
node scripts/bundle-report.mjs     # emitted files raw and gzipped, each chunk by package / src folder
node scripts/perf/startup.mjs      # navigation to first weather tile: requests, bytes, order (--latency MS, --verbose)
node scripts/perf/frames.mjs       # per-frame layer cost, GL calls, allocations: radar loop, wind particles (--profile N)
node scripts/perf/decode.mjs       # WXT1 decode per tile, WebAssembly vs TypeScript, and loads end to end
node scripts/perf/memory.mjs       # cache vs GPU memory over a long loop (--minutes, --budget MB), leaks on layer switches
```

`build:wasm` needs Rust and `rustup target add wasm32-unknown-unknown`. It
builds `../crates/wx-tile-wasm` and copies the module (about 55 KB, 24 KB
gzipped) to `public/wasm/`, which `npm run build` then copies to `dist/wasm/`.
Without it the decode workers use the TypeScript decoder, with the same
results; the settings panel says which one is in use.

| Variable | Effect |
|---|---|
| `VITE_MOCK=1` | Serve the API, tiles, WebSocket and basemap from `mock/` |
| `WX_BACKEND` | Server to proxy to without the mock (default `http://127.0.0.1:8080`) |
| `PORT`, `HOST` | Dev server address (defaults `5180` / `5181` for preview, `127.0.0.1`) |
| `MOCK_BASEMAP=0` | Mock answers `basemap_url: null` |
| `MOCK_BASEMAP_FILE` | Archive the mock serves at `/basemap/<its file name>` (default `../data/basemap/world.pmtiles`, else `us.pmtiles`) |
| `MOCK_LATENCY` | Extra milliseconds of latency on every mock response |
| `MOCK_WIND=westerly` / `vortex` | Replace the mock's wind with 10 m/s from the west, or a counter-clockwise vortex over Kansas |

The mock serves a worldwide set of sources (`mock/sources.ts`): GFS under
every model layer and the CMORPH2 satellite estimate under the radar's past
frames, HRRR over North America, HRRR Alaska (whose grid crosses the
antimeridian) and ICON-EU over Europe with solid boxes, ragged MRMS and OPERA
radar composites, a ten-minute radar nowcast, RTMA, and five geostationary
imagers for the infrared `satellite` layer. `mock/catalog.ts` assembles every
layer's timeline from their parts by the rules in `crates/wx-core/src/frame.rs`,
and its tile route checks each stack the way the server does (404 for a stack
no frame has, or a zoom too deep). `/api/provenance` says which parts make the
value at a point. Alerts are worldwide (NWS plus synthetic national feeds in
several languages, one without a shape), served as vector tiles named by the
alert set's revision; stations (airports, buoys, AMeDAS) and point forecasts
(NWS style in the US, MET Norway style elsewhere) are worldwide too.
It also accepts `POST /__mock/advance?minutes=N` (move its clock forward,
emitting the `timeline` events that would have happened), `GET /__mock/misses`
(tile requests it refused), `POST /__mock/alert` (issue the Kansas City
Tornado Warning again) and `POST /__mock/alert-update` (a CAP Update: one
alert comes back under a new id, with a new revision).

The client loads only the shown layer's timeline at startup
(`/api/frames/{layer}`) and the others when they are first shown; alerts and
stations are asked for by the area in view.

Add `?basemap=off` or `?basemap=broken` to the app URL to see the fallback basemap,
`?wasm=off` to decode tiles in TypeScript, `?particles=nofloat` to act out a
device that cannot draw wind particles, and `?debug` to expose `window.__wx` in
a production build.

## Layout

- `src/api`: protocol types, HTTP calls, device token, the reconnecting WebSocket.
- `src/tiles`: WXT1 decoder, decode workers, tile maths, the tile-stack rule (`stack.ts`, the client's `tile_parts`), the quadtree walk that picks a frame's tiles at mixed zooms (`plan.ts`), the tile store (cache keyed by tile URL and shared across frames, cancellation, eviction), CPU sampling for read-outs.
- `src/render`: the MapLibre custom layers and their GLSL: the colour layer, and wind particles (`windParticles.ts`).
- `src/core`: palettes, units (defaults by the browser's region), timelines (frames, the per-layer dictionary of parts, `timeline` events) and the slider scale, alert colours, the store.
- `src/app`: app state, data loading and live updates, playback, push, wording.
- `src/map`: map setup and the first view (`firstView.ts`, `zones.ts`), basemap styles and fallback, the credits control, overlays and interactions.
- `src/ui`: React components. `src/status`: the status page.
- `public`: service worker, manifest, icons, and `basemap-assets`: the Protomaps basemap's glyphs (Noto Sans, SIL Open Font License, `fonts/OFL.txt`) and dark sprites (from Mapzen's MIT-licensed tangrams/icons, built by Protomaps under BSD-3-Clause; both licences in `sprites/`), copied from [protomaps/basemaps-assets](https://github.com/protomaps/basemaps-assets) so the page loads nothing from another host. `mock`: the mock backend.
