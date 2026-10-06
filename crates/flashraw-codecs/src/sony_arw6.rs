//! Sony ARW "compressed" format 6 (ILCE-7M5 "Compressed"/"Compressed HQ").
//!
//! The raw strip holds one or more tile streams. Each stream is a 3-level 5/3
//! integer wavelet pyramid of three planes at half resolution — the green average
//! of each 2x2 quad and red/blue residuals against it — plus a final level that
//! restores the two greens of every quad. Coefficients are coded in packets; every
//! packet record (a band row, or a few) is an independent MSB-first bit stream of
//! groups of four coefficients with an adaptive bit width, zero runs and sign bits.
//! Decoded 12-bit codes map to 16-bit linear values through a fixed curve
//! ([`crate::sony_arw6_curve::LLVC3_CURVE`]).
//!
//! Layout of a stream (all big-endian): `"A000"`/`"0000"` magic; width at 0x08,
//! half height at 0x0A, format words at 0x0C/0x0E; five 24-bit group sizes at
//! 0x10; per-group packet directories from 0x30; packets from 0x80.
//!
//! Tiles are independent and decode in parallel; within a tile the packet records
//! decode in parallel and the three planes synthesise in parallel.

use crate::sony_arw6_curve::LLVC3_CURVE;
use flashraw_core::{Error, Result, par};

const STREAM_OFFSET: usize = 0x200;
const BIAS: i32 = 2048;
const MAX_PLANE_SAMPLES: usize = 128 * 1024 * 1024;

fn corrupt(what: &str) -> Error {
    Error::corrupt(format!("ARW6: {what}"))
}

fn require(cond: bool, what: &str) -> Result<()> {
    if cond { Ok(()) } else { Err(corrupt(what)) }
}

fn be16(d: &[u8], p: usize) -> Result<u32> {
    d.get(p..p + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as u32).ok_or_else(|| corrupt("truncated header"))
}

fn be32(d: &[u8], p: usize) -> Result<u32> {
    d.get(p..p + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| corrupt("truncated header"))
}

fn le32(d: &[u8], p: usize) -> Result<u32> {
    d.get(p..p + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| corrupt("truncated header"))
}

/// A 2-D plane of signed coefficients.
#[derive(Clone, Default)]
struct Plane {
    rows: usize,
    cols: usize,
    data: Vec<i32>,
}

impl Plane {
    fn new(rows: usize, cols: usize) -> Result<Self> {
        let n = rows.checked_mul(cols).filter(|&n| n <= MAX_PLANE_SAMPLES).ok_or_else(|| corrupt("plane too large"))?;
        Ok(Plane { rows, cols, data: vec![0; n] })
    }
    #[inline(always)]
    fn at(&self, r: usize, c: usize) -> i32 {
        self.data[r * self.cols + c]
    }
    #[inline(always)]
    fn set(&mut self, r: usize, c: usize, v: i32) {
        self.data[r * self.cols + c] = v;
    }
    fn row(&self, r: usize) -> &[i32] {
        &self.data[r * self.cols..(r + 1) * self.cols]
    }
    fn row_mut(&mut self, r: usize) -> &mut [i32] {
        &mut self.data[r * self.cols..(r + 1) * self.cols]
    }
    /// First `rows` rows.
    fn head(&self, rows: usize) -> Result<Plane> {
        require(rows <= self.rows, "plane row count")?;
        Ok(Plane { rows, cols: self.cols, data: self.data[..rows * self.cols].to_vec() })
    }
}

/// Arithmetic shift = floor division by a power of two.
#[inline(always)]
fn fshr(x: i32, bits: u32) -> i32 {
    x >> bits
}

/// Sign-extend the low 16 bits.
#[inline(always)]
fn sign16(x: i32) -> i32 {
    x as u16 as i16 as i32
}

/// Boundary detail reconstruction used at stripe edges.
#[inline(always)]
fn edge_detail(x: i32, odd_mode: bool) -> i32 {
    let half = x.signum();
    let odd = x % 2 != 0;
    let use_half = if odd_mode { odd } else { x != 0 && !odd };
    x.wrapping_mul(2).wrapping_add(if use_half { half } else { 0 })
}

// ---------------------------------------------------------------------------
// Bit readers

/// MSB-first reader for packet headers (exact-bound checked).
struct HeaderBits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl HeaderBits<'_> {
    fn read(&mut self, n: u32) -> Result<u32> {
        require(self.pos + n as usize <= self.data.len() * 8, "packet header beyond data")?;
        let mut out = 0u32;
        for _ in 0..n {
            let b = (self.data[self.pos >> 3] >> (7 - (self.pos & 7))) & 1;
            out = (out << 1) | b as u32;
            self.pos += 1;
        }
        Ok(out)
    }
}

/// Coefficient bit stream of one packet record: MSB-first, with an allowance of
/// whole 64-bit word pairs; reading past it yields zeros and sets `status` (the
/// decoder then zero-fills, as the format intends). Bytes past the end of the
/// packet read as zero.
struct CoeffBits<'a> {
    data: &'a [u8],
    /// Next byte to load into the cache.
    next: usize,
    /// Valid bits, left-aligned.
    cache: u64,
    nbits: u32,
    /// Bits still within the allowance.
    remaining: u64,
    status: u8,
}

impl<'a> CoeffBits<'a> {
    fn new(packet: &'a [u8], payload_offset: usize, byte_length: usize) -> Self {
        let words = ((byte_length.div_ceil(8)) + 1) & !1;
        CoeffBits { data: packet, next: payload_offset, cache: 0, nbits: 0, remaining: words as u64 * 64, status: 0 }
    }

    /// Top up the cache with whole bytes (to at least 56 bits).
    #[inline(always)]
    fn refill(&mut self) {
        if self.nbits > 55 {
            return;
        }
        let take = (63 - self.nbits) >> 3;
        let w = match self.data.get(self.next..self.next + 8) {
            Some(b) => u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
            None => {
                let mut b = [0u8; 8];
                if let Some(t) = self.data.get(self.next..) {
                    let k = t.len().min(8);
                    b[..k].copy_from_slice(&t[..k]);
                }
                u64::from_be_bytes(b)
            }
        };
        // Only the `take` whole bytes that fit are added.
        let bits = take * 8;
        self.cache |= (w >> (64 - bits)) << (64 - bits - self.nbits);
        self.next += take as usize;
        self.nbits += bits;
    }

    #[inline(always)]
    fn consume(&mut self, n: u32) {
        self.cache <<= n;
        self.nbits -= n;
        self.remaining -= n as u64;
    }

    /// Up to 32 bits.
    #[inline(always)]
    fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if n as u64 > self.remaining {
            return self.read_past_limit(n);
        }
        if self.nbits < n {
            self.refill();
        }
        let v = (self.cache >> (64 - n)) as u32;
        self.consume(n);
        v
    }

    #[cold]
    fn read_past_limit(&mut self, n: u32) -> u32 {
        // Bits up to the limit are real; the rest read as zero.
        let avail = self.remaining as u32;
        let head = if avail > 0 { self.read(avail) } else { 0 };
        self.status = 2;
        self.remaining = 0;
        // A full 32-bit read at the limit shifts by 32 (head is then 0).
        head.checked_shl(n - avail).unwrap_or(0)
    }

    #[inline(always)]
    fn bit(&mut self) -> u32 {
        self.read(1)
    }

    /// Count zeros up to and including the terminating one (returns zeros + 1).
    fn unary_plus_one(&mut self) -> i32 {
        let mut zeros = 0i32;
        loop {
            if self.status != 0 {
                return zeros + 1;
            }
            if self.remaining == 0 {
                // The next bit lies past the allowance.
                self.status = 2;
                return zeros + 1;
            }
            self.refill();
            let avail = (self.nbits as u64).min(self.remaining) as u32;
            let lz = self.cache.leading_zeros();
            if lz < avail {
                self.consume(lz + 1);
                return zeros + lz as i32 + 1;
            }
            self.consume(avail);
            zeros += avail as i32;
        }
    }

    /// Position of the first one bit within the next `k` bits (consuming it), or
    /// `None` after consuming `k` zero bits.
    #[inline(always)]
    fn leading_one_within(&mut self, k: u32) -> Option<u32> {
        if k as u64 <= self.remaining && k <= 32 {
            self.refill();
            let lz = self.cache.leading_zeros();
            if lz < k {
                self.consume(lz + 1);
                return Some(lz);
            }
            self.consume(k);
            return None;
        }
        (0..k).find(|_| self.bit() != 0)
    }
}

/// Adapt the coefficient bit width: `0` keep, `10` grow by a unary count, `11`
/// shrink by the position of the next one bit (or to zero).
#[inline(always)]
fn update_width(br: &mut CoeffBits, w: i32) -> i32 {
    if br.bit() == 0 || br.status != 0 {
        return w;
    }
    if br.bit() == 0 || br.status != 0 {
        return w + br.unary_plus_one();
    }
    match br.leading_one_within((w - 1).max(0) as u32) {
        Some(z) => w - (z as i32 + 1),
        None => 0,
    }
}

/// Length of a run of all-zero groups (Elias-gamma-like, capped at `remaining`).
fn zero_run(br: &mut CoeffBits, remaining: i32) -> i32 {
    if remaining <= 1 {
        return remaining;
    }
    let max_prefix = 32 - ((remaining - 1) as u32).leading_zeros() as i32;
    let mut zeros = 0;
    while zeros < max_prefix {
        if br.bit() != 0 {
            break;
        }
        if br.status != 0 {
            return remaining;
        }
        zeros += 1;
    }
    if zeros >= max_prefix {
        return remaining;
    }
    let base = 1 << zeros;
    if base >= remaining {
        return remaining;
    }
    let extra = if zeros > 0 { br.read(zeros as u32) as i32 } else { 0 };
    (base + extra).min(remaining)
}

/// Decode one band row of `groups` four-coefficient groups into `dst`.
fn decode_component(br: &mut CoeffBits, groups: usize, shift: u32, dst: &mut [i32]) {
    let row_width = dst.len();
    let mut width = update_width(br, 0);
    let mut gi = 0usize;
    let mut out = 0usize;
    while gi < groups {
        if br.status != 0 || width > 0x13 {
            if br.status == 0 {
                br.status = 1;
            }
            dst[out..].fill(0);
            return;
        }
        if width == 0 {
            let run = zero_run(br, (groups - gi) as i32) as usize;
            let n = (run * 4).min(row_width - out);
            dst[out..out + n].fill(0);
            out += n;
            gi += run;
            if gi >= groups {
                break;
            }
            width = br.unary_plus_one();
            continue;
        }
        let mut vals = [0i32; 4];
        for v in vals.iter_mut() {
            let x = br.read(width as u32);
            *v = if shift > 0 && x > 0 {
                (((2 * x as u64 + 1) << (shift - 1)) - (x as u64 & 1)) as i32
            } else {
                x as i32
            };
        }
        let next = if gi + 1 < groups { update_width(br, width) } else { width };
        for v in vals.iter_mut() {
            if *v > 0 {
                let s = br.bit() as i32;
                *v = v.wrapping_sub(2i32.wrapping_mul(*v).wrapping_mul(s));
            }
        }
        for v in vals {
            if out < row_width {
                dst[out] = v;
                out += 1;
            }
        }
        width = next;
        gi += 1;
    }
    dst[out..].fill(0);
}

// ---------------------------------------------------------------------------
// Stream structure

#[derive(Debug, Clone, Copy)]
struct DirEntry {
    group: usize,
    index: usize,
    start: usize,
    length: usize,
}

/// A tile stream inside the strip.
#[derive(Debug, Clone, Copy)]
pub struct TileStream {
    pub offset: usize,
    pub length: usize,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// `(coded width, half height)` if `stream` starts with a valid stream header.
fn parse_stream_header(stream: &[u8]) -> Option<(usize, usize)> {
    if stream.len() < 0x80 || !(stream.starts_with(b"A000") || stream.starts_with(b"0000")) {
        return None;
    }
    let wc = be16(stream, 0x0c).ok()?;
    let we = be16(stream, 0x0e).ok()?;
    if (wc >> 4) & 0x3f != 16 || we >> 13 != 3 || (we >> 10) & 3 != 3 {
        return None;
    }
    let w = be16(stream, 8).ok()? as usize;
    let hh = be16(stream, 0x0a).ok()? as usize;
    if w == 0 || hh == 0 || w % 16 != 0 {
        return None;
    }
    Some((w, hh))
}

/// Five 24-bit group sizes (in 16-byte units) starting at `p`.
fn group_sizes(stream: &[u8], p: usize) -> Result<[usize; 5]> {
    let mut g = [0usize; 5];
    for (i, v) in g.iter_mut().enumerate() {
        *v = ((be32(stream, p + 3 * i)? >> 4) & 0x0fff_fff0) as usize;
    }
    Ok(g)
}

fn stream_length(stream: &[u8]) -> Result<usize> {
    require(stream.len() >= 0x80, "stream too short")?;
    let payload: usize = group_sizes(stream, 0x10)?.iter().sum();
    require(payload <= stream.len() - 0x80, "stream payload beyond strip")?;
    Ok(0x80 + payload)
}

/// Locate the tile streams of a strip covering a `full_w x full_h` frame.
pub fn find_streams(strip: &[u8], full_w: usize, full_h: usize) -> Vec<TileStream> {
    let mut streams = Vec::new();
    if full_w == 0 || full_h == 0 || strip.len() < STREAM_OFFSET + 0x80 {
        return streams;
    }
    let count = le32(strip, 0).unwrap_or(0) as usize;
    if (1..=16).contains(&count) {
        'tiles: for index in 0..count {
            let entry = 0x08 + index * 0x18;
            let fields: Option<Vec<u32>> = (0..6).map(|k| le32(strip, entry + 4 * k).ok()).collect();
            let Some(f) = fields else {
                streams.clear();
                break;
            };
            let (table_offset, tx, ty, tw, th) = (f[0] as usize, f[2] as usize, f[3] as usize, f[4], f[5]);
            let start = if table_offset != 0 { table_offset } else { STREAM_OFFSET };
            if start > strip.len() - 0x80 {
                streams.clear();
                break;
            }
            let end = (strip.len() - 0x80).min(start.saturating_add(0x1000));
            let mut cand = start;
            while cand <= end {
                if let Some((w, hh)) = parse_stream_header(&strip[cand..]) {
                    if let Ok(len) = stream_length(&strip[cand..]) {
                        let tile_w = if tw != 0 { tw as usize } else { w };
                        let tile_h = if th != 0 { th as usize } else { hh * 2 };
                        if len > 0x80
                            && cand + len <= strip.len()
                            && tile_w == w
                            && tile_h == hh * 2
                            && tx + tile_w <= full_w
                            && ty + tile_h <= full_h
                        {
                            streams.push(TileStream {
                                offset: cand,
                                length: len,
                                x: tx,
                                y: ty,
                                width: w,
                                height: hh * 2,
                            });
                            continue 'tiles;
                        }
                    }
                }
                cand += 0x10;
            }
            streams.clear();
            break;
        }
        if streams.len() == count {
            return streams;
        }
        streams.clear();
    }
    if let Some((w, hh)) = parse_stream_header(&strip[STREAM_OFFSET..]) {
        if let Ok(len) = stream_length(&strip[STREAM_OFFSET..]) {
            if w == full_w && hh * 2 == full_h {
                streams.push(TileStream { offset: STREAM_OFFSET, length: len, x: 0, y: 0, width: w, height: hh * 2 });
            }
        }
    }
    streams
}

fn parse_directory(stream: &[u8]) -> Result<Vec<DirEntry>> {
    let groups = group_sizes(stream, 0x10)?;
    let mut entries = Vec::new();
    let mut pos = 0x30usize;
    let mut base = 0usize;
    for (group, &group_len) in groups.iter().enumerate() {
        require(pos + 0x10 <= stream.len(), "directory beyond stream")?;
        let n = (stream[pos] & 0x0f) as usize;
        require(n <= 9, "directory entry count")?;
        let mut values = [0usize; 9];
        for (i, v) in values.iter_mut().take(5).enumerate() {
            *v = ((be32(stream, pos + 3 * i)? & 0x00ff_ffff) << 4) as usize;
        }
        let mut consumed = 0x10;
        if n >= 5 {
            require(pos + 0x20 <= stream.len(), "directory beyond stream")?;
            for i in 0..4 {
                values[5 + i] = ((be32(stream, pos + 0x10 + 3 * i)? & 0x00ff_ffff) << 4) as usize;
            }
            consumed = 0x20;
        }
        let mut local = 0usize;
        for (index, &length) in values.iter().take(n).enumerate() {
            let start = 0x80 + base + local;
            require(start <= stream.len() && length <= stream.len() - start, "packet beyond stream")?;
            entries.push(DirEntry { group, index, start, length });
            local += length;
        }
        require(local <= group_len, "group size mismatch")?;
        base += group_len;
        pos += consumed;
    }
    Ok(entries)
}

struct Record {
    byte_length: usize,
    selectors: [u8; 3],
    payload_offset: usize,
}

struct Packet<'a> {
    kind: usize,
    data: &'a [u8],
    records: Vec<Record>,
}

fn parse_packet(data: &[u8]) -> Result<Packet<'_>> {
    require(data.len() >= 16, "packet too short")?;
    let mut br = HeaderBits { data, pos: 0 };
    let control_count = br.read(16)? as usize;
    let extra_count = br.read(24)? as usize;
    let tag4 = br.read(4)?;
    let reserved4 = br.read(4)?;
    let kind = br.read(2)? as usize;
    let control_words = br.read(6)?;
    let block_count = br.read(16)? as usize;
    let width_marker = br.read(8)?;
    let reserved8 = br.read(8)?;
    for _ in 0..5 {
        require(br.read(8)? == 0, "packet header padding")?;
    }
    require(tag4 == 4 && reserved4 == 0 && control_words == 0, "packet header tag")?;
    require(kind == 1 || kind == 3, "packet type")?;
    require(block_count > 0 && block_count <= 30000, "packet block count")?;
    require(width_marker == 0x10 && reserved8 == 0, "packet width marker")?;
    let control_bytes = (control_count + 1) << 4;
    let total = (control_count + 1 + extra_count) << 4;
    require(total == data.len() && control_bytes <= data.len(), "packet size")?;
    let mut cursor = control_bytes;
    let mut records = Vec::with_capacity(block_count);
    for _ in 0..block_count {
        let byte_length = br.read(16)? as usize;
        let mut selectors = [0u8; 3];
        for s in selectors.iter_mut().take(kind) {
            *s = br.read(4)? as u8;
        }
        require(byte_length <= data.len() && cursor <= data.len() - byte_length, "packet record beyond packet")?;
        records.push(Record { byte_length, selectors, payload_offset: cursor });
        cursor += byte_length;
    }
    Ok(Packet { kind, data, records })
}

fn row_multiplier(group: usize) -> usize {
    match group {
        4 => 8,
        0 => 1,
        g => 1 << (g - 1),
    }
}

/// Decode the coefficient planes of packet `(group, index)`.
fn packet_planes(
    stream: &[u8],
    dir: &[DirEntry],
    mosaic_w: usize,
    coded_h: usize,
    group: usize,
    index: usize,
    parallel: bool,
) -> Result<Vec<Plane>> {
    let e = dir.iter().find(|e| e.group == group && e.index == index).ok_or_else(|| corrupt("missing packet"))?;
    let p = parse_packet(&stream[e.start..e.start + e.length])?;
    let comps = if p.kind == 1 { 1 } else { 3 };
    let row_width = match (p.kind, group) {
        (1, 4) => mosaic_w / 2,
        (1, _) => mosaic_w / 16,
        (3, 1..=3) => mosaic_w >> (5 - group),
        _ => return Err(corrupt("invalid packet type or group")),
    };
    let rm = row_multiplier(group);
    let rows = p.records.len() * rm;
    require(coded_h > 0 && coded_h <= 0x7fff, "coded height")?;
    let expected = (coded_h.div_ceil(16) + 1) * rm;
    let groups_per_row = row_width.div_ceil(4);
    require(row_width > 0 && groups_per_row > 0 && rows > 0, "packet geometry")?;
    if rows != expected {
        require(coded_h % 16 != 0 && expected.checked_sub(rows) == Some(rm), "packet row count")?;
    }
    require(p.records.len() <= stream.len() / 16 + 1, "packet record count")?;
    let mut planes: Vec<Plane> = (0..comps).map(|_| Plane::new(expected, row_width)).collect::<Result<_>>()?;
    // Records are independent bit streams: decode them in parallel, each into its
    // own rows of every component plane.
    let record_rows = rm * row_width;
    let mut bufs: Vec<Vec<i32>> = Vec::new();
    let decode_record = |ri: usize| -> Vec<i32> {
        let rec = &p.records[ri];
        let mut buf = vec![0i32; comps * record_rows];
        if rec.byte_length > 0 {
            let mut bits = CoeffBits::new(p.data, rec.payload_offset, rec.byte_length);
            for ci in 0..comps {
                let shift = if ci < p.kind { rec.selectors[ci] as u32 } else { 0 };
                for r in 0..rm {
                    let off = ci * record_rows + r * row_width;
                    decode_component(&mut bits, groups_per_row, shift, &mut buf[off..off + row_width]);
                }
            }
        }
        buf
    };
    bufs.extend(par::map((0..p.records.len()).collect(), parallel, decode_record));
    for (ri, buf) in bufs.iter().enumerate() {
        for (ci, plane) in planes.iter_mut().enumerate() {
            let dst = &mut plane.data[ri * record_rows..(ri + 1) * record_rows];
            dst.copy_from_slice(&buf[ci * record_rows..(ci + 1) * record_rows]);
        }
    }
    Ok(planes)
}

// ---------------------------------------------------------------------------
// Wavelet synthesis

/// Horizontal DPCM of the band-0 rows (sign-extended 16-bit deltas).
fn integrate(coeffs: &Plane, rows: usize, start: usize) -> Result<Plane> {
    require(rows <= coeffs.rows && start <= rows, "band-0 rows")?;
    let mut out = Plane::new(rows - start, coeffs.cols)?;
    for y in start..rows {
        let src = coeffs.row(y);
        let dst = out.row_mut(y - start);
        let mut acc = 0i32;
        for (d, &s) in dst.iter_mut().zip(src) {
            acc = acc.wrapping_add(sign16(s).wrapping_mul(2));
            *d = acc / 2;
        }
    }
    Ok(out)
}

/// Rows per parallel work item of the lifting passes.
const LIFT_ROWS: usize = 16;

/// Inverse 5/3 lifting along rows (vertical): `low`/`high` interleave to 2x rows.
fn inv53_vertical(low: &Plane, high: &Plane, parallel: bool) -> Result<Plane> {
    require(low.rows == high.rows && low.cols == high.cols, "vertical lifting shapes")?;
    let (h, w) = (low.rows, low.cols);
    let mut lo2 = Plane::new(h, w)?;
    par::for_each_row_chunk(&mut lo2.data, w, LIFT_ROWS, parallel, |first, chunk| {
        for (i, d) in chunk.chunks_mut(w).enumerate() {
            let y = first + i;
            let hp = high.row(y.saturating_sub(1));
            let hc = high.row(y);
            let l = low.row(y);
            for (x, d) in d.iter_mut().enumerate() {
                *d = l[x].wrapping_sub(fshr(hp[x].wrapping_add(hc[x]).wrapping_add(2), 2));
            }
        }
    });
    let mut out = Plane::new(h * 2, w)?;
    let lo2 = &lo2;
    par::for_each_row_chunk(&mut out.data, 2 * w, LIFT_ROWS, parallel, |first, chunk| {
        for (i, pair) in chunk.chunks_mut(2 * w).enumerate() {
            let y = first + i;
            let ln = lo2.row(if y + 1 < h { y + 1 } else { y });
            let lc = lo2.row(y);
            let hc = high.row(y);
            let (r0, r1) = pair.split_at_mut(w);
            r0.copy_from_slice(lc);
            for (x, d) in r1.iter_mut().enumerate() {
                *d = hc[x].wrapping_add(fshr(lc[x].wrapping_add(ln[x]), 1));
            }
        }
    });
    Ok(out)
}

/// Inverse 5/3 lifting along columns (horizontal): interleaves to 2x columns.
fn inv53_horizontal(low: &Plane, high: &Plane, parallel: bool) -> Result<Plane> {
    require(low.rows == high.rows && low.cols == high.cols, "horizontal lifting shapes")?;
    let (h, w) = (low.rows, low.cols);
    let mut out = Plane::new(h, w * 2)?;
    if w == 0 {
        return Ok(out);
    }
    par::for_each_row_chunk(&mut out.data, 2 * w, LIFT_ROWS, parallel, |first, chunk| {
        let mut lo2 = vec![0i32; w];
        for (i, o) in chunk.chunks_mut(2 * w).enumerate() {
            let y = first + i;
            let l = low.row(y);
            let hi = high.row(y);
            lo2[0] = l[0].wrapping_sub(fshr(hi[0].wrapping_add(hi[0]).wrapping_add(2), 2));
            for x in 1..w {
                lo2[x] = l[x].wrapping_sub(fshr(hi[x - 1].wrapping_add(hi[x]).wrapping_add(2), 2));
            }
            for x in 0..w {
                let ln = lo2[if x + 1 < w { x + 1 } else { x }];
                o[2 * x] = lo2[x];
                o[2 * x + 1] = hi[x].wrapping_add(fshr(lo2[x].wrapping_add(ln), 1));
            }
        }
    });
    Ok(out)
}

/// Vertical lifting where the high band leads (odd-phase stripe boundaries).
fn inv53_vertical_high_leading(low: &Plane, high: &Plane) -> Result<Plane> {
    require(low.cols == high.cols, "lifting shapes")?;
    require(high.rows == low.rows || high.rows == low.rows + 1, "lifting row counts")?;
    require(low.rows > 0, "empty lifting band")?;
    let (lh, hh, w) = (low.rows, high.rows, low.cols);
    let mut lo2 = Plane::new(lh, w)?;
    let mut hi2 = Plane::new(hh, w)?;
    for y in 0..lh {
        let next = if hh == lh + 1 || y + 1 < hh { y + 1 } else { y };
        let next = next.min(hh - 1);
        for x in 0..w {
            let v = high.at(y, x).wrapping_add(high.at(next, x)).wrapping_add(2);
            lo2.set(y, x, low.at(y, x).wrapping_sub(fshr(v, 2)));
        }
    }
    if hh == lh + 1 {
        for x in 0..w {
            hi2.set(0, x, high.at(0, x).wrapping_add(lo2.at(0, x)));
            hi2.set(hh - 1, x, high.at(hh - 1, x).wrapping_add(lo2.at(lh - 1, x)));
        }
        for y in 1..hh - 1 {
            for x in 0..w {
                let v = fshr(lo2.at(y - 1, x).wrapping_add(lo2.at(y, x)), 1);
                hi2.set(y, x, high.at(y, x).wrapping_add(v));
            }
        }
        let mut out = Plane::new(lh * 2 + 1, w)?;
        for y in 0..lh {
            out.row_mut(2 * y).copy_from_slice(hi2.row(y));
            out.row_mut(2 * y + 1).copy_from_slice(lo2.row(y));
        }
        let last = out.rows - 1;
        out.row_mut(last).copy_from_slice(hi2.row(hh - 1));
        return Ok(out);
    }
    for x in 0..w {
        hi2.set(0, x, high.at(0, x).wrapping_add(lo2.at(0, x)));
    }
    for y in 1..hh {
        for x in 0..w {
            let v = fshr(lo2.at(y - 1, x).wrapping_add(lo2.at(y, x)), 1);
            hi2.set(y, x, high.at(y, x).wrapping_add(v));
        }
    }
    let mut out = Plane::new(lh * 2, w)?;
    for y in 0..lh {
        out.row_mut(2 * y).copy_from_slice(hi2.row(y));
        out.row_mut(2 * y + 1).copy_from_slice(lo2.row(y));
    }
    Ok(out)
}

/// One synthesis level without stripe guard rows.
fn synth_level(ll: &Plane, s0: &Plane, s1: &Plane, s2: &Plane, parallel: bool) -> Result<Plane> {
    let (h, w) = (ll.rows, ll.cols);
    require(s0.cols == w && s1.cols == w && s2.cols == w, "subband widths")?;
    require(s0.rows > h && s1.rows > h && s2.rows > h && h > 0, "subband rows")?;
    let mut lh = Plane::new(h, w)?;
    let mut hh = Plane::new(h, w)?;
    for y in 0..h - 1 {
        lh.row_mut(y).copy_from_slice(s1.row(y + 1));
        hh.row_mut(y).copy_from_slice(s2.row(y + 1));
    }
    lh.row_mut(h - 1).copy_from_slice(s0.row(h));
    hh.row_mut(h - 1).copy_from_slice(s1.row(h));
    let lo = inv53_vertical(ll, &lh, parallel)?;
    let hi = inv53_vertical(&s0.head(h)?, &hh, parallel)?;
    inv53_horizontal(&lo, &hi, parallel)
}

/// Level with `edge` stripe guard rows at the top and bottom of the subbands.
fn synth_level_stride(
    ll: &Plane,
    s0: &Plane,
    s1: &Plane,
    s2: &Plane,
    edge: usize,
    odd: bool,
    parallel: bool,
) -> Result<Plane> {
    if edge == 0 {
        return synth_level(ll, s0, s1, s2, parallel);
    }
    let (h, w) = (ll.rows, ll.cols);
    require(s0.cols == w && s1.cols == w && s2.cols == w, "subband widths")?;
    require(h >= 2 * edge && s0.rows >= h + 2 * edge && s1.rows >= h + edge && s2.rows >= h, "subband rows")?;
    let mut hl = Plane::new(h, w)?;
    let mut lh = Plane::new(h, w)?;
    let mut hh = Plane::new(h, w)?;
    for y in 0..edge {
        hl.row_mut(y).copy_from_slice(s0.row(y));
        lh.row_mut(y).copy_from_slice(s0.row(y + edge));
        for x in 0..w {
            hh.set(y, x, edge_detail(s1.at(y, x), odd));
        }
    }
    for y in edge..h - edge {
        hl.row_mut(y).copy_from_slice(s0.row(y + edge));
        lh.row_mut(y).copy_from_slice(s1.row(y + edge));
        hh.row_mut(y).copy_from_slice(s2.row(y + edge));
    }
    for y in h - edge..h {
        let k = y - (h - edge);
        hl.row_mut(y).copy_from_slice(s0.row(h + k));
        lh.row_mut(y).copy_from_slice(s0.row(h + edge + k));
        for x in 0..w {
            hh.set(y, x, edge_detail(s1.at(h + k, x), odd));
        }
    }
    let lo = inv53_vertical(ll, &lh, parallel)?;
    let hi = inv53_vertical(&hl, &hh, parallel)?;
    inv53_horizontal(&lo, &hi, parallel)
}

/// Guarded (height not a multiple of 16) level 1.
fn synth_guard1(ll: &Plane, s0: &Plane, s1: &Plane, s2: &Plane, parallel: bool) -> Result<Plane> {
    let (h, w) = (ll.rows, ll.cols);
    require(s0.cols == w && s1.cols == w && s2.cols == w, "subband widths")?;
    require(s0.rows >= h + 2 && s1.rows >= h + 2 && s2.rows >= h + 2, "subband rows")?;
    let mut lh = Plane::new(h + 1, w)?;
    let mut hh = Plane::new(h + 1, w)?;
    let mut hl = Plane::new(h, w)?;
    for y in 0..h {
        hl.row_mut(y).copy_from_slice(s0.row(y + 1));
        lh.row_mut(y).copy_from_slice(s1.row(y + 1));
        hh.row_mut(y).copy_from_slice(s2.row(y + 1));
    }
    lh.row_mut(h).copy_from_slice(s0.row(h + 1));
    hh.row_mut(h).copy_from_slice(s1.row(h + 1));
    let lo = inv53_vertical_high_leading(ll, &lh)?;
    let hi = inv53_vertical_high_leading(&hl, &hh)?;
    inv53_horizontal(&lo, &hi, parallel)
}

/// Guarded level 2.
fn synth_guard2(ll: &Plane, s0: &Plane, s1: &Plane, s2: &Plane, odd: bool, parallel: bool) -> Result<Plane> {
    let (h, w) = (ll.rows, ll.cols);
    require(s0.cols == w && s1.cols == w && s2.cols == w, "subband widths")?;
    require(h > 0 && s0.rows >= h + 3 && s1.rows > h && s2.rows > h, "subband rows")?;
    let mut hl = Plane::new(h, w)?;
    let mut lh = Plane::new(h, w)?;
    let mut hh = Plane::new(h, w)?;
    lh.row_mut(0).copy_from_slice(s0.row(0));
    for x in 0..w {
        hh.set(0, x, edge_detail(s0.at(1, x), odd));
    }
    for y in 0..h {
        hl.row_mut(y).copy_from_slice(s0.row(y + 2));
    }
    for y in 1..h {
        lh.row_mut(y).copy_from_slice(s1.row(y + 1));
        hh.row_mut(y).copy_from_slice(s2.row(y + 1));
    }
    let lo = inv53_vertical_high_leading(ll, &lh)?;
    let hi = inv53_vertical_high_leading(&hl, &hh)?;
    inv53_horizontal(&lo, &hi, parallel)
}

/// Guarded level 3.
fn synth_guard3(ll: &Plane, s0: &Plane, s1: &Plane, s2: &Plane, odd: bool, parallel: bool) -> Result<Plane> {
    let (h, w) = (ll.rows, ll.cols);
    require(s0.cols == w && s1.cols == w && s2.cols == w, "subband widths")?;
    require(h >= 2 && s0.rows >= h + 5 && s1.rows >= h + 2 && s2.rows >= h + 2, "subband rows")?;
    let mut hl = Plane::new(h, w)?;
    let mut lh = Plane::new(h, w)?;
    let mut hh = Plane::new(h, w)?;
    hl.row_mut(0).copy_from_slice(s0.row(0));
    lh.row_mut(0).copy_from_slice(s0.row(1));
    lh.row_mut(h - 1).copy_from_slice(s0.row(h + 3));
    for x in 0..w {
        hh.set(0, x, edge_detail(s0.at(2, x), odd));
        hh.set(h - 1, x, edge_detail(s0.at(h + 4, x), odd));
    }
    for y in 1..h {
        hl.row_mut(y).copy_from_slice(s0.row(y + 3));
    }
    for y in 1..h - 1 {
        lh.row_mut(y).copy_from_slice(s1.row(y + 3));
        hh.row_mut(y).copy_from_slice(s2.row(y + 3));
    }
    let lo = inv53_vertical(ll, &lh, parallel)?;
    let hi = inv53_vertical(&hl, &hh, parallel)?;
    inv53_horizontal(&lo, &hi, parallel)
}

/// Final level: restore the two greens of each quad from their average (`ll`) and
/// the band-4 detail.
fn final_green(ll: &Plane, detail: &Plane, top_rows: usize, parallel: bool) -> Result<Plane> {
    let (h, w) = (ll.rows, ll.cols);
    require(top_rows <= 8 && detail.cols == w && detail.rows >= 8 + h.saturating_sub(top_rows), "green detail")?;
    if h == 0 || w == 0 {
        return Plane::new(h, w * 2);
    }
    // Band-4 rows used for each output row (the guard rows are skipped).
    let sel = |y: usize| detail.row(if y < top_rows { y } else { y + 8 - top_rows });
    let mut odd = Plane::new(h, w)?;
    par::for_each_row_chunk(&mut odd.data, w, LIFT_ROWS, parallel, |first, chunk| {
        for (i, o) in chunk.chunks_mut(w).enumerate() {
            let y = first + i;
            let (cur, prev, l) = (sel(y), sel(y.saturating_sub(1)), ll.row(y));
            for x in 0..w {
                let pred = if x + 1 < w {
                    fshr(cur[x + 1].wrapping_add(prev[x]).wrapping_add(cur[x]).wrapping_add(prev[x + 1]), 2)
                } else {
                    fshr(prev[x].wrapping_add(cur[x]).wrapping_mul(2), 2)
                };
                o[x] = ((2 * l[x] as i64 - pred as i64) >> 1) as i32;
            }
        }
    });
    let mut out = Plane::new(h, w * 2)?;
    let odd = &odd;
    par::for_each_row_chunk(&mut out.data, 2 * w, LIFT_ROWS, parallel, |first, chunk| {
        for (i, o) in chunk.chunks_mut(2 * w).enumerate() {
            let y = first + i;
            let (od, on, sl) = (odd.row(y), odd.row(if y + 1 < h { y + 1 } else { y }), sel(y));
            let v = (od[0].wrapping_add(on[0]) as i64 * 2) >> 2;
            o[0] = sl[0].wrapping_add(v as i32);
            o[1] = od[0];
            for x in 1..w {
                let s = od[x - 1].wrapping_add(on[x - 1]).wrapping_add(on[x]).wrapping_add(od[x]);
                o[2 * x] = sl[x].wrapping_add(fshr(s, 2));
                o[2 * x + 1] = od[x];
            }
        }
    });
    Ok(out)
}

/// Decoded planes of one tile (half resolution except `full_green`).
struct DecodedTile {
    full_green: Plane,
    red: Plane,
    blue: Plane,
}

/// Decode one tile stream.
fn decode_stream(stream: &[u8], parallel: bool) -> Result<DecodedTile> {
    let (width, half_h) = parse_stream_header(stream).ok_or_else(|| corrupt("invalid stream header"))?;
    let dir = parse_directory(stream)?;
    let coded_h = half_h * 2;
    let padded = coded_h.div_ceil(16) * 16;
    let guarded = coded_h != padded;
    let low_rows = padded / 16;
    let low_start = guarded as usize;

    // Three planes (green average, red and blue residuals) are independent until
    // the final green level.
    let plane = |index: usize| -> Result<Plane> {
        let b0 = packet_planes(stream, &dir, width, coded_h, 0, index, parallel)?;
        let mut p = integrate(&b0[0], low_rows, low_start)?;
        for group in 1..=3 {
            let edge = [0, 0, 1, 2][group];
            let odd = group == 3 && index != 0;
            let s = packet_planes(stream, &dir, width, coded_h, group, index, parallel)?;
            require(s.len() == 3, "detail packet with one component")?;
            p = if guarded {
                match group {
                    1 => synth_guard1(&p, &s[0], &s[1], &s[2], parallel)?,
                    2 => synth_guard2(&p, &s[0], &s[1], &s[2], false, parallel)?,
                    _ => synth_guard3(&p, &s[0], &s[1], &s[2], odd, parallel)?,
                }
            } else {
                synth_level_stride(&p, &s[0], &s[1], &s[2], edge, odd, parallel)?
            };
        }
        Ok(p)
    };
    let mut planes = par::map(vec![0usize, 1, 2], parallel, plane).into_iter();
    let (Some(green), Some(red), Some(blue)) = (planes.next(), planes.next(), planes.next()) else {
        return Err(corrupt("missing colour plane"));
    };
    let (green, red, blue) = (green?, red?, blue?);
    let g4 = packet_planes(stream, &dir, width, coded_h, 4, 0, parallel)?;
    let full_green = final_green(&green, &g4[0], if guarded { 2 } else { 4 }, parallel)?;
    Ok(DecodedTile { full_green, red, blue })
}

#[inline(always)]
fn clamp_code(x: i32) -> i32 {
    x.saturating_add(BIAS).clamp(0, 4095) - BIAS
}

#[inline(always)]
fn sample(x: i32) -> u16 {
    LLVC3_CURVE[x.saturating_add(BIAS).clamp(0, 4095) as usize]
}

/// Decode a whole ARW6 strip into `out` (`raw_w x raw_h`, row-major).
pub fn decode(strip: &[u8], out: &mut [u16], raw_w: usize, raw_h: usize, parallel: bool) -> Result<()> {
    require(raw_w > 0 && raw_h > 0 && raw_w % 16 == 0 && raw_h % 2 == 0, "frame size")?;
    require(out.len() >= raw_w * raw_h, "output buffer")?;
    require(strip.len() >= STREAM_OFFSET + 0x80, "strip too short")?;
    let streams = find_streams(strip, raw_w, raw_h);
    require(!streams.is_empty(), "no tile streams")?;
    let tiles: Vec<Result<DecodedTile>> =
        par::map(streams.clone(), parallel, |s| decode_stream(&strip[s.offset..s.offset + s.length], parallel));
    for (s, tile) in streams.iter().zip(tiles) {
        let t = tile?;
        let (half_h, half_w) = (s.height / 2, s.width / 2);
        require(
            t.full_green.rows >= half_h
                && t.full_green.cols == s.width
                && t.red.rows >= half_h
                && t.red.cols == half_w
                && t.blue.rows >= half_h
                && t.blue.cols == half_w,
            "tile plane sizes",
        )?;
        // Each output row pair of the tile is independent.
        let band = &mut out[s.y * raw_w..(s.y + s.height) * raw_w];
        par::for_each_row_chunk(band, 2 * raw_w, 8, parallel, |first, chunk| {
            for (i, pair) in chunk.chunks_mut(2 * raw_w).enumerate() {
                let y = first + i;
                let (r0, r1) = pair.split_at_mut(raw_w);
                let (r0, r1) = (&mut r0[s.x..s.x + s.width], &mut r1[s.x..s.x + s.width]);
                let (fg, red, blue) = (t.full_green.row(y), t.red.row(y), t.blue.row(y));
                for x in 0..half_w {
                    let g0 = clamp_code(fg[2 * x]);
                    let g1 = clamp_code(fg[2 * x + 1]);
                    let gavg = ((g0 as i64 + g1 as i64) >> 1) as i32;
                    r0[2 * x] = sample(gavg.wrapping_add(red[x].wrapping_mul(2)));
                    r0[2 * x + 1] = sample(fg[2 * x + 1]);
                    r1[2 * x] = sample(fg[2 * x]);
                    r1[2 * x + 1] = sample(gavg.wrapping_add(blue[x].wrapping_mul(2)));
                }
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_match_definitions() {
        for x in -9..=9 {
            assert_eq!(fshr(x, 1), (x as f64 / 2.0).floor() as i32);
            assert_eq!(fshr(x, 2), (x as f64 / 4.0).floor() as i32);
        }
        assert_eq!(edge_detail(3, true), 7);
        assert_eq!(edge_detail(3, false), 6);
        assert_eq!(edge_detail(-4, false), -9);
        assert_eq!(edge_detail(-4, true), -8);
        assert_eq!(edge_detail(0, false), 0);
        assert_eq!(sign16(0xffff), -1);
        assert_eq!(sample(-5000), LLVC3_CURVE[0]);
        assert_eq!(sample(5000), 39002);
    }

    #[test]
    fn coefficient_bits_and_limits() {
        let data = [0b1010_0000u8, 0xff, 0, 0, 0, 0, 0, 0, 0x80];
        let mut b = CoeffBits::new(&data, 0, 1);
        assert_eq!(b.read(3), 0b101);
        assert_eq!(b.read(8), 0b0000_0111);
        // allowance is two words (128 bits); bits past it read as zero with status set
        let mut b = CoeffBits::new(&data, 0, 1);
        b.read(32);
        b.read(32);
        b.read(32);
        b.read(24);
        assert_eq!(b.read(16), 0);
        assert_eq!(b.status, 2);
    }

    #[test]
    fn inverse_lifting_reconstructs_forward_53() {
        // Forward 5/3 on a 1-D signal, then our horizontal inverse.
        let sig: Vec<i32> = vec![10, 13, 7, 2, 8, 30, 31, 29, 5, 6];
        let n = sig.len() / 2;
        let mut high = vec![0; n];
        let mut low = vec![0; n];
        for i in 0..n {
            let r = if 2 * i + 2 < sig.len() { sig[2 * i + 2] } else { sig[2 * i] };
            high[i] = sig[2 * i + 1] - fshr(sig[2 * i] + r, 1);
        }
        for i in 0..n {
            let hp = high[i.saturating_sub(1)];
            low[i] = sig[2 * i] + fshr(hp + high[i] + 2, 2);
        }
        let lo = Plane { rows: 1, cols: n, data: low };
        let hi = Plane { rows: 1, cols: n, data: high };
        assert_eq!(inv53_horizontal(&lo, &hi, false).unwrap().data, sig);
    }

    /// Bit-at-a-time reference of the coefficient stream semantics.
    struct RefBits<'a> {
        data: &'a [u8],
        start: usize,
        pos: usize,
        limit: usize,
        status: u8,
    }

    impl RefBits<'_> {
        fn bit(&mut self) -> u32 {
            if self.pos >= self.limit {
                self.status = 2;
                return 0;
            }
            let byte = self.start + self.pos / 8;
            let b = self.data.get(byte).map(|&b| (b >> (7 - self.pos % 8)) & 1).unwrap_or(0) as u32;
            self.pos += 1;
            b
        }
        fn read(&mut self, n: u32) -> u32 {
            (0..n).fold(0, |a, _| a << 1 | self.bit())
        }
        fn unary_plus_one(&mut self) -> i32 {
            let mut zeros = 0;
            while self.status == 0 {
                let b = self.bit();
                if self.status != 0 {
                    break;
                }
                if b != 0 {
                    return zeros + 1;
                }
                zeros += 1;
            }
            zeros + 1
        }
        fn leading_one_within(&mut self, k: u32) -> Option<u32> {
            (0..k).find(|_| self.bit() != 0)
        }
    }

    #[test]
    fn coefficient_reader_matches_bit_reference() {
        let mut x = 99u32;
        let data: Vec<u8> = (0..300)
            .map(|i| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                let b = (x >> 24) as u8;
                if i % 5 == 0 { 0 } else { b & (b >> 1) }
            })
            .collect();
        for (off, len) in [(0usize, 40usize), (3, 100), (17, 7), (250, 60)] {
            let mut a = CoeffBits::new(&data, off, len);
            let words = ((len.div_ceil(8)) + 1) & !1;
            let mut b = RefBits { data: &data, start: off, pos: 0, limit: words * 64, status: 0 };
            let mut y = off as u32 + 7;
            for step in 0..400 {
                y = y.wrapping_mul(1103515245).wrapping_add(12345);
                match (y >> 16) % 3 {
                    0 => {
                        let n = (y >> 8) % 33;
                        assert_eq!(a.read(n), b.read(n), "read {n} step {step}");
                    }
                    1 => assert_eq!(a.unary_plus_one(), b.unary_plus_one(), "unary step {step}"),
                    _ => {
                        let k = (y >> 8) % 20;
                        assert_eq!(a.leading_one_within(k), b.leading_one_within(k), "lead {k} step {step}");
                    }
                }
                assert_eq!(a.status != 0, b.status != 0, "status step {step}");
            }
        }
    }

    #[test]
    fn garbage_strips_are_rejected() {
        let mut out = vec![0u16; 64 * 32];
        assert!(decode(&[0u8; 100], &mut out, 64, 32, false).is_err());
        let mut strip = vec![0u8; 0x400];
        strip[0x200..0x204].copy_from_slice(b"A000");
        assert!(decode(&strip, &mut out, 64, 32, false).is_err());
    }
}
