//! Eight `f32` lanes for the DCT passes: AVX2 on x86-64 (chosen at run time), NEON on aarch64,
//! SIMD128 in WebAssembly builds that enable it, and plain arrays elsewhere. Every operation is
//! one IEEE single-precision operation per lane (no fused multiply-add), so every
//! implementation gives the same result bit for bit.

/// `[e(0), e(1), ...]` for an array of 4 or 8, without closures (which might not be inlined into
/// the functions compiled with wider vector units).
macro_rules! arr {
    (4, |$i:ident| $e:expr) => {
        [{ let $i = 0usize; $e }, { let $i = 1usize; $e }, { let $i = 2usize; $e }, { let $i = 3usize; $e }]
    };
    (8, |$i:ident| $e:expr) => {
        [
            { let $i = 0usize; $e },
            { let $i = 1usize; $e },
            { let $i = 2usize; $e },
            { let $i = 3usize; $e },
            { let $i = 4usize; $e },
            { let $i = 5usize; $e },
            { let $i = 6usize; $e },
            { let $i = 7usize; $e },
        ]
    };
}
pub(super) use arr;

/// Eight lanes of `f32`.
pub(super) trait Lanes: Copy {
    fn splat(v: f32) -> Self;
    /// The first eight values of `s`.
    fn load(s: &[f32]) -> Self;
    /// Into the first eight values of `s`.
    fn store(self, s: &mut [f32]);
    fn add(self, o: Self) -> Self;
    fn sub(self, o: Self) -> Self;
    fn mul(self, o: Self) -> Self;
    fn div(self, o: Self) -> Self;
    /// The lanes whose magnitude is at least `t` (zero elsewhere, NaN included), and their count.
    fn keep_ge(self, t: Self) -> (Self, u32);
    /// Sum of the lanes, in the order of [`lane_sum`].
    fn sum(self) -> f32;
    /// `out[a]` lane `b` is `x[b]` lane `a`.
    fn transpose(x: &[Self; 8]) -> [Self; 8];
}

/// Sum of eight values in a fixed pairwise order.
#[inline(always)]
pub(super) fn lane_sum(e: &[f32; 8]) -> f32 {
    ((e[0] + e[4]) + (e[1] + e[5])) + ((e[2] + e[6]) + (e[3] + e[7]))
}

/// The portable implementation.
#[derive(Clone, Copy)]
pub(super) struct Scalar([f32; 8]);

impl Lanes for Scalar {
    #[inline(always)]
    fn splat(v: f32) -> Self {
        Scalar([v; 8])
    }
    #[inline(always)]
    fn load(s: &[f32]) -> Self {
        Scalar(s[..8].try_into().unwrap())
    }
    #[inline(always)]
    fn store(self, s: &mut [f32]) {
        s[..8].copy_from_slice(&self.0);
    }
    #[inline(always)]
    fn add(self, o: Self) -> Self {
        Scalar(arr!(8, |t| self.0[t] + o.0[t]))
    }
    #[inline(always)]
    fn sub(self, o: Self) -> Self {
        Scalar(arr!(8, |t| self.0[t] - o.0[t]))
    }
    #[inline(always)]
    fn mul(self, o: Self) -> Self {
        Scalar(arr!(8, |t| self.0[t] * o.0[t]))
    }
    #[inline(always)]
    fn div(self, o: Self) -> Self {
        Scalar(arr!(8, |t| self.0[t] / o.0[t]))
    }
    #[inline(always)]
    fn keep_ge(self, t: Self) -> (Self, u32) {
        let mut n = 0;
        let mut v = self.0;
        for (v, t) in v.iter_mut().zip(t.0) {
            let keep = v.abs() >= t;
            n += keep as u32;
            *v = if keep { *v } else { 0.0 };
        }
        (Scalar(v), n)
    }
    #[inline(always)]
    fn sum(self) -> f32 {
        lane_sum(&self.0)
    }
    #[inline(always)]
    fn transpose(x: &[Self; 8]) -> [Self; 8] {
        arr!(8, |a| Scalar(arr!(8, |b| x[b].0[a])))
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
pub(super) use avx::Avx;

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
mod avx {
    use super::{Lanes, lane_sum};
    use std::arch::x86_64::*;

    /// AVX lanes. Only used inside functions compiled with AVX2 after a run-time check: the
    /// methods are always inlined into them.
    #[derive(Clone, Copy)]
    pub(in crate::denoise) struct Avx(__m256);

    // SAFETY (every method): the type is only used by code that checked that the CPU supports
    // AVX2; loads and stores check the slice length.
    impl Lanes for Avx {
        #[inline(always)]
        fn splat(v: f32) -> Self {
            Avx(unsafe { _mm256_set1_ps(v) })
        }
        #[inline(always)]
        fn load(s: &[f32]) -> Self {
            assert!(s.len() >= 8);
            Avx(unsafe { _mm256_loadu_ps(s.as_ptr()) })
        }
        #[inline(always)]
        fn store(self, s: &mut [f32]) {
            assert!(s.len() >= 8);
            unsafe { _mm256_storeu_ps(s.as_mut_ptr(), self.0) }
        }
        #[inline(always)]
        fn add(self, o: Self) -> Self {
            Avx(unsafe { _mm256_add_ps(self.0, o.0) })
        }
        #[inline(always)]
        fn sub(self, o: Self) -> Self {
            Avx(unsafe { _mm256_sub_ps(self.0, o.0) })
        }
        #[inline(always)]
        fn mul(self, o: Self) -> Self {
            Avx(unsafe { _mm256_mul_ps(self.0, o.0) })
        }
        #[inline(always)]
        fn div(self, o: Self) -> Self {
            Avx(unsafe { _mm256_div_ps(self.0, o.0) })
        }
        #[inline(always)]
        fn keep_ge(self, t: Self) -> (Self, u32) {
            unsafe {
                let abs = _mm256_andnot_ps(_mm256_set1_ps(-0.0), self.0);
                let m = _mm256_cmp_ps::<_CMP_GE_OQ>(abs, t.0);
                (Avx(_mm256_and_ps(self.0, m)), (_mm256_movemask_ps(m) as u32).count_ones())
            }
        }
        #[inline(always)]
        fn sum(self) -> f32 {
            let mut a = [0f32; 8];
            self.store(&mut a);
            lane_sum(&a)
        }
        #[inline(always)]
        fn transpose(x: &[Self; 8]) -> [Self; 8] {
            unsafe {
                let t0 = _mm256_unpacklo_ps(x[0].0, x[1].0);
                let t1 = _mm256_unpackhi_ps(x[0].0, x[1].0);
                let t2 = _mm256_unpacklo_ps(x[2].0, x[3].0);
                let t3 = _mm256_unpackhi_ps(x[2].0, x[3].0);
                let t4 = _mm256_unpacklo_ps(x[4].0, x[5].0);
                let t5 = _mm256_unpackhi_ps(x[4].0, x[5].0);
                let t6 = _mm256_unpacklo_ps(x[6].0, x[7].0);
                let t7 = _mm256_unpackhi_ps(x[6].0, x[7].0);
                let s0 = _mm256_shuffle_ps::<0x44>(t0, t2);
                let s1 = _mm256_shuffle_ps::<0xee>(t0, t2);
                let s2 = _mm256_shuffle_ps::<0x44>(t1, t3);
                let s3 = _mm256_shuffle_ps::<0xee>(t1, t3);
                let s4 = _mm256_shuffle_ps::<0x44>(t4, t6);
                let s5 = _mm256_shuffle_ps::<0xee>(t4, t6);
                let s6 = _mm256_shuffle_ps::<0x44>(t5, t7);
                let s7 = _mm256_shuffle_ps::<0xee>(t5, t7);
                [
                    Avx(_mm256_permute2f128_ps::<0x20>(s0, s4)),
                    Avx(_mm256_permute2f128_ps::<0x20>(s1, s5)),
                    Avx(_mm256_permute2f128_ps::<0x20>(s2, s6)),
                    Avx(_mm256_permute2f128_ps::<0x20>(s3, s7)),
                    Avx(_mm256_permute2f128_ps::<0x31>(s0, s4)),
                    Avx(_mm256_permute2f128_ps::<0x31>(s1, s5)),
                    Avx(_mm256_permute2f128_ps::<0x31>(s2, s6)),
                    Avx(_mm256_permute2f128_ps::<0x31>(s3, s7)),
                ]
            }
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
pub(super) use neon::Neon;

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
mod neon {
    use super::{Lanes, lane_sum};
    use std::arch::aarch64::*;

    /// Two NEON registers.
    #[derive(Clone, Copy)]
    pub(in crate::denoise) struct Neon(float32x4_t, float32x4_t);

    #[inline(always)]
    fn t4(a: float32x4_t, b: float32x4_t, c: float32x4_t, d: float32x4_t) -> [float32x4_t; 4] {
        // SAFETY: NEON is part of the aarch64 baseline.
        #[allow(unused_unsafe)]
        unsafe {
            let ab0 = vtrn1q_f32(a, b);
            let ab1 = vtrn2q_f32(a, b);
            let cd0 = vtrn1q_f32(c, d);
            let cd1 = vtrn2q_f32(c, d);
            let f = |x: float32x4_t| vreinterpretq_f64_f32(x);
            let g = |x: float64x2_t| vreinterpretq_f32_f64(x);
            [
                g(vtrn1q_f64(f(ab0), f(cd0))),
                g(vtrn1q_f64(f(ab1), f(cd1))),
                g(vtrn2q_f64(f(ab0), f(cd0))),
                g(vtrn2q_f64(f(ab1), f(cd1))),
            ]
        }
    }

    // SAFETY (every method): NEON is part of the aarch64 baseline; loads and stores check the
    // slice length.
    #[allow(unused_unsafe)]
    impl Lanes for Neon {
        #[inline(always)]
        fn splat(v: f32) -> Self {
            unsafe { Neon(vdupq_n_f32(v), vdupq_n_f32(v)) }
        }
        #[inline(always)]
        fn load(s: &[f32]) -> Self {
            assert!(s.len() >= 8);
            unsafe { Neon(vld1q_f32(s.as_ptr()), vld1q_f32(s.as_ptr().add(4))) }
        }
        #[inline(always)]
        fn store(self, s: &mut [f32]) {
            assert!(s.len() >= 8);
            unsafe {
                vst1q_f32(s.as_mut_ptr(), self.0);
                vst1q_f32(s.as_mut_ptr().add(4), self.1);
            }
        }
        #[inline(always)]
        fn add(self, o: Self) -> Self {
            unsafe { Neon(vaddq_f32(self.0, o.0), vaddq_f32(self.1, o.1)) }
        }
        #[inline(always)]
        fn sub(self, o: Self) -> Self {
            unsafe { Neon(vsubq_f32(self.0, o.0), vsubq_f32(self.1, o.1)) }
        }
        #[inline(always)]
        fn mul(self, o: Self) -> Self {
            unsafe { Neon(vmulq_f32(self.0, o.0), vmulq_f32(self.1, o.1)) }
        }
        #[inline(always)]
        fn div(self, o: Self) -> Self {
            unsafe { Neon(vdivq_f32(self.0, o.0), vdivq_f32(self.1, o.1)) }
        }
        #[inline(always)]
        fn keep_ge(self, t: Self) -> (Self, u32) {
            unsafe {
                let m0 = vcgeq_f32(vabsq_f32(self.0), t.0);
                let m1 = vcgeq_f32(vabsq_f32(self.1), t.1);
                let n = vaddvq_u32(vshrq_n_u32::<31>(m0)) + vaddvq_u32(vshrq_n_u32::<31>(m1));
                let keep =
                    |v: float32x4_t, m: uint32x4_t| vreinterpretq_f32_u32(vandq_u32(vreinterpretq_u32_f32(v), m));
                (Neon(keep(self.0, m0), keep(self.1, m1)), n)
            }
        }
        #[inline(always)]
        fn sum(self) -> f32 {
            let mut a = [0f32; 8];
            self.store(&mut a);
            lane_sum(&a)
        }
        #[inline(always)]
        fn transpose(x: &[Self; 8]) -> [Self; 8] {
            // [A B; C D] (4x4 blocks) -> [A' C'; B' D'].
            let a = t4(x[0].0, x[1].0, x[2].0, x[3].0);
            let b = t4(x[0].1, x[1].1, x[2].1, x[3].1);
            let c = t4(x[4].0, x[5].0, x[6].0, x[7].0);
            let d = t4(x[4].1, x[5].1, x[6].1, x[7].1);
            [
                Neon(a[0], c[0]),
                Neon(a[1], c[1]),
                Neon(a[2], c[2]),
                Neon(a[3], c[3]),
                Neon(b[0], d[0]),
                Neon(b[1], d[1]),
                Neon(b[2], d[2]),
                Neon(b[3], d[3]),
            ]
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "wasm32", target_feature = "simd128"))]
pub(super) use wasm::Wasm;

#[cfg(all(feature = "simd", target_arch = "wasm32", target_feature = "simd128"))]
mod wasm {
    use super::{Lanes, lane_sum};
    use std::arch::wasm32::*;

    /// Two SIMD128 registers.
    #[derive(Clone, Copy)]
    pub(in crate::denoise) struct Wasm(v128, v128);

    #[inline(always)]
    fn t4(a: v128, b: v128, c: v128, d: v128) -> [v128; 4] {
        let ab0 = i32x4_shuffle::<0, 4, 1, 5>(a, b);
        let ab1 = i32x4_shuffle::<2, 6, 3, 7>(a, b);
        let cd0 = i32x4_shuffle::<0, 4, 1, 5>(c, d);
        let cd1 = i32x4_shuffle::<2, 6, 3, 7>(c, d);
        [
            i64x2_shuffle::<0, 2>(ab0, cd0),
            i64x2_shuffle::<1, 3>(ab0, cd0),
            i64x2_shuffle::<0, 2>(ab1, cd1),
            i64x2_shuffle::<1, 3>(ab1, cd1),
        ]
    }

    impl Lanes for Wasm {
        #[inline(always)]
        fn splat(v: f32) -> Self {
            Wasm(f32x4_splat(v), f32x4_splat(v))
        }
        #[inline(always)]
        fn load(s: &[f32]) -> Self {
            assert!(s.len() >= 8);
            // SAFETY: eight values are in bounds (checked above).
            unsafe { Wasm(v128_load(s.as_ptr().cast()), v128_load(s.as_ptr().add(4).cast())) }
        }
        #[inline(always)]
        fn store(self, s: &mut [f32]) {
            assert!(s.len() >= 8);
            // SAFETY: eight values are in bounds (checked above).
            unsafe {
                v128_store(s.as_mut_ptr().cast(), self.0);
                v128_store(s.as_mut_ptr().add(4).cast(), self.1);
            }
        }
        #[inline(always)]
        fn add(self, o: Self) -> Self {
            Wasm(f32x4_add(self.0, o.0), f32x4_add(self.1, o.1))
        }
        #[inline(always)]
        fn sub(self, o: Self) -> Self {
            Wasm(f32x4_sub(self.0, o.0), f32x4_sub(self.1, o.1))
        }
        #[inline(always)]
        fn mul(self, o: Self) -> Self {
            Wasm(f32x4_mul(self.0, o.0), f32x4_mul(self.1, o.1))
        }
        #[inline(always)]
        fn div(self, o: Self) -> Self {
            Wasm(f32x4_div(self.0, o.0), f32x4_div(self.1, o.1))
        }
        #[inline(always)]
        fn keep_ge(self, t: Self) -> (Self, u32) {
            let m0 = f32x4_ge(f32x4_abs(self.0), t.0);
            let m1 = f32x4_ge(f32x4_abs(self.1), t.1);
            let n = i32x4_bitmask(m0).count_ones() + i32x4_bitmask(m1).count_ones();
            (Wasm(v128_and(self.0, m0), v128_and(self.1, m1)), n)
        }
        #[inline(always)]
        fn sum(self) -> f32 {
            let mut a = [0f32; 8];
            self.store(&mut a);
            lane_sum(&a)
        }
        #[inline(always)]
        fn transpose(x: &[Self; 8]) -> [Self; 8] {
            let a = t4(x[0].0, x[1].0, x[2].0, x[3].0);
            let b = t4(x[0].1, x[1].1, x[2].1, x[3].1);
            let c = t4(x[4].0, x[5].0, x[6].0, x[7].0);
            let d = t4(x[4].1, x[5].1, x[6].1, x[7].1);
            [
                Wasm(a[0], c[0]),
                Wasm(a[1], c[1]),
                Wasm(a[2], c[2]),
                Wasm(a[3], c[3]),
                Wasm(b[0], d[0]),
                Wasm(b[1], d[1]),
                Wasm(b[2], d[2]),
                Wasm(b[3], d[3]),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check<L: Lanes>() {
        let x: [L; 8] = std::array::from_fn(|i| {
            let v: Vec<f32> = (0..8).map(|j| (i * 8 + j) as f32 - 20.0).collect();
            L::load(&v)
        });
        let t = L::transpose(&x);
        for (a, r) in t.iter().enumerate() {
            let mut o = [0f32; 8];
            r.store(&mut o);
            for (b, v) in o.iter().enumerate() {
                assert_eq!(*v, (b * 8 + a) as f32 - 20.0);
            }
        }
        let (k, n) = x[2].keep_ge(L::splat(3.5));
        let mut o = [0f32; 8];
        k.store(&mut o);
        assert_eq!(o, [-4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(n, 1);
        assert_eq!(x[3].sum(), (24..32).map(|v| v as f32 - 20.0).sum::<f32>());
    }

    #[test]
    fn lanes_agree() {
        check::<Scalar>();
        #[cfg(all(feature = "simd", target_arch = "x86_64"))]
        if std::arch::is_x86_feature_detected!("avx2") {
            check::<Avx>();
        }
        #[cfg(all(feature = "simd", target_arch = "aarch64"))]
        check::<Neon>();
    }
}
