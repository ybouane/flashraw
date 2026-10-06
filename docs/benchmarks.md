# Benchmarks

Every number here was measured on real camera files with the tools in this repository, and
can be reproduced with them. FlashRaw is compared with the two widely used open-source RAW
decoders, [LibRaw](https://www.libraw.org/) and [RawSpeed](https://github.com/darktable-org/rawspeed),
built from source on the same machines.

## Headline

Speedup of FlashRaw (other library's time ÷ FlashRaw's time; > 1 = FlashRaw faster), over the
62 files of [`benches/popular-cameras.tsv`](../benches/popular-cameras.tsv) and over all 159
files (popular cameras + one large and one median file of every format/codec pair in
[`benches/benchmark-set.tsv`](../benches/benchmark-set.tsv)).

Each cell: geometric mean / median speedup · files where FlashRaw is faster.

| Comparison | threads | 🐧 popular (62) | 🐧 all (159) | 🍎 popular (62) | 🍎 all (159) |
|---|---|---:|---:|---:|---:|
| decode vs LibRaw | all cores | **6.14×** / 8.26× · 57/59 | **6.30×** / 7.66× · 151/156 | **9.15×** / 11.2× · 59/59 | **8.59×** / 10.9× · 149/156 |
| decode vs LibRaw | 1 | **2.39×** / 2.92× · 57/59 | **2.54×** / 2.51× · 143/156 | **2.01×** / 1.81× · 57/59 | **2.24×** / 1.86× · 139/156 |
| decode vs RawSpeed | all cores | **2.21×** / 2.26× · 47/49 | **2.60×** / 2.30× · 114/126 | **2.00×** / 1.44× · 37/49 | **2.44×** / 2.64× · 94/126 |
| decode vs RawSpeed | 1 | **1.26×** / 1.26× · 39/49 | **1.48×** / 1.25× · 95/126 | 0.86× / 0.84× · 14/49 | **1.08×** / 0.85× · 42/126 |
| RAW → RGB vs LibRaw | all cores | **7.32×** / 8.42× · 59/59 | **7.50×** / 8.45× · 156/156 | **13.5×** / 15.1× · 59/59 | **14.2×** / 16.6× · 156/156 |
| RAW → RGB vs LibRaw | 1 | **1.74×** / 1.81× · 58/59 | **1.67×** / 1.67× · 151/156 | **1.62×** / 1.63× · 58/59 | **1.60×** / 1.62× · 152/156 |

- **Against LibRaw** FlashRaw wins on almost every file, single-threaded too: LibRaw's
  decoders are mostly sequential and its RGB pipeline (`dcraw_process`) is 7–16× slower.
- **Against RawSpeed**, the fastest raw-only decoder, FlashRaw wins with all cores on most
  files because it parallelises the codecs RawSpeed runs on one thread (lossless JPEG,
  Nikon, Pentax, Hasselblad, Olympus, Panasonic v8 …). **On one thread RawSpeed is often as
  fast or faster**, especially on Apple Silicon; see [where FlashRaw is not the fastest](#where-flashraw-is-not-the-fastest).
- Popular-camera tables: [Linux](#popular-cameras-linux-x86-64) · [macOS](#popular-cameras-apple-m1-ultra).
  Per format: [table](#per-format). Raw data: [`benches/results/`](../benches/results/).

## What is measured

| Mode | FlashRaw | LibRaw | RawSpeed |
|---|---|---|---|
| **decode** | `open` + full sensor decode to 16-bit samples (`--output raw-u16`, CPU backend) | `open_buffer()` + `unpack()` | `RawParser::getDecoder()` + `checkSupport()` + `decodeRaw()` + `decodeMetaData()` |
| **RAW → RGB** | decode + bilinear demosaic + as-shot white balance + colour matrix + 8-bit sRGB + orientation (`--output rgba8`) | `unpack()` + `dcraw_process()` with `user_qual=0` (bilinear), `use_camera_wb=1`, `output_bps=8`, sRGB output, + `dcraw_make_mem_image()` | no RGB pipeline |
| **GPU** (macOS only) | `--backend gpu --output rgba8`: CPU decode (or GPU unpack for packed data), GPU develop, frame downloaded to host memory | – | – |

- Input is read into memory before timing (RawSpeed gets a padded copy, as its own reader
  provides). Each value is the **median of 7 timed runs after one warm-up**, in a **fresh
  process per (file, library, mode, thread count)**, so peak RSS is per decode.
- **All cores**: FlashRaw's default thread pool; RawSpeed with OpenMP; LibRaw built **with
  OpenMP**, which parallelises its CR3, Fuji-compressed and Panasonic v8 decoders and parts of
  `dcraw_process`. **One thread**: FlashRaw with `RAYON_NUM_THREADS=1`, RawSpeed with
  `OMP_NUM_THREADS=1`, LibRaw's default (non-OpenMP) build. In every case the decode runs on
  the calling thread.
- The machines also run other jobs: each full run was done twice and, for every measurement,
  the lower of the two medians is kept (both are in the `.jsonl` files as `ms_passes`).
- Builds favour the other libraries where we could: `-O3`, `-march=native` on x86-64 (RawSpeed
  sets it itself), OpenMP enabled. Versions and flags are in the headers of the result files.

### What the outputs are

The libraries do not produce byte-identical results, so the comparison is of equivalent
work, not identical output:

- **decode**: all three decode the full sensor buffer. FlashRaw and LibRaw report it whole,
  masked (optical black) borders included; RawSpeed reports a view cropped to the camera's
  active area. All three also return black/white levels and white balance. FlashRaw applies
  DNG opcode lists by default (bad pixels, gain maps, …); RawSpeed applies a subset of list 1,
  LibRaw none (without the Adobe DNG SDK).
- **RAW → RGB**: FlashRaw and LibRaw both demosaic bilinearly with the camera white balance and
  convert to 8-bit sRGB. LibRaw additionally runs its automatic brightness histogram and
  writes 3-channel RGB with a BT.709-style gamma; FlashRaw writes RGBA with the sRGB curve and
  applies DNG opcode list 3. FlashRaw's demosaic is fused with colour conversion in a single
  multithreaded pass.

## Reproducing

```sh
python3 tools/corpus/fetch.py                        # CC0 corpus (raw.pixls.us)
cargo build --release -p flashraw-cli --features jpegxl     # add gpu for the GPU column

# FlashRaw alone (regression tracking):
python3 tools/bench/run.py benches/benchmark-set.tsv --iterations 5 \
    --out benches/results/<host>.jsonl --markdown benches/results/<host>.md

# Against LibRaw and RawSpeed: see the header of tools/bench/compare.py for how to get and
# build them (nothing is vendored); then
tools/bench/compare.py build --libraw-src <LibRaw> --rawspeed-src <rawspeed> --rawspeed-build <build>
tools/bench/compare.py run benches/popular-cameras.tsv benches/benchmark-set.tsv \
    --out pass1.jsonl [--max-busy 0.15]              # --max-busy: wait for a quiet machine (Linux)
tools/bench/compare.py report pass1.jsonl pass2.jsonl --popular benches/popular-cameras.tsv \
    --markdown benches/results/<host>-compare.md

cargo bench -p flashraw --bench kernels              # micro-benchmarks (criterion)
cargo bench -p flashraw-gpu --bench gpu_pipeline     # GPU (needs an adapter)
```

## Machines and conditions

| | 🐧 Linux x86-64 | 🍎 macOS Apple Silicon |
|---|---|---|
| CPU | AMD Ryzen 7 9700X, 8 cores / 16 threads, 32 MB L3 | Apple M1 Ultra, 16 performance + 4 efficiency cores |
| Memory, OS | 64 GB DDR5, Linux 6.12 (Debian 13) | 64 GB, macOS 27.0 |
| Compilers | rustc 1.99, GCC 14.2 | rustc 1.95, Apple clang 17.0 |
| Threads (all cores) | 16 | 20 |
| Conditions | shared with other jobs (builds, browser tests); before each measurement the script waited until other processes used < 15–30 % of the CPU (they used ≤ 15 % for 99.8 % of the kept values, median 0.9 %); three passes, the third at night; the two lowest passes agree within 1 % (median) / 8 % (90th percentile) | desktop session, otherwise idle (`top`: 90–92 % idle); the two passes agree within 1 % (median) / 5 % (90th percentile) |
| Results | [`linux-x86_64-compare.md`](../benches/results/linux-x86_64-compare.md) | [`macos-arm64-compare.md`](../benches/results/macos-arm64-compare.md) |

## Popular cameras (Linux x86-64)

Times in ms, all cores, **bold** = fastest. Single-thread tables: [result file](../benches/results/linux-x86_64-compare.md).

| Camera | Format · compression | MP | Decode: FlashRaw | LibRaw | RawSpeed | RAW→RGB: FlashRaw | LibRaw |
|---|---|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | CR3 · canon-crx | 46.7 | **56.6** | 58.2 | ❌ | **105.3** | 482.6 |
| Canon EOS R5 (C-RAW) | CR3 · canon-crx | 46.7 | 71.1 | **68.6** | ❌ | **120.4** | 503.4 |
| Canon EOS R5 Mark II (RAW) | CR3 · canon-crx | 47.9 | **57.3** | 59.0 | ❌ | **105.6** | 486.7 |
| Canon EOS R6 Mark II (RAW) | CR3 · canon-crx | 25.5 | **32.7** | 32.8 | ❌ | **58.2** | 263.3 |
| Canon EOS R6 Mark III (RAW) | CR3 · canon-crx | 34.0 | **41.9** | 42.5 | ❌ | **75.9** | 348.5 |
| Canon EOS R7 (RAW) | CR3 · canon-crx | 33.7 | **41.1** | 42.4 | ❌ | **75.6** | 352.0 |
| Canon EOS R8 (RAW) | CR3 · canon-crx | 25.5 | 33.1 | **32.9** | ❌ | **58.1** | 263.1 |
| Canon EOS R50 (RAW) | CR3 · canon-crx | 25.1 | **30.7** | 31.8 | ❌ | **56.3** | 267.6 |
| Canon EOS 90D (RAW) | CR3 · canon-crx | 33.7 | **34.2** | 42.0 | ❌ | **68.0** | 352.1 |
| Canon EOS 5D Mark IV (RAW) | CR2 · lossless-jpeg | 31.3 | **23.9** | 347.2 | 69.0 | **57.4** | 632.6 |
| Canon EOS 6D Mark II (RAW) | CR2 · lossless-jpeg | 27.0 | **19.9** | 264.8 | 82.2 | **49.1** | 519.2 |
| Canon EOS Rebel T7 / 2000D (RAW) | CR2 · lossless-jpeg | 24.7 | **22.9** | 309.1 | 87.3 | **49.9** | 543.7 |
| Nikon Z8 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **47.6** | 424.3 | 178.3 | **96.7** | 856.4 |
| Nikon Z9 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **48.3** | 403.5 | 133.6 | **99.2** | 835.5 |
| Nikon Z6III (lossless) | NEF · nikon-huffman | 24.5 | **27.5** | 228.5 | 85.3 | **53.8** | 460.7 |
| Nikon Zf (lossless) | NEF · nikon-huffman | 24.5 | **26.5** | 217.7 | 68.3 | **57.3** | 451.4 |
| Nikon Z7II (lossless) | NEF · nikon-huffman | 45.7 | **48.5** | 400.6 | 117.5 | **98.4** | 833.6 |
| Nikon Z50 (lossless) | NEF · nikon-huffman | 20.9 | **23.6** | 200.7 | 72.8 | **46.5** | 407.9 |
| Nikon D850 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **48.5** | 397.2 | 118.7 | **98.4** | 828.1 |
| Nikon D750 (14-bit lossless) | NEF · nikon-huffman | 24.3 | **25.9** | 203.6 | 57.5 | **54.9** | 435.5 |
| Sony A7 IV (lossless) | ARW · sony-lossless | 36.7 | **14.9** | 312.5 | 15.6 | **50.9** | 628.9 |
| Sony A7 IV (compressed) | ARW · sony-arw2 | 33.0 | **8.15** | 80.1 | 10.9 | **43.9** | 394.8 |
| Sony A7R V (lossless) | ARW · sony-lossless | 64.7 | **26.7** | 578.1 | 31.4 | **101.7** | 1176.1 |
| Sony A1 (compressed) | ARW · sony-arw2 | 50.2 | **11.5** | 118.0 | 16.7 | **69.4** | 599.0 |
| Sony A7 III (compressed) | ARW · sony-arw2 | 24.3 | **5.78** | 59.5 | 20.0 | **32.4** | 299.0 |
| Sony A7C II (compressed) | ARW · sony-arw2 | 33.0 | **7.96** | 78.4 | 23.8 | **47.5** | 394.5 |
| Sony A9 III (compressed) | ARW · sony-arw2 | 24.3 | **5.74** | 59.9 | 8.23 | **32.8** | 292.2 |
| Sony A6700 (lossless) | ARW · sony-lossless | 30.7 | **14.1** | 276.8 | 15.5 | **42.4** | 522.5 |
| Sony A6400 (compressed) | ARW · sony-arw2 | 24.3 | **5.32** | 52.6 | 7.67 | **32.2** | 284.1 |
| Sony ZV-E10 (compressed) | ARW · sony-arw2 | 24.3 | **5.06** | 53.9 | 8.06 | **28.4** | 285.5 |
| Fujifilm X-T5 (lossless compressed) | RAF · fuji-compressed | 40.9 | **53.3** | 155.8 | 114.6 | **108.7** | 539.8 |
| Fujifilm X-H2 (compressed) | RAF · fuji-compressed | 40.9 | **69.3** | 174.8 | ❌ | **137.7** | 596.3 |
| Fujifilm X-S20 (lossless compressed) | RAF · fuji-compressed | 26.5 | **38.0** | 122.7 | 86.1 | **78.4** | 377.1 |
| Fujifilm X100VI (lossless compressed) | RAF · fuji-compressed | 40.9 | **54.4** | 164.2 | 119.0 | **127.9** | 577.8 |
| Fujifilm X-T4 (uncompressed) | RAF · uncompressed | 26.7 | **3.69** | 16.3 | 4.33 | **42.1** | 267.9 |
| Fujifilm GFX100 II (lossless compressed) | RAF · fuji-compressed | 103.4 | **114.4** | 258.7 | 225.6 | **216.7** | 1256.8 |
| Panasonic Lumix S5II | RW2 · panasonic-v8 | 24.1 | **26.2** | 87.5 | 89.9 | **50.7** | 350.5 |
| Panasonic Lumix GH6 | RW2 · panasonic-v8 | 25.2 | **28.2** | 91.4 | 94.6 | **55.9** | 366.1 |
| Panasonic Lumix G9II | RW2 · panasonic-v8 | 25.1 | **26.3** | 95.8 | 95.7 | **56.0** | 369.1 |
| Panasonic Lumix S5 | RW2 · panasonic-v7 | 24.3 | **4.62** | 9.87 | 7.80 | **32.7** | 271.5 |
| Panasonic Lumix S1R | RW2 · panasonic-v6 | 47.6 | **13.1** | 85.6 | 24.2 | **67.2** | 598.4 |
| Panasonic Lumix GH5 | RW2 · panasonic-v4 | 20.6 | **6.48** | 55.7 | 18.2 | **29.1** | 277.4 |
| OM System OM-1 Mark II | ORF · olympus | 20.4 | **65.0** | 331.8 | 141.4 | **88.6** | 525.3 |
| OM System OM-5 | ORF · olympus | 20.5 | **60.6** | 276.4 | 136.9 | **82.9** | 471.8 |
| Olympus E-M1 Mark III | ORF · olympus | 20.5 | **57.5** | 251.1 | 133.8 | **80.9** | 452.3 |
| Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | **34.0** | 222.1 | 56.0 | **64.2** | 468.8 |
| Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | **44.3** | 318.2 | 90.9 | **85.8** | 655.8 |
| Ricoh GR III | DNG · lossless-jpeg | 24.8 | **22.2** | 264.9 | 67.9 | **49.8** | 496.9 |
| Leica Q2 | DNG · uncompressed | 47.4 | **8.41** | 192.3 | 32.4 | **58.9** | 636.8 |
| Leica SL2 | DNG · uncompressed | 47.4 | **8.39** | 191.8 | 32.2 | **61.7** | 640.3 |
| Leica M10-R | DNG · lossless-jpeg | 41.0 | **42.6** | 437.1 | 151.3 | **88.0** | 824.9 |
| Hasselblad X2D 100C (compressed) | FFF · hasselblad | 105.3 | **96.1** | 852.5 | 489.9 | **213.0** | 1820.6 |
| Phase One IQ4 150MP (IIQ L) | IIQ · phaseone | 154.0 | **56.8** | 653.4 | ❌ | **224.1** | 2461.7 |
| Sigma fp L | DNG · lossless-jpeg | 60.5 | **18.4** | 622.2 | 35.2 | **133.4** | 1195.2 |
| Apple iPhone 12 Pro (ProRAW) | DNG · lossless-jpeg | 12.2 | **20.3** | 318.9 | ❌ | **38.3** | 383.6 |
| Google Pixel 8 Pro | DNG · lossless-jpeg | 50.1 | **14.6** | 526.7 | 30.9 | **119.5** | 1097.7 |
| Google Pixel 6 Pro | DNG · lossless-jpeg | 12.5 | **3.37** | 131.9 | 12.0 | **28.8** | 254.3 |
| Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 49.9 | **103.0** | 1271.2 | 379.2 | **258.7** | 1509.0 |
| Samsung Galaxy S22 (JPEG XL) | DNG · jpeg-xl | 12.0 | **235.4** | ❌ | ❌ | **285.7** | ❌ |
| DJI Phantom 4 Pro | DNG · uncompressed | 20.0 | **2.53** | 10.4 | 3.14 | **78.6** | 201.9 |
| GoPro HERO11 Black (GPR) | DNG · gopro-vc5 | 27.1 | 45.9 | ❌ | **26.5** | **99.3** | ❌ |
| GoPro HERO10 Black (GPR) | DNG · gopro-vc5 | 23.3 | 41.5 | ❌ | **23.2** | **85.4** | ❌ |

## Popular cameras (Apple M1 Ultra)

Times in ms, all cores, **bold** = fastest. Single-thread tables: [result file](../benches/results/macos-arm64-compare.md).

| Camera | Format · compression | MP | Decode: FlashRaw | LibRaw | RawSpeed | RAW→RGB: FlashRaw | LibRaw | FlashRaw GPU |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | CR3 · canon-crx | 46.7 | **80.4** | 136.6 | ❌ | **110.4** | 731.8 | 97.3 |
| Canon EOS R5 (C-RAW) | CR3 · canon-crx | 46.7 | **100.4** | 139.9 | ❌ | **131.0** | 733.4 | 117.6 |
| Canon EOS R5 Mark II (RAW) | CR3 · canon-crx | 47.9 | **82.9** | 140.3 | ❌ | **112.8** | 729.4 | 100.4 |
| Canon EOS R6 Mark II (RAW) | CR3 · canon-crx | 25.5 | **45.5** | 76.4 | ❌ | **62.2** | 406.4 | 56.6 |
| Canon EOS R6 Mark III (RAW) | CR3 · canon-crx | 34.0 | **59.4** | 100.3 | ❌ | **81.3** | 534.9 | 72.9 |
| Canon EOS R7 (RAW) | CR3 · canon-crx | 33.7 | **59.5** | 99.7 | ❌ | **81.5** | 537.7 | 72.5 |
| Canon EOS R8 (RAW) | CR3 · canon-crx | 25.5 | **45.2** | 75.7 | ❌ | **61.5** | 405.7 | 55.6 |
| Canon EOS R50 (RAW) | CR3 · canon-crx | 25.1 | **44.6** | 74.9 | ❌ | **61.1** | 404.2 | 54.5 |
| Canon EOS 90D (RAW) | CR3 · canon-crx | 33.7 | **31.3** | 99.6 | ❌ | **53.6** | 538.9 | 44.3 |
| Canon EOS 5D Mark IV (RAW) | CR2 · lossless-jpeg | 31.3 | **19.3** | 423.6 | 107.1 | **39.4** | 834.0 | 30.2 |
| Canon EOS 6D Mark II (RAW) | CR2 · lossless-jpeg | 27.0 | **17.1** | 380.1 | 117.1 | **34.8** | 745.0 | 28.2 |
| Canon EOS Rebel T7 / 2000D (RAW) | CR2 · lossless-jpeg | 24.7 | **18.7** | 394.0 | 119.6 | **35.6** | 723.2 | 28.7 |
| Nikon Z8 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **43.9** | 540.4 | 222.4 | **73.8** | 1145.2 | 62.6 |
| Nikon Z9 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **44.1** | 500.0 | 181.8 | **75.0** | 1106.4 | 62.6 |
| Nikon Z6III (lossless) | NEF · nikon-huffman | 24.5 | **24.9** | 279.4 | 108.2 | **41.6** | 622.0 | 36.6 |
| Nikon Zf (lossless) | NEF · nikon-huffman | 24.5 | **24.6** | 267.8 | 97.9 | **41.0** | 601.4 | 35.3 |
| Nikon Z7II (lossless) | NEF · nikon-huffman | 45.7 | **44.3** | 480.4 | 172.5 | **74.9** | 1093.5 | 64.1 |
| Nikon Z50 (lossless) | NEF · nikon-huffman | 20.9 | **22.0** | 244.5 | 93.6 | **36.4** | 536.2 | 31.7 |
| Nikon D850 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **45.0** | 484.7 | 170.2 | **75.8** | 1080.6 | 63.1 |
| Nikon D750 (14-bit lossless) | NEF · nikon-huffman | 24.3 | **24.6** | 253.1 | 83.0 | **42.3** | 588.3 | 36.2 |
| Sony A7 IV (lossless) | ARW · sony-lossless | 36.7 | 18.2 | 416.7 | **11.2** | **41.6** | 859.9 | 31.1 |
| Sony A7 IV (compressed) | ARW · sony-arw2 | 33.0 | **5.81** | 84.5 | 8.23 | **28.3** | 533.0 | 16.6 |
| Sony A7R V (lossless) | ARW · sony-lossless | 64.7 | 32.4 | 758.9 | **22.9** | **71.6** | 1559.1 | 53.8 |
| Sony A1 (compressed) | ARW · sony-arw2 | 50.2 | **8.15** | 127.8 | 13.3 | **41.7** | 791.1 | 23.5 |
| Sony A7 III (compressed) | ARW · sony-arw2 | 24.3 | **4.40** | 62.1 | 6.18 | **21.3** | 394.3 | 12.9 |
| Sony A7C II (compressed) | ARW · sony-arw2 | 33.0 | **5.77** | 84.1 | 7.92 | **28.4** | 530.1 | 16.7 |
| Sony A9 III (compressed) | ARW · sony-arw2 | 24.3 | **4.43** | 61.8 | 7.19 | **20.5** | 396.3 | 12.9 |
| Sony A6700 (lossless) | ARW · sony-lossless | 30.7 | 16.9 | 365.1 | **11.5** | **34.5** | 721.7 | 27.1 |
| Sony A6400 (compressed) | ARW · sony-arw2 | 24.3 | **4.34** | 63.4 | 5.74 | **21.0** | 397.1 | 13.4 |
| Sony ZV-E10 (compressed) | ARW · sony-arw2 | 24.3 | **4.28** | 63.5 | 6.18 | **18.7** | 394.6 | 12.3 |
| Fujifilm X-T5 (lossless compressed) | RAF · fuji-compressed | 40.9 | **35.3** | 150.2 | 44.2 | **74.7** | 687.6 | 51.8 |
| Fujifilm X-H2 (compressed) | RAF · fuji-compressed | 40.9 | **46.3** | 175.0 | ❌ | **94.0** | 878.6 | 63.6 |
| Fujifilm X-S20 (lossless compressed) | RAF · fuji-compressed | 26.5 | **28.5** | 127.6 | 36.2 | **55.0** | 483.0 | 40.1 |
| Fujifilm X100VI (lossless compressed) | RAF · fuji-compressed | 40.9 | **34.9** | 153.5 | 44.3 | **83.0** | 897.3 | 54.4 |
| Fujifilm X-T4 (uncompressed) | RAF · uncompressed | 26.7 | 1.58 | 16.5 | **1.36** | **27.9** | 376.0 | 9.06 |
| Fujifilm GFX100 II (lossless compressed) | RAF · fuji-compressed | 103.4 | **58.7** | 263.7 | 71.3 | **124.1** | 1588.9 | 90.8 |
| Panasonic Lumix S5II | RW2 · panasonic-v8 | 24.1 | **14.2** | 129.7 | 62.8 | **30.7** | 470.8 | 24.1 |
| Panasonic Lumix GH6 | RW2 · panasonic-v8 | 25.2 | **14.9** | 130.9 | 63.7 | **32.5** | 491.4 | 25.6 |
| Panasonic Lumix G9II | RW2 · panasonic-v8 | 25.1 | **14.3** | 137.5 | 57.0 | **31.5** | 496.1 | 23.9 |
| Panasonic Lumix S5 | RW2 · panasonic-v7 | 24.3 | 2.46 | 8.21 | **0.61** | **18.9** | 352.0 | 11.0 |
| Panasonic Lumix S1R | RW2 · panasonic-v6 | 47.6 | 8.52 | 119.6 | **7.62** | **39.7** | 797.5 | 23.4 |
| Panasonic Lumix GH5 | RW2 · panasonic-v4 | 20.6 | **4.59** | 88.5 | 4.85 | **19.0** | 380.8 | 12.6 |
| OM System OM-1 Mark II | ORF · olympus | 20.4 | **113.0** | 384.0 | 159.0 | **128.5** | 674.1 | 122.4 |
| OM System OM-5 | ORF · olympus | 20.5 | **122.1** | 343.9 | 152.6 | **138.3** | 633.4 | 132.5 |
| Olympus E-M1 Mark III | ORF · olympus | 20.5 | **121.7** | 337.9 | 146.5 | **137.6** | 624.4 | 132.3 |
| Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | **29.4** | 195.4 | 83.3 | **47.1** | 557.3 | 40.6 |
| Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | **37.4** | 276.9 | 128.0 | **61.6** | 776.9 | 53.7 |
| Ricoh GR III | DNG · lossless-jpeg | 24.8 | **18.2** | 311.9 | 94.1 | **34.5** | 649.3 | 28.2 |
| Leica Q2 | DNG · uncompressed | 47.4 | **4.22** | 286.5 | 48.9 | **35.5** | 910.0 | CPU¹ |
| Leica SL2 | DNG · uncompressed | 47.4 | **4.23** | 287.9 | 47.6 | **35.3** | 915.0 | CPU¹ |
| Leica M10-R | DNG · lossless-jpeg | 41.0 | **28.5** | 543.3 | 194.9 | **55.8** | 1092.4 | 43.8 |
| Hasselblad X2D 100C (compressed) | FFF · hasselblad | 105.3 | **75.5** | 991.4 | 442.3 | **146.6** | 2315.7 | 118.8 |
| Phase One IQ4 150MP (IIQ L) | IIQ · phaseone | 154.0 | **40.4** | 1010.5 | ❌ | **139.2** | 4360.4 | 85.2 |
| Sigma fp L | DNG · lossless-jpeg | 60.5 | 27.6 | 751.8 | **21.1** | **85.1** | 1544.0 | CPU¹ |
| Apple iPhone 12 Pro (ProRAW) | DNG · lossless-jpeg | 12.2 | **15.2** | 460.9 | ❌ | **23.8** | 586.7 | 20.8 |
| Google Pixel 8 Pro | DNG · lossless-jpeg | 50.1 | 21.8 | 636.1 | **16.2** | **71.3** | 1422.0 | CPU¹ |
| Google Pixel 6 Pro | DNG · lossless-jpeg | 12.5 | 6.17 | 158.2 | **4.66** | **21.3** | 344.0 | CPU¹ |
| Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 49.9 | **127.7** | 1864.1 | 534.9 | **196.1** | 2150.5 | CPU¹ |
| Samsung Galaxy S22 (JPEG XL) | DNG · jpeg-xl | 12.0 | **271.1** | ❌ | ❌ | **292.8** | ❌ | CPU¹ |
| DJI Phantom 4 Pro | DNG · uncompressed | 20.0 | 1.21 | 16.6 | **1.15** | **31.9** | 294.4 | CPU¹ |
| GoPro HERO11 Black (GPR) | DNG · gopro-vc5 | 27.1 | 38.5 | ❌ | **18.2** | **66.3** | ❌ | CPU¹ |
| GoPro HERO10 Black (GPR) | DNG · gopro-vc5 | 23.3 | 34.8 | ❌ | **17.4** | **59.6** | ❌ | CPU¹ |

FlashRaw GPU = `--backend gpu --output rgba8`: CPU decode or GPU unpack, GPU develop, frame downloaded to host memory. ¹ FlashRaw's GPU pipeline does not handle this file's processing yet and ran it on the CPU (same result, CPU time).

## Per format

Geometric mean speedup of FlashRaw per (format, codec) over the files of both lists, all
cores. ❌ = the other library decoded none of these files (LibRaw and RawSpeed were built
without the optional libjpeg, Adobe DNG SDK and GoPro SDK).

| Format | Codec | files | 🐧 decode vs LibRaw | 🐧 decode vs RawSpeed | 🐧 RGB vs LibRaw | 🍎 decode vs LibRaw | 🍎 decode vs RawSpeed | 🍎 RGB vs LibRaw |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| 3FR | hasselblad | 2 | **8.15×** | **4.86×** | **8.56×** | **12.6×** | **5.85×** | **15.8×** |
| 3FR | uncompressed | 2 | **1.42×** | **1.23×** | **7.99×** | 0.79× | 0.52× | **17.5×** |
| ARW | other | 2 | **3.73×** | ❌ | **5.16×** | **12.5×** | ❌ | **15.0×** |
| ARW | sony-arw1 | 2 | **14.9×** | **13.3×** | **10.3×** | **18.0×** | **9.75×** | **19.0×** |
| ARW | sony-arw2 | 9 | **10.2×** | **1.76×** | **8.98×** | **14.8×** | **1.47×** | **19.2×** |
| ARW | sony-lossless | 4 | **13.8×** | **1.11×** | **9.30×** | **18.6×** | 0.67× | **17.5×** |
| ARW | uncompressed | 2 | **3.61×** | 0.63× | **5.64×** | **8.23×** | 0.36× | **12.8×** |
| CR2 | canon-sraw | 2 | **8.38×** | **2.08×** | **7.00×** | **17.9×** | **4.40×** | **14.8×** |
| CR2 | lossless-jpeg | 5 | **13.8×** | **3.41×** | **10.8×** | **21.2×** | **6.00×** | **20.7×** |
| CR3 | canon-crx | 11 | **1.03×** | ❌ | **4.57×** | **1.74×** | ❌ | **6.66×** |
| CRW | canon-crw | 2 | **3.31×** | **2.22×** | **4.72×** | **2.74×** | **1.97×** | **5.17×** |
| CRW | uncompressed | 1 | **7.19×** | ❌ | **10.4×** | **7.16×** | ❌ | **17.3×** |
| DCR | kodak-65000 | 2 | **4.86×** | **4.63×** | **6.47×** | **2.64×** | **3.52×** | **7.78×** |
| DCS | lossless-jpeg | 1 | **8.52×** | **1.95×** | **10.4×** | **12.0×** | **3.16×** | **15.5×** |
| DCS | uncompressed | 2 | **5.30×** | **21.6×** | **9.67×** | **4.35×** | **35.1×** | **21.1×** |
| DNG | deflate | 2 | **10.1×** | **1.16×** | **10.3×** | **14.0×** | 0.98× | **17.0×** |
| DNG | gopro-vc5 | 2 | ❌ | 0.57× | ❌ | ❌ | 0.49× | ❌ |
| DNG | jpeg-xl | 1 | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| DNG | lossless-jpeg | 9 | **17.6×** | **2.90×** | **8.81×** | **21.4×** | **2.56×** | **17.4×** |
| DNG | uncompressed | 5 | **8.39×** | **3.33×** | **5.50×** | **27.8×** | **4.50×** | **16.3×** |
| ERF | uncompressed | 2 | **3.98×** | 0.84× | **6.66×** | **8.77×** | **1.43×** | **16.1×** |
| FFF | hasselblad | 2 | **8.78×** | **5.13×** | **8.65×** | **12.9×** | **5.80×** | **15.7×** |
| IIQ | phaseone | 3 | **11.1×** | **19.8×** | **10.5×** | **23.2×** | **28.5×** | **30.6×** |
| IIQ | phaseone-s | 2 | **6.31×** | ❌ | **8.24×** | **14.0×** | ❌ | **28.2×** |
| IIQ | uncompressed | 1 | **1.28×** | ❌ | **9.89×** | 0.88× | ❌ | **36.3×** |
| KDC | kodak-dc120 | 1 | **1.75×** | ❌ | **13.4×** | **1.98×** | ❌ | **13.6×** |
| KDC | other | 1 | **1.19×** | ❌ | **3.95×** | **1.47×** | ❌ | **2.87×** |
| KDC | uncompressed | 2 | **46.5×** | **8.02×** | **8.98×** | **51.9×** | **10.4×** | **22.5×** |
| MEF | uncompressed | 1 | **19.6×** | **3.92×** | **9.28×** | **56.4×** | **11.6×** | **26.4×** |
| MOS | leaf | 2 | **12.1×** | ❌ | **9.24×** | **12.2×** | ❌ | **15.5×** |
| MOS | uncompressed | 1 | **5.36×** | **4.18×** | **7.02×** | **11.9×** | **11.0×** | **20.9×** |
| MRW | minolta-packed | 2 | **79.6×** | **13.7×** | **9.72×** | **50.2×** | **10.2×** | **21.9×** |
| MRW | uncompressed | 2 | **18.2×** | **17.0×** | **8.56×** | **9.56×** | **9.12×** | **18.4×** |
| NEF | nikon-huffman | 9 | **8.37×** | **2.80×** | **8.47×** | **11.1×** | **4.09×** | **14.7×** |
| NEF | uncompressed | 2 | **6.13×** | **2.51×** | **8.54×** | **6.37×** | **2.79×** | **19.8×** |
| NRW | uncompressed | 2 | **2.49×** | 0.96× | **7.92×** | **3.27×** | **1.40×** | **17.8×** |
| ORF | olympus | 5 | **4.62×** | **2.21×** | **5.75×** | **2.97×** | **1.27×** | **4.75×** |
| ORF | uncompressed | 2 | **11.3×** | **2.52×** | **8.92×** | **15.9×** | **2.61×** | **19.1×** |
| PEF | pentax-huffman | 4 | **7.00×** | **1.93×** | **7.57×** | **7.17×** | **3.16×** | **12.2×** |
| PEF | uncompressed | 2 | **32.3×** | **12.8×** | **8.75×** | **19.2×** | **8.18×** | **19.1×** |
| RAF | fuji-compressed | 7 | **2.77×** | **2.11×** | **4.93×** | **4.34×** | **1.24×** | **10.3×** |
| RAF | uncompressed | 3 | **3.03×** | **1.02×** | **6.93×** | **4.34×** | 0.82× | **15.1×** |
| RAW | uncompressed | 2 | **3.05×** | **5.14×** | **10.0×** | **3.83×** | **11.8×** | **21.4×** |
| RW2 | panasonic-v4 | 3 | **8.77×** | **1.41×** | **9.45×** | **19.4×** | **1.06×** | **20.2×** |
| RW2 | panasonic-v5 | 2 | **7.80×** | 0.69× | **8.40×** | **6.42×** | 0.62× | **17.5×** |
| RW2 | panasonic-v6 | 2 | **6.49×** | **1.49×** | **9.02×** | **13.6×** | 0.93× | **19.6×** |
| RW2 | panasonic-v7 | 3 | **2.21×** | **1.12×** | **8.64×** | **3.61×** | 0.25× | **19.1×** |
| RW2 | panasonic-v8 | 5 | **3.20×** | **3.31×** | **6.68×** | **8.60×** | **3.93×** | **15.3×** |
| RW2 | uncompressed | 2 | **8.83×** | **5.28×** | **8.52×** | **7.74×** | 0.75× | **20.9×** |
| SRW | samsung-v0 | 2 | **10.8×** | **5.10×** | **9.17×** | **15.5×** | **5.06×** | **18.1×** |
| SRW | samsung-v1 | 2 | **4.26×** | **2.00×** | **5.70×** | **2.69×** | **1.07×** | **5.26×** |
| SRW | samsung-v2 | 2 | **1.43×** | **1.05×** | **2.78×** | **1.76×** | 0.91× | **3.86×** |
| SRW | uncompressed | 2 | **20.9×** | **4.38×** | **9.71×** | **48.5×** | **9.81×** | **21.8×** |
| X3F | sigma-x3f | 2 | **10.7×** | ❌ | **10.3×** | **6.29×** | ❌ | **7.06×** |
| other | kodak-ycbcr | 2 | **4.89×** | ❌ | **4.74×** | **8.76×** | ❌ | **8.38×** |
| other | uncompressed | 2 | **6.64×** | **4.44×** | **8.38×** | **8.40×** | **11.8×** | **18.0×** |

## Where FlashRaw is not the fastest

Honest list, from the tables above:

- **RAW → RGB with all cores: never.** FlashRaw is faster than LibRaw on all 156 files both
  can decode, on both machines. On one thread LibRaw is faster on a few files (5 on Linux, 4 on
  macOS), mostly large uncompressed DNG/ARW (DJI Phantom 4 Pro, Sony pixel-shift ARQ) and
  Kodak YCbCr files: up to 1.6×.
- **Decode with all cores, Linux: 16 of 159 files.**
  - Canon CR3: close to LibRaw's OpenMP build, which also decodes CR3 in parallel. FlashRaw is
    1–4 % slower on 4 of the 13 CR3 files and up to 1.2× faster on the others (on the M1
    Ultra it is 1.7× faster on all of them).
  - GoPro GPR (VC-5): RawSpeed 1.7–1.8× faster.
  - Sony pixel-shift ARQ (4 × 42 MP uncompressed): RawSpeed 3.1× faster.
  - Small uncompressed files that take 1–4 ms (Epson R-D1, Nikon Coolpix A1000, Fujifilm
    X-E2, GITUP), and Panasonic v5/v7 (GH5S, G9 high-res, S5): RawSpeed or LibRaw up to 2× faster.
- **Decode with all cores, Apple M1 Ultra: 34 of 159 files**, all against RawSpeed (LibRaw
  also on 7 uncompressed files):
  - Memory-bound uncompressed and packed data (3FR, RAF, NEF, NRW, ARW, RW2 uncompressed,
    Panasonic v5–v7): RawSpeed 1.1–2× faster, and **4× on Panasonic v7** (Lumix S5). These take
    1–10 ms in every library; FlashRaw caps such copy kernels at a few threads, which is right on
    the Ryzen but leaves bandwidth unused on the M1 Ultra.
  - Sony lossless ARW: RawSpeed 1.4–1.6× faster. Tiled lossless-JPEG DNG from phones and the
    Sigma fp L: RawSpeed 1.3× faster. GoPro GPR: RawSpeed 2× faster. Samsung NX1/NX500 (V2):
    RawSpeed 1.1× faster (a strictly sequential codec). Deflate DNG: a tie.
- **One thread**: FlashRaw beats LibRaw on 143 (Linux) and 139 (macOS) of 156 files. Against
  RawSpeed it is faster on 95 of 126 files on Linux, but **only 42 of 126 on the M1 Ultra**,
  where RawSpeed's single-threaded decoders are 1.2× faster on the popular cameras (geometric
  mean). FlashRaw's lead with all cores comes from parallelising codecs that RawSpeed runs on
  one thread (lossless JPEG, Nikon, Pentax, Hasselblad, Olympus, Panasonic v8, …).
- **Formats the others do not decode at all** are not in these counts: Canon CR3, Phase One
  IIQ S and 150 MP, Sigma X3F, Sony ARW lossy "other" (RawSpeed); GoPro GPR and JPEG XL DNG
  (LibRaw, without optional SDKs); JPEG XL DNG, iPhone ProRAW (RawSpeed).

## Memory

Peak RSS of the whole process (input file held in memory included) is in the "All files"
tables of the result files. Ratio FlashRaw ÷ other library, all cores:

| | decode vs LibRaw | decode vs RawSpeed | RAW → RGB vs LibRaw |
|---|---:|---:|---:|
| Linux: median (90th percentile) | 1.04 (2.31) | 0.99 (2.20) | **0.55** (0.82) |
| macOS: median (90th percentile) | 1.14 (2.94) | 1.05 (2.86) | **0.62** (1.04) |

A decode needs about the same memory everywhere (input + 16-bit output). FlashRaw's
speculative parallel decoders hold extra per-thread buffers, which is what the 90th
percentile shows: e.g. Canon CR3 (EOS R5: 278 MB vs 122 MB for LibRaw on Linux) and the Sony
pixel-shift ARQ (657 MB vs 415 MB for RawSpeed). For RAW → RGB FlashRaw typically needs half
of LibRaw's memory, which keeps a 4-channel 16-bit working image. The exception are DNGs with
an opcode list 3, which FlashRaw applies to the demosaiced image in floating point and LibRaw
skips: the DJI Phantom 4 Pro file (`FixVignetteRadial`) needs 692 MB vs 291 MB, and 73 ms of
post-processing instead of ~30 ms (`--no-opcodes`: 166 MB).

## FlashRaw alone

Results of `tools/bench/run.py` (all formats of the benchmark set, both modes):
[`linux-x86_64.md`](../benches/results/linux-x86_64.md),
[`macos-arm64.md`](../benches/results/macos-arm64.md). Median throughput over the set:

| | decode | RAW → RGBA8 |
|---|---:|---:|
| Linux, Ryzen 7 9700X (16 threads, CPU) | 2,061 MP/s | 583 MP/s |
| macOS, M1 Ultra (20 threads, `Backend::Auto`: GPU develop where it wins) | 3,148 MP/s | 1,122 MP/s |

Uncompressed and packed formats run at memory bandwidth; entropy-coded formats are
bound by their entropy decoder (parallel over tiles, strips or blocks where the format
has them, and by speculation over single streams otherwise — see
[architecture.md](architecture.md)). Single sequential streams are decoded in parallel by
speculation (lossless JPEG, Hasselblad, Nikon, Pentax, Panasonic v8 strips, Sony ARW1).
Samsung V2 cannot be: rows start wherever the previous one ended and are predicted from
earlier rows, so it runs on one thread with a staged block decoder.

### Kernels (criterion, `cargo bench -p flashraw --bench kernels`)

| Kernel | Throughput |
|---|---:|
| 12-bit MSB unpack, scalar reference | 1.05 G samples/s |
| 12-bit MSB unpack, SSE4.1 | 8.8 G samples/s |
| 16-bit LE unpack (fast / SIMD) | 35–37 G samples/s |
| Lossless JPEG decode, 2 components (synthetic) | 617 M samples/s |

### Thread scaling and memory-bound kernels

Unpack/copy kernels are memory-bound: on the Ryzen they peak at ~4 threads and get
1.7–1.9× slower at 16 threads (memory-controller and page-fault contention), e.g. a
24.6 MP 14-bit DNG unpacks in 7.8 ms (1 thread), 3.0 ms (4), 5.2 ms (16). FlashRaw caps
such kernels with `par::memory_bound_workers()` (threads/4, clamped to 2–8) while
entropy decoders and the develop pass use all cores. Large output buffers are
advised as transparent huge pages (`MADV_HUGEPAGE`), cutting page faults ~500×.

A single-threaded decode (`threads: 1`, `--threads 1`) runs on the calling thread, exactly like
`RAYON_NUM_THREADS=1`. (It used to run on a dedicated one-worker pool, whose thread macOS
could schedule on an efficiency core: about 3× slower on the M1 Ultra in one benchmark round.)
Explicit pools (`threads: n`) run their workers with the user-initiated quality of service on
Apple platforms.

## GPU (Apple M1 Ultra, Metal)

The "FlashRaw GPU" column of the macOS tables is end to end through the CLI (per-process
timing, frame downloaded to host memory). The GPU develop pass replaces the CPU one, so it
helps most where development dominates: on the popular cameras RAW → RGB is 1.31× faster with
the GPU than with 20 CPU threads (geometric mean over the 52 files where it ran; up to 3.08×
for packed and uncompressed data) and 17.3× faster than LibRaw. Some files fall back to the CPU
(marked CPU¹). Resident output (no download) and per-kernel timings are in
[gpu.md](gpu.md) (`cargo bench -p flashraw-gpu --bench gpu_pipeline`, median of 7):

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
`Backend::Auto` picks the faster side. No discrete GPU was available for measurement.

## WASM

From [wasm.md](wasm.md): single-threaded WASM runs lossless JPEG at 1.2–1.6× native
single-thread time and the RAW → RGBA8 pipeline at 1.2–1.4×; the threaded build lands at
1.3–2.2× native all-core time on 46–103 MP files (M1 Ultra), and WebGPU develops a 42 MP
uncompressed ARW to a resident texture in 15 ms. SIMD128 halves packed unpack time vs the
non-SIMD build.

Bundle size with all decoders (wasm-opt -O3):

| Binary | Raw | gzip | brotli |
|---|---:|---:|---:|
| `flashraw_simd.wasm` | 1978 KiB | 678 KiB | 507 KiB |
| `flashraw.wasm` (fallback) | 1946 KiB | 666 KiB | 497 KiB |

## Performance regression detection

`tools/bench/run.py ... --baseline <old.jsonl> --tolerance 15` prints per-file
before/after and fails on slowdowns beyond the tolerance (used by `perf.yml`, which
benchmarks the PR and `main` on the same runner).
