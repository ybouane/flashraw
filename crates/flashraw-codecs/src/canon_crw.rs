//! Canon CRW compressed raw payload: sample-domain block Huffman coding with a
//! per-parity running predictor and an optional plane of uncompressed low bits.
//!
//! The entropy-coded stream is a JPEG-style bit string (byte stuffing `FF 00`) whose 64-entry
//! "blocks" are 64 consecutive samples of an 8-row stripe, coded as differences.  Two canonical
//! Huffman tables are used: one for the first symbol of a block, one for the rest.

use flashraw_core::{Error, Result};
use std::sync::OnceLock;

/// Parameters of a CRW compressed payload.
#[derive(Debug, Clone, Copy)]
pub struct CrwParams<'a> {
    /// Buffer width in samples.
    pub width: usize,
    /// Buffer height in samples.
    pub height: usize,
    /// Decoder-table id from the CIFF record 0x1835; values above 2 select table set 2.
    pub table: u32,
    /// Uncompressed low-bit plane (4 samples per byte), when the file has one.
    pub lowbits: Option<&'a [u8]>,
    /// Width-2672 sensors: add 2 to merged 12-bit values below 512.
    pub low_offset_quirk: bool,
}

/// A Huffman table in JPEG DHT form.
struct DhtSpec {
    counts: [u8; 16],
    symbols: &'static [u8],
}

const FIRST_TABLES: [DhtSpec; 3] = [
    DhtSpec {
        counts: [0, 1, 4, 2, 3, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        symbols: &[4, 3, 5, 6, 2, 7, 1, 8, 9, 0, 10, 11, 255],
    },
    DhtSpec {
        counts: [0, 2, 2, 3, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0],
        symbols: &[3, 2, 4, 1, 5, 0, 6, 7, 9, 8, 10, 11, 255],
    },
    DhtSpec {
        counts: [0, 0, 6, 3, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        symbols: &[6, 5, 7, 4, 8, 3, 9, 2, 0, 10, 1, 11, 255],
    },
];
const SECOND_TABLES: [DhtSpec; 3] = [
    DhtSpec {
        counts: [0, 2, 2, 2, 1, 4, 2, 1, 2, 5, 1, 1, 0, 0, 0, 139],
        symbols: &[
            3, 4, 2, 5, 1, 6, 7, 8, 18, 19, 17, 20, 9, 21, 34, 0, 33, 22, 10, 240, 35, 23, 36, 49, 50, 24, 25, 51, 37,
            65, 52, 66, 53, 81, 54, 55, 56, 41, 121, 38, 26, 57, 86, 87, 40, 39, 82, 85, 88, 67, 118, 89, 119, 84, 97,
            249, 113, 120, 117, 150, 151, 73, 183, 83, 215, 116, 182, 152, 71, 72, 149, 105, 153, 145, 250, 184, 104,
            181, 185, 214, 247, 216, 103, 70, 69, 148, 137, 248, 129, 213, 246, 180, 136, 177, 42, 68, 114, 217, 135,
            102, 212, 245, 58, 167, 115, 169, 168, 134, 98, 199, 101, 200, 201, 161, 244, 209, 233, 90, 146, 133, 166,
            231, 147, 232, 193, 198, 122, 100, 225, 74, 106, 230, 179, 241, 211, 165, 138, 178, 154, 186, 132, 164, 99,
            229, 197, 243, 210, 196, 130, 170, 218, 228, 242, 202, 131, 163, 162, 195, 234, 194, 226, 227,
        ],
    },
    DhtSpec {
        counts: [0, 2, 2, 1, 4, 1, 4, 1, 3, 3, 1, 0, 0, 0, 0, 140],
        symbols: &[
            2, 3, 1, 4, 5, 18, 17, 6, 19, 7, 8, 20, 34, 9, 33, 0, 35, 21, 49, 50, 10, 22, 240, 36, 51, 65, 66, 25, 23,
            37, 24, 81, 52, 67, 82, 41, 53, 97, 57, 113, 98, 54, 83, 38, 56, 26, 55, 129, 39, 145, 121, 85, 69, 40,
            114, 89, 161, 177, 68, 105, 84, 88, 209, 250, 87, 225, 241, 185, 73, 71, 99, 106, 249, 86, 70, 168, 42, 74,
            120, 153, 58, 117, 116, 134, 101, 193, 118, 182, 150, 214, 137, 133, 201, 245, 149, 180, 199, 247, 138,
            151, 184, 115, 183, 216, 217, 135, 167, 122, 72, 130, 132, 234, 244, 166, 197, 90, 148, 164, 198, 146, 195,
            104, 181, 200, 228, 229, 230, 233, 162, 163, 227, 194, 102, 103, 147, 170, 212, 213, 231, 248, 136, 154,
            215, 119, 196, 100, 226, 152, 165, 202, 218, 232, 243, 246, 169, 178, 179, 242, 210, 131, 186, 211,
        ],
    },
    DhtSpec {
        counts: [0, 0, 6, 2, 1, 3, 3, 2, 5, 1, 2, 2, 8, 10, 0, 117],
        symbols: &[
            4, 5, 3, 6, 2, 7, 1, 8, 9, 18, 19, 20, 17, 21, 10, 22, 23, 240, 0, 34, 33, 24, 35, 25, 36, 50, 49, 37, 51,
            56, 55, 52, 53, 54, 57, 121, 87, 88, 89, 40, 86, 120, 39, 65, 41, 119, 38, 66, 118, 153, 26, 85, 152, 151,
            249, 72, 84, 150, 137, 71, 183, 73, 250, 117, 104, 182, 103, 105, 185, 184, 216, 82, 215, 136, 181, 116,
            81, 70, 217, 248, 58, 214, 135, 69, 122, 149, 213, 246, 134, 180, 169, 148, 83, 42, 168, 67, 245, 247, 212,
            102, 167, 90, 68, 138, 201, 232, 200, 231, 154, 106, 115, 74, 97, 199, 244, 198, 101, 233, 114, 230, 113,
            145, 147, 166, 218, 146, 133, 98, 243, 197, 178, 164, 132, 186, 100, 165, 179, 210, 129, 229, 211, 170,
            196, 202, 242, 177, 228, 209, 131, 99, 234, 195, 226, 130, 241, 163, 194, 161, 193, 227, 162, 225,
        ],
    },
];

/// Bits looked up in one step.
const FAST_BITS: u32 = 11;
/// Entry flag: end of block (second table, symbol 0, or an unassigned pattern).
const F_EOB: u32 = 1 << 19;
/// Entry flag: symbol 0xFF (no difference bits, one position).
const F_SKIP: u32 = 1 << 20;
/// Entry flag: the code is longer than `FAST_BITS`; use the slow path.
const F_LONG: u32 = 1 << 21;
/// Truncation tolerance, in bits read past the end of the usable data.
const MAX_OVERRUN_BITS: u64 = 72;

/// Decoding tables for one Huffman code.
struct Huff {
    /// Indexed by the next `bits` bits. Entry layout: bits 0-5 code plus difference length, 6-10 code
    /// length, 11-14 difference length, 15-18 run, 19-21 flags, 22-31 inline difference (fused entries).
    fast: Box<[u32; 1 << FAST_BITS]>,
    /// Codes longer than `bits`: table indexed by `16-bit prefix - long_base` (these codes form the
    /// tail of the code space). Unassigned patterns map to an end-of-block entry of length 0.
    long: Vec<u32>,
    long_base: u32,
}

fn entry(sym: u8, code_len: u32, second: bool) -> u32 {
    if second && sym == 0 {
        return code_len | (code_len << 6) | F_EOB;
    }
    if sym == 0xFF {
        return code_len | (code_len << 6) | F_SKIP;
    }
    let dlen = sym as u32 & 15;
    (code_len + dlen) | (code_len << 6) | (dlen << 11) | ((sym as u32 >> 4) << 15)
}

impl Huff {
    fn build(spec: &DhtSpec, second: bool) -> Huff {
        let maxlen = (1..=16u32).rev().find(|&l| spec.counts[l as usize - 1] > 0).unwrap_or(1);
        let bits = FAST_BITS;
        let fast: Box<[u32; 1 << FAST_BITS]> =
            vec![F_EOB | F_LONG; 1 << FAST_BITS].into_boxed_slice().try_into().expect("table size");
        let mut h = Huff { fast, long: Vec::new(), long_base: 0 };
        let mut code = 0u32;
        let mut k = 0usize;
        for len in 1..=16u32 {
            let n = spec.counts[len as usize - 1] as u32;
            if len == bits + 1 && maxlen > bits {
                // Everything from here on lies in the tail of the code space.
                h.long_base = code << (16 - len);
                h.long = vec![F_EOB; (1usize << 16) - h.long_base as usize];
            }
            for i in 0..n {
                let sym = spec.symbols[k + i as usize];
                let e = entry(sym, len, second);
                if len > bits {
                    let at = ((code + i) << (16 - len)) as usize - h.long_base as usize;
                    for slot in &mut h.long[at..at + (1usize << (16 - len))] {
                        *slot = e;
                    }
                    continue;
                }
                let c = (code + i) << (bits - len);
                let dlen = (e >> 11) & 15;
                let plain = e & (F_EOB | F_SKIP) == 0;
                if plain && dlen > 0 && len + dlen <= bits && dlen <= 9 {
                    // One slot per value of the difference bits.
                    let step = 1usize << (bits - len - dlen);
                    for v in 0..(1u32 << dlen) {
                        let half = 1u32 << (dlen - 1);
                        let diff = if v < half { v as i32 + 1 - (1i32 << dlen) } else { v as i32 };
                        let fe = (len + dlen) | (e & (15 << 15)) | ((diff as u32) << 22);
                        let at = c as usize + v as usize * step;
                        for slot in &mut h.fast[at..at + step] {
                            *slot = fe;
                        }
                    }
                } else {
                    for slot in &mut h.fast[c as usize..c as usize + (1usize << (bits - len))] {
                        *slot = e;
                    }
                }
            }
            k += n as usize;
            code = (code + n) << 1;
        }
        h
    }

    /// Entry for a code longer than `bits` starting at the top of `acc`; an unassigned pattern ends
    /// the block without consuming bits.
    #[inline(always)]
    fn slow(&self, acc: u64) -> u32 {
        let p = (acc >> 48) as usize;
        match p.checked_sub(self.long_base as usize) {
            Some(i) => self.long.get(i).copied().unwrap_or(F_EOB),
            None => F_EOB,
        }
    }
}

struct TableSet {
    first: Huff,
    second: Huff,
}

fn table_sets() -> &'static [TableSet; 3] {
    static SETS: OnceLock<[TableSet; 3]> = OnceLock::new();
    SETS.get_or_init(|| {
        std::array::from_fn(|i| TableSet {
            first: Huff::build(&FIRST_TABLES[i], false),
            second: Huff::build(&SECOND_TABLES[i], true),
        })
    })
}

/// Bytes of de-stuffed data buffered at a time.
const CHUNK: usize = 1 << 15;
/// Size of the de-stuffing buffer (a power of two so that load offsets can be masked).
const BUF_LEN: usize = 1 << 16;

const LO: u64 = 0x0101_0101_0101_0101;

/// Hot reader state, kept by value so that it lives in registers.
#[derive(Clone, Copy)]
struct State {
    /// Left-aligned bit buffer. Bits below the `n` valid ones are either zero or the true following
    /// stream bits.
    acc: u64,
    n: u32,
    /// Next byte of the de-stuffed buffer to load, and the number of valid bytes in it.
    pos: usize,
    blen: usize,
}

/// Where de-stuffed bytes come from.
struct Feed<'a> {
    src: &'a [u8],
    spos: usize,
    /// No more data can be taken from `src`.
    ended: bool,
    /// Zero bits appended after the end of the data.
    pad: u64,
}

impl Feed<'_> {
    /// Move the unread bytes to the front of `buf` and de-stuff more input behind them.
    fn fill(&mut self, buf: &mut [u8; BUF_LEN], st: &mut State) {
        buf.copy_within(st.pos..st.blen, 0);
        st.blen -= st.pos;
        st.pos = 0;
        let (src, mut sp, mut bl) = (self.src, self.spos, st.blen);
        while !self.ended && bl < CHUNK {
            if bl + 64 <= CHUNK {
                if let Some(c) = src.get(sp..sp + 64) {
                    // Vectorises: no 0xFF anywhere in the 64 bytes.
                    if !c.iter().fold(false, |a, &b| a | (b == 0xFF)) {
                        buf[bl..bl + 64].copy_from_slice(c);
                        bl += 64;
                        sp += 64;
                        continue;
                    }
                }
            }
            if let Some(c) = src.get(sp..sp + 8) {
                let w = u64::from_be_bytes(c.try_into().unwrap());
                if bl + 8 <= CHUNK && (!w).wrapping_sub(LO) & w & (LO << 7) == 0 {
                    buf[bl..bl + 8].copy_from_slice(c);
                    bl += 8;
                    sp += 8;
                    continue;
                }
            }
            match src.get(sp) {
                Some(&0xFF) if src.get(sp + 1) == Some(&0) => {
                    buf[bl] = 0xFF;
                    bl += 1;
                    sp += 2;
                }
                Some(&0xFF) | None => self.ended = true,
                Some(&b) => {
                    buf[bl] = b;
                    bl += 1;
                    sp += 1;
                }
            }
        }
        self.spos = sp;
        st.blen = bl;
    }
}

#[cold]
#[inline(never)]
fn refill_cold(buf: &mut [u8; BUF_LEN], feed: &mut Feed, mut st: State) -> State {
    if !feed.ended && st.blen - st.pos < CHUNK / 2 {
        feed.fill(buf, &mut st);
    }
    if st.pos + 8 <= st.blen {
        let w = u64::from_be_bytes(buf[st.pos..st.pos + 8].try_into().unwrap());
        st.acc |= w >> st.n;
        let k = (63 - st.n) >> 3;
        st.pos += k as usize;
        st.n += k * 8;
        return st;
    }
    // Fewer than eight bytes left: take them one at a time, then zero padding.
    st.acc = if st.n == 0 { 0 } else { st.acc & (!0u64 << (64 - st.n)) };
    while st.n <= 56 {
        let b = if st.pos < st.blen {
            st.pos += 1;
            buf[st.pos - 1]
        } else {
            feed.pad += 8;
            0
        };
        st.acc |= (b as u64) << (56 - st.n);
        st.n += 8;
    }
    st
}

/// MSB-first reader over a byte-stuffed stream, de-stuffed chunk by chunk into a fixed buffer. Past
/// the end of the data (end of slice or a marker) it yields zero bits and counts them.
struct Bits<'a, 'b> {
    st: State,
    buf: &'b mut [u8; BUF_LEN],
    feed: Feed<'a>,
}

impl<'a, 'b> Bits<'a, 'b> {
    fn new(src: &'a [u8], buf: &'b mut [u8; BUF_LEN]) -> Self {
        Bits { st: State { acc: 0, n: 0, pos: 0, blen: 0 }, buf, feed: Feed { src, spos: 0, ended: false, pad: 0 } }
    }

    /// Make at least 56 bits available (zero bits after the end of the data).
    #[inline(always)]
    fn refill(&mut self) {
        let st = &mut self.st;
        if st.pos + 8 <= st.blen {
            // `pos + 8 <= blen <= CHUNK`, so the mask changes nothing but lets the compiler drop the bounds check.
            let i = st.pos & (CHUNK - 1);
            let w = u64::from_be_bytes(self.buf[i..i + 8].try_into().unwrap());
            st.acc |= w >> st.n;
            let k = (63 - st.n) >> 3;
            st.pos += k as usize;
            st.n += k * 8;
        } else {
            self.st = refill_cold(self.buf, &mut self.feed, self.st);
        }
    }

    #[inline(always)]
    fn consume(&mut self, k: u32) {
        self.st.acc <<= k;
        self.st.n -= k;
    }

    /// Look up the next symbol without consuming it.
    #[inline(always)]
    fn symbol(&self, t: &Huff) -> u32 {
        let e = t.fast[(self.st.acc >> (64 - FAST_BITS)) as usize];
        if e & F_LONG != 0 { t.slow(self.st.acc) } else { e }
    }

    /// Like `symbol`, but the result may be a long code still to be resolved (`F_LONG`).
    #[inline(always)]
    fn peek(&self, t: &Huff) -> u32 {
        t.fast[(self.st.acc >> (64 - FAST_BITS)) as usize]
    }

    /// Consume the code of symbol `e` and the difference bits that follow it, and return the
    /// difference (sign extension of ITU-T T.81 F.1.2.1.1).
    #[inline(always)]
    fn take(&mut self, e: u32) -> u16 {
        let clen = (e >> 6) & 31;
        let dlen = (e >> 11) & 15;
        let v = (((self.st.acc << clen) >> 32) >> (32 - dlen)) as u32;
        self.consume(e & 63);
        let half = (1u32 << dlen) >> 1;
        let d = if v < half { v.wrapping_add(1).wrapping_sub(1 << dlen) as u16 } else { v as u16 };
        // Fused entries carry the value inline and have a zero difference length.
        d.wrapping_add(((e as i32) >> 22) as u16)
    }

    /// Bits consumed beyond the end of the data.
    #[inline(always)]
    fn overrun(&self) -> u64 {
        self.feed.pad.saturating_sub(self.st.n as u64)
    }
}

/// Decode one block of 64 differences into `d` (which must be zero on entry).
#[inline(always)]
fn read_block(br: &mut Bits, t: &TableSet, d: &mut [u16; 64]) {
    br.refill();
    let e = br.symbol(&t.first);
    let mut c = 0usize;
    if e & F_SKIP == 0 {
        // `F_EOB` cannot come from a complete first table; if it does the block simply has no data.
        c = (e >> 15 & 15) as usize;
        let v = br.take(e);
        if c < 64 {
            d[c] = v;
        }
    } else {
        br.consume(e & 63);
    }
    c += 1;
    // One symbol of the second table; `continue`/`break` act on the loop below.
    macro_rules! step {
        () => {
            let mut e = br.peek(&t.second);
            if e & F_LONG != 0 {
                e = t.second.slow(br.st.acc);
            }
            if e & (F_EOB | F_SKIP) != 0 {
                br.consume(e & 63);
                if e & F_EOB != 0 {
                    break;
                }
                c += 1;
                continue;
            }
            c += (e >> 15 & 15) as usize;
            let v = br.take(e);
            if c < 64 {
                d[c] = v;
            }
            c += 1;
        };
    }
    // After a refill at least 56 bits are buffered: a second symbol (at most 31 bits) needs a
    // refill only when the first one was long.
    while c < 64 {
        br.refill();
        step!();
        if c >= 64 {
            break;
        }
        if br.st.n < 32 {
            br.refill();
        }
        step!();
        if c >= 64 {
            break;
        }
        if br.st.n < 32 {
            br.refill();
        }
        step!();
    }
}

/// Decode the entropy-coded stream into `out` (`width * height` samples, row-major).
pub fn decode(stream: &[u8], p: &CrwParams, out: &mut [u16]) -> Result<()> {
    let (w, h) = (p.width, p.height);
    if w == 0 || h == 0 {
        return Err(Error::InvalidArgument("CRW: zero width or height".into()));
    }
    let total = w.checked_mul(h).ok_or_else(|| Error::corrupt("CRW: image size overflows"))?;
    if out.len() < total {
        return Err(Error::InvalidArgument("CRW: output buffer too small".into()));
    }
    let out = &mut out[..total];
    let sets = table_sets();
    let t = &sets[(p.table as usize).min(2)];
    let mut buf = vec![0u8; BUF_LEN].into_boxed_slice();
    let buf: &mut [u8; BUF_LEN] = (&mut *buf).try_into().expect("buffer length");
    let mut br = Bits::new(stream, buf);
    let mut dc_total = 0u16;
    let mut row = 0usize;
    while row < h {
        let rows = (h - row).min(8);
        let m = rows * w;
        let start = row * w;
        let stripe = &mut out[start..start + m];
        let mut col = 0usize;
        let (mut p0, mut p1) = (512u16, 512u16);
        for blk in stripe.chunks_exact_mut(64) {
            let mut d = [0u16; 64];
            read_block(&mut br, t, &mut d);
            dc_total = dc_total.wrapping_add(d[0]);
            d[0] = dc_total;
            if br.feed.pad != 0 && br.overrun() > MAX_OVERRUN_BITS {
                return Err(Error::corrupt("CRW: entropy-coded data ends early"));
            }
            if col + 64 <= w {
                if col == 0 {
                    p0 = 512;
                    p1 = 512;
                }
                (p0, p1) = running_sums(blk, &d, p0, p1);
                col += 64;
                if col == w {
                    col = 0;
                }
            } else if w >= 64 && w % 2 == 0 {
                // The block crosses one row boundary at an even offset: two runs, the second
                // restarting from the initial predictors.
                let cut = w - col;
                running_sums(&mut blk[..cut], &d[..cut], p0, p1);
                (p0, p1) = running_sums(&mut blk[cut..], &d[cut..], 512, 512);
                col = 64 - cut;
            } else {
                // The block crosses a row boundary: restart the predictors at the right sample.
                for (k, (o, dd)) in blk.iter_mut().zip(d.iter()).enumerate() {
                    if col == 0 {
                        p0 = 512;
                        p1 = 512;
                    }
                    let q = if k & 1 == 0 { &mut p0 } else { &mut p1 };
                    *q = q.wrapping_add(*dd);
                    *o = *q;
                    col += 1;
                    if col == w {
                        col = 0;
                    }
                }
            }
        }
        if let Some(plane) = p.lowbits {
            merge_lowbits(stripe, start, plane, p.low_offset_quirk);
        }
        row += rows;
    }
    Ok(())
}

/// `out[k] = pred[k % 2] += d[k]` over an even number of samples; returns the final predictors.
#[inline(always)]
fn running_sums(out: &mut [u16], d: &[u16], mut p0: u16, mut p1: u16) -> (u16, u16) {
    for (o, dd) in out.chunks_exact_mut(2).zip(d.chunks_exact(2)) {
        p0 = p0.wrapping_add(dd[0]);
        p1 = p1.wrapping_add(dd[1]);
        o[0] = p0;
        o[1] = p1;
    }
    (p0, p1)
}

/// Merge the two low bits of every sample of `s` (global index `base + i`) from `plane`.
fn merge_lowbits(s: &mut [u16], base: usize, plane: &[u8], quirk: bool) {
    let add: u16 = if quirk { 2 } else { 0 };
    let slow = |s: &mut [u16], base: usize| {
        for (i, v) in s.iter_mut().enumerate() {
            let n = base + i;
            let bits = (plane.get(n >> 2).copied().unwrap_or(0xFF) >> ((n & 3) * 2)) & 3;
            let m = (*v << 2) | bits as u16;
            *v = m + add * (m < 512) as u16;
        }
    };
    let head = ((4 - (base & 3)) & 3).min(s.len());
    let (a, rest) = s.split_at_mut(head);
    slow(a, base);
    let first = (base + head) >> 2;
    let groups = (rest.len() / 4).min(plane.len().saturating_sub(first));
    let (body, tail) = rest.split_at_mut(groups * 4);
    for (q, &pb) in body.chunks_exact_mut(4).zip(&plane[first.min(plane.len())..]) {
        for (j, v) in q.iter_mut().enumerate() {
            let m = (*v << 2) | ((pb >> (2 * j)) & 3) as u16;
            *v = m + add * (m < 512) as u16;
        }
    }
    slow(tail, base + head + groups * 4);
}

/// Whether the file stores a low-bit plane in front of the entropy-coded stream.
///
/// The compressed stream never holds `FF` followed by a non-zero byte, a plane of 2-bit pairs does.
pub fn has_lowbits(file: &[u8]) -> bool {
    let end = file.len().min(16384);
    let mut stuffed = false;
    let mut any_ff = false;
    let mut i = 540;
    while i + 1 < end {
        if file[i] == 0xFF {
            any_ff = true;
            if file[i + 1] != 0 {
                return true;
            }
            stuffed = true;
        }
        i += 1;
    }
    !stuffed && !any_ff
}
