//! WebAssembly bindings for FlashRaw (consumed by the `flashraw` npm package in `npm/`).
//!
//! The raw wasm-bindgen surface is deliberately small and "pointer-shaped": the npm package
//! (`npm/src/core.js`) wraps it into the public JS API. Design:
//!
//! - **Input**: JS allocates an [`InputBuffer`] inside WASM memory and writes the file into it
//!   (one copy, or zero intermediate copies when streaming a `File`/fd). [`open`] takes
//!   ownership; the decoder borrows from it for the lifetime of the [`RawFile`].
//! - **Metadata**: one JSON string per call, parsed once on the JS side.
//! - **Output**: a [`Frame`] keeps the pixels in WASM memory and exposes `ptr`/`len`; JS
//!   either copies them out (default; then frees the frame) or hands out a view.
//! - **Errors**: JS `Error` objects with a `kind` property (`flashraw::Error::kind`).
//!
//! Binary variants (see `docs/wasm.md`):
//!
//! - default: single-threaded (SIMD128 and scalar builds);
//! - feature `threads`: shared memory + rayon. JS starts the rayon workers in Web Workers /
//!   `worker_threads` ([`thread_pool_build`] + [`thread_run`]) and runs every call that may use
//!   the pool on a *coordinator* worker through the pointer-based `job*` exports, because the
//!   browser main thread is not allowed to block (`memory.atomic.wait` traps there);
//! - feature `jpegxl`: JPEG XL DNG support, combined with the SIMD and threaded variants;
//! - feature `gpu`: WebGPU backend through wgpu, with asynchronous exports (`gpuInit`,
//!   `RawFile.decodeGpu`) and GPU-resident outputs ([`GpuOutput`]).
//!
//! See `docs/wasm.md` for the memory model.

// The WebGPU build stays single-threaded: decoding would run on the coordinator worker, but
// WebGPU objects cannot move between workers, so GPU-resident outputs would be unusable on the
// thread that asked for them (and `flashraw`'s pool `install` needs `Send` frames).
#[cfg(all(feature = "gpu", feature = "threads", target_arch = "wasm32"))]
compile_error!("the `gpu` and `threads` features are separate WASM variants and cannot be combined");

pub mod api;
pub mod json;

#[cfg(target_arch = "wasm32")]
mod bindings {
    use crate::api::{self, ApiError, Decoded};
    use crate::json;
    use flashraw::FlashRaw;
    use std::sync::{Arc, Mutex};
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = performance, js_name = now)]
        fn performance_now() -> f64;
    }

    static LAST_PANIC: Mutex<Option<String>> = Mutex::new(None);

    /// Runs once when the module is instantiated.
    #[wasm_bindgen(start)]
    fn start() {
        // `Instant` is unavailable on wasm32-unknown-unknown; stage timings use performance.now().
        flashraw::low_level::core::profile::set_clock(performance_now);
        // With panic=abort a panic traps; keep the message so JS can attach it to the error.
        std::panic::set_hook(Box::new(|info| {
            if let Ok(mut p) = LAST_PANIC.lock() {
                *p = Some(info.to_string());
            }
        }));
    }

    fn to_js(e: ApiError) -> JsValue {
        let err = js_sys::Error::new(&e.message);
        let _ = js_sys::Reflect::set(&err, &"kind".into(), &e.kind.into());
        if let Some(c) = &e.corrupt {
            let c = js_sys::JSON::parse(&c.to_string()).unwrap_or(JsValue::NULL);
            let _ = js_sys::Reflect::set(&err, &"corrupt".into(), &c);
        }
        err.into()
    }

    /// Message of the last Rust panic (a trap surfaces in JS as `RuntimeError: unreachable`).
    #[wasm_bindgen(js_name = takeLastPanic)]
    pub fn take_last_panic() -> Option<String> {
        LAST_PANIC.lock().ok().and_then(|mut p| p.take())
    }

    /// Library version.
    #[wasm_bindgen]
    pub fn version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// Number of rayon worker threads started (0 for single-threaded builds or before
    /// `threadPoolBuild`).
    fn thread_count() -> usize {
        #[cfg(feature = "threads")]
        {
            threads::THREADS.load(std::sync::atomic::Ordering::Acquire)
        }
        #[cfg(not(feature = "threads"))]
        {
            0
        }
    }

    /// Build features of this binary as JSON (`{ simd, gpu, threads, jpegxl }`).
    #[wasm_bindgen(js_name = buildInfo)]
    pub fn build_info() -> String {
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "simd": cfg!(target_feature = "simd128"),
            "simdLevel": flashraw::low_level::core::simd::detect().name(),
            "gpu": cfg!(feature = "gpu"),
            "gpuAvailable": flashraw::gpu_available(),
            "threads": cfg!(feature = "threads"),
            "jpegxl": cfg!(feature = "jpegxl"),
            "threadCount": thread_count(),
            "maxSamples": api::DEFAULT_MAX_SAMPLES,
        })
        .to_string()
    }

    /// Cheap format detection: the format name (`"DNG"`, `"CR3"`, ...) or `undefined`.
    #[wasm_bindgen]
    pub fn detect(bytes: &[u8]) -> Option<String> {
        api::detect(bytes).map(str::to_string)
    }

    /// A byte buffer allocated inside WASM memory, filled by JS, then consumed by [`open`].
    ///
    /// The bytes start uninitialised: the JS layer writes all of them (or frees the buffer)
    /// before [`open`] reads it, so zero-filling would only cost a pass over the whole input.
    #[wasm_bindgen]
    pub struct InputBuffer {
        buf: Vec<std::mem::MaybeUninit<u8>>,
    }

    impl InputBuffer {
        /// The filled bytes.
        fn into_bytes(self) -> Vec<u8> {
            let mut v = std::mem::ManuallyDrop::new(self.buf);
            // SAFETY: same allocation, length and capacity, and `MaybeUninit<u8>` has the layout
            // of `u8`; the JS layer has written every byte before handing the buffer to `open`
            // (`copyIn` / `streamIn` / `fileIn` in npm/src/core.js free it otherwise).
            unsafe { Vec::from_raw_parts(v.as_mut_ptr().cast::<u8>(), v.len(), v.capacity()) }
        }
    }

    #[wasm_bindgen]
    impl InputBuffer {
        /// Allocate `len` bytes (fails with `OutOfMemory` rather than aborting).
        #[wasm_bindgen(constructor)]
        pub fn new(len: usize) -> Result<InputBuffer, JsValue> {
            let mut buf = Vec::new();
            buf.try_reserve_exact(len).map_err(|_| {
                to_js(ApiError {
                    corrupt: None,
                    kind: "OutOfMemory",
                    message: format!("cannot allocate {len} bytes for the input"),
                })
            })?;
            // SAFETY: capacity is at least `len`, and `MaybeUninit` needs no initialisation.
            unsafe { buf.set_len(len) };
            Ok(InputBuffer { buf })
        }
        /// Address of the buffer in WASM memory.
        #[wasm_bindgen(getter)]
        pub fn ptr(&self) -> usize {
            self.buf.as_ptr() as usize
        }
        #[wasm_bindgen(getter)]
        pub fn len(&self) -> usize {
            self.buf.len()
        }
        /// Shrink to `len` bytes (when a stream turned out shorter than announced).
        pub fn truncate(&mut self, len: usize) {
            self.buf.truncate(len);
        }
    }

    /// Open a RAW file, taking ownership of `input` (the JS `InputBuffer` becomes unusable).
    #[wasm_bindgen]
    pub fn open(input: InputBuffer, options_json: &str) -> Result<RawFile, JsValue> {
        let raw = api::open(input.into_bytes(), options_json).map_err(to_js)?;
        Ok(RawFile { raw: Arc::new(raw) })
    }

    /// An opened RAW file. Owns the input bytes; previews are views into them.
    #[wasm_bindgen]
    pub struct RawFile {
        // Shared so that asynchronous (GPU) decodes and coordinator jobs (threads) can hold
        // the file independently of the JS object's borrow.
        raw: Arc<FlashRaw<'static>>,
    }

    #[wasm_bindgen]
    impl RawFile {
        #[wasm_bindgen(js_name = metadataJson)]
        pub fn metadata_json(&self) -> String {
            json::metadata(&self.raw).to_string()
        }
        #[wasm_bindgen(js_name = frameInfoJson)]
        pub fn frame_info_json(&self) -> String {
            json::frame_info(self.raw.frame_info()).to_string()
        }
        #[wasm_bindgen(js_name = capabilitiesJson)]
        pub fn capabilities_json(&self) -> String {
            json::capabilities(&self.raw).to_string()
        }
        #[wasm_bindgen(js_name = unitsJson)]
        pub fn units_json(&self) -> String {
            json::units(&self.raw.units()).to_string()
        }
        #[wasm_bindgen(js_name = parseMs)]
        pub fn parse_ms(&self) -> f64 {
            self.raw.parse_ms()
        }
        /// Address and length of the input bytes in WASM memory.
        #[wasm_bindgen(js_name = inputPtr)]
        pub fn input_ptr(&self) -> usize {
            self.raw.bytes().as_ptr() as usize
        }
        #[wasm_bindgen(js_name = inputLen)]
        pub fn input_len(&self) -> usize {
            self.raw.bytes().len()
        }
        /// `[ptr, len]` of an embedded preview (`"largest"`, `"smallest"` or an index) plus its
        /// JSON description, as `[ptr, len, json]`.
        #[wasm_bindgen(js_name = previewSpan)]
        pub fn preview_span(&self, which: &str) -> Result<js_sys::Array, JsValue> {
            let (off, len, info) = api::preview_span(&self.raw, which).map_err(to_js)?;
            let ptr = self.input_ptr() + off;
            Ok(js_sys::Array::of3(&(ptr as f64).into(), &(len as f64).into(), &info.into()))
        }
        /// Decode a preview to RGBA8.
        #[wasm_bindgen(js_name = decodePreview)]
        pub fn decode_preview(&self, which: &str) -> Result<Frame, JsValue> {
            api::decode_preview_rgba(&self.raw, which).map(Frame::from).map_err(to_js)
        }
        /// Decode the sensor data. `options_json`: see `npm/src/types.d.ts` (`DecodeOptions`).
        pub fn decode(&self, options_json: &str) -> Result<Frame, JsValue> {
            api::decode(&self.raw, options_json).map(Frame::from).map_err(to_js)
        }
        #[wasm_bindgen(js_name = decodeTile)]
        pub fn decode_tile(&self, index: u32, options_json: &str) -> Result<Frame, JsValue> {
            api::decode_tile(&self.raw, index, options_json).map(Frame::from).map_err(to_js)
        }
    }

    /// Decoded pixels living in WASM memory.
    #[wasm_bindgen]
    pub struct Frame {
        inner: Decoded,
    }

    impl From<Decoded> for Frame {
        fn from(inner: Decoded) -> Self {
            Frame { inner }
        }
    }

    #[wasm_bindgen]
    impl Frame {
        /// Address of the first element (aligned for the element type).
        #[wasm_bindgen(getter)]
        pub fn ptr(&self) -> usize {
            self.inner.pixels.ptr()
        }
        /// Number of elements (not bytes).
        #[wasm_bindgen(getter)]
        pub fn len(&self) -> usize {
            self.inner.pixels.len()
        }
        /// Element type: `"u8"`, `"u16"` or `"f32"`.
        #[wasm_bindgen(getter)]
        pub fn kind(&self) -> String {
            self.inner.pixels.kind().to_string()
        }
        #[wasm_bindgen(js_name = infoJson)]
        pub fn info_json(&self) -> String {
            self.inner.info.clone()
        }
    }

    // ---- Threads (feature `threads`) -----------------------------------------------------------
    //
    // Protocol (npm/src/threads.js):
    //  1. main thread: instantiate with a shared `WebAssembly.Memory`, start the coordinator
    //     worker on the same module + memory;
    //  2. coordinator: `threadPoolBuild(n, onSpawn)`; rayon hands out one `ThreadBuilder` per
    //     thread (forwarded to the main thread as a pointer), then blocks until all threads run;
    //  3. main thread: one worker per pointer, each calling `threadRun(ptr)`, which never
    //     returns (the worker *is* the rayon thread);
    //  4. the coordinator then runs the `job*` exports: blocking is allowed there, so rayon's
    //     `join`/`par_iter` from a non-pool thread works. The main thread only creates/frees
    //     buffers and reads results (the browser main thread must never block).
    // Objects cross threads as raw `Box` pointers: the JS layer guarantees each pointer is
    // consumed exactly once and that shared handles outlive the jobs using them.

    #[cfg(feature = "threads")]
    mod threads {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        pub(super) static THREADS: AtomicUsize = AtomicUsize::new(0);

        fn internal(message: String) -> JsValue {
            to_js(ApiError { corrupt: None, kind: "Internal", message })
        }

        /// Configure rayon's global pool with `n` threads. For each thread, `on_spawn(ptr)` is
        /// called with an opaque pointer that a new worker must pass to [`thread_run`]. Blocks
        /// until every thread has started (rayon waits for its pool to be primed), so it must
        /// run on a worker (the coordinator), never on the browser main thread. Once per module.
        #[wasm_bindgen(js_name = threadPoolBuild)]
        pub fn thread_pool_build(n: usize, on_spawn: &js_sys::Function) -> Result<usize, JsValue> {
            if n == 0 {
                return Err(to_js(ApiError::invalid("thread count must be at least 1")));
            }
            rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .spawn_handler(|t| {
                    let ptr = Box::into_raw(Box::new(t)) as usize as u32;
                    on_spawn
                        .call1(&JsValue::NULL, &JsValue::from(ptr))
                        .map(|_| ())
                        .map_err(|_| std::io::Error::other("the thread spawn callback failed"))
                })
                .build_global()
                .map_err(|e| internal(format!("cannot start the thread pool: {e}")))?;
            THREADS.store(n, Ordering::Release);
            Ok(n)
        }

        /// Body of a rayon worker thread (never returns while the pool lives).
        #[wasm_bindgen(js_name = threadRun)]
        pub fn thread_run(ptr: u32) {
            // SAFETY: `ptr` was produced by `Box::into_raw` in `thread_pool_build` and the JS
            // layer hands every pointer to exactly one worker.
            let t = unsafe { Box::from_raw(ptr as usize as *mut rayon::ThreadBuilder) };
            t.run();
        }

        #[wasm_bindgen]
        impl InputBuffer {
            /// Give up the JS object; returns a pointer for [`job_open`].
            #[wasm_bindgen(js_name = intoRaw)]
            pub fn into_raw(self) -> usize {
                Box::into_raw(Box::new(self)) as usize
            }
        }

        #[wasm_bindgen]
        impl RawFile {
            /// A new strong reference to the file for coordinator jobs (release with
            /// [`release_shared`]).
            pub fn share(&self) -> usize {
                Box::into_raw(Box::new(self.raw.clone())) as usize
            }
        }

        /// SAFETY contract (all `job*`/`*FromRaw` functions): pointers come from the matching
        /// `into_raw`/`share`/`job*` call and are used as documented by the JS layer.
        fn shared<'a>(ptr: usize) -> &'a Arc<FlashRaw<'static>> {
            // SAFETY: see above; the JS layer keeps the box alive until `releaseShared`.
            unsafe { &*(ptr as *const Arc<FlashRaw<'static>>) }
        }

        fn boxed_frame(r: Result<Decoded, ApiError>) -> Result<usize, JsValue> {
            r.map(|d| Box::into_raw(Box::new(Frame::from(d))) as usize).map_err(to_js)
        }

        #[wasm_bindgen(js_name = releaseShared)]
        pub fn release_shared(ptr: usize) {
            // SAFETY: produced by `RawFile::share`, released exactly once.
            drop(unsafe { Box::from_raw(ptr as *mut Arc<FlashRaw<'static>>) });
        }

        /// Coordinator: open (consumes the input pointer); returns a `RawFile` pointer.
        #[wasm_bindgen(js_name = jobOpen)]
        pub fn job_open(input: usize, options_json: &str) -> Result<usize, JsValue> {
            // SAFETY: produced by `InputBuffer::into_raw`, consumed exactly once.
            let input = unsafe { Box::from_raw(input as *mut InputBuffer) };
            let f = open(*input, options_json)?;
            Ok(Box::into_raw(Box::new(f)) as usize)
        }

        /// Coordinator: decode (`tile < 0` = full frame / options.region); returns a `Frame` pointer.
        #[wasm_bindgen(js_name = jobDecode)]
        pub fn job_decode(shared_ptr: usize, tile: i32, options_json: &str) -> Result<usize, JsValue> {
            let raw = shared(shared_ptr);
            boxed_frame(if tile < 0 {
                api::decode(raw, options_json)
            } else {
                api::decode_tile(raw, tile as u32, options_json)
            })
        }

        /// Coordinator: decode an embedded preview to RGBA8; returns a `Frame` pointer.
        #[wasm_bindgen(js_name = jobDecodePreview)]
        pub fn job_decode_preview(shared_ptr: usize, which: &str) -> Result<usize, JsValue> {
            boxed_frame(api::decode_preview_rgba(shared(shared_ptr), which))
        }

        /// Main thread: take ownership of a `RawFile` produced by [`job_open`].
        #[wasm_bindgen(js_name = rawFileFromRaw)]
        pub fn raw_file_from_raw(ptr: usize) -> RawFile {
            // SAFETY: produced by `job_open`, consumed exactly once.
            *unsafe { Box::from_raw(ptr as *mut RawFile) }
        }

        /// Main thread: take ownership of a `Frame` produced by a `job*` call.
        #[wasm_bindgen(js_name = frameFromRaw)]
        pub fn frame_from_raw(ptr: usize) -> Frame {
            // SAFETY: produced by `boxed_frame`, consumed exactly once.
            *unsafe { Box::from_raw(ptr as *mut Frame) }
        }
    }

    // ---- WebGPU (feature `gpu`) ----------------------------------------------------------------

    #[cfg(feature = "gpu")]
    mod gpu {
        use super::*;
        use flashraw::gpu::{GpuImage, GpuPixelFormat};
        use std::rc::Rc;
        use wasm_bindgen_futures::future_to_promise;

        fn adapter_json(info: &flashraw::gpu::GpuAdapterInfo) -> String {
            serde_json::json!({
                "name": info.name,
                "backend": info.backend,
                "driver": info.driver,
                "class": info.class.name(),
                "timestamps": info.timestamps,
                "maxBufferSize": info.max_buffer_size,
                "maxStorageBinding": info.max_storage_binding,
                "description": info.to_string(),
            })
            .to_string()
        }

        /// Request a WebGPU adapter + device (idempotent). Resolves to the adapter JSON;
        /// rejects with a `GpuUnavailable` error.
        #[wasm_bindgen(js_name = gpuInit)]
        pub async fn gpu_init() -> Result<String, JsValue> {
            let ctx = flashraw::gpu::init().await.map_err(|e| to_js(e.into()))?;
            Ok(adapter_json(ctx.info()))
        }

        /// The `GPUDevice` FlashRaw uses (after `gpuInit`), or `undefined`.
        #[wasm_bindgen(js_name = gpuDevice)]
        pub fn gpu_device() -> JsValue {
            flashraw::gpu::try_context()
                .and_then(|c| c.device().as_webgpu().map(|d| JsValue::from(d.clone())))
                .unwrap_or(JsValue::UNDEFINED)
        }

        /// Free FlashRaw's pooled GPU buffers.
        #[wasm_bindgen(js_name = gpuTrim)]
        pub fn gpu_trim() {
            if let Some(c) = flashraw::gpu::try_context() {
                c.trim();
            }
        }

        #[wasm_bindgen]
        impl RawFile {
            /// Asynchronous GPU decode (`tile < 0` = full frame / options.region). Resolves to a
            /// [`Frame`] (host outputs) or a [`GpuOutput`] (`gpu-*` outputs).
            #[wasm_bindgen(js_name = decodeGpu)]
            pub fn decode_gpu(&self, tile: i32, options_json: String) -> js_sys::Promise {
                let raw = self.raw.clone();
                future_to_promise(async move {
                    let tile = u32::try_from(tile).ok();
                    match api::decode_gpu(&raw, tile, &options_json).await.map_err(to_js)? {
                        api::GpuDecoded::Host(d) => Ok(Frame::from(d).into()),
                        api::GpuDecoded::Resident { frame, info, backend } => {
                            Ok(GpuOutput { frame: Some(Rc::from(frame)), info, backend }.into())
                        }
                    }
                })
            }
        }

        /// A decoded image resident on the GPU. Its buffer belongs to FlashRaw's pool and is
        /// recycled by `free()`, unless taken over with `detachBuffer()`.
        #[wasm_bindgen]
        pub struct GpuOutput {
            // `Rc`: WebGPU handles are not `Send`/`Sync` (the WebGPU build is single-threaded).
            frame: Option<Rc<flashraw::RawFrame>>,
            info: String,
            backend: String,
        }

        fn closed(what: &str) -> JsValue {
            to_js(ApiError { corrupt: None, kind: "Closed", message: format!("the GPU buffer was {what}") })
        }

        impl GpuOutput {
            fn image(&self) -> Result<&Arc<GpuImage>, JsValue> {
                self.frame.as_ref().and_then(|f| f.as_gpu()).ok_or_else(|| closed("detached"))
            }
        }

        fn format_name(f: GpuPixelFormat) -> &'static str {
            match f {
                GpuPixelFormat::U16 => "u16",
                GpuPixelFormat::F32 => "f32",
                GpuPixelFormat::Rgba8 => "rgba8",
            }
        }

        /// The single buffer of an untiled image. Frames larger than the adapter's binding
        /// limits are stored as bands; those are read with `download()` / `createTexture()`.
        fn webgpu_buffer(b: Option<&flashraw::gpu::wgpu::Buffer>) -> Result<JsValue, JsValue> {
            let b = b.ok_or_else(|| {
                to_js(ApiError {
                    corrupt: None,
                    kind: "NotAvailable",
                    message: "the frame exceeds the adapter's buffer limits and is stored in bands; use `download()` or `createTexture()`".into(),
                })
            })?;
            b.as_webgpu().map(|b| JsValue::from(b.clone())).ok_or_else(|| {
                to_js(ApiError { corrupt: None, kind: "GpuUnavailable", message: "not a WebGPU buffer".into() })
            })
        }

        #[wasm_bindgen]
        impl GpuOutput {
            #[wasm_bindgen(js_name = infoJson)]
            pub fn info_json(&self) -> String {
                self.info.clone()
            }
            /// The `GPUBuffer` holding the pixels (may be larger than `byteLength`).
            #[wasm_bindgen(getter)]
            pub fn buffer(&self) -> Result<JsValue, JsValue> {
                webgpu_buffer(self.image()?.buffer())
            }
            /// Bytes of pixel data at the start of the buffer.
            #[wasm_bindgen(getter, js_name = byteLength)]
            pub fn byte_length(&self) -> Result<usize, JsValue> {
                Ok(self.image()?.byte_len())
            }
            /// `"u16"` (two samples per 32-bit word), `"f32"` or `"rgba8"` (one pixel per word).
            #[wasm_bindgen(getter)]
            pub fn format(&self) -> Result<String, JsValue> {
                Ok(format_name(self.image()?.format()).to_string())
            }
            /// Copy an RGBA8 image into a new `rgba8unorm` `GPUTexture` (usage
            /// `STORAGE_BINDING | TEXTURE_BINDING | COPY_SRC`), owned by the caller.
            #[wasm_bindgen(js_name = createTexture)]
            pub fn create_texture(&self) -> Result<JsValue, JsValue> {
                let tex = self.image()?.to_texture().map_err(|e| to_js(e.into()))?;
                tex.as_webgpu().map(|t| JsValue::from(t.clone())).ok_or_else(|| {
                    to_js(ApiError { corrupt: None, kind: "GpuUnavailable", message: "not a WebGPU texture".into() })
                })
            }
            /// Read the pixels back into WASM memory; resolves to a [`Frame`].
            pub fn download(&self) -> Result<js_sys::Promise, JsValue> {
                let frame = self.frame.clone().ok_or_else(|| closed("detached"))?;
                let backend = self.backend.clone();
                Ok(future_to_promise(async move {
                    let d = api::download_gpu(&frame, &backend).await.map_err(to_js)?;
                    Ok(Frame::from(d).into())
                }))
            }
            /// Take ownership of the `GPUBuffer`: FlashRaw no longer recycles it (call
            /// `destroy()` on it when done). This object becomes unusable.
            #[wasm_bindgen(js_name = detachBuffer)]
            pub fn detach_buffer(&mut self) -> Result<JsValue, JsValue> {
                let frame = self.frame.take().ok_or_else(|| closed("already detached"))?;
                let buffer = webgpu_buffer(frame.as_gpu().ok_or_else(|| closed("detached"))?.buffer());
                // The pool must never recycle this buffer (the caller owns it now), so the Rust
                // side is leaked: wgpu's WebGPU handles are plain JS references, the leak is a
                // few bytes, and the GPUBuffer lives until the caller destroys it.
                std::mem::forget(frame);
                buffer
            }
        }
    }
}
