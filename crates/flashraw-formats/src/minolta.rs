//! Minolta / Konica Minolta MRW files, the DiMAGE Z2 dump and the RD-175.
//!
//! An MRW file is an 8 byte header (`\0MR` plus `M` or `I` for the byte order of the fields, then
//! the length of the block area), a sequence of tagged blocks (`PRD` sensor geometry, `WBG` white
//! balance, `TTW` an embedded TIFF with the EXIF data and thumbnail, ...), and the image rows.
//! The Z2 and the RD-175 are headerless dumps recognised by their size.

use crate::common::*;
use crate::sony::table as colour_table;
use flashraw_codecs::{PackedLayout, unpack};
use flashraw_core::bytes::ReadAt;
use flashraw_core::tiff::{Tiff, TiffOptions, parse_ifd_at, tags};
use flashraw_core::*;

/// Contents of the `PRD` block.
#[derive(Debug, Clone, Copy, Default)]
pub struct Prd {
    pub raw_height: u16,
    pub raw_width: u16,
    pub image_height: u16,
    pub image_width: u16,
    /// Bits of a stored word (12 or 16).
    pub total_bps: u8,
    /// Significant bits.
    pub active_bps: u8,
    /// Storage method: 0x52 unpacked 16-bit words, 0x59 packed 12-bit values.
    pub storage: u8,
    /// Bayer pattern id: 1 for RGGB, 4 for GBRG.
    pub bayer: u8,
}

/// Parsed MRW header blocks.
#[derive(Debug, Clone, Default)]
pub struct MrwBlocks {
    pub order: Option<ByteOrder>,
    pub prd: Option<Prd>,
    /// As-shot multipliers in (R, G, B, G2) order.
    pub wb: Option<[f32; 4]>,
    /// Absolute offset and length of the embedded TIFF (`TTW`).
    pub ttw: Option<(usize, usize)>,
    /// Absolute offset of the image data (end of the header).
    pub data_offset: usize,
}

const TAG_PRD: u32 = u32::from_be_bytes(*b"\0PRD");
const TAG_WBG: u32 = u32::from_be_bytes(*b"\0WBG");
const TAG_TTW: u32 = u32::from_be_bytes(*b"\0TTW");

const STORAGE_UNPACKED: u8 = 0x52;
const STORAGE_PACKED: u8 = 0x59;

/// The sizes and gains of PRD and WBG blocks are read at fixed offsets from the start of the block
/// body, wherever the block's declared length ends; single-byte PRD fields past the end of the file read as 0.
fn parse_prd(tail: &[u8], order: ByteOrder) -> Result<Prd> {
    let byte = |i: usize| tail.get(i).copied().unwrap_or(0);
    Ok(Prd {
        raw_height: tail.u16_at(order, 8)?,
        raw_width: tail.u16_at(order, 10)?,
        image_height: tail.u16_at(order, 12)?,
        image_width: tail.u16_at(order, 14)?,
        total_bps: byte(16),
        active_bps: byte(17),
        storage: byte(18),
        bayer: byte(23),
    })
}

/// The four gains of a WBG block, in stored order.
fn parse_wbg(tail: &[u8], order: ByteOrder) -> Result<[u16; 4]> {
    Ok([tail.u16_at(order, 4)?, tail.u16_at(order, 6)?, tail.u16_at(order, 8)?, tail.u16_at(order, 10)?])
}

/// Parse the MRW block structure that starts at `base`.
pub fn parse_blocks(data: &[u8], base: usize) -> Result<MrwBlocks> {
    let header =
        base.checked_add(8).and_then(|end| data.get(base..end)).ok_or_else(|| Error::corrupt("MRW: header cut off"))?;
    if header[..3] != *b"\0MR" {
        return Err(Error::UnsupportedFormat("not an MRW block structure".into()));
    }
    let order = if header[3] == b'I' { ByteOrder::Little } else { ByteOrder::Big };
    let area_len = header.u32_at(order, 4)? as usize;
    let data_offset = base.saturating_add(area_len).saturating_add(8);
    let mut blocks = MrwBlocks { order: Some(order), data_offset, ..Default::default() };
    let mut gains = None;

    // Blocks start while their header lies inside the block area (and before the file's last 8 bytes).
    let limit = data_offset.min(data.len().saturating_sub(8));
    let mut pos = base.saturating_add(8);
    while pos < limit {
        let Ok(tag) = data.u32be(pos) else { break };
        let Ok(len) = data.u32_at(order, pos + 4) else { break };
        let body_start = pos + 8;
        let Some(body) = body_start.checked_add(len as usize).and_then(|end| data.get(body_start..end)) else { break };
        match tag {
            TAG_PRD => blocks.prd = Some(parse_prd(&data[body_start..], order)?),
            TAG_WBG => gains = Some(parse_wbg(&data[body_start..], order)?),
            TAG_TTW => blocks.ttw = Some((body_start, body.len())),
            _ => {}
        }
        pos = body_start + body.len();
    }

    blocks.wb = gains.map(|g| {
        let g = g.map(f32::from);
        if blocks.prd.is_some_and(|p| p.bayer == 4) { [g[2], g[3], g[1], g[0]] } else { [g[0], g[1], g[3], g[2]] }
    });
    Ok(blocks)
}

/// DiMAGE Z2 dumps: 2288x1710 12-bit samples in two interleaved fields.
const Z2_SIZE: usize = 5_869_568;
const Z2_WIDTH: usize = 2288;
const Z2_HEIGHT: usize = 1710;
/// Bytes of one stored Z2 row (858 words of 32 bits) and rows per field.
const Z2_ROW_BYTES: usize = Z2_WIDTH * 12 / 8;
const Z2_FIELD_ROWS: usize = Z2_HEIGHT / 2;

/// RD-175 dumps: three CCD readouts of 768 bytes per line.
const RD175_SIZE: usize = 1_138_688;
const RD175_WIDTH: usize = 1534;
const RD175_HEIGHT: usize = 986;
const RD175_HEADER: usize = 513;
const RD175_LINE: usize = 768;
const RD175_LINES: usize = 1481;

fn starts_with_tiff_byte_order(data: &[u8]) -> bool {
    data.starts_with(b"II") || data.starts_with(b"MM")
}

fn is_rd175(data: &[u8]) -> bool {
    data.len() == RD175_SIZE && !starts_with_tiff_byte_order(data) && !data.starts_with(b"\0MR")
}

/// Z2 dumps share their size with other cameras' zero-padded dumps; theirs end in zeros.
fn is_z2(data: &[u8]) -> bool {
    data.len() == Z2_SIZE
        && !starts_with_tiff_byte_order(data)
        && data[data.len() - 424..].iter().filter(|&&b| b != 0).count() > 20
}

/// MRW signature, or an RD-175 / DiMAGE Z2 dump identified by its size.
pub fn probe(data: &[u8]) -> bool {
    data.starts_with(b"\0MRM") || is_rd175(data) || is_z2(data)
}

/// Open an MRW file or one of the two header-less dumps.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if is_rd175(data) {
        open_rd175(data, opts)
    } else if is_z2(data) {
        open_z2(data, opts)
    } else {
        open_mrw(data, opts)
    }
}

/// Minolta-branded TIFF files carry no raw data of their own.
pub fn open_tiff<'a>(_data: &'a [u8], _tiff: Tiff<'a>, _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("Minolta TIFF without MRW raw data".into()))
}

// ---------------------------------------------------------------------------------------------
// MRW files
// ---------------------------------------------------------------------------------------------

/// Regional names of the 5D and 7D share the colour table entry of the Japanese model name.
fn table_model(model: &str) -> &str {
    const SAME_AS: [(&str, &[&str]); 2] = [
        ("DG-5D", &["DYNAX 5D", "MAXXUM 5D", "ALPHA-5 DIGITAL", "ALPHA SWEET DIGITAL"]),
        ("DG-7D", &["DYNAX 7D", "MAXXUM 7D", "ALPHA-7 DIGITAL"]),
    ];
    SAME_AS
        .iter()
        .find(|(_, aliases)| aliases.iter().any(|a| a.eq_ignore_ascii_case(model)))
        .map_or(model, |(canonical, _)| canonical)
}

/// Black and white level from the colour table entry of `model` (white defaults to `2^bits - 1`).
fn levels_for(entry: Option<&flashraw_camera_db::FallbackMatrix>, bits: u8) -> (BlackLevel, WhiteLevel) {
    let (mut black, mut cblack, mut white) = (0u32, [0u32; 4], None);
    colour_table::apply_levels(entry, &mut black, &mut cblack, &mut white);
    (BlackLevel::uniform(black as f32), WhiteLevel::uniform(white.unwrap_or((1u32 << bits) - 1)))
}

/// Previews found in the embedded TIFF: its directories and the thumbnail of the Minolta maker note.
fn embedded_previews(data: &[u8], base: usize, tiff: &Tiff, orientation: Orientation) -> Vec<PreviewInfo> {
    let mut previews = tiff_previews(data, base, tiff, orientation);
    let note = tiff.find_entry(tags::MAKER_NOTE).and_then(|e| {
        let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
        parse_ifd_at(tiff.data, tiff.order, u32::try_from(e.value_pos).ok()?, opts).ok()
    });
    if let Some(note) = note {
        const THUMBNAIL_INLINE: u16 = 0x0081;
        const THUMBNAIL_OFFSET: u16 = 0x0088;
        const THUMBNAIL_LENGTH: u16 = 0x0089;
        const UNDEFINED: u16 = 7;
        if let Some(e) = note.get(THUMBNAIL_INLINE).filter(|e| e.typ == UNDEFINED) {
            push_jpeg(data, base.saturating_add(e.value_pos), e.data.len(), orientation, &mut previews);
        }
        if let (Some(offset), Some(length)) = (note.u32(THUMBNAIL_OFFSET), note.u32(THUMBNAIL_LENGTH)) {
            push_jpeg(data, base.saturating_add(offset as usize), length as usize, orientation, &mut previews);
        }
    }
    classify_previews(&mut previews);
    previews
}

fn open_mrw<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let blocks = parse_blocks(data, 0)?;
    let prd = blocks.prd.ok_or_else(|| Error::invalid_metadata("MRW without PRD block"))?;
    let order = blocks.order.unwrap_or(ByteOrder::Big);

    // The embedded TIFF supplies make, model, exposure data and a thumbnail.
    let mut meta = RawMetadata::default();
    let mut orientation = Orientation::Normal;
    let embedded = blocks
        .ttw
        .and_then(|(o, l)| Some((o, data.get(o..o.checked_add(l)?)?)))
        .and_then(|(o, bytes)| Some((o, Tiff::parse(bytes).ok()?)));
    if let Some((_, t)) = &embedded {
        for ifd in &t.ifds {
            flashraw_core::exif::read_ifd0(ifd, &mut meta);
        }
        orientation = meta.orientation;
    }

    let (w, h) = (u32::from(prd.raw_width), u32::from(prd.raw_height));
    meta.set_image_size(u32::from(prd.image_width), u32::from(prd.image_height));
    opts.limits.check_dims(w, h, 1)?;
    let packed = match prd.storage {
        STORAGE_PACKED => true,
        STORAGE_UNPACKED => false,
        s => return Err(Error::unsupported_compression("MRW", format!("storage method {s:#04x}"))),
    };
    let bits = if prd.active_bps != 0 && prd.total_bps != 0 { prd.active_bps } else { 12 };
    if !(8..=16).contains(&bits) {
        return Err(Error::invalid_metadata(format!("MRW: unsupported bit depth {bits}")));
    }
    db_lookup(&mut meta, if packed { "packed" } else { "unpacked" });

    let entry = colour_table::lookup("Minolta", table_model(&meta.model));
    let (black_level, white_level) = levels_for(entry, bits);
    let mut info = FrameInfo::new(w, h, bits);
    info.compression = if packed { Compression::MinoltaPacked } else { Compression::Uncompressed };
    info.cfa = Some(if prd.bayer == 4 { CfaPattern::GBRG } else { CfaPattern::RGGB });
    info.layout = SensorLayout::Bayer;
    info.black_level = black_level;
    info.white_level = white_level;
    info.white_balance = blocks.wb.and_then(|m| WhiteBalance::from_multipliers(m[0], m[1], m[2], m[3]));
    info.orientation = orientation;
    info.color_matrices = colour_table::color(entry, &meta);
    if let Some((base, t)) = &embedded {
        meta.previews = embedded_previews(data, *base, t, orientation);
    }

    // Stored rows: 12-bit values most significant bit first, or 16-bit words in the file's byte order.
    let (stored_bits, pack_order) = match (packed, order) {
        (true, _) => (12u32, PackOrder::MsbBytes),
        (false, ByteOrder::Big) => (16, PackOrder::MsbBytes),
        (false, ByteOrder::Little) => (16, PackOrder::LsbBytes),
    };
    let (wu, hu) = (w as usize, h as usize);
    let row_bytes = (wu * stored_bits as usize).div_ceil(8);
    let offset = blocks.data_offset;
    let payload = hu.checked_mul(row_bytes).and_then(|n| offset.checked_add(n)).and_then(|end| data.get(offset..end));

    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let src = ctx.sensor_rows(data, DataUnit::Payload, offset, RowGeometry::tight(hu, wu, stored_bits))?;
        let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
        let layout =
            PackedLayout { width: wu, height: hu, bits: stored_bits, order: pack_order, row_stride: row_bytes };
        ctx.profiler
            .time(Stage::Unpack, || unpack(src, layout, img.u16_mut(), wu, ctx.parallel, ctx.simd, ctx.recover))?;
        Ok(img)
    };
    let mut decoder = SimpleDecoder::new(RawFormat::Mrw, meta, info, Box::new(decode));
    decoder.random_access = RandomAccess::Rows;
    decoder.packed = payload.map(|data| PackedPayload {
        data,
        width: w,
        height: h,
        bits: stored_bits,
        order: pack_order,
        row_stride: row_bytes,
    });
    Ok(Box::new(decoder))
}

// ---------------------------------------------------------------------------------------------
// DiMAGE Z2
// ---------------------------------------------------------------------------------------------

/// Eight 12-bit samples from three 32-bit words (read little-endian, bits consumed from the top).
#[inline(always)]
fn z2_group(a: u32, b: u32, c: u32) -> [u16; 8] {
    [
        (a >> 20) as u16,
        ((a >> 8) & 0xFFF) as u16,
        (((a & 0xFF) << 4) | (b >> 28)) as u16,
        ((b >> 16) & 0xFFF) as u16,
        ((b >> 4) & 0xFFF) as u16,
        (((b & 0xF) << 8) | (c >> 24)) as u16,
        ((c >> 12) & 0xFFF) as u16,
        (c & 0xFFF) as u16,
    ]
}

/// Unpack one stored Z2 row of `Z2_ROW_BYTES` bytes into `out`.
fn z2_row(stored: &[u8], out: &mut [u16]) {
    let word = |c: &[u8], i: usize| u32::from_le_bytes([c[4 * i], c[4 * i + 1], c[4 * i + 2], c[4 * i + 3]]);
    for (g, o) in stored.chunks_exact(12).zip(out.chunks_exact_mut(8)) {
        o.copy_from_slice(&z2_group(word(g, 0), word(g, 1), word(g, 2)));
    }
}

fn open_z2<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    opts.limits.check_dims(Z2_WIDTH as u32, Z2_HEIGHT as u32, 1)?;
    let mut meta = RawMetadata::new("Minolta", "DiMAGE Z2");
    db_lookup(&mut meta, "z2-headerless");
    let entry = colour_table::lookup("Minolta", "DiMAGE Z2");
    let (black_level, white_level) = levels_for(entry, 12);
    let mut info = FrameInfo::new(Z2_WIDTH as u32, Z2_HEIGHT as u32, 12);
    info.compression = Compression::MinoltaPacked;
    info.cfa = Some(CfaPattern::BGGR);
    info.black_level = black_level;
    info.white_level = white_level;
    info.color_matrices = colour_table::color(entry, &meta);

    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        // The odd rows form a second field that starts halfway through the file.
        let second_field = (Z2_SIZE >> 3) << 2;
        let need = |start: usize| start + Z2_ROW_BYTES * Z2_FIELD_ROWS;
        if data.len() < need(0).max(need(second_field)) {
            return Err(Error::corrupt("DiMAGE Z2 data is shorter than its two fields"));
        }
        let mut img = RawImage::new_u16(Z2_WIDTH as u32, Z2_HEIGHT as u32, 1, &ctx.limits)?;
        ctx.profiler.time(Stage::Unpack, || {
            par::for_each_row_chunk(img.u16_mut(), 2 * Z2_WIDTH, 16, ctx.parallel, |first, chunk| {
                for (i, pair) in chunk.chunks_exact_mut(2 * Z2_WIDTH).enumerate() {
                    let n = first + i;
                    let (even, odd) = pair.split_at_mut(Z2_WIDTH);
                    z2_row(&data[n * Z2_ROW_BYTES..], even);
                    z2_row(&data[second_field + n * Z2_ROW_BYTES..], odd);
                }
            });
        });
        Ok(img)
    };
    Ok(Box::new(SimpleDecoder::new(RawFormat::Mrw, meta, info, Box::new(decode))))
}

// ---------------------------------------------------------------------------------------------
// RD-175
// ---------------------------------------------------------------------------------------------

/// Where one stored line of the RD-175 goes in the mosaic.
#[derive(Clone, Copy)]
enum Placement {
    /// Every second sample of row `row`, starting at `row % 2`, from the doubled line.
    Even { row: usize },
    /// Two interleaved rows (`row` and `row - 1`, `row` odd), with interpolated samples.
    Quincunx { row: usize },
}

/// The placement of stored line `line` (0-based), if it is used.
fn rd175_placement(line: usize) -> Option<Placement> {
    let box_index = line / 82;
    let k = line % 82;
    Some(match line {
        0..=1475 if box_index >= 12 => Placement::Even { row: 12 * k + 2 * (box_index - 12) },
        0..=1475 if box_index % 2 == 0 => Placement::Even { row: 12 * k + (box_index | 1) },
        0..=1475 => Placement::Quincunx { row: 12 * k + box_index },
        1476 => Placement::Even { row: 984 },
        1478 => Placement::Quincunx { row: 985 },
        1480 => Placement::Even { row: 985 },
        _ => return None,
    })
}

fn rd175_place(px: &[u8], placement: Placement, out: &mut [u16]) {
    let w = RD175_WIDTH;
    let double = |i: usize| 2 * u16::from(px[i]);
    match placement {
        Placement::Even { row } => {
            for x in (row % 2..w).step_by(2) {
                out[row * w + x] = double(x / 2);
            }
        }
        Placement::Quincunx { row } => {
            let neighbours = |i: usize| {
                let left = if i > 0 { u16::from(px[i - 1]) } else { 0 };
                let right = px.get(i + 1).map_or(0, |&v| u16::from(v));
                left + right
            };
            for x in (0..w - 1).filter(|&x| x != 1) {
                let target = if x % 2 == 0 { row } else { row - 1 };
                out[target * w + x] = if (x + 1) & 2 != 0 { neighbours(x / 2) } else { double(x / 2) };
            }
            out[(row - 1) * w + 1] = double(1);
            out[(row - 1) * w + w - 1] = double(765);
        }
    }
}

fn open_rd175<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    opts.limits.check_dims(RD175_WIDTH as u32, RD175_HEIGHT as u32, 1)?;
    let mut meta = RawMetadata::new("Minolta", "RD175");
    db_lookup(&mut meta, "rd175");
    let mut info = FrameInfo::new(RD175_WIDTH as u32, RD175_HEIGHT as u32, 9);
    info.compression = Compression::Uncompressed;
    info.cfa = Some(CfaPattern::GRBG);
    // 8-bit samples, doubled by the placement: 2 * 255.
    info.white_level = WhiteLevel::uniform(0x1FE);
    info.color_matrices = db_color(&meta, None);

    let decode = move |ctx: &DecodeContext| -> Result<RawImage> {
        let lines = data
            .get(RD175_HEADER..RD175_HEADER + RD175_LINES * RD175_LINE)
            .ok_or_else(|| Error::corrupt("RD-175 data is shorter than its lines"))?;
        let mut img = RawImage::new_u16(RD175_WIDTH as u32, RD175_HEIGHT as u32, 1, &ctx.limits)?;
        ctx.profiler.time(Stage::Unpack, || {
            for (line, px) in lines.chunks_exact(RD175_LINE).enumerate() {
                if let Some(p) = rd175_placement(line) {
                    rd175_place(px, p, img.u16_mut());
                }
            }
        });
        Ok(img)
    };
    Ok(Box::new(SimpleDecoder::new(RawFormat::Other, meta, info, Box::new(decode))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{self, Lcg, Tag, TiffWriter, ascii, short};

    type Block = ([u8; 4], Vec<u8>);

    fn put16(order: ByteOrder, v: u16) -> [u8; 2] {
        if order == ByteOrder::Big { v.to_be_bytes() } else { v.to_le_bytes() }
    }

    fn put32(order: ByteOrder, v: u32) -> [u8; 4] {
        if order == ByteOrder::Big { v.to_be_bytes() } else { v.to_le_bytes() }
    }

    fn prd_block(order: ByteOrder, (w, h): (u16, u16), storage: u8, bits: (u8, u8), bayer: u8) -> Block {
        let mut body = b"21810002".to_vec();
        for v in [h, w, h, w] {
            body.extend(put16(order, v));
        }
        body.extend([bits.0, bits.1, storage, 0, 0, 0, 0, bayer]);
        (*b"\0PRD", body)
    }

    fn wbg_block(order: ByteOrder, gains: [u16; 4]) -> Block {
        let mut body = vec![2u8; 4];
        gains.iter().for_each(|&g| body.extend(put16(order, g)));
        (*b"\0WBG", body)
    }

    /// The file: header, the blocks in the given order, the payload.
    fn mrw_file(order: ByteOrder, blocks: &[Block], payload: &[u8]) -> Vec<u8> {
        let mut area = Vec::new();
        for (tag, body) in blocks {
            area.extend(tag);
            area.extend(put32(order, body.len() as u32));
            area.extend(body);
        }
        let mut file = vec![0, b'M', b'R', if order == ByteOrder::Big { b'M' } else { b'I' }];
        file.extend(put32(order, area.len() as u32));
        file.extend(area);
        file.extend(payload);
        file
    }

    /// A file with PRD and WBG blocks only.
    fn mrw(order: ByteOrder, (w, h): (u16, u16), storage: u8, bayer: u8, gains: [u16; 4], payload: &[u8]) -> Vec<u8> {
        let bits = if storage == STORAGE_PACKED { (12, 12) } else { (16, 12) };
        let blocks = [prd_block(order, (w, h), storage, bits, bayer), wbg_block(order, gains)];
        mrw_file(order, &blocks, payload)
    }

    fn decode(data: &[u8], recover: bool) -> (Result<RawImage>, Vec<DecodeWarning>) {
        let dec = open(data, &OpenOptions::default()).unwrap();
        let profiler = Profiler::default();
        let mut ctx = DecodeContext::new(&profiler);
        ctx.recover = recover;
        let img = dec.decode(&ctx);
        (img, ctx.take_warnings())
    }

    fn samples(n: usize) -> Vec<u16> {
        (0..n).map(|i| ((i * 37 + 5) % 4096) as u16).collect()
    }

    fn pack12(s: &[u16]) -> Vec<u8> {
        s.chunks(2)
            .flat_map(|p| [(p[0] >> 4) as u8, ((p[0] & 15) << 4) as u8 | (p[1] >> 8) as u8, p[1] as u8])
            .collect()
    }

    #[test]
    fn round_trips() {
        let (w, h) = (24usize, 22usize);
        let s = samples(w * h);
        let size = (w as u16, h as u16);
        let be: Vec<u8> = s.iter().flat_map(|v| v.to_be_bytes()).collect();
        let file = mrw(ByteOrder::Big, size, STORAGE_UNPACKED, 1, [400, 256, 256, 500], &be);
        let dec = open(&file, &OpenOptions::default()).unwrap();
        let wb = dec.frame_info().white_balance.unwrap();
        assert_eq!((wb.coeffs[0], wb.coeffs[2]), (400.0 / 256.0, 500.0 / 256.0));
        assert_eq!(dec.frame_info().cfa, Some(CfaPattern::RGGB));
        assert_eq!(decode(&file, true).0.unwrap().u16(), &s[..]);

        let le: Vec<u8> = s.iter().flat_map(|v| v.to_le_bytes()).collect();
        let file = mrw(ByteOrder::Little, size, STORAGE_UNPACKED, 1, [1, 2, 3, 4], &le);
        assert_eq!(decode(&file, true).0.unwrap().u16(), &s[..]);

        let file = mrw(ByteOrder::Big, size, STORAGE_PACKED, 4, [400, 256, 256, 500], &pack12(&s));
        let dec = open(&file, &OpenOptions::default()).unwrap();
        assert_eq!(dec.frame_info().cfa, Some(CfaPattern::GBRG));
        // Pattern id 4 stores the gains as (G2, B, R, G).
        assert_eq!(dec.frame_info().white_balance.unwrap().coeffs[0], 256.0 / 500.0);
        assert!(dec.packed_payload().is_some());
        assert_eq!(decode(&file, true).0.unwrap().u16(), &s[..]);
    }

    #[test]
    fn truncated_payload() {
        let (w, h) = (24usize, 22usize);
        let s = samples(w * h);
        let packed = pack12(&s);
        let file = mrw(ByteOrder::Big, (24, 22), STORAGE_PACKED, 1, [1, 1, 1, 1], &packed[..packed.len() - 100]);
        assert!(open(&file, &OpenOptions::default()).unwrap().packed_payload().is_none());
        let (img, warnings) = decode(&file, true);
        let img = img.unwrap();
        let n = w * h - 67;
        assert_eq!(&img.u16()[..n], &s[..n]);
        assert!(img.u16()[n..].iter().all(|&v| v == 0));
        assert_eq!((warnings.len(), warnings[0].samples_filled), (1, 67));
        let err = decode(&file, false).0.unwrap_err();
        let t = err.truncation().unwrap();
        assert_eq!(t.expected - t.available, 100);
    }

    #[test]
    fn garbage_never_panics() {
        let mut rng = Lcg(17);
        for len in 0..64 {
            let mut f = b"\0MRM".to_vec();
            f.extend(rng.bytes(len));
            let _ = parse_blocks(&f, 0);
            let _ = open(&f, &OpenOptions::default());
        }
        let mut f = b"\0MRM\0\0\0\x20\0PRD\xFF\xFF\xFF\xFF".to_vec();
        f.resize(64, 0);
        assert!(open(&f, &OpenOptions::default()).is_err());
        assert!(parse_blocks(&f, usize::MAX).is_err());
        assert!(parse_blocks(b"II*\0....", 0).is_err());
        // Random damage of a valid file.
        let s = samples(40 * 30);
        let base = mrw(ByteOrder::Big, (40, 30), STORAGE_PACKED, 1, [1, 2, 3, 4], &pack12(&s));
        for round in 0..2000 {
            let mut f = base.clone();
            for _ in 0..1 + round % 4 {
                let at = rng.below(f.len() as u64) as usize;
                f[at] = rng.next() as u8;
            }
            f.truncate(f.len() - rng.below(50) as usize * (round % 2));
            let _ = parse_blocks(&f, 0);
            if let Ok(d) = open(&f, &OpenOptions::default()) {
                for recover in [true, false] {
                    let profiler = Profiler::default();
                    let mut ctx = DecodeContext::new(&profiler);
                    ctx.recover = recover;
                    let _ = d.decode(&ctx);
                }
            }
        }
    }

    #[test]
    fn model_aliases() {
        assert_eq!(table_model("Alpha Sweet Digital"), "DG-5D");
        assert_eq!(table_model("maxxum 7d"), "DG-7D");
        assert_eq!(table_model("DiMAGE A2"), "DiMAGE A2");
    }

    #[test]
    fn blocks_in_any_order_give_the_same_result() {
        let order = ByteOrder::Big;
        let s = samples(8 * 6);
        let blocks = [
            prd_block(order, (8, 6), STORAGE_PACKED, (12, 12), 1),
            wbg_block(order, [10, 20, 30, 40]),
            (*b"\0TTW", b"MM\0*\0\0\0\x08\0\0\0\0\0\0".to_vec()),
            (*b"\0RIF", vec![7; 33]),
            (*b"\0PAD", vec![0; 9]),
        ];
        let reference = mrw_file(order, &blocks, &pack12(&s));
        let want = parse_blocks(&reference, 0).unwrap();
        let want_info = format!("{:?}", open(&reference, &OpenOptions::default()).unwrap().frame_info());
        let mut permutation = [0usize, 1, 2, 3, 4];
        let mut rng = Lcg(3);
        for _ in 0..60 {
            for i in (1..permutation.len()).rev() {
                permutation.swap(i, rng.below(i as u64 + 1) as usize);
            }
            let shuffled: Vec<Block> = permutation.iter().map(|&i| blocks[i].clone()).collect();
            let file = mrw_file(order, &shuffled, &pack12(&s));
            let got = parse_blocks(&file, 0).unwrap();
            assert_eq!((got.prd.map(|p| p.raw_width), got.wb), (want.prd.map(|p| p.raw_width), want.wb));
            let (o, l) = got.ttw.unwrap();
            assert_eq!(&file[o..o + l], &blocks[2].1[..]);
            assert_eq!(got.data_offset, file.len() - pack12(&s).len());
            let info = format!("{:?}", open(&file, &OpenOptions::default()).unwrap().frame_info());
            assert_eq!(info, want_info);
        }
    }

    #[test]
    fn block_errors_by_kind() {
        let order = ByteOrder::Big;
        let kind = |blocks: &[Block]| {
            open(&mrw_file(order, blocks, &[0; 200]), &OpenOptions::default()).err().map(|e| e.kind())
        };
        let wbg = wbg_block(order, [1, 1, 1, 1]);
        assert_eq!(kind(std::slice::from_ref(&wbg)), Some("InvalidMetadata"));
        assert_eq!(kind(&[prd_block(order, (8, 6), 0x33, (12, 12), 1), wbg.clone()]), Some("UnsupportedCompression"));
        assert_eq!(kind(&[prd_block(order, (8, 6), STORAGE_PACKED, (12, 7), 1), wbg.clone()]), Some("InvalidMetadata"));
        assert_eq!(
            kind(&[prd_block(order, (8, 6), STORAGE_PACKED, (12, 17), 1), wbg.clone()]),
            Some("InvalidMetadata")
        );
        assert_eq!(
            kind(&[prd_block(order, (0, 6), STORAGE_PACKED, (12, 12), 1), wbg.clone()]),
            Some("InvalidMetadata")
        );
        // No bit depth recorded: 12 bits.
        let file = mrw_file(order, &[prd_block(order, (8, 6), STORAGE_PACKED, (0, 0), 1)], &[0; 72]);
        assert_eq!(open(&file, &OpenOptions::default()).unwrap().frame_info().bits_per_sample, 12);
        let too_big = prd_block(order, (65535, 65535), STORAGE_PACKED, (12, 12), 1);
        let limits = OpenOptions { limits: Limits::default().with_max_samples(1000) };
        assert_eq!(open(&mrw_file(order, &[too_big], &[]), &limits).err().map(|e| e.kind()), Some("LimitExceeded"));
        // A plain TIFF is not an MRW structure, and the offset of a structure may follow other data.
        assert!(matches!(parse_blocks(b"II*\0\x08\0\0\0\0\0\0\0", 0), Err(Error::UnsupportedFormat(_))));
        let file = mrw(order, (8, 6), STORAGE_PACKED, 1, [1, 1, 1, 1], &[0; 72]);
        let mut shifted = vec![0xEE; 13];
        shifted.extend(&file);
        let at = parse_blocks(&shifted, 13).unwrap();
        let direct = parse_blocks(&file, 0).unwrap();
        assert_eq!(
            (at.prd.map(|p| p.raw_height), at.wb, at.data_offset),
            (direct.prd.map(|p| p.raw_height), direct.wb, direct.data_offset + 13)
        );
    }

    #[test]
    fn little_endian_structures_and_later_blocks_replace_earlier_ones() {
        let order = ByteOrder::Little;
        let blocks = [
            prd_block(order, (8, 6), STORAGE_PACKED, (12, 12), 1),
            wbg_block(order, [1, 2, 3, 4]),
            wbg_block(order, [5, 6, 7, 8]),
        ];
        let b = parse_blocks(&mrw_file(order, &blocks, &[]), 0).unwrap();
        assert_eq!(b.order, Some(ByteOrder::Little));
        assert_eq!(b.wb, Some([5.0, 6.0, 8.0, 7.0]));
        assert_eq!(b.prd.map(|p| (p.raw_width, p.raw_height)), Some((8, 6)));
    }

    /// The embedded TIFF of a file: make and model, orientation, and a maker note whose
    /// directory points at a JPEG that follows the TIFF inside the block.
    fn ttw_block() -> Block {
        let build = |jpeg_at: u32| {
            let note = {
                let mut ifd = 2u16.to_le_bytes().to_vec();
                for (tag, v) in [(0x0088u16, jpeg_at), (0x0089, JPEG.len() as u32)] {
                    ifd.extend(tag.to_le_bytes());
                    ifd.extend(4u16.to_le_bytes());
                    ifd.extend(1u32.to_le_bytes());
                    ifd.extend(v.to_le_bytes());
                }
                ifd.extend(0u32.to_le_bytes());
                ifd
            };
            let mut t = TiffWriter::new();
            let tags = vec![
                ascii(0x010F, "Konica Minolta Camera, Inc."),
                ascii(0x0110, "DYNAX 7D"),
                short(0x0112, &[6]),
                Tag { tag: 0x927C, typ: 7, count: note.len() as u32, value: note },
            ];
            let ifd0 = t.ifd(tags);
            t.finish(ifd0)
        };
        let len = build(0).len();
        let mut body = build(len as u32);
        body.extend(JPEG);
        (*b"\0TTW", body)
    }

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xD9, 1, 2, 3, 4];

    #[test]
    fn embedded_tiff_supplies_metadata_and_thumbnail() {
        let order = ByteOrder::Big;
        let s = samples(8 * 6);
        let blocks =
            [prd_block(order, (8, 6), STORAGE_PACKED, (12, 12), 1), ttw_block(), wbg_block(order, [10, 20, 30, 40])];
        let file = mrw_file(order, &blocks, &pack12(&s));
        let b = parse_blocks(&file, 0).unwrap();
        let (o, l) = b.ttw.unwrap();
        assert_eq!(&file[o..o + l], &blocks[1].1[..]);
        let d = open(&file, &OpenOptions::default()).unwrap();
        let m = d.metadata();
        assert_eq!((m.make.as_str(), m.model.as_str()), ("Konica Minolta Camera, Inc.", "DYNAX 7D"));
        assert_eq!(d.frame_info().orientation, Orientation::Rotate90);
        assert_eq!(m.extra("mode"), Some("packed"));
        let jpeg_at = (o + l - JPEG.len()) as u64;
        let found: Vec<(u64, u64)> = m.previews.iter().map(|p| (p.offset, p.length)).collect();
        assert_eq!(found, [(jpeg_at, JPEG.len() as u64)]);
        // The table entry of the 7D supplies the colour matrix whatever the regional name.
        assert!(!d.frame_info().color_matrices.color.is_empty());
    }

    /// Encode `samples` (rows of 2288) as the Z2 dump: two fields of 32-bit little-endian words
    /// whose bits are consumed from the top.
    fn z2_dump(samples: &[u16], tail: u8) -> Vec<u8> {
        let mut file = vec![tail; Z2_SIZE];
        let second_field = (Z2_SIZE >> 3) << 2;
        for (y, row) in samples.chunks(Z2_WIDTH).enumerate() {
            let start = if y % 2 == 0 { 0 } else { second_field } + (y / 2) * Z2_ROW_BYTES;
            let mut bits = 0u64;
            let mut count = 0;
            let mut words = Vec::new();
            for &v in row {
                bits = (bits << 12) | u64::from(v);
                count += 12;
                if count >= 32 {
                    words.push((bits >> (count - 32)) as u32);
                    count -= 32;
                    bits &= (1u64 << count) - 1;
                }
            }
            for (i, w) in words.iter().enumerate() {
                file[start + 4 * i..start + 4 * i + 4].copy_from_slice(&w.to_le_bytes());
            }
        }
        file
    }

    #[test]
    fn z2_fields_interleave() {
        let mut rng = Lcg(21);
        let s: Vec<u16> = (0..Z2_WIDTH * Z2_HEIGHT).map(|_| (rng.next() & 0xFFF) as u16).collect();
        let file = z2_dump(&s, 0xA5);
        assert!(probe(&file));
        let d = open(&file, &OpenOptions::default()).unwrap();
        let info = d.frame_info();
        assert_eq!((info.width, info.height, info.cfa), (2288, 1710, Some(CfaPattern::BGGR)));
        assert_eq!(d.metadata().model, "DiMAGE Z2");
        for parallel in [false, true] {
            let profiler = Profiler::default();
            let mut ctx = DecodeContext::new(&profiler);
            ctx.parallel = parallel;
            assert!(d.decode(&ctx).unwrap().u16() == &s[..], "parallel {parallel}");
        }
        // The same size with a zero padded tail is another camera's dump; one that is
        // a TIFF or a plain zero file is not a Z2 either.
        assert!(!probe(&z2_dump(&s, 0)));
        let mut tiff = file.clone();
        tiff[..2].copy_from_slice(b"II");
        assert!(!probe(&tiff));
    }

    #[test]
    fn rd175_lines_land_where_the_format_puts_them() {
        let mut rng = Lcg(8);
        let mut file = rng.bytes(RD175_SIZE);
        file[0] = 0x01;
        assert!(probe(&file));
        let d = open(&file, &OpenOptions::default()).unwrap();
        assert_eq!(
            (d.frame_info().width, d.frame_info().height, d.frame_info().white_level.values[0]),
            (1534, 986, 510)
        );
        let img = decode(&file, true).0.unwrap();
        let out = img.u16();
        let px = |line: usize, i: usize| u16::from(file[RD175_HEADER + line * RD175_LINE + i]);
        let at = |row: usize, x: usize| out[row * RD175_WIDTH + x];
        // Line 0: box 0, row 1, every second sample from column 1.
        assert_eq!(at(1, 1), 2 * px(0, 0));
        assert_eq!(at(1, 3), 2 * px(0, 1));
        // Line 82: box 1, row 1 and row 0 interleaved, with interpolated columns.
        assert_eq!(at(1, 0), 2 * px(82, 0));
        assert_eq!(at(0, 3), 2 * px(82, 1));
        assert_eq!(at(1, 2), px(82, 0) + px(82, 2));
        assert_eq!(at(0, 5), px(82, 1) + px(82, 3));
        assert_eq!((at(0, 1), at(0, 1533)), (2 * px(82, 1), 2 * px(82, 765)));
        // Line 1476: box 18, row 984.
        assert_eq!(at(984, 100), 2 * px(1476, 50));
        // Nothing but the 513 header bytes and the last 2 lines of padding are ignored.
        let mut other = file.clone();
        other[10] ^= 0xFF;
        other[RD175_HEADER + 1477 * RD175_LINE + 5] ^= 0xFF;
        assert!(decode(&other, true).0.unwrap().u16() == out);
    }

    #[test]
    fn rd175_lines_cover_the_mosaic_once() {
        let mut seen = vec![0u8; RD175_WIDTH * RD175_HEIGHT];
        for line in 0..RD175_LINES {
            let Some(p) = rd175_placement(line) else { continue };
            let mut out = vec![0u16; RD175_WIDTH * RD175_HEIGHT];
            rd175_place(&[1u8; RD175_LINE], p, &mut out);
            for (s, v) in seen.iter_mut().zip(&out) {
                *s += u8::from(*v != 0);
            }
        }
        assert!(seen.iter().all(|&n| n == 1));
    }

    #[test]
    fn z2_groups() {
        // 12-bit samples 0x123, 0x456, ... packed MSB first into three words.
        let v: [u16; 8] = [0x123, 0x456, 0x789, 0xABC, 0xDEF, 0x012, 0x345, 0x678];
        let mut bits = 0u128;
        for s in v {
            bits = (bits << 12) | u128::from(s);
        }
        let word = |i: usize| (bits >> (96 - 32 * (i + 1))) as u32;
        assert_eq!(z2_group(word(0), word(1), word(2)), v);
    }

    #[test]
    fn only_the_mrw_signature_and_two_sizes_are_probed() {
        assert!(probe(b"\0MRM...."));
        assert!(!probe(b"\0MRI...."));
        assert!(!probe(&[0u8; 100]));
        // Zero files of the dump sizes: the RD-175 rule has no content test, the Z2 rule does.
        assert!(probe(&vec![0u8; RD175_SIZE]));
        assert!(!probe(&vec![0u8; Z2_SIZE]));
        // A Minolta-branded TIFF has no raw data of its own.
        let mut t = TiffWriter::new();
        let ifd = t.ifd(vec![ascii(0x010F, "Minolta")]);
        let file = t.finish(ifd);
        let tiff = Tiff::parse(&file).unwrap();
        assert!(matches!(open_tiff(&file, tiff, &OpenOptions::default()), Err(Error::UnsupportedFormat(_))));
    }

    /// Fingerprints of what the reader reports for the corpus files (see `testkit::check_golden`).
    #[test]
    #[ignore = "needs the corpus"]
    fn golden_metadata_of_corpus_files() {
        testkit::check_golden(GOLDEN);
    }

    const GOLDEN: &[(&str, &str)] = &[
        ("KONICA MINOLTA/ALPHA SWEET DIGITAL/PICT1038.MRW", "06acf0af5d780a04"),
        ("KONICA MINOLTA/ALPHA-7 DIGITAL/PICT0108.MRW", "2c85533311156078"),
        ("MINOLTA/DYNAX 5D/Minolta_Dynax_5D.MRW", "4abcbc7314c225b8"),
        ("Minolta Co., Ltd./DiMAGE 5/20250104_0010.MRW", "01dc0ea732f02f3e"),
        ("Minolta/ALPHA-7 DIGITAL/PICT6107.MRW", "b35b07c50a49991d"),
        ("Minolta/DiMAGE 7/RAW_MINOLTA_DIMAGE7.MRW", "f6d68a056c7a2d03"),
        ("Minolta/DiMAGE 7Hi/RAW_MINOLTA_DIMAGE_7HI.MRW", "ba3a703d4f31045d"),
        ("Minolta/DiMAGE 7i/RAW_MINOLTA_DIMAGE_7I.MRW", "e7780a6929a397e1"),
        ("Minolta/DiMAGE A1/RAW_MINOLTA_A1.MRW", "9b7c30f080f78e93"),
        ("Minolta/DiMAGE A2/PICT0881.MRW", "748496a1a942315c"),
        ("Minolta/DiMAGE A200/RAW_MINOLTA_DIMAGE_A200.MRW", "ee85738e32aba3cf"),
        ("Minolta/Dimage Z2/RAW_MINOLTA_DIMAGE_Z2.MRW", "797f4b3c506d57b9"),
        ("Minolta/Dynax 7D/PICT3410.MRW", "1fac81b4a156acbe"),
        ("Minolta/Maxxum 7D/PICT0041.MRW", "2f71c683a12c24bf"),
        ("Minolta/RD 175/M001.MDC", "f8e0c29b57df9fa5"),
    ];
}
