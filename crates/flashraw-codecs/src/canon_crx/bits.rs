//! MSB-first bit reader specialised for CRX Golomb–Rice code words.
//!
//! A 64-bit cache is refilled 8 bytes at a time (at most once per code word), so a
//! typical code word (`zeros + 1 + k <= 56` bits) is extracted with one
//! leading-zero count and two shifts. Bits past the end of the input read as zero
//! and are counted, so corrupt streams terminate.

#[derive(Clone)]
pub(crate) struct CrxBits<'a> {
    data: &'a [u8],
    pos: usize,
    /// Next bits, left-aligned. Bits below `avail` are the true following stream
    /// bits or zero (never stale data), so re-reading them is idempotent.
    cache: u64,
    avail: u32,
    overrun: usize,
}

impl<'a> CrxBits<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        CrxBits { data, pos: 0, cache: 0, avail: 0, overrun: 0 }
    }

    /// Ensure at least 32 (normally 56+) bits are buffered.
    #[inline(always)]
    fn refill(&mut self) {
        if self.avail >= 32 {
            return;
        }
        if let Some(chunk) = self.data.get(self.pos..self.pos + 8) {
            let w = u64::from_be_bytes(chunk.try_into().unwrap_or([0; 8]));
            self.cache |= w >> self.avail;
            self.pos += ((63 - self.avail) >> 3) as usize;
            self.avail |= 56;
        } else {
            self.refill_slow();
        }
    }

    #[cold]
    fn refill_slow(&mut self) {
        // Stop at 56..=63 bits like the fast path: a full 64 would make consuming the
        // whole cache a 64-bit shift.
        while self.avail < 56 {
            let b = match self.data.get(self.pos) {
                Some(&b) => b,
                None => {
                    self.overrun += 1;
                    0
                }
            };
            self.pos += 1;
            self.cache |= (b as u64) << (56 - self.avail);
            self.avail += 8;
        }
    }

    #[inline(always)]
    fn consume(&mut self, n: u32) {
        debug_assert!(n <= self.avail && n < 64);
        self.cache <<= n;
        self.avail -= n;
    }

    /// Read `n` (1..=32) bits.
    #[inline(always)]
    pub(crate) fn get(&mut self, n: u32) -> u32 {
        debug_assert!((1..=32).contains(&n));
        self.refill();
        let v = (self.cache >> (64 - n)) as u32;
        self.consume(n);
        v
    }

    /// Read one bit.
    #[inline(always)]
    pub(crate) fn bit(&mut self) -> bool {
        self.get(1) != 0
    }

    /// Count zero bits up to the next one bit (consumed).
    pub(crate) fn zeros(&mut self) -> u32 {
        let mut n = 0u32;
        loop {
            self.refill();
            let lz = self.cache.leading_zeros();
            if lz < self.avail {
                self.consume(lz + 1);
                return n + lz;
            }
            n = n.saturating_add(self.avail);
            self.consume(self.avail);
            self.cache = 0;
            if self.exhausted() {
                return n;
            }
        }
    }

    /// Golomb–Rice code word with parameter `k` (`<= 15`): `zeros` unary prefix and
    /// `k` low bits; prefixes `>= escape` (`<= 41`) are followed by a raw
    /// `escape_bits`-bit value instead.
    #[inline(always)]
    pub(crate) fn code(&mut self, k: u32, escape: u32, escape_bits: u32) -> u32 {
        debug_assert!(k <= 15 && escape <= 41);
        self.refill();
        let lz = self.cache.leading_zeros();
        if lz < escape && lz + 1 + k <= self.avail {
            let rest = self.cache << (lz + 1);
            // `(rest >> 1) >> (63 - k)` is `rest >> (64 - k)` and 0 for k = 0.
            let low = ((rest >> 1) >> (63 - k)) as u32;
            self.cache = rest << k;
            self.avail -= lz + 1 + k;
            return (lz << k) | low;
        }
        self.code_slow(k, escape, escape_bits)
    }

    #[cold]
    fn code_slow(&mut self, k: u32, escape: u32, escape_bits: u32) -> u32 {
        let z = self.zeros();
        if z >= escape {
            self.get(escape_bits)
        } else if k != 0 {
            self.get(k) | (z << k)
        } else {
            z
        }
    }

    /// Bytes requested past the end of the input beyond normal look-ahead.
    #[inline]
    pub(crate) fn exhausted(&self) -> bool {
        self.overrun > 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashraw_core::bits::{BitPumpMsb, BitRead};

    /// Reference decoding with the generic core bit pump.
    fn reference_code(b: &mut BitPumpMsb, k: u32, escape: u32, escape_bits: u32) -> u32 {
        let mut z = 0;
        while b.get(1) == 0 {
            z += 1;
            if b.exhausted() {
                return z;
            }
        }
        if z >= escape {
            b.get(escape_bits)
        } else if k != 0 {
            b.get(k) | (z << k)
        } else {
            z
        }
    }

    #[test]
    fn fast_path_matches_reference() {
        let mut seed = 99u32;
        let data: Vec<u8> = (0..4096)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                // Mix sparse (long zero runs) and dense regions.
                if (i / 64) % 3 == 0 { ((seed >> 24) as u8) & ((seed >> 16) as u8) & 0x11 } else { (seed >> 24) as u8 }
            })
            .collect();
        let mut a = CrxBits::new(&data);
        let mut b = BitPumpMsb::new(&data);
        let mut k = 0u32;
        for i in 0..6000 {
            if b.byte_position() + 16 > data.len() {
                break;
            }
            let (esc, eb) = if i % 5 == 0 { (23, 8) } else { (41, 21) };
            let x = a.code(k, esc, eb);
            let y = reference_code(&mut b, k, esc, eb);
            assert_eq!(x, y, "symbol {i}");
            if i % 7 == 0 {
                assert_eq!(a.bit(), b.get(1) != 0);
            }
            k = (x % 16).min(15);
            if b.exhausted() {
                break;
            }
        }
    }

    #[test]
    fn zero_stream_terminates() {
        let data = [0u8; 40];
        let mut a = CrxBits::new(&data);
        let n = a.zeros();
        assert!(n >= 320);
        assert!(a.exhausted());
        let _ = a.code(3, 41, 21);
    }
}
