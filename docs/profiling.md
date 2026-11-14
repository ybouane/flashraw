# Profiling

## Built-in stage timers

Every decode records wall-clock time per stage (`ContainerParse`, `EntropyDecode`,
`Unpack`, `Calibration`, `Postprocess`, GPU upload/compute/download) into
`DecodeStatistics::stages`. From the CLI:

```sh
flashraw benchmark --profile --iterations 20 file.CR2
flashraw benchmark --profile --output rgba8 --threads 1 file.NEF   # single-threaded view
```

These are the first stop: they show which stage dominates and how it scales with
`--threads` (memory-bound stages stop scaling at a few threads, entropy decoding scales
with cores when the format allows it).

## Sampling profilers

Release builds keep line tables (`[profile.release] debug = "line-tables-only"`), so sampled stacks
resolve to source functions without a special build.

**macOS (Instruments' Time Profiler, no installation beyond Xcode command-line tools):**

```sh
xcrun xctrace record --template "Time Profiler" --output decode.trace \
    --launch -- target/release/flashraw benchmark --iterations 20 --backend simd file.RAF
xcrun xctrace export --input decode.trace \
    --xpath '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]' > profile.xml
```

The exported XML deduplicates backtraces and frames through `id`/`ref` attributes;
resolve references when aggregating, otherwise most samples appear to have no stack.
Open the `.trace` bundle in Instruments for flame graphs.

**Linux (`perf`, where installed):**

```sh
perf record -g --call-graph dwarf -F 2000 -- target/release/flashraw benchmark --iterations 20 file.ARW
perf report --no-children --sort symbol
```

Inline-heavy hot loops (bit pumps, Huffman decode) are attributed to their caller
after inlining; use `--sort symbol,srcline` or the per-stage timers to separate them.

## Findings (Apple M1 Ultra, 20 threads, Time Profiler)

Raw decode (`raw-u16`); "busy threads" = on-CPU samples / trace span (a lower bound,
since process start and file reading are included).

| Format | ms | busy threads | Where the time goes | Lever |
|---|---:|---:|---|---|
| CR2 lossless JPEG (31 MP) | 20.7 | 11.7 | entropy decode; speculative synchronisation 22%, idle workers 8.5%, bit pump ~30% | fewer, larger chunks |
| CR3 CRX (45 MP) | 80.7 | 4.3 | CRX line decoder 84% (`adapt_k`, line prediction, code reading) | finer-grained units: only 3–5 threads work |
| NEF Huffman (46 MP) | 43.2 | 16.1 | Huffman difference decode ~57%, speculation 6% | per-symbol cost |
| RAF compressed (103 MP) | 104.5 | 14.2 | stripe decoder; inner-loop slice/bounds checks ~10% | stripes unevenly loaded (680–2044 samples per thread) |
| RW2 v8 (100 MP) | 278.5 | 2.9 | `next_diff` 58%, clamping 13% | one strip is the critical path: split below strip level |
| SRW v2 (28 MP) | 182 | 1.0 | sequential bit parser and motion-compensated reconstruction | rows chain through bit positions and earlier rows |
| ARW1 (14 MP) | 98.6 | 1.0 | sequential column-order loop | single bit stream; column-major stores |
| 3FR Hasselblad (61 MP) | 41.5 | 13.8 | Huffman/bit path; speculation 8% | per-symbol cost |
| ARW lossless 61 MP → RGBA8 | 72.8 | 15.7 | develop 54% (row kernel 31%, partly scalar), tile decode 44% | vectorise the row kernel |
| IIQ L (154 MP) | 42.4 | 14.9 | row decode and its 32-bit-word bit pump | — |

Two findings changed code. The IIQ L row decoder became 35% slower on aarch64 after
unrelated code was added to the codec crate: compiled out of line, its bit-pump calls
lost their register allocation. It is now `#[inline(always)]` into its per-row caller
(60.1 → 42.4 ms). And a two-thread parse/reconstruct pipeline for Samsung V2 was 7%
slower than the fused decoder on this machine (7% faster on x86-64), so it was
dropped.
