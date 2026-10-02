# Benchmarks

All numbers in this document were measured on real corpus files with the tools in
this repository. They are reproducible:

```sh
python3 tools/corpus/fetch.py                     # CC0 corpus
    --out benches/results/<host>.jsonl --markdown benches/results/<host>.md
cargo bench -p flashraw --bench kernels           # micro-benchmarks (criterion)
cargo bench -p flashraw-gpu --bench gpu_pipeline  # GPU (needs an adapter)
```

## Method

- **Equivalent work only.** "Decode" = open + full sensor decode to 16-bit samples
  bilinear demosaic + as-shot white balance + colour matrix + 8-bit sRGB on *both*
- Input is read into memory before timing. Each figure is the median of 5 timed runs
  after a warm-up, each tool in a fresh process (so peak RSS is per decode).
- Benchmark set (`benches/benchmark-set.tsv`): the largest and the median-size file of
  0.4 MP to 154 MP covering every supported codec.
- Hardware: AMD Ryzen 7 9700X (8 cores / 16 threads, 32 MB L3), 64 GB DDR5, Linux 6.12.


Full results: [`benches/results/linux-x86_64.md`](../benches/results/linux-x86_64.md).

| | Decode | RAW → displayable RGB |
|---|---:|---:|
| Median speedup | **4.5×** | **9.3×** |
| P10 speedup (= P90 latency ratio) | **1.7×** | **4.2×** |
| Files ≥ 40 MP, median | **4.4×** | **9.6×** |
| Best | **80×** | **14.9×** |
| Worst | 0.66× | 2.7× |

Against the project targets: median decode ≥ 2× ✓ (4.5×); P90 formats ≥ 1.5× ✓
(1.7×); best ≥ 4× ✓ (80×); RGB median ≥ 3× ✓ (9.3×); large RAW RGB ≥ 5× ✓ (9.6×);
best-case RGB ≥ 8× ✓ (14.9×).

Per format (median of the files in the set):

| Format | files | decode | RAW→RGB | | Format | files | decode | RAW→RGB |
|---|---:|---:|---:|---|---|---:|---:|---:|
| 3FR | 4 | 1.7× | 6.8× | | MOS | 3 | 15.7× | 11.2× |
| ARW | 10 | 4.5× | 6.2× | | MRW | 4 | 36.0× | 10.4× |
| CR2 | 4 | 2.6× | 3.5× | | NEF | 4 | 3.2× | 7.4× |
| CR3 | 2 | 3.9× | 6.6× | | NRW | 2 | 4.6× | 9.3× |
| CRW | 3 | 3.5× | 5.0× | | ORF | 4 | 8.1× | 8.5× |
| DCR / DCS | 5 | 4.5× | 7–10× | | PEF | 4 | 6.5× | 7.8× |
| DNG | 6 | 5.0× | 6.5× | | RAF | 4 | 8.1× | 10.1× |
| ERF | 2 | 4.2× | 7.5× | | RW2 | 12 | 6.3× | 10.4× |
| FFF | 2 | 2.0× | 3.8× | | SRW | 8 | 3.8× | 6.2× |
| IIQ | 5 | 7.4× | 10.6× | | X3F | 2 | 11.5× | 11.1× |
| KDC | 4 | 19.3× | 10.9× | | headerless / other | 6 | 5–7× | 7.5–11.7× |

Largest files (decode, ms):

| Camera | Codec | MP | FlashRaw | reference | Speedup |
|---|---|---:|---:|---:|---:|
| Phase One IQ4 150MP | IIQ L | 154 | 60.8 | 630.4 | 10.4× |
| Phase One iXM-RS150F | IIQ S | 154 | 42.9 | 319.6 | 7.4× |
| Hasselblad X2D 100C | uncompressed 3FR | 105 | 14.8 | 20.7 | 1.4× |
| Hasselblad X2D 100C | Hasselblad LJPEG | 105 | 425.1 | 851.5 | 2.0× |
| Fujifilm GFX100S | Fuji compressed | 103 | 141.1 | 2539.9 | 18.0× |
| Fujifilm GFX100S | 16-bit uncompressed | 103 | 13.1 | 17.1 | 1.3× |
| Panasonic DC-GH6 | RW2 v8 | 100 | 207.6 | 755.8 | 3.6× |
| Panasonic DC-S5 | RW2 v7 | 97 | 17.8 | 38.6 | 2.2× |
| Panasonic DC-G9 | RW2 v5 | 82 | 14.1 | 86.1 | 6.1× |
| Olympus PEN-F high-res | uncompressed | 81 | 16.9 | 226.3 | 13.4× |

### Where FlashRaw is not faster

- **Small uncompressed 16-bit files** (Coolpix A1000, GitUp: 1.9 vs 1.3 ms). The work is
  a memcpy-like copy; FlashRaw zero-initialises its output buffer (fresh pages from the
  heap memory without zeroing. Zero-initialisation is kept on purpose: it guarantees
  deterministic output for samples a truncated or partial payload does not cover.
- **Sequential entropy streams** gain only from per-thread efficiency: CR2 (2.6×),
  Hasselblad LJPEG (2.0×), Samsung V2 (~1.3×), single-strip lossless DNG. These streams
  cannot be split; FlashRaw's advantage there is the bit pump/Huffman fast path.


[`benches/results/reference-linux-x86_64.md`](../benches/results/reference-linux-x86_64.md)

| | Decode |
|---|---:|
| Median speedup | **1.6×** |
| P10 | 0.79× |
| Best | 21× |

representation differs), Epson ERF and Panasonic v6/v7 small files (0.74–0.95×, a few
it is a sequential stream where per-sample cost decides (FlashRaw ~4.5 ns/sample,
(Fuji compressed, Phase One, Panasonic v4/v5, Olympus, Nikon, Sony ARW2/lossless tiles).

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
`Backend::Auto` picked the faster side in every case. Not yet measured on discrete
GPUs or browser WebGPU.

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

before/after and fails on slowdowns beyond the tolerance (used by `perf.yml`, which
benchmarks the PR and `main` on the same runner).
