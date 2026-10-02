//! Running kernels: upload once, one fused dispatch, GPU-resident result.

use crate::context::{GpuContext, Kernel, PipelineKey};
use crate::image::{GpuImage, GpuPixelFormat};
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
    /// Host → device: staging copy plus transfer, until the data is resident.
    pub upload_ms: f64,
    /// Kernel dispatch until completion (wall clock).
    pub compute_ms: f64,
    /// Kernel execution measured with GPU timestamps, when supported.
    pub kernel_ms: Option<f64>,
}

/// Result of [`GpuContext::run`]: a GPU-resident image plus timings.
#[derive(Debug)]
pub struct GpuRun {
    pub image: GpuImage,
    pub timings: GpuTimings,
}

pub(crate) fn buffer_init(device: &wgpu::Device, label: &str, data: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
    let size = (data.len() as u64).div_ceil(4).max(1) * 4;
    let buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: true });
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

impl GpuContext {
    /// Upload `frame`, run `op` and return the GPU-resident result.
    pub async fn run(self: &Arc<Self>, frame: &GpuFrame<'_>, info: &FrameInfo, op: &GpuOp) -> Result<GpuRun> {
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
            src_y: win.y,
            origin_x: frame.origin.0,
            origin_y: frame.origin.1,
            orientation: 1,
            wb: [1.0; 4],
            ..Default::default()
        };
        // ---- Input description ----
        let raster_samples = frame.raster_width as u64 * frame.raster_height as u64 * cpp as u64;
        let (input_mode, order, src_bytes): (u32, u32, &[u8]) = match frame.input {
            GpuInput::Packed { data, bits, order, row_stride } => {
                if !(1..=16).contains(&bits) {
                    return Err(Error::InvalidArgument(format!("unsupported packed bit depth {bits}")));
                }
                let row_bytes = (frame.raster_width as u64 * cpp as u64 * bits as u64).div_ceil(8);
                p.bits = bits;
                p.src_stride = u32_checked(row_stride as u64, "row stride")?;
                p.row_bytes = u32_checked(row_bytes, "row size")?;
                p.src_len = u32_checked(data.len() as u64, "payload")?;
                // Every byte address the kernel can form must fit in u32.
                u32_checked(row_stride as u64 * frame.raster_height as u64 + row_bytes + 8, "payload")?;
                (2, kernels::order_index(order), data)
            }
            GpuInput::U16(d) => {
                if (d.len() as u64) < raster_samples {
                    return Err(Error::InvalidArgument("u16 raster shorter than its dimensions".into()));
                }
                p.src_stride = u32_checked(frame.raster_width as u64 * cpp as u64, "row")?;
                (0, 0, bytemuck::cast_slice(d))
            }
            GpuInput::F32(d) => {
                if (d.len() as u64) < raster_samples {
                    return Err(Error::InvalidArgument("f32 raster shorter than its dimensions".into()));
                }
                p.src_stride = u32_checked(frame.raster_width as u64 * cpp as u64, "row")?;
                (1, 0, bytemuck::cast_slice(d))
            }
        };
        u32_checked(raster_samples, "raster")?;
        let use_lut = frame.linearization.is_some() && input_mode != 1;
        // ---- Output description ----
        let calib = kernels::calib_table(info, cpp as usize);
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
        let samples = w as u64 * h as u64 * cpp as u64;
        let (key, out_w, out_h, channels, format, groups) = match op {
            GpuOp::Unpack => {
                if input_mode == 1 {
                    return Err(Error::InvalidArgument("unpack needs integer input".into()));
                }
                p.total = u32_checked(samples, "frame")?;
                let key = PipelineKey {
                    kernel: Kernel::Unpack,
                    input_mode,
                    order,
                    use_lut,
                    layout: 0,
                    out_mode: 0,
                };
                (key, w, h, cpp, GpuPixelFormat::U16, grid_1d(samples.div_ceil(2)))
            }
            GpuOp::Normalize => {
                p.total = u32_checked(samples, "frame")?;
                let key = PipelineKey {
                    kernel: Kernel::Normalize,
                    input_mode,
                    order,
                    use_lut,
                    layout: 0,
                    out_mode: 0,
                };
                (key, w, h, cpp, GpuPixelFormat::F32, grid_1d(samples))
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
                };
                p.out_w = ow;
                p.out_h = oh;
                u32_checked(dw as u64 * dh as u64 * 3, "output")?;
                let key = PipelineKey {
                    kernel: if d.half_size { Kernel::DevelopHalf } else { Kernel::Develop },
                    input_mode,
                    order,
                    use_lut,
                    layout,
                    out_mode,
                };
                (key, ow, oh, ch, fmt, (dw.div_ceil(16), dh.div_ceil(16)))
            }
        };
        let out_bytes = out_w as u64 * out_h as u64 * channels as u64 * format.bytes_per_sample() as u64;
        let out_size = out_bytes.div_ceil(4).max(1) * 4;
        let src_size = (src_bytes.len() as u64).div_ceil(4).max(1) * 4;
        self.check_size("input", src_size)?;
        self.check_size("output", out_size)?;

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

        let dev = &self.device;
        let validation = dev.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom = dev.push_error_scope(wgpu::ErrorFilter::OutOfMemory);

        // ---- Upload ----
        let t0 = flashraw_core::profile::now_ms();
        let src = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flashraw.src"),
            size: src_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        write_padded(&self.queue, &src, src_bytes);
        let tab = buffer_init(dev, "flashraw.calib", bytemuck::cast_slice(&calib.tab), wgpu::BufferUsages::STORAGE);
        let lutb = buffer_init(dev, "flashraw.lut", bytemuck::cast_slice(&lut), wgpu::BufferUsages::STORAGE);
        let ubuf = buffer_init(dev, "flashraw.params", bytemuck::bytes_of(&p), wgpu::BufferUsages::UNIFORM);
        let dst = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flashraw.output"),
            size: out_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        self.queue.submit([]);
        self.wait_idle().await?;
        let upload_ms = flashraw_core::profile::now_ms() - t0;

        // ---- Compute ----
        let pipeline = self.pipeline(key);
        let t1 = flashraw_core::profile::now_ms();
        let bg = dev.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flashraw.kernel"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: tab.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: lutb.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: dst.as_entire_binding() },
            ],
        });
        let ts = self.info.timestamps.then(|| {
            let qs = dev.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("flashraw.timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            });
            let resolve = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flashraw.ts_resolve"),
                size: 16,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let read = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flashraw.ts_read"),
                size: 16,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            (qs, resolve, read)
        });
        let mut enc = dev.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("flashraw.run") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("flashraw.kernel"),
                timestamp_writes: ts.as_ref().map(|(qs, _, _)| wgpu::ComputePassTimestampWrites {
                    query_set: qs,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                }),
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(groups.0, groups.1, 1);
        }
        if let Some((qs, resolve, read)) = &ts {
            enc.resolve_query_set(qs, 0..2, resolve, 0);
            enc.copy_buffer_to_buffer(resolve, 0, read, 0, 16);
        }
        self.queue.submit([enc.finish()]);
        self.wait_idle().await?;
        let compute_ms = flashraw_core::profile::now_ms() - t1;
        let kernel_ms = match &ts {
            Some((_, _, read)) => {
                let t = self.read_mapped(read, 16, |b| [bytemuck::pod_read_unaligned::<u64>(&b[0..8]), bytemuck::pod_read_unaligned::<u64>(&b[8..16])]).await?;
                let ns = t[1].saturating_sub(t[0]) as f64 * self.queue.get_timestamp_period() as f64;
                // Some drivers report zeros when sampling is unsupported mid-pass.
                (ns > 0.0).then_some(ns / 1e6)
            }
            None => None,
        };
        let e2 = oom.pop().await;
        let e1 = validation.pop().await;
        if let Some(e) = e2.or(e1) {
            return Err(Error::GpuUnavailable(format!("GPU error: {e}")));
        }
        if let Some(e) = self.take_error() {
            return Err(Error::GpuUnavailable(format!("GPU error: {e}")));
        }
        Ok(GpuRun {
            image: GpuImage { ctx: self.clone(), buffer: dst, width: out_w, height: out_h, channels, format },
            timings: GpuTimings { upload_ms, compute_ms, kernel_ms },
        })
    }

    /// Blocking [`GpuContext::run`] (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run_blocking(self: &Arc<Self>, frame: &GpuFrame<'_>, info: &FrameInfo, op: &GpuOp) -> Result<GpuRun> {
        pollster::block_on(self.run(frame, info, op))
    }
}
