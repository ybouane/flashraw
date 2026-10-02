//! Runtime CPU feature detection shared by SIMD kernels.

/// SIMD capability level detected at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SimdLevel {
    Scalar,
    Sse41,
    Avx2,
    Avx512,
    Neon,
    WasmSimd128,
}

impl SimdLevel {
    pub fn name(self) -> &'static str {
        match self {
            SimdLevel::Scalar => "scalar",
            SimdLevel::Sse41 => "SSE4.1",
            SimdLevel::Avx2 => "AVX2",
            SimdLevel::Avx512 => "AVX-512",
            SimdLevel::Neon => "NEON",
            SimdLevel::WasmSimd128 => "WASM SIMD128",
        }
    }
}

/// Best SIMD level available on this CPU.
pub fn detect() -> SimdLevel {
    #[cfg(target_arch = "x86_64")]
    {
        use std::sync::OnceLock;
        static LEVEL: OnceLock<SimdLevel> = OnceLock::new();
        *LEVEL.get_or_init(|| {
            if std::arch::is_x86_feature_detected!("avx512bw") && std::arch::is_x86_feature_detected!("avx512vbmi") {
                SimdLevel::Avx512
            } else if std::arch::is_x86_feature_detected!("avx2") {
                SimdLevel::Avx2
            } else if std::arch::is_x86_feature_detected!("sse4.1") {
                SimdLevel::Sse41
            } else {
                SimdLevel::Scalar
            }
        })
    }
    #[cfg(target_arch = "aarch64")]
    {
        SimdLevel::Neon
    }
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        SimdLevel::WasmSimd128
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        all(target_arch = "wasm32", target_feature = "simd128")
    )))]
    {
        SimdLevel::Scalar
    }
}
