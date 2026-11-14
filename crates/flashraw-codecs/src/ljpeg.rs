//! Lossless JPEG (ITU-T T.81 process 14, Huffman coded, SOF3) decoder.
//!
//! A stream is parsed into an [`LjpegHeader`] (frame geometry, one Huffman table per value
//! position of a minimum coded unit, predictor, restart interval) and then decoded row by
//! row. Rows of non-subsampled frames that use predictor 1 take a fast path with the
//! per-component predictors held in registers; the other predictors and the Canon sRAW /
//! Sony Y/Cb/Cr layouts (extra luma samples per MCU) go through [`reconstruct_row`].
//!
//! Restart intervals are honoured at row starts only. After each row the amount of data
//! consumed beyond the end of the interval is checked, so truncated files yield the rows
//! that exist and then an error.

use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, Result, par};

pub mod encoder;
mod unstuff;

pub use crate::ljpeg_par::decode_frame_parallel;

/// Bytes a decoder may consume past the end of the data of the last row of a frame.
const END_SLACK_LAST_ROW: usize = 2;
/// The same for every other row.
const END_SLACK_OTHER_ROW: usize = 64;

/// Frame component: identifier and sampling factors.
#[derive(Debug, Clone, Copy)]
pub struct Component {
    pub id: u8,
    pub h: u8,
    pub v: u8,
}

/// A parsed lossless JPEG stream (headers only).
#[derive(Debug, Clone)]
pub struct LjpegHeader {
    /// Sample precision in bits after the point transform (P - Pt).
    pub bits: u32,
    /// Frame width in MCUs.
    pub width: u32,
    pub height: u32,
    pub components: Vec<Component>,
    /// Values per MCU: components + extra luma samples.
    pub clrs: u32,
    /// Extra luma samples per MCU (Canon sRAW), 0 for ordinary frames.
    pub sraw: u32,
    pub predictor: u32,
    pub point_transform: u32,
    pub restart_interval: u32,
    /// Huffman table per value position of an MCU (`clrs` entries).
    pub(crate) tables: Vec<HuffTable>,
    /// Table identity per entry of `tables`: equal ids mean byte-identical definitions.
    pub(crate) table_ids: Vec<u8>,
    /// Offset of the entropy-coded data in the input.
    pub scan_offset: usize,
}

/// Decoder options.
#[derive(Debug, Clone, Copy)]
pub struct LjpegOptions {
    /// Huffman symbol 16 means a difference of -32768 with no extra bits.
    pub dng_bug16: bool,
}

impl Default for LjpegOptions {
    fn default() -> Self {
        LjpegOptions { dng_bug16: true }
    }
}

/// Next marker code at or after `pos` (non-0xFF bytes are skipped, runs of 0xFF are fill),
/// and the offset just behind it.
fn next_marker(data: &[u8], mut pos: usize) -> Option<(u8, usize)> {
    while *data.get(pos)? != 0xFF {
        pos += 1;
    }
    while *data.get(pos)? == 0xFF {
        pos += 1;
    }
    Some((data[pos], pos + 1))
}

/// The raw definition (counts then values) of a Huffman table slot.
type RawTable = Vec<u8>;

impl LjpegHeader {
    /// Parse the markers up to and including the first SOS.
    pub fn parse(data: &[u8], opts: LjpegOptions) -> Result<Self> {
        if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
            return Err(Error::corrupt("not a JPEG stream"));
        }
        let mut slots: [Option<(HuffTable, RawTable)>; 4] = [None, None, None, None];
        let mut frame: Option<Frame> = None;
        let mut restart_interval = 0u32;
        let mut pos = 2usize;
        loop {
            let Some((marker, after)) = next_marker(data, pos) else {
                return Err(Error::corrupt("truncated JPEG header"));
            };
            pos = after;
            match marker {
                0xD8 | 0x01 | 0xD0..=0xD7 => continue,
                0xD9 => return Err(Error::corrupt("end of image before the scan")),
                _ => {}
            }
            let Some(len) = data.get(pos..pos + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize) else {
                return Err(Error::corrupt("truncated JPEG segment"));
            };
            if len < 2 || pos + len > data.len() {
                return Err(Error::corrupt("JPEG segment runs past the end of the data"));
            }
            let body = &data[pos + 2..pos + len];
            pos += len;
            match marker {
                0xC3 | 0xC0 | 0xC1 => frame = Some(Frame::parse(marker, body)?),
                0xC4 => read_tables(body, opts, &mut slots)?,
                0xDD => {
                    if body.len() < 2 {
                        return Err(Error::corrupt("short restart interval segment"));
                    }
                    restart_interval = u16::from_be_bytes([body[0], body[1]]) as u32;
                }
                0xDA => return finish(data, body, pos, frame, slots, restart_interval),
                _ => {}
            }
        }
    }

    /// Values per decoded row (`width * clrs`).
    pub fn row_len(&self) -> usize {
        self.width as usize * self.clrs as usize
    }

    /// Huffman table used for value position `i` of an MCU.
    pub fn table(&self, i: usize) -> Option<&HuffTable> {
        self.tables.get(i)
    }
}

/// Frame header fields.
struct Frame {
    marker: u8,
    precision: u32,
    width: u32,
    height: u32,
    components: Vec<Component>,
    sraw: u32,
}

impl Frame {
    fn parse(marker: u8, body: &[u8]) -> Result<Self> {
        if body.len() < 6 {
            return Err(Error::corrupt("short frame header"));
        }
        let nf = body[5] as usize;
        if nf == 0 || nf > 4 || body.len() < 6 + 3 * nf {
            return Err(Error::corrupt("bad component count in frame header"));
        }
        let components: Vec<Component> =
            body[6..6 + 3 * nf].chunks_exact(3).map(|c| Component { id: c[0], h: c[1] >> 4, v: c[1] & 15 }).collect();
        // Extra luma samples (Canon sRAW, Sony Y/Cb/Cr): one MCU holds h*v luma samples.
        let luma = components[0].h as u32 * components[0].v as u32;
        Ok(Frame {
            marker,
            precision: body[0] as u32,
            height: u16::from_be_bytes([body[1], body[2]]) as u32,
            width: u16::from_be_bytes([body[3], body[4]]) as u32,
            components,
            sraw: luma.wrapping_sub(1) & 3,
        })
    }
}

fn read_tables(body: &[u8], opts: LjpegOptions, slots: &mut [Option<(HuffTable, RawTable)>; 4]) -> Result<()> {
    let mut rest = body;
    while !rest.is_empty() {
        if rest.len() < 17 {
            return Err(Error::corrupt("short Huffman table segment"));
        }
        let slot = (rest[0] & 15) as usize;
        if slot > 3 {
            return Err(Error::corrupt("Huffman table slot out of range"));
        }
        let counts = &rest[1..17];
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if rest.len() < 17 + total {
            return Err(Error::corrupt("Huffman table runs past its segment"));
        }
        let values = &rest[17..17 + total];
        let mut table = HuffTable::from_counts(counts, values, true)?;
        table.dng_bug16 = opts.dng_bug16;
        slots[slot] = Some((table, rest[1..17 + total].to_vec()));
        rest = &rest[17 + total..];
    }
    Ok(())
}

fn finish(
    data: &[u8],
    body: &[u8],
    scan_offset: usize,
    frame: Option<Frame>,
    slots: [Option<(HuffTable, RawTable)>; 4],
    restart_interval: u32,
) -> Result<LjpegHeader> {
    let _ = data;
    let Some(frame) = frame else { return Err(Error::corrupt("scan before frame header")) };
    let ns = body.first().copied().unwrap_or(0) as usize;
    if ns != frame.components.len() || body.len() < 1 + 2 * ns + 3 {
        return Err(Error::corrupt("scan does not match the frame components"));
    }
    let predictor = body[1 + 2 * ns] as u32;
    let point_transform = (body[3 + 2 * ns] & 0x0F) as u32;
    if matches!(frame.marker, 0xC0 | 0xC1) && !(1..=7).contains(&predictor) {
        return Err(Error::unsupported_compression("ljpeg", "DCT-coded JPEG stream, not lossless"));
    }
    let first_defined = slots.iter().position(|s| s.is_some());
    let clrs = frame.components.len() as u32 + frame.sraw;
    let mut tables = Vec::with_capacity(clrs as usize);
    let mut table_ids = Vec::with_capacity(clrs as usize);
    for p in 0..clrs as usize {
        let k = p.saturating_sub(frame.sraw as usize);
        let wanted = ((body[2 + 2 * k] >> 4) as usize).min(3);
        let slot = if slots[wanted].is_some() { Some(wanted) } else { first_defined };
        let Some(slot) = slot else { return Err(Error::corrupt("no Huffman table defined")) };
        let (table, raw) = slots[slot].as_ref().expect("slot is defined");
        // Identity: the lowest slot whose definition is byte-identical.
        let id = slots.iter().position(|s| s.as_ref().is_some_and(|(_, r)| r == raw)).unwrap_or(slot);
        tables.push(table.clone());
        table_ids.push(id as u8);
    }
    let (p, pt) = (frame.precision, point_transform);
    if p == 0 || p > 16 || pt >= p || frame.width == 0 || frame.height == 0 {
        return Err(Error::corrupt("invalid lossless JPEG frame parameters"));
    }
    Ok(LjpegHeader {
        bits: p - pt,
        width: frame.width,
        height: frame.height,
        components: frame.components,
        clrs,
        sraw: frame.sraw,
        predictor,
        point_transform: pt,
        restart_interval,
        tables,
        table_ids,
        scan_offset,
    })
}

/// For each value position of an MCU, the first position whose table has the same definition:
/// positions that share a table then read one copy, which keeps the lookup tables in cache.
fn table_slots(hdr: &LjpegHeader) -> [usize; 6] {
    std::array::from_fn(|c| match hdr.table_ids.get(c) {
        Some(id) => hdr.table_ids.iter().position(|x| x == id).unwrap_or(c),
        None => c,
    })
}

/// Initial prediction of the first sample of a scan or restart interval: half the range.
#[inline]
fn initial_prediction(bits: u32) -> u16 {
    1u32.checked_shl(bits.wrapping_sub(1)).unwrap_or(0) as u16
}

/// Entropy-coded data with byte stuffing removed, split at restart markers.
pub struct Unstuffed {
    pub(crate) data: Vec<u8>,
    /// `(start, end)` of each restart interval within `data`.
    pub(crate) segments: Vec<(usize, usize)>,
}

impl Unstuffed {
    /// Bytes of interval `i` (empty if there is no such interval).
    pub(crate) fn segment(&self, i: usize) -> &[u8] {
        match self.segments.get(i) {
            Some(&(a, b)) => &self.data[a..b],
            None => &[],
        }
    }
}

/// Remove byte stuffing; restart markers split the output, any other marker ends it.
pub fn unstuff(scan: &[u8]) -> Unstuffed {
    unstuff_scan(scan, true)
}

/// [`unstuff`] with a switch: when `restarts` is false a restart marker ends the scan like any other marker.
pub fn unstuff_scan(scan: &[u8], restarts: bool) -> Unstuffed {
    let (data, marks) = unstuff::unstuff(scan, restarts);
    let mut segments = Vec::with_capacity(marks.len() + 1);
    let mut start = 0;
    for &m in &marks {
        segments.push((start, m));
        start = m;
    }
    segments.push((start, data.len()));
    Unstuffed { data, segments }
}

/// True when rows can be decoded by the predictor-1 / single-row path.
pub(crate) fn fast_rows(hdr: &LjpegHeader) -> bool {
    hdr.sraw == 0 && (hdr.predictor == 1 || hdr.height == 1)
}

/// Decode every row of the scan, calling `sink(row_index, samples)` in order.
pub fn decode_rows<F>(data: &[u8], hdr: &LjpegHeader, mut sink: F) -> Result<()>
where
    F: FnMut(usize, &[u16]) -> Result<()>,
{
    let clrs = hdr.clrs as usize;
    if clrs > 6 {
        return Err(Error::corrupt("lossless JPEG: too many values per MCU"));
    }
    if hdr.tables.len() < clrs {
        return Err(Error::corrupt("lossless JPEG: missing Huffman tables"));
    }
    let Some(scan) = data.get(hdr.scan_offset..) else {
        return Err(Error::corrupt("lossless JPEG: scan offset past the end of the data"));
    };
    let unstuffed = unstuff_scan(scan, hdr.restart_interval != 0);
    let fast = fast_rows(hdr);
    let rl = hdr.row_len();
    let width = hdr.width as u64;
    let restart = hdr.restart_interval as u64;
    let init = initial_prediction(hdr.bits);
    let mut vpred = [init; 6];
    let mut cur = vec![0u16; rl];
    let mut prev = if fast { Vec::new() } else { vec![0u16; rl] };
    let mut interval = 0usize;
    let mut bits = BitPumpMsb::new(unstuffed.segment(0));
    for y in 0..hdr.height as usize {
        if y > 0 && restart != 0 && (y as u64 * width) % restart == 0 {
            interval += 1;
            if interval >= unstuffed.segments.len() {
                return Err(Error::corrupt(format!("lossless JPEG: missing restart marker before row {y}")));
            }
            bits = BitPumpMsb::new(unstuffed.segment(interval));
            vpred = [init; 6];
        }
        decode_row(hdr, fast, &mut bits, &mut cur, &prev, y, &mut vpred)?;
        sink(y, &cur)?;
        if !fast {
            std::mem::swap(&mut cur, &mut prev);
        }
    }
    Ok(())
}

/// Decode row `jrow` into `cur` (see the spec for `prev`, `vpred` and the end-of-data check).
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
    if clrs == 0 || clrs > 6 || hdr.tables.len() < clrs {
        return Err(Error::corrupt("lossless JPEG: bad table or component count"));
    }
    let rl = hdr.row_len();
    if cur.len() < rl {
        return Err(Error::corrupt("lossless JPEG: row buffer too small"));
    }
    let cur = &mut cur[..rl];
    let slots = table_slots(hdr);
    if fast && clrs <= 4 {
        let failed = match clrs {
            1 => fast_row::<1>(&hdr.tables, slots, bits, cur, vpred),
            2 => fast_row::<2>(&hdr.tables, slots, bits, cur, vpred),
            3 => fast_row::<3>(&hdr.tables, slots, bits, cur, vpred),
            _ => fast_row::<4>(&hdr.tables, slots, bits, cur, vpred),
        };
        if failed {
            return Err(Error::corrupt(format!("invalid Huffman code in row {jrow}")));
        }
    } else if let Some(failed) = two_dimensional_row(hdr, slots, bits, cur, prev, jrow, vpred) {
        if failed {
            return Err(Error::corrupt(format!("invalid Huffman code in row {jrow}")));
        }
    } else {
        let tables = &hdr.tables;
        let pump = &mut *bits;
        reconstruct_row(hdr, |c| tables[slots[c]].decode_diff(pump), cur, prev, jrow, clrs, vpred)?;
    }
    let slack = if jrow + 1 >= hdr.height as usize { END_SLACK_LAST_ROW } else { END_SLACK_OTHER_ROW };
    if bits.past_end() > slack {
        return Err(Error::corrupt(format!("lossless JPEG data ends early in row {jrow}")));
    }
    Ok(())
}

/// Predictor 1 on a frame without extra luma samples: `N` interleaved components, each
/// predicted from its left neighbour. Returns true when an invalid code was met.
#[inline(always)]
fn fast_row<const N: usize>(
    tables: &[HuffTable],
    slots: [usize; 6],
    bits: &mut BitPumpMsb<'_>,
    cur: &mut [u16],
    vpred: &mut [u16; 6],
) -> bool {
    let mut pump = *bits;
    let mut bad = false;
    let t: [&HuffTable; N] = std::array::from_fn(|c| &tables[slots[c]]);
    let mut run = [0u16; N];
    let mut mcus = cur.chunks_exact_mut(N);
    if let Some(first) = mcus.next() {
        for c in 0..N {
            prepare(&mut pump, c);
            let d = t[c].decode_diff_buffered(&mut pump, &mut bad);
            run[c] = vpred[c].wrapping_add(d as u16);
            first[c] = run[c];
            vpred[c] = run[c];
        }
    }
    for mcu in mcus {
        for c in 0..N {
            prepare(&mut pump, c);
            let d = t[c].decode_diff_buffered(&mut pump, &mut bad);
            run[c] = run[c].wrapping_add(d as u16);
            mcu[c] = run[c];
        }
    }
    *bits = pump;
    bad
}

/// Rows of predictors 2..=7 below the first row of a frame without extra luma samples, for
/// up to four components. `None` when the layout is not handled here; otherwise whether an
/// invalid code was met.
fn two_dimensional_row(
    hdr: &LjpegHeader,
    slots: [usize; 6],
    bits: &mut BitPumpMsb<'_>,
    cur: &mut [u16],
    prev: &[u16],
    jrow: usize,
    vpred: &mut [u16; 6],
) -> Option<bool> {
    let clrs = hdr.clrs as usize;
    if hdr.sraw != 0 || jrow == 0 || clrs > 4 || prev.len() < cur.len() {
        return None;
    }
    macro_rules! by_components {
        ($psv:literal) => {
            match clrs {
                1 => dependent_row::<$psv, 1>(&hdr.tables, slots, bits, cur, prev, vpred),
                2 => dependent_row::<$psv, 2>(&hdr.tables, slots, bits, cur, prev, vpred),
                3 => dependent_row::<$psv, 3>(&hdr.tables, slots, bits, cur, prev, vpred),
                _ => dependent_row::<$psv, 4>(&hdr.tables, slots, bits, cur, prev, vpred),
            }
        };
    }
    Some(match hdr.predictor {
        2 => by_components!(2),
        3 => by_components!(3),
        4 => by_components!(4),
        5 => by_components!(5),
        6 => by_components!(6),
        7 => by_components!(7),
        _ => return None,
    })
}

/// One row (not the first of its frame) of `N` interleaved components with predictor `PSV`.
/// The first MCU continues from the first-column state; later ones use the left, upper and
/// upper-left neighbours, which are kept in registers. Returns true on an invalid code.
#[inline(always)]
fn dependent_row<const PSV: u32, const N: usize>(
    tables: &[HuffTable],
    slots: [usize; 6],
    bits: &mut BitPumpMsb<'_>,
    cur: &mut [u16],
    prev: &[u16],
    vpred: &mut [u16; 6],
) -> bool {
    let mut pump = *bits;
    let mut bad = false;
    let t: [&HuffTable; N] = std::array::from_fn(|c| &tables[slots[c]]);
    let mut left = [0u16; N];
    let mut upper_left = [0u16; N];
    let mut mcus = cur.chunks_exact_mut(N).zip(prev.chunks_exact(N));
    if let Some((out, above)) = mcus.next() {
        for c in 0..N {
            prepare(&mut pump, c);
            let d = t[c].decode_diff_buffered(&mut pump, &mut bad);
            left[c] = vpred[c].wrapping_add(d as u16);
            out[c] = left[c];
            vpred[c] = left[c];
            upper_left[c] = above[c];
        }
    }
    for (out, above) in mcus {
        for c in 0..N {
            prepare(&mut pump, c);
            let d = t[c].decode_diff_buffered(&mut pump, &mut bad);
            let v = predict::<PSV>(left[c], above[c], upper_left[c]).wrapping_add(d) as u16;
            out[c] = v;
            left[c] = v;
            upper_left[c] = above[c];
        }
    }
    *bits = pump;
    bad
}

/// Bit-buffer upkeep before the `k`-th value of an MCU: every second value tops the buffer
/// up unconditionally (a branch-free refill to at least 56 bits, enough for two values of at
/// most 27 bits); the others only check that a rare long code did not use up the reserve,
/// which never fires on ordinary data.
#[inline(always)]
fn prepare(pump: &mut BitPumpMsb<'_>, k: usize) {
    if k % 2 == 0 {
        pump.refill_force();
    } else {
        pump.fill_to(27);
    }
}

/// Prediction of a sample from its left (`ra`), upper (`rb`) and upper-left (`rc`)
/// neighbours by predictor `PSV` (T.81 Table H.1); 0 for selectors outside 1..=7.
#[inline(always)]
fn predict<const PSV: u32>(ra: u16, rb: u16, rc: u16) -> i32 {
    let (a, b, c) = (ra as i32, rb as i32, rc as i32);
    match PSV {
        1 => a,
        2 => b,
        3 => c,
        4 => a + b - c,
        5 => a + ((b - c) >> 1),
        6 => b + ((a - c) >> 1),
        7 => (a + b) >> 1,
        _ => 0,
    }
}

/// Rebuild one row from its differences.
#[inline(always)]
pub(crate) fn reconstruct_row(
    hdr: &LjpegHeader,
    next: impl FnMut(usize) -> Result<i32>,
    cur: &mut [u16],
    prev: &[u16],
    jrow: usize,
    clrs: usize,
    vpred: &mut [u16; 6],
) -> Result<()> {
    if clrs == 0 || clrs > 6 {
        return Err(Error::corrupt("lossless JPEG: too many values per MCU"));
    }
    let rl = hdr.width as usize * clrs;
    if cur.len() < rl {
        return Err(Error::corrupt("lossless JPEG: row buffer too small"));
    }
    let cur = &mut cur[..rl];
    let psv = if (1..=7).contains(&hdr.predictor) { hdr.predictor } else { 0 };
    if jrow > 0 && psv != 1 && prev.len() < rl {
        return Err(Error::corrupt("lossless JPEG: previous row missing"));
    }
    if hdr.sraw > 0 {
        if clrs <= hdr.sraw as usize {
            return Err(Error::corrupt("lossless JPEG: inconsistent values per MCU"));
        }
        return match psv {
            1 => sraw_row::<1>(hdr.sraw as usize, next, cur, prev, jrow, clrs, vpred),
            2 => sraw_row::<2>(hdr.sraw as usize, next, cur, prev, jrow, clrs, vpred),
            3 => sraw_row::<3>(hdr.sraw as usize, next, cur, prev, jrow, clrs, vpred),
            4 => sraw_row::<4>(hdr.sraw as usize, next, cur, prev, jrow, clrs, vpred),
            5 => sraw_row::<5>(hdr.sraw as usize, next, cur, prev, jrow, clrs, vpred),
            6 => sraw_row::<6>(hdr.sraw as usize, next, cur, prev, jrow, clrs, vpred),
            7 => sraw_row::<7>(hdr.sraw as usize, next, cur, prev, jrow, clrs, vpred),
            _ => sraw_row::<0>(hdr.sraw as usize, next, cur, prev, jrow, clrs, vpred),
        };
    }
    match psv {
        1 => plain_row::<1>(next, cur, prev, jrow, clrs, vpred),
        2 => plain_row::<2>(next, cur, prev, jrow, clrs, vpred),
        3 => plain_row::<3>(next, cur, prev, jrow, clrs, vpred),
        4 => plain_row::<4>(next, cur, prev, jrow, clrs, vpred),
        5 => plain_row::<5>(next, cur, prev, jrow, clrs, vpred),
        6 => plain_row::<6>(next, cur, prev, jrow, clrs, vpred),
        7 => plain_row::<7>(next, cur, prev, jrow, clrs, vpred),
        _ => plain_row::<0>(next, cur, prev, jrow, clrs, vpred),
    }
}

/// Row of a frame with one value per component.
#[inline(always)]
fn plain_row<const PSV: u32>(
    mut next: impl FnMut(usize) -> Result<i32>,
    cur: &mut [u16],
    prev: &[u16],
    jrow: usize,
    clrs: usize,
    vpred: &mut [u16; 6],
) -> Result<()> {
    if cur.len() < clrs {
        return Ok(());
    }
    let (first, rest) = cur.split_at_mut(clrs);
    for (c, s) in first.iter_mut().enumerate() {
        let v = vpred[c].wrapping_add(next(c)? as u16);
        *s = v;
        vpred[c] = v;
    }
    if jrow == 0 || PSV == 1 {
        // Only the left neighbour counts (first row, or predictor 1 everywhere).
        for i in 0..rest.len() {
            let ra = if i < clrs { first[i] } else { rest[i - clrs] };
            rest[i] = ra.wrapping_add(next(i % clrs)? as u16);
        }
        return Ok(());
    }
    for i in 0..rest.len() {
        let c = i % clrs;
        let ra = if i < clrs { first[i] } else { rest[i - clrs] };
        let rb = prev[clrs + i];
        let rc = prev[i];
        let px = predict::<PSV>(ra, rb, rc);
        rest[i] = px.wrapping_add(next(c)?) as u16;
    }
    Ok(())
}

/// Row of a frame with `sraw` extra luma samples per MCU: luma values chain through the
/// previous luma value in stream order, chroma values follow their own column.
fn sraw_row<const PSV: u32>(
    sraw: usize,
    mut next: impl FnMut(usize) -> Result<i32>,
    cur: &mut [u16],
    prev: &[u16],
    jrow: usize,
    clrs: usize,
    vpred: &mut [u16; 6],
) -> Result<()> {
    for x in 0..cur.len() / clrs {
        let base = x * clrs;
        for c in 0..clrs {
            let i = base + c;
            let d = next(c)?;
            let luma = c <= sraw;
            if x == 0 && (c == 0 || !luma) {
                let v = vpred[c].wrapping_add(d as u16);
                cur[i] = v;
                vpred[c] = v;
                continue;
            }
            let ra = if !luma {
                cur[i - clrs]
            } else if c > 0 {
                cur[i - 1]
            } else {
                cur[i - clrs + sraw]
            };
            let px =
                if jrow == 0 || PSV == 1 || x == 0 { ra as i32 } else { predict::<PSV>(ra, prev[i], prev[i - clrs]) };
            cur[i] = px.wrapping_add(d) as u16;
        }
    }
    Ok(())
}

/// [`decode_rows`] that may decode large scans on several threads when `parallel` is set.
pub fn decode_rows_par<F>(data: &[u8], hdr: &LjpegHeader, parallel: bool, sink: F) -> Result<()>
where
    F: FnMut(usize, &[u16]) -> Result<()>,
{
    decode_rows_par_limit(data, hdr, parallel, hdr.height as usize, sink)
}

/// [`decode_rows_par`] for callers that only need the first `rows` rows.
pub fn decode_rows_par_limit<F>(data: &[u8], hdr: &LjpegHeader, parallel: bool, rows: usize, mut sink: F) -> Result<()>
where
    F: FnMut(usize, &[u16]) -> Result<()>,
{
    let rows = rows.min(hdr.height as usize);
    let rl = hdr.row_len();
    let min_samples =
        if hdr.restart_interval != 0 { par::PAR_MIN_SAMPLES } else { crate::ljpeg_par::LJPEG_PARALLEL_MIN_SAMPLES };
    if parallel && rl > 0 && rows > 0 && par::threads() > 1 && rl.saturating_mul(rows) >= min_samples {
        if let Some(total) = rl.checked_mul(rows) {
            let mut frame = vec![0u16; total];
            if crate::ljpeg_par::decode_frame_rows(data, hdr, rows, &mut frame)? {
                for (y, row) in frame.chunks_exact(rl).enumerate() {
                    sink(y, row)?;
                }
                return Ok(());
            }
        }
    }
    decode_rows(data, hdr, sink)
}

/// Decode a lossless JPEG tile into `dst` (rows of `tile_row_len` samples, clipped to `clip_w` x `clip_h`).
pub fn decode_tile(
    data: &[u8],
    opts: LjpegOptions,
    dst: &mut [u16],
    dst_stride: usize,
    tile_row_len: usize,
    clip_w: usize,
    clip_h: usize,
) -> Result<LjpegHeader> {
    decode_tile_with(data, opts, dst, dst_stride, tile_row_len, clip_w, clip_h, false)
}

#[allow(clippy::too_many_arguments)]
fn decode_tile_with(
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
    let rl = hdr.row_len();
    decode_rows_par(data, &hdr, parallel, |y, row| {
        // The samples of this JPEG row, numbered in the concatenated stream, are cut into tile rows.
        let mut s = y.saturating_mul(rl);
        let mut taken = 0usize;
        while taken < rl {
            let (r, c) = (s / tile_row_len, s % tile_row_len);
            let n = (tile_row_len - c).min(rl - taken);
            if r >= clip_h {
                break;
            }
            if c < clip_w {
                let m = n.min(clip_w - c);
                if let Some(start) = r.checked_mul(dst_stride).and_then(|v| v.checked_add(c)) {
                    let m = m.min(dst.len().saturating_sub(start));
                    if m > 0 {
                        dst[start..start + m].copy_from_slice(&row[taken..taken + m]);
                    }
                }
            }
            s += n;
            taken += n;
        }
        Ok(())
    })?;
    Ok(hdr)
}

/// Decode a stream whose rows map 1:1 onto `width`-sample image rows.
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

/// [`decode_into`] that may use several threads.
pub fn decode_into_par(
    data: &[u8],
    opts: LjpegOptions,
    dst: &mut [u16],
    dst_stride: usize,
    width: usize,
    height: usize,
    parallel: bool,
) -> Result<LjpegHeader> {
    decode_tile_with(data, opts, dst, dst_stride, width, width, height, parallel)
}

#[cfg(test)]
pub(crate) mod tests;
