# FlashRaw compared — macOS Apple Silicon (M1 Ultra)

| | |
|---|---|
| Machine | Apple M1 Ultra (16 performance + 4 efficiency cores), 64 GB, macOS 27.0 |
| Date | 2026-11-16 |
| Files | `benches/popular-cameras.tsv` (62 files, every library) + `benches/benchmark-set.tsv` (159 files in all; FlashRaw, LibRaw and RawSpeed) |
| FlashRaw | 0.3.1 (commit 61c32fb), `cargo build --release -p flashraw-cli --features gpu,jpegxl`, rustc 1.95.0; CPU backend (`--backend simd`, NEON), 20 threads / `RAYON_NUM_THREADS=1`; GPU column: Metal |
| LibRaw | 0.22.0 (4f0144031c4a), `Makefile.dist`, `-O3 -DUSE_ZLIB -DUSE_X3FTOOLS -DUSE_6BY9RPI`, Apple clang 21.0.0; all cores = OpenMP build (LLVM libomp 22.1.5, 20 threads), 1 thread = default build |
| LibRaw + Little CMS | the LibRaw builds above + Little CMS 2.19.1 (`-O3`, static); transform over 32-row blocks with OpenMP (all cores) or one call (1 thread) |
| RawSpeed | develop c835b05aecfa, CMake Release, `WITH_OPENMP=ON` (libomp, 20 threads; `OMP_NUM_THREADS=1` for one thread), `WITH_JPEG=OFF`, Apple clang 21.0.0 |
| rawler | 0.8.0 (crates.io), `cargo build --release`, rustc 1.95.0, rayon (20 threads / `RAYON_NUM_THREADS=1`) |
| rawpy | 0.27.1 wheel (LibRaw 0.22.1 inside; the macOS wheels have no OpenMP), CPython 3.9.6 |
| dcraw | 9.28, `cc -O3 -DNODEPS`, Apple clang 21.0.0; one thread, whole process per run |
| Statistic | median of 7 timed runs after 1 warm-up, fresh process per (file, library, mode, threads); two full passes of the popular cameras, one of the set; lowest median kept (the passes agree within 0.7 % median, 4.0 % 90th percentile) |
| Load | Desktop session with background services (storage indexing, window server) that could not be stopped; load average median 7.2 during the runs (the benchmark's own 20 threads included), up to 62 at times. The other libraries' times are within 1 % of the 17 October run (median), so the noise did not move the comparison |

Times in ms (median of the timed runs; lower is better); memory = peak RSS in MB. ❌ = the library failed or does not support the file; – = not measured. Speedup = other library's time ÷ FlashRaw's time; memory ratio = other library's peak RSS ÷ FlashRaw's (> 1 = FlashRaw uses less). **Bold** = fastest. ² dcraw has no multithreading: its one-thread time is used in the all-cores columns too.

### Summary: popular cameras (62 files)

| Comparison | threads | files | geomean speedup | median speedup | FlashRaw faster | median memory ratio |
|---|---|---:|---:|---:|---:|---:|
| decode vs LibRaw | all | 59 | **11.4×** | 19.2× | 59/59 | 0.81 |
| decode vs LibRaw | 1 | 59 | **2.50×** | 2.08× | 59/59 | 0.94 |
| decode vs RawSpeed | all | 49 | **2.72×** | 1.98× | 43/49 | 0.93 |
| decode vs RawSpeed | 1 | 49 | **1.13×** | 1.06× | 31/49 | 0.98 |
| decode vs rawler | all | 59 | **3.89×** | 2.89× | 58/59 | 1.53 |
| decode vs rawler | 1 | 59 | **1.92×** | 1.99× | 58/59 | 1.61 |
| decode vs rawpy | all | 59 | **17.4×** | 21.1× | 59/59 | 1.00 |
| decode vs rawpy | 1 | 59 | **2.60×** | 2.05× | 59/59 | 1.11 |
| decode vs dcraw | all | 42 | **39.7×** | 35.2× | 42/42 | 1.92 |
| decode vs dcraw | 1 | 42 | **5.99×** | 5.08× | 42/42 | 2.75 |
| RAW→RGB vs LibRaw | all | 59 | **17.0×** | 22.1× | 59/59 | 1.48 |
| RAW→RGB vs LibRaw | 1 | 59 | **2.04×** | 2.06× | 59/59 | 1.88 |
| RAW→sRGB vs LibRaw + Little CMS | all | 59 | **23.6×** | 29.4× | 59/59 | 2.21 |
| RAW→sRGB vs LibRaw + Little CMS | 1 | 59 | **12.8×** | 12.8× | 59/59 | 2.79 |
| RAW→RGB vs rawler | all | 59 | **7.00×** | 7.24× | 58/59 | 4.53 |
| RAW→RGB vs rawler | 1 | 59 | **4.22×** | 4.44× | 58/59 | 5.13 |
| RAW→RGB vs rawpy | all | 59 | **20.9×** | 25.2× | 59/59 | 1.56 |
| RAW→RGB vs rawpy | 1 | 59 | **2.09×** | 2.09× | 59/59 | 1.97 |
| RAW→RGB vs dcraw | all | 42 | **24.6×** | 27.9× | 42/42 | 0.99 |
| RAW→RGB vs dcraw | 1 | 42 | **2.35×** | 2.47× | 42/42 | 1.32 |
| RAW→RGB, FlashRaw GPU vs LibRaw | all | 52 | **19.7×** | 25.3× | 52/52 | 0.90 |

### Summary: all files (159; rawler, rawpy, dcraw and Little CMS were measured on the popular cameras only)

| Comparison | threads | files | geomean speedup | median speedup | FlashRaw faster | median memory ratio |
|---|---|---:|---:|---:|---:|---:|
| decode vs LibRaw | all | 156 | **10.6×** | 14.9× | 150/156 | 0.89 |
| decode vs LibRaw | 1 | 156 | **3.08×** | 2.24× | 148/156 | 0.94 |
| decode vs RawSpeed | all | 126 | **3.04×** | 3.53× | 106/126 | 0.94 |
| decode vs RawSpeed | 1 | 126 | **1.44×** | 1.12× | 79/126 | 0.98 |
| decode vs rawler | all | 59 | **3.89×** | 2.89× | 58/59 | 1.53 |
| decode vs rawler | 1 | 59 | **1.92×** | 1.99× | 58/59 | 1.61 |
| decode vs rawpy | all | 59 | **17.4×** | 21.1× | 59/59 | 1.00 |
| decode vs rawpy | 1 | 59 | **2.60×** | 2.05× | 59/59 | 1.11 |
| decode vs dcraw | all | 42 | **39.7×** | 35.2× | 42/42 | 1.92 |
| decode vs dcraw | 1 | 42 | **5.99×** | 5.08× | 42/42 | 2.75 |
| RAW→RGB vs LibRaw | all | 156 | **16.8×** | 20.9× | 156/156 | 1.67 |
| RAW→RGB vs LibRaw | 1 | 156 | **2.06×** | 2.09× | 155/156 | 1.83 |
| RAW→sRGB vs LibRaw + Little CMS | all | 59 | **23.6×** | 29.4× | 59/59 | 2.21 |
| RAW→sRGB vs LibRaw + Little CMS | 1 | 59 | **12.8×** | 12.8× | 59/59 | 2.79 |
| RAW→RGB vs rawler | all | 59 | **7.00×** | 7.24× | 58/59 | 4.53 |
| RAW→RGB vs rawler | 1 | 59 | **4.22×** | 4.44× | 58/59 | 5.13 |
| RAW→RGB vs rawpy | all | 59 | **20.9×** | 25.2× | 59/59 | 1.56 |
| RAW→RGB vs rawpy | 1 | 59 | **2.09×** | 2.09× | 59/59 | 1.97 |
| RAW→RGB vs dcraw | all | 42 | **24.6×** | 27.9× | 42/42 | 0.99 |
| RAW→RGB vs dcraw | 1 | 42 | **2.35×** | 2.47× | 42/42 | 1.32 |
| RAW→RGB, FlashRaw GPU vs LibRaw | all | 145 | **22.1×** | 25.9× | 145/145 | 0.96 |

## Popular cameras: decode, all cores (ms)

| Camera | Format · compression | MP | FlashRaw | LibRaw | RawSpeed | rawler | rawpy | dcraw ² |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | CR3 · canon-crx | 46.7 | **77.5** | 134.2 | ❌ | 245.6 | 448.7 | ❌ |
| Canon EOS R5 (C-RAW) | CR3 · canon-crx | 46.7 | **91.6** | 138.0 | ❌ | 198.9 | 369.1 | ❌ |
| Canon EOS R5 Mark II (RAW) | CR3 · canon-crx | 47.9 | **79.5** | 138.9 | ❌ | 257.9 | 452.6 | ❌ |
| Canon EOS R6 Mark II (RAW) | CR3 · canon-crx | 25.5 | **44.2** | 75.6 | ❌ | 133.3 | 237.0 | ❌ |
| Canon EOS R6 Mark III (RAW) | CR3 · canon-crx | 34.0 | **57.1** | 98.1 | ❌ | 178.1 | 319.0 | ❌ |
| Canon EOS R7 (RAW) | CR3 · canon-crx | 33.7 | **57.2** | 98.5 | ❌ | 178.4 | 316.9 | ❌ |
| Canon EOS R8 (RAW) | CR3 · canon-crx | 25.5 | **43.7** | 74.2 | ❌ | 134.4 | 246.0 | ❌ |
| Canon EOS R50 (RAW) | CR3 · canon-crx | 25.1 | **43.5** | 73.9 | ❌ | 130.1 | 234.4 | ❌ |
| Canon EOS 90D (RAW) | CR3 · canon-crx | 33.7 | **30.3** | 98.7 | ❌ | 108.5 | 324.5 | ❌ |
| Canon EOS 5D Mark IV (RAW) | CR2 · lossless-jpeg | 31.3 | **20.0** | 434.8 | 110.8 | 256.0 | 430.1 | 632.1 |
| Canon EOS 6D Mark II (RAW) | CR2 · lossless-jpeg | 27.0 | **17.0** | 391.1 | 120.6 | 241.5 | 390.3 | 569.0 |
| Canon EOS Rebel T7 / 2000D (RAW) | CR2 · lossless-jpeg | 24.7 | **18.4** | 397.4 | 123.2 | 241.8 | 396.7 | 564.1 |
| Nikon Z8 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **25.7** | 544.4 | 220.1 | 360.9 | 580.8 | 894.7 |
| Nikon Z9 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **24.7** | 503.8 | 180.8 | 330.1 | 528.2 | 828.6 |
| Nikon Z6III (lossless) | NEF · nikon-huffman | 24.5 | **14.7** | 282.0 | 108.7 | 190.5 | 299.4 | 465.6 |
| Nikon Zf (lossless) | NEF · nikon-huffman | 24.5 | **13.3** | 268.6 | 96.2 | 176.5 | 286.7 | 444.2 |
| Nikon Z7II (lossless) | NEF · nikon-huffman | 45.7 | **23.6** | 484.1 | 168.5 | 325.1 | 516.7 | 800.5 |
| Nikon Z50 (lossless) | NEF · nikon-huffman | 20.9 | **12.0** | 245.4 | 93.6 | 158.9 | 262.9 | 405.4 |
| Nikon D850 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **23.7** | 481.1 | 168.5 | 323.3 | 510.7 | 796.6 |
| Nikon D750 (14-bit lossless) | NEF · nikon-huffman | 24.3 | **13.5** | 254.2 | 81.2 | 161.9 | 265.6 | 425.8 |
| Sony A7 IV (lossless) | ARW · sony-lossless | 36.7 | 19.0 | 426.5 | **12.3** | 39.3 | 429.7 | ❌ |
| Sony A7 IV (compressed) | ARW · sony-arw2 | 33.0 | **4.21** | 87.7 | 8.02 | 8.39 | 86.0 | 292.2 |
| Sony A7R V (lossless) | ARW · sony-lossless | 64.7 | 31.0 | 775.4 | **22.1** | 62.6 | 780.7 | ❌ |
| Sony A1 (compressed) | ARW · sony-arw2 | 50.2 | **6.10** | 130.4 | 11.6 | 11.8 | 128.8 | 432.8 |
| Sony A7 III (compressed) | ARW · sony-arw2 | 24.3 | **3.27** | 64.0 | 6.61 | 6.07 | 63.7 | 212.6 |
| Sony A7C II (compressed) | ARW · sony-arw2 | 33.0 | **4.21** | 87.4 | 8.12 | 8.15 | 86.6 | 292.1 |
| Sony A9 III (compressed) | ARW · sony-arw2 | 24.3 | **3.46** | 63.2 | 6.93 | 6.37 | 63.5 | 215.9 |
| Sony A6700 (lossless) | ARW · sony-lossless | 30.7 | 16.4 | 374.7 | **12.2** | 43.1 | 380.1 | ❌ |
| Sony A6400 (compressed) | ARW · sony-arw2 | 24.3 | **3.19** | 64.1 | 6.32 | 6.24 | 63.5 | 220.1 |
| Sony ZV-E10 (compressed) | ARW · sony-arw2 | 24.3 | **3.31** | 63.8 | 6.45 | 6.27 | 63.2 | 213.2 |
| Fujifilm X-T5 (lossless compressed) | RAF · fuji-compressed | 40.9 | **33.3** | 149.0 | 44.2 | 93.7 | 1186.4 | ❌ |
| Fujifilm X-H2 (compressed) | RAF · fuji-compressed | 40.9 | **44.4** | 174.5 | ❌ | 128.4 | 1482.2 | ❌ |
| Fujifilm X-S20 (lossless compressed) | RAF · fuji-compressed | 26.5 | **26.7** | 122.4 | 36.3 | 81.3 | 813.0 | ❌ |
| Fujifilm X100VI (lossless compressed) | RAF · fuji-compressed | 40.9 | **33.3** | 151.8 | 44.5 | 94.3 | 1183.5 | ❌ |
| Fujifilm X-T4 (uncompressed) | RAF · uncompressed | 26.7 | 1.76 | 19.6 | **1.24** | 3.32 | 19.7 | 187.1 |
| Fujifilm GFX100 II (lossless compressed) | RAF · fuji-compressed | 103.4 | **61.8** | 269.1 | 78.8 | 143.1 | 2608.0 | ❌ |
| Panasonic Lumix S5II | RW2 · panasonic-v8 | 24.1 | **12.1** | 130.8 | 61.2 | 120.8 | 248.7 | 257.6 |
| Panasonic Lumix GH6 | RW2 · panasonic-v8 | 25.2 | **12.4** | 132.0 | 62.3 | 120.3 | 260.5 | 270.3 |
| Panasonic Lumix G9II | RW2 · panasonic-v8 | 25.1 | **12.4** | 136.8 | 56.7 | 121.8 | 271.8 | 262.2 |
| Panasonic Lumix S5 | RW2 · panasonic-v7 | 24.3 | 1.78 | 8.74 | **0.61** | 3.90 | 8.71 | 258.6 |
| Panasonic Lumix S1R | RW2 · panasonic-v6 | 47.6 | **4.13** | 123.5 | 7.67 | 9.99 | 123.8 | 495.8 |
| Panasonic Lumix GH5 | RW2 · panasonic-v4 | 20.6 | **3.82** | 94.7 | 4.79 | 7.04 | 94.8 | 219.8 |
| OM System OM-1 Mark II | ORF · olympus | 20.4 | **106.6** | 384.7 | 161.8 | 226.6 | 387.7 | 162.9 |
| OM System OM-5 | ORF · olympus | 20.5 | **106.8** | 346.5 | 152.8 | 226.2 | 352.0 | 169.6 |
| Olympus E-M1 Mark III | ORF · olympus | 20.5 | **113.4** | 339.6 | 147.4 | 197.0 | 344.5 | 456.4 |
| Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | **14.5** | 199.8 | 85.9 | 176.2 | 293.0 | 472.3 |
| Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | **20.6** | 290.5 | 131.6 | 255.6 | 412.0 | 676.3 |
| Ricoh GR III | DNG · lossless-jpeg | 24.8 | **13.1** | 323.2 | 99.8 | 214.9 | 334.1 | 509.5 |
| Leica Q2 | DNG · uncompressed | 47.4 | **3.90** | 293.3 | 49.5 | 4.40 | 375.9 | 723.3 |
| Leica SL2 | DNG · uncompressed | 47.4 | **3.93** | 293.0 | 49.6 | 4.51 | 375.0 | 724.2 |
| Leica M10-R | DNG · lossless-jpeg | 41.0 | **20.6** | 558.6 | 202.5 | 386.9 | 562.7 | 888.1 |
| Hasselblad X2D 100C (compressed) | FFF · hasselblad | 105.3 | **65.4** | 1010.0 | 454.2 | ❌ | 1115.2 | 2336.1 |
| Phase One IQ4 150MP (IIQ L) | IIQ · phaseone | 154.0 | **40.4** | 1031.5 | ❌ | 341.4 | 1058.3 | 4255.9 |
| Sigma fp L | DNG · lossless-jpeg | 60.5 | **18.1** | 760.7 | 25.4 | 41.0 | 772.1 | 1232.1 |
| Apple iPhone 12 Pro (ProRAW) | DNG · lossless-jpeg | 12.2 | **18.6** | 464.0 | ❌ | 28.1 | 466.7 | 592.9 |
| Google Pixel 8 Pro | DNG · lossless-jpeg | 50.1 | **14.9** | 656.2 | 16.6 | 34.0 | 637.5 | 1038.4 |
| Google Pixel 6 Pro | DNG · lossless-jpeg | 12.5 | **4.41** | 164.7 | 5.22 | 9.88 | 164.4 | 261.0 |
| Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 49.9 | **150.9** | 1899.0 | 581.0 | 1140.6 | 1833.2 | 2355.1 |
| Samsung Galaxy S22 (JPEG XL) | DNG · jpeg-xl | 12.0 | 284.8 | ❌ | ❌ | **178.6** | ❌ | ❌ |
| DJI Phantom 4 Pro | DNG · uncompressed | 20.0 | 1.13 | 17.7 | **1.06** | 1.43 | 17.9 | 165.8 |
| GoPro HERO11 Black (GPR) | DNG · gopro-vc5 | 27.1 | **17.5** | ❌ | 23.8 | ❌ | ❌ | ❌ |
| GoPro HERO10 Black (GPR) | DNG · gopro-vc5 | 23.3 | **15.8** | ❌ | 25.6 | ❌ | ❌ | ❌ |

## Popular cameras: RAW → RGB, all cores (ms)

| Camera | FlashRaw | LibRaw | LibRaw + Little CMS | rawler | rawpy | dcraw ² | FlashRaw GPU |
|---|---:|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | **101.3** | 731.8 | 1106.9 | 507.3 | 1130.0 | ❌ | 95.9 |
| Canon EOS R5 (C-RAW) | **114.9** | 732.9 | 1097.7 | 444.2 | 1051.4 | ❌ | 109.1 |
| Canon EOS R5 Mark II (RAW) | **105.5** | 736.1 | 1104.9 | 497.6 | 1122.2 | ❌ | 98.4 |
| Canon EOS R6 Mark II (RAW) | **57.7** | 406.4 | 609.2 | 279.1 | 607.3 | ❌ | 55.0 |
| Canon EOS R6 Mark III (RAW) | **75.2** | 539.1 | 799.8 | 358.9 | 809.0 | ❌ | 71.8 |
| Canon EOS R7 (RAW) | **78.0** | 538.4 | 807.9 | 357.4 | 808.0 | ❌ | 72.8 |
| Canon EOS R8 (RAW) | **58.7** | 404.3 | 611.5 | 269.6 | 607.3 | ❌ | 54.3 |
| Canon EOS R50 (RAW) | **57.2** | 402.9 | 600.0 | 265.4 | 596.2 | ❌ | 54.1 |
| Canon EOS 90D (RAW) | **47.6** | 538.7 | 811.7 | 297.9 | 810.7 | ❌ | 43.0 |
| Canon EOS 5D Mark IV (RAW) | **36.3** | 844.4 | 1100.7 | 435.5 | 884.0 | 993.2 | 31.7 |
| Canon EOS 6D Mark II (RAW) | **31.1** | 746.6 | 969.8 | 389.3 | 782.3 | 892.6 | 28.8 |
| Canon EOS Rebel T7 / 2000D (RAW) | **31.5** | 726.3 | 920.2 | 378.9 | 763.6 | 857.8 | 29.4 |
| Nikon Z8 (14-bit lossless) | **50.1** | 1156.2 | 1548.7 | 630.3 | 1281.1 | 1349.1 | 44.3 |
| Nikon Z9 (14-bit lossless) | **50.4** | 1110.5 | 1494.0 | 584.0 | 1225.2 | 1274.4 | 41.3 |
| Nikon Z6III (lossless) | **27.8** | 616.1 | 823.1 | 330.5 | 673.9 | 705.1 | 26.0 |
| Nikon Zf (lossless) | **27.4** | 603.2 | 806.4 | 315.0 | 654.7 | 677.9 | 22.9 |
| Nikon Z7II (lossless) | **48.7** | 1093.1 | 1470.4 | 581.7 | 1200.2 | 1251.6 | 40.7 |
| Nikon Z50 (lossless) | **23.9** | 531.3 | 712.2 | 280.5 | 577.5 | 608.6 | 20.4 |
| Nikon D850 (14-bit lossless) | **48.1** | 1096.8 | 1469.2 | 584.4 | 1198.0 | 1344.6 | 41.4 |
| Nikon D750 (14-bit lossless) | **26.7** | 592.2 | 781.7 | 298.0 | 633.7 | 715.5 | 22.5 |
| Sony A7 IV (lossless) | **36.0** | 867.4 | 1141.6 | 222.9 | 925.0 | ❌ | 30.7 |
| Sony A7 IV (compressed) | **21.9** | 530.5 | 803.1 | 194.8 | 580.0 | 688.1 | 15.5 |
| Sony A7R V (lossless) | **62.4** | 1587.0 | 2083.8 | 396.1 | 1705.5 | ❌ | 54.1 |
| Sony A1 (compressed) | **32.0** | 804.5 | 1208.6 | 284.9 | 879.0 | 1047.8 | 21.4 |
| Sony A7 III (compressed) | **16.3** | 395.3 | 600.3 | 145.2 | 425.7 | 515.5 | 13.0 |
| Sony A7C II (compressed) | **21.8** | 527.5 | 804.5 | 195.1 | 579.8 | 694.9 | 15.4 |
| Sony A9 III (compressed) | **17.3** | 393.8 | 596.9 | 143.3 | 424.5 | 508.9 | 12.1 |
| Sony A6700 (lossless) | **30.5** | 727.4 | 955.9 | 191.8 | 772.8 | ❌ | 26.4 |
| Sony A6400 (compressed) | **16.7** | 388.8 | 591.2 | 144.0 | 428.2 | 512.7 | 12.3 |
| Sony ZV-E10 (compressed) | **14.0** | 394.3 | 590.9 | 141.7 | 422.1 | 448.4 | 10.9 |
| Fujifilm X-T5 (lossless compressed) | **66.1** | 692.2 | 1018.0 | 391.6 | 1818.2 | ❌ | 49.9 |
| Fujifilm X-H2 (compressed) | **90.0** | 881.8 | 1194.3 | 422.6 | 2285.2 | ❌ | 62.9 |
| Fujifilm X-S20 (lossless compressed) | **48.3** | 476.0 | 697.0 | 272.2 | 1229.7 | ❌ | 38.3 |
| Fujifilm X100VI (lossless compressed) | **78.6** | 897.3 | 1201.7 | 385.5 | 2033.4 | ❌ | 50.2 |
| Fujifilm X-T4 (uncompressed) | **23.8** | 376.1 | 585.8 | 201.2 | 427.0 | 454.7 | 9.33 |
| Fujifilm GFX100 II (lossless compressed) | **116.2** | 1601.1 | 2437.1 | 685.5 | 4141.8 | ❌ | 96.9 |
| Panasonic Lumix S5II | **25.1** | 500.8 | 696.7 | 228.7 | 648.2 | 498.3 | 21.9 |
| Panasonic Lumix GH6 | **26.5** | 514.4 | 720.6 | 231.3 | 681.9 | 525.3 | 22.6 |
| Panasonic Lumix G9II | **25.6** | 521.9 | 736.1 | 234.3 | 688.5 | 578.5 | 21.9 |
| Panasonic Lumix S5 | **15.3** | 381.5 | 581.6 | 111.1 | 407.9 | 502.9 | 9.71 |
| Panasonic Lumix S1R | **29.3** | 820.9 | 1208.6 | 211.3 | 907.6 | 950.5 | 18.7 |
| Panasonic Lumix GH5 | **15.7** | 412.2 | 578.7 | 97.6 | 429.9 | 464.8 | 12.1 |
| OM System OM-1 Mark II | **117.4** | 671.1 | 841.3 | 344.0 | 701.4 | 357.1 | 114.7 |
| OM System OM-5 | **119.7** | 634.2 | 802.6 | 342.0 | 660.6 | 365.2 | 117.4 |
| Olympus E-M1 Mark III | **125.3** | 622.8 | 803.1 | 315.9 | 653.2 | 711.4 | 124.5 |
| Pentax K-3 Mark III | **28.8** | 559.3 | 771.7 | 322.9 | 685.8 | 792.3 | 23.7 |
| Pentax K-1 Mark II | **39.8** | 779.0 | 1086.2 | 464.3 | 955.3 | 1118.2 | 35.4 |
| Ricoh GR III | **26.6** | 653.3 | 852.6 | 353.3 | 693.8 | 800.0 | 22.2 |
| Leica Q2 | **28.4** | 924.4 | 1303.8 | 260.6 | 1080.8 | 1299.1 | CPU¹ |
| Leica SL2 | **28.2** | 925.5 | 1290.6 | 259.5 | 1082.7 | 1276.6 | CPU¹ |
| Leica M10-R | **43.9** | 1113.5 | 1461.3 | 604.5 | 1180.1 | 1390.4 | 36.0 |
| Hasselblad X2D 100C (compressed) | **121.2** | 2369.1 | 3190.2 | ❌ | 2655.9 | 3379.0 | 108.2 |
| Phase One IQ4 150MP (IIQ L) | **116.6** | 4463.9 | 5674.2 | 977.4 | 4792.4 | 6147.4 | 84.9 |
| Sigma fp L | **57.5** | 1558.3 | 2075.0 | 374.4 | 1672.4 | 1962.0 | CPU¹ |
| Apple iPhone 12 Pro (ProRAW) | **27.0** | 595.1 | 669.6 | 74.7 | 597.9 | 693.7 | 24.1 |
| Google Pixel 8 Pro | **45.9** | 1476.6 | 1884.1 | 309.6 | 1545.2 | 1650.6 | CPU¹ |
| Google Pixel 6 Pro | **13.2** | 353.5 | 458.5 | 82.8 | 354.7 | 419.0 | CPU¹ |
| Samsung Galaxy S23 Ultra | **194.0** | 2191.1 | 2480.2 | 1305.4 | 2094.9 | 2679.2 | CPU¹ |
| Samsung Galaxy S22 (JPEG XL) | 296.1 | ❌ | ❌ | **222.2** | ❌ | ❌ | CPU¹ |
| DJI Phantom 4 Pro | **19.0** | 290.9 | 463.1 | 116.3 | 318.8 | 407.8 | CPU¹ |
| GoPro HERO11 Black (GPR) | **35.7** | ❌ | ❌ | ❌ | ❌ | ❌ | CPU¹ |
| GoPro HERO10 Black (GPR) | **35.1** | ❌ | ❌ | ❌ | ❌ | ❌ | CPU¹ |

FlashRaw GPU = `--backend gpu --output rgba8`: CPU decode or GPU unpack, GPU develop, frame downloaded to host memory. ¹ FlashRaw's GPU pipeline does not handle this file's processing yet and ran it on the CPU.

## Popular cameras: decode, one thread (ms)

| Camera | Format · compression | MP | FlashRaw | LibRaw | RawSpeed | rawler | rawpy | dcraw ² |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | CR3 · canon-crx | 46.7 | **304.2** | 440.5 | ❌ | 652.3 | 447.5 | ❌ |
| Canon EOS R5 (C-RAW) | CR3 · canon-crx | 46.7 | **331.1** | 362.6 | ❌ | 658.9 | 367.0 | ❌ |
| Canon EOS R5 Mark II (RAW) | CR3 · canon-crx | 47.9 | **313.5** | 440.2 | ❌ | 653.1 | 452.0 | ❌ |
| Canon EOS R6 Mark II (RAW) | CR3 · canon-crx | 25.5 | **169.4** | 234.2 | ❌ | 354.7 | 240.0 | ❌ |
| Canon EOS R6 Mark III (RAW) | CR3 · canon-crx | 34.0 | **225.1** | 313.4 | ❌ | 470.7 | 315.8 | ❌ |
| Canon EOS R7 (RAW) | CR3 · canon-crx | 33.7 | **225.2** | 308.5 | ❌ | 462.4 | 318.8 | ❌ |
| Canon EOS R8 (RAW) | CR3 · canon-crx | 25.5 | **167.4** | 241.2 | ❌ | 355.8 | 243.2 | ❌ |
| Canon EOS R50 (RAW) | CR3 · canon-crx | 25.1 | **167.5** | 227.7 | ❌ | 341.3 | 233.3 | ❌ |
| Canon EOS 90D (RAW) | CR3 · canon-crx | 33.7 | **221.5** | 310.9 | ❌ | 451.3 | 318.8 | ❌ |
| Canon EOS 5D Mark IV (RAW) | CR2 · lossless-jpeg | 31.3 | 208.9 | 434.2 | **110.6** | 255.9 | 428.7 | 632.1 |
| Canon EOS 6D Mark II (RAW) | CR2 · lossless-jpeg | 27.0 | 193.6 | 392.3 | **120.8** | 241.8 | 388.6 | 569.0 |
| Canon EOS Rebel T7 / 2000D (RAW) | CR2 · lossless-jpeg | 24.7 | 193.1 | 400.8 | **124.4** | 242.1 | 396.6 | 564.1 |
| Nikon Z8 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **205.5** | 545.7 | 218.5 | 363.0 | 578.0 | 894.7 |
| Nikon Z9 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **170.4** | 502.1 | 181.1 | 329.9 | 529.1 | 828.6 |
| Nikon Z6III (lossless) | NEF · nikon-huffman | 24.5 | 110.7 | 284.8 | **108.1** | 189.1 | 300.5 | 465.6 |
| Nikon Zf (lossless) | NEF · nikon-huffman | 24.5 | **88.6** | 268.3 | 94.4 | 176.2 | 283.9 | 444.2 |
| Nikon Z7II (lossless) | NEF · nikon-huffman | 45.7 | **162.1** | 484.8 | 169.0 | 322.9 | 514.7 | 800.5 |
| Nikon Z50 (lossless) | NEF · nikon-huffman | 20.9 | **85.4** | 246.8 | 93.6 | 158.2 | 262.5 | 405.4 |
| Nikon D850 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **162.8** | 481.9 | 168.8 | 320.7 | 507.9 | 796.6 |
| Nikon D750 (14-bit lossless) | NEF · nikon-huffman | 24.3 | 85.7 | 251.8 | **82.1** | 164.5 | 264.8 | 425.8 |
| Sony A7 IV (lossless) | ARW · sony-lossless | 36.7 | 272.7 | 429.4 | **145.8** | 331.2 | 433.7 | ❌ |
| Sony A7 IV (compressed) | ARW · sony-arw2 | 33.0 | **42.0** | 84.0 | 91.5 | 84.3 | 84.7 | 292.2 |
| Sony A7R V (lossless) | ARW · sony-lossless | 64.7 | 483.6 | 774.9 | **265.9** | 585.9 | 778.7 | ❌ |
| Sony A1 (compressed) | ARW · sony-arw2 | 50.2 | **65.6** | 128.7 | 135.0 | 128.4 | 128.6 | 432.8 |
| Sony A7 III (compressed) | ARW · sony-arw2 | 24.3 | **31.8** | 63.5 | 67.3 | 62.3 | 63.2 | 212.6 |
| Sony A7C II (compressed) | ARW · sony-arw2 | 33.0 | **42.7** | 87.1 | 89.3 | 86.1 | 86.5 | 292.1 |
| Sony A9 III (compressed) | ARW · sony-arw2 | 24.3 | **31.8** | 63.6 | 68.7 | 63.5 | 63.9 | 215.9 |
| Sony A6700 (lossless) | ARW · sony-lossless | 30.7 | 243.1 | 374.2 | **137.9** | 293.1 | 377.1 | ❌ |
| Sony A6400 (compressed) | ARW · sony-arw2 | 24.3 | **31.6** | 63.5 | 62.5 | 62.6 | 64.0 | 220.1 |
| Sony ZV-E10 (compressed) | ARW · sony-arw2 | 24.3 | **31.5** | 63.6 | 62.7 | 63.2 | 64.3 | 213.2 |
| Fujifilm X-T5 (lossless compressed) | RAF · fuji-compressed | 40.9 | **354.6** | 1167.5 | 468.8 | 922.9 | 1174.8 | ❌ |
| Fujifilm X-H2 (compressed) | RAF · fuji-compressed | 40.9 | **471.4** | 1459.9 | ❌ | 1188.5 | 1481.8 | ❌ |
| Fujifilm X-S20 (lossless compressed) | RAF · fuji-compressed | 26.5 | **229.2** | 793.4 | 308.7 | 644.7 | 820.1 | ❌ |
| Fujifilm X100VI (lossless compressed) | RAF · fuji-compressed | 40.9 | **353.2** | 1172.9 | 468.6 | 936.8 | 1189.8 | ❌ |
| Fujifilm X-T4 (uncompressed) | RAF · uncompressed | 26.7 | 1.93 | 19.7 | **1.19** | 3.88 | 19.9 | 187.1 |
| Fujifilm GFX100 II (lossless compressed) | RAF · fuji-compressed | 103.4 | **833.0** | 2584.7 | 1024.4 | 1916.8 | 2622.1 | ❌ |
| Panasonic Lumix S5II | RW2 · panasonic-v8 | 24.1 | 132.7 | 247.9 | **116.0** | 227.1 | 248.7 | 257.6 |
| Panasonic Lumix GH6 | RW2 · panasonic-v8 | 25.2 | 137.1 | 261.4 | **121.8** | 234.0 | 260.5 | 270.3 |
| Panasonic Lumix G9II | RW2 · panasonic-v8 | 25.1 | 136.1 | 267.7 | **108.1** | 236.8 | 273.8 | 262.2 |
| Panasonic Lumix S5 | RW2 · panasonic-v7 | 24.3 | 4.49 | 8.70 | **3.77** | 19.3 | 8.89 | 258.6 |
| Panasonic Lumix S1R | RW2 · panasonic-v6 | 47.6 | **37.7** | 123.0 | 87.1 | 95.3 | 123.1 | 495.8 |
| Panasonic Lumix GH5 | RW2 · panasonic-v4 | 20.6 | **42.5** | 92.7 | 46.2 | 80.1 | 94.9 | 219.8 |
| OM System OM-1 Mark II | ORF · olympus | 20.4 | **134.2** | 388.7 | 159.2 | 225.6 | 391.4 | 162.9 |
| OM System OM-5 | ORF · olympus | 20.5 | **135.8** | 350.3 | 153.5 | 226.3 | 353.4 | 169.6 |
| Olympus E-M1 Mark III | ORF · olympus | 20.5 | **135.8** | 346.1 | 147.3 | 199.4 | 348.0 | 456.4 |
| Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | 91.8 | 205.2 | **86.4** | 176.9 | 290.9 | 472.3 |
| Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | 140.1 | 287.2 | **131.5** | 256.8 | 415.0 | 676.3 |
| Ricoh GR III | DNG · lossless-jpeg | 24.8 | **93.1** | 325.2 | 99.0 | 215.9 | 334.7 | 509.5 |
| Leica Q2 | DNG · uncompressed | 47.4 | **11.8** | 291.7 | 49.8 | 40.7 | 374.8 | 723.3 |
| Leica SL2 | DNG · uncompressed | 47.4 | **11.7** | 292.2 | 49.5 | 41.1 | 373.7 | 724.2 |
| Leica M10-R | DNG · lossless-jpeg | 41.0 | **156.8** | 558.0 | 203.2 | 386.4 | 558.3 | 888.1 |
| Hasselblad X2D 100C (compressed) | FFF · hasselblad | 105.3 | 671.4 | 1016.6 | **451.7** | ❌ | 1133.0 | 2336.1 |
| Phase One IQ4 150MP (IIQ L) | IIQ · phaseone | 154.0 | **562.5** | 1028.3 | ❌ | 950.8 | 1061.7 | 4255.9 |
| Sigma fp L | DNG · lossless-jpeg | 60.5 | **226.1** | 761.2 | 232.6 | 582.9 | 769.8 | 1232.1 |
| Apple iPhone 12 Pro (ProRAW) | DNG · lossless-jpeg | 12.2 | **265.8** | 467.1 | ❌ | 402.4 | 468.6 | 592.9 |
| Google Pixel 8 Pro | DNG · lossless-jpeg | 50.1 | **182.6** | 650.2 | 189.5 | 476.4 | 648.0 | 1038.4 |
| Google Pixel 6 Pro | DNG · lossless-jpeg | 12.5 | **46.6** | 163.4 | 50.5 | 120.0 | 162.8 | 261.0 |
| Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 49.9 | 1125.1 | 1901.7 | **889.3** | 1443.9 | 1823.4 | 2355.1 |
| Samsung Galaxy S22 (JPEG XL) | DNG · jpeg-xl | 12.0 | 571.6 | ❌ | ❌ | **467.2** | ❌ | ❌ |
| DJI Phantom 4 Pro | DNG · uncompressed | 20.0 | 1.45 | 17.9 | **1.00** | 1.54 | 17.5 | 165.8 |
| GoPro HERO11 Black (GPR) | DNG · gopro-vc5 | 27.1 | **127.8** | ❌ | 164.1 | ❌ | ❌ | ❌ |
| GoPro HERO10 Black (GPR) | DNG · gopro-vc5 | 23.3 | **106.6** | ❌ | 154.4 | ❌ | ❌ | ❌ |

## Popular cameras: RAW → RGB, one thread (ms)

| Camera | FlashRaw | LibRaw | LibRaw + Little CMS | rawler | rawpy | dcraw ² |
|---|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | **641.1** | 1096.8 | 7209.3 | 2649.7 | 1115.7 | ❌ |
| Canon EOS R5 (C-RAW) | **663.4** | 1021.9 | 7128.4 | 2660.6 | 1036.6 | ❌ |
| Canon EOS R5 Mark II (RAW) | **646.1** | 1110.8 | 7213.5 | 2629.4 | 1119.6 | ❌ |
| Canon EOS R6 Mark II (RAW) | **351.2** | 597.2 | 3890.8 | 1446.2 | 604.1 | ❌ |
| Canon EOS R6 Mark III (RAW) | **465.5** | 793.8 | 5226.0 | 1899.3 | 798.8 | ❌ |
| Canon EOS R7 (RAW) | **466.1** | 795.1 | 5248.3 | 1927.1 | 810.7 | ❌ |
| Canon EOS R8 (RAW) | **346.6** | 602.3 | 3876.9 | 1418.7 | 609.0 | ❌ |
| Canon EOS R50 (RAW) | **348.0** | 589.5 | 3878.3 | 1422.4 | 597.2 | ❌ |
| Canon EOS 90D (RAW) | **458.9** | 797.8 | 5288.1 | 1912.0 | 804.0 | ❌ |
| Canon EOS 5D Mark IV (RAW) | **434.9** | 882.5 | 5067.5 | 1619.1 | 890.9 | 993.2 |
| Canon EOS 6D Mark II (RAW) | **387.8** | 779.1 | 4387.0 | 1422.0 | 785.7 | 892.6 |
| Canon EOS Rebel T7 / 2000D (RAW) | **372.2** | 757.7 | 4002.4 | 1333.0 | 760.1 | 857.8 |
| Nikon Z8 (14-bit lossless) | **541.4** | 1219.0 | 7442.5 | 2446.2 | 1269.1 | 1349.1 |
| Nikon Z9 (14-bit lossless) | **505.0** | 1182.1 | 7482.9 | 2375.6 | 1220.1 | 1274.4 |
| Nikon Z6III (lossless) | **291.6** | 649.4 | 4018.9 | 1310.5 | 668.3 | 705.1 |
| Nikon Zf (lossless) | **271.1** | 635.2 | 3998.0 | 1291.3 | 649.7 | 677.9 |
| Nikon Z7II (lossless) | **493.7** | 1162.1 | 7462.3 | 2386.6 | 1200.8 | 1251.6 |
| Nikon Z50 (lossless) | **237.9** | 558.9 | 3425.4 | 1110.2 | 582.0 | 608.6 |
| Nikon D850 (14-bit lossless) | **500.4** | 1162.7 | 7459.9 | 2417.9 | 1198.1 | 1344.6 |
| Nikon D750 (14-bit lossless) | **267.8** | 617.3 | 3842.0 | 1209.5 | 629.6 | 715.5 |
| Sony A7 IV (lossless) | **513.8** | 913.0 | 5447.2 | 1796.2 | 919.1 | ❌ |
| Sony A7 IV (compressed) | **286.3** | 577.0 | 5089.4 | 1586.9 | 579.4 | 688.1 |
| Sony A7R V (lossless) | **939.9** | 1681.7 | 10072.8 | 3364.5 | 1698.0 | ❌ |
| Sony A1 (compressed) | **432.2** | 866.0 | 7760.8 | 2359.8 | 881.6 | 1047.8 |
| Sony A7 III (compressed) | **209.4** | 423.8 | 3758.9 | 1176.8 | 427.5 | 515.5 |
| Sony A7C II (compressed) | **284.3** | 573.6 | 5103.9 | 1594.8 | 576.2 | 694.9 |
| Sony A9 III (compressed) | **206.8** | 418.5 | 3747.4 | 1167.8 | 424.2 | 508.9 |
| Sony A6700 (lossless) | **429.8** | 762.3 | 4348.4 | 1490.8 | 766.9 | ❌ |
| Sony A6400 (compressed) | **208.0** | 423.8 | 3748.9 | 1148.3 | 427.7 | 512.7 |
| Sony ZV-E10 (compressed) | **180.8** | 422.4 | 3703.3 | 1146.6 | 424.1 | 448.4 |
| Fujifilm X-T5 (lossless compressed) | **829.8** | 1795.5 | 7325.7 | 3785.0 | 1820.8 | ❌ |
| Fujifilm X-H2 (compressed) | **1156.4** | 2257.0 | 7746.1 | 4059.1 | 2273.3 | ❌ |
| Fujifilm X-S20 (lossless compressed) | **543.4** | 1205.4 | 4814.9 | 2504.3 | 1234.0 | ❌ |
| Fujifilm X100VI (lossless compressed) | **1034.5** | 2002.7 | 7499.1 | 3792.3 | 2031.2 | ❌ |
| Fujifilm X-T4 (uncompressed) | **316.9** | 426.6 | 4005.5 | 1861.6 | 436.5 | 454.7 |
| Fujifilm GFX100 II (lossless compressed) | **1578.4** | 4090.0 | 18022.8 | 6461.3 | 4141.8 | ❌ |
| Panasonic Lumix S5II | **309.9** | 639.0 | 3920.4 | 1285.7 | 646.7 | 498.3 |
| Panasonic Lumix GH6 | **324.8** | 671.4 | 4151.2 | 1349.3 | 679.3 | 525.3 |
| Panasonic Lumix G9II | **325.3** | 679.8 | 4131.9 | 1306.0 | 689.6 | 578.5 |
| Panasonic Lumix S5 | **181.9** | 402.0 | 3733.6 | 1059.6 | 409.8 | 502.9 |
| Panasonic Lumix S1R | **381.7** | 890.7 | 7320.6 | 2146.4 | 904.6 | 950.5 |
| Panasonic Lumix GH5 | **192.5** | 422.2 | 3207.4 | 945.0 | 432.4 | 464.8 |
| OM System OM-1 Mark II | **284.1** | 689.4 | 3486.1 | 1149.4 | 699.6 | 357.1 |
| OM System OM-5 | **289.3** | 656.9 | 3474.8 | 1112.9 | 661.7 | 365.2 |
| Olympus E-M1 Mark III | **288.1** | 650.4 | 3464.9 | 1109.6 | 656.3 | 711.4 |
| Pentax K-3 Mark III | **282.4** | 586.8 | 4153.6 | 1320.4 | 681.4 | 792.3 |
| Pentax K-1 Mark II | **407.1** | 832.3 | 5851.6 | 1892.8 | 959.4 | 1118.2 |
| Ricoh GR III | **269.8** | 682.0 | 4007.4 | 1293.2 | 691.3 | 800.0 |
| Leica Q2 | **353.9** | 993.4 | 7535.4 | 2137.2 | 1085.9 | 1299.1 |
| Leica SL2 | **356.0** | 990.6 | 7235.2 | 2095.8 | 1080.2 | 1276.6 |
| Leica M10-R | **461.4** | 1170.9 | 6816.3 | 2213.7 | 1168.8 | 1390.4 |
| Hasselblad X2D 100C (compressed) | **1431.6** | 2517.3 | 16549.3 | ❌ | 2652.5 | 3379.0 |
| Phase One IQ4 150MP (IIQ L) | **1678.7** | 4701.6 | 25445.9 | 7582.5 | 4777.8 | 6147.4 |
| Sigma fp L | **737.5** | 1664.3 | 9959.2 | 3275.7 | 1684.4 | 1962.0 |
| Apple iPhone 12 Pro (ProRAW) | **371.5** | 593.9 | 2214.1 | 625.3 | 595.4 | 693.7 |
| Google Pixel 8 Pro | **608.9** | 1541.4 | 8477.7 | 2696.8 | 1539.7 | 1650.6 |
| Google Pixel 6 Pro | **151.8** | 348.0 | 2084.0 | 691.4 | 353.0 | 419.0 |
| Samsung Galaxy S23 Ultra | **1737.0** | 2184.2 | 7529.9 | 2420.9 | 2123.6 | 2679.2 |
| Samsung Galaxy S22 (JPEG XL) | 752.7 | ❌ | ❌ | **700.8** | ❌ | ❌ |
| DJI Phantom 4 Pro | **223.7** | 312.7 | 3052.6 | 921.0 | 318.5 | 407.8 |
| GoPro HERO11 Black (GPR) | **361.6** | ❌ | ❌ | ❌ | ❌ | ❌ |
| GoPro HERO10 Black (GPR) | **306.2** | ❌ | ❌ | ❌ | ❌ | ❌ |

## Popular cameras: peak memory, all cores (MB)

| Camera | MP | decode: FlashRaw | LibRaw | RawSpeed | rawler | rawpy | dcraw ² | RGB: FlashRaw | LibRaw | LibRaw + Little CMS | rawler | rawpy | dcraw ² |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | 46.7 | 227 | 134 | ❌ | 410 | 152 | ❌ | 410 | 604 | 903 | 1886 | 622 | ❌ |
| Canon EOS R5 (C-RAW) | 46.7 | 216 | 127 | ❌ | 396 | 148 | ❌ | 393 | 598 | 896 | 1872 | 614 | ❌ |
| Canon EOS R5 Mark II (RAW) | 47.9 | 247 | 151 | ❌ | 428 | 168 | ❌ | 427 | 623 | 925 | 1916 | 641 | ❌ |
| Canon EOS R6 Mark II (RAW) | 25.5 | 137 | 83 | ❌ | 240 | 101 | ❌ | 234 | 337 | 500 | 1038 | 356 | ❌ |
| Canon EOS R6 Mark III (RAW) | 34.0 | 173 | 104 | ❌ | 278 | 123 | ❌ | 302 | 444 | 660 | 1348 | 461 | ❌ |
| Canon EOS R7 (RAW) | 33.7 | 172 | 101 | ❌ | 275 | 120 | ❌ | 300 | 445 | 662 | 1342 | 462 | ❌ |
| Canon EOS R8 (RAW) | 25.5 | 137 | 82 | ❌ | 239 | 99 | ❌ | 233 | 337 | 499 | 1038 | 355 | ❌ |
| Canon EOS R50 (RAW) | 25.1 | 138 | 85 | ❌ | 240 | 103 | ❌ | 235 | 337 | 498 | 1033 | 355 | ❌ |
| Canon EOS 90D (RAW) | 33.7 | 174 | 105 | ❌ | 294 | 122 | ❌ | 303 | 446 | 663 | 1358 | 465 | ❌ |
| Canon EOS 5D Mark IV (RAW) | 31.3 | 276 | 97 | 99 | 190 | 116 | 294 | 406 | 416 | 620 | 1125 | 435 | 294 |
| Canon EOS 6D Mark II (RAW) | 27.0 | 231 | 92 | 95 | 126 | 111 | 254 | 327 | 369 | 543 | 984 | 386 | 254 |
| Canon EOS Rebel T7 / 2000D (RAW) | 24.7 | 250 | 89 | 92 | 170 | 109 | 234 | 345 | 344 | 507 | 911 | 363 | 234 |
| Nikon Z8 (14-bit lossless) | 45.7 | 295 | 150 | 153 | 235 | 170 | 439 | 463 | 631 | 937 | 1719 | 650 | 439 |
| Nikon Z9 (14-bit lossless) | 45.7 | 287 | 147 | 150 | 228 | 166 | 439 | 451 | 627 | 933 | 1712 | 647 | 439 |
| Nikon Z6III (lossless) | 24.5 | 174 | 83 | 86 | 144 | 101 | 236 | 274 | 341 | 504 | 1219 | 359 | 236 |
| Nikon Zf (lossless) | 24.5 | 154 | 80 | 84 | 141 | 100 | 236 | 249 | 339 | 504 | 937 | 358 | 236 |
| Nikon Z7II (lossless) | 45.7 | 273 | 144 | 146 | 224 | 162 | 439 | 453 | 626 | 931 | 1711 | 644 | 439 |
| Nikon Z50 (lossless) | 20.9 | 140 | 74 | 77 | 131 | 93 | 202 | 219 | 294 | 434 | 810 | 313 | 202 |
| Nikon D850 (14-bit lossless) | 45.7 | 276 | 145 | 148 | 225 | 164 | 439 | 450 | 626 | 931 | 1711 | 643 | 439 |
| Nikon D750 (14-bit lossless) | 24.3 | 155 | 76 | 79 | 133 | 96 | 235 | 243 | 332 | 496 | 924 | 351 | 235 |
| Sony A7 IV (lossless) | 36.7 | 140 | 122 | 126 | 218 | 141 | ❌ | 267 | 468 | 688 | 1324 | 487 | ❌ |
| Sony A7 IV (compressed) | 33.0 | 112 | 106 | 110 | 180 | 125 | 318 | 243 | 453 | 674 | 1251 | 471 | 318 |
| Sony A7R V (lossless) | 64.7 | 230 | 210 | 214 | 344 | 229 | ❌ | 461 | 853 | 1259 | 2359 | 869 | ❌ |
| Sony A1 (compressed) | 50.2 | 166 | 160 | 165 | 256 | 180 | 481 | 367 | 687 | 1022 | 1884 | 706 | 481 |
| Sony A7 III (compressed) | 24.3 | 81 | 76 | 79 | 135 | 95 | 234 | 178 | 331 | 493 | 925 | 349 | 234 |
| Sony A7C II (compressed) | 33.0 | 111 | 106 | 110 | 180 | 125 | 318 | 243 | 453 | 674 | 1250 | 472 | 318 |
| Sony A9 III (compressed) | 24.3 | 84 | 78 | 83 | 141 | 97 | 234 | 181 | 333 | 495 | 930 | 352 | 234 |
| Sony A6700 (lossless) | 30.7 | 124 | 104 | 109 | 196 | 124 | ❌ | 223 | 379 | 552 | 1084 | 397 | ❌ |
| Sony A6400 (compressed) | 24.3 | 80 | 75 | 78 | 133 | 94 | 234 | 177 | 330 | 493 | 923 | 348 | 234 |
| Sony ZV-E10 (compressed) | 24.3 | 80 | 75 | 79 | 134 | 94 | 234 | 162 | 330 | 493 | 923 | 348 | 234 |
| Fujifilm X-T5 (lossless compressed) | 40.9 | 130 | 126 | 128 | 198 | 142 | ❌ | 288 | 548 | 816 | 1510 | 564 | ❌ |
| Fujifilm X-H2 (compressed) | 40.9 | 117 | 112 | ❌ | 176 | 130 | ❌ | 275 | 535 | 803 | 1486 | 552 | ❌ |
| Fujifilm X-S20 (lossless compressed) | 26.5 | 93 | 90 | 91 | 150 | 107 | ❌ | 198 | 364 | 540 | 1000 | 382 | ❌ |
| Fujifilm X100VI (lossless compressed) | 40.9 | 136 | 131 | 134 | 208 | 150 | ❌ | 294 | 554 | 822 | 1517 | 571 | ❌ |
| Fujifilm X-T4 (uncompressed) | 26.7 | 116 | 111 | 113 | 197 | 130 | 252 | 220 | 385 | 560 | 998 | 403 | 252 |
| Fujifilm GFX100 II (lossless compressed) | 103.4 | 314 | 311 | 312 | 457 | 327 | ❌ | 716 | 1380 | 2063 | 3778 | 1398 | ❌ |
| Panasonic Lumix S5II | 24.1 | 239 | 91 | 89 | 152 | 106 | 232 | 333 | 342 | 507 | 931 | 360 | 232 |
| Panasonic Lumix GH6 | 25.2 | 231 | 99 | 99 | 169 | 117 | 243 | 333 | 364 | 532 | 984 | 380 | 243 |
| Panasonic Lumix G9II | 25.1 | 227 | 96 | 95 | 162 | 113 | 242 | 325 | 359 | 529 | 977 | 376 | 242 |
| Panasonic Lumix S5 | 24.3 | 97 | 93 | 96 | 169 | 112 | 234 | 195 | 347 | 510 | 950 | 366 | 234 |
| Panasonic Lumix S1R | 47.6 | 167 | 162 | 165 | 263 | 182 | 453 | 356 | 658 | 974 | 1787 | 676 | 453 |
| Panasonic Lumix GH5 | 20.6 | 72 | 66 | 71 | 125 | 86 | 197 | 154 | 281 | 418 | 783 | 299 | 197 |
| OM System OM-1 Mark II | 20.4 | 73 | 64 | 68 | 120 | 85 | 197 | 154 | 281 | 419 | 785 | 299 | 197 |
| OM System OM-5 | 20.5 | 69 | 62 | 65 | 114 | 82 | 198 | 151 | 279 | 416 | 780 | 297 | 198 |
| Olympus E-M1 Mark III | 20.5 | 67 | 60 | 64 | 112 | 79 | 198 | 150 | 277 | 416 | 777 | 296 | 198 |
| Pentax K-3 Mark III | 26.6 | 174 | 86 | 89 | 120 | 106 | 257 | 272 | 359 | 532 | 969 | 377 | 257 |
| Pentax K-1 Mark II | 36.6 | 248 | 119 | 123 | 153 | 138 | 352 | 397 | 503 | 746 | 1337 | 521 | 352 |
| Ricoh GR III | 24.8 | 236 | 84 | 87 | 166 | 102 | 235 | 328 | 339 | 501 | 911 | 357 | 235 |
| Leica Q2 | 47.4 | 184 | 178 | 181 | 212 | 199 | 454 | 374 | 675 | 991 | 1748 | 693 | 454 |
| Leica SL2 | 47.4 | 186 | 180 | 183 | 214 | 199 | 454 | 376 | 675 | 991 | 1751 | 693 | 454 |
| Leica M10-R | 41.0 | 392 | 135 | 138 | 248 | 154 | 394 | 544 | 566 | 841 | 1500 | 585 | 394 |
| Hasselblad X2D 100C (compressed) | 105.3 | 603 | 359 | 362 | ❌ | 378 | 1007 | 1001 | 1431 | 2113 | ❌ | 1449 | 1007 |
| Phase One IQ4 150MP (IIQ L) | 154.0 | 466 | 460 | ❌ | 655 | 479 | 1451 | 1060 | 2341 | 3353 | 5586 | 2359 | 1451 |
| Sigma fp L | 60.5 | 219 | 209 | 213 | 376 | 228 | 580 | 456 | 844 | 1249 | 2224 | 863 | 580 |
| Apple iPhone 12 Pro (ProRAW) | 12.2 | 126 | 125 | ❌ | 191 | 145 | 96 | 172 | 254 | 337 | 718 | 273 | 96 |
| Google Pixel 8 Pro | 50.1 | 158 | 143 | 148 | 290 | 163 | 481 | 354 | 671 | 1006 | 1823 | 689 | 481 |
| Google Pixel 6 Pro | 12.5 | 50 | 40 | 44 | 112 | 60 | 122 | 98 | 173 | 257 | 494 | 191 | 122 |
| Samsung Galaxy S23 Ultra | 49.9 | 728 | 462 | 370 | 688 | 482 | 384 | 746 | 987 | 1321 | 2403 | 1006 | 384 |
| Samsung Galaxy S22 (JPEG XL) | 12.0 | 691 | ❌ | ❌ | 514 | ❌ | ❌ | 735 | ❌ | ❌ | 988 | ❌ | ❌ |
| DJI Phantom 4 Pro | 20.0 | 86 | 81 | 84 | 115 | 100 | 193 | 218 | 292 | 425 | 763 | 311 | 193 |
| GoPro HERO11 Black (GPR) | 27.1 | 251 | ❌ | 282 | ❌ | ❌ | ❌ | 356 | ❌ | ❌ | ❌ | ❌ | ❌ |
| GoPro HERO10 Black (GPR) | 23.3 | 216 | ❌ | 239 | ❌ | ❌ | ❌ | 304 | ❌ | ❌ | ❌ | ❌ | ❌ |

## LibRaw + Little CMS pipeline, stages (all cores, ms)

decode = LibRaw `open_buffer` + `unpack`; develop = `dcraw_process` to 16-bit linear ProPhoto + `dcraw_make_mem_image`; Little CMS = profiles, transform and conversion to 8-bit sRGB RGBA; FlashRaw = RAW → RGBA8 in one pass.

| Camera | LibRaw decode | LibRaw develop | Little CMS | total | FlashRaw | speedup |
|---|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | 133.5 | 562.4 | 408.6 | 1106.9 | **101.3** | 10.9× |
| Canon EOS R5 (C-RAW) | 138.5 | 565.1 | 398.0 | 1097.7 | **114.9** | 9.55× |
| Canon EOS R5 Mark II (RAW) | 136.6 | 566.6 | 403.2 | 1104.9 | **105.5** | 10.5× |
| Canon EOS R6 Mark II (RAW) | 74.6 | 314.3 | 220.6 | 609.2 | **57.7** | 10.6× |
| Canon EOS R6 Mark III (RAW) | 97.0 | 411.9 | 291.2 | 799.8 | **75.2** | 10.6× |
| Canon EOS R7 (RAW) | 97.9 | 409.3 | 296.9 | 807.9 | **78.0** | 10.4× |
| Canon EOS R8 (RAW) | 73.4 | 315.1 | 221.2 | 611.5 | **58.7** | 10.4× |
| Canon EOS R50 (RAW) | 73.2 | 310.1 | 220.3 | 600.0 | **57.2** | 10.5× |
| Canon EOS 90D (RAW) | 96.6 | 407.8 | 312.2 | 811.7 | **47.6** | 17.0× |
| Canon EOS 5D Mark IV (RAW) | 436.0 | 384.2 | 281.8 | 1100.7 | **36.3** | 30.3× |
| Canon EOS 6D Mark II (RAW) | 392.6 | 337.8 | 234.1 | 969.8 | **31.1** | 31.2× |
| Canon EOS Rebel T7 / 2000D (RAW) | 396.2 | 311.3 | 213.6 | 920.2 | **31.5** | 29.2× |
| Nikon Z8 (14-bit lossless) | 553.8 | 580.0 | 420.5 | 1548.7 | **50.1** | 30.9× |
| Nikon Z9 (14-bit lossless) | 503.2 | 572.3 | 413.2 | 1494.0 | **50.4** | 29.6× |
| Nikon Z6III (lossless) | 282.8 | 319.4 | 223.6 | 823.1 | **27.8** | 29.6× |
| Nikon Zf (lossless) | 269.4 | 316.1 | 222.2 | 806.4 | **27.4** | 29.4× |
| Nikon Z7II (lossless) | 486.4 | 573.6 | 410.1 | 1470.4 | **48.7** | 30.2× |
| Nikon Z50 (lossless) | 245.4 | 271.9 | 188.3 | 712.2 | **23.9** | 29.8× |
| Nikon D850 (14-bit lossless) | 481.1 | 575.5 | 414.9 | 1469.2 | **48.1** | 30.6× |
| Nikon D750 (14-bit lossless) | 253.9 | 316.8 | 212.7 | 781.7 | **26.7** | 29.3× |
| Sony A7 IV (lossless) | 427.7 | 417.9 | 300.3 | 1141.6 | **36.0** | 31.7× |
| Sony A7 IV (compressed) | 85.6 | 423.7 | 295.8 | 803.1 | **21.9** | 36.7× |
| Sony A7R V (lossless) | 776.3 | 762.5 | 541.8 | 2083.8 | **62.4** | 33.4× |
| Sony A1 (compressed) | 129.3 | 628.6 | 444.9 | 1208.6 | **32.0** | 37.7× |
| Sony A7 III (compressed) | 64.1 | 312.3 | 220.6 | 600.3 | **16.3** | 36.9× |
| Sony A7C II (compressed) | 86.1 | 412.7 | 308.7 | 804.5 | **21.8** | 36.9× |
| Sony A9 III (compressed) | 63.1 | 312.9 | 219.4 | 596.9 | **17.3** | 34.6× |
| Sony A6700 (lossless) | 376.7 | 337.9 | 238.7 | 955.9 | **30.5** | 31.4× |
| Sony A6400 (compressed) | 63.5 | 311.9 | 218.4 | 591.2 | **16.7** | 35.4× |
| Sony ZV-E10 (compressed) | 63.5 | 310.6 | 216.9 | 590.9 | **14.0** | 42.1× |
| Fujifilm X-T5 (lossless compressed) | 149.2 | 505.2 | 363.7 | 1018.0 | **66.1** | 15.4× |
| Fujifilm X-H2 (compressed) | 173.9 | 666.7 | 357.6 | 1194.3 | **90.0** | 13.3× |
| Fujifilm X-S20 (lossless compressed) | 122.7 | 338.7 | 236.8 | 697.0 | **48.3** | 14.4× |
| Fujifilm X100VI (lossless compressed) | 152.1 | 697.3 | 352.9 | 1201.7 | **78.6** | 15.3× |
| Fujifilm X-T4 (uncompressed) | 20.3 | 331.0 | 232.1 | 585.8 | **23.8** | 24.6× |
| Fujifilm GFX100 II (lossless compressed) | 270.2 | 1276.2 | 888.1 | 2437.1 | **116.2** | 21.0× |
| Panasonic Lumix S5II | 132.3 | 352.2 | 215.3 | 696.7 | **25.1** | 27.7× |
| Panasonic Lumix GH6 | 133.1 | 363.9 | 230.0 | 720.6 | **26.5** | 27.2× |
| Panasonic Lumix G9II | 137.4 | 367.5 | 229.8 | 736.1 | **25.6** | 28.7× |
| Panasonic Lumix S5 | 8.89 | 350.7 | 218.4 | 581.6 | **15.3** | 37.9× |
| Panasonic Lumix S1R | 123.5 | 666.6 | 417.4 | 1208.6 | **29.3** | 41.3× |
| Panasonic Lumix GH5 | 95.3 | 297.8 | 185.1 | 578.7 | **15.7** | 36.8× |
| OM System OM-1 Mark II | 391.2 | 262.2 | 191.2 | 841.3 | **117.4** | 7.16× |
| OM System OM-5 | 348.1 | 269.7 | 185.6 | 802.6 | **119.7** | 6.70× |
| Olympus E-M1 Mark III | 342.7 | 266.0 | 191.3 | 803.1 | **125.3** | 6.41× |
| Pentax K-3 Mark III | 201.5 | 331.2 | 234.7 | 771.7 | **28.8** | 26.8× |
| Pentax K-1 Mark II | 288.8 | 460.8 | 330.3 | 1086.2 | **39.8** | 27.3× |
| Ricoh GR III | 322.1 | 312.3 | 220.5 | 852.6 | **26.6** | 32.0× |
| Leica Q2 | 291.3 | 588.3 | 419.1 | 1303.8 | **28.4** | 45.9× |
| Leica SL2 | 294.1 | 588.0 | 406.7 | 1290.6 | **28.2** | 45.7× |
| Leica M10-R | 560.0 | 525.3 | 370.3 | 1461.3 | **43.9** | 33.3× |
| Hasselblad X2D 100C (compressed) | 1015.7 | 1267.9 | 910.1 | 3190.2 | **121.2** | 26.3× |
| Phase One IQ4 150MP (IIQ L) | 1043.1 | 3302.2 | 1325.1 | 5674.2 | **116.6** | 48.7× |
| Sigma fp L | 767.8 | 762.3 | 538.3 | 2075.0 | **57.5** | 36.1× |
| Apple iPhone 12 Pro (ProRAW) | 453.5 | 110.5 | 109.2 | 669.6 | **27.0** | 24.8× |
| Google Pixel 8 Pro | 653.4 | 785.3 | 448.6 | 1884.1 | **45.9** | 41.1× |
| Google Pixel 6 Pro | 162.6 | 180.0 | 116.0 | 458.5 | **13.2** | 34.7× |
| Samsung Galaxy S23 Ultra | 1880.6 | 249.1 | 346.1 | 2480.2 | **194.0** | 12.8× |
| DJI Phantom 4 Pro | 19.4 | 262.5 | 185.7 | 463.1 | **19.0** | 24.4× |

## Per format (geometric mean speedup of FlashRaw, all cores)

| Format | Compression | files | decode vs LibRaw | decode vs RawSpeed | decode vs rawler | decode vs rawpy | decode vs dcraw | RGB vs LibRaw | RGB vs LibRaw + LCMS | RGB vs rawler | RGB vs rawpy | RGB vs dcraw | RGB, default demosaics (FlashRaw auto, LibRaw AHD / Markesteijn), vs LibRaw |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 3FR | hasselblad | 2 | 15.6× | 7.33× | – | – | – | 20.7× | – | – | – | – | – |
| 3FR | uncompressed | 2 | 0.94× | 0.55× | – | – | – | 22.0× | – | – | – | – | – |
| ARW | other | 2 | 20.9× | ❌ | – | – | – | 21.7× | – | – | – | – | – |
| ARW | sony-arw1 | 2 | 18.8× | 10.4× | – | – | – | 20.7× | – | – | – | – | – |
| ARW | sony-arw2 | 9 | 20.1× | 1.98× | 1.92× | 19.8× | 67.2× | 24.2× | 37.1× | 8.94× | 26.7× | 31.4× | – |
| ARW | sony-lossless | 4 | 19.1× | 0.70× | 2.22× | 23.6× | ❌ | 19.5× | 32.2× | 6.28× | 26.1× | ❌ | – |
| ARW | uncompressed | 2 | 9.05× | 0.40× | – | – | – | 15.3× | – | – | – | – | – |
| CR2 | canon-sraw | 2 | 17.3× | 4.18× | – | – | – | 14.4× | – | – | – | – | – |
| CR2 | lossless-jpeg | 5 | 21.8× | 6.24× | 13.4× | 22.0× | 31.9× | 23.1× | 30.2× | 12.2× | 24.6× | 27.7× | – |
| CR3 | canon-crx | 11 | 1.78× | ❌ | 3.03× | 5.78× | ❌ | 7.20× | 11.0× | 4.77× | 11.0× | ❌ | – |
| CRW | canon-crw | 2 | 3.28× | 2.47× | – | – | – | 5.97× | – | – | – | – | – |
| CRW | uncompressed | 1 | 12.2× | ❌ | – | – | – | 13.5× | – | – | – | – | – |
| DCR | kodak-65000 | 2 | 13.0× | 17.9× | – | – | – | 18.5× | – | – | – | – | – |
| DCS | lossless-jpeg | 1 | 17.1× | 4.59× | – | – | – | 21.0× | – | – | – | – | – |
| DCS | uncompressed | 2 | 4.49× | 36.3× | – | – | – | 21.2× | – | – | – | – | – |
| DNG | deflate | 2 | 20.1× | 1.60× | – | – | – | 21.5× | – | – | – | – | – |
| DNG | gopro-vc5 | 2 | ❌ | 1.49× | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | – |
| DNG | jpeg-xl | 1 | ❌ | ❌ | 0.63× | ❌ | ❌ | ❌ | ❌ | 0.75× | ❌ | ❌ | – |
| DNG | lossless-jpeg | 9 | 27.6× | 3.66× | 4.55× | 28.2× | 42.0× | 23.3× | 29.0× | 7.13× | 23.9× | 27.9× | – |
| DNG | uncompressed | 5 | 28.2× | 4.56× | 1.18× | 52.7× | 171.3× | 21.4× | 37.1× | 8.03× | 29.1× | 35.4× | – |
| ERF | uncompressed | 2 | 31.4× | 4.70× | – | – | – | 23.1× | – | – | – | – | – |
| FFF | hasselblad | 2 | 15.7× | 7.15× | ❌ | 17.0× | 35.7× | 20.0× | 26.3× | ❌ | 21.9× | 27.9× | – |
| IIQ | phaseone | 3 | 23.0× | 18.1× | 8.45× | 26.2× | 105.3× | 34.3× | 48.7× | 8.39× | 41.1× | 52.7× | – |
| IIQ | phaseone-s | 2 | 14.0× | ❌ | – | – | – | 30.2× | – | – | – | – | – |
| IIQ | uncompressed | 1 | 0.79× | ❌ | – | – | – | 37.1× | – | – | – | – | – |
| KDC | kodak-dc120 | 1 | 9.64× | ❌ | – | – | – | 5.38× | – | – | – | – | – |
| KDC | other | 1 | 1.71× | ❌ | – | – | – | 3.39× | – | – | – | – | – |
| KDC | uncompressed | 2 | 51.7× | 10.7× | – | – | – | 26.5× | – | – | – | – | – |
| MEF | uncompressed | 1 | 53.9× | 11.0× | – | – | – | 27.6× | – | – | – | – | – |
| MOS | leaf | 2 | 26.3× | ❌ | – | – | – | 24.5× | – | – | – | – | – |
| MOS | uncompressed | 1 | 11.8× | 11.3× | – | – | – | 20.9× | – | – | – | – | – |
| MRW | minolta-packed | 2 | 54.6× | 11.1× | – | – | – | 26.0× | – | – | – | – | – |
| MRW | uncompressed | 2 | 8.30× | 7.42× | – | – | – | 20.1× | – | – | – | – | – |
| NEF | nikon-huffman | 9 | 19.9× | 7.25× | 13.3× | 21.3× | 33.3× | 22.2× | 29.9× | 11.8× | 24.4× | 26.0× | – |
| NEF | uncompressed | 2 | 5.87× | 2.66× | – | – | – | 23.2× | – | – | – | – | – |
| NRW | uncompressed | 2 | 5.93× | 2.57× | – | – | – | 22.1× | – | – | – | – | – |
| ORF | olympus | 5 | 3.25× | 1.39× | 1.98× | 3.31× | 2.14× | 5.30× | 6.75× | 2.76× | 5.56× | 3.75× | – |
| ORF | uncompressed | 2 | 18.3× | 2.87× | – | – | – | 23.3× | – | – | – | – | – |
| PEF | pentax-huffman | 4 | 13.8× | 6.10× | 12.3× | 20.1× | 32.7× | 19.4× | 27.1× | 11.4× | 23.9× | 27.8× | – |
| PEF | uncompressed | 2 | 17.5× | 7.63× | – | – | – | 21.5× | – | – | – | – | – |
| RAF | fuji-compressed | 7 | 4.43× | 1.32× | 2.77× | 35.2× | ❌ | 11.1× | 15.7× | 5.38× | 27.7× | ❌ | – |
| RAF | uncompressed | 3 | 4.06× | 0.66× | 1.88× | 11.2× | 106.1× | 17.6× | 24.6× | 8.46× | 18.0× | 19.1× | – |
| RAW | uncompressed | 3 | 9.81× | 13.9× | – | – | – | 18.5× | – | – | – | – | – |
| RW2 | panasonic-v4 | 3 | 23.4× | 1.21× | 1.84× | 24.8× | 57.6× | 26.7× | 36.8× | 6.20× | 27.3× | 29.5× | – |
| RW2 | panasonic-v5 | 2 | 10.5× | 1.03× | – | – | – | 21.4× | – | – | – | – | – |
| RW2 | panasonic-v6 | 2 | 28.3× | 1.90× | 2.42× | 30.0× | 120.2× | 27.8× | 41.3× | 7.22× | 31.0× | 32.5× | – |
| RW2 | panasonic-v7 | 3 | 5.00× | 0.34× | 2.19× | 4.89× | 145.2× | 24.7× | 37.9× | 7.24× | 26.6× | 32.8× | – |
| RW2 | panasonic-v8 | 5 | 10.1× | 4.49× | 9.82× | 21.1× | 21.4× | 19.7× | 27.9× | 8.99× | 26.1× | 20.7× | – |
| RW2 | uncompressed | 2 | 7.26× | 0.72× | – | – | – | 18.8× | – | – | – | – | – |
| SRW | samsung-v0 | 2 | 7.13× | 2.50× | – | – | – | 6.95× | – | – | – | – | – |
| SRW | samsung-v1 | 2 | 3.12× | 1.27× | – | – | – | 5.81× | – | – | – | – | – |
| SRW | samsung-v2 | 2 | 2.48× | 1.24× | – | – | – | 4.45× | – | – | – | – | – |
| SRW | uncompressed | 2 | 20.8× | 4.02× | – | – | – | 12.1× | – | – | – | – | – |
| X3F | sigma-x3f | 2 | 10.1× | ❌ | – | – | – | 10.8× | – | – | – | – | – |
| other | kodak-ycbcr | 1 | 6.10× | ❌ | – | – | – | 7.07× | – | – | – | – | – |
| other | uncompressed | 2 | 7.18× | 11.6× | – | – | – | 18.5× | – | – | – | – | – |

## All files (FlashRaw, LibRaw, RawSpeed)

| File | Camera | Codec | MP | threads | Decode FlashRaw | LibRaw | RawSpeed | RGB FlashRaw | LibRaw | Peak RSS MB (decode) FR / LR / RS |
|---|---|---|---:|---|---:|---:|---:|---:|---:|---|
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | all | **77.5** | 134.2 | ❌ | **101.3** | 731.8 | 227 / 134 / ❌ |
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | 1 | **304.2** | 440.5 | ❌ | **641.1** | 1096.8 | 226 / 134 / ❌ |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | all | **91.6** | 138.0 | ❌ | **114.9** | 732.9 | 216 / 127 / ❌ |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | 1 | **331.1** | 362.6 | ❌ | **663.4** | 1021.9 | 214 / 123 / ❌ |
| RAW.CR3 | Canon EOS R5 Mark II | CR3 · canon-crx | 47.9 | all | **79.5** | 138.9 | ❌ | **105.5** | 736.1 | 247 / 151 / ❌ |
| RAW.CR3 | Canon EOS R5 Mark II | CR3 · canon-crx | 47.9 | 1 | **313.5** | 440.2 | ❌ | **646.1** | 1110.8 | 245 / 148 / ❌ |
| canon-r6m2-img_0346-raw-normal-full.cr3 | Canon EOS R6 Mark II | CR3 · canon-crx | 25.5 | all | **44.2** | 75.6 | ❌ | **57.7** | 406.4 | 137 / 83 / ❌ |
| canon-r6m2-img_0346-raw-normal-full.cr3 | Canon EOS R6 Mark II | CR3 · canon-crx | 25.5 | 1 | **169.4** | 234.2 | ❌ | **351.2** | 597.2 | 136 / 82 / ❌ |
| IMG_4241.CR3 | Canon EOS R6 Mark III | CR3 · canon-crx | 34.0 | all | **57.1** | 98.1 | ❌ | **75.2** | 539.1 | 173 / 104 / ❌ |
| IMG_4241.CR3 | Canon EOS R6 Mark III | CR3 · canon-crx | 34.0 | 1 | **225.1** | 313.4 | ❌ | **465.5** | 793.8 | 172 / 103 / ❌ |
| 443A0157.CR3 | Canon EOS R7 | CR3 · canon-crx | 33.7 | all | **57.2** | 98.5 | ❌ | **78.0** | 538.4 | 172 / 101 / ❌ |
| 443A0157.CR3 | Canon EOS R7 | CR3 · canon-crx | 33.7 | 1 | **225.2** | 308.5 | ❌ | **466.1** | 795.1 | 170 / 100 / ❌ |
| RAW_FULL_FRAME.CR3 | Canon EOS R8 | CR3 · canon-crx | 25.5 | all | **43.7** | 74.2 | ❌ | **58.7** | 404.3 | 137 / 82 / ❌ |
| RAW_FULL_FRAME.CR3 | Canon EOS R8 | CR3 · canon-crx | 25.5 | 1 | **167.4** | 241.2 | ❌ | **346.6** | 602.3 | 135 / 82 / ❌ |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | Canon EOS R50 | CR3 · canon-crx | 25.1 | all | **43.5** | 73.9 | ❌ | **57.2** | 402.9 | 138 / 85 / ❌ |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | Canon EOS R50 | CR3 · canon-crx | 25.1 | 1 | **167.5** | 227.7 | ❌ | **348.0** | 589.5 | 137 / 83 / ❌ |
| Canon-90D-RAW-ISO-100.CR3 | Canon EOS 90D | CR3 · canon-crx | 33.7 | all | **30.3** | 98.7 | ❌ | **47.6** | 538.7 | 174 / 105 / ❌ |
| Canon-90D-RAW-ISO-100.CR3 | Canon EOS 90D | CR3 · canon-crx | 33.7 | 1 | **221.5** | 310.9 | ❌ | **458.9** | 797.8 | 173 / 104 / ❌ |
| B13A0734.CR2 | Canon EOS 5D Mark IV | CR2 · lossless-jpeg | 31.3 | all | **20.0** | 434.8 | 110.8 | **36.3** | 844.4 | 276 / 97 / 99 |
| B13A0734.CR2 | Canon EOS 5D Mark IV | CR2 · lossless-jpeg | 31.3 | 1 | 208.9 | 434.2 | **110.6** | **434.9** | 882.5 | 132 / 96 / 99 |
| 6DII-RAW.CR2 | Canon EOS 6D Mark II | CR2 · lossless-jpeg | 27.0 | all | **17.0** | 391.1 | 120.6 | **31.1** | 746.6 | 231 / 92 / 95 |
| 6DII-RAW.CR2 | Canon EOS 6D Mark II | CR2 · lossless-jpeg | 27.0 | 1 | 193.6 | 392.3 | **120.8** | **387.8** | 779.1 | 128 / 91 / 95 |
| IMG_0662.CR2 | Canon EOS Rebel T7 | CR2 · lossless-jpeg | 24.7 | all | **18.4** | 397.4 | 123.2 | **31.5** | 726.3 | 250 / 89 / 92 |
| IMG_0662.CR2 | Canon EOS Rebel T7 | CR2 · lossless-jpeg | 24.7 | 1 | 193.1 | 400.8 | **124.4** | **372.2** | 757.7 | 125 / 88 / 92 |
| Nikon_Z8_raw_14_bit_lossless_compression.NEF | Nikon Z 8 | NEF · nikon-huffman | 45.7 | all | **25.7** | 544.4 | 220.1 | **50.1** | 1156.2 | 295 / 150 / 153 |
| Nikon_Z8_raw_14_bit_lossless_compression.NEF | Nikon Z 8 | NEF · nikon-huffman | 45.7 | 1 | **205.5** | 545.7 | 218.5 | **541.4** | 1219.0 | 155 / 149 / 153 |
| Nikon_-_NIKON_Z_9_-_14bit_compressed_(Lossless).NEF | Nikon Z 9 | NEF · nikon-huffman | 45.7 | all | **24.7** | 503.8 | 180.8 | **50.4** | 1110.5 | 287 / 147 / 150 |
| Nikon_-_NIKON_Z_9_-_14bit_compressed_(Lossless).NEF | Nikon Z 9 | NEF · nikon-huffman | 45.7 | 1 | **170.4** | 502.1 | 181.1 | **505.0** | 1182.1 | 152 / 146 / 150 |
| Nikon_Z6__3_Lossless_compressed_FX.NEF | Nikon Z6_3 | NEF · nikon-huffman | 24.5 | all | **14.7** | 282.0 | 108.7 | **27.8** | 616.1 | 174 / 83 / 86 |
| Nikon_Z6__3_Lossless_compressed_FX.NEF | Nikon Z6_3 | NEF · nikon-huffman | 24.5 | 1 | 110.7 | 284.8 | **108.1** | **291.6** | 649.4 | 88 / 82 / 86 |
| DSC_0040.NEF | Nikon Z f | NEF · nikon-huffman | 24.5 | all | **13.3** | 268.6 | 96.2 | **27.4** | 603.2 | 154 / 80 / 84 |
| DSC_0040.NEF | Nikon Z f | NEF · nikon-huffman | 24.5 | 1 | **88.6** | 268.3 | 94.4 | **271.1** | 635.2 | 86 / 81 / 84 |
| Z72_0027.NEF | Nikon Z 7_2 | NEF · nikon-huffman | 45.7 | all | **23.6** | 484.1 | 168.5 | **48.7** | 1093.1 | 273 / 144 / 146 |
| Z72_0027.NEF | Nikon Z 7_2 | NEF · nikon-huffman | 45.7 | 1 | **162.1** | 484.8 | 169.0 | **493.7** | 1162.1 | 148 / 143 / 147 |
| DSC_0450.NEF | Nikon Z 50 | NEF · nikon-huffman | 20.9 | all | **12.0** | 245.4 | 93.6 | **23.9** | 531.3 | 140 / 74 / 77 |
| DSC_0450.NEF | Nikon Z 50 | NEF · nikon-huffman | 20.9 | 1 | **85.4** | 246.8 | 93.6 | **237.9** | 558.9 | 79 / 73 / 78 |
| Nikon-D850-14bit-lossless-compressed.NEF | Nikon D850 | NEF · nikon-huffman | 45.7 | all | **23.7** | 481.1 | 168.5 | **48.1** | 1096.8 | 276 / 145 / 148 |
| Nikon-D850-14bit-lossless-compressed.NEF | Nikon D850 | NEF · nikon-huffman | 45.7 | 1 | **162.8** | 481.9 | 168.8 | **500.4** | 1162.7 | 149 / 144 / 148 |
| lossless_compressed_14_bit.NEF | Nikon D750 | NEF · nikon-huffman | 24.3 | all | **13.5** | 254.2 | 81.2 | **26.7** | 592.2 | 155 / 76 / 79 |
| lossless_compressed_14_bit.NEF | Nikon D750 | NEF · nikon-huffman | 24.3 | 1 | 85.7 | 251.8 | **82.1** | **267.8** | 617.3 | 81 / 76 / 79 |
| ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW | Sony ILCE-7M4 | ARW · sony-lossless | 36.7 | all | 19.0 | 426.5 | **12.3** | **36.0** | 867.4 | 140 / 122 / 126 |
| ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW | Sony ILCE-7M4 | ARW · sony-lossless | 36.7 | 1 | 272.7 | 429.4 | **145.8** | **513.8** | 913.0 | 127 / 121 / 125 |
| ILCE-7M4_DSC06677_FullFrame-Raw-Compressed.ARW | Sony ILCE-7M4 | ARW · sony-arw2 | 33.0 | all | **4.21** | 87.7 | 8.02 | **21.9** | 530.5 | 112 / 106 / 110 |
| ILCE-7M4_DSC06677_FullFrame-Raw-Compressed.ARW | Sony ILCE-7M4 | ARW · sony-arw2 | 33.0 | 1 | **42.0** | 84.0 | 91.5 | **286.3** | 577.0 | 111 / 106 / 110 |
| 7RM5-LosslessCompressedLarge.ARW | Sony ILCE-7RM5 | ARW · sony-lossless | 64.7 | all | 31.0 | 775.4 | **22.1** | **62.4** | 1587.0 | 230 / 210 / 214 |
| 7RM5-LosslessCompressedLarge.ARW | Sony ILCE-7RM5 | ARW · sony-lossless | 64.7 | 1 | 483.6 | 774.9 | **265.9** | **939.9** | 1681.7 | 216 / 210 / 213 |
| A1_full_compressed.ARW | Sony ILCE-1 | ARW · sony-arw2 | 50.2 | all | **6.10** | 130.4 | 11.6 | **32.0** | 804.5 | 166 / 160 / 165 |
| A1_full_compressed.ARW | Sony ILCE-1 | ARW · sony-arw2 | 50.2 | 1 | **65.6** | 128.7 | 135.0 | **432.2** | 866.0 | 165 / 160 / 163 |
| _DSC0009.ARW | Sony ILCE-7M3 | ARW · sony-arw2 | 24.3 | all | **3.27** | 64.0 | 6.61 | **16.3** | 395.3 | 81 / 76 / 79 |
| _DSC0009.ARW | Sony ILCE-7M3 | ARW · sony-arw2 | 24.3 | 1 | **31.8** | 63.5 | 67.3 | **209.4** | 423.8 | 80 / 75 / 79 |
| DSC02435.ARW | Sony ILCE-7CM2 | ARW · sony-arw2 | 33.0 | all | **4.21** | 87.4 | 8.12 | **21.8** | 527.5 | 111 / 106 / 110 |
| DSC02435.ARW | Sony ILCE-7CM2 | ARW · sony-arw2 | 33.0 | 1 | **42.7** | 87.1 | 89.3 | **284.3** | 573.6 | 111 / 105 / 109 |
| DSC00111.ARW | Sony ILCE-9M3 | ARW · sony-arw2 | 24.3 | all | **3.46** | 63.2 | 6.93 | **17.3** | 393.8 | 84 / 78 / 83 |
| DSC00111.ARW | Sony ILCE-9M3 | ARW · sony-arw2 | 24.3 | 1 | **31.8** | 63.6 | 68.7 | **206.8** | 418.5 | 84 / 78 / 82 |
| DSC00001.ARW | Sony ILCE-6700 | ARW · sony-lossless | 30.7 | all | 16.4 | 374.7 | **12.2** | **30.5** | 727.4 | 124 / 104 / 109 |
| DSC00001.ARW | Sony ILCE-6700 | ARW · sony-lossless | 30.7 | 1 | 243.1 | 374.2 | **137.9** | **429.8** | 762.3 | 111 / 104 / 108 |
| DSC00087.ARW | Sony ILCE-6400 | ARW · sony-arw2 | 24.3 | all | **3.19** | 64.1 | 6.32 | **16.7** | 388.8 | 80 / 75 / 78 |
| DSC00087.ARW | Sony ILCE-6400 | ARW · sony-arw2 | 24.3 | 1 | **31.6** | 63.5 | 62.5 | **208.0** | 423.8 | 79 / 74 / 78 |
| DSC00002.ARW | Sony ZV-E10 | ARW · sony-arw2 | 24.3 | all | **3.31** | 63.8 | 6.45 | **14.0** | 394.3 | 80 / 75 / 79 |
| DSC00002.ARW | Sony ZV-E10 | ARW · sony-arw2 | 24.3 | 1 | **31.5** | 63.6 | 62.7 | **180.8** | 422.4 | 79 / 74 / 78 |
| DSCF0021.RAF | Fujifilm X-T5 | RAF · fuji-compressed | 40.9 | all | **33.3** | 149.0 | 44.2 | **66.1** | 692.2 | 130 / 126 / 128 |
| DSCF0021.RAF | Fujifilm X-T5 | RAF · fuji-compressed | 40.9 | 1 | **354.6** | 1167.5 | 468.8 | **829.8** | 1795.5 | 128 / 123 / 127 |
| Fuji-X-H2-Compressed-Raw.RAF | Fujifilm X-H2 | RAF · fuji-compressed | 40.9 | all | **44.4** | 174.5 | ❌ | **90.0** | 881.8 | 117 / 112 / ❌ |
| Fuji-X-H2-Compressed-Raw.RAF | Fujifilm X-H2 | RAF · fuji-compressed | 40.9 | 1 | **471.4** | 1459.9 | ❌ | **1156.4** | 2257.0 | 115 / 111 / ❌ |
| DSCF2526.RAF | Fujifilm X-S20 | RAF · fuji-compressed | 26.5 | all | **26.7** | 122.4 | 36.3 | **48.3** | 476.0 | 93 / 90 / 91 |
| DSCF2526.RAF | Fujifilm X-S20 | RAF · fuji-compressed | 26.5 | 1 | **229.2** | 793.4 | 308.7 | **543.4** | 1205.4 | 92 / 87 / 90 |
| DSCF0075.RAF | Fujifilm X100VI | RAF · fuji-compressed | 40.9 | all | **33.3** | 151.8 | 44.5 | **78.6** | 897.3 | 136 / 131 / 134 |
| DSCF0075.RAF | Fujifilm X100VI | RAF · fuji-compressed | 40.9 | 1 | **353.2** | 1172.9 | 468.6 | **1034.5** | 2002.7 | 134 / 129 / 133 |
| DSCF0266.RAF | Fujifilm X-T4 | RAF · uncompressed | 26.7 | all | 1.76 | 19.6 | **1.24** | **23.8** | 376.1 | 116 / 111 / 113 |
| DSCF0266.RAF | Fujifilm X-T4 | RAF · uncompressed | 26.7 | 1 | 1.93 | 19.7 | **1.19** | **316.9** | 426.6 | 115 / 110 / 113 |
| _DSF0267.RAF | Fujifilm GFX100 II | RAF · fuji-compressed | 103.4 | all | **61.8** | 269.1 | 78.8 | **116.2** | 1601.1 | 314 / 311 / 312 |
| _DSF0267.RAF | Fujifilm GFX100 II | RAF · fuji-compressed | 103.4 | 1 | **833.0** | 2584.7 | 1024.4 | **1578.4** | 4090.0 | 313 / 307 / 311 |
| P1126458_mechanical.RW2 | Panasonic DC-S5M2 | RW2 · panasonic-v8 | 24.1 | all | **12.1** | 130.8 | 61.2 | **25.1** | 500.8 | 239 / 91 / 89 |
| P1126458_mechanical.RW2 | Panasonic DC-S5M2 | RW2 · panasonic-v8 | 24.1 | 1 | 132.7 | 247.9 | **116.0** | **309.9** | 639.0 | 91 / 88 / 89 |
| 4-3.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 25.2 | all | **12.4** | 132.0 | 62.3 | **26.5** | 514.4 | 231 / 99 / 99 |
| 4-3.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 25.2 | 1 | 137.1 | 261.4 | **121.8** | **324.8** | 671.4 | 100 / 96 / 98 |
| P1000019.RW2 | Panasonic DC-G9M2 | RW2 · panasonic-v8 | 25.1 | all | **12.4** | 136.8 | 56.7 | **25.6** | 521.9 | 227 / 96 / 95 |
| P1000019.RW2 | Panasonic DC-G9M2 | RW2 · panasonic-v8 | 25.1 | 1 | 136.1 | 267.7 | **108.1** | **325.3** | 679.8 | 97 / 93 / 95 |
| P1047510.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 24.3 | all | 1.78 | 8.74 | **0.61** | **15.3** | 381.5 | 97 / 93 / 96 |
| P1047510.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 24.3 | 1 | 4.49 | 8.70 | **3.77** | **181.9** | 402.0 | 97 / 91 / 95 |
| P1000077.RW2 | Panasonic DC-S1R | RW2 · panasonic-v6 | 47.6 | all | **4.13** | 123.5 | 7.67 | **29.3** | 820.9 | 167 / 162 / 165 |
| P1000077.RW2 | Panasonic DC-S1R | RW2 · panasonic-v6 | 47.6 | 1 | **37.7** | 123.0 | 87.1 | **381.7** | 890.7 | 166 / 162 / 165 |
| _T012010.RW2 | Panasonic DC-GH5 | RW2 · panasonic-v4 | 20.6 | all | **3.82** | 94.7 | 4.79 | **15.7** | 412.2 | 72 / 66 / 71 |
| _T012010.RW2 | Panasonic DC-GH5 | RW2 · panasonic-v4 | 20.6 | 1 | **42.5** | 92.7 | 46.2 | **192.5** | 422.2 | 71 / 66 / 70 |
| 20mp-12bit.orf | OM System OM-1 Mark II | ORF · olympus | 20.4 | all | **106.6** | 384.7 | 161.8 | **117.4** | 671.1 | 73 / 64 / 68 |
| 20mp-12bit.orf | OM System OM-1 Mark II | ORF · olympus | 20.4 | 1 | **134.2** | 388.7 | 159.2 | **284.1** | 689.4 | 70 / 65 / 68 |
| C1291096.ORF | OM System OM-5 | ORF · olympus | 20.5 | all | **106.8** | 346.5 | 152.8 | **119.7** | 634.2 | 69 / 62 / 65 |
| C1291096.ORF | OM System OM-5 | ORF · olympus | 20.5 | 1 | **135.8** | 350.3 | 153.5 | **289.3** | 656.9 | 67 / 62 / 65 |
| _3160531.ORF | Olympus E-M1MarkIII | ORF · olympus | 20.5 | all | **113.4** | 339.6 | 147.4 | **125.3** | 622.8 | 67 / 60 / 64 |
| _3160531.ORF | Olympus E-M1MarkIII | ORF · olympus | 20.5 | 1 | **135.8** | 346.1 | 147.3 | **288.1** | 650.4 | 66 / 60 / 64 |
| _IMG0115.PEF | Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | all | **14.5** | 199.8 | 85.9 | **28.8** | 559.3 | 174 / 86 / 89 |
| _IMG0115.PEF | Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | 1 | 91.8 | 205.2 | **86.4** | **282.4** | 586.8 | 91 / 86 / 89 |
| _K1I9780.PEF | Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | all | **20.6** | 290.5 | 131.6 | **39.8** | 779.0 | 248 / 119 / 123 |
| _K1I9780.PEF | Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | 1 | 140.1 | 287.2 | **131.5** | **407.1** | 832.3 | 124 / 119 / 123 |
| R0000357.DNG | Ricoh GR III | DNG · lossless-jpeg | 24.8 | all | **13.1** | 323.2 | 99.8 | **26.6** | 653.3 | 236 / 84 / 87 |
| R0000357.DNG | Ricoh GR III | DNG · lossless-jpeg | 24.8 | 1 | **93.1** | 325.2 | 99.0 | **269.8** | 682.0 | 117 / 83 / 87 |
| L1000750.DNG | Leica Q2 | DNG · uncompressed | 47.4 | all | **3.90** | 293.3 | 49.5 | **28.4** | 924.4 | 184 / 178 / 181 |
| L1000750.DNG | Leica Q2 | DNG · uncompressed | 47.4 | 1 | **11.8** | 291.7 | 49.8 | **353.9** | 993.4 | 183 / 178 / 181 |
| L1020010.DNG | Leica SL2 | DNG · uncompressed | 47.4 | all | **3.93** | 293.0 | 49.6 | **28.2** | 925.5 | 186 / 180 / 183 |
| L1020010.DNG | Leica SL2 | DNG · uncompressed | 47.4 | 1 | **11.7** | 292.2 | 49.5 | **356.0** | 990.6 | 185 / 179 / 183 |
| M10R0324.DNG | Leica M10-R | DNG · lossless-jpeg | 41.0 | all | **20.6** | 558.6 | 202.5 | **43.9** | 1113.5 | 392 / 135 / 138 |
| M10R0324.DNG | Leica M10-R | DNG · lossless-jpeg | 41.0 | 1 | **156.8** | 558.0 | 203.2 | **461.4** | 1170.9 | 188 / 135 / 138 |
| Job_0079.fff | Hasselblad X2D 100C | FFF · hasselblad | 105.3 | all | **65.4** | 1010.0 | 454.2 | **121.2** | 2369.1 | 603 / 359 / 362 |
| Job_0079.fff | Hasselblad X2D 100C | FFF · hasselblad | 105.3 | 1 | 671.4 | 1016.6 | **451.7** | **1431.6** | 2517.3 | 366 / 359 / 362 |
| IQ4_150MP_L_14.iiq | Phase One IQ4 150MP | IIQ · phaseone | 154.0 | all | **40.4** | 1031.5 | ❌ | **116.6** | 4463.9 | 466 / 460 / ❌ |
| IQ4_150MP_L_14.iiq | Phase One IQ4 150MP | IIQ · phaseone | 154.0 | 1 | **562.5** | 1028.3 | ❌ | **1678.7** | 4701.6 | 465 / 458 / ❌ |
| SDIM0915.DNG | Sigma fp L | DNG · lossless-jpeg | 60.5 | all | **18.1** | 760.7 | 25.4 | **57.5** | 1558.3 | 219 / 209 / 213 |
| SDIM0915.DNG | Sigma fp L | DNG · lossless-jpeg | 60.5 | 1 | **226.1** | 761.2 | 232.6 | **737.5** | 1664.3 | 214 / 208 / 211 |
| IMG_1361.DNG | Apple iPhone 12 Pro | DNG · lossless-jpeg | 12.2 | all | **18.6** | 464.0 | ❌ | **27.0** | 595.1 | 126 / 125 / ❌ |
| IMG_1361.DNG | Apple iPhone 12 Pro | DNG · lossless-jpeg | 12.2 | 1 | **265.8** | 467.1 | ❌ | **371.5** | 593.9 | 108 / 126 / ❌ |
| PXL_20240415_103400204.RAW-02.ORIGINAL.dng | Google Pixel 8 Pro | DNG · lossless-jpeg | 50.1 | all | **14.9** | 656.2 | 16.6 | **45.9** | 1476.6 | 158 / 143 / 148 |
| PXL_20240415_103400204.RAW-02.ORIGINAL.dng | Google Pixel 8 Pro | DNG · lossless-jpeg | 50.1 | 1 | **182.6** | 650.2 | 189.5 | **608.9** | 1541.4 | 152 / 143 / 146 |
| PXL_20220910_093206982.dng | Google Pixel 6 Pro | DNG · lossless-jpeg | 12.5 | all | **4.41** | 164.7 | 5.22 | **13.2** | 353.5 | 50 / 40 / 44 |
| PXL_20220910_093206982.dng | Google Pixel 6 Pro | DNG · lossless-jpeg | 12.5 | 1 | **46.6** | 163.4 | 50.5 | **151.8** | 348.0 | 45 / 40 / 43 |
| 20231214_130645.dng | Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 49.9 | all | **150.9** | 1899.0 | 581.0 | **194.0** | 2191.1 | 728 / 462 / 370 |
| 20231214_130645.dng | Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 49.9 | 1 | 1125.1 | 1901.7 | **889.3** | **1737.0** | 2184.2 | 441 / 462 / 370 |
| 20250305_173647.dng | Samsung SM-S901E | DNG · jpeg-xl | 12.0 | all | **284.8** | ❌ | ❌ | **296.1** | ❌ | 691 / ❌ / ❌ |
| 20250305_173647.dng | Samsung SM-S901E | DNG · jpeg-xl | 12.0 | 1 | **571.6** | ❌ | ❌ | **752.7** | ❌ | 714 / ❌ / ❌ |
| DJI_0220.DNG | DJI FC6310 | DNG · uncompressed | 20.0 | all | 1.13 | 17.7 | **1.06** | **19.0** | 290.9 | 86 / 81 / 84 |
| DJI_0220.DNG | DJI FC6310 | DNG · uncompressed | 20.0 | 1 | 1.45 | 17.9 | **1.00** | **223.7** | 312.7 | 86 / 80 / 84 |
| GOPR0123.GPR | GoPro HERO11 Black | DNG · gopro-vc5 | 27.1 | all | **17.5** | ❌ | 23.8 | **35.7** | ❌ | 251 / ❌ / 282 |
| GOPR0123.GPR | GoPro HERO11 Black | DNG · gopro-vc5 | 27.1 | 1 | **127.8** | ❌ | 164.1 | **361.6** | ❌ | 246 / ❌ / 283 |
| GOPR0018.GPR | GoPro HERO10 Black | DNG · gopro-vc5 | 23.3 | all | **15.8** | ❌ | 25.6 | **35.1** | ❌ | 216 / ❌ / 239 |
| GOPR0018.GPR | GoPro HERO10 Black | DNG · gopro-vc5 | 23.3 | 1 | **106.6** | ❌ | 154.4 | **306.2** | ❌ | 226 / ❌ / 244 |
| A9391120.3FR | Hasselblad H3D | 3FR · hasselblad | 39.6 | all | **23.0** | 355.8 | 166.0 | **43.7** | 885.3 | 240 / 132 / 134 |
| A9391120.3FR | Hasselblad H3D | 3FR · hasselblad | 39.6 | 1 | 246.6 | 356.2 | **164.2** | **534.6** | 936.4 | 136 / 133 / 134 |
| A0000902.3FR | Hasselblad H3D | 3FR · hasselblad | 60.9 | all | **33.8** | 533.8 | 252.1 | **63.7** | 1344.2 | 358 / 198 / 201 |
| A0000902.3FR | Hasselblad H3D | 3FR · hasselblad | 60.9 | 1 | 368.2 | 538.7 | **252.8** | **812.0** | 1418.9 | 203 / 197 / 201 |
| B0003499.3FR | Hasselblad CFV 100C | 3FR · uncompressed | 105.3 | all | 9.62 | 9.25 | **5.25** | **62.3** | 1353.2 | 417 / 407 / 410 |
| B0003499.3FR | Hasselblad CFV 100C | 3FR · uncompressed | 105.3 | 1 | 14.7 | 8.60 | **5.22** | **755.7** | 1523.0 | 416 / 406 / 410 |
| B0000080.3FR | Hasselblad X2D 100C | 3FR · uncompressed | 105.3 | all | 9.83 | 8.95 | **5.45** | **61.2** | 1360.9 | 419 / 408 / 412 |
| B0000080.3FR | Hasselblad X2D 100C | 3FR · uncompressed | 105.3 | 1 | 15.2 | 9.04 | **4.96** | **769.3** | 1531.7 | 417 / 407 / 412 |
| full_compressed.ARW | Sony ILCE-7M5 | ARW · other | 33.0 | all | **20.6** | 444.0 | ❌ | **40.9** | 895.4 | 371 / 692 / ❌ |
| full_compressed.ARW | Sony ILCE-7M5 | ARW · other | 33.0 | 1 | **224.8** | 441.8 | ❌ | **466.4** | 931.1 | 354 / 680 / ❌ |
| full_compressed_HQ.ARW | Sony ILCE-7M5 | ARW · other | 33.0 | all | **21.5** | 435.6 | ❌ | **42.1** | 902.7 | 386 / 698 / ❌ |
| full_compressed_HQ.ARW | Sony ILCE-7M5 | ARW · other | 33.0 | 1 | **226.4** | 446.7 | ❌ | **468.8** | 924.0 | 354 / 702 / ❌ |
| DSC05353.ARW | Sony DSLR-A380 | ARW · sony-arw1 | 14.1 | all | **9.49** | 189.3 | 103.4 | **19.2** | 392.0 | 105 / 51 / 53 |
| DSC05353.ARW | Sony DSLR-A380 | ARW · sony-arw1 | 14.1 | 1 | **69.3** | 189.0 | 104.1 | **173.9** | 399.2 | 55 / 50 / 53 |
| RAW_SONY_A390.ARW | Sony DSLR-A390 | ARW · sony-arw1 | 14.1 | all | **9.92** | 175.4 | 98.2 | **17.8** | 374.3 | 103 / 45 / 48 |
| RAW_SONY_A390.ARW | Sony DSLR-A390 | ARW · sony-arw1 | 14.1 | 1 | **67.8** | 176.4 | 98.9 | **173.6** | 387.8 | 50 / 44 / 48 |
| SONY ILCE-7M2 tungsten.arw | Sony ILCE-7M2 | ARW · sony-arw2 | 24.3 | all | **3.40** | 64.3 | 6.46 | **18.3** | 399.3 | 80 / 75 / 79 |
| SONY ILCE-7M2 tungsten.arw | Sony ILCE-7M2 | ARW · sony-arw2 | 24.3 | 1 | **32.0** | 63.8 | 63.4 | **213.3** | 426.7 | 79 / 74 / 78 |
| 7RM5-LossyCompressed.ARW | Sony ILCE-7RM5 | ARW · sony-arw2 | 61.2 | all | **7.39** | 162.4 | 16.7 | **40.4** | 978.6 | 196 / 190 / 194 |
| 7RM5-LossyCompressed.ARW | Sony ILCE-7RM5 | ARW · sony-arw2 | 61.2 | 1 | **79.8** | 161.1 | 168.2 | **525.2** | 1066.4 | 195 / 190 / 193 |
| DSC00800.ARW | Sony ILCE-7CR | ARW · sony-lossless | 18.4 | all | **26.1** | 269.9 | ❌ | **36.3** | 359.0 | 347 / 186 / ❌ |
| DSC00800.ARW | Sony ILCE-7CR | ARW · sony-lossless | 18.4 | 1 | **209.7** | 271.0 | ❌ | **312.3** | 359.4 | 295 / 185 / ❌ |
| DSC03792.ARW | Sony ILCE-7RM3 | ARW · uncompressed | 42.6 | all | 2.37 | 30.8 | **2.06** | **25.6** | 592.3 | 173 / 168 / 170 |
| DSC03792.ARW | Sony ILCE-7RM3 | ARW · uncompressed | 42.6 | 1 | 3.14 | 31.6 | **2.08** | **313.4** | 662.0 | 172 / 168 / 170 |
| Indoor_Multi-20190712-094102.ARQ | Sony ILCE-7RM3 | ARW · uncompressed | 42.6 | all | 10.6 | 66.6 | **1.92** | **33.0** | 335.1 | 660 / 655 / 414 |
| Indoor_Multi-20190712-094102.ARQ | Sony ILCE-7RM3 | ARW · uncompressed | 42.6 | 1 | 20.1 | 66.2 | **2.08** | **326.5** | 333.5 | 659 / 654 / 414 |
| 5G4A9395.CR2 | Canon EOS 5D Mark III | CR2 · canon-sraw | 10.5 | all | **16.5** | 280.5 | 64.5 | **23.5** | 341.4 | 184 / 100 / 113 |
| 5G4A9395.CR2 | Canon EOS 5D Mark III | CR2 · canon-sraw | 10.5 | 1 | 130.7 | 281.5 | **76.7** | **203.8** | 340.1 | 96 / 99 / 112 |
| _DSR2003.CR2 | Canon EOS 5DS R | CR2 · canon-sraw | 28.0 | all | **43.6** | 763.2 | 194.4 | **63.8** | 916.2 | 460 / 262 / 291 |
| _DSR2003.CR2 | Canon EOS 5DS R | CR2 · canon-sraw | 28.0 | 1 | 364.6 | 760.7 | **250.5** | **549.0** | 904.7 | 251 / 263 / 290 |
| B13A0729.CR2 | Canon EOS 5D Mark IV | CR2 · lossless-jpeg | 31.3 | all | **19.3** | 432.0 | 109.6 | **36.6** | 847.7 | 307 / 123 / 126 |
| B13A0729.CR2 | Canon EOS 5D Mark IV | CR2 · lossless-jpeg | 31.3 | 1 | 209.0 | 436.1 | **108.5** | **434.2** | 881.2 | 158 / 123 / 126 |
| IMG_0493.CR2 | Canon EOS Rebel T1i | CR2 · lossless-jpeg | 15.5 | all | **12.1** | 248.7 | 76.8 | **20.8** | 465.0 | 162 / 57 / 59 |
| IMG_0493.CR2 | Canon EOS Rebel T1i | CR2 · lossless-jpeg | 15.5 | 1 | 123.5 | 248.2 | **76.3** | **228.6** | 471.7 | 81 / 56 / 59 |
| Canon_EOS_R5_RAW_ISO_100_nocrop_dual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | all | **77.9** | 134.4 | ❌ | **103.9** | 733.6 | 258 / 165 / ❌ |
| Canon_EOS_R5_RAW_ISO_100_nocrop_dual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | 1 | **307.3** | 442.1 | ❌ | **636.2** | 1098.4 | 257 / 163 / ❌ |
| canon-r6m2-img_0355-craw-dual-full.cr3 | Canon EOS R6 Mark II | CR3 · canon-crx | 25.5 | all | **51.4** | 80.0 | ❌ | **65.3** | 405.0 | 142 / 92 / ❌ |
| canon-r6m2-img_0355-craw-dual-full.cr3 | Canon EOS R6 Mark II | CR3 · canon-crx | 25.5 | 1 | **177.6** | 199.0 | ❌ | **356.9** | 558.6 | 139 / 87 / ❌ |
| CRW_7673.CRW | Canon EOS 10D | CRW · canon-crw | 6.5 | all | **25.5** | 83.7 | 62.4 | **29.6** | 179.7 | 31 / 25 / 27 |
| CRW_7673.CRW | Canon EOS 10D | CRW · canon-crw | 6.5 | 1 | **25.2** | 84.9 | 62.5 | **73.8** | 181.7 | 30 / 24 / 27 |
| crw_2127.crw | Canon EOS 10D | CRW · canon-crw | 6.5 | all | **25.2** | 82.8 | 63.2 | **29.2** | 171.7 | 34 / 27 / 30 |
| crw_2127.crw | Canon EOS 10D | CRW · canon-crw | 6.5 | 1 | **25.4** | 82.1 | 62.6 | **72.4** | 179.5 | 33 / 27 / 30 |
| RAW_CANON_PRO70_SRGB.CRW | Canon PowerShot Pro70 | CRW · uncompressed | 1.6 | all | **0.37** | 4.50 | ❌ | **2.96** | 40.0 | 15 / 10 / ❌ |
| RAW_CANON_PRO70_SRGB.CRW | Canon PowerShot Pro70 | CRW · uncompressed | 1.6 | 1 | **0.44** | 4.67 | ❌ | **23.6** | 37.2 | 14 / 9 / ❌ |
| D4C50031.DCR | Kodak DCS Pro 14n | DCR · kodak-65000 | 13.6 | all | **4.79** | 61.8 | 85.0 | **13.1** | 251.0 | 51 / 46 / 49 |
| D4C50031.DCR | Kodak DCS Pro 14n | DCR · kodak-65000 | 13.6 | 1 | **32.5** | 63.1 | 84.4 | **133.3** | 262.4 | 51 / 45 / 49 |
| D7465857.DCR | Kodak DCS Pro 14nx | DCR · kodak-65000 | 13.6 | all | **4.55** | 60.0 | 82.5 | **13.6** | 244.4 | 50 / 44 / 47 |
| D7465857.DCR | Kodak DCS Pro 14nx | DCR · kodak-65000 | 13.6 | 1 | **32.5** | 64.8 | 82.1 | **135.3** | 264.4 | 49 / 44 / 47 |
| RAW_KODAK_DCS560C.TIF | Kodak DCS560C | DCS · lossless-jpeg | 6.1 | all | **4.94** | 84.5 | 22.7 | **8.75** | 183.7 | 71 / 22 / 25 |
| RAW_KODAK_DCS560C.TIF | Kodak DCS560C | DCS · lossless-jpeg | 6.1 | 1 | **20.8** | 84.7 | 35.1 | **67.0** | 176.9 | 32 / 20 / 25 |
| RAW_KODAK_DCS460D_FILEVERSION_3.TIF | Kodak DCS460D | DCS · uncompressed | 6.3 | all | **0.63** | 2.99 | 24.5 | **5.59** | 124.4 | 28 / 23 / 26 |
| RAW_KODAK_DCS460D_FILEVERSION_3.TIF | Kodak DCS460D | DCS · uncompressed | 6.3 | 1 | **1.99** | 3.09 | 24.6 | **48.7** | 116.8 | 27 / 23 / 26 |
| RAW_CANON_DCS1.TIF | Kodak EOS DCS 1 | DCS · uncompressed | 6.3 | all | **0.72** | 3.09 | 24.5 | **5.75** | 116.2 | 28 / 23 / 26 |
| RAW_CANON_DCS1.TIF | Kodak EOS DCS 1 | DCS · uncompressed | 6.3 | 1 | **2.03** | 3.01 | 24.7 | **48.9** | 117.5 | 27 / 22 / 26 |
| hdrmerge-bayer-fp24-w-pred-deflate.dng | Canon EOS 5D Mark III | DNG · deflate | 23.4 | all | **15.5** | 300.8 | 25.3 | **27.2** | 607.7 | 188 / 177 / 144 |
| hdrmerge-bayer-fp24-w-pred-deflate.dng | Canon EOS 5D Mark III | DNG · deflate | 23.4 | 1 | **186.8** | 301.2 | 260.5 | **353.5** | 634.9 | 177 / 178 / 131 |
| hdrmerge-bayer-fp32-w-pred-deflate.dng | Canon EOS 5D Mark III | DNG · deflate | 23.4 | all | **18.7** | 390.2 | 29.4 | **33.5** | 693.1 | 209 / 197 / 165 |
| hdrmerge-bayer-fp32-w-pred-deflate.dng | Canon EOS 5D Mark III | DNG · deflate | 23.4 | 1 | **239.7** | 384.7 | 336.2 | **412.1** | 725.3 | 198 / 197 / 151 |
| M2462362.DNG | Leica M Monochrom (Typ 246) | DNG · lossless-jpeg | 23.9 | all | **11.9** | 306.0 | 90.6 | **19.7** | 433.5 | 215 / 71 / 74 |
| M2462362.DNG | Leica M Monochrom (Typ 246) | DNG · lossless-jpeg | 23.9 | 1 | **78.8** | 304.1 | 89.7 | **174.0** | 458.7 | 96 / 71 / 74 |
| _K1I9784.DNG | Pentax K-1 Mark II | DNG · lossless-jpeg | 36.6 | all | **18.6** | 471.0 | 148.0 | **38.5** | 964.2 | 458 / 247 / 250 |
| _K1I9784.DNG | Pentax K-1 Mark II | DNG · lossless-jpeg | 36.6 | 1 | **137.3** | 466.8 | 147.2 | **404.6** | 1012.9 | 293 / 247 / 250 |
| L16_00148.dng | Light L16 | DNG · uncompressed | 64.6 | all | 10.0 | 255.7 | **7.84** | **41.4** | 606.3 | 763 / 881 / 761 |
| L16_00148.dng | Light L16 | DNG · uncompressed | 64.6 | 1 | 42.3 | 247.1 | **37.8** | **474.6** | 597.8 | 762 / 881 / 760 |
| IMG_20200324_073611.dng | Xiaomi Mi 9 Lite | DNG · uncompressed | 12.0 | all | **1.36** | 10.8 | 22.9 | **12.8** | 240.2 | 57 / 50 / 54 |
| IMG_20200324_073611.dng | Xiaomi Mi 9 Lite | DNG · uncompressed | 12.0 | 1 | **1.48** | 10.6 | 22.8 | **128.2** | 241.1 | 55 / 50 / 53 |
| EPSN3575.ERF | Epson R-D1s | ERF · uncompressed | 6.2 | all | **0.69** | 22.1 | 3.41 | **5.02** | 117.8 | 31 / 26 / 29 |
| EPSN3575.ERF | Epson R-D1s | ERF · uncompressed | 6.2 | 1 | **1.89** | 22.5 | 3.31 | **47.3** | 112.6 | 30 / 25 / 29 |
| _EPS1919.ERF | Epson R-D1x | ERF · uncompressed | 6.2 | all | **0.74** | 22.5 | 3.28 | **5.05** | 115.2 | 31 / 26 / 29 |
| _EPS1919.ERF | Epson R-D1x | ERF · uncompressed | 6.2 | 1 | **1.88** | 22.3 | 3.45 | **48.0** | 113.7 | 30 / 24 / 29 |
| 20151114_CCSG_on_Hasselblad_H5D50c-0017.fff | Hasselblad H5D-50c | FFF · hasselblad | 52.8 | all | **30.3** | 485.9 | 223.7 | **57.1** | 1169.0 | 328 / 182 / 185 |
| 20151114_CCSG_on_Hasselblad_H5D50c-0017.fff | Hasselblad H5D-50c | FFF · hasselblad | 52.8 | 1 | 327.5 | 483.9 | **225.0** | **709.9** | 1247.2 | 186 / 181 / 185 |
| 0008_20_23.IIQ | Leaf Credo 40 | IIQ · phaseone | 40.8 | all | **11.1** | 245.7 | 200.7 | **41.3** | 1203.8 | 123 / 117 / 126 |
| 0008_20_23.IIQ | Leaf Credo 40 | IIQ · phaseone | 40.8 | 1 | **139.2** | 244.0 | 317.3 | **558.6** | 1263.8 | 122 / 116 / 125 |
| IQ4_150MPIIQ_L_16bit.iiq | Phase One IQ4 150MP | IIQ · phaseone | 154.0 | all | **46.4** | 995.8 | ❌ | **122.7** | 4446.0 | 507 / 500 / ❌ |
| IQ4_150MPIIQ_L_16bit.iiq | Phase One IQ4 150MP | IIQ · phaseone | 154.0 | 1 | **572.1** | 1000.2 | ❌ | **1684.4** | 4675.0 | 506 / 499 / ❌ |
| RGB_86_0_4.IIQ | Phase One iXM-RS150F | IIQ · phaseone-s | 154.0 | all | **33.9** | 486.4 | ❌ | **111.1** | 3802.2 | 416 / 410 / ❌ |
| RGB_86_0_4.IIQ | Phase One iXM-RS150F | IIQ · phaseone-s | 154.0 | 1 | 483.6 | **483.0** | ❌ | **1604.5** | 4021.3 | 415 / 409 / ❌ |
| cap_22908.IIQ | Phase One iXU180 | IIQ · phaseone-s | 81.1 | all | **18.4** | 250.4 | ❌ | **77.1** | 2058.1 | 222 / 215 / ❌ |
| cap_22908.IIQ | Phase One iXU180 | IIQ · phaseone-s | 81.1 | 1 | **251.3** | 251.4 | ❌ | **1074.9** | 2193.1 | 221 / 215 / ❌ |
| H25_IT8.7-2_Card.TIF | Phase One H25 | IIQ · uncompressed | 22.7 | all | 1.48 | **1.17** | ❌ | **22.1** | 820.9 | 98 / 92 / ❌ |
| H25_IT8.7-2_Card.TIF | Phase One H25 | IIQ · uncompressed | 22.7 | 1 | 2.70 | **1.16** | ❌ | **288.5** | 841.3 | 98 / 92 / ❌ |
| RAW_KODAK_DC120.KDC | Kodak DC120 | KDC · kodak-dc120 | 0.8 | all | **0.07** | 0.67 | ❌ | **4.16** | 22.4 | 12 / 8 / ❌ |
| RAW_KODAK_DC120.KDC | Kodak DC120 | KDC · kodak-dc120 | 0.8 | 1 | **0.06** | 0.74 | ❌ | 28.6 | **20.3** | 11 / 8 / ❌ |
| RAW_KODAK_DC50.KDC | Kodak DC50 | KDC · other | 0.4 | all | **1.94** | 3.31 | ❌ | **2.94** | 9.98 | 10 / 8 / ❌ |
| RAW_KODAK_DC50.KDC | Kodak DC50 | KDC · other | 0.4 | 1 | **1.95** | 3.40 | ❌ | **4.96** | 9.19 | 11 / 6 / ❌ |
| 100_1548.KDC | Kodak EasyShare Z981 | KDC · uncompressed | 13.9 | all | **0.85** | 47.4 | 9.78 | **9.11** | 247.3 | 61 / 56 / 58 |
| 100_1548.KDC | Kodak EasyShare Z981 | KDC · uncompressed | 13.9 | 1 | **1.17** | 47.4 | 9.69 | **102.8** | 252.3 | 60 / 55 / 58 |
| RAW_KODAK_EASYSHARE_Z1015-IS.KDC | Kodak Z1015 IS | KDC · uncompressed | 10.0 | all | **0.71** | 33.9 | 7.03 | **7.11** | 184.3 | 48 / 43 / 46 |
| RAW_KODAK_EASYSHARE_Z1015-IS.KDC | Kodak Z1015 IS | KDC · uncompressed | 10.0 | 1 | **0.80** | 34.2 | 7.15 | **74.7** | 183.3 | 47 / 42 / 46 |
| RAW_MAMIYA_ZD.MEF | Mamiya ZD | MEF · uncompressed | 21.5 | all | **1.37** | 73.7 | 15.1 | **20.7** | 569.9 | 86 / 80 / 83 |
| RAW_MAMIYA_ZD.MEF | Mamiya ZD | MEF · uncompressed | 21.5 | 1 | **1.80** | 73.2 | 14.9 | **276.5** | 593.7 | 85 / 79 / 83 |
| L_000859.mos | Leaf AFi-II 12 | MOS · leaf | 80.0 | all | **35.2** | 1087.1 | ❌ | **78.6** | 2069.0 | 400 / 237 / ❌ |
| L_000859.mos | Leaf AFi-II 12 | MOS · leaf | 80.0 | 1 | **275.3** | 1086.3 | ❌ | **864.6** | 2222.9 | 395 / 237 / ❌ |
| RAW_APTUS_75.MOS | Leaf Aptus 75 | MOS · leaf | 33.3 | all | **22.4** | 502.3 | ❌ | **52.4** | 1199.2 | 117 / 109 / ❌ |
| RAW_APTUS_75.MOS | Leaf Aptus 75 | MOS · leaf | 33.3 | 1 | **151.1** | 507.8 | ❌ | **590.0** | 1224.1 | 114 / 108 / ❌ |
| L_003173.mos | Leaf Aptus 22 | MOS · uncompressed | 21.4 | all | **1.46** | 17.3 | 16.5 | **21.7** | 452.4 | 92 / 87 / 90 |
| L_003173.mos | Leaf Aptus 22 | MOS · uncompressed | 21.4 | 1 | **2.19** | 17.0 | 16.0 | **274.3** | 470.7 | 91 / 86 / 90 |
| Minolta_Dynax_5D.MRW | Minolta Dynax 5D | MRW · minolta-packed | 6.1 | all | **0.41** | 20.8 | 4.31 | **4.64** | 117.7 | 30 / 25 / 28 |
| Minolta_Dynax_5D.MRW | Minolta Dynax 5D | MRW · minolta-packed | 6.1 | 1 | **0.52** | 20.8 | 4.38 | **44.3** | 110.5 | 29 / 24 / 28 |
| PICT0881.MRW | Minolta DiMAGE A2 | MRW · minolta-packed | 8.0 | all | **0.46** | 27.5 | 5.54 | **5.80** | 155.0 | 37 / 31 / 34 |
| PICT0881.MRW | Minolta DiMAGE A2 | MRW · minolta-packed | 8.0 | 1 | **0.66** | 27.8 | 5.72 | **60.6** | 146.8 | 36 / 31 / 34 |
| RAW_MINOLTA_DIMAGE7.MRW | Minolta DiMAGE 7 | MRW · uncompressed | 5.0 | all | **0.50** | 4.11 | 3.61 | **4.25** | 87.6 | 29 / 23 / 26 |
| RAW_MINOLTA_DIMAGE7.MRW | Minolta DiMAGE 7 | MRW · uncompressed | 5.0 | 1 | **0.49** | 4.13 | 3.77 | **38.0** | 77.4 | 28 / 22 / 26 |
| RAW_MINOLTA_DIMAGE_7I.MRW | Minolta DiMAGE 7i | MRW · uncompressed | 5.0 | all | **0.49** | 4.12 | 3.74 | **4.39** | 86.6 | 28 / 23 / 26 |
| RAW_MINOLTA_DIMAGE_7I.MRW | Minolta DiMAGE 7i | MRW · uncompressed | 5.0 | 1 | **0.52** | 4.07 | 3.79 | **37.3** | 79.0 | 28 / 23 / 26 |
| Nikon_Z6__3_Lossless_compressed_1_1.NEF | Nikon Z6_3 | NEF · nikon-huffman | 16.4 | all | **10.3** | 189.1 | 71.5 | **20.0** | 417.0 | 124 / 56 / 60 |
| Nikon_Z6__3_Lossless_compressed_1_1.NEF | Nikon Z6_3 | NEF · nikon-huffman | 16.4 | 1 | 74.3 | 190.0 | **70.9** | **195.4** | 433.0 | 62 / 56 / 60 |
| _DSC2256[1].NEF | Nikon D4 | NEF · uncompressed | 16.4 | all | **1.13** | 56.0 | 11.7 | **10.8** | 288.3 | 68 / 62 / 65 |
| _DSC2256[1].NEF | Nikon D4 | NEF · uncompressed | 16.4 | 1 | **1.36** | 56.3 | 11.6 | **120.7** | 299.3 | 67 / 62 / 65 |
| Nikon-D850-14bit-uncompressed.NEF | Nikon D850 | NEF · uncompressed | 45.7 | all | 3.35 | 2.32 | **2.29** | **30.3** | 609.1 | 191 / 186 / 188 |
| Nikon-D850-14bit-uncompressed.NEF | Nikon D850 | NEF · uncompressed | 45.7 | 1 | 3.35 | 2.37 | **2.20** | **338.5** | 681.6 | 190 / 185 / 188 |
| DSCN0014.NRW | Nikon Coolpix P950 | NRW · uncompressed | 16.0 | all | **1.22** | 54.5 | 11.1 | **12.2** | 277.3 | 66 / 61 / 63 |
| DSCN0014.NRW | Nikon Coolpix P950 | NRW · uncompressed | 16.0 | 1 | **3.30** | 54.2 | 11.3 | **120.6** | 295.3 | 65 / 60 / 63 |
| DSCN0417.NRW | Nikon Coolpix A1000 | NRW · uncompressed | 15.9 | all | 1.26 | 1.00 | **0.92** | **10.5** | 226.7 | 74 / 68 / 71 |
| DSCN0417.NRW | Nikon Coolpix A1000 | NRW · uncompressed | 15.9 | 1 | 1.41 | 1.07 | **0.95** | **119.0** | 235.5 | 73 / 68 / 71 |
| 80mp-14bit-tripod.orf | OM System OM-1 Mark II | ORF · olympus | 80.9 | all | **422.0** | 1585.8 | 619.6 | **465.6** | 2665.6 | 253 / 243 / 245 |
| 80mp-14bit-tripod.orf | OM System OM-1 Mark II | ORF · olympus | 80.9 | 1 | **536.3** | 1594.9 | 617.3 | **1127.6** | 2798.7 | 249 / 242 / 245 |
| PA040002.ORF | Olympus E-M10 Mark IV | ORF · olympus | 20.3 | all | **109.7** | 301.2 | 137.2 | **120.5** | 585.6 | 67 / 61 / 63 |
| PA040002.ORF | Olympus E-M10 Mark IV | ORF · olympus | 20.3 | 1 | **134.5** | 301.6 | 136.5 | **288.8** | 603.3 | 65 / 60 / 63 |
| P3307182.ORF | Olympus E-330 | ORF · uncompressed | 8.0 | all | **1.62** | 28.8 | 4.51 | **6.51** | 152.9 | 38 / 33 / 36 |
| P3307182.ORF | Olympus E-330 | ORF · uncompressed | 8.0 | 1 | 6.56 | 28.6 | **4.53** | **61.5** | 147.1 | 37 / 32 / 36 |
| PenFHiRes.orf | Olympus PEN-F | ORF · uncompressed | 81.1 | all | **15.2** | 287.2 | 44.7 | **58.2** | 1344.9 | 290 / 284 / 287 |
| PenFHiRes.orf | Olympus PEN-F | ORF · uncompressed | 81.1 | 1 | 65.3 | 289.2 | **45.2** | **655.2** | 1487.2 | 289 / 284 / 287 |
| _K1I9781.PEF | Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | all | **21.2** | 292.0 | 132.7 | **40.0** | 777.9 | 375 / 242 / 245 |
| _K1I9781.PEF | Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | 1 | 140.5 | 291.3 | **132.9** | **407.1** | 832.2 | 247 / 241 / 245 |
| IMGP9854.PEF | Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | all | **15.1** | 204.8 | 88.4 | **29.4** | 565.5 | 174 / 86 / 89 |
| IMGP9854.PEF | Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | 1 | 94.4 | 204.0 | **87.9** | **282.9** | 596.4 | 91 / 85 / 89 |
| IMGP1754.PEF | Pentax K100D | PEF · uncompressed | 6.2 | all | **0.53** | 21.1 | 4.26 | **4.94** | 117.7 | 32 / 26 / 30 |
| IMGP1754.PEF | Pentax K100D | PEF · uncompressed | 6.2 | 1 | **0.65** | 21.3 | 4.37 | **45.7** | 114.8 | 32 / 26 / 30 |
| pentax.PEF | Pentax *ist D | PEF · uncompressed | 6.2 | all | **0.63** | 4.86 | 4.57 | **5.24** | 101.9 | 34 / 29 / 32 |
| pentax.PEF | Pentax *ist D | PEF · uncompressed | 6.2 | 1 | **0.70** | 5.09 | 4.69 | **45.9** | 96.8 | 34 / 28 / 32 |
| Fujifilm-GFX100S-16bits-losslesscompressed-4_3.RAF | Fujifilm GFX100S | RAF · fuji-compressed | 103.4 | all | **62.7** | 288.3 | 79.0 | **116.1** | 1625.3 | 337 / 333 / 335 |
| Fujifilm-GFX100S-16bits-losslesscompressed-4_3.RAF | Fujifilm GFX100S | RAF · fuji-compressed | 103.4 | 1 | **839.5** | 2698.5 | 1029.1 | **1589.6** | 4209.3 | 336 / 331 / 334 |
| DSCF0734.RAF | Fujifilm X100V | RAF · fuji-compressed | 26.7 | all | **26.7** | 121.2 | 36.4 | **50.9** | 479.2 | 92 / 88 / 90 |
| DSCF0734.RAF | Fujifilm X100V | RAF · fuji-compressed | 26.7 | 1 | **233.8** | 795.9 | 313.4 | **546.9** | 1207.2 | 91 / 86 / 89 |
| Fujifilm-GFX100S-16bits-uncompress-4_3.RAF | Fujifilm GFX100S | RAF · uncompressed | 103.4 | all | 7.63 | 5.24 | **4.73** | **60.9** | 1343.6 | 407 / 402 / 405 |
| Fujifilm-GFX100S-16bits-uncompress-4_3.RAF | Fujifilm GFX100S | RAF · uncompressed | 103.4 | 1 | 7.42 | **5.00** | 5.08 | **758.7** | 1514.0 | 407 / 401 / 405 |
| DSCF9050.RAF | Fujifilm X-E2 | RAF · uncompressed | 16.5 | all | 1.39 | 12.2 | **0.91** | **15.6** | 241.7 | 73 / 68 / 71 |
| DSCF9050.RAF | Fujifilm X-E2 | RAF · uncompressed | 16.5 | 1 | 1.38 | 12.3 | **0.75** | **194.3** | 265.6 | 73 / 67 / 71 |
| CRW_6488.CRW | Canon PowerShot SX20 IS | RAW · uncompressed | 12.4 | all | **1.51** | 43.5 | ❌ | **9.07** | 272.7 | 53 / 46 / ❌ |
| CRW_6488.CRW | Canon PowerShot SX20 IS | RAW · uncompressed | 12.4 | 1 | **1.87** | 43.0 | ❌ | **91.4** | 273.0 | 52 / 45 / ❌ |
| git2p_43.raw | GITUP GIT2P 4:3 | RAW · uncompressed | 16.1 | all | **1.01** | 1.01 | 14.0 | **11.1** | 269.4 | 71 / 65 / 69 |
| git2p_43.raw | GITUP GIT2P 4:3 | RAW · uncompressed | 16.1 | 1 | 1.37 | **0.88** | 13.7 | **118.0** | 290.5 | 70 / 65 / 69 |
| PANA2371_16x9.RW2 | Panasonic DC-G9 | RW2 · panasonic-v4 | 20.6 | all | **4.30** | 95.2 | 4.79 | **15.6** | 419.2 | 72 / 67 / 71 |
| PANA2371_16x9.RW2 | Panasonic DC-G9 | RW2 · panasonic-v4 | 20.6 | 1 | **43.2** | 92.2 | 46.8 | **194.5** | 422.7 | 71 / 66 / 70 |
| P1020077.RW2 | Panasonic DC-LX100M2 | RW2 · panasonic-v4 | 16.7 | all | **3.26** | 76.4 | 4.17 | **12.5** | 338.8 | 60 / 55 / 61 |
| P1020077.RW2 | Panasonic DC-LX100M2 | RW2 · panasonic-v4 | 16.7 | 1 | **34.3** | 74.5 | 37.7 | **152.3** | 343.5 | 60 / 55 / 60 |
| P1000475.RW2 | Panasonic DC-G9 | RW2 · panasonic-v5 | 81.7 | all | **5.11** | 73.5 | 6.78 | **46.2** | 1135.3 | 291 / 286 / 291 |
| P1000475.RW2 | Panasonic DC-G9 | RW2 · panasonic-v5 | 81.7 | 1 | **36.6** | 73.9 | 66.4 | **633.2** | 1277.6 | 290 / 285 / 289 |
| P1022090.RW2 | Panasonic DC-GH5S | RW2 · panasonic-v5 | 10.0 | all | 1.12 | 8.52 | **0.89** | **8.31** | 154.1 | 47 / 41 / 45 |
| P1022090.RW2 | Panasonic DC-GH5S | RW2 · panasonic-v5 | 10.0 | 1 | **4.57** | 8.56 | 8.43 | **78.8** | 156.9 | 46 / 41 / 44 |
| P1000006.RW2 | Panasonic DC-S1H | RW2 · panasonic-v6 | 24.4 | all | **2.38** | 63.6 | 4.62 | **15.9** | 439.1 | 90 / 85 / 89 |
| P1000006.RW2 | Panasonic DC-S1H | RW2 · panasonic-v6 | 24.4 | 1 | **18.8** | 63.9 | 44.7 | **198.2** | 454.3 | 90 / 85 / 88 |
| P1047513.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 24.3 | all | 1.81 | 8.89 | **0.63** | **15.9** | 375.9 | 98 / 94 / 96 |
| P1047513.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 24.3 | 1 | 4.62 | 8.57 | **3.86** | **182.5** | 404.1 | 97 / 92 / 95 |
| dc-s5_hr_96MP.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 97.0 | all | 6.50 | 33.8 | **2.07** | **56.3** | 1445.9 | 360 / 355 / 358 |
| dc-s5_hr_96MP.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 97.0 | 1 | 18.1 | 35.4 | **15.8** | **724.7** | 1609.3 | 359 / 355 / 357 |
| 3-2.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 25.2 | all | **12.9** | 133.3 | 62.1 | **26.5** | 522.0 | 234 / 100 / 99 |
| 3-2.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 25.2 | 1 | 137.7 | 260.8 | **121.5** | **327.7** | 671.4 | 100 / 98 / 99 |
| P1000116.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 100.3 | all | **46.8** | 374.5 | 153.9 | **95.2** | 1832.5 | 816 / 352 / 348 |
| P1000116.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 100.3 | 1 | 549.4 | 1112.4 | **455.2** | **1302.3** | 2753.8 | 350 / 348 / 348 |
| RAW_LEICA_DLUX3.RAW | Leica D-LUX 3 | RW2 · uncompressed | 10.6 | all | 1.08 | 7.96 | **0.78** | **9.50** | 172.6 | 50 / 45 / 48 |
| RAW_LEICA_DLUX3.RAW | Leica D-LUX 3 | RW2 · uncompressed | 10.6 | 1 | 0.98 | 7.94 | **0.72** | **96.7** | 186.7 | 49 / 44 / 48 |
| RAW_PANASONIC_FZ30_SRGB.RAW | Panasonic DMC-FZ30 | RW2 · uncompressed | 8.1 | all | 0.84 | 6.04 | **0.61** | **6.54** | 127.5 | 40 / 36 / 38 |
| RAW_PANASONIC_FZ30_SRGB.RAW | Panasonic DMC-FZ30 | RW2 · uncompressed | 8.1 | 1 | 0.86 | 6.16 | **0.57** | **61.7** | 137.3 | 40 / 35 / 38 |
| 2015-03-07-163604_sam_7204.srw | Samsung NX30 | SRW · samsung-v0 | 20.6 | all | **9.82** | 170.3 | 59.6 | **33.9** | 468.4 | 83 / 74 / 77 |
| 2015-03-07-163604_sam_7204.srw | Samsung NX30 | SRW · samsung-v0 | 20.6 | 1 | 74.8 | 169.8 | **56.3** | **250.5** | 544.9 | 81 / 73 / 77 |
| nx300.SRW | Samsung NX300 | SRW · samsung-v0 | 20.6 | all | **57.6** | 168.8 | 59.4 | **134.5** | 469.8 | 78 / 67 / 69 |
| nx300.SRW | Samsung NX300 | SRW · samsung-v0 | 20.6 | 1 | 79.5 | 172.0 | **56.2** | **229.8** | 477.9 | 74 / 67 / 69 |
| _3184416.SRW | Samsung NX3000 | SRW · samsung-v1 | 20.8 | all | **72.5** | 229.0 | 92.0 | **86.4** | 495.8 | 70 / 66 / 68 |
| _3184416.SRW | Samsung NX3000 | SRW · samsung-v1 | 20.8 | 1 | **71.7** | 225.4 | 90.4 | **221.0** | 523.2 | 69 / 64 / 68 |
| SAM_4260.SRW | Samsung NX3300 | SRW · samsung-v1 | 20.8 | all | **73.1** | 225.2 | 92.8 | **85.7** | 503.6 | 70 / 64 / 68 |
| SAM_4260.SRW | Samsung NX3300 | SRW · samsung-v1 | 20.8 | 1 | **72.6** | 226.3 | 93.2 | **224.8** | 534.4 | 70 / 65 / 68 |
| 2016-07-23-142101_sam_9364.srw | Samsung NX1 | SRW · samsung-v2 | 28.2 | all | **114.6** | 273.1 | 137.6 | **130.5** | 654.0 | 99 / 94 / 96 |
| 2016-07-23-142101_sam_9364.srw | Samsung NX1 | SRW · samsung-v2 | 28.2 | 1 | **113.8** | 274.2 | 137.5 | **328.3** | 692.8 | 98 / 94 / 96 |
| SAM_2935.SRW | Samsung NX500 | SRW · samsung-v2 | 28.2 | all | **115.4** | 298.3 | 147.7 | **219.4** | 868.9 | 88 / 84 / 86 |
| SAM_2935.SRW | Samsung NX500 | SRW · samsung-v2 | 28.2 | 1 | **133.4** | 316.6 | 155.3 | **389.6** | 805.1 | 88 / 84 / 86 |
| 03194133.SRW | Samsung NX20 | SRW · uncompressed | 21.0 | all | **3.76** | 73.6 | 14.2 | **38.6** | 352.1 | 86 / 83 / 84 |
| 03194133.SRW | Samsung NX20 | SRW · uncompressed | 21.0 | 1 | **2.02** | 71.8 | 14.2 | **152.8** | 380.5 | 86 / 81 / 84 |
| nx210.SRW | Samsung NX210 | SRW · uncompressed | 21.0 | all | **3.29** | 72.8 | 14.1 | **21.6** | 347.4 | 83 / 79 / 80 |
| nx210.SRW | Samsung NX210 | SRW · uncompressed | 21.0 | 1 | **1.89** | 71.4 | 14.2 | **152.0** | 377.2 | 82 / 77 / 80 |
| SDIM0555.X3F | Sigma DP1 Merrill | X3F · sigma-x3f | 16.1 | all | **63.8** | 886.2 | ❌ | **76.8** | 1060.1 | 248 / 280 / ❌ |
| SDIM0555.X3F | Sigma DP1 Merrill | X3F · sigma-x3f | 16.1 | 1 | **179.5** | 878.2 | ❌ | **333.3** | 1033.3 | 247 / 279 / ❌ |
| SDIM0061.X3F | Sigma sd Quattro H | X3F · sigma-x3f | 29.8 | all | **116.1** | 853.5 | ❌ | **131.2** | 1106.2 | 329 / 382 / ❌ |
| SDIM0061.X3F | Sigma sd Quattro H | X3F · sigma-x3f | 29.8 | 1 | **186.9** | 844.3 | ❌ | **364.9** | 1102.9 | 328 / 381 / ❌ |
| KODAK_C603_C643_FORMAT_NONE_YRGB0001.RAW | Kodak C603 | other · kodak-ycbcr | 6.1 | all | **2.56** | 15.6 | ❌ | **8.37** | 59.2 | 54 / 60 / ❌ |
| KODAK_C603_C643_FORMAT_NONE_YRGB0001.RAW | Kodak C603 | other · kodak-ycbcr | 6.1 | 1 | **9.00** | 15.6 | ❌ | **50.9** | 58.9 | 53 / 60 / ❌ |
| 2016_0104_005847_665To_send.RAW | Sjcam SJ5000X Elite | RAW · uncompressed | 12.0 | all | **0.93** | 30.5 | ❌ | **11.2** | 96.5 | 50 / 114 / ❌ |
| 2016_0104_005847_665To_send.RAW | Sjcam SJ5000X Elite | RAW · uncompressed | 12.0 | 1 | **1.25** | 30.9 | ❌ | **91.7** | 96.5 | 49 / 113 / ❌ |
| Pi_V1.3_picamera.raw | Raspberry Pi RPi OV5647 v.2 | other · uncompressed | 5.1 | all | **1.05** | 4.39 | ❌ | **4.88** | 86.8 | 26 / 21 / ❌ |
| Pi_V1.3_picamera.raw | Raspberry Pi RPi OV5647 v.2 | other · uncompressed | 5.1 | 1 | **3.59** | 4.26 | ❌ | **40.9** | 80.2 | 25 / 20 / ❌ |
| Tomatio_03.sti | Sinar eVolution 75 | other · uncompressed | 33.3 | all | **2.18** | 26.8 | 25.2 | **24.8** | 473.7 | 141 / 136 / 139 |
| Tomatio_03.sti | Sinar eVolution 75 | other · uncompressed | 33.3 | 1 | **3.13** | 27.2 | 25.2 | **315.3** | 526.7 | 141 / 135 / 139 |

## Failures and unsupported files

| File | library | mode | threads | error |
|---|---|---|---|---|
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| RAW.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| RAW.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| RAW.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| RAW.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| canon-r6m2-img_0346-raw-normal-full.cr3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| canon-r6m2-img_0346-raw-normal-full.cr3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| canon-r6m2-img_0346-raw-normal-full.cr3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| canon-r6m2-img_0346-raw-normal-full.cr3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| IMG_4241.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| IMG_4241.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| IMG_4241.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| IMG_4241.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| 443A0157.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| 443A0157.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| 443A0157.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| 443A0157.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| RAW_FULL_FRAME.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| RAW_FULL_FRAME.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| RAW_FULL_FRAME.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| RAW_FULL_FRAME.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon-90D-RAW-ISO-100.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| Canon-90D-RAW-ISO-100.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| Canon-90D-RAW-ISO-100.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon-90D-RAW-ISO-100.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW | dcraw | decode | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/Sony/ILCE-7M4/ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-La |
| ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW | dcraw | rgb | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/Sony/ILCE-7M4/ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-La |
| 7RM5-LosslessCompressedLarge.ARW | dcraw | decode | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/Sony/ILCE-7RM5/7RM5-LosslessCompressedLarge.ARW |
| 7RM5-LosslessCompressedLarge.ARW | dcraw | rgb | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/Sony/ILCE-7RM5/7RM5-LosslessCompressedLarge.ARW |
| DSC00001.ARW | dcraw | decode | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/Sony/ILCE-6700/DSC00001.ARW |
| DSC00001.ARW | dcraw | rgb | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/Sony/ILCE-6700/DSC00001.ARW |
| DSCF0021.RAF | dcraw | decode | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| DSCF0021.RAF | dcraw | rgb | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| Fuji-X-H2-Compressed-Raw.RAF | RawSpeed | decode | all | rawspeed::FujiDecompressor::FujiDecompressor(RawImage, ByteStream), line 854: compressed RAF header check |
| Fuji-X-H2-Compressed-Raw.RAF | RawSpeed | decode | 1 | rawspeed::FujiDecompressor::FujiDecompressor(RawImage, ByteStream), line 854: compressed RAF header check |
| Fuji-X-H2-Compressed-Raw.RAF | dcraw | decode | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| Fuji-X-H2-Compressed-Raw.RAF | dcraw | rgb | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| DSCF2526.RAF | dcraw | decode | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| DSCF2526.RAF | dcraw | rgb | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| DSCF0075.RAF | dcraw | decode | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| DSCF0075.RAF | dcraw | rgb | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| _DSF0267.RAF | dcraw | decode | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| _DSF0267.RAF | dcraw | rgb | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| Job_0079.fff | rawler | decode | all | Failed to decode image, possibly corrupt image: decompress_16le(): buffer too short (148949934 < 210510336) |
| Job_0079.fff | rawler | rgb | all | Failed to decode image, possibly corrupt image: decompress_16le(): buffer too short (148949934 < 210510336) |
| Job_0079.fff | rawler | decode | 1 | Failed to decode image, possibly corrupt image: decompress_16le(): buffer too short (148949934 < 210510336) |
| Job_0079.fff | rawler | rgb | 1 | Failed to decode image, possibly corrupt image: decompress_16le(): buffer too short (148949934 < 210510336) |
| IQ4_150MP_L_14.iiq | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| IQ4_150MP_L_14.iiq | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| IMG_1361.DNG | RawSpeed | decode | all | void rawspeed::AbstractDngDecompressor::decompress() const, line 251: Too many errors encountered. Giving up. First Erro |
| IMG_1361.DNG | RawSpeed | decode | 1 | void rawspeed::AbstractDngDecompressor::decompress() const, line 251: Too many errors encountered. Giving up. First Erro |
| 20250305_173647.dng | LibRaw | decode | all | Unsupported file format or not RAW file |
| 20250305_173647.dng | RawSpeed | decode | all | virtual RawImage rawspeed::DngDecoder::decodeRawInternal(), line 457: No RAW chunks found |
| 20250305_173647.dng | rawpy | decode | all | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| 20250305_173647.dng | LibRaw | rgb | all | Unsupported file format or not RAW file |
| 20250305_173647.dng | rawpy | rgb | all | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| 20250305_173647.dng | LibRaw + Little CMS | rgb | all | Unsupported file format or not RAW file |
| 20250305_173647.dng | LibRaw | decode | 1 | Unsupported file format or not RAW file |
| 20250305_173647.dng | RawSpeed | decode | 1 | virtual RawImage rawspeed::DngDecoder::decodeRawInternal(), line 457: No RAW chunks found |
| 20250305_173647.dng | rawpy | decode | 1 | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| 20250305_173647.dng | dcraw | decode | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/samsung/SM-S901E/20250305_173647.dng |
| 20250305_173647.dng | LibRaw | rgb | 1 | Unsupported file format or not RAW file |
| 20250305_173647.dng | rawpy | rgb | 1 | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| 20250305_173647.dng | dcraw | rgb | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/samsung/SM-S901E/20250305_173647.dng |
| 20250305_173647.dng | LibRaw + Little CMS | rgb | 1 | Unsupported file format or not RAW file |
| GOPR0123.GPR | LibRaw | decode | all | Unsupported file format or not RAW file |
| GOPR0123.GPR | rawler | decode | all | Failed to decode image, possibly corrupt image: TODO: Unsupported DNG compression |
| GOPR0123.GPR | rawpy | decode | all | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| GOPR0123.GPR | LibRaw | rgb | all | Unsupported file format or not RAW file |
| GOPR0123.GPR | rawler | rgb | all | Failed to decode image, possibly corrupt image: TODO: Unsupported DNG compression |
| GOPR0123.GPR | rawpy | rgb | all | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| GOPR0123.GPR | LibRaw + Little CMS | rgb | all | Unsupported file format or not RAW file |
| GOPR0123.GPR | LibRaw | decode | 1 | Unsupported file format or not RAW file |
| GOPR0123.GPR | rawler | decode | 1 | Failed to decode image, possibly corrupt image: TODO: Unsupported DNG compression |
| GOPR0123.GPR | rawpy | decode | 1 | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| GOPR0123.GPR | dcraw | decode | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/GoPro/HERO11 Black/GOPR0123.GPR |
| GOPR0123.GPR | LibRaw | rgb | 1 | Unsupported file format or not RAW file |
| GOPR0123.GPR | rawler | rgb | 1 | Failed to decode image, possibly corrupt image: TODO: Unsupported DNG compression |
| GOPR0123.GPR | rawpy | rgb | 1 | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| GOPR0123.GPR | dcraw | rgb | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/GoPro/HERO11 Black/GOPR0123.GPR |
| GOPR0123.GPR | LibRaw + Little CMS | rgb | 1 | Unsupported file format or not RAW file |
| GOPR0018.GPR | LibRaw | decode | all | Unsupported file format or not RAW file |
| GOPR0018.GPR | rawler | decode | all | Failed to decode image, possibly corrupt image: TODO: Unsupported DNG compression |
| GOPR0018.GPR | rawpy | decode | all | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| GOPR0018.GPR | LibRaw | rgb | all | Unsupported file format or not RAW file |
| GOPR0018.GPR | rawler | rgb | all | Failed to decode image, possibly corrupt image: TODO: Unsupported DNG compression |
| GOPR0018.GPR | rawpy | rgb | all | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| GOPR0018.GPR | LibRaw + Little CMS | rgb | all | Unsupported file format or not RAW file |
| GOPR0018.GPR | LibRaw | decode | 1 | Unsupported file format or not RAW file |
| GOPR0018.GPR | rawler | decode | 1 | Failed to decode image, possibly corrupt image: TODO: Unsupported DNG compression |
| GOPR0018.GPR | rawpy | decode | 1 | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| GOPR0018.GPR | dcraw | decode | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/GoPro/HERO10 Black/GOPR0018.GPR |
| GOPR0018.GPR | LibRaw | rgb | 1 | Unsupported file format or not RAW file |
| GOPR0018.GPR | rawler | rgb | 1 | Failed to decode image, possibly corrupt image: TODO: Unsupported DNG compression |
| GOPR0018.GPR | rawpy | rgb | 1 | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| GOPR0018.GPR | dcraw | rgb | 1 | Cannot decode file /Users/ybouane/flashraw-bench/corpus/GoPro/HERO10 Black/GOPR0018.GPR |
| GOPR0018.GPR | LibRaw + Little CMS | rgb | 1 | Unsupported file format or not RAW file |
| full_compressed.ARW | RawSpeed | decode | all | virtual RawImage rawspeed::ArwDecoder::decodeRawInternal(), line 186: Unsupported compression 32766 |
| full_compressed.ARW | RawSpeed | decode | 1 | virtual RawImage rawspeed::ArwDecoder::decodeRawInternal(), line 186: Unsupported compression 32766 |
| full_compressed_HQ.ARW | RawSpeed | decode | all | virtual RawImage rawspeed::ArwDecoder::decodeRawInternal(), line 186: Unsupported compression 32766 |
| full_compressed_HQ.ARW | RawSpeed | decode | 1 | virtual RawImage rawspeed::ArwDecoder::decodeRawInternal(), line 186: Unsupported compression 32766 |
| DSC00800.ARW | RawSpeed | decode | all | void rawspeed::ArwDecoder::DecodeLJpeg(const TiffIFD *), line 307: Unsupported photometric interpretation: 6 |
| DSC00800.ARW | RawSpeed | decode | 1 | void rawspeed::ArwDecoder::DecodeLJpeg(const TiffIFD *), line 307: Unsupported photometric interpretation: 6 |
| Canon_EOS_R5_RAW_ISO_100_nocrop_dual.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| Canon_EOS_R5_RAW_ISO_100_nocrop_dual.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| canon-r6m2-img_0355-craw-dual-full.cr3 | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| canon-r6m2-img_0355-craw-dual-full.cr3 | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| RAW_CANON_PRO70_SRGB.CRW | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| RAW_CANON_PRO70_SRGB.CRW | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| IQ4_150MPIIQ_L_16bit.iiq | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| IQ4_150MPIIQ_L_16bit.iiq | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| RGB_86_0_4.IIQ | RawSpeed | decode | all | virtual void rawspeed::IiqDecoder::checkSupportInternal(const CameraMetaData *), line 489: Couldn't find camera Phase On |
| RGB_86_0_4.IIQ | RawSpeed | decode | 1 | virtual void rawspeed::IiqDecoder::checkSupportInternal(const CameraMetaData *), line 489: Couldn't find camera Phase On |
| cap_22908.IIQ | RawSpeed | decode | all | virtual void rawspeed::IiqDecoder::checkSupportInternal(const CameraMetaData *), line 489: Couldn't find camera Phase On |
| cap_22908.IIQ | RawSpeed | decode | 1 | virtual void rawspeed::IiqDecoder::checkSupportInternal(const CameraMetaData *), line 489: Couldn't find camera Phase On |
| H25_IT8.7-2_Card.TIF | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| H25_IT8.7-2_Card.TIF | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| RAW_KODAK_DC120.KDC | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| RAW_KODAK_DC120.KDC | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| RAW_KODAK_DC50.KDC | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| RAW_KODAK_DC50.KDC | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| L_000859.mos | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| L_000859.mos | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| RAW_APTUS_75.MOS | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| RAW_APTUS_75.MOS | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| CRW_6488.CRW | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| CRW_6488.CRW | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| SDIM0555.X3F | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| SDIM0555.X3F | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| SDIM0061.X3F | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| SDIM0061.X3F | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| KODAK_C603_C643_FORMAT_NONE_YRGB0001.RAW | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| KODAK_C603_C643_FORMAT_NONE_YRGB0001.RAW | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| 2016_0104_005847_665To_send.RAW | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| 2016_0104_005847_665To_send.RAW | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const CameraMetaData *, const std::string &, const std::string &, const s |
| Pi_V1.3_picamera.raw | RawSpeed | decode | all | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
| Pi_V1.3_picamera.raw | RawSpeed | decode | 1 | virtual std::unique_ptr<RawDecoder> rawspeed::RawParser::getDecoder(const CameraMetaData *), line 97: No decoder found.  |
