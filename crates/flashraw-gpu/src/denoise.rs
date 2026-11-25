//! Luma and chroma noise reduction on the GPU (`shaders/denoise.wgsl`, `shaders/denoise_encode.wgsl`):
//! the method of `flashraw::denoise` (docs/design/noise-reduction.md) on a developed linear RGB
//! [`GpuImage`], followed by the tone, encoding and orientation of the develop kernels.
//!
//! Planes are stored in row chunks that each fit one storage binding (one chunk for most frames):
//! a chunk holds its interior rows plus a halo of [`HALO`] rows that the host copies from the
//! neighbouring chunks after every kernel that writes the plane, so every kernel reads only its
//! own chunk. Chunk boundaries are multiples of 32 rows at full resolution, so the chunks of
//! every pyramid level line up. The noise model is measured on the GPU (the DCT of the sampled
//! blocks) and computed on the CPU (sorting and medians) by the caller's [`DenoiseSpec::models`];
//! that is the one read-back of the run. The DCT passes write each block's weighted samples to a
//! block buffer (in bands of block rows that fit [`BLOCKS_MAX`]) and gather them per sample.

use crate::context::{GpuContext, Kernel, PipelineKey};
use crate::image::{GpuImage, GpuPixelFormat, GpuTile};
use crate::kernels::{self, DnParams, EncParams, Params};
use crate::run::{DevelopOutput, GpuCurve, GpuRun, GpuTimings, GpuTone, band_rect, buffer_init};
use flashraw_core::{Error, Orientation, Rect, Result};
use std::sync::Arc;

/// Rows of a chunk copied from its neighbours (the widest footprint of a kernel is 8).
const HALO: u32 = 16;
/// Chunk boundaries at full resolution are multiples of this (2^5: six pyramid levels).
const ALIGN: u32 = 32;
/// Largest block buffer of a DCT pass.
const BLOCKS_MAX: u64 = 128 << 20;
/// Words per noise model in the table: 16 header words, 12 bins x 64 deviations.
const MODEL_WORDS: usize = 16 + 12 * 64;

/// The noise model of one pyramid level of a plane: per bin of luma mean, the deviation of
/// each of the 64 DCT frequencies (entry `l * 8 + k`: horizontal `l`, vertical `k`; entry 0
/// unused) and the noise variance per pixel.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NoiseModel {
    /// Luma means of the bins, increasing (at most 12).
    pub means: Vec<f32>,
    pub sigmas: Vec<[f32; 64]>,
    pub pixel_var: Vec<f32>,
}

/// The CPU side of the noise estimate (see [`DenoiseSpec::models`]).
pub type ModelsFn = dyn Fn(&[Vec<Option<&[[f32; 64]]>>]) -> Vec<Vec<Option<NoiseModel>>>;

/// How one plane is denoised.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneSpec {
    /// Pyramid levels (the full image counts as one).
    pub levels: u32,
    /// Multiplier of the noise deviations (thresholds and Wiener noise).
    pub gain: f32,
    /// Block step of the two finest levels (2 beyond).
    pub steps: [u32; 2],
    /// The full-resolution level gets the hard-threshold pass only.
    pub hard_only0: bool,
    /// Fraction of the difference to the input put back (the strength below 1).
    pub keep: f32,
}

/// Everything [`GpuContext::denoise`] needs (built by `flashraw::gpu` from the CPU settings).
pub struct DenoiseSpec<'a> {
    /// The luma plane; `None` leaves it unchanged.
    pub luma: Option<PlaneSpec>,
    /// The colour planes; `None` leaves them unchanged.
    pub chroma: Option<PlaneSpec>,
    /// Radius of the guided filter's windows at half resolution (chroma only).
    pub chroma_radius: u32,
    /// Hard threshold of the first pass, in noise deviations.
    pub hard: f32,
    /// Most blocks the noise estimate measures per level.
    pub max_blocks: u32,
    /// The noise models of every level of each plane (luma, B - Y, R - Y), finest first, from the
    /// blocks measured on them (per block: the luma mean, then the squared coefficients; `None`:
    /// the level is too small to measure). A coarse level is capped by the model of the finer one.
    pub models: &'a ModelsFn,
    pub output: DevelopOutput,
    /// EXIF orientation of the display outputs.
    pub orientation: Orientation,
    pub tone: Option<GpuTone>,
    pub curve: Option<GpuCurve>,
}

/// Interior rows `[y0, y1)` and stored rows `[s0, s1)` of one chunk of a plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    y0: u32,
    y1: u32,
    s0: u32,
    s1: u32,
}

/// The row chunks of every level: boundaries at full resolution.
#[derive(Debug, Clone)]
struct Chunking {
    bounds: Vec<u32>,
}

impl Chunking {
    /// Chunks of a `w` x `h` plane whose stored rows fit `budget` bytes.
    fn new(w: u32, h: u32, budget: u64) -> Result<Self> {
        let row = w as u64 * 4;
        if h as u64 * row <= budget {
            return Ok(Chunking { bounds: vec![0, h] });
        }
        let rows = (budget / row).saturating_sub(2 * HALO as u64);
        let rows = rows - rows % ALIGN as u64;
        if rows < ALIGN as u64 {
            return Err(Error::LimitExceeded("frame too wide for the noise reduction on this GPU".into()));
        }
        let mut bounds: Vec<u32> = (0..h).step_by(rows as usize).collect();
        bounds.push(h);
        Ok(Chunking { bounds })
    }

    fn spans(&self, l: u32, h: u32) -> Vec<Span> {
        let k = self.bounds.len() - 1;
        let halo = if k > 1 { HALO } else { 0 };
        (0..k)
            .map(|i| {
                let y0 = self.bounds[i] >> l;
                let y1 = if i + 1 == k { h } else { self.bounds[i + 1] >> l };
                Span { y0, y1, s0: y0.saturating_sub(halo), s1: (y1 + halo).min(h) }
            })
            .collect()
    }
}

/// One pyramid level of a plane (or a filter intermediate), in chunks.
struct Plane {
    w: u32,
    h: u32,
    /// Level (0 = full resolution): selects the chunk boundaries.
    l: u32,
    chunks: Vec<(Span, wgpu::Buffer)>,
}

impl Plane {
    fn bind(&self, i: usize) -> Bind {
        let (s, b) = &self.chunks[i];
        Bind { buf: b.clone(), offset: 0, size: (s.s1 - s.s0) as u64 * self.w as u64 * 4 }
    }
    fn first(&self, i: usize) -> u32 {
        self.chunks[i].0.s0
    }
}

#[derive(Clone)]
struct Bind {
    buf: wgpu::Buffer,
    offset: u64,
    size: u64,
}

enum Op {
    Dn { entry: &'static str, q: DnParams, binds: [Option<Bind>; 7], groups: (u32, u32) },
    Copy { src: wgpu::Buffer, src_off: u64, dst: wgpu::Buffer, dst_off: u64, size: u64 },
    Enc { p: Params, eq: EncParams, dst: Bind, planes: [Bind; 3], groups: (u32, u32), out_mode: u32 },
}

/// Block rows of a DCT pass's buffers for a plane `w` wide: per block row, 8 rows of weighted
/// samples (the weights are an eighth of that).
fn block_rows(binding: u64, w: u32) -> u32 {
    ((BLOCKS_MAX.min(binding) / (w as u64 * 32)) as u32).min(4096)
}

/// Output columns of a workgroup of `dn_rows`.
const SEG: u32 = 48;

/// Block rows that cover one row of samples (at step 2: four, and the last block).
const MIN_BLOCK_ROWS: u32 = 5;

/// Size of level `l` of a side of `n` (the pyramid's repeated halving, rounded up).
#[cfg(test)]
fn level_size(n: u32, l: u32) -> u32 {
    (0..l).fold(n, |n, _| n.div_ceil(2))
}

/// The block grid of a side of `n` samples padded by 8: blocks at `0, step, ...` up to `n + 8`,
/// plus one at `n + 8` when that is not a multiple of `step` (`flashraw::denoise::positions`).
#[derive(Debug, Clone, Copy)]
struct Grid {
    step: u32,
    nreg: u32,
    last: u32,
    n: u32,
}

impl Grid {
    fn new(side: u32, step: u32) -> Self {
        let last = side + 8;
        let nreg = last / step + 1;
        Grid { step, nreg, last, n: nreg + ((nreg - 1) * step != last) as u32 }
    }
    #[cfg(test)]
    fn pos(&self, b: u32) -> u32 {
        if b < self.nreg { b * self.step } else { self.last }
    }
    /// First block at or after padded position `p` (`n`: none).
    fn first_from(&self, p: u32) -> u32 {
        let b = p.div_ceil(self.step);
        if b < self.nreg {
            b
        } else if self.n > self.nreg && self.last >= p {
            self.nreg
        } else {
            self.n
        }
    }
    /// One past the last block at or before padded position `q`.
    fn end_at(&self, q: u32) -> u32 {
        if self.n > self.nreg && self.last <= q { self.n } else { (q / self.step).min(self.nreg - 1) + 1 }
    }
}

/// What the measured blocks of one plane level are: their place in the stats buffer and grid.
#[derive(Debug, Clone, Copy)]
struct StatsRegion {
    /// First block (64 words each) in the stats buffer.
    first: u64,
    cols: u32,
    rows: u32,
}

/// The step (in blocks) of the noise estimate's grid of a `w` x `h` level (`None`: too small),
/// like `flashraw::denoise::estimate_step`.
fn estimate_step(w: u32, h: u32, max_blocks: u32) -> Option<u32> {
    let (bx, by) = (w as u64 / 8, h as u64 / 8);
    if bx * by < 32 {
        return None;
    }
    Some((((bx * by) as f64 / max_blocks as f64).sqrt().ceil() as u32).max(1))
}

struct Run<'c> {
    ctx: &'c Arc<GpuContext>,
    ops: Vec<Op>,
    chunking: Chunking,
    dummy_r: wgpu::Buffer,
    dummy_w: wgpu::Buffer,
    /// Buffers this run is done with, for its later planes: the GPU runs the recorded work in
    /// order, so their earlier readers finish before the new writers. They go back to the
    /// context's pool only once all the work has completed (the pool may destroy buffers).
    free: Vec<wgpu::Buffer>,
    /// Every buffer this run took from the context.
    owned: Vec<wgpu::Buffer>,
    binding: u64,
}

const PLANE_USAGE: wgpu::BufferUsages =
    wgpu::BufferUsages::STORAGE.union(wgpu::BufferUsages::COPY_SRC).union(wgpu::BufferUsages::COPY_DST);

impl<'c> Run<'c> {
    fn plane(&mut self, l: u32, w: u32, h: u32) -> Plane {
        let chunks = self
            .chunking
            .spans(l, h)
            .into_iter()
            .map(|s| (s, self.buffer((s.s1 - s.s0) as u64 * w as u64 * 4)))
            .collect();
        Plane { w, h, l, chunks }
    }

    /// A buffer of at least `bytes`: a freed one of similar size, or a new one.
    fn buffer(&mut self, bytes: u64) -> wgpu::Buffer {
        let bytes = bytes.max(16);
        let best = self
            .free
            .iter()
            .enumerate()
            .filter(|(_, b)| b.size() >= bytes && b.size() <= bytes + bytes / 4 + (1 << 20))
            .min_by_key(|(_, b)| b.size())
            .map(|(i, _)| i);
        if let Some(i) = best {
            return self.free.swap_remove(i);
        }
        let b = self.ctx.acquire("flashraw.denoise_plane", bytes, PLANE_USAGE);
        self.owned.push(b.clone());
        b
    }

    /// Done with `p`: its buffers serve the later planes of the run.
    fn free(&mut self, p: Plane) {
        for (_, b) in p.chunks {
            self.free.push(b);
        }
    }

    /// Copy the halo rows of every chunk of `p` from the chunks whose interior holds them.
    fn sync(&mut self, p: &Plane) {
        if p.chunks.len() < 2 {
            return;
        }
        let row = p.w as u64 * 4;
        for (i, (s, dst)) in p.chunks.iter().enumerate() {
            for (a, b) in [(s.s0, s.y0), (s.y1, s.s1)] {
                for (j, (t, src)) in p.chunks.iter().enumerate() {
                    let (lo, hi) = (a.max(t.y0), b.min(t.y1));
                    if j == i || lo >= hi {
                        continue;
                    }
                    self.ops.push(Op::Copy {
                        src: src.clone(),
                        src_off: (lo - t.s0) as u64 * row,
                        dst: dst.clone(),
                        dst_off: (lo - s.s0) as u64 * row,
                        size: (hi - lo) as u64 * row,
                    });
                }
            }
        }
    }

    /// One dispatch over rows `[q.rows0, q.rows1)` of a `q.w` wide plane, 16x16 groups.
    fn rows(&mut self, entry: &'static str, q: DnParams, binds: [Option<Bind>; 7]) {
        if q.rows1 > q.rows0 {
            let groups = (q.w.div_ceil(16), (q.rows1 - q.rows0).div_ceil(16));
            self.ops.push(Op::Dn { entry, q, binds, groups });
        }
    }

    /// `out` (level `l`) from interleaved RGB `input`: Y, B - Y or R - Y (`mode` 0, 1, 2).
    fn split(&mut self, input: &GpuImage, out: &Plane, mode: u32) {
        let w = out.w;
        let row = w as u64 * 12;
        let align = self.ctx.device.limits().min_storage_buffer_offset_alignment.max(4) as u64;
        for (i, (s, _)) in out.chunks.iter().enumerate() {
            for part in &input.parts {
                let (lo, hi) = (s.s0.max(part.rect.y), s.s1.min(part.rect.bottom()));
                // Rows per binding of the input (it may be larger than one binding).
                let per = ((self.binding.saturating_sub(align)) / row).max(1) as u32;
                let mut y = lo;
                while y < hi {
                    let y2 = (y + per).min(hi);
                    let start = (y - part.rect.y) as u64 * row;
                    let off = start / align * align;
                    let size = (y2 - part.rect.y) as u64 * row - off;
                    let q = DnParams {
                        w,
                        h: out.h,
                        rows0: y,
                        rows1: y2,
                        a_first: y,
                        a_off: ((start - off) / 4) as u32,
                        o_first: out.first(i),
                        mode,
                        ..Default::default()
                    };
                    let a = Bind { buf: part.buffer.clone(), offset: off, size };
                    self.rows("dn_split", q, [Some(a), None, None, None, None, Some(out.bind(i)), None]);
                    y = y2;
                }
            }
        }
        self.sync(out);
    }

    /// Level `src.l + 1` of the pyramid of 2x2 averages; with `minus`, `minus - down(src)`.
    fn down(&mut self, src: &Plane, minus: Option<&Plane>) -> Plane {
        let (w, h) = (src.w.div_ceil(2), src.h.div_ceil(2));
        let out = self.plane(src.l + 1, w, h);
        for i in 0..out.chunks.len() {
            let s = out.chunks[i].0;
            let q = DnParams {
                w,
                h,
                rows0: s.y0,
                rows1: s.y1,
                a_first: src.first(i),
                b_first: minus.map_or(0, |m| m.first(i)),
                o_first: out.first(i),
                cw: src.w,
                ch: src.h,
                mode: minus.is_some() as u32,
                ..Default::default()
            };
            self.rows(
                "dn_down",
                q,
                [Some(src.bind(i)), minus.map(|m| m.bind(i)), None, None, None, Some(out.bind(i)), None],
            );
        }
        self.sync(&out);
        out
    }

    /// `d += up(coarse)`.
    fn add_up(&mut self, d: &Plane, coarse: &Plane) {
        for i in 0..d.chunks.len() {
            let s = d.chunks[i].0;
            let q = DnParams {
                w: d.w,
                h: d.h,
                rows0: s.y0,
                rows1: s.y1,
                a_first: coarse.first(i),
                o_first: d.first(i),
                cw: coarse.w,
                ch: coarse.h,
                ..Default::default()
            };
            self.rows("dn_add_up", q, [Some(coarse.bind(i)), None, None, None, None, Some(d.bind(i)), None]);
        }
        self.sync(d);
    }

    /// Pointwise `dn_point` of `mode` on `out` with `a` (same level).
    fn point(&mut self, out: &Plane, a: &Plane, mode: u32, kf0: f32, off_var: u32, tab: &Bind) {
        for i in 0..out.chunks.len() {
            let s = out.chunks[i].0;
            let q = DnParams {
                w: out.w,
                h: out.h,
                rows0: s.y0,
                rows1: s.y1,
                a_first: a.first(i),
                o_first: out.first(i),
                mode,
                off_var,
                kf0,
                ..Default::default()
            };
            self.rows("dn_point", q, [Some(a.bind(i)), None, None, None, Some(tab.clone()), Some(out.bind(i)), None]);
        }
        self.sync(out);
    }

    /// A copy of `p`.
    fn copy(&mut self, p: &Plane) -> Plane {
        let out = self.plane(p.l, p.w, p.h);
        for (i, (s, src)) in p.chunks.iter().enumerate() {
            let size = (s.s1 - s.s0) as u64 * p.w as u64 * 4;
            self.ops.push(Op::Copy { src: src.clone(), src_off: 0, dst: out.chunks[i].1.clone(), dst_off: 0, size });
        }
        out
    }

    /// Box mean of radius `r` of `a` (or `a * b`).
    fn box_mean(&mut self, a: &Plane, b: Option<&Plane>, r: u32) -> Plane {
        let tmp = self.plane(a.l, a.w, a.h);
        for i in 0..tmp.chunks.len() {
            let s = tmp.chunks[i].0;
            let q = DnParams {
                w: a.w,
                h: a.h,
                rows0: s.y0,
                rows1: s.y1,
                a_first: a.first(i),
                b_first: b.map_or(0, |b| b.first(i)),
                o_first: tmp.first(i),
                mode: b.is_some() as u32,
                r,
                ..Default::default()
            };
            self.rows(
                "dn_box_v",
                q,
                [Some(a.bind(i)), b.map(|b| b.bind(i)), None, None, None, Some(tmp.bind(i)), None],
            );
        }
        let out = self.plane(a.l, a.w, a.h);
        let n = ((2 * r + 1) * (2 * r + 1)) as f32;
        for i in 0..out.chunks.len() {
            let s = out.chunks[i].0;
            let q = DnParams {
                w: a.w,
                h: a.h,
                rows0: s.y0,
                rows1: s.y1,
                a_first: tmp.first(i),
                o_first: out.first(i),
                r,
                kf0: n,
                ..Default::default()
            };
            self.rows("dn_box_h", q, [Some(tmp.bind(i)), None, None, None, None, Some(out.bind(i)), None]);
        }
        self.free(tmp);
        self.sync(&out);
        out
    }

    /// The measured blocks of `p` (luma guide `g`) into `stats` at `region`.
    fn stats(&mut self, p: &Plane, g: &Plane, step: u32, region: StatsRegion, stats: &wgpu::Buffer) {
        let px = step * 8;
        for i in 0..p.chunks.len() {
            let s = p.chunks[i].0;
            let (g0, g1) = (s.y0.div_ceil(px), s.y1.div_ceil(px).min(region.rows));
            if g1 <= g0 {
                continue;
            }
            let q = DnParams {
                w: p.w,
                h: p.h,
                rows1: g1,
                sg0: g0,
                a_first: p.first(i),
                b_first: g.first(i),
                step,
                scols: region.cols,
                ..Default::default()
            };
            let out = Bind {
                buf: stats.clone(),
                offset: region.first * 256,
                size: region.cols as u64 * region.rows as u64 * 256,
            };
            self.ops.push(Op::Dn {
                entry: "dn_stats",
                q,
                binds: [Some(p.bind(i)), Some(g.bind(i)), None, None, None, Some(out), None],
                groups: (region.cols.div_ceil(64), g1 - g0),
            });
        }
    }

    /// One DCT pass over `src`: hard thresholding without `pilot`, the Wiener filter with it.
    #[allow(clippy::too_many_arguments)]
    fn pass(
        &mut self,
        src: &Plane,
        guide: &Plane,
        pilot: Option<&Plane>,
        step: u32,
        off_model: u32,
        gain: f32,
        hard: f32,
        tab: &Bind,
    ) -> Plane {
        assert!(step.is_power_of_two(), "the GPU passes take block steps that are powers of two");
        let out = self.plane(src.l, src.w, src.h);
        let (gx, gy) = (Grid::new(src.w, step), Grid::new(src.h, step));
        let row_bytes = src.w as u64 * 32;
        let max_rows = block_rows(self.binding, src.w);
        let vals = self.buffer(max_rows as u64 * row_bytes);
        let wgts = self.buffer(max_rows as u64 * src.w as u64 * 4);
        for i in 0..out.chunks.len() {
            let s = out.chunks[i].0;
            let mut ya = s.y0;
            while ya < s.y1 {
                // Rows [ya, yb) need the blocks at padded rows [ya + 1, yb + 7].
                let i0 = gy.first_from(ya + 1);
                let mut yb = s.y1.min(ya + max_rows.saturating_sub(6).max(1) * step);
                let mut i1 = gy.end_at(yb + 7);
                while i1 - i0 > max_rows && yb > ya + 1 {
                    yb -= 1;
                    i1 = gy.end_at(yb + 7);
                }
                let base = DnParams {
                    w: src.w,
                    h: src.h,
                    a_first: src.first(i),
                    b_first: guide.first(i),
                    c_first: pilot.map_or(0, |p| p.first(i)),
                    o_first: out.first(i),
                    step,
                    sh: step.trailing_zeros(),
                    nbx: gx.n,
                    nreg_x: gx.nreg,
                    last_x: gx.last,
                    nreg_y: gy.nreg,
                    last_y: gy.last,
                    i0,
                    i1,
                    off_model,
                    ..Default::default()
                };
                let bytes = (i1 - i0) as u64 * row_bytes;
                let vb = Bind { buf: vals.clone(), offset: 0, size: bytes };
                let wb = Bind { buf: wgts.clone(), offset: 0, size: (i1 - i0) as u64 * src.w as u64 * 4 };
                let q = DnParams { mode: pilot.is_some() as u32, kf0: hard * gain, kf1: gain * gain, ..base };
                self.ops.push(Op::Dn {
                    entry: "dn_rows",
                    q,
                    binds: [
                        Some(src.bind(i)),
                        Some(guide.bind(i)),
                        pilot.map(|p| p.bind(i)),
                        None,
                        Some(tab.clone()),
                        Some(vb.clone()),
                        Some(wb.clone()),
                    ],
                    groups: (src.w.div_ceil(SEG), i1 - i0),
                });
                let q = DnParams { rows0: ya, rows1: yb, c_first: src.first(i), ..base };
                self.rows("dn_gather", q, [Some(vb), Some(wb), Some(src.bind(i)), None, None, Some(out.bind(i)), None]);
                ya = yb;
            }
        }
        self.free.push(vals);
        self.free.push(wgts);
        self.sync(&out);
        out
    }

    /// Denoise one plane over its pyramid `levels` with the luma pyramid `guides`; `models`:
    /// the offset of each level's model in the table (`None`: the level is kept).
    fn denoise_plane(
        &mut self,
        levels: &[&Plane],
        guides: &[&Plane],
        models: &[Option<u32>],
        spec: &PlaneSpec,
        hard: f32,
        tab: &Bind,
    ) -> Plane {
        let mut result: Option<Plane> = None;
        for l in (0..levels.len()).rev() {
            let (src, guide) = (levels[l], guides[l]);
            let d = match models[l] {
                Some(off) => {
                    let step = spec.steps.get(l).copied().unwrap_or(2);
                    let pilot = self.pass(src, guide, None, step, off, spec.gain, hard, tab);
                    if l == 0 && spec.hard_only0 {
                        pilot
                    } else {
                        let d = self.pass(src, guide, Some(&pilot), step, off, spec.gain, hard, tab);
                        self.free(pilot);
                        d
                    }
                }
                None => self.copy(src),
            };
            if let Some(coarse) = result.take() {
                // The detail of this level over the denoised coarser level.
                let diff = self.down(&d, Some(&coarse));
                self.add_up(&d, &diff);
                self.free(diff);
                self.free(coarse);
            }
            result = Some(d);
        }
        result.expect("at least one level")
    }

    /// Record the work into command buffers and submit it.
    fn flush(&mut self, enc_tables: Option<(&wgpu::Buffer, &wgpu::Buffer)>) {
        let ctx = self.ctx;
        let dev = &ctx.device;
        let align = dev.limits().min_uniform_buffer_offset_alignment as u64;
        let stride = |size: usize| (size as u64).div_ceil(align) * align;
        let (sq, sp, se) = (stride(size_of::<DnParams>()), stride(size_of::<Params>()), stride(size_of::<EncParams>()));
        let ops = std::mem::take(&mut self.ops);
        let mut qbytes: Vec<u8> = Vec::new();
        let mut pbytes: Vec<u8> = Vec::new();
        let mut ebytes: Vec<u8> = Vec::new();
        for op in &ops {
            match op {
                Op::Dn { q, .. } => {
                    let o = qbytes.len();
                    qbytes.resize(o + sq as usize, 0);
                    qbytes[o..o + size_of::<DnParams>()].copy_from_slice(bytemuck::bytes_of(q));
                }
                Op::Enc { p, eq, .. } => {
                    let o = pbytes.len();
                    pbytes.resize(o + sp as usize, 0);
                    pbytes[o..o + size_of::<Params>()].copy_from_slice(bytemuck::bytes_of(p));
                    let o = ebytes.len();
                    ebytes.resize(o + se as usize, 0);
                    ebytes[o..o + size_of::<EncParams>()].copy_from_slice(bytemuck::bytes_of(eq));
                }
                Op::Copy { .. } => {}
            }
        }
        let qbuf = buffer_init(dev, "flashraw.denoise_params", &qbytes, wgpu::BufferUsages::UNIFORM);
        let pbuf = buffer_init(dev, "flashraw.denoise_out_params", &pbytes, wgpu::BufferUsages::UNIFORM);
        let ebuf = buffer_init(dev, "flashraw.denoise_enc_params", &ebytes, wgpu::BufferUsages::UNIFORM);
        let dummy_r = Bind { buf: self.dummy_r.clone(), offset: 0, size: 16 };
        let dummy_w = Bind { buf: self.dummy_w.clone(), offset: 0, size: 16 };
        let mut encoder =
            dev.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("flashraw.denoise") });
        let (mut iq, mut ip) = (0u64, 0u64);
        let mut k = 0;
        while k < ops.len() {
            if let Op::Copy { src, src_off, dst, dst_off, size } = &ops[k] {
                encoder.copy_buffer_to_buffer(src, *src_off, dst, *dst_off, *size);
                k += 1;
                continue;
            }
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("flashraw.denoise"),
                timestamp_writes: None,
            });
            while k < ops.len() {
                match &ops[k] {
                    Op::Copy { .. } => break,
                    Op::Dn { entry: name, binds, groups, .. } => {
                        let mut entries = vec![wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &qbuf,
                                offset: iq * sq,
                                size: wgpu::BufferSize::new(size_of::<DnParams>() as u64),
                            }),
                        }];
                        for (j, b) in binds.iter().enumerate() {
                            let fallback = if j >= 5 { &dummy_w } else { &dummy_r };
                            entries.push(entry(j as u32 + 1, b.as_ref().unwrap_or(fallback)));
                        }
                        let bg = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some(name),
                            layout: &ctx.dn_layout,
                            entries: &entries,
                        });
                        pass.set_pipeline(&ctx.dn_pipeline(name));
                        pass.set_bind_group(0, &bg, &[]);
                        pass.dispatch_workgroups(groups.0, groups.1, 1);
                        iq += 1;
                    }
                    Op::Enc { dst, planes, groups, out_mode, .. } => {
                        let (tab, lut) = enc_tables.expect("output tables");
                        let bg0 = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("flashraw.denoise_encode"),
                            layout: &ctx.layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                        buffer: &pbuf,
                                        offset: ip * sp,
                                        size: wgpu::BufferSize::new(size_of::<Params>() as u64),
                                    }),
                                },
                                entry(1, &dummy_r),
                                wgpu::BindGroupEntry { binding: 2, resource: tab.as_entire_binding() },
                                wgpu::BindGroupEntry { binding: 3, resource: lut.as_entire_binding() },
                                entry(4, dst),
                            ],
                        });
                        let bg1 = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("flashraw.denoise_planes"),
                            layout: &ctx.enc_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                        buffer: &ebuf,
                                        offset: ip * se,
                                        size: wgpu::BufferSize::new(size_of::<EncParams>() as u64),
                                    }),
                                },
                                entry(1, &planes[0]),
                                entry(2, &planes[1]),
                                entry(3, &planes[2]),
                            ],
                        });
                        let key = PipelineKey {
                            kernel: Kernel::DenoiseEncode,
                            input_mode: 0,
                            order: 0,
                            use_lut: false,
                            layout: 0,
                            out_mode: *out_mode,
                        };
                        pass.set_pipeline(&ctx.pipeline(key));
                        pass.set_bind_group(0, &bg0, &[]);
                        pass.set_bind_group(1, &bg1, &[]);
                        pass.dispatch_workgroups(groups.0, groups.1, 1);
                        ip += 1;
                    }
                }
                k += 1;
            }
        }
        ctx.queue.submit([encoder.finish()]);
    }
}

fn entry(binding: u32, b: &Bind) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: &b.buf,
            offset: b.offset,
            size: wgpu::BufferSize::new(b.size.div_ceil(4).max(1) * 4),
        }),
    }
}

/// The noise model table: per model [bins, means x 12, pad x 3, deviations 12 x 64].
fn push_model(tab: &mut Vec<f32>, m: &NoiseModel) -> u32 {
    let off = tab.len() as u32;
    let n = m.means.len().min(12);
    let mut v = vec![0f32; MODEL_WORDS];
    v[0] = n as f32;
    v[1..1 + n].copy_from_slice(&m.means[..n]);
    for (b, s) in m.sigmas.iter().take(n).enumerate() {
        v[16 + b * 64..16 + (b + 1) * 64].copy_from_slice(s);
    }
    tab.extend_from_slice(&v);
    off
}

/// The pixel variance table: [bins, means x 12, pad x 3, variances x 12].
fn push_variance(tab: &mut Vec<f32>, m: Option<&NoiseModel>) -> u32 {
    let off = tab.len() as u32;
    let mut v = vec![0f32; 28];
    match m.filter(|m| !m.means.is_empty()) {
        Some(m) => {
            let n = m.means.len().min(12);
            v[0] = n as f32;
            v[1..1 + n].copy_from_slice(&m.means[..n]);
            v[16..16 + n].copy_from_slice(&m.pixel_var[..n]);
        }
        // No model: the regularisation of the CPU filter without a noise estimate.
        None => {
            v[0] = 1.0;
            v[16] = 1e-6;
        }
    }
    tab.extend_from_slice(&v);
    off
}

impl GpuContext {
    /// Denoise a developed linear RGB image (`DevelopOutput::LinearRgbF32`, as produced by
    /// [`GpuContext::run`]) and encode it to `spec.output`.
    pub async fn denoise(self: &Arc<Self>, input: &GpuImage, spec: &DenoiseSpec<'_>) -> Result<GpuRun> {
        if input.format != GpuPixelFormat::F32 || input.channels != 3 {
            return Err(Error::InvalidArgument("the noise reduction needs linear f32 RGB input".into()));
        }
        let t0 = flashraw_core::profile::now_ms();
        let (w, h) = (input.width, input.height);
        let binding = self.max_binding_bytes();
        let chunking = Chunking::new(w, h, binding)?;
        if block_rows(binding, w) < MIN_BLOCK_ROWS {
            return Err(Error::LimitExceeded("frame too wide for the noise reduction on this GPU".into()));
        }
        let dev = &self.device;
        let validation = dev.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom = dev.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let mut run = Run {
            ctx: self,
            ops: Vec::new(),
            chunking,
            dummy_r: self.acquire("flashraw.dummy", 16, PLANE_USAGE),
            dummy_w: self.acquire("flashraw.dummy", 16, PLANE_USAGE),
            free: Vec::new(),
            owned: Vec::new(),
            binding,
        };
        let small = w < 8 || h < 8;
        let luma = spec.luma.filter(|_| !small);
        let chroma = spec.chroma.filter(|_| !small);
        let y = run.plane(0, w, h);
        let u = run.plane(0, w, h);
        let v = run.plane(0, w, h);
        for (mode, p) in [(0, &y), (1, &u), (2, &v)] {
            run.split(input, p, mode);
        }
        // The luma pyramid (the guides), and the colour pyramids.
        let nl = luma.map_or(1, |s| s.levels).max(chroma.map_or(1, |s| s.levels)).max(1) as usize;
        let pyramid = |run: &mut Run<'_>, p: &Plane, n: usize| -> Vec<Plane> {
            let mut v: Vec<Plane> = Vec::new();
            for _ in 1..n {
                let d = run.down(v.last().unwrap_or(p), None);
                v.push(d);
            }
            v
        };
        let ycoarse = pyramid(&mut run, &y, nl);
        let guides: Vec<&Plane> = std::iter::once(&y).chain(&ycoarse).collect();
        let nc = chroma.map_or(0, |s| s.levels as usize);
        let ucoarse = pyramid(&mut run, &u, nc.max(1));
        let vcoarse = pyramid(&mut run, &v, nc.max(1));
        let ulevels: Vec<&Plane> = std::iter::once(&u).chain(&ucoarse).collect();
        let vlevels: Vec<&Plane> = std::iter::once(&v).chain(&vcoarse).collect();

        // ---- Noise estimate: blocks on the GPU, models on the CPU ----
        let nyl = if luma.is_some() { luma.map_or(1, |s| s.levels) as usize } else { 1 };
        let mut regions: Vec<(usize, usize, StatsRegion, u32)> = Vec::new(); // (plane, level, region, step)
        let mut total = 0u64;
        let want =
            [if luma.is_some() || chroma.is_some() { nyl } else { 0 }, nc.min(ulevels.len()), nc.min(vlevels.len())];
        let planes_of = [&guides, &ulevels, &vlevels];
        for (pi, &n) in want.iter().enumerate() {
            for l in 0..n {
                let p = planes_of[pi][l];
                if let Some(step) = estimate_step(p.w, p.h, spec.max_blocks) {
                    let (cols, rows) = ((p.w / 8).div_ceil(step), (p.h / 8).div_ceil(step));
                    regions.push((pi, l, StatsRegion { first: total, cols, rows }, step));
                    total += cols as u64 * rows as u64;
                }
            }
        }
        let stats_buf = self.acquire("flashraw.denoise_stats", (total * 256).max(256), PLANE_USAGE);
        for &(pi, l, region, step) in &regions {
            run.stats(planes_of[pi][l], guides[l], step, region, &stats_buf);
        }
        run.flush(None);
        let stats: Vec<[f32; 64]> = if total > 0 {
            let staging = self.acquire(
                "flashraw.denoise_readback",
                total * 256,
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            );
            let mut enc = dev.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
            enc.copy_buffer_to_buffer(&stats_buf, 0, &staging, 0, total * 256);
            self.queue.submit([enc.finish()]);
            let r = self
                .read_mapped(&staging, total * 256, |b| {
                    b.chunks_exact(256).map(bytemuck::pod_read_unaligned::<[f32; 64]>).collect()
                })
                .await;
            self.release(staging);
            r?
        } else {
            Vec::new()
        };
        self.release(stats_buf);
        let level_stats = |pi: usize, l: usize| -> Option<&[[f32; 64]]> {
            regions.iter().find(|r| r.0 == pi && r.1 == l).map(|r| {
                let a = r.2.first as usize;
                &stats[a..a + r.2.cols as usize * r.2.rows as usize]
            })
        };
        // Models of the measured levels of every plane, finest first (the CPU side, in one call).
        let planes: Vec<Vec<Option<&[[f32; 64]]>>> =
            want.iter().enumerate().map(|(pi, &n)| (0..n).map(|l| level_stats(pi, l)).collect()).collect();
        let mut all = (spec.models)(&planes).into_iter();
        let (ym, um, vm) =
            (all.next().unwrap_or_default(), all.next().unwrap_or_default(), all.next().unwrap_or_default());
        let y_model = ym.first().cloned().flatten();
        let mut tab: Vec<f32> = Vec::new();
        let offsets = |ms: &[Option<NoiseModel>], tab: &mut Vec<f32>| -> Vec<Option<u32>> {
            ms.iter().map(|m| m.as_ref().map(|m| push_model(tab, m))).collect()
        };
        let ymods = luma.map(|_| offsets(&ym, &mut tab));
        let umods = chroma.map(|_| offsets(&um, &mut tab));
        let vmods = chroma.map(|_| offsets(&vm, &mut tab));
        let off_var = push_variance(&mut tab, y_model.as_ref());
        let tbuf = buffer_init(dev, "flashraw.denoise_models", bytemuck::cast_slice(&tab), wgpu::BufferUsages::STORAGE);
        let tb = Bind { buf: tbuf.clone(), offset: 0, size: tab.len() as u64 * 4 };

        // ---- Passes ----
        let y2 = match (luma, &ymods) {
            (Some(s), Some(m)) => {
                let n = s.levels as usize;
                let d = run.denoise_plane(&guides[..n], &guides[..n], m, &s, spec.hard, &tb);
                if s.keep > 0.0 {
                    run.point(&d, &y, 0, s.keep, 0, &tb);
                }
                Some(d)
            }
            _ => None,
        };
        let y2r = y2.as_ref().unwrap_or(&y);
        let (u2, v2) = match (chroma, &umods, &vmods) {
            (Some(s), Some(um), Some(vm)) => {
                let n = s.levels as usize;
                let ud = run.denoise_plane(&ulevels[..n], &guides[..n], um, &s, spec.hard, &tb);
                let vd = run.denoise_plane(&vlevels[..n], &guides[..n], vm, &s, spec.hard, &tb);
                // The guided filter, coefficients at half resolution.
                let r = spec.chroma_radius.max(1);
                let i = run.down(y2r, None);
                let mean = run.box_mean(&i, None, r);
                let var = run.box_mean(&i, Some(&i), r);
                run.point(&var, &mean, 1, 0.0, 0, &tb);
                run.point(&var, &mean, 2, 0.0, off_var, &tb);
                let one = |run: &mut Run<'_>, p: Plane, orig: &Plane| -> Plane {
                    let ps = run.down(&p, None);
                    run.free(p);
                    let mp = run.box_mean(&ps, None, r);
                    let cov = run.box_mean(&i, Some(&ps), r);
                    run.free(ps);
                    let a = run.plane(1, i.w, i.h);
                    let b = run.plane(1, i.w, i.h);
                    for k in 0..a.chunks.len() {
                        let sp = a.chunks[k].0;
                        let q = DnParams {
                            w: i.w,
                            h: i.h,
                            rows0: sp.y0,
                            rows1: sp.y1,
                            a_first: cov.first(k),
                            b_first: mp.first(k),
                            c_first: mean.first(k),
                            d_first: var.first(k),
                            o_first: a.first(k),
                            o2_first: b.first(k),
                            ..Default::default()
                        };
                        run.rows(
                            "dn_gf_coef",
                            q,
                            [
                                Some(cov.bind(k)),
                                Some(mp.bind(k)),
                                Some(mean.bind(k)),
                                Some(var.bind(k)),
                                None,
                                Some(a.bind(k)),
                                Some(b.bind(k)),
                            ],
                        );
                    }
                    run.sync(&a);
                    run.sync(&b);
                    run.free(cov);
                    run.free(mp);
                    let ma = run.box_mean(&a, None, r);
                    let mb = run.box_mean(&b, None, r);
                    run.free(a);
                    run.free(b);
                    let out = run.plane(0, w, h);
                    for k in 0..out.chunks.len() {
                        let sp = out.chunks[k].0;
                        let q = DnParams {
                            w,
                            h,
                            rows0: sp.y0,
                            rows1: sp.y1,
                            a_first: ma.first(k),
                            b_first: y2r.first(k),
                            c_first: mb.first(k),
                            o_first: out.first(k),
                            cw: i.w,
                            ch: i.h,
                            ..Default::default()
                        };
                        run.rows(
                            "dn_gf_apply",
                            q,
                            [
                                Some(ma.bind(k)),
                                Some(y2r.bind(k)),
                                Some(mb.bind(k)),
                                None,
                                None,
                                Some(out.bind(k)),
                                None,
                            ],
                        );
                    }
                    run.sync(&out);
                    run.free(ma);
                    run.free(mb);
                    if s.keep > 0.0 {
                        run.point(&out, orig, 0, s.keep, 0, &tb);
                    }
                    out
                };
                let uq = one(&mut run, ud, &u);
                let vq = one(&mut run, vd, &v);
                run.free(i);
                run.free(mean);
                run.free(var);
                (Some(uq), Some(vq))
            }
            _ => (None, None),
        };
        let (u2r, v2r) = (u2.as_ref().unwrap_or(&u), v2.as_ref().unwrap_or(&v));

        // ---- Output ----
        let oriented = matches!(spec.output, DevelopOutput::Rgba8 | DevelopOutput::Rgb16);
        let orientation = if oriented { spec.orientation.exif() } else { 1 };
        let (out_mode, channels, format) = match spec.output {
            DevelopOutput::Rgba8 => (0, 4, GpuPixelFormat::Rgba8),
            DevelopOutput::LinearRgbF32 => (1, 3, GpuPixelFormat::F32),
            DevelopOutput::Rgb16 => (2, 4, GpuPixelFormat::U16),
            DevelopOutput::LinearRgb16 => (3, 4, GpuPixelFormat::U16),
            DevelopOutput::LinearRgbF16 => (4, 4, GpuPixelFormat::U16),
        };
        let (out_w, out_h) = if orientation >= 5 { (h, w) } else { (w, h) };
        let bps = format.bytes_per_sample() as u64;
        let px_bytes = channels as u64 * bps;
        let out_bytes = out_w as u64 * out_h as u64 * px_bytes;
        let mut p = Params { srgb_n: 4096, dev_w: w, dev_h: h, out_w, out_h, orientation, ..Default::default() };
        let mut etab: Vec<f32> = Vec::new();
        if let Some(t) = spec.tone.filter(|t| matches!(out_mode, 0 | 2) && !t.table.is_empty()) {
            p.off_tone = 0;
            p.tone = 1;
            p.tone_n = t.table.len() as u32;
            etab.extend(t.table.iter().flatten());
            let m = t.to_prophoto;
            p.tm0 = [m[0][0], m[0][1], m[0][2], 0.0];
            p.tm1 = [m[1][0], m[1][1], m[1][2], 0.0];
            p.tm2 = [m[2][0], m[2][1], m[2][2], 0.0];
            p.tw = [t.white[0], t.white[1], t.white[2], 0.0];
            p.tk = [t.xmax, t.scale, 0.0, 0.0];
        }
        if out_mode == 2 {
            p.off_srgb16 = etab.len() as u32;
            match spec.curve {
                Some(c) if c.lut16.len() == kernels::srgb16_table().len() => etab.extend_from_slice(c.lut16),
                _ => etab.extend_from_slice(kernels::srgb16_table()),
            }
        }
        etab.push(0.0);
        let lut: Vec<u32> = match spec.curve {
            Some(c) if out_mode == 0 && c.lut8.len() > 1 => {
                p.srgb_n = (c.lut8.len() - 1) as u32;
                c.lut8.iter().map(|&v| v as u32).collect()
            }
            _ => self.srgb.clone(),
        };
        let etab_buf =
            buffer_init(dev, "flashraw.denoise_out_tab", bytemuck::cast_slice(&etab), wgpu::BufferUsages::STORAGE);
        let lut_buf =
            buffer_init(dev, "flashraw.denoise_out_lut", bytemuck::cast_slice(&lut), wgpu::BufferUsages::STORAGE);
        let mut dst_usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC;
        if self.uma {
            dst_usage |= wgpu::BufferUsages::MAP_READ;
        }
        // Bands of developed rows: the chunks, cut further so that each band's output fits a binding.
        let single = out_bytes.div_ceil(4) * 4 <= binding;
        let row_out = w as u64 * px_bytes;
        let mut bands: Vec<(usize, u32, u32)> = Vec::new();
        for (k, (s, _)) in y.chunks.iter().enumerate() {
            let per = if single { s.y1 - s.y0 } else { ((binding / row_out).max(1) as u32).min(s.y1 - s.y0) };
            let mut a = s.y0;
            while a < s.y1 {
                let b = (a + per).min(s.y1);
                bands.push((k, a, b));
                a = b;
            }
        }
        let mut parts: Vec<GpuTile> = Vec::new();
        if single {
            parts.push(GpuTile {
                rect: Rect::full(out_w, out_h),
                buffer: self.acquire("flashraw.output", out_bytes, dst_usage),
            });
        }
        for &(k, a, b) in &bands {
            let rect = if oriented { band_rect(a, b, w, h, orientation) } else { Rect::new(0, a, w, b - a) };
            let mut q = Params { band_y: a, band_end: b, ..p };
            let dst = if single {
                q.dst_stride = out_w;
                Bind { buf: parts[0].buffer.clone(), offset: 0, size: out_bytes }
            } else {
                q.dst_x = rect.x;
                q.dst_y = rect.y;
                q.dst_stride = rect.width;
                let size = rect.width as u64 * rect.height as u64 * px_bytes;
                let buf = self.acquire("flashraw.output", size, dst_usage);
                parts.push(GpuTile { rect, buffer: buf.clone() });
                Bind { buf, offset: 0, size }
            };
            let eq = EncParams { y_first: y2r.first(k), u_first: u2r.first(k), v_first: v2r.first(k), w };
            run.ops.push(Op::Enc {
                p: q,
                eq,
                dst,
                planes: [y2r.bind(k), u2r.bind(k), v2r.bind(k)],
                groups: (w.div_ceil(16), (b - a).div_ceil(16)),
                out_mode,
            });
        }
        run.flush(Some((&etab_buf, &lut_buf)));
        let chunks = y.chunks.len() as u32;
        drop((y, u, v, y2, u2, v2, ycoarse, ucoarse, vcoarse));
        let done = self.wait_idle().await;
        drop(run.free);
        for b in run.owned.into_iter().chain([run.dummy_r, run.dummy_w]) {
            self.release(b);
        }
        done?;
        let e2 = oom.pop().await;
        let e1 = validation.pop().await;
        if let Some(e) = e2.or(e1) {
            return Err(Error::GpuUnavailable(format!("GPU error: {e}")));
        }
        if let Some(e) = self.take_error() {
            return Err(Error::GpuUnavailable(format!("GPU error: {e}")));
        }
        let tiled = !single;
        Ok(GpuRun {
            image: GpuImage { ctx: self.clone(), parts, tiled, width: out_w, height: out_h, channels, format },
            timings: GpuTimings {
                upload_ms: 0.0,
                compute_ms: flashraw_core::profile::now_ms() - t0,
                kernel_ms: None,
                bands: chunks,
            },
        })
    }

    /// Blocking [`GpuContext::denoise`] (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn denoise_blocking(self: &Arc<Self>, input: &GpuImage, spec: &DenoiseSpec<'_>) -> Result<GpuRun> {
        pollster::block_on(self.denoise(input, spec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_matches_the_cpu_positions() {
        for (side, step) in [(37u32, 2u32), (40, 4), (41, 4), (100, 3), (8, 2)] {
            let n = side + 16;
            let last = n - 8;
            let mut v: Vec<u32> = (0..=last).step_by(step as usize).collect();
            if *v.last().unwrap() != last {
                v.push(last);
            }
            let g = Grid::new(side, step);
            assert_eq!(g.n as usize, v.len());
            for (b, &p) in v.iter().enumerate() {
                assert_eq!(g.pos(b as u32), p);
            }
            for p in 0..n {
                let first = v.iter().position(|&x| x >= p).unwrap_or(v.len()) as u32;
                assert_eq!(g.first_from(p), first, "{side} {step} {p}");
                let end = v.iter().rposition(|&x| x <= p).map_or(0, |i| i + 1) as u32;
                assert_eq!(g.end_at(p), end, "{side} {step} {p}");
            }
        }
    }

    #[test]
    fn chunks_line_up_on_every_level() {
        let c = Chunking::new(1000, 5000, 1000 * 4 * 700).unwrap();
        assert!(c.bounds.len() > 2);
        for l in 0..6 {
            let h = level_size(5000, l);
            let s = c.spans(l, h);
            assert_eq!(s[0].y0, 0);
            assert_eq!(s.last().unwrap().y1, h);
            for (i, w) in s.windows(2).enumerate() {
                assert_eq!(w[0].y1, w[1].y0);
                assert!(w[0].y0 < w[0].y1);
                // Every level starts its chunks at the full-resolution boundaries, halved.
                assert_eq!(w[1].y0, c.bounds[i + 1] >> l);
            }
            for x in &s {
                assert!(x.s0 <= x.y0 && x.y1 <= x.s1 && x.s1 <= h);
                assert!((x.s1 - x.s0) as u64 * 1000 * 4 <= 1000 * 4 * 700);
            }
        }
        assert_eq!(Chunking::new(100, 100, 1 << 20).unwrap().bounds, vec![0, 100]);
    }
}
