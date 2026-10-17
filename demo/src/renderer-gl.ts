// WebGL2 renderer: the linear-light decode lives in an RGB16F texture (plus a half-float
// mip pyramid for zoomed-out views); adjustments, tone curve and sRGB encoding run in a
// fragment shader, so slider changes cost one draw call.

import { GLSL_ORIENT, GLSL_PIPELINE, orientedSize, type Uniforms } from './pipeline';
import { histogramOf, nextFrame, type Histogram, type LinearImage, type PreviewImage, type Renderer, type Source } from './renderer';
import type { View } from './viewport';

const VS = /* glsl */ `#version 300 es
uniform vec4 u_rect; // clip-space x0, y0 (top), x1, y1 (bottom)
uniform int u_orient;
out vec2 v_uv;
${GLSL_ORIENT}
void main() {
  vec2 c = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
  v_uv = orientUv(c, u_orient);
  gl_Position = vec4(mix(u_rect.x, u_rect.z, c.x), mix(u_rect.y, u_rect.w, c.y), 0.0, 1.0);
}`;

const FS_DEVELOP = /* glsl */ `#version 300 es
precision highp float;
uniform sampler2D u_tex;
in vec2 v_uv;
out vec4 o;
${GLSL_PIPELINE}
void main() { o = vec4(developPixel(texture(u_tex, v_uv).rgb), 1.0); }`;

const FS_COPY = /* glsl */ `#version 300 es
precision highp float;
uniform sampler2D u_tex;
in vec2 v_uv;
out vec4 o;
void main() { o = vec4(texture(u_tex, v_uv).rgb, 1.0); }`;

interface Level {
  tex: WebGLTexture;
  w: number;
  h: number;
}

interface Program {
  prog: WebGLProgram;
  loc: Record<string, WebGLUniformLocation | null>;
}

export class GlRenderer implements Renderer {
  readonly kind = 'WebGL2';
  readonly canvas: HTMLCanvasElement;
  readonly maxSize: number;
  private gl: WebGL2RenderingContext;
  private develop: Program;
  private copy: Program;
  private levels: Level[] = [];
  private orientation = 1;
  private preview: { tex: WebGLTexture; w: number; h: number; orientation: number } | null = null;
  private floatTargets: boolean;
  private fbo: WebGLFramebuffer;
  private vao: WebGLVertexArrayObject;
  private bg: [number, number, number];
  lost = false;
  onLost: () => void = () => {};

  static create(): GlRenderer | null {
    const canvas = document.createElement('canvas');
    const gl = canvas.getContext('webgl2', { antialias: false, alpha: false, depth: false, stencil: false, premultipliedAlpha: false, powerPreference: 'high-performance' });
    if (!gl) return null;
    try {
      return new GlRenderer(canvas, gl);
    } catch (e) {
      console.warn('WebGL2 renderer unavailable:', e);
      return null;
    }
  }

  private constructor(canvas: HTMLCanvasElement, gl: WebGL2RenderingContext) {
    this.canvas = canvas;
    this.gl = gl;
    this.floatTargets = !!(gl.getExtension('EXT_color_buffer_float') || gl.getExtension('EXT_color_buffer_half_float'));
    this.maxSize = Math.min(gl.getParameter(gl.MAX_TEXTURE_SIZE) as number, 16384);
    this.develop = this.program(FS_DEVELOP, ['u_tex', 'u_rect', 'u_orient', 'u_gain', 'u_contrast', 'u_highlights', 'u_shadows', 'u_vibrance', 'u_saturation', 'u_tone']);
    this.copy = this.program(FS_COPY, ['u_tex', 'u_rect', 'u_orient']);
    this.fbo = gl.createFramebuffer()!;
    this.vao = gl.createVertexArray()!;
    const bg = getComputedStyle(document.documentElement).getPropertyValue('--stage-bg').trim() || '#ebeaf3';
    const n = parseInt(bg.slice(1), 16);
    this.bg = [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
    canvas.addEventListener('webglcontextlost', (e) => {
      e.preventDefault();
      this.lost = true;
      this.onLost();
    });
  }

  private program(fs: string, names: string[]): Program {
    const gl = this.gl;
    const sh = (type: number, src: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? 'shader error');
      return s;
    };
    const prog = gl.createProgram()!;
    gl.attachShader(prog, sh(gl.VERTEX_SHADER, VS));
    gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, fs));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog) ?? 'link error');
    const loc: Program['loc'] = {};
    for (const n of names) loc[n] = gl.getUniformLocation(prog, n);
    return { prog, loc };
  }

  private texture(filter: number): WebGLTexture {
    const gl = this.gl;
    const t = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, t);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return t;
  }

  private freeLevels() {
    for (const l of this.levels) this.gl.deleteTexture(l.tex);
    this.levels = [];
  }

  setImage(img: LinearImage) {
    const gl = this.gl;
    this.freeLevels();
    this.orientation = img.orientation;
    const base = this.texture(gl.LINEAR);
    gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGB16F, img.width, img.height);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 2);
    // upload in bands: no single huge staging copy inside the browser
    const band = Math.max(1, Math.floor((32 << 20) / (img.width * 6)));
    // WebGL rejects views of a SharedArrayBuffer (threaded WASM memory): stage those bands
    const shared = typeof SharedArrayBuffer !== 'undefined' && img.data.buffer instanceof SharedArrayBuffer;
    const staging = shared ? new Uint16Array(band * img.width * 3) : null;
    for (let y = 0; y < img.height; y += band) {
      const h = Math.min(band, img.height - y);
      let rows = img.data.subarray(y * img.width * 3, (y + h) * img.width * 3);
      if (staging) {
        staging.set(rows);
        rows = staging.subarray(0, rows.length);
      }
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, y, img.width, h, gl.RGB, gl.HALF_FLOAT, rows);
    }
    this.levels.push({ tex: base, w: img.width, h: img.height });
    if (!this.floatTargets) return;
    // half-float pyramid: each level a 2x2 box average of the previous one (one bilinear tap)
    let { w, h } = this.levels[0];
    while (Math.max(w, h) > 512) {
      const nw = Math.max(1, w >> 1);
      const nh = Math.max(1, h >> 1);
      const tex = this.texture(gl.LINEAR);
      gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA16F, nw, nh);
      this.renderTo(tex, nw, nh, () => {
        this.use(this.copy, this.levels[this.levels.length - 1].tex, 1, [-1, -1, 1, 1]);
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      });
      this.levels.push({ tex, w: nw, h: nh });
      w = nw;
      h = nh;
    }
  }

  setPreview(p: PreviewImage | null) {
    const gl = this.gl;
    if (this.preview) gl.deleteTexture(this.preview.tex);
    this.preview = null;
    if (!p) return;
    const tex = this.texture(gl.LINEAR);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 4);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, p.bitmap);
    this.preview = { tex, w: p.bitmap.width, h: p.bitmap.height, orientation: p.orientation };
  }

  private renderTo(tex: WebGLTexture, w: number, h: number, fn: () => void) {
    const gl = this.gl;
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.fbo);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
    gl.viewport(0, 0, w, h);
    fn();
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
  }

  private use(p: Program, tex: WebGLTexture, orient: number, rect: [number, number, number, number], u?: Uniforms) {
    const gl = this.gl;
    gl.useProgram(p.prog);
    gl.bindVertexArray(this.vao);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.uniform1i(p.loc.u_tex, 0);
    gl.uniform4f(p.loc.u_rect, ...rect);
    gl.uniform1i(p.loc.u_orient, orient);
    if (u) {
      gl.uniform3f(p.loc.u_gain, ...u.gain);
      gl.uniform1f(p.loc.u_contrast, u.contrast);
      gl.uniform1f(p.loc.u_highlights, u.highlights);
      gl.uniform1f(p.loc.u_shadows, u.shadows);
      gl.uniform1f(p.loc.u_vibrance, u.vibrance);
      gl.uniform1f(p.loc.u_saturation, u.saturation);
      gl.uniform1i(p.loc.u_tone, u.tone);
    }
  }

  /** Pyramid level for a draw at `scale` display pixels per base pixel. */
  private levelFor(scale: number): Level {
    const k = Math.max(0, Math.min(this.levels.length - 1, Math.floor(Math.log2(1 / scale))));
    return this.levels[k];
  }

  draw(v: View, u: Uniforms, source: Source, pixelated: boolean) {
    const gl = this.gl;
    if (this.lost) return;
    if (this.canvas.width !== v.cw || this.canvas.height !== v.ch) {
      this.canvas.width = v.cw;
      this.canvas.height = v.ch;
    }
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, v.cw, v.ch);
    gl.clearColor(...this.bg, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    const src = source === 'preview' && this.preview ? this.preview : null;
    const base = src ?? this.levels[0];
    if (!base) return;
    const orient = src ? src.orientation : this.orientation;
    const [W, H] = orientedSize(base.w, base.h, orient);
    const rect: [number, number, number, number] = [
      (v.x / v.cw) * 2 - 1,
      1 - (v.y / v.ch) * 2,
      ((v.x + W * v.scale) / v.cw) * 2 - 1,
      1 - ((v.y + H * v.scale) / v.ch) * 2,
    ];
    if (src) {
      this.use(this.copy, src.tex, orient, rect);
    } else {
      const lvl = this.levelFor(v.scale);
      this.use(this.develop, lvl.tex, orient, rect, u);
      const f = pixelated && lvl === this.levels[0] ? gl.NEAREST : gl.LINEAR;
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, f);
    }
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  private readTarget(w: number, h: number): WebGLTexture {
    const gl = this.gl;
    const tex = this.texture(gl.NEAREST);
    gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA8, w, h);
    return tex;
  }

  histogram(u: Uniforms): Histogram {
    const gl = this.gl;
    const lvl = this.levels.find((l) => Math.max(l.w, l.h) <= 1024) ?? this.levels[this.levels.length - 1];
    const [w, h] = orientedSize(lvl.w, lvl.h, this.orientation);
    const tex = this.readTarget(w, h);
    const px = new Uint8Array(w * h * 4);
    this.renderTo(tex, w, h, () => {
      this.use(this.develop, lvl.tex, this.orientation, [-1, -1, 1, 1], u);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, px);
    });
    gl.deleteTexture(tex);
    return histogramOf(px);
  }

  async exportCanvas(u: Uniforms, onProgress?: (f: number) => void): Promise<HTMLCanvasElement> {
    const gl = this.gl;
    const base = this.levels[0];
    const [W, H] = orientedSize(base.w, base.h, this.orientation);
    const out = document.createElement('canvas');
    out.width = W;
    out.height = H;
    const ctx = out.getContext('2d');
    if (!ctx || out.width !== W) throw new Error(`the browser cannot create a ${W}×${H} canvas`);
    const T = Math.min(2048, gl.getParameter(gl.MAX_VIEWPORT_DIMS)[0] as number);
    const tex = this.readTarget(T, T);
    const px = new Uint8ClampedArray(T * T * 4);
    const tiles = Math.ceil(W / T) * Math.ceil(H / T);
    let done = 0;
    try {
      for (let ty = 0; ty < H; ty += T) {
        for (let tx = 0; tx < W; tx += T) {
          const tw = Math.min(T, W - tx);
          const th = Math.min(T, H - ty);
          this.renderTo(tex, T, T, () => {
            gl.viewport(0, 0, tw, th);
            const rect: [number, number, number, number] = [-1 - (2 * tx) / tw, -1 - (2 * ty) / th, -1 + (2 * (W - tx)) / tw, -1 + (2 * (H - ty)) / th];
            this.use(this.develop, base.tex, this.orientation, rect, u);
            gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
            gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
            gl.readPixels(0, 0, tw, th, gl.RGBA, gl.UNSIGNED_BYTE, px.subarray(0, tw * th * 4));
          });
          ctx.putImageData(new ImageData(px.slice(0, tw * th * 4), tw, th), tx, ty);
          onProgress?.(++done / tiles);
          await nextFrame();
        }
      }
    } finally {
      gl.deleteTexture(tex);
    }
    return out;
  }

  dispose() {
    this.freeLevels();
    this.setPreview(null);
    this.gl.getExtension('WEBGL_lose_context')?.loseContext();
  }
}
