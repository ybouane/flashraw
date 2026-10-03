// Compile-only test of the public type declarations (`npm run test:types`).
import {
  FlashRaw,
  decodeRaw,
  readMetadata,
  extractPreview,
  extractThumbnail,
  supportsFile,
  detectFormat,
  getCapabilities,
  init,
  FlashRawError,
  GpuUnavailableError,
  GpuFrame,
  getGpuDevice,
  initThreads,
  GPU_OUTPUTS,
  type FlashRawGPUBuffer,
  type GpuOutputFormat,
  type WasmBinary,
  type RawMetadata,
  type DecodedFrame,
  type DecodeOptions,
  type Capabilities,
  type ErrorKind,
} from 'flashraw';
import { createWorkerPool, type WorkerFrame } from 'flashraw/worker';

export async function usage(buffer: ArrayBuffer, file: Blob): Promise<void> {
  const info = await init({ simd: true });
  const variant: 'simd' | 'fallback' = info.variant;

  const raw = await FlashRaw.open(buffer);
  const camera: string = raw.metadata.camera.name;
  const w: number = raw.metadata.width;
  const h: number = raw.metadata.height;
  const cfa: string | undefined = raw.metadata.cfa?.pattern;
  const wb: [number, number, number, number] | null = raw.metadata.whiteBalance;
  const iso: number | null = raw.metadata.exposure.iso;

  const preview = await raw.preview();
  if (preview) {
    const bytes = preview.data;
    const mime: string = preview.mimeType;
    void new Blob([bytes], { type: mime });
  }
  const thumb = await raw.thumbnail({ format: 'rgba8' });
  if (thumb) {
    const px: Uint8ClampedArray = thumb.data;
    void px;
  }

  const frame = await raw.decode({ backend: 'auto', output: 'raw-u16' });
  const u16: Uint16Array = frame.data;
  const backend: string = frame.backend.decode;
  const total: number = frame.timings.totalMs;
  const pattern: string | undefined = frame.cfa?.pattern;

  const img = await raw.decode({ output: 'rgba8' });
  const clamped: Uint8ClampedArray = img.data;
  const imageData: ImageData = img.toImageData();

  const lin = await raw.decode({ output: 'linear-rgb', demosaic: 'half-size' });
  const f32: Float32Array = lin.data;

  const dflt = await raw.decode();
  const dfltData: Uint16Array = dflt.data;

  const region = await raw.decodeRegion({ x: 0, y: 0, width: 64, height: 64 }, { output: 'normalized' });
  const norm: Float32Array = region.data;
  const tile = await raw.decodeTile(0);
  const zc = await raw.decode({ zeroCopy: true });
  zc.free();
  {
    using frame2 = await raw.decode({ zeroCopy: true, output: 'raw-f32' });
    const d: Float32Array = frame2.data;
    void d;
  }

  const opts: DecodeOptions = { backend: 'gpu', fallback: 'cpu' };
  try {
    await raw.decode({ ...opts, fallback: 'error' });
  } catch (e) {
    if (e instanceof GpuUnavailableError) {
      const k: 'GpuUnavailable' = e.kind;
      void k;
    } else if (e instanceof FlashRawError) {
      const k: ErrorKind = e.kind;
      void k;
    }
  }
  // WebGPU build: GPU-resident outputs (or the host-output fallback).
  const g = await raw.decode({ output: 'gpu-rgba8' });
  if (g instanceof GpuFrame) {
    const out: 'gpu-rgba8' = g.output;
    const fmtName: 'u16' | 'f32' | 'rgba8' = g.format;
    const bytes: number = g.byteLength;
    const buf: FlashRawGPUBuffer = g.buffer;
    const tex = g.createTexture();
    const host: DecodedFrame<'rgba8'> = await g.download();
    g.free();
    void [out, fmtName, bytes, buf, tex, host];
  } else {
    const fallbackRgba: Uint8ClampedArray = g.data;
    void fallbackRgba;
  }
  const device = await getGpuDevice();
  const outs: readonly GpuOutputFormat[] = GPU_OUTPUTS;
  void [device, outs];
  raw.close();

  // Threads.
  const n: number = await initThreads(4);
  const binary: WasmBinary = (await init({ threads: false, gpu: false })).binary;
  const tc: number = (await getCapabilities()).threads.count;
  void [n, binary, tc];

  const m: RawMetadata = await readMetadata(file);
  const f: DecodedFrame<'rgba8'> = await decodeRaw(new Uint8Array(buffer), { output: 'rgba8' });
  const p = await extractPreview(buffer);
  const t = await extractThumbnail(buffer, { format: 'rgba8' });
  const ok: boolean = await supportsFile(buffer);
  const fmt = await detectFormat(buffer);
  const caps: Capabilities = await getCapabilities();
  const webgpu: boolean = caps.gpu.webgpu;

  const pool = createWorkerPool({ size: 2 });
  const wf: WorkerFrame<'rgba8'> = await pool.decode(file, { output: 'rgba8' });
  const wd: Uint8ClampedArray = wf.data;
  const remote = await pool.open(buffer, { transfer: true });
  const rf = await remote.decode();
  const rd: Uint16Array = rf.data;
  await remote.close();
  pool.terminate();

  void [variant, camera, w, h, cfa, wb, iso, u16, backend, total, pattern, clamped, imageData, f32, dfltData, norm, tile];
  void [m, f, p, t, ok, fmt, webgpu, wd, rd];
}

// @ts-expect-error the constructor is private
new FlashRaw();
// @ts-expect-error unknown output
void FlashRaw.prototype.decode.call(null as unknown as FlashRaw, { output: 'png' });
