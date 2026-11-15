//! Nikon High Efficiency (HE and HE★) sensor data.
//!
//! The Z-series cameras write these NEF files with the sensor mosaic as a JPEG XS style
//! codestream (the ISO/IEC 21122 family) in a camera specific flavour. The 14-bit RGGB
//! mosaic is split into four quarter-resolution planes (one per position of the 2x2 cell),
//! the planes are decorrelated by a lifting colour transform, wavelet transformed with the
//! integer 5/3 filter (several horizontal levels and one vertical level), quantised, and
//! entropy coded. A companding curve is undone at the very end. The codec is lossy; the
//! decoder here reproduces the camera's reference arithmetic exactly.
//!
//! # Credit
//!
//! Support for these files was first contributed by Ovidiu Ionescu (pull request #2), whose decoder
//! was based on the MIT-licensed Nikon HE decoder of RAWmakase (Piotr Chmolowski and the RAWmakase
//! contributors, <https://github.com/pch/rawmakase>). This module is a later, independent
//! implementation of the same codestream, written from a format description and checked against
//! the same sample files.
//!
//! # Stream layout
//!
//! Marker segments (`SOC`, `CAP`, picture header, component table, weights table), then
//! slices. A slice is a run of precincts, a precinct holds four sensor rows: two rows of every
//! plane, the vertical low-pass line and the vertical high-pass line. Every precinct has eight
//! packets (two lines times four planes); a packet carries the wavelet bands of one plane and
//! line as three sub-packets: GCLI (the coded bit-plane count of each group of four
//! coefficients, predicted from the line above and coded with a unary code), the bit planes,
//! and the signs.
//!
//! # Decoding
//!
//! 1. The headers and the precinct walk run on one thread (they only look at lengths).
//! 2. Slices are independent: each one is entropy decoded, dequantised and horizontally
//!    inverse transformed while its coefficients are still in cache, one task per slice.
//! 3. The rest works on bands of plane rows with a small halo: the vertical inverse transform,
//!    the five lifting passes of the colour transform, the output curve and the mosaic
//!    assembly, so intermediate rows never leave the cache. Bands are independent.
//!
//! The results do not depend on the number of threads. All transform arithmetic wraps
//! (two's complement `i32`), as the camera's does, so hostile streams produce garbage but
//! never a panic.

use flashraw_core::{Error, Limits, Result, par};

/// Most horizontal wavelet levels a component may use.
pub const MAX_LEVELS: usize = 8;
/// Components (planes) of a picture.
const NC: usize = 4;
/// Packets of a precinct: two lines of four planes.
const PACKETS: usize = 2 * NC;
/// Plane rows reconstructed per cache-resident band.
const BAND_ROWS: usize = 16;
/// Size of the output curve table.
const LUT_SIZE: usize = 1 << 16;
/// Largest output sample (14 bits).
const SAMPLE_MAX: i64 = 16383;

const SOC: u16 = 0xFF10;
const EOC: u16 = 0xFF11;
const PIH: u16 = 0xFF12;
const CDT: u16 = 0xFF13;
const WGT: u16 = 0xFF14;
const SLH: u16 = 0xFF20;

fn unsupported(what: &str) -> Error {
    Error::unsupported_compression("NEF", format!("high-efficiency: {what}"))
}

fn corrupt(what: &str) -> Error {
    Error::corrupt(format!("high-efficiency: {what}"))
}

// ---------------------------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------------------------

/// What the marker segments before the first slice say.
#[derive(Debug, Clone)]
struct Header {
    width: usize,
    height: usize,
    /// Slice height in precincts.
    slice_height: usize,
    /// Significance group size in coefficient groups.
    ss: usize,
    /// Horizontal wavelet levels of each plane.
    levels: [usize; NC],
    /// `(gain, priority)` of every band, in band order.
    weights: Vec<(u8, u8)>,
    /// Output curve parameters.
    black: i64,
    gain: i64,
    knee: i64,
    /// Offset of the first slice marker.
    first_slice: usize,
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    let s = d.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([s[0], s[1]]))
}

/// Parse everything up to the first slice header.
fn parse_headers(d: &[u8]) -> Result<Header> {
    if be16(d, 0) != Some(SOC) {
        return Err(unsupported("not a codestream"));
    }
    let mut at = 2usize;
    let mut pih: Option<Header> = None;
    let mut comps: Vec<usize> = Vec::new();
    let mut weights: Vec<(u8, u8)> = Vec::new();
    loop {
        let Some(marker) = be16(d, at) else { return Err(unsupported("no slices")) };
        match marker {
            SLH => break,
            EOC => return Err(unsupported("no slices")),
            _ => {}
        }
        let Some(len) = be16(d, at + 2) else { return Err(unsupported("truncated marker")) };
        let len = usize::from(len);
        if len < 2 {
            return Err(unsupported("invalid marker length"));
        }
        let body = d.get(at + 4..at + 2 + len).ok_or_else(|| unsupported("truncated marker"))?;
        at += 2 + len;
        match marker {
            PIH => {
                pih = Some(parse_pih(body)?);
                comps.clear();
            }
            CDT => {
                if pih.is_none() {
                    return Err(unsupported("component table before picture header"));
                }
                parse_cdt(body, &mut comps)?;
            }
            WGT => {
                if pih.is_none() {
                    return Err(unsupported("weights table before picture header"));
                }
                if body.len() % 2 != 0 {
                    return Err(corrupt("odd weights table"));
                }
                weights = body.chunks_exact(2).map(|p| (p[0], p[1])).collect();
            }
            _ => {}
        }
    }
    let mut h = pih.ok_or_else(|| unsupported("missing picture header"))?;
    if comps.len() != NC {
        return Err(unsupported("missing component table"));
    }
    h.levels.copy_from_slice(&comps);
    h.weights = weights;
    h.first_slice = at;
    Ok(h)
}

fn parse_pih(b: &[u8]) -> Result<Header> {
    if b.len() != 37 {
        return Err(unsupported("picture header size"));
    }
    let u16_at = |i: usize| usize::from(u16::from_be_bytes([b[i], b[i + 1]]));
    let (width, height) = (u16_at(8), u16_at(10));
    let (cw, hsl) = (u16_at(12), u16_at(14));
    let (nc, ng, ss) = (b[16], b[17], usize::from(b[18]));
    if cw != 0 || nc != NC as u8 || ng != 4 || ss == 0 || hsl == 0 || (b[21] >> 4) & 7 != 1 || b[22] & 15 != 1 {
        return Err(unsupported("picture header layout"));
    }
    if width < 8 || height < 8 || width % 2 != 0 || height % 4 != 0 {
        return Err(corrupt("invalid picture dimensions"));
    }
    if b[23] & 0x3F != 0x14 {
        return Err(unsupported("coding flags"));
    }
    if b[20] != 0x44 {
        return Err(unsupported("coefficient precision"));
    }
    // Vendor extension: the output curve.
    let t = &b[24..37];
    let black = i64::from(((t[3] & 15) as u16) << 8 | u16::from(t[4]));
    let gain = i64::from(u32::from(t[7] & 3) << 16 | u32::from(t[8]) << 8 | u32::from(t[9]));
    let knee = i64::from(u16::from(t[10]) << 8 | u16::from(t[11]));
    Ok(Header {
        width,
        height,
        slice_height: hsl,
        ss,
        levels: [0; NC],
        weights: Vec::new(),
        black,
        gain,
        knee,
        first_slice: 0,
    })
}

fn parse_cdt(b: &[u8], comps: &mut Vec<usize>) -> Result<()> {
    if b.len() != 3 * NC {
        return Err(unsupported("component table size"));
    }
    for rec in b.chunks_exact(3) {
        if rec[0] != 14 || rec[1] != 0x11 {
            return Err(unsupported("component depth or sampling"));
        }
        let (h, v) = (usize::from(rec[2] >> 4), usize::from(rec[2] & 15));
        let ok = (h == 0 && v == 0) || (v == 1 && h >= 1);
        if !ok {
            return Err(unsupported("component decomposition"));
        }
        if h > MAX_LEVELS {
            return Err(unsupported("more than 8 horizontal levels"));
        }
        comps.push(h);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Band layout
// ---------------------------------------------------------------------------------------------

/// One wavelet band of one packet.
#[derive(Debug, Clone, Copy)]
struct Band {
    /// Column of the band in its coefficient row.
    offset: usize,
    width: usize,
    groups: usize,
    gain: u8,
    priority: u8,
    /// Start of the band's GCLI line in the slice scratch.
    gcli_at: usize,
    /// Start of the band's significance flags in the packet scratch.
    flags_at: usize,
    /// Significance flags of the band.
    flags: usize,
}

/// Sizes and band structure derived from the headers.
#[derive(Debug)]
struct Layout {
    w2: usize,
    h2: usize,
    /// Precincts.
    p: usize,
    slice_height: usize,
    ss: usize,
    levels: [usize; NC],
    /// `len_i`: width of the low-pass part after `i` horizontal levels.
    lens: [usize; MAX_LEVELS + 1],
    bands: Vec<Band>,
    /// Band indices of each of the eight packets.
    packets: [Vec<usize>; PACKETS],
    header_len: usize,
    gcli_total: usize,
    /// Most significance flags of any packet.
    flags_max: usize,
}

impl Layout {
    fn new(h: &Header) -> Result<Layout> {
        let w2 = h.width / 2;
        let mut lens = [0usize; MAX_LEVELS + 1];
        lens[0] = w2;
        for i in 0..MAX_LEVELS {
            lens[i + 1] = lens[i] - lens[i] / 2;
        }
        let mut bands: Vec<(usize, usize)> = Vec::new(); // (offset, width)
        let mut packets: [Vec<usize>; PACKETS] = Default::default();
        let mut shared = [usize::MAX; NC];
        for line in 0..2 {
            for c in 0..NC {
                let l = h.levels[c];
                let pk = line * NC + c;
                if l == 0 {
                    if line == 0 {
                        shared[c] = bands.len();
                        bands.push((0, w2));
                    }
                    packets[pk].push(shared[c]);
                } else if line == 0 {
                    let mut off = 0;
                    let mut push = |w: usize, bands: &mut Vec<(usize, usize)>| {
                        packets[pk].push(bands.len());
                        bands.push((off, w));
                        off += w;
                    };
                    push(lens[l], &mut bands);
                    for i in (0..l).rev() {
                        push(lens[i] / 2, &mut bands);
                    }
                } else {
                    let first = w2 - w2 / 2;
                    packets[pk].push(bands.len());
                    bands.push((0, first));
                    packets[pk].push(bands.len());
                    bands.push((first, w2 / 2));
                }
            }
        }
        if h.weights.len() != bands.len() {
            return Err(corrupt("weights do not match the bands"));
        }
        let header_len = 5 + (2 * bands.len()).div_ceil(8);
        let mut out = Vec::with_capacity(bands.len());
        let mut gcli_total = 0;
        for (i, &(offset, width)) in bands.iter().enumerate() {
            let groups = width.div_ceil(4);
            out.push(Band {
                offset,
                width,
                groups,
                gain: h.weights[i].0,
                priority: h.weights[i].1,
                gcli_at: gcli_total,
                flags_at: 0,
                flags: groups.div_ceil(h.ss),
            });
            gcli_total += groups;
        }
        let mut flags_max = 0;
        for pk in &packets {
            let mut at = 0;
            for &b in pk {
                out[b].flags_at = at;
                at += out[b].flags;
            }
            flags_max = flags_max.max(at);
        }
        Ok(Layout {
            w2,
            h2: h.height / 2,
            p: h.height / 4,
            slice_height: h.slice_height,
            ss: h.ss,
            levels: h.levels,
            lens,
            bands: out,
            packets,
            header_len,
            gcli_total,
            flags_max,
        })
    }

    fn slices(&self) -> usize {
        self.p.div_ceil(self.slice_height)
    }
}

/// Find the start of every precinct and check the stream ends where the slices end.
fn walk_precincts(d: &[u8], l: &Layout, first: usize) -> Result<Vec<usize>> {
    let mut at = first;
    let mut offsets = Vec::with_capacity(l.p);
    for p in 0..l.p {
        if p % l.slice_height == 0 {
            if be16(d, at) != Some(SLH) {
                return Err(corrupt("missing slice header"));
            }
            at = at.checked_add(6).ok_or_else(|| corrupt("codestream too long"))?;
        }
        let len =
            d.get(at..at.saturating_add(3)).filter(|s| s.len() == 3).ok_or_else(|| corrupt("truncated codestream"))?;
        let lprc = usize::from(len[0]) << 16 | usize::from(len[1]) << 8 | usize::from(len[2]);
        offsets.push(at);
        at = at
            .checked_add(l.header_len)
            .and_then(|a| a.checked_add(lprc))
            .filter(|&a| a <= d.len())
            .ok_or_else(|| corrupt("codestream truncated"))?;
    }
    if be16(d, at) != Some(EOC) {
        return Err(corrupt("trailing data after the last precinct"));
    }
    Ok(offsets)
}

// ---------------------------------------------------------------------------------------------
// Bit reader
// ---------------------------------------------------------------------------------------------

/// MSB-first bit reader over a payload. Reading past the end returns zeros; the caller
/// compares [`Bits::pos`] with the payload length where the spec's reads would fail.
struct Bits<'a> {
    d: &'a [u8],
    /// Bits consumed since the start of `d`.
    pos: usize,
    /// Unconsumed bits, left aligned.
    w: u64,
    /// Valid bits in `w`.
    avail: u32,
}

/// Eight bytes from `d[at..]` as a big endian word, zero padded past the end.
#[inline(always)]
fn load8(d: &[u8], at: usize) -> u64 {
    if let Some(s) = d.get(at..at.wrapping_add(8)) {
        return u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
    }
    let mut b = [0u8; 8];
    if at < d.len() {
        let n = d.len() - at;
        b[..n].copy_from_slice(&d[at..]);
    }
    u64::from_be_bytes(b)
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], byte: usize) -> Self {
        let mut b = Bits { d, pos: byte.saturating_mul(8), w: 0, avail: 0 };
        b.reload();
        b
    }

    #[inline(always)]
    fn reload(&mut self) {
        let sh = (self.pos & 7) as u32;
        self.w = load8(self.d, self.pos >> 3) << sh;
        self.avail = 64 - sh;
    }

    /// Make at least `n <= 57` bits available.
    #[inline(always)]
    fn ensure(&mut self, n: u32) {
        if self.avail < n {
            self.reload();
        }
    }

    #[inline(always)]
    fn skip(&mut self, n: u32) {
        self.pos += n as usize;
        self.w <<= n;
        self.avail -= n;
    }

    /// Read `n <= 57` bits (0 gives 0).
    #[inline(always)]
    fn take(&mut self, n: u32) -> u64 {
        self.ensure(n);
        let v = (self.w >> 1) >> (63 - n);
        self.skip(n);
        v
    }

    /// A unary code: the run of one bits before a zero bit. `None` when the run is 32 or longer.
    #[inline(always)]
    fn unary(&mut self) -> Option<u32> {
        self.ensure(33);
        let x = (!self.w).leading_zeros();
        if x >= 32 {
            return None;
        }
        self.skip(x + 1);
        Some(x)
    }

    /// Position rounded up to whole bytes.
    fn byte_pos(&self) -> usize {
        self.pos.div_ceil(8)
    }
}

// ---------------------------------------------------------------------------------------------
// Entropy decoding
// ---------------------------------------------------------------------------------------------

/// `SPREAD[n]`: the four bits of `n` (most significant first) as the low bits of four 16-bit lanes.
const SPREAD: [u64; 16] = {
    let mut t = [0u64; 16];
    let mut n = 0;
    while n < 16 {
        let mut k = 0;
        while k < 4 {
            t[n] |= (((n >> (3 - k)) & 1) as u64) << (16 * k);
            k += 1;
        }
        n += 1;
    }
    t
};

/// Per-slice prediction state and scratch.
struct SliceState {
    /// GCLI line of every band (the previous line is the prediction).
    gcli: Vec<u8>,
    /// Truncation position the band's GCLI line was coded with.
    prev_t: Vec<u8>,
    has_prev: Vec<bool>,
    /// GCLI truncation position of every band in the current precinct.
    t: Vec<u8>,
    mode: Vec<u8>,
    flags: Vec<u8>,
}

impl SliceState {
    fn new(l: &Layout) -> Self {
        let nb = l.bands.len();
        SliceState {
            gcli: vec![0; l.gcli_total],
            prev_t: vec![0; nb],
            has_prev: vec![false; nb],
            t: vec![0; nb],
            mode: vec![0; nb],
            flags: vec![0; l.flags_max],
        }
    }
}

/// Entropy decode precinct `at` into `blk` (eight rows of `w2` coefficients, zeroed).
fn decode_precinct(l: &Layout, d: &[u8], at: usize, st: &mut SliceState, blk: &mut [i32]) -> Result<()> {
    let truncated = || corrupt("codestream truncated");
    let hdr = d.get(at..at + l.header_len).ok_or_else(truncated)?;
    let lprc = usize::from(hdr[0]) << 16 | usize::from(hdr[1]) << 8 | usize::from(hdr[2]);
    let start = at + l.header_len;
    let payload = d.get(start..start.checked_add(lprc).ok_or_else(truncated)?).ok_or_else(truncated)?;
    let (q, r) = (hdr[3], hdr[4]);
    for (b, band) in l.bands.iter().enumerate() {
        let shifted = q.saturating_sub(band.gain).saturating_sub(u8::from(band.priority < r));
        st.t[b] = shifted.min(15);
        st.mode[b] = (hdr[5 + b / 4] >> (6 - 2 * (b % 4))) & 3;
    }
    let mut pos = 0;
    for pk in 0..PACKETS {
        pos = decode_packet(l, pk, payload, pos, st, blk)?;
    }
    Ok(())
}

/// Decode one packet starting at byte `start` of the payload; returns the byte after it.
fn decode_packet(
    l: &Layout,
    pk: usize,
    payload: &[u8],
    start: usize,
    st: &mut SliceState,
    blk: &mut [i32],
) -> Result<usize> {
    let truncated = || corrupt("packet truncated");
    let end_bits = payload.len() * 8;
    let mut hb = Bits::new(payload, start);
    let a = hb.take(32) as u32;
    let b = hb.take(24) as u32;
    if hb.pos > end_bits {
        return Err(truncated());
    }
    let raw = a >> 31 != 0;
    let data_len = ((a >> 11) & 0xF_FFFF) as usize;
    let gcli_len = (((a & 0x7FF) << 9) | (b >> 15)) as usize;
    let sign_len = (b & 0x7FFF) as usize;
    let bands = &l.packets[pk];
    let (line, comp) = (pk / NC, pk % NC);

    // Significance flags.
    if !raw {
        for &bi in bands {
            if st.mode[bi] & 2 == 0 {
                continue;
            }
            let band = &l.bands[bi];
            let flags = &mut st.flags[band.flags_at..band.flags_at + band.flags];
            for chunk in flags.chunks_mut(32) {
                let v = hb.take(chunk.len() as u32) as u32;
                let n = chunk.len();
                for (i, f) in chunk.iter_mut().enumerate() {
                    *f = ((v >> (n - 1 - i)) & 1) as u8;
                }
            }
        }
        hb.pos = hb.byte_pos() * 8;
        hb.reload();
    }
    if hb.pos > end_bits {
        return Err(truncated());
    }
    let gcli_start = hb.pos / 8;

    // GCLI sub-packet.
    for &bi in bands {
        let band = &l.bands[bi];
        let t = st.t[bi];
        let mode = st.mode[bi];
        let g = &mut st.gcli[band.gcli_at..band.gcli_at + band.groups];
        let mut bad = false;
        if raw {
            for v in g.iter_mut() {
                *v = hb.take(4) as u8;
            }
        } else {
            let sig = (mode & 2 != 0).then(|| &st.flags[band.flags_at..band.flags_at + band.flags]);
            if st.has_prev[bi] && mode & 1 != 0 {
                let floor = st.prev_t[bi].max(t);
                bad = gcli_predicted(&mut hb, g, t, floor, sig, l.ss)?;
            } else {
                bad = gcli_plain(&mut hb, g, t, sig, l.ss)?;
            }
        }
        if bad {
            return Err(corrupt("GCLI out of range"));
        }
        st.prev_t[bi] = t;
        st.has_prev[bi] = true;
    }
    if hb.pos > end_bits {
        return Err(truncated());
    }
    let data_start = gcli_start.checked_add(gcli_len).ok_or_else(truncated)?;
    if data_start < hb.byte_pos() || data_start > payload.len() {
        return Err(corrupt("bad GCLI sub-packet length"));
    }
    let sign_start = data_start.checked_add(data_len).filter(|&e| e <= payload.len()).ok_or_else(truncated)?;
    let sign_end = sign_start.checked_add(sign_len).filter(|&e| e <= payload.len()).ok_or_else(truncated)?;

    // Bit planes and signs, read together from their two sub-packets.
    let mut db = Bits::new(payload, data_start);
    let mut sb = Bits::new(payload, sign_start);
    for &bi in bands {
        let band = &l.bands[bi];
        let g = &st.gcli[band.gcli_at..band.gcli_at + band.groups];
        let at = (line * NC + comp) * l.w2 + band.offset;
        let row = blk.get_mut(at..at + band.width).ok_or_else(|| corrupt("band outside the precinct"))?;
        decode_band_data(&mut db, &mut sb, g, st.t[bi], row);
    }
    if db.byte_pos() > sign_start || sb.byte_pos() > sign_end {
        return Err(corrupt("bad sub-packet length"));
    }
    Ok(sign_end)
}

/// GCLIs of a band predicted from the line above. `true` when a value is out of range.
#[inline(always)]
fn gcli_predicted(hb: &mut Bits, g: &mut [u8], t: u8, floor: u8, sig: Option<&[u8]>, ss: usize) -> Result<bool> {
    let bad_code = || corrupt("bad GCLI code");
    let mut bad = false;
    let t = i32::from(t);
    let floor = i32::from(floor);
    let chunk = if sig.is_some() { ss } else { g.len().max(1) };
    for (ci, run) in g.chunks_mut(chunk).enumerate() {
        if sig.is_some_and(|f| f[ci] != 0) {
            for v in run {
                *v = (i32::from(*v)).max(floor) as u8;
            }
            continue;
        }
        for v in run {
            let m = i32::from(*v).max(floor);
            let x = hb.unary().ok_or_else(bad_code)? as i32;
            let room = (m - t).max(0);
            let delta = if x > 2 * room {
                x - room
            } else if x & 1 != 0 {
                -((x + 1) >> 1)
            } else {
                x >> 1
            };
            let gcli = m + delta;
            bad |= !(0..=15).contains(&gcli);
            *v = gcli as u8;
        }
    }
    Ok(bad)
}

/// GCLIs of a band without a previous line to predict from.
#[inline(always)]
fn gcli_plain(hb: &mut Bits, g: &mut [u8], t: u8, sig: Option<&[u8]>, ss: usize) -> Result<bool> {
    let bad_code = || corrupt("bad GCLI code");
    let mut bad = false;
    let chunk = if sig.is_some() { ss } else { g.len().max(1) };
    for (ci, run) in g.chunks_mut(chunk).enumerate() {
        if sig.is_some_and(|f| f[ci] != 0) {
            run.fill(t);
            continue;
        }
        for v in run {
            let gcli = u32::from(t) + hb.unary().ok_or_else(bad_code)?;
            bad |= gcli > 15;
            *v = gcli as u8;
        }
    }
    Ok(bad)
}

/// Bit planes of one group: `n` planes of four bits, most significant plane first, each plane
/// giving one bit to each of the four coefficients. Returns the four values in 16-bit lanes.
#[inline(always)]
fn read_planes(db: &mut Bits, n: u32) -> u64 {
    #[inline(always)]
    fn spread(chunk: u64, n: u32, mut acc: u64) -> u64 {
        for j in (0..n).rev() {
            acc = (acc << 1) | SPREAD[((chunk >> (4 * j)) & 15) as usize];
        }
        acc
    }
    if n <= 14 {
        let chunk = db.take(4 * n);
        spread(chunk, n, 0)
    } else {
        let first = spread(db.take(4), 1, 0);
        let chunk = db.take(4 * (n - 1));
        spread(chunk, n - 1, first)
    }
}

/// Decode the magnitudes and signs of one band into `row` (already zero).
#[inline(always)]
fn decode_band_data(db: &mut Bits, sb: &mut Bits, g: &[u8], t: u8, row: &mut [i32]) {
    let t = u32::from(t);
    let full = row.len() / 4;
    let (head, tail) = row.split_at_mut(full * 4);
    for (dst, &gc) in head.chunks_exact_mut(4).zip(g) {
        if u32::from(gc) > t {
            dst.copy_from_slice(&group(db, sb, u32::from(gc), t));
        }
    }
    if !tail.is_empty() {
        if let Some(&gc) = g.get(full) {
            if u32::from(gc) > t {
                let v = group(db, sb, u32::from(gc), t);
                tail.copy_from_slice(&v[..tail.len()]);
            }
        }
    }
}

/// One group with `gcli > t`: four dequantised, signed coefficients.
#[inline(always)]
fn group(db: &mut Bits, sb: &mut Bits, gcli: u32, t: u32) -> [i32; 4] {
    let n = gcli - t;
    let acc = read_planes(db, n);
    let mut mags = [0u32; 4];
    let mut nonzero = 0u32;
    for (k, m) in mags.iter_mut().enumerate() {
        let v = ((acc >> (16 * k)) & 0xFFFF) as u32;
        let mut m0 = v << t;
        nonzero += u32::from(m0 != 0);
        if t > 0 && m0 != 0 {
            // Re-expand the uniform quantiser: m + (m >> s) + (m >> 2s) + ...
            let s = n + 1;
            let mut r = m0;
            m0 = 0;
            while r > 0 {
                m0 += r;
                r >>= s;
            }
        }
        *m = m0 << 4;
    }
    let signs = sb.take(nonzero);
    let mut rem = nonzero;
    let mut out = [0i32; 4];
    for (o, &m) in out.iter_mut().zip(&mags) {
        if m != 0 {
            rem -= 1;
            let neg = ((signs >> rem) & 1) as i32;
            *o = ((m as i32) ^ -neg).wrapping_add(neg);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Horizontal inverse wavelet
// ---------------------------------------------------------------------------------------------

/// Inverse 5/3 lifting on `x`, which holds the `ceil(n/2)` low-pass values followed by the
/// `n/2` high-pass values; the result is the interleaved signal. `tmp` holds at least `n`
/// values of scratch.
pub(crate) fn inverse_53(x: &mut [i32], tmp: &mut [i32]) {
    let n = x.len();
    if n < 2 {
        return;
    }
    let lows = n.div_ceil(2);
    let highs = n / 2;
    let (lo, hi) = x.split_at_mut(lows);
    // Even samples.
    lo[0] = lo[0].wrapping_sub(hi[0].wrapping_mul(2).wrapping_add(2) >> 2);
    for ((l, h), h1) in lo[1..highs].iter_mut().zip(&hi[..highs - 1]).zip(&hi[1..highs]) {
        *l = l.wrapping_sub(h.wrapping_add(*h1).wrapping_add(2) >> 2);
    }
    if lows > highs {
        let h = hi[highs - 1];
        lo[highs] = lo[highs].wrapping_sub(h.wrapping_mul(2).wrapping_add(2) >> 2);
    }
    // Odd samples.
    let pairs = lows.min(highs + 1) - 1;
    for ((h, l), l1) in hi[..pairs].iter_mut().zip(&lo[..pairs]).zip(&lo[1..=pairs]) {
        *h = h.wrapping_add(l.wrapping_add(*l1) >> 1);
    }
    if pairs < highs {
        // Even length: the last odd sample mirrors the last even one.
        hi[pairs] = hi[pairs].wrapping_add(lo[pairs].wrapping_add(lo[pairs]) >> 1);
    }
    // Interleave.
    let tmp = &mut tmp[..n];
    for ((o, &l), &h) in tmp.chunks_exact_mut(2).zip(lo.iter()).zip(hi.iter()) {
        o[0] = l;
        o[1] = h;
    }
    if n % 2 == 1 {
        tmp[n - 1] = lo[lows - 1];
    }
    x.copy_from_slice(tmp);
}

/// Horizontal inverse transform of the eight rows of one precinct.
fn horizontal_inverse(l: &Layout, blk: &mut [i32], tmp: &mut [i32]) {
    for c in 0..NC {
        let lv = l.levels[c];
        if lv == 0 {
            continue;
        }
        let low = &mut blk[c * l.w2..(c + 1) * l.w2];
        for i in (0..lv).rev() {
            inverse_53(&mut low[..l.lens[i]], tmp);
        }
        inverse_53(&mut blk[(NC + c) * l.w2..(NC + c + 1) * l.w2], tmp);
    }
}

// ---------------------------------------------------------------------------------------------
// Vertical inverse wavelet, colour transform, output curve
// ---------------------------------------------------------------------------------------------

/// The output curve: companded plane value + 32768 (clamped to 16 bits) to a 14-bit sample.
pub(crate) fn build_curve(black: i64, gain: i64, knee: i64) -> Box<[u16; LUT_SIZE]> {
    let mut lut = Box::new([0u16; LUT_SIZE]);
    for (i, v) in lut.iter_mut().enumerate() {
        let i = i as i64;
        {
            let d = i - knee;
            let q = if d < 0 {
                -(d * d)
            } else {
                let t = (d * gain + 32768) >> 16;
                t * t
            };
            *v = (black + ((q + 163_840) >> 18)).clamp(0, SAMPLE_MAX) as u16;
        }
    }
    lut
}

/// Rows of the coefficient blocks of all slices, addressed by precinct.
struct Coefficients<'a> {
    blocks: &'a [Vec<i32>],
    w2: usize,
    slice_height: usize,
}

impl Coefficients<'_> {
    /// Row `line` (0 low, 1 high) of plane `c` of precinct `p`, after the horizontal transform.
    #[inline(always)]
    fn row(&self, p: usize, line: usize, c: usize) -> &[i32] {
        let blk = &self.blocks[p / self.slice_height];
        let at = ((p % self.slice_height) * 8 + line * NC + c) * self.w2;
        &blk[at..at + self.w2]
    }
}

type Plane = Vec<i32>;

/// Scratch of one worker: four planes of `BAND_ROWS + 3` rows.
struct BandBuffers {
    planes: [Plane; NC],
}

impl BandBuffers {
    fn new(w2: usize) -> Self {
        let n = (BAND_ROWS + 3) * w2;
        BandBuffers { planes: [vec![0; n], vec![0; n], vec![0; n], vec![0; n]] }
    }
}

/// Vertical inverse of plane `c` for plane rows `ya..=yb` into `buf`.
fn vertical_plane(co: &Coefficients, c: usize, levels: usize, ya: usize, yb: usize, buf: &mut [i32]) {
    let w2 = co.w2;
    let local = |y: usize| (y - ya) * w2;
    if levels == 0 {
        for y in ya..=yb {
            buf[local(y)..local(y) + w2].copy_from_slice(co.row(y / 2, y & 1, c));
        }
        return;
    }
    // Even rows: e_p = lo_p - ((hi_{p-1} + hi_p + 2) >> 2), with hi_{-1} = hi_0.
    let mut y = ya;
    while y <= yb {
        let p = y / 2;
        let lo = co.row(p, 0, c);
        let hi = co.row(p, 1, c);
        let prev = co.row(p.saturating_sub(1), 1, c);
        let dst = &mut buf[local(y)..local(y) + w2];
        for (((d, &l), &hm), &h) in dst.iter_mut().zip(lo).zip(prev).zip(hi) {
            *d = l.wrapping_sub(hm.wrapping_add(h).wrapping_add(2) >> 2);
        }
        y += 2;
    }
    // Odd rows: hi_p + ((e_p + e_{p+1}) >> 1), with e_P = e_{P-1}.
    let mut y = ya + 1;
    while y <= yb {
        let p = y / 2;
        let hi = co.row(p, 1, c);
        let (head, tail) = buf.split_at_mut(local(y));
        let e = &head[local(y - 1)..];
        let (dst, rest) = tail.split_at_mut(w2);
        if y < yb {
            let en = &rest[..w2];
            for (((d, &h), &e0), &e1) in dst.iter_mut().zip(hi).zip(e).zip(en) {
                *d = h.wrapping_add(e0.wrapping_add(e1) >> 1);
            }
        } else {
            for ((d, &h), &e0) in dst.iter_mut().zip(hi).zip(e) {
                *d = h.wrapping_add(e0.wrapping_add(e0) >> 1);
            }
        }
        y += 2;
    }
}

#[inline(always)]
fn sum4(a: i32, b: i32, c: i32, d: i32) -> i32 {
    a.wrapping_add(b).wrapping_add(c).wrapping_add(d)
}

/// Pass 1: `A -= (D[up][x] + D[up][rx] + D[r][x] + D[r][rx]) >> 3`.
fn pass1(a: &mut [i32], du: &[i32], d: &[i32]) {
    let n = a.len();
    let (du, d) = (&du[..n], &d[..n]);
    for x in 0..n - 1 {
        a[x] = a[x].wrapping_sub(sum4(du[x], du[x + 1], d[x], d[x + 1]) >> 3);
    }
    let x = n - 1;
    a[x] = a[x].wrapping_sub(sum4(du[x], du[x], d[x], d[x]) >> 3);
}

/// Pass 2: `D -= (B[r][x] + B[dn][x] + E[r][lx] + E[r][x]) >> 3`, `D += (A[r][lx] + A[r][x] +
/// A[dn][lx] + A[dn][x]) >> 2`.
fn pass2(d: &mut [i32], b: &[i32], bn: &[i32], e: &[i32], a: &[i32], an: &[i32]) {
    let n = d.len();
    let (b, bn, e, a, an) = (&b[..n], &bn[..n], &e[..n], &a[..n], &an[..n]);
    let first = d[0].wrapping_sub(sum4(b[0], bn[0], e[0], e[0]) >> 3).wrapping_add(sum4(a[0], a[0], an[0], an[0]) >> 2);
    for x in 1..n {
        d[x] = d[x]
            .wrapping_sub(sum4(b[x], bn[x], e[x - 1], e[x]) >> 3)
            .wrapping_add(sum4(a[x - 1], a[x], an[x - 1], an[x]) >> 2);
    }
    d[0] = first;
}

/// Pass 3: `A -= (B[r][x] + B[r][rx] + E[up][x] + E[r][x]) >> 3`.
fn pass3(a: &mut [i32], b: &[i32], eu: &[i32], e: &[i32]) {
    let n = a.len();
    let (b, eu, e) = (&b[..n], &eu[..n], &e[..n]);
    for x in 0..n - 1 {
        a[x] = a[x].wrapping_sub(sum4(b[x], b[x + 1], eu[x], e[x]) >> 3);
    }
    let x = n - 1;
    a[x] = a[x].wrapping_sub(sum4(b[x], b[x], eu[x], e[x]) >> 3);
}

/// Pass 4: `B += (A[r][lx] + A[r][x] + D[up][x] + D[r][x]) >> 2`.
fn pass4(b: &mut [i32], a: &[i32], du: &[i32], d: &[i32]) {
    let n = b.len();
    let (a, du, d) = (&a[..n], &du[..n], &d[..n]);
    b[0] = b[0].wrapping_add(sum4(a[0], a[0], du[0], d[0]) >> 2);
    for x in 1..n {
        b[x] = b[x].wrapping_add(sum4(a[x - 1], a[x], du[x], d[x]) >> 2);
    }
}

/// Pass 5: `E += (A[r][x] + A[dn][x] + D[r][x] + D[r][rx]) >> 2`.
fn pass5(e: &mut [i32], a: &[i32], an: &[i32], d: &[i32]) {
    let n = e.len();
    let (a, an, d) = (&a[..n], &an[..n], &d[..n]);
    for x in 0..n - 1 {
        e[x] = e[x].wrapping_add(sum4(a[x], an[x], d[x], d[x + 1]) >> 2);
    }
    let x = n - 1;
    e[x] = e[x].wrapping_add(sum4(a[x], an[x], d[x], d[x]) >> 2);
}

/// Run the five colour transform passes over the planes of rows `ya..=yb` for output rows `r0..r1`.
fn star_tetrix_band(planes: &mut [Plane; NC], w2: usize, h2: usize, ya: usize, r0: usize, r1: usize) {
    let yb = r1.min(h2 - 1);
    let [pa, pb, pd, pe] = planes;
    let row = |y: usize| (y - ya) * w2;
    let up = |r: usize| r.saturating_sub(1);
    let dn = |r: usize| (r + 1).min(h2 - 1);
    // Pass 1.
    for r in r0.saturating_sub(1)..=yb {
        let (u, c) = (row(up(r)), row(r));
        pass1(&mut pa[c..c + w2], &pd[u..u + w2], &pd[c..c + w2]);
    }
    // Pass 2.
    for r in r0.saturating_sub(1)..r1 {
        let (c, n) = (row(r), row(dn(r)));
        pass2(&mut pd[c..c + w2], &pb[c..c + w2], &pb[n..n + w2], &pe[c..c + w2], &pa[c..c + w2], &pa[n..n + w2]);
    }
    // Pass 3.
    for r in r0..=yb {
        let (u, c) = (row(up(r)), row(r));
        pass3(&mut pa[c..c + w2], &pb[c..c + w2], &pe[u..u + w2], &pe[c..c + w2]);
    }
    // Pass 4.
    for r in r0..r1 {
        let (u, c) = (row(up(r)), row(r));
        pass4(&mut pb[c..c + w2], &pa[c..c + w2], &pd[u..u + w2], &pd[c..c + w2]);
    }
    // Pass 5.
    for r in r0..r1 {
        let (c, n) = (row(r), row(dn(r)));
        pass5(&mut pe[c..c + w2], &pa[c..c + w2], &pa[n..n + w2], &pd[c..c + w2]);
    }
}

#[inline(always)]
fn curve(lut: &[u16; LUT_SIZE], v: i32) -> u16 {
    lut[(v.wrapping_add(32768).clamp(0, 65535) as usize) & (LUT_SIZE - 1)]
}

/// Reconstruct plane rows `r0..r1` (`r0` even) of the mosaic into `out` (rows `2*r0..2*r1`).
fn reconstruct_band(
    l: &Layout,
    co: &Coefficients,
    lut: &[u16; LUT_SIZE],
    bufs: &mut BandBuffers,
    rows: std::ops::Range<usize>,
    out: &mut [u16],
    pitch: usize,
) {
    let (r0, r1) = (rows.start, rows.end);
    let (w2, h2) = (l.w2, l.h2);
    let ya = r0.saturating_sub(2);
    let yb = r1.min(h2 - 1);
    for c in 0..NC {
        vertical_plane(co, c, l.levels[c], ya, yb, &mut bufs.planes[c]);
    }
    star_tetrix_band(&mut bufs.planes, w2, h2, ya, r0, r1);
    let [pa, pb, pd, pe] = &bufs.planes;
    for r in r0..r1 {
        let at = (r - ya) * w2;
        let (a, b, d, e) = (&pa[at..at + w2], &pb[at..at + w2], &pd[at..at + w2], &pe[at..at + w2]);
        let base = 2 * (r - r0) * pitch;
        let (top, bottom) = out[base..].split_at_mut(pitch);
        let top = &mut top[..2 * w2];
        let bottom = &mut bottom[..2 * w2];
        for ((((t, bt), &a), &b), (&d, &e)) in
            top.chunks_exact_mut(2).zip(bottom.chunks_exact_mut(2)).zip(a).zip(b).zip(d.iter().zip(e))
        {
            t[0] = curve(lut, b);
            t[1] = curve(lut, a);
            bt[0] = curve(lut, d);
            bt[1] = curve(lut, e);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------------

/// Decode a High Efficiency codestream (starting at its `SOC` marker) into `out`.
///
/// `width` and `height` are the container's dimensions; they must equal the stream's. The
/// decoded `width * height` samples (14 bit, RGGB, red at the origin) go to `out` with a row
/// stride of `pitch`; columns `width..pitch` are not touched. Every error is fatal: there is
/// no partial image.
///
/// Errors: [`Error::UnsupportedCompression`] for a valid stream in a mode that is not
/// implemented, [`Error::CorruptFile`] for damaged data, [`Error::InvalidArgument`] for a
/// buffer that is too small and [`Error::LimitExceeded`] when the dimensions exceed `limits`.
pub fn decode(
    codestream: &[u8],
    out: &mut [u16],
    width: usize,
    height: usize,
    pitch: usize,
    limits: &Limits,
    parallel: bool,
) -> Result<()> {
    let need = pitch.checked_mul(height).ok_or_else(|| Error::InvalidArgument("output size overflows".into()))?;
    if pitch < width || out.len() < need {
        return Err(Error::InvalidArgument("output buffer too small".into()));
    }
    let hdr = parse_headers(codestream)?;
    if hdr.width != width || hdr.height != height {
        return Err(corrupt("dimensions differ from the container's"));
    }
    let (Ok(w32), Ok(h32)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err(corrupt("invalid picture dimensions"));
    };
    limits.check_dims(w32, h32, 1)?;
    let l = Layout::new(&hdr)?;
    let offsets = walk_precincts(codestream, &l, hdr.first_slice)?;

    // Entropy decoding, dequantisation and the horizontal transform, one task per slice.
    let slice_ids: Vec<usize> = (0..l.slices()).collect();
    let blocks = par::map(slice_ids, parallel, |s| decode_slice(&l, codestream, &offsets, s));
    let blocks = blocks.into_iter().collect::<Result<Vec<_>>>()?;

    // Vertical transform, colour transform and the output curve, band by band.
    let lut = build_curve(hdr.black, hdr.gain, hdr.knee);
    reconstruct(&l, &blocks, &lut, &mut out[..need], pitch, parallel);
    Ok(())
}

/// Turn the horizontally inverse transformed coefficients of all slices into the mosaic.
/// `out` holds exactly `pitch * height` samples.
fn reconstruct(l: &Layout, blocks: &[Vec<i32>], lut: &[u16; LUT_SIZE], out: &mut [u16], pitch: usize, parallel: bool) {
    let co = Coefficients { blocks, w2: l.w2, slice_height: l.slice_height };
    let bands = l.h2.div_ceil(BAND_ROWS);
    let workers = if parallel { par::threads().max(1) } else { 1 };
    // More tasks than threads smooths out uneven bands; one thread needs just one.
    let tasks = if workers > 1 { bands.min(workers * 4).max(1) } else { 1 };
    let rows_per_task = bands.div_ceil(tasks) * BAND_ROWS;
    let items: Vec<(usize, &mut [u16])> =
        out.chunks_mut(pitch * 2 * rows_per_task).enumerate().map(|(i, c)| (i * rows_per_task, c)).collect();
    par::map(items, parallel, |(first, chunk)| {
        let mut bufs = BandBuffers::new(l.w2);
        let end = (first + rows_per_task).min(l.h2);
        let mut r0 = first;
        while r0 < end {
            let r1 = (r0 + BAND_ROWS).min(end);
            reconstruct_band(l, &co, lut, &mut bufs, r0..r1, &mut chunk[2 * (r0 - first) * pitch..], pitch);
            r0 = r1;
        }
    });
}

/// Decode slice `s`: its precincts' coefficients, horizontally inverse transformed.
fn decode_slice(l: &Layout, d: &[u8], offsets: &[usize], s: usize) -> Result<Vec<i32>> {
    let p0 = s * l.slice_height;
    let p1 = (p0 + l.slice_height).min(l.p);
    let row = 8 * l.w2;
    let mut block = vec![0i32; (p1 - p0) * row];
    let mut st = SliceState::new(l);
    let mut tmp = vec![0i32; l.w2];
    for (i, p) in (p0..p1).enumerate() {
        let blk = &mut block[i * row..(i + 1) * row];
        decode_precinct(l, d, offsets[p], &mut st, blk)?;
        horizontal_inverse(l, blk, &mut tmp);
    }
    Ok(block)
}

/// Decode on the calling thread only (the single-thread reference for tests).
#[doc(hidden)]
pub fn decode_sequential_for_test(codestream: &[u8], width: usize, height: usize) -> Result<Vec<u16>> {
    let mut out = vec![0xA5A5u16; width * height];
    par::sequential(|| decode(codestream, &mut out, width, height, width, &Limits::default(), false))?;
    Ok(out)
}

#[cfg(test)]
#[path = "nikon_he_tests.rs"]
mod tests;
