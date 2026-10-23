//! Decoded pixel storage.

use crate::{Error, Rect, Result};

/// Safety limits applied while decoding untrusted input.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum number of samples (width * height * components) in a decoded image.
    pub max_samples: u64,
    /// Maximum image width or height.
    pub max_dimension: u32,
}

impl Default for Limits {
    fn default() -> Self {
        // 1.2 G samples comfortably covers 400 MP multi-shot medium format (x3 linear).
        Limits { max_samples: 1_200_000_000, max_dimension: 65_535 }
    }
}

impl Limits {
    /// These limits with a different sample cap.
    pub fn with_max_samples(mut self, max_samples: u64) -> Self {
        self.max_samples = max_samples;
        self
    }

    /// These limits with a different width/height cap.
    pub fn with_max_dimension(mut self, max_dimension: u32) -> Self {
        self.max_dimension = max_dimension;
        self
    }

    /// Check that an image of `width × height × cpp` samples is within the limits.
    pub fn check_dims(&self, width: u32, height: u32, cpp: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::invalid_metadata(format!("zero image dimension {width}x{height}")));
        }
        if width > self.max_dimension || height > self.max_dimension {
            return Err(Error::LimitExceeded(format!("dimension {width}x{height} exceeds {}", self.max_dimension)));
        }
        let n = (width as u64 * height as u64).checked_mul(cpp.max(1) as u64);
        if n.is_none_or(|n| n > self.max_samples) {
            return Err(Error::LimitExceeded(format!(
                "{width}x{height}x{cpp} samples exceeds limit {}",
                self.max_samples
            )));
        }
        Ok(())
    }
}

/// Allocate a zero-initialised vector without aborting on allocation failure.
///
/// Large zeroed allocations are served by the OS as lazily-mapped zero pages
/// (calloc semantics), avoiding an explicit memset of 100+ MB buffers that the
/// decoder is about to overwrite anyway.
pub fn alloc_zeroed<T: zeroable::Zeroable>(len: usize) -> Result<Vec<T>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let layout = std::alloc::Layout::array::<T>(len)
        .map_err(|_| Error::OutOfMemory(format!("allocation of {len} elements overflows")))?;
    // SAFETY: `layout` has non-zero size (len > 0 and T is a non-ZST numeric type,
    // enforced by the `Zeroable` bound which is only implemented for primitive
    // integers and floats). `alloc_zeroed` returns either null (handled) or a block
    // valid for `layout`, all-zero bytes, which is a valid bit pattern for every
    // `Zeroable` type. Ownership transfers to the Vec with matching capacity/length
    // and the global allocator, so Vec's drop deallocates with the same layout.
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout) as *mut T;
        if ptr.is_null() {
            return Err(Error::OutOfMemory(format!("failed to allocate {} bytes", layout.size())));
        }
        advise_huge_pages(ptr as *mut u8, layout.size());
        Ok(Vec::from_raw_parts(ptr, len, len))
    }
}

/// Ask the kernel to back large buffers with transparent huge pages. Decoders write
/// every byte of their output, so first-touch page faults dominate memory-bound paths
/// (e.g. 16-bit uncompressed unpack); 2 MiB pages cut the fault count ~500x and avoid
/// fault-lock contention between worker threads. Purely advisory.
#[cfg(target_os = "linux")]
fn advise_huge_pages(ptr: *mut u8, size: usize) {
    const HUGE: usize = 2 << 20;
    if size < 4 * HUGE {
        return;
    }
    let start = (ptr as usize).next_multiple_of(HUGE);
    let end = (ptr as usize + size) & !(HUGE - 1);
    if end > start {
        // SAFETY: [start, end) lies within the allocation we just obtained; madvise with
        // MADV_HUGEPAGE only changes how the kernel backs these pages, never their contents
        // or ownership, and failure is harmless.
        unsafe {
            libc::madvise(start as *mut libc::c_void, end - start, libc::MADV_HUGEPAGE);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn advise_huge_pages(_ptr: *mut u8, _size: usize) {}

/// Marker for types whose all-zero bit pattern is a valid value.
pub mod zeroable {
    /// # Safety
    /// Implementors must be plain numeric types for which all-zero bytes are valid
    /// and that are not zero-sized.
    pub unsafe trait Zeroable: Copy + Send + Sync + 'static {}
    // SAFETY: `u8` is a primitive numeric type, not zero-sized; all-zero bytes are a valid value.
    unsafe impl Zeroable for u8 {}
    // SAFETY: `u16` is a primitive numeric type, not zero-sized; all-zero bytes are a valid value.
    unsafe impl Zeroable for u16 {}
    // SAFETY: `u32` is a primitive numeric type, not zero-sized; all-zero bytes are a valid value.
    unsafe impl Zeroable for u32 {}
    // SAFETY: `i32` is a primitive numeric type, not zero-sized; all-zero bytes are a valid value.
    unsafe impl Zeroable for i32 {}
    // SAFETY: `f32` is a primitive numeric type, not zero-sized; all-zero bytes are a valid value.
    unsafe impl Zeroable for f32 {}
}

/// Sample storage for a decoded image.
#[derive(Clone, PartialEq)]
pub enum PixelData {
    /// Integer samples.
    U16(Vec<u16>),
    /// Floating-point samples (e.g. floating-point DNG).
    F32(Vec<f32>),
}

impl std::fmt::Debug for PixelData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PixelData::U16(v) => write!(f, "U16([{} samples])", v.len()),
            PixelData::F32(v) => write!(f, "F32([{} samples])", v.len()),
        }
    }
}

impl PixelData {
    /// Number of samples (not pixels).
    pub fn len(&self) -> usize {
        match self {
            PixelData::U16(v) => v.len(),
            PixelData::F32(v) => v.len(),
        }
    }
    /// True if there are no samples.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The samples if this is `U16` storage.
    pub fn as_u16(&self) -> Option<&[u16]> {
        match self {
            PixelData::U16(v) => Some(v),
            _ => None,
        }
    }
    /// The samples, mutably, if this is `U16` storage.
    pub fn as_u16_mut(&mut self) -> Option<&mut [u16]> {
        match self {
            PixelData::U16(v) => Some(v),
            _ => None,
        }
    }
    /// The samples if this is `F32` storage.
    pub fn as_f32(&self) -> Option<&[f32]> {
        match self {
            PixelData::F32(v) => Some(v),
            _ => None,
        }
    }
    /// Size of one sample in bytes (2 or 4).
    pub fn bytes_per_sample(&self) -> usize {
        match self {
            PixelData::U16(_) => 2,
            PixelData::F32(_) => 4,
        }
    }
}

/// A decoded image buffer: `width x height` pixels of `cpp` interleaved samples.
#[derive(Debug, Clone, PartialEq)]
pub struct RawImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Components (samples) per pixel: 1 for CFA/mono, 3 for linear RGB / Foveon, 4 for some.
    pub cpp: u32,
    /// Row-major samples, `width * height * cpp` in total.
    pub data: PixelData,
    /// Largest non-negative, non-NaN sample of float data, when the decoder measured it
    /// while decoding (saves a pass over the buffer when converting to integers).
    pub float_max: Option<f32>,
}

/// Largest non-negative, non-NaN value of `v` as IEEE bits (0 when there is none). For
/// such values bit patterns order like integers, so this runs on integer lanes
/// (vectorisable, unlike `f32::max`); NaN patterns (above +inf) are masked out.
#[inline]
pub fn max_nonneg_bits(v: &[f32]) -> u32 {
    v.iter().fold(0i32, |m, &x| {
        let b = x.to_bits() as i32;
        m.max(if b > 0x7f80_0000 { 0 } else { b })
    }) as u32
}

impl RawImage {
    /// Allocate a zeroed `u16` image, enforcing `limits`.
    pub fn new_u16(width: u32, height: u32, cpp: u32, limits: &Limits) -> Result<Self> {
        limits.check_dims(width, height, cpp)?;
        let n = width as usize * height as usize * cpp as usize;
        Ok(RawImage { width, height, cpp, data: PixelData::U16(alloc_zeroed(n)?), float_max: None })
    }
    /// Allocate a zeroed `f32` image, enforcing `limits`.
    pub fn new_f32(width: u32, height: u32, cpp: u32, limits: &Limits) -> Result<Self> {
        limits.check_dims(width, height, cpp)?;
        let n = width as usize * height as usize * cpp as usize;
        Ok(RawImage { width, height, cpp, data: PixelData::F32(alloc_zeroed(n)?), float_max: None })
    }
    /// Samples per row.
    #[inline]
    pub fn row_len(&self) -> usize {
        self.width as usize * self.cpp as usize
    }
    /// `u16` samples. Panics if this is a float image (a decoder bug, not bad input).
    pub fn u16(&self) -> &[u16] {
        self.data.as_u16().expect("u16 image")
    }
    /// Mutable `u16` samples. Panics if this is a float image (a decoder bug, not bad input).
    pub fn u16_mut(&mut self) -> &mut [u16] {
        self.data.as_u16_mut().expect("u16 image")
    }
    /// Mutable `f32` samples. Panics if this is a `u16` image.
    pub fn f32_mut(&mut self) -> &mut [f32] {
        match &mut self.data {
            PixelData::F32(v) => v,
            _ => panic!("f32 image"),
        }
    }
    /// Copy out a sub-rectangle (rows copied in parallel for large images).
    pub fn crop(&self, r: Rect) -> Result<RawImage> {
        if r.is_empty() || r.right() > self.width || r.bottom() > self.height {
            return Err(Error::InvalidArgument(format!("crop {r} outside {}x{}", self.width, self.height)));
        }
        let cpp = self.cpp as usize;
        let (x0, w) = (r.x as usize * cpp, r.width as usize * cpp);
        let rl = self.row_len();
        let y0 = r.y as usize;
        fn copy<T: Copy + Send + Sync + crate::image::zeroable::Zeroable>(
            v: &[T],
            rl: usize,
            x0: usize,
            y0: usize,
            w: usize,
            h: usize,
        ) -> Result<Vec<T>> {
            let mut out = alloc_zeroed::<T>(w * h)?;
            crate::par::for_each_row_chunk(&mut out, w, 64, true, |first, rows| {
                for (i, row) in rows.chunks_mut(w).enumerate() {
                    let s = (y0 + first + i) * rl + x0;
                    row.copy_from_slice(&v[s..s + w]);
                }
            });
            Ok(out)
        }
        let h = r.height as usize;
        let data = match &self.data {
            PixelData::U16(v) => PixelData::U16(copy(v, rl, x0, y0, w, h)?),
            PixelData::F32(v) => PixelData::F32(copy(v, rl, x0, y0, w, h)?),
        };
        Ok(RawImage { width: r.width, height: r.height, cpp: self.cpp, data, float_max: self.float_max })
    }
    /// Sample at `(x, y, c)` as f32 (convenience for tests/tools; not for hot loops).
    pub fn sample(&self, x: u32, y: u32, c: u32) -> f32 {
        let i = (y as usize * self.width as usize + x as usize) * self.cpp as usize + c as usize;
        match &self.data {
            PixelData::U16(v) => v[i] as f32,
            PixelData::F32(v) => v[i],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_and_crop() {
        let mut img = RawImage::new_u16(4, 3, 1, &Limits::default()).unwrap();
        for (i, v) in img.u16_mut().iter_mut().enumerate() {
            *v = i as u16;
        }
        let c = img.crop(Rect::new(1, 1, 2, 2)).unwrap();
        assert_eq!(c.u16(), &[5, 6, 9, 10]);
        assert!(img.crop(Rect::new(3, 0, 2, 1)).is_err());
    }

    #[test]
    fn limits() {
        let l = Limits { max_samples: 100, max_dimension: 50 };
        assert!(RawImage::new_u16(10, 10, 1, &l).is_ok());
        assert!(RawImage::new_u16(10, 11, 1, &l).is_err());
        assert!(RawImage::new_u16(60, 1, 1, &l).is_err());
        assert!(RawImage::new_u16(0, 1, 1, &l).is_err());
    }
}
