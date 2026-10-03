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
