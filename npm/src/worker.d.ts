// Type definitions for `flashraw/worker`.
import type {
  DecodeOptions,
  FileCapabilities,
  FrameFields,
  FrameData,
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
  init?: Omit<InitOptions, 'wasm'>;
  /** Custom worker script URL (must serve `worker-entry.browser.js` / `worker-entry.node.js`). */
  workerUrl?: string | URL;
  /** Full control over worker creation (e.g. bundler-specific `?worker` imports). */
  createWorker?: () => any;
}

export interface TransferOption {
  /** Move (not copy) the input ArrayBuffer to the worker; it becomes detached for the caller. */
  transfer?: boolean;
}

/** A decoded frame received from a worker: plain fields + a standalone typed array. */
export type WorkerFrame<O extends OutputFormat = OutputFormat> = FrameFields<O> & { data: FrameData[O] };
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
  readonly closed: boolean;
  decode<O extends OutputFormat = 'raw-u16'>(options?: Omit<DecodeOptions, 'zeroCopy'> & { output?: O }): Promise<WorkerFrame<O>>;
  decodeRegion<O extends OutputFormat = 'raw-u16'>(
    region: Rect,
    options?: Omit<DecodeOptions, 'zeroCopy' | 'region'> & { output?: O },
  ): Promise<WorkerFrame<O>>;
  decodeTile<O extends OutputFormat = 'raw-u16'>(
    index: number,
    options?: Omit<DecodeOptions, 'zeroCopy' | 'region'> & { output?: O },
  ): Promise<WorkerFrame<O>>;
  preview<T extends Omit<PreviewOptions, 'zeroCopy'> | undefined = undefined>(options?: T): Promise<PreviewFor<T> | null>;
  thumbnail<T extends Omit<PreviewOptions, 'zeroCopy'> | undefined = undefined>(options?: T): Promise<PreviewFor<T> | null>;
  close(): Promise<void>;
  [Symbol.asyncDispose](): Promise<void>;
}

export declare class WorkerPool {
  private constructor();
  readonly size: number;
  /** Pending requests per spawned worker. */
  readonly load: number[];
  decode<O extends OutputFormat = 'raw-u16'>(
    input: RawInput,
    options?: OpenOptions & Omit<DecodeOptions, 'zeroCopy'> & TransferOption & { output?: O },
  ): Promise<WorkerFrame<O>>;
  readMetadata(input: RawInput, options?: OpenOptions & TransferOption): Promise<RawMetadata>;
  extractPreview<T extends Omit<PreviewOptions, 'zeroCopy'> | undefined = undefined>(
    input: RawInput,
    options?: OpenOptions & TransferOption & T,
  ): Promise<PreviewFor<T> | null>;
  extractThumbnail<T extends Omit<PreviewOptions, 'zeroCopy'> | undefined = undefined>(
    input: RawInput,
    options?: OpenOptions & TransferOption & T,
  ): Promise<PreviewFor<T> | null>;
  detectFormat(input: RawInput): Promise<RawFormatName | null>;
  getCapabilities(): Promise<Capabilities>;
  /** Open a file inside one worker for repeated calls (metadata is returned eagerly). */
  open(input: RawInput, options?: OpenOptions & TransferOption): Promise<RemoteFlashRaw>;
  /** Stop all workers and reject pending requests. */
  terminate(): void;
  [Symbol.dispose](): void;
}

export function createWorkerPool(options?: WorkerPoolOptions): WorkerPool;
