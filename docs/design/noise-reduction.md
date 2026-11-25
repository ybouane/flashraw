# Noise reduction (`luma_noise_reduction`, `chroma_noise_reduction`)

RGB outputs can be denoised after the develop pass, with one strength for brightness noise and one for colour
noise, each from 0 (off, the default) to 1. The module is `crates/flashraw/src/denoise/` (on the GPU:
`crates/flashraw-gpu/src/denoise.rs`); it is written from the published descriptions of the methods named below.

| | Rust | CLI | Python | JavaScript | C |
|---|---|---|---|---|---|
| luma | `DecodeOptions::luma_noise_reduction` | `--luma-noise-reduction 0.5` | `luma_noise_reduction=0.5` | `lumaNoiseReduction: 0.5` | `flashraw_decode_options.luma_noise_reduction` |
| chroma | `DecodeOptions::chroma_noise_reduction` | `--chroma-noise-reduction 0.5` | `chroma_noise_reduction=0.5` | `chromaNoiseReduction: 0.5` | `flashraw_decode_options.chroma_noise_reduction` |

What the strengths mean: 1 removes all the noise the filter measures in the image; 0.5 is a moderate setting
that keeps the finest grain of brightness noise and removes colour noise about as thoroughly as the default
colour noise reduction of common raw developers (the comparison with one is in
[benchmarks](../benchmarks.md#noise-reduction)). Values outside `[0, 1]` are clamped, NaN is an
`InvalidArgument` error. The raw outputs (`raw-u16`, `raw-f32`, `normalized`) are never denoised. It runs on the
CPU and on the GPU backend ([below](#implementation); tested on Metal and on software Vulkan, the same kernels
serve Direct3D 12 and WebGPU), which agree within rounding.

This is separate from `DecodeOptions::noise_reduction`, the light 3x3 smoothing inside the edge-aware demosaic
([edge-aware demosaic](edge-aware-demosaic.md)).

## Why after the develop pass

Denoising the mosaic before demosaicing models the noise most simply (each sample is independent, with a variance
that grows with the signal), but a filter on the mosaic must know the pattern, and X-Trans, four-colour sensors and
Foveon would each need their own. After the develop pass every sensor gives the same thing: linear RGB. What the
develop pass does to the noise (the white balance amplifies red and blue, the colour matrix mixes the channels,
the demosaic spreads each sample's noise over its neighbours, so the noise is no longer white) is not modelled:
the filter measures the noise of the image it is given, per brightness and per frequency, at every scale.

## Method

1. **Planes.** RGB is split into luminance `Y = 0.2126 R + 0.7152 G + 0.0722 B` and two colour differences,
   `B - Y` and `R - Y`. The luma strength acts on `Y` only and the chroma strength on the differences only, so
   each slider changes only its own kind of noise (measured below). An orthonormal opponent transform (equal
   weights) was tried first: its "luma" plane leaves part of the visible brightness noise in the colour planes,
   so luma 1 removed only 57 % of the luminance noise and the chroma setting changed it.
2. **Scales.** Each plane is split into a pyramid of 2x2 averages: four levels for luma, six for chroma (down
   to sides of 48 pixels). Each level is denoised on its own; then, from the coarsest up, the low frequencies of a
   level (its 2x2 average, upsampled) are replaced by the denoised coarser level, upsampled (Laplacian
   recomposition). Without the denoising this reconstructs the plane exactly. Noise that the demosaic spread over
   several pixels, and the blotchy colour noise of high ISO files, is removed at the scale where it is still
   noise-like, which a single-scale filter with an 8x8 window cannot do (multiscale DCT denoising, Pierazzo,
   Morel and Facciolo, IPOL 2017).
3. **Noise model.** On every level, the 8x8 DCT of a grid of blocks (at most 50,000) is taken; the blocks are
   sorted by the mean of the luma plane over them and split into up to 12 bins of equal count. In each bin the
   flatter half of the blocks (smaller total AC energy) gives, per frequency, the median of the squared
   coefficient divided by the median of a chi-square variable with one degree of freedom (0.4549), times 1.10 to
   undo the selection of the flatter half (on white noise the estimate is then within 4 % of the true deviation,
   unit test). The bins are smoothed with weights 1/4, 1/2, 1/4. The model is the deviation of each of the 64
   frequencies as a function of brightness: it follows photon noise (larger in brighter areas), the white balance
   (larger in the colour differences) and the demosaic (smaller at high frequencies), with no per-camera profile.
   Coarse levels have few flat blocks, and texture measured as noise there made chroma noise reduction shift
   colours on clean files (median 0.9 Delta E on 32x32 block means of an ISO 100 frame). Each coarse level's
   estimate is therefore capped at 1.5 times what the finer level predicts for its 2x2 averages (a quarter of the
   mean variance of the 15 lowest AC frequencies, which is exact for white noise and larger for correlated noise):
   that brought the change on the ISO 100 frame to a median of 0.09 to 0.31 Delta E (95th percentile below 0.9),
   and changed the result on noisy frames by under 0.01 in the noise measures below.
4. **DCT denoising.** On each level, overlapping 8x8 blocks every 2 pixels (every 4 on the two finest levels of
   the colour planes, where the guided filter of step 5 decides the detail) are transformed. A first pass zeroes
   the coefficients below 2.7 deviations of their frequency at the block's brightness (hard thresholding, DCT
   denoising of Yu and Sapiro, IPOL 2011) and averages the overlapping blocks with weights
   `1 / (1 + kept coefficients)`. A second pass transforms the same blocks of the image and of that first estimate
   and multiplies each coefficient by the empirical Wiener factor `p^2 / (p^2 + sigma^2)` (`p` the pilot's
   coefficient), weighting the blocks by the inverse of their residual variance. These are the two stages of BM3D
   (Dabov et al., 2007) without the block matching: the matching is what makes BM3D several times slower, and the
   multiscale step recovers much of what it adds on correlated camera noise. The full-resolution colour planes get
   the first pass only, and so does the full-resolution luma plane at luma strengths up to 0.25, where most of the
   original noise is put back ([what these shortcuts cost](#speed)). The DC coefficient is kept, so the mean of
   every block is kept.
5. **Colour edges.** The denoised colour planes go through a guided filter (He, Sun and Tang, 2010) with the
   (denoised) luma plane as the guide and a window of radius 6 at strength 0.5: within a window, colour is a linear
   function of brightness, so colour edges that are also brightness edges stay sharp and colour elsewhere is
   averaged over the window. The regularisation is the luma noise variance at the window's brightness, so windows
   whose brightness varies by no more than its noise get the plain mean colour. The coefficients are computed at
   half resolution and applied at full resolution (fast guided filter, He and Sun, 2015): the same result within
   0.01 in the measures below at half the cost.
6. **Strength.** 1 is the full result. Below 1, part of the original noise is put back: luma keeps
   `(1 - s)^1.2` of the difference between the input and the full result, chroma keeps `1 - 2s` up to 0.5 (0.5 and
   above: none). Above 0.5 the chroma thresholds and Wiener noise grow (gain `1 + 2 (s - 0.5)`) and the guided
   filter window widens (radius `6 (0.5 + s)`). The first version scaled the noise deviations by the strength; its
   luma 0.5 and 1 were nearly the same (the Wiener stage removes all noise once the assumed deviation reaches the
   measured one), so the strength did not work as a slider.

All steps are mean-preserving except where values are clipped at display: on the noisy frames below, the mean of
each channel moves by less than 1 % (integration test), and edges keep their contrast within 8 % at strength 1
(`crates/flashraw/tests/noise_reduction.rs`). Cancelling a decode (`DecodeOptions::cancel`) is checked between
block rows, levels and stages on the CPU, and before and after the GPU work.

## Implementation

**CPU.** The DCT passes run on eight-lane vectors (`crates/flashraw/src/denoise/lanes.rs`): AVX2 chosen at run time
on x86-64, NEON on aarch64, SIMD128 in WebAssembly builds that enable it, plain arrays elsewhere. Every operation is
one IEEE single-precision operation per lane (no fused multiply-add), so every implementation gives the same result
bit for bit (unit test), and so does every thread count (the work is cut into fixed groups of block rows). The 1-D
transforms use the even/odd symmetries of the DCT matrix (50 vector operations instead of 72). The vertical transform
of every column is shared by the blocks of a row; the blocks of a row are then summed per column in the frequency
domain and inverse-transformed vertically once per column, so a block costs one horizontal forward and one
horizontal inverse transform. The plane steps are fused (one pass for the split into Y, B - Y, R - Y; the Laplacian
recomposition as one upsampled difference; the guided filter's box means without intermediate planes and its
output without upsampled coefficient planes), the luma pyramid serves as the luma plane's own pyramid, the luma
noise model of the full-resolution level is measured once for the luma passes and the guided filter, and the noise
estimate sorts block indices instead of blocks.

**GPU.** `crates/flashraw-gpu/src/denoise.rs` and `shaders/denoise.wgsl`, after the develop kernel has delivered
linear RGB; a last kernel (`shaders/denoise_encode.wgsl`) converts back to RGB and applies the tone, encoding and
orientation of the develop kernels. The steps and their order are the CPU's:

* **Noise model:** the DCT of the measured blocks (up to 50,000 per level) runs on the GPU; their luma means and
  squared coefficients are read back once (all planes and levels together, 256 bytes per block) and the sorting and
  medians run on the CPU, with the CPU code, all levels in parallel.
* **DCT passes:** a workgroup takes 48 output columns of one block row: it loads the 8 rows it needs into workgroup
  memory, transforms every column vertically, then each of the blocks that cover the 48 columns horizontally (eight
  threads per block, one per vertical frequency), thresholds or filters the coefficients, transforms back
  horizontally, sums the blocks per column (in block order, like the CPU) and transforms each column back
  vertically. A second kernel sums, per sample, the block rows that cover it. WGSL has no floating-point atomics:
  every sum runs in a fixed order, so the result does not depend on scheduling.
* **Memory limits:** a plane that does not fit one storage binding (`maxStorageBufferBindingSize`, 128 MiB by default
  in WebGPU) is stored in row chunks; each chunk keeps a halo of 16 rows that the host copies from its neighbours
  after every kernel that writes the plane, and chunk boundaries are multiples of 32 rows so that the chunks of all
  pyramid levels line up. The block rows of a pass run in bands whose buffers fit 128 MB. A chunked run gives the
  same result bit for bit as one chunk (test with 2 and 6 chunks).
* **Equivalence** with the CPU (`crates/flashraw-gpu/tests/equivalence.rs`, `tests/pipeline.rs`): on linear RGB a
  mean difference of at most 2e-7 and a 99.9th percentile of at most 6e-6 (Metal and Vulkan; the noise reduction
  itself moves samples by about 1e-2), test bounds 2e-6 and 1e-4; on RGBA8 no sample more than 1 code off (bound:
  3 codes, at most 0.1 % more than 1). They are not bit-identical: the GPU may fuse multiply-adds and its sums run in
  another order, and a coefficient that sits on the hard threshold can then flip.

## Speed

Measured shortcuts. Version 0.5.0 transformed and filtered every level of every plane twice. Three steps now
do less, each kept because the output stays close on the high ISO studio frames below (Nikon D610 ISO 25600,
Olympus E-PL9 ISO 25600, Fujifilm X-H1 ISO 51200) and on a clean one (E-PL9 ISO 100). Tolerance: the noise ratios of
the table below within 0.01 of the previous output, the contrast of the strongest 2 % of brightness edges and of the
strongest 1 % of colour edges within 0.01, and a mean Delta E against the previous output of at most 0.11. Measured
(the noise measure of the next section, ratios to the frame without noise reduction):

| Shortcut | Strengths | Change of the noise ratios | Edge contrast | Mean Delta E to the previous output |
|---|---|---|---|---|
| colour planes: one pass at full resolution, block step 4 on the second level | chroma 0.25 to 1 | C -0.003 to +0.001, L within 0.001 | within 0.003 | 0.008 to 0.093 |
| luma: one pass at full resolution | luma up to 0.25 | L fine +0.003 to +0.008, the rest within 0.003 | within 0.004 | 0.021 to 0.101 |

Measured and rejected: no Wiener pass on any luma level (luma 0.5: L fine +0.017, edge contrast -0.012, mean
Delta E 0.22), a block step of 3 or 4 at full resolution for luma (Delta E 0.13 to 0.49), a pilot at step 4 for the
Wiener pass at step 2 (luma 1: L fine +0.012), no denoising of the full-resolution colour planes (chroma 0.5: C fine
0.137 to 0.196), a single pass on the second colour level as well (Delta E 0.12) or a block step of 4 on the third
(0.17), and a single pass on every colour level (0.39 to 0.61).

## Measurements

Noise in flat areas of 16-bit sRGB renderings, in CIELab: "fine" is the deviation of `x - (3x3 mean of x)`, "coarse"
that of the 4x4 block means minus their 5x5 neighbourhood mean (blotches); L for luminance, C for `a` and `b`
pooled; median over the flattest quarter of 32x32 blocks with L between 8 and 95, chosen on the frame without noise
reduction. Ratios to the same frame without noise reduction (the measurement script is not in the repository; the
frames are studio scenes at high ISO and one real-world file from the corpus).

| File (sensor, ISO) | luma 0.25 | luma 0.5 | luma 1 | chroma 0.25 | chroma 0.5 | chroma 1 |
|---|---|---|---|---|---|---|
| | L fine / coarse | L fine / coarse | L fine / coarse | C fine / coarse | C fine / coarse | C fine / coarse |
| Olympus E-PL9 (Bayer, 25600) | 0.73 / 0.72 | 0.47 / 0.47 | 0.16 / 0.08 | 0.51 / 0.51 | 0.12 / 0.05 | 0.09 / 0.04 |
| Nikon D610 (Bayer, 25600) | 0.74 / 0.72 | 0.52 / 0.47 | 0.30 / 0.08 | 0.50 / 0.51 | 0.08 / 0.03 | 0.07 / 0.02 |
| Canon, real scene (Bayer, 16000) | 0.74 / 0.73 | 0.54 / 0.49 | 0.35 / 0.16 | 0.74 / 0.55 | 0.58 / 0.22 | 0.58 / 0.22 |
| Fujifilm X-H1 (X-Trans, 51200) | 0.74 / 0.73 | 0.51 / 0.47 | 0.26 / 0.08 | 0.51 / 0.51 | 0.08 / 0.03 | 0.05 / 0.01 |
| Fujifilm X-T1 (X-Trans, 6400) | 0.76 / 0.73 | 0.55 / 0.47 | 0.35 / 0.09 | 0.51 / 0.51 | 0.11 / 0.02 | 0.09 / 0.01 |

The luma setting leaves the colour noise within 5 % and the chroma setting leaves the luminance noise within 3 %
(except the real Canon scene, where chroma 0.5 lowers L by 2 to 3 %). On the Canon scene, lit by coloured stage
lights, part of what the measure counts as fine colour noise is colour in the scene, so the ratio stays at 0.58.

Against the ISO 100 frame of the same studio scene (Olympus E-PL9; 16x16 block means, after a per-channel gain that
matches the exposures), chroma 0.5 brings the ISO 25600 frame closer at every brightness: median Delta E 7.66 to
6.58 for L 10 to 25, 4.87 to 3.44 for L 25 to 50, 3.38 to 2.02 above 50.

Time of a whole decode (`flashraw benchmark`, RGB16 output, bilinear demosaic, best of three or five), before
these changes (0.5.0) and now; the decode without noise reduction is in the first column. The noise reduction itself
is the difference to it.

AMD Ryzen 7 9700X (8 cores, 16 threads), Linux, CPU:

| File | off | luma 0.25 | luma 0.5 | chroma 0.5 | both 0.5 |
|---|---|---|---|---|---|
| Nikon D610 NEF, 24 MP, 0.5.0 | 0.06 s | 0.69 s | 0.70 s | 1.00 s | 1.53 s |
| Nikon D610 NEF, 24 MP, now | 0.05 s | 0.27 s | 0.34 s | 0.46 s | 0.68 s |
| Canon EOS R5 Mark II CR3, 45 MP, 0.5.0 | 0.12 s | 1.28 s | 1.28 s | 1.84 s | 2.80 s |
| Canon EOS R5 Mark II CR3, 45 MP, now | 0.12 s | 0.52 s | 0.67 s | 0.88 s | 1.27 s |

Apple M1 Ultra (20 CPU cores, 64-core GPU), macOS, CPU (NEON) and GPU (Metal):

| File | off | luma 0.25 | luma 0.5 | chroma 0.5 | both 0.5 |
|---|---|---|---|---|---|
| Nikon D610 NEF, CPU, 0.5.0 | 0.04 s | 0.39 s | 0.41 s | 0.53 s | 0.83 s |
| Nikon D610 NEF, CPU, now | 0.04 s | 0.15 s | 0.20 s | 0.24 s | 0.39 s |
| Nikon D610 NEF, GPU | 0.03 s | 0.09 s | 0.11 s | 0.11 s | 0.16 s |
| Canon R5 Mark II CR3, CPU, 0.5.0 | 0.13 s | 0.75 s | 0.74 s | 1.18 s | 1.88 s |
| Canon R5 Mark II CR3, CPU, now | 0.13 s | 0.32 s | 0.40 s | 0.59 s | 0.78 s |
| Canon R5 Mark II CR3, GPU | 0.11 s | 0.23 s | 0.27 s | 0.28 s | 0.37 s |

On the GPU most of the time is the DCT passes (about 80 of the 130 ms of both strengths on the 24 MP file, GPU
timestamps); the medians of the noise estimate on the CPU take about 6 ms. Strengths above 0.5 cost the
same as 0.5. `Backend::Auto` weighs the noise reduction against each side's calibrated develop speed
([GPU backend](../gpu.md#backend-selection-backendauto)) and takes the GPU on the M1 Ultra.

Peak memory (resident set of the command-line tool, Linux): 0.33 GB without noise reduction, 1.17 GB with luma 0.5,
1.34 GB with chroma 0.5 and 1.43 GB with both on the 24 MP file (0.5.0: 1.21, 1.63 and 1.61 GB); 0.51, 2.02, 2.36
and 2.56 GB on the 45 MP file (0.5.0: 2.19, 2.95 and 3.01 GB): 8 to 9 floats per pixel with luma alone and 10 to
11 with chroma or both. On the GPU the noise reduction holds about 8 (luma) to 12.5 (both) floats per pixel besides its linear RGB
input and its output (1.2 GB for both strengths on the 24 MP file, 1.9 GB on the 45 MP file). In WebAssembly, whose
memory is limited to 4 GB, files above about 60 MP may not fit with chroma noise reduction on the CPU.

## Limits

* No learned denoiser: the published learned models for raw images need training data and weights under their own
  licences and are much slower on a CPU. This is the strongest classical approach that runs at these speeds.
* Strong noise reduction reveals colour casts that the noise hid: in deep shadows of very high ISO frames a black
  level that is slightly off shows as a tint once the colour noise no longer averages it away.
* Chroma above 0.5 changes little in the measures (the colour noise is already gone) but widens the guided filter
  window, so fine colour detail (thin coloured lines a few pixels wide) fades more.
* There is no detail or contrast control, and no sharpening.
