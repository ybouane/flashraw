// CPU fallback (no WebGL2): the same pipeline in JS on a downscaled proxy for the screen and
// the histogram; the full-resolution export processes every pixel in bands.

import { developPixels, halfTable, orientedSize, sensorPixel, type Uniforms } from './pipeline';
import { histogramOf, nextFrame, type Histogram, type LinearImage, type PreviewImage, type Renderer, type Source } from './renderer';
import type { View } from './viewport';

const PROXY_MAX = 1600;

export class CpuRenderer implements Renderer {
  readonly kind = 'CPU (JavaScript)';
  readonly canvas = document.createElement('canvas');
  readonly maxSize = 32767;
  private ctx = this.canvas.getContext('2d')!;
  private img: LinearImage | null = null;
  private proxy: { data: Float32Array; w: number; h: number; factor: number } | null = null;
  private proxyCanvas = document.createElement('canvas');
  private proxyKey = '';
  private preview: PreviewImage | null = null;

  setImage(img: LinearImage) {
    this.img = img;
    this.proxyKey = '';
    // oriented, box-downsampled float proxy
    const lut = halfTable();
    const [W, H] = orientedSize(img.width, img.height, img.orientation);
    const f = Math.max(1, Math.ceil(Math.max(W, H) / PROXY_MAX));
    const pw = Math.max(1, Math.floor(W / f));
    const ph = Math.max(1, Math.floor(H / f));
    const out = new Float32Array(pw * ph * 3);
    const step = Math.max(1, Math.floor(f / 2)); // sub-sample inside each box
    for (let y = 0; y < ph; y++) {
      for (let x = 0; x < pw; x++) {
        let r = 0, g = 0, b = 0, n = 0;
        for (let sy = 0; sy < f; sy += step) {
          for (let sx = 0; sx < f; sx += step) {
            const [ix, iy] = sensorPixel(x * f + sx, y * f + sy, img.width, img.height, img.orientation);
            const i = (iy * img.width + ix) * 3;
            r += lut[img.data[i]];
            g += lut[img.data[i + 1]];
            b += lut[img.data[i + 2]];
            n++;
          }
        }
        const o = (y * pw + x) * 3;
        out[o] = r / n;
        out[o + 1] = g / n;
        out[o + 2] = b / n;
      }
    }
    this.proxy = { data: out, w: pw, h: ph, factor: f };
    this.proxyCanvas.width = pw;
    this.proxyCanvas.height = ph;
  }

  setPreview(p: PreviewImage | null) {
    this.preview = p;
  }

  private developProxy(u: Uniforms): ImageData {
    const p = this.proxy!;
    const id = new ImageData(p.w, p.h);
    developPixels(u, p.data, id.data, p.w * p.h, (i) => i * 3, (i) => i);
    return id;
  }

  draw(v: View, u: Uniforms, source: Source, pixelated: boolean) {
    const c = this.canvas;
    if (c.width !== v.cw || c.height !== v.ch) {
      c.width = v.cw;
      c.height = v.ch;
    }
    const ctx = this.ctx;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = getComputedStyle(document.documentElement).getPropertyValue('--stage-bg') || '#ebeaf3';
    ctx.fillRect(0, 0, c.width, c.height);
    ctx.imageSmoothingEnabled = !pixelated;
    ctx.imageSmoothingQuality = 'high';
    if (source === 'preview' && this.preview) {
      const { bitmap, orientation } = this.preview;
      ctx.translate(v.x, v.y);
      ctx.scale(v.scale, v.scale);
      orientTransform(ctx, bitmap.width, bitmap.height, orientation);
      ctx.drawImage(bitmap, 0, 0);
      return;
    }
    if (!this.proxy || !this.img) return;
    const key = JSON.stringify(u);
    if (key !== this.proxyKey) {
      this.proxyCanvas.getContext('2d')!.putImageData(this.developProxy(u), 0, 0);
      this.proxyKey = key;
    }
    const [W, H] = orientedSize(this.img.width, this.img.height, this.img.orientation);
    ctx.drawImage(this.proxyCanvas, v.x, v.y, W * v.scale, H * v.scale);
  }

  histogram(u: Uniforms): Histogram {
    return histogramOf(this.developProxy(u).data);
  }

  async exportCanvas(u: Uniforms, onProgress?: (f: number) => void): Promise<HTMLCanvasElement> {
    const img = this.img!;
    const lut = halfTable();
    const [W, H] = orientedSize(img.width, img.height, img.orientation);
    const out = document.createElement('canvas');
    out.width = W;
    out.height = H;
    const ctx = out.getContext('2d');
    if (!ctx || out.width !== W) throw new Error(`the browser cannot create a ${W}×${H} canvas`);
    const band = Math.max(1, Math.floor(4_000_000 / W));
    const row = new Float32Array(W * band * 3);
    for (let y0 = 0; y0 < H; y0 += band) {
      const bh = Math.min(band, H - y0);
      for (let y = 0; y < bh; y++) {
        for (let x = 0; x < W; x++) {
          const [ix, iy] = sensorPixel(x, y0 + y, img.width, img.height, img.orientation);
          const i = (iy * img.width + ix) * 3;
          const o = (y * W + x) * 3;
          row[o] = lut[img.data[i]];
          row[o + 1] = lut[img.data[i + 1]];
          row[o + 2] = lut[img.data[i + 2]];
        }
      }
      const id = new ImageData(W, bh);
      developPixels(u, row, id.data, W * bh, (i) => i * 3, (i) => i);
      ctx.putImageData(id, 0, y0);
      onProgress?.((y0 + bh) / H);
      await nextFrame();
    }
    return out;
  }

  dispose() {
    this.img = null;
    this.proxy = null;
  }
}

/** Canvas transform that draws a (w x h) sensor-oriented image in display orientation `o`. */
function orientTransform(ctx: CanvasRenderingContext2D, w: number, h: number, o: number) {
  switch (o) {
    case 2: ctx.transform(-1, 0, 0, 1, w, 0); break;
    case 3: ctx.transform(-1, 0, 0, -1, w, h); break;
    case 4: ctx.transform(1, 0, 0, -1, 0, h); break;
    case 5: ctx.transform(0, 1, 1, 0, 0, 0); break;
    case 6: ctx.transform(0, 1, -1, 0, h, 0); break;
    case 7: ctx.transform(0, -1, -1, 0, h, w); break;
    case 8: ctx.transform(0, -1, 1, 0, 0, w); break;
  }
}
