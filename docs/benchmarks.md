# Benchmarks

All numbers in this document were measured on real corpus files with the tools in
this repository. They are reproducible:

```sh
python3 tools/corpus/fetch.py                     # CC0 corpus
cargo build --release -p flashraw-cli
python3 tools/bench/run.py benches/benchmark-set.tsv --iterations 5 \
    --out benches/results/<host>.jsonl --markdown benches/results/<host>.md
cargo bench -p flashraw --bench kernels           # micro-benchmarks (criterion)
cargo bench -p flashraw-gpu --bench gpu_pipeline  # GPU (needs an adapter)
```

## Method

- "Decode" = open + full sensor decode to 16-bit samples (`raw-u16`). "RAW→RGB" =
  decode + bilinear demosaic + as-shot white balance + colour matrix + oriented 8-bit
  sRGB (`rgba8`).
- Input is read into memory before timing. Each figure is the median of 5 timed runs
  after a warm-up, in a fresh process per file (so peak RSS is per decode).
- Benchmark set (`benches/benchmark-set.tsv`): the largest and the median-size file of
  every (format, compression) pair in the CC0 corpus — 101 files from 0.4 MP to 154 MP
  covering every supported codec.
- Hardware: AMD Ryzen 7 9700X (8 cores / 16 threads, 32 MB L3), 64 GB DDR5, Linux 6.12,
  all threads.

## Results

Full results: [`benches/results/linux-x86_64.md`](../benches/results/linux-x86_64.md).
Median throughput over the set: **1,960 MP/s** decode, **580 MP/s** RAW → RGBA8.

Per format (median throughput of the files in the set, MP/s):

| Format | files | decode | RAW→RGB | | Format | files | decode | RAW→RGB |
|---|---:|---:|---:|---|---|---:|---:|---:|
| 3FR | 4 | 4,114 | 659 | | MOS | 3 | 1,545 | 546 |
| ARW | 10 | 1,748 | 587 | | MRW | 4 | 33,106 | 772 |
| CR2 | 4 | 717 | 368 | | NEF | 4 | 4,375 | 592 |
| CR3 | 2 | 724 | 382 | | NRW | 2 | 6,076 | 702 |
| CRW | 3 | 318 | 227 | | ORF | 4 | 2,462 | 441 |
| DCR / DCS | 5 | 800–16,429 | 412–755 | | PEF | 4 | 12,104 | 557 |
| DNG | 6 | 1,084 | 476 | | RAF | 4 | 4,296 | 525 |
| ERF | 2 | 1,614 | 531 | | RW2 | 12 | 4,398 | 694 |
| FFF | 2 | 1,108 | 494 | | SRW | 8 | 552 | 327 |
| IIQ | 5 | 3,438 | 637 | | X3F | 2 | 309 | 220 |
| KDC / MEF | 5 | 7,715–10,059 | 642–723 | | headerless / other | 6 | 4,759–6,941 | 657–749 |

Uncompressed and packed formats run at memory bandwidth; entropy-coded formats are
bound by their entropy decoder (parallel over tiles, strips or blocks where the format
has them, and by speculation over single streams otherwise — see
[architecture.md](architecture.md)).

Largest files (ms):

| Camera | Codec | MP | Decode | RAW→RGBA8 |
|---|---|---:|---:|---:|
| Phase One IQ4 150MP | IIQ L | 154 | 52.7 | 216.2 |
| Phase One iXM-RS150F | IIQ S | 154 | 44.8 | 204.8 |
| Hasselblad X2D 100C | uncompressed 3FR | 105 | 14.6 | 124.2 |
| Hasselblad X2D 100C | Hasselblad LJPEG | 105 | 92.4 | 202.2 |
| Fujifilm GFX100S | Fuji compressed | 103 | 142.6 | 232.7 |
| Fujifilm GFX100S | 16-bit uncompressed | 103 | 13.1 | 123.9 |
| Panasonic DC-GH6 | RW2 v8 | 100 | 101.6 | 212.8 |
| Panasonic DC-S5 | RW2 v7 | 97 | 16.8 | 127.8 |
| Panasonic DC-G9 | RW2 v5 | 82 | 14.3 | 102.3 |

Single sequential streams are decoded in parallel by speculation (lossless JPEG,
Hasselblad, Nikon, Pentax, Panasonic v8 strips, Sony ARW1). Samsung V2 cannot be: rows
start wherever the previous one ended and are predicted from earlier rows, so it runs
on one thread with a staged block decoder (a parse/reconstruct pipeline was measured
and rejected — it gained 7% on x86 and lost 7% on Apple M1).

## Kernels (criterion, `cargo bench -p flashraw --bench kernels`)

| Kernel | Throughput |
|---|---:|
| 12-bit MSB unpack, scalar reference | 1.05 G samples/s |
| 12-bit MSB unpack, SSE4.1 | 8.8 G samples/s |
| 16-bit LE unpack (fast / SIMD) | 35–37 G samples/s |
| Lossless JPEG decode, 2 components (synthetic) | 617 M samples/s |
| Develop to RGBA8 (bilinear), 12 MP | see `cargo bench` |

## Thread scaling and memory-bound kernels

Unpack/copy kernels are memory-bound: on this machine they peak at ~4 threads and get
1.7–1.9× slower at 16 threads (memory-controller and page-fault contention), e.g. a
24.6 MP 14-bit DNG unpacks in 7.8 ms (1 thread), 3.0 ms (4), 5.2 ms (16). FlashRaw caps
such kernels with `par::memory_bound_workers()` (threads/4, clamped to 2–8) while
entropy decoders and the develop pass use all cores. Large output buffers are
advised as transparent huge pages (`MADV_HUGEPAGE`), cutting page faults ~500×.

## GPU (Apple M1 Ultra, Metal)

From [gpu.md](gpu.md) (20 CPU threads vs GPU; median of 7):

| File | Case | CPU | GPU (download) | GPU (resident) |
|---|---|---:|---:|---:|
| Leica Q2 47 MP, packed 14-bit | raw u16 unpack | **2.3 ms** | 6.4 ms | 3.4 ms |
| Leica Q2 | RAW → RGBA8 | 34.5 ms | 13.9 ms | **8.5 ms** |
| Leica Q2 | RAW → RGBA8 half-size | 23.7 ms | 6.9 ms | **5.3 ms** |
| Nokia 1020 38 MP, packed 10-bit | RAW → RGBA8 | 27.8 ms | 11.4 ms | **6.6 ms** |
| Leica CL 24.6 MP | RAW → RGBA8 | 19.2 ms | 7.7 ms | **4.8 ms** |
| Pentax 645Z 52 MP, LJPEG | RAW → RGBA8 | 278.0 ms | 261.1 ms | **258.1 ms** |

The GPU wins the development pipeline (2.5–4× resident) and loses raw unpacking (the
CPU is at memory bandwidth); for entropy-coded files the CPU decode dominates.
`Backend::Auto` picked the faster side in every case. Banded (tiled) processing for
frames beyond the adapter's limits and browser WebGPU are measured in [gpu.md](gpu.md)
and [wasm.md](wasm.md); no discrete GPU was available for measurement.

## WASM

From [wasm.md](wasm.md) (single-threaded WASM vs single-threaded native, same
machine): lossless JPEG decode 1.2–1.6× native time; RAW → RGBA8 1.2–1.4×; packed
formats 2–3 ms per frame (2–3× native, which uses wider SIMD). SIMD128 halves packed
unpack time vs the non-SIMD build. A JS worker pool gives 2.7× on 8 files with 4
workers.

Bundle size with all decoders (wasm-opt -O3):

| Binary | Raw | gzip | brotli |
|---|---:|---:|---:|
| `flashraw_simd.wasm` | 1978 KiB | 678 KiB | 507 KiB |
| `flashraw.wasm` (fallback) | 1946 KiB | 666 KiB | 497 KiB |

Code is ~1.5 MB spread across ~40 format decoders (no single dominant function) plus
~0.5 MB of read-only tables (camera database, size tables) that compress well. An
`opt-level = "s"` build saves only ~10–12% (gzip) at a measurable speed cost, so the
release build stays speed-optimised.

## Performance regression detection

`tools/bench/run.py ... --baseline <old.jsonl> --tolerance 15` prints per-file
before/after and fails on slowdowns beyond the tolerance (used by `perf.yml`, which
benchmarks the PR and `main` on the same runner).
