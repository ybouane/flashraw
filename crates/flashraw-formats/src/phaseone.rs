//! Phase One IIQ (incl. Leaf Credo / re-badged backs) and Leaf MOS.
//!
//! ## IIQ
//! An IIQ file is usually a TIFF (previews + EXIF) whose maker note is the Phase One
//! directory: an `IIII`/`MMMM` header with a `Raw` magic at offset 8 of the file
//! (offset 0 for bare IIQ). All offsets in the directory are relative to that header.
//! The directory gives the sensor geometry, the payload format, a per-row offset
//! table (every row is an independent bit stream — rows decode in parallel and
//! regions decode only the rows they touch), black references and calibration.
//!
//! Payload formats: 0/1/2 uncompressed 16-bit (1/2 scrambled with a key pair), 3/5/8
//! "IIQ L" (see [`flashraw_codecs::phaseone`]), 6 "IIQ S".
//!
//! samples scaled by four, no black subtraction, no sensor calibration. The vendor
//! to the raw buffer. FlashRaw reports the true sensor black (`t_black` plus the
//! reports 0 / `0xfffc - t_black` for its black-subtracted buffer.
//!
//! ## Leaf MOS
//! TIFF with a `PKTS` packet tree (tag 0x8606) holding the back type, neutrals and
//! mosaic/rotation info. Payload: one lossless-JPEG tile, 16-bit uncompressed, or
//! horizontal uncompressed bands.

use crate::common::*;
use crate::medium::{ErrorSlot, cfa_at_origin, even_margins, matrix_from, read_u16s, fallback_table};
use flashraw_codecs::phaseone::{IiqLParams, decode_l_row, decode_s_row, unscramble_pairs};
use flashraw_core::bits::{BitPumpMsb, BitPumpMsb32};
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;
use std::sync::Arc;

/// Leaf PKTS metadata tag.
pub const TAG_LEAF_PKTS: u16 = 0x8606;

/// Position of the IIQ header (`IIII`/`MMMM` followed by `?Raw`) in the first 32 bytes.
fn iiq_base(data: &[u8]) -> Option<usize> {
    let head = data.get(..32.min(data.len()))?;
    for (i, w) in head.windows(4).enumerate() {
        if w == b"IIII" || w == b"MMMM" {
            let magic = data.get(i + 4..i + 8)?;
            if &magic[1..4] == b"waR" || &magic[0..3] == b"Raw" {
                return Some(i);
            }
        }
    }
    None
}

/// Magic-signature probe: an IIQ directory header (bare or TIFF-wrapped).
pub fn probe(data: &[u8]) -> bool {
    iiq_base(data).is_some()
}

#[derive(Debug, Clone, Copy)]
struct IiqEntry {
    tag: u32,
    len: u32,
    data: u32,
    /// Absolute position of the entry's `data` field.
    data_pos: usize,
}

/// Parsed Phase One directory.
struct IiqDir<'a> {
    data: &'a [u8],
    order: ByteOrder,
    base: usize,
    entries: Vec<IiqEntry>,
}

impl<'a> IiqDir<'a> {
    fn parse(data: &'a [u8], base: usize) -> Result<Self> {
        let order = match data.get(base..base + 2) {
            Some(b"II") => ByteOrder::Little,
            Some(b"MM") => ByteOrder::Big,
            _ => return Err(Error::UnsupportedFormat("not an IIQ header".into())),
        };
        let dir = data.u32_at(order, base + 8)? as usize;
        if dir == 0x0bad_0bad {
            return Err(Error::corrupt("IIQ: directory marked invalid"));
        }
        let dpos = base.checked_add(dir).ok_or_else(|| Error::corrupt("IIQ: directory offset overflow"))?;
        let n = data.u32_at(order, dpos)? as usize;
        if n > 8192 {
            return Err(Error::corrupt(format!("IIQ: {n} directory entries")));
        }
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let p = dpos + 8 + 16 * i;
            if p + 16 > data.len() {
                break;
            }
            entries.push(IiqEntry {
                tag: data.u32_at(order, p)?,
                len: data.u32_at(order, p + 8)?,
                data: data.u32_at(order, p + 12)?,
                data_pos: p + 12,
            });
        }
        if entries.is_empty() {
            return Err(Error::corrupt("IIQ: empty directory"));
        }
        Ok(IiqDir { data, order, base, entries })
    }

    fn get(&self, tag: u32) -> Option<IiqEntry> {
        self.entries.iter().copied().find(|e| e.tag == tag)
    }
    /// Inline 32-bit value of `tag`.
    fn val(&self, tag: u32) -> Option<u32> {
        self.get(tag).map(|e| e.data)
    }
    /// Absolute offset of the data referenced by `tag`.
    fn offset(&self, tag: u32) -> Option<usize> {
        self.get(tag).and_then(|e| self.base.checked_add(e.data as usize))
    }
    /// Bytes referenced by `tag` (clamped to the file).
    fn bytes(&self, tag: u32) -> Option<&'a [u8]> {
        let e = self.get(tag)?;
        let o = self.base.checked_add(e.data as usize)?;
        let end = o.checked_add(e.len as usize)?.min(self.data.len());
        self.data.get(o..end)
    }
    fn floats(&self, tag: u32, n: usize) -> Option<Vec<f32>> {
        let b = self.bytes(tag)?;
        (0..n).map(|i| b.u32_at(self.order, i * 4).ok().map(f32::from_bits)).collect()
    }
    fn string(&self, tag: u32) -> Option<String> {
        let s = bytes::ascii_field(self.bytes(tag)?);
        (!s.is_empty()).then_some(s)
    }
}

/// Payload kinds of IIQ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IiqPayload {
    /// Formats 0/1/2: 16-bit samples, optionally key-scrambled.
    Raw { format: u32 },
    /// Formats 3/5/8 (IIQ L).
    L(IiqLParams),
    /// Format 6 (IIQ S).
    S,
}

struct IiqRaw<'a> {
    data: &'a [u8],
    order: ByteOrder,
    width: u32,
    height: u32,
    payload: IiqPayload,
    data_offset: usize,
    data_end: usize,
    /// Absolute start of every row (compressed formats).
    rows: Vec<usize>,
    /// Absolute end of every row's data (next row start in file order).
    row_ends: Vec<usize>,
    key: (u16, u16),
}

impl IiqRaw<'_> {
    fn decode_rows(&self, ctx: &DecodeContext, y0: u32, y1: u32) -> Result<RawImage> {
        let w = self.width as usize;
        let mut img = RawImage::new_u16(self.width, y1 - y0, 1, &ctx.limits)?;
        let out = img.u16_mut();
        match self.payload {
            IiqPayload::Raw { format } => {
                ctx.profiler.time(Stage::Unpack, || {
                    let start = self.data_offset.saturating_add(y0 as usize * w * 2);
                    let src = self.data.get(start..self.data_end.max(start)).unwrap_or(&[]);
                    if src.len() < w * 2 {
                        return Err(Error::corrupt("IIQ: uncompressed payload truncated"));
                    }
                    crate::medium::unpack_u16_rows(src, self.order, w, out, w * 2, ctx.parallel)?;
                    if format != 0 {
                        // Pairs run over the flat raster; a region starting on an odd sample
                        // index never happens for even widths, and odd widths are decoded
                        // from row 0 (see `region`).
                        unscramble_pairs(out, self.key.0, self.key.1, format);
                    }
                    Ok(())
                })?;
            }
            IiqPayload::L(params) => {
                let errs = ErrorSlot::default();
                ctx.profiler.time(Stage::EntropyDecode, || {
                    par::for_each_row_chunk(out, w, 4, ctx.parallel, |first, chunk| {
                        for (i, row) in chunk.chunks_mut(w).enumerate() {
                            let y = y0 as usize + first + i;
                            if let Err(e) = self.row_bytes(y).and_then(|src| match self.order {
                                ByteOrder::Little => decode_l_row(&mut BitPumpMsb32::new(src), row, params),
                                ByteOrder::Big => decode_l_row(&mut BitPumpMsb::new(src), row, params),
                            }) {
                                errs.set(e);
                                return;
                            }
                        }
                    })
                });
                errs.into_result()?;
            }
            IiqPayload::S => {
                let errs = ErrorSlot::default();
                ctx.profiler.time(Stage::EntropyDecode, || {
                    par::for_each_row_chunk(out, w, 4, ctx.parallel, |first, chunk| {
                        for (i, row) in chunk.chunks_mut(w).enumerate() {
                            let y = y0 as usize + first + i;
                            if let Err(e) =
                                self.row_bytes(y).and_then(|src| decode_s_row(&mut BitPumpMsb32::new(src), row))
                            {
                                errs.set(e);
                                return;
                            }
                        }
                    })
                });
                errs.into_result()?;
            }
        }
        Ok(img)
    }

    fn row_bytes(&self, y: usize) -> Result<&[u8]> {
        let (s, e) = (self.rows[y], self.row_ends[y]);
        if s >= self.data.len() {
            return Err(Error::corrupt_at(format!("IIQ: row {y} starts beyond end of file"), s as u64));
        }
        Ok(&self.data[s..e.clamp(s, self.data.len())])
    }

    fn region(&self, ctx: &DecodeContext, r: Rect) -> Result<RawImage> {
        let full = Rect::full(self.width, self.height);
        let r = r.intersect(&full).ok_or_else(|| Error::InvalidArgument(format!("region {r} outside image")))?;
        // Scrambled pairs straddle rows when the width is odd: decode from the top then.
        let y0 = if matches!(self.payload, IiqPayload::Raw { format } if format != 0) && self.width % 2 == 1 {
            0
        } else {
            r.y
        };
        let img = self.decode_rows(ctx, y0, r.bottom())?;
        if y0 == 0 && r.y == 0 && r.x == 0 && r.width == self.width {
            return Ok(img);
        }
        img.crop(Rect::new(r.x, r.y - y0, r.width, r.height))
    }

    fn units(&self) -> Vec<DecodeUnit> {
        if self.rows.is_empty() {
            return Vec::new();
        }
        (0..self.height as usize)
            .map(|y| DecodeUnit {
                rect: Rect::new(0, y as u32, self.width, 1),
                offset: self.rows[y] as u64,
                length: self.row_ends[y].saturating_sub(self.rows[y]) as u64,
            })
            .collect()
    }
}

/// Open a Phase One IIQ file (bare or TIFF-wrapped).
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let base = iiq_base(data).ok_or_else(|| Error::UnsupportedFormat("not an IIQ file".into()))?;
    let dir = IiqDir::parse(data, base)?;
    let tiff = if base > 0 { Tiff::parse(data).ok() } else { None };

    let mut meta = RawMetadata::default();
    if let Some(t) = &tiff {
        if let Some(ifd0) = t.ifds.first() {
            flashraw_core::exif::read_ifd0(ifd0, &mut meta);
        }
    }
    let firmware = dir.string(0x0301);
    if meta.make.is_empty() {
        meta.make = "Phase One".into();
    }
    if meta.model.is_empty() {
        // "IQ4 150MP, v7.00.7" / "P25-H, Firmware: ..." -> "IQ4 150MP" / "P25".
        if let Some(f) = &firmware {
            let mut m = f.split(" camera").next().unwrap_or(f).split(',').next().unwrap_or(f).to_string();
            if let Some(i) = m.find('-') {
                m.truncate(i);
            }
            meta.model = m.trim().to_string();
        }
    }
    // Achromatic backs (IQ3 Achromatic, P45+ Achromatic...) have no colour filter array.
    let achromatic = firmware.as_deref().is_some_and(|f| f.contains("Achromatic"));
    if let Some(f) = firmware {
        meta.firmware = Some(f);
    }
    if let Some(s) = dir.string(0x0102) {
        meta.serial = Some(s);
    }
    if let Some(t) = dir.val(0x0210) {
        meta.set_extra("sensor_temperature", f32::from_bits(t));
    }
    let cam = db_lookup(&mut meta, "");

    let w = dir.val(0x0108).ok_or_else(|| Error::invalid_metadata("IIQ: missing raw width"))?;
    let h = dir.val(0x0109).ok_or_else(|| Error::invalid_metadata("IIQ: missing raw height"))?;
    opts.limits.check_dims(w, h, 1)?;
    let format = dir.val(0x010e).unwrap_or(0);
    let payload = match format {
        0..=2 => IiqPayload::Raw { format },
        6 => IiqPayload::S,
        3 | 5 | 8 => IiqPayload::L(IiqLParams { shift: if format == 8 { 0 } else { 2 }, low_curve: format == 5 }),
        f => return Err(Error::unsupported_compression("IIQ", format!("payload format {f}"))),
    };
    let data_e = dir.get(0x010f).ok_or_else(|| Error::invalid_metadata("IIQ: missing data offset"))?;
    let data_offset = base + data_e.data as usize;
    if data_offset >= data.len() {
        return Err(Error::corrupt_at("IIQ: payload beyond end of file", data_offset as u64));
    }
    let data_end = data_offset.saturating_add(data_e.len as usize).min(data.len());

    // Row offset table for compressed formats.
    let (mut rows, mut row_ends) = (Vec::new(), Vec::new());
    if !matches!(payload, IiqPayload::Raw { .. }) {
        let so = dir.offset(0x021c).ok_or_else(|| Error::invalid_metadata("IIQ: missing row offset table"))?;
        let table = bytes::slice_at(data, so, h as usize * 4)?;
        rows = (0..h as usize)
            .map(|i| table.u32_at(dir.order, i * 4).map(|o| data_offset.saturating_add(o as usize)))
            .collect::<Result<Vec<_>>>()?;
        // Row ends: next row start in file order (rows are not necessarily stored in order).
        let mut sorted: Vec<(usize, usize)> = rows.iter().enumerate().map(|(i, &o)| (o, i)).collect();
        sorted.sort_unstable();
        row_ends = vec![0; h as usize];
        for k in 0..sorted.len() {
            let end = sorted.get(k + 1).map(|&(o, _)| o).unwrap_or(data_end.max(sorted[k].0));
            row_ends[sorted[k].1] = end.max(sorted[k].0);
        }
        // The last row's data may extend to the end of the payload (IIQ L reads past its row).
        if let Some(&(_, i)) = sorted.last() {
            row_ends[i] = data.len();
        }
        if let IiqPayload::L(_) = payload {
            // IIQ L rows are self-delimiting; let each pump see the rest of the file so
            // end-of-row look-ahead never trips over a neighbour's start.
            for e in row_ends.iter_mut() {
                *e = data.len();
            }
        }
    }
    let key = match dir.get(0x0112) {
        Some(e) => (data.u16_at(dir.order, e.data_pos)?, data.u16_at(dir.order, e.data_pos + 2)?),
        None => (0, 0),
    };

    let (lm, tm) = (dir.val(0x010a).unwrap_or(0), dir.val(0x010b).unwrap_or(0));
    let (vw, vh) = (dir.val(0x010c).unwrap_or(w), dir.val(0x010d).unwrap_or(h));
    let visible = Rect::new(lm, tm, vw, vh).clamp_to(w, h);
    let bits = if matches!(payload, IiqPayload::L(p) if p.shift == 0) || matches!(payload, IiqPayload::Raw { .. }) {
        16
    } else {
        14
    };
    let mut info = FrameInfo::new(w, h, 16);
    info.active_area = even_margins(visible);
    info.crop = info.active_area;
    if achromatic {
        info.cfa = None;
        info.layout = SensorLayout::Monochrome;
    } else {
        info.cfa = Some(cfa_at_origin(CfaPattern::RGGB, visible.x, visible.y));
        info.layout = SensorLayout::Bayer;
    }
    info.orientation = match dir.val(0x0100).map(|v| v & 3) {
        Some(1) => Orientation::from_exif(6),
        Some(2) => Orientation::from_exif(8),
        Some(3) => Orientation::from_exif(3),
        _ => Orientation::Normal,
    };
    info.compression = match payload {
        IiqPayload::Raw { .. } => Compression::Uncompressed,
        IiqPayload::L(_) => Compression::PhaseOne,
        IiqPayload::S => Compression::PhaseOneS,
    };
    info.white_level = WhiteLevel::uniform(if bits == 14 { 0xfffc } else { 0xffff });

    // Black: sensor black plus per-row (left/right halves) and per-column (top/bottom
    // halves) references, which the vendor subtracts as `black - row_ref - col_ref`.
    let t_black = dir.val(0x021d).unwrap_or(0) as f32;
    let mut black = BlackLevel::uniform(t_black);
    let split_col = dir.val(0x0222).unwrap_or(0).min(w) as usize;
    let split_row = dir.val(0x0224).unwrap_or(0).min(h) as usize;
    let halves = |n: usize, split: usize, b: &[u8]| -> Vec<f32> {
        // Average of the two halves, weighted by the number of samples each covers.
        let (a_w, b_w) = (split as f32, (n - split) as f32);
        b.chunks_exact(4)
            .map(|c| {
                let a = i16::from_ne_bytes(dir.order.u16([c[0], c[1]]).to_ne_bytes()) as f32;
                let bb = i16::from_ne_bytes(dir.order.u16([c[2], c[3]]).to_ne_bytes()) as f32;
                -(a * a_w + bb * b_w) / (a_w + b_w).max(1.0)
            })
            .collect()
    };
    if let Some(o) = dir.offset(0x0223).filter(|_| dir.val(0x0223).unwrap_or(0) != 0) {
        if let Ok(b) = bytes::slice_at(data, o, h as usize * 4) {
            black.row_deltas = halves(w as usize, split_col, b);
        }
    }
    if let Some(o) = dir.offset(0x0225).filter(|_| dir.val(0x0225).unwrap_or(0) != 0) {
        if let Ok(b) = bytes::slice_at(data, o, w as usize * 4) {
            black.col_deltas = halves(h as usize, split_row, b);
        }
    }
    if black.row_deltas.iter().all(|&v| v == 0.0) {
        black.row_deltas.clear();
    }
    if black.col_deltas.iter().all(|&v| v == 0.0) {
        black.col_deltas.clear();
    }
    info.black_level = black;
    if let Some(m) = dir.floats(0x0107, 3) {
        info.white_balance = WhiteBalance::from_multipliers(m[0], m[1], m[2], m[1]);
    }
    if let (Some(m), Some(_)) = (dir.meta_len(), dir.get(0x0110)) {
        meta.set_extra("calibration_bytes", m);
    }
    meta.set_extra("iiq_format", format);
    if let Some(t) = fallback_table(&meta.canonical_make, &meta.canonical_model) {
        info.color_matrices = matrix_from(t);
    }
    apply_db(
        &mut info,
        &meta,
        cam,
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, color: false, masked: true },
    );
    if let Some(t) = &tiff {
        meta.previews = tiff_previews(data, 0, t, info.orientation);
    }

    let raw = Arc::new(IiqRaw {
        data,
        order: dir.order,
        width: w,
        height: h,
        payload,
        data_offset,
        data_end,
        rows,
        row_ends,
        key,
    });
    let (r1, r2) = (raw.clone(), raw.clone());
    let mut dec =
        SimpleDecoder::new(RawFormat::Iiq, meta, info, Box::new(move |ctx| r1.decode_rows(ctx, 0, r1.height)));
    dec.region = Some(Box::new(move |ctx, r| r2.region(ctx, r)));
    dec.random_access = RandomAccess::Rows;
    dec.units = raw.units();
    if let IiqPayload::Raw { format: 0 } = payload {
        if let Ok(d) = bytes::slice_at_lenient(data, data_offset, w as usize * h as usize * 2) {
            dec.packed = Some(PackedPayload {
                data: d,
                width: w,
                height: h,
                bits: 16,
                order: if dir.order == ByteOrder::Little { PackOrder::LsbBytes } else { PackOrder::MsbBytes },
                row_stride: w as usize * 2,
            });
        }
    }
    Ok(Box::new(dec))
}

impl IiqDir<'_> {
    fn meta_len(&self) -> Option<u32> {
        self.get(0x0110).map(|e| e.len)
    }
}

/// TIFF-based files dispatched to this module: Leaf MOS (PKTS metadata) and Mamiya MEF.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if probe(data) {
        return open(data, opts);
    }
    if tiff.find_entry(TAG_LEAF_PKTS).is_some() {
        return open_mos(data, tiff, opts);
    }
    let make = tiff.all_ifds().into_iter().find_map(|i| i.string(tags::MAKE)).unwrap_or_default();
    if flashraw_camera_db::canonical_make(&make) == "Mamiya" {
        return crate::medium::mamiya::open_tiff(data, tiff, opts);
    }
    Err(Error::UnsupportedFormat(format!("{make}: TIFF without Phase One / Leaf raw data")))
}

/// Values extracted from a Leaf PKTS packet tree.
#[derive(Debug, Default)]
struct LeafPackets {
    model: Option<String>,
    neutrals: Option<[i32; 4]>,
    /// CFA table index (`RGGB, GRBG, BGGR, GBRG`) when a single-plane mosaic was described.
    mosaic: Option<u32>,
    /// Number of planes (last value seen).
    planes: i32,
    flip: i32,
    preview: Option<(usize, usize)>,
    /// "Rows_data" flags; bit 0: JPEG rows are interleaved (even rows top-down, odd rows bottom-up).
    rows_flags: u32,
    body: Option<String>,
    serial: Option<String>,
}

const LEAF_BACKS: [&str; 39] = [
    "",
    "DCB2",
    "Volare",
    "Cantare",
    "CMost",
    "Valeo 6",
    "Valeo 11",
    "Valeo 22",
    "Valeo 11p",
    "Valeo 17",
    "",
    "Aptus 17",
    "Aptus 22",
    "Aptus 75",
    "Aptus 65",
    "Aptus 54S",
    "Aptus 65S",
    "Aptus 75S",
    "AFi 5",
    "AFi 6",
    "AFi 7",
    "AFi-II 7",
    "Aptus-II 7",
    "",
    "Aptus-II 6",
    "AFi-II 10",
    "",
    "Aptus-II 10",
    "Aptus-II 5",
    "",
    "DM33",
    "",
    "",
    "Aptus-II 10R",
    "Aptus-II 8",
    "",
    "Aptus-II 12",
    "",
    "AFi-II 12",
];

/// Leading integers of an ASCII packet payload (`scanf("%d")`-style).
fn ints(b: &[u8], n: usize) -> Vec<i32> {
    let s = String::from_utf8_lossy(b);
    s.split(|c: char| c.is_whitespace() || c == '\0')
        .filter(|t| !t.is_empty())
        .map_while(|t| {
            let t = t.trim_end_matches(|c: char| !c.is_ascii_digit());
            t.parse::<i32>().ok()
        })
        .take(n)
        .collect()
}

fn parse_pkts(data: &[u8], mut pos: usize, depth: u32, out: &mut LeafPackets, budget: &mut u32) {
    if depth > 8 {
        return;
    }
    // CFA at the end of the level that describes it, with the rotation known *then*.
    let (mut planes, mut frot) = (0i32, 0u32);
    // Packets: "PKTS" u32(version) name[40] u32(size) payload; payloads may nest packets.
    while *budget > 0 {
        *budget -= 1;
        if data.get(pos..pos + 4) != Some(b"PKTS") {
            break;
        }
        let Some(name) = data.get(pos + 8..pos + 48) else { break };
        let Ok(size) = data.u32_at(ByteOrder::Big, pos + 48) else { break };
        let from = pos + 52;
        let end = from.saturating_add(size as usize).min(data.len());
        let payload = data.get(from..end).unwrap_or(&[]);
        let name = bytes::ascii_field(name);
        match name.as_str() {
            "ShootObj_back_type" => {
                if let Some(&i) = ints(payload, 1).first() {
                    if let Some(m) = LEAF_BACKS.get(i as usize).filter(|m| !m.is_empty()) {
                        out.model = Some(m.to_string());
                    }
                }
            }
            "NeutObj_neutrals" if out.neutrals.is_none() => {
                let v = ints(payload, 4);
                if v.len() == 4 {
                    out.neutrals = Some([v[0], v[1], v[2], v[3]]);
                }
            }
            "CaptProf_number_of_planes" => {
                planes = ints(payload, 1).first().copied().unwrap_or(0);
                out.planes = planes;
            }
            "CaptProf_raw_data_rotation" => {
                if let Some(&v) = ints(payload, 1).first() {
                    out.flip = v;
                }
            }
            "CaptProf_mosaic_pattern" => {
                for (c, &v) in ints(payload, 4).iter().enumerate() {
                    if v == 1 {
                        frot = (c ^ (c >> 1)) as u32;
                    }
                }
            }
            "ImgProf_rotation_angle" => {
                if let Some(&v) = ints(payload, 1).first() {
                    out.flip = v.wrapping_sub(out.flip);
                }
            }
            "JPEG_preview_data" => out.preview = Some((from, size as usize)),
            "Rows_data" => out.rows_flags = payload.u32_at(ByteOrder::Big, 0).unwrap_or(0),
            "CameraObj_camera_type" => {
                let s = bytes::ascii_field(payload);
                if !s.is_empty() {
                    out.body = Some(s);
                }
            }
            "back_serial_number" => {
                if let Some(s) = String::from_utf8_lossy(payload).split_whitespace().next() {
                    out.serial = Some(s.trim_matches('\0').to_string());
                }
            }
            _ => {}
        }
        parse_pkts(data, from, depth + 1, out, budget);
        pos = from.saturating_add(size as usize);
    }
    if planes != 0 {
        // `flip / 90` with C truncation; an unknown flip (-1) counts as 0.
        out.mosaic = Some(if planes == 1 { ((out.flip / 90) + frot as i32).rem_euclid(4) as u32 } else { 4 });
    }
}

fn leaf_orientation(flip: i32) -> Option<Orientation> {
    let code = if !(-89..=89).contains(&flip) {
        match (flip + 3600).rem_euclid(360) {
            270 => 5,
            180 => 3,
            90 => 6,
            _ => 0,
        }
    } else if flip == -1 {
        return None;
    } else {
        flip
    };
    Some(match code {
        3 => Orientation::from_exif(3),
        5 => Orientation::from_exif(8),
        6 => Orientation::from_exif(6),
        _ => Orientation::Normal,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MosPayload {
    Ljpeg,
    /// Uncompressed 16-bit; `band` rows per tile/strip (bands are stored separately).
    Raw16 {
        band: u32,
    },
}

/// Open a Leaf MOS file.
fn open_mos<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let pk_entry = *tiff.find_entry(TAG_LEAF_PKTS).ok_or_else(|| Error::invalid_metadata("MOS without PKTS"))?;
    let mut pk = LeafPackets { flip: -1, ..Default::default() };
    let mut budget = 4096;
    parse_pkts(data, pk_entry.value_pos, 0, &mut pk, &mut budget);

    let ifd0 = tiff.ifds.first().ok_or_else(|| Error::corrupt("MOS without IFD0"))?;
    let mut meta = RawMetadata::default();
    flashraw_core::exif::read_ifd0(ifd0, &mut meta);
    if meta.make.is_empty() || flashraw_camera_db::canonical_make(&meta.make) != "Mamiya" {
        meta.make = "Leaf".into();
    }
    if let Some(m) = &pk.model {
        meta.model = m.clone();
    }
    meta.serial = pk.serial.clone().or(meta.serial.take());
    if let Some(b) = &pk.body {
        meta.set_extra("body", b.clone());
    }
    let cam = db_lookup(&mut meta, "");

    // Raw IFD: the largest CFA/greyscale image with > 8 bits.
    let raw = tiff
        .all_ifds()
        .into_iter()
        .filter(|i| i.bps() > 8 && i.width() > 0 && i.height() > 0)
        .max_by_key(|i| i.width() as u64 * i.height() as u64)
        .ok_or_else(|| Error::invalid_metadata("MOS: no raw image"))?;
    let samples = raw.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
    if samples > 1 {
        return Err(Error::unsupported_compression("MOS", format!("multi-shot capture ({samples} planes)")));
    }
    let (w, h) = (raw.width(), raw.height());
    opts.limits.check_dims(w, h, 1)?;
    let units: Vec<(usize, usize)> = if raw.has(tags::TILE_OFFSETS) { tiles(raw) } else { strips(raw) };
    if units.is_empty() {
        return Err(Error::invalid_metadata("MOS: no data offsets"));
    }
    let band = raw.u32(tags::TILE_LENGTH).or_else(|| raw.u32(tags::ROWS_PER_STRIP)).unwrap_or(h).clamp(1, h.max(1));
    let tile_w = raw.u32(tags::TILE_WIDTH).unwrap_or(w);
    if tile_w != w && raw.has(tags::TILE_OFFSETS) {
        return Err(Error::unsupported_compression("MOS", "tiles narrower than the image"));
    }
    let payload = match raw.compression() {
        1 => MosPayload::Raw16 { band },
        99 | 7 => MosPayload::Ljpeg,
        c => return Err(Error::unsupported_compression("MOS", format!("TIFF compression {c}"))),
    };
    if raw.bps() != 16 {
        return Err(Error::unsupported_compression("MOS", format!("{}-bit samples", raw.bps())));
    }

    let mut info = FrameInfo::new(w, h, 14);
    info.compression = if payload == MosPayload::Ljpeg { Compression::LeafLjpeg } else { Compression::Uncompressed };
    let mut white = 0x3fff;
    if payload == MosPayload::Ljpeg {
        if let Some(&(o, l)) = units.first() {
            if let Ok(src) = bytes::slice_at_lenient(data, o, l) {
                if flashraw_codecs::ljpeg::LjpegHeader::parse(src, Default::default()).is_ok_and(|h| h.bits == 15) {
                    white = 0x1fff;
                }
            }
        }
    }
    info.white_level = WhiteLevel::uniform(white);
    // CFA: from the packet tree's plane count, mosaic pattern and rotation.
    let pat = match pk.mosaic {
        Some(1) => CfaPattern::GRBG,
        Some(2) => CfaPattern::BGGR,
        Some(3) => CfaPattern::GBRG,
        Some(4) => return Err(Error::unsupported_compression("MOS", format!("{}-plane capture", pk.planes))),
        _ => CfaPattern::RGGB,
    };
    info.cfa = Some(pat);
    info.orientation = leaf_orientation(pk.flip).unwrap_or_else(|| {
        Orientation::from_exif(raw.u32(tags::ORIENTATION).or(ifd0.u32(tags::ORIENTATION)).unwrap_or(1))
    });
    if let Some(n) = pk.neutrals {
        if n[1] > 0 && n[2] > 0 && n[3] > 0 {
            let m = [n[0] as f32 / n[1] as f32, n[0] as f32 / n[2] as f32, n[0] as f32 / n[3] as f32];
            info.white_balance = WhiteBalance::from_multipliers(m[0], m[1], m[2], m[1]);
        }
    }
    let full = Rect::full(w, h);
    let aa = if w == 2116 {
        Rect::new(55, 30, w - 110, h - 60)
    } else if w == 3171 {
        Rect::new(24, 24, w - 48, h - 48)
    } else if w + h == 3144 + 2060 {
        if w > h { Rect::new(32, 6, 3072, 2048) } else { Rect::new(6, 32, 2048, 3072) }
    } else {
        full
    };
    info.active_area = aa;
    info.crop = aa;
    apply_db(
        &mut info,
        &meta,
        cam,
        Known { cfa: true, crop: true, active_area: true, black: true, white: true, color: false, masked: true },
    );
    meta.previews = tiff_previews(data, 0, &tiff, info.orientation);
    if let Some((o, l)) = pk.preview {
        push_jpeg(data, o, l, info.orientation, &mut meta.previews);
        classify_previews(&mut meta.previews);
    }
    let order = tiff.order;
    let interleaved = pk.rows_flags & 1 != 0;
    let vis_h = aa.height as usize;
    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        let out = img.u16_mut();
        match payload {
            MosPayload::Ljpeg => ctx.profiler.time(Stage::EntropyDecode, || {
                let &(o, l) = units.first().ok_or_else(|| Error::corrupt("MOS: no tiles"))?;
                let src = bytes::slice_at_lenient(data, o, l)?;
                let hdr = flashraw_codecs::ljpeg::LjpegHeader::parse(src, Default::default())?;
                if !interleaved {
                    return flashraw_codecs::leaf::decode_linear(src, &hdr, out, ctx.parallel);
                }
                // Interleaved rows: decode the stream, then scatter JPEG row j to raw row
                // j/2 (even j) or vis_h - 1 - j/2 (odd j).
                let wu = w as usize;
                let mut tmp = alloc_zeroed::<u16>(out.len())?;
                flashraw_codecs::leaf::decode_linear(src, &hdr, &mut tmp, ctx.parallel)?;
                let tmp = &tmp;
                par::for_each_row_chunk_bounded(out, wu, par::memory_bound_workers(), ctx.parallel, |first, chunk| {
                    for (i, row) in chunk.chunks_mut(wu).enumerate() {
                        let r = first + i;
                        // Inverse mapping: raw row r came from JPEG row j.
                        let j = if r < vis_h.div_ceil(2) {
                            Some(2 * r)
                        } else if r < vis_h {
                            Some(2 * (vis_h - 1 - r) + 1)
                        } else {
                            None
                        };
                        if let Some(src_row) = j.and_then(|j| tmp.get(j * wu..(j + 1) * wu)) {
                            row.copy_from_slice(src_row);
                        }
                    }
                });
                Ok(())
            })?,
            MosPayload::Raw16 { band } => ctx.profiler.time(Stage::Unpack, || {
                let rows_per = band as usize * w as usize;
                if units.len() == 1 {
                    let (o, l) = units[0];
                    let src = bytes::slice_at_lenient(data, o, l.max(rows_per * 2))?;
                    return crate::medium::unpack_u16_rows(src, order, w as usize, out, w as usize * 2, ctx.parallel);
                }
                let errs = ErrorSlot::default();
                par::for_each_row_chunk_bounded(
                    out,
                    w as usize,
                    par::memory_bound_workers(),
                    ctx.parallel,
                    |first, chunk| {
                        let i = first / band as usize;
                        let Some(&(o, l)) = units.get(i) else {
                            errs.set(Error::corrupt(format!("MOS: missing band {i}")));
                            return;
                        };
                        match bytes::slice_at_lenient(data, o, l.max(chunk.len() * 2)) {
                            Ok(src) => {
                                read_u16s(src, order, chunk);
                            }
                            Err(e) => errs.set(e),
                        }
                    },
                );
                errs.into_result()
            })?,
        }
        Ok(img)
    };
    let mut dec = SimpleDecoder::new(RawFormat::Mos, meta, info, Box::new(decode));
    dec.random_access = match payload {
        MosPayload::Raw16 { band } if band < h => RandomAccess::Strips { rows: band },
        MosPayload::Raw16 { .. } => RandomAccess::Rows,
        MosPayload::Ljpeg => RandomAccess::Sequential,
    };
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal bare IIQ with an uncompressed (format 0) or IIQ L payload.
    fn build_iiq(w: u32, h: u32, format: u32, payload: &[u8], row_offsets: Option<&[u32]>) -> Vec<u8> {
        let mut f = b"IIIICwaR".to_vec();
        f.extend(0u32.to_le_bytes()); // dir offset placeholder
        let data_off = f.len() as u32;
        f.extend_from_slice(payload);
        let table_off = f.len() as u32;
        if let Some(t) = row_offsets {
            for o in t {
                f.extend(o.to_le_bytes());
            }
        }
        let dir = f.len() as u32;
        let mut entries: Vec<(u32, u32, u32, u32)> = vec![
            (0x108, 4, 4, w),
            (0x109, 4, 4, h),
            (0x10a, 4, 4, 0),
            (0x10b, 4, 4, 0),
            (0x10c, 4, 4, w),
            (0x10d, 4, 4, h),
            (0x10e, 4, 4, format),
            (0x10f, 2, payload.len() as u32, data_off),
        ];
        if row_offsets.is_some() {
            entries.push((0x21c, 4, h * 4, table_off));
        }
        f.extend((entries.len() as u32).to_le_bytes());
        f.extend(0u32.to_le_bytes());
        for (t, ty, l, d) in entries {
            for v in [t, ty, l, d] {
                f.extend(v.to_le_bytes());
            }
        }
        f[8..12].copy_from_slice(&dir.to_le_bytes());
        f
    }

    fn decode(data: &[u8]) -> Result<RawImage> {
        let d = open(data, &OpenOptions::default())?;
        let p = Profiler::default();
        d.decode(&DecodeContext::new(&p))
    }

    #[test]
    fn probe_detects_bare_and_wrapped() {
        let f = build_iiq(4, 2, 0, &[0; 16], None);
        assert!(probe(&f));
        let mut wrapped = b"II*\0\0\0\0\0".to_vec();
        wrapped.extend(&f);
        assert!(probe(&wrapped));
        assert!(!probe(b"II*\0\x08\0\0\0IIIIxxxx"));
    }

    #[test]
    fn uncompressed_round_trip_and_region() {
        let (w, h) = (6u32, 4u32);
        let samples: Vec<u16> = (0..w * h).map(|i| (i * 1000) as u16).collect();
        let payload: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let f = build_iiq(w, h, 0, &payload, None);
        let img = decode(&f).unwrap();
        assert_eq!(img.u16(), &samples[..]);
        let d = open(&f, &OpenOptions::default()).unwrap();
        let p = Profiler::default();
        let r = d.decode_region(&DecodeContext::new(&p), Rect::new(1, 2, 3, 2)).unwrap();
        assert_eq!(r.u16(), &[13000, 14000, 15000, 19000, 20000, 21000]);
    }

    #[test]
    fn compressed_rows_all_literal() {
        // IIQ L rows of width 3 (< 8): every sample is a 16-bit literal.
        let (w, h) = (3u32, 2u32);
        let rows: [[u16; 3]; 2] = [[100, 200, 300], [4000, 5000, 6000]];
        let mut payload = Vec::new();
        let mut offs = Vec::new();
        for r in rows {
            offs.push(payload.len() as u32);
            // 3 x 16 bits = 48 bits -> two 32-bit LE words
            let bits: u64 = ((r[0] as u64) << 48) | ((r[1] as u64) << 32) | ((r[2] as u64) << 16);
            payload.extend(((bits >> 32) as u32).to_le_bytes());
            payload.extend((bits as u32).to_le_bytes());
            payload.extend(0u32.to_le_bytes());
        }
        let f = build_iiq(w, h, 3, &payload, Some(&offs));
        let img = decode(&f).unwrap();
        assert_eq!(img.u16(), &[400, 800, 1200, 16000, 20000, 24000]);
    }

    #[test]
    fn malformed_inputs_error_without_panic() {
        let (w, h) = (3u32, 2u32);
        let f = build_iiq(w, h, 3, &[0u8; 24], Some(&[0, 12]));
        // Truncations at every length must never panic.
        for n in 0..f.len() {
            let _ = open(&f[..n], &OpenOptions::default()).and_then(|d| {
                let p = Profiler::default();
                d.decode(&DecodeContext::new(&p))
            });
        }
        // Row offsets beyond the file.
        let bad = build_iiq(w, h, 3, &[0u8; 24], Some(&[0, 0xFFFF_FF00]));
        assert!(decode(&bad).is_err());
        // Byte flips.
        for i in 0..f.len() {
            let mut g = f.clone();
            g[i] ^= 0xA5;
            let _ = decode(&g);
        }
    }

    #[test]
    fn leaf_flip_codes() {
        assert_eq!(leaf_orientation(90), Some(Orientation::from_exif(6)));
        assert_eq!(leaf_orientation(270), Some(Orientation::from_exif(8)));
        assert_eq!(leaf_orientation(0), Some(Orientation::Normal));
        assert_eq!(leaf_orientation(-1), None);
    }

    #[test]
    fn pkts_parsing() {
        let mut d = Vec::new();
        let pkt = |name: &str, payload: &[u8], d: &mut Vec<u8>| {
            d.extend(b"PKTS");
            d.extend(0u32.to_be_bytes());
            let mut n = name.as_bytes().to_vec();
            n.resize(40, 0);
            d.extend(n);
            d.extend((payload.len() as u32).to_be_bytes());
            d.extend(payload);
        };
        pkt("ShootObj_back_type", b"12", &mut d);
        pkt("NeutObj_neutrals", b"1000 500 800 600", &mut d);
        pkt("CaptProf_number_of_planes", b"1", &mut d);
        pkt("CaptProf_raw_data_rotation", b"90", &mut d);
        let mut pk = LeafPackets { flip: -1, ..Default::default() };
        let mut budget = 100;
        parse_pkts(&d, 0, 0, &mut pk, &mut budget);
        assert_eq!(pk.mosaic, Some(1)); // rotation known when the level ends
        assert_eq!(pk.model.as_deref(), Some("Aptus 22"));
        assert_eq!(pk.neutrals, Some([1000, 500, 800, 600]));
        assert_eq!(pk.planes, 1);
        assert_eq!(pk.flip, 90);
    }
}
