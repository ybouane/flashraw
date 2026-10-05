//! Olympus / OM System ORF lossless compression.
//!
//! A single MSB-first bit stream codes every sample of the frame as an adaptive
//! Golomb-like residual. Even and odd columns form two interleaved channels: each
//! channel keeps its own adaptation state (previous code magnitude, a run counter
//! of small codes and a smoothed residual), and predicts from same-colour
//! neighbours two pixels to the left (`W`), above (`N`) and diagonal (`NW`).
//!
//! Per sample:
//! 1. sign bit, then `low_bits` (usually 2) literal low bits, then `extra_bits`
//!    (usually 0) bits appended below the reconstructed value;
//! 2. a unary-coded "high" part (number of zeros before a one, escape after
//!    `max_high` zeros to a fixed-width field), then `wbits` "low" bits, where
//!    `wbits` adapts to the magnitude of the previous same-channel code;
//! 3. the code is combined with the smoothed residual and added to the
//!    edge-sensitive median prediction.
//!
//! The camera describes the coder with tags 0x640..0x653 of the makernote
//! ImageProcessing directory ([`OlympusParams`]); every body to date uses the same
//! values, which are also the defaults. The stream is inherently sequential.

use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::{Error, Result};

/// Coder parameters (makernote ImageProcessing tags 0x640..0x653).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OlympusParams {
    /// 0x640: extra literal low bits per sample (value is shifted left by this).
    pub extra_bits: u32,
    /// 0x641: log2 of the gradient threshold of the predictor.
    pub grad_shift: u32,
    /// 0x642: prediction used outside the image (first rows/columns).
    pub edge_pred: i32,
    /// 0x643: literal low bits coded before the adaptive part (0, 1 or 2).
    pub low_bits: u32,
    /// 0x644: smoothing shift of the residual accumulator (15 disables it).
    pub smooth_shift: u32,
    /// 0x645: codes up to `1 << small_shift` count as "small" for the run counter.
    pub small_shift: u32,
    /// 0x646: run length of small codes that switches to the fine width model.
    pub run_switch: i32,
    /// 0x647/0x648: width model after a run (offset, magnitude threshold).
    pub run_offset: i32,
    pub run_threshold: i32,
    /// 0x649/0x650: default width model (offset, magnitude threshold).
    pub base_offset: i32,
    pub base_threshold: i32,
    /// 0x651: maximum adaptive width.
    pub max_wbits: i32,
    /// 0x652: unary escape length.
    pub max_high: u32,
    /// 0x653: length of the big-endian stream header (value 1) preceding two zero bytes.
    pub header_len: u32,
}

impl Default for OlympusParams {
    fn default() -> Self {
        OlympusParams {
            extra_bits: 0,
            grad_shift: 5,
            edge_pred: 0,
            low_bits: 2,
            smooth_shift: 5,
            small_shift: 4,
            run_switch: 3,
            run_offset: 2,
            run_threshold: 3,
            base_offset: 4,
            base_threshold: 7,
            max_wbits: 15,
            max_high: 12,
            header_len: 5,
        }
    }
}

impl OlympusParams {
    /// Build from the 14 tag values 0x640..=0x653 (`None` = default).
    pub fn from_tags(t: &[Option<u32>; 14]) -> Self {
        let d = Self::default();
        let g = |i: usize, def: u32| t[i].unwrap_or(def);
        OlympusParams {
            extra_bits: g(0, d.extra_bits),
            grad_shift: g(1, d.grad_shift),
            edge_pred: g(2, d.edge_pred as u32) as i32,
            low_bits: g(3, d.low_bits),
            smooth_shift: g(4, d.smooth_shift),
            small_shift: g(5, d.small_shift),
            run_switch: g(6, d.run_switch as u32) as i32,
            run_offset: g(7, d.run_offset as u32) as i32,
            run_threshold: g(8, d.run_threshold as u32) as i32,
            base_offset: g(9, d.base_offset as u32) as i32,
            base_threshold: g(10, d.base_threshold as u32) as i32,
            max_wbits: g(11, d.max_wbits as u32) as i32,
            max_high: g(12, d.max_high).max(1),
            header_len: g(13, d.header_len),
        }
    }

    fn validate(&self) -> Result<()> {
        let ok = self.extra_bits <= 4
            && self.grad_shift <= 16
            && self.low_bits <= 2
            && self.small_shift <= 31
            && self.max_high <= 24
            && (1..=8).contains(&self.header_len)
            && (0..=15).contains(&self.max_wbits);
        if ok { Ok(()) } else { Err(Error::unsupported_compression("ORF", format!("coder parameters {self:?}"))) }
    }
}

/// Offset of the bit stream after the header, checking the header contents.
pub fn stream_start(data: &[u8], p: &OlympusParams) -> Result<usize> {
    let n = p.header_len as usize;
    let hdr = data.get(..n + 2).ok_or_else(|| Error::corrupt("ORF: compressed data too short"))?;
    let v = hdr[..n].iter().fold(0u64, |a, &b| a << 8 | b as u64);
    if v != 1 || hdr[n] != 0 || hdr[n + 1] != 0 {
        return Err(Error::corrupt("ORF: bad compressed stream header"));
    }
    Ok(n + 2)
}

/// Per-channel (even/odd column) adaptation state.
#[derive(Clone, Copy, Default)]
struct Chan {
    /// Previous code magnitude (`glc`).
    prev_code: u32,
    /// Run counter of small codes.
    run: i32,
    /// Smoothed residual (`vbits >> smooth_shift` of the previous sample).
    smooth: i32,
    /// Previous reconstructed value of this channel (unshifted, untruncated).
    west: i32,
}

/// Decode a full `width x height` frame into `out` (row-major, `width` stride).
/// `data` starts at the stream header. Returns an error on malformed parameters or
/// a stream that runs out of data.
pub fn decode(data: &[u8], p: &OlympusParams, out: &mut [u16], width: usize, height: usize) -> Result<()> {
    decode_with(data, p, out, width, height, false)
}

/// [`decode`], optionally overlapping entropy decoding and prediction on two threads.
pub fn decode_with(
    data: &[u8],
    p: &OlympusParams,
    out: &mut [u16],
    width: usize,
    height: usize,
    parallel: bool,
) -> Result<()> {
    p.validate()?;
    if out.len() < width * height {
        return Err(Error::InvalidArgument("ORF output buffer too small".into()));
    }
    let start = stream_start(data, p)?;
    let standard = OlympusParams { header_len: p.header_len, ..OlympusParams::default() };
    if *p == standard {
        decode_standard(&data[start..], out, width, height, parallel)
    } else {
        decode_generic(&data[start..], p, out, width, height)
    }
}

/// The coder with the parameters every camera uses, with all of them folded into
/// constants. Each sample needs at most 31 bits, so one 32-bit peek after a refill
/// serves the whole code.
///
/// The bit stream is sequential, but its parsing never depends on reconstructed
/// pixel values: only the prediction does. Decoding therefore runs as two stages —
/// entropy decoding of residual codes for a band of rows, and prediction of a band
/// (two rows at a time, i.e. four independent dependency chains) — which overlap
/// on two threads when `parallel` is set.
fn decode_standard(stream: &[u8], out: &mut [u16], width: usize, height: usize, parallel: bool) -> Result<()> {
    const BAND: usize = 32;
    let mut bits = PosBits::new(stream);
    let mut ch = [EntropyChan::default(); 2];
    let mut entropy_band = |y0: usize, codes: &mut Vec<i32>| -> Result<()> {
        let y1 = (y0 + BAND).min(height);
        codes.clear();
        codes.resize((y1 - y0) * width, 0);
        for (i, row) in codes.chunks_exact_mut(width).enumerate() {
            standard_entropy_row(&mut bits, &mut ch, row);
            if bits.exhausted() {
                return Err(Error::corrupt(format!("ORF: compressed data ends at row {}/{height}", y0 + i)));
            }
        }
        Ok(())
    };
    if width == 0 || height == 0 {
        return Ok(());
    }
    // WASM threads cannot `std::thread::spawn` (the threaded build traps with "failed to spawn
    // thread"), so there the two stages overlap through `rayon::join` instead: entropy-decode band
    // k while predicting band k-1. `join` degrades to sequential execution on a one-thread pool.
    #[cfg(all(feature = "rayon", target_arch = "wasm32"))]
    if parallel && height > BAND {
        let mut cur = Vec::new();
        let mut prev: Option<(usize, Vec<i32>)> = None;
        for y0 in (0..height).step_by(BAND) {
            let (res, done) = rayon::join(
                || entropy_band(y0, &mut cur).map(|_| ()),
                || {
                    if let Some((py, codes)) = prev.take() {
                        predict_band(out, &codes, py, width);
                        Some(codes)
                    } else {
                        None
                    }
                },
            );
            res?;
            let recycled = done.unwrap_or_default();
            prev = Some((y0, std::mem::replace(&mut cur, recycled)));
        }
        if let Some((py, codes)) = prev {
            predict_band(out, &codes, py, width);
        }
        return Ok(());
    }
    // Two OS threads; `std::thread` cannot spawn on wasm32 (its threads are rayon's pool).
    #[cfg(all(feature = "rayon", not(target_arch = "wasm32")))]
    if parallel && height > BAND {
        use std::sync::mpsc;
        return std::thread::scope(|s| {
            let (tx, rx) = mpsc::sync_channel::<(usize, Vec<i32>)>(4);
            let (back_tx, back_rx) = mpsc::channel::<Vec<i32>>();
            let consumer = s.spawn(move || {
                for (y0, codes) in rx {
                    predict_band(out, &codes, y0, width);
                    let _ = back_tx.send(codes);
                }
            });
            let mut result = Ok(());
            for y0 in (0..height).step_by(BAND) {
                let mut buf = back_rx.try_recv().unwrap_or_default();
                if let Err(e) = entropy_band(y0, &mut buf) {
                    result = Err(e);
                    break;
                }
                if tx.send((y0, buf)).is_err() {
                    break;
                }
            }
            drop(tx);
            let _ = consumer.join();
            result
        });
    }
    let _ = parallel;
    let mut buf = Vec::new();
    for y0 in (0..height).step_by(BAND) {
        entropy_band(y0, &mut buf)?;
        predict_band(out, &buf, y0, width);
    }
    Ok(())
}

/// Bit reader addressed by absolute bit position: every peek is one unaligned
/// big-endian 8-byte load, so there is no refill bookkeeping on the critical path
/// (which is one data-dependent bit count per sample).
struct PosBits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> PosBits<'a> {
    fn new(data: &'a [u8]) -> Self {
        PosBits { data, pos: 0 }
    }

    /// Next 32 bits (zeros past the end of the data).
    #[inline(always)]
    fn peek32(&self) -> u32 {
        let byte = self.pos >> 3;
        let w = match self.data.get(byte..byte + 8) {
            Some(b) => u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
            None => self.peek_tail(byte),
        };
        ((w << (self.pos & 7)) >> 32) as u32
    }

    #[cold]
    fn peek_tail(&self, byte: usize) -> u64 {
        let mut b = [0u8; 8];
        if let Some(t) = self.data.get(byte..) {
            b[..t.len().min(8)].copy_from_slice(&t[..t.len().min(8)]);
        }
        u64::from_be_bytes(b)
    }

    #[inline(always)]
    fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    /// True once more than 8 bytes past the end of the data were consumed.
    fn exhausted(&self) -> bool {
        self.pos > (self.data.len() + 8) * 8
    }
}

/// Entropy-side adaptation state of one channel (even or odd columns).
#[derive(Clone, Copy, Default)]
struct EntropyChan {
    prev_code: u32,
    run: i32,
    smooth: i32,
}

/// Decode the residual codes of one row.
#[inline(always)]
fn standard_entropy_row(bits: &mut PosBits, ch: &mut [EntropyChan; 2], codes: &mut [i32]) {
    let width = codes.len();
    let [c0, c1] = ch;
    codes[0] = standard_code::<true>(bits, c0);
    if width > 1 {
        codes[1] = standard_code::<true>(bits, c1);
    }
    let mut col = 2;
    while col + 1 < width {
        codes[col] = standard_code::<false>(bits, c0);
        codes[col + 1] = standard_code::<false>(bits, c1);
        col += 2;
    }
    if col < width {
        codes[col] = standard_code::<false>(bits, c0);
    }
}

/// One residual code with the standard parameters. `EDGE`: first two columns.
#[inline(always)]
fn standard_code<const EDGE: bool>(bits: &mut PosBits, c: &mut EntropyChan) -> i32 {
    let wbits = if EDGE {
        c.run = 0;
        4
    } else {
        let run = if c.prev_code <= 16 { c.run + 1 } else { 0 };
        c.run = run;
        let hb = 32 - (c.prev_code & 0xffff).leading_zeros();
        let w = if run >= 3 { hb.max(2) } else { hb.saturating_sub(2).max(4) };
        w.min(15)
    };
    let v = bits.peek32();
    let sign = v >> 31;
    let low2 = ((v >> 29) & 3) as i32;
    let rest = v << 3;
    let (high, low, used) = if rest >> 20 != 0 {
        let z = rest.leading_zeros();
        let after = rest << (z + 1);
        (z, after >> (32 - wbits), 4 + z + wbits)
    } else {
        // Escape: 12 zeros, a (15 - wbits)-bit high part, one spare bit.
        let after = rest << 12;
        let hn = 15 - wbits;
        let high = if hn == 0 { 0 } else { after >> (32 - hn) };
        let after2 = (after << hn) << 1;
        (high, after2 >> (32 - wbits), 31)
    };
    bits.skip(used);
    let glc = low | (high << wbits);
    c.prev_code = glc;
    let code = (glc ^ sign.wrapping_neg()) as i32;
    let s = if EDGE { 0 } else { c.smooth };
    let code = low2 + 4 * (s + code);
    let vbits = ((code >> 1) & !1) + s + (code >> 2);
    c.smooth = vbits >> 5;
    code
}

/// Reconstruct the rows `y0..` of a band from their residual codes.
fn predict_band(out: &mut [u16], codes: &[i32], y0: usize, width: usize) {
    let rows = codes.len() / width;
    let mut i = 0;
    while i < rows {
        let y = y0 + i;
        if i + 1 < rows && y >= 2 {
            // Rows y and y+1 depend on y-2 and y-1 only: interleave them.
            let (above, cur) = out.split_at_mut(y * width);
            let (r0, r1) = cur[..2 * width].split_at_mut(width);
            let up0 = &above[(y - 2) * width..(y - 1) * width];
            let up1 = &above[(y - 1) * width..y * width];
            predict_two_rows(
                &codes[i * width..(i + 1) * width],
                &codes[(i + 1) * width..(i + 2) * width],
                r0,
                r1,
                up0,
                up1,
            );
            i += 2;
        } else {
            let (above, cur) = out.split_at_mut(y * width);
            let up = if y >= 2 { &above[(y - 2) * width..(y - 1) * width] } else { &[][..] };
            predict_row(&codes[i * width..(i + 1) * width], &mut cur[..width], up);
            i += 1;
        }
    }
}

/// Predict one row (`up` empty for the first two rows of the frame).
fn predict_row(codes: &[i32], cur: &mut [u16], up: &[u16]) {
    let width = cur.len();
    let mut west = [0i32; 2];
    for col in 0..width {
        let (n, nw) = match (up.get(col), col >= 2) {
            (Some(&n), true) => (n as i32, up[col - 2] as i32),
            (Some(&n), false) => (n as i32, 0),
            (None, _) => (0, 0),
        };
        let w = if col >= 2 { west[col & 1] } else { 0 };
        let v = codes[col].wrapping_add(predict(w, n, nw, 32));
        west[col & 1] = v;
        cur[col] = v as u16;
    }
}

/// Predict two consecutive rows with four interleaved dependency chains.
#[inline(never)]
fn predict_two_rows(c0: &[i32], c1: &[i32], r0: &mut [u16], r1: &mut [u16], u0: &[u16], u1: &[u16]) {
    let width = r0.len();
    if width < 4 {
        predict_row(c0, r0, u0);
        predict_row(c1, r1, u1);
        return;
    }
    // Columns 0 and 1: west and north-west are outside the image.
    let mut w = [0i32; 4];
    for k in 0..2 {
        let a = c0[k].wrapping_add(predict(0, u0[k] as i32, 0, 32));
        let b = c1[k].wrapping_add(predict(0, u1[k] as i32, 0, 32));
        w[k] = a;
        w[2 + k] = b;
        r0[k] = a as u16;
        r1[k] = b as u16;
    }
    let mut col = 2;
    while col + 1 < width {
        let a0 = c0[col].wrapping_add(predict(w[0], u0[col] as i32, u0[col - 2] as i32, 32));
        let a1 = c0[col + 1].wrapping_add(predict(w[1], u0[col + 1] as i32, u0[col - 1] as i32, 32));
        let b0 = c1[col].wrapping_add(predict(w[2], u1[col] as i32, u1[col - 2] as i32, 32));
        let b1 = c1[col + 1].wrapping_add(predict(w[3], u1[col + 1] as i32, u1[col - 1] as i32, 32));
        w = [a0, a1, b0, b1];
        r0[col] = a0 as u16;
        r0[col + 1] = a1 as u16;
        r1[col] = b0 as u16;
        r1[col + 1] = b1 as u16;
        col += 2;
    }
    if col < width {
        let a0 = c0[col].wrapping_add(predict(w[0], u0[col] as i32, u0[col - 2] as i32, 32));
        let b0 = c1[col].wrapping_add(predict(w[2], u1[col] as i32, u1[col - 2] as i32, 32));
        r0[col] = a0 as u16;
        r1[col] = b0 as u16;
    }
}

/// Edge-sensitive predictor from the west, north and north-west same-colour samples.
///
/// Written as data-independent selects: the decision pattern changes from pixel to
/// pixel, so branches would mispredict constantly.
#[inline(always)]
fn predict(w: i32, n: i32, nw: i32, grad: i32) -> i32 {
    let a = (w < n) | (nw < w);
    let b = (nw <= n) & (w >= n);
    let c = (nw < n) | (w >= n);
    let d = (nw > w) | (w >= n);
    let smooth = ((n - nw).abs() <= grad) & ((w - nw).abs() <= grad);
    let g = if smooth { (n + w) >> 1 } else { n + w - nw };
    let use_w = a & (b | !c);
    let use_g = a & !b & c & d;
    let r = if use_g { g } else { n };
    if use_w { w } else { r }
}

/// Branchy form of [`predict`], kept as the specification.
#[cfg(test)]
fn predict_reference(w: i32, n: i32, nw: i32, grad: i32) -> i32 {
    let mut n = n;
    if w < n || nw < w {
        if nw <= n && w >= n {
            n = w;
        } else if nw < n || w >= n {
            if nw > w || w >= n {
                if (n - nw).abs() > grad {
                    n += w - nw;
                } else if (w - nw).abs() <= grad {
                    n = (n + w) >> 1;
                } else {
                    n = n + w - nw;
                }
            }
        } else {
            n = w;
        }
    }
    n
}

/// Any parameter set (slower: parameters are runtime values).
fn decode_generic(stream: &[u8], p: &OlympusParams, out: &mut [u16], width: usize, height: usize) -> Result<()> {
    let mut bits = BitPumpMsb::new(stream);
    let small_limit = 1u32 << (p.small_shift & 31);
    let grad = 1i32 << p.grad_shift;
    let extra = p.extra_bits;
    let mut ch = [Chan::default(); 2];
    for row in 0..height {
        let (above, cur) = out.split_at_mut(row * width);
        let cur = &mut cur[..width];
        let up = if row >= 2 { Some(&above[(row - 2) * width..(row - 1) * width]) } else { None };
        for col in 0..width {
            let c = &mut ch[col & 1];
            // Adaptive width of the low part from the previous same-channel code.
            let (wbits, run) = if col > 1 {
                let run = if small_limit >= c.prev_code { c.run + 1 } else { 0 };
                let hb = 32 - (c.prev_code & 0xffff).leading_zeros() as i32;
                let (t1, t2) = if run >= p.run_switch {
                    (p.run_threshold, p.run_offset)
                } else {
                    (p.base_threshold, p.base_offset)
                };
                (t2 + (hb - t1).max(-1) + 1, run)
            } else {
                (p.base_offset, 0)
            };
            let wbits = wbits.min(p.max_wbits);
            if !(0..16).contains(&wbits) {
                return Err(Error::corrupt("ORF: invalid code width"));
            }
            let wbits = wbits as u32;
            c.run = run;

            // Entropy decode one code (fill() guarantees 32 buffered bits).
            bits.fill();
            let sign = bits.peek_no_fill(1);
            bits.skip_no_fill(1);
            let lowbits = if p.low_bits > 0 {
                let v = bits.peek_no_fill(p.low_bits);
                bits.skip_no_fill(p.low_bits);
                v
            } else {
                0
            };
            let extrabits = if extra > 0 {
                let v = bits.peek_no_fill(extra);
                bits.skip_no_fill(extra);
                v
            } else {
                0
            };
            let window = bits.peek_no_fill(p.max_high);
            let high = if window != 0 {
                let zeros = window.leading_zeros() - (32 - p.max_high);
                bits.skip_no_fill(zeros + 1);
                zeros
            } else {
                // Escape: `max_high` zeros, then a fixed-width high part and one spare bit.
                bits.skip_no_fill(p.max_high);
                bits.fill();
                let h = if wbits != 15 {
                    let n = 15 - wbits;
                    let v = bits.peek_no_fill(n);
                    bits.skip_no_fill(n);
                    v
                } else {
                    0
                };
                bits.skip_no_fill(1);
                h
            };
            let low = if wbits > 0 {
                bits.fill();
                let v = bits.peek_no_fill(wbits);
                bits.skip_no_fill(wbits);
                v
            } else {
                0
            };
            let glc = low | high.wrapping_shl(wbits);
            let mut code = (glc ^ (sign.wrapping_neg())) as i32;
            c.prev_code = glc;

            // Combine with the smoothed residual.
            let v = if col < 2 { 0 } else { c.smooth };
            let vbits = match p.low_bits {
                0 => 0,
                1 => {
                    code = lowbits as i32 + 2 * (v + code);
                    v + (code >> 1)
                }
                _ => {
                    code = lowbits as i32 + 4 * (v + code);
                    ((code >> 1) & !1) + v + (code >> 2)
                }
            };
            c.smooth = if p.smooth_shift >= 15 { 0 } else { vbits >> p.smooth_shift };

            // Edge-sensitive prediction from same-colour neighbours.
            let w = if col < 2 { p.edge_pred } else { c.west };
            let (mut n, nw) = match up {
                Some(u) => ((u[col] >> extra) as i32, if col < 2 { p.edge_pred } else { (u[col - 2] >> extra) as i32 }),
                None => (p.edge_pred, p.edge_pred),
            };
            if w < n || nw < w {
                if nw <= n && w >= n {
                    n = w;
                } else if nw < n || w >= n {
                    if nw > w || w >= n {
                        if (n - nw).abs() > grad {
                            n += w - nw;
                        } else if (w - nw).abs() <= grad {
                            n = (n + w) >> 1;
                        } else {
                            n = n + w - nw;
                        }
                    }
                } else {
                    n = w;
                }
            }
            let val = code.wrapping_add(n);
            c.west = val;
            cur[col] = (extrabits as i32).wrapping_add(val.wrapping_shl(extra)) as u16;
        }
        if bits.exhausted() {
            return Err(Error::corrupt(format!("ORF: compressed data ends at row {row}/{height}")));
        }
    }
    Ok(())
}

/// Straightforward reference implementation kept for testing the optimised decoder:
/// mirrors the camera's coder description one bit at a time.
#[cfg(test)]
pub(crate) fn decode_reference(data: &[u8], p: &OlympusParams, out: &mut [u16], width: usize, height: usize) {
    let start = stream_start(data, p).unwrap();
    let mut bitpos = start * 8;
    let mut bit = || {
        let b = data.get(bitpos / 8).map(|&b| (b >> (7 - bitpos % 8)) & 1).unwrap_or(0) as u32;
        bitpos += 1;
        b
    };
    let mut ctx = [0i32; 4];
    let (mut lpred, mut pred1, mut glc, mut gcode_prev) = (0i32, 0i32, 0u32, 0u32);
    for row in 0..height {
        for col in 0..width {
            let pcode = gcode_prev;
            gcode_prev = glc;
            let psel = if col > 1 && (1u32 << p.small_shift) >= pcode { ctx[3] + 1 } else { 0 };
            ctx[3] = ctx[2];
            ctx[2] = psel;
            let mut wbits = if col > 1 {
                let hb = 32 - (pcode & 0xffff).leading_zeros() as i32;
                let (t1, t2) = if psel >= p.run_switch {
                    (p.run_threshold, p.run_offset)
                } else {
                    (p.base_threshold, p.base_offset)
                };
                t2 + (hb - t1).max(-1) + 1
            } else {
                p.base_offset
            };
            wbits = wbits.min(p.max_wbits);
            let wbits = wbits as u32;
            let mut rd = |n: u32| (0..n).fold(0u32, |a, _| a << 1 | bit());
            let sign = rd(1);
            let t643 = rd(p.low_bits);
            let t640 = rd(p.extra_bits);
            let mut high = 0;
            loop {
                if rd(1) == 1 {
                    break;
                }
                high += 1;
                if high == p.max_high {
                    high = if wbits != 15 { rd(15 - wbits) } else { 0 };
                    rd(1);
                    break;
                }
            }
            let low = rd(wbits);
            glc = low | (high << wbits);
            let mut gcode = (glc ^ sign.wrapping_neg()) as i32;
            let vbits = if p.low_bits == 2 {
                let v = if col < 2 { 0 } else { pred1 };
                gcode = t643 as i32 + 4 * (v + gcode);
                ((gcode >> 1) & !1) + v + (gcode >> 2)
            } else {
                0
            };
            pred1 = ctx[0];
            ctx[0] = vbits >> p.smooth_shift;
            let w = if col < 2 { p.edge_pred } else { ctx[1] };
            let mut n = if row < 2 { p.edge_pred } else { (out[(row - 2) * width + col] >> p.extra_bits) as i32 };
            let nw = if row < 2 || col < 2 {
                p.edge_pred
            } else {
                (out[(row - 2) * width + col - 2] >> p.extra_bits) as i32
            };
            ctx[1] = lpred;
            if w < n || nw < w {
                if nw <= n && w >= n {
                    n = w;
                } else if nw < n || w >= n {
                    if nw > w || w >= n {
                        if (n - nw).abs() > 1 << p.grad_shift {
                            n += w - nw;
                        } else if (w - nw).abs() <= 1 << p.grad_shift {
                            n = (n + w) >> 1;
                        } else {
                            n = n + w - nw;
                        }
                    }
                } else {
                    n = w;
                }
            }
            lpred = gcode + n;
            out[row * width + col] = (t640 as i32 + ((gcode + n) << p.extra_bits)) as u16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(seed: u32, len: usize, header_len: usize) -> Vec<u8> {
        let mut v = vec![0u8; header_len - 1];
        v.push(1);
        v.extend([0, 0]);
        let mut x = seed;
        // Biased towards small codes so the stream exercises runs and escapes.
        v.extend((0..len).map(|i| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            let b = (x >> 24) as u8;
            if i % 7 == 0 { b & 0x0f } else { b | 0x11 }
        }));
        v
    }

    #[test]
    fn optimised_matches_reference_on_random_streams() {
        for seed in 1..12 {
            let p = OlympusParams::default();
            let data = stream(seed, 40_000, 5);
            let (w, h) = (66, 40);
            let mut a = vec![0u16; w * h];
            let mut b = vec![0u16; w * h];
            let _ = decode(&data, &p, &mut a, w, h);
            decode_reference(&data, &p, &mut b, w, h);
            assert_eq!(a, b, "seed {seed}");
            // Pipelined (two-thread) decoding gives the same result.
            let mut c = vec![0u16; w * h];
            let _ = decode_with(&data, &p, &mut c, w, h, true);
            assert_eq!(c, b, "parallel seed {seed}");
        }
    }

    #[test]
    fn predictor_matches_specification() {
        let vals = [-70, -33, -32, -31, -1, 0, 1, 2, 31, 32, 33, 64, 100, 4095];
        for &w in &vals {
            for &n in &vals {
                for &nw in &vals {
                    assert_eq!(predict(w, n, nw, 32), predict_reference(w, n, nw, 32), "{w} {n} {nw}");
                }
            }
        }
    }

    #[test]
    fn generic_path_matches_reference() {
        let p = OlympusParams { grad_shift: 4, ..OlympusParams::default() };
        for seed in 1..6 {
            let data = stream(seed, 40_000, 5);
            let (w, h) = (34, 30);
            let mut a = vec![0u16; w * h];
            let mut b = vec![0u16; w * h];
            let _ = decode(&data, &p, &mut a, w, h);
            decode_reference(&data, &p, &mut b, w, h);
            assert_eq!(a, b, "seed {seed}");
        }
    }

    #[test]
    fn header_and_truncation_checks() {
        let p = OlympusParams::default();
        let mut out = vec![0u16; 64 * 64];
        assert!(decode(&[0, 0, 0, 0, 2, 0, 0, 0xff], &p, &mut out, 64, 64).is_err());
        let data = stream(3, 200, 5);
        assert!(decode(&data, &p, &mut out, 64, 64).is_err());
        let bad = OlympusParams { max_wbits: 40, ..p };
        assert!(decode(&stream(3, 100_000, 5), &bad, &mut out, 64, 64).is_err());
    }
}
