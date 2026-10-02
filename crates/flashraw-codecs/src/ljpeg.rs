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

use flashraw_core::bits::{BitPumpJpeg, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, ReadAt, Result};

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
    tables: Vec<HuffTable>,
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
                    // Some encoders (e.g. Blackmagic) label lossless streams SOF0/SOF1; like
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
                    for c in 0..clrs as usize {
                        let comp = if c <= sraw as usize { 0 } else { c - sraw as usize };
                        let id = comp_tables[comp];
                        let t = dht[id]
                            .clone()
                            .or_else(|| dht.iter().flatten().next().cloned())
                            .ok_or_else(|| Error::corrupt("lossless JPEG: missing Huffman table"))?;
                        tables.push(t);
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
    let mut bits = BitPumpJpeg::new(scan);
    let clrs = hdr.clrs as usize;
    let init = 1u32 << (hdr.bits - 1);
    let mut vpred = [init as u16; 6];
    let restart = if hdr.restart_interval > 0 { hdr.restart_interval as usize } else { usize::MAX };
    let fast = hdr.sraw == 0 && (hdr.predictor == 1 || hdr.height == 1);
    for jrow in 0..hdr.height as usize {
        if restart != usize::MAX && (jrow * hdr.width as usize) % restart == 0 {
            vpred = [init as u16; 6];
            if jrow > 0 && !bits.reset_at_restart() {
                return Err(Error::corrupt("lossless JPEG: missing restart marker"));
            }
        }
        if fast {
            decode_row_pred1(&hdr.tables, &mut bits, &mut cur, clrs, &mut vpred)?;
        } else {
            decode_row_generic(hdr, &mut bits, &mut cur, &prev, jrow, clrs, &mut vpred)?;
        }
        // Up to two bytes consumed past the end are tolerated at the very end (some
        // encoders drop the final partial byte); anything more means truncation.
        let slack = if jrow + 1 == hdr.height as usize { 2 } else { 64 };
        if bits.past_end() > slack {
            return Err(Error::corrupt(format!("lossless JPEG: data ends at row {jrow}/{}", hdr.height)));
        }
        sink(jrow, &cur)?;
        std::mem::swap(&mut cur, &mut prev);
    }
    Ok(())
}

/// Predictor 1 (left neighbour), the overwhelmingly common case.
#[inline]
fn decode_row_pred1(
    tables: &[HuffTable],
    bits: &mut BitPumpJpeg,
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
        2 => {
            let (t0, t1) = (&tables[0], &tables[1]);
            let (mut p0, mut p1) = (cur[0] as i32, cur[1] as i32);
            for px in cur[2..].chunks_exact_mut(2) {
                p0 += t0.decode_diff(bits)?;
                p1 += t1.decode_diff(bits)?;
                px[0] = p0 as u16;
                px[1] = p1 as u16;
                p0 = px[0] as i32;
                p1 = px[1] as i32;
            }
        }
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

#[inline(never)]
fn decode_row_generic(
    hdr: &LjpegHeader,
    bits: &mut BitPumpJpeg,
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
            let diff = hdr.tables[c].decode_diff(bits)?;
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
    let hdr = LjpegHeader::parse(data, opts)?;
    if tile_row_len == 0 || clip_w == 0 || clip_h == 0 {
        return Ok(hdr);
    }
    let clip_w = clip_w.min(tile_row_len).min(dst_stride);
    let (mut row, mut col) = (0usize, 0usize);
    decode_rows(data, &hdr, |_, mut src| {
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

/// Minimal lossless JPEG encoder, used to build synthetic fixtures for tests,
/// fuzzing seeds and malformed-input suites (not optimised).
pub mod encoder {
    /// Encode `samples` (`height` rows of `width * comps` interleaved values) as a
    /// lossless JPEG with predictor `psv` (1..=7) and a fixed Huffman table.
    pub fn encode(width: usize, height: usize, comps: usize, bits: u32, psv: u32, samples: &[u16]) -> Vec<u8> {
        // Huffman table: lengths 0..=16 with simple code lengths.
        let counts: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0];
        let values: Vec<u8> = (0..=16).collect();
        let mut codes = vec![(0u32, 0u32); 17];
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
        // SOS
        out.extend([0xFF, 0xDA]);
        out.extend(((6 + 2 * comps) as u16).to_be_bytes());
        out.push(comps as u8);
        for c in 0..comps {
            out.extend([c as u8 + 1, 0x00]);
        }
        out.extend([psv as u8, 0, 0]);
        // Entropy data
        let mut acc: u64 = 0;
        let mut n = 0u32;
        let mut ent = vec![];
        let mut put = |v: u32, len: u32, ent: &mut Vec<u8>| {
            for i in (0..len).rev() {
                acc = (acc << 1) | ((v >> i) & 1) as u64;
                n += 1;
                if n == 8 {
                    ent.push(acc as u8);
                    if acc as u8 == 0xFF {
                        ent.push(0);
                    }
                    acc = 0;
                    n = 0;
                }
            }
        };
        let rl = width * comps;
        let s = |r: usize, i: usize| samples[r * rl + i] as i32;
        for r in 0..height {
            for col in 0..width {
                for c in 0..comps {
                    let i = col * comps + c;
                    let pred = if r == 0 && col == 0 {
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
                        put(c16, l16, &mut ent);
                        continue;
                    }
                    let len = if diff == 0 { 0 } else { 32 - diff.unsigned_abs().leading_zeros() };
                    let (cd, l) = codes[len as usize];
                    put(cd, l, &mut ent);
                    if len > 0 {
                        if diff < 0 {
                            diff -= 1;
                        }
                        put(diff as u32 & ((1 << len) - 1), len, &mut ent);
                    }
                }
            }
        }
        // Pad the final partial byte with 1-bits (JPEG convention).
        put(0x7F, 7, &mut ent);
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
