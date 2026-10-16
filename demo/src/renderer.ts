import type { Uniforms } from './pipeline';
import type { View } from './viewport';

/** The linear-light image: FlashRaw `linear-rgb-f16` (3 half floats per pixel, sensor orientation). */
export interface LinearImage {
  data: Uint16Array;
  width: number;
  height: number;
  orientation: number;
}

/** The embedded preview, decoded by the browser (orientation NOT applied). */
export interface PreviewImage {
  bitmap: ImageBitmap;
  orientation: number;
}

export type Source = 'raw' | 'preview';

export interface Histogram {
  r: Uint32Array;
  g: Uint32Array;
  b: Uint32Array;
  l: Uint32Array;
  total: number;
}

export interface Renderer {
  readonly kind: string;
  readonly canvas: HTMLCanvasElement;
  /** Largest texture side: images beyond it are decoded half-size. */
  readonly maxSize: number;
  setImage(img: LinearImage): void;
  setPreview(p: PreviewImage | null): void;
  draw(view: View, u: Uniforms, source: Source, pixelated: boolean): void;
  histogram(u: Uniforms): Histogram;
  /** Full-resolution adjusted image on a 2D canvas (for PNG/JPEG export). */
  exportCanvas(u: Uniforms, onProgress?: (f: number) => void): Promise<HTMLCanvasElement>;
  dispose(): void;
}

export function emptyHistogram(): Histogram {
  return { r: new Uint32Array(256), g: new Uint32Array(256), b: new Uint32Array(256), l: new Uint32Array(256), total: 0 };
}

export function histogramOf(rgba: Uint8Array | Uint8ClampedArray, h = emptyHistogram()): Histogram {
  for (let i = 0; i < rgba.length; i += 4) {
    const r = rgba[i];
    const g = rgba[i + 1];
    const b = rgba[i + 2];
    h.r[r]++;
    h.g[g]++;
    h.b[b]++;
    h.l[(0.2126 * r + 0.7152 * g + 0.0722 * b + 0.5) | 0]++;
  }
  h.total += rgba.length / 4;
  return h;
}

export const nextFrame = () => new Promise<void>((r) => requestAnimationFrame(() => r()));
