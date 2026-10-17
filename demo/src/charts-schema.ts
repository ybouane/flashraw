// Schema of the benchmark chart data, converted at build time from docs/charts/data.json
// (vite.config.ts, `fromBenchData`). See README.md, "Chart data".

export type LibraryId = 'flashraw' | 'libraw' | 'rawspeed' | (string & {});
export type MachineId = 'linux' | 'macos' | (string & {});

export interface ChartsData {
  version: 1;
  source?: string;
  description?: string;
  libraries: { id: LibraryId; name: string }[];
  machines: { id: MachineId; name: string; cpu?: string; icon?: string }[];
  /** Speedup summaries (other library time / FlashRaw time). */
  headline: {
    metric: 'decode' | 'rgb';
    vs: LibraryId;
    threads: 'all' | '1';
    /** 'popular' = popular cameras list, 'all' = full benchmark set */
    set: 'popular' | 'all';
    machine: MachineId;
    geomean: number;
    median: number;
    /** files where FlashRaw is faster / files both decode */
    wins?: number;
    files?: number;
  }[];
  /** Per-camera wall times in ms (null = library cannot decode the file). */
  cameras: {
    machine: MachineId;
    camera: string;
    format: string;
    codec: string;
    mp: number | null;
    decode: Record<LibraryId, number | null>;
    rgb: Record<LibraryId, number | null>;
    /** FlashRaw RAW → RGB on the GPU, ms (optional) */
    gpu?: number | null;
  }[];
  /** Per format/codec and machine. */
  formats: {
    format: string;
    codec: string;
    files: number | null;
    /** Geometric-mean wall time in ms per machine, metric and library (null = not decoded). */
    ms: Record<MachineId, Record<'decode' | 'rgb', Record<LibraryId, number | null>>>;
  }[];
}
