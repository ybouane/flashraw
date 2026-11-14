# Homogeneity-directed demosaic (`Demosaic::Ahd`)

`Demosaic::Ahd` is the demosaic that the default, `Demosaic::Auto`, picks on Bayer sensors (the alternative is
[`Demosaic::EdgeAware`](edge-aware-demosaic.md), which `Auto` keeps for X-Trans). It follows a published, widely used method: adaptive homogeneity-directed demosaicing (Hirakawa and Parks, 2005).
It is written from the paper's description; the module is `crates/flashraw/src/ahd.rs`.

| | CLI | Python | JavaScript | C |
|---|---|---|---|---|
| name | `--demosaic ahd` | `demosaic="ahd"` | `demosaic: 'ahd'` | `FLASHRAW_DEMOSAIC_AHD` |

Sensors that are not Bayer (X-Trans, four-colour, Foveon, ...) use the edge-aware demosaic where it exists and bilinear
otherwise.

The GPU backend runs it too (`ahd_green`, `ahd_lab`, `ahd_hom`, `ahd_blend` in
`crates/flashraw-gpu/shaders/edge.wgsl`, then the edge-aware encode kernel). The kernels follow the CPU's operations, but
AHD decides with thresholds: where GPU rounding moves a lightness or chroma step across a threshold, a homogeneity count
changes and the pixel's blend with it. On an Apple M1 Ultra (Metal) 0.35 % to 0.75 % of the 8-bit samples of the
synthetic test frames differ from the CPU, by up to 25 codes (mean |diff| 2.6e-5 in linear float); on Mesa's llvmpipe
none differ (`crates/flashraw-gpu/tests/ahd.rs`). A 24 MP Canon CR2 to RGBA8 on that machine: 130 ms with
`--backend gpu` (kernel time 26 ms) against 168 ms on the CPU.

## Method

Per pixel, on the white-balanced mosaic (clipped at the white level):

1. **Two candidates.** Green is interpolated along rows (H) and along columns (V): the mean of the two nearest greens
   plus a quarter of the second difference of the colour at the pixel, limited to the range of those two greens.
   Red and blue follow from colour differences: a missing colour is the pixel's green plus the mean of the
   differences `R - G` (or `B - G`) measured in the 3x3 window, each candidate using its own green plane.
2. **Perceptual coordinates.** Each candidate maps to a lightness `L = (R' + 2 G' + B') / 4` and chroma
   `a = R' - G'`, `b = B' - G'` with `X' = sqrt(max(X, 0))`. This is a cheap stand-in for CIELab: on the benchmark
   tiles and real crops it gave the same scores to the second decimal, and costs a square root instead of a
   cube root with a table.
3. **Homogeneity.** The thresholds at a pixel are the smaller, over H (neighbours along the row) and V (neighbours along
   the column), of the largest lightness step and of the largest squared chroma step to the two neighbours. A
   candidate's homogeneity there is the number of the 8 neighbours within both thresholds, summed over the 3x3 window.
4. **Blend.** The two candidates are mixed in proportion to their sums, `w_H = h_H / (h_H + h_V)` (halves when both
   are zero). The paper keeps the candidate with the larger sum; the blend was measured against that and kept (below).

Support is 6 samples on every side, so every pixel is computed by the same operations whatever the band (64 rows),
column chunk (192 columns), thread count or crop; pixels closer than 6 samples to the frame border use bilinear.
The plane loops are compiled a second time for AVX2 (no FMA) and dispatched at run time; results are bit-identical.
The homogeneity counts run over contiguous planes (a first version over interleaved colour triples was 2.5 times
slower).

## Hard choice or blend

On the 11 held-out photographic tiles of the benchmark (`tools/bench/demosaic/own.py --split test`, which gives the same
numbers as `truth.py` for the same binary; PSNR / edge PSNR, dB) and the real-file speckle index of the 13
Bayer files of `real.py` (lower is cleaner), with the weight `w_H = h_H^p / (h_H^p + h_V^p)`:

| selection | clean | moderate noise | high noise | speckle |
|---|---:|---:|---:|---:|
| hard choice (the paper) | 41.51 / 38.87 | 34.97 / 33.99 | 30.95 / 30.42 | 1.62 |
| `p = 8` | 41.59 / 38.93 | 35.18 / 34.16 | 31.18 / 30.64 | |
| `p = 4` | 41.66 / 38.98 | 35.36 / 34.31 | 31.39 / 30.83 | |
| `p = 2` | 41.72 / 39.02 | 35.55 / 34.48 | 31.60 / 31.03 | 1.45 |
| **`p = 1` (shipped)** | **41.76 / 39.02** | 35.67 / 34.59 | 31.74 / 31.16 | **1.44** |
| `p = 1/2` | 41.76 / 38.99 | 35.72 / 34.62 | 31.79 / 31.21 | 1.45 |
| plain average (`p = 0`) | 41.69 / 38.87 | 35.71 / 34.59 | 31.80 / 31.20 | 1.48 |

The hard choice is the worst on every measure. Under noise most of the gain comes from averaging the two candidates
(which averages their noise); on clean tiles and on real files the homogeneity weights still help (the plain average
loses 0.15 dB of edge PSNR and has more false colour), and `p = 1` is the best balance.

## Against the other demosaics

Bayer, ground-truth tiles (PSNR over all pixels / over the highest-gradient pixels, dB, photographic tiles), the
no-reference speckle index of real files, from `docs/benchmarks.md`:

| | clean | moderate noise | speckle index (13 files) |
|---|---:|---:|---:|
| `Demosaic::Ahd` | 41.76 / 39.02 | 35.67 / 34.59 | 1.44 |
| `Demosaic::EdgeAware` | 42.04 / 39.43 | 37.67 / 36.16 | 2.09 |

`EdgeAware` is closer to the truth on clean and (much more) on noisy tiles, because its trained filters keep the
measured sample and it smooths noise; `Ahd` has the fewest false-colour dots on real files of every demosaic measured,
FlashRaw's or the reference converters'. Use `Ahd` for the cleanest colour, `EdgeAware` for the smallest error.
