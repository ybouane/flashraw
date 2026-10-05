//! Lossless JPEG (ITU T.81 process 14, SOF3) decoder.
//!
//! This is the workhorse codec of the RAW world: CR2, most DNGs, Sony lossless ARW,
//! Hasselblad, Leaf, Kodak, Pentax K-1 DNG, Phase One (some), etc.
//!
//! Output model: the frame is decoded row by row into an L1-resident row buffer of
//! `width * clrs` samples, which is handed to a sink. Callers map JPEG rows to their
//! final layout (CR2 vertical slices, DNG tile placement, ...) in the sink, so no
//! full-size intermediate image is ever allocated.
//!
//! Canon sRAW/mRAW frames (component 0 with sampling factors > 1) are decoded as
//! `clrs = ncomp + sraw` interleaved values per MCU, where luma samples chain-predict
//! from the previous luma sample (matching Canon's encoder).

use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, ReadAt, Result};

pub use crate::ljpeg_par::decode_frame_parallel;

/// Frame component.
#[derive(Debug, Clone, Copy)]
pub struct Component {
    pub id: u8,
    pub h: u8,
    pub v: u8,
}

/// A parsed lossless JPEG stream (headers only; decoding is on demand).
#[derive(Debug, Clone)]
pub struct LjpegHeader {
    /// Sample precision in bits (after point transform).
    pub bits: u32,
    /// Frame width in MCUs (samples per line per component).
    pub width: u32,
    pub height: u32,
    pub components: Vec<Component>,
    /// Values per MCU column: ncomp + extra luma samples for sRAW.
    pub clrs: u32,
    /// Number of extra luma samples per MCU (0 = normal frame).
    pub sraw: u32,
    pub predictor: u32,
    pub point_transform: u32,
    pub restart_interval: u32,
    /// Huffman table per value position in the MCU.
    pub(crate) tables: Vec<HuffTable>,
    /// Table identity per entry of `tables`: equal ids mean identical tables (slots with
    /// byte-identical definitions share the id of the first).
    pub(crate) table_ids: Vec<u8>,
    /// Offset of entropy-coded data in the input.
    pub scan_offset: usize,
}

/// Decoder options.
#[derive(Debug, Clone, Copy)]
pub struct LjpegOptions {
    /// Length-16 diffs are `-32768` with no extra bits (true for everything except DNG < 1.1).
    pub dng_bug16: bool,
}

impl Default for LjpegOptions {
    fn default() -> Self {
        LjpegOptions { dng_bug16: true }
    }
}

impl LjpegHeader {
    /// Parse markers up to and including the first SOS.
    pub fn parse(data: &[u8], opts: LjpegOptions) -> Result<Self> {
        if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
            return Err(Error::corrupt("lossless JPEG: missing SOI"));
        }
        let mut pos = 2usize;
        let mut dht: [Option<HuffTable>; 4] = [None, None, None, None];
        // Raw DHT bytes per slot: slots holding identical tables share an id.
        let mut dht_raw: [Option<&[u8]>; 4] = [None; 4];
        let mut frame: Option<(u32, u32, u32, Vec<Component>, u32)> = None;
        let mut restart = 0u32;
        let mut sof_marker = 0xC3u8;
        loop {
            // Skip fill bytes / garbage until a marker.
            while pos < data.len() && data[pos] != 0xFF {
                pos += 1;
            }
            while pos < data.len() && data[pos] == 0xFF {
                pos += 1;
            }
            let marker = *data.get(pos).ok_or_else(|| Error::corrupt("lossless JPEG: truncated header"))?;
            pos += 1;
            if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
                continue;
            }
            if marker == 0xD9 {
                return Err(Error::corrupt("lossless JPEG: EOI before SOS"));
            }
            let len = data.u16be(pos)? as usize;
            if len < 2 {
                return Err(Error::corrupt("lossless JPEG: bad segment length"));
            }
            let seg = flashraw_core::bytes::slice_at(data, pos + 2, len - 2)?;
            match marker {
                0xC3 | 0xC0 | 0xC1 => {
                    // Some encoders (e.g. Blackmagic) label lossless streams SOF0/SOF1; they are
                    // accepted and the scan's predictor is verified at SOS.
                    sof_marker = marker;
                    if seg.len() < 6 {
                        return Err(Error::corrupt("lossless JPEG: short SOF"));
                    }
                    let bits = seg[0] as u32;
                    let height = seg.u16be(1)? as u32;
                    let width = seg.u16be(3)? as u32;
                    let n = seg[5] as usize;
                    if n == 0 || n > 4 || seg.len() < 6 + 3 * n {
                        return Err(Error::corrupt(format!("lossless JPEG: {n} components")));
                    }
                    let comps: Vec<Component> = (0..n)
                        .map(|i| Component { id: seg[6 + 3 * i], h: seg[7 + 3 * i] >> 4, v: seg[7 + 3 * i] & 15 })
                        .collect();
                    // Canon sRAW: extra luma samples per MCU from component 0's sampling factors.
                    let sraw = ((comps[0].h as u32 * comps[0].v as u32).wrapping_sub(1)) & 3;
                    frame = Some((bits, width, height, comps, sraw));
                }
                0xC4 => {
                    let mut p = 0usize;
                    while p < seg.len() {
                        let class_id = seg[p];
                        let id = (class_id & 0x0F) as usize;
                        if id > 3 || p + 17 > seg.len() {
                            return Err(Error::corrupt("lossless JPEG: bad DHT"));
                        }
                        let counts = &seg[p + 1..p + 17];
                        let total: usize = counts.iter().map(|&c| c as usize).sum();
                        let vals = flashraw_core::bytes::slice_at(seg, p + 17, total)?;
                        let mut t = HuffTable::from_counts(counts, vals, true)?;
                        t.dng_bug16 = opts.dng_bug16;
                        dht[id] = Some(t);
                        dht_raw[id] = Some(&seg[p + 1..p + 17 + total]);
                        p += 17 + total;
                    }
                }
                0xDD => {
                    restart = seg.u16be(0)? as u32;
                }
                0xDA => {
                    let (bits, width, height, comps, sraw) =
                        frame.take().ok_or_else(|| Error::corrupt("lossless JPEG: SOS before SOF"))?;
                    let ns = *seg.first().ok_or_else(|| Error::corrupt("lossless JPEG: short SOS"))? as usize;
                    if ns != comps.len() || seg.len() < 1 + 2 * ns + 3 {
                        return Err(Error::corrupt("lossless JPEG: SOS component mismatch"));
                    }
                    let mut comp_tables = Vec::with_capacity(ns);
                    for i in 0..ns {
                        let td = (seg[2 + 2 * i] >> 4) as usize;
                        comp_tables.push(td.min(3));
                    }
                    let predictor = seg[1 + 2 * ns] as u32;
                    if sof_marker != 0xC3 && !(1..=7).contains(&predictor) {
                        return Err(Error::unsupported_compression(
                            "ljpeg",
                            format!("JPEG SOF{:X} DCT stream (not lossless)", sof_marker - 0xC0),
                        ));
                    }
                    let pt = (seg[3 + 2 * ns] & 15) as u32;
                    let clrs = comps.len() as u32 + sraw;
                    // Table per MCU position: luma table repeated for sRAW extra samples.
                    let mut tables = Vec::with_capacity(clrs as usize);
                    let mut table_ids = Vec::with_capacity(clrs as usize);
                    for c in 0..clrs as usize {
                        let comp = c.saturating_sub(sraw as usize);
                        // A missing table falls back to the first one defined.
                        let id = if dht[comp_tables[comp]].is_some() {
                            comp_tables[comp]
                        } else {
                            dht.iter().position(|t| t.is_some()).unwrap_or(0)
                        };
                        let t =
                            dht[id].clone().ok_or_else(|| Error::corrupt("lossless JPEG: missing Huffman table"))?;
                        tables.push(t);
                        let canon = dht_raw.iter().position(|r| r.is_some() && *r == dht_raw[id]).unwrap_or(id);
                        table_ids.push(canon as u8);
                    }
                    if bits == 0 || bits > 16 || pt >= bits || width == 0 || height == 0 {
                        return Err(Error::corrupt(format!(
                            "lossless JPEG: invalid frame {width}x{height} @ {bits} bits"
                        )));
                    }
                    return Ok(LjpegHeader {
                        bits: bits - pt,
                        width,
                        height,
                        components: comps,
                        clrs,
                        sraw,
                        predictor,
                        point_transform: pt,
                        restart_interval: restart,
                        tables,
                        table_ids,
                        scan_offset: pos + len,
                    });
                }
                _ => {}
            }
            pos += len;
        }
    }

    /// Values per decoded row.
    pub fn row_len(&self) -> usize {
        self.width as usize * self.clrs as usize
    }

    /// Huffman table used for value position `i` of an MCU (`i < clrs`). Lets
    /// vendor codecs with non-standard sample layouts (e.g. Sony YCbCr) reuse the
    /// parsed stream headers.
    pub fn table(&self, i: usize) -> Option<&HuffTable> {
        self.tables.get(i)
    }
}

/// Decode all rows of `data` (a complete lossless JPEG stream), invoking
/// `sink(jpeg_row, samples)` for each row in order.
pub fn decode_rows<F>(data: &[u8], hdr: &LjpegHeader, mut sink: F) -> Result<()>
where
    F: FnMut(usize, &[u16]) -> Result<()>,
{
    let rl = hdr.row_len();
    let mut cur = vec![0u16; rl];
    let mut prev = vec![0u16; rl];
    let scan = data.get(hdr.scan_offset..).ok_or_else(|| Error::corrupt("lossless JPEG: scan beyond data"))?;
    // Remove byte stuffing once, up front: the hot loop then runs on a plain MSB pump
    // whose refill needs no per-byte 0xFF checks.
    let un = unstuff_scan(scan, hdr.restart_interval != 0);
    let mut segment = 0usize;
    let mut bits = BitPumpMsb::new(un.segment(0));
    let init = 1u32 << (hdr.bits - 1);
    let mut vpred = [init as u16; 6];
    let restart = if hdr.restart_interval > 0 { hdr.restart_interval as usize } else { usize::MAX };
    let fast = fast_rows(hdr);
    for jrow in 0..hdr.height as usize {
        if restart != usize::MAX && (jrow * hdr.width as usize) % restart == 0 {
            vpred = [init as u16; 6];
            if jrow > 0 {
                segment += 1;
                if segment >= un.segments.len() {
                    return Err(Error::corrupt("lossless JPEG: missing restart marker"));
                }
                bits = BitPumpMsb::new(un.segment(segment));
            }
        }
        decode_row(hdr, fast, &mut bits, &mut cur, &prev, jrow, &mut vpred)?;
        sink(jrow, &cur)?;
        std::mem::swap(&mut cur, &mut prev);
    }
    Ok(())
}

/// Rows can use the predictor-1 row decoder.
pub(crate) fn fast_rows(hdr: &LjpegHeader) -> bool {
    hdr.sraw == 0 && (hdr.predictor == 1 || hdr.height == 1)
}

/// Decode row `jrow` into `cur` and check the stream has not run out.
#[inline(always)]
pub(crate) fn decode_row(
    hdr: &LjpegHeader,
    fast: bool,
    bits: &mut BitPumpMsb<'_>,
    cur: &mut [u16],
    prev: &[u16],
    jrow: usize,
    vpred: &mut [u16; 6],
) -> Result<()> {
    let clrs = hdr.clrs as usize;
    if fast {
        decode_row_pred1(&hdr.tables, bits, cur, clrs, vpred)?;
    } else {
        decode_row_generic(hdr, bits, cur, prev, jrow, clrs, vpred)?;
    }
    // Up to two bytes consumed past the end are tolerated at the very end (some
    // encoders drop the final partial byte); anything more means truncation.
    let slack = if jrow + 1 == hdr.height as usize { 2 } else { 64 };
    if bits.past_end() > slack {
        return Err(Error::corrupt(format!("lossless JPEG: data ends at row {jrow}/{}", hdr.height)));
    }
    Ok(())
}

/// Entropy-coded data with `FF 00` stuffing removed, split at restart markers.
pub struct Unstuffed {
    pub(crate) data: Vec<u8>,
    /// `(start, end)` of each restart interval within `data`.
    pub(crate) segments: Vec<(usize, usize)>,
}

impl Unstuffed {
    pub(crate) fn segment(&self, i: usize) -> &[u8] {
        self.segments.get(i).map(|&(a, b)| &self.data[a..b]).unwrap_or(&[])
    }
}

/// Bit mask of the 0xFF bytes of `w` (the high bit of each such byte; bits above the first
/// hit may be spurious, the lowest one is exact).
#[inline(always)]
fn ff_mask(w: u64) -> u64 {
    let inv = !w;
    inv.wrapping_sub(0x0101_0101_0101_0101) & !inv & 0x8080_8080_8080_8080
}

/// Index of the first 0xFF byte (word-at-a-time scan).
#[cfg(test)]
fn find_ff(s: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 8 <= s.len() {
        let hit = ff_mask(u64::from_le_bytes(s[i..i + 8].try_into().unwrap()));
        if hit != 0 {
            return Some(i + (hit.trailing_zeros() / 8) as usize);
        }
        i += 8;
    }
    s[i..].iter().position(|&b| b == 0xFF).map(|p| i + p)
}

/// Strip byte stuffing from a JPEG scan. `FF 00` becomes `FF`; `FF D0..D7` (restart)
/// starts a new segment; any other marker (or a lone trailing `FF`) ends the scan,
/// exactly where the byte-wise JPEG pump would stop feeding data.
pub fn unstuff(scan: &[u8]) -> Unstuffed {
    unstuff_scan(scan, true)
}

/// [`unstuff`]; without `restarts` a restart marker ends the scan like any other marker
/// (a decoder reading one restart interval must not pull in the rest of the stream).
pub fn unstuff_scan(scan: &[u8], restarts: bool) -> Unstuffed {
    if scan.len() >= PAR_UNSTUFF_MIN && flashraw_core::par::threads() > 1 {
        return unstuff_parallel(scan, restarts);
    }
    unstuff_serial(scan, restarts)
}

fn unstuff_serial(scan: &[u8], restarts: bool) -> Unstuffed {
    // Output never outgrows the input; 8 bytes of slack let every step store a whole word.
    let mut data = vec![0u8; scan.len() + 8];
    let mut breaks = Vec::new();
    let (n, _) = unstuff_into(scan, restarts, &mut data, &mut breaks);
    data.truncate(n);
    Unstuffed::new(data, breaks)
}

/// Scans at least this long are unstuffed on all threads.
const PAR_UNSTUFF_MIN: usize = 2 << 20;

impl Unstuffed {
    /// From the output and the output positions of restart markers.
    fn new(data: Vec<u8>, breaks: Vec<usize>) -> Self {
        let mut segments = Vec::with_capacity(breaks.len() + 1);
        let mut start = 0;
        for b in breaks {
            segments.push((start, b));
            start = b;
        }
        segments.push((start, data.len()));
        Unstuffed { data, segments }
    }
}

/// Unstuff `scan` into `out`, which must hold the whole output (plus 8 bytes for the
/// fast path to be used up to the end). Pushes the output position of each restart
/// marker to `breaks`; returns the output length and whether a terminating marker (or
/// a lone trailing `FF`) was met.
fn unstuff_into(scan: &[u8], restarts: bool, out: &mut [u8], breaks: &mut Vec<usize>) -> (usize, bool) {
    let (mut i, mut o) = (0usize, 0usize);
    loop {
        // Copy whole words until one holds a 0xFF; the word is stored anyway and the
        // output cursor advances only past the bytes before the 0xFF.
        let mut at_ff = false;
        while i + 8 <= scan.len() && o + 8 <= out.len() {
            let w = u64::from_le_bytes(scan[i..i + 8].try_into().unwrap());
            out[o..o + 8].copy_from_slice(&w.to_le_bytes());
            let hit = ff_mask(w);
            if hit != 0 {
                let k = (hit.trailing_zeros() / 8) as usize;
                i += k;
                o += k;
                at_ff = true;
                break;
            }
            i += 8;
            o += 8;
        }
        if !at_ff {
            // Near the end of the input or of the output.
            match scan[i..].iter().position(|&b| b == 0xFF) {
                None => {
                    let n = scan.len() - i;
                    out[o..o + n].copy_from_slice(&scan[i..]);
                    return (o + n, false);
                }
                Some(k) => {
                    out[o..o + k].copy_from_slice(&scan[i..i + k]);
                    i += k;
                    o += k;
                }
            }
        }
        // `scan[i]` is 0xFF.
        match scan.get(i + 1) {
            Some(0x00) => {
                out[o] = 0xFF;
                o += 1;
                i += 2;
            }
            Some(&m) if restarts && (0xD0..=0xD7).contains(&m) => {
                breaks.push(o);
                i += 2;
            }
            _ => return (o, true),
        }
    }
}

/// Output length of [`unstuff_into`] and whether the scan terminates.
fn unstuffed_len(scan: &[u8], restarts: bool) -> (usize, bool) {
    let mut removed = 0usize;
    let mut i = 0usize;
    loop {
        let mut at_ff = false;
        while i + 8 <= scan.len() {
            let hit = ff_mask(u64::from_le_bytes(scan[i..i + 8].try_into().unwrap()));
            if hit != 0 {
                i += (hit.trailing_zeros() / 8) as usize;
                at_ff = true;
                break;
            }
            i += 8;
        }
        if !at_ff {
            match scan[i..].iter().position(|&b| b == 0xFF) {
                None => return (scan.len() - removed, false),
                Some(k) => i += k,
            }
        }
        match scan.get(i + 1) {
            Some(0x00) => removed += 1,
            Some(&m) if restarts && (0xD0..=0xD7).contains(&m) => removed += 2,
            _ => return (i - removed, true),
        }
        i += 2;
    }
}

/// [`unstuff_scan`] on all threads. Every `FF` starts a two-byte sequence, so the scan
/// splits into chunks that do not start right after an `FF`, and each chunk unstuffs
/// on its own: lengths first, then each chunk writes its own part of the output. The
/// output is that of the first chunks up to the one that terminates.
fn unstuff_parallel(scan: &[u8], restarts: bool) -> Unstuffed {
    use flashraw_core::par;
    let n = par::threads() * 2;
    let mut cuts = vec![0usize];
    for k in 1..n {
        let mut b = (scan.len() * k / n).max(*cuts.last().unwrap() + 1);
        while b < scan.len() && scan[b - 1] == 0xFF {
            b += 1;
        }
        if b < scan.len() {
            cuts.push(b);
        }
    }
    cuts.push(scan.len());
    let chunks: Vec<&[u8]> = cuts.windows(2).map(|w| &scan[w[0]..w[1]]).collect();
    let lens = par::map(chunks.clone(), true, |c| unstuffed_len(c, restarts));
    let used = lens.iter().position(|&(_, end)| end).map_or(lens.len(), |k| k + 1);
    let mut bounds = Vec::with_capacity(used);
    let mut total = 0usize;
    for &(len, _) in &lens[..used] {
        bounds.push((total, total + len));
        total += len;
    }
    let mut data = vec![0u8; total];
    let Some(parts) = par::split_disjoint(&mut data, &bounds) else { return unstuff_serial(scan, restarts) };
    let jobs: Vec<(&[u8], &mut [u8], usize)> =
        chunks.into_iter().zip(parts).zip(&bounds).map(|((c, p), &(start, _))| (c, p, start)).collect();
    let breaks = par::map(jobs, true, |(c, p, start)| {
        let mut b = Vec::new();
        unstuff_into(c, restarts, p, &mut b);
        b.into_iter().map(|x| x + start).collect::<Vec<_>>()
    });
    Unstuffed::new(data, breaks.into_iter().flatten().collect())
}

/// Predictor 1 (left neighbour), the overwhelmingly common case.
#[inline]
fn decode_row_pred1<B: BitRead>(
    tables: &[HuffTable],
    bits: &mut B,
    cur: &mut [u16],
    clrs: usize,
    vpred: &mut [u16; 6],
) -> Result<()> {
    // First MCU predicts from the first MCU of the previous row (vpred accumulates).
    for c in 0..clrs {
        let diff = tables[c].decode_diff(bits)?;
        vpred[c] = (vpred[c] as i32 + diff) as u16;
        cur[c] = vpred[c];
    }
    match clrs {
        1 => {
            let t = &tables[0];
            let mut p = cur[0] as i32;
            for v in cur[1..].iter_mut() {
                p = (p + t.decode_diff(bits)?) as u16 as i32;
                *v = p as u16;
            }
        }
        2 => decode_interleaved::<B, 2>(tables, bits, cur)?,
        3 => decode_interleaved::<B, 3>(tables, bits, cur)?,
        4 => decode_interleaved::<B, 4>(tables, bits, cur)?,
        _ => {
            for i in clrs..cur.len() {
                let c = i % clrs;
                let diff = tables[c].decode_diff(bits)?;
                cur[i] = (cur[i - clrs] as i32 + diff) as u16;
            }
        }
    }
    Ok(())
}

/// Predictor-1 decode of `N` interleaved components (CR2 uses 2 or 4): predictors stay in
/// registers and there is no per-sample component index arithmetic.
#[inline(always)]
fn decode_interleaved<B: BitRead, const N: usize>(tables: &[HuffTable], bits: &mut B, cur: &mut [u16]) -> Result<()> {
    let t: [&HuffTable; N] = std::array::from_fn(|c| &tables[c]);
    let mut p: [u16; N] = std::array::from_fn(|c| cur[c]);
    let mut err = false;
    for px in cur[N..].chunks_exact_mut(N) {
        // One refill (>= 56 bits for byte pumps) covers two table-hit samples (<= 27 bits
        // each); the odd-sample check only triggers for pumps with coarser refills.
        for c in 0..N {
            bits.fill_to(if c % 2 == 0 { 54 } else { 27 });
            p[c] = (p[c] as i32 + t[c].decode_diff_buffered(bits, &mut err)) as u16;
            px[c] = p[c];
        }
    }
    if err {
        return Err(Error::corrupt("lossless JPEG: invalid Huffman code"));
    }
    Ok(())
}

#[inline(never)]
fn decode_row_generic<B: BitRead>(
    hdr: &LjpegHeader,
    bits: &mut B,
    cur: &mut [u16],
    prev: &[u16],
    jrow: usize,
    clrs: usize,
    vpred: &mut [u16; 6],
) -> Result<()> {
    reconstruct_row(hdr, |c| hdr.tables[c].decode_diff(bits), cur, prev, jrow, clrs, vpred)
}

/// Rebuild one row from its differences (`next(c)` yields the next difference, `c` being
/// the MCU position): every predictor, sRAW sample layouts, and the vertical chain of
/// first-column predictors kept in `vpred`.
#[inline(always)]
pub(crate) fn reconstruct_row(
    hdr: &LjpegHeader,
    mut next: impl FnMut(usize) -> Result<i32>,
    cur: &mut [u16],
    prev: &[u16],
    jrow: usize,
    clrs: usize,
    vpred: &mut [u16; 6],
) -> Result<()> {
    let sraw = hdr.sraw as usize;
    let psv = hdr.predictor;
    let mut spred: i32 = 0;
    let width = hdr.width as usize;
    for col in 0..width {
        for c in 0..clrs {
            let i = col * clrs + c;
            let diff = next(c)?;
            let mut pred: i32 = if sraw > 0 && c <= sraw && (col | c) != 0 {
                spred
            } else if col > 0 {
                cur[i - clrs] as i32
            } else {
                let p = vpred[c] as i32;
                vpred[c] = (p + diff) as u16;
                p
            };
            if jrow > 0 && col > 0 {
                let rb = prev[i] as i32;
                let rc = prev[i - clrs] as i32;
                pred = match psv {
                    1 => pred,
                    2 => rb,
                    3 => rc,
                    4 => pred + rb - rc,
                    5 => pred + ((rb - rc) >> 1),
                    6 => rb + ((pred - rc) >> 1),
                    7 => (pred + rb) >> 1,
                    _ => 0,
                };
            }
            let v = (pred + diff) as u16;
            cur[i] = v;
            if c <= sraw {
                spred = v as i32;
            }
        }
    }
    Ok(())
}

/// [`decode_rows`] that first decodes large scans on all threads when `parallel` is set
/// ([`decode_frame_parallel`]; scans with restart markers one interval per task). Rows reach `sink` in order either
/// way with identical values; corrupt streams always take the sequential path, so errors
/// are identical too.
pub fn decode_rows_par<F>(data: &[u8], hdr: &LjpegHeader, parallel: bool, sink: F) -> Result<()>
where
    F: FnMut(usize, &[u16]) -> Result<()>,
{
    decode_rows_par_limit(data, hdr, parallel, hdr.height as usize, sink)
}

/// [`decode_rows_par`] for callers that only need the first `rows` rows and ignore what
/// the stream holds after them (the parallel path then stops there; the sequential one
/// still delivers every row).
pub fn decode_rows_par_limit<F>(data: &[u8], hdr: &LjpegHeader, parallel: bool, rows: usize, mut sink: F) -> Result<()>
where
    F: FnMut(usize, &[u16]) -> Result<()>,
{
    let rl = hdr.row_len();
    let rows = rows.min(hdr.height as usize);
    let total = rl * rows;
    // Restart intervals are decoded independently, without speculation, so they pay off
    // on smaller frames.
    let min = if hdr.restart_interval != 0 {
        flashraw_core::par::PAR_MIN_SAMPLES
    } else {
        crate::ljpeg_par::PARALLEL_MIN_SAMPLES
    };
    if parallel && total >= min {
        let mut frame = vec![0u16; total];
        if crate::ljpeg_par::decode_frame_rows(data, hdr, rows, &mut frame)? {
            for (jrow, row) in frame.chunks_exact(rl).enumerate() {
                sink(jrow, row)?;
            }
            return Ok(());
        }
    }
    decode_rows(data, hdr, sink)
}

/// Decode a lossless JPEG tile into a destination buffer.
///
/// The decoded sample stream fills a virtual tile whose rows are `tile_row_len`
/// samples long (JPEG rows need not match tile rows: DNG commonly encodes a
/// `W`-pixel tile row as a `W/2 x 2-component` JPEG row, and some encoders put two
/// tile rows in one JPEG row). Only samples with column `< clip_w` and row
/// `< clip_h` are written, at `dst[row * dst_stride + col]` — this clips tiles
/// that overhang the image edge.
pub fn decode_tile(
    data: &[u8],
    opts: LjpegOptions,
    dst: &mut [u16],
    dst_stride: usize,
    tile_row_len: usize,
    clip_w: usize,
    clip_h: usize,
) -> Result<LjpegHeader> {
    decode_tile_impl(data, opts, dst, dst_stride, tile_row_len, clip_w, clip_h, false)
}

#[allow(clippy::too_many_arguments)]
fn decode_tile_impl(
    data: &[u8],
    opts: LjpegOptions,
    dst: &mut [u16],
    dst_stride: usize,
    tile_row_len: usize,
    clip_w: usize,
    clip_h: usize,
    parallel: bool,
) -> Result<LjpegHeader> {
    let hdr = LjpegHeader::parse(data, opts)?;
    if tile_row_len == 0 || clip_w == 0 || clip_h == 0 {
        return Ok(hdr);
    }
    let clip_w = clip_w.min(tile_row_len).min(dst_stride);
    let (mut row, mut col) = (0usize, 0usize);
    decode_rows_par(data, &hdr, parallel, |_, mut src| {
        while !src.is_empty() && row < clip_h {
            let take = (tile_row_len - col).min(src.len());
            if col < clip_w {
                let n = (clip_w - col).min(take);
                let off = row * dst_stride + col;
                if let Some(d) = dst.get_mut(off..off + n) {
                    d.copy_from_slice(&src[..n]);
                }
            }
            src = &src[take..];
            col += take;
            if col == tile_row_len {
                col = 0;
                row += 1;
            }
        }
        Ok(())
    })?;
    Ok(hdr)
}

/// Decode a lossless JPEG stream whose rows map 1:1 onto `width`-sample image rows.
pub fn decode_into(
    data: &[u8],
    opts: LjpegOptions,
    dst: &mut [u16],
    dst_stride: usize,
    width: usize,
    height: usize,
) -> Result<LjpegHeader> {
    decode_tile(data, opts, dst, dst_stride, width, width, height)
}

/// [`decode_into`] that may decode large restart-free streams on all threads
/// (see [`decode_rows_par`]).
pub fn decode_into_par(
    data: &[u8],
    opts: LjpegOptions,
    dst: &mut [u16],
    dst_stride: usize,
    width: usize,
    height: usize,
    parallel: bool,
) -> Result<LjpegHeader> {
    decode_tile_impl(data, opts, dst, dst_stride, width, width, height, parallel)
}

/// Minimal lossless JPEG encoder, used to build synthetic fixtures for tests,
/// fuzzing seeds and malformed-input suites (not optimised).
pub mod encoder {
    /// Encode `samples` (`height` rows of `width * comps` interleaved values) as a
    /// lossless JPEG with predictor `psv` (1..=7) and a fixed Huffman table.
    pub fn encode(width: usize, height: usize, comps: usize, bits: u32, psv: u32, samples: &[u16]) -> Vec<u8> {
        encode_with_restarts(width, height, comps, bits, psv, samples, 0)
    }

    /// [`encode`] with a restart interval of `restart` MCUs (0: none). Intervals must
    /// start at row starts; each one resets the first-column predictors, and other
    /// predictors keep reading the row above (as the decoder does).
    pub fn encode_with_restarts(
        width: usize,
        height: usize,
        comps: usize,
        bits: u32,
        psv: u32,
        samples: &[u16],
        restart: usize,
    ) -> Vec<u8> {
        // Huffman table: lengths 0..=16 with simple code lengths.
        let counts: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0];
        let values: Vec<u8> = (0..=16).collect();
        let mut codes = [(0u32, 0u32); 17];
        let mut code = 0u32;
        let mut k = 0;
        for len in 1..=16u32 {
            for _ in 0..counts[len as usize - 1] {
                codes[values[k] as usize] = (code, len);
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        let mut out = vec![0xFF, 0xD8];
        // SOF3
        out.extend([0xFF, 0xC3]);
        out.extend(((8 + 3 * comps) as u16).to_be_bytes());
        out.push(bits as u8);
        out.extend((height as u16).to_be_bytes());
        out.extend((width as u16).to_be_bytes());
        out.push(comps as u8);
        for c in 0..comps {
            out.extend([c as u8 + 1, 0x11, 0]);
        }
        // DHT
        out.extend([0xFF, 0xC4]);
        out.extend(((2 + 17 + values.len()) as u16).to_be_bytes());
        out.push(0x00);
        out.extend(counts);
        out.extend(&values);
        if restart > 0 {
            out.extend([0xFF, 0xDD, 0, 4]);
            out.extend((restart as u16).to_be_bytes());
        }
        // SOS
        out.extend([0xFF, 0xDA]);
        out.extend(((6 + 2 * comps) as u16).to_be_bytes());
        out.push(comps as u8);
        for c in 0..comps {
            out.extend([c as u8 + 1, 0x00]);
        }
        out.extend([psv as u8, 0, 0]);
        // Entropy data
        // Bit accumulator and its fill.
        let mut st = (0u64, 0u32);
        let mut ent = vec![];
        let put = |v: u32, len: u32, ent: &mut Vec<u8>, (acc, n): &mut (u64, u32)| {
            for i in (0..len).rev() {
                *acc = (*acc << 1) | ((v >> i) & 1) as u64;
                *n += 1;
                if *n == 8 {
                    ent.push(*acc as u8);
                    if *acc as u8 == 0xFF {
                        ent.push(0);
                    }
                    *acc = 0;
                    *n = 0;
                }
            }
        };
        let rl = width * comps;
        let s = |r: usize, i: usize| samples[r * rl + i] as i32;
        let mut markers = 0u8;
        for r in 0..height {
            let restarted = restart > 0 && (r * width) % restart == 0;
            if restarted && r > 0 {
                // Pad the partial byte with 1-bits and emit RSTn.
                if st.1 > 0 {
                    put(0x7F, 8 - st.1, &mut ent, &mut st);
                }
                ent.extend([0xFF, 0xD0 + markers % 8]);
                markers += 1;
            }
            for col in 0..width {
                for c in 0..comps {
                    let i = col * comps + c;
                    let pred = if (r == 0 || restarted) && col == 0 {
                        1 << (bits - 1)
                    } else if r == 0 {
                        s(r, i - comps)
                    } else if col == 0 {
                        s(r - 1, i)
                    } else {
                        let (ra, rb, rc) = (s(r, i - comps), s(r - 1, i), s(r - 1, i - comps));
                        match psv {
                            1 => ra,
                            2 => rb,
                            3 => rc,
                            4 => ra + rb - rc,
                            5 => ra + ((rb - rc) >> 1),
                            6 => rb + ((ra - rc) >> 1),
                            _ => (ra + rb) >> 1,
                        }
                    };
                    let mut diff = (s(r, i) - pred) as i16 as i32;
                    if diff == -32768 {
                        let (c16, l16) = codes[16];
                        put(c16, l16, &mut ent, &mut st);
                        continue;
                    }
                    let len = if diff == 0 { 0 } else { 32 - diff.unsigned_abs().leading_zeros() };
                    let (cd, l) = codes[len as usize];
                    put(cd, l, &mut ent, &mut st);
                    if len > 0 {
                        if diff < 0 {
                            diff -= 1;
                        }
                        put(diff as u32 & ((1 << len) - 1), len, &mut ent, &mut st);
                    }
                }
            }
        }
        // Pad the final partial byte with 1-bits (JPEG convention).
        put(0x7F, 7, &mut ent, &mut st);
        out.extend(ent);
        out.extend([0xFF, 0xD9]);
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::encoder::encode;
    use super::*;

    fn pseudo(n: usize, bits: u32, seed: u32) -> Vec<u16> {
        let mut x = seed;
        let mut v = 1000i32;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                v = (v + ((x >> 16) as i32 % 200) - 100).clamp(0, (1 << bits) - 1);
                if x % 97 == 0 { ((x >> 8) % (1 << bits)) as u16 } else { v as u16 }
            })
            .collect()
    }

    #[test]
    fn roundtrip_all_predictors() {
        for psv in 1..=7 {
            for comps in 1..=4 {
                let (w, h) = (37, 11);
                let s = pseudo(w * h * comps, 14, psv * 31 + comps as u32);
                let jpg = encode(w, h, comps, 14, psv, &s);
                let hdr = LjpegHeader::parse(&jpg, LjpegOptions::default()).unwrap();
                let mut out = vec![];
                decode_rows(&jpg, &hdr, |_, r| {
                    out.extend_from_slice(r);
                    Ok(())
                })
                .unwrap();
                assert_eq!(out, s, "psv {psv} comps {comps}");
            }
        }
    }

    #[test]
    fn unstuffing_stops_at_restart_without_restarts() {
        // One restart interval read on its own must not pull in the following ones.
        let scan = [1u8, 0xFF, 0x00, 2, 0xFF, 0xD0, 3, 4, 0xFF, 0xD1, 5];
        let un = unstuff_scan(&scan, false);
        assert_eq!(un.segments.len(), 1);
        assert_eq!(un.segment(0), &[1, 0xFF, 2]);
        assert_eq!(unstuff_scan(&scan, true).segments.len(), 3);
    }

    #[test]
    fn unstuffing_and_restart_segments() {
        let scan = [0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56, 0xFF, 0x00, 0xFF, 0xD1, 0x78, 0xFF, 0xD9, 0x99];
        let u = unstuff(&scan);
        assert_eq!(u.segment(0), &[0x12, 0xFF, 0x34]);
        assert_eq!(u.segment(1), &[0x56, 0xFF]);
        assert_eq!(u.segment(2), &[0x78]);
        assert_eq!(u.segments.len(), 3);
        // Long runs without 0xFF go through the word-at-a-time scanner.
        let long: Vec<u8> = (0..1000u32).map(|i| (i % 255) as u8).collect();
        assert_eq!(unstuff(&long).segment(0), &long[..]);
        assert_eq!(find_ff(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 0xFF]), Some(9));
        assert_eq!(find_ff(&[0xFF]), Some(0));
        assert_eq!(find_ff(&[1, 2]), None);
    }

    /// Byte-at-a-time reference for [`unstuff_scan`].
    fn unstuff_reference(scan: &[u8], restarts: bool) -> (Vec<u8>, Vec<(usize, usize)>) {
        let (mut data, mut segs, mut start, mut i) = (Vec::new(), Vec::new(), 0, 0);
        while i < scan.len() {
            if scan[i] != 0xFF {
                data.push(scan[i]);
                i += 1;
                continue;
            }
            match scan.get(i + 1) {
                Some(0) => data.push(0xFF),
                Some(&m) if restarts && (0xD0..=0xD7).contains(&m) => {
                    segs.push((start, data.len()));
                    start = data.len();
                }
                _ => break,
            }
            i += 2;
        }
        segs.push((start, data.len()));
        (data, segs)
    }

    #[test]
    fn unstuffing_matches_bytewise_reference() {
        let mut x = 7u32;
        for case in 0..600 {
            let len = case * 3 % 97 + (case % 5) * 40 + if case >= 400 { 5000 } else { 0 };
            let dense = case % 3; // how often 0xFF appears
            let scan: Vec<u8> = (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    let r = (x >> 24) as u8;
                    match (dense, r % 16) {
                        (0, _) => r,
                        (_, 0..=3) => 0xFF,
                        (2, 4..=7) => 0x00,
                        (_, 8) => 0xD0 + r % 8,
                        (_, 9) if case % 7 == 0 => 0xD9,
                        _ => r,
                    }
                })
                .collect();
            for restarts in [false, true] {
                let u = unstuff_scan(&scan, restarts);
                let (data, segs) = unstuff_reference(&scan, restarts);
                assert_eq!(u.data, data, "case {case} restarts {restarts}");
                assert_eq!(u.segments, segs, "case {case} restarts {restarts}");
                let p = unstuff_parallel(&scan, restarts);
                assert_eq!(p.data, data, "parallel: case {case} restarts {restarts}");
                assert_eq!(p.segments, segs, "parallel: case {case} restarts {restarts}");
                assert_eq!(unstuffed_len(&scan, restarts).0, data.len());
            }
        }
    }

    #[test]
    fn full_16_bit_range() {
        let (w, h) = (16, 4);
        let mut s = pseudo(w * h, 16, 7);
        s[5] = 0;
        s[6] = 65535;
        s[7] = 0;
        let jpg = encode(w, h, 1, 16, 1, &s);
        let mut out = vec![0u16; w * h];
        decode_into(&jpg, LjpegOptions::default(), &mut out, w, w, h).unwrap();
        assert_eq!(out, s);
    }

    #[test]
    fn tile_mapping_two_component_rows_and_clipping() {
        // A 8x4 tile encoded as a 4-wide, 2-component JPEG; clip to 6x3 into a stride-10 buffer.
        let (w, h) = (8usize, 4usize);
        let s = pseudo(w * h, 12, 11);
        let jpg = encode(w / 2, h, 2, 12, 1, &s);
        let mut out = vec![0u16; 10 * 3];
        decode_tile(&jpg, LjpegOptions::default(), &mut out, 10, w, 6, 3).unwrap();
        for y in 0..3 {
            assert_eq!(&out[y * 10..y * 10 + 6], &s[y * w..y * w + 6]);
            assert_eq!(&out[y * 10 + 6..y * 10 + 10], &[0, 0, 0, 0]);
        }
        // Two tile rows per JPEG row.
        let jpg = encode(w * 2, h / 2, 1, 12, 1, &s);
        let mut out = vec![0u16; w * h];
        decode_tile(&jpg, LjpegOptions::default(), &mut out, w, w, w, h).unwrap();
        assert_eq!(out, s);
    }

    #[test]
    fn truncated_stream_errors() {
        let (w, h) = (64, 64);
        let s = pseudo(w * h, 12, 3);
        let jpg = encode(w, h, 1, 12, 1, &s);
        for cut in [jpg.len() / 4, jpg.len() / 2] {
            let r = decode_into(&jpg[..cut], LjpegOptions::default(), &mut vec![0; w * h], w, w, h);
            assert!(r.is_err());
        }
    }
}
