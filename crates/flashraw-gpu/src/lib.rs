//! # flashraw-gpu
//!
//! Optional wgpu compute backend for FlashRaw (Vulkan, Metal, DX12, GL and WebGPU in
//! the browser). It runs the post-decode pipeline on the GPU:
//!
//! * **unpack** of uniformly bit-packed payloads (every `PackOrder`, 1..16 bits)
//!   straight from the file bytes, so uncompressed RAWs never touch the CPU;
//! * **normalise** (per-position black pattern, row/column deltas, white scaling);
//! * **develop**: one fused kernel for linearisation LUT → normalisation → bilinear /
//!   3x3 same-colour demosaic (or 2x2 superpixel) → white balance → clip → colour
//!   matrix → sRGB + EXIF orientation → RGBA8 (or linear f32 RGB).
//!
//! Results are GPU-resident [`GpuImage`]s that can be bound by the application
//! ([`GpuImage::buffer`], [`GpuImage::to_texture`]) or downloaded.
//!
//! Frames of any size are supported: when a frame exceeds the adapter's storage-binding
//! or buffer limits it is processed in pipelined horizontal bands, and outputs larger
//! than one buffer become tiled [`GpuImage`]s ([`GpuImage::tiles`]).
//!
//! Every kernel has a CPU scalar reference (`flashraw::process`,
//! `flashraw_codecs::packed`) and equivalence tests (`tests/equivalence.rs`).
//! WGSL sources live in this crate's `shaders/` directory.
//!
//! ```no_run
//! # fn main() -> flashraw_core::Result<()> {
//! use flashraw_gpu::{GpuFrame, GpuOp};
//! let ctx = flashraw_gpu::context()?;
//! println!("{}", ctx.info());
//! # let (data, info) = (vec![0u16; 16], flashraw_core::FrameInfo::new(4, 4, 12));
//! let run = ctx.run_blocking(&GpuFrame::from_u16(&data, 4, 4, 1), &info, &GpuOp::Normalize)?;
//! let (pixels, _ms) = run.image.download()?;
//! # Ok(()) }
//! ```

#![deny(unsafe_code)]

mod context;
mod image;
mod kernels;
mod oneshot;
pub mod policy;
mod run;

pub use context::{
    AdapterClass, GpuAdapterInfo, GpuContext, adapter_info, available, context, init, set_context, try_context,
};
pub use image::{GpuImage, GpuPixelFormat, GpuTile, HostPixels};
pub use kernels::{reference, srgb16_table, wgsl_sources};
pub use run::{DevelopOutput, DevelopSpec, GpuCurve, GpuFrame, GpuInput, GpuOp, GpuRun, GpuTimings};
/// Re-export of the wgpu version used, for interop.
pub use wgpu;

/// Block on a future (native only; used by the synchronous `flashraw` API).
#[cfg(not(target_arch = "wasm32"))]
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    pollster::block_on(f)
}
