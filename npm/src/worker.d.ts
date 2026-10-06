// Type definitions for `flashraw/worker`.
import type {
  DecodeOptions,
  FileCapabilities,
  FrameFields,
  FrameDataFor,
  SampleType,
  FrameInfo,
  InitOptions,
  OpenOptions,
  OutputFormat,
  PreviewOptions,
  EncodedPreview,
  RgbaPreview,
  RawFormatName,
  RawInput,
  RawMetadata,
  Rect,
  TileInfo,
  Capabilities,
} from './index.js';

export interface WorkerPoolOptions {
  /** Number of workers (default: min(4, hardwareConcurrency - 1), at least 1). Spawned lazily. */
  size?: number;
  /** Options for `init()` in each worker (e.g. `{ simd: false }`). */
  init?: Omit<InitOptions, 'wasm' | 'jpegxlWasm'>;
  /** Custom worker script URL (must serve `worker-entry.browser.js` / `worker-entry.node.js`). */
  workerUrl?: string | URL;
  /** Full control over worker creation (e.g. bundler-specific `?worker` imports). */
  createWorker?: () => any;
}

export interface TransferOption {
  /**
   * Move (not copy) the input ArrayBuffer to the worker; it becomes detached for the caller.
   * The input is sent when a worker starts the request (requests queue until one is free).
   */
  transfer?: boolean;
}

export interface CancelOption {
  /**
   * Cancel the request: a queued request is dropped; a running one terminates its worker (WASM
   * cannot be interrupted), which also closes the remote handles opened in that worker. The
   * promise rejects with `signal.reason` (an `AbortError` by default). `AbortSignal.timeout(ms)`
   * bounds how long a decode may take.
   */
  signal?: AbortSignal;
}

/** A decoded frame received from a worker: plain fields + a standalone typed array. */
export type WorkerFrame<O extends OutputFormat = OutputFormat, S extends SampleType = 'f32'> = FrameFields<O> & {
  data: FrameDataFor<O, S>;
};
type PreviewFor<T> = T extends { format: 'rgba8' } ? RgbaPreview : EncodedPreview;

/** A FlashRaw handle that lives inside one worker. */
export declare class RemoteFlashRaw {
  private constructor();
  readonly metadata: RawMetadata;
  readonly frameInfo: FrameInfo;
  readonly capabilities: FileCapabilities;
  readonly tiles: TileInfo[];
  readonly byteLength: number;
  readonly parseMs: number;
  readonly format: RawFormatName;
  /** True after `close()`, or once the worker holding the file has terminated. */
  readonly closed: boolean;
  decode<O extends OutputFormat = 'raw-u16', S extends SampleType = 'f32'>(
    options?: Omit<DecodeOptions, 'zeroCopy'> & CancelOption & { output?: O; sampleType?: S },
  ): Promise<WorkerFrame<O, S>>;
  decodeRegion<O extends OutputFormat = 'raw-u16', S extends SampleType = 'f32'>(
    region: Rect,
    options?: Omit<DecodeOptions, 'zeroCopy' | 'region'> & CancelOption & { output?: O; sampleType?: S },
  ): Promise<WorkerFrame<O, S>>;
  decodeTile<O extends OutputFormat = 'raw-u16', S extends SampleType = 'f32'>(
    index: number,
    options?: Omit<DecodeOptions, 'zeroCopy' | 'region'> & CancelOption & { output?: O; sampleType?: S },
  ): Promise<WorkerFrame<O, S>>;
  preview<T extends Omit<PreviewOptions, 'zeroCopy'> | undefined = undefined>(options?: T & CancelOption): Promise<PreviewFor<T> | null>;
  thumbnail<T extends Omit<PreviewOptions, 'zeroCopy'> | undefined = undefined>(options?: T & CancelOption): Promise<PreviewFor<T> | null>;
  close(): Promise<void>;
  [Symbol.asyncDispose](): Promise<void>;
}

export declare class WorkerPool {
  private constructor();
  readonly size: number;
  /** Requests running or waiting per spawned worker. */
  readonly load: number[];
  /** Requests waiting for a free worker. */
  readonly queued: number;
  decode<O extends OutputFormat = 'raw-u16', S extends SampleType = 'f32'>(
    input: RawInput,
    options?: OpenOptions & Omit<DecodeOptions, 'zeroCopy'> & TransferOption & CancelOption & { output?: O; sampleType?: S },
  ): Promise<WorkerFrame<O, S>>;
  readMetadata(input: RawInput, options?: OpenOptions & TransferOption & CancelOption): Promise<RawMetadata>;
  extractPreview<T extends Omit<PreviewOptions, 'zeroCopy'> | undefined = undefined>(
    input: RawInput,
    options?: OpenOptions & TransferOption & CancelOption & T,
  ): Promise<PreviewFor<T> | null>;
  extractThumbnail<T extends Omit<PreviewOptions, 'zeroCopy'> | undefined = undefined>(
    input: RawInput,
    options?: OpenOptions & TransferOption & CancelOption & T,
  ): Promise<PreviewFor<T> | null>;
  detectFormat(input: RawInput, options?: CancelOption): Promise<RawFormatName | null>;
  getCapabilities(): Promise<Capabilities>;
  /** Open a file inside one worker for repeated calls (metadata is returned eagerly). */
  open(input: RawInput, options?: OpenOptions & TransferOption & CancelOption): Promise<RemoteFlashRaw>;
  /** Stop all workers and reject pending requests. */
  terminate(): void;
  [Symbol.dispose](): void;
}

export function createWorkerPool(options?: WorkerPoolOptions): WorkerPool;
