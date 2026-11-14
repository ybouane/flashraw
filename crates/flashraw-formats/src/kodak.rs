//! Kodak raw files: the TIFF based DCS/DCR/KDC families and the header-less compact-camera dumps.
//!
//! Three kinds of input end up here:
//!
//! * TIFF files of the DCS, DCR and KDC families. Their payload layout is chosen from the model
//!   name, the compression tags and, for the EasyShare/P-series, a Kodak private directory
//!   (tag 0x8290 or 0xFE00) that also holds white balance presets, the linearisation table and,
//!   for KDC, the image size.
//! * Dumps of compact cameras (C330, C603, ...), identified by their exact size in bytes.
//! * Files that start with the 15 bytes `KodakCommonInfo`, which are such dumps too.
//!
//! The sample codecs live in `flashraw_codecs::kodak`; this module decides which one runs where.

use crate::common::*;
use crate::medium::{ErrorSlot, cfa_at_origin, decode_u16_rows, even_margins, fallback_table, matrix_from};
use crate::orientation;
use flashraw_codecs::PackedLayout;
use flashraw_codecs::kodak as codec;
use flashraw_codecs::packed::unpack;
use flashraw_core::bytes::ReadAt;
use flashraw_core::tiff::{Entry, Ifd, Tiff, TiffOptions, parse_ifd_at, tags};
use flashraw_core::*;

// ---------------------------------------------------------------------------------------------
// Tags of the Kodak private directory
// ---------------------------------------------------------------------------------------------

/// Tags that point at the private directory (DCR/DCS and KDC respectively).
const TAG_PRIVATE_DCR: u16 = 0x8290;
const TAG_PRIVATE_KDC: u16 = 0xFE00;
/// DCR: index of the white balance preset in use, the software white balance, and the six presets.
const TAG_DCR_SELECTOR: u16 = 0x03FC;
const TAG_DCR_SOFTWARE_WB: u16 = 0x03FD;
const TAG_DCR_PRESET_FIRST: u16 = 0x0848;
const TAG_DCR_PRESET_LAST: u16 = 0x084D;
/// KDC: index of the white balance preset in use, and the presets.
const TAG_KDC_SELECTOR: u16 = 0xFA0D;
const TAG_KDC_PRESET_FIRST: u16 = 0xFA25;
const TAG_KDC_PRESET_LAST: u16 = 0xFA2A;
/// Preset tag chosen by each value of the KDC selector (`None`: no preset).
const KDC_SELECTED: [Option<u16>; 7] =
    [Some(0xFA25), Some(0xFA28), Some(0xFA27), Some(0xFA29), None, None, Some(0xFA2A)];
const TAG_KDC_WIDTH: u16 = 0xFA13;
const TAG_KDC_HEIGHT: u16 = 0xFA14;
const TAG_LINEARISATION: u16 = 0x090D;
/// Offsets of the packed 12-bit data inside the KDC tag 0xFD04.
const TAG_KDC_OFFSETS: u16 = 0xFD04;
/// TIFF TransferFunction, which Kodak uses as a linearisation table.
const TAG_TRANSFER_FUNCTION: u16 = 0x0123;
/// CompressedBitsPerPixel (RADC bit rate).
const TAG_COMPRESSED_BPP: u16 = 0x9102;

/// Bytes of a stored DC120 row.
const DC120_ROW: usize = 848;

// ---------------------------------------------------------------------------------------------
// Model names
// ---------------------------------------------------------------------------------------------

/// Position of the first ASCII-case-insensitive occurrence of `needle` (ASCII) in `hay`.
fn find_ignore_case(hay: &str, needle: &str) -> Option<usize> {
    let n = needle.as_bytes();
    hay.as_bytes().windows(n.len()).position(|w| w.eq_ignore_ascii_case(n))
}

/// The model without maker words and file-version suffixes: "KODAK EasyShare Z981 Digital Camera"
/// becomes "EasyShare Z981", the DC-series bodies collapse to their bare name.
fn short_model(model: &str) -> String {
    const PREFIXES: [&str; 4] = ["eastman kodak company ", "kodak ", "digital camera ", "digital science "];
    const DC_NAMES: [&str; 4] = ["DC120", "DC50", "DC40", "DC25"];
    let is_space = |c: char| c.is_ascii_whitespace();
    let mut s = model.trim_matches(is_space);
    if let Some(i) = find_ignore_case(s, " digital camera") {
        s = &s[..i];
    }
    if let Some(i) = s.find("FILE VERSION") {
        s = &s[..i];
    }
    s = s.trim_matches(is_space);
    for prefix in PREFIXES {
        let n = prefix.len();
        // Byte-wise: the prefix is ASCII, so `n` is a character boundary whenever it matches.
        if s.len() > n && s.as_bytes()[..n].eq_ignore_ascii_case(prefix.as_bytes()) {
            s = s[n..].trim_matches(is_space);
        }
    }
    match DC_NAMES.iter().find(|name| s.split_whitespace().any(|w| w == **name)) {
        Some(name) => (*name).to_string(),
        None => s.to_string(),
    }
}

/// The short model without the marketing words "EasyShare" and "ZOOM": the key of the colour table.
fn normalised_model(short: &str) -> String {
    let kept: Vec<&str> = short
        .split_whitespace()
        .filter(|w| !w.eq_ignore_ascii_case("EasyShare") && !w.eq_ignore_ascii_case("ZOOM"))
        .collect();
    kept.join(" ")
}

// ---------------------------------------------------------------------------------------------
// The Kodak private directory
// ---------------------------------------------------------------------------------------------

/// What the private directory tells about the image.
#[derive(Default)]
struct PrivateInfo {
    /// As-shot multipliers (R, G, B, G2) before normalisation.
    white_balance: Option<[f32; 4]>,
    /// Image size of KDC files (0 when absent).
    width: u32,
    height: u32,
    /// Linearisation table (tag 0x090D).
    table: Option<Vec<u16>>,
}

/// Integer table values as 16 bit entries (at most 65536); `None` when the entry holds none.
fn table_values(e: &Entry) -> Option<Vec<u16>> {
    let v: Vec<u16> = e.u32s().into_iter().take(1 << 16).map(|x| x as u16).collect();
    (!v.is_empty()).then_some(v)
}

/// Multipliers of a DCR preset: the stored triple is a gain-like (R, G, B) value, the red and
/// blue multipliers are relative to green squared.
fn dcr_preset(e: &Entry) -> [f32; 4] {
    let value = |i: usize| e.f32(i).filter(|v| v.is_finite() && *v > 0.001).unwrap_or(1.0);
    let (r, g, b) = (value(0), value(1), value(2));
    let g2 = g * g;
    [(g2 / r).trunc(), g.trunc(), (g2 / b).trunc(), g.trunc()]
}

/// The white balance written by the camera software: three 16 bit values at fixed offsets of a
/// 72 byte record, each giving the gain `2048 / value`.
fn software_white_balance(e: &Entry) -> Option<[f32; 4]> {
    let bytes = e.bytes();
    let value = |at: usize| bytes.u16_at(e.order, at).ok().map(|v| f32::from(v.max(1)));
    let (r, g, b) = (value(40)?, value(42)?, value(44)?);
    Some([2048.0 / r, 2048.0 / g, 2048.0 / b, 2048.0 / g])
}

fn read_private(tiff: &Tiff) -> PrivateInfo {
    let mut info = PrivateInfo::default();
    let Some(pointer) = tiff.find_entry(TAG_PRIVATE_DCR).or_else(|| tiff.find_entry(TAG_PRIVATE_KDC)) else {
        return info;
    };
    let Some(offset) = pointer.u32(0) else { return info };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let Ok(ifd) = parse_ifd_at(tiff.data, tiff.order, offset, opts) else { return info };

    // Entries are evaluated in tag order: the selectors come before the presets they pick.
    let mut entries: Vec<&Entry> = ifd.entries.iter().collect();
    entries.sort_by_key(|e| e.tag);
    let mut selected: Option<u16> = None;
    let mut software = false;
    for e in entries {
        match e.tag {
            TAG_DCR_SELECTOR => {
                if let Some(s) = e.i32(0) {
                    selected = (0..=5).contains(&s).then(|| TAG_DCR_PRESET_FIRST + s as u16);
                }
            }
            TAG_DCR_SOFTWARE_WB if e.count == 72 => {
                if let Some(wb) = software_white_balance(e) {
                    info.white_balance = Some(wb);
                    software = true;
                    selected = None;
                }
            }
            TAG_DCR_PRESET_FIRST..=TAG_DCR_PRESET_LAST if !software => {
                if e.tag == TAG_DCR_PRESET_LAST && selected.is_none() {
                    selected = Some(TAG_DCR_PRESET_LAST);
                }
                if selected == Some(e.tag) {
                    info.white_balance = Some(dcr_preset(e));
                }
            }
            TAG_KDC_SELECTOR => {
                if let Some(&b) = e.bytes().first() {
                    selected = KDC_SELECTED.get(usize::from(b)).copied().flatten();
                }
            }
            TAG_KDC_PRESET_FIRST..=TAG_KDC_PRESET_LAST if !software && selected == Some(e.tag) => {
                let v = e.u32s();
                if v.len() >= 3 {
                    info.white_balance = Some([v[0] as f32, v[1] as f32, v[2] as f32, v[1] as f32]);
                }
            }
            TAG_KDC_WIDTH => {
                if let Some(v) = e.u32(0) {
                    info.width = v;
                }
            }
            TAG_KDC_HEIGHT => {
                if let Some(v) = e.u32(0) {
                    info.height = v.saturating_add(1) & !1;
                }
            }
            TAG_LINEARISATION => {
                if let Some(t) = table_values(e) {
                    info.table = Some(t);
                }
            }
            _ => {}
        }
    }
    info
}

// ---------------------------------------------------------------------------------------------
// Linearisation curve
// ---------------------------------------------------------------------------------------------

/// A 65536 entry lookup table built from a stored table (entries past its end repeat the last).
struct Curve {
    map: Vec<u16>,
    /// The last stored entry.
    white: u16,
}

impl Curve {
    fn from_table(table: &[u16]) -> Option<Curve> {
        let white = *table.last()?;
        let mut map = vec![white; 1 << 16];
        let n = table.len().min(map.len());
        map[..n].copy_from_slice(&table[..n]);
        Some(Curve { map, white })
    }
}

/// The curve of the file: the Kodak table if there is one, else the table (tag 0x0123 or the DNG
/// linearisation table) of the last directory that has one.
fn file_curve(tiff: &Tiff, private: &PrivateInfo) -> Option<Curve> {
    if let Some(t) = &private.table {
        return Curve::from_table(t);
    }
    let table =
        tiff.all_ifds().into_iter().rev().find_map(|ifd| {
            table_values(ifd.get(TAG_TRANSFER_FUNCTION).or_else(|| ifd.get(tags::LINEARIZATION_TABLE))?)
        })?;
    Curve::from_table(&table)
}

/// A table of 65536 entries where entry `i` is `i`.
fn identity_curve() -> Vec<u16> {
    (0..=u16::MAX).collect()
}

// ---------------------------------------------------------------------------------------------
// Payload kinds and their decoding
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 65000 block coder, one sample per pixel.
    Block65000,
    /// 65000 block coder, RGB pixels.
    Block65000Rgb,
    /// 65000 block coder, subsampled YCbCr.
    Block65000Ycbcr,
    Dc120,
    /// DC40/DC50 stream; the value is its bit rate field.
    Radc {
        cbpp: u32,
    },
    /// Lossless JPEG of the given length.
    Ljpeg {
        len: usize,
    },
    /// One byte per sample through the curve, `stride` bytes per row.
    Eight {
        stride: usize,
    },
    /// 12 bit samples, most significant bits first.
    Packed12,
    Raw16,
    /// Compact camera YUV rows; with `skip32` 32 rows are skipped after every 32 rows.
    C330 {
        skip32: bool,
    },
    C603,
}

impl Kind {
    fn cpp(self) -> u32 {
        match self {
            Kind::Block65000Rgb | Kind::Block65000Ycbcr | Kind::C330 { .. } | Kind::C603 => 3,
            _ => 1,
        }
    }
}

/// Everything the decode closure needs.
struct Plan {
    kind: Kind,
    offset: usize,
    width: u32,
    height: u32,
    /// Lookup table of 65536 entries; `None` is the identity map.
    curve: Option<Vec<u16>>,
}

impl Plan {
    fn decode(&self, data: &[u8], ctx: &DecodeContext) -> Result<RawImage> {
        let mut img = RawImage::new_u16(self.width, self.height, self.kind.cpp(), &ctx.limits)?;
        let (w, h) = (self.width as usize, self.height as usize);
        let out = img.u16_mut();
        let curve = self.curve.as_deref();
        let offset = self.offset;
        match self.kind {
            Kind::Block65000 => {
                let identity;
                let curve = match curve {
                    Some(c) => c,
                    None => {
                        identity = identity_curve();
                        &identity
                    }
                };
                ctx.profiler.time(Stage::EntropyDecode, || self.decode_block65000(data, ctx, curve, out))?
            }
            Kind::Block65000Rgb => {
                ctx.profiler.time(Stage::EntropyDecode, || decode_rgb65000(data, offset, w, h, out))?
            }
            Kind::Block65000Ycbcr => {
                let identity;
                let curve = match curve {
                    Some(c) => c,
                    None => {
                        identity = identity_curve();
                        &identity
                    }
                };
                ctx.profiler.time(Stage::EntropyDecode, || decode_ycbcr65000(data, offset, w, h, curve, out))?
            }
            Kind::Dc120 => ctx.profiler.time(Stage::Unpack, || decode_dc120(data, ctx, offset, w, h, out))?,
            Kind::Radc { cbpp } => ctx.profiler.time(Stage::EntropyDecode, || {
                let src = data.get(offset..).ok_or_else(|| Error::corrupt("RADC: offset beyond file"))?;
                codec::decode_radc(src, w, h, cbpp, out)
            })?,
            Kind::Ljpeg { len } => ctx.profiler.time(Stage::EntropyDecode, || -> Result<()> {
                let out = std::cell::RefCell::new(out);
                compressed_unit(
                    data,
                    ctx,
                    DataUnit::Payload,
                    (offset, len),
                    (w as u64) * (h as u64),
                    |src| {
                        let mut out = out.borrow_mut();
                        flashraw_codecs::ljpeg::decode_into_par(
                            src,
                            Default::default(),
                            &mut out,
                            w,
                            w,
                            h,
                            ctx.parallel,
                        )
                        .map(|_| ())
                    },
                    || out.borrow_mut().fill(0),
                )?;
                if let Some(c) = curve {
                    apply_curve(&mut out.borrow_mut(), c, ctx.parallel);
                }
                Ok(())
            })?,
            Kind::Eight { stride } => ctx
                .profiler
                .time(Stage::Unpack, || decode_bytes_through_curve(data, ctx, offset, stride, curve, w, h, out))?,
            Kind::Packed12 => ctx.profiler.time(Stage::Unpack, || {
                let src = ctx.sensor_rows(data, DataUnit::Payload, offset, RowGeometry::tight(h, w, 12))?;
                unpack(
                    src,
                    PackedLayout::tight(w, h, 12, PackOrder::MsbBytes),
                    out,
                    w,
                    ctx.parallel,
                    ctx.simd,
                    ctx.recover,
                )
            })?,
            Kind::Raw16 => ctx.profiler.time(Stage::Unpack, || {
                decode_u16_rows(ctx, data, DataUnit::Payload, offset, ByteOrder::Little, w, w.saturating_mul(2), out)
            })?,
            Kind::C330 { skip32 } => {
                let curve = curve.unwrap_or(&[]);
                ctx.profiler.time(Stage::Unpack, || decode_c330(data, ctx, offset, skip32, curve, w, h, out))?
            }
            Kind::C603 => {
                let curve = curve.unwrap_or(&[]);
                ctx.profiler.time(Stage::Unpack, || decode_c603(data, ctx, offset, curve, w, h, out))?
            }
        }
        Ok(img)
    }

    /// The 65000 block coder with one sample per pixel; any error fails the decode.
    fn decode_block65000(&self, data: &[u8], ctx: &DecodeContext, curve: &[u16], out: &mut [u16]) -> Result<()> {
        let w = self.width as usize;
        if !ctx.parallel {
            let mut pos = self.offset;
            for row in out.chunks_mut(w) {
                pos = codec::decode_65000_row(data, pos, row, curve)?;
            }
            return Ok(());
        }
        let starts = codec::scan_65000_rows(data, self.offset, w, self.height as usize)?;
        let errors = ErrorSlot::default();
        par::for_each_row_chunk(out, w, 16, true, |first, chunk| {
            for (i, row) in chunk.chunks_mut(w).enumerate() {
                if let Err(e) = codec::decode_65000_row(data, starts[first + i], row, curve) {
                    errors.set(e);
                    return;
                }
            }
        });
        errors.into_result()
    }
}

/// Decode one 65000 block of `len` samples into `buf`; returns whether it holds absolute values
/// and the position of the next block.
fn next_block(data: &[u8], pos: usize, len: usize, buf: &mut [i16]) -> Result<(bool, usize)> {
    let absolute = codec::decode_block_65000(data, pos, len, buf)?;
    let size = codec::block_len_65000(data, pos, len).ok_or_else(|| Error::corrupt("truncated data"))?.0;
    let next = pos.checked_add(size).ok_or_else(|| Error::corrupt("Kodak 65000 offset overflow"))?;
    Ok((absolute, next))
}

fn decode_rgb65000(data: &[u8], offset: usize, w: usize, h: usize, out: &mut [u16]) -> Result<()> {
    let mut buf = [0i16; 776];
    let mut pos = offset;
    for row in 0..h {
        let mut col = 0;
        while col < w {
            let len = (w - col).min(256);
            let (absolute, next) = next_block(data, pos, 3 * len, &mut buf)?;
            let mut sum = [0i32; 3];
            for i in 0..len {
                let base = (row * w + col + i) * 3;
                for c in 0..3 {
                    let d = i32::from(buf[3 * i + c]);
                    sum[c] = if absolute { d } else { sum[c] + d };
                    out[base + c] = sum[c] as u16;
                }
            }
            pos = next;
            col += len;
        }
    }
    Ok(())
}

fn decode_ycbcr65000(data: &[u8], offset: usize, w: usize, h: usize, curve: &[u16], out: &mut [u16]) -> Result<()> {
    let mut buf = [0i16; 776];
    let mut pos = offset;
    for row in (0..h).step_by(2) {
        let mut col = 0;
        while col < w {
            let len = (w - col).min(128);
            let (_, next) = next_block(data, pos, 3 * len, &mut buf)?;
            let (mut cb, mut cr) = (0i32, 0i32);
            let mut luma = [[0i32; 2]; 2];
            for i in (0..len).step_by(2) {
                let at = 3 * i;
                cb += i32::from(buf[at + 4]);
                cr += i32::from(buf[at + 5]);
                let g = -((cb + cr + 2) >> 2);
                let offsets = [g + cr, g, g + cb];
                for (n, (j, k)) in [(0usize, 0usize), (0, 1), (1, 0), (1, 1)].into_iter().enumerate() {
                    luma[j][k] = luma[j][1 - k] + i32::from(buf[at + n]);
                    let (y, x) = (row + j, col + i + k);
                    if y < h && x < w {
                        let base = (y * w + x) * 3;
                        for c in 0..3 {
                            out[base + c] = curve[(luma[j][k] + offsets[c]).clamp(0, 4095) as usize];
                        }
                    }
                }
            }
            pos = next;
            col += len;
        }
    }
    Ok(())
}

fn decode_dc120(data: &[u8], ctx: &DecodeContext, offset: usize, w: usize, h: usize, out: &mut [u16]) -> Result<()> {
    let src = ctx.sensor_bytes(data, DataUnit::Payload, offset, DC120_ROW.saturating_mul(h))?;
    let complete = src.bytes.len() / DC120_ROW;
    for (r, (stored, row)) in src.bytes.chunks_exact(DC120_ROW).zip(out.chunks_mut(w)).enumerate() {
        codec::dc120_row(stored, r, row);
    }
    if let Some(t) = src.truncation {
        ctx.recovered(CorruptReason::Truncated(t), (h.saturating_sub(complete) * w) as u64);
    }
    Ok(())
}

/// One byte per sample, mapped through the curve (identity without one).
#[allow(clippy::too_many_arguments)]
fn decode_bytes_through_curve(
    data: &[u8],
    ctx: &DecodeContext,
    offset: usize,
    stride: usize,
    curve: Option<&[u16]>,
    w: usize,
    h: usize,
    out: &mut [u16],
) -> Result<()> {
    let src = ctx.sensor_rows(data, DataUnit::Payload, offset, RowGeometry::packed(h, w, 8, stride))?;
    let stride = stride.max(w);
    // The first 256 entries are all a byte can index: a fixed-size table needs no bounds checks.
    let lut: Option<[u16; 256]> = curve.map(|c| std::array::from_fn(|i| c.get(i).copied().unwrap_or(0)));
    // A table lookup per byte is light enough that every worker still adds throughput.
    par::for_each_row_chunk_bounded(out, w, par::threads(), ctx.parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(w).enumerate() {
            let stored = src.get((first + i) * stride..).unwrap_or(&[]);
            match &lut {
                Some(c) => row.iter_mut().zip(stored).for_each(|(o, &b)| *o = c[usize::from(b)]),
                None => row.iter_mut().zip(stored).for_each(|(o, &b)| *o = u16::from(b)),
            }
        }
    });
    Ok(())
}

/// C330 rows: `2 * (w + 16)` bytes each; `skip32` dumps leave a gap of 32 row-widths after every 32 rows.
#[allow(clippy::too_many_arguments)]
fn decode_c330(
    data: &[u8],
    ctx: &DecodeContext,
    offset: usize,
    skip32: bool,
    curve: &[u16],
    w: usize,
    h: usize,
    out: &mut [u16],
) -> Result<()> {
    let stored = 2 * (w + 16);
    let row_start = |r: usize| -> usize {
        let gap = if skip32 { (r / 32).saturating_mul((w + 16) * 32) } else { 0 };
        r.saturating_mul(stored).saturating_add(gap)
    };
    let src =
        ctx.sensor_bytes(data, DataUnit::Payload, offset, row_start(h.saturating_sub(1)).saturating_add(stored))?;
    let bytes = src.bytes;
    let row_at = |r: usize| {
        let start = row_start(r);
        bytes.get(start..start.checked_add(stored)?)
    };
    let present = (0..h).take_while(|&r| row_at(r).is_some()).count();
    par::for_each_row_chunk(out, 3 * w, 16, ctx.parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(3 * w).enumerate() {
            if let Some(px) = row_at(first + i) {
                codec::c330_row(px, w, curve, row);
            }
        }
    });
    if let Some(t) = src.truncation {
        ctx.recovered(CorruptReason::Truncated(t), ((h - present) * 3 * w) as u64);
    }
    Ok(())
}

/// C603 rows: one `3 * w` byte record per pair of rows.
fn decode_c603(
    data: &[u8],
    ctx: &DecodeContext,
    offset: usize,
    curve: &[u16],
    w: usize,
    h: usize,
    out: &mut [u16],
) -> Result<()> {
    let record = 3 * w;
    let src = ctx.sensor_bytes(data, DataUnit::Payload, offset, h.div_ceil(2).saturating_mul(record))?;
    let bytes = src.bytes;
    // One band of rows per worker: the rows are cheap to convert, and fewer, longer runs of
    // output let each worker fault in its own pages without contending with the others.
    par::for_each_row_chunk_bounded(out, 3 * w, par::threads(), ctx.parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(3 * w).enumerate() {
            let r = first + i;
            if let Some(px) = bytes.get((r / 2) * record..(r / 2 + 1) * record) {
                codec::c603_row(px, w, r & 1 == 1, curve, row);
            }
        }
    });
    if let Some(t) = src.truncation {
        let rows = (2 * (bytes.len() / record)).min(h);
        ctx.recovered(CorruptReason::Truncated(t), ((h - rows) * record) as u64);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// TIFF based files
// ---------------------------------------------------------------------------------------------

/// Where and how the sensor data is stored.
struct Placement {
    kind: Kind,
    offset: usize,
    width: u32,
    height: u32,
    bits: u8,
    /// A white level the layout dictates.
    white: Option<u32>,
}

/// The directory that holds the raw image: the largest one (the last on ties) that is not a
/// reduced-resolution copy and looks like sensor data.
fn find_raw_ifd<'t, 'a>(tiff: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    let mut best: Option<(&Ifd, u64)> = None;
    for ifd in tiff.all_ifds() {
        let (w, h) = (ifd.width(), ifd.height());
        if w == 0 || h == 0 || ifd.is_reduced_resolution() {
            continue;
        }
        let photometric = ifd.u32(tags::PHOTOMETRIC);
        let sensor_like = photometric == Some(32803)
            || ifd.compression() == 65000
            || ifd.bps() > 8
            || (ifd.bps() == 8 && photometric == Some(1) && ifd.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1) == 1);
        let area = u64::from(w) * u64::from(h);
        if sensor_like && best.is_none_or(|(_, a)| area >= a) {
            best = Some((ifd, area));
        }
    }
    best.map(|(ifd, _)| ifd)
}

fn first_strip_offset(raw: &Ifd) -> Result<usize> {
    strips(raw).first().map(|s| s.0).ok_or_else(|| Error::invalid_metadata("Kodak: raw image without strips"))
}

/// Decide the payload layout of a TIFF based file.
fn place_payload(
    data: &[u8],
    tiff: &Tiff,
    raw: Option<&Ifd>,
    model: &str,
    private: &PrivateInfo,
    curve: Option<&Curve>,
) -> Result<Placement> {
    if model == "DC120" {
        let r = raw.ok_or_else(|| Error::invalid_metadata("no raw image"))?;
        if r.compression() == 7 {
            return Err(Error::unsupported_compression("KDC", "DC120 JPEG-compressed raw"));
        }
        let offset = first_strip_offset(r)?;
        return Ok(Placement { kind: Kind::Dc120, offset, width: 848, height: 976, bits: 8, white: Some(255) });
    }
    if model == "DC50" || model == "DC40" {
        let r = raw.ok_or_else(|| Error::invalid_metadata("no raw image"))?;
        let cbpp = r
            .get(TAG_COMPRESSED_BPP)
            .or_else(|| tiff.find_entry(TAG_COMPRESSED_BPP))
            .and_then(|e| e.bytes().u32_at(e.order, 0).ok())
            .unwrap_or(0);
        let offset = if model == "DC40" { 1152 } else { 19712 };
        return Ok(Placement {
            kind: Kind::Radc { cbpp },
            offset,
            width: 768,
            height: 512,
            bits: 14,
            white: Some(0x3FFF),
        });
    }
    if let Some(e) = tiff.find_entry(TAG_KDC_OFFSETS).filter(|e| e.len() >= 13) {
        let field = |at: usize| e.bytes().u32_at(e.order, at).unwrap_or(0) as usize;
        let mut offset = field(16).saturating_add(field(48));
        if model.as_bytes().get(..9).is_some_and(|p| p.eq_ignore_ascii_case(b"EasyShare")) {
            offset = if offset < 0x15000 { 0x15000 } else { 0x17000 };
        }
        return Ok(Placement {
            kind: Kind::Packed12,
            offset,
            width: private.width,
            height: private.height,
            bits: 12,
            white: None,
        });
    }

    let r = raw.ok_or_else(|| no_raw_ifd(tiff, "Kodak", Error::invalid_metadata("Kodak: no raw image")))?;
    let (width, height) = (r.width(), r.height());
    let offset = first_strip_offset(r)?;
    let (compression, photometric, bps) = (r.compression(), r.u32(tags::PHOTOMETRIC).unwrap_or(32803), r.bps());
    let mut place = Placement { kind: Kind::Packed12, offset, width, height, bits: 12, white: None };
    match (compression, photometric, bps) {
        (65000, 2, _) => place.kind = Kind::Block65000Rgb,
        (65000, 6, _) => place.kind = Kind::Block65000Ycbcr,
        (65000, _, _) => place.kind = Kind::Block65000,
        (7 | 99, _, _) => {
            let len = strips(r).first().map_or(0, |s| s.1).max(data.len().saturating_sub(offset));
            place.kind = Kind::Ljpeg { len };
        }
        (1, _, 8) => {
            place.kind = Kind::Eight { stride: width as usize };
            place.bits = 8;
            place.white = Some(curve.map_or(255, |c| u32::from(c.map[255])));
        }
        (1, _, 12) => {}
        (1, _, 16) => {
            place.kind = Kind::Raw16;
            place.bits = 16;
        }
        (262, _, _) => return Err(Error::unsupported_compression("Kodak", "262 (DCS Pro 14 legacy)")),
        (c, p, b) => {
            return Err(Error::unsupported_compression("Kodak", format!("compression {c}, photometric {p}, {b} bits")));
        }
    }
    Ok(place)
}

/// Open a Kodak TIFF-based raw (DCR, KDC, DCS TIFF).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("Kodak: TIFF without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    let model = short_model(&meta.model);
    let normalised = normalised_model(&model);
    let cam = db_lookup(&mut meta, "");
    meta.make = "Kodak".to_string();
    meta.model = model.clone();
    if cam.is_none() {
        meta.canonical_model = normalised.clone();
    }
    let table = fallback_table("Kodak", &normalised);

    let private = read_private(&tiff);
    let curve = file_curve(&tiff, &private);
    let raw = find_raw_ifd(&tiff);
    let place = place_payload(data, &tiff, raw, &model, &private, curve.as_ref())?;
    let (w, h) = (place.width, place.height);
    if w == 0 || h == 0 {
        return Err(Error::invalid_metadata("Kodak: missing raw dimensions"));
    }
    opts.limits.check_dims(w, h, 3)?;

    // Geometry and levels.
    let mut info = FrameInfo::new(w, h, place.bits);
    info.compression = match place.kind {
        Kind::Block65000 => Compression::Kodak65000,
        Kind::Block65000Rgb => Compression::KodakRgb,
        Kind::Block65000Ycbcr => Compression::KodakYcbcr,
        Kind::Ljpeg { .. } => Compression::LosslessJpeg,
        Kind::Dc120 => Compression::KodakDc120,
        Kind::Radc { .. } => Compression::Other,
        _ => Compression::Uncompressed,
    };
    let white = place
        .white
        .or_else(|| table.map(|t| t.white).filter(|&v| v > 0).map(|v| v as u32))
        .or_else(|| curve.as_ref().map(|c| u32::from(c.white)))
        .unwrap_or((1u32 << place.bits) - 1);
    info.white_level = WhiteLevel::uniform(white.min(0xFFFF));
    let mut black = 0.0;
    if let Some(t) = table {
        if t.black > 0 || (t.black < 0 && black == 0.0) {
            black = t.black.unsigned_abs() as f32;
        }
    }
    info.black_level = BlackLevel::uniform(black);

    // Some bodies record two extra columns on each side of the image.
    let bordered = ["NC2000", "EOSDCS", "DCS4"].iter().any(|p| model.starts_with(p));
    let visible = if bordered { Rect::new(2, 0, w.saturating_sub(4), h) } else { Rect::full(w, h) };
    info.active_area = even_margins(visible);
    info.crop = info.active_area;

    if matches!(place.kind, Kind::Block65000Rgb | Kind::Block65000Ycbcr) {
        info.cpp = 3;
        info.cfa = None;
        info.layout = SensorLayout::Linear;
    } else {
        let from_file = if place.kind == Kind::Packed12 { None } else { raw.and_then(tiff_ep_cfa) };
        let cfa = from_file
            .or_else(|| cam.and_then(|c| c.cfa).and_then(parse_db_cfa))
            .unwrap_or_else(|| cfa_at_origin(CfaPattern::GRBG, visible.x, visible.y));
        info.cfa = Some(cfa);
        info.layout = layout_for(&cfa);
    }
    info.white_balance = private.white_balance.and_then(|m| WhiteBalance::from_multipliers(m[0], m[1], m[2], m[3]));
    if matches!(place.kind, Kind::Radc { .. }) {
        info.white_balance = WhiteBalance::from_multipliers(1.0, 1.0, 1.0, 1.0);
    }
    if let Some(t) = table {
        info.color_matrices = matrix_from(t);
    }
    if place.kind == Kind::Dc120 {
        info.pixel_aspect = (976.0 / 0.75 / 848.0) as f32;
    }

    // Orientation: the raw directory's tag wins over IFD0's; the camera JPEG may know better.
    let exif = raw
        .and_then(|r| r.get(tags::ORIENTATION))
        .or_else(|| ifd0.get(tags::ORIENTATION))
        .and_then(|e| e.u32(0))
        .and_then(Orientation::from_exif_checked);
    let mut previews = tiff_previews(data, 0, &tiff, Orientation::Normal);
    let area = |p: &PreviewInfo| u64::from(p.width) * u64::from(p.height);
    let camera_jpeg = previews
        .iter()
        .filter(|p| p.encoding == PreviewEncoding::Jpeg)
        .fold(None::<&PreviewInfo>, |best, p| match best {
            Some(b) if area(b) > area(p) => best,
            _ => Some(p),
        })
        .and_then(|p| {
            let start = usize::try_from(p.offset).ok()?;
            data.get(start..start.checked_add(usize::try_from(p.length).ok()?)?)
        })
        .and_then(orientation::jpeg_exif_orientation);
    let policy = orientation::policy_for(&meta, cam);
    info.orientation = orientation::apply(&mut meta, exif, camera_jpeg, None, policy);
    orientation::resolve_previews(data, &mut previews, info.orientation, info.crop.width, info.crop.height);
    meta.previews = previews;
    meta.set_extra("kodak_model", normalised);

    let format = match place.kind {
        Kind::Packed12 | Kind::Dc120 | Kind::Radc { .. } => RawFormat::Kdc,
        Kind::Block65000 | Kind::Block65000Rgb | Kind::Block65000Ycbcr => RawFormat::Dcr,
        _ => RawFormat::Dcs,
    };
    let plan = Plan { kind: place.kind, offset: place.offset, width: w, height: h, curve: curve.map(|c| c.map) };
    let decode = move |ctx: &DecodeContext| plan.decode(data, ctx);
    Ok(Box::new(SimpleDecoder::new(format, meta, info, Box::new(decode))))
}

// ---------------------------------------------------------------------------------------------
// Header-less dumps
// ---------------------------------------------------------------------------------------------

/// A dump layout identified by the file size.
struct Dump {
    size: usize,
    raw_width: u32,
    raw_height: u32,
    /// Margins around the visible image: left, top, right, bottom.
    margins: [u32; 4],
    /// Colour index (0 R, 1 G, 2 B) of the four positions of the 2x2 pattern, two bits each from
    /// the least significant end; 0 means three samples per pixel.
    cfa_code: u8,
    model: &'static str,
    offset: usize,
}

const fn dump(
    size: usize,
    w: u32,
    h: u32,
    margins: [u32; 4],
    cfa_code: u8,
    model: &'static str,
    offset: usize,
) -> Dump {
    Dump { size, raw_width: w, raw_height: h, margins, cfa_code, model, offset }
}

const DUMPS: [Dump; 12] = [
    dump(4159302, 2338, 1779, [1, 33, 1, 2], 0x94, "C330", 0),
    dump(4162462, 2338, 1779, [1, 33, 1, 2], 0x94, "C330", 3160),
    dump(2247168, 1232, 912, [0, 0, 16, 0], 0x00, "C330", 0),
    dump(3370752, 1232, 912, [0, 0, 16, 0], 0x00, "C330", 0),
    dump(6163328, 2864, 2152, [0, 0, 0, 0], 0x94, "C603", 0),
    dump(6166488, 2864, 2152, [0, 0, 0, 0], 0x94, "C603", 3160),
    dump(460800, 640, 480, [0, 0, 0, 0], 0x00, "C603", 0),
    dump(9116448, 2848, 2134, [0, 0, 0, 0], 0x00, "C603", 0),
    dump(12241200, 4040, 3030, [2, 0, 0, 13], 0x49, "12MP", 0),
    dump(12272756, 4040, 3030, [2, 0, 0, 13], 0x49, "12MP", 31556),
    dump(18000000, 4000, 3000, [0, 0, 0, 0], 0x00, "12MP", 0),
    dump(614400, 640, 480, [0, 3, 0, 0], 0x94, "KAI-0340", 0),
];

fn dump_pattern(code: u8) -> Option<CfaPattern> {
    match code {
        0x94 => Some(CfaPattern::RGGB),
        0x49 => Some(CfaPattern::GBRG),
        0x16 => Some(CfaPattern::BGGR),
        0x61 => Some(CfaPattern::GRBG),
        _ => None,
    }
}

/// The 256 entry curve stored in the file at `at` (little-endian, missing bytes read as 0),
/// extended to 65536 entries by repeating its last entry.
fn stored_dump_curve(data: &[u8], at: usize) -> Vec<u16> {
    let byte = |i: usize| data.get(i).copied().unwrap_or(0);
    let mut map = vec![0u16; 1 << 16];
    for (i, v) in map[..256].iter_mut().enumerate() {
        *v = u16::from_le_bytes([byte(at + 2 * i), byte(at + 2 * i + 1)]);
    }
    let last = map[255];
    map[256..].fill(last);
    map
}

/// Signature probe: the "KodakCommonInfo" dump header.
pub fn probe(data: &[u8]) -> bool {
    data.starts_with(b"KodakCommonInfo")
}

/// Open a file recognised by [`probe`] (a size-identified dump).
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    open_headerless(data, opts).unwrap_or_else(|| Err(Error::UnsupportedFormat("unknown Kodak dump size".into())))
}

/// Header-less Kodak dumps identified by exact file size; `None` when the size is not one of them.
pub fn open_headerless<'a>(data: &'a [u8], opts: &OpenOptions) -> Option<Result<Box<dyn RawDecoder + 'a>>> {
    let d = DUMPS.iter().find(|d| d.size == data.len())?;
    if d.cfa_code == 0 && d.model == "12MP" && looks_like_packed12(data, d.raw_width as usize) {
        return None;
    }
    Some(open_dump(data, d, opts))
}

fn open_dump<'a>(data: &'a [u8], d: &Dump, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let (rw, rh) = (d.raw_width, d.raw_height);
    opts.limits.check_dims(rw, rh, 3)?;
    let mut meta = RawMetadata::new("Kodak", d.model);
    let cam = db_lookup(&mut meta, "");
    let [left, top, right, bottom] = d.margins;
    let visible = Rect::new(left, top, rw - left - right, rh - top - bottom);
    let bits_per_pixel = ((data.len() - d.offset) as u64 * 8 / (u64::from(rw) * u64::from(rh))) as u32;
    let pattern = dump_pattern(d.cfa_code);
    let compact = matches!(d.model, "C603" | "C330" | "12MP");
    let curve = compact.then(|| {
        if d.cfa_code != 0 && d.offset > 0 {
            stored_dump_curve(data, if d.offset < 4096 { 168 } else { 5252 })
        } else {
            codec::gamma_curve(0.0, 3.875, 255)
        }
    });
    let white_255 = curve.as_ref().map_or(255, |c| u32::from(c[255]));
    let (kind, (w, h), bits, white) = match (pattern.is_some(), compact) {
        (false, true) if d.model == "C330" => {
            (Kind::C330 { skip32: bits_per_pixel > 16 }, (visible.width, visible.height), 16u8, white_255)
        }
        (false, true) => (Kind::C603, (visible.width, visible.height), 16, white_255),
        (true, true) => (Kind::Eight { stride: rw as usize }, (rw, rh), 8, white_255),
        (_, false) => (Kind::Raw16, (rw, rh), 12, 4095),
    };
    let mut info = FrameInfo::new(w, h, bits);
    info.white_level = WhiteLevel::uniform(white);
    match pattern {
        Some(p) => {
            info.cfa = Some(cfa_at_origin(p, visible.x, visible.y));
            info.layout = SensorLayout::Bayer;
            info.active_area = even_margins(visible.clamp_to(w, h));
            info.crop = info.active_area;
        }
        None => {
            info.cpp = 3;
            info.layout = SensorLayout::Linear;
        }
    }
    info.compression = if matches!(kind, Kind::C330 { .. } | Kind::C603) {
        Compression::KodakYcbcr
    } else {
        Compression::Uncompressed
    };
    // Only the colour matrix comes from the camera database.
    let known =
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, masked: true, color: false };
    apply_db(&mut info, &meta, cam, known);

    let plan = Plan { kind, offset: d.offset, width: w, height: h, curve };
    let decode = move |ctx: &DecodeContext| plan.decode(data, ctx);
    Ok(Box::new(SimpleDecoder::new(RawFormat::Other, meta, info, Box::new(decode))))
}

/// Whether `data` reads as 12-bit packed sensor rows of `width` samples rather than 8-bit YCbCr bytes.
///
/// Heuristic: in packed data neighbouring samples are about equally smooth in both halves of every
/// three byte group; YCbCr bytes read this way make the first sample jump by multiples of 256.
pub(crate) fn looks_like_packed12(data: &[u8], width: usize) -> bool {
    let stride = width.saturating_mul(3) / 2;
    if stride == 0 || data.len() < stride {
        return false;
    }
    let rows = data.len() / stride;
    let step = (rows / 64).max(1);
    let (mut first, mut second) = (0u64, 0u64);
    for row in data.chunks_exact(stride).step_by(step) {
        let mut previous: Option<(i32, i32)> = None;
        for t in row.chunks_exact(3) {
            let s0 = i32::from(t[0]) | (i32::from(t[1] & 15) << 8);
            let s1 = i32::from(t[1] >> 4) | (i32::from(t[2]) << 4);
            if let Some((p0, p1)) = previous {
                first += u64::from((s0 - p0).unsigned_abs());
                second += u64::from((s1 - p1).unsigned_abs());
            }
            previous = Some((s0, s1));
        }
    }
    first < 3 * second.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_model_names() {
        assert_eq!(short_model("KODAK EasyShare Z981 Digital Camera    "), "EasyShare Z981");
        assert_eq!(short_model("Kodak DC120 ZOOM Digital Camera"), "DC120");
        assert_eq!(short_model("Kodak Digital Science DC50 Zoom Camera"), "DC50");
        assert_eq!(short_model("DCS460D         FILE VERSION 3  "), "DCS460D");
        assert_eq!(short_model("KODAK P880 ZOOM DIGITAL CAMERA"), "P880 ZOOM");
        assert_eq!(short_model("kodak "), "kodak");
        assert_eq!(short_model("Kod\u{e4}k \u{ff} Digital Camera"), "Kod\u{e4}k \u{ff}");
        assert_eq!(
            short_model("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"),
            "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"
        );
        assert_eq!(short_model(""), "");
    }

    #[test]
    fn normalised_model_names() {
        assert_eq!(normalised_model("P712 ZOOM"), "P712");
        assert_eq!(normalised_model("EASYSHARE Z1015 IS"), "Z1015 IS");
        assert_eq!(normalised_model(" "), "");
    }

    #[test]
    fn curves_extend_their_last_entry() {
        let table: Vec<u16> = (0..256).map(|i| i * 10).collect();
        let c = Curve::from_table(&table).unwrap();
        assert_eq!((c.map.len(), c.map[300], c.white), (65536, 2550, 2550));
        assert!(Curve::from_table(&[]).is_none());
    }

    /// Deterministic pseudo-random bytes.
    fn noise(n: usize, mut seed: u32) -> Vec<u8> {
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn packed12_versus_ycbcr_bytes() {
        let (w, h) = (4000usize, 3000usize);
        // A smooth scene of 12-bit values, stored as 8-bit bytes with a little noise ...
        let scene = |x: usize, y: usize| (1000 + (x / 3) % 200 + (y / 5) % 300) as u16;
        let jitter = noise(18_000_000, 3);
        let bytes: Vec<u8> = (0..18_000_000usize)
            .map(|i| match i % 3 {
                // luma changes from one byte triple to the next, chroma stays flat
                0 => ((i / 3) as u8).wrapping_mul(37) ^ (jitter[i] & 3),
                _ => (scene(i % w, i / w) >> 4) as u8 ^ (jitter[i] & 3),
            })
            .collect();
        assert!(!looks_like_packed12(&bytes, w));
        assert!(open_headerless(&bytes, &OpenOptions::default()).is_some());

        // ... and as 12-bit packed little-endian pairs.
        let mut packed = Vec::with_capacity(w * h * 3 / 2);
        for y in 0..h {
            for x in (0..w).step_by(2) {
                let (a, b) = (scene(x, y), scene(x + 1, y));
                packed.extend([a as u8, ((a >> 8) as u8 & 15) | ((b as u8 & 15) << 4), (b >> 4) as u8]);
            }
        }
        packed.resize(18_000_000, 0);
        assert!(looks_like_packed12(&packed, w));
        assert!(open_headerless(&packed, &OpenOptions::default()).is_none());
        assert!(open_headerless(&[0u8; 1000], &OpenOptions::default()).is_none());
    }

    #[test]
    fn packed12_probe_never_panics() {
        for width in [0usize, 1, 2, 3, 5, 4000, usize::MAX, usize::MAX / 3] {
            let _ = looks_like_packed12(&noise(5000, 7), width);
        }
        assert!(!looks_like_packed12(&[], 4));
    }

    #[test]
    fn signature_probe() {
        assert!(probe(b"KodakCommonInfo and more"));
        assert!(!probe(b"KodakCommon"));
        assert!(matches!(open(b"KodakCommonInfo....", &OpenOptions::default()), Err(Error::UnsupportedFormat(_))));
    }

    // -----------------------------------------------------------------------------------------
    // Containers built from the format description
    // -----------------------------------------------------------------------------------------

    use crate::testkit::{self, Lcg, Tag, TiffWriter, ascii, bytes, long, rationals, short};

    /// A Kodak TIFF with one raw image: `private` becomes the directory behind tag `private_tag`.
    #[allow(clippy::too_many_arguments)]
    fn kodak_tiff(
        model: &str,
        (w, h): (u32, u32),
        (bps, compression): (u16, u16),
        payload: &[u8],
        extra: Vec<Tag>,
        private: Option<(u16, Vec<Tag>)>,
    ) -> Vec<u8> {
        // The payload follows the directories, so a shortened payload is a shortened file.
        let build = |data_at: u32| {
            let mut t = TiffWriter::new();
            let mut tags = vec![
                ascii(0x010F, "KODAK"),
                ascii(0x0110, model),
                long(0x0100, &[w]),
                long(0x0101, &[h]),
                short(0x0102, &[bps]),
                short(0x0103, &[compression]),
                short(0x0106, &[32803]),
                long(0x0111, &[data_at]),
                long(0x0117, &[payload.len() as u32]),
            ];
            if let Some((tag, entries)) = &private {
                let at = t.ifd(
                    entries
                        .iter()
                        .map(|e| Tag { tag: e.tag, typ: e.typ, count: e.count, value: e.value.clone() })
                        .collect(),
                );
                tags.push(long(*tag, &[at]));
            }
            tags.extend(extra.iter().map(|e| Tag { tag: e.tag, typ: e.typ, count: e.count, value: e.value.clone() }));
            let ifd0 = t.ifd(tags);
            t.finish(ifd0)
        };
        let mut file = build(0);
        file = build(file.len() as u32);
        file.extend(payload);
        file
    }

    fn decode_with(d: &dyn RawDecoder, recover: bool, parallel: bool) -> (Result<RawImage>, Vec<DecodeWarning>) {
        let profiler = Profiler::default();
        let mut ctx = DecodeContext::new(&profiler);
        ctx.recover = recover;
        ctx.parallel = parallel;
        let image = d.decode(&ctx);
        (image, ctx.take_warnings())
    }

    fn open_bytes(data: &[u8]) -> Result<Box<dyn RawDecoder + '_>> {
        crate::open(data, &OpenOptions::default())
    }

    fn pack12(samples: &[u16]) -> Vec<u8> {
        samples
            .chunks(2)
            .flat_map(|p| {
                let (a, b) = (p[0], p.get(1).copied().unwrap_or(0));
                [(a >> 4) as u8, ((a & 15) << 4) as u8 | (b >> 8) as u8, b as u8]
            })
            .collect()
    }

    #[test]
    fn uncompressed_sixteen_bit_rows() {
        let samples: Vec<u16> = (0..24).map(|i| (i * 2711 + 5) as u16).collect();
        let payload: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let file = kodak_tiff("XT-1", (6, 4), (16, 1), &payload, vec![], None);
        let d = open_bytes(&file).unwrap();
        let (info, meta) = (d.frame_info(), d.metadata());
        assert_eq!(
            (d.format(), info.bits_per_sample, info.compression),
            (RawFormat::Dcs, 16, Compression::Uncompressed)
        );
        assert_eq!(info.white_level.values, [65535]);
        assert_eq!((meta.make.as_str(), meta.model.as_str()), ("Kodak", "XT-1"));
        assert_eq!(meta.extra("kodak_model"), Some("XT-1"));
        assert_eq!(info.cfa, Some(CfaPattern::GRBG));
        for parallel in [false, true] {
            assert_eq!(decode_with(&*d, true, parallel).0.unwrap().u16(), &samples[..]);
        }
        // Two bytes short: the last sample is lost; strict fails, recover reports it.
        let full = kodak_tiff("XT-1", (6, 4), (16, 1), &payload, vec![], None);
        let mut declared = full.clone();
        declared.truncate(full.len() - 2);
        let d = open_bytes(&declared).unwrap();
        let (img, warnings) = decode_with(&*d, true, false);
        assert_eq!(img.unwrap().u16()[..23], samples[..23]);
        assert_eq!(warnings.iter().map(|w| w.samples_filled).sum::<u64>(), 1);
        assert!(decode_with(&*d, false, false).0.unwrap_err().truncation().is_some());
    }

    #[test]
    fn packed_twelve_bit_rows() {
        let samples: Vec<u16> = (0..24).map(|i| (i * 311 + 7) as u16 & 0xFFF).collect();
        let file = kodak_tiff("XT-1", (6, 4), (12, 1), &pack12(&samples), vec![], None);
        let d = open_bytes(&file).unwrap();
        assert_eq!(d.frame_info().white_level.values, [4095]);
        assert_eq!(decode_with(&*d, true, true).0.unwrap().u16(), &samples[..]);
    }

    #[test]
    fn eight_bit_rows_use_the_transfer_function() {
        let curve: Vec<u16> = (0..256).map(|i| i * 4).collect();
        let bytes_in: Vec<u8> = (0..40).map(|i| (i * 7) as u8).collect();
        let file = kodak_tiff("DCS4XX", (10, 4), (8, 1), &bytes_in, vec![short(0x0123, &curve)], None);
        let d = open_bytes(&file).unwrap();
        let info = d.frame_info();
        assert_eq!(info.white_level.values, [1020]);
        // Two border columns on each side are not part of the image.
        assert_eq!(info.active_area, Rect::new(2, 0, 6, 4));
        assert_eq!(info.crop, info.active_area);
        let want: Vec<u16> = bytes_in.iter().map(|&b| u16::from(b) * 4).collect();
        assert_eq!(decode_with(&*d, true, false).0.unwrap().u16(), &want[..]);
        // Without a curve the bytes are used as they are.
        let file = kodak_tiff("XT-1", (10, 4), (8, 1), &bytes_in, vec![], None);
        let d = open_bytes(&file).unwrap();
        assert_eq!(d.frame_info().white_level.values, [255]);
        let want: Vec<u16> = bytes_in.iter().map(|&b| u16::from(b)).collect();
        assert_eq!(decode_with(&*d, true, false).0.unwrap().u16(), &want[..]);
    }

    #[test]
    fn dc120_rows() {
        let mut rng = Lcg(5);
        let payload = rng.bytes(848 * 976);
        let model = "Kodak DC120 ZOOM Digital Camera";
        let file = kodak_tiff(model, (848, 976), (8, 1), &payload, vec![], None);
        let d = open_bytes(&file).unwrap();
        let info = d.frame_info();
        assert_eq!(
            (d.format(), info.compression, d.metadata().model.as_str()),
            (RawFormat::Kdc, Compression::KodakDc120, "DC120")
        );
        assert_eq!((info.width, info.height, info.white_level.values[0]), (848, 976, 255));
        assert_eq!(info.pixel_aspect, (976.0f64 / 0.75 / 848.0) as f32);
        let img = decode_with(&*d, true, false).0.unwrap();
        let mut row = vec![0u16; 848];
        codec::dc120_row(&payload[848 * 5..848 * 6], 5, &mut row);
        assert_eq!(&img.u16()[848 * 5..848 * 6], &row[..]);
        // Half the rows missing.
        let cut = kodak_tiff(model, (848, 976), (8, 1), &payload[..848 * 488 + 10], vec![], None);
        let d = open_bytes(&cut).unwrap();
        let (_, warnings) = decode_with(&*d, true, false);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].samples_filled, 488 * 848);
        // JPEG-compressed DC120 raws are refused.
        let jpeg = kodak_tiff(model, (848, 976), (8, 7), &payload, vec![], None);
        assert_eq!(open_bytes(&jpeg).err().map(|e| e.kind()), Some("UnsupportedCompression"));
    }

    #[test]
    fn unsupported_modes_are_reported() {
        let file = kodak_tiff("XT-1", (4, 2), (16, 262), &[0; 16], vec![], None);
        assert_eq!(open_bytes(&file).err().map(|e| e.kind()), Some("UnsupportedCompression"));
        let file = kodak_tiff("XT-1", (4, 2), (16, 34713), &[0; 16], vec![], None);
        assert_eq!(open_bytes(&file).err().map(|e| e.kind()), Some("UnsupportedCompression"));
        let file = kodak_tiff("XT-1", (0, 2), (16, 1), &[0; 16], vec![], None);
        assert_eq!(open_bytes(&file).err().map(|e| e.kind()), Some("InvalidMetadata"));
    }

    fn software_white_balance_record(v: [u16; 3]) -> Tag {
        let mut b = vec![0u8; 72];
        for (i, x) in v.iter().enumerate() {
            b[40 + 2 * i..42 + 2 * i].copy_from_slice(&x.to_le_bytes());
        }
        bytes(0x03FD, &b)
    }

    fn coefficients(file: &[u8]) -> Option<[f32; 4]> {
        open_bytes(file).unwrap().frame_info().white_balance.map(|w| w.coeffs)
    }

    #[test]
    fn dcr_white_balance_presets() {
        let payload = vec![0u8; 16];
        let preset = |tag: u16, triple: [u32; 3]| rationals(tag, &triple.map(|v| (v, 1)));
        let make =
            |private: Vec<Tag>| kodak_tiff("DCS Pro 14N", (4, 2), (16, 1), &payload, vec![], Some((0x8290, private)));
        let presets = |selector: Option<u16>| {
            let mut v = vec![preset(0x0848, [4, 8, 16]), preset(0x084D, [6080, 5704, 4180])];
            v.extend(selector.map(|s| short(0x03FC, &[s])));
            v
        };
        // Selector 5 and no selector both pick the "as shot" preset 0x084D.
        for selector in [Some(5), None] {
            let c = coefficients(&make(presets(selector))).unwrap();
            assert_eq!(c, [5351.0 / 5704.0, 1.0, 7783.0 / 5704.0, 1.0]);
        }
        // Selector 0 picks 0x0848: green squared over red and blue.
        let c = coefficients(&make(presets(Some(0)))).unwrap();
        assert_eq!(c, [(64.0f32 / 4.0) / 8.0, 1.0, (64.0f32 / 16.0) / 8.0, 1.0]);
        // A selector without its preset, or out of range, leaves the balance unset.
        assert_eq!(coefficients(&make(vec![short(0x03FC, &[2]), preset(0x0848, [4, 8, 16])])), None);
        assert_eq!(coefficients(&make(vec![short(0x03FC, &[9]), preset(0x0848, [4, 8, 16])])), None);
        // The camera software's record overrides the presets.
        let mut with_software = presets(Some(0));
        with_software.push(software_white_balance_record([2048, 2700, 1950]));
        let c = coefficients(&make(with_software)).unwrap();
        assert_eq!((c[0] * 10000.0).round(), 13184.0);
        assert_eq!((c[2] * 10000.0).round(), 13846.0);
        // Only records of exactly 72 bytes count.
        let mut short_record = presets(Some(0));
        short_record.push(bytes(0x03FD, &[1; 71]));
        assert_eq!(coefficients(&make(short_record)).unwrap()[0], (64.0f32 / 4.0) / 8.0);
    }

    #[test]
    fn kdc_files_take_size_and_balance_from_the_private_directory() {
        let samples: Vec<u16> = (0..32).map(|i| (i * 97 + 3) as u16 & 0xFFF).collect();
        // The packed data sits at one of two fixed offsets for EasyShare models.
        let build = |selector: u8, offsets: u32| {
            let private = vec![
                bytes(0xFA0D, &[selector]),
                long(0xFA25, &[65536, 65536, 65536]),
                long(0xFA28, &[1, 2, 3]),
                long(0xFA29, &[101376, 65536, 128256]),
                long(0xFA2A, &[119296, 65536, 117248]),
                short(0xFA13, &[8]),
                short(0xFA14, &[3]),
            ];
            let mut words = vec![0u32; 13];
            words[4] = offsets;
            let mut t = TiffWriter::new();
            let priv_at = t.ifd(private);
            let mut file_tags = vec![ascii(0x010F, "KODAK"), ascii(0x0110, "KODAK EasyShare Z999 Digital Camera")];
            file_tags.push(long(0xFE00, &[priv_at]));
            file_tags.push(long(0xFD04, &words));
            let ifd0 = t.ifd(file_tags);
            let mut file = t.finish(ifd0);
            file.resize(0x15000, 0);
            file.extend(pack12(&samples));
            file
        };
        let file = build(3, 100);
        let d = open_bytes(&file).unwrap();
        let info = d.frame_info();
        assert_eq!((d.format(), info.width, info.height), (RawFormat::Kdc, 8, 4));
        assert_eq!(d.metadata().model, "EasyShare Z999");
        assert_eq!(d.metadata().extra("kodak_model"), Some("Z999"));
        let c = info.white_balance.unwrap().coeffs;
        assert_eq!(c, [101376.0 / 65536.0, 1.0, 128256.0 / 65536.0, 1.0]);
        assert_eq!(decode_with(&*d, true, true).0.unwrap().u16(), &samples[..]);
        // Selector 6 picks 0xFA2A, selector 4 and 5 pick nothing.
        let c = open_bytes(&build(6, 100)).unwrap().frame_info().white_balance.unwrap().coeffs;
        assert_eq!(c[0], 119296.0 / 65536.0);
        assert!(open_bytes(&build(4, 100)).unwrap().frame_info().white_balance.is_none());
        assert!(open_bytes(&build(200, 100)).unwrap().frame_info().white_balance.is_none());
    }

    #[test]
    fn dump_sizes_select_the_layout() {
        // (size, width, height, samples per pixel, bits)
        let cases = [
            (4159302usize, 2338u32, 1779u32, 1u32, 8u8),
            (4162462, 2338, 1779, 1, 8),
            (2247168, 1216, 912, 3, 16),
            (3370752, 1216, 912, 3, 16),
            (6163328, 2864, 2152, 1, 8),
            (6166488, 2864, 2152, 1, 8),
            (460800, 640, 480, 3, 16),
            (9116448, 2848, 2134, 3, 16),
            (12241200, 4040, 3030, 1, 8),
            (12272756, 4040, 3030, 1, 8),
            (614400, 640, 480, 1, 12),
        ];
        for (size, w, h, cpp, bits) in cases {
            let data = vec![0u8; size];
            let d = open_bytes(&data).unwrap_or_else(|e| panic!("{size}: {e}"));
            let info = d.frame_info();
            assert_eq!((info.width, info.height, info.cpp, info.bits_per_sample), (w, h, cpp, bits), "{size}");
            assert_eq!(d.metadata().make, "Kodak");
            for off in [1usize, size.wrapping_sub(1)] {
                let data = vec![0u8; if off == 1 { size + 1 } else { off }];
                assert!(open_headerless(&data, &OpenOptions::default()).is_none() || data.len() == 12272756);
            }
        }
        // Unknown sizes are not dumps; a "KodakCommonInfo" file of an unknown size is refused.
        assert!(open_headerless(&[0u8; 4159303], &OpenOptions::default()).is_none());
        let mut named = b"KodakCommonInfo".to_vec();
        named.resize(1000, 0);
        assert_eq!(open_bytes(&named).err().map(|e| e.kind()), Some("UnsupportedFormat"));
    }

    #[test]
    fn dumps_decode_by_policy() {
        // C330 with 16 bits per pixel: rows of 2 * (w + 16) bytes.
        let data = vec![128u8; 2247168];
        let d = open_bytes(&data).unwrap();
        let img = decode_with(&*d, true, false).0.unwrap();
        assert!(img.u16().iter().all(|&v| v == img.u16()[0]));
        assert_eq!(decode_with(&*d, true, true).0.unwrap().u16(), img.u16());
        // 100 kB short: complete rows stay, the rest is reported.
        let cut = &data[..data.len() - 100_000];
        let d = open_bytes(&data[..2247168]).unwrap();
        let _ = d;
        let truncated = Kind::C330 { skip32: false };
        let plan = Plan { kind: truncated, offset: 0, width: 1216, height: 912, curve: None };
        let profiler = Profiler::default();
        let ctx = DecodeContext::new(&profiler);
        let img = plan.decode(cut, &ctx).unwrap();
        let rows_present = cut.len() / (2 * (1216 + 16));
        assert!(img.u16()[rows_present * 3 * 1216..].iter().all(|&v| v == 0));
        let missing = (912 - rows_present) as u64 * 3 * 1216;
        assert_eq!(ctx.take_warnings().iter().map(|w| w.samples_filled).sum::<u64>(), missing);
        let mut strict = DecodeContext::new(&profiler);
        strict.recover = false;
        assert!(plan.decode(cut, &strict).unwrap_err().truncation().is_some());
    }

    #[test]
    fn damaged_containers_never_panic() {
        let samples: Vec<u16> = (0..24).map(|i| (i * 311 + 7) as u16 & 0xFFF).collect();
        let curve: Vec<u16> = (0..256).map(|i| i * 4).collect();
        let private = [bytes(0xFA0D, &[3]), long(0xFA29, &[1, 2, 3]), short(0xFA13, &[6]), short(0xFA14, &[4])];
        let files = [
            kodak_tiff("XT-1", (6, 4), (12, 1), &pack12(&samples), vec![], None),
            kodak_tiff("DCS4XX", (6, 4), (8, 1), &[9; 24], vec![short(0x0123, &curve)], None),
            kodak_tiff("DC50 Zoom", (6, 4), (16, 1), &[1; 48], vec![long(0x9102, &[7])], None),
            kodak_tiff(
                "DCS Pro",
                (6, 4),
                (16, 65000),
                &[7; 64],
                vec![],
                Some((0x8290, private.iter().map(|_| bytes(1, &[0])).collect())),
            ),
            kodak_tiff("Kodak DC120", (848, 976), (8, 1), &[3; 4000], vec![], None),
        ];
        let mut rng = Lcg(99);
        for file in &files {
            for round in 0..200 {
                let mut f = file.clone();
                for _ in 0..1 + round % 5 {
                    let at = rng.below(f.len() as u64) as usize;
                    f[at] = rng.next() as u8;
                }
                f.truncate(f.len() - rng.below(8) as usize * (round % 3));
                if let Ok(d) = open_bytes(&f) {
                    for recover in [true, false] {
                        let _ = decode_with(&*d, recover, round % 2 == 0).0;
                    }
                }
            }
        }
    }

    #[test]
    fn model_names_survive_any_text() {
        for model in [
            "\u{e9}",
            "kodak \u{e9}\u{e9}",
            "digital camera \u{20ac}",
            "\u{1F4F7} Digital Camera",
            "eastman kodak compan\u{e9} x",
        ] {
            let _ = normalised_model(&short_model(model));
        }
    }

    /// Fingerprints of what the reader reports for the corpus files (see `testkit::check_golden`).
    #[test]
    #[ignore = "needs the corpus"]
    fn golden_metadata_of_corpus_files() {
        testkit::check_golden(GOLDEN);
    }

    const GOLDEN: &[(&str, &str)] = &[
        ("Kodak/DC120/P000439.kdc", "f87bc838cc435000"),
        ("Kodak/DC120/RAW_KODAK_DC120.KDC", "dfb9a6cd0088e311"),
        ("Kodak/DC120/RAW_KODAK_DC120_WITH_JPEG.KDC", "error:UnsupportedCompression"),
        ("Kodak/DC50/RAW_KODAK_DC50.KDC", "ff5a8112938382ab"),
        ("Kodak/DCS Pro 14N/D4C50031.DCR", "e314b80b05e66f44"),
        ("Kodak/DCS Pro 14nx/D7465857.DCR", "e4e6f0242c1e6486"),
        ("Kodak/DCS Pro SLRc/F3G93631.DCR", "209e8dbdba82600d"),
        ("Kodak/DCS Pro SLRn/G3X25544.DCR", "8fc56691ff326a04"),
        ("Kodak/DCS460D/RAW_KODAK_DCS460D_FILEVERSION_3.TIF", "bbd784e08d81960b"),
        ("Kodak/DCS520C/23HK3627.TIF", "34479f4453626f2c"),
        ("Kodak/DCS560C/RAW_KODAK_DCS560C.TIF", "d53604a86b880ed7"),
        ("Kodak/DCS760C/86L57188.DCR", "af42fea51b0f40e1"),
        ("Kodak/EOS DCS 1/RAW_CANON_DCS1.TIF", "048f83e35e81ca34"),
        ("Kodak/EOS DCS 3/DCS00240.TIF", "18b5773f7aefcf0d"),
        ("Kodak/EasyShare Z981/100_1548.KDC", "bcd8ef0e6ead5cf5"),
        ("Kodak/EasyShare Z990/100_0891.KDC", "f90cf9d5b71a3d85"),
        ("Kodak/KODAK P712 ZOOM DIGITAL CAMERA/100_4343.KDC", "bd212fd7cf1f5de1"),
        ("Kodak/Kodak C330/RAW_KODAK_C330_FORMAT420_YRGB.RAW", "3dcf2e59d717b44c"),
        ("Kodak/Kodak C330/RAW_KODAK_C330_FORMAT422_YRGB.RAW", "3dcf2e59d717b44c"),
        ("Kodak/Kodak C330/RAW_KODAK_C330_FORMAT_NONE_YRGB.RAW", "3dcf2e59d717b44c"),
        ("Kodak/Kodak C603  Kodak C643/KODAK_C603_C643_FORMAT_422_CCDI0001.RAW", "2a16aaea75467bc6"),
        ("Kodak/Kodak C603  Kodak C643/KODAK_C603_C643_FORMAT_422_CFAI0001.RAW", "c08587e666c3496c"),
        ("Kodak/Kodak C603  Kodak C643/KODAK_C603_C643_FORMAT_NONE_CCDI0001.RAW", "2a16aaea75467bc6"),
        ("Kodak/Kodak C603  Kodak C643/KODAK_C603_C643_FORMAT_NONE_CFAI0001.RAW", "c08587e666c3496c"),
        ("Kodak/Kodak C603  Kodak C643/KODAK_C603_C643_FORMAT_NONE_YRGB0001.RAW", "47713ce8e2e289c8"),
        ("Kodak/Kodak C603  Kodak C643/KODAK_C603_C643_Format_420_CCDI0001.RAW", "2a16aaea75467bc6"),
        ("Kodak/Kodak C603  Kodak C643/KODAK_C603_C643_Format_420_CFAI0001.RAW", "c08587e666c3496c"),
        ("Kodak/Kodak C603  Kodak C643/KODAK_C603_C643_Format_420_YRGB0001.RAW", "47713ce8e2e289c8"),
        ("Kodak/P880/100_3710.KDC", "c0e471ebbaf86ce2"),
        ("Kodak/Z1015 IS/RAW_KODAK_EASYSHARE_Z1015-IS.KDC", "31492f467c08e925"),
    ];
}
