//! GPU-resident images.

use crate::context::GpuContext;
use flashraw_core::{Error, Rect, Result};
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

/// One band of a [`GpuImage`]: the pixels of `rect` (image coordinates), stored
/// row-major with `rect.width * channels` samples per row from byte 0 of `buffer`.
#[derive(Debug)]
pub struct GpuTile {
    pub rect: Rect,
    pub buffer: wgpu::Buffer,
}

/// An image living in GPU storage buffers (`STORAGE | COPY_SRC`, plus `MAP_READ` on
/// unified-memory adapters), on the device of the [`GpuContext`] that produced it.
///
/// Most images are a single buffer: use [`GpuImage::buffer`] to bind it in your own
/// wgpu/WebGPU pipelines, [`GpuImage::to_texture`] for an `Rgba8Unorm` texture, or
/// download it. Images larger than the adapter's storage-binding limit are **tiled**
/// ([`GpuImage::is_tiled`]): [`GpuImage::tiles`] lists one buffer per band with its
/// rectangle in the image (full-width row bands, or full-height column bands for
/// RGBA8 outputs rotated by 90/270 degrees). `download` and `to_texture` reassemble
/// tiled images.
///
/// Buffers come from FlashRaw's buffer pool and are **returned to the pool when the
/// `GpuImage` is dropped** (large allocations are expensive to recreate). Keep the
/// `GpuImage` alive while the GPU may still use them, or take ownership with
/// [`GpuImage::into_buffer`] / [`GpuImage::into_tiles`]. A buffer may be larger than
/// the data it holds.
pub struct GpuImage {
    pub(crate) ctx: Arc<GpuContext>,
    /// Untiled: one part covering the image. Empty only after `into_tiles`.
    pub(crate) parts: Vec<GpuTile>,
    pub(crate) tiled: bool,
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
            .field("tiles", &if self.tiled { self.parts.len() } else { 0 })
            .field("adapter", &self.ctx.info.name)
            .finish()
    }
}

/// Identity semantics: two `GpuImage`s are equal when they are the same buffers.
impl PartialEq for GpuImage {
    fn eq(&self, other: &Self) -> bool {
        self.parts.len() == other.parts.len() && self.parts.iter().zip(&other.parts).all(|(a, b)| a.buffer == b.buffer)
    }
}

impl Drop for GpuImage {
    fn drop(&mut self) {
        for t in self.parts.drain(..) {
            self.ctx.release(t.buffer);
        }
    }
}

/// Copy `src` into `dst` using several threads for large buffers (native): the
/// destination is usually fresh memory, and page faults dominate a serial copy.
fn par_copy(dst: &mut [u8], src: &[u8]) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        const CHUNK: usize = 8 << 20;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8);
        if src.len() >= 2 * CHUNK && threads > 1 {
            let per = src.len().div_ceil(threads).div_ceil(4096) * 4096;
            std::thread::scope(|s| {
                for (d, c) in dst.chunks_mut(per).zip(src.chunks(per)) {
                    s.spawn(move || d.copy_from_slice(c));
                }
            });
            return;
        }
    }
    dst.copy_from_slice(src);
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
    /// Bytes of image data (buffers may be larger).
    pub fn byte_len(&self) -> usize {
        self.len() * self.format.bytes_per_sample()
    }
    /// The storage buffer holding the whole image (row-major, from byte 0), or `None`
    /// for a tiled image (see [`GpuImage::tiles`]). See the type docs for lifetimes.
    pub fn buffer(&self) -> Option<&wgpu::Buffer> {
        if self.tiled { None } else { self.parts.first().map(|t| &t.buffer) }
    }
    /// The image is split into several buffers (it is larger than one storage binding).
    pub fn is_tiled(&self) -> bool {
        self.tiled
    }
    /// The buffers and the rectangle each one holds (one tile covering the image when
    /// untiled). Tiles are disjoint, cover the image, and are ordered by position.
    pub fn tiles(&self) -> &[GpuTile] {
        &self.parts
    }
    /// Take ownership of the buffer of an untiled image (it will not be recycled).
    /// A tiled image is returned unchanged as the error.
    pub fn into_buffer(mut self) -> std::result::Result<wgpu::Buffer, GpuImage> {
        if self.tiled || self.parts.is_empty() {
            return Err(self);
        }
        Ok(self.parts.remove(0).buffer)
    }
    /// Take ownership of all buffers (they will not be recycled).
    pub fn into_tiles(mut self) -> Vec<GpuTile> {
        std::mem::take(&mut self.parts)
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

    /// Download to host memory (tiled images are reassembled). Returns the pixels and
    /// the wall time in ms.
    pub async fn download_async(&self) -> Result<(HostPixels, f64)> {
        let t0 = flashraw_core::profile::now_ms();
        let n = self.len();
        let mut px = match self.format {
            GpuPixelFormat::Rgba8 => HostPixels::U8(vec![0u8; n]),
            GpuPixelFormat::U16 => HostPixels::U16(vec![0u16; n]),
            GpuPixelFormat::F32 => HostPixels::F32(vec![0f32; n]),
        };
        let out: &mut [u8] = match &mut px {
            HostPixels::U8(v) => v,
            HostPixels::U16(v) => bytemuck::cast_slice_mut(v),
            HostPixels::F32(v) => bytemuck::cast_slice_mut(v),
        };
        let ctx = &self.ctx;
        let bpp = self.channels as usize * self.format.bytes_per_sample();
        let row = self.width as usize * bpp;
        // Untiled: the image starts at byte 0 of the single buffer, row-major.
        for tile in &self.parts {
            let r = tile.rect;
            let tile_row = r.width as usize * bpp;
            let bytes = tile_row * r.height as usize;
            if bytes == 0 {
                continue;
            }
            let size = (bytes as u64).div_ceil(4) * 4;
            let copy_out = |src: &[u8]| {
                if tile_row == row {
                    let o = r.y as usize * row;
                    par_copy(&mut out[o..o + bytes], &src[..bytes]);
                } else {
                    for (y, s) in src[..bytes].chunks_exact(tile_row).enumerate() {
                        let o = (r.y as usize + y) * row + r.x as usize * bpp;
                        out[o..o + tile_row].copy_from_slice(s);
                    }
                }
            };
            let buf = &tile.buffer;
            if buf.usage().contains(wgpu::BufferUsages::MAP_READ) {
                // Unified memory: read the storage buffer in place.
                ctx.read_mapped(buf, size, copy_out).await?;
            } else {
                let staging =
                    ctx.acquire("flashraw.readback", size, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
                let mut enc =
                    ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
                enc.copy_buffer_to_buffer(buf, 0, &staging, 0, size);
                ctx.queue.submit([enc.finish()]);
                let r = ctx.read_mapped(&staging, size, copy_out).await;
                ctx.release(staging);
                r?;
            }
        }
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

    /// Copy an RGBA8 image (tiled or not) into a new `Rgba8Unorm` texture (usage
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
        let mut enc = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("to_texture") });
        for tile in &self.parts {
            let r = tile.rect;
            if r.is_empty() {
                continue;
            }
            let params: [u32; 4] = [r.width, r.height, r.x, r.y];
            let ubuf = crate::run::buffer_init(
                &ctx.device,
                "flashraw.tex_params",
                bytemuck::cast_slice(&params),
                wgpu::BufferUsages::UNIFORM,
            );
            let bytes = r.width as u64 * r.height as u64 * 4;
            let bg = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("flashraw.to_texture"),
                layout: &ctx.tex_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &tile.buffer,
                            offset: 0,
                            size: wgpu::BufferSize::new(bytes),
                        }),
                    },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view) },
                ],
            });
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(ctx.tex_pipeline());
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(r.width.div_ceil(16), r.height.div_ceil(16), 1);
        }
        ctx.queue.submit([enc.finish()]);
        if let Some(e) = ctx.take_error() {
            return Err(Error::GpuUnavailable(format!("GPU error in to_texture: {e}")));
        }
        Ok(tex)
    }
}
