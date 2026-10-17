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
  /**
   * JPEG XL DNG support (DNG 1.7 compression 52546), which lives in separate, larger binaries
   * (`flashraw_simd_jxl.wasm`, `flashraw_simd_threads_jxl.wasm`):
   * - `'auto'` (default): load the small build; the first decode of a JPEG XL DNG loads the
   *   matching JPEG XL variant (once) and decodes that file with it. Other files never load it.
   * - `'always'`: load the JPEG XL variant of the selected build up front, instead of the small one.
   * - `'never'`: never load it; decoding a JPEG XL DNG throws `UnsupportedCompressionError`.
   * Requires the SIMD build. Node.js: `FLASHRAW_WASM_JPEGXL`.
   */
  jpegxl?: JpegXlMode;
  /** Explicit module source of the JPEG XL variant (like `wasm`; must match the threaded or
   * single-threaded selection). */
  jpegxlWasm?: BufferSource | WebAssembly.Module | Response | Promise<Response> | URL | string;
}

export type JpegXlMode = 'auto' | 'always' | 'never';

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
  /** Whether the loaded binary includes the JPEG XL codec (`jpegxl: 'always'`). */
  jpegxl: boolean;
  jpegxlMode: JpegXlMode;
  /** Why JPEG XL DNGs cannot be decoded (null when they can, loading the variant if needed). */
  jpegxlReason: string | null;
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

/** `simd`: flashraw_simd.wasm, `fallback`: flashraw.wasm, `threads`: flashraw_simd_threads.wasm,
 * `gpu`: flashraw_simd_gpu.wasm, `simd-jxl`: flashraw_simd_jxl.wasm, `threads-jxl`:
 * flashraw_simd_threads_jxl.wasm. */
export type WasmBinary = 'simd' | 'fallback' | 'threads' | 'gpu' | 'simd-jxl' | 'threads-jxl';

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

/** Which record `RawMetadata.orientation` was taken from. */
export type OrientationSource = 'exif' | 'maker-note' | 'camera-profile' | 'default';

/** How a camera's standard and vendor orientation records are combined (from the camera database). */
export type OrientationPolicy =
  | 'exif-only'
  | 'makernote-preferred'
  | 'makernote-when-exif-normal'
  | 'canon-auto-rotate-aware';

/** The orientation records found in the file and how `orientation` was resolved from them. */
export interface OrientationInfo {
  /** Standard TIFF/EXIF Orientation; `null` when missing or invalid (e.g. 0). */
  exif: Orientation | null;
  /** Vendor record: MakerNote (Canon CameraOrientation, Nikon edit rotation) or the EXIF of the camera's own embedded JPEG (Kodak). */
  makerNote: Orientation | null;
  /** Camera Auto Rotate setting (`true` = on), when recorded. */
  autoRotate: boolean | null;
  /** The orientation to apply; equal to `RawMetadata.orientation`. */
  resolved: Orientation;
  source: OrientationSource;
  policy: OrientationPolicy;
}

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
  /**
   * Orientation to apply to this preview's stored pixels, resolved independently of the RAW
   * image (a preview with its own EXIF orientation, or one already rendered rotated, differs).
   */
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
  /** Orientation to apply to the RAW image (resolved; see `orientationInfo`). */
  orientation: Orientation;
  /** Standard and vendor orientation records and how `orientation` was chosen. */
  orientationInfo: OrientationInfo;
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
  /** True when the camera already applied `whiteBalance` to the samples (Sony lossless YCbCr
   * M/S, Nikon sNEF): RGB outputs develop with unit multipliers; do not apply it again. */
  whiteBalanceApplied: boolean;
  colorMatrices: ColorMatrices;
  previews: PreviewDescriptor[];
  /** Vendor-specific extras (mode names, quality, crop mode, ...). */
  extra: Record<string, string>;
  /** DNG rendering hints FlashRaw does not apply itself; `null` for non-DNG files. */
  dng: DngExtras | null;
  /** DNG opcode lists with parameters, known before decoding (`[]` when there are none). */
  opcodes: OpcodeInfo[];
}

/** DNG camera-profile tags (presence and table sizes; the tables are not decoded). */
export interface DngProfileInfo {
  name: string | null;
  embedPolicy: number | null;
  /** ProfileHueSatMapDims `[hue, sat, val]` when HueSatMap data exists. */
  hueSatMapDims: [number, number, number] | null;
  /** Number of ProfileHueSatMapData tables: 0, 1, 2 (or 3 with DNG 1.6 triple illuminant). */
  hueSatMapCount: number;
  /** ProfileLookTableDims `[hue, sat, val]` when ProfileLookTableData exists. */
  lookTableDims: [number, number, number] | null;
  /** ProfileToneCurve present. */
  toneCurve: boolean;
  /** BaselineExposureOffset (stops). */
  baselineExposureOffset: number | null;
}

/** DNG rendering hints from IFD0 / the raw IFD. */
export interface DngExtras {
  /** BaselineExposure in stops (signed). */
  baselineExposure: number | null;
  baselineNoise: number | null;
  baselineSharpness: number | null;
  /** LinearResponseLimit (fraction of white where the sensor stops being linear). */
  linearResponseLimit: number | null;
  /** AsShotNeutral as stored (camera RGB of the neutral). */
  asShotNeutral: number[] | null;
  /** AsShotWhiteXY chromaticity (used instead of AsShotNeutral by some writers). */
  asShotWhiteXY: [number, number] | null;
  /** DefaultUserCrop as fractions of the default-cropped image: top, left, bottom, right. */
  defaultUserCrop: [number, number, number, number] | null;
  /** DefaultScale (horizontal, vertical). */
  defaultScale: [number, number] | null;
  /** NoiseProfile: (scale, offset) pairs per plane. */
  noiseProfile: number[] | null;
  profile: DngProfileInfo;
}

/** One DNG opcode (OpcodeList1/2/3). */
export interface OpcodeInfo {
  list: 1 | 2 | 3;
  /** DNG opcode id (1 = WarpRectilinear ... 13 = ScalePerColumn). */
  id: number;
  /** e.g. 'WarpRectilinear', 'FixVignetteRadial', 'GainMap'; unknown ids are 'Opcode<id>'. */
  name: string;
  optional: boolean;
  previewOnly: boolean;
  /**
   * Whether FlashRaw applies it during decode (geometric ones never are). List 1 applies to
   * every output, list 2 to `normalized` and RGB outputs, list 3 to RGB outputs only.
   */
  applied: boolean;
  /** Decoded parameters for the opcodes an editor implements itself, else null. */
  params: WarpRectilinearParams | FixVignetteRadialParams | null;
}

/** Lens distortion / lateral CA model (DNG §6.2.1); `center` in relative image coordinates. */
export interface WarpRectilinearParams {
  kind: 'WarpRectilinear';
  /** One entry per plane (1 = all planes, else R, G, B). */
  planes: { kr: [number, number, number, number]; kt: [number, number] }[];
  center: [number, number];
}

/** Radial vignette gain `1 + k0 r^2 + ... + k4 r^10` (DNG §6.2.3). */
export interface FixVignetteRadialParams {
  kind: 'FixVignetteRadial';
  k: [number, number, number, number, number];
  center: [number, number];
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
  /** See `RawMetadata.whiteBalanceApplied`. */
  whiteBalanceApplied: boolean;
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

/**
 * - `raw-u16`, `raw-f32`: sensor samples as stored. `normalized`: black/white-normalised samples.
 * - `linear-rgb` (f32), `linear-rgb-f16` (half floats as `Uint16Array` bits, half the memory),
 *   `linear-rgb16` (`[0, 1]` → `[0, 65535]` integers): demosaiced, white-balanced linear sRGB
 *   primaries in the sensor orientation (`frame.orientation` says what to apply).
 * - `rgba8` (`Uint8ClampedArray`, for `ImageData`) and `rgb16` (`Uint16Array`, 3 channels):
 *   display-ready sRGB with the orientation applied.
 */
export type OutputFormat =
  | 'raw-u16'
  | 'raw-f32'
  | 'normalized'
  | 'linear-rgb'
  | 'linear-rgb16'
  | 'linear-rgb-f16'
  | 'rgba8'
  | 'rgb16';

/** Outputs left on the GPU (WebGPU build, `init({ gpu: true })`): a `GpuFrame`. */
export type GpuOutputFormat = 'gpu-raw-u16' | 'gpu-normalized' | 'gpu-linear-rgb' | 'gpu-rgba8';

/** Host output with the same content as a GPU output (`'gpu-rgba8'` → `'rgba8'`). */
export type HostOutput<O extends OutputFormat | GpuOutputFormat> = O extends `gpu-${infer H extends OutputFormat}` ? H : O;

/**
 * What `decode()` resolves to: a `DecodedFrame`, or for `gpu-*` outputs a `GpuFrame` (or the
 * host-output `DecodedFrame` when WebGPU was unavailable and `fallback` is `'cpu'`).
 */
export type DecodeResult<O extends OutputFormat | GpuOutputFormat, S extends SampleType = 'f32'> = O extends GpuOutputFormat
  ? GpuFrame<O> | DecodedFrame<HostOutput<O>>
  : O extends OutputFormat
    ? DecodedFrame<O, S>
    : never;

/**
 * Sample type of `normalized` and `linear-rgb` outputs: `f16` = IEEE half floats as `Uint16Array`
 * bits (`dataType: 'f16'`).
 */
export type SampleType = 'f32' | 'f16';

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

/** See `DecodeOptions.corruptionPolicy`. */
export type CorruptionPolicy = 'recover' | 'strict';

/** Where a frame's pixels come from: the sensor data, or the embedded preview (`previewFallback`). */
export type FrameSource = 'sensor' | 'preview';

/** Structured cause of corrupt sensor data (`CorruptFileError.corrupt`, `DecodeWarning`). */
export interface CorruptInfo {
  /**
   * `truncated`: the data runs past the end of the file; `invalid-unit`: a compressed strip/tile
   * failed to decode; `preview-fallback` (warnings only): the sensor data could not be decoded and
   * the frame holds the embedded full-size preview (`previewFallback: true`).
   */
  kind: 'truncated' | 'invalid-unit' | 'preview-fallback';
  /** Which part of the payload. */
  unit: 'strip' | 'tile' | 'plane' | 'payload';
  /** Strip/tile/plane index (null for `payload`). */
  index: number | null;
  /** `truncated`: file offset where the unit starts. */
  offset?: number;
  /** `truncated`: bytes the unit needs. */
  expected?: number;
  /** `truncated`: bytes present before the end of the file. */
  available?: number;
  /** `truncated`: length of the input. */
  fileLength?: number;
  /** `invalid-unit`, `preview-fallback`: what the decoder reported. */
  message?: string;
}

/** A repair made under `corruptionPolicy: 'recover'`, or the preview substitution of `previewFallback`. */
export interface DecodeWarning extends Omit<CorruptInfo, 'message'> {
  /** Output samples left zero because their data is missing or invalid. */
  samplesFilled: number;
  /** Readable description, e.g. "recovered truncated strip 17: 48291 / 131072 bytes available; ...". */
  message: string;
}

export interface DecodeOptions {
  backend?: Backend;
  /** What to do when `backend: "gpu"` / a `gpu-*` output is unavailable: decode on the CPU (default) or throw. */
  fallback?: 'cpu' | 'error';
  /** Default `raw-u16`. `gpu-*` outputs stay on the GPU (WebGPU build). */
  output?: OutputFormat | GpuOutputFormat;
  /** Demosaic for RGB outputs. `half-size` = 2x2 binning (fast). Default `bilinear`. */
  demosaic?: 'bilinear' | 'half-size';
  /**
   * RGB outputs: crop to the default crop (default true). Raw outputs (`raw-u16`, `raw-f32`,
   * `normalized`) are never cropped by `true`; `'active'` crops them to `frameInfo.activeArea`
   * (like `region: activeArea`): `origin` reports the buffer position, `cfa` is anchored at
   * the origin, `blackLevel`/`maskedAreas` keep buffer semantics (black patterns stay anchored at
   * buffer (0, 0); masked-area black levels are still measured from the masked areas).
   * An explicit `region` wins. On RGB outputs `'active'` behaves like `true`.
   */
  crop?: boolean | 'active';
  /**
   * `normalized` and `linear-rgb` only: `'f16'` returns IEEE half floats (as `Uint16Array` bits,
   * round to nearest even, saturating at 65504; `dataType: 'f16'`) — half the memory and upload
   * size of `'f32'` (default). `normalized` is converted in place in WASM memory; `linear-rgb`
   * is developed straight to halves (the `linear-rgb-f16` output, reported as `linear-rgb`).
   */
  sampleType?: SampleType;
  /**
   * RGB outputs: exposure in EV (stops), applied to linear values after the colour conversion
   * and before `blackPoint`/`whitePoint` and the output encoding (range [-32, 32], default 0).
   * A convenience control: no tone curve or highlight recovery.
   */
  exposure?: number;
  /** RGB outputs: linear value (after exposure) mapped to black. Default 0. */
  blackPoint?: number;
  /**
   * RGB outputs: linear value (after exposure) mapped to white; must be above `blackPoint`.
   * Integer outputs clip outside [black, white]; float outputs keep the values. Default 1.
   */
  whitePoint?: number;
  /**
   * Apply DNG opcode lists (bad pixels, lens-shading gain maps, vignetting) in their
   * specified stages. Geometric opcodes are never applied. Default true.
   */
  applyOpcodes?: boolean;
  /**
   * Truncated or corrupt sensor data. `'recover'` (default): decode every complete sample,
   * zero-fill the rest and report it in `frame.warnings` (`frame.incomplete` is then true).
   * `'strict'`: throw a `CorruptFileError` whose `corrupt` field describes the problem.
   * Recovery covers uncompressed/packed strips, tiles and payloads, compressed DNG
   * strips/tiles and Nikon/Pentax Huffman and Samsung SRW streams cut short by the end of
   * the file; other compressed streams throw under both policies.
   */
  corruptionPolicy?: CorruptionPolicy;
  /**
   * RGB outputs (`rgba8`, `linear-rgb`) of files whose sensor data cannot be decoded
   * (`UnsupportedCompressionError`, e.g. Nikon High Efficiency NEF): return the embedded
   * full-size preview instead of throwing. The frame then has `source: 'preview'` and a
   * `preview-fallback` warning; its levels and colour fields describe the 8-bit preview. Needs a
   * preview covering at least 90% of the default crop; never applies to raw outputs or regions.
   * Default false (throw).
   */
  previewFallback?: boolean;
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
  /** The WASM binary that decoded the frame (`'simd-jxl'` / `'threads-jxl'` for JPEG XL DNGs
   * opened with the small build). */
  binary: WasmBinary;
  /** Whether that binary includes the JPEG XL codec. */
  jpegxl: boolean;
}

export interface FrameData {
  'raw-u16': Uint16Array<ArrayBuffer>;
  'raw-f32': Float32Array<ArrayBuffer>;
  normalized: Float32Array<ArrayBuffer>;
  'linear-rgb': Float32Array<ArrayBuffer>;
  'linear-rgb16': Uint16Array<ArrayBuffer>;
  /** IEEE half floats as bits. */
  'linear-rgb-f16': Uint16Array<ArrayBuffer>;
  rgba8: Uint8ClampedArray<ArrayBuffer>;
  rgb16: Uint16Array<ArrayBuffer>;
}

/** `data` of a frame: `FrameData[O]`, or `Uint16Array` half floats for `normalized` / `linear-rgb` + `sampleType: 'f16'`. */
export type FrameDataFor<O extends OutputFormat, S extends SampleType = 'f32'> = O extends 'normalized' | 'linear-rgb'
  ? S extends 'f16'
    ? Uint16Array<ArrayBuffer>
    : FrameData[O]
  : FrameData[O];

/** Element type of `data`. */
export type DataType = 'u8' | 'u16' | 'f16' | 'f32';

/** Decoded frame metadata (no pixels): what the worker pool returns alongside `data`. */
export interface FrameFields<O extends OutputFormat = OutputFormat> {
  width: number;
  height: number;
  /** Samples per pixel: 1 (CFA), 3 (linear RGB outputs, rgb16, linear DNG), 4 (rgba8). */
  channels: number;
  output: O;
  dataType: DataType;
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
  /** See `RawMetadata.whiteBalanceApplied`. */
  whiteBalanceApplied: boolean;
  colorMatrices: ColorMatrices;
  /** Orientation still to apply (always 1 for rgba8 and rgb16, which are already oriented). */
  orientation: Orientation;
  backend: BackendReport;
  timings: Timings;
  inputBytes: number;
  outputBytes: number;
  /** DNG opcode lists: what was applied and what was not (empty without lists). */
  opcodes: string[];
  /** True when some samples hold no decoded data (recovered from truncated or corrupt input). */
  incomplete: boolean;
  /** Repairs made under `corruptionPolicy: 'recover'`; empty for a complete decode. */
  warnings: DecodeWarning[];
  /** `'preview'` when `previewFallback` substituted the embedded preview; `'sensor'` otherwise. */
  source: FrameSource;
}

/** A decoded frame as returned by `FlashRaw#decode`. */
export declare class DecodedFrame<O extends OutputFormat = OutputFormat, S extends SampleType = 'f32'> {
  private constructor();
  readonly width: number;
  readonly height: number;
  readonly channels: number;
  readonly output: O;
  readonly dataType: DataType;
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
  /** See `RawMetadata.whiteBalanceApplied`. */
  readonly whiteBalanceApplied: boolean;
  readonly colorMatrices: ColorMatrices;
  readonly orientation: Orientation;
  readonly backend: BackendReport;
  readonly timings: Timings;
  readonly inputBytes: number;
  readonly outputBytes: number;
  /** DNG opcode lists: what was applied and what was not (empty without lists). */
  readonly opcodes: string[];
  /** True when some samples hold no decoded data (recovered from truncated or corrupt input). */
  readonly incomplete: boolean;
  /** Repairs made under `corruptionPolicy: 'recover'` (the default); empty for a complete decode. */
  readonly warnings: DecodeWarning[];
  /** `'preview'` when `previewFallback` substituted the embedded preview; `'sensor'` otherwise. */
  readonly source: FrameSource;
  /** Pixels, row-major, `channels` interleaved samples per pixel. */
  readonly data: FrameDataFor<O, S>;
  /** True while `data` is a view into WASM memory (`zeroCopy: true`). */
  readonly zeroCopy: boolean;
  /** Copy the pixels out of WASM memory and free the WASM buffer (zero-copy frames). */
  detach(): FrameDataFor<O, S>;
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
  /** See `RawMetadata.whiteBalanceApplied`. */
  readonly whiteBalanceApplied: boolean;
  readonly colorMatrices: ColorMatrices;
  readonly orientation: Orientation;
  readonly backend: BackendReport;
  readonly timings: Timings;
  readonly inputBytes: number;
  readonly outputBytes: number;
  /** DNG opcode lists: what was applied and what was not (empty without lists). */
  readonly opcodes: string[];
  /** True when some samples hold no decoded data (recovered from truncated or corrupt input). */
  readonly incomplete: boolean;
  /** Repairs made under `corruptionPolicy: 'recover'` (the default); empty for a complete decode. */
  readonly warnings: DecodeWarning[];
  /** `'preview'` when `previewFallback` substituted the embedded preview; `'sensor'` otherwise. */
  readonly source: FrameSource;
  readonly gpuResident: true;
  /**
   * Storage buffer (`STORAGE | COPY_SRC`) holding the pixels; may be larger than `byteLength`.
   * Throws for frames beyond the adapter's buffer limits, which are stored in bands (use
   * `download()` or `createTexture()`).
   */
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
  /** The WASM binary serving this file (changes to the JPEG XL variant on the first decode of a
   * JPEG XL DNG in `jpegxl: 'auto'` mode). */
  readonly binary: WasmBinary;

  /** Largest embedded preview, or null if there is none. */
  preview<T extends PreviewOptions | undefined = undefined>(options?: T): Promise<PreviewFor<T> | null>;
  /** Smallest embedded preview, or null if there is none. */
  thumbnail<T extends PreviewOptions | undefined = undefined>(options?: T): Promise<PreviewFor<T> | null>;

  decode<O extends OutputFormat | GpuOutputFormat = 'raw-u16', S extends SampleType = 'f32'>(
    options?: DecodeOptions & { output?: O; sampleType?: S },
  ): Promise<DecodeResult<O, S>>;
  decodeRegion<O extends OutputFormat | GpuOutputFormat = 'raw-u16', S extends SampleType = 'f32'>(
    region: Rect,
    options?: Omit<DecodeOptions, 'region'> & { output?: O; sampleType?: S },
  ): Promise<DecodeResult<O, S>>;
  decodeTile<O extends OutputFormat | GpuOutputFormat = 'raw-u16', S extends SampleType = 'f32'>(
    index: number,
    options?: Omit<DecodeOptions, 'region'> & { output?: O; sampleType?: S },
  ): Promise<DecodeResult<O, S>>;

  /** Free the WASM memory of this file (idempotent). */
  close(): void;
  [Symbol.dispose](): void;
}

// ------------------------------------------------------------------------------- functions

export function init(options?: InitOptions): Promise<BuildInfo>;
/**
 * Select another build at run time (e.g. `{ gpu: true }` after the default threaded one), unlike
 * `init()`, which honours only its first options. Open handles keep the build they were opened
 * in; new handles use the new one. Every binary instantiated stays in memory for the life of the
 * page (wasm-bindgen glue holds its instance in module state; WebAssembly memory never shrinks)
 * and is reused when selected again. Rejects with `NotAvailableError` when asked for the SIMD
 * build after the scalar one or vice versa (they share one glue module); the previous selection
 * then stays in place.
 */
export function reinit(options?: InitOptions): Promise<BuildInfo>;
export function decodeRaw<O extends OutputFormat = 'raw-u16', S extends SampleType = 'f32'>(
  input: RawInput,
  options?: OpenOptions & Omit<DecodeOptions, 'zeroCopy'> & { output?: O; sampleType?: S },
): Promise<DecodedFrame<O, S>>;
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
  jpegxl: {
    mode: JpegXlMode | null;
    /** The selected binary has the JPEG XL codec (`'always'`). */
    compiled: boolean;
    /** A binary with the JPEG XL codec is loaded. */
    loaded: boolean;
    /** That binary (`'simd-jxl'` / `'threads-jxl'`), or null. */
    binary: WasmBinary | null;
    /** JPEG XL DNGs can be decoded (possibly after loading the variant). */
    available: boolean;
    /** Why they cannot, or why the last load failed. */
    reason: string | null;
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

/** Load the JPEG XL variant now instead of on the first JPEG XL decode; resolves to its binary
 * name. Rejects with `NotAvailableError` (`jpegxl: 'never'`, no SIMD, load failure). */
export function loadJpegXlSupport(): Promise<WasmBinary>;

export const JPEGXL_MODES: readonly JpegXlMode[];

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
  constructor(message: string, kind?: ErrorKind, options?: { cause?: unknown; corrupt?: CorruptInfo });
  readonly kind: ErrorKind;
  /** `CorruptFile` errors with a known cause (e.g. truncated sensor data): the details. */
  readonly corrupt?: CorruptInfo;
  toJSON(): { name: string; kind: ErrorKind; message: string; corrupt?: CorruptInfo };
}
declare class KindError<K extends ErrorKind> extends FlashRawError {
  constructor(message: string, options?: { cause?: unknown; corrupt?: CorruptInfo });
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
