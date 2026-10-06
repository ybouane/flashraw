//! Running kernels: upload once, one fused dispatch, GPU-resident result.

use crate::context::{GpuContext, Kernel, PipelineKey};
use crate::image::{GpuImage, GpuPixelFormat, GpuTile};
use crate::kernels::{self, Params};
use flashraw_core::{Error, FrameInfo, Orientation, PackOrder, PackedPayload, Rect, Result};
use std::sync::Arc;

/// Input raster.
#[derive(Debug, Clone, Copy)]
pub enum GpuInput<'a> {
    /// Uniformly bit-packed payload (unpacked on the GPU).
    Packed {
        data: &'a [u8],
        bits: u32,
        order: PackOrder,
        /// Bytes from one row start to the next.
        row_stride: usize,
    },
    /// Decoded u16 samples, `raster_width * cpp` per row.
    U16(&'a [u16]),
    /// Decoded f32 samples, `raster_width * cpp` per row.
    F32(&'a [f32]),
}

/// A frame to process: an input raster plus the window and coordinate anchors.
#[derive(Debug, Clone, Copy)]
pub struct GpuFrame<'a> {
    pub input: GpuInput<'a>,
    /// Size of the uploaded raster in pixels.
    pub raster_width: u32,
    pub raster_height: u32,
    pub cpp: u32,
    /// Window of the raster to process (region decodes); full raster by default.
    pub window: Rect,
    /// Sensor coordinates of the window origin (anchors the CFA and black patterns).
    pub origin: (u32, u32),
    /// Optional linearisation curve applied to integer samples (`v -> lut[min(v, len-1)]`).
    pub linearization: Option<&'a [u16]>,
}

impl<'a> GpuFrame<'a> {
    pub fn from_packed(p: &PackedPayload<'a>) -> Self {
        GpuFrame {
            input: GpuInput::Packed { data: p.data, bits: p.bits, order: p.order, row_stride: p.row_stride },
            raster_width: p.width,
            raster_height: p.height,
            cpp: 1,
            window: Rect::full(p.width, p.height),
            origin: (0, 0),
            linearization: None,
        }
    }
    pub fn from_u16(data: &'a [u16], width: u32, height: u32, cpp: u32) -> Self {
        Self::plain(GpuInput::U16(data), width, height, cpp)
    }
    pub fn from_f32(data: &'a [f32], width: u32, height: u32, cpp: u32) -> Self {
        Self::plain(GpuInput::F32(data), width, height, cpp)
    }
    fn plain(input: GpuInput<'a>, width: u32, height: u32, cpp: u32) -> Self {
        GpuFrame {
            input,
            raster_width: width,
            raster_height: height,
            cpp,
            window: Rect::full(width, height),
            origin: (0, 0),
            linearization: None,
        }
    }
    /// Process only `r` (raster coordinates); the origin moves with it.
    pub fn with_window(mut self, r: Rect) -> Self {
        self.origin = (self.origin.0 + r.x - self.window.x, self.origin.1 + r.y - self.window.y);
        self.window = r;
        self
    }
    pub fn with_origin(mut self, origin: (u32, u32)) -> Self {
        self.origin = origin;
        self
    }
    pub fn with_linearization(mut self, lut: Option<&'a [u16]>) -> Self {
        self.linearization = lut.filter(|l| !l.is_empty());
        self
    }
    /// Bytes uploaded for this frame.
    pub fn upload_bytes(&self) -> usize {
        match self.input {
            GpuInput::Packed { data, .. } => data.len(),
            GpuInput::U16(d) => d.len() * 2,
            GpuInput::F32(d) => d.len() * 4,
        }
    }
}

/// Output of the develop kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevelopOutput {
    /// sRGB RGBA8 with EXIF orientation applied.
    Rgba8,
    /// Linear sRGB-primaries f32 RGB (no orientation), like `OutputFormat::LinearRgbF32`.
    LinearRgbF32,
    /// sRGB 16-bit with EXIF orientation applied, like `OutputFormat::Rgb16`, stored as
    /// RGBA (alpha 65535): a [`GpuPixelFormat::U16`] image with 4 channels.
    Rgb16,
    /// Linear 16-bit integers (no orientation), like `OutputFormat::LinearRgb16`, stored
    /// as RGBA (alpha 65535).
    LinearRgb16,
    /// Linear half floats (no orientation), like `OutputFormat::LinearRgbF16`, stored as
    /// RGBA bit patterns (alpha 1.0) in a [`GpuPixelFormat::U16`] image with 4 channels.
    LinearRgbF16,
}

/// Parameters of the fused develop pass (mirror of `flashraw::process::DevelopParams`).
#[derive(Debug, Clone, Copy)]
pub struct DevelopSpec {
    /// Region to develop, in window coordinates.
    pub crop: Rect,
    /// 2x2 superpixel (half resolution) instead of bilinear.
    pub half_size: bool,
    /// White-balance multipliers per CFA colour (R, G, B, 4th).
    pub wb: [f32; 4],
    pub cam_to_rgb: [[f32; 3]; 3],
    pub orientation: Orientation,
    pub output: DevelopOutput,
    /// Exposure and black/white points after the colour conversion: `v * scale - offset`
    /// (`[scale, offset]`; `None` for none).
    pub levels: Option<[f32; 2]>,
}

/// What to compute.
#[derive(Debug, Clone, Copy)]
pub enum GpuOp {
    /// u16 samples (packed unpack and/or linearisation).
    Unpack,
    /// Black-subtracted, white-normalised f32 samples (sensor layout).
    Normalize,
    Develop(DevelopSpec),
}

/// Per-stage timings of one run, in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GpuTimings {
    /// Host → device: time the host spent staging input (summed over bands; with
    /// several bands it overlaps the kernels of earlier bands).
    pub upload_ms: f64,
    /// The rest of the run until the result is complete (wall clock).
    pub compute_ms: f64,
    /// Kernel execution measured with GPU timestamps (summed over bands), when supported.
    pub kernel_ms: Option<f64>,
    /// Number of bands the frame was processed in (1 = untiled).
    pub bands: u32,
}

/// Result of [`GpuContext::run`]: a GPU-resident image plus timings.
#[derive(Debug)]
pub struct GpuRun {
    pub image: GpuImage,
    pub timings: GpuTimings,
}

pub(crate) fn buffer_init(device: &wgpu::Device, label: &str, data: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
    let size = (data.len() as u64).div_ceil(4).max(1) * 4;
    let buf =
        device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: true });
    {
        let mut view = buf.get_mapped_range_mut(..).expect("buffer mapped at creation");
        let mut padded = data.to_vec();
        padded.resize(size as usize, 0);
        view.copy_from_slice(&padded);
    }
    buf.unmap();
    buf
}

/// Write `data` at offset 0, padding the final partial word with zeros
/// (`write_buffer` requires 4-byte multiples) without copying the bulk.
fn write_padded(queue: &wgpu::Queue, buf: &wgpu::Buffer, data: &[u8]) {
    let bulk = data.len() & !3;
    if bulk > 0 {
        queue.write_buffer(buf, 0, &data[..bulk]);
    }
    if bulk < data.len() {
        let mut tail = [0u8; 4];
        tail[..data.len() - bulk].copy_from_slice(&data[bulk..]);
        queue.write_buffer(buf, bulk as u64, &tail);
    }
}

/// 2-D grid of 256-wide workgroups covering `n` invocations (respects the 65535 limit).
fn grid_1d(n: u64) -> (u32, u32) {
    let groups = n.div_ceil(256).max(1);
    let x = groups.min(32768);
    (x as u32, groups.div_ceil(x) as u32)
}

fn u32_checked(v: u64, what: &str) -> Result<u32> {
    u32::try_from(v).map_err(|_| Error::LimitExceeded(format!("{what} too large for GPU addressing")))
}

fn round4(v: u64) -> u64 {
    v.div_ceil(4).max(1) * 4
}

/// How the output is laid out across bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutLayout {
    /// One buffer, bound whole by every band (fits one storage binding).
    Single,
    /// One buffer (fits `max_buffer_size`), each band binds the sub-range it writes.
    SubRange,
    /// One buffer per band ([`GpuImage::tiles`]).
    Tiled,
}

/// One band: rows `[y0, y1)` of the computed image (frame rows for unpack/normalise,
/// developed rows for develop) and the frame rows `[in0, in1)` it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Band {
    pub y0: u32,
    pub y1: u32,
    pub in0: u32,
    pub in1: u32,
}

/// Geometry of a banded run, independent of the device (unit-testable).
#[derive(Debug, Clone, Copy)]
pub(crate) struct BandGeometry {
    /// Rows of the computed image.
    pub rows: u32,
    /// Bytes uploaded per frame row (packed: max(stride, row bytes)).
    pub in_row_bytes: u64,
    /// Output bytes per computed row.
    pub out_row_bytes: u64,
    /// Frame rows read for computed rows `[y0, y1)` (unclamped above).
    pub kind: BandKind,
    /// Frame height (clamps the input rows).
    pub frame_h: u32,
    /// Computed rows must start at a multiple of this (unpack of odd-length rows: 2).
    pub row_align: u32,
    /// Preferred band granularity (develop: the 16-row workgroup tile).
    pub tile_rows: u32,
    pub max_rows: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BandKind {
    /// Same rows (unpack, normalise).
    Direct,
    /// 3x3 neighbourhood: rows `crop_y + y - 1 ..= crop_y + y + 1`.
    Bilinear { crop_y: u32 },
    /// 2x2 superpixel: rows `crop_y + 2y ..= crop_y + 2y + 1`.
    Half { crop_y: u32 },
}

impl BandGeometry {
    fn input_rows(&self, y0: u32, y1: u32) -> (u32, u32) {
        let h = self.frame_h;
        match self.kind {
            BandKind::Direct => (y0, y1.min(h)),
            BandKind::Bilinear { crop_y } => ((crop_y + y0).saturating_sub(1), (crop_y + y1 + 1).min(h)),
            BandKind::Half { crop_y } => ((crop_y + 2 * y0).min(h), (crop_y + 2 * y1).min(h)),
        }
    }

    /// Split into bands whose input (+ `in_slack` bytes) fits `budget` and, unless the
    /// output is a single binding, whose output (+ `out_slack`) fits it too.
    pub fn plan(&self, budget: u64, in_slack: u64, out_whole: bool, out_slack: u64) -> Result<Vec<Band>> {
        let rows_in = budget.saturating_sub(in_slack) / self.in_row_bytes.max(1);
        let n_in = match self.kind {
            BandKind::Direct => rows_in,
            BandKind::Bilinear { .. } => rows_in.saturating_sub(2),
            BandKind::Half { .. } => rows_in / 2,
        };
        let n_out = if out_whole { u64::MAX } else { budget.saturating_sub(out_slack) / self.out_row_bytes.max(1) };
        let mut n = n_in.min(n_out).min(self.max_rows as u64).min(self.rows as u64) as u32;
        if n < self.rows {
            if n >= self.tile_rows {
                n -= n % self.tile_rows;
            }
            n -= n % self.row_align;
        }
        if n == 0 {
            return Err(Error::LimitExceeded(format!(
                "one band of {} rows exceeds the GPU buffer limit ({:.1} MB)",
                self.row_align,
                budget as f64 / 1e6
            )));
        }
        let mut bands = Vec::with_capacity(self.rows.div_ceil(n) as usize);
        let mut y0 = 0;
        while y0 < self.rows {
            let y1 = (y0 + n).min(self.rows);
            let (in0, in1) = self.input_rows(y0, y1);
            bands.push(Band { y0, y1, in0, in1 });
            y0 = y1;
        }
        Ok(bands)
    }
}

/// Output rectangle written by developed rows `[y0, y1)` (`dw` x `dh` developed image)
/// under `orientation` (EXIF 1..8): a row band, or a column band when rotated.
pub(crate) fn band_rect(y0: u32, y1: u32, dw: u32, dh: u32, orientation: u32) -> Rect {
    let n = y1 - y0;
    match orientation {
        3 | 4 => Rect::new(0, dh - y1, dw, n),
        5 | 8 => Rect::new(y0, 0, n, dw),
        6 | 7 => Rect::new(dh - y1, 0, n, dw),
        _ => Rect::new(0, y0, dw, n),
    }
}

/// Input bytes of a band: the slice to upload plus `src_offset` / `src_len`.
struct InputSlice<'a> {
    bytes: &'a [u8],
    offset: u32,
    len: u32,
}

/// Double-buffered upload slot.
struct Slot {
    src: wgpu::Buffer,
    staging: Option<wgpu::Buffer>,
    last: Option<wgpu::SubmissionIndex>,
}

impl GpuContext {
    /// Upload `frame`, run `op` and return the GPU-resident result.
    ///
    /// Frames whose input or output exceed one storage binding
    /// ([`GpuContext::max_binding_bytes`]) are processed in horizontal bands: each band
    /// uploads only the rows it needs (plus a one-row halo for the demosaic), the upload
    /// of band N+1 overlaps the kernel of band N, and the output is either one buffer
    /// (bound per band) or a tiled [`GpuImage`].
    pub async fn run(self: &Arc<Self>, frame: &GpuFrame<'_>, info: &FrameInfo, op: &GpuOp) -> Result<GpuRun> {
        self.run_banded(frame, info, op, None).await
    }

    /// [`GpuContext::run`] with an explicit per-binding budget (bytes) instead of the
    /// device limit (used by calibration to measure the per-band cost).
    pub(crate) async fn run_banded(
        self: &Arc<Self>,
        frame: &GpuFrame<'_>,
        info: &FrameInfo,
        op: &GpuOp,
        budget: Option<u64>,
    ) -> Result<GpuRun> {
        let (w, h, cpp) = (frame.window.width, frame.window.height, frame.cpp.max(1));
        let win = frame.window;
        if w == 0 || h == 0 || win.right() > frame.raster_width || win.bottom() > frame.raster_height {
            return Err(Error::InvalidArgument(format!(
                "window {win} outside {}x{} raster",
                frame.raster_width, frame.raster_height
            )));
        }
        let mut p = Params {
            width: w,
            height: h,
            cpp,
            src_x: win.x,
            origin_x: frame.origin.0,
            origin_y: frame.origin.1,
            orientation: 1,
            wb: [1.0; 4],
            ..Default::default()
        };
        // ---- Input description ----
        let raster_samples = frame.raster_width as u64 * frame.raster_height as u64 * cpp as u64;
        let raster_row = frame.raster_width as u64 * cpp as u64;
        let (input_mode, order, in_row_bytes): (u32, u32, u64) = match frame.input {
            GpuInput::Packed { bits, order, row_stride, .. } => {
                if !(1..=16).contains(&bits) {
                    return Err(Error::InvalidArgument(format!("unsupported packed bit depth {bits}")));
                }
                let row_bytes = (raster_row * bits as u64).div_ceil(8);
                p.bits = bits;
                p.src_stride = u32_checked(row_stride as u64, "row stride")?;
                p.row_bytes = u32_checked(row_bytes, "row size")?;
                (2, kernels::order_index(order), (row_stride as u64).max(row_bytes))
            }
            GpuInput::U16(d) => {
                if (d.len() as u64) < raster_samples {
                    return Err(Error::InvalidArgument("u16 raster shorter than its dimensions".into()));
                }
                p.src_stride = u32_checked(raster_row, "row")?;
                (0, 0, raster_row * 2)
            }
            GpuInput::F32(d) => {
                if (d.len() as u64) < raster_samples {
                    return Err(Error::InvalidArgument("f32 raster shorter than its dimensions".into()));
                }
                p.src_stride = u32_checked(raster_row, "row")?;
                (1, 0, raster_row * 4)
            }
        };
        let use_lut = frame.linearization.is_some() && input_mode != 1;
        let mut calib = kernels::calib_table(info, cpp as usize);
        p.pat_w = calib.pat_w;
        p.pat_h = calib.pat_h;
        p.off_scale = calib.off_scale;
        p.off_rowd = calib.off_rowd;
        p.off_cold = calib.off_cold;
        p.n_rowd = calib.n_rowd;
        p.n_cold = calib.n_cold;
        let (cfa_w, cfa_h, sites) = kernels::cfa_sites(info);
        p.cfa_w = cfa_w;
        p.cfa_h = cfa_h;

        // ---- Output description ----
        let row_samples = w as u64 * cpp as u64;
        let mut geo = BandGeometry {
            rows: h,
            in_row_bytes,
            out_row_bytes: 0,
            kind: BandKind::Direct,
            frame_h: h,
            row_align: 1,
            tile_rows: 1,
            max_rows: u32::MAX,
        };
        let (key, out_w, out_h, channels, format) = match op {
            GpuOp::Unpack => {
                if input_mode == 1 {
                    return Err(Error::InvalidArgument("unpack needs integer input".into()));
                }
                geo.out_row_bytes = row_samples * 2;
                // A band must start on a whole output word (two u16 samples).
                geo.row_align = if row_samples % 2 == 1 { 2 } else { 1 };
                let key = PipelineKey { kernel: Kernel::Unpack, input_mode, order, use_lut, layout: 0, out_mode: 0 };
                (key, w, h, cpp, GpuPixelFormat::U16)
            }
            GpuOp::Normalize => {
                geo.out_row_bytes = row_samples * 4;
                let key = PipelineKey { kernel: Kernel::Normalize, input_mode, order, use_lut, layout: 0, out_mode: 0 };
                (key, w, h, cpp, GpuPixelFormat::F32)
            }
            GpuOp::Develop(d) => {
                let crop = d.crop.clamp_to(w, h);
                let (dw, dh) = if d.half_size { (crop.width / 2, crop.height / 2) } else { (crop.width, crop.height) };
                if dw == 0 || dh == 0 {
                    return Err(Error::InvalidArgument("develop region too small".into()));
                }
                p.crop_x = crop.x;
                p.crop_y = crop.y;
                p.dev_w = dw;
                p.dev_h = dh;
                p.wb = d.wb;
                let m = d.cam_to_rgb;
                p.m0 = [m[0][0], m[0][1], m[0][2], 0.0];
                p.m1 = [m[1][0], m[1][1], m[1][2], 0.0];
                p.m2 = [m[2][0], m[2][1], m[2][2], 0.0];
                if let Some([scale, offset]) = d.levels {
                    p.levels = [scale, offset, 1.0, 0.0];
                }
                let layout = if cpp == 1 && info.cfa.is_some_and(|c| c.is_bayer()) {
                    3
                } else if cpp == 1 && info.cfa.is_some() {
                    1
                } else if cpp >= 3 {
                    2
                } else {
                    0
                };
                let (out_mode, ow, oh, ch, fmt) = match d.output {
                    DevelopOutput::Rgba8 => {
                        p.orientation = d.orientation.exif();
                        let (ow, oh) = if d.orientation.swaps_dimensions() { (dh, dw) } else { (dw, dh) };
                        (0, ow, oh, 4, GpuPixelFormat::Rgba8)
                    }
                    DevelopOutput::LinearRgbF32 => (1, dw, dh, 3, GpuPixelFormat::F32),
                    DevelopOutput::Rgb16 => {
                        p.orientation = d.orientation.exif();
                        let (ow, oh) = if d.orientation.swaps_dimensions() { (dh, dw) } else { (dw, dh) };
                        (2, ow, oh, 4, GpuPixelFormat::U16)
                    }
                    DevelopOutput::LinearRgb16 => (3, dw, dh, 4, GpuPixelFormat::U16),
                    DevelopOutput::LinearRgbF16 => (4, dw, dh, 4, GpuPixelFormat::U16),
                };
                p.out_w = ow;
                p.out_h = oh;
                geo.rows = dh;
                geo.out_row_bytes = dw as u64 * ch as u64 * fmt.bytes_per_sample() as u64;
                geo.kind =
                    if d.half_size { BandKind::Half { crop_y: crop.y } } else { BandKind::Bilinear { crop_y: crop.y } };
                geo.tile_rows = 16;
                geo.max_rows = 16 * self.device.limits().max_compute_workgroups_per_dimension;
                let key = PipelineKey {
                    kernel: if d.half_size { Kernel::DevelopHalf } else { Kernel::Develop },
                    input_mode,
                    order,
                    use_lut,
                    layout,
                    out_mode,
                };
                (key, ow, oh, ch, fmt)
            }
        };
        if key.out_mode == 2 {
            p.off_srgb16 = calib.tab.len() as u32;
            calib.tab.extend_from_slice(kernels::srgb16_table());
        }
        let bps = format.bytes_per_sample() as u64;
        let out_bytes = out_w as u64 * out_h as u64 * channels as u64 * bps;
        // Rotated RGBA8: a band of developed rows is a column band of the output.
        let columns = p.orientation >= 5;

        // ---- Band plan ----
        let limits = self.device.limits();
        let max_buffer = self.max_buffer_bytes();
        let binding = budget.unwrap_or_else(|| self.max_binding_bytes()).min(self.max_binding_bytes()) & !3;
        let align = limits.min_storage_buffer_offset_alignment.max(4) as u64;
        let layout = if round4(out_bytes) <= binding {
            OutLayout::Single
        } else if out_bytes <= max_buffer && !columns {
            OutLayout::SubRange
        } else {
            OutLayout::Tiled
        };
        let bands = geo.plan(binding, 8, layout == OutLayout::Single, align + 4)?;

        // ---- Tables ----
        let mut lut: Vec<u32> = Vec::new();
        if let Some(l) = frame.linearization.filter(|_| use_lut) {
            lut.extend(l.iter().map(|&v| v as u32));
            p.lut_len = l.len() as u32;
        }
        p.off_srgb = lut.len() as u32;
        lut.extend_from_slice(&self.srgb);
        p.off_cfa = lut.len() as u32;
        lut.extend_from_slice(&sites);

        // ---- Per-band input slices and parameters ----
        let slice_of = |b: &Band| -> InputSlice<'_> {
            let (r0, r1) = ((win.y + b.in0) as u64, (win.y + b.in1) as u64);
            match frame.input {
                GpuInput::Packed { data, row_stride, .. } => {
                    let len = data.len() as u64;
                    let start = (r0 * row_stride as u64).min(len);
                    let end = if r1 > r0 { ((r1 - 1) * row_stride as u64 + in_row_bytes).min(len) } else { start };
                    let al = start & !3;
                    InputSlice {
                        bytes: &data[al as usize..end as usize],
                        offset: (start - al) as u32,
                        len: (end - start) as u32,
                    }
                }
                GpuInput::U16(d) => {
                    let s = &d[(r0 * raster_row) as usize..(r1 * raster_row) as usize];
                    InputSlice { bytes: bytemuck::cast_slice(s), offset: 0, len: 0 }
                }
                GpuInput::F32(d) => {
                    let s = &d[(r0 * raster_row) as usize..(r1 * raster_row) as usize];
                    InputSlice { bytes: bytemuck::cast_slice(s), offset: 0, len: 0 }
                }
            }
        };
        let slices: Vec<InputSlice<'_>> = bands.iter().map(slice_of).collect();
        let in_max = slices.iter().map(|s| round4(s.bytes.len() as u64)).max().unwrap_or(4);
        let develop = matches!(op, GpuOp::Develop(_));
        // Output binding of each band: (output rectangle, byte offset, byte size) and the
        // `dst_*` parameters.
        let mut params = Vec::with_capacity(bands.len());
        let mut out_ranges = Vec::with_capacity(bands.len());
        for (b, s) in bands.iter().zip(&slices) {
            let mut q = p;
            q.src_y = b.in0;
            q.src_offset = s.offset;
            q.src_len = s.len;
            q.band_y = b.y0;
            q.band_end = b.y1;
            q.total = u32_checked((b.y1 - b.y0) as u64 * row_samples, "band")?;
            let rect = if develop {
                band_rect(b.y0, b.y1, p.dev_w, p.dev_h, p.orientation)
            } else {
                Rect::new(0, b.y0, w, b.y1 - b.y0)
            };
            let row_out = out_w as u64 * channels as u64 * bps;
            let (offset, size) = match layout {
                OutLayout::Single => {
                    q.dst_stride = out_w;
                    q.dst_off = if develop { 0 } else { u32_checked(rect.y as u64 * row_out / 4, "output")? };
                    (0, round4(out_bytes))
                }
                OutLayout::SubRange => {
                    let start = rect.y as u64 * row_out;
                    let a = start / align * align;
                    let end = round4(rect.bottom() as u64 * row_out);
                    q.dst_y = rect.y;
                    q.dst_stride = out_w;
                    q.dst_off = ((start - a) / 4) as u32;
                    (a, end - a)
                }
                OutLayout::Tiled => {
                    q.dst_x = rect.x;
                    q.dst_y = rect.y;
                    q.dst_stride = rect.width;
                    (0, round4(rect.width as u64 * rect.height as u64 * channels as u64 * bps))
                }
            };
            params.push(q);
            out_ranges.push((rect, offset, size));
        }

        let dev = &self.device;
        let validation = dev.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom = dev.push_error_scope(wgpu::ErrorFilter::OutOfMemory);

        // ---- Buffers ----
        let ustride = (std::mem::size_of::<Params>() as u64)
            .div_ceil(limits.min_uniform_buffer_offset_alignment as u64)
            * limits.min_uniform_buffer_offset_alignment as u64;
        let mut ubytes = vec![0u8; ustride as usize * params.len()];
        for (i, q) in params.iter().enumerate() {
            let o = i * ustride as usize;
            ubytes[o..o + std::mem::size_of::<Params>()].copy_from_slice(bytemuck::bytes_of(q));
        }
        let ubuf = buffer_init(dev, "flashraw.params", &ubytes, wgpu::BufferUsages::UNIFORM);
        let tab = buffer_init(dev, "flashraw.calib", bytemuck::cast_slice(&calib.tab), wgpu::BufferUsages::STORAGE);
        let lutb = buffer_init(dev, "flashraw.lut", bytemuck::cast_slice(&lut), wgpu::BufferUsages::STORAGE);
        let mut dst_usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC;
        if self.uma {
            dst_usage |= wgpu::BufferUsages::MAP_READ;
        }
        let outputs: Vec<wgpu::Buffer> = match layout {
            OutLayout::Single | OutLayout::SubRange => {
                vec![self.acquire("flashraw.output", round4(out_bytes), dst_usage)]
            }
            OutLayout::Tiled => {
                out_ranges.iter().map(|&(_, _, size)| self.acquire("flashraw.output", size, dst_usage)).collect()
            }
        };
        // Upload path: unified memory writes straight into the storage buffer; otherwise
        // a reused mapped staging buffer + device copy, or `write_buffer` for small inputs.
        let staged = !self.uma && in_max >= 1 << 20;
        let nslots = bands.len().min(2);
        let mut slots: Vec<Slot> = (0..nslots)
            .map(|_| {
                let src_usage = if self.uma {
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::MAP_WRITE
                } else {
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST
                };
                Slot {
                    src: self.acquire("flashraw.src", in_max, src_usage),
                    staging: staged.then(|| {
                        self.acquire(
                            "flashraw.upload",
                            in_max,
                            wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                        )
                    }),
                    last: None,
                }
            })
            .collect();
        let ts = (self.info.timestamps && bands.len() * 2 <= wgpu::QUERY_SET_MAX_QUERIES as usize).then(|| {
            let n = bands.len() as u32 * 2;
            let qs = dev.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("flashraw.timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: n,
            });
            let resolve = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flashraw.ts_resolve"),
                size: n as u64 * 8,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let read = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flashraw.ts_read"),
                size: n as u64 * 8,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            (qs, resolve, read)
        });
        let pipeline = self.pipeline(key);

        // ---- Bands: upload N+1 while N computes ----
        let t0 = flashraw_core::profile::now_ms();
        let mut upload_ms = 0.0;
        for (i, (b, s)) in bands.iter().zip(&slices).enumerate() {
            let slot = &mut slots[i % nslots];
            let in_size = round4(s.bytes.len() as u64);
            // Only the host copy counts as upload; waiting for the slot to be released by
            // band N-2 is part of the (overlapped) compute.
            upload_ms += if self.uma {
                self.write_mapped(&slot.src, s.bytes, slot.last.take()).await?
            } else if let Some(st) = &slot.staging {
                self.write_mapped(st, s.bytes, slot.last.take()).await?
            } else {
                let tu = flashraw_core::profile::now_ms();
                write_padded(&self.queue, &slot.src, s.bytes);
                flashraw_core::profile::now_ms() - tu
            };
            let (_, out_offset, out_size) = out_ranges[i];
            let out_buf = if layout == OutLayout::Tiled { &outputs[i] } else { &outputs[0] };
            let bg = dev.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("flashraw.kernel"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &ubuf,
                            offset: i as u64 * ustride,
                            size: wgpu::BufferSize::new(std::mem::size_of::<Params>() as u64),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &slot.src,
                            offset: 0,
                            size: wgpu::BufferSize::new(in_size),
                        }),
                    },
                    wgpu::BindGroupEntry { binding: 2, resource: tab.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: lutb.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: out_buf,
                            offset: out_offset,
                            size: wgpu::BufferSize::new(out_size),
                        }),
                    },
                ],
            });
            let groups = match op {
                GpuOp::Unpack => grid_1d((params[i].total as u64).div_ceil(2)),
                GpuOp::Normalize => grid_1d(params[i].total as u64),
                GpuOp::Develop(_) => (p.dev_w.div_ceil(16), (b.y1 - b.y0).div_ceil(16)),
            };
            let mut enc = dev.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("flashraw.band") });
            if let Some(st) = &slot.staging {
                enc.copy_buffer_to_buffer(st, 0, &slot.src, 0, in_size);
            }
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("flashraw.kernel"),
                    timestamp_writes: ts.as_ref().map(|(qs, _, _)| wgpu::ComputePassTimestampWrites {
                        query_set: qs,
                        beginning_of_pass_write_index: Some(2 * i as u32),
                        end_of_pass_write_index: Some(2 * i as u32 + 1),
                    }),
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(groups.0, groups.1, 1);
            }
            if let Some((qs, resolve, read)) = ts.as_ref().filter(|_| i + 1 == bands.len()) {
                let n = bands.len() as u32 * 2;
                enc.resolve_query_set(qs, 0..n, resolve, 0);
                enc.copy_buffer_to_buffer(resolve, 0, read, 0, n as u64 * 8);
            }
            slot.last = Some(self.queue.submit([enc.finish()]));
        }
        self.wait_idle().await?;
        let compute_ms = (flashraw_core::profile::now_ms() - t0 - upload_ms).max(0.0);
        let kernel_ms = match &ts {
            Some((_, _, read)) => {
                let n = bands.len() * 2;
                let ticks = self
                    .read_mapped(read, n as u64 * 8, |b| {
                        b.chunks_exact(16)
                            .map(|c| {
                                let t0 = bytemuck::pod_read_unaligned::<u64>(&c[0..8]);
                                let t1 = bytemuck::pod_read_unaligned::<u64>(&c[8..16]);
                                t1.saturating_sub(t0)
                            })
                            .sum::<u64>()
                    })
                    .await?;
                let ns = ticks as f64 * self.queue.get_timestamp_period() as f64;
                // Some drivers report zeros when sampling is unsupported mid-pass.
                (ns > 0.0).then_some(ns / 1e6)
            }
            None => None,
        };
        for s in slots {
            self.release(s.src);
            if let Some(st) = s.staging {
                self.release(st);
            }
        }
        let e2 = oom.pop().await;
        let e1 = validation.pop().await;
        if let Some(e) = e2.or(e1) {
            return Err(Error::GpuUnavailable(format!("GPU error: {e}")));
        }
        if let Some(e) = self.take_error() {
            return Err(Error::GpuUnavailable(format!("GPU error: {e}")));
        }
        let tiled = layout == OutLayout::Tiled;
        let parts: Vec<GpuTile> = if tiled {
            outputs.into_iter().zip(&out_ranges).map(|(buffer, &(rect, _, _))| GpuTile { rect, buffer }).collect()
        } else {
            outputs.into_iter().map(|buffer| GpuTile { rect: Rect::full(out_w, out_h), buffer }).collect()
        };
        Ok(GpuRun {
            image: GpuImage { ctx: self.clone(), parts, tiled, width: out_w, height: out_h, channels, format },
            timings: GpuTimings { upload_ms, compute_ms, kernel_ms, bands: bands.len() as u32 },
        })
    }

    /// Blocking [`GpuContext::run`] (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run_blocking(self: &Arc<Self>, frame: &GpuFrame<'_>, info: &FrameInfo, op: &GpuOp) -> Result<GpuRun> {
        pollster::block_on(self.run(frame, info, op))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo(rows: u32, kind: BandKind, frame_h: u32) -> BandGeometry {
        BandGeometry {
            rows,
            in_row_bytes: 100,
            out_row_bytes: 400,
            kind,
            frame_h,
            row_align: 1,
            tile_rows: 16,
            max_rows: u32::MAX,
        }
    }

    fn check_cover(bands: &[Band], rows: u32) {
        assert_eq!(bands.first().unwrap().y0, 0);
        assert_eq!(bands.last().unwrap().y1, rows);
        assert!(bands.windows(2).all(|w| w[0].y1 == w[1].y0));
    }

    #[test]
    fn plan_untiled_when_it_fits() {
        let g = geo(100, BandKind::Bilinear { crop_y: 0 }, 100);
        let b = g.plan(1 << 20, 8, true, 0).unwrap();
        assert_eq!(b, vec![Band { y0: 0, y1: 100, in0: 0, in1: 100 }]);
    }

    #[test]
    fn plan_bilinear_halo_and_tile_rounding() {
        // 40 input rows fit: 38 developed rows, rounded down to 32.
        let g = geo(100, BandKind::Bilinear { crop_y: 3 }, 110);
        let b = g.plan(4008, 8, true, 0).unwrap();
        check_cover(&b, 100);
        assert_eq!(b[0], Band { y0: 0, y1: 32, in0: 2, in1: 36 });
        assert_eq!(b[1], Band { y0: 32, y1: 64, in0: 34, in1: 68 });
        assert_eq!(b.last().unwrap().in1, 104);
        // Output-bound: 400 bytes per row, 10 rows per band (below the 16-row tile).
        let b = g.plan(4000 + 8, 8, false, 8).unwrap();
        assert!(b.iter().all(|b| b.y1 - b.y0 <= 10));
        check_cover(&b, 100);
    }

    #[test]
    fn plan_half_and_alignment() {
        let g = geo(50, BandKind::Half { crop_y: 1 }, 101);
        let b = g.plan(2008, 8, true, 0).unwrap();
        check_cover(&b, 50);
        assert_eq!(b[0], Band { y0: 0, y1: 10, in0: 1, in1: 21 });
        let mut g = geo(9, BandKind::Direct, 9);
        g.row_align = 2;
        g.tile_rows = 1;
        let b = g.plan(308, 8, true, 0).unwrap();
        assert!(b.iter().all(|b| b.y0 % 2 == 0));
        check_cover(&b, 9);
        assert!(g.plan(108, 8, true, 0).is_err());
    }

    #[test]
    fn band_rects_cover_every_orientation() {
        let (dw, dh) = (7u32, 10u32);
        for o in 1..=8u32 {
            let (ow, oh) = if o >= 5 { (dh, dw) } else { (dw, dh) };
            let mut seen = vec![0u8; (ow * oh) as usize];
            for (y0, y1) in [(0, 3), (3, 4), (4, 10)] {
                let r = band_rect(y0, y1, dw, dh, o);
                assert!(r.right() <= ow && r.bottom() <= oh, "o {o} {r}");
                // Every developed pixel of the band maps inside its rectangle.
                for dy in y0..y1 {
                    for dx in 0..dw {
                        let (ox, oy) = match o {
                            2 => (dw - 1 - dx, dy),
                            3 => (dw - 1 - dx, dh - 1 - dy),
                            4 => (dx, dh - 1 - dy),
                            5 => (dy, dx),
                            6 => (dh - 1 - dy, dx),
                            7 => (dh - 1 - dy, dw - 1 - dx),
                            8 => (dy, dw - 1 - dx),
                            _ => (dx, dy),
                        };
                        assert!(r.contains_rect(&Rect::new(ox, oy, 1, 1)), "o {o} ({ox},{oy}) outside {r}");
                        seen[(oy * ow + ox) as usize] += 1;
                    }
                }
            }
            assert!(seen.iter().all(|&s| s == 1), "o {o}");
        }
    }
}
