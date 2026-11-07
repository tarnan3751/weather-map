export interface Program {
  program: WebGLProgram;
  uniforms: Record<string, WebGLUniformLocation | null>;
  attribs: Record<string, number>;
}

function compile(gl: WebGL2RenderingContext, type: number, src: string): WebGLShader {
  const sh = gl.createShader(type);
  if (!sh) throw new Error('could not create shader');
  gl.shaderSource(sh, src);
  gl.compileShader(sh);
  if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS) && !gl.isContextLost()) {
    const log = gl.getShaderInfoLog(sh);
    gl.deleteShader(sh);
    throw new Error(`shader compile failed: ${log}`);
  }
  return sh;
}

/** `attribs` are bound to locations 0, 1, ... in order, before linking. */
export function createProgram(gl: WebGL2RenderingContext, vs: string, fs: string, attribs: string[] = []): Program {
  const program = gl.createProgram();
  if (!program) throw new Error('could not create program');
  const v = compile(gl, gl.VERTEX_SHADER, vs);
  const f = compile(gl, gl.FRAGMENT_SHADER, fs);
  gl.attachShader(program, v);
  gl.attachShader(program, f);
  attribs.forEach((name, i) => gl.bindAttribLocation(program, i, name));
  gl.linkProgram(program);
  gl.deleteShader(v);
  gl.deleteShader(f);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS) && !gl.isContextLost()) {
    throw new Error(`program link failed: ${gl.getProgramInfoLog(program)}`);
  }
  const uniforms: Record<string, WebGLUniformLocation | null> = {};
  const nu = gl.getProgramParameter(program, gl.ACTIVE_UNIFORMS) as number;
  for (let i = 0; i < nu; i++) {
    const info = gl.getActiveUniform(program, i);
    if (info) uniforms[info.name] = gl.getUniformLocation(program, info.name);
  }
  const locations: Record<string, number> = {};
  const na = gl.getProgramParameter(program, gl.ACTIVE_ATTRIBUTES) as number;
  for (let i = 0; i < na; i++) {
    const info = gl.getActiveAttrib(program, i);
    if (info) locations[info.name] = gl.getAttribLocation(program, info.name);
  }
  return { program, uniforms, attribs: locations };
}

/** Upload into `tex` (or a new texture): tightly packed rows (257-sample
 *  rows are not 4-byte aligned; MapLibre restores its own pixel store state
 *  after a custom layer runs), nearest sampling, clamped edges. */
function upload(gl: WebGL2RenderingContext, tex: WebGLTexture | null, put: () => void): WebGLTexture | null {
  const t = tex ?? gl.createTexture();
  if (!t) return null;
  gl.bindTexture(gl.TEXTURE_2D, t);
  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
  gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
  put();
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  return t;
}

/** Upload raw samples as an unsigned 16-bit integer texture of 1, 2 or 4
 *  channels (`textureChannels`), in a format the GPU holds as it is. Integer
 *  textures cannot be filtered, which is fine: the shader interpolates
 *  itself, skipping no-data. */
export function uploadU16(gl: WebGL2RenderingContext, data: Uint16Array, width: number, height: number, channels: number): WebGLTexture | null {
  const [internal, format] = channels >= 4 ? [gl.RGBA16UI, gl.RGBA_INTEGER] : channels === 3 ? [gl.RGB16UI, gl.RGB_INTEGER] : channels === 2 ? [gl.RG16UI, gl.RG_INTEGER] : [gl.R16UI, gl.RED_INTEGER];
  return upload(gl, null, () => gl.texImage2D(gl.TEXTURE_2D, 0, internal, width, height, 0, format, gl.UNSIGNED_SHORT, data));
}

/** A palette lookup table. Nearest keeps the GPU colour identical to the CPU
 *  lookup used for the legend and read-out; 1024 entries are smooth enough
 *  without filtering. */
export function uploadRgba8(gl: WebGL2RenderingContext, tex: WebGLTexture | null, data: Uint8Array, width: number, height: number): WebGLTexture | null {
  return upload(gl, tex, () => gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, data));
}
