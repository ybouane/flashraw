//! Removal of JPEG byte stuffing from an entropy-coded segment, in bulk.
//!
//! Scans are mostly plain bytes: blocks of 32 bytes are tested for 0xFF at once and copied
//! whole. Very large scans are cut into pieces that are processed on several threads; a piece
//! always starts right after a byte that is not 0xFF, which is a token boundary, so the pieces
//! concatenate to exactly the sequential result. The threaded path first counts the output
//! of each piece, then lets every piece write its own slice of the final buffer.

use flashraw_core::par;

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGHS: u64 = 0x8080_8080_8080_8080;

/// Scans at least this long may be split across threads when few threads are available ...
const SPLIT_MIN_BYTES: usize = 8 << 20;
/// ... and this long when there are more than `FEW_THREADS` (measured: with many workers a
/// scan below this decodes faster when unstuffed on one thread, the wake-ups cost more than they save).
const SPLIT_MIN_BYTES_MANY: usize = 12 << 20;
const FEW_THREADS: usize = 4;
/// Smallest piece handed to one thread.
const PIECE_MIN_BYTES: usize = 1 << 20;
/// Bytes unstuffed on the calling thread before the rest is fanned out: callers may hand over
/// far more data than the scan holds (everything after a restart marker, say).
const WINDOW_BYTES: usize = 128 << 10;

/// High bit set in the lowest byte of `w` that equals 0xFF (exact for that byte; higher
/// flagged bytes may be false positives, which is why only the lowest is used).
#[inline(always)]
fn ff_flags(w: u64) -> u64 {
    let x = !w;
    x.wrapping_sub(ONES) & !x & HIGHS
}

/// Bit `k` of the result is set when `block[k]` is 0xFF.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn ff_mask(block: &[u8; 32]) -> u32 {
    use std::arch::x86_64::{__m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_set1_epi8};
    // SAFETY: SSE2 is part of the x86-64 baseline, and both unaligned 16-byte loads lie inside `block`.
    unsafe {
        let ff = _mm_set1_epi8(-1);
        let lo = _mm_loadu_si128(block.as_ptr() as *const __m128i);
        let hi = _mm_loadu_si128(block.as_ptr().add(16) as *const __m128i);
        let a = _mm_movemask_epi8(_mm_cmpeq_epi8(lo, ff)) as u32 & 0xFFFF;
        let b = _mm_movemask_epi8(_mm_cmpeq_epi8(hi, ff)) as u32 & 0xFFFF;
        a | (b << 16)
    }
}

/// Portable version of [`ff_mask`]: exact for the lowest set bit (all that is used) and zero
/// exactly when the block holds no 0xFF.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
#[inline(always)]
fn ff_mask_portable(block: &[u8; 32]) -> u32 {
    for (k, w) in block.chunks_exact(8).enumerate() {
        let flags = ff_flags(u64::from_le_bytes(w.try_into().expect("8 bytes")));
        if flags != 0 {
            return 1u32 << (8 * k as u32 + flags.trailing_zeros() / 8);
        }
    }
    0
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn ff_mask(block: &[u8; 32]) -> u32 {
    ff_mask_portable(block)
}

/// Destination of unstuffed bytes: a vector, a slice of the final buffer, or just a counter.
trait Sink {
    fn block(&mut self, b: &[u8; 32]);
    fn bytes(&mut self, b: &[u8]);
    fn byte(&mut self, b: u8);
    fn len(&self) -> usize;
}

impl Sink for Vec<u8> {
    #[inline(always)]
    fn block(&mut self, b: &[u8; 32]) {
        self.extend_from_slice(b);
    }
    #[inline(always)]
    fn bytes(&mut self, b: &[u8]) {
        self.extend_from_slice(b);
    }
    #[inline(always)]
    fn byte(&mut self, b: u8) {
        self.push(b);
    }
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

struct Count(usize);

impl Sink for Count {
    #[inline(always)]
    fn block(&mut self, _: &[u8; 32]) {
        self.0 += 32;
    }
    #[inline(always)]
    fn bytes(&mut self, b: &[u8]) {
        self.0 += b.len();
    }
    #[inline(always)]
    fn byte(&mut self, _: u8) {
        self.0 += 1;
    }
    fn len(&self) -> usize {
        self.0
    }
}

struct Slice<'a> {
    dst: &'a mut [u8],
    n: usize,
}

impl Sink for Slice<'_> {
    #[inline(always)]
    fn block(&mut self, b: &[u8; 32]) {
        self.dst[self.n..self.n + 32].copy_from_slice(b);
        self.n += 32;
    }
    #[inline(always)]
    fn bytes(&mut self, b: &[u8]) {
        self.dst[self.n..self.n + b.len()].copy_from_slice(b);
        self.n += b.len();
    }
    #[inline(always)]
    fn byte(&mut self, b: u8) {
        self.dst[self.n] = b;
        self.n += 1;
    }
    fn len(&self) -> usize {
        self.n
    }
}

/// Unstuff `scan` into `out`. Returns the output offsets of the restart markers met and
/// whether a terminating marker was met.
fn unstuff_piece<S: Sink>(scan: &[u8], restarts: bool, out: &mut S) -> (Vec<usize>, bool) {
    let mut marks = Vec::new();
    let mut i = 0usize;
    let mut ended = false;
    while i + 32 <= scan.len() {
        let block: &[u8; 32] = scan[i..i + 32].try_into().expect("32 bytes");
        let m = ff_mask(block);
        if m == 0 {
            out.block(block);
            i += 32;
            continue;
        }
        let k = m.trailing_zeros() as usize;
        out.bytes(&block[..k]);
        let j = i + k;
        match scan.get(j + 1) {
            Some(0) => {
                out.byte(0xFF);
                i = j + 2;
            }
            Some(0xD0..=0xD7) if restarts => {
                marks.push(out.len());
                i = j + 2;
            }
            _ => {
                ended = true;
                break;
            }
        }
    }
    // The last bytes, one at a time.
    while !ended && i < scan.len() {
        let b = scan[i];
        if b != 0xFF {
            out.byte(b);
            i += 1;
            continue;
        }
        match scan.get(i + 1) {
            Some(0) => {
                out.byte(0xFF);
                i += 2;
            }
            Some(0xD0..=0xD7) if restarts => {
                marks.push(out.len());
                i += 2;
            }
            _ => ended = true,
        }
    }
    (marks, ended)
}

/// Unstuffed bytes and the output offsets at which restart markers were found.
pub(super) fn unstuff(scan: &[u8], restarts: bool) -> (Vec<u8>, Vec<usize>) {
    let split_min = if par::threads() > FEW_THREADS { SPLIT_MIN_BYTES_MANY } else { SPLIT_MIN_BYTES };
    let starts = if scan.len() >= split_min && par::threads() > 1 { split_points(scan) } else { Vec::new() };
    if starts.len() < 3 {
        return sequential(scan, restarts);
    }
    let mut data: Vec<u8> = Vec::with_capacity(starts[1]);
    let (mut marks, ended) = unstuff_piece(&scan[..starts[1]], restarts, &mut data);
    if ended {
        return (data, marks);
    }
    // Pass 1: the output size, restart marks and end of each remaining piece.
    let ranges: Vec<(usize, usize)> =
        starts[1..].iter().zip(starts[2..].iter().chain(std::iter::once(&scan.len()))).map(|(&a, &b)| (a, b)).collect();
    let counted = par::map(ranges.clone(), true, |(a, b)| {
        let mut n = Count(0);
        let (m, ended) = unstuff_piece(&scan[a..b], restarts, &mut n);
        (n.0, m, ended)
    });
    // Keep the pieces up to the first one that met the end of the scan.
    let keep = counted.iter().position(|c| c.2).map_or(counted.len(), |p| p + 1);
    let head = data.len();
    let mut bounds = Vec::with_capacity(keep);
    let mut total = head;
    for (n, m, _) in &counted[..keep] {
        marks.extend(m.iter().map(|&o| o + total));
        bounds.push((total - head, total - head + n));
        total += n;
    }
    // Pass 2: every piece writes its own slice of the result.
    data.resize(total, 0);
    let pieces = match par::split_disjoint(&mut data[head..], &bounds) {
        Some(p) => p,
        None => return sequential(scan, restarts),
    };
    par::map(pieces.into_iter().zip(&ranges).collect(), true, |(dst, &(a, b))| {
        let mut w = Slice { dst, n: 0 };
        unstuff_piece(&scan[a..b], restarts, &mut w);
    });
    (data, marks)
}

/// Unstuff the whole scan on the calling thread.
fn sequential(scan: &[u8], restarts: bool) -> (Vec<u8>, Vec<usize>) {
    let mut data = Vec::with_capacity(scan.len());
    let (marks, _) = unstuff_piece(scan, restarts, &mut data);
    (data, marks)
}

/// Start offsets of the pieces: 0, then the end of a small first window, then the rest cut into
/// one piece per worker. Every start follows a byte that is not 0xFF.
fn split_points(scan: &[u8]) -> Vec<usize> {
    let boundary = |mut p: usize| {
        while p < scan.len() && scan[p - 1] == 0xFF {
            p += 1;
        }
        p
    };
    let window = boundary(WINDOW_BYTES);
    let rest = scan.len().saturating_sub(window);
    let n = par::threads().min(rest / PIECE_MIN_BYTES).max(1);
    let mut starts = vec![0usize, window];
    for k in 1..n {
        let p = boundary(window + rest / n * k);
        if p < scan.len() && p > *starts.last().unwrap() {
            starts.push(p);
        }
    }
    if window >= scan.len() {
        starts.truncate(1);
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_mask_agrees_on_the_first_marker_byte() {
        let mut block = [0x12u8; 32];
        assert_eq!(ff_mask_portable(&block), 0);
        assert_eq!(ff_mask(&block), 0);
        for k in 0..32 {
            for extra in [None, Some(31usize)] {
                block = [0x12; 32];
                block[k] = 0xFF;
                if let Some(e) = extra {
                    block[e.max(k)] = 0xFF;
                }
                assert_eq!(ff_mask_portable(&block).trailing_zeros() as usize, k);
                assert_eq!(ff_mask(&block).trailing_zeros() as usize, k);
            }
        }
        // Bytes just below 0xFF and above 0x7F must not be mistaken for it.
        block = [0xFE; 32];
        block[17] = 0x7F;
        assert_eq!((ff_mask_portable(&block), ff_mask(&block)), (0, 0));
    }
}
