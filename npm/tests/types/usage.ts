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
  reinit,
  FlashRawError,
  GpuUnavailableError,
  GpuFrame,
  getGpuDevice,
  initThreads,
  GPU_OUTPUTS,
  JPEGXL_MODES,
  loadJpegXlSupport,
  type JpegXlMode,
  type FlashRawGPUBuffer,
  type GpuOutputFormat,
  type WasmBinary,
  type BuildInfo,
  type Orientation,
  type OrientationInfo,
  type OrientationPolicy,
  type OrientationSource,
  type RawMetadata,
  type DecodedFrame,
  type DecodeOptions,
  type Capabilities,
  type ErrorKind,
  type CorruptInfo,
  type DecodeWarning,
  type FrameSource,
  type ColorSpace,
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
  const wbApplied: boolean = raw.metadata.whiteBalanceApplied && raw.frameInfo.whiteBalanceApplied;
  const iso: number | null = raw.metadata.exposure.iso;
  const oi: OrientationInfo = raw.metadata.orientationInfo;
  const resolved: Orientation = oi.resolved;
  const mnOrientation: Orientation | null = oi.makerNote;
  const autoRotate: boolean | null = oi.autoRotate;
  const source: OrientationSource = oi.source;
  const policy: OrientationPolicy = oi.policy;
  // @ts-expect-error source is a closed set
  const badSource: OrientationSource = 'xmp';

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
  const frameWbApplied: boolean = frame.whiteBalanceApplied && frame.toJSON().whiteBalanceApplied;

  const img = await raw.decode({ output: 'rgba8' });
  const clamped: Uint8ClampedArray = img.data;
  const imageData: ImageData = img.toImageData();

  const lin = await raw.decode({ output: 'linear-rgb', demosaic: 'half-size' });
  const f32: Float32Array = lin.data;

  // 16-bit and half-float RGB, with exposure and black/white points.
  const rgb16 = await raw.decode({ output: 'rgb16', exposure: 0.5, blackPoint: 0.01, whitePoint: 0.95 });
  const rgb16Data: Uint16Array = rgb16.data;
  const lin16: Uint16Array = (await raw.decode({ output: 'linear-rgb16' })).data;
  const linHalf: Uint16Array = (await raw.decode({ output: 'linear-rgb-f16' })).data;
  const linHalf2: Uint16Array = (await raw.decode({ output: 'linear-rgb', sampleType: 'f16' })).data;
  // @ts-expect-error f16 linear data is not a Float32Array
  const notF32Lin: Float32Array = (await raw.decode({ output: 'linear-rgb', sampleType: 'f16' })).data;
  // @ts-expect-error exposure is a number
  await raw.decode({ output: 'rgb16', exposure: '1' });

  const dflt = await raw.decode();
  const dfltData: Uint16Array = dflt.data;

  const region = await raw.decodeRegion({ x: 0, y: 0, width: 64, height: 64 }, { output: 'normalized' });
  const norm: Float32Array = region.data;
  // Editor options: half-float normalized output cropped to the active area.
  const half = await raw.decode({ output: 'normalized', sampleType: 'f16', crop: 'active', zeroCopy: true });
  const halfBits: Uint16Array = half.data;
  const dt: 'u8' | 'u16' | 'f16' | 'f32' = half.dataType;
  const origin: number = half.origin.x;
  half.free();
  // @ts-expect-error f16 normalized data is not a Float32Array
  const notF32: Float32Array = (await raw.decode({ output: 'normalized', sampleType: 'f16' })).data;
  const be: number | null | undefined = raw.metadata.dng?.baselineExposure;
  const xy: [number, number] | null | undefined = raw.metadata.dng?.asShotWhiteXY;
  for (const op of raw.metadata.opcodes) {
    if (op.params?.kind === 'WarpRectilinear') {
      const kr: [number, number, number, number] = op.params.planes[0].kr;
      void kr;
    }
  }
  void [halfBits, dt, origin, notF32, be, xy, wbApplied, frameWbApplied];
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
  const switched: BuildInfo = await reinit({ gpu: true });
  const back: WasmBinary = (await reinit()).binary;
  void [n, binary, tc, switched, back];

  // JPEG XL variants.
  const jx = await init({ jpegxl: 'auto' as JpegXlMode, jpegxlWasm: new URL('https://cdn.example/flashraw_simd_jxl.wasm') });
  const jxlBinary: WasmBinary = await loadJpegXlSupport();
  const jxlCaps: { loaded: boolean; binary: WasmBinary | null; reason: string | null } = (await getCapabilities()).jpegxl;
  const jxlFrame = await decodeRaw(new Uint8Array(buffer));
  const servedBy: WasmBinary = jxlFrame.backend.binary;
  const hasJxl: boolean = jxlFrame.backend.jpegxl;
  const modes: readonly JpegXlMode[] = JPEGXL_MODES;
  void [jx.jpegxl, jx.jpegxlMode, jxlBinary, jxlCaps, servedBy, hasJxl, modes];

  const m: RawMetadata = await readMetadata(file);
  const f: DecodedFrame<'rgba8'> = await decodeRaw(new Uint8Array(buffer), { output: 'rgba8' });
  const strict = await decodeRaw(new Uint8Array(buffer), { corruptionPolicy: 'strict' });
  const incomplete: boolean = strict.incomplete;
  const filled: number = strict.warnings.map((w: DecodeWarning) => w.samplesFilled).reduce((a, b) => a + b, 0);
  const unitIndex: number | null = strict.warnings[0]?.index ?? null;
  try {
    await decodeRaw(new Uint8Array(buffer), { corruptionPolicy: 'recover' });
  } catch (e) {
    if (e instanceof FlashRawError && e.corrupt) {
      const info: CorruptInfo = e.corrupt;
      const avail: number | undefined = info.available;
      void avail;
    }
  }
  const fb = await decodeRaw(new Uint8Array(buffer), { output: 'rgba8', previewFallback: true });
  const frameSource: FrameSource = fb.source;
  const substituted: boolean = fb.warnings.some((w: DecodeWarning) => w.kind === 'preview-fallback');
  void [incomplete, filled, unitIndex, frameSource, substituted];
  const wide = await decodeRaw(new Uint8Array(buffer), { output: 'rgba8', colorSpace: 'display-p3', intent: 'relative' });
  const space: ColorSpace | 'custom' = wide.colorSpace;
  const icc: Uint8Array | null = wide.iccProfile;
  const tagged = await decodeRaw(new Uint8Array(buffer), { output: 'rgb16', outputProfile: icc ?? new Uint8Array(0) });
  void [space, tagged.colorSpace];
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
  const wh = await pool.decode(file, { output: 'normalized', sampleType: 'f16' });
  const whd: Uint16Array = wh.data;
  void whd;
  const ac = new AbortController();
  const rc: Uint8ClampedArray = (await remote.decode({ output: 'rgba8', signal: ac.signal })).data;
  const timed = await pool.readMetadata(file, { signal: AbortSignal.timeout(5000) });
  const queued: number = pool.queued;
  await remote.close();
  pool.terminate();

  void [variant, camera, w, h, cfa, wb, iso, u16, backend, total, pattern, clamped, imageData, f32, dfltData, norm, tile];
  void [m, f, p, t, ok, fmt, webgpu, wd, rd, rc, timed, queued];
}

// @ts-expect-error the constructor is private
new FlashRaw();
// @ts-expect-error unknown output
void FlashRaw.prototype.decode.call(null as unknown as FlashRaw, { output: 'png' });
// @ts-expect-error previewFallback is a boolean
void FlashRaw.prototype.decode.call(null as unknown as FlashRaw, { previewFallback: 'yes' });
// @ts-expect-error unknown corruption policy
void FlashRaw.prototype.decode.call(null as unknown as FlashRaw, { corruptionPolicy: 'lenient' });
// @ts-expect-error unknown colour space
void FlashRaw.prototype.decode.call(null as unknown as FlashRaw, { colorSpace: 'lab' });
