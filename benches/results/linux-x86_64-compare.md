# FlashRaw compared — Linux x86-64 (Ryzen 7 9700X)

| | |
|---|---|
| Machine | AMD Ryzen 7 9700X (8 cores / 16 threads, 32 MB L3), 64 GB DDR5, Linux 6.12 (Debian 13) |
| Date | 2026-11-16 |
| Files | `benches/popular-cameras.tsv` (62 files, every library) + `benches/benchmark-set.tsv` (159 files in all; FlashRaw, LibRaw and RawSpeed) |
| FlashRaw | 0.3.1 (commit 61c32fb), `cargo build --release -p flashraw-cli --features jpegxl`, rustc 1.99.1; CPU backend (runtime-selected SIMD), 16 threads / `RAYON_NUM_THREADS=1` |
| LibRaw | 0.22.0 (4f0144031c4a), `Makefile.dist`, `-O3 -march=native -DUSE_ZLIB -DUSE_X3FTOOLS -DUSE_6BY9RPI`, GCC 14.2; all cores = OpenMP build (16 threads), 1 thread = default build |
| LibRaw + Little CMS | the LibRaw builds above + Little CMS 2.19.1 (`-O3 -march=native`, static); transform over 32-row blocks with OpenMP (all cores) or one call (1 thread) |
| RawSpeed | develop c835b05aecfa, CMake Release (`-O3 -march=native`), `WITH_OPENMP=ON` (16 threads; `OMP_NUM_THREADS=1` for one thread), `WITH_JPEG=OFF`, GCC 14.2 |
| rawler | 0.8.0 (crates.io), `cargo build --release`, rustc 1.99.1, rayon (16 threads / `RAYON_NUM_THREADS=1`) |
| rawpy | 0.27.1 wheel (LibRaw 0.22.1 inside, OpenMP), CPython 3.13.5 |
| dcraw | 9.28, `cc -O3 -march=native -DNODEPS`, GCC 14.2; one thread, whole process per run |
| Statistic | median of 7 timed runs after 1 warm-up, fresh process per (file, library, mode, threads); one full pass of the popular cameras and one of the set |
| Load | Machine otherwise idle during the runs (load average median 2.7, mostly the benchmark's own threads). Before each measurement the script waited for other processes to use under 15 % of the CPU; for 99.4 % of the measurements they did (median 1.4 %) |

Times in ms (median of the timed runs; lower is better); memory = peak RSS in MB. ❌ = the library failed or does not support the file; – = not measured. Speedup = other library's time ÷ FlashRaw's time; memory ratio = other library's peak RSS ÷ FlashRaw's (> 1 = FlashRaw uses less). **Bold** = fastest. ² dcraw has no multithreading: its one-thread time is used in the all-cores columns too.

### Summary: popular cameras (62 files)

| Comparison | threads | files | geomean speedup | median speedup | FlashRaw faster | median memory ratio |
|---|---|---:|---:|---:|---:|---:|
| decode vs LibRaw | all | 59 | **7.78×** | 12.6× | 58/59 | 0.90 |
| decode vs LibRaw | 1 | 59 | **2.87×** | 3.43× | 59/59 | 0.98 |
| decode vs RawSpeed | all | 49 | **2.71×** | 2.68× | 48/49 | 0.99 |
| decode vs RawSpeed | 1 | 49 | **1.57×** | 1.45× | 49/49 | 1.01 |
| decode vs rawler | all | 59 | **3.26×** | 3.20× | 58/59 | 1.13 |
| decode vs rawler | 1 | 59 | **2.05×** | 1.90× | 58/59 | 1.48 |
| decode vs rawpy | all | 59 | **8.45×** | 15.3× | 58/59 | 1.05 |
| decode vs rawpy | 1 | 59 | **3.11×** | 3.51× | 59/59 | 1.16 |
| decode vs dcraw | all | 42 | **19.0×** | 19.0× | 42/42 | 1.97 |
| decode vs dcraw | 1 | 42 | **5.02×** | 4.84× | 42/42 | 2.84 |
| RAW→RGB vs LibRaw | all | 59 | **9.61×** | 11.8× | 59/59 | 1.84 |
| RAW→RGB vs LibRaw | 1 | 59 | **2.23×** | 2.41× | 59/59 | 1.92 |
| RAW→sRGB vs LibRaw + Little CMS | all | 59 | **18.4×** | 20.8× | 59/59 | 2.73 |
| RAW→sRGB vs LibRaw + Little CMS | 1 | 59 | **14.5×** | 15.0× | 59/59 | 2.85 |
| RAW→RGB vs rawler | all | 59 | **7.92×** | 9.00× | 59/59 | 4.08 |
| RAW→RGB vs rawler | 1 | 59 | **5.16×** | 5.49× | 58/59 | 4.31 |
| RAW→RGB vs rawpy | all | 59 | **10.6×** | 13.2× | 59/59 | 1.94 |
| RAW→RGB vs rawpy | 1 | 59 | **2.47×** | 2.69× | 59/59 | 2.02 |
| RAW→RGB vs dcraw | all | 42 | **11.6×** | 12.5× | 42/42 | 1.30 |
| RAW→RGB vs dcraw | 1 | 42 | **2.22×** | 2.39× | 42/42 | 1.34 |

### Summary: all files (159; rawler, rawpy, dcraw and Little CMS were measured on the popular cameras only)

| Comparison | threads | files | geomean speedup | median speedup | FlashRaw faster | median memory ratio |
|---|---|---:|---:|---:|---:|---:|
| decode vs LibRaw | all | 156 | **8.19×** | 11.2× | 152/156 | 0.95 |
| decode vs LibRaw | 1 | 156 | **3.36×** | 3.45× | 152/156 | 0.97 |
| decode vs RawSpeed | all | 126 | **3.26×** | 3.28× | 120/126 | 1.00 |
| decode vs RawSpeed | 1 | 126 | **1.90×** | 1.62× | 117/126 | 1.01 |
| decode vs rawler | all | 59 | **3.26×** | 3.20× | 58/59 | 1.13 |
| decode vs rawler | 1 | 59 | **2.05×** | 1.90× | 58/59 | 1.48 |
| decode vs rawpy | all | 59 | **8.45×** | 15.3× | 58/59 | 1.05 |
| decode vs rawpy | 1 | 59 | **3.11×** | 3.51× | 59/59 | 1.16 |
| decode vs dcraw | all | 42 | **19.0×** | 19.0× | 42/42 | 1.97 |
| decode vs dcraw | 1 | 42 | **5.02×** | 4.84× | 42/42 | 2.84 |
| RAW→RGB vs LibRaw | all | 156 | **9.65×** | 11.5× | 156/156 | 1.80 |
| RAW→RGB vs LibRaw | 1 | 156 | **2.13×** | 2.25× | 154/156 | 1.88 |
| RAW→sRGB vs LibRaw + Little CMS | all | 59 | **18.4×** | 20.8× | 59/59 | 2.73 |
| RAW→sRGB vs LibRaw + Little CMS | 1 | 59 | **14.5×** | 15.0× | 59/59 | 2.85 |
| RAW→RGB vs rawler | all | 59 | **7.92×** | 9.00× | 59/59 | 4.08 |
| RAW→RGB vs rawler | 1 | 59 | **5.16×** | 5.49× | 58/59 | 4.31 |
| RAW→RGB vs rawpy | all | 59 | **10.6×** | 13.2× | 59/59 | 1.94 |
| RAW→RGB vs rawpy | 1 | 59 | **2.47×** | 2.69× | 59/59 | 2.02 |
| RAW→RGB vs dcraw | all | 42 | **11.6×** | 12.5× | 42/42 | 1.30 |
| RAW→RGB vs dcraw | 1 | 42 | **2.22×** | 2.39× | 42/42 | 1.34 |

## Popular cameras: decode, all cores (ms)

| Camera | Format · compression | MP | FlashRaw | LibRaw | RawSpeed | rawler | rawpy | dcraw ² |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | CR3 · canon-crx | 46.7 | **55.5** | 58.4 | ❌ | 183.2 | 60.3 | ❌ |
| Canon EOS R5 (C-RAW) | CR3 · canon-crx | 46.7 | 69.7 | 68.7 | ❌ | 149.2 | **67.8** | ❌ |
| Canon EOS R5 Mark II (RAW) | CR3 · canon-crx | 47.9 | **56.2** | 61.9 | ❌ | 199.7 | 61.2 | ❌ |
| Canon EOS R6 Mark II (RAW) | CR3 · canon-crx | 25.5 | **32.4** | 33.3 | ❌ | 112.0 | 39.6 | ❌ |
| Canon EOS R6 Mark III (RAW) | CR3 · canon-crx | 34.0 | **42.7** | 47.1 | ❌ | 134.2 | 51.6 | ❌ |
| Canon EOS R7 (RAW) | CR3 · canon-crx | 33.7 | **40.3** | 42.9 | ❌ | 133.8 | 43.7 | ❌ |
| Canon EOS R8 (RAW) | CR3 · canon-crx | 25.5 | **32.9** | 36.9 | ❌ | 103.7 | 39.8 | ❌ |
| Canon EOS R50 (RAW) | CR3 · canon-crx | 25.1 | **30.5** | 36.3 | ❌ | 100.0 | 36.0 | ❌ |
| Canon EOS 90D (RAW) | CR3 · canon-crx | 33.7 | **35.8** | 45.2 | ❌ | 93.1 | 43.5 | ❌ |
| Canon EOS 5D Mark IV (RAW) | CR2 · lossless-jpeg | 31.3 | **21.5** | 348.1 | 69.1 | 130.8 | 364.4 | 409.6 |
| Canon EOS 6D Mark II (RAW) | CR2 · lossless-jpeg | 27.0 | **16.4** | 270.6 | 83.4 | 129.2 | 279.2 | 311.6 |
| Canon EOS Rebel T7 / 2000D (RAW) | CR2 · lossless-jpeg | 24.7 | **20.9** | 311.2 | 88.2 | 135.6 | 335.1 | 352.4 |
| Nikon Z8 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **28.4** | 424.4 | 177.6 | 206.4 | 453.6 | 508.3 |
| Nikon Z9 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **26.5** | 434.8 | 136.6 | 174.3 | 437.0 | 496.4 |
| Nikon Z6III (lossless) | NEF · nikon-huffman | 24.5 | **15.9** | 229.6 | 86.9 | 104.8 | 241.5 | 274.1 |
| Nikon Zf (lossless) | NEF · nikon-huffman | 24.5 | **15.4** | 218.8 | 68.5 | 90.6 | 231.2 | 262.2 |
| Nikon Z7II (lossless) | NEF · nikon-huffman | 45.7 | **24.2** | 402.6 | 117.4 | 160.2 | 424.1 | 482.7 |
| Nikon Z50 (lossless) | NEF · nikon-huffman | 20.9 | **13.1** | 201.0 | 72.8 | 87.6 | 215.5 | 236.2 |
| Nikon D850 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **24.9** | 478.6 | 118.8 | 159.7 | 424.0 | 480.1 |
| Nikon D750 (14-bit lossless) | NEF · nikon-huffman | 24.3 | **16.4** | 204.7 | 58.2 | 80.8 | 215.7 | 253.7 |
| Sony A7 IV (lossless) | ARW · sony-lossless | 36.7 | **12.7** | 321.8 | 15.3 | 36.4 | 339.3 | ❌ |
| Sony A7 IV (compressed) | ARW · sony-arw2 | 33.0 | **6.02** | 80.4 | 10.5 | 12.8 | 96.4 | 173.6 |
| Sony A7R V (lossless) | ARW · sony-lossless | 64.7 | **21.9** | 575.4 | 30.7 | 61.8 | 633.5 | ❌ |
| Sony A1 (compressed) | ARW · sony-arw2 | 50.2 | **8.43** | 119.1 | 16.7 | 19.5 | 144.4 | 257.8 |
| Sony A7 III (compressed) | ARW · sony-arw2 | 24.3 | **4.04** | 62.4 | 8.25 | 8.22 | 70.9 | 128.3 |
| Sony A7C II (compressed) | ARW · sony-arw2 | 33.0 | **5.86** | 78.5 | 11.0 | 12.4 | 95.3 | 174.0 |
| Sony A9 III (compressed) | ARW · sony-arw2 | 24.3 | **4.53** | 60.3 | 8.86 | 8.62 | 72.2 | 129.8 |
| Sony A6700 (lossless) | ARW · sony-lossless | 30.7 | **12.3** | 277.3 | 23.3 | 36.6 | 305.6 | ❌ |
| Sony A6400 (compressed) | ARW · sony-arw2 | 24.3 | **4.19** | 52.8 | 8.29 | 8.48 | 66.0 | 124.5 |
| Sony ZV-E10 (compressed) | ARW · sony-arw2 | 24.3 | **4.24** | 54.5 | 7.99 | 8.37 | 66.6 | 125.1 |
| Fujifilm X-T5 (lossless compressed) | RAF · fuji-compressed | 40.9 | **47.0** | 157.9 | 121.0 | 130.3 | 159.3 | ❌ |
| Fujifilm X-H2 (compressed) | RAF · fuji-compressed | 40.9 | **62.0** | 169.7 | ❌ | 145.0 | 186.6 | ❌ |
| Fujifilm X-S20 (lossless compressed) | RAF · fuji-compressed | 26.5 | **37.6** | 138.9 | 102.8 | 112.1 | 131.6 | ❌ |
| Fujifilm X100VI (lossless compressed) | RAF · fuji-compressed | 40.9 | **46.8** | 157.8 | 117.7 | 135.7 | 153.5 | ❌ |
| Fujifilm X-T4 (uncompressed) | RAF · uncompressed | 26.7 | 8.67 | 20.8 | **8.49** | 16.0 | 17.1 | 115.1 |
| Fujifilm GFX100 II (lossless compressed) | RAF · fuji-compressed | 103.4 | **171.7** | 297.6 | 253.9 | 256.2 | 303.1 | ❌ |
| Panasonic Lumix S5II | RW2 · panasonic-v8 | 24.1 | **17.9** | 89.7 | 89.0 | 82.5 | 84.7 | 175.2 |
| Panasonic Lumix GH6 | RW2 · panasonic-v8 | 25.2 | **20.6** | 86.9 | 94.2 | 84.7 | 84.8 | 186.2 |
| Panasonic Lumix G9II | RW2 · panasonic-v8 | 25.1 | **21.5** | 96.5 | 96.2 | 87.9 | 108.4 | 198.9 |
| Panasonic Lumix S5 | RW2 · panasonic-v7 | 24.3 | **3.08** | 13.8 | 8.56 | 9.43 | 9.62 | 173.4 |
| Panasonic Lumix S1R | RW2 · panasonic-v6 | 47.6 | **8.47** | 85.9 | 15.8 | 17.8 | 93.2 | 344.5 |
| Panasonic Lumix GH5 | RW2 · panasonic-v4 | 20.6 | **6.17** | 105.3 | 16.1 | 10.3 | 96.2 | 132.0 |
| OM System OM-1 Mark II | ORF · olympus | 20.4 | **53.0** | 342.1 | 142.3 | 188.9 | 323.2 | 82.7 |
| OM System OM-5 | ORF · olympus | 20.5 | **52.1** | 284.3 | 138.1 | 202.3 | 303.9 | 80.8 |
| Olympus E-M1 Mark III | ORF · olympus | 20.5 | **51.9** | 259.4 | 133.5 | 167.0 | 247.5 | 333.6 |
| Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | **16.4** | 231.3 | 56.4 | 86.4 | 251.0 | 270.3 |
| Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | **22.6** | 313.4 | 91.1 | 135.2 | 355.2 | 379.9 |
| Ricoh GR III | DNG · lossless-jpeg | 24.8 | **18.6** | 266.5 | 68.8 | 118.4 | 285.4 | 309.9 |
| Leica Q2 | DNG · uncompressed | 47.4 | **8.19** | 192.6 | 32.3 | 9.03 | 237.6 | 280.9 |
| Leica SL2 | DNG · uncompressed | 47.4 | **8.33** | 192.7 | 32.4 | 9.79 | 237.5 | 290.7 |
| Leica M10-R | DNG · lossless-jpeg | 41.0 | **35.6** | 437.4 | 151.5 | 195.6 | 471.0 | 664.5 |
| Hasselblad X2D 100C (compressed) | FFF · hasselblad | 105.3 | **75.9** | 867.7 | 489.3 | ❌ | 1789.3 | 1217.5 |
| Phase One IQ4 150MP (IIQ L) | IIQ · phaseone | 154.0 | **55.4** | 663.7 | ❌ | 357.4 | 943.5 | 1703.4 |
| Sigma fp L | DNG · lossless-jpeg | 60.5 | **16.6** | 621.0 | 23.4 | 53.1 | 699.4 | 749.1 |
| Apple iPhone 12 Pro (ProRAW) | DNG · lossless-jpeg | 12.2 | **13.4** | 317.7 | ❌ | 26.8 | 353.0 | 324.5 |
| Google Pixel 8 Pro | DNG · lossless-jpeg | 50.1 | **14.4** | 563.9 | 21.1 | 45.2 | 564.7 | 637.2 |
| Google Pixel 6 Pro | DNG · lossless-jpeg | 12.5 | **3.04** | 129.2 | 4.04 | 11.5 | 138.6 | 161.6 |
| Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 49.9 | **118.7** | 1263.2 | 382.6 | 488.3 | 1404.1 | 1294.1 |
| Samsung Galaxy S22 (JPEG XL) | DNG · jpeg-xl | 12.0 | 317.3 | ❌ | ❌ | **227.5** | ❌ | ❌ |
| DJI Phantom 4 Pro | DNG · uncompressed | 20.0 | **2.48** | 10.4 | 3.16 | 4.96 | 25.2 | 79.5 |
| GoPro HERO11 Black (GPR) | DNG · gopro-vc5 | 27.1 | **41.4** | ❌ | 45.6 | ❌ | ❌ | ❌ |
| GoPro HERO10 Black (GPR) | DNG · gopro-vc5 | 23.3 | **21.1** | ❌ | 24.1 | ❌ | ❌ | ❌ |

## Popular cameras: RAW → RGB, all cores (ms)

| Camera | FlashRaw | LibRaw | LibRaw + Little CMS | rawler | rawpy | dcraw ² |
|---|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | **91.6** | 483.3 | 1130.9 | 639.0 | 540.0 | ❌ |
| Canon EOS R5 (C-RAW) | **103.9** | 498.4 | 1090.1 | 618.0 | 542.1 | ❌ |
| Canon EOS R5 Mark II (RAW) | **92.1** | 499.9 | 1090.7 | 640.2 | 534.8 | ❌ |
| Canon EOS R6 Mark II (RAW) | **54.8** | 278.0 | 696.3 | 376.5 | 302.8 | ❌ |
| Canon EOS R6 Mark III (RAW) | **71.6** | 359.4 | 823.6 | 502.5 | 405.3 | ❌ |
| Canon EOS R7 (RAW) | **67.5** | 352.7 | 799.4 | 466.7 | 387.7 | ❌ |
| Canon EOS R8 (RAW) | **53.2** | 271.6 | 616.4 | 365.1 | 296.0 | ❌ |
| Canon EOS R50 (RAW) | **51.9** | 268.5 | 619.7 | 355.1 | 296.6 | ❌ |
| Canon EOS 90D (RAW) | **57.9** | 358.7 | 791.3 | 433.8 | 388.2 | ❌ |
| Canon EOS 5D Mark IV (RAW) | **47.2** | 634.2 | 1071.9 | 447.0 | 726.5 | 684.0 |
| Canon EOS 6D Mark II (RAW) | **39.0** | 527.0 | 881.2 | 402.3 | 566.8 | 547.6 |
| Canon EOS Rebel T7 / 2000D (RAW) | **45.5** | 558.5 | 890.1 | 392.7 | 600.8 | 568.0 |
| Nikon Z8 (14-bit lossless) | **67.7** | 858.9 | 1475.3 | 682.3 | 931.7 | 826.3 |
| Nikon Z9 (14-bit lossless) | **71.4** | 858.0 | 1519.0 | 659.2 | 932.6 | 812.0 |
| Nikon Z6III (lossless) | **42.1** | 463.6 | 804.3 | 360.7 | 500.9 | 436.4 |
| Nikon Zf (lossless) | **35.9** | 452.8 | 774.8 | 345.2 | 489.0 | 427.0 |
| Nikon Z7II (lossless) | **65.2** | 836.1 | 1441.8 | 630.7 | 904.2 | 793.5 |
| Nikon Z50 (lossless) | **31.4** | 404.7 | 686.6 | 303.3 | 435.5 | 379.8 |
| Nikon D850 (14-bit lossless) | **66.3** | 831.8 | 1449.8 | 619.7 | 896.0 | 870.6 |
| Nikon D750 (14-bit lossless) | **37.0** | 448.2 | 751.1 | 351.3 | 476.5 | 461.2 |
| Sony A7 IV (lossless) | **39.3** | 627.4 | 1097.6 | 373.6 | 696.6 | ❌ |
| Sony A7 IV (compressed) | **32.5** | 397.6 | 838.4 | 346.8 | 445.3 | 456.4 |
| Sony A7R V (lossless) | **69.9** | 1155.3 | 1964.5 | 679.8 | 1262.8 | ❌ |
| Sony A1 (compressed) | **48.8** | 597.4 | 1266.4 | 520.2 | 670.8 | 689.2 |
| Sony A7 III (compressed) | **24.0** | 292.6 | 609.9 | 256.5 | 328.5 | 337.3 |
| Sony A7C II (compressed) | **32.5** | 392.0 | 836.3 | 351.9 | 443.7 | 456.7 |
| Sony A9 III (compressed) | **24.1** | 294.8 | 613.1 | 262.7 | 328.8 | 336.8 |
| Sony A6700 (lossless) | **33.4** | 523.3 | 906.7 | 306.0 | 583.3 | ❌ |
| Sony A6400 (compressed) | **24.3** | 285.6 | 604.4 | 260.6 | 323.1 | 330.5 |
| Sony ZV-E10 (compressed) | **21.2** | 287.2 | 602.6 | 256.9 | 323.6 | 301.3 |
| Fujifilm X-T5 (lossless compressed) | **109.5** | 555.9 | 1773.5 | 712.3 | 600.8 | ❌ |
| Fujifilm X-H2 (compressed) | **138.1** | 583.1 | 1147.3 | 623.3 | 636.6 | ❌ |
| Fujifilm X-S20 (lossless compressed) | **72.4** | 372.7 | 726.6 | 412.9 | 390.4 | ❌ |
| Fujifilm X100VI (lossless compressed) | **123.9** | 572.8 | 1104.1 | 656.0 | 632.0 | ❌ |
| Fujifilm X-T4 (uncompressed) | **40.5** | 268.9 | 620.0 | 323.7 | 292.7 | 327.6 |
| Fujifilm GFX100 II (lossless compressed) | **169.4** | 1371.6 | 3884.3 | 1530.2 | 1438.8 | ❌ |
| Panasonic Lumix S5II | **39.4** | 350.6 | 666.2 | 298.4 | 386.0 | 341.9 |
| Panasonic Lumix GH6 | **41.7** | 360.4 | 697.7 | 308.0 | 400.1 | 414.4 |
| Panasonic Lumix G9II | **45.8** | 517.4 | 1082.8 | 502.8 | 504.9 | 443.7 |
| Panasonic Lumix S5 | **30.4** | 315.1 | 683.3 | 251.6 | 312.6 | 342.2 |
| Panasonic Lumix S1R | **46.3** | 594.1 | 1232.6 | 439.7 | 865.0 | 699.2 |
| Panasonic Lumix GH5 | **46.9** | 308.9 | 570.5 | 215.8 | 316.4 | 306.8 |
| OM System OM-1 Mark II | **71.1** | 539.3 | 828.8 | 404.1 | 539.7 | 213.3 |
| OM System OM-5 | **68.8** | 477.5 | 749.0 | 394.4 | 488.6 | 213.1 |
| Olympus E-M1 Mark III | **68.8** | 454.1 | 737.2 | 379.1 | 469.6 | 541.2 |
| Pentax K-3 Mark III | **39.8** | 473.9 | 917.7 | 420.2 | 590.5 | 498.7 |
| Pentax K-1 Mark II | **58.0** | 656.0 | 1142.2 | 504.1 | 750.2 | 692.9 |
| Ricoh GR III | **39.6** | 500.8 | 824.5 | 355.6 | 540.3 | 516.4 |
| Leica Q2 | **45.3** | 636.6 | 1260.7 | 485.6 | 731.5 | 897.5 |
| Leica SL2 | **45.3** | 641.5 | 1253.2 | 480.2 | 774.0 | 679.6 |
| Leica M10-R | **69.4** | 820.0 | 1394.6 | 754.7 | 960.9 | 915.7 |
| Hasselblad X2D 100C (compressed) | **160.5** | 1822.1 | 3183.7 | ❌ | 2179.4 | 1916.3 |
| Phase One IQ4 150MP (IIQ L) | **164.8** | 2438.8 | 4438.2 | 1734.2 | 3033.9 | 3022.6 |
| Sigma fp L | **71.9** | 1193.8 | 2005.6 | 662.6 | 1316.8 | 1270.8 |
| Apple iPhone 12 Pro (ProRAW) | **30.4** | 394.2 | 539.7 | 124.5 | 433.2 | 461.2 |
| Google Pixel 8 Pro | **60.8** | 1093.6 | 1767.1 | 547.2 | 1218.6 | 1068.3 |
| Google Pixel 6 Pro | **15.6** | 253.9 | 427.3 | 145.4 | 278.5 | 268.2 |
| Samsung Galaxy S23 Ultra | **217.5** | 1497.2 | 2022.0 | 914.1 | 1702.5 | 1843.8 |
| Samsung Galaxy S22 (JPEG XL) | **362.1** | ❌ | ❌ | 484.5 | ❌ | ❌ |
| DJI Phantom 4 Pro | **28.9** | 202.3 | 473.6 | 212.4 | 238.0 | 250.4 |
| GoPro HERO11 Black (GPR) | **50.0** | ❌ | ❌ | ❌ | ❌ | ❌ |
| GoPro HERO10 Black (GPR) | **42.5** | ❌ | ❌ | ❌ | ❌ | ❌ |

## Popular cameras: decode, one thread (ms)

| Camera | Format · compression | MP | FlashRaw | LibRaw | RawSpeed | rawler | rawpy | dcraw ² |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | CR3 · canon-crx | 46.7 | **182.2** | 232.0 | ❌ | 594.8 | 233.7 | ❌ |
| Canon EOS R5 (C-RAW) | CR3 · canon-crx | 46.7 | **216.6** | 253.8 | ❌ | 444.5 | 246.3 | ❌ |
| Canon EOS R5 Mark II (RAW) | CR3 · canon-crx | 47.9 | **179.9** | 232.1 | ❌ | 599.4 | 235.4 | ❌ |
| Canon EOS R6 Mark II (RAW) | CR3 · canon-crx | 25.5 | **102.5** | 131.7 | ❌ | 350.0 | 138.1 | ❌ |
| Canon EOS R6 Mark III (RAW) | CR3 · canon-crx | 34.0 | **132.5** | 168.9 | ❌ | 414.9 | 170.3 | ❌ |
| Canon EOS R7 (RAW) | CR3 · canon-crx | 33.7 | **129.4** | 166.2 | ❌ | 423.5 | 166.5 | ❌ |
| Canon EOS R8 (RAW) | CR3 · canon-crx | 25.5 | **103.7** | 130.6 | ❌ | 323.9 | 131.1 | ❌ |
| Canon EOS R50 (RAW) | CR3 · canon-crx | 25.1 | **97.0** | 123.0 | ❌ | 305.4 | 123.8 | ❌ |
| Canon EOS 90D (RAW) | CR3 · canon-crx | 33.7 | **127.4** | 161.7 | ❌ | 407.7 | 163.3 | ❌ |
| Canon EOS 5D Mark IV (RAW) | CR2 · lossless-jpeg | 31.3 | **63.0** | 348.0 | 69.2 | 130.8 | 365.5 | 409.6 |
| Canon EOS 6D Mark II (RAW) | CR2 · lossless-jpeg | 27.0 | **74.0** | 267.7 | 83.1 | 129.7 | 279.5 | 311.6 |
| Canon EOS Rebel T7 / 2000D (RAW) | CR2 · lossless-jpeg | 24.7 | **80.4** | 317.6 | 89.0 | 136.0 | 330.8 | 352.4 |
| Nikon Z8 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **130.7** | 427.6 | 178.3 | 207.3 | 458.0 | 508.3 |
| Nikon Z9 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **101.2** | 416.1 | 135.3 | 174.4 | 440.5 | 496.4 |
| Nikon Z6III (lossless) | NEF · nikon-huffman | 24.5 | **69.1** | 231.2 | 85.4 | 104.3 | 241.4 | 274.1 |
| Nikon Zf (lossless) | NEF · nikon-huffman | 24.5 | **51.3** | 218.3 | 68.7 | 90.2 | 233.2 | 262.2 |
| Nikon Z7II (lossless) | NEF · nikon-huffman | 45.7 | **91.1** | 402.6 | 117.1 | 159.8 | 428.8 | 482.7 |
| Nikon Z50 (lossless) | NEF · nikon-huffman | 20.9 | **52.6** | 202.1 | 72.9 | 87.8 | 213.5 | 236.2 |
| Nikon D850 (14-bit lossless) | NEF · nikon-huffman | 45.7 | **92.8** | 396.9 | 118.7 | 159.8 | 426.0 | 480.1 |
| Nikon D750 (14-bit lossless) | NEF · nikon-huffman | 24.3 | **48.6** | 203.7 | 57.8 | 79.0 | 216.2 | 253.7 |
| Sony A7 IV (lossless) | ARW · sony-lossless | 36.7 | **92.3** | 316.2 | 100.0 | 161.8 | 337.7 | ❌ |
| Sony A7 IV (compressed) | ARW · sony-arw2 | 33.0 | **35.8** | 80.9 | 82.1 | 66.9 | 97.0 | 173.6 |
| Sony A7R V (lossless) | ARW · sony-lossless | 64.7 | **166.6** | 575.1 | 184.4 | 285.7 | 615.5 | ❌ |
| Sony A1 (compressed) | ARW · sony-arw2 | 50.2 | **55.8** | 123.0 | 122.2 | 100.6 | 142.2 | 257.8 |
| Sony A7 III (compressed) | ARW · sony-arw2 | 24.3 | **26.2** | 59.4 | 60.2 | 49.0 | 71.0 | 128.3 |
| Sony A7C II (compressed) | ARW · sony-arw2 | 33.0 | **36.0** | 78.8 | 80.4 | 67.3 | 94.8 | 174.0 |
| Sony A9 III (compressed) | ARW · sony-arw2 | 24.3 | **26.2** | 61.0 | 61.2 | 49.8 | 72.1 | 129.8 |
| Sony A6700 (lossless) | ARW · sony-lossless | 30.7 | **91.0** | 276.2 | 100.7 | 150.3 | 312.4 | ❌ |
| Sony A6400 (compressed) | ARW · sony-arw2 | 24.3 | **26.7** | 53.4 | 55.1 | 49.8 | 69.0 | 124.5 |
| Sony ZV-E10 (compressed) | ARW · sony-arw2 | 24.3 | **26.3** | 54.3 | 56.1 | 50.0 | 67.1 | 125.1 |
| Fujifilm X-T5 (lossless compressed) | RAF · fuji-compressed | 40.9 | **300.0** | 1118.9 | 829.4 | 882.9 | 1133.3 | ❌ |
| Fujifilm X-H2 (compressed) | RAF · fuji-compressed | 40.9 | **403.7** | 1251.6 | ❌ | 959.3 | 1303.0 | ❌ |
| Fujifilm X-S20 (lossless compressed) | RAF · fuji-compressed | 26.5 | **193.6** | 769.5 | 544.9 | 604.2 | 779.6 | ❌ |
| Fujifilm X100VI (lossless compressed) | RAF · fuji-compressed | 40.9 | **297.1** | 1126.3 | 830.6 | 886.4 | 1135.4 | ❌ |
| Fujifilm X-T4 (uncompressed) | RAF · uncompressed | 26.7 | **4.27** | 16.5 | 4.56 | 8.92 | 16.8 | 115.1 |
| Fujifilm GFX100 II (lossless compressed) | RAF · fuji-compressed | 103.4 | **907.6** | 3570.0 | 2173.1 | 1903.7 | 2601.6 | ❌ |
| Panasonic Lumix S5II | RW2 · panasonic-v8 | 24.1 | **114.1** | 163.6 | 172.5 | 156.8 | 161.6 | 175.2 |
| Panasonic Lumix GH6 | RW2 · panasonic-v8 | 25.2 | **119.4** | 170.1 | 187.5 | 164.7 | 168.1 | 186.2 |
| Panasonic Lumix G9II | RW2 · panasonic-v8 | 25.1 | **131.3** | 179.2 | 204.7 | 182.9 | 195.9 | 198.9 |
| Panasonic Lumix S5 | RW2 · panasonic-v7 | 24.3 | **5.55** | 10.1 | 14.6 | 19.8 | 10.5 | 173.4 |
| Panasonic Lumix S1R | RW2 · panasonic-v6 | 47.6 | **33.2** | 85.3 | 87.4 | 83.5 | 93.5 | 344.5 |
| Panasonic Lumix GH5 | RW2 · panasonic-v4 | 20.6 | **35.6** | 56.0 | 46.0 | 58.5 | 61.0 | 132.0 |
| OM System OM-1 Mark II | ORF · olympus | 20.4 | **77.5** | 314.5 | 142.5 | 188.7 | 323.0 | 82.7 |
| OM System OM-5 | ORF · olympus | 20.5 | **76.3** | 262.7 | 138.0 | 188.9 | 273.9 | 80.8 |
| Olympus E-M1 Mark III | ORF · olympus | 20.5 | **76.3** | 243.7 | 133.8 | 166.0 | 246.6 | 333.6 |
| Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | **52.8** | 222.7 | 56.1 | 86.2 | 250.9 | 270.3 |
| Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | **84.9** | 488.0 | 92.0 | 135.2 | 359.0 | 379.9 |
| Ricoh GR III | DNG · lossless-jpeg | 24.8 | **56.7** | 263.8 | 69.1 | 105.0 | 286.7 | 309.9 |
| Leica Q2 | DNG · uncompressed | 47.4 | **15.7** | 194.6 | 32.3 | 32.1 | 240.4 | 280.9 |
| Leica SL2 | DNG · uncompressed | 47.4 | **15.6** | 198.1 | 34.4 | 32.3 | 242.3 | 290.7 |
| Leica M10-R | DNG · lossless-jpeg | 41.0 | **99.7** | 438.1 | 151.4 | 203.3 | 476.0 | 664.5 |
| Hasselblad X2D 100C (compressed) | FFF · hasselblad | 105.3 | **399.5** | 849.2 | 489.9 | ❌ | 1025.9 | 1217.5 |
| Phase One IQ4 150MP (IIQ L) | IIQ · phaseone | 154.0 | **410.5** | 651.9 | ❌ | 799.3 | 936.0 | 1703.4 |
| Sigma fp L | DNG · lossless-jpeg | 60.5 | **135.4** | 625.3 | 163.0 | 323.5 | 684.8 | 749.1 |
| Apple iPhone 12 Pro (ProRAW) | DNG · lossless-jpeg | 12.2 | **89.8** | 318.2 | ❌ | 186.4 | 345.2 | 324.5 |
| Google Pixel 8 Pro | DNG · lossless-jpeg | 50.1 | **107.2** | 521.7 | 131.4 | 272.2 | 582.9 | 637.2 |
| Google Pixel 6 Pro | DNG · lossless-jpeg | 12.5 | **27.4** | 130.5 | 35.1 | 68.2 | 139.3 | 161.6 |
| Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 49.9 | **344.0** | 1265.2 | 500.1 | 611.0 | 1414.1 | 1294.1 |
| Samsung Galaxy S22 (JPEG XL) | DNG · jpeg-xl | 12.0 | 491.3 | ❌ | ❌ | **353.4** | ❌ | ❌ |
| DJI Phantom 4 Pro | DNG · uncompressed | 20.0 | **2.76** | 10.4 | 3.21 | 4.03 | 25.1 | 79.5 |
| GoPro HERO11 Black (GPR) | DNG · gopro-vc5 | 27.1 | **131.1** | ❌ | 201.9 | ❌ | ❌ | ❌ |
| GoPro HERO10 Black (GPR) | DNG · gopro-vc5 | 23.3 | **109.3** | ❌ | 186.3 | ❌ | ❌ | ❌ |

## Popular cameras: RAW → RGB, one thread (ms)

| Camera | FlashRaw | LibRaw | LibRaw + Little CMS | rawler | rawpy | dcraw ² |
|---|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | **412.5** | 713.9 | 5162.9 | 2358.6 | 759.0 | ❌ |
| Canon EOS R5 (C-RAW) | **453.7** | 748.7 | 5208.2 | 2206.7 | 765.8 | ❌ |
| Canon EOS R5 Mark II (RAW) | **411.8** | 722.9 | 5222.8 | 2368.6 | 782.5 | ❌ |
| Canon EOS R6 Mark II (RAW) | **229.8** | 391.6 | 2808.1 | 1273.3 | 412.6 | ❌ |
| Canon EOS R6 Mark III (RAW) | **299.7** | 508.9 | 3743.6 | 1628.8 | 545.1 | ❌ |
| Canon EOS R7 (RAW) | **297.4** | 506.7 | 3777.3 | 1681.0 | 548.1 | ❌ |
| Canon EOS R8 (RAW) | **229.7** | 391.1 | 2786.0 | 1238.1 | 413.4 | ❌ |
| Canon EOS R50 (RAW) | **220.2** | 375.5 | 2780.4 | 1211.7 | 402.4 | ❌ |
| Canon EOS 90D (RAW) | **295.8** | 506.5 | 3772.4 | 1674.7 | 539.8 | ❌ |
| Canon EOS 5D Mark IV (RAW) | **219.5** | 669.3 | 3807.2 | 1307.8 | 719.2 | 684.0 |
| Canon EOS 6D Mark II (RAW) | **213.2** | 555.5 | 3242.3 | 1127.8 | 598.8 | 547.6 |
| Canon EOS Rebel T7 / 2000D (RAW) | **208.7** | 579.6 | 3003.0 | 1080.0 | 626.0 | 568.0 |
| Nikon Z8 (14-bit lossless) | **371.1** | 910.1 | 5506.9 | 1967.8 | 997.6 | 826.3 |
| Nikon Z9 (14-bit lossless) | **347.5** | 911.1 | 5455.1 | 1939.8 | 981.3 | 812.0 |
| Nikon Z6III (lossless) | **196.3** | 488.0 | 2957.7 | 1049.6 | 527.2 | 436.4 |
| Nikon Zf (lossless) | **177.8** | 477.3 | 2939.3 | 1035.2 | 520.7 | 427.0 |
| Nikon Z7II (lossless) | **331.0** | 894.9 | 5460.6 | 1933.1 | 952.7 | 793.5 |
| Nikon Z50 (lossless) | **159.7** | 430.0 | 2533.3 | 912.1 | 463.0 | 379.8 |
| Nikon D850 (14-bit lossless) | **331.6** | 877.6 | 5517.5 | 1948.1 | 951.4 | 870.6 |
| Nikon D750 (14-bit lossless) | **174.2** | 460.4 | 2807.1 | 1002.7 | 498.8 | 461.2 |
| Sony A7 IV (lossless) | **263.1** | 663.1 | 3958.2 | 1374.3 | 727.8 | ❌ |
| Sony A7 IV (compressed) | **208.0** | 432.8 | 3719.3 | 1301.7 | 479.7 | 456.4 |
| Sony A7R V (lossless) | **479.4** | 1223.5 | 7354.7 | 2556.1 | 1339.4 | ❌ |
| Sony A1 (compressed) | **318.8** | 652.6 | 5670.7 | 1917.7 | 726.7 | 689.2 |
| Sony A7 III (compressed) | **152.1** | 316.7 | 2745.3 | 982.2 | 356.2 | 337.3 |
| Sony A7C II (compressed) | **208.1** | 426.6 | 3725.3 | 1339.0 | 476.2 | 456.7 |
| Sony A9 III (compressed) | **152.1** | 316.6 | 2736.8 | 985.8 | 355.9 | 336.8 |
| Sony A6700 (lossless) | **224.7** | 563.8 | 3168.4 | 1158.5 | 612.9 | ❌ |
| Sony A6400 (compressed) | **152.3** | 309.2 | 2729.0 | 999.1 | 347.7 | 330.5 |
| Sony ZV-E10 (compressed) | **132.5** | 318.0 | 2747.7 | 1046.3 | 358.9 | 301.3 |
| Fujifilm X-T5 (lossless compressed) | **665.1** | 1604.8 | 5672.5 | 2971.5 | 1634.2 | ❌ |
| Fujifilm X-H2 (compressed) | **821.1** | 1718.6 | 5728.6 | 3083.8 | 1834.9 | ❌ |
| Fujifilm X-S20 (lossless compressed) | **438.5** | 1075.3 | 3697.5 | 1954.8 | 1089.2 | ❌ |
| Fujifilm X100VI (lossless compressed) | **720.6** | 2015.8 | 8516.5 | 3575.9 | 2200.3 | ❌ |
| Fujifilm X-T4 (uncompressed) | **245.3** | 313.4 | 3372.0 | 1481.2 | 450.4 | 327.6 |
| Fujifilm GFX100 II (lossless compressed) | **1406.4** | 3891.1 | 17484.2 | 6174.8 | 3643.5 | ❌ |
| Panasonic Lumix S5II | **239.1** | 448.1 | 2843.7 | 1024.7 | 489.6 | 341.9 |
| Panasonic Lumix GH6 | **250.1** | 467.4 | 4229.7 | 1353.0 | 707.3 | 414.4 |
| Panasonic Lumix G9II | **351.8** | 674.7 | 4114.1 | 1382.7 | 640.0 | 443.7 |
| Panasonic Lumix S5 | **130.9** | 298.3 | 2719.6 | 971.7 | 337.2 | 342.2 |
| Panasonic Lumix S1R | **280.7** | 642.7 | 5554.0 | 1826.9 | 725.9 | 699.2 |
| Panasonic Lumix GH5 | **141.2** | 297.6 | 2320.5 | 807.7 | 336.3 | 306.8 |
| OM System OM-1 Mark II | **182.8** | 531.3 | 2627.1 | 951.4 | 561.6 | 213.3 |
| OM System OM-5 | **181.1** | 480.1 | 2543.4 | 942.1 | 509.7 | 213.1 |
| Olympus E-M1 Mark III | **181.4** | 465.8 | 3255.3 | 982.4 | 609.3 | 541.2 |
| Pentax K-3 Mark III | **203.8** | 496.5 | 3083.5 | 1190.2 | 552.1 | 498.7 |
| Pentax K-1 Mark II | **275.9** | 710.4 | 4625.8 | 1512.6 | 774.5 | 692.9 |
| Ricoh GR III | **183.3** | 521.9 | 3157.8 | 1061.1 | 568.1 | 516.4 |
| Leica Q2 | **264.1** | 690.9 | 5453.1 | 1857.6 | 783.3 | 897.5 |
| Leica SL2 | **263.7** | 707.9 | 5392.0 | 1844.8 | 799.1 | 679.6 |
| Leica M10-R | **315.5** | 877.9 | 6167.0 | 1739.0 | 1169.3 | 915.7 |
| Hasselblad X2D 100C (compressed) | **928.3** | 1928.9 | 12253.0 | ❌ | 2231.8 | 1916.3 |
| Phase One IQ4 150MP (IIQ L) | **1195.7** | 2600.3 | 17757.7 | 6460.3 | 3224.2 | 3022.6 |
| Sigma fp L | **496.2** | 1258.6 | 7340.3 | 2709.4 | 1398.1 | 1270.8 |
| Apple iPhone 12 Pro (ProRAW) | **172.8** | 385.1 | 1572.5 | 400.0 | 433.3 | 461.2 |
| Google Pixel 8 Pro | **402.3** | 1146.6 | 6169.3 | 2209.6 | 1278.2 | 1068.3 |
| Google Pixel 6 Pro | **102.3** | 261.3 | 1520.9 | 554.3 | 291.4 | 268.2 |
| Samsung Galaxy S23 Ultra | **1101.2** | 1496.2 | 8189.9 | 1559.9 | 1680.2 | 1843.8 |
| Samsung Galaxy S22 (JPEG XL) | 678.7 | ❌ | ❌ | **556.8** | ❌ | ❌ |
| DJI Phantom 4 Pro | **168.5** | 221.6 | 2213.8 | 783.4 | 259.0 | 250.4 |
| GoPro HERO11 Black (GPR) | **291.3** | ❌ | ❌ | ❌ | ❌ | ❌ |
| GoPro HERO10 Black (GPR) | **246.8** | ❌ | ❌ | ❌ | ❌ | ❌ |

## Popular cameras: peak memory, all cores (MB)

| Camera | MP | decode: FlashRaw | LibRaw | RawSpeed | rawler | rawpy | dcraw ² | RGB: FlashRaw | LibRaw | LibRaw + Little CMS | rawler | rawpy | dcraw ² |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | 46.7 | 248 | 134 | ❌ | 342 | 157 | ❌ | 444 | 604 | 903 | 1408 | 627 | ❌ |
| Canon EOS R5 (C-RAW) | 46.7 | 279 | 122 | ❌ | 317 | 146 | ❌ | 430 | 592 | 891 | 1349 | 616 | ❌ |
| Canon EOS R5 Mark II (RAW) | 47.9 | 358 | 149 | ❌ | 364 | 173 | ❌ | 487 | 623 | 925 | 1420 | 647 | ❌ |
| Canon EOS R6 Mark II (RAW) | 25.5 | 195 | 83 | ❌ | 203 | 106 | ❌ | 289 | 337 | 499 | 759 | 361 | ❌ |
| Canon EOS R6 Mark III (RAW) | 34.0 | 203 | 104 | ❌ | 284 | 128 | ❌ | 346 | 443 | 659 | 1071 | 467 | ❌ |
| Canon EOS R7 (RAW) | 33.7 | 249 | 101 | ❌ | 267 | 125 | ❌ | 343 | 443 | 661 | 1054 | 467 | ❌ |
| Canon EOS R8 (RAW) | 25.5 | 182 | 82 | ❌ | 202 | 106 | ❌ | 277 | 337 | 499 | 758 | 360 | ❌ |
| Canon EOS R50 (RAW) | 25.1 | 195 | 84 | ❌ | 202 | 108 | ❌ | 290 | 337 | 497 | 782 | 360 | ❌ |
| Canon EOS 90D (RAW) | 33.7 | 228 | 104 | ❌ | 259 | 128 | ❌ | 346 | 446 | 664 | 1013 | 470 | ❌ |
| Canon EOS 5D Mark IV (RAW) | 31.3 | 275 | 97 | 100 | 172 | 122 | 294 | 329 | 416 | 619 | 929 | 439 | 294 |
| Canon EOS 6D Mark II (RAW) | 27.0 | 213 | 93 | 96 | 109 | 117 | 254 | 320 | 368 | 543 | 812 | 392 | 254 |
| Canon EOS Rebel T7 / 2000D (RAW) | 24.7 | 245 | 90 | 93 | 153 | 114 | 234 | 293 | 343 | 505 | 752 | 367 | 234 |
| Nikon Z8 (14-bit lossless) | 45.7 | 293 | 151 | 154 | 218 | 175 | 438 | 346 | 630 | 936 | 1389 | 654 | 439 |
| Nikon Z9 (14-bit lossless) | 45.7 | 279 | 148 | 151 | 211 | 172 | 438 | 335 | 627 | 933 | 1385 | 651 | 438 |
| Nikon Z6III (lossless) | 24.5 | 164 | 84 | 86 | 127 | 108 | 236 | 194 | 340 | 505 | 782 | 365 | 236 |
| Nikon Zf (lossless) | 24.5 | 146 | 82 | 85 | 124 | 106 | 236 | 184 | 339 | 503 | 779 | 362 | 236 |
| Nikon Z7II (lossless) | 45.7 | 257 | 144 | 147 | 207 | 168 | 439 | 329 | 625 | 930 | 1383 | 648 | 439 |
| Nikon Z50 (lossless) | 20.9 | 131 | 75 | 78 | 114 | 99 | 202 | 164 | 294 | 434 | 673 | 318 | 202 |
| Nikon D850 (14-bit lossless) | 45.7 | 254 | 145 | 148 | 207 | 169 | 439 | 330 | 626 | 931 | 1384 | 649 | 439 |
| Nikon D750 (14-bit lossless) | 24.3 | 136 | 77 | 80 | 116 | 101 | 235 | 179 | 332 | 495 | 767 | 355 | 235 |
| Sony A7 IV (lossless) | 36.7 | 131 | 122 | 126 | 200 | 146 | ❌ | 257 | 468 | 689 | 1047 | 491 | ❌ |
| Sony A7 IV (compressed) | 33.0 | 109 | 107 | 111 | 123 | 131 | 317 | 238 | 452 | 673 | 1005 | 477 | 317 |
| Sony A7R V (lossless) | 64.7 | 221 | 211 | 215 | 323 | 235 | ❌ | 451 | 851 | 1258 | 1885 | 874 | ❌ |
| Sony A1 (compressed) | 50.2 | 164 | 161 | 165 | 178 | 185 | 481 | 358 | 687 | 1022 | 1518 | 711 | 481 |
| Sony A7 III (compressed) | 24.3 | 79 | 76 | 80 | 117 | 100 | 234 | 173 | 330 | 493 | 768 | 354 | 234 |
| Sony A7C II (compressed) | 33.0 | 109 | 107 | 111 | 123 | 131 | 317 | 237 | 453 | 672 | 1005 | 477 | 317 |
| Sony A9 III (compressed) | 24.3 | 82 | 80 | 84 | 124 | 103 | 234 | 176 | 333 | 495 | 773 | 356 | 234 |
| Sony A6700 (lossless) | 30.7 | 115 | 106 | 110 | 177 | 130 | ❌ | 213 | 379 | 552 | 848 | 402 | ❌ |
| Sony A6400 (compressed) | 24.3 | 77 | 75 | 79 | 115 | 100 | 234 | 172 | 330 | 492 | 766 | 353 | 234 |
| Sony ZV-E10 (compressed) | 24.3 | 78 | 76 | 79 | 115 | 100 | 234 | 158 | 330 | 492 | 766 | 354 | 234 |
| Fujifilm X-T5 (lossless compressed) | 40.9 | 128 | 125 | 128 | 180 | 149 | ❌ | 282 | 548 | 815 | 1219 | 571 | ❌ |
| Fujifilm X-H2 (compressed) | 40.9 | 116 | 112 | ❌ | 161 | 136 | ❌ | 269 | 534 | 803 | 1237 | 558 | ❌ |
| Fujifilm X-S20 (lossless compressed) | 26.5 | 91 | 89 | 92 | 133 | 112 | ❌ | 193 | 364 | 539 | 831 | 387 | ❌ |
| Fujifilm X100VI (lossless compressed) | 40.9 | 134 | 132 | 134 | 190 | 155 | ❌ | 288 | 554 | 822 | 1224 | 578 | ❌ |
| Fujifilm X-T4 (uncompressed) | 26.7 | 114 | 111 | 114 | 178 | 135 | 252 | 216 | 385 | 560 | 827 | 408 | 252 |
| Fujifilm GFX100 II (lossless compressed) | 103.4 | 312 | 310 | 312 | 438 | 334 | ❌ | 705 | 1381 | 2062 | 3063 | 1405 | ❌ |
| Panasonic Lumix S5II | 24.1 | 223 | 89 | 90 | 139 | 112 | 232 | 246 | 342 | 503 | 781 | 365 | 232 |
| Panasonic Lumix GH6 | 25.2 | 217 | 98 | 99 | 155 | 121 | 243 | 215 | 363 | 531 | 790 | 386 | 243 |
| Panasonic Lumix G9II | 25.1 | 221 | 95 | 96 | 149 | 118 | 242 | 203 | 358 | 527 | 786 | 382 | 242 |
| Panasonic Lumix S5 | 24.3 | 96 | 93 | 97 | 151 | 117 | 233 | 190 | 347 | 509 | 752 | 371 | 233 |
| Panasonic Lumix S1R | 47.6 | 165 | 163 | 167 | 246 | 187 | 453 | 348 | 658 | 973 | 1431 | 682 | 453 |
| Panasonic Lumix GH5 | 20.6 | 70 | 68 | 72 | 107 | 92 | 197 | 150 | 281 | 417 | 647 | 305 | 197 |
| OM System OM-1 Mark II | 20.4 | 70 | 66 | 69 | 103 | 90 | 197 | 148 | 281 | 417 | 650 | 304 | 197 |
| OM System OM-5 | 20.5 | 67 | 63 | 66 | 97 | 87 | 198 | 145 | 278 | 416 | 646 | 302 | 198 |
| Olympus E-M1 Mark III | 20.5 | 65 | 62 | 65 | 94 | 86 | 198 | 144 | 277 | 414 | 643 | 300 | 198 |
| Pentax K-3 Mark III | 26.6 | 161 | 87 | 90 | 102 | 111 | 257 | 195 | 359 | 533 | 798 | 383 | 257 |
| Pentax K-1 Mark II | 36.6 | 237 | 121 | 123 | 136 | 145 | 352 | 279 | 503 | 746 | 1110 | 526 | 352 |
| Ricoh GR III | 24.8 | 233 | 85 | 87 | 148 | 109 | 235 | 275 | 339 | 501 | 751 | 363 | 235 |
| Leica Q2 | 47.4 | 182 | 179 | 182 | 196 | 203 | 454 | 364 | 675 | 991 | 1459 | 699 | 454 |
| Leica SL2 | 47.4 | 183 | 181 | 184 | 197 | 205 | 454 | 366 | 675 | 990 | 1460 | 698 | 454 |
| Leica M10-R | 41.0 | 288 | 136 | 139 | 230 | 160 | 394 | 307 | 566 | 840 | 1248 | 590 | 394 |
| Hasselblad X2D 100C (compressed) | 105.3 | 655 | 360 | 362 | ❌ | 384 | 1007 | 769 | 1431 | 2112 | ❌ | 1454 | 1007 |
| Phase One IQ4 150MP (IIQ L) | 154.0 | 462 | 460 | ❌ | 637 | 484 | 1451 | 1046 | 2047 | 3058 | 4527 | 2071 | 1451 |
| Sigma fp L | 60.5 | 214 | 209 | 214 | 354 | 233 | 580 | 447 | 844 | 1248 | 1850 | 867 | 580 |
| Apple iPhone 12 Pro (ProRAW) | 12.2 | 115 | 126 | ❌ | 144 | 150 | 96 | 161 | 255 | 336 | 563 | 278 | 96 |
| Google Pixel 8 Pro | 50.1 | 153 | 144 | 149 | 271 | 168 | 481 | 345 | 671 | 1005 | 1514 | 694 | 481 |
| Google Pixel 6 Pro | 12.5 | 45 | 41 | 45 | 90 | 65 | 122 | 94 | 172 | 256 | 425 | 195 | 122 |
| Samsung Galaxy S23 Ultra | 49.9 | 657 | 463 | 371 | 669 | 487 | 384 | 660 | 987 | 1321 | 2098 | 1011 | 384 |
| Samsung Galaxy S22 (JPEG XL) | 12.0 | 433 | ❌ | ❌ | 308 | ❌ | ❌ | 436 | ❌ | ❌ | 545 | ❌ | ❌ |
| DJI Phantom 4 Pro | 20.0 | 84 | 82 | 85 | 98 | 106 | 193 | 179 | 291 | 425 | 632 | 315 | 193 |
| GoPro HERO11 Black (GPR) | 27.1 | 221 | ❌ | 387 | ❌ | ❌ | ❌ | 252 | ❌ | ❌ | ❌ | ❌ | ❌ |
| GoPro HERO10 Black (GPR) | 23.3 | 169 | ❌ | 351 | ❌ | ❌ | ❌ | 216 | ❌ | ❌ | ❌ | ❌ | ❌ |

## LibRaw + Little CMS pipeline, stages (all cores, ms)

decode = LibRaw `open_buffer` + `unpack`; develop = `dcraw_process` to 16-bit linear ProPhoto + `dcraw_make_mem_image`; Little CMS = profiles, transform and conversion to 8-bit sRGB RGBA; FlashRaw = RAW → RGBA8 in one pass.

| Camera | LibRaw decode | LibRaw develop | Little CMS | total | FlashRaw | speedup |
|---|---:|---:|---:|---:|---:|---:|
| Canon EOS R5 (RAW) | 63.6 | 439.7 | 626.1 | 1130.9 | **91.6** | 12.3× |
| Canon EOS R5 (C-RAW) | 70.6 | 435.0 | 583.4 | 1090.1 | **103.9** | 10.5× |
| Canon EOS R5 Mark II (RAW) | 61.4 | 436.1 | 588.5 | 1090.7 | **92.1** | 11.8× |
| Canon EOS R6 Mark II (RAW) | 46.5 | 266.5 | 377.6 | 696.3 | **54.8** | 12.7× |
| Canon EOS R6 Mark III (RAW) | 48.9 | 319.6 | 450.6 | 823.6 | **71.6** | 11.5× |
| Canon EOS R7 (RAW) | 44.1 | 322.8 | 430.6 | 799.4 | **67.5** | 11.8× |
| Canon EOS R8 (RAW) | 37.5 | 242.3 | 336.5 | 616.4 | **53.2** | 11.6× |
| Canon EOS R50 (RAW) | 38.0 | 238.1 | 342.6 | 619.7 | **51.9** | 11.9× |
| Canon EOS 90D (RAW) | 44.8 | 313.6 | 432.2 | 791.3 | **57.9** | 13.7× |
| Canon EOS 5D Mark IV (RAW) | 351.7 | 295.7 | 426.0 | 1071.9 | **47.2** | 22.7× |
| Canon EOS 6D Mark II (RAW) | 270.1 | 256.4 | 352.6 | 881.2 | **39.0** | 22.6× |
| Canon EOS Rebel T7 / 2000D (RAW) | 323.1 | 240.0 | 334.0 | 890.1 | **45.5** | 19.6× |
| Nikon Z8 (14-bit lossless) | 427.0 | 440.1 | 602.6 | 1475.3 | **67.7** | 21.8× |
| Nikon Z9 (14-bit lossless) | 420.1 | 450.1 | 645.2 | 1519.0 | **71.4** | 21.3× |
| Nikon Z6III (lossless) | 233.4 | 245.1 | 323.3 | 804.3 | **42.1** | 19.1× |
| Nikon Zf (lossless) | 220.4 | 235.0 | 320.1 | 774.8 | **35.9** | 21.6× |
| Nikon Z7II (lossless) | 404.4 | 440.0 | 599.9 | 1441.8 | **65.2** | 22.1× |
| Nikon Z50 (lossless) | 205.0 | 202.3 | 278.7 | 686.6 | **31.4** | 21.8× |
| Nikon D850 (14-bit lossless) | 401.5 | 452.2 | 596.5 | 1449.8 | **66.3** | 21.9× |
| Nikon D750 (14-bit lossless) | 205.3 | 234.9 | 306.4 | 751.1 | **37.0** | 20.3× |
| Sony A7 IV (lossless) | 323.0 | 321.0 | 447.9 | 1097.6 | **39.3** | 27.9× |
| Sony A7 IV (compressed) | 83.3 | 318.4 | 436.2 | 838.4 | **32.5** | 25.8× |
| Sony A7R V (lossless) | 584.6 | 582.7 | 797.7 | 1964.5 | **69.9** | 28.1× |
| Sony A1 (compressed) | 122.0 | 483.1 | 661.2 | 1266.4 | **48.8** | 26.0× |
| Sony A7 III (compressed) | 61.1 | 234.0 | 314.7 | 609.9 | **24.0** | 25.4× |
| Sony A7C II (compressed) | 81.3 | 317.1 | 436.8 | 836.3 | **32.5** | 25.7× |
| Sony A9 III (compressed) | 62.3 | 234.2 | 315.5 | 613.1 | **24.1** | 25.4× |
| Sony A6700 (lossless) | 307.5 | 250.5 | 348.8 | 906.7 | **33.4** | 27.1× |
| Sony A6400 (compressed) | 55.1 | 233.9 | 312.9 | 604.4 | **24.3** | 24.8× |
| Sony ZV-E10 (compressed) | 56.6 | 234.3 | 310.7 | 602.6 | **21.2** | 28.4× |
| Fujifilm X-T5 (lossless compressed) | 291.6 | 630.0 | 833.7 | 1773.5 | **109.5** | 16.2× |
| Fujifilm X-H2 (compressed) | 172.9 | 416.9 | 560.6 | 1147.3 | **138.1** | 8.31× |
| Fujifilm X-S20 (lossless compressed) | 121.0 | 256.5 | 350.5 | 726.6 | **72.4** | 10.0× |
| Fujifilm X100VI (lossless compressed) | 160.0 | 415.6 | 525.9 | 1104.1 | **123.9** | 8.91× |
| Fujifilm X-T4 (uncompressed) | 17.4 | 254.4 | 348.7 | 620.0 | **40.5** | 15.3× |
| Fujifilm GFX100 II (lossless compressed) | 461.9 | 1253.7 | 1902.4 | 3884.3 | **169.4** | 22.9× |
| Panasonic Lumix S5II | 88.0 | 264.0 | 315.0 | 666.2 | **39.4** | 16.9× |
| Panasonic Lumix GH6 | 87.7 | 275.9 | 333.5 | 697.7 | **41.7** | 16.7× |
| Panasonic Lumix G9II | 122.8 | 439.5 | 476.1 | 1082.8 | **45.8** | 23.6× |
| Panasonic Lumix S5 | 11.6 | 281.4 | 385.2 | 683.3 | **30.4** | 22.5× |
| Panasonic Lumix S1R | 87.6 | 521.0 | 612.2 | 1232.6 | **46.3** | 26.6× |
| Panasonic Lumix GH5 | 58.7 | 224.9 | 287.2 | 570.5 | **46.9** | 12.2× |
| OM System OM-1 Mark II | 336.9 | 201.1 | 291.2 | 828.8 | **71.1** | 11.7× |
| OM System OM-5 | 277.1 | 198.0 | 273.7 | 749.0 | **68.8** | 10.9× |
| Olympus E-M1 Mark III | 268.4 | 199.3 | 279.4 | 737.2 | **68.8** | 10.7× |
| Pentax K-3 Mark III | 241.7 | 255.0 | 404.0 | 917.7 | **39.8** | 23.1× |
| Pentax K-1 Mark II | 316.2 | 350.4 | 474.4 | 1142.2 | **58.0** | 19.7× |
| Ricoh GR III | 274.0 | 235.6 | 313.2 | 824.5 | **39.6** | 20.8× |
| Leica Q2 | 194.4 | 454.4 | 612.0 | 1260.7 | **45.3** | 27.8× |
| Leica SL2 | 194.9 | 455.2 | 603.1 | 1253.2 | **45.3** | 27.7× |
| Leica M10-R | 455.5 | 398.1 | 544.8 | 1394.6 | **69.4** | 20.1× |
| Hasselblad X2D 100C (compressed) | 856.8 | 985.8 | 1338.9 | 3183.7 | **160.5** | 19.8× |
| Phase One IQ4 150MP (IIQ L) | 652.8 | 1800.4 | 1983.9 | 4438.2 | **164.8** | 26.9× |
| Sigma fp L | 631.6 | 581.1 | 788.2 | 2005.6 | **71.9** | 27.9× |
| Apple iPhone 12 Pro (ProRAW) | 317.9 | 65.5 | 155.1 | 539.7 | **30.4** | 17.8× |
| Google Pixel 8 Pro | 527.3 | 584.7 | 644.9 | 1767.1 | **60.8** | 29.1× |
| Google Pixel 6 Pro | 132.1 | 122.1 | 165.6 | 427.3 | **15.6** | 27.3× |
| Samsung Galaxy S23 Ultra | 1273.9 | 232.8 | 517.9 | 2022.0 | **217.5** | 9.30× |
| DJI Phantom 4 Pro | 11.7 | 193.3 | 268.2 | 473.6 | **28.9** | 16.4× |

## Per format (geometric mean speedup of FlashRaw, all cores)

| Format | Compression | files | decode vs LibRaw | decode vs RawSpeed | decode vs rawler | decode vs rawpy | decode vs dcraw | RGB vs LibRaw | RGB vs LibRaw + LCMS | RGB vs rawler | RGB vs rawpy | RGB vs dcraw | RGB, default demosaics (FlashRaw auto, LibRaw AHD / Markesteijn), vs LibRaw |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 3FR | hasselblad | 2 | 10.8× | 6.22× | – | – | – | 11.4× | – | – | – | – | – |
| 3FR | uncompressed | 2 | 1.29× | 1.09× | – | – | – | 12.1× | – | – | – | – | – |
| ARW | other | 2 | 7.22× | ❌ | – | – | – | 9.35× | – | – | – | – | – |
| ARW | sony-arw1 | 2 | 22.1× | 19.7× | – | – | – | 13.1× | – | – | – | – | – |
| ARW | sony-arw2 | 9 | 13.3× | 1.89× | 2.07× | 16.3× | 29.8× | 12.3× | 25.9× | 10.9× | 13.8× | 14.0× | – |
| ARW | sony-lossless | 4 | 16.0× | 1.48× | 2.89× | 26.8× | ❌ | 11.8× | 27.7× | 9.46× | 17.8× | ❌ | – |
| ARW | uncompressed | 2 | 3.55× | 0.63× | – | – | – | 6.70× | – | – | – | – | – |
| CR2 | canon-sraw | 2 | 10.6× | 2.63× | – | – | – | 8.93× | – | – | – | – | – |
| CR2 | lossless-jpeg | 5 | 16.0× | 3.94× | 6.77× | 16.7× | 18.3× | 13.3× | 21.6× | 9.45× | 14.4× | 13.7× | – |
| CR3 | canon-crx | 11 | 1.09× | ❌ | 3.07× | 1.14× | ❌ | 5.25× | 12.0× | 6.86× | 5.75× | ❌ | – |
| CRW | canon-crw | 2 | 4.00× | 2.68× | – | – | – | 5.77× | – | – | – | – | – |
| CRW | uncompressed | 1 | 19.8× | ❌ | – | – | – | 7.66× | – | – | – | – | – |
| DCR | kodak-65000 | 2 | 18.0× | 17.2× | – | – | – | 13.3× | – | – | – | – | – |
| DCS | lossless-jpeg | 1 | 10.5× | 2.43× | – | – | – | 14.5× | – | – | – | – | – |
| DCS | uncompressed | 2 | 6.27× | 27.7× | – | – | – | 13.3× | – | – | – | – | – |
| DNG | deflate | 2 | 11.4× | 1.25× | – | – | – | 12.8× | – | – | – | – | – |
| DNG | gopro-vc5 | 2 | ❌ | 1.12× | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | – |
| DNG | jpeg-xl | 1 | ❌ | ❌ | 0.72× | ❌ | ❌ | ❌ | ❌ | 1.34× | ❌ | ❌ | – |
| DNG | lossless-jpeg | 9 | 20.2× | 2.54× | 3.78× | 24.1× | 26.3× | 12.8× | 20.5× | 7.47× | 14.5× | 14.2× | – |
| DNG | uncompressed | 5 | 8.56× | 3.75× | 1.37× | 20.3× | 33.7× | 8.27× | 23.3× | 9.42× | 13.2× | 13.7× | – |
| ERF | uncompressed | 2 | 15.9× | 3.44× | – | – | – | 11.7× | – | – | – | – | – |
| FFF | hasselblad | 2 | 11.2× | 6.43× | ❌ | 23.6× | 16.0× | 11.3× | 19.8× | ❌ | 13.6× | 11.9× | – |
| IIQ | phaseone | 3 | 12.4× | 22.1× | 6.45× | 17.0× | 30.7× | 13.0× | 26.9× | 10.5× | 18.4× | 18.3× | – |
| IIQ | phaseone-s | 2 | 6.73× | ❌ | – | – | – | 9.73× | – | – | – | – | – |
| IIQ | uncompressed | 1 | 1.27× | ❌ | – | – | – | 10.8× | – | – | – | – | – |
| KDC | kodak-dc120 | 1 | 16.6× | ❌ | – | – | – | 2.02× | – | – | – | – | – |
| KDC | other | 1 | 1.49× | ❌ | – | – | – | 3.50× | – | – | – | – | – |
| KDC | uncompressed | 2 | 53.8× | 9.31× | – | – | – | 11.9× | – | – | – | – | – |
| MEF | uncompressed | 1 | 19.6× | 3.89× | – | – | – | 10.1× | – | – | – | – | – |
| MOS | leaf | 2 | 23.0× | ❌ | – | – | – | 13.5× | – | – | – | – | – |
| MOS | uncompressed | 1 | 5.06× | 4.02× | – | – | – | 7.58× | – | – | – | – | – |
| MRW | minolta-packed | 2 | 76.7× | 13.3× | – | – | – | 12.9× | – | – | – | – | – |
| MRW | uncompressed | 2 | 18.1× | 16.9× | – | – | – | 11.4× | – | – | – | – | – |
| NEF | nikon-huffman | 9 | 15.5× | 5.07× | 6.34× | 15.8× | 17.9× | 12.4× | 21.2× | 9.45× | 13.3× | 11.9× | – |
| NEF | uncompressed | 2 | 5.71× | 2.33× | – | – | – | 11.3× | – | – | – | – | – |
| NRW | uncompressed | 2 | 3.99× | 1.59× | – | – | – | 10.9× | – | – | – | – | – |
| ORF | olympus | 5 | 5.56× | 2.57× | 3.54× | 5.54× | 2.50× | 7.05× | 11.1× | 5.64× | 7.16× | 4.18× | – |
| ORF | uncompressed | 2 | 12.9× | 2.86× | – | – | – | 11.5× | – | – | – | – | – |
| PEF | pentax-huffman | 4 | 13.6× | 3.74× | 5.62× | 15.5× | 16.7× | 11.9× | 21.3× | 9.59× | 13.9× | 12.2× | – |
| PEF | uncompressed | 2 | 34.5× | 13.7× | – | – | – | 12.0× | – | – | – | – | – |
| RAF | fuji-compressed | 7 | 2.98× | 2.25× | 2.42× | 2.91× | ❌ | 5.50× | 12.3× | 6.04× | 5.68× | ❌ | – |
| RAF | uncompressed | 3 | 2.38× | 0.94× | 1.84× | 1.97× | 13.3× | 7.76× | 15.3× | 7.99× | 7.22× | 8.09× | – |
| RAW | uncompressed | 3 | 9.44× | 4.86× | – | – | – | 10.7× | – | – | – | – | – |
| RW2 | panasonic-v4 | 3 | 12.1× | 1.57× | 1.68× | 15.6× | 21.4× | 9.89× | 12.2× | 4.60× | 6.74× | 6.54× | – |
| RW2 | panasonic-v5 | 2 | 10.9× | 0.97× | – | – | – | 11.0× | – | – | – | – | – |
| RW2 | panasonic-v6 | 2 | 9.73× | 2.57× | 2.10× | 11.0× | 40.7× | 12.5× | 26.6× | 9.50× | 18.7× | 15.1× | – |
| RW2 | panasonic-v7 | 3 | 3.61× | 1.89× | 3.06× | 3.13× | 56.4× | 11.6× | 22.5× | 8.29× | 10.3× | 11.3× | – |
| RW2 | panasonic-v8 | 5 | 4.22× | 4.40× | 4.26× | 4.61× | 9.34× | 9.11× | 18.8× | 8.50× | 10.1× | 9.42× | – |
| RW2 | uncompressed | 2 | 8.69× | 5.19× | – | – | – | 10.0× | – | – | – | – | – |
| SRW | samsung-v0 | 2 | 13.8× | 6.52× | – | – | – | 12.1× | – | – | – | – | – |
| SRW | samsung-v1 | 2 | 4.32× | 2.02× | – | – | – | 6.37× | – | – | – | – | – |
| SRW | samsung-v2 | 2 | 1.97× | 1.44× | – | – | – | 3.78× | – | – | – | – | – |
| SRW | uncompressed | 2 | 20.8× | 4.38× | – | – | – | 12.9× | – | – | – | – | – |
| X3F | sigma-x3f | 2 | 11.2× | ❌ | – | – | – | 10.6× | – | – | – | – | – |
| other | kodak-ycbcr | 1 | 6.23× | ❌ | – | – | – | 6.39× | – | – | – | – | – |
| other | uncompressed | 2 | 6.85× | 4.24× | – | – | – | 10.5× | – | – | – | – | – |

## All files (FlashRaw, LibRaw, RawSpeed)

| File | Camera | Codec | MP | threads | Decode FlashRaw | LibRaw | RawSpeed | RGB FlashRaw | LibRaw | Peak RSS MB (decode) FR / LR / RS |
|---|---|---|---:|---|---:|---:|---:|---:|---:|---|
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | all | **55.5** | 58.4 | ❌ | **91.6** | 483.3 | 248 / 134 / ❌ |
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | 1 | **182.2** | 232.0 | ❌ | **412.5** | 713.9 | 224 / 133 / ❌ |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | all | 69.7 | **68.7** | ❌ | **103.9** | 498.4 | 279 / 122 / ❌ |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | 1 | **216.6** | 253.8 | ❌ | **453.7** | 748.7 | 209 / 120 / ❌ |
| RAW.CR3 | Canon EOS R5 Mark II | CR3 · canon-crx | 47.9 | all | **56.2** | 61.9 | ❌ | **92.1** | 499.9 | 358 / 149 / ❌ |
| RAW.CR3 | Canon EOS R5 Mark II | CR3 · canon-crx | 47.9 | 1 | **179.9** | 232.1 | ❌ | **411.8** | 722.9 | 242 / 149 / ❌ |
| canon-r6m2-img_0346-raw-normal-full.cr3 | Canon EOS R6 Mark II | CR3 · canon-crx | 25.5 | all | **32.4** | 33.3 | ❌ | **54.8** | 278.0 | 195 / 83 / ❌ |
| canon-r6m2-img_0346-raw-normal-full.cr3 | Canon EOS R6 Mark II | CR3 · canon-crx | 25.5 | 1 | **102.5** | 131.7 | ❌ | **229.8** | 391.6 | 133 / 82 / ❌ |
| IMG_4241.CR3 | Canon EOS R6 Mark III | CR3 · canon-crx | 34.0 | all | **42.7** | 47.1 | ❌ | **71.6** | 359.4 | 203 / 104 / ❌ |
| IMG_4241.CR3 | Canon EOS R6 Mark III | CR3 · canon-crx | 34.0 | 1 | **132.5** | 168.9 | ❌ | **299.7** | 508.9 | 170 / 104 / ❌ |
| 443A0157.CR3 | Canon EOS R7 | CR3 · canon-crx | 33.7 | all | **40.3** | 42.9 | ❌ | **67.5** | 352.7 | 249 / 101 / ❌ |
| 443A0157.CR3 | Canon EOS R7 | CR3 · canon-crx | 33.7 | 1 | **129.4** | 166.2 | ❌ | **297.4** | 506.7 | 167 / 101 / ❌ |
| RAW_FULL_FRAME.CR3 | Canon EOS R8 | CR3 · canon-crx | 25.5 | all | **32.9** | 36.9 | ❌ | **53.2** | 271.6 | 182 / 82 / ❌ |
| RAW_FULL_FRAME.CR3 | Canon EOS R8 | CR3 · canon-crx | 25.5 | 1 | **103.7** | 130.6 | ❌ | **229.7** | 391.1 | 132 / 82 / ❌ |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | Canon EOS R50 | CR3 · canon-crx | 25.1 | all | **30.5** | 36.3 | ❌ | **51.9** | 268.5 | 195 / 84 / ❌ |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | Canon EOS R50 | CR3 · canon-crx | 25.1 | 1 | **97.0** | 123.0 | ❌ | **220.2** | 375.5 | 134 / 84 / ❌ |
| Canon-90D-RAW-ISO-100.CR3 | Canon EOS 90D | CR3 · canon-crx | 33.7 | all | **35.8** | 45.2 | ❌ | **57.9** | 358.7 | 228 / 104 / ❌ |
| Canon-90D-RAW-ISO-100.CR3 | Canon EOS 90D | CR3 · canon-crx | 33.7 | 1 | **127.4** | 161.7 | ❌ | **295.8** | 506.5 | 170 / 104 / ❌ |
| B13A0734.CR2 | Canon EOS 5D Mark IV | CR2 · lossless-jpeg | 31.3 | all | **21.5** | 348.1 | 69.1 | **47.2** | 634.2 | 275 / 97 / 100 |
| B13A0734.CR2 | Canon EOS 5D Mark IV | CR2 · lossless-jpeg | 31.3 | 1 | **63.0** | 348.0 | 69.2 | **219.5** | 669.3 | 129 / 97 / 100 |
| 6DII-RAW.CR2 | Canon EOS 6D Mark II | CR2 · lossless-jpeg | 27.0 | all | **16.4** | 270.6 | 83.4 | **39.0** | 527.0 | 213 / 93 / 96 |
| 6DII-RAW.CR2 | Canon EOS 6D Mark II | CR2 · lossless-jpeg | 27.0 | 1 | **74.0** | 267.7 | 83.1 | **213.2** | 555.5 | 126 / 93 / 96 |
| IMG_0662.CR2 | Canon EOS Rebel T7 | CR2 · lossless-jpeg | 24.7 | all | **20.9** | 311.2 | 88.2 | **45.5** | 558.5 | 245 / 90 / 93 |
| IMG_0662.CR2 | Canon EOS Rebel T7 | CR2 · lossless-jpeg | 24.7 | 1 | **80.4** | 317.6 | 89.0 | **208.7** | 579.6 | 122 / 90 / 93 |
| Nikon_Z8_raw_14_bit_lossless_compression.NEF | Nikon Z 8 | NEF · nikon-huffman | 45.7 | all | **28.4** | 424.4 | 177.6 | **67.7** | 858.9 | 293 / 151 / 154 |
| Nikon_Z8_raw_14_bit_lossless_compression.NEF | Nikon Z 8 | NEF · nikon-huffman | 45.7 | 1 | **130.7** | 427.6 | 178.3 | **371.1** | 910.1 | 153 / 151 / 154 |
| Nikon_-_NIKON_Z_9_-_14bit_compressed_(Lossless).NEF | Nikon Z 9 | NEF · nikon-huffman | 45.7 | all | **26.5** | 434.8 | 136.6 | **71.4** | 858.0 | 279 / 148 / 151 |
| Nikon_-_NIKON_Z_9_-_14bit_compressed_(Lossless).NEF | Nikon Z 9 | NEF · nikon-huffman | 45.7 | 1 | **101.2** | 416.1 | 135.3 | **347.5** | 911.1 | 149 / 147 / 151 |
| Nikon_Z6__3_Lossless_compressed_FX.NEF | Nikon Z6_3 | NEF · nikon-huffman | 24.5 | all | **15.9** | 229.6 | 86.9 | **42.1** | 463.6 | 164 / 84 / 86 |
| Nikon_Z6__3_Lossless_compressed_FX.NEF | Nikon Z6_3 | NEF · nikon-huffman | 24.5 | 1 | **69.1** | 231.2 | 85.4 | **196.3** | 488.0 | 86 / 83 / 87 |
| DSC_0040.NEF | Nikon Z f | NEF · nikon-huffman | 24.5 | all | **15.4** | 218.8 | 68.5 | **35.9** | 452.8 | 146 / 82 / 85 |
| DSC_0040.NEF | Nikon Z f | NEF · nikon-huffman | 24.5 | 1 | **51.3** | 218.3 | 68.7 | **177.8** | 477.3 | 83 / 82 / 85 |
| Z72_0027.NEF | Nikon Z 7_2 | NEF · nikon-huffman | 45.7 | all | **24.2** | 402.6 | 117.4 | **65.2** | 836.1 | 257 / 144 / 147 |
| Z72_0027.NEF | Nikon Z 7_2 | NEF · nikon-huffman | 45.7 | 1 | **91.1** | 402.6 | 117.1 | **331.0** | 894.9 | 146 / 144 / 147 |
| DSC_0450.NEF | Nikon Z 50 | NEF · nikon-huffman | 20.9 | all | **13.1** | 201.0 | 72.8 | **31.4** | 404.7 | 131 / 75 / 78 |
| DSC_0450.NEF | Nikon Z 50 | NEF · nikon-huffman | 20.9 | 1 | **52.6** | 202.1 | 72.9 | **159.7** | 430.0 | 77 / 75 / 78 |
| Nikon-D850-14bit-lossless-compressed.NEF | Nikon D850 | NEF · nikon-huffman | 45.7 | all | **24.9** | 478.6 | 118.8 | **66.3** | 831.8 | 254 / 145 / 148 |
| Nikon-D850-14bit-lossless-compressed.NEF | Nikon D850 | NEF · nikon-huffman | 45.7 | 1 | **92.8** | 396.9 | 118.7 | **331.6** | 877.6 | 147 / 145 / 148 |
| lossless_compressed_14_bit.NEF | Nikon D750 | NEF · nikon-huffman | 24.3 | all | **16.4** | 204.7 | 58.2 | **37.0** | 448.2 | 136 / 77 / 80 |
| lossless_compressed_14_bit.NEF | Nikon D750 | NEF · nikon-huffman | 24.3 | 1 | **48.6** | 203.7 | 57.8 | **174.2** | 460.4 | 79 / 77 / 80 |
| ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW | Sony ILCE-7M4 | ARW · sony-lossless | 36.7 | all | **12.7** | 321.8 | 15.3 | **39.3** | 627.4 | 131 / 122 / 126 |
| ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW | Sony ILCE-7M4 | ARW · sony-lossless | 36.7 | 1 | **92.3** | 316.2 | 100.0 | **263.1** | 663.1 | 125 / 122 / 125 |
| ILCE-7M4_DSC06677_FullFrame-Raw-Compressed.ARW | Sony ILCE-7M4 | ARW · sony-arw2 | 33.0 | all | **6.02** | 80.4 | 10.5 | **32.5** | 397.6 | 109 / 107 / 111 |
| ILCE-7M4_DSC06677_FullFrame-Raw-Compressed.ARW | Sony ILCE-7M4 | ARW · sony-arw2 | 33.0 | 1 | **35.8** | 80.9 | 82.1 | **208.0** | 432.8 | 109 / 107 / 110 |
| 7RM5-LosslessCompressedLarge.ARW | Sony ILCE-7RM5 | ARW · sony-lossless | 64.7 | all | **21.9** | 575.4 | 30.7 | **69.9** | 1155.3 | 221 / 211 / 215 |
| 7RM5-LosslessCompressedLarge.ARW | Sony ILCE-7RM5 | ARW · sony-lossless | 64.7 | 1 | **166.6** | 575.1 | 184.4 | **479.4** | 1223.5 | 213 / 211 / 214 |
| A1_full_compressed.ARW | Sony ILCE-1 | ARW · sony-arw2 | 50.2 | all | **8.43** | 119.1 | 16.7 | **48.8** | 597.4 | 164 / 161 / 165 |
| A1_full_compressed.ARW | Sony ILCE-1 | ARW · sony-arw2 | 50.2 | 1 | **55.8** | 123.0 | 122.2 | **318.8** | 652.6 | 163 / 161 / 164 |
| _DSC0009.ARW | Sony ILCE-7M3 | ARW · sony-arw2 | 24.3 | all | **4.04** | 62.4 | 8.25 | **24.0** | 292.6 | 79 / 76 / 80 |
| _DSC0009.ARW | Sony ILCE-7M3 | ARW · sony-arw2 | 24.3 | 1 | **26.2** | 59.4 | 60.2 | **152.1** | 316.7 | 78 / 76 / 79 |
| DSC02435.ARW | Sony ILCE-7CM2 | ARW · sony-arw2 | 33.0 | all | **5.86** | 78.5 | 11.0 | **32.5** | 392.0 | 109 / 107 / 111 |
| DSC02435.ARW | Sony ILCE-7CM2 | ARW · sony-arw2 | 33.0 | 1 | **36.0** | 78.8 | 80.4 | **208.1** | 426.6 | 109 / 106 / 110 |
| DSC00111.ARW | Sony ILCE-9M3 | ARW · sony-arw2 | 24.3 | all | **4.53** | 60.3 | 8.86 | **24.1** | 294.8 | 82 / 80 / 84 |
| DSC00111.ARW | Sony ILCE-9M3 | ARW · sony-arw2 | 24.3 | 1 | **26.2** | 61.0 | 61.2 | **152.1** | 316.6 | 81 / 79 / 82 |
| DSC00001.ARW | Sony ILCE-6700 | ARW · sony-lossless | 30.7 | all | **12.3** | 277.3 | 23.3 | **33.4** | 523.3 | 115 / 106 / 110 |
| DSC00001.ARW | Sony ILCE-6700 | ARW · sony-lossless | 30.7 | 1 | **91.0** | 276.2 | 100.7 | **224.7** | 563.8 | 108 / 105 / 109 |
| DSC00087.ARW | Sony ILCE-6400 | ARW · sony-arw2 | 24.3 | all | **4.19** | 52.8 | 8.29 | **24.3** | 285.6 | 77 / 75 / 79 |
| DSC00087.ARW | Sony ILCE-6400 | ARW · sony-arw2 | 24.3 | 1 | **26.7** | 53.4 | 55.1 | **152.3** | 309.2 | 77 / 75 / 78 |
| DSC00002.ARW | Sony ZV-E10 | ARW · sony-arw2 | 24.3 | all | **4.24** | 54.5 | 7.99 | **21.2** | 287.2 | 78 / 76 / 79 |
| DSC00002.ARW | Sony ZV-E10 | ARW · sony-arw2 | 24.3 | 1 | **26.3** | 54.3 | 56.1 | **132.5** | 318.0 | 77 / 75 / 79 |
| DSCF0021.RAF | Fujifilm X-T5 | RAF · fuji-compressed | 40.9 | all | **47.0** | 157.9 | 121.0 | **109.5** | 555.9 | 128 / 125 / 128 |
| DSCF0021.RAF | Fujifilm X-T5 | RAF · fuji-compressed | 40.9 | 1 | **300.0** | 1118.9 | 829.4 | **665.1** | 1604.8 | 127 / 124 / 127 |
| Fuji-X-H2-Compressed-Raw.RAF | Fujifilm X-H2 | RAF · fuji-compressed | 40.9 | all | **62.0** | 169.7 | ❌ | **138.1** | 583.1 | 116 / 112 / ❌ |
| Fuji-X-H2-Compressed-Raw.RAF | Fujifilm X-H2 | RAF · fuji-compressed | 40.9 | 1 | **403.7** | 1251.6 | ❌ | **821.1** | 1718.6 | 113 / 111 / ❌ |
| DSCF2526.RAF | Fujifilm X-S20 | RAF · fuji-compressed | 26.5 | all | **37.6** | 138.9 | 102.8 | **72.4** | 372.7 | 91 / 89 / 92 |
| DSCF2526.RAF | Fujifilm X-S20 | RAF · fuji-compressed | 26.5 | 1 | **193.6** | 769.5 | 544.9 | **438.5** | 1075.3 | 90 / 88 / 91 |
| DSCF0075.RAF | Fujifilm X100VI | RAF · fuji-compressed | 40.9 | all | **46.8** | 157.8 | 117.7 | **123.9** | 572.8 | 134 / 132 / 134 |
| DSCF0075.RAF | Fujifilm X100VI | RAF · fuji-compressed | 40.9 | 1 | **297.1** | 1126.3 | 830.6 | **720.6** | 2015.8 | 133 / 130 / 133 |
| DSCF0266.RAF | Fujifilm X-T4 | RAF · uncompressed | 26.7 | all | 8.67 | 20.8 | **8.49** | **40.5** | 268.9 | 114 / 111 / 114 |
| DSCF0266.RAF | Fujifilm X-T4 | RAF · uncompressed | 26.7 | 1 | **4.27** | 16.5 | 4.56 | **245.3** | 313.4 | 113 / 111 / 114 |
| _DSF0267.RAF | Fujifilm GFX100 II | RAF · fuji-compressed | 103.4 | all | **171.7** | 297.6 | 253.9 | **169.4** | 1371.6 | 312 / 310 / 312 |
| _DSF0267.RAF | Fujifilm GFX100 II | RAF · fuji-compressed | 103.4 | 1 | **907.6** | 3570.0 | 2173.1 | **1406.4** | 3891.1 | 311 / 309 / 312 |
| P1126458_mechanical.RW2 | Panasonic DC-S5M2 | RW2 · panasonic-v8 | 24.1 | all | **17.9** | 89.7 | 89.0 | **39.4** | 350.6 | 223 / 89 / 90 |
| P1126458_mechanical.RW2 | Panasonic DC-S5M2 | RW2 · panasonic-v8 | 24.1 | 1 | **114.1** | 163.6 | 172.5 | **239.1** | 448.1 | 89 / 87 / 90 |
| 4-3.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 25.2 | all | **20.6** | 86.9 | 94.2 | **41.7** | 360.4 | 217 / 98 / 99 |
| 4-3.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 25.2 | 1 | **119.4** | 170.1 | 187.5 | **250.1** | 467.4 | 98 / 97 / 99 |
| P1000019.RW2 | Panasonic DC-G9M2 | RW2 · panasonic-v8 | 25.1 | all | **21.5** | 96.5 | 96.2 | **45.8** | 517.4 | 221 / 95 / 96 |
| P1000019.RW2 | Panasonic DC-G9M2 | RW2 · panasonic-v8 | 25.1 | 1 | **131.3** | 179.2 | 204.7 | **351.8** | 674.7 | 95 / 93 / 96 |
| P1047510.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 24.3 | all | **3.08** | 13.8 | 8.56 | **30.4** | 315.1 | 96 / 93 / 97 |
| P1047510.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 24.3 | 1 | **5.55** | 10.1 | 14.6 | **130.9** | 298.3 | 95 / 93 / 97 |
| P1000077.RW2 | Panasonic DC-S1R | RW2 · panasonic-v6 | 47.6 | all | **8.47** | 85.9 | 15.8 | **46.3** | 594.1 | 165 / 163 / 167 |
| P1000077.RW2 | Panasonic DC-S1R | RW2 · panasonic-v6 | 47.6 | 1 | **33.2** | 85.3 | 87.4 | **280.7** | 642.7 | 164 / 163 / 166 |
| _T012010.RW2 | Panasonic DC-GH5 | RW2 · panasonic-v4 | 20.6 | all | **6.17** | 105.3 | 16.1 | **46.9** | 308.9 | 70 / 68 / 72 |
| _T012010.RW2 | Panasonic DC-GH5 | RW2 · panasonic-v4 | 20.6 | 1 | **35.6** | 56.0 | 46.0 | **141.2** | 297.6 | 69 / 67 / 71 |
| 20mp-12bit.orf | OM System OM-1 Mark II | ORF · olympus | 20.4 | all | **53.0** | 342.1 | 142.3 | **71.1** | 539.3 | 70 / 66 / 69 |
| 20mp-12bit.orf | OM System OM-1 Mark II | ORF · olympus | 20.4 | 1 | **77.5** | 314.5 | 142.5 | **182.8** | 531.3 | 68 / 66 / 69 |
| C1291096.ORF | OM System OM-5 | ORF · olympus | 20.5 | all | **52.1** | 284.3 | 138.1 | **68.8** | 477.5 | 67 / 63 / 66 |
| C1291096.ORF | OM System OM-5 | ORF · olympus | 20.5 | 1 | **76.3** | 262.7 | 138.0 | **181.1** | 480.1 | 65 / 63 / 66 |
| _3160531.ORF | Olympus E-M1MarkIII | ORF · olympus | 20.5 | all | **51.9** | 259.4 | 133.5 | **68.8** | 454.1 | 65 / 62 / 65 |
| _3160531.ORF | Olympus E-M1MarkIII | ORF · olympus | 20.5 | 1 | **76.3** | 243.7 | 133.8 | **181.4** | 465.8 | 64 / 62 / 64 |
| _IMG0115.PEF | Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | all | **16.4** | 231.3 | 56.4 | **39.8** | 473.9 | 161 / 87 / 90 |
| _IMG0115.PEF | Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | 1 | **52.8** | 222.7 | 56.1 | **203.8** | 496.5 | 89 / 87 / 90 |
| _K1I9780.PEF | Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | all | **22.6** | 313.4 | 91.1 | **58.0** | 656.0 | 237 / 121 / 123 |
| _K1I9780.PEF | Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | 1 | **84.9** | 488.0 | 92.0 | **275.9** | 710.4 | 123 / 120 / 124 |
| R0000357.DNG | Ricoh GR III | DNG · lossless-jpeg | 24.8 | all | **18.6** | 266.5 | 68.8 | **39.6** | 500.8 | 233 / 85 / 87 |
| R0000357.DNG | Ricoh GR III | DNG · lossless-jpeg | 24.8 | 1 | **56.7** | 263.8 | 69.1 | **183.3** | 521.9 | 115 / 84 / 88 |
| L1000750.DNG | Leica Q2 | DNG · uncompressed | 47.4 | all | **8.19** | 192.6 | 32.3 | **45.3** | 636.6 | 182 / 179 / 182 |
| L1000750.DNG | Leica Q2 | DNG · uncompressed | 47.4 | 1 | **15.7** | 194.6 | 32.3 | **264.1** | 690.9 | 181 / 179 / 183 |
| L1020010.DNG | Leica SL2 | DNG · uncompressed | 47.4 | all | **8.33** | 192.7 | 32.4 | **45.3** | 641.5 | 183 / 181 / 184 |
| L1020010.DNG | Leica SL2 | DNG · uncompressed | 47.4 | 1 | **15.6** | 198.1 | 34.4 | **263.7** | 707.9 | 183 / 181 / 184 |
| M10R0324.DNG | Leica M10-R | DNG · lossless-jpeg | 41.0 | all | **35.6** | 437.4 | 151.5 | **69.4** | 820.0 | 288 / 136 / 139 |
| M10R0324.DNG | Leica M10-R | DNG · lossless-jpeg | 41.0 | 1 | **99.7** | 438.1 | 151.4 | **315.5** | 877.9 | 186 / 136 / 139 |
| Job_0079.fff | Hasselblad X2D 100C | FFF · hasselblad | 105.3 | all | **75.9** | 867.7 | 489.3 | **160.5** | 1822.1 | 655 / 360 / 362 |
| Job_0079.fff | Hasselblad X2D 100C | FFF · hasselblad | 105.3 | 1 | **399.5** | 849.2 | 489.9 | **928.3** | 1928.9 | 364 / 359 / 363 |
| IQ4_150MP_L_14.iiq | Phase One IQ4 150MP | IIQ · phaseone | 154.0 | all | **55.4** | 663.7 | ❌ | **164.8** | 2438.8 | 462 / 460 / ❌ |
| IQ4_150MP_L_14.iiq | Phase One IQ4 150MP | IIQ · phaseone | 154.0 | 1 | **410.5** | 651.9 | ❌ | **1195.7** | 2600.3 | 462 / 459 / ❌ |
| SDIM0915.DNG | Sigma fp L | DNG · lossless-jpeg | 60.5 | all | **16.6** | 621.0 | 23.4 | **71.9** | 1193.8 | 214 / 209 / 214 |
| SDIM0915.DNG | Sigma fp L | DNG · lossless-jpeg | 60.5 | 1 | **135.4** | 625.3 | 163.0 | **496.2** | 1258.6 | 211 / 209 / 212 |
| IMG_1361.DNG | Apple iPhone 12 Pro | DNG · lossless-jpeg | 12.2 | all | **13.4** | 317.7 | ❌ | **30.4** | 394.2 | 115 / 126 / ❌ |
| IMG_1361.DNG | Apple iPhone 12 Pro | DNG · lossless-jpeg | 12.2 | 1 | **89.8** | 318.2 | ❌ | **172.8** | 385.1 | 106 / 126 / ❌ |
| PXL_20240415_103400204.RAW-02.ORIGINAL.dng | Google Pixel 8 Pro | DNG · lossless-jpeg | 50.1 | all | **14.4** | 563.9 | 21.1 | **60.8** | 1093.6 | 153 / 144 / 149 |
| PXL_20240415_103400204.RAW-02.ORIGINAL.dng | Google Pixel 8 Pro | DNG · lossless-jpeg | 50.1 | 1 | **107.2** | 521.7 | 131.4 | **402.3** | 1146.6 | 150 / 144 / 148 |
| PXL_20220910_093206982.dng | Google Pixel 6 Pro | DNG · lossless-jpeg | 12.5 | all | **3.04** | 129.2 | 4.04 | **15.6** | 253.9 | 45 / 41 / 45 |
| PXL_20220910_093206982.dng | Google Pixel 6 Pro | DNG · lossless-jpeg | 12.5 | 1 | **27.4** | 130.5 | 35.1 | **102.3** | 261.3 | 43 / 40 / 44 |
| 20231214_130645.dng | Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 49.9 | all | **118.7** | 1263.2 | 382.6 | **217.5** | 1497.2 | 657 / 463 / 371 |
| 20231214_130645.dng | Samsung Galaxy S23 Ultra | DNG · lossless-jpeg | 49.9 | 1 | **344.0** | 1265.2 | 500.1 | **1101.2** | 1496.2 | 438 / 463 / 370 |
| 20250305_173647.dng | Samsung SM-S901E | DNG · jpeg-xl | 12.0 | all | **317.3** | ❌ | ❌ | **362.1** | ❌ | 433 / ❌ / ❌ |
| 20250305_173647.dng | Samsung SM-S901E | DNG · jpeg-xl | 12.0 | 1 | **491.3** | ❌ | ❌ | **678.7** | ❌ | 432 / ❌ / ❌ |
| DJI_0220.DNG | DJI FC6310 | DNG · uncompressed | 20.0 | all | **2.48** | 10.4 | 3.16 | **28.9** | 202.3 | 84 / 82 / 85 |
| DJI_0220.DNG | DJI FC6310 | DNG · uncompressed | 20.0 | 1 | **2.76** | 10.4 | 3.21 | **168.5** | 221.6 | 84 / 81 / 85 |
| GOPR0123.GPR | GoPro HERO11 Black | DNG · gopro-vc5 | 27.1 | all | **41.4** | ❌ | 45.6 | **50.0** | ❌ | 221 / ❌ / 387 |
| GOPR0123.GPR | GoPro HERO11 Black | DNG · gopro-vc5 | 27.1 | 1 | **131.1** | ❌ | 201.9 | **291.3** | ❌ | 180 / ❌ / 221 |
| GOPR0018.GPR | GoPro HERO10 Black | DNG · gopro-vc5 | 23.3 | all | **21.1** | ❌ | 24.1 | **42.5** | ❌ | 169 / ❌ / 351 |
| GOPR0018.GPR | GoPro HERO10 Black | DNG · gopro-vc5 | 23.3 | 1 | **109.3** | ❌ | 186.3 | **246.8** | ❌ | 152 / ❌ / 187 |
| A9391120.3FR | Hasselblad H3D | 3FR · hasselblad | 39.6 | all | **31.7** | 318.6 | 181.6 | **76.5** | 893.7 | 237 / 132 / 135 |
| A9391120.3FR | Hasselblad H3D | 3FR · hasselblad | 39.6 | 1 | **141.0** | 305.0 | 182.3 | **348.4** | 720.8 | 134 / 132 / 135 |
| A0000902.3FR | Hasselblad H3D | 3FR · hasselblad | 60.9 | all | **43.0** | 503.2 | 290.8 | **101.5** | 1131.4 | 343 / 199 / 202 |
| A0000902.3FR | Hasselblad H3D | 3FR · hasselblad | 60.9 | 1 | **243.4** | 497.7 | 387.0 | **791.4** | 1217.6 | 201 / 199 / 202 |
| B0003499.3FR | Hasselblad CFV 100C | 3FR · uncompressed | 105.3 | all | 18.3 | 21.5 | **17.9** | **101.4** | 1430.3 | 414 / 408 / 411 |
| B0003499.3FR | Hasselblad CFV 100C | 3FR · uncompressed | 105.3 | 1 | 20.5 | 22.8 | **18.1** | **549.5** | 1098.9 | 413 / 408 / 411 |
| B0000080.3FR | Hasselblad X2D 100C | 3FR · uncompressed | 105.3 | all | **14.4** | 20.4 | 17.7 | **95.2** | 994.2 | 414 / 410 / 412 |
| B0000080.3FR | Hasselblad X2D 100C | 3FR · uncompressed | 105.3 | 1 | 20.4 | 20.7 | **17.8** | **553.5** | 1104.2 | 414 / 409 / 412 |
| full_compressed.ARW | Sony ILCE-7M5 | ARW · other | 33.0 | all | **44.0** | 312.8 | ❌ | **68.4** | 662.5 | 683 / 395 / ❌ |
| full_compressed.ARW | Sony ILCE-7M5 | ARW · other | 33.0 | 1 | **221.2** | 314.9 | ❌ | **391.1** | 713.7 | 298 / 395 / ❌ |
| full_compressed_HQ.ARW | Sony ILCE-7M5 | ARW · other | 33.0 | all | **42.8** | 314.2 | ❌ | **70.1** | 633.2 | 683 / 395 / ❌ |
| full_compressed_HQ.ARW | Sony ILCE-7M5 | ARW · other | 33.0 | 1 | **219.2** | 314.4 | ❌ | **390.3** | 664.5 | 298 / 394 / ❌ |
| DSC05353.ARW | Sony DSLR-A380 | ARW · sony-arw1 | 14.1 | all | **6.50** | 151.5 | 132.9 | **24.7** | 292.3 | 91 / 51 / 54 |
| DSC05353.ARW | Sony DSLR-A380 | ARW · sony-arw1 | 14.1 | 1 | **36.5** | 148.6 | 133.0 | **110.9** | 303.4 | 53 / 51 / 54 |
| RAW_SONY_A390.ARW | Sony DSLR-A390 | ARW · sony-arw1 | 14.1 | all | **6.44** | 135.5 | 122.6 | **18.9** | 272.4 | 88 / 46 / 48 |
| RAW_SONY_A390.ARW | Sony DSLR-A390 | ARW · sony-arw1 | 14.1 | 1 | **35.5** | 137.9 | 123.1 | **109.8** | 287.4 | 47 / 45 / 49 |
| SONY ILCE-7M2 tungsten.arw | Sony ILCE-7M2 | ARW · sony-arw2 | 24.3 | all | **4.79** | 53.4 | 8.08 | **24.1** | 284.7 | 78 / 76 / 79 |
| SONY ILCE-7M2 tungsten.arw | Sony ILCE-7M2 | ARW · sony-arw2 | 24.3 | 1 | **26.3** | 53.7 | 55.0 | **152.2** | 310.5 | 77 / 75 / 78 |
| 7RM5-LossyCompressed.ARW | Sony ILCE-7RM5 | ARW · sony-arw2 | 61.2 | all | **10.2** | 145.0 | 19.6 | **58.0** | 728.6 | 193 / 191 / 195 |
| 7RM5-LossyCompressed.ARW | Sony ILCE-7RM5 | ARW · sony-arw2 | 61.2 | 1 | **65.7** | 145.3 | 149.4 | **383.4** | 805.4 | 193 / 191 / 194 |
| DSC00800.ARW | Sony ILCE-7CR | ARW · sony-lossless | 18.4 | all | **35.8** | 155.1 | ❌ | **48.9** | 226.6 | 314 / 184 / ❌ |
| DSC00800.ARW | Sony ILCE-7CR | ARW · sony-lossless | 18.4 | 1 | **139.7** | 154.9 | ❌ | **223.6** | 225.8 | 290 / 183 / ❌ |
| DSC03792.ARW | Sony ILCE-7RM3 | ARW · uncompressed | 42.6 | all | **5.66** | 25.6 | 7.17 | **39.7** | 429.0 | 170 / 168 / 171 |
| DSC03792.ARW | Sony ILCE-7RM3 | ARW · uncompressed | 42.6 | 1 | **6.49** | 25.8 | 7.04 | **229.2** | 473.1 | 170 / 168 / 171 |
| Indoor_Multi-20190712-094102.ARQ | Sony ILCE-7RM3 | ARW · uncompressed | 42.6 | all | 22.1 | 61.5 | **7.03** | **64.4** | 267.9 | 657 / 656 / 415 |
| Indoor_Multi-20190712-094102.ARQ | Sony ILCE-7RM3 | ARW · uncompressed | 42.6 | 1 | 25.9 | 61.0 | **6.89** | 282.5 | **268.8** | 657 / 656 / 415 |
| 5G4A9395.CR2 | Canon EOS 5D Mark III | CR2 · canon-sraw | 10.5 | all | **19.0** | 211.7 | 47.2 | **27.7** | 258.7 | 177 / 100 / 114 |
| 5G4A9395.CR2 | Canon EOS 5D Mark III | CR2 · canon-sraw | 10.5 | 1 | **80.7** | 211.4 | 100.7 | **139.1** | 258.1 | 94 / 100 / 113 |
| _DSR2003.CR2 | Canon EOS 5DS R | CR2 · canon-sraw | 28.0 | all | **54.9** | 556.3 | 152.2 | **80.1** | 682.7 | 436 / 262 / 292 |
| _DSR2003.CR2 | Canon EOS 5DS R | CR2 · canon-sraw | 28.0 | 1 | **230.8** | 559.0 | 294.9 | **391.1** | 682.7 | 249 / 262 / 291 |
| B13A0729.CR2 | Canon EOS 5D Mark IV | CR2 · lossless-jpeg | 31.3 | all | **19.8** | 348.1 | 67.6 | **44.5** | 633.8 | 290 / 124 / 127 |
| B13A0729.CR2 | Canon EOS 5D Mark IV | CR2 · lossless-jpeg | 31.3 | 1 | **61.7** | 345.8 | 67.6 | **218.0** | 669.6 | 155 / 124 / 127 |
| IMG_0493.CR2 | Canon EOS Rebel T1i | CR2 · lossless-jpeg | 15.5 | all | **13.6** | 202.6 | 54.7 | **26.7** | 349.4 | 154 / 57 / 60 |
| IMG_0493.CR2 | Canon EOS Rebel T1i | CR2 · lossless-jpeg | 15.5 | 1 | **50.2** | 204.6 | 54.9 | **134.5** | 382.0 | 78 / 57 / 60 |
| Canon_EOS_R5_RAW_ISO_100_nocrop_dual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | all | 61.5 | **58.5** | ❌ | **106.8** | 526.3 | 323 / 165 / ❌ |
| Canon_EOS_R5_RAW_ISO_100_nocrop_dual.CR3 | Canon EOS R5 | CR3 · canon-crx | 46.7 | 1 | **303.9** | 350.1 | ❌ | **496.6** | 859.6 | 255 / 165 / ❌ |
| canon-r6m2-img_0355-craw-dual-full.cr3 | Canon EOS R6 Mark II | CR3 · canon-crx | 25.5 | all | **54.9** | 66.8 | ❌ | **58.9** | 331.7 | 173 / 86 / ❌ |
| canon-r6m2-img_0355-craw-dual-full.cr3 | Canon EOS R6 Mark II | CR3 · canon-crx | 25.5 | 1 | **121.5** | 144.9 | ❌ | **246.7** | 421.7 | 133 / 85 / ❌ |
| CRW_7673.CRW | Canon EOS 10D | CRW · canon-crw | 6.5 | all | **16.4** | 67.9 | 45.0 | **22.4** | 131.3 | 28 / 25 / 29 |
| CRW_7673.CRW | Canon EOS 10D | CRW · canon-crw | 6.5 | 1 | **16.4** | 68.4 | 45.2 | **49.2** | 134.0 | 27 / 25 / 28 |
| crw_2127.crw | Canon EOS 10D | CRW · canon-crw | 6.5 | all | **16.3** | 62.9 | 42.8 | **22.4** | 127.1 | 30 / 28 / 32 |
| crw_2127.crw | Canon EOS 10D | CRW · canon-crw | 6.5 | 1 | **16.4** | 63.3 | 42.8 | **49.3** | 129.4 | 31 / 28 / 32 |
| RAW_CANON_PRO70_SRGB.CRW | Canon PowerShot Pro70 | CRW · uncompressed | 1.6 | all | **0.15** | 3.02 | ❌ | **3.18** | 24.4 | 16 / 16 / ❌ |
| RAW_CANON_PRO70_SRGB.CRW | Canon PowerShot Pro70 | CRW · uncompressed | 1.6 | 1 | **0.39** | 3.00 | ❌ | **24.3** | 24.8 | 16 / 16 / ❌ |
| D4C50031.DCR | Kodak DCS Pro 14n | DCR · kodak-65000 | 13.6 | all | **4.66** | 83.5 | 79.2 | **16.5** | 224.0 | 48 / 47 / 49 |
| D4C50031.DCR | Kodak DCS Pro 14n | DCR · kodak-65000 | 13.6 | 1 | **27.8** | 83.5 | 79.1 | **99.4** | 229.0 | 48 / 46 / 49 |
| D7465857.DCR | Kodak DCS Pro 14nx | DCR · kodak-65000 | 13.6 | all | **4.39** | 79.4 | 76.9 | **16.4** | 214.5 | 47 / 45 / 48 |
| D7465857.DCR | Kodak DCS Pro 14nx | DCR · kodak-65000 | 13.6 | 1 | **27.4** | 79.5 | 76.9 | **99.4** | 223.6 | 46 / 44 / 48 |
| RAW_KODAK_DCS560C.TIF | Kodak DCS560C | DCS · lossless-jpeg | 6.1 | all | **5.43** | 56.8 | 13.2 | **7.95** | 115.4 | 58 / 22 / 26 |
| RAW_KODAK_DCS560C.TIF | Kodak DCS560C | DCS · lossless-jpeg | 6.1 | 1 | **11.7** | 56.7 | 18.0 | **43.8** | 120.7 | 30 / 22 / 25 |
| RAW_KODAK_DCS460D_FILEVERSION_3.TIF | Kodak DCS460D | DCS · uncompressed | 6.3 | all | **0.29** | 1.69 | 8.08 | **5.52** | 74.3 | 25 / 23 / 26 |
| RAW_KODAK_DCS460D_FILEVERSION_3.TIF | Kodak DCS460D | DCS · uncompressed | 6.3 | 1 | **1.11** | 1.75 | 6.91 | **33.5** | 78.6 | 25 / 23 / 26 |
| RAW_CANON_DCS1.TIF | Kodak EOS DCS 1 | DCS · uncompressed | 6.3 | all | **0.25** | 1.69 | 6.89 | **5.72** | 74.6 | 25 / 23 / 26 |
| RAW_CANON_DCS1.TIF | Kodak EOS DCS 1 | DCS · uncompressed | 6.3 | 1 | **1.11** | 1.68 | 6.89 | **33.5** | 78.3 | 25 / 23 / 26 |
| hdrmerge-bayer-fp24-w-pred-deflate.dng | Canon EOS 5D Mark III | DNG · deflate | 23.4 | all | **20.6** | 231.2 | 26.0 | **35.8** | 449.5 | 183 / 175 / 139 |
| hdrmerge-bayer-fp24-w-pred-deflate.dng | Canon EOS 5D Mark III | DNG · deflate | 23.4 | 1 | **137.6** | 230.3 | 226.9 | **245.4** | 467.5 | 175 / 175 / 131 |
| hdrmerge-bayer-fp32-w-pred-deflate.dng | Canon EOS 5D Mark III | DNG · deflate | 23.4 | all | **23.7** | 276.0 | 29.3 | **37.7** | 491.2 | 204 / 195 / 160 |
| hdrmerge-bayer-fp32-w-pred-deflate.dng | Canon EOS 5D Mark III | DNG · deflate | 23.4 | 1 | **166.6** | 276.0 | 282.5 | **274.0** | 514.1 | 196 / 195 / 152 |
| M2462362.DNG | Leica M Monochrom (Typ 246) | DNG · lossless-jpeg | 23.9 | all | **16.5** | 249.3 | 58.8 | **31.4** | 374.4 | 203 / 72 / 75 |
| M2462362.DNG | Leica M Monochrom (Typ 246) | DNG · lossless-jpeg | 23.9 | 1 | **46.0** | 249.1 | 58.9 | **143.7** | 399.2 | 94 / 72 / 75 |
| _K1I9784.DNG | Pentax K-1 Mark II | DNG · lossless-jpeg | 36.6 | all | **28.4** | 382.9 | 100.8 | **61.4** | 725.9 | 412 / 248 / 251 |
| _K1I9784.DNG | Pentax K-1 Mark II | DNG · lossless-jpeg | 36.6 | 1 | **85.7** | 378.5 | 100.9 | **275.7** | 762.8 | 292 / 248 / 251 |
| L16_00148.dng | Light L16 | DNG · uncompressed | 64.6 | all | **48.0** | 145.5 | 52.4 | **105.4** | 437.2 | 761 / 881 / 762 |
| L16_00148.dng | Light L16 | DNG · uncompressed | 64.6 | 1 | **39.1** | 147.4 | 41.4 | **410.5** | 439.6 | 760 / 881 / 761 |
| IMG_20200324_073611.dng | Xiaomi Mi 9 Lite | DNG · uncompressed | 12.0 | all | **0.80** | 5.32 | 27.7 | **19.5** | 130.6 | 53 / 51 / 55 |
| IMG_20200324_073611.dng | Xiaomi Mi 9 Lite | DNG · uncompressed | 12.0 | 1 | **1.58** | 5.37 | 18.5 | **88.0** | 140.0 | 53 / 51 / 54 |
| EPSN3575.ERF | Epson R-D1s | ERF · uncompressed | 6.2 | all | **0.92** | 14.7 | 3.27 | **6.38** | 73.7 | 28 / 27 / 30 |
| EPSN3575.ERF | Epson R-D1s | ERF · uncompressed | 6.2 | 1 | **1.89** | 14.7 | 3.10 | **33.4** | 77.9 | 28 / 26 / 30 |
| _EPS1919.ERF | Epson R-D1x | ERF · uncompressed | 6.2 | all | **0.93** | 14.7 | 3.10 | **6.28** | 74.4 | 28 / 26 / 30 |
| _EPS1919.ERF | Epson R-D1x | ERF · uncompressed | 6.2 | 1 | **1.79** | 14.7 | 3.11 | **33.4** | 79.2 | 28 / 26 / 30 |
| 20151114_CCSG_on_Hasselblad_H5D50c-0017.fff | Hasselblad H5D-50c | FFF · hasselblad | 52.8 | all | **39.4** | 434.5 | 253.3 | **83.3** | 929.4 | 327 / 183 / 186 |
| 20151114_CCSG_on_Hasselblad_H5D50c-0017.fff | Hasselblad H5D-50c | FFF · hasselblad | 52.8 | 1 | **191.4** | 422.9 | 252.5 | **462.6** | 969.5 | 184 / 182 / 186 |
| 0008_20_23.IIQ | Leaf Credo 40 | IIQ · phaseone | 40.8 | all | **13.0** | 152.2 | 286.4 | **65.3** | 654.9 | 120 / 117 / 126 |
| 0008_20_23.IIQ | Leaf Credo 40 | IIQ · phaseone | 40.8 | 1 | **102.7** | 149.8 | 360.6 | **466.2** | 702.3 | 119 / 117 / 125 |
| IQ4_150MPIIQ_L_16bit.iiq | Phase One IQ4 150MP | IIQ · phaseone | 154.0 | all | **46.7** | 637.2 | ❌ | **165.2** | 2420.4 | 503 / 500 / ❌ |
| IQ4_150MPIIQ_L_16bit.iiq | Phase One IQ4 150MP | IIQ · phaseone | 154.0 | 1 | **414.7** | 637.4 | ❌ | **1197.6** | 2592.9 | 503 / 500 / ❌ |
| RGB_86_0_4.IIQ | Phase One iXM-RS150F | IIQ · phaseone-s | 154.0 | all | **46.5** | 317.8 | ❌ | **164.5** | 1912.1 | 413 / 410 / ❌ |
| RGB_86_0_4.IIQ | Phase One iXM-RS150F | IIQ · phaseone-s | 154.0 | 1 | 348.6 | **317.3** | ❌ | **1140.5** | 2082.2 | 412 / 410 / ❌ |
| cap_22908.IIQ | Phase One iXU180 | IIQ · phaseone-s | 81.1 | all | **25.0** | 165.8 | ❌ | **128.9** | 1050.7 | 219 / 216 / ❌ |
| cap_22908.IIQ | Phase One iXU180 | IIQ · phaseone-s | 81.1 | 1 | 182.6 | **165.5** | ❌ | **907.6** | 1167.1 | 218 / 216 / ❌ |
| H25_IT8.7-2_Card.TIF | Phase One H25 | IIQ · uncompressed | 22.7 | all | **3.06** | 3.88 | ❌ | **32.5** | 351.8 | 96 / 94 / ❌ |
| H25_IT8.7-2_Card.TIF | Phase One H25 | IIQ · uncompressed | 22.7 | 1 | **3.19** | 3.79 | ❌ | **149.3** | 372.5 | 96 / 94 / ❌ |
| RAW_KODAK_DC120.KDC | Kodak DC120 | KDC · kodak-dc120 | 0.8 | all | **0.04** | 0.63 | ❌ | **6.73** | 13.6 | 16 / 16 / ❌ |
| RAW_KODAK_DC120.KDC | Kodak DC120 | KDC · kodak-dc120 | 0.8 | 1 | **0.04** | 0.63 | ❌ | 36.0 | **13.4** | 16 / 16 / ❌ |
| RAW_KODAK_DC50.KDC | Kodak DC50 | KDC · other | 0.4 | all | **1.47** | 2.19 | ❌ | **2.01** | 7.02 | 16 / 16 / ❌ |
| RAW_KODAK_DC50.KDC | Kodak DC50 | KDC · other | 0.4 | 1 | **1.46** | 2.18 | ❌ | **3.50** | 6.44 | 16 / 16 / ❌ |
| 100_1548.KDC | Kodak EasyShare Z981 | KDC · uncompressed | 13.9 | all | **0.80** | 30.8 | 5.34 | **13.5** | 163.4 | 59 / 56 / 59 |
| 100_1548.KDC | Kodak EasyShare Z981 | KDC · uncompressed | 13.9 | 1 | **1.03** | 30.8 | 5.30 | **74.2** | 177.7 | 58 / 56 / 60 |
| RAW_KODAK_EASYSHARE_Z1015-IS.KDC | Kodak Z1015 IS | KDC · uncompressed | 10.0 | all | **0.30** | 22.3 | 3.86 | **10.0** | 118.1 | 46 / 44 / 46 |
| RAW_KODAK_EASYSHARE_Z1015-IS.KDC | Kodak Z1015 IS | KDC · uncompressed | 10.0 | 1 | **0.52** | 22.4 | 3.86 | **53.6** | 126.4 | 45 / 43 / 46 |
| RAW_MAMIYA_ZD.MEF | Mamiya ZD | MEF · uncompressed | 21.5 | all | **2.52** | 49.3 | 9.78 | **30.8** | 311.9 | 84 / 81 / 84 |
| RAW_MAMIYA_ZD.MEF | Mamiya ZD | MEF · uncompressed | 21.5 | 1 | **2.77** | 49.0 | 9.55 | **143.1** | 332.9 | 83 / 81 / 84 |
| L_000859.mos | Leaf AFi-II 12 | MOS · leaf | 80.0 | all | **34.1** | 811.8 | ❌ | **97.3** | 1462.1 | 397 / 238 / ❌ |
| L_000859.mos | Leaf AFi-II 12 | MOS · leaf | 80.0 | 1 | **172.2** | 809.6 | ❌ | **594.9** | 1553.0 | 393 / 238 / ❌ |
| RAW_APTUS_75.MOS | Leaf Aptus 75 | MOS · leaf | 33.3 | all | **16.5** | 369.1 | ❌ | **60.0** | 726.5 | 115 / 109 / ❌ |
| RAW_APTUS_75.MOS | Leaf Aptus 75 | MOS · leaf | 33.3 | 1 | **94.8** | 368.8 | ❌ | **310.7** | 760.8 | 112 / 109 / ❌ |
| L_003173.mos | Leaf Aptus 22 | MOS · uncompressed | 21.4 | all | **2.82** | 14.3 | 11.3 | **31.0** | 234.7 | 90 / 87 / 91 |
| L_003173.mos | Leaf Aptus 22 | MOS · uncompressed | 21.4 | 1 | **3.08** | 14.3 | 11.2 | **142.9** | 256.1 | 89 / 87 / 91 |
| Minolta_Dynax_5D.MRW | Minolta Dynax 5D | MRW · minolta-packed | 6.1 | all | **0.18** | 13.5 | 2.33 | **5.42** | 72.1 | 28 / 26 / 28 |
| Minolta_Dynax_5D.MRW | Minolta Dynax 5D | MRW · minolta-packed | 6.1 | 1 | **0.25** | 13.4 | 2.32 | **31.5** | 76.2 | 28 / 26 / 29 |
| PICT0881.MRW | Minolta DiMAGE A2 | MRW · minolta-packed | 8.0 | all | **0.22** | 17.8 | 3.08 | **7.59** | 94.5 | 34 / 32 / 35 |
| PICT0881.MRW | Minolta DiMAGE A2 | MRW · minolta-packed | 8.0 | 1 | **0.33** | 17.9 | 3.07 | **42.2** | 100.6 | 34 / 32 / 35 |
| RAW_MINOLTA_DIMAGE7.MRW | Minolta DiMAGE 7 | MRW · uncompressed | 5.0 | all | **0.15** | 2.39 | 2.25 | **4.38** | 51.3 | 27 / 25 / 27 |
| RAW_MINOLTA_DIMAGE7.MRW | Minolta DiMAGE 7 | MRW · uncompressed | 5.0 | 1 | **0.19** | 2.40 | 2.21 | **25.7** | 54.3 | 26 / 24 / 27 |
| RAW_MINOLTA_DIMAGE_7I.MRW | Minolta DiMAGE 7i | MRW · uncompressed | 5.0 | all | **0.12** | 2.39 | 2.21 | **4.61** | 51.2 | 26 / 24 / 27 |
| RAW_MINOLTA_DIMAGE_7I.MRW | Minolta DiMAGE 7i | MRW · uncompressed | 5.0 | 1 | **0.19** | 2.38 | 2.21 | **25.5** | 53.8 | 26 / 24 / 27 |
| Nikon_Z6__3_Lossless_compressed_1_1.NEF | Nikon Z6_3 | NEF · nikon-huffman | 16.4 | all | **8.89** | 150.6 | 55.0 | **23.5** | 309.9 | 112 / 58 / 61 |
| Nikon_Z6__3_Lossless_compressed_1_1.NEF | Nikon Z6_3 | NEF · nikon-huffman | 16.4 | 1 | **45.6** | 152.3 | 54.8 | **130.5** | 323.9 | 60 / 57 / 61 |
| _DSC2256[1].NEF | Nikon D4 | NEF · uncompressed | 16.4 | all | **1.47** | 36.6 | 6.25 | **15.7** | 193.2 | 66 / 63 / 66 |
| _DSC2256[1].NEF | Nikon D4 | NEF · uncompressed | 16.4 | 1 | **1.60** | 37.7 | 6.33 | **86.8** | 208.0 | 65 / 63 / 66 |
| Nikon-D850-14bit-uncompressed.NEF | Nikon D850 | NEF · uncompressed | 45.7 | all | **5.93** | 7.78 | 7.61 | **42.0** | 439.2 | 189 / 186 / 189 |
| Nikon-D850-14bit-uncompressed.NEF | Nikon D850 | NEF · uncompressed | 45.7 | 1 | **6.52** | 7.77 | 7.40 | **247.3** | 489.4 | 188 / 186 / 189 |
| DSCN0014.NRW | Nikon Coolpix P950 | NRW · uncompressed | 16.0 | all | **1.48** | 36.4 | 5.73 | **15.7** | 189.2 | 64 / 61 / 64 |
| DSCN0014.NRW | Nikon Coolpix P950 | NRW · uncompressed | 16.0 | 1 | **3.56** | 36.4 | 5.73 | **86.8** | 205.2 | 63 / 61 / 64 |
| DSCN0417.NRW | Nikon Coolpix A1000 | NRW · uncompressed | 15.9 | all | 2.00 | **1.30** | 1.30 | **15.8** | 154.4 | 72 / 70 / 72 |
| DSCN0417.NRW | Nikon Coolpix A1000 | NRW · uncompressed | 15.9 | 1 | 1.80 | 1.32 | **1.28** | **86.3** | 171.2 | 71 / 69 / 72 |
| 80mp-14bit-tripod.orf | OM System OM-1 Mark II | ORF · olympus | 80.9 | all | **207.9** | 1413.5 | 546.0 | **271.7** | 2173.2 | 248 / 244 / 246 |
| 80mp-14bit-tripod.orf | OM System OM-1 Mark II | ORF · olympus | 80.9 | 1 | **305.8** | 1297.0 | 548.0 | **722.7** | 2155.6 | 245 / 243 / 246 |
| PA040002.ORF | Olympus E-M10 Mark IV | ORF · olympus | 20.3 | all | **51.5** | 229.0 | 121.0 | **68.1** | 426.8 | 65 / 62 / 64 |
| PA040002.ORF | Olympus E-M10 Mark IV | ORF · olympus | 20.3 | 1 | **75.3** | 214.5 | 122.3 | **180.3** | 431.8 | 63 / 61 / 64 |
| P3307182.ORF | Olympus E-330 | ORF · uncompressed | 8.0 | all | **1.47** | 19.7 | 4.11 | **8.94** | 96.1 | 36 / 33 / 37 |
| P3307182.ORF | Olympus E-330 | ORF · uncompressed | 8.0 | 1 | 5.40 | 19.6 | **4.13** | **43.5** | 102.7 | 35 / 33 / 37 |
| PenFHiRes.orf | Olympus PEN-F | ORF · uncompressed | 81.1 | all | **16.5** | 204.0 | 48.3 | **80.1** | 988.5 | 287 / 285 / 288 |
| PenFHiRes.orf | Olympus PEN-F | ORF · uncompressed | 81.1 | 1 | 59.0 | 203.8 | **48.4** | **484.2** | 1058.3 | 287 / 285 / 288 |
| _K1I9781.PEF | Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | all | **21.7** | 313.7 | 91.3 | **52.5** | 657.3 | 355 / 243 / 246 |
| _K1I9781.PEF | Pentax K-1 Mark II | PEF · pentax-huffman | 36.6 | 1 | **85.3** | 316.9 | 91.1 | **275.9** | 697.8 | 245 / 243 / 246 |
| IMGP9854.PEF | Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | all | **17.2** | 207.6 | 57.7 | **38.2** | 454.4 | 154 / 87 / 90 |
| IMGP9854.PEF | Pentax K-3 Mark III | PEF · pentax-huffman | 26.6 | 1 | **55.0** | 215.2 | 57.9 | **189.3** | 480.7 | 89 / 86 / 90 |
| IMGP1754.PEF | Pentax K100D | PEF · uncompressed | 6.2 | all | **0.20** | 13.7 | 2.37 | **5.65** | 74.0 | 30 / 28 / 31 |
| IMGP1754.PEF | Pentax K100D | PEF · uncompressed | 6.2 | 1 | **0.30** | 13.7 | 2.35 | **31.4** | 76.5 | 30 / 28 / 31 |
| pentax.PEF | Pentax *ist D | PEF · uncompressed | 6.2 | all | **0.17** | 3.03 | 2.77 | **5.65** | 62.6 | 33 / 30 / 33 |
| pentax.PEF | Pentax *ist D | PEF · uncompressed | 6.2 | 1 | **0.27** | 3.02 | 2.79 | **31.5** | 67.3 | 32 / 30 / 33 |
| Fujifilm-GFX100S-16bits-losslesscompressed-4_3.RAF | Fujifilm GFX100S | RAF · fuji-compressed | 103.4 | all | **88.3** | 264.0 | 178.6 | **172.7** | 1225.2 | 335 / 333 / 335 |
| Fujifilm-GFX100S-16bits-losslesscompressed-4_3.RAF | Fujifilm GFX100S | RAF · fuji-compressed | 103.4 | 1 | **692.8** | 2559.3 | 1712.2 | **1230.5** | 3624.8 | 334 / 332 / 335 |
| DSCF0734.RAF | Fujifilm X100V | RAF · fuji-compressed | 26.7 | all | **34.5** | 121.6 | 83.3 | **71.0** | 367.5 | 90 / 88 / 91 |
| DSCF0734.RAF | Fujifilm X100V | RAF · fuji-compressed | 26.7 | 1 | **194.4** | 757.4 | 539.1 | **437.2** | 1058.2 | 89 / 87 / 90 |
| Fujifilm-GFX100S-16bits-uncompress-4_3.RAF | Fujifilm GFX100S | RAF · uncompressed | 103.4 | all | **13.1** | 17.2 | 17.0 | **92.5** | 983.3 | 406 / 403 / 406 |
| Fujifilm-GFX100S-16bits-uncompress-4_3.RAF | Fujifilm GFX100S | RAF · uncompressed | 103.4 | 1 | **15.4** | 17.3 | 16.8 | **553.6** | 1099.1 | 405 / 403 / 406 |
| DSCF9050.RAF | Fujifilm X-E2 | RAF · uncompressed | 16.5 | all | 2.10 | 8.98 | **1.35** | **25.3** | 167.7 | 72 / 69 / 72 |
| DSCF9050.RAF | Fujifilm X-E2 | RAF · uncompressed | 16.5 | 1 | 1.89 | 8.85 | **1.34** | **151.6** | 194.6 | 71 / 69 / 72 |
| CRW_6488.CRW | Canon PowerShot SX20 IS | RAW · uncompressed | 12.4 | all | **0.98** | 27.9 | ❌ | **12.3** | 184.8 | 50 / 47 / ❌ |
| CRW_6488.CRW | Canon PowerShot SX20 IS | RAW · uncompressed | 12.4 | 1 | **1.32** | 28.0 | ❌ | **66.1** | 196.1 | 50 / 46 / ❌ |
| git2p_43.raw | GITUP GIT2P 4:3 | RAW · uncompressed | 16.1 | all | 1.77 | **1.26** | 8.62 | **15.9** | 190.6 | 69 / 67 / 70 |
| git2p_43.raw | GITUP GIT2P 4:3 | RAW · uncompressed | 16.1 | 1 | 1.63 | **1.28** | 8.63 | **86.2** | 202.2 | 69 / 66 / 70 |
| PANA2371_16x9.RW2 | Panasonic DC-G9 | RW2 · panasonic-v4 | 20.6 | all | **5.50** | 55.8 | 6.86 | **22.5** | 275.8 | 70 / 68 / 72 |
| PANA2371_16x9.RW2 | Panasonic DC-G9 | RW2 · panasonic-v4 | 20.6 | 1 | **35.3** | 55.3 | 45.5 | **140.1** | 297.3 | 69 / 67 / 71 |
| P1020077.RW2 | Panasonic DC-LX100M2 | RW2 · panasonic-v4 | 16.7 | all | **4.22** | 43.5 | 4.96 | **18.5** | 221.3 | 59 / 56 / 60 |
| P1020077.RW2 | Panasonic DC-LX100M2 | RW2 · panasonic-v4 | 16.7 | 1 | **27.8** | 43.5 | 36.3 | **112.6** | 235.7 | 58 / 55 / 59 |
| P1000475.RW2 | Panasonic DC-G9 | RW2 · panasonic-v5 | 81.7 | all | **13.4** | 93.2 | 13.6 | **77.5** | 887.2 | 289 / 286 / 291 |
| P1000475.RW2 | Panasonic DC-G9 | RW2 · panasonic-v5 | 81.7 | 1 | **25.9** | 93.2 | 45.7 | **455.7** | 950.6 | 288 / 286 / 290 |
| P1022090.RW2 | Panasonic DC-GH5S | RW2 · panasonic-v5 | 10.0 | all | 0.67 | 11.5 | **0.63** | **10.1** | 106.4 | 44 / 42 / 46 |
| P1022090.RW2 | Panasonic DC-GH5S | RW2 · panasonic-v5 | 10.0 | 1 | **3.16** | 11.5 | 5.32 | **55.2** | 115.2 | 44 / 42 / 45 |
| P1000006.RW2 | Panasonic DC-S1H | RW2 · panasonic-v6 | 24.4 | all | **4.61** | 43.0 | 16.4 | **25.3** | 307.4 | 88 / 86 / 90 |
| P1000006.RW2 | Panasonic DC-S1H | RW2 · panasonic-v6 | 24.4 | 1 | **16.4** | 43.1 | 44.4 | **142.4** | 331.7 | 88 / 86 / 89 |
| P1047513.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 24.3 | all | **3.10** | 9.83 | 4.00 | **22.8** | 271.7 | 96 / 94 / 98 |
| P1047513.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 24.3 | 1 | **4.78** | 9.82 | 12.0 | **131.0** | 298.1 | 95 / 93 / 97 |
| dc-s5_hr_96MP.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 97.0 | all | **12.3** | 40.9 | 23.4 | **87.8** | 1092.2 | 358 / 356 / 359 |
| dc-s5_hr_96MP.RW2 | Panasonic DC-S5 | RW2 · panasonic-v7 | 97.0 | 1 | **19.5** | 40.4 | 48.8 | **528.6** | 1185.4 | 357 / 356 / 359 |
| 3-2.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 25.2 | all | **19.5** | 87.0 | 94.8 | **41.0** | 360.3 | 212 / 98 / 100 |
| 3-2.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 25.2 | 1 | **119.4** | 170.6 | 188.9 | **250.2** | 469.5 | 99 / 97 / 100 |
| P1000116.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 100.3 | all | **79.9** | 254.3 | 265.6 | **161.5** | 1328.3 | 812 / 349 / 349 |
| P1000116.RW2 | Panasonic DC-GH6 | RW2 · panasonic-v8 | 100.3 | 1 | **538.7** | 732.4 | 786.1 | **1068.8** | 1921.2 | 348 / 346 / 349 |
| RAW_LEICA_DLUX3.RAW | Leica D-LUX 3 | RW2 · uncompressed | 10.6 | all | **0.70** | 5.06 | 3.02 | **15.4** | 138.4 | 47 / 46 / 49 |
| RAW_LEICA_DLUX3.RAW | Leica D-LUX 3 | RW2 · uncompressed | 10.6 | 1 | **0.69** | 5.11 | 3.09 | **66.6** | 146.8 | 47 / 45 / 49 |
| RAW_PANASONIC_FZ30_SRGB.RAW | Panasonic DMC-FZ30 | RW2 · uncompressed | 8.1 | all | **0.37** | 3.87 | 2.32 | **8.04** | 90.3 | 38 / 36 / 39 |
| RAW_PANASONIC_FZ30_SRGB.RAW | Panasonic DMC-FZ30 | RW2 · uncompressed | 8.1 | 1 | **0.44** | 3.86 | 2.31 | **42.4** | 95.9 | 38 / 36 / 39 |
| 2015-03-07-163604_sam_7204.srw | Samsung NX30 | SRW · samsung-v0 | 20.6 | all | **8.55** | 116.2 | 54.7 | **25.7** | 311.4 | 79 / 74 / 77 |
| 2015-03-07-163604_sam_7204.srw | Samsung NX30 | SRW · samsung-v0 | 20.6 | 1 | 61.7 | 114.6 | **54.2** | **166.6** | 332.6 | 77 / 74 / 78 |
| nx300.SRW | Samsung NX300 | SRW · samsung-v0 | 20.6 | all | **8.22** | 116.0 | 54.6 | **25.8** | 310.1 | 71 / 66 / 69 |
| nx300.SRW | Samsung NX300 | SRW · samsung-v0 | 20.6 | 1 | 63.0 | 113.7 | **54.8** | **167.0** | 329.0 | 69 / 66 / 69 |
| _3184416.SRW | Samsung NX3000 | SRW · samsung-v1 | 20.8 | all | **40.5** | 177.9 | 83.5 | **57.3** | 368.2 | 68 / 66 / 69 |
| _3184416.SRW | Samsung NX3000 | SRW · samsung-v1 | 20.8 | 1 | **40.4** | 179.2 | 84.1 | **144.0** | 390.1 | 67 / 65 / 68 |
| SAM_4260.SRW | Samsung NX3300 | SRW · samsung-v1 | 20.8 | all | **42.1** | 178.7 | 83.0 | **59.2** | 374.3 | 68 / 66 / 69 |
| SAM_4260.SRW | Samsung NX3300 | SRW · samsung-v1 | 20.8 | 1 | **41.8** | 180.0 | 83.7 | **145.5** | 394.6 | 68 / 66 / 69 |
| 2016-07-23-142101_sam_9364.srw | Samsung NX1 | SRW · samsung-v2 | 28.2 | all | **96.6** | 194.6 | 138.7 | **120.6** | 458.6 | 96 / 94 / 97 |
| 2016-07-23-142101_sam_9364.srw | Samsung NX1 | SRW · samsung-v2 | 28.2 | 1 | **96.8** | 199.7 | 138.4 | **244.2** | 487.5 | 96 / 94 / 97 |
| SAM_2935.SRW | Samsung NX500 | SRW · samsung-v2 | 28.2 | all | **97.7** | 188.8 | 141.7 | **121.3** | 455.5 | 86 / 84 / 87 |
| SAM_2935.SRW | Samsung NX500 | SRW · samsung-v2 | 28.2 | 1 | **97.7** | 191.1 | 141.7 | **245.6** | 484.8 | 86 / 84 / 87 |
| 03194133.SRW | Samsung NX20 | SRW · uncompressed | 21.0 | all | **2.25** | 48.4 | 10.2 | **19.0** | 244.1 | 84 / 82 / 85 |
| 03194133.SRW | Samsung NX20 | SRW · uncompressed | 21.0 | 1 | **2.68** | 48.4 | 10.2 | **106.4** | 264.1 | 84 / 81 / 85 |
| nx210.SRW | Samsung NX210 | SRW · uncompressed | 21.0 | all | **2.41** | 48.5 | 10.2 | **18.9** | 244.2 | 80 / 78 / 81 |
| nx210.SRW | Samsung NX210 | SRW · uncompressed | 21.0 | 1 | **2.65** | 48.5 | 10.2 | **106.5** | 264.1 | 80 / 78 / 81 |
| SDIM0555.X3F | Sigma DP1 Merrill | X3F · sigma-x3f | 16.1 | all | **50.5** | 793.7 | ❌ | **68.7** | 934.9 | 275 / 280 / ❌ |
| SDIM0555.X3F | Sigma DP1 Merrill | X3F · sigma-x3f | 16.1 | 1 | **121.1** | 773.0 | ❌ | **230.7** | 907.2 | 243 / 279 / ❌ |
| SDIM0061.X3F | Sigma sd Quattro H | X3F · sigma-x3f | 29.8 | all | **95.1** | 764.4 | ❌ | **118.6** | 984.0 | 340 / 382 / ❌ |
| SDIM0061.X3F | Sigma sd Quattro H | X3F · sigma-x3f | 29.8 | 1 | **130.4** | 739.0 | ❌ | **271.4** | 967.0 | 326 / 382 / ❌ |
| KODAK_C603_C643_FORMAT_NONE_YRGB0001.RAW | Kodak C603 | other · kodak-ycbcr | 6.1 | all | **2.39** | 14.9 | ❌ | **8.31** | 53.1 | 51 / 60 / ❌ |
| KODAK_C603_C643_FORMAT_NONE_YRGB0001.RAW | Kodak C603 | other · kodak-ycbcr | 6.1 | 1 | **9.60** | 14.7 | ❌ | **42.7** | 52.8 | 51 / 60 / ❌ |
| 2016_0104_005847_665To_send.RAW | Sjcam SJ5000X Elite | RAW · uncompressed | 12.0 | all | **0.72** | 29.9 | ❌ | **11.7** | 79.1 | 48 / 114 / ❌ |
| 2016_0104_005847_665To_send.RAW | Sjcam SJ5000X Elite | RAW · uncompressed | 12.0 | 1 | **0.98** | 29.9 | ❌ | **64.9** | 79.6 | 47 / 114 / ❌ |
| Pi_V1.3_picamera.raw | Raspberry Pi RPi OV5647 v.2 | other · uncompressed | 5.1 | all | **0.41** | 3.59 | ❌ | **4.61** | 52.3 | 23 / 21 / ❌ |
| Pi_V1.3_picamera.raw | Raspberry Pi RPi OV5647 v.2 | other · uncompressed | 5.1 | 1 | **1.30** | 3.60 | ❌ | **27.1** | 55.1 | 23 / 21 / ❌ |
| Tomatio_03.sti | Sinar eVolution 75 | other · uncompressed | 33.3 | all | **4.14** | 22.3 | 17.6 | **34.7** | 338.9 | 139 / 137 / 140 |
| Tomatio_03.sti | Sinar eVolution 75 | other · uncompressed | 33.3 | 1 | **4.69** | 22.3 | 17.3 | **201.6** | 373.2 | 139 / 137 / 140 |

## Failures and unsupported files

| File | library | mode | threads | error |
|---|---|---|---|---|
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| RAW.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| RAW.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| RAW.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| RAW.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| canon-r6m2-img_0346-raw-normal-full.cr3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| canon-r6m2-img_0346-raw-normal-full.cr3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| canon-r6m2-img_0346-raw-normal-full.cr3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| canon-r6m2-img_0346-raw-normal-full.cr3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| IMG_4241.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| IMG_4241.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| IMG_4241.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| IMG_4241.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| 443A0157.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| 443A0157.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| 443A0157.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| 443A0157.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| RAW_FULL_FRAME.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| RAW_FULL_FRAME.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| RAW_FULL_FRAME.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| RAW_FULL_FRAME.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon_R50_ISO100_RAW_mechanicShutter.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon-90D-RAW-ISO-100.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| Canon-90D-RAW-ISO-100.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| Canon-90D-RAW-ISO-100.CR3 | dcraw | decode | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| Canon-90D-RAW-ISO-100.CR3 | dcraw | rgb | 1 | dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty) |
| ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW | dcraw | decode | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/Sony/ILCE-7M4/ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed |
| ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW | dcraw | rgb | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/Sony/ILCE-7M4/ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed |
| 7RM5-LosslessCompressedLarge.ARW | dcraw | decode | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/Sony/ILCE-7RM5/7RM5-LosslessCompressedLarge.ARW |
| 7RM5-LosslessCompressedLarge.ARW | dcraw | rgb | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/Sony/ILCE-7RM5/7RM5-LosslessCompressedLarge.ARW |
| DSC00001.ARW | dcraw | decode | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/Sony/ILCE-6700/DSC00001.ARW |
| DSC00001.ARW | dcraw | rgb | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/Sony/ILCE-6700/DSC00001.ARW |
| DSCF0021.RAF | dcraw | decode | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| DSCF0021.RAF | dcraw | rgb | 1 | dcraw wrote an all-zero image (it cannot decode this compression) |
| Fuji-X-H2-Compressed-Raw.RAF | RawSpeed | decode | all | rawspeed::FujiDecompressor::FujiDecompressor(rawspeed::RawImage, rawspeed::ByteStream), line 854: compressed RAF header  |
| Fuji-X-H2-Compressed-Raw.RAF | RawSpeed | decode | 1 | rawspeed::FujiDecompressor::FujiDecompressor(rawspeed::RawImage, rawspeed::ByteStream), line 854: compressed RAF header  |
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
| IQ4_150MP_L_14.iiq | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| IQ4_150MP_L_14.iiq | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| IMG_1361.DNG | RawSpeed | decode | all | void rawspeed::AbstractDngDecompressor::decompress() const, line 250: Too many errors encountered. Giving up. First Erro |
| IMG_1361.DNG | RawSpeed | decode | 1 | void rawspeed::AbstractDngDecompressor::decompress() const, line 250: Too many errors encountered. Giving up. First Erro |
| 20250305_173647.dng | LibRaw | decode | all | Unsupported file format or not RAW file |
| 20250305_173647.dng | RawSpeed | decode | all | virtual rawspeed::RawImage rawspeed::DngDecoder::decodeRawInternal(), line 457: No RAW chunks found |
| 20250305_173647.dng | rawpy | decode | all | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| 20250305_173647.dng | LibRaw | rgb | all | Unsupported file format or not RAW file |
| 20250305_173647.dng | rawpy | rgb | all | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| 20250305_173647.dng | LibRaw + Little CMS | rgb | all | Unsupported file format or not RAW file |
| 20250305_173647.dng | LibRaw | decode | 1 | Unsupported file format or not RAW file |
| 20250305_173647.dng | RawSpeed | decode | 1 | virtual rawspeed::RawImage rawspeed::DngDecoder::decodeRawInternal(), line 457: No RAW chunks found |
| 20250305_173647.dng | rawpy | decode | 1 | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| 20250305_173647.dng | dcraw | decode | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/samsung/SM-S901E/20250305_173647.dng |
| 20250305_173647.dng | LibRaw | rgb | 1 | Unsupported file format or not RAW file |
| 20250305_173647.dng | rawpy | rgb | 1 | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| 20250305_173647.dng | dcraw | rgb | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/samsung/SM-S901E/20250305_173647.dng |
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
| GOPR0123.GPR | dcraw | decode | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/GoPro/HERO11 Black/GOPR0123.GPR |
| GOPR0123.GPR | LibRaw | rgb | 1 | Unsupported file format or not RAW file |
| GOPR0123.GPR | rawler | rgb | 1 | Failed to decode image, possibly corrupt image: TODO: Unsupported DNG compression |
| GOPR0123.GPR | rawpy | rgb | 1 | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| GOPR0123.GPR | dcraw | rgb | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/GoPro/HERO11 Black/GOPR0123.GPR |
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
| GOPR0018.GPR | dcraw | decode | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/GoPro/HERO10 Black/GOPR0018.GPR |
| GOPR0018.GPR | LibRaw | rgb | 1 | Unsupported file format or not RAW file |
| GOPR0018.GPR | rawler | rgb | 1 | Failed to decode image, possibly corrupt image: TODO: Unsupported DNG compression |
| GOPR0018.GPR | rawpy | rgb | 1 | LibRawFileUnsupportedError: b'Unsupported file format or not RAW file' |
| GOPR0018.GPR | dcraw | rgb | 1 | Cannot decode file /var/projects/flashraw-realworld/corpus/GoPro/HERO10 Black/GOPR0018.GPR |
| GOPR0018.GPR | LibRaw + Little CMS | rgb | 1 | Unsupported file format or not RAW file |
| full_compressed.ARW | RawSpeed | decode | all | virtual rawspeed::RawImage rawspeed::ArwDecoder::decodeRawInternal(), line 186: Unsupported compression 32766 |
| full_compressed.ARW | RawSpeed | decode | 1 | virtual rawspeed::RawImage rawspeed::ArwDecoder::decodeRawInternal(), line 186: Unsupported compression 32766 |
| full_compressed_HQ.ARW | RawSpeed | decode | all | virtual rawspeed::RawImage rawspeed::ArwDecoder::decodeRawInternal(), line 186: Unsupported compression 32766 |
| full_compressed_HQ.ARW | RawSpeed | decode | 1 | virtual rawspeed::RawImage rawspeed::ArwDecoder::decodeRawInternal(), line 186: Unsupported compression 32766 |
| DSC00800.ARW | RawSpeed | decode | all | void rawspeed::ArwDecoder::DecodeLJpeg(const rawspeed::TiffIFD*), line 307: Unsupported photometric interpretation: 6 |
| DSC00800.ARW | RawSpeed | decode | 1 | void rawspeed::ArwDecoder::DecodeLJpeg(const rawspeed::TiffIFD*), line 307: Unsupported photometric interpretation: 6 |
| Canon_EOS_R5_RAW_ISO_100_nocrop_dual.CR3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| Canon_EOS_R5_RAW_ISO_100_nocrop_dual.CR3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| canon-r6m2-img_0355-craw-dual-full.cr3 | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| canon-r6m2-img_0355-craw-dual-full.cr3 | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| RAW_CANON_PRO70_SRGB.CRW | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| RAW_CANON_PRO70_SRGB.CRW | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| IQ4_150MPIIQ_L_16bit.iiq | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| IQ4_150MPIIQ_L_16bit.iiq | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| RGB_86_0_4.IIQ | RawSpeed | decode | all | virtual void rawspeed::IiqDecoder::checkSupportInternal(const rawspeed::CameraMetaData*), line 489: Couldn't find camera |
| RGB_86_0_4.IIQ | RawSpeed | decode | 1 | virtual void rawspeed::IiqDecoder::checkSupportInternal(const rawspeed::CameraMetaData*), line 489: Couldn't find camera |
| cap_22908.IIQ | RawSpeed | decode | all | virtual void rawspeed::IiqDecoder::checkSupportInternal(const rawspeed::CameraMetaData*), line 489: Couldn't find camera |
| cap_22908.IIQ | RawSpeed | decode | 1 | virtual void rawspeed::IiqDecoder::checkSupportInternal(const rawspeed::CameraMetaData*), line 489: Couldn't find camera |
| H25_IT8.7-2_Card.TIF | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| H25_IT8.7-2_Card.TIF | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| RAW_KODAK_DC120.KDC | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| RAW_KODAK_DC120.KDC | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| RAW_KODAK_DC50.KDC | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| RAW_KODAK_DC50.KDC | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| L_000859.mos | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| L_000859.mos | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| RAW_APTUS_75.MOS | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| RAW_APTUS_75.MOS | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| CRW_6488.CRW | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| CRW_6488.CRW | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| SDIM0555.X3F | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| SDIM0555.X3F | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| SDIM0061.X3F | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| SDIM0061.X3F | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| KODAK_C603_C643_FORMAT_NONE_YRGB0001.RAW | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| KODAK_C603_C643_FORMAT_NONE_YRGB0001.RAW | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| 2016_0104_005847_665To_send.RAW | RawSpeed | decode | all | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| 2016_0104_005847_665To_send.RAW | RawSpeed | decode | 1 | bool rawspeed::RawDecoder::handleCameraSupport(const rawspeed::CameraMetaData*, const std::string&, const std::string&,  |
| Pi_V1.3_picamera.raw | RawSpeed | decode | all | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
| Pi_V1.3_picamera.raw | RawSpeed | decode | 1 | virtual std::unique_ptr<rawspeed::RawDecoder> rawspeed::RawParser::getDecoder(const rawspeed::CameraMetaData*), line 97: |
