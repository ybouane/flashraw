//! Adapter/device management: lazy, cached, async initialisation (with a blocking
//! wrapper on native), adapter classification, pipeline cache and small helpers
//! shared by the kernels.

use crate::kernels;
use crate::oneshot;
use flashraw_core::{Error, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Coarse adapter class used by backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterClass {
    Discrete,
    /// Integrated / unified-memory GPU (e.g. Apple Silicon).
    Integrated,
    Virtual,
    /// Software rasteriser (llvmpipe, lavapipe, WARP, SwiftShader). Never auto-selected.
    Software,
    /// The platform does not say (common on WebGPU, where adapter info is redacted).
    Unknown,
}

impl AdapterClass {
    pub fn name(self) -> &'static str {
        match self {
            AdapterClass::Discrete => "discrete",
            AdapterClass::Integrated => "integrated",
            AdapterClass::Virtual => "virtual",
            AdapterClass::Software => "software",
            AdapterClass::Unknown => "unknown",
        }
    }
}

/// Description of the adapter in use.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuAdapterInfo {
    pub name: String,
    pub backend: String,
    pub driver: String,
    pub class: AdapterClass,
    /// GPU timestamp queries are available (kernel times are measured on the GPU).
    pub timestamps: bool,
    pub max_buffer_size: u64,
    pub max_storage_binding: u64,
}

impl std::fmt::Display for GpuAdapterInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}, {})", self.name, self.backend, self.class.name())
    }
}

/// Kernel entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kernel {
    Unpack,
    Normalize,
    Develop,
    DevelopHalf,
}

/// A specialised pipeline: kernel + override constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PipelineKey {
    pub kernel: Kernel,
    pub input_mode: u32,
    pub order: u32,
    pub use_lut: bool,
    /// Develop only.
    pub layout: u32,
    pub out_mode: u32,
}

/// A device plus everything FlashRaw needs to run its kernels on it.
///
/// Create one with [`GpuContext::new`] (default adapter), or wrap an application's
/// existing device with [`GpuContext::from_device`] so that FlashRaw outputs live on
/// the same device as the rest of the app (required for zero-copy interop).
pub struct GpuContext {
    pub(crate) adapter: Option<wgpu::Adapter>,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) info: GpuAdapterInfo,
    pub(crate) layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    pub(crate) tex_layout: wgpu::BindGroupLayout,
    tex_pipeline: OnceLock<wgpu::ComputePipeline>,
    modules: Mutex<HashMap<Kernel, wgpu::ShaderModule>>,
    pipelines: Mutex<HashMap<PipelineKey, wgpu::ComputePipeline>>,
    pub(crate) srgb: Vec<u32>,
    pub(crate) last_error: Arc<Mutex<Option<String>>>,
    pub(crate) calibration: OnceLock<crate::policy::GpuCalibration>,
    /// Storage buffers are host-mappable (unified memory): no staging copies.
    pub(crate) uma: bool,
    /// Recycled buffers (large allocations are expensive: page mapping + zero-fill).
    pool: Mutex<Vec<wgpu::Buffer>>,
    /// Caps below the adapter limits (0 = none), see [`GpuContext::override_limits`].
    binding_cap: AtomicU64,
    buffer_cap: AtomicU64,
}

/// Upper bound of memory kept in the buffer pool.
const POOL_MAX_BYTES: u64 = 1 << 30;

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext").field("adapter", &self.info).finish()
    }
}

fn classify(t: wgpu::DeviceType) -> AdapterClass {
    match t {
        wgpu::DeviceType::DiscreteGpu => AdapterClass::Discrete,
        wgpu::DeviceType::IntegratedGpu => AdapterClass::Integrated,
        wgpu::DeviceType::VirtualGpu => AdapterClass::Virtual,
        wgpu::DeviceType::Cpu => AdapterClass::Software,
        wgpu::DeviceType::Other => AdapterClass::Unknown,
    }
}

fn is_software_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["llvmpipe", "lavapipe", "softpipe", "swiftshader", "microsoft basic render", "warp"].iter().any(|s| n.contains(s))
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

impl GpuContext {
    /// Request the default high-performance adapter and create a device.
    pub async fn new() -> Result<Self> {
        if disabled_by_env() {
            return Err(Error::GpuUnavailable("disabled by `FLASHRAW_GPU`".into()));
        }
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| Error::GpuUnavailable(format!("no GPU adapter: {e}")))?;
        let mut features = wgpu::Features::empty();
        if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            features |= wgpu::Features::TIMESTAMP_QUERY;
        }
        // Unified memory (Apple Silicon, most integrated GPUs): map storage buffers
        // directly instead of staging through extra copies.
        let integrated = adapter.get_info().device_type == wgpu::DeviceType::IntegratedGpu;
        if integrated && adapter.features().contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS) {
            features |= wgpu::Features::MAPPABLE_PRIMARY_BUFFERS;
        }
        // Large frames (60 MP RGBA8 is 240 MB) need more than the WebGPU default limits.
        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("flashraw"),
                required_features: features,
                required_limits: limits,
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| Error::GpuUnavailable(format!("device request failed: {e}")))?;
        let info = adapter.get_info();
        Ok(Self::build(Some(adapter), device, queue, &info))
    }

    /// Use an application-owned device (e.g. the one a wgpu/WebGPU renderer already
    /// uses) so that GPU-resident outputs can be consumed without copies.
    /// `TIMESTAMP_QUERY` is used when the device was created with it.
    pub fn from_device(device: wgpu::Device, queue: wgpu::Queue, info: &wgpu::AdapterInfo) -> Self {
        Self::build(None, device, queue, info)
    }

    fn build(adapter: Option<wgpu::Adapter>, device: wgpu::Device, queue: wgpu::Queue, ai: &wgpu::AdapterInfo) -> Self {
        let mut class = classify(ai.device_type);
        if is_software_name(&ai.name) {
            class = AdapterClass::Software;
        }
        let limits = device.limits();
        let uma = device.features().contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS);
        let info = GpuAdapterInfo {
            name: if ai.name.is_empty() { "WebGPU adapter".into() } else { ai.name.clone() },
            backend: format!("{:?}", ai.backend),
            driver: [ai.driver.as_str(), ai.driver_info.as_str()]
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
            class,
            timestamps: device.features().contains(wgpu::Features::TIMESTAMP_QUERY),
            max_buffer_size: limits.max_buffer_size,
            max_storage_binding: limits.max_storage_buffer_binding_size,
        };
        let last_error: Arc<Mutex<Option<String>>> = Arc::default();
        {
            // Library code must not panic on device errors (wgpu's default handler does):
            // record them; operations check error scopes and this slot.
            let slot = last_error.clone();
            device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
                if let Ok(mut s) = slot.lock() {
                    *s = Some(e.to_string());
                }
            }));
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flashraw.kernels"),
            entries: &[
                uniform_entry(0),
                storage_entry(1, true),
                storage_entry(2, true),
                storage_entry(3, true),
                storage_entry(4, false),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flashraw.kernels"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flashraw.to_texture"),
            entries: &[
                uniform_entry(0),
                storage_entry(1, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        GpuContext {
            adapter,
            device,
            queue,
            info,
            layout,
            pipeline_layout,
            tex_layout,
            tex_pipeline: OnceLock::new(),
            modules: Mutex::default(),
            pipelines: Mutex::default(),
            srgb: kernels::srgb_table(),
            last_error,
            calibration: OnceLock::new(),
            uma,
            pool: Mutex::default(),
            binding_cap: AtomicU64::new(0),
            buffer_cap: AtomicU64::new(0),
        }
    }

    /// A buffer of at least `size` bytes with exactly `usage`, recycled when possible.
    /// Large sizes are rounded up so that frames of similar size share buffers.
    pub(crate) fn acquire(&self, label: &str, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
        let size = size.max(4);
        {
            let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
            let best = pool
                .iter()
                .enumerate()
                .filter(|(_, b)| b.usage() == usage && b.size() >= size && b.size() <= size + size / 4 + (16 << 20))
                .min_by_key(|(_, b)| b.size())
                .map(|(i, _)| i);
            if let Some(i) = best {
                return pool.swap_remove(i);
            }
        }
        let granule: u64 = if size > (64 << 20) { 4 << 20 } else { 256 };
        // Never round past the adapter's buffer size limit.
        let rounded = (size.div_ceil(granule) * granule).min(self.info.max_buffer_size & !3).max(size);
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: rounded,
            usage,
            mapped_at_creation: false,
        })
    }

    /// Return a buffer to the pool (the caller guarantees no one else uses it any more).
    pub(crate) fn release(&self, b: wgpu::Buffer) {
        if b.size() < (1 << 20) {
            return; // small buffers are cheap to recreate
        }
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        pool.push(b);
        let mut total: u64 = pool.iter().map(|b| b.size()).sum();
        while total > POOL_MAX_BYTES && !pool.is_empty() {
            let old = pool.remove(0);
            total -= old.size();
            old.destroy();
        }
    }

    /// Free all pooled buffers.
    pub fn trim(&self) {
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        for b in pool.drain(..) {
            b.destroy();
        }
    }

    /// True when storage buffers are mapped directly (unified memory).
    pub fn unified_memory(&self) -> bool {
        self.uma
    }

    pub fn info(&self) -> &GpuAdapterInfo {
        &self.info
    }
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
    /// The adapter, when this context created its own device.
    pub fn adapter(&self) -> Option<&wgpu::Adapter> {
        self.adapter.as_ref()
    }

    fn module(&self, k: Kernel) -> wgpu::ShaderModule {
        let mut m = self.modules.lock().unwrap_or_else(|e| e.into_inner());
        m.entry(k)
            .or_insert_with(|| {
                let (label, src) = match k {
                    Kernel::Unpack => ("flashraw.unpack", kernels::UNPACK_WGSL),
                    Kernel::Normalize => ("flashraw.normalize", kernels::NORMALIZE_WGSL),
                    Kernel::Develop | Kernel::DevelopHalf => ("flashraw.develop", kernels::DEVELOP_WGSL),
                };
                self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(label),
                    source: wgpu::ShaderSource::Wgsl(src.into()),
                })
            })
            .clone()
    }

    /// A pipeline specialised with override constants (compiled once, cached).
    pub(crate) fn pipeline(&self, key: PipelineKey) -> wgpu::ComputePipeline {
        if let Some(p) = self.pipelines.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return p.clone();
        }
        let module = self.module(key.kernel);
        let entry = match key.kernel {
            Kernel::Unpack => "unpack",
            Kernel::Normalize => "normalize",
            Kernel::Develop => "develop",
            Kernel::DevelopHalf => "develop_half",
        };
        let mut constants: Vec<(&str, f64)> = vec![
            ("INPUT_MODE", key.input_mode as f64),
            ("ORDER", key.order as f64),
            ("USE_LUT", if key.use_lut { 1.0 } else { 0.0 }),
        ];
        if matches!(key.kernel, Kernel::Develop | Kernel::DevelopHalf) {
            constants.push(("LAYOUT", key.layout as f64));
            constants.push(("OUT_MODE", key.out_mode as f64));
        }
        let p = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&self.pipeline_layout),
            module: &module,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &constants,
                // Every workgroup-memory element is written before it is read.
                zero_initialize_workgroup_memory: false,
            },
            cache: None,
        });
        self.pipelines.lock().unwrap_or_else(|e| e.into_inner()).insert(key, p.clone());
        p
    }

    pub(crate) fn tex_pipeline(&self) -> &wgpu::ComputePipeline {
        self.tex_pipeline.get_or_init(|| {
            let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("flashraw.to_texture"),
                source: wgpu::ShaderSource::Wgsl(kernels::TO_TEXTURE_WGSL.into()),
            });
            let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("flashraw.to_texture"),
                bind_group_layouts: &[Some(&self.tex_layout)],
                immediate_size: 0,
            });
            self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("to_texture"),
                layout: Some(&layout),
                module: &module,
                entry_point: Some("to_texture"),
                compilation_options: Default::default(),
                cache: None,
            })
        })
    }

    /// Wait until all submitted work has completed. On native this blocks in
    /// `Device::poll`; in the browser it yields to the event loop.
    pub async fn wait_idle(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.queue.on_submitted_work_done(move || tx.send(()));
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.device
                .poll(wgpu::PollType::wait_indefinitely())
                .map_err(|e| Error::GpuUnavailable(format!("device poll failed: {e}")))?;
            if rx.try_take().is_some() {
                return Ok(());
            }
        }
        rx.await.ok_or_else(|| Error::GpuUnavailable("GPU work did not complete (device lost?)".into()))
    }

    /// Map `buffer[..size]` for reading and copy it out with `f`.
    pub(crate) async fn read_mapped<T>(
        &self,
        buffer: &wgpu::Buffer,
        size: u64,
        f: impl FnOnce(&[u8]) -> T,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        buffer.map_async(wgpu::MapMode::Read, ..size, move |r| tx.send(r));
        #[cfg(not(target_arch = "wasm32"))]
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| Error::GpuUnavailable(format!("device poll failed: {e}")))?;
        let r = match rx.try_take() {
            Some(r) => Some(r),
            None => rx.await,
        };
        match r {
            Some(Ok(())) => {}
            Some(Err(e)) => return Err(Error::GpuUnavailable(format!("buffer mapping failed: {e}"))),
            None => return Err(Error::GpuUnavailable("buffer mapping was cancelled".into())),
        }
        let out = {
            let view = buffer
                .get_mapped_range(..size)
                .map_err(|e| Error::GpuUnavailable(format!("mapped range unavailable: {e}")))?;
            f(&view)
        };
        buffer.unmap();
        Ok(out)
    }

    /// Map `buffer[..size]` for writing and fill it with `data` (zero-padded).
    ///
    /// `after` is the last submission that used the buffer (`None`: idle). Only that
    /// submission is waited for, so later work (e.g. the previous band's kernel) keeps
    /// running while the host fills the buffer. Returns the time spent copying (ms),
    /// excluding the wait for the buffer.
    pub(crate) async fn write_mapped(
        &self,
        buffer: &wgpu::Buffer,
        data: &[u8],
        after: Option<wgpu::SubmissionIndex>,
    ) -> Result<f64> {
        let size = (data.len() as u64).div_ceil(4).max(1) * 4;
        let (tx, rx) = oneshot::channel();
        buffer.map_async(wgpu::MapMode::Write, ..size, move |r| tx.send(r));
        #[cfg(not(target_arch = "wasm32"))]
        {
            let poll = match after {
                Some(i) => wgpu::PollType::Wait { submission_index: Some(i), timeout: None },
                None => wgpu::PollType::Poll,
            };
            self.device.poll(poll).map_err(|e| Error::GpuUnavailable(format!("device poll failed: {e}")))?;
            if !rx.is_ready() {
                self.device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .map_err(|e| Error::GpuUnavailable(format!("device poll failed: {e}")))?;
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = after;
        let r = match rx.try_take() {
            Some(r) => Some(r),
            None => rx.await,
        };
        match r {
            Some(Ok(())) => {}
            Some(Err(e)) => return Err(Error::GpuUnavailable(format!("buffer mapping failed: {e}"))),
            None => return Err(Error::GpuUnavailable("buffer mapping was cancelled".into())),
        }
        let t0 = flashraw_core::profile::now_ms();
        {
            let mut view = buffer
                .get_mapped_range_mut(..size)
                .map_err(|e| Error::GpuUnavailable(format!("mapped range unavailable: {e}")))?;
            let bulk = data.len();
            view.slice(..bulk).copy_from_slice(data);
            if (bulk as u64) < size {
                view.slice(bulk..).fill(0);
            }
        }
        buffer.unmap();
        Ok(flashraw_core::profile::now_ms() - t0)
    }

    /// Take the last uncaptured device error, if any.
    pub(crate) fn take_error(&self) -> Option<String> {
        self.last_error.lock().ok()?.take()
    }

    /// Cap the storage-binding size and/or the buffer size FlashRaw plans with, below
    /// the adapter's limits (`None` restores the adapter limit). Frames are processed
    /// in bands that fit these caps, so this bounds the size of every buffer FlashRaw
    /// binds (useful to limit peak device memory, and to exercise tiling in tests).
    /// Values are rounded down to a multiple of 256 bytes, with a 4 KB minimum.
    pub fn override_limits(&self, max_binding: Option<u64>, max_buffer: Option<u64>) {
        let v = |o: Option<u64>| o.map(|b| (b & !255).max(4096)).unwrap_or(0);
        self.binding_cap.store(v(max_binding), Ordering::Relaxed);
        self.buffer_cap.store(v(max_buffer), Ordering::Relaxed);
    }

    /// Largest storage binding FlashRaw uses (adapter limit, caps, 32-bit addressing).
    pub fn max_binding_bytes(&self) -> u64 {
        let mut b = self.info.max_storage_binding.min(self.info.max_buffer_size).min(u32::MAX as u64 & !255);
        let cap = self.binding_cap.load(Ordering::Relaxed);
        if cap > 0 {
            b = b.min(cap);
        }
        b.min(self.max_buffer_bytes())
    }

    /// Largest single buffer FlashRaw allocates (adapter limit and cap).
    pub fn max_buffer_bytes(&self) -> u64 {
        let cap = self.buffer_cap.load(Ordering::Relaxed);
        if cap > 0 { self.info.max_buffer_size.min(cap) } else { self.info.max_buffer_size }
    }

    /// Number of bands a frame with `input_bytes` of upload and `output_bytes` of
    /// result needs on this device (an estimate for cost models; 1 = untiled).
    pub fn estimate_bands(&self, input_bytes: u64, output_bytes: u64) -> u32 {
        let b = self.max_binding_bytes().max(1);
        let out = if output_bytes <= b { 1 } else { output_bytes.div_ceil(b) };
        input_bytes.div_ceil(b).max(out).max(1).min(u32::MAX as u64) as u32
    }
}

/// `FLASHRAW_GPU=0|off|false|none` disables the GPU backend.
fn disabled_by_env() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        matches!(
            std::env::var("FLASHRAW_GPU").map(|v| v.to_ascii_lowercase()).as_deref(),
            Ok("0" | "off" | "false" | "none" | "cpu")
        )
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

// ---- Global context -------------------------------------------------------------

#[derive(Default)]
struct Slot {
    ctx: Option<Arc<GpuContext>>,
    error: Option<String>,
    tried: bool,
}

#[cfg(not(target_arch = "wasm32"))]
fn with_slot<T>(f: impl FnOnce(&mut Slot) -> T) -> T {
    static SLOT: OnceLock<Mutex<Slot>> = OnceLock::new();
    let mut g = SLOT.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

#[cfg(target_arch = "wasm32")]
fn with_slot<T>(f: impl FnOnce(&mut Slot) -> T) -> T {
    // wgpu's WebGPU objects are not Send/Sync; the browser main thread owns them.
    thread_local! { static SLOT: std::cell::RefCell<Slot> = Default::default(); }
    SLOT.with(|s| f(&mut s.borrow_mut()))
}

/// Initialise the global GPU context (idempotent). Required before any GPU work in
/// the browser, where initialisation cannot block.
pub async fn init() -> Result<Arc<GpuContext>> {
    if let Some(r) = with_slot(|s| s.tried.then(|| s.ctx.clone().ok_or_else(|| s.error.clone().unwrap_or_default()))) {
        return r.map_err(Error::GpuUnavailable);
    }
    let r = GpuContext::new().await.map(Arc::new);
    with_slot(|s| {
        if let Some(c) = &s.ctx {
            return Ok(c.clone()); // initialised concurrently
        }
        s.tried = true;
        match r {
            Ok(c) => {
                s.ctx = Some(c.clone());
                Ok(c)
            }
            Err(e) => {
                s.error = Some(e.to_string());
                Err(e)
            }
        }
    })
}

/// The global GPU context, initialising it on first use (native: blocking).
/// In the browser this only returns a context after [`init`] has completed.
pub fn context() -> Result<Arc<GpuContext>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(r) =
            with_slot(|s| s.tried.then(|| s.ctx.clone().ok_or_else(|| s.error.clone().unwrap_or_default())))
        {
            return r.map_err(Error::GpuUnavailable);
        }
        pollster::block_on(init())
    }
    #[cfg(target_arch = "wasm32")]
    {
        with_slot(|s| s.ctx.clone())
            .ok_or_else(|| Error::GpuUnavailable("call `flashraw_gpu::init().await` before using the GPU".into()))
    }
}

/// The global context if it has already been initialised (never initialises).
pub fn try_context() -> Option<Arc<GpuContext>> {
    with_slot(|s| s.ctx.clone())
}

/// Install an application-provided context (see [`GpuContext::from_device`]) as the
/// global one used by FlashRaw.
pub fn set_context(ctx: Arc<GpuContext>) {
    with_slot(|s| {
        s.ctx = Some(ctx);
        s.tried = true;
        s.error = None;
    })
}

/// Whether a GPU adapter is available (initialises the context on native).
pub fn available() -> bool {
    context().is_ok()
}

/// Adapter description of the global context.
pub fn adapter_info() -> Option<GpuAdapterInfo> {
    context().ok().map(|c| c.info.clone())
}
