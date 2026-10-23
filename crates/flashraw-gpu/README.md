# flashraw-gpu

The optional [wgpu](https://wgpu.rs) compute backend of [FlashRaw](https://github.com/ybouane/flashraw):
the post-decode pipeline (unpack, normalise, demosaic, colour conversion, encode) as WGSL compute shaders
for Vulkan, Metal, DX12 and WebGPU. The WGSL sources are in `shaders/` inside this crate.

Do not depend on this crate directly: enable the `gpu` feature of [`flashraw`](https://crates.io/crates/flashraw)
and select `Backend::Gpu` or `Backend::Auto`. It has no semver guarantee of its own. Output is within
rounding of the CPU path, not bit-identical. See [docs/gpu.md](https://github.com/ybouane/flashraw/blob/main/docs/gpu.md).

* Documentation: <https://docs.rs/flashraw-gpu>
* Licence: MIT OR Apache-2.0
