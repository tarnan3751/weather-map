// Wind as moving particles: a separate custom layer that draws above the
// colour layer, using the same tiles, frames and cross-fade.
//
// Each frame, on the GPU:
//   1. velocity: the visible tiles' u/v are drawn into a screen-aligned
//      half-float texture (a third of the screen's resolution), blending the
//      two frames being cross-faded exactly as the colour layer does;
//   2. advect: particle positions (screen fractions) live in two float
//      textures used in turn; each step reads the velocity under a particle
//      and moves it, re-seeding particles that age out, leave the view or land
//      on no-data;
//   3. trails: the previous trail image is faded into a second one and this
//      step's movements are drawn on top as 1-pixel segments;
//   4. composite: the trail image is drawn over the map.
//
// On-screen speed: u is eastward and v northward (m/s); screen y points down.
// Web-Mercator magnifies a distance at latitude φ by 1/cos φ, so a particle
// moves K · (u, −v) · 1/cos φ pixels a second. K is fixed rather than tied to
// the zoom (a real-time speed would be invisible zoomed out and frantic zoomed
// in), so the same wind reads the same at every zoom.
//
// Needs float render targets (EXT_color_buffer_float); without them, or with
// reduced motion, the app shows arrows instead.

import type { CustomLayerInterface, CustomRenderMethodInput, Map as MlMap } from 'maplibre-gl';
import type { LayerSpec } from '../api/types.ts';
import { latToY } from '../tiles/tileMath.ts';
import { createProgram, type Program, uploadU16 } from './gl.ts';
import { COMMON, VERTEX } from './shaders.ts';
import { bandsFor, setQuad, type WeatherLayer } from './weatherLayer.ts';

/** Pixels per second per m/s of wind at the equator. */
const SPEED = 3;
const FRAME_MS = 1000 / 30;

const VELOCITY_FRAGMENT = /* glsl */ `${COMMON}
uniform vec2 u_q0; // u
uniform vec2 u_q1; // v

vec3 velocityAt(usampler2D tex, vec4 uvT) {
  Around k = around(tex, uvT.xy + v_uv * uvT.z);
  vec2 u = bilinear(k, 0);
  vec2 v = bilinear(k, 1);
  if (u.y < 0.5 || v.y < 0.5) return vec3(0.0);
  return vec3(dequant(u_q0, u.x), dequant(u_q1, v.x), 1.0);
}

void main() {
  vec3 a = u_uvA.w > 0.5 ? velocityAt(u_texA, u_uvA) : vec3(0.0);
  vec3 b = u_uvB.w > 0.5 ? velocityAt(u_texB, u_uvB) : vec3(0.0);
  vec3 vel = a;
  if (a.z > 0.5 && b.z > 0.5) vel = vec3(mix(a.xy, b.xy, u_mix), 1.0);
  else if (b.z > 0.5 && (a.z < 0.5 || u_mix > 0.5)) vel = b;
  fragColor = vec4(vel, 1.0);
}`;

const QUAD_VERTEX = /* glsl */ `#version 300 es
in vec2 a_pos;
out vec2 v_uv;
void main() {
  v_uv = a_pos;
  gl_Position = vec4(a_pos * 2.0 - 1.0, 0.0, 1.0);
}`;

const UPDATE_FRAGMENT = /* glsl */ `#version 300 es
precision highp float;
uniform sampler2D u_state;
uniform sampler2D u_vel;
uniform vec2 u_screen;   // CSS pixels
uniform vec2 u_merc;     // mercator y of the view's top and bottom
uniform float u_dt;
uniform float u_time;
uniform float u_reset;
uniform float u_speed;
out vec4 fragColor;

float rand(vec2 co) {
  return fract(sin(dot(co, vec2(12.9898, 78.233))) * 43758.5453);
}

vec4 spawn(vec2 seed, bool stagger) {
  float life = mix(2.0, 5.0, rand(seed + 3.7));
  return vec4(rand(seed), rand(seed + 1.3), stagger ? rand(seed + 2.1) * life : 0.0, life);
}

void main() {
  ivec2 ij = ivec2(gl_FragCoord.xy);
  vec2 seed = vec2(ij) * 0.731 + fract(u_time * 0.0137) * 917.0;
  vec4 s = texelFetch(u_state, ij, 0);
  if (u_reset > 0.5) { fragColor = spawn(seed, true); return; }
  if (s.z >= s.w) { fragColor = spawn(seed, false); return; }
  // The velocity texture has GL's origin at the bottom; positions count
  // from the top of the screen.
  vec4 v = texture(u_vel, vec2(s.x, 1.0 - s.y));
  if (v.z < 0.5) { fragColor = spawn(seed, false); return; }
  float my = mix(u_merc.x, u_merc.y, s.y);
  // 1 / cos(latitude) at mercator y.
  float stretch = cosh(3.14159265 * (1.0 - 2.0 * my));
  vec2 px = vec2(v.x, -v.y) * u_speed * stretch * u_dt;
  vec2 p = s.xy + px / u_screen;
  if (p.x < 0.0 || p.x > 1.0 || p.y < 0.0 || p.y > 1.0) { fragColor = spawn(seed, false); return; }
  fragColor = vec4(p, s.z + u_dt, s.w);
}`;

const LINE_VERTEX = /* glsl */ `#version 300 es
precision highp float;
uniform sampler2D u_prev;
uniform sampler2D u_next;
uniform int u_size;
out float v_alpha;
void main() {
  int id = gl_VertexID / 2;
  ivec2 ij = ivec2(id % u_size, id / u_size);
  vec4 a = texelFetch(u_prev, ij, 0);
  vec4 b = texelFetch(u_next, ij, 0);
  // Age only grows while a particle moves; one that was just re-seeded (its
  // age went back to 0) would draw a line across the screen, so it draws a
  // point-sized, transparent segment instead.
  bool moved = b.z > a.z;
  vec4 s = (gl_VertexID % 2 == 1 || !moved) ? b : a;
  v_alpha = moved ? smoothstep(0.0, 0.5, b.z) * (1.0 - smoothstep(b.w - 0.7, b.w, b.z)) : 0.0;
  gl_Position = vec4(s.x * 2.0 - 1.0, 1.0 - s.y * 2.0, 0.0, 1.0);
}`;

const LINE_FRAGMENT = /* glsl */ `#version 300 es
precision highp float;
in float v_alpha;
out vec4 fragColor;
void main() {
  fragColor = vec4(0.85) * v_alpha;
}`;

const FADE_FRAGMENT = /* glsl */ `#version 300 es
precision highp float;
uniform sampler2D u_tex;
uniform float u_fade;
in vec2 v_uv;
out vec4 fragColor;
void main() {
  // Subtracting a step stops 8-bit rounding leaving permanent ghosts.
  fragColor = max(texture(u_tex, v_uv) * u_fade - vec4(1.5 / 255.0), 0.0);
}`;

const COMPOSITE_FRAGMENT = /* glsl */ `#version 300 es
precision highp float;
uniform sampler2D u_tex;
uniform float u_opacity;
in vec2 v_uv;
out vec4 fragColor;
void main() {
  fragColor = texture(u_tex, v_uv) * u_opacity;
}`;

interface Target {
  tex: WebGLTexture;
  fbo: WebGLFramebuffer;
}

export interface ParticleOptions {
  /** Upper bound on particles; scaled down for small screens. */
  maxParticles: number;
  /** Screen pixels per particle. */
  pixelsPerParticle: number;
  /** Behave as if float render targets were missing (for tests). */
  noFloat?: boolean;
}

export class WindParticles implements CustomLayerInterface {
  readonly id = 'wind-particles';
  readonly type = 'custom' as const;
  readonly renderingMode = '2d' as const;

  private map: MlMap | null = null;
  private gl: WebGL2RenderingContext | null = null;
  private programs: Record<string, Program> = {};
  private quad: WebGLVertexArrayObject | null = null;
  private quadBuf: WebGLBuffer | null = null;
  private lineVao: WebGLVertexArrayObject | null = null;
  private dummy: WebGLTexture | null = null;
  private velocity: Target | null = null;
  private state: Target[] = [];
  private trail: Target[] = [];
  private size = 0;
  private w = 0;
  private h = 0;
  private cur = 0;
  private trailCur = 0;
  private needReset = true;
  private moving = false;
  private wanted = false;
  private supported: boolean | null = null;
  private lastTime = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private lastFrameT = Number.NaN;
  private opacity = 0.9;
  private matrix = new Float32Array(16);
  private readonly weather: WeatherLayer;
  private readonly opts: ParticleOptions;
  /** Told whether this device can draw particles at all. */
  onSupport: (ok: boolean) => void = () => {};
  /** For tests and `?debug`: how many particles, steps taken, resets, and
   *  the last step's length in seconds. */
  stats = { particles: 0, steps: 0, resets: 0, dt: 0, running: false };

  constructor(weather: WeatherLayer, opts: ParticleOptions) {
    this.weather = weather;
    this.opts = opts;
  }

  /** Draw (and animate) only while the wind layer is shown in particle mode. */
  setActive(on: boolean, opacity: number) {
    this.opacity = opacity;
    if (on && !this.wanted) this.needReset = true;
    this.wanted = on;
    this.map?.triggerRepaint();
  }

  private get running(): boolean {
    const on = this.wanted && this.supported === true && !this.moving && (typeof document === 'undefined' || document.visibilityState === 'visible');
    this.stats.running = on;
    return on;
  }

  onAdd(map: MlMap, gl: WebGL2RenderingContext) {
    this.map = map;
    this.gl = gl;
    this.moving = false;
    this.dropGlState();
    // Float render targets are what this needs; without them, arrows.
    this.supported = !this.opts.noFloat && !!gl.getExtension('EXT_color_buffer_float');
    this.onSupport(this.supported);
    map.on('movestart', this.onMoveStart);
    map.on('moveend', this.onMoveEnd);
    map.on('resize', this.onResize);
    document.addEventListener('visibilitychange', this.onVisibility);
  }

  onRemove(map: MlMap, gl: WebGL2RenderingContext) {
    map.off('movestart', this.onMoveStart);
    map.off('moveend', this.onMoveEnd);
    map.off('resize', this.onResize);
    document.removeEventListener('visibilitychange', this.onVisibility);
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    // Like the colour layer: MapLibre removes custom layers when the context
    // is lost and adds them back with a new one.
    if (!gl.isContextLost()) this.deleteGlObjects(gl);
    this.dropGlState();
    this.map = null;
    this.gl = null;
  }

  private onMoveStart = () => {
    this.moving = true;
  };

  private onMoveEnd = () => {
    this.moving = false;
    this.needReset = true;
    this.map?.triggerRepaint();
  };

  private onResize = () => {
    this.w = this.h = 0;
    this.needReset = true;
  };

  private onVisibility = () => {
    if (document.visibilityState === 'visible') {
      this.needReset = true;
      this.map?.triggerRepaint();
    }
  };

  private deleteGlObjects(gl: WebGL2RenderingContext) {
    for (const p of Object.values(this.programs)) gl.deleteProgram(p.program);
    gl.deleteVertexArray(this.quad);
    gl.deleteVertexArray(this.lineVao);
    gl.deleteBuffer(this.quadBuf);
    gl.deleteTexture(this.dummy);
    this.deleteTargets(gl);
  }

  private deleteTargets(gl: WebGL2RenderingContext) {
    for (const t of [this.velocity, ...this.state, ...this.trail]) {
      if (!t) continue;
      gl.deleteTexture(t.tex);
      gl.deleteFramebuffer(t.fbo);
    }
  }

  private dropGlState() {
    this.programs = {};
    this.quad = this.lineVao = null;
    this.quadBuf = null;
    this.dummy = null;
    this.velocity = null;
    this.state = [];
    this.trail = [];
    this.size = this.w = this.h = 0;
    this.needReset = true;
  }

  // ------------------------------------------------------------ resources

  /** A texture to render into; throws if this device cannot render to it. */
  private target(gl: WebGL2RenderingContext, w: number, h: number, internal: number, format: number, type: number, filter: number): Target {
    const tex = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texImage2D(gl.TEXTURE_2D, 0, internal, w, h, 0, format, type, null);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    const fbo = gl.createFramebuffer()!;
    gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
    const t = { tex, fbo };
    if (gl.checkFramebufferStatus(gl.FRAMEBUFFER) !== gl.FRAMEBUFFER_COMPLETE) {
      gl.deleteTexture(tex);
      gl.deleteFramebuffer(fbo);
      throw new Error('cannot render to a float texture');
    }
    return t;
  }

  private unsupported(e: Error): false {
    console.warn('wind particles unavailable:', e.message);
    this.supported = false;
    this.onSupport(false);
    return false;
  }

  private ensure(gl: WebGL2RenderingContext): boolean {
    if (!this.programs.velocity) {
      try {
        this.programs = {
          velocity: createProgram(gl, VERTEX, VELOCITY_FRAGMENT, ['a_pos']),
          update: createProgram(gl, QUAD_VERTEX, UPDATE_FRAGMENT, ['a_pos']),
          line: createProgram(gl, LINE_VERTEX, LINE_FRAGMENT),
          fade: createProgram(gl, QUAD_VERTEX, FADE_FRAGMENT, ['a_pos']),
          composite: createProgram(gl, QUAD_VERTEX, COMPOSITE_FRAGMENT, ['a_pos']),
        };
      } catch (e) {
        return this.unsupported(e as Error);
      }
      this.quad = gl.createVertexArray();
      this.quadBuf = gl.createBuffer();
      gl.bindVertexArray(this.quad);
      gl.bindBuffer(gl.ARRAY_BUFFER, this.quadBuf);
      gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]), gl.STATIC_DRAW);
      gl.enableVertexAttribArray(0);
      gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
      // Lines read everything from textures by vertex id.
      this.lineVao = gl.createVertexArray();
      gl.bindVertexArray(null);
      this.dummy = uploadU16(gl, new Uint16Array([0]), 1, 1, 1);
    }
    const canvas = this.map!.getCanvas();
    const w = Math.max(1, Math.min(2048, canvas.clientWidth));
    const h = Math.max(1, Math.min(2048, canvas.clientHeight));
    if (w === this.w && h === this.h && this.velocity) return true;
    this.deleteTargets(gl);
    this.w = w;
    this.h = h;
    const n = Math.max(1024, Math.min(this.opts.maxParticles, Math.round((w * h) / this.opts.pixelsPerParticle)));
    this.size = Math.ceil(Math.sqrt(n));
    this.stats.particles = this.size * this.size;
    try {
      this.velocity = this.target(gl, Math.ceil(w / 3), Math.ceil(h / 3), gl.RGBA16F, gl.RGBA, gl.HALF_FLOAT, gl.LINEAR);
      this.state = [0, 1].map(() => this.target(gl, this.size, this.size, gl.RGBA32F, gl.RGBA, gl.FLOAT, gl.NEAREST));
      this.trail = [0, 1].map(() => this.target(gl, w, h, gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, gl.LINEAR));
    } catch (e) {
      return this.unsupported(e as Error);
    } finally {
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    }
    this.needReset = true;
    return true;
  }

  // ------------------------------------------------------------- drawing

  /** The wind under each screen pixel (a third of the resolution), from the
   *  same quads, tiles and cross-fade the colour layer draws. */
  private drawVelocity(gl: WebGL2RenderingContext, args: CustomRenderMethodInput, layer: LayerSpec) {
    const p = this.programs.velocity;
    const u = p.uniforms;
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.velocity!.fbo);
    gl.viewport(0, 0, Math.ceil(this.w / 3), Math.ceil(this.h / 3));
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    const quads = this.weather.quads(gl);
    if (!quads.length) return;
    gl.useProgram(p.program);
    gl.bindVertexArray(this.quad);
    this.matrix.set(args.defaultProjectionData.mainMatrix as ArrayLike<number>);
    gl.uniformMatrix4fv(u.u_matrix, false, this.matrix);
    gl.uniform1i(u.u_texA, 0);
    gl.uniform1i(u.u_texB, 1);
    const [iu, iv] = bandsFor(layer.render);
    gl.uniform2f(u.u_q0, layer.bands[iu].scale, layer.bands[iu].offset);
    gl.uniform2f(u.u_q1, layer.bands[iv].scale, layer.bands[iv].offset);
    for (const q of quads) {
      setQuad(gl, u, q, this.dummy);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    }
  }

  prerender(gl: WebGL2RenderingContext, args: CustomRenderMethodInput) {
    const d = this.weather.getDisplay();
    const layer = d.layer;
    if (!this.running || !layer || layer.render.kind !== 'wind') return;
    if (!this.ensure(gl)) return;
    const now = performance.now();
    const dt = Math.min(0.05, Math.max(0.001, (now - (this.lastTime || now)) / 1000));
    this.lastTime = now;
    // A different frame chosen by the user (not a step of playback, which
    // cross-fades): start the streaks afresh rather than drag old ones along.
    const frameT = d.a?.t ?? Number.NaN;
    if (frameT !== this.lastFrameT && !Number.isNaN(frameT) && !d.b) this.needReset = true;
    this.lastFrameT = frameT;

    gl.disable(gl.BLEND);
    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.STENCIL_TEST);
    gl.disable(gl.SCISSOR_TEST);
    gl.disable(gl.CULL_FACE);
    gl.colorMask(true, true, true, true);

    this.drawVelocity(gl, args, layer);

    // Advect.
    const reset = this.needReset;
    if (reset) {
      this.stats.resets++;
      for (const t of this.trail) {
        gl.bindFramebuffer(gl.FRAMEBUFFER, t.fbo);
        gl.viewport(0, 0, this.w, this.h);
        gl.clearColor(0, 0, 0, 0);
        gl.clear(gl.COLOR_BUFFER_BIT);
      }
    }
    const map = this.map!;
    const top = latToY(map.unproject([0, 0]).lat);
    const bottom = latToY(map.unproject([0, this.h]).lat);
    const up = this.programs.update;
    const prev = this.cur;
    const next = 1 - prev;
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.state[next].fbo);
    gl.viewport(0, 0, this.size, this.size);
    gl.useProgram(up.program);
    gl.bindVertexArray(this.quad);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.state[prev].tex);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.velocity!.tex);
    gl.uniform1i(up.uniforms.u_state, 0);
    gl.uniform1i(up.uniforms.u_vel, 1);
    gl.uniform2f(up.uniforms.u_screen, this.w, this.h);
    gl.uniform2f(up.uniforms.u_merc, top, bottom);
    gl.uniform1f(up.uniforms.u_dt, dt);
    gl.uniform1f(up.uniforms.u_time, now / 1000);
    gl.uniform1f(up.uniforms.u_reset, reset ? 1 : 0);
    gl.uniform1f(up.uniforms.u_speed, SPEED);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    this.cur = next;
    this.needReset = false;
    this.stats.steps++;
    this.stats.dt = reset ? 0 : dt;

    // Trails: fade the last image into the other buffer, then add this step.
    const from = this.trailCur;
    const to = 1 - from;
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.trail[to].fbo);
    gl.viewport(0, 0, this.w, this.h);
    const fade = this.programs.fade;
    gl.useProgram(fade.program);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.trail[from].tex);
    gl.uniform1i(fade.uniforms.u_tex, 0);
    // About a second of trail at any frame rate.
    gl.uniform1f(fade.uniforms.u_fade, Math.pow(0.04, dt));
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    if (!reset) {
      gl.enable(gl.BLEND);
      gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
      const line = this.programs.line;
      gl.useProgram(line.program);
      gl.bindVertexArray(this.lineVao);
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, this.state[prev].tex);
      gl.activeTexture(gl.TEXTURE1);
      gl.bindTexture(gl.TEXTURE_2D, this.state[next].tex);
      gl.uniform1i(line.uniforms.u_prev, 0);
      gl.uniform1i(line.uniforms.u_next, 1);
      gl.uniform1i(line.uniforms.u_size, this.size);
      gl.drawArrays(gl.LINES, 0, this.size * this.size * 2);
      gl.disable(gl.BLEND);
    }
    this.trailCur = to;
    gl.bindVertexArray(null);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    this.schedule();
  }

  /** Keep animating at about 30 frames a second while running. */
  private schedule() {
    if (this.timer || !this.running) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      if (this.running) this.map?.triggerRepaint();
    }, FRAME_MS);
  }

  render(gl: WebGL2RenderingContext) {
    if (!this.running || !this.trail.length || this.needReset) return;
    const layer = this.weather.getDisplay().layer;
    if (!layer || layer.render.kind !== 'wind') return;
    const c = this.programs.composite;
    gl.useProgram(c.program);
    gl.bindVertexArray(this.quad);
    gl.disable(gl.DEPTH_TEST);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.trail[this.trailCur].tex);
    gl.uniform1i(c.uniforms.u_tex, 0);
    gl.uniform1f(c.uniforms.u_opacity, this.opacity);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    gl.bindVertexArray(null);
  }

  /**
   * For tests: where `count` particles were before and after the last step
   * (`stats.dt` seconds), in CSS pixels, skipping any that were re-seeded.
   */
  debugSteps(count: number): { from: [number, number]; to: [number, number] }[] {
    const gl = this.gl;
    if (!gl || !this.state.length) return [];
    const read = (t: Target) => {
      const out = new Float32Array(this.size * this.size * 4);
      gl.bindFramebuffer(gl.FRAMEBUFFER, t.fbo);
      gl.readPixels(0, 0, this.size, this.size, gl.RGBA, gl.FLOAT, out);
      return out;
    };
    const after = read(this.state[this.cur]);
    const before = read(this.state[1 - this.cur]);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    const steps: { from: [number, number]; to: [number, number] }[] = [];
    for (let i = 0; i < this.size * this.size && steps.length < count; i++) {
      const o = i * 4;
      if (!(after[o + 2] > before[o + 2])) continue;
      steps.push({ from: [before[o] * this.w, before[o + 1] * this.h], to: [after[o] * this.w, after[o + 1] * this.h] });
    }
    return steps;
  }
}
