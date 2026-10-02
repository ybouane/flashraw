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
//! See `docs/wasm.md` for the memory model.

pub mod api;
pub mod json;

#[cfg(target_arch = "wasm32")]
mod bindings {
    use crate::api::{self, ApiError, Decoded};
    use crate::json;
    use flashraw::FlashRaw;
    use std::sync::Mutex;
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

    /// Build features of this binary as JSON (`{ simd, gpu, threads }`).
    #[wasm_bindgen(js_name = buildInfo)]
    pub fn build_info() -> String {
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "simd": cfg!(target_feature = "simd128"),
            "simdLevel": flashraw::low_level::core::simd::detect().name(),
            "gpu": cfg!(feature = "gpu"),
            "gpuAvailable": flashraw::gpu_available(),
            "threads": false,
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
    #[wasm_bindgen]
    pub struct InputBuffer {
        buf: Vec<u8>,
    }

    #[wasm_bindgen]
    impl InputBuffer {
        /// Allocate `len` bytes (fails with `OutOfMemory` rather than aborting).
        #[wasm_bindgen(constructor)]
        pub fn new(len: usize) -> Result<InputBuffer, JsValue> {
            let mut buf = Vec::new();
            buf.try_reserve_exact(len).map_err(|_| {
                to_js(ApiError { kind: "OutOfMemory", message: format!("cannot allocate {len} bytes for the input") })
            })?;
            buf.resize(len, 0);
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
        let raw = api::open(input.buf, options_json).map_err(to_js)?;
        Ok(RawFile { raw })
    }

    /// An opened RAW file. Owns the input bytes; previews are views into them.
    #[wasm_bindgen]
    pub struct RawFile {
        raw: FlashRaw<'static>,
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
}
