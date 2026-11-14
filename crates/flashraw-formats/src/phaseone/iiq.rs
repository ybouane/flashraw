//! Phase One IIQ: the vendor directory, sensor geometry, black calibration, white balance,
//! colour choice and the hand-off to the row decoders.

use super::OpenOptions;
use crate::common::{Known, SimpleDecoder, apply_db, db_color, db_lookup, tiff_previews};
use crate::medium::{
    ErrorSlot, cfa_at_origin, decode_u16_rows, even_margins, fallback_table, matrix_from, matrix_from_romm,
};
use flashraw_codecs::phaseone::{IiqLParams, decode_l_row, decode_s_row, unscramble_pairs};
use flashraw_core::bits::{BitPumpMsb, BitPumpMsb32};
use flashraw_core::bytes::ascii_field;
use flashraw_core::integrity::check_range;
use flashraw_core::tiff::Tiff;
use flashraw_core::*;
use std::sync::Arc;

// ------------------------------------------------------------------------------------------ header

/// Header signature scan window: the header sits at offset 0 (bare) or 8 (after a TIFF header).
const SCAN_WINDOW: usize = 28;
const DIR_INVALID: u32 = 0x0bad_0bad;
const MAX_ENTRIES: u32 = 4096;
const ENTRY_LEN: usize = 16;

/// Offset of the IIQ header (the base of all stored offsets), if the file has one.
pub(super) fn find_header(data: &[u8]) -> Option<usize> {
    (0..=SCAN_WINDOW).find(|&o| {
        let Some(h) = data.get(o..o + 8) else { return false };
        (&h[..4] == b"IIII" || &h[..4] == b"MMMM") && (&h[5..8] == b"waR" || &h[4..7] == b"Raw")
    })
}

// ------------------------------------------------------------------------------------ directory

mod tag {
    pub const ORIENTATION: u32 = 0x0100;
    pub const SERIAL: u32 = 0x0102;
    pub const MATRIX_ROMM: u32 = 0x0106;
    pub const WB_RGB: u32 = 0x0107;
    pub const WIDTH: u32 = 0x0108;
    pub const HEIGHT: u32 = 0x0109;
    pub const LEFT: u32 = 0x010a;
    pub const TOP: u32 = 0x010b;
    pub const VISIBLE_W: u32 = 0x010c;
    pub const VISIBLE_H: u32 = 0x010d;
    pub const FORMAT: u32 = 0x010e;
    pub const PAYLOAD: u32 = 0x010f;
    pub const CALIBRATION: u32 = 0x0110;
    pub const KEYS: u32 = 0x0112;
    pub const TEMPERATURE: u32 = 0x0210;
    pub const ROW_INDEX: u32 = 0x021c;
    pub const T_BLACK: u32 = 0x021d;
    pub const SPLIT_COL: u32 = 0x0222;
    pub const ROW_REFS: u32 = 0x0223;
    pub const SPLIT_ROW: u32 = 0x0224;
    pub const COL_REFS: u32 = 0x0225;
    pub const FIRMWARE: u32 = 0x0301;
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    tag: u32,
    len: u32,
    /// The value itself or an offset from the base.
    data: u32,
    /// The four stored bytes of `data` (the scramble keys are two u16 in there).
    raw: [u8; 4],
}

struct Directory<'a> {
    file: &'a [u8],
    base: usize,
    order: ByteOrder,
    entries: Vec<Entry>,
}

impl<'a> Directory<'a> {
    fn parse(file: &'a [u8], base: usize) -> Result<Self> {
        let order = match file.get(base..base + 2) {
            Some(b"II") => ByteOrder::Little,
            Some(b"MM") => ByteOrder::Big,
            _ => return Err(Error::UnsupportedFormat("not an IIQ header".into())),
        };
        let offset = file.u32_at(order, base + 8)?;
        if offset == DIR_INVALID {
            return Err(Error::corrupt("IIQ: directory marked invalid"));
        }
        let pos = base
            .checked_add(offset as usize)
            .filter(|p| p.checked_add(8).is_some())
            .ok_or_else(|| Error::corrupt("IIQ: directory offset overflow"))?;
        let count = file.u32_at(order, pos)?;
        file.u32_at(order, pos + 4)?;
        if count > MAX_ENTRIES {
            return Err(Error::corrupt(format!("IIQ: invalid directory entry count {count}")));
        }
        let table = file.get(pos + 8..).unwrap_or(&[]);
        let entries: Vec<Entry> = table
            .chunks_exact(ENTRY_LEN)
            .take(count as usize)
            .map(|e| {
                let word = |i: usize| order.u32([e[i], e[i + 1], e[i + 2], e[i + 3]]);
                Entry { tag: word(0), len: word(8), data: word(12), raw: [e[12], e[13], e[14], e[15]] }
            })
            .collect();
        if entries.is_empty() {
            return Err(Error::corrupt("IIQ: empty directory"));
        }
        Ok(Directory { file, base, order, entries })
    }

    fn find(&self, tag: u32) -> Option<&Entry> {
        self.entries.iter().find(|e| e.tag == tag)
    }

    /// The inline value of an entry.
    fn inline(&self, tag: u32) -> Option<u32> {
        self.find(tag).map(|e| e.data)
    }

    /// Where an offset stored in the directory points in the file.
    fn at(&self, offset: u32) -> usize {
        self.base.saturating_add(offset as usize)
    }

    /// The `len` bytes an entry points to, cut to the end of the file.
    fn bytes(&self, e: &Entry) -> &'a [u8] {
        let start = self.at(e.data);
        let end = start.saturating_add(e.len as usize).min(self.file.len());
        self.file.get(start..end).unwrap_or(&[])
    }

    /// `N` floats stored at an entry's offset, in file order; `None` when the entry is too short.
    fn floats<const N: usize>(&self, tag: u32) -> Option<[f32; N]> {
        let e = self.find(tag)?;
        let b = self.bytes(e);
        if b.len() < N * 4 {
            return None;
        }
        let mut out = [0f32; N];
        for (v, c) in out.iter_mut().zip(b.chunks_exact(4)) {
            *v = f32::from_bits(self.order.u32([c[0], c[1], c[2], c[3]]));
        }
        Some(out)
    }

    fn text(&self, tag: u32) -> Option<String> {
        let s = ascii_field(self.bytes(self.find(tag)?));
        (!s.is_empty()).then_some(s)
    }
}

// ---------------------------------------------------------------------------------------- payload

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    /// Uncompressed 16-bit samples; formats 1 and 2 are scrambled.
    Raw(u32),
    L(IiqLParams),
    S,
}

impl Codec {
    fn from_format(f: u32) -> Option<Self> {
        Some(match f {
            0..=2 => Codec::Raw(f),
            3 | 5 => Codec::L(IiqLParams { shift: 2, low_curve: f == 5 }),
            8 => Codec::L(IiqLParams { shift: 0, low_curve: false }),
            6 => Codec::S,
            _ => return None,
        })
    }
}

/// Byte range handed to the row decoder of one image row.
#[derive(Debug, Clone, Copy)]
struct Extent {
    start: usize,
    end: usize,
}

enum RowIndex {
    NotUsed,
    /// The table does not lie inside the file: every decode fails.
    Outside {
        pos: usize,
        len: usize,
    },
    Rows(Vec<Extent>),
}

fn row_index(dir: &Directory, codec: Codec, entry: Option<&Entry>, h: usize, payload_start: usize) -> RowIndex {
    let Some(e) = entry.filter(|_| matches!(codec, Codec::L(_) | Codec::S)) else { return RowIndex::NotUsed };
    let (pos, len) = (dir.at(e.data), h.saturating_mul(4));
    if check_range(dir.file.len(), DataUnit::Payload, pos, len).is_err() {
        return RowIndex::Outside { pos, len };
    }
    let starts: Vec<usize> = dir.file[pos..pos + len]
        .chunks_exact(4)
        .map(|c| payload_start.saturating_add(dir.order.u32([c[0], c[1], c[2], c[3]]) as usize))
        .collect();
    let file_len = dir.file.len();
    let rows = if codec == Codec::S {
        // A row runs up to the next larger row start.
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        sorted.dedup();
        starts
            .iter()
            .map(|&start| {
                let next = sorted.get(sorted.partition_point(|&s| s <= start)).copied().unwrap_or(file_len);
                Extent { start, end: next.min(file_len) }
            })
            .collect()
    } else {
        // The L coder reads ahead of its last symbol: it gets the rest of the file.
        starts.iter().map(|&start| Extent { start, end: file_len }).collect()
    };
    RowIndex::Rows(rows)
}

/// Everything the decode closures need; shared between them.
struct Plan<'a> {
    file: &'a [u8],
    width: u32,
    height: u32,
    codec: Codec,
    order: ByteOrder,
    payload_start: usize,
    keys: (u16, u16),
    rows: RowIndex,
}

impl Plan<'_> {
    /// Decode image rows `y0 .. y0 + rows` into a `width x rows` buffer.
    fn decode_rows(&self, ctx: &DecodeContext, y0: usize, rows: usize) -> Result<RawImage> {
        let w = self.width as usize;
        let mut img = RawImage::new_u16(self.width, rows as u32, 1, &ctx.limits)?;
        match self.codec {
            Codec::Raw(format) => {
                let start = self.payload_start.saturating_add(y0.saturating_mul(w).saturating_mul(2));
                let out = img.u16_mut();
                ctx.profiler.time(Stage::Unpack, || {
                    decode_u16_rows(ctx, self.file, DataUnit::Payload, start, self.order, w, w * 2, out)
                })?;
                if format != 0 {
                    unscramble_pairs(out, self.keys.0, self.keys.1, format);
                }
            }
            Codec::L(_) | Codec::S => {
                let extents = match &self.rows {
                    RowIndex::Rows(e) => e,
                    RowIndex::Outside { pos, len } => {
                        let t = check_range(self.file.len(), DataUnit::Payload, *pos, *len).err();
                        return Err(t.map(Error::truncated).unwrap_or_else(|| Error::corrupt("IIQ: row index")));
                    }
                    RowIndex::NotUsed => return Err(Error::invalid_metadata("IIQ: missing row offset table")),
                };
                let errs = ErrorSlot::default();
                ctx.profiler.time(Stage::EntropyDecode, || {
                    par::for_each_row_chunk(img.u16_mut(), w, 4, ctx.parallel, |first, chunk| {
                        if errs.has_error() {
                            return;
                        }
                        for (i, row) in chunk.chunks_mut(w).enumerate() {
                            let y = y0 + first + i;
                            if let Err(e) = self.decode_row(extents, y, row) {
                                errs.set(e);
                                return;
                            }
                        }
                    });
                });
                errs.into_result()?;
            }
        }
        Ok(img)
    }

    fn decode_row(&self, extents: &[Extent], y: usize, out: &mut [u16]) -> Result<()> {
        let ext = extents.get(y).ok_or_else(|| Error::corrupt("IIQ: row outside the index"))?;
        let src = match self.file.get(ext.start..ext.end.max(ext.start)) {
            Some(s) if ext.start < self.file.len() => s,
            _ => {
                let needed = ext.end.saturating_sub(ext.start).max(1);
                let t =
                    Truncation::new(DataUnit::Strip(y as u32), ext.start as u64, needed as u64, self.file.len() as u64);
                return Err(Error::truncated(t));
            }
        };
        match (self.codec, self.order) {
            (Codec::L(p), ByteOrder::Little) => decode_l_row(&mut BitPumpMsb32::new(src), out, p),
            (Codec::L(p), ByteOrder::Big) => decode_l_row(&mut BitPumpMsb::new(src), out, p),
            _ => decode_s_row(&mut BitPumpMsb32::new(src), out),
        }
    }

    fn decode_region(&self, ctx: &DecodeContext, r: Rect) -> Result<RawImage> {
        let visible = r
            .intersect(&Rect::full(self.width, self.height))
            .ok_or_else(|| Error::InvalidArgument(format!("region {r} outside image")))?;
        // Scrambled pairs straddle rows when the width is odd: start at the top then.
        let from = match self.codec {
            Codec::Raw(f) if f != 0 && self.width % 2 == 1 => 0,
            _ => visible.y,
        };
        let img = self.decode_rows(ctx, from as usize, (visible.bottom() - from) as usize)?;
        let part = Rect::new(visible.x, visible.y - from, visible.width, visible.height);
        if part == Rect::full(img.width, img.height) {
            return Ok(img);
        }
        img.crop(part)
    }

    fn units(&self) -> Vec<DecodeUnit> {
        let RowIndex::Rows(rows) = &self.rows else { return Vec::new() };
        rows.iter()
            .enumerate()
            .map(|(y, e)| DecodeUnit {
                rect: Rect::new(0, y as u32, self.width, 1),
                offset: e.start as u64,
                length: e.end.saturating_sub(e.start) as u64,
            })
            .collect()
    }
}

// ------------------------------------------------------------------------------------------- black

/// Per-line black adjustments from a table of `(a, b)` i16 pairs: the vendor subtracts `a` over the
/// first `split` samples of the line (of length `n`) and `b` over the rest, reported as the negated,
/// sample-weighted mean. All-zero results are dropped.
fn black_deltas(dir: &Directory, table: u32, split: u32, lines: usize, n: usize) -> Vec<f32> {
    let Some(e) = dir.find(table).filter(|e| e.data != 0) else { return Vec::new() };
    let pos = dir.at(e.data);
    let Some(bytes) = pos.checked_add(lines.saturating_mul(4)).and_then(|end| dir.file.get(pos..end)) else {
        return Vec::new();
    };
    let s = (split as usize).min(n);
    let (a_w, b_w) = (s as f32, (n - s) as f32);
    let denom = (a_w + b_w).max(1.0);
    let at = |b: &[u8]| dir.order.u16([b[0], b[1]]) as i16 as f32;
    let deltas: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| {
            let sum = at(&c[0..2]) * a_w + at(&c[2..4]) * b_w;
            -sum / denom
        })
        .collect();
    if deltas.iter().all(|&d| d == 0.0) { Vec::new() } else { deltas }
}

// ------------------------------------------------------------------------------------------ open

/// Camera model from a firmware description such as `"IQ4 150MP, v7.00.7 (..."`.
fn model_from_firmware(fw: &str) -> String {
    let mut s = fw;
    for cut in [" camera", ",", "-"] {
        if let Some(i) = s.find(cut) {
            s = &s[..i];
        }
    }
    s.trim().to_string()
}

pub(super) fn open<'a>(
    data: &'a [u8],
    base: usize,
    given: Option<Tiff<'a>>,
    opts: &OpenOptions,
) -> Result<Box<dyn RawDecoder + 'a>> {
    let dir = Directory::parse(data, base)?;
    let w = dir.inline(tag::WIDTH).ok_or_else(|| Error::invalid_metadata("IIQ: missing raw width"))?;
    let h = dir.inline(tag::HEIGHT).ok_or_else(|| Error::invalid_metadata("IIQ: missing raw height"))?;
    let payload = *dir.find(tag::PAYLOAD).ok_or_else(|| Error::invalid_metadata("IIQ: missing data offset"))?;
    let format = dir.inline(tag::FORMAT).unwrap_or(0);
    let codec = Codec::from_format(format)
        .ok_or_else(|| Error::unsupported_compression("IIQ", format!("payload format {format}")))?;
    opts.limits.check_dims(w, h, 1)?;
    let index = dir.find(tag::ROW_INDEX).copied();
    if index.is_none() && matches!(codec, Codec::L(_) | Codec::S) {
        return Err(Error::invalid_metadata("IIQ: missing row offset table"));
    }
    let order = dir.order;
    let payload_start = dir.at(payload.data);

    // Metadata: the TIFF wrapper (if any) first, then what the directory knows.
    let tiff = if base > 0 { given.or_else(|| Tiff::parse(data).ok()) } else { None };
    let mut meta = RawMetadata::default();
    if let Some(ifd0) = tiff.as_ref().and_then(|t| t.ifds.first()) {
        flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    }
    if meta.make.is_empty() {
        meta.make = "Phase One".into();
    }
    let firmware = dir.text(tag::FIRMWARE);
    if meta.model.is_empty() {
        meta.model = model_from_firmware(firmware.as_deref().unwrap_or(""));
    }
    meta.firmware = firmware.clone().or(meta.firmware.take());
    if let Some(s) = dir.text(tag::SERIAL) {
        meta.serial = Some(s);
    }
    if let Some(t) = dir.inline(tag::TEMPERATURE) {
        meta.set_extra("sensor_temperature", f32::from_bits(t));
    }
    let cam = db_lookup(&mut meta, "");
    if let Some(c) = dir.find(tag::CALIBRATION) {
        meta.set_extra("calibration_bytes", c.len);
    }
    meta.set_extra("iiq_format", format);

    // Geometry: the vendor's RGGB starts at the visible rectangle.
    let visible = Rect::new(
        dir.inline(tag::LEFT).unwrap_or(0),
        dir.inline(tag::TOP).unwrap_or(0),
        dir.inline(tag::VISIBLE_W).unwrap_or(w),
        dir.inline(tag::VISIBLE_H).unwrap_or(h),
    )
    .clamp_to(w, h);
    let mut info = FrameInfo::new(w, h, 16);
    info.active_area = even_margins(visible);
    info.crop = info.active_area;
    if firmware.as_deref().is_some_and(|f| f.contains("Achromatic")) {
        info.cfa = None;
        info.layout = SensorLayout::Monochrome;
    } else {
        info.cfa = Some(cfa_at_origin(CfaPattern::RGGB, visible.x, visible.y));
        info.layout = SensorLayout::Bayer;
    }
    info.orientation = Orientation::from_exif(match dir.inline(tag::ORIENTATION).unwrap_or(0) & 3 {
        1 => 6,
        2 => 8,
        3 => 3,
        _ => 1,
    });
    info.compression = match codec {
        Codec::Raw(_) => Compression::Uncompressed,
        Codec::L(_) => Compression::PhaseOne,
        Codec::S => Compression::PhaseOneS,
    };

    // Black and white as reported (calibration data is not applied).
    // Formats 3, 5 and 6 store 14-bit codes << 2: the iXM-RS150F (format 6) and IQ4 150MP 14-bit
    // (format 3) samples clip at 0xfffc.
    info.white_level = WhiteLevel::uniform(if matches!(format, 3 | 5 | 6) { 0xfffc } else { 0xffff });
    info.black_level = BlackLevel::uniform(dir.inline(tag::T_BLACK).unwrap_or(0) as f32);
    info.black_level.row_deltas =
        black_deltas(&dir, tag::ROW_REFS, dir.inline(tag::SPLIT_COL).unwrap_or(0), h as usize, w as usize);
    info.black_level.col_deltas =
        black_deltas(&dir, tag::COL_REFS, dir.inline(tag::SPLIT_ROW).unwrap_or(0), w as usize, h as usize);

    if let Some([r, g, b]) = dir.floats::<3>(tag::WB_RGB) {
        info.white_balance = WhiteBalance::from_multipliers(r, g, b, g);
    }

    // Colour: fallback table, else the file's ROMM matrix when the database has none.
    if let Some(m) = fallback_table(&meta.canonical_make, &meta.canonical_model).map(matrix_from) {
        info.color_matrices = m;
    }
    if info.color_matrices.color.is_empty() && db_color(&meta, cam).color.is_empty() {
        if let Some(m) = dir.floats::<9>(tag::MATRIX_ROMM).and_then(|m| matrix_from_romm(&m)) {
            info.color_matrices = m;
        }
    }
    apply_db(
        &mut info,
        &meta,
        cam,
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, masked: true, color: false },
    );
    if let Some(t) = &tiff {
        meta.previews = tiff_previews(data, 0, t, info.orientation);
    }

    let keys = dir
        .find(tag::KEYS)
        .map(|e| (order.u16([e.raw[0], e.raw[1]]), order.u16([e.raw[2], e.raw[3]])))
        .unwrap_or((0, 0));
    let rows = row_index(&dir, codec, index.as_ref(), h as usize, payload_start);
    let plan = Arc::new(Plan { file: data, width: w, height: h, codec, order, payload_start, keys, rows });

    let whole = Arc::clone(&plan);
    let mut dec = SimpleDecoder::new(
        RawFormat::Iiq,
        meta,
        info,
        Box::new(move |ctx: &DecodeContext| whole.decode_rows(ctx, 0, whole.height as usize)),
    );
    let part = Arc::clone(&plan);
    dec.region = Some(Box::new(move |ctx: &DecodeContext, r: Rect| part.decode_region(ctx, r)));
    dec.random_access = RandomAccess::Rows;
    dec.units = plan.units();
    if codec == Codec::Raw(0) {
        let len = (w as usize).saturating_mul(h as usize).saturating_mul(2);
        dec.packed = bytes::slice_at_lenient(data, payload_start, len).ok().map(|d| PackedPayload {
            data: d,
            width: w,
            height: h,
            bits: 16,
            order: if order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
            row_stride: w as usize * 2,
        });
    }
    Ok(Box::new(dec))
}
