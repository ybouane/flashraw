//! Lightweight stage profiling and a portable clock.
//!
//! `std::time::Instant` is unavailable on `wasm32-unknown-unknown`; the WASM
//! bindings install a `performance.now()`-backed clock via [`set_clock`].

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Decode pipeline stages reported by `--profile` and `DecodeStatistics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Stage {
    /// Container structure (TIFF/BMFF/vendor headers).
    ContainerParse,
    /// EXIF, maker notes, camera database lookup.
    Metadata,
    /// Huffman / arithmetic / wavelet entropy decoding.
    EntropyDecode,
    /// Unpacking of bit-packed uncompressed samples.
    Unpack,
    /// Predictor / delta reconstruction when separable from entropy decoding.
    Predictor,
    /// Post-decode calibration applied in the decoder (linearisation curves, ...).
    Calibration,
    /// Optional processing after decoding (black subtraction, demosaic, colour).
    Postprocess,
    /// Host → GPU transfer.
    GpuUpload,
    /// GPU compute dispatch.
    GpuCompute,
    /// GPU → host transfer.
    GpuDownload,
    /// Embedded preview extraction.
    Preview,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::ContainerParse => "Container parse",
            Stage::Metadata => "Metadata",
            Stage::EntropyDecode => "Entropy decode",
            Stage::Unpack => "Pixel unpack",
            Stage::Predictor => "Pixel reconstruction",
            Stage::Calibration => "Calibration",
            Stage::Postprocess => "Post-process",
            Stage::GpuUpload => "GPU upload",
            Stage::GpuCompute => "GPU compute",
            Stage::GpuDownload => "GPU download",
            Stage::Preview => "Preview",
        }
    }
}

static CLOCK: AtomicUsize = AtomicUsize::new(0);

/// Install a millisecond clock (used on targets without `Instant`).
pub fn set_clock(f: fn() -> f64) {
    CLOCK.store(f as usize, Ordering::Relaxed);
}

/// Monotonic milliseconds from an arbitrary origin.
pub fn now_ms() -> f64 {
    let p = CLOCK.load(Ordering::Relaxed);
    if p != 0 {
        // SAFETY: the only non-zero values ever stored are `fn() -> f64` pointers
        // written by `set_clock`; function pointers round-trip through usize.
        let f: fn() -> f64 = unsafe { std::mem::transmute::<usize, fn() -> f64>(p) };
        return f();
    }
    default_clock()
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn default_clock() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn default_clock() -> f64 {
    0.0
}

/// Collects per-stage wall-clock durations. Cheap enough to be always on: a handful
/// of entries per decode, recorded from whichever thread runs the stage.
#[derive(Debug, Default)]
pub struct Profiler {
    entries: Mutex<Vec<(Stage, f64)>>,
}

impl Profiler {
    pub fn new() -> Self {
        Self::default()
    }
    /// Add `ms` to `stage`.
    pub fn record(&self, stage: Stage, ms: f64) {
        if let Ok(mut e) = self.entries.lock() {
            if let Some(x) = e.iter_mut().find(|(s, _)| *s == stage) {
                x.1 += ms;
            } else {
                e.push((stage, ms));
            }
        }
    }
    /// Time `f` under `stage`.
    pub fn time<T>(&self, stage: Stage, f: impl FnOnce() -> T) -> T {
        let t = now_ms();
        let r = f();
        self.record(stage, now_ms() - t);
        r
    }
    /// Snapshot of `(stage, milliseconds)` sorted by pipeline order.
    pub fn stages(&self) -> Vec<(Stage, f64)> {
        let mut v = self.entries.lock().map(|e| e.clone()).unwrap_or_default();
        v.sort_by_key(|(s, _)| *s);
        v
    }
}
