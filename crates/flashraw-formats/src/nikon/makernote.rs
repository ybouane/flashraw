//! Nikon makernote (EXIF 0x927C) parsing.
//!
//! Three layouts exist:
//! - type 2 (`"Nikon\0\x02..."` + an embedded TIFF header at offset 10; offsets are
//!   relative to that header) — every camera since the D100/Coolpix 5000 era;
//! - type 1 (`"Nikon\0\x01"`, early Coolpix) — carries nothing FlashRaw needs;
//! - type 3 (no header; a plain IFD whose offsets are relative to the file) — D1 family.
//!
//! Tags used for decoding (values verified against the corpus):
//! `0x000C` WB_RBLevels, `0x0014` Coolpix/NRW colour block (WB, black), `0x001D`
//! serial, `0x003D` black level (RGGB, 14-bit units), `0x0093` NEF compression,
//! `0x0096`/`0x008C` linearisation table / Huffman metadata, `0x0097` colour balance
//! (encrypted for versions 2xx), `0x00A7` shutter count (decryption key), `0x0011`
//! preview IFD, `0x0084` lens specification.

use super::crypt;
use flashraw_core::bytes::{ByteOrder, ReadAt};
use flashraw_core::tiff::{Entry, TiffOptions, parse_ifd_at, types};

/// Values extracted from a Nikon makernote.
#[derive(Debug, Clone, Default)]
pub struct MakerNote {
    pub order: Option<ByteOrder>,
    pub iso: Option<u32>,
    /// any camera-specific rewriting); zero when unknown.
    pub cam_mul: [f32; 4],
    /// Black level from tag 0x003D in R, G1, G2, B order.
    pub black: Option<[u16; 4]>,
    /// Black level from the Coolpix NRW block (tag 0x0014).
    pub nrw_black: Option<u32>,
    pub nef_compression: Option<u16>,
    /// Absolute file offset of the Huffman/linearisation metadata (0x0096, else 0x008C).
    pub lin_offset: Option<usize>,
    /// Colour balance (0x0097) version, e.g. 219 for "0219".
    pub color_balance_version: u32,
    /// Embedded previews: absolute (offset, length) pairs.
    pub previews: Vec<(usize, usize)>,
    /// Lens min/max focal length and max aperture at those (0x0084).
    pub lens_spec: Option<[f32; 4]>,
    pub serial: Option<String>,
}

/// Parse the makernote stored in `entry` (an entry of the file's EXIF IFD, parsed from
/// `data` with absolute offsets). `model` is the EXIF model without the make prefix.
pub fn parse(data: &[u8], entry: &Entry, file_order: ByteOrder, model: &str) -> MakerNote {
    let mut mn = MakerNote::default();
    let pos = entry.value_pos;
    let Some(head) = data.get(pos..pos + 10) else { return mn };
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    // (base slice, byte order, IFD offset within the slice)
    let (base, order, ifd_off) = if &head[..6] == b"Nikon\0" {
        if head[6] != 2 {
            return mn; // type 1: nothing usable
        }
        let base = pos + 10;
        let order = match data.get(base..base + 2) {
            Some(b"II") => ByteOrder::Little,
            Some(b"MM") => ByteOrder::Big,
            _ => return mn,
        };
        let Ok(off) = data.u32_at(order, base + 4) else { return mn };
        (base, order, off)
    } else {
        (0usize, file_order, pos as u32)
    };
    let slice = &data[base..];
    let Ok(ifd) = parse_ifd_at(slice, order, ifd_off, opts) else { return mn };
    mn.order = Some(order);

    let u16_at = |b: &[u8], off: usize, o: ByteOrder| b.u16_at(o, off).ok();
    let mut serial: Vec<u8> = Vec::new();
    let mut cb_version = 0u32;
    let mut cb_data: Option<Vec<u8>> = None;
    for e in &ifd.entries {
        let v = e.data;
        match e.tag {
            0x0002 => {
                if mn.iso.is_none() {
                    mn.iso = e.u32(1).filter(|&i| i > 0);
                }
            }
            0x000c if e.count == 4 && e.typ == types::RATIONAL => {
                let f = |i| e.f32(i).unwrap_or(0.0);
                mn.cam_mul = [f(0), f(2), f(1), f(3)];
            }
            0x0011 => {
                if let Some(off) = e.u32(0) {
                    if let Ok(p) = parse_ifd_at(slice, order, off, opts) {
                        if let (Some(o), Some(l)) = (p.u32(0x0201), p.u32(0x0202)) {
                            mn.previews.push((base + o as usize, l as usize));
                        }
                    }
                }
            }
            0x0014 if e.typ == types::UNDEFINED => parse_color_block(e, order, model, &mut mn),
            0x001d => {
                serial = v.to_vec();
                mn.serial = Some(e.string()).filter(|s| !s.is_empty());
            }
            0x003d => {
                if let (Some(a), Some(b), Some(c), Some(d)) = (e.u16(0), e.u16(1), e.u16(2), e.u16(3)) {
                    mn.black = Some([a, b, c, d]);
                }
            }
            0x0084 => {
                let f = e.f32s();
                if f.len() >= 4 && f[0] > 0.0 {
                    mn.lens_spec = Some([f[0], f[1], f[2], f[3]]);
                }
            }
            0x0093 => mn.nef_compression = e.u16(0),
            0x008c | 0x0096 => mn.lin_offset = Some(base + e.value_pos),
            0x0097 => {
                cb_version = v
                    .iter()
                    .take(4)
                    .fold(0u32, |a, &c| a.wrapping_mul(10).wrapping_add((c as u32).wrapping_sub(b'0' as u32)));
                mn.color_balance_version = cb_version;
                match cb_version {
                    100 => {
                        // D100: R, B, G, G at 0x48.
                        if let (Some(r), Some(b), Some(g1), Some(g2)) = (
                            u16_at(v, 0x48, order),
                            u16_at(v, 0x4a, order),
                            u16_at(v, 0x4c, order),
                            u16_at(v, 0x4e, order),
                        ) {
                            mn.cam_mul = [r as f32, g1 as f32, b as f32, g2 as f32];
                        }
                    }
                    102 => {
                        // D2H: R, G, G, B at 0x0A.
                        if let (Some(r), Some(g1), Some(g2), Some(b)) = (
                            u16_at(v, 0x0a, order),
                            u16_at(v, 0x0c, order),
                            u16_at(v, 0x0e, order),
                            u16_at(v, 0x10, order),
                        ) {
                            mn.cam_mul = [r as f32, g1 as f32, b as f32, g2 as f32];
                        }
                    }
                    103 => {
                        // D70: R, G, B, G at 0x14.
                        if let (Some(r), Some(g1), Some(b), Some(g2)) = (
                            u16_at(v, 0x14, order),
                            u16_at(v, 0x16, order),
                            u16_at(v, 0x18, order),
                            u16_at(v, 0x1a, order),
                        ) {
                            mn.cam_mul = [r as f32, g1 as f32, b as f32, g2 as f32];
                        }
                    }
                    _ => {}
                }
                if cb_version >= 200 {
                    let start = if cb_version != 205 { 4 + 0x118 } else { 4 };
                    // Some cameras (D50) declare a shorter tag than the 324-byte block
                    let p = e.value_pos + start;
                    cb_data = slice.get(p..p + 324).map(|s| s.to_vec());
                }
            }
            0x00a7 => {
                // Encrypted colour balance (versions 200..=217): decrypt with the
                // serial/shutter-count keystream and read the four multipliers.
                if let (Some(buf), Some(cnt)) = (cb_data.as_mut(), v.get(..4)) {
                    if (200..218).contains(&cb_version) {
                        let key =
                            crypt::Key::new(crypt::serial_index(&serial, model), [cnt[0], cnt[1], cnt[2], cnt[3]]);
                        key.decrypt(buf);
                        let sel = b"66666>666;6A;:;555"[(cb_version - 200) as usize] - b'0';
                        let off = (sel & !1) as usize;
                        let mut m = [0f32; 4];
                        for c in 0..4usize {
                            if let Some(x) = u16_at(buf, off + c * 2, order) {
                                m[c ^ (c >> 1) ^ (sel as usize & 1)] = x as f32;
                            }
                        }
                        mn.cam_mul = m;
                    }
                }
            }
            _ => {}
        }
    }
    mn
}

/// Tag 0x0014: Coolpix colour block (E5400/E8x00, E5700, NRW cameras).
fn parse_color_block(e: &Entry, order: ByteOrder, model: &str, mn: &mut MakerNote) {
    let v = e.data;
    let g2 = |off: usize, o: ByteOrder| v.u16_at(o, off).ok().map(|x| x as f32);
    let g4 = |off: usize| v.u32_at(order, off).ok().map(|x| x as f32);
    match e.count {
        2560 => {
            // E5400, E8400, E8700, E8800: big-endian R, B scaled by 256.
            if let (Some(r), Some(b)) = (g2(0x4e0, ByteOrder::Big), g2(0x4e2, ByteOrder::Big)) {
                mn.cam_mul = [r / 256.0, 1.0, b / 256.0, 1.0];
            }
        }
        1280 => mn.cam_mul = [1.0; 4], // E5000, E5700: no usable as-shot WB
        _ => {
            if v.get(..4) != Some(b"NRW ") {
                return;
            }
            let _ = model;
            if v.get(4..8) == Some(b"0100") {
                // P6000
                let o = 10 + 0x13de;
                if let (Some(r), Some(g), Some(g2v), Some(b)) = (g4(o), g4(o + 4), g4(o + 8), g4(o + 12)) {
                    mn.cam_mul = [r * 2.0, g, b * 2.0, g2v];
                }
            } else {
                // P7000, P7100, B700, P1000, ...
                mn.nrw_black = v.u16_at(order, 10 + 0x16).ok().map(|x| x as u32);
                if mn.cam_mul[0] < 0.1 {
                    let o = 10 + 0x16 + 2 + 0x16;
                    if let (Some(r), Some(g), Some(g2v), Some(b)) = (g4(o), g4(o + 4), g4(o + 8), g4(o + 12)) {
                        mn.cam_mul = [r * 2.0, g, b * 2.0, g2v];
                    }
                }
            }
        }
    }
}
