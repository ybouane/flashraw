//! High-throughput bit readers ("bit pumps").
//!
//! RAW codecs consume bit streams in several orders:
//!
//! | Pump        | Bit order inside a word                    | Used by (examples)                         |
//! |-------------|--------------------------------------------|--------------------------------------------|
//! | [`BitPumpMsb`]   | MSB-first, bytes in order              | Nikon, Pentax, Canon CRW/CRX, packed BE    |
//! | [`BitPumpJpeg`]  | MSB-first with `FF 00` unstuffing      | lossless JPEG (CR2, DNG, Sony, Kodak...)  |
//! | [`BitPumpLsb`]   | LSB-first, bytes in order              | Panasonic, Olympus? (no), packed LE        |
//! | [`BitPumpMsb16`] | MSB-first inside 16-bit LE words       | Sony/Olympus packed variants, Fuji        |
//! | [`BitPumpMsb32`] | MSB-first inside 32-bit LE words       | Samsung, Phase One, Hasselblad packed      |
//!
//! All pumps keep a 64-bit cache. The hot refill reads 8 bytes at once and tops the
//! cache up to at least 56 valid bits without per-byte branches; the last few bytes
//! of a stream go through a bounds-checked slow path that pads with zeros. Reading
//! past the end never panics: it yields zero bits and increments an overrun counter
//! that decoders check (see [`BitRead::overrun`]) to reject truncated/corrupt data
//! instead of looping forever.

/// Common interface of all bit pumps.
pub trait BitRead {
    /// Ensure at least 32 bits are buffered (zero-padded past the end of input).
    fn fill(&mut self);
    /// Peek `n <= 32` bits. Requires a prior [`BitRead::fill`].
    fn peek_no_fill(&self, n: u32) -> u32;
    /// Discard `n <= 32` bits. Requires that `n` bits are buffered.
    fn skip_no_fill(&mut self, n: u32);
    /// Number of bytes consumed from the input (bits not yet used are excluded).
    fn byte_position(&self) -> usize;
    /// Bytes requested past the end of the input (0 for a well-formed stream). Includes
    /// look-ahead that has not been consumed; see [`BitRead::past_end`].
    fn overrun(&self) -> usize;
    /// Bits currently buffered (not yet consumed).
    fn buffered(&self) -> u32;

    /// Bytes *consumed* beyond the end of the input (padding fed but still buffered is
    /// not counted). Decoders use this to detect truncated streams precisely.
    #[inline]
    fn past_end(&self) -> usize {
        self.overrun().saturating_sub(self.buffered() as usize / 8)
    }

    /// Ensure at least `n` bits (`n <= 56`) are buffered (zero padding past the end).
    #[inline(always)]
    fn fill_to(&mut self, n: u32) {
        if self.buffered() < n {
            self.refill_force();
        }
    }
    /// Refill to the pump's maximum fill level even if 32 or more bits are buffered.
    fn refill_force(&mut self);

    #[inline(always)]
    fn peek(&mut self, n: u32) -> u32 {
        self.fill();
        self.peek_no_fill(n)
    }
    #[inline(always)]
    fn get(&mut self, n: u32) -> u32 {
        self.fill();
        let v = self.peek_no_fill(n);
        self.skip_no_fill(n);
        v
    }
    #[inline(always)]
    fn skip(&mut self, mut n: u32) {
        while n > 32 {
            self.fill();
            self.skip_no_fill(32);
            n -= 32;
        }
        self.fill();
        self.skip_no_fill(n);
    }
    /// Read a single bit.
    #[inline(always)]
    fn bit(&mut self) -> u32 {
        self.get(1)
    }
    /// True if the stream ran (meaningfully) past the end of its data.
    #[inline]
    fn exhausted(&self) -> bool {
        // Up to 8 bytes of padding are normal look-ahead; anything beyond means
        // the decoder consumed bits that do not exist.
        self.overrun() > 8
    }
}

#[inline(always)]
fn mask(n: u32) -> u64 {
    (1u64 << n) - 1
}

macro_rules! common_fields {
    () => {
        /// Create a pump over `data`.
        pub fn new(data: &'a [u8]) -> Self {
            Self { data, pos: 0, cache: 0, bits: 0, overrun: 0 }
        }
        /// Remaining unread input (from the byte position of the refill cursor).
        pub fn data(&self) -> &'a [u8] {
            self.data
        }
        /// Bits currently buffered.
        #[inline(always)]
        pub fn buffered_bits(&self) -> u32 {
            self.bits
        }
    };
}

/// MSB-first bit pump (big-endian bit order).
#[derive(Clone, Copy)]
pub struct BitPumpMsb<'a> {
    data: &'a [u8],
    pos: usize,
    /// Valid bits are left-aligned (bit 63 is the next bit).
    cache: u64,
    bits: u32,
    overrun: usize,
}

impl<'a> BitPumpMsb<'a> {
    common_fields!();

    /// The end-of-input refill. Takes and returns the pump by value so a pump held in
    /// a local stays in registers in hot loops (a `&mut self` call would pin it to memory).
    #[cold]
    #[inline(never)]
    fn filled_slow(mut self) -> Self {
        self.fill_slow();
        self
    }

    fn fill_slow(&mut self) {
        while self.bits <= 56 {
            let b = match self.data.get(self.pos) {
                Some(&b) => b,
                None => {
                    self.overrun += 1;
                    0
                }
            };
            self.pos += 1;
            self.cache |= (b as u64) << (56 - self.bits);
            self.bits += 8;
        }
    }

    /// Discard bits up to the next byte boundary.
    pub fn align_byte(&mut self) {
        let r = self.bits % 8;
        self.skip_no_fill(r);
    }

    /// Bits consumed from the start of the input (padding past the end included).
    #[inline(always)]
    pub fn bit_position(&self) -> u64 {
        self.pos as u64 * 8 - self.bits as u64
    }

    /// A pump positioned `bit` bits into `data`.
    pub fn at_bit(data: &'a [u8], bit: u64) -> Self {
        let mut p = Self::new(data.get((bit / 8) as usize..).unwrap_or(&[]));
        p.fill();
        p.skip_no_fill((bit % 8) as u32);
        p.pos += (bit / 8) as usize;
        p.data = data;
        p
    }
}

impl BitRead for BitPumpMsb<'_> {
    #[inline(always)]
    fn fill(&mut self) {
        if self.bits >= 32 {
            return;
        }
        self.refill_force();
    }
    #[inline(always)]
    fn refill_force(&mut self) {
        if let Some(chunk) = self.data.get(self.pos..self.pos + 8) {
            let w = u64::from_be_bytes(chunk.try_into().unwrap());
            self.cache |= w >> self.bits;
            self.pos += ((63 - self.bits) >> 3) as usize;
            self.bits |= 56;
        } else {
            *self = self.filled_slow();
        }
    }
    #[inline(always)]
    fn peek_no_fill(&self, n: u32) -> u32 {
        (self.cache.rotate_left(n) & mask(n)) as u32
    }
    #[inline(always)]
    fn skip_no_fill(&mut self, n: u32) {
        debug_assert!(n <= self.bits);
        self.cache <<= n;
        self.bits -= n;
    }
    fn byte_position(&self) -> usize {
        self.pos - (self.bits as usize >> 3)
    }
    fn overrun(&self) -> usize {
        self.overrun
    }
    fn buffered(&self) -> u32 {
        self.bits
    }
}

/// LSB-first bit pump (little-endian bit order).
#[derive(Clone)]
pub struct BitPumpLsb<'a> {
    data: &'a [u8],
    pos: usize,
    /// Valid bits are right-aligned (bit 0 is the next bit).
    cache: u64,
    bits: u32,
    overrun: usize,
}

impl<'a> BitPumpLsb<'a> {
    common_fields!();

    #[cold]
    fn fill_slow(&mut self) {
        while self.bits <= 56 {
            let b = match self.data.get(self.pos) {
                Some(&b) => b,
                None => {
                    self.overrun += 1;
                    0
                }
            };
            self.pos += 1;
            self.cache |= (b as u64) << self.bits;
            self.bits += 8;
        }
    }
}

impl BitRead for BitPumpLsb<'_> {
    #[inline(always)]
    fn fill(&mut self) {
        if self.bits >= 32 {
            return;
        }
        self.refill_force();
    }
    #[inline(always)]
    fn refill_force(&mut self) {
        if let Some(chunk) = self.data.get(self.pos..self.pos + 8) {
            let w = u64::from_le_bytes(chunk.try_into().unwrap());
            self.cache |= w << self.bits;
            self.pos += ((63 - self.bits) >> 3) as usize;
            self.bits |= 56;
        } else {
            self.fill_slow();
        }
    }
    #[inline(always)]
    fn peek_no_fill(&self, n: u32) -> u32 {
        (self.cache & mask(n)) as u32
    }
    #[inline(always)]
    fn skip_no_fill(&mut self, n: u32) {
        debug_assert!(n <= self.bits);
        self.cache >>= n;
        self.bits -= n;
    }
    fn byte_position(&self) -> usize {
        self.pos - (self.bits as usize >> 3)
    }
    fn overrun(&self) -> usize {
        self.overrun
    }
    fn buffered(&self) -> u32 {
        self.bits
    }
}

/// MSB-first bit order inside little-endian words of `W` bytes (2 or 4).
#[derive(Clone)]
pub struct BitPumpMsbWords<'a, const W: usize> {
    data: &'a [u8],
    pos: usize,
    cache: u64,
    bits: u32,
    overrun: usize,
}

/// MSB-first inside 16-bit little-endian words.
pub type BitPumpMsb16<'a> = BitPumpMsbWords<'a, 2>;
/// MSB-first inside 32-bit little-endian words.
pub type BitPumpMsb32<'a> = BitPumpMsbWords<'a, 4>;

impl<'a, const W: usize> BitPumpMsbWords<'a, W> {
    common_fields!();

    /// Bits consumed from the start of the input (padding past the end included).
    #[inline(always)]
    pub fn bit_position(&self) -> u64 {
        self.pos as u64 * 8 - self.bits as u64
    }

    /// A pump positioned `bit` bits into `data` (word-aligned start, then skipped).
    pub fn at_bit(data: &'a [u8], bit: u64) -> Self {
        let word_bits = 8 * W as u64;
        let base = (bit / word_bits) as usize * W;
        let mut p = Self::new(data.get(base..).unwrap_or(&[]));
        p.fill();
        p.skip_no_fill((bit % word_bits) as u32);
        p.pos += base;
        p.data = data;
        p
    }

    #[cold]
    fn fill_slow(&mut self) {
        while self.bits <= 64 - 8 * W as u32 {
            let mut w: u64 = 0;
            for i in (0..W).rev() {
                let b = match self.data.get(self.pos + i) {
                    Some(&b) => b,
                    None => {
                        self.overrun += 1;
                        0
                    }
                };
                w = (w << 8) | b as u64;
            }
            self.pos += W;
            self.cache |= w << (64 - 8 * W as u32 - self.bits);
            self.bits += 8 * W as u32;
        }
    }
}

impl<const W: usize> BitRead for BitPumpMsbWords<'_, W> {
    #[inline(always)]
    fn fill(&mut self) {
        if self.bits >= 32 {
            return;
        }
        self.refill_force();
    }
    #[inline(always)]
    fn refill_force(&mut self) {
        if let Some(chunk) = self.data.get(self.pos..self.pos + 8) {
            let le = u64::from_le_bytes(chunk.try_into().unwrap());
            // Reorder so the first word occupies the top bits.
            let w = if W == 2 {
                let x = le;
                ((x & 0xFFFF) << 48) | ((x & 0xFFFF_0000) << 16) | ((x >> 16) & 0xFFFF_0000) | (x >> 48)
            } else {
                le.rotate_left(32)
            };
            // Take as many whole words as fit (bits < 32 so at least one fits).
            let words = (64 - self.bits) / (8 * W as u32);
            let take = words * 8 * W as u32;
            let part = if take == 64 { w } else { w >> (64 - take) << (64 - take) };
            self.cache |= part >> self.bits;
            self.bits += take;
            self.pos += (take / 8) as usize;
        } else {
            self.fill_slow();
        }
    }
    #[inline(always)]
    fn peek_no_fill(&self, n: u32) -> u32 {
        (self.cache.rotate_left(n) & mask(n)) as u32
    }
    #[inline(always)]
    fn skip_no_fill(&mut self, n: u32) {
        debug_assert!(n <= self.bits);
        self.cache = if n == 64 { 0 } else { self.cache << n };
        self.bits -= n;
    }
    fn byte_position(&self) -> usize {
        self.pos - (self.bits as usize >> 3)
    }
    fn overrun(&self) -> usize {
        self.overrun
    }
    fn buffered(&self) -> u32 {
        self.bits
    }
}

/// MSB-first pump for JPEG entropy-coded segments: removes `FF 00` byte stuffing and
/// stops (feeding zeros) at a marker (`FF xx`, `xx != 0`).
#[derive(Clone)]
pub struct BitPumpJpeg<'a> {
    data: &'a [u8],
    pos: usize,
    cache: u64,
    bits: u32,
    overrun: usize,
    marker_hit: bool,
}

impl<'a> BitPumpJpeg<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, cache: 0, bits: 0, overrun: 0, marker_hit: false }
    }
    /// Whether a marker terminated the stream.
    pub fn marker_hit(&self) -> bool {
        self.marker_hit
    }
    /// Byte offset of the read cursor in the source (after unstuffing accounting).
    pub fn raw_position(&self) -> usize {
        self.pos
    }

    #[cold]
    fn fill_slow(&mut self) {
        while self.bits <= 56 {
            let mut b = 0u8;
            if !self.marker_hit {
                match self.data.get(self.pos) {
                    Some(&0xFF) => {
                        match self.data.get(self.pos + 1) {
                            Some(&0x00) => {
                                b = 0xFF;
                                self.pos += 2;
                            }
                            _ => {
                                // A marker (or EOF after FF): stop consuming input.
                                self.marker_hit = true;
                                self.overrun += 1;
                            }
                        }
                    }
                    Some(&v) => {
                        b = v;
                        self.pos += 1;
                    }
                    None => {
                        self.marker_hit = true;
                        self.overrun += 1;
                    }
                }
            } else {
                self.overrun += 1;
            }
            self.cache |= (b as u64) << (56 - self.bits);
            self.bits += 8;
        }
    }

    /// Reset to the byte position after a restart marker (`FF D0..D7`), if present.
    pub fn reset_at_restart(&mut self) -> bool {
        // Discard buffered bits; scan forward to the marker.
        let mut p = self.byte_position_raw_estimate();
        while p + 1 < self.data.len() {
            if self.data[p] == 0xFF && (0xD0..=0xD7).contains(&self.data[p + 1]) {
                self.pos = p + 2;
                self.cache = 0;
                self.bits = 0;
                self.marker_hit = false;
                // Zero padding fed while stopped at a restart marker is legitimate,
                // not an overrun of the segment.
                self.overrun = 0;
                return true;
            }
            p += 1;
        }
        false
    }

    fn byte_position_raw_estimate(&self) -> usize {
        // Buffered bytes may include unstuffed FF00 pairs; back up conservatively.
        self.pos.saturating_sub(((self.bits as usize) >> 3) * 2)
    }
}

impl BitRead for BitPumpJpeg<'_> {
    #[inline(always)]
    fn fill(&mut self) {
        if self.bits >= 32 {
            return;
        }
        self.refill_force();
    }
    #[inline(always)]
    fn refill_force(&mut self) {
        if let Some(chunk) = self.data.get(self.pos..self.pos + 8) {
            let w = u64::from_be_bytes(chunk.try_into().unwrap());
            // SWAR test for any 0xFF byte in the word.
            let inv = !w;
            let has_ff = (inv.wrapping_sub(0x0101_0101_0101_0101) & !inv & 0x8080_8080_8080_8080) != 0;
            if !has_ff {
                self.cache |= w >> self.bits;
                self.pos += ((63 - self.bits) >> 3) as usize;
                self.bits |= 56;
                return;
            }
        }
        self.fill_slow();
    }
    #[inline(always)]
    fn peek_no_fill(&self, n: u32) -> u32 {
        (self.cache.rotate_left(n) & mask(n)) as u32
    }
    #[inline(always)]
    fn skip_no_fill(&mut self, n: u32) {
        debug_assert!(n <= self.bits);
        self.cache <<= n;
        self.bits -= n;
    }
    fn byte_position(&self) -> usize {
        self.pos.saturating_sub(self.bits as usize >> 3)
    }
    fn overrun(&self) -> usize {
        self.overrun
    }
    fn buffered(&self) -> u32 {
        self.bits
    }
}

/// Sign-extend a JPEG-style difference value: `len` bits `v` → signed diff.
#[inline(always)]
pub fn extend_diff(v: u32, len: u32) -> i32 {
    if len == 0 {
        return 0;
    }
    let v = v as i32;
    if v & (1 << (len - 1)) == 0 { v - (1 << len) + 1 } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_msb(data: &[u8], widths: &[u32]) -> Vec<u32> {
        let mut out = vec![];
        let mut bitpos = 0usize;
        for &w in widths {
            let mut v = 0u32;
            for _ in 0..w {
                let byte = data.get(bitpos / 8).copied().unwrap_or(0);
                let bit = (byte >> (7 - bitpos % 8)) & 1;
                v = (v << 1) | bit as u32;
                bitpos += 1;
            }
            out.push(v);
        }
        out
    }
    fn ref_lsb(data: &[u8], widths: &[u32]) -> Vec<u32> {
        let mut out = vec![];
        let mut bitpos = 0usize;
        for &w in widths {
            let mut v = 0u32;
            for i in 0..w {
                let byte = data.get(bitpos / 8).copied().unwrap_or(0);
                let bit = (byte >> (bitpos % 8)) & 1;
                v |= (bit as u32) << i;
                bitpos += 1;
            }
            out.push(v);
        }
        out
    }
    fn swap_words(data: &[u8], w: usize) -> Vec<u8> {
        let mut d = data.to_vec();
        d.resize(data.len().div_ceil(w) * w, 0);
        for c in d.chunks_mut(w) {
            c.reverse();
        }
        d
    }

    fn pseudo(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn pumps_match_reference() {
        for seed in 1..20u64 {
            let data = pseudo(97, seed);
            let widths: Vec<u32> = (0..120).map(|i| ((i * 7 + seed as usize) % 33) as u32).collect();
            let mut m = BitPumpMsb::new(&data);
            let got: Vec<u32> = widths.iter().map(|&w| m.get(w)).collect();
            assert_eq!(got, ref_msb(&data, &widths), "msb seed {seed}");
            let mut l = BitPumpLsb::new(&data);
            let got: Vec<u32> = widths.iter().map(|&w| l.get(w)).collect();
            assert_eq!(got, ref_lsb(&data, &widths), "lsb seed {seed}");
            let mut m16 = BitPumpMsb16::new(&data);
            let got: Vec<u32> = widths.iter().map(|&w| m16.get(w)).collect();
            assert_eq!(got, ref_msb(&swap_words(&data, 2), &widths), "msb16 seed {seed}");
            let mut m32 = BitPumpMsb32::new(&data);
            let got: Vec<u32> = widths.iter().map(|&w| m32.get(w)).collect();
            assert_eq!(got, ref_msb(&swap_words(&data, 4), &widths), "msb32 seed {seed}");
        }
    }

    #[test]
    fn jpeg_unstuffing() {
        let data = [0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD9, 0x55];
        let mut p = BitPumpJpeg::new(&data);
        assert_eq!(p.get(8), 0x12);
        assert_eq!(p.get(8), 0xFF);
        assert_eq!(p.get(8), 0x34);
        assert_eq!(p.get(8), 0x00); // marker: zeros
        assert!(p.marker_hit());
        // long stuffed stream through the fast path
        let mut d = pseudo(200, 3);
        for b in d.iter_mut() {
            if *b == 0xFF {
                *b = 0xFE;
            }
        }
        let mut stuffed = vec![];
        for (i, &b) in d.iter().enumerate() {
            stuffed.push(if i % 17 == 0 { 0xFF } else { b });
            if i % 17 == 0 {
                stuffed.push(0);
            }
        }
        let plain: Vec<u8> = d.iter().enumerate().map(|(i, &b)| if i % 17 == 0 { 0xFF } else { b }).collect();
        let widths: Vec<u32> = (0..300).map(|i| (i % 13) as u32 + 1).collect();
        let mut p = BitPumpJpeg::new(&stuffed);
        let got: Vec<u32> = widths.iter().map(|&w| p.get(w)).collect();
        assert_eq!(got, ref_msb(&plain, &widths));
    }

    #[test]
    fn overrun_is_tracked_not_panicking() {
        let data = [0xAAu8; 3];
        let mut p = BitPumpMsb::new(&data);
        for _ in 0..100 {
            p.get(32);
        }
        assert!(p.exhausted());
        let mut p = BitPumpLsb::new(&[]);
        assert_eq!(p.get(32), 0);
        assert!(p.overrun() > 0);
    }

    #[test]
    fn diff_extension() {
        assert_eq!(extend_diff(0, 1), -1);
        assert_eq!(extend_diff(1, 1), 1);
        assert_eq!(extend_diff(0b010, 3), -5);
        assert_eq!(extend_diff(0b110, 3), 6);
        assert_eq!(extend_diff(0, 0), 0);
    }
}
