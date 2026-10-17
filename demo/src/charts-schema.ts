// Schema of the benchmark chart data: docs/charts/data.json (when present) or the sample in
// src/charts-data.sample.json (generated from docs/benchmarks.md by
// scripts/make-sample-chart-data.mjs). See README.md, "Chart data".

export type LibraryId = 'flashraw' | 'libraw' | 'rawspeed' | (string & {});
export type MachineId = 'linux' | 'macos' | (string & {});

export interface ChartsData {
  version: 1;
  /** true for the generated sample */
  sample?: boolean;
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
  /** Geometric-mean speedups per format/codec and machine. */
  formats: {
    format: string;
    codec: string;
    files: number | null;
    speedup: Record<MachineId, { decodeVsLibraw: number | null; decodeVsRawspeed: number | null; rgbVsLibraw: number | null }>;
  }[];
}
