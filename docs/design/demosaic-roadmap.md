# Demosaic audit, results and open work

What was measured on the demosaic algorithms (Linux x86-64, 8 cores / 16 threads), what was built from it, and
what is left. The default is `Demosaic::Auto` (AHD on Bayer, edge-aware on X-Trans, bilinear on other patterns); `Ppg`,
`Rcd`, `EdgeAware` on Bayer and `Bilinear` are opt-in. The published RAW-to-RGB speed comparisons stay on bilinear.

## Algorithm selection

`Demosaic::Auto` picks the algorithm per sensor when the file is decoded: AHD on Bayer, edge-aware on X-Trans,
bilinear on any other pattern; an explicit smoothing level (`noise_reduction` low, medium or high) selects edge-aware
on Bayer too, the demosaic that smooths. The choice depends only on the sensor type and the options, so it is the
same on every machine.

On X-Trans edge-aware is both the closest to ground truth and the cleanest on real files. On Bayer the two measures
disagree: edge-aware has the smaller error against the (binned) ground-truth tiles, AHD the fewest false-colour
artefacts on real files (speckle index 1.44 against 2.09 over 13 files) and about a third of
edge-aware's develop time. `Auto` was edge-aware on Bayer until the real-file measurements and a side-by-side review
of crops made AHD the default there. Gating the choice on the frame's noise estimate was tried and dropped: the
estimate rates a sharp ISO 64 foliage frame (Nikon Z8) as noisier than an ISO 3200 one, and AHD looked crisper on
it. See [benchmarks](../benchmarks.md) and the table in [bayer-demosaics](bayer-demosaics.md). `Ppg` and `Rcd` fail with `Error::InvalidArgument` on a sensor that is not Bayer when an
RGB output is requested; `Ahd` on another sensor, and edge-aware on other patterns, keep their documented fallbacks so
that existing callers keep working.

## Is edge-aware "less detailed" than AHD?

No. The fine-detail index (luma Laplacian of the central 1024x1024 crop) is higher for AHD than for edge-aware on real
Bayer files (0.0219 against 0.0179 on one 20 MP file, bilinear 0.0166), but that index also counts residual noise and
zipper energy. Against ground truth the order is the other way: on the 98 tiles of the new set edge-aware has the better
PSNR and edge PSNR than AHD at every noise level (clean 35.08 / 29.67 against 34.75 / 29.42 dB), and AHD has the lower
colour error on clean and moderate tiles. On a clean file the noise smoothing is off, so it does not explain the gap
either (output with and without it is identical on that file).

## Where edge-aware spends its time

One thread, 18 MP Bayer, 624 ms in all; per stage (timers inside the demosaic, one run):

| Stage | ms | Share |
|---|---:|---:|
| 7x7 directional filters (4 directions, 2 colours) | 205 | 33 % |
| colour conversion and 8-bit encoding (every mode pays this) | about 130 | 21 % |
| chroma pass 1 (gated mean of colour differences) | 112 | 18 % |
| chroma pass 2 | 55 | 9 % |
| direction weights | 52 | 8 % |
| first green estimate `G0` | 37 | 6 % |
| white-balanced plane build | 33 | 5 % |

Chroma pass 1 computed the same gate weight twice (red and blue); it is computed once now with identical output.
Scaling stops at about 8 threads, the physical core count of the machine; allocation, noise estimation (about 1 ms)
and column tiling were tried and did not explain it.

## Built from it

* `Demosaic::Auto`, `Demosaic::Ppg`, `Demosaic::Rcd` ([bayer-demosaics](bayer-demosaics.md)). The comparison with other implementations is in [benchmarks](../benchmarks.md).
* `Demosaic::Ahd` blends its two candidates by their homogeneity instead of a hard choice: +0.25 dB clean and +0.7 to
  +0.8 dB under noise, and the lowest real-file speckle index of every demosaic measured ([ahd-demosaic](ahd-demosaic.md)).
* Edge-aware X-Trans filters trained with a 5x5 support (below).
* Edge-aware X-Trans: the 3x3 median of the colour differences is followed by the gated second pass of the Bayer stage.
  Better on the held-out tiles at every noise level and on the real-file speckle index (2.18 to 1.84), [edge-aware-demosaic](edge-aware-demosaic.md) step 4.
* Speed, all with unchanged output unless noted: `Ppg` and `Rcd` in branch-free row loops with a 4-sample halo (25 % and
  14 % faster; the border ring changes) and 256-column chunks (cache-resident with many threads: 19 % and 24 % faster on
  16 threads of the x86-64 machine), the black-level normalisation vectorised (it was about a fifth of the
  post-process time of every demosaic), the 16-bit encode loop, `clamp` replaced in the AHD green loop (it kept the
  loop scalar; 10 % faster), and the bilinear path of X-Trans and other patterns (11 % faster). Figures from an Apple
  M1, one thread.
* `tools/bench/demosaic/own.py`: a ground-truth benchmark that needs numpy only (the older scripts need extra external packages), and `edgeaware_train.py gt` accepts X-Trans files (`.raf`). `truth.py --only`
  merges into the earlier results, as its help said.

## Tried and not kept

* **Homogeneity choice among the four directional candidates on X-Trans** (candidate images from the edge-aware
  filters, AHD-style thresholds and counts over the 3x3 window; hard choice among candidates within 90 % of the best
  count, or the gradient weights times the relative count to a power): on 98 tiles, clean X-Trans, 34.99 / 29.66 dB
  for the hard choice, 35.28 / 29.93 at best for the combination, against 35.30 / 29.93 for edge-aware. No gain.
* **A second pass that recomputes the direction weights from the first pass's green**: 35.29 / 29.91, level with
  edge-aware. The trained filters are tuned to the first-pass weights, so the combination stage is not where the
  remaining error is.
* **Residual interpolation for Bayer**: below `Rcd` everywhere; see [bayer-demosaics](bayer-demosaics.md).

## Filter support: 5x5 against 7x7

`edgeaware_train.py train --support 2` trains 5x5 filters inside the 7x7 table layout (outer taps exactly zero, which
the CPU kernels skip). Trained on the tiles of the published benchmark with the test split left out and scored with
`truth.py` on the 11 held-out photographic tiles (PSNR / edge PSNR, dB):

| held-out tiles | clean | moderate | high |
|---|---|---|---|
| Bayer 7x7 (shipped) | 42.04 / 39.43 | 37.67 / 36.16 | 34.40 / 33.40 |
| Bayer 5x5 | 41.99 / 39.32 | 37.67 / 36.15 | 34.41 / 33.41 |
| X-Trans 7x7 | 41.89 / 39.14 | 37.64 / 36.03 | 34.44 / 33.38 |
| X-Trans 5x5 (shipped) | 41.92 / 39.20 | 37.63 / 36.03 | 34.43 / 33.38 |

(Both with the one-pass median X-Trans chroma stage of the time; the two-pass stage now shipped scores 42.00 / 39.37,
37.69 / 36.13 and 34.51 / 33.47 with the 5x5 filters.)

X-Trans now ships the 5x5 filters (retrained on all tiles; a 7x7 retrain on the same tiles reproduces the previous
table byte for byte, so the pipeline is reproducible). Bayer keeps 7x7: the 5x5 fit loses 0.11 dB of edge PSNR on
clean tiles. The time saved is small, 5 to 7 % of the X-Trans develop on one thread (a measurement on a busy machine), because the halo stays at 8 (Bayer
still needs it) and the filters are only part of the work. On a different, 98-tile set (half-size renders of 95
real-world files) the two supports were also level on Bayer.

## Open

* Training data: full-resolution linear targets with simulated blur, clipping and read noise; split by source image
  and camera. The 2x2-binned renders are slightly soft. Not started.
* The shared output stage: the 16-bit sRGB encode now runs in blocks (index, table read, interpolation; 256 to 231 ms
  for a 24 MP bilinear develop to `rgb16`, one thread, output unchanged) and the linear encoders in flat per-sample
  loops (`linear-rgb16` 185 to 170, `linear-rgb-f16` 188 to 149). An AVX2 gather of the table was slower than scalar
  reads on the Ryzen. `finish_rgb` still works on interleaved triples: moving it into a separate pass over each row
  was 35 % slower than leaving it fused with the bilinear interpolation, so a faster version needs planar rows.
* Edge-aware on aarch64: the two chroma passes kept twelve row-wide sum arrays in memory, which cost twice the share
  of the develop on an Apple M1 that it does on x86-64; they now keep 32-column blocks in registers (output
  unchanged), and the filter taps use fused multiply-adds on aarch64. One thread, 18 MP Bayer on the M1: 1313 to
  899 ms in the speed table (5.4 times bilinear, 4.4 on the Ryzen 7 9700X). The rest of the gap is the 4-lane NEON
  baseline against an AVX2 compile.
* A per-pattern halo (7 for X-Trans with 5x5 filters) would save a little more on X-Trans.
* New X-Trans algorithms beyond edge-aware: the homogeneity choice and the second pass of the direction weights did
  not help; the two-pass chroma stage did (above). Better training data is the next candidate.
* The ground-truth tiles and the real files disagree on Bayer chroma: the gated pass 2 alone on the filtered
  differences is +0.17 dB clean and +0.74 dB at high noise on the tiles but raises the real-file speckle from 2.09 to
  2.96. The 2x2-binned tiles are softer than real files at the Nyquist frequency, where false colour appears, which
  is one more reason for the full-resolution training data above.
* Tried as full-resolution ground truth: the CC0 Sigma Merrill files of the corpus (Foveon, all three colours
  measured at every pixel, 7 files, 18 bright tiles). Their colour noise at ISO 100 dominates the score: plain
  bilinear has the highest PSNR (32.88 dB against 31.48 for edge-aware, 30.50 for AHD), because averaging the noisy
  reference wins. Not usable as training or test data without denoising it first, which would bias the result.
