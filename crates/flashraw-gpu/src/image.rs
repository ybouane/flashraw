//! GPU-resident images.

use crate::context::GpuContext;
use flashraw_core::{Error, Result};
use std::sync::Arc;

/// Element format of a [`GpuImage`] buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuPixelFormat {
    /// u16 samples, two per 32-bit word (byte-identical to a host `Vec<u16>`).
    U16,
    /// f32 samples.
    F32,
    /// sRGB RGBA8, one pixel per 32-bit word (`R | G << 8 | B << 16 | A << 24`).
    Rgba8,
}

impl GpuPixelFormat {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            GpuPixelFormat::U16 => 2,
            GpuPixelFormat::F32 => 4,
            GpuPixelFormat::Rgba8 => 1,
        }
    }
}

/// An image living in a GPU storage buffer (`STORAGE | COPY_SRC`), on the device of
/// the [`GpuContext`] that produced it.
///
/// Use [`GpuImage::buffer`] to bind it in your own wgpu/WebGPU pipelines,
/// [`GpuImage::to_texture`] for an `Rgba8Unorm` texture, or download it.
pub struct GpuImage {
    pub(crate) ctx: Arc<GpuContext>,
    pub(crate) buffer: wgpu::Buffer,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) channels: u32,
    pub(crate) format: GpuPixelFormat,
}

impl std::fmt::Debug for GpuImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("channels", &self.channels)
            .field("format", &self.format)
            .field("adapter", &self.ctx.info.name)
            .finish()
    }
}

/// Identity semantics: two `GpuImage`s are equal when they are the same buffer.
impl PartialEq for GpuImage {
    fn eq(&self, other: &Self) -> bool {
        self.buffer == other.buffer
    }
}

/// Host copy of a downloaded [`GpuImage`].
#[derive(Debug, Clone, PartialEq)]
pub enum HostPixels {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
}

impl GpuImage {
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Samples per pixel (1 for CFA/mono, 3 for RGB, 4 for RGBA8).
    pub fn channels(&self) -> u32 {
        self.channels
    }
    pub fn format(&self) -> GpuPixelFormat {
        self.format
    }
    /// Number of samples (`width * height * channels`; bytes for RGBA8).
    pub fn len(&self) -> usize {
        self.width as usize * self.height as usize * self.channels as usize
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Bytes of image data (the buffer may be up to 3 bytes larger).
    pub fn byte_len(&self) -> usize {
        self.len() * self.format.bytes_per_sample()
    }
    /// The underlying storage buffer (usage `STORAGE | COPY_SRC`).
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
    pub fn device(&self) -> &wgpu::Device {
        &self.ctx.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.ctx.queue
    }
    pub fn context(&self) -> &Arc<GpuContext> {
        &self.ctx
    }

    /// Download to host memory. Returns the pixels and the wall time in ms.
    pub async fn download_async(&self) -> Result<(HostPixels, f64)> {
        let t0 = flashraw_core::profile::now_ms();
        let size = (self.byte_len() as u64).div_ceil(4) * 4;
        let ctx = &self.ctx;
        let staging = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flashraw.readback"),
            size: size.max(4),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_buffer_to_buffer(&self.buffer, 0, &staging, 0, size);
        ctx.queue.submit([enc.finish()]);
        let n = self.len();
        let px = ctx
            .read_mapped(&staging, size.max(4), |bytes| match self.format {
                GpuPixelFormat::Rgba8 => HostPixels::U8(bytes[..n].to_vec()),
                GpuPixelFormat::U16 => {
                    let mut v = vec![0u16; n];
                    bytemuck::cast_slice_mut::<u16, u8>(&mut v).copy_from_slice(&bytes[..n * 2]);
                    HostPixels::U16(v)
                }
                GpuPixelFormat::F32 => {
                    let mut v = vec![0f32; n];
                    bytemuck::cast_slice_mut::<f32, u8>(&mut v).copy_from_slice(&bytes[..n * 4]);
                    HostPixels::F32(v)
                }
            })
            .await?;
        if let Some(e) = ctx.take_error() {
            return Err(Error::GpuUnavailable(format!("GPU error during download: {e}")));
        }
        Ok((px, flashraw_core::profile::now_ms() - t0))
    }

    /// Blocking download (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn download(&self) -> Result<(HostPixels, f64)> {
        pollster::block_on(self.download_async())
    }

    /// Copy an RGBA8 image into a new `Rgba8Unorm` texture (usage
    /// `STORAGE_BINDING | TEXTURE_BINDING | COPY_SRC`) for sampling/rendering.
    /// The copy is queued; it completes before any later submission on the queue.
    pub fn to_texture(&self) -> Result<wgpu::Texture> {
        if self.format != GpuPixelFormat::Rgba8 {
            return Err(Error::InvalidArgument("to_texture requires an RGBA8 GpuImage".into()));
        }
        let ctx = &self.ctx;
        let max = ctx.device.limits().max_texture_dimension_2d;
        if self.width > max || self.height > max {
            return Err(Error::LimitExceeded(format!(
                "{}x{} exceeds the adapter's {max} px texture limit",
                self.width, self.height
            )));
        }
        let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("flashraw.rgba8"),
            size: wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let params: [u32; 4] = [self.width, self.height, 0, 0];
        let ubuf = crate::run::buffer_init(
            &ctx.device,
            "flashraw.tex_params",
            bytemuck::cast_slice(&params),
            wgpu::BufferUsages::UNIFORM,
        );
        let bg = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flashraw.to_texture"),
            layout: &ctx.tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view) },
            ],
        });
        let mut enc = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("to_texture") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(ctx.tex_pipeline());
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(self.width.div_ceil(16), self.height.div_ceil(16), 1);
        }
        ctx.queue.submit([enc.finish()]);
        if let Some(e) = ctx.take_error() {
            return Err(Error::GpuUnavailable(format!("GPU error in to_texture: {e}")));
        }
        Ok(tex)
    }
}
