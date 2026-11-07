// The weather layer: a MapLibre custom layer that draws raw-value WXT1 tiles,
// coloured in the shader. Each frame is a mosaic of parts, and its tiles are
// named by the stack of parts they are rendered from, so the layer plans the
// view frame by frame (a quadtree walk that goes deep only where the data
// does), asks the TileStore for those tiles by URL (and for the frames around
// the current one, so playback does not stall), uploads them as integer
// textures, and draws every cell of the view from the best tile available:
// the frame's own tile, else a magnified ancestor of it, else the previously
// shown frame, so there are no holes. Frames that share a stack somewhere
// share that tile, its download and its texture.

import type { CustomLayerInterface, CustomRenderMethodInput, Map as MlMap } from 'maplibre-gl';
import type { LayerSpec, Part, Render } from '../api/types.ts';
import { buildLut, getPalette, type Palette, TRANSFER_CODE, transferFn } from '../core/palettes.ts';
import type { TFrame } from '../core/timeline.ts';
import { type Cell, FrameTiles, type Leaf, leafAt, MAX_TILE_ZOOM, planCells, type ViewRect } from '../tiles/plan.ts';
import { bilinearSample, dequantize, nearestPositiveSample, nearestSample } from '../tiles/sampling.ts';
import { tileParts, topPartAt } from '../tiles/stack.ts';
import { latToY, lonToX, MAX_MERCATOR_LAT, type SubRect, subRect, tileFraction, tileZoomFor, type View, viewRect, xToLon, yToLat } from '../tiles/tileMath.ts';
import type { TileEntry, TileRequest, TileStore } from '../tiles/tileStore.ts';
import { createProgram, type Program, uploadRgba8, uploadU16 } from './gl.ts';
import { PRECIP_FRAGMENT, SCALAR_FRAGMENT, VERTEX, WIND_FRAGMENT } from './shaders.ts';

export interface WeatherDisplay {
  layer: LayerSpec | null;
  /** Frame shown, and the one being faded towards (with `mix` 0..1). */
  a: TFrame | null;
  b: TFrame | null;
  mix: number;
  /** The frame shown before `a`; drawn where `a` has nothing loaded yet. */
  fallback: TFrame | null;
  /** Frames to load ahead of time, most wanted first. */
  prefetch: TFrame[];
  /** While playing: the loop's frames, in order. */
  loop?: readonly TFrame[];
}

export interface WeatherStyle {
  palette: Palette;
  stepped: boolean;
  opacity: number;
  windByGust: boolean;
  arrows: boolean;
}

/** One textured quad of the weather layer, in tile coordinates (x may lie
 *  outside 0..2^z on a world copy). */
export interface Quad {
  z: number;
  x: number;
  y: number;
  texA: WebGLTexture | null;
  uvA: SubRect | null;
  texB: WebGLTexture | null;
  uvB: SubRect | null;
  mix: number;
}

/** Bind a quad's two textures (units 0 and 1) and set where it is and how
 *  it reads them: what the colour layer and the particles' velocity pass do
 *  for every quad. */
export function setQuad(gl: WebGL2RenderingContext, u: Program['uniforms'], q: Quad, placeholder: WebGLTexture | null) {
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, q.texA ?? placeholder);
  gl.activeTexture(gl.TEXTURE1);
  gl.bindTexture(gl.TEXTURE_2D, q.texB ?? placeholder);
  gl.uniform4f(u.u_uvA, q.uvA?.ox ?? 0, q.uvA?.oy ?? 0, q.uvA?.s ?? 1, q.uvA && q.texA ? 1 : 0);
  gl.uniform4f(u.u_uvB, q.uvB?.ox ?? 0, q.uvB?.oy ?? 0, q.uvB?.s ?? 1, q.uvB && q.texB ? 1 : 0);
  gl.uniform1f(u.u_mix, q.mix);
  const n = 2 ** q.z;
  gl.uniform3f(u.u_rect, q.x / n, q.y / n, 1 / n);
}

/** A whole tile drawn as is. */
const WHOLE: SubRect = { ox: 0, oy: 0, s: 1 };

export interface Sample {
  frame: TFrame;
  /** Stored-unit values of the rendered bands, in `bandsFor(render)` order. */
  values: (number | null)[];
  /** No tile covers this point (yet), or the tile is empty (204). */
  status: 'ok' | 'empty' | 'loading';
  /** The top part whose bounds hold the point: where the value comes from. */
  top: Part | null;
}

const BANDS = new WeakMap<Render, number[]>();

/** The bands a render kind needs, in texture channel order (one array per
 *  render spec: callers must not change it). */
export function bandsFor(render: Render): number[] {
  let b = BANDS.get(render);
  if (!b) {
    b = render.kind === 'scalar' ? [render.band] : render.kind === 'wind' ? (render.gust === null ? [render.u, render.v] : [render.u, render.v, render.gust]) : [render.rate, render.ptype];
    BANDS.set(render, b);
  }
  return b;
}

const UPLOADS_PER_FRAME = 6;
const PREFETCH_TILE_LIMIT = 260;

/** The tile a frame's cell is drawn from: a loaded tile, nothing to draw
 *  (an empty tile), or nothing loaded yet. */
type Pick = Leaf | 'empty' | null;

type Stats = { slots: number; exact: number; ancestor: number; fallback: number; empty: number; missing: number; zooms: number[] };
const noStats = (slots: number): Stats => ({ slots, exact: 0, ancestor: 0, fallback: 0, empty: 0, missing: 0, zooms: [] });

/** The view's cells for a, b and fallback together, for one view and frames. */
interface Plan {
  view: number;
  a: TFrame | null;
  b: TFrame | null;
  fallback: TFrame | null;
  cells: Cell[];
}

/** What a frame's tiles were drawn from: valid while the plan, the store and
 *  whether a fade is under way stay the same. Quads whose mix is the
 *  display's (`fades`) take its current value each frame. */
interface Drawn {
  plan: Plan;
  version: number;
  fading: boolean;
  quads: Quad[];
  fades: boolean[];
  mix: number;
  stats: Stats;
  protect: Set<string>;
}

const sameFrames = (x: readonly TFrame[], y: readonly TFrame[]) => x.length === y.length && x.every((f, i) => f === y[i]);

export class WeatherLayer implements CustomLayerInterface {
  readonly id = 'weather';
  readonly type = 'custom' as const;
  readonly renderingMode = '2d' as const;

  private map: MlMap | null = null;
  private gl: WebGL2RenderingContext | null = null;
  private programs = new Map<Render['kind'], Program>();
  private vao: WebGLVertexArrayObject | null = null;
  private vbo: WebGLBuffer | null = null;
  private lut: WebGLTexture | null = null;
  private lutKey = '';
  private dummy: WebGLTexture | null = null;
  private display: WeatherDisplay = { layer: null, a: null, b: null, mix: 0, fallback: null, prefetch: [] };
  private style: WeatherStyle = { palette: getPalette('reflectivity'), stepped: false, opacity: 0.85, windByGust: false, arrows: true };
  private plan: Plan | null = null;
  /** The view planned for, and a number that changes whenever it does. */
  private view: View = { centerX: NaN, centerY: NaN, zoom: NaN, width: 0, height: 0 };
  private viewGen = 0;
  private rect: ViewRect = { x0: 0, x1: 0, y0: 0, y1: 0 };
  private zWant = 0;
  /** The last requests made, and what they were made from. */
  private asked: { plan: Plan; prefetch: readonly TFrame[]; version: number; reqs: TileRequest[]; required: number } | null = null;
  private requests = new WeakMap<Leaf, TileRequest>();
  private drawn: Drawn | null = null;
  private matrix = new Float32Array(16);
  private readonly store: TileStore;
  private readonly urlFor: (layer: string, z: number, x: number, y: number, parts: string) => string;
  private frameTiles = new WeakMap<TFrame, FrameTiles>();
  private singlePlans = new WeakMap<TFrame, { view: number; cells: Cell[] }>();
  private lostContext = false;
  private sourcesKey = '';
  /** What the last draw used, for the smoke test and debugging. */
  stats: Stats = noStats(0);
  onError: (e: Error) => void = (e) => console.error(e);
  /** The sources with a part in view (in the frames shown) changed. */
  onViewSources: (ids: string[]) => void = () => {};

  constructor(store: TileStore, urlFor: (layer: string, z: number, x: number, y: number, parts: string) => string) {
    this.store = store;
    this.urlFor = urlFor;
    store.disposeTexture = (t) => this.gl?.deleteTexture(t);
  }

  setDisplay(d: WeatherDisplay) {
    this.display = d;
    this.map?.triggerRepaint();
  }

  setStyle(s: WeatherStyle) {
    this.style = s;
    this.map?.triggerRepaint();
  }

  getDisplay(): Readonly<WeatherDisplay> {
    return this.display;
  }

  onAdd(map: MlMap, gl: WebGL2RenderingContext) {
    this.map = map;
    this.gl = gl;
    this.lostContext = false;
    this.dropGlState();
    map.on('webglcontextlost', this.onLost);
    map.on('webglcontextrestored', this.onRestored);
    this.init(gl);
  }

  onRemove(map: MlMap, gl: WebGL2RenderingContext) {
    map.off('webglcontextlost', this.onLost);
    map.off('webglcontextrestored', this.onRestored);
    // MapLibre also removes custom layers when its context is lost or the
    // style is replaced, and adds them back later with a new context: every
    // GL object made here must be forgotten, not just deleted.
    if (!gl.isContextLost()) {
      for (const p of this.programs.values()) gl.deleteProgram(p.program);
      gl.deleteVertexArray(this.vao);
      gl.deleteBuffer(this.vbo);
      gl.deleteTexture(this.lut);
      gl.deleteTexture(this.dummy);
    }
    this.dropGlState();
    this.map = null;
    this.gl = null;
  }

  private dropGlState() {
    // The textures die with the context; the decoded samples do not.
    this.store.forgetTextures();
    this.drawn = null;
    this.programs.clear();
    this.vao = null;
    this.vbo = null;
    this.lut = null;
    this.lutKey = '';
    this.dummy = null;
  }

  private onLost = () => {
    this.lostContext = true;
    this.dropGlState();
  };

  private onRestored = () => {
    this.lostContext = false;
    if (this.gl) this.init(this.gl);
  };

  private init(gl: WebGL2RenderingContext) {
    this.vao = gl.createVertexArray();
    this.vbo = gl.createBuffer();
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.vbo);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    gl.bindVertexArray(null);
    // Unused integer samplers still need an integer texture bound.
    this.dummy = uploadU16(gl, new Uint16Array([0]), 1, 1, 1);
  }

  private program(gl: WebGL2RenderingContext, kind: Render['kind']): Program | null {
    let p = this.programs.get(kind);
    if (p) return p;
    const fs = kind === 'scalar' ? SCALAR_FRAGMENT : kind === 'wind' ? WIND_FRAGMENT : PRECIP_FRAGMENT;
    try {
      p = createProgram(gl, VERTEX, fs, ['a_pos']);
    } catch (e) {
      this.onError(e as Error);
      return null;
    }
    this.programs.set(kind, p);
    return p;
  }

  // ------------------------------------------------------------- planning

  /** Stacks and URLs of a frame's tiles. */
  tilesOf(f: TFrame): FrameTiles {
    let t = this.frameTiles.get(f);
    if (!t) {
      // A frame belongs to one layer; the display's layer is it.
      const layer = this.display.layer?.id ?? '';
      t = new FrameTiles(f.stack, (z, x, y, parts) => this.urlFor(layer, z, x, y, parts));
      this.frameTiles.set(f, t);
    }
    return t;
  }

  /** Take the map's current view; plans made for another view are stale. */
  private updateView() {
    const map = this.map!;
    const c = map.getCenter();
    const canvas = map.getCanvas();
    const v = this.view;
    const cx = lonToX(c.lng);
    const cy = latToY(c.lat);
    const zoom = map.getZoom();
    if (v.centerX === cx && v.centerY === cy && v.zoom === zoom && v.width === canvas.clientWidth && v.height === canvas.clientHeight) return;
    this.view = { centerX: cx, centerY: cy, zoom, width: canvas.clientWidth, height: canvas.clientHeight };
    this.rect = viewRect(this.view);
    this.zWant = tileZoomFor(zoom, MAX_TILE_ZOOM);
    this.viewGen++;
  }

  /** One frame's own cells for the current view (for prefetch and readiness). */
  private planOf(f: TFrame): Cell[] {
    const p = this.singlePlans.get(f);
    if (p && p.view === this.viewGen) return p.cells;
    const cells = planCells(this.rect, this.zWant, [this.tilesOf(f)]);
    this.singlePlans.set(f, { view: this.viewGen, cells });
    return cells;
  }

  private request(l: Leaf, pick: number[]): TileRequest {
    let r = this.requests.get(l);
    if (!r || r.pick !== pick) this.requests.set(l, (r = { url: l.url!, tile: l, pick }));
    return r;
  }

  private planView() {
    const { layer, a, b, fallback, prefetch } = this.display;
    if (!layer || !this.map) {
      this.plan = null;
      this.store.want([]);
      return;
    }
    this.updateView();
    const fb = fallback && fallback !== a ? fallback : null;
    const p = this.plan;
    if (!p || p.view !== this.viewGen || p.a !== a || p.b !== b || p.fallback !== fb) {
      this.plan = { view: this.viewGen, a, b, fallback: fb, cells: planCells(this.rect, this.zWant, [a, b, fb].map((f) => (f ? this.tilesOf(f) : null))) };
      this.reportSources();
    }
    const plan = this.plan!;
    // The same plan, prefetch and tiles ask for the same tiles again.
    const last = this.asked;
    if (!last || last.plan !== plan || last.version !== this.store.version || !sameFrames(last.prefetch, prefetch)) {
      const pick = bandsFor(layer.render);
      const reqs: TileRequest[] = [];
      const seen = new Set<string>();
      const add = (l: Leaf) => {
        if (!l.url || seen.has(l.url)) return;
        seen.add(l.url);
        reqs.push(this.request(l, pick));
      };
      if (a) {
        const ft = this.tilesOf(a);
        // Where a cell has nothing to draw yet (new ground after a pan or zoom),
        // ask first for its tile two levels up: one tile covers sixteen,
        // arrives quickly, and is magnified until the sharp tiles land.
        for (const c of plan.cells) {
          const la = c.leaves[0];
          if (!la?.url || la.z < 2 || this.pickFor(ft, la)) continue;
          add(ft.at(la.z - 2, la.x >> 2, la.y >> 2));
        }
        for (const c of plan.cells) if (c.leaves[0]) add(c.leaves[0]);
      }
      if (b) for (const c of plan.cells) if (c.leaves[1]) add(c.leaves[1]);
      const required = reqs.length;
      for (const f of prefetch) {
        if (reqs.length >= required + PREFETCH_TILE_LIMIT) break;
        for (const c of this.planOf(f)) {
          if (reqs.length >= required + PREFETCH_TILE_LIMIT) break;
          if (c.leaves[0]) add(c.leaves[0]);
        }
      }
      this.asked = { plan, prefetch, version: this.store.version, reqs, required };
    }
    // What is on screen is always wanted; prefetch only as far as memory allows.
    this.store.want(this.asked!.reqs, performance.now(), { required: this.asked!.required });
  }

  /** Every tile of frame `f` in view is settled (loaded, empty or gone). A
   *  failed tile is not: it will be asked for again, and until then the
   *  frame would be drawn with holes (or not at all). */
  isReady(f: TFrame | null): boolean {
    if (!f || !this.display.layer || !this.map) return false;
    if (!this.viewGen) this.updateView();
    for (const c of this.planOf(f)) {
      const url = c.leaves[0]?.url;
      if (!url) continue;
      const st = this.store.state(url);
      if (st === undefined || st === 'loading' || st === 'error') return false;
    }
    return true;
  }

  /**
   * Which sources have a part in view in the frame shown (and the one being
   * faded towards): the parts each cell's stack names whose bounds reach the
   * visible part of the cell, so a source just off screen is not credited.
   */
  private reportSources() {
    const { a, b } = this.display;
    const ids = new Set<string>();
    const rect = this.rect;
    for (const c of this.plan!.cells) {
      const n = 2 ** c.z;
      const x0 = Math.max(c.x / n, rect.x0 - c.wrap);
      const x1 = Math.min((c.x + 1) / n, rect.x1 - c.wrap);
      const y0 = Math.max(c.y / n, rect.y0);
      const y1 = Math.min((c.y + 1) / n, rect.y1);
      if (!(x1 > x0 && y1 > y0)) continue;
      const box = { west: xToLon(x0), east: xToLon(x1), north: yToLat(y0), south: yToLat(y1) };
      for (const [i, f] of [a, b].entries()) {
        const leaf = c.leaves[i];
        if (!f || !leaf?.url) continue;
        for (const p of tileParts(leaf.parts, box)) ids.add(p.source);
      }
    }
    const list = [...ids].sort();
    const key = list.join(',');
    if (key === this.sourcesKey) return;
    this.sourcesKey = key;
    this.onViewSources(list);
  }

  // ------------------------------------------------------------- drawing

  prerender(gl: WebGL2RenderingContext) {
    if (this.lostContext) return;
    try {
      this.planView();
      // Upload a few prefetched tiles per frame so playback finds them ready.
      let n = 0;
      for (const e of this.store.pendingUploads()) {
        if (n++ >= UPLOADS_PER_FRAME) {
          this.map?.triggerRepaint();
          break;
        }
        this.upload(gl, e);
      }
    } catch (e) {
      this.onError(e as Error);
    }
  }

  private upload(gl: WebGL2RenderingContext, e: TileEntry): WebGLTexture | null {
    if (e.texture) return e.texture;
    if (!e.data) return null;
    // A scratch unit: uploads happen mid-draw, and binding on the active unit
    // would replace the palette texture on unit 2 or a tile on 0 and 1.
    gl.activeTexture(gl.TEXTURE3);
    const tex = uploadU16(gl, e.data, e.width, e.height, e.channels);
    this.store.setTexture(e, tex);
    return tex;
  }

  private ensureLut(gl: WebGL2RenderingContext) {
    const key = `${this.style.palette.id}|${this.style.stepped}`;
    if (key === this.lutKey && this.lut) return;
    gl.activeTexture(gl.TEXTURE3);
    const { data, width, height } = buildLut(this.style.palette, this.style.stepped);
    this.lut = uploadRgba8(gl, this.lut, data, width, height);
    this.lutKey = key;
  }

  /**
   * The tile to draw a cell from for one frame whose own tile there is
   * `leaf` (the cell or an ancestor of it): that tile once loaded, else the
   * nearest loaded ancestor, magnified. An empty (204) tile or ancestor means
   * there is nothing to draw. Null: nothing usable is loaded.
   */
  private pickFor(ft: FrameTiles, leaf: Leaf): Pick {
    if (!leaf.url) return 'empty';
    for (let z = leaf.z; z >= 0; z--) {
      const t = z === leaf.z ? leaf : ft.at(z, leaf.x >> (leaf.z - z), leaf.y >> (leaf.z - z));
      const s = this.store.state(t.url!);
      if (s === 'ready') return t;
      if (s === 'empty') return 'empty';
    }
    return null;
  }

  render(gl: WebGL2RenderingContext, args: CustomRenderMethodInput) {
    if (this.lostContext) return;
    const { layer } = this.display;
    if (!layer || !this.vao || !this.plan) {
      this.stats = noStats(this.plan?.cells.length ?? 0);
      return;
    }
    const prog = this.program(gl, layer.render.kind);
    if (!prog) {
      this.stats = noStats(this.plan.cells.length);
      return;
    }
    try {
      this.draw(gl, args, prog, layer);
    } catch (e) {
      this.onError(e as Error);
    } finally {
      gl.bindVertexArray(null);
    }
  }

  private draw(gl: WebGL2RenderingContext, args: CustomRenderMethodInput, prog: Program, layer: LayerSpec) {
    this.ensureLut(gl);
    const u = prog.uniforms;
    const pal = this.style.palette;
    const tf = transferFn(pal.transfer);
    this.matrix.set(args.defaultProjectionData.mainMatrix as ArrayLike<number>);

    gl.useProgram(prog.program);
    gl.bindVertexArray(this.vao);
    gl.uniformMatrix4fv(u.u_matrix, false, this.matrix);
    gl.uniform1i(u.u_texA, 0);
    gl.uniform1i(u.u_texB, 1);
    gl.uniform1i(u.u_lut, 2);
    gl.activeTexture(gl.TEXTURE2);
    gl.bindTexture(gl.TEXTURE_2D, this.lut);
    gl.uniform3f(u.u_lutRange, tf(pal.domain[0]), tf(pal.domain[1]), TRANSFER_CODE[pal.transfer]);
    gl.uniform1f(u.u_opacity, this.style.opacity);
    const bands = bandsFor(layer.render);
    for (let i = 0; i < bands.length; i++) {
      const loc = u[`u_q${i}`];
      const band = layer.bands[bands[i]];
      if (loc && band) gl.uniform2f(loc, band.scale, band.offset);
    }
    const r = layer.render;
    const wind = r.kind === 'wind';
    if (wind) {
      gl.uniform1f(u.u_hasGust, r.gust === null ? 0 : 1);
      gl.uniform1f(u.u_byGust, this.style.windByGust ? 1 : 0);
      gl.uniform1f(u.u_arrows, this.style.arrows ? 1 : 0);
    }

    const zoom = this.map!.getZoom();
    const drawn = this.collect(gl);
    this.stats = drawn.stats;
    for (const q of drawn.quads) {
      setQuad(gl, u, q, this.dummy);
      if (wind) {
        const pxPerTile = 512 * 2 ** (zoom - q.z);
        gl.uniform1f(u.u_cells, Math.max(2, Math.min(48, Math.round(pxPerTile / 58))));
      }
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    }
    const { loop } = this.display;
    this.store.evict(drawn.protect, loop?.length ? () => this.nextUse(loop) : undefined);
  }

  /** How many frames ahead of the one shown each of the loop's tiles in view
   *  is next drawn. */
  private nextUse(loop: readonly TFrame[]): Map<string, number> {
    const out = new Map<string, number>();
    const start = Math.max(0, loop.indexOf(this.display.a!));
    for (let k = 0; k < loop.length; k++) {
      for (const c of this.planOf(loop[(start + k) % loop.length])) {
        const url = c.leaves[0]?.url;
        if (url && !out.has(url)) out.set(url, k);
      }
    }
    return out;
  }

  /**
   * What covers the view for the frames on display: one quad per cell (or
   * per loaded child, just after zooming out), with each frame's texture and
   * where in it the cell lies. `render` draws these; the wind particles read
   * their velocities from the same quads. Uploads textures as needed, so it
   * belongs between `prerender` and the end of `render`.
   */
  quads(gl: WebGL2RenderingContext): Quad[] {
    if (!this.display.layer || this.lostContext || !this.plan) return [];
    return this.collect(gl).quads;
  }

  /** The quads for the current plan, worked out again only when the plan or
   *  the store's tiles changed, or a fade started or ended. */
  private collect(gl: WebGL2RenderingContext): Drawn {
    const { mix } = this.display;
    const plan = this.plan!;
    let d = this.drawn;
    if (!d || d.plan !== plan || d.version !== this.store.version || d.fading !== mix > 0) d = this.drawn = this.build(gl, plan, mix > 0);
    if (d.mix !== mix) {
      for (let i = 0; i < d.quads.length; i++) if (d.fades[i]) d.quads[i].mix = mix;
      d.mix = mix;
    }
    return d;
  }

  private build(gl: WebGL2RenderingContext, plan: Plan, fading: boolean): Drawn {
    const { a, b, fallback, mix } = this.display;
    const stats = noStats(plan.cells.length);
    const protect = new Set<string>();
    const quads: Quad[] = [];
    const fades: boolean[] = [];
    const zooms = new Set<number>();
    const texFor = (p: Pick): WebGLTexture | null => {
      if (!p || p === 'empty') return null;
      const e = this.store.get(p.url!);
      if (!e) return null;
      protect.add(e.key);
      zooms.add(p.z);
      return this.upload(gl, e);
    };
    const fta = a ? this.tilesOf(a) : null;
    const ftb = b ? this.tilesOf(b) : null;
    const ftf = fallback && fallback !== a ? this.tilesOf(fallback) : null;
    for (const c of plan.cells) {
      const [la, lb, lf] = c.leaves;
      for (const l of [la, lb]) if (l?.url) protect.add(l.url);
      let pa: Pick = fta && la ? this.pickFor(fta, la) : null;
      let fromFallback = false;
      if (!pa && ftf && lf) {
        pa = this.pickFor(ftf, lf);
        fromFallback = !!pa;
        if (pa) stats.fallback++;
      }
      const pb: Pick = ftb && lb && fading ? this.pickFor(ftb, lb) : null;
      const n = 2 ** c.z;
      if (!pa && !pb) {
        // Just zoomed out: the coarser tile is still loading, but the four
        // sharper ones it replaces are probably still here. Use those.
        if (fta && la && this.childQuads(fta, la, c, texFor, quads)) stats.ancestor++;
        else stats.missing++;
        while (fades.length < quads.length) fades.push(false);
        continue;
      }
      if ((pa ?? 'empty') === 'empty' && (pb ?? 'empty') === 'empty') {
        stats.empty++;
        continue;
      }
      if (pa && pa !== 'empty' && !fromFallback) {
        if (la && pa.z === la.z) stats.exact++;
        else stats.ancestor++;
      }
      const texA = texFor(pa);
      const texB = texFor(pb);
      // Never fade towards a frame that has not loaded: hold the current one.
      const fade = !!pb && !!pa;
      quads.push({
        z: c.z,
        x: c.x + c.wrap * n,
        y: c.y,
        texA,
        uvA: pa && pa !== 'empty' ? subRect(c, pa) : null,
        texB,
        uvB: pb && pb !== 'empty' ? subRect(c, pb) : null,
        mix: !pb ? 0 : !pa ? 1 : mix,
      });
      fades.push(fade);
    }
    // The zooms of the tiles drawn from, not of the cells they fill.
    stats.zooms = [...zooms].sort((x, y) => x - y);
    return { plan, version: this.store.version, fading, quads, fades, mix, stats, protect };
  }

  /** A cell covered by its four children one zoom level down, where frame
   *  `a` has them loaded. True if any was. */
  private childQuads(ft: FrameTiles, leaf: Leaf, c: Cell, texFor: (p: Pick) => WebGLTexture | null, out: Quad[]): boolean {
    if (leaf.z !== c.z || !leaf.url || c.z + 1 > Math.min(leaf.maxZoom, MAX_TILE_ZOOM)) return false;
    let found = false;
    for (let dy = 0; dy < 2; dy++) {
      for (let dx = 0; dx < 2; dx++) {
        const child = ft.at(c.z + 1, c.x * 2 + dx, c.y * 2 + dy);
        if (!child.url || this.store.state(child.url) !== 'ready') continue;
        const tex = texFor(child);
        if (!tex) continue;
        out.push({ z: child.z, x: child.x + c.wrap * 2 ** child.z, y: child.y, texA: tex, uvA: WHOLE, texB: null, uvB: null, mix: 0 });
        found = true;
      }
    }
    return found;
  }

  // ------------------------------------------------------------- read-out

  /** Values under a point, from the same tiles and interpolation the shader
   *  uses, for the frame that dominates the current cross-fade. */
  sample(lon: number, lat: number): Sample | null {
    const { layer, a, b, mix, fallback } = this.display;
    const frame = b && mix >= 0.5 ? b : a;
    if (!layer || !frame) return null;
    // Web-Mercator ends here: nothing is drawn nearer the poles.
    if (Math.abs(lat) > MAX_MERCATOR_LAT) return { frame, values: bandsFor(layer.render).map(() => null), status: 'empty', top: null };
    const wlon = ((((lon + 180) % 360) + 360) % 360) - 180;
    const mx = lonToX(wlon);
    const my = latToY(lat);
    const pick = bandsFor(layer.render);
    if (!this.viewGen && this.map) this.updateView();
    const tryFrame = (f: TFrame): Sample | null => {
      const top = topPartAt(f.stack, wlon, lat);
      const ft = this.tilesOf(f);
      const p = this.pickFor(ft, leafAt(ft, mx, my, this.zWant));
      if (!p) return null;
      if (p === 'empty') return { frame: f, values: pick.map(() => null), status: 'empty', top };
      const e = this.store.get(p.url!);
      if (!e?.data) return null;
      const { u, v } = tileFraction(p, mx, my);
      const values = pick.map((bandIdx, c) => {
        const band = layer.bands[bandIdx];
        const q =
          band.kind !== 'categorical'
            ? bilinearSample(e.data!, e.width, e.height, e.channels, c, u, v)
            : layer.render.kind === 'precip_type'
              ? nearestPositiveSample(e.data!, e.width, e.height, e.channels, c, u, v, band)
              : nearestSample(e.data!, e.width, e.height, e.channels, c, u, v);
        return q === null ? null : dequantize(band, q);
      });
      return { frame: f, values, status: 'ok', top };
    };
    return (
      tryFrame(frame) ??
      (fallback && fallback !== frame ? tryFrame(fallback) : null) ?? { frame, values: pick.map(() => null), status: 'loading', top: topPartAt(frame.stack, wlon, lat) }
    );
  }

  /** For tests and `?debug`: the URLs of the tiles frame `f` needs in view. */
  urlsInView(f: TFrame | null = this.display.a): string[] {
    if (!f || !this.map) return [];
    if (!this.viewGen) this.updateView();
    return [...new Set(this.planOf(f).flatMap((c) => (c.leaves[0]?.url ? [c.leaves[0].url] : [])))];
  }
}
