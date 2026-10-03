// Type definitions for the `flashraw` npm package.

// ---------------------------------------------------------------------------------- inputs

/** Anything FlashRaw can read: bytes (copied once into WASM memory), a Blob/File (streamed),
 * or — in Node.js — a file path / file: URL. */
export type RawInput = ArrayBuffer | SharedArrayBuffer | ArrayBufferView | Blob | string | URL;

export interface InitOptions {
  /** Force the SIMD (`true`) or scalar fallback (`false`) binary. Default: runtime detection. */
  simd?: boolean;
  /** Directory URL holding `flashraw_simd.wasm` / `flashraw.wasm` (e.g. a CDN path). */
  baseUrl?: string | URL;
  /** Explicit module source: bytes, a compiled `WebAssembly.Module`, a `Response`, or a URL.
   * It must match the selected build (pass `threads: false` for a single-threaded binary,
   * `gpu: true` for the WebGPU binary). */
  wasm?: BufferSource | WebAssembly.Module | Response | Promise<Response> | URL | string;
  /**
   * Multithreaded build: `true`/`'auto'` (default: when cross-origin isolated in browsers, always
   * in Node.js; falls back to single-threaded otherwise), `false` (single-threaded), or a thread
   * count. Node.js: `FLASHRAW_WASM_THREADS`.
   */
  threads?: boolean | number | 'auto';
  /** Load the WebGPU build (`flashraw_simd_gpu.wasm`; single-threaded). Default false. */
  gpu?: boolean;
  /** Custom URL of `thread-worker.browser.js` / `thread-worker.node.js` (threads). */
  threadWorkerUrl?: string | URL;
}

export interface BuildInfo {
  version: string;
  /** Whether the loaded binary was compiled with SIMD128. */
  simd: boolean;
  simdLevel: string;
  /** Whether the binary includes the WebGPU backend. */
  gpu: boolean;
  gpuAvailable: boolean;
  /** Whether the loaded binary is the multithreaded build. */
  threads: boolean;
  /** Worker threads started (0 when single-threaded). */
  threadCount: number;
  maxSamples: number;
  /** SIMD128 (`simd`) or scalar (`fallback`) code. */
  variant: 'simd' | 'fallback';
  /** The binary in use. */
  binary: WasmBinary;
  simdSupported: boolean;
  /** Why the scalar binary is used. */
  fallbackReason: string | null;
  /** Why the multithreaded build is not used (null when it is). */
  threadsReason: string | null;
  /** Why the WebGPU build requested with `gpu: true` could not be loaded. */
  gpuReason: string | null;
  initMs: number;
}

/** `simd`: flashraw_simd.wasm, `fallback`: flashraw.wasm, `threads`: flashraw_simd_threads.wasm, `gpu`: flashraw_simd_gpu.wasm. */
export type WasmBinary = 'simd' | 'fallback' | 'threads' | 'gpu';

export interface Limits {
  /** Maximum width*height*channels of a decoded image (default 400M on WASM). */
  maxSamples?: number;
  /** Maximum width or height (default 65535). */
  maxDimension?: number;
}

export interface OpenOptions {
  limits?: Limits;
  /** Options for the implicit `init()` (only honoured by the first initialisation). */
  init?: InitOptions;
}

// ------------------------------------------------------------------------------- metadata

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type CfaLetter = 'R' | 'G' | 'B' | 'C' | 'M' | 'Y' | 'W' | 'E';

export interface CfaInfo {
  /** Row-major pattern, e.g. "RGGB" or a 36-letter X-Trans pattern. Anchored at buffer (0,0). */
  pattern: string;
  width: number;
  height: number;
  colors: CfaLetter[];
}

export type SensorLayout = 'bayer' | 'x-trans' | 'quad-bayer' | 'other-cfa' | 'monochrome' | 'linear' | 'foveon';

export interface BlackLevel {
  /** Repeat pattern size; `values` has rows*cols*samples entries, anchored at buffer (0,0). */
  rows: number;
  cols: number;
  samples: number;
  values: number[];
  mean: number;
  /** Per-row additive deltas (DNG BlackLevelDeltaV). */
  rowDeltas?: number[];
  /** Per-column additive deltas (DNG BlackLevelDeltaH). */
  colDeltas?: number[];
}

export interface ColorMatrix {
  /** EXIF LightSource / DNG CalibrationIlluminant code (21 = D65, 17 = Standard A, ...). */
  illuminant: number;
  /** Correlated colour temperature of the illuminant (K), when known. */
  temperature: number | null;
  rows: number;
  /** Row-major `rows x 3` values. */
  values: number[];
}

export interface ColorMatrices {
  /** XYZ → camera matrices (DNG ColorMatrix1/2/3 or camera database). */
  color: ColorMatrix[];
  /** Camera → XYZ(D50) (DNG ForwardMatrix). */
  forward: ColorMatrix[];
  /** DNG CameraCalibration. */
  calibration: ColorMatrix[];
  analogBalance: number[];
  source: 'file' | 'camera-db' | 'none';
}

/** EXIF orientation code: 1 = normal, 3 = 180°, 6 = 90° CW, 8 = 270° CW, 2/4/5/7 = mirrored. */
export type Orientation = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;

export type PreviewKind = 'thumbnail' | 'preview' | 'full-size';
export type PreviewEncoding = 'jpeg' | 'rgb8' | 'ycbcr422' | 'jpeg-xl' | 'heif';

export interface PreviewDescriptor {
  index: number;
  kind: PreviewKind;
  encoding: PreviewEncoding;
  /** `image/jpeg`, `image/jxl`, `image/heif`, or `application/x-flashraw-rgb8` for raw RGB. */
  mimeType: string;
  width: number;
  height: number;
  byteLength: number;
  /** Byte offset inside the RAW file. */
  offset: number;
  orientation: Orientation;
}

export interface CameraInfo {
  /** Human-friendly "Make Model" using canonical names. */
  name: string;
  make: string;
  model: string;
  canonicalMake: string;
  canonicalModel: string;
  uniqueModel: string | null;
  serial: string | null;
  firmware: string | null;
}

export interface LensInfo {
  make: string | null;
  model: string | null;
  serial: string | null;
  /** Min/max focal length (mm). */
  focalRange: [number, number] | null;
  /** Max aperture at min/max focal length. */
  apertureRange: [number, number] | null;
}

export interface ExposureInfo {
  iso: number | null;
  /** Seconds. */
  exposureTime: number | null;
  fNumber: number | null;
  /** mm. */
  focalLength: number | null;
  focalLength35mm: number | null;
  exposureBias: number | null;
  flash: number | null;
  meteringMode: number | null;
  exposureProgram: number | null;
}

export interface GpsInfo {
  latitude: number | null;
  longitude: number | null;
  altitude: number | null;
  timestamp: string | null;
}

export type RawFormatName =
  | 'DNG' | 'CR2' | 'CR3' | 'CRW' | 'NEF' | 'NRW' | 'ARW' | 'SR2' | 'SRF' | 'RAF' | 'RW2' | 'ORF' | 'PEF'
  | 'IIQ' | 'MOS' | '3FR' | 'FFF' | 'MRW' | 'SRW' | 'X3F' | 'KDC' | 'DCR' | 'DCS' | 'ERF' | 'MEF' | 'MDC'
  | 'RAW' | 'ARI' | 'other';

/** Normalised metadata, available right after `open()` without decoding pixels. */
export interface RawMetadata {
  format: RawFormatName;
  camera: CameraInfo;
  software: string | null;
  artist: string | null;
  copyright: string | null;
  lens: LensInfo;
  exposure: ExposureInfo;
  /** `YYYY-MM-DDTHH:MM:SS[.sss][±HH:MM]` when available. */
  captureTime: string | null;
  gps: GpsInfo | null;
  orientation: Orientation;
  /** Image size after the default crop. */
  width: number;
  height: number;
  /** Full sensor buffer size (incl. masked borders). */
  rawWidth: number;
  rawHeight: number;
  bitsPerSample: number;
  /** Payload compression, e.g. "uncompressed", "lossless-jpeg", "canon-crx". */
  compression: string;
  /** e.g. "1.4.0.0" for DNG files. */
  dngVersion: string | null;
  cfa: CfaInfo | null;
  layout: SensorLayout;
  blackLevel: BlackLevel;
  /** Saturation per channel. */
  whiteLevel: number[];
  /** As-shot multipliers [R, G, B, G2], normalised to G = 1. */
  whiteBalance: [number, number, number, number] | null;
  colorMatrices: ColorMatrices;
  previews: PreviewDescriptor[];
  /** Vendor-specific extras (mode names, quality, crop mode, ...). */
  extra: Record<string, string>;
}

/** Geometry and calibration of the sensor buffer (known before decoding). */
export interface FrameInfo {
  width: number;
  height: number;
  channels: number;
  bitsPerSample: number;
  sampleFormat: 'u16' | 'f32';
  cfa: CfaInfo | null;
  layout: SensorLayout;
  activeArea: Rect;
  crop: Rect;
  maskedAreas: Rect[];
  blackLevel: BlackLevel;
  /** When true, `blackLevel` is measured from masked areas at decode time. */
  blackFromMasked: boolean;
  whiteLevel: number[];
  whiteBalance: [number, number, number, number] | null;
  colorMatrices: ColorMatrices;
  orientation: Orientation;
  pixelAspect: number;
  compression: string;
}

export type RandomAccess =
  | { kind: 'sequential' }
  | { kind: 'rows' }
  | { kind: 'strips'; rows: number }
  | { kind: 'tiles'; width: number; height: number }
  | { kind: 'blocks' };

export interface FileCapabilities {
  randomAccess: RandomAccess;
  /** Number of independently decodable tiles/strips. */
  tiles: number;
  previews: number;
  /** Uniformly bit-packed payload (GPU-unpackable). */
  gpuUnpack: boolean;
  /** Floating-point samples (float DNG). */
  float: boolean;
}

export interface TileInfo {
  index: number;
  rect: Rect;
  offset: number;
  byteLength: number;
}

// --------------------------------------------------------------------------------- decoding

export type OutputFormat = 'raw-u16' | 'raw-f32' | 'normalized' | 'linear-rgb' | 'rgba8';

/** Outputs left on the GPU (WebGPU build, `init({ gpu: true })`): a `GpuFrame`. */
export type GpuOutputFormat = 'gpu-raw-u16' | 'gpu-normalized' | 'gpu-linear-rgb' | 'gpu-rgba8';

/** Host output with the same content as a GPU output (`'gpu-rgba8'` → `'rgba8'`). */
export type HostOutput<O extends OutputFormat | GpuOutputFormat> = O extends `gpu-${infer H extends OutputFormat}` ? H : O;

/**
 * What `decode()` resolves to: a `DecodedFrame`, or for `gpu-*` outputs a `GpuFrame` (or the
 * host-output `DecodedFrame` when WebGPU was unavailable and `fallback` is `'cpu'`).
 */
export type DecodeResult<O extends OutputFormat | GpuOutputFormat> = O extends GpuOutputFormat
  ? GpuFrame<O> | DecodedFrame<HostOutput<O>>
  : O extends OutputFormat
    ? DecodedFrame<O>
    : never;

// WebGPU types resolve to the real ones when they are in scope (e.g. `@webgpu/types`), else `object`.
type WebGpuType<K extends string> = K extends keyof typeof globalThis
  ? (typeof globalThis)[K] extends { prototype: infer P }
    ? P
    : object
  : object;
export type FlashRawGPUBuffer = WebGpuType<'GPUBuffer'>;
export type FlashRawGPUTexture = WebGpuType<'GPUTexture'>;
export type FlashRawGPUDevice = WebGpuType<'GPUDevice'>;

/**
 * - `auto`: fastest available (WASM SIMD when loaded; GPU once available).
 * - `cpu`: portable scalar reference kernels. `cpu-simd`: explicit SIMD kernels.
 * - `gpu` / `webgpu`: WebGPU compute (falls back per `fallback`).
 */
export type Backend = 'auto' | 'cpu' | 'scalar' | 'cpu-simd' | 'simd' | 'gpu' | 'webgpu';

export interface DecodeOptions {
  backend?: Backend;
  /** What to do when `backend: "gpu"` / a `gpu-*` output is unavailable: decode on the CPU (default) or throw. */
  fallback?: 'cpu' | 'error';
  /** Default `raw-u16`. `gpu-*` outputs stay on the GPU (WebGPU build). */
  output?: OutputFormat | GpuOutputFormat;
  /** Demosaic for RGB outputs. `half-size` = 2x2 binning (fast). Default `bilinear`. */
  demosaic?: 'bilinear' | 'half-size';
  /** Crop RGB outputs to the default crop (default true; raw outputs are never cropped). */
  crop?: boolean;
  /** Decode only this region (sensor-buffer coordinates). */
  region?: Rect;
  limits?: Limits;
  /**
   * Return a view into WASM memory instead of a copy. The frame must then be released with
   * `free()` (or `using`), and `frame.data` must be re-read after other FlashRaw calls.
   */
  zeroCopy?: boolean;
}

export type StageName =
  | 'containerParse' | 'metadata' | 'entropyDecode' | 'unpack' | 'predictor' | 'calibration'
  | 'postprocess' | 'gpuUpload' | 'gpuCompute' | 'gpuDownload' | 'preview';

export interface Timings {
  /** Total time inside FlashRaw (incl. container parse at open), ms. */
  totalMs: number;
  stages: Partial<Record<StageName, number>>;
  /** Copying the pixels out of WASM memory, ms (0 for zero-copy frames). */
  copyMs: number;
  /** Wall time of the JS `decode()` call, ms. */
  wallMs: number;
}

export interface BackendReport {
  /** Backend that was asked for. */
  requested: string;
  /** What ran: `cpu` or `gpu`. */
  kind: 'cpu' | 'gpu';
  /** e.g. "CPU WASM SIMD128 x1 thread". */
  decode: string;
  postprocess: string | null;
  simd: string;
  threads: number;
  gpuAdapter: string | null;
  /** Why the requested backend was not used (e.g. GPU not built / no WebGPU). */
  fallbackReason: string | null;
}

export interface FrameData {
  'raw-u16': Uint16Array<ArrayBuffer>;
  'raw-f32': Float32Array<ArrayBuffer>;
  normalized: Float32Array<ArrayBuffer>;
  'linear-rgb': Float32Array<ArrayBuffer>;
  rgba8: Uint8ClampedArray<ArrayBuffer>;
}

/** Decoded frame metadata (no pixels): what the worker pool returns alongside `data`. */
export interface FrameFields<O extends OutputFormat = OutputFormat> {
  width: number;
  height: number;
  /** Samples per pixel: 1 (CFA), 3 (linear RGB / linear DNG), 4 (rgba8). */
  channels: number;
  output: O;
  dataType: 'u8' | 'u16' | 'f32';
  bitsPerSample: number;
  /** CFA pattern for raw outputs (anchored at the frame origin's sensor position). */
  cfa: CfaInfo | null;
  layout: SensorLayout;
  activeArea: Rect;
  crop: Rect;
  /** Position of this frame in sensor-buffer coordinates (non-zero for regions/tiles). */
  origin: { x: number; y: number };
  maskedAreas: Rect[];
  blackLevel: BlackLevel;
  whiteLevel: number[];
  whiteBalance: [number, number, number, number] | null;
  colorMatrices: ColorMatrices;
  /** Orientation still to apply (always 1 for rgba8, which is already oriented). */
  orientation: Orientation;
  backend: BackendReport;
  timings: Timings;
  inputBytes: number;
  outputBytes: number;
}

/** A decoded frame as returned by `FlashRaw#decode`. */
export declare class DecodedFrame<O extends OutputFormat = OutputFormat> {
  private constructor();
  readonly width: number;
  readonly height: number;
  readonly channels: number;
  readonly output: O;
  readonly dataType: 'u8' | 'u16' | 'f32';
  readonly bitsPerSample: number;
  readonly cfa: CfaInfo | null;
  readonly layout: SensorLayout;
  readonly activeArea: Rect;
  readonly crop: Rect;
  readonly origin: { x: number; y: number };
  readonly maskedAreas: Rect[];
  readonly blackLevel: BlackLevel;
  readonly whiteLevel: number[];
  readonly whiteBalance: [number, number, number, number] | null;
  readonly colorMatrices: ColorMatrices;
  readonly orientation: Orientation;
  readonly backend: BackendReport;
  readonly timings: Timings;
  readonly inputBytes: number;
  readonly outputBytes: number;
  /** Pixels, row-major, `channels` interleaved samples per pixel. */
  readonly data: FrameData[O];
  /** True while `data` is a view into WASM memory (`zeroCopy: true`). */
  readonly zeroCopy: boolean;
  /** Copy the pixels out of WASM memory and free the WASM buffer (zero-copy frames). */
  detach(): FrameData[O];
  /** Free the WASM buffer of a zero-copy frame (no-op otherwise). */
  free(): void;
  [Symbol.dispose](): void;
  /** `new ImageData(data, width, height)` for `rgba8` frames. */
  toImageData(): ImageData;
  toJSON(): FrameFields<O>;
}

/**
 * A decoded image resident on the GPU (`output: 'gpu-*'`, WebGPU build). Lives on FlashRaw's
 * device (`getGpuDevice()`). `free()` returns the buffer to FlashRaw's pool.
 */
export declare class GpuFrame<O extends GpuOutputFormat = GpuOutputFormat> {
  private constructor();
  readonly width: number;
  readonly height: number;
  readonly channels: number;
  readonly output: O;
  readonly bitsPerSample: number;
  readonly cfa: CfaInfo | null;
  readonly layout: SensorLayout;
  readonly activeArea: Rect;
  readonly crop: Rect;
  readonly origin: { x: number; y: number };
  readonly maskedAreas: Rect[];
  readonly blackLevel: BlackLevel;
  readonly whiteLevel: number[];
  readonly whiteBalance: [number, number, number, number] | null;
  readonly colorMatrices: ColorMatrices;
  readonly orientation: Orientation;
  readonly backend: BackendReport;
  readonly timings: Timings;
  readonly inputBytes: number;
  readonly outputBytes: number;
  readonly gpuResident: true;
  /** Storage buffer (`STORAGE | COPY_SRC`) holding the pixels; may be larger than `byteLength`. */
  readonly buffer: FlashRawGPUBuffer;
  /** Bytes of pixel data at the start of `buffer`. */
  readonly byteLength: number;
  /** `u16`: two samples per 32-bit word; `f32`; `rgba8`: one `R | G<<8 | B<<16 | A<<24` pixel per word. */
  readonly format: 'u16' | 'f32' | 'rgba8';
  /** The device owning `buffer`. */
  readonly device: FlashRawGPUDevice;
  /** `gpu-rgba8` only: a new `rgba8unorm` texture (TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC), owned by the caller. */
  createTexture(): FlashRawGPUTexture;
  /** Read back into host memory. */
  download(): Promise<DecodedFrame<HostOutput<O>>>;
  /** Take ownership of `buffer` (no longer recycled; call `destroy()` on it). Frees this frame. */
  detachBuffer(): FlashRawGPUBuffer;
  free(): void;
  [Symbol.dispose](): void;
  toJSON(): Omit<FrameFields, 'output' | 'dataType'> & { output: O };
}

export interface PreviewOptions {
  /** `encoded` (default): the embedded bytes (usually JPEG). `rgba8`: decoded pixels. */
  format?: 'encoded' | 'rgba8';
  /** Pick a specific preview (see `metadata.previews`) instead of the largest/smallest. */
  index?: number;
  /** `encoded` only: return a view into the input buffer (valid until `close()`). */
  zeroCopy?: boolean;
}

export interface EncodedPreview extends PreviewDescriptor {
  format: 'encoded';
  /** The embedded image bytes; wrap in a Blob with `mimeType` to display. */
  data: Uint8Array<ArrayBuffer>;
}

export interface RgbaPreview extends PreviewDescriptor {
  format: 'rgba8';
  /** Decoded RGBA pixels (orientation NOT applied; see `orientation`). */
  data: Uint8ClampedArray<ArrayBuffer>;
  decodeMs: number;
}

export type Preview = EncodedPreview | RgbaPreview;
type PreviewFor<T extends PreviewOptions | undefined> = T extends { format: 'rgba8' } ? RgbaPreview : EncodedPreview;

/** An opened RAW file. */
export declare class FlashRaw {
  private constructor();
  /** Open from bytes (copied once into WASM memory), a Blob/File (streamed), or a path (Node.js). */
  static open(input: RawInput, options?: OpenOptions): Promise<FlashRaw>;
  /** Node.js only: open a file by path (read in chunks straight into WASM memory). */
  static openFile(path: string | URL, options?: OpenOptions): Promise<FlashRaw>;
  static init(options?: InitOptions): Promise<BuildInfo>;

  readonly closed: boolean;
  readonly metadata: RawMetadata;
  readonly frameInfo: FrameInfo;
  readonly capabilities: FileCapabilities;
  readonly tiles: TileInfo[];
  readonly format: RawFormatName;
  readonly previews: PreviewDescriptor[];
  readonly byteLength: number;
  readonly parseMs: number;

  /** Largest embedded preview, or null if there is none. */
  preview<T extends PreviewOptions | undefined = undefined>(options?: T): Promise<PreviewFor<T> | null>;
  /** Smallest embedded preview, or null if there is none. */
  thumbnail<T extends PreviewOptions | undefined = undefined>(options?: T): Promise<PreviewFor<T> | null>;

  decode<O extends OutputFormat | GpuOutputFormat = 'raw-u16'>(options?: DecodeOptions & { output?: O }): Promise<DecodeResult<O>>;
  decodeRegion<O extends OutputFormat | GpuOutputFormat = 'raw-u16'>(
    region: Rect,
    options?: Omit<DecodeOptions, 'region'> & { output?: O },
  ): Promise<DecodeResult<O>>;
  decodeTile<O extends OutputFormat | GpuOutputFormat = 'raw-u16'>(
    index: number,
    options?: Omit<DecodeOptions, 'region'> & { output?: O },
  ): Promise<DecodeResult<O>>;

  /** Free the WASM memory of this file (idempotent). */
  close(): void;
  [Symbol.dispose](): void;
}

// ------------------------------------------------------------------------------- functions

export function init(options?: InitOptions): Promise<BuildInfo>;
export function decodeRaw<O extends OutputFormat = 'raw-u16'>(
  input: RawInput,
  options?: OpenOptions & Omit<DecodeOptions, 'zeroCopy'> & { output?: O },
): Promise<DecodedFrame<O>>;
export function readMetadata(input: RawInput, options?: OpenOptions): Promise<RawMetadata>;
export function extractPreview<T extends Omit<PreviewOptions, 'zeroCopy'> | undefined = undefined>(
  input: RawInput,
  options?: OpenOptions & T,
): Promise<PreviewFor<T> | null>;
export function extractThumbnail<T extends Omit<PreviewOptions, 'zeroCopy'> | undefined = undefined>(
  input: RawInput,
  options?: OpenOptions & T,
): Promise<PreviewFor<T> | null>;
/** Cheap detection: does this look like a RAW file FlashRaw understands? */
export function supportsFile(input: RawInput): Promise<boolean>;
/** Cheap detection: container format name, or null. */
export function detectFormat(input: RawInput): Promise<RawFormatName | null>;
/** Whether the JS engine supports WebAssembly SIMD128 (synchronous). */
export function simdSupported(): boolean;

export interface Capabilities {
  version: string | null;
  platform: 'browser' | 'worker' | 'node' | 'unknown';
  wasm: {
    loaded: boolean;
    variant: 'simd' | 'fallback' | null;
    binary: WasmBinary | null;
    initMs: number | null;
    error: string | null;
  };
  simd: { supported: boolean; active: boolean; level: string | null; fallbackReason: string | null };
  threads: {
    /** Multithreaded WASM (atomics + SharedArrayBuffer) build in use. */
    wasmThreads: boolean;
    /** Decoder worker threads in use (0 = single-threaded). */
    count: number;
    /** Why the multithreaded build is not in use (null when it is). */
    reason: string | null;
    sharedArrayBuffer: boolean;
    crossOriginIsolated: boolean;
    hardwareConcurrency: number;
    /** Worker-based parallelism (`flashraw/worker`) available. */
    workers: boolean;
  };
  gpu: {
    /** The WebGPU build is loaded (`init({ gpu: true })`). */
    compiled: boolean;
    webgpu: boolean;
    /** A WebGPU device was created (requested on first use; this call may request it). */
    available: boolean;
    reason: string | null;
    adapter: GpuAdapterInfo | null;
  };
  outputs: (OutputFormat | GpuOutputFormat)[];
  backends: Backend[];
  limits: { maxSamples: number | null };
}

export interface GpuAdapterInfo {
  name: string;
  backend: string;
  driver: string;
  class: 'discrete' | 'integrated' | 'virtual' | 'software' | 'unknown';
  timestamps: boolean;
  maxBufferSize: number;
  maxStorageBinding: number;
  description: string;
}

export function getCapabilities(): Promise<Capabilities>;

/**
 * Use the multithreaded build with `count` threads (default: hardware concurrency). Call before
 * any other FlashRaw call; resolves to the thread count in use (0 = single-threaded; see
 * `getCapabilities().threads.reason`).
 */
export function initThreads(count?: number): Promise<number>;

/** The `GPUDevice` FlashRaw uses (WebGPU build), or null without WebGPU. Requests it if needed. */
export function getGpuDevice(): Promise<FlashRawGPUDevice | null>;

export const OUTPUTS: readonly OutputFormat[];
export const GPU_OUTPUTS: readonly GpuOutputFormat[];
export const BACKENDS: readonly Backend[];

// ---------------------------------------------------------------------------------- errors

export type ErrorKind =
  | 'UnsupportedFormat'
  | 'UnsupportedCompression'
  | 'CorruptFile'
  | 'InvalidMetadata'
  | 'CameraUnsupported'
  | 'GpuUnavailable'
  | 'OutOfMemory'
  | 'LimitExceeded'
  | 'NotAvailable'
  | 'InvalidArgument'
  | 'Io'
  | 'Closed'
  | 'Internal';

/** Base class of every error thrown by flashraw. */
export declare class FlashRawError extends Error {
  constructor(message: string, kind?: ErrorKind, options?: { cause?: unknown });
  readonly kind: ErrorKind;
  toJSON(): { name: string; kind: ErrorKind; message: string };
}
declare class KindError<K extends ErrorKind> extends FlashRawError {
  constructor(message: string, options?: { cause?: unknown });
  readonly kind: K;
}
export declare class UnsupportedFormatError extends KindError<'UnsupportedFormat'> {}
export declare class UnsupportedCompressionError extends KindError<'UnsupportedCompression'> {}
export declare class CorruptFileError extends KindError<'CorruptFile'> {}
export declare class InvalidMetadataError extends KindError<'InvalidMetadata'> {}
export declare class CameraUnsupportedError extends KindError<'CameraUnsupported'> {}
export declare class GpuUnavailableError extends KindError<'GpuUnavailable'> {}
export declare class OutOfMemoryError extends KindError<'OutOfMemory'> {}
export declare class LimitExceededError extends KindError<'LimitExceeded'> {}
export declare class NotAvailableError extends KindError<'NotAvailable'> {}
export declare class InvalidArgumentError extends KindError<'InvalidArgument'> {}
export declare class IoError extends KindError<'Io'> {}
export declare class ClosedError extends KindError<'Closed'> {}
/** WASM trap or unexpected failure; the handle that raised it becomes unusable. */
export declare class InternalError extends KindError<'Internal'> {}
export const ERROR_KINDS: readonly ErrorKind[];
export function errorFromKind(kind: ErrorKind | string, message: string, options?: { cause?: unknown }): FlashRawError;
