// The adjustment pipeline, applied to FlashRaw's linear-light output (`linear-rgb-f16`:
// demosaiced, white-balanced, linear sRGB primaries). The GLSL version (WebGL2 renderer) and
// the JS version (CPU fallback) implement the same maths; keep them in sync.

export interface Adjustments {
  exposure: number; // EV
  contrast: number; // -100..100
  highlights: number; // -100..100
  shadows: number; // -100..100
  temperature: number; // -100..100 (relative to as-shot)
  tint: number; // -100..100
  vibrance: number; // -100..100
  saturation: number; // -100..100
  tone: 0 | 1; // 0 = linear + clip, 1 = soft highlight roll-off
}

export const DEFAULTS: Adjustments = {
  exposure: 0,
  contrast: 0,
  highlights: 0,
  shadows: 0,
  temperature: 0,
  tint: 0,
  vibrance: 0,
  saturation: 0,
  tone: 0,
};

export interface SliderDef {
  key: Exclude<keyof Adjustments, 'tone'>;
  label: string;
  min: number;
  max: number;
  step: number;
  group: string;
  format: (v: number) => string;
  track?: string;
}

const signed = (v: number) => (v > 0 ? `+${v}` : `${v}`);
export const SLIDERS: SliderDef[] = [
  { key: 'exposure', label: 'Exposure', min: -5, max: 5, step: 0.05, group: 'Light', format: (v) => `${v > 0 ? '+' : ''}${v.toFixed(2)} EV` },
  { key: 'contrast', label: 'Contrast', min: -100, max: 100, step: 1, group: 'Light', format: signed },
  { key: 'highlights', label: 'Highlights', min: -100, max: 100, step: 1, group: 'Light', format: signed },
  { key: 'shadows', label: 'Shadows', min: -100, max: 100, step: 1, group: 'Light', format: signed },
  { key: 'temperature', label: 'Temperature', min: -100, max: 100, step: 1, group: 'White balance', format: signed, track: 'temp-track' },
  { key: 'tint', label: 'Tint', min: -100, max: 100, step: 1, group: 'White balance', format: signed, track: 'tint-track' },
  { key: 'vibrance', label: 'Vibrance', min: -100, max: 100, step: 1, group: 'Colour', format: signed },
  { key: 'saturation', label: 'Saturation', min: -100, max: 100, step: 1, group: 'Colour', format: signed },
];

export function isDefault(a: Adjustments): boolean {
  return (Object.keys(DEFAULTS) as (keyof Adjustments)[]).every((k) => a[k] === DEFAULTS[k]);
}

/** Uniform values derived from the slider values (shared by both renderers). */
export interface Uniforms {
  gain: [number, number, number]; // white balance x exposure
  contrast: number; // log-slope delta
  highlights: number; // stops at the top of the range
  shadows: number; // stops at the bottom
  vibrance: number;
  saturation: number;
  tone: number;
}

export function uniforms(a: Adjustments): Uniforms {
  const t = a.temperature / 100;
  const g = a.tint / 100;
  let r = 2 ** (0.55 * t);
  let gg = 2 ** (-0.4 * g);
  let b = 2 ** (-0.55 * t);
  // keep the luminance of neutral grey constant
  const n = 0.2126 * r + 0.7152 * gg + 0.0722 * b;
  const e = 2 ** a.exposure;
  r = (r / n) * e;
  gg = (gg / n) * e;
  b = (b / n) * e;
  return {
    gain: [r, gg, b],
    contrast: (a.contrast / 100) * 0.65,
    highlights: (a.highlights / 100) * 1.6,
    shadows: (a.shadows / 100) * 1.6,
    vibrance: a.vibrance / 100,
    saturation: a.saturation / 100,
    tone: a.tone,
  };
}

export const GLSL_PIPELINE = /* glsl */ `
uniform vec3 u_gain;
uniform float u_contrast;
uniform float u_highlights;
uniform float u_shadows;
uniform float u_vibrance;
uniform float u_saturation;
uniform int u_tone;

const vec3 LUMA = vec3(0.2126, 0.7152, 0.0722);

float srgbEncode(float x) {
  return x <= 0.0031308 ? 12.92 * x : 1.055 * pow(x, 1.0 / 2.4) - 0.055;
}

float shoulder(float x) {
  const float k = 0.7;
  return x <= k ? x : k + (1.0 - k) * (1.0 - exp(-(x - k) / (1.0 - k)));
}

vec3 developPixel(vec3 c) {
  c = max(c, vec3(0.0)) * u_gain;
  float L = dot(c, LUMA);
  if (u_highlights != 0.0 || u_shadows != 0.0 || u_contrast != 0.0) {
    float l = max(L, 1e-6);
    float st = log2(l / 0.18);
    float ws = 1.0 - smoothstep(-6.0, 0.5, st);
    float wh = clamp((st + 1.0) / 3.47, 0.0, 1.6);
    wh = wh * wh * (3.0 - 2.0 * min(wh, 1.0));
    float stops = u_shadows * ws + u_highlights * wh + u_contrast * st;
    float k = exp2(stops);
    c *= k;
    L *= k;
  }
  if (u_vibrance != 0.0 || u_saturation != 0.0) {
    float mx = max(c.r, max(c.g, c.b));
    float mn = min(c.r, min(c.g, c.b));
    float sat = mx > 1e-6 ? (mx - mn) / mx : 0.0;
    float s = 1.0 + u_saturation + u_vibrance * (1.0 - sat) * (u_vibrance > 0.0 ? 1.0 : 0.6);
    c = max(vec3(L) + (c - vec3(L)) * s, vec3(0.0));
  }
  if (u_tone == 1) c = vec3(shoulder(c.r), shoulder(c.g), shoulder(c.b));
  c = clamp(c, 0.0, 1.0);
  return vec3(srgbEncode(c.r), srgbEncode(c.g), srgbEncode(c.b));
}
`;

// ------------------------------------------------------------------- JS mirror

const SRGB_LUT_SIZE = 4096;
const srgbLut = new Uint8ClampedArray(SRGB_LUT_SIZE + 1);
for (let i = 0; i <= SRGB_LUT_SIZE; i++) {
  const x = i / SRGB_LUT_SIZE;
  srgbLut[i] = Math.round(255 * (x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055));
}
const shoulder = (x: number) => (x <= 0.7 ? x : 0.7 + 0.3 * (1 - Math.exp(-(x - 0.7) / 0.3)));
const smoothstep = (a: number, b: number, x: number) => {
  const t = Math.min(1, Math.max(0, (x - a) / (b - a)));
  return t * t * (3 - 2 * t);
};

/**
 * Develops `count` linear RGB pixels (`src`, 3 floats each, starting at `srcOffset`) into
 * RGBA8 `dst` (starting at pixel `dstOffset`). `srcStride`/`dstStep` allow strided access.
 */
export function developPixels(
  u: Uniforms,
  src: Float32Array,
  dst: Uint8ClampedArray,
  count: number,
  srcIndex: (i: number) => number,
  dstIndex: (i: number) => number,
) {
  const [gr, gg, gb] = u.gain;
  const local = u.highlights !== 0 || u.shadows !== 0 || u.contrast !== 0;
  const colour = u.vibrance !== 0 || u.saturation !== 0;
  for (let i = 0; i < count; i++) {
    const s = srcIndex(i);
    let r = Math.max(src[s], 0) * gr;
    let g = Math.max(src[s + 1], 0) * gg;
    let b = Math.max(src[s + 2], 0) * gb;
    let L = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if (local) {
      const st = Math.log2(Math.max(L, 1e-6) / 0.18);
      const ws = 1 - smoothstep(-6, 0.5, st);
      let wh = Math.min(1.6, Math.max(0, (st + 1) / 3.47));
      wh = wh * wh * (3 - 2 * Math.min(wh, 1));
      const k = 2 ** (u.shadows * ws + u.highlights * wh + u.contrast * st);
      r *= k;
      g *= k;
      b *= k;
      L *= k;
    }
    if (colour) {
      const mx = Math.max(r, g, b);
      const mn = Math.min(r, g, b);
      const sat = mx > 1e-6 ? (mx - mn) / mx : 0;
      const sc = 1 + u.saturation + u.vibrance * (1 - sat) * (u.vibrance > 0 ? 1 : 0.6);
      r = Math.max(L + (r - L) * sc, 0);
      g = Math.max(L + (g - L) * sc, 0);
      b = Math.max(L + (b - L) * sc, 0);
    }
    if (u.tone === 1) {
      r = shoulder(r);
      g = shoulder(g);
      b = shoulder(b);
    }
    const d = dstIndex(i) * 4;
    dst[d] = srgbLut[(Math.min(r, 1) * SRGB_LUT_SIZE + 0.5) | 0];
    dst[d + 1] = srgbLut[(Math.min(g, 1) * SRGB_LUT_SIZE + 0.5) | 0];
    dst[d + 2] = srgbLut[(Math.min(b, 1) * SRGB_LUT_SIZE + 0.5) | 0];
    dst[d + 3] = 255;
  }
}

/** IEEE half float bits → float32 (lookup table, built on first use). */
let halfLut: Float32Array | null = null;
export function halfTable(): Float32Array {
  if (halfLut) return halfLut;
  halfLut = new Float32Array(65536);
  for (let h = 0; h < 65536; h++) {
    const s = h & 0x8000 ? -1 : 1;
    const e = (h >> 10) & 0x1f;
    const f = h & 0x3ff;
    halfLut[h] = e === 0 ? s * 2 ** -14 * (f / 1024) : e === 31 ? (f ? NaN : s * Infinity) : s * 2 ** (e - 15) * (1 + f / 1024);
  }
  return halfLut;
}

/** Orientation (EXIF 1..8): displayed size and the sensor pixel shown at a displayed position. */
export function orientedSize(w: number, h: number, o: number): [number, number] {
  return o >= 5 ? [h, w] : [w, h];
}

/** Sensor (x, y) for displayed (x, y), integer pixel coordinates. */
export function sensorPixel(dx: number, dy: number, w: number, h: number, o: number): [number, number] {
  switch (o) {
    case 2: return [w - 1 - dx, dy];
    case 3: return [w - 1 - dx, h - 1 - dy];
    case 4: return [dx, h - 1 - dy];
    case 5: return [dy, dx];
    case 6: return [dy, h - 1 - dx];
    case 7: return [w - 1 - dy, h - 1 - dx];
    case 8: return [w - 1 - dy, dx];
    default: return [dx, dy];
  }
}

export const GLSL_ORIENT = /* glsl */ `
vec2 orientUv(vec2 d, int o) {
  if (o == 2) return vec2(1.0 - d.x, d.y);
  if (o == 3) return 1.0 - d;
  if (o == 4) return vec2(d.x, 1.0 - d.y);
  if (o == 5) return d.yx;
  if (o == 6) return vec2(d.y, 1.0 - d.x);
  if (o == 7) return vec2(1.0 - d.y, 1.0 - d.x);
  if (o == 8) return vec2(1.0 - d.y, d.x);
  return d;
}
`;
