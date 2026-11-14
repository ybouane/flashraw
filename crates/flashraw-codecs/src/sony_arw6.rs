//! Sony ARW compressed format 6 (TIFF compression 32766, photometric 32803).
//!
//! The strip holds one or more tile streams (magic `A000`). A stream carries five groups of
//! packets: a small low-resolution band and three detail pyramid levels for each of three
//! colour planes (green average, red residual, blue residual), plus the detail of the final
//! green level. Packets hold rows of coefficients coded with an adaptive-width, zero-run
//! scheme. Decoding is: entropy decode, integrate the low band, three reversible 5/3 wavelet
//! synthesis levels per colour plane, the final green level, then the colour reconstruction
//! through the codec's output curve into an RGGB mosaic.

use crate::sony_arw6_curve::LLVC3_CURVE;
use flashraw_core::{Error, Result, par};

/// One tile stream located inside a strip. All values are in bytes (`offset`, `length`)
/// or sensor pixels (the rest).
#[derive(Debug, Clone, Copy)]
pub struct TileStream {
    /// Offset of the stream header inside the strip.
    pub offset: usize,
    /// Total stream length (header plus payload).
    pub length: usize,
    /// Left edge of the tile in the frame.
    pub x: usize,
    /// Top edge of the tile in the frame.
    pub y: usize,
    /// Tile width.
    pub width: usize,
    /// Tile height.
    pub height: usize,
}

const STREAM_START: usize = 0x200;
const HEADER_LEN: usize = 0x80;
const MIN_STRIP: usize = 0x280;
const MAX_TABLE: usize = 16;
/// Widest coefficient code (in bits) the row decoder accepts. Our own bound, not a format
/// value: decoded coefficients are `i32` magnitudes, so a code of more than 31 bits cannot
/// describe a coefficient and marks a corrupt row.
const MAX_COEF_BITS: u32 = 31;

fn bad<T>(what: &str) -> Result<T> {
    Err(Error::corrupt(format!("ARW6: {what}")))
}

fn be16(b: &[u8], at: usize) -> usize {
    ((b[at] as usize) << 8) | b[at + 1] as usize
}

fn be24(b: &[u8], at: usize) -> usize {
    ((b[at] as usize) << 16) | ((b[at + 1] as usize) << 8) | b[at + 2] as usize
}

fn le32(b: &[u8], at: usize) -> Option<usize> {
    let w = b.get(at..at + 4)?;
    Some(u32::from_le_bytes([w[0], w[1], w[2], w[3]]) as usize)
}

// ---------------------------------------------------------------------------------------
// Stream location
// ---------------------------------------------------------------------------------------

/// The fixed fields of a stream header.
#[derive(Clone, Copy)]
struct StreamHead {
    /// Coded width.
    w: usize,
    /// Half of the coded height.
    hh: usize,
    /// Byte sizes of the five packet groups.
    groups: [usize; 5],
}

impl StreamHead {
    /// Parse and sanity check the header at the start of `b`.
    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < HEADER_LEN || !(b.starts_with(b"A000") || b.starts_with(b"0000")) {
            return None;
        }
        let p = be16(b, 0x0c);
        let q = be16(b, 0x0e);
        if (p >> 4) & 0x3f != 16 || q >> 13 != 3 || (q >> 10) & 3 != 3 {
            return None;
        }
        let w = be16(b, 8);
        let hh = be16(b, 0x0a);
        if w == 0 || w % 16 != 0 || hh == 0 {
            return None;
        }
        let mut groups = [0usize; 5];
        for (g, size) in groups.iter_mut().enumerate() {
            *size = be24(b, 0x10 + 3 * g) * 16;
        }
        Some(StreamHead { w, hh, groups })
    }

    /// Total stream length, if the groups fit in `avail` bytes (the rest of the strip).
    fn length(&self, avail: usize) -> Option<usize> {
        let sum: usize = self.groups.iter().sum();
        (sum <= avail - HEADER_LEN).then_some(HEADER_LEN + sum)
    }
}

fn streams_from_table(strip: &[u8], count: usize, full_w: usize, full_h: usize) -> Option<Vec<TileStream>> {
    let mut found = Vec::with_capacity(count);
    for i in 0..count {
        let at = 8 + 24 * i;
        let hint = le32(strip, at)?;
        let (x, y, w, h) = (le32(strip, at + 8)?, le32(strip, at + 12)?, le32(strip, at + 16)?, le32(strip, at + 20)?);
        let start = if hint != 0 { hint } else { STREAM_START };
        let last = strip.len().checked_sub(HEADER_LEN)?;
        if start > last {
            return None;
        }
        // The hint may be stale, so the 16-byte aligned candidates run to the last position
        // that can still hold a header (our own rule: the whole rest of the strip).
        let mut tile = None;
        let mut c = start;
        while c <= last {
            let found_head =
                StreamHead::parse(&strip[c..]).and_then(|head| Some((head, head.length(strip.len() - c)?)));
            if let Some((head, len)) = found_head {
                let (sw, sh) = (head.w, 2 * head.hh);
                let tw = if w == 0 { sw } else { w };
                let th = if h == 0 { sh } else { h };
                if len > HEADER_LEN
                    && c + len <= strip.len()
                    && tw == sw
                    && th == sh
                    && x.saturating_add(tw) <= full_w
                    && y.saturating_add(th) <= full_h
                {
                    tile = Some(TileStream { offset: c, length: len, x, y, width: sw, height: sh });
                    break;
                }
            }
            c += 0x10;
        }
        found.push(tile?);
    }
    Some(found)
}

/// Locate the tile streams of a strip that covers a `full_w x full_h` frame.
pub fn find_streams(strip: &[u8], full_w: usize, full_h: usize) -> Vec<TileStream> {
    if full_w == 0 || full_h == 0 || strip.len() < MIN_STRIP {
        return Vec::new();
    }
    if let Some(count) = le32(strip, 0).filter(|c| (1..=MAX_TABLE).contains(c)) {
        if let Some(v) = streams_from_table(strip, count, full_w, full_h) {
            return v;
        }
    }
    let rest = &strip[STREAM_START..];
    let whole = StreamHead::parse(rest).and_then(|head| Some((head, head.length(rest.len())?)));
    if let Some((head, length)) = whole {
        if head.w == full_w && 2 * head.hh == full_h {
            return vec![TileStream { offset: STREAM_START, length, x: 0, y: 0, width: head.w, height: 2 * head.hh }];
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------------------
// Packets
// ---------------------------------------------------------------------------------------

/// Payload of one record of a packet.
#[derive(Clone, Copy)]
struct Record {
    /// Offset of the payload inside the packet.
    off: usize,
    len: usize,
    /// Quantisation shifts of the components.
    sel: [u8; 3],
}

struct PacketInfo<'a> {
    data: &'a [u8],
    comps: usize,
    records: Vec<Record>,
}

/// Group directory: the packets of every group as byte ranges of the stream.
fn directory(s: &[u8], head: &StreamHead) -> Result<[Vec<(usize, usize)>; 5]> {
    let mut dir: [Vec<(usize, usize)>; 5] = Default::default();
    let mut pos = 0x30;
    let mut base = HEADER_LEN;
    for g in 0..5 {
        if pos + 16 > s.len() {
            return bad("directory beyond stream");
        }
        let n = (s[pos] & 15) as usize;
        let rec_len = if n >= 5 { 32 } else { 16 };
        if n > 9 {
            return bad("too many packets");
        }
        if pos + rec_len > s.len() {
            return bad("directory beyond stream");
        }
        let mut lens = [0usize; 9];
        for (i, l) in lens.iter_mut().enumerate().take(n.min(5)) {
            *l = be24(s, pos + 1 + 3 * i) * 16;
        }
        for i in 5..n {
            lens[i] = be24(s, pos + 0x11 + 3 * (i - 5)) * 16;
        }
        let mut start = base;
        let mut total = 0usize;
        for &l in &lens[..n] {
            if start > s.len() || l > s.len() - start {
                return bad("packet beyond stream");
            }
            dir[g].push((start, l));
            start += l;
            total += l;
        }
        if total > head.groups[g] {
            return bad("packets exceed group");
        }
        base += head.groups[g];
        pos += rec_len;
    }
    Ok(dir)
}

fn parse_packet(data: &[u8]) -> Result<PacketInfo<'_>> {
    let total = data.len();
    if total < 16 {
        return bad("short packet");
    }
    let control = (be16(data, 0) + 1) * 16;
    let ext = be24(data, 2);
    let kind = (data[6] >> 6) as usize;
    let nrec = be16(data, 7);
    let fixed_ok = data[5] == 0x40
        && data[6] & 0x3f == 0
        && (kind == 1 || kind == 3)
        && nrec >= 1
        && data[9] == 0x10
        && data[10] == 0
        && data[11..16].iter().all(|&b| b == 0);
    if !fixed_ok {
        return bad("bad packet header");
    }
    if (control / 16 + ext) * 16 != total {
        return bad("packet size mismatch");
    }
    let per = 16 + 4 * kind;
    if 128 + nrec * per > 8 * total {
        return bad("record table beyond packet");
    }
    let mut bitpos = 128usize;
    let mut off = control;
    let mut records = Vec::with_capacity(nrec);
    for _ in 0..nrec {
        let mut take = |n: usize| {
            let mut v = 0usize;
            for _ in 0..n {
                v = (v << 1) | ((data[bitpos >> 3] >> (7 - (bitpos & 7))) & 1) as usize;
                bitpos += 1;
            }
            v
        };
        let len = take(16);
        let mut sel = [0u8; 3];
        for s in sel.iter_mut().take(kind) {
            *s = take(4) as u8;
        }
        if len > total || off + len > total {
            return bad("record beyond packet");
        }
        records.push(Record { off, len, sel });
        off += len;
    }
    Ok(PacketInfo { data, comps: kind, records })
}

// ---------------------------------------------------------------------------------------
// Coefficient entropy decoding
// ---------------------------------------------------------------------------------------

/// MSB-first bit source over one record. The window is the record's bytes rounded up to
/// 16; `pos` may run past `lim` (the bits there are zero) and that is the overrun condition.
/// `cache` holds the next `avail` bits (left aligned, zero below them).
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    lim: usize,
    cache: u64,
    avail: u32,
}

impl<'a> Bits<'a> {
    fn new(packet: &'a [u8], rec: &Record) -> Self {
        let lim_bytes = rec.len.div_ceil(16) * 16;
        let end = packet.len().min(rec.off + lim_bytes);
        Bits { d: &packet[rec.off..end], pos: 0, lim: lim_bytes * 8, cache: 0, avail: 0 }
    }

    /// Reload the cache from `pos`: at least 57 valid bits.
    #[inline(always)]
    fn refill(&mut self) {
        let byte = self.pos >> 3;
        let sh = (self.pos & 7) as u32;
        let w = match self.d.get(byte..byte + 8) {
            Some(c) => u64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]),
            None => self.tail_word(byte),
        };
        self.cache = w << sh;
        self.avail = 64 - sh;
    }

    #[cold]
    #[inline(never)]
    fn tail_word(&self, byte: usize) -> u64 {
        let mut v = 0u64;
        for i in 0..8 {
            v = (v << 8) | self.d.get(byte + i).copied().unwrap_or(0) as u64;
        }
        v
    }

    /// Make sure at least `n` (<= 57) bits are cached.
    #[inline(always)]
    fn ensure(&mut self, n: u32) {
        if self.avail < n {
            self.refill();
        }
    }

    /// Consume `n` (1..=57) cached bits.
    #[inline(always)]
    fn skip(&mut self, n: u32) {
        self.cache <<= n;
        self.avail -= n;
        self.pos += n as usize;
    }

    /// Take `n` (1..=32) bits that are known to be cached.
    #[inline(always)]
    fn take(&mut self, n: u32) -> u32 {
        let v = (self.cache >> (64 - n)) as u32;
        self.skip(n);
        v
    }

    #[inline(always)]
    fn read(&mut self, n: u32) -> u32 {
        self.ensure(n);
        self.take(n)
    }

    #[inline(always)]
    fn over(&self) -> bool {
        self.pos > self.lim
    }

    /// Mark the source as overrun for good.
    fn poison(&mut self) {
        self.pos = self.lim + 1;
        self.avail = 0;
    }

    /// Zero bits up to and including the first one; the count including that one.
    #[inline(always)]
    fn unary(&mut self) -> u32 {
        let start = self.pos;
        loop {
            let lz = self.cache.leading_zeros();
            if lz < self.avail {
                self.skip(lz + 1);
                return (self.pos - start) as u32;
            }
            self.pos += self.avail as usize;
            if self.pos >= self.lim {
                let real = self.lim.saturating_sub(start);
                self.poison();
                return real as u32 + 1;
            }
            self.refill();
        }
    }

    /// Width adaptation code.
    #[inline(always)]
    fn adapt(&mut self, w: u32) -> u32 {
        // 2 selector bits plus up to `MAX_COEF_BITS - 1` bits of the short-code prefix below.
        self.ensure(32);
        let top = self.cache >> 62;
        if top < 2 {
            self.skip(1);
            return w;
        }
        self.skip(2);
        if top == 2 {
            return w + self.unary();
        }
        let k = w.saturating_sub(1);
        if k == 0 {
            return 0;
        }
        let lz = ((self.cache >> (64 - k)) as u32).leading_zeros() - (32 - k);
        if lz < k {
            self.skip(lz + 1);
            w - (lz + 1)
        } else {
            self.skip(k);
            0
        }
    }

    /// Zero-run length in groups, `rem >= 1` groups being left in the row.
    fn run(&mut self, rem: usize) -> usize {
        if rem <= 1 {
            return rem;
        }
        self.ensure(32);
        let maxp = usize::BITS - (rem - 1).leading_zeros();
        let lz = ((self.cache >> (64 - maxp)) as u32).leading_zeros() - (32 - maxp);
        if lz >= maxp {
            // No one bit within the prefix: the prefix is consumed unless it overruns.
            if self.pos + maxp as usize > self.lim {
                self.poison();
            } else {
                self.skip(maxp);
            }
            return rem;
        }
        self.skip(lz + 1);
        let base = 1usize << lz;
        if base >= rem {
            return rem;
        }
        let extra = if lz > 0 { self.read(lz) as usize } else { 0 };
        (base + extra).min(rem)
    }
}

/// Decode one row of coefficients into `out` (zero on entry; its length is the row width).
fn decode_row(rd: &mut Bits, shift: u32, out: &mut [i32]) {
    let row_width = out.len();
    let groups = row_width.div_ceil(4);
    let mut w = rd.adapt(0);
    let mut gi = 0usize;
    while gi < groups {
        if rd.over() || w > MAX_COEF_BITS {
            rd.poison();
            return;
        }
        if w == 0 {
            gi += rd.run(groups - gi);
            if gi >= groups {
                return;
            }
            w = rd.unary();
            continue;
        }
        let x = if w <= 14 {
            rd.ensure(4 * w);
            [rd.take(w), rd.take(w), rd.take(w), rd.take(w)]
        } else {
            [rd.read(w), rd.read(w), rd.read(w), rd.read(w)]
        };
        let mut v = [0i32; 4];
        if shift == 0 {
            for j in 0..4 {
                v[j] = x[j] as i32;
            }
        } else {
            for j in 0..4 {
                let xj = x[j] as i64;
                v[j] = if xj == 0 { 0 } else { (((2 * xj + 1) << (shift - 1)) - (xj & 1)) as i32 };
            }
        }
        let next = if gi + 1 < groups { rd.adapt(w) } else { w };
        rd.ensure(4);
        for e in v.iter_mut() {
            if *e > 0 && rd.take(1) == 1 {
                *e = e.wrapping_neg();
            }
        }
        let o = gi * 4;
        if o + 4 <= row_width {
            out[o..o + 4].copy_from_slice(&v);
        } else {
            out[o..].copy_from_slice(&v[..row_width - o]);
        }
        w = next;
        gi += 1;
    }
}

/// One record's work: its bytes and the rows it fills.
struct RecordJob<'a, 'b> {
    packet: &'a [u8],
    rec: Record,
    comps: usize,
    rm: usize,
    row_width: usize,
    planes: [&'b mut [i32]; 3],
}

fn decode_record(job: RecordJob) {
    let RecordJob { packet, rec, comps, rm, row_width, planes } = job;
    let mut rd = Bits::new(packet, &rec);
    for (c, plane) in planes.into_iter().enumerate().take(comps) {
        for row in plane.chunks_exact_mut(row_width).take(rm) {
            decode_row(&mut rd, rec.sel[c] as u32, row);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Planes and wavelet synthesis
// ---------------------------------------------------------------------------------------

struct Plane {
    rows: usize,
    cols: usize,
    data: Vec<i32>,
}

impl Plane {
    fn zeros(rows: usize, cols: usize) -> Result<Plane> {
        // No fixed sample cap: every plane of a tile is at most a small multiple of the tile
        // area, and `decode_tile` has already checked that the tile lies inside the output
        // buffer the caller allocated (and limited).
        match rows.checked_mul(cols) {
            Some(n) => Ok(Plane { rows, cols, data: vec![0; n] }),
            None => bad("plane too large"),
        }
    }

    fn row(&self, y: usize) -> &[i32] {
        &self.data[y * self.cols..(y + 1) * self.cols]
    }

    fn rows_in(&self, range: std::ops::Range<usize>) -> Vec<&[i32]> {
        range.map(|y| self.row(y)).collect()
    }
}

/// Boundary detail expansion of one coefficient row.
fn expand(row: &[i32], odd: bool) -> Vec<i32> {
    row.iter()
        .map(|&x| {
            let twice = x.wrapping_mul(2);
            if (odd && x % 2 != 0) || (!odd && x != 0 && x % 2 == 0) { twice.wrapping_add(x.signum()) } else { twice }
        })
        .collect()
}

/// Horizontal low-leading lifting of one row pair `(lo, hi)` into the interleaved `out`.
#[inline(always)]
fn lift_row(lo: &[i32], hi: &[i32], t: &mut [i32], out: &mut [i32]) {
    let w = lo.len();
    t[0] = lo[0].wrapping_sub(hi[0].wrapping_add(hi[0]).wrapping_add(2) >> 2);
    for x in 1..w {
        t[x] = lo[x].wrapping_sub(hi[x - 1].wrapping_add(hi[x]).wrapping_add(2) >> 2);
    }
    for x in 0..w - 1 {
        out[2 * x] = t[x];
        out[2 * x + 1] = hi[x].wrapping_add(t[x].wrapping_add(t[x + 1]) >> 1);
    }
    out[2 * (w - 1)] = t[w - 1];
    out[2 * w - 1] = hi[w - 1].wrapping_add(t[w - 1].wrapping_add(t[w - 1]) >> 1);
}

/// `t[x] = l[x] - ((h0[x] + h1[x] + 2) >> 2)`.
#[inline(always)]
fn low_step(l: &[i32], h0: &[i32], h1: &[i32], t: &mut [i32]) {
    for x in 0..t.len() {
        t[x] = l[x].wrapping_sub(h0[x].wrapping_add(h1[x]).wrapping_add(2) >> 2);
    }
}

/// `o[x] = h[x] + ((t0[x] + t1[x]) >> 1)`.
#[inline(always)]
fn high_step(h: &[i32], t0: &[i32], t1: &[i32], o: &mut [i32]) {
    for x in 0..o.len() {
        o[x] = h[x].wrapping_add(t0[x].wrapping_add(t1[x]) >> 1);
    }
}

/// Rows of the four sub-bands of one synthesis step, each `h` rows of `w` samples.
struct Quad<'a> {
    a: Vec<&'a [i32]>,
    b: Vec<&'a [i32]>,
    c: Vec<&'a [i32]>,
    d: Vec<&'a [i32]>,
}

/// Fused vertical and horizontal synthesis (`Z(V(A,B), V(C,D))`) of the pair rows
/// starting at `y0`; `out` holds those rows of the `2h x 2w` result.
fn synth_pairs(q: &Quad, w: usize, y0: usize, out: &mut [i32]) {
    let h = q.a.len();
    let mut ta = vec![0i32; w];
    let mut ta_next = vec![0i32; w];
    let mut tc = vec![0i32; w];
    let mut tc_next = vec![0i32; w];
    let mut lo_odd = vec![0i32; w];
    let mut hi_odd = vec![0i32; w];
    let mut zt = vec![0i32; w];
    let p = y0.saturating_sub(1);
    low_step(q.a[y0], q.b[p], q.b[y0], &mut ta);
    low_step(q.c[y0], q.d[p], q.d[y0], &mut tc);
    for (i, pair) in out.chunks_exact_mut(4 * w).enumerate() {
        let y = y0 + i;
        let yn = (y + 1).min(h - 1);
        if yn == y {
            ta_next.copy_from_slice(&ta);
            tc_next.copy_from_slice(&tc);
        } else {
            low_step(q.a[yn], q.b[y], q.b[yn], &mut ta_next);
            low_step(q.c[yn], q.d[y], q.d[yn], &mut tc_next);
        }
        high_step(q.b[y], &ta, &ta_next, &mut lo_odd);
        high_step(q.d[y], &tc, &tc_next, &mut hi_odd);
        let (even, odd) = pair.split_at_mut(2 * w);
        lift_row(&ta, &tc, &mut zt, even);
        lift_row(&lo_odd, &hi_odd, &mut zt, odd);
        std::mem::swap(&mut ta, &mut ta_next);
        std::mem::swap(&mut tc, &mut tc_next);
    }
}

fn synth_fused(q: &Quad, w: usize, parallel: bool) -> Result<Plane> {
    let h = q.a.len();
    let mut out = Plane::zeros(2 * h, 2 * w)?;
    let per_chunk = h.div_ceil(par::threads() * 4).max(4);
    par::for_each_row_chunk(&mut out.data, 4 * w, per_chunk, parallel, |y0, chunk| synth_pairs(q, w, y0, chunk));
    Ok(out)
}

/// High-leading vertical lifting of `l` (`m` rows) with `hi` (`m` or `m + 1` rows).
fn lift_high_leading(l: &[&[i32]], hi: &[&[i32]], w: usize) -> Result<Plane> {
    let m = l.len();
    let n_hi = hi.len();
    if m == 0 {
        return bad("empty band");
    }
    if n_hi != m && n_hi != m + 1 {
        return bad("band shape");
    }
    let mut t = vec![0i32; m * w];
    for y in 0..m {
        low_step(l[y], hi[y], hi[(y + 1).min(n_hi - 1)], &mut t[y * w..(y + 1) * w]);
    }
    let mut out = Plane::zeros(2 * m + usize::from(n_hi == m + 1), w)?;
    for y in 0..m {
        out.data[(2 * y + 1) * w..(2 * y + 2) * w].copy_from_slice(&t[y * w..(y + 1) * w]);
        let dst = &mut out.data[2 * y * w..(2 * y + 1) * w];
        if y == 0 {
            for x in 0..w {
                dst[x] = hi[0][x].wrapping_add(t[x]);
            }
        } else {
            high_step(hi[y], &t[(y - 1) * w..y * w], &t[y * w..(y + 1) * w], dst);
        }
    }
    if n_hi == m + 1 {
        let last = &t[(m - 1) * w..m * w];
        let dst = &mut out.data[2 * m * w..(2 * m + 1) * w];
        for x in 0..w {
            dst[x] = hi[m][x].wrapping_add(last[x]);
        }
    }
    Ok(out)
}

/// Horizontal lifting of two equally sized planes.
fn lift_horizontal(lo: &Plane, hi: &Plane) -> Result<Plane> {
    if lo.rows != hi.rows || lo.cols != hi.cols {
        return bad("band shape");
    }
    let w = lo.cols;
    let mut out = Plane::zeros(lo.rows, 2 * w)?;
    let mut t = vec![0i32; w];
    for y in 0..lo.rows {
        lift_row(lo.row(y), hi.row(y), &mut t, &mut out.data[y * 2 * w..(y + 1) * 2 * w]);
    }
    Ok(out)
}

fn same_cols(planes: &[&Plane; 3], w: usize) -> Result<()> {
    if planes.iter().any(|p| p.cols != w) {
        return bad("band width");
    }
    Ok(())
}

/// Level 1 of an unpadded stream.
fn level_one(a: &Plane, s: [&Plane; 3], parallel: bool) -> Result<Plane> {
    let h = a.rows;
    let w = a.cols;
    same_cols(&s, w)?;
    if h == 0 || s.iter().any(|p| p.rows <= h) {
        return bad("level 1 shape");
    }
    let mut q = Quad { a: a.rows_in(0..h), b: s[1].rows_in(1..h), c: s[0].rows_in(0..h), d: s[2].rows_in(1..h) };
    q.b.push(s[0].row(h));
    q.d.push(s[1].row(h));
    synth_fused(&q, w, parallel)
}

/// Levels 2 and 3 of an unpadded stream (`e` = 1 or 2).
fn level_edge(a: &Plane, s: [&Plane; 3], e: usize, odd: bool, parallel: bool) -> Result<Plane> {
    let h = a.rows;
    let w = a.cols;
    same_cols(&s, w)?;
    if h < 2 * e || s[0].rows < h + 2 * e || s[1].rows < h + e || s[2].rows < h {
        return bad("level shape");
    }
    let edge: Vec<Vec<i32>> =
        (0..e).map(|y| expand(s[1].row(y), odd)).chain((0..e).map(|k| expand(s[1].row(h + k), odd))).collect();
    let mut q = Quad { a: a.rows_in(0..h), b: Vec::new(), c: Vec::new(), d: Vec::new() };
    for y in 0..h {
        if y < e {
            q.c.push(s[0].row(y));
            q.b.push(s[0].row(y + e));
            q.d.push(&edge[y]);
        } else if y < h - e {
            q.c.push(s[0].row(y + e));
            q.b.push(s[1].row(y + e));
            q.d.push(s[2].row(y + e));
        } else {
            let k = y - (h - e);
            q.c.push(s[0].row(h + k));
            q.b.push(s[0].row(h + e + k));
            q.d.push(&edge[e + k]);
        }
    }
    synth_fused(&q, w, parallel)
}

/// Level 1 of a padded stream (high-leading vertical lifting).
fn level_padded_first(a: &Plane, s: [&Plane; 3]) -> Result<Plane> {
    let h = a.rows;
    let w = a.cols;
    same_cols(&s, w)?;
    if h == 0 || s.iter().any(|p| p.rows < h + 2) {
        return bad("level 1 shape");
    }
    let c = s[0].rows_in(1..h + 1);
    let mut b = s[1].rows_in(1..h + 1);
    b.push(s[0].row(h + 1));
    let mut d = s[2].rows_in(1..h + 1);
    d.push(s[1].row(h + 1));
    let lo = lift_high_leading(&a.rows_in(0..h), &b, w)?;
    let hi = lift_high_leading(&c, &d, w)?;
    lift_horizontal(&lo, &hi)
}

/// Level 2 of a padded stream.
fn level_padded_second(a: &Plane, s: [&Plane; 3]) -> Result<Plane> {
    let h = a.rows;
    let w = a.cols;
    same_cols(&s, w)?;
    if h == 0 || s[0].rows < h + 3 || s[1].rows <= h || s[2].rows <= h {
        return bad("level 2 shape");
    }
    let first_d = expand(s[0].row(1), false);
    let c = s[0].rows_in(2..h + 2);
    let mut b = vec![s[0].row(0)];
    b.extend(s[1].rows_in(2..h + 1));
    let mut d: Vec<&[i32]> = vec![&first_d];
    d.extend(s[2].rows_in(2..h + 1));
    let lo = lift_high_leading(&a.rows_in(0..h), &b, w)?;
    let hi = lift_high_leading(&c, &d, w)?;
    lift_horizontal(&lo, &hi)
}

/// Level 3 of a padded stream.
fn level_padded_last(a: &Plane, s: [&Plane; 3], odd: bool, parallel: bool) -> Result<Plane> {
    let h = a.rows;
    let w = a.cols;
    same_cols(&s, w)?;
    if h < 2 || s[0].rows < h + 5 || s[1].rows < h + 2 || s[2].rows < h + 2 {
        return bad("level 3 shape");
    }
    let first_d = expand(s[0].row(2), odd);
    let last_d = expand(s[0].row(h + 4), odd);
    let mut q = Quad { a: a.rows_in(0..h), b: vec![s[0].row(1)], c: vec![s[0].row(0)], d: vec![&first_d] };
    for y in 1..h {
        q.c.push(s[0].row(y + 3));
        if y == h - 1 {
            q.b.push(s[0].row(h + 3));
            q.d.push(&last_d);
        } else {
            q.b.push(s[1].row(y + 3));
            q.d.push(s[2].row(y + 3));
        }
    }
    synth_fused(&q, w, parallel)
}

/// Band 0 (`coef` is the group 0 plane): integrate rows horizontally.
fn integrate_band(coef: &Plane, n16: usize, padded: bool) -> Result<Plane> {
    let start = usize::from(padded);
    if n16 <= start {
        return bad("empty band");
    }
    let mut out = Plane::zeros(n16 - start, coef.cols)?;
    for (y, dst) in out.data.chunks_exact_mut(coef.cols).enumerate() {
        let mut acc = 0i32;
        for (d, &c) in dst.iter_mut().zip(coef.row(y + start)) {
            acc = acc.wrapping_add(c as i16 as i32);
            *d = acc;
        }
    }
    Ok(out)
}

/// The coefficient planes of one colour: band 0 and the three detail levels.
struct ColourInput {
    band: Plane,
    levels: [[Plane; 3]; 3],
}

fn refs(l: &[Plane; 3]) -> [&Plane; 3] {
    [&l[0], &l[1], &l[2]]
}

/// Build one colour plane (index 0 = green average, 1 = red, 2 = blue residual).
fn build_colour(inp: ColourInput, index: usize, n16: usize, padded: bool, parallel: bool) -> Result<Plane> {
    let a0 = integrate_band(&inp.band, n16, padded)?;
    drop(inp.band);
    let odd = index != 0;
    let [l1, l2, l3] = &inp.levels;
    if padded {
        let a1 = level_padded_first(&a0, refs(l1))?;
        let a2 = level_padded_second(&a1, refs(l2))?;
        level_padded_last(&a2, refs(l3), odd, parallel)
    } else {
        let a1 = level_one(&a0, refs(l1), parallel)?;
        let a2 = level_edge(&a1, refs(l2), 1, false, parallel)?;
        level_edge(&a2, refs(l3), 2, odd, parallel)
    }
}

// ---------------------------------------------------------------------------------------
// Final green level and colour reconstruction
// ---------------------------------------------------------------------------------------

/// Row of the final-level detail plane that belongs to green row `y`.
///
/// The detail plane is stored in blocks of 8 rows. Its first `lead` rows line up with the
/// first `lead` green rows; the rest of the first block (`8 - lead` rows) belongs to the
/// block seam and is skipped, after which detail row and green row advance together.
#[inline(always)]
fn detail_row(y: usize, lead: usize) -> usize {
    let skipped_at_seam = 8 - lead;
    y + if y >= lead { skipped_at_seam } else { 0 }
}

struct FinalStage<'a> {
    green: &'a Plane,
    detail: &'a Plane,
    red: &'a Plane,
    blue: &'a Plane,
    /// Rows of the detail plane that map one to one onto the first output rows (see
    /// [`detail_row`]).
    lead: usize,
    /// Tile column in the frame and the frame's row stride.
    tile_x: usize,
    stride: usize,
}

impl FinalStage<'_> {
    fn sel(&self, y: usize) -> &[i32] {
        self.detail.row(detail_row(y, self.lead))
    }

    /// Row `y` of the half-height green difference plane.
    fn diff_row(&self, y: usize, o: &mut [i32]) {
        let cur = self.sel(y);
        let prv = self.sel(y.saturating_sub(1));
        let l = self.green.row(y);
        let n = o.len();
        for x in 0..n - 1 {
            let p = cur[x + 1].wrapping_add(prv[x]).wrapping_add(cur[x]).wrapping_add(prv[x + 1]) >> 2;
            o[x] = l[x].wrapping_sub((p >> 1) + (p & 1));
        }
        let p = prv[n - 1].wrapping_add(cur[n - 1]).wrapping_mul(2) >> 2;
        o[n - 1] = l[n - 1].wrapping_sub((p >> 1) + (p & 1));
    }

    /// Output rows of sensor pairs `y0 ..`, `chunk` holding whole frame rows (pairs of them).
    fn emit(&self, y0: usize, chunk: &mut [u16]) {
        let w2 = self.green.cols;
        let h = self.green.rows;
        let mut cur = vec![0i32; w2];
        let mut next = vec![0i32; w2];
        let mut g0 = vec![0i32; w2];
        self.diff_row(y0, &mut cur);
        for (i, pair) in chunk.chunks_exact_mut(2 * self.stride).enumerate() {
            let y = y0 + i;
            self.diff_row((y + 1).min(h - 1), &mut next);
            let sl = self.sel(y);
            g0[0] = sl[0].wrapping_add(cur[0].wrapping_add(next[0]) >> 1);
            for x in 1..w2 {
                g0[x] = sl[x]
                    .wrapping_add(cur[x - 1].wrapping_add(next[x - 1]).wrapping_add(next[x]).wrapping_add(cur[x]) >> 2);
            }
            let (r, b) = (self.red.row(y), self.blue.row(y));
            let (even, odd) = pair.split_at_mut(self.stride);
            let even = &mut even[self.tile_x..self.tile_x + 2 * w2];
            let odd = &mut odd[self.tile_x..self.tile_x + 2 * w2];
            for x in 0..w2 {
                let (v0, v1) = (g0[x], cur[x]);
                let avg = (v0.clamp(-2048, 2047) + v1.clamp(-2048, 2047)) >> 1;
                even[2 * x] = curve(avg.wrapping_add(r[x].wrapping_mul(2)));
                even[2 * x + 1] = curve(v1);
                odd[2 * x] = curve(v0);
                odd[2 * x + 1] = curve(avg.wrapping_add(b[x].wrapping_mul(2)));
            }
            std::mem::swap(&mut cur, &mut next);
        }
    }
}

#[inline(always)]
fn curve(v: i32) -> u16 {
    LLVC3_CURVE[(v.saturating_add(2048).clamp(0, 4095) as usize) & 4095]
}

// ---------------------------------------------------------------------------------------
// Tile and strip decoding
// ---------------------------------------------------------------------------------------

/// Per group: component count, row width divisor (of the stream width) and rows per record.
const GROUPS: [(usize, usize, usize); 5] = [(1, 16, 1), (3, 16, 1), (3, 8, 2), (3, 4, 4), (1, 2, 8)];

/// A packet with its coefficient planes.
struct Coded<'a> {
    info: PacketInfo<'a>,
    rm: usize,
    row_width: usize,
    planes: Vec<Plane>,
}

fn load_packet<'a>(
    s: &'a [u8],
    dir: &[Vec<(usize, usize)>; 5],
    g: usize,
    i: usize,
    w: usize,
    n16: usize,
    padded: bool,
) -> Result<Coded<'a>> {
    let Some(&(start, len)) = dir[g].get(i) else {
        return bad("missing packet");
    };
    let info = parse_packet(&s[start..start + len])?;
    let (comps, div, rm) = GROUPS[g];
    if info.comps != comps {
        return bad("packet kind");
    }
    let rows = (n16 + 1) * rm;
    let n = info.records.len();
    if n * rm != rows && !(padded && n * rm == rows - rm) {
        return bad("record count");
    }
    let row_width = w / div;
    let planes = (0..comps).map(|_| Plane::zeros(rows, row_width)).collect::<Result<Vec<_>>>()?;
    Ok(Coded { info, rm, row_width, planes })
}

fn decode_tile(s: &[u8], tile: &TileStream, out: &mut [u16], raw_w: usize, raw_h: usize, parallel: bool) -> Result<()> {
    let Some(head) = StreamHead::parse(s) else {
        return bad("stream header");
    };
    let (w, h) = (head.w, 2 * head.hh);
    if h > 32767 {
        return bad("tile height");
    }
    if tile.x + w > raw_w || tile.y + h > raw_h {
        return bad("tile outside frame");
    }
    let n16 = h.div_ceil(16);
    let padded = h % 16 != 0;
    let green_rows = if padded { 8 * n16 - 4 } else { h / 2 };
    if green_rows.checked_mul(w).is_none() {
        return bad("plane too large");
    }
    let dir = directory(s, &head)?;
    let mut coded = Vec::with_capacity(13);
    for g in 0..4 {
        for i in 0..3 {
            coded.push(load_packet(s, &dir, g, i, w, n16, padded)?);
        }
    }
    coded.push(load_packet(s, &dir, 4, 0, w, n16, padded)?);

    // Entropy decoding: every record of every packet is an independent job.
    let mut jobs = Vec::new();
    for pk in coded.iter_mut() {
        let (rm, row_width) = (pk.rm, pk.row_width);
        let mut chunks: Vec<_> = pk.planes.iter_mut().map(|p| p.data.chunks_mut(rm * row_width)).collect();
        for rec in &pk.info.records {
            let mut planes: [&mut [i32]; 3] = Default::default();
            for (slot, it) in planes.iter_mut().zip(chunks.iter_mut()) {
                *slot = it.next().unwrap_or_default();
            }
            if rec.len > 0 {
                jobs.push(RecordJob { packet: pk.info.data, rec: *rec, comps: pk.info.comps, rm, row_width, planes });
            }
        }
    }
    par::map(jobs, parallel, decode_record);

    // Wavelet synthesis of the three colour planes.
    let mut planes: Vec<Option<Vec<Plane>>> = coded.into_iter().map(|c| Some(c.planes)).collect();
    let detail = planes[12].take().and_then(|mut v| v.pop()).ok_or_else(|| Error::corrupt("ARW6: detail plane"))?;
    let mut inputs = Vec::with_capacity(3);
    for i in 0..3 {
        let mut take = |g: usize| planes[g * 3 + i].take().unwrap_or_default();
        let band = take(0).pop().ok_or_else(|| Error::corrupt("ARW6: band"))?;
        let mut level =
            |g: usize| -> Result<[Plane; 3]> { take(g).try_into().map_err(|_| Error::corrupt("ARW6: level planes")) };
        let levels = [level(1)?, level(2)?, level(3)?];
        inputs.push((i, ColourInput { band, levels }));
    }
    let mut colours = par::map(inputs, parallel, |(i, inp)| build_colour(inp, i, n16, padded, parallel))
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    let blue = colours.pop().ok_or_else(|| Error::corrupt("ARW6: planes"))?;
    let red = colours.pop().ok_or_else(|| Error::corrupt("ARW6: planes"))?;
    let green = colours.pop().ok_or_else(|| Error::corrupt("ARW6: planes"))?;

    let (half_w, half_h) = (w / 2, h / 2);
    let lead = if padded { 2 } else { 4 };
    if green.cols != half_w || detail.cols != half_w || detail.rows < (8 + green.rows).saturating_sub(lead) {
        return bad("tile plane sizes");
    }
    if green.rows < half_h || red.rows < half_h || blue.rows < half_h || red.cols != half_w || blue.cols != half_w {
        return bad("tile plane sizes");
    }
    let stage =
        FinalStage { green: &green, detail: &detail, red: &red, blue: &blue, lead, tile_x: tile.x, stride: raw_w };
    let dst = &mut out[tile.y * raw_w..(tile.y + h) * raw_w];
    let per_chunk = half_h.div_ceil(par::threads() * 4).max(2);
    par::for_each_row_chunk(dst, 2 * raw_w, per_chunk, parallel, |y0, chunk| stage.emit(y0, chunk));
    Ok(())
}

/// Decode a whole strip into `out` (`raw_w x raw_h` samples, row-major).
pub fn decode(strip: &[u8], out: &mut [u16], raw_w: usize, raw_h: usize, parallel: bool) -> Result<()> {
    if raw_w == 0 || raw_h == 0 || raw_w % 16 != 0 || raw_h % 2 != 0 {
        return bad("bad frame size");
    }
    if raw_w.checked_mul(raw_h).is_none_or(|n| out.len() < n) {
        return bad("output too small");
    }
    if strip.len() < MIN_STRIP {
        return bad("strip too short");
    }
    let tiles = find_streams(strip, raw_w, raw_h);
    if tiles.is_empty() {
        return bad("no tile streams");
    }
    for tile in &tiles {
        decode_tile(&strip[tile.offset..tile.offset + tile.length], tile, out, raw_w, raw_h, parallel)?;
    }
    Ok(())
}
