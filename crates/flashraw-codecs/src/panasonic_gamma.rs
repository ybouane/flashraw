//! Tone curve of Panasonic RW2 raw format 8 (tags 0x39 and 0x3A).

/// Value of the curve at `idx`, for `idx` in `0..=0xffff`.
///
/// `points` and `slopes` are the six entries of tags 0x3A and 0x39 (each `points` entry
/// packs a breakpoint in its low 16 bits and an output base in its high 16 bits); `clip`
/// is tag 0x3B. Indices above `0xffff` are never requested.
pub(crate) fn gamma_curve(points: &[u32; 6], slopes: &[u32; 6], clip: u32, idx: u32) -> u16 {
    // Segment: the number of consecutive breakpoints 1..=5 that `idx` has reached.
    let seg = points[1..].iter().take_while(|&&p| idx >= (p & 0xffff)).count();
    let code = slopes[seg];
    let mode = code & SLOPE_MODE_MASK;
    let base = u64::from(points[seg] >> 16);
    let value: u64 = match mode {
        // Step to the next segment's base; the last segment ends at full scale.
        MODE_STEP if seg == 5 => 0xffff,
        MODE_STEP => u64::from(points[seg + 1] >> 16),
        MODE_HOLD => base,
        _ => {
            let delta = idx.wrapping_sub(points[seg] & 0xffff);
            let scaled = if code & SLOPE_LEFT_SHIFT != 0 {
                delta << (code & 15)
            } else {
                // Division by 2^mode rounded half up (a no-op for mode 0).
                delta.wrapping_add((1u32 << mode) >> 1) >> mode
            };
            u64::from(scaled) + base
        }
    };
    value.min(u64::from(clip)) as u16
}

/// Low five bits of a slope entry select how the segment is evaluated.
const SLOPE_MASK_BITS: u32 = 5;
const SLOPE_MODE_MASK: u32 = (1 << SLOPE_MASK_BITS) - 1;
/// Mode: the segment is a step up to the next segment's base.
const MODE_STEP: u32 = 31;
/// Mode: the segment holds its base value.
const MODE_HOLD: u32 = 15;
/// Flag in the mode bits: shift left by the low four bits instead of dividing.
const SLOPE_LEFT_SHIFT: u32 = 16;

#[cfg(test)]
mod tests {
    use super::gamma_curve;

    /// Small xorshift generator so the test needs no dependencies.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 16) as u32
        }
        fn word(&mut self) -> u32 {
            self.next() ^ (self.next() << 16)
        }
    }

    const CHUNK: usize = 48;
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    fn fnv(h: u64, v: u16) -> u64 {
        let h = (h ^ u64::from(v & 0xff)).wrapping_mul(0x100_0000_01b3);
        (h ^ u64::from(v >> 8)).wrapping_mul(0x100_0000_01b3)
    }

    const GOLDEN: [u64; 29] = [
        0xdf92745ad000bd6d,
        0xe08ecc8610060f8d,
        0x8eb75ab5ff8ef655,
        0x7912b16e375ab685,
        0x12d02dabccd48205,
        0xc71585124bc2f8e5,
        0x8f13c777a32ed33d,
        0xa2d76cce356718dd,
        0x51df67aa108bbe71,
        0xd6f0b3d4adb6e780,
        0x84feb86e15854f3b,
        0x4d59a3b04fee2c9e,
        0x4ec421f51d74cc7d,
        0x3c5c857d8664fd5d,
        0xb3430e28a7d91002,
        0x1c0ff26a0f6d7ab6,
        0x8826ec29cdbbbdfd,
        0x69516e999ca552f0,
        0xd07ad023c274e6fd,
        0x12fccb1b544d88d,
        0x9023471c6ac422f4,
        0xa14bcf97e1bd862d,
        0x1e244ca97c50b2e8,
        0xa47f64a353b8764,
        0x4b6ba1699fb09f47,
        0x63a661bf8e37fcc4,
        0x8e1e40b1a7aad75f,
        0xd0f3a8e78ce70b3c,
        0x2c9583bb617a06e5,
    ];

    #[test]
    fn identity_vector_is_identity() {
        for clip in [0xffff, 0xffff_ffff] {
            for idx in 0..=0xffffu32 {
                assert_eq!(u32::from(gamma_curve(&[0; 6], &[65536; 6], clip, idx)), idx);
            }
        }
    }

    #[test]
    fn special_codes() {
        let pts = [0, 0x100, 0x200, 0x300, 0x400, 0x500];
        // Mode 15 yields the base, mode 31 the next base (65535 on the last segment).
        assert_eq!(gamma_curve(&[0x10000; 6], &[15; 6], 0xffff, 7), 1);
        let p = [0, 0x1_0100, 0x2_0200, 0x3_0300, 0x4_0400, 0x5_0500];
        assert_eq!(gamma_curve(&p, &[31; 6], 0xffff, 0), 1);
        assert_eq!(gamma_curve(&p, &[31; 6], 0xffff, 0x500), 0xffff);
        // Rounded division by 2^m and left shift by the low four bits.
        assert_eq!(gamma_curve(&pts, &[2; 6], 0xffff, 5), 1);
        assert_eq!(gamma_curve(&pts, &[2; 6], 0xffff, 6), 2);
        assert_eq!(gamma_curve(&pts, &[16 | 3; 6], 0xffff, 5), 40);
        // The clip applies before the 16-bit truncation.
        assert_eq!(gamma_curve(&pts, &[0; 6], 100, 5000), 100);
        assert_eq!(gamma_curve(&pts, &[0; 6], 0x1_0005, 0xffff), 0xfaff);
    }

    /// Hashes of the whole curve over 1352 parameter sets (all 32 slope codes, boundary
    /// and random values over the full u32 domain, reader-reachable shapes, tone-curve
    /// shapes), computed once and checked equal to a previous implementation at every index.
    #[test]
    fn matches_golden_hashes() {
        let mut sets: Vec<([u32; 6], [u32; 6], u32)> = Vec::new();
        // The vector found in every known format-8 file (identity curve).
        sets.push(([0; 6], [65536; 6], 0xffff));
        sets.push(([0; 6], [65536; 6], 0xffff_ffff));
        // Boundary values: all 32 low-5-bit slope codes, with several upper-bit patterns.
        for code in 0..32u32 {
            for hi in [0u32, 0x20, 0x1_0000, 0xffff_ffe0] {
                let s = hi | code;
                sets.push(([0, 0x1000, 0x2000, 0x3000, 0x4000, 0x5000], [s; 6], 0xffff));
                sets.push((
                    [0x0100_0000, 0x1234_1000, 0x2345_2000, 0x3456_3000, 0x4567_4000, 0x5678_5000],
                    [s; 6],
                    0xffff,
                ));
                sets.push(([0xffff_ffff; 6], [s; 6], 0xffff_ffff));
                sets.push(([0xffff; 6], [s; 6], 0));
                sets.push(([0x8000_0001; 6], [s; 6], 0x1_0001));
            }
        }
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        // Reader-reachable shape: bases 0, monotone and non-monotone breakpoints.
        for _ in 0..200 {
            let points: [u32; 6] = std::array::from_fn(|_| rng.next() & 0xffff);
            let slopes: [u32; 6] = std::array::from_fn(|_| rng.word());
            let clip = rng.next() & 0xffff;
            sets.push((points, slopes, clip));
            let mut sorted = points;
            sorted.sort_unstable();
            sets.push((sorted, slopes, clip));
        }
        // The whole 32-bit parameter domain; every third set biases slope codes to the special ones.
        for n in 0..300 {
            let points: [u32; 6] = std::array::from_fn(|_| rng.word());
            let slopes: [u32; 6] = std::array::from_fn(|_| {
                let w = rng.word();
                if n % 3 == 0 { (w & !0x1f) | [0, 15, 16, 31, 1, 14, 17, 30][(rng.next() % 8) as usize] } else { w }
            });
            let clip = if n % 2 == 0 { rng.word() } else { rng.next() & 0xffff };
            sets.push((points, slopes, clip));
        }
        // Sorted breakpoints with real tone-curve shapes (with and without output bases).
        for (shift, mult) in [(0u32, 1u32), (1, 1), (2, 2), (4, 3), (14, 1)] {
            let pts: [u32; 6] = std::array::from_fn(|i| (i as u32) * 0x2000);
            let slopes: [u32; 6] = std::array::from_fn(|i| shift + (i as u32) * mult);
            sets.push((pts, slopes, 0xffff));
            let bases: [u32; 6] = std::array::from_fn(|i| (((i as u32) * 0x2800) << 16) | pts[i]);
            sets.push((bases, slopes, 0xffff));
        }
        assert_eq!(sets.len(), 1352);
        let hashes: Vec<u64> = sets
            .chunks(CHUNK)
            .map(|chunk| {
                let mut h = FNV_OFFSET;
                for &(p, s, c) in chunk {
                    for idx in 0..=0xffffu32 {
                        h = fnv(h, gamma_curve(&p, &s, c, idx));
                    }
                }
                h
            })
            .collect();
        assert_eq!(hashes, GOLDEN);
    }
}
