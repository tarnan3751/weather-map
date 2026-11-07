// GLSL ES 3.00 for the weather layer. Tiles are integer textures holding the
// raw u16 samples; all interpolation, no-data handling, dequantization and
// colouring happens here, so palettes and units change without refetching.
//
// Every draw covers one tile slot. Each of the two frames being cross-faded
// (A and B) may come from a different tile (the exact one, or an ancestor
// magnified while the sharper tile loads), so each has its own texture and a
// transform from the slot's 0..1 coordinates into that texture.

export const VERTEX = /* glsl */ `#version 300 es
precision highp float;
uniform mat4 u_matrix;
// Slot in mercator units: x0, y0, size.
uniform vec3 u_rect;
in vec2 a_pos;
out vec2 v_uv;
void main() {
  v_uv = a_pos;
  gl_Position = u_matrix * vec4(u_rect.xy + a_pos * u_rect.z, 0.0, 1.0);
}`;

export const COMMON = /* glsl */ `#version 300 es
precision highp float;
precision highp int;
precision highp usampler2D;

uniform usampler2D u_texA;
uniform usampler2D u_texB;
// Texture transform (offset.xy, scale) and whether the frame is drawn at all.
uniform vec4 u_uvA;
uniform vec4 u_uvB;
uniform float u_mix;
uniform float u_opacity;
uniform sampler2D u_lut;
// Lookup domain in transfer space (lo, hi) and the transfer code.
uniform vec3 u_lutRange;

in vec2 v_uv;
out vec4 fragColor;

float transferF(float v) {
  if (u_lutRange.z < 0.5) return v;
  if (u_lutRange.z < 1.5) return sqrt(max(v, 0.0));
  return log(max(v, 1e-6)) * 0.4342944819;
}

float lutX(float v) {
  return clamp((transferF(v) - u_lutRange.x) / (u_lutRange.y - u_lutRange.x), 0.0, 1.0);
}

// The four samples around a position, fetched once for every channel: q[c]
// holds channel c of the samples at i0, i0 + (1, 0), i0 + (0, 1) and
// i0 + (1, 1), and w their bilinear weights. Samples sit on lattice points:
// texel (i, j) is at uv (i, j) / (size - 1).
struct Around {
  mat4 q;
  vec4 w;
};

Around around(usampler2D tex, vec2 uv) {
  ivec2 size = textureSize(tex, 0);
  vec2 p = clamp(uv, 0.0, 1.0) * vec2(size - 1);
  ivec2 i0 = min(ivec2(floor(p)), size - 2);
  vec2 f = p - vec2(i0);
  mat4 t = mat4(
    vec4(texelFetch(tex, i0, 0)),
    vec4(texelFetch(tex, i0 + ivec2(1, 0), 0)),
    vec4(texelFetch(tex, i0 + ivec2(0, 1), 0)),
    vec4(texelFetch(tex, i0 + ivec2(1, 1), 0)));
  return Around(transpose(t), vec4((1.0 - f.x) * (1.0 - f.y), f.x * (1.0 - f.y), (1.0 - f.x) * f.y, f.x * f.y));
}

// Bilinear over the four samples of channel c, skipping no-data (0) and
// renormalising; less than half the weight valid means no data. Returns the
// interpolated raw sample (x) and validity (y). Mirrors src/tiles/sampling.ts.
vec2 bilinear(Around a, int c) {
  vec4 q = a.q[c];
  vec4 ok = step(0.5, q);
  float sw = dot(a.w, ok);
  if (sw < 0.5) return vec2(0.0);
  return vec2(dot(a.w * ok, q) / sw, 1.0);
}

// value = offset + (q - 1) * scale
float dequant(vec2 scaleOffset, float q) {
  return scaleOffset.y + (q - 1.0) * scaleOffset.x;
}

vec4 premul(vec4 c) {
  return vec4(c.rgb * c.a, c.a);
}
`;

export const SCALAR_FRAGMENT = /* glsl */ `${COMMON}
uniform vec2 u_q0;

vec4 shade(usampler2D tex, vec4 uvT) {
  vec2 s = bilinear(around(tex, uvT.xy + v_uv * uvT.z), 0);
  if (s.y < 0.5) return vec4(0.0);
  float v = dequant(u_q0, s.x);
  return premul(texture(u_lut, vec2(lutX(v), 0.5)));
}

void main() {
  vec4 a = u_uvA.w > 0.5 ? shade(u_texA, u_uvA) : vec4(0.0);
  vec4 b = u_uvB.w > 0.5 ? shade(u_texB, u_uvB) : vec4(0.0);
  fragColor = mix(a, b, u_mix) * u_opacity;
}`;

export const PRECIP_FRAGMENT = /* glsl */ `${COMMON}
uniform vec2 u_q0; // rate
uniform vec2 u_q1; // type code
const float ROWS = 6.0;

// Type of the nearest of the four surrounding samples that has one. Plain
// nearest-sample would make the outline of every rain area follow the sample
// grid in steps; this way the outline follows the smoothly interpolated rate
// and only boundaries between two types are drawn at sample resolution.
// Mirrors nearestPositiveSample in src/tiles/sampling.ts.
float typeAt(Around a) {
  vec4 w = a.w;
  vec4 q = a.q[1];
  float best = 0.0;
  float bestW = -1.0;
  for (int k = 0; k < 4; k++) {
    if (q[k] < 0.5) continue;
    float c = floor(dequant(u_q1, q[k]) + 0.5);
    if (c > 0.5 && w[k] > bestW) {
      bestW = w[k];
      best = c;
    }
  }
  return best;
}

vec4 shade(usampler2D tex, vec4 uvT) {
  Around k = around(tex, uvT.xy + v_uv * uvT.z);
  vec2 r = bilinear(k, 0);
  if (r.y < 0.5) return vec4(0.0);
  float rate = dequant(u_q0, r.x);
  if (rate < 0.01) return vec4(0.0);
  float code = typeAt(k);
  if (code < 0.5 || code > 5.5) return vec4(0.0);
  return premul(texture(u_lut, vec2(lutX(rate), (code + 0.5) / ROWS)));
}

void main() {
  vec4 a = u_uvA.w > 0.5 ? shade(u_texA, u_uvA) : vec4(0.0);
  vec4 b = u_uvB.w > 0.5 ? shade(u_texB, u_uvB) : vec4(0.0);
  fragColor = mix(a, b, u_mix) * u_opacity;
}`;

export const WIND_FRAGMENT = /* glsl */ `${COMMON}
uniform vec2 u_q0; // u
uniform vec2 u_q1; // v
uniform vec2 u_q2; // gust
uniform float u_hasGust;
uniform float u_byGust;
// Arrow grid cells across the slot, and whether to draw arrows.
uniform float u_cells;
uniform float u_arrows;
// Width of one pixel in arrow-cell units, taken in main(): derivatives are
// undefined inside the non-uniform branches below.
float g_px;

// Earth-relative wind at a texture position: (u, v, gust, valid).
vec4 windAt(usampler2D tex, vec2 uv) {
  Around k = around(tex, uv);
  vec2 u = bilinear(k, 0);
  vec2 v = bilinear(k, 1);
  if (u.y < 0.5 || v.y < 0.5) return vec4(0.0);
  float g = -1.0;
  if (u_hasGust > 0.5) {
    vec2 gs = bilinear(k, 2);
    if (gs.y > 0.5) g = dequant(u_q2, gs.x);
  }
  return vec4(dequant(u_q0, u.x), dequant(u_q1, v.x), g, 1.0);
}

float segDist(vec2 p, vec2 a, vec2 b) {
  vec2 pa = p - a;
  vec2 ba = b - a;
  float h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
  return length(pa - ba * h);
}

// Coverage of a direction arrow in this fragment's grid cell, pointing where
// the wind blows toward, longer for stronger wind.
float arrow(usampler2D tex, vec4 uvT) {
  vec2 cellPos = v_uv * u_cells;
  vec2 cell = floor(cellPos);
  vec2 local = cellPos - cell - 0.5;
  vec2 centre = (cell + 0.5) / u_cells;
  vec4 w = windAt(tex, uvT.xy + centre * uvT.z);
  if (w.w < 0.5) return 0.0;
  float speed = length(w.xy);
  if (speed < 0.8) return 0.0;
  // Texture rows run north to south, so north is -y here.
  vec2 dir = normalize(vec2(w.x, -w.y));
  float len = mix(0.12, 0.32, clamp(speed / 18.0, 0.0, 1.0));
  vec2 tail = -dir * len;
  vec2 tip = dir * len;
  vec2 side = vec2(-dir.y, dir.x);
  float d = segDist(local, tail, tip);
  d = min(d, segDist(local, tip, tip - dir * 0.12 + side * 0.075));
  d = min(d, segDist(local, tip, tip - dir * 0.12 - side * 0.075));
  // About 1.5 px wide whatever the zoom.
  float half_w = g_px * 0.8;
  return 1.0 - smoothstep(half_w, half_w + g_px, d);
}

vec4 shade(usampler2D tex, vec4 uvT) {
  vec4 w = windAt(tex, uvT.xy + v_uv * uvT.z);
  if (w.w < 0.5) return vec4(0.0);
  float value = (u_byGust > 0.5 && w.z >= 0.0) ? w.z : length(w.xy);
  vec4 c = premul(texture(u_lut, vec2(lutX(value), 0.5)));
  if (u_arrows > 0.5) {
    float a = arrow(tex, uvT) * 0.62;
    c = vec4(1.0) * a + c * (1.0 - a);
  }
  return c;
}

void main() {
  g_px = fwidth(v_uv.x * u_cells);
  vec4 a = u_uvA.w > 0.5 ? shade(u_texA, u_uvA) : vec4(0.0);
  vec4 b = u_uvB.w > 0.5 ? shade(u_texB, u_uvB) : vec4(0.0);
  fragColor = mix(a, b, u_mix) * u_opacity;
}`;
