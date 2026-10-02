//! Headerless ("naked") sensor dumps identified by their exact file size.
//!
//! Two families of camera-database entries carry a `filesize` hint:
//!
//!   6 bits → Minolta RD-175 interleave, 8 → bytes (through a tone curve), 10 → MIPI
//!   (byte order and right shift from the flags). Canon CHDK `.CRW` dumps measure
//!   their black level from the left/right optical-black columns.
//!   Kodak C330/C603/"12MP" dumps are either 8-bit CFA data or YCbCr; the latter
//!   unused fourth channel).
//!   `offset`/`bits`/`order` hints, uniformly packed rows.
//!
//! path runs after every signature-based format has declined the file.

use crate::common::*;
use flashraw_camera_db::Camera;
use flashraw_codecs::misc_packed::{self, PackedParams};
use flashraw_core::*;

/// Find a database entry for this file size and open the file with it.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Option<Result<Box<dyn RawDecoder + 'a>>> {
    let size = data.len().to_string();
    let matches: Vec<&'static Camera> =
        flashraw_camera_db::all().iter().filter(|c| c.hint("filesize") == Some(size.as_str())).collect();
    if let Some(c) = matches.iter().find(|c| c.mode.starts_with("naked-")) {
        return Some(open_table(data, c, opts));
    }
    if let Some(c) = matches.iter().find(|c| c.mode.starts_with("chdk") && !c.no_size_match) {
        return Some(open_size_matched(data, c, opts));
    }
    None
}

fn hint_u(c: &Camera, k: &str) -> Option<u64> {
    c.hint(k).and_then(|v| v.trim().parse().ok())
}

#[derive(Debug, Clone)]
enum Loader {
    Packed(PackedParams),
    /// 16-bit words.
    Words {
        big_endian: bool,
        shift: u32,
        bottom_up: bool,
    },
    /// Bytes through a curve.
    EightBit(Vec<u16>),
    Tight10,
    Loose10,
    Rd175,
    /// Kodak YCbCr dumps (4:2:2 pairs or 4:2:0 row pairs) → RGB samples.
    KodakYcc {
        c330: bool,
        skip_32: bool,
        curve: Vec<u16>,
    },
}

struct Naked<'a> {
    data: &'a [u8],
    offset: usize,
    raw_w: usize,
    raw_h: usize,
    /// Output buffer geometry.
    out_w: u32,
    out_h: u32,
    cpp: u32,
    loader: Loader,
}

/// (linearising) curve.
fn gamma_curve(pwr: f64, ts: f64, imax: u32) -> Vec<u16> {
    let mut g = [pwr, ts, 0.0, 0.0, 0.0, 0.0];
    let mut bnd = [0.0f64, 0.0];
    bnd[(g[1] >= 1.0) as usize] = 1.0;
    if g[1] != 0.0 && (g[1] - 1.0) * (g[0] - 1.0) <= 0.0 {
        for _ in 0..48 {
            g[2] = (bnd[0] + bnd[1]) / 2.0;
            if g[0] != 0.0 {
                bnd[(((g[2] / g[1]).powf(-g[0]) - 1.0) / g[0] - 1.0 / g[2] > -1.0) as usize] = g[2];
            } else {
                bnd[(g[2] / (1.0 - 1.0 / g[2]).exp() < g[1]) as usize] = g[2];
            }
        }
        g[3] = g[2] / g[1];
        if g[0] != 0.0 {
            g[4] = g[2] * (1.0 / g[0] - 1.0);
        }
    }
    (0..256u32)
        .map(|i| {
            let r = i as f64 / imax as f64;
            if r >= 1.0 {
                return 0xffff;
            }
            let v = if r < g[2] {
                r / g[1]
            } else if g[0] != 0.0 {
                ((r + g[4]) / (1.0 + g[4])).powf(1.0 / g[0])
            } else {
                ((r - 1.0) / g[2]).exp()
            };
            (65536.0 * v) as u16
        })
        .collect()
}

fn open_table<'a>(data: &'a [u8], cam: &'static Camera, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let fsize = data.len() as u64;
    let rw = hint_u(cam, "full_width").unwrap_or(0) as usize;
    let rh = hint_u(cam, "full_height").unwrap_or(0) as usize;
    let lf = hint_u(cam, "load_flags").unwrap_or(0) as u32;
    let max_shift = hint_u(cam, "max_shift").unwrap_or(0) as u32;
    let flip = hint_u(cam, "flip").unwrap_or(0) as u32;
    let (offset, bottom_up) = match cam.hint("offset") {
        Some("reversed") => (0u64, true),
        Some(v) => (v.trim().parse().unwrap_or(0), false),
        None => (0, false),
    };
    if rw == 0 || rh == 0 || offset >= fsize {
        return Err(Error::invalid_metadata("naked raw entry without geometry"));
    }
    opts.limits.check_dims(rw as u32, rh as u32, 1)?;
    let mut bits = ((fsize - offset) * 8 / (rw as u64 * rh as u64)) as u32;
    let mut cfa = if cam.hint("monochrome").is_some() { None } else { cam.cfa.and_then(parse_db_cfa) };
    let (make, model) = (cam.make, cam.model);
    let kodak_ycc = make == "Kodak" && matches!(model, "C603" | "C330" | "12MP");
    let loader = if kodak_ycc {
        let curve = if cfa.is_some() && offset != 0 {
            let at = if offset < 4096 { 168 } else { 5252 };
            data.get(at..at + 512)
                .ok_or_else(|| Error::corrupt("Kodak tone curve missing"))?
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect()
        } else {
            gamma_curve(0.0, 3.875, 255)
        };
        let skip_32 = bits > 16;
        bits = 8;
        if cfa.is_some() { Loader::EightBit(curve) } else { Loader::KodakYcc { c330: model == "C330", skip_32, curve } }
    } else {
        match bits {
            6 if make == "Minolta" && rw == 1534 && rh == 986 => Loader::Rd175,
            8 => Loader::EightBit((0..256).collect()),
            10 if (fsize - offset) / rh as u64 * 3 >= rw as u64 * 4 => Loader::Loose10,
            10 if lf & 1 != 0 => Loader::Tight10,
            10 | 12 => Loader::Packed(PackedParams { width: rw, height: rh, bits, flags: lf | 128 }),
            16 => {
                bits -= lf >> 4;
                let shift = (lf >> 1) & 7;
                bits = bits.saturating_sub(shift);
                // The Samsung WB550 table entry flags big-endian, overridden to Intel order.
                let big_endian = lf & 1 != 0 && !(make == "Samsung" && model == "WB550");
                Loader::Words { big_endian, shift, bottom_up }
            }
            b => {
                return Err(Error::unsupported_compression("headerless raw", format!("{b} bits per sample")));
            }
        }
    };
    if bits == 0 || bits > 16 {
        return Err(Error::corrupt("headerless raw with invalid bit depth"));
    }
    let mut white = (1u32 << bits) - (1u32 << max_shift.min(bits));
    if let Loader::EightBit(c) | Loader::KodakYcc { curve: c, .. } = &loader {
        white = c.get(255).copied().unwrap_or(255) as u32;
    }
    if let Loader::Rd175 = loader {
        white = 0xff << 1;
        cfa = Some(CfaPattern::GRBG);
    }

    // the data: the S2 IS has non-black samples in this column band.
    let model =
        if make == "Canon" && model == "PowerShot A610" && is_canon_s2is(data) { "PowerShot S2 IS" } else { model };
    let mut meta = RawMetadata { make: make.to_string(), model: model.to_string(), ..Default::default() };
    db_lookup(&mut meta, cam.mode);
    meta.canonical_make = if cam.canonical_make.is_empty() { make.into() } else { cam.canonical_make.into() };
    meta.canonical_model = model.to_string();
    meta.set_extra("identified_by", "file size");

    let crop = cam.crop.unwrap_or([0, 0, rw as i32, rh as i32]);
    let (mut lm, mut tm, mut w, mut h) =
        (crop[0].max(0) as u32, crop[1].max(0) as u32, crop[2].max(0) as u32, crop[3].max(0) as u32);
    if cfa.is_some() && !matches!(loader, Loader::KodakYcc { .. }) {
        if lm % 2 == 1 {
            lm += 1;
            w = w.saturating_sub(1);
        }
        if tm % 2 == 1 {
            tm += 1;
            h = h.saturating_sub(1);
        }
    }
    let (out_w, out_h, cpp) = match loader {
        Loader::KodakYcc { .. } => (w, h, 3),
        _ => (rw as u32, rh as u32, 1),
    };
    let mut info = FrameInfo::new(out_w, out_h, bits as u8);
    info.cpp = cpp;
    info.orientation = super::orientation_from_flip(flip);
    info.compression = match loader {
        Loader::Packed(_) | Loader::Tight10 | Loader::Loose10 => Compression::Uncompressed,
        Loader::KodakYcc { .. } => Compression::KodakYcbcr,
        _ => Compression::Uncompressed,
    };
    if cpp == 1 {
        info.active_area = Rect::new(lm, tm, w, h);
        if let Some(p) = cfa {
            // re-anchor it at the buffer origin.
            let (pw, ph) = (p.width(), p.height());
            let p = p.shifted(pw - crop[0].max(0) as usize % pw, ph - crop[1].max(0) as usize % ph);
            info.cfa = Some(p);
            info.layout = layout_for(&p);
        } else {
            info.layout = SensorLayout::Monochrome;
        }
    } else {
        info.layout = SensorLayout::Linear;
    }
    info.crop = info.active_area;
    let fallback = flashraw_camera_db::find_matrix(&meta.canonical_make, &meta.canonical_model);
    info.black_level = BlackLevel::uniform(fallback.map(|m| m.black.max(0)).unwrap_or(0) as f32);
    if let Some(m) = fallback.filter(|m| m.white > 0) {
        white = m.white as u32;
    }
    info.white_level = WhiteLevel::uniform(white);
    let sides = match &loader {
        Loader::Packed(p) => p.flags & 32 != 0,
        Loader::EightBit(_) => !model.starts_with("DC2"),
        _ => false,
    };
    if sides && cpp == 1 {
        let a = info.active_area;
        let left = Rect::new(0, a.y, a.x, a.height);
        let trim = hint_u(cam, "right_mask_trim").unwrap_or(0) as u32;
        let right = Rect::new(a.right(), a.y, (rw as u32).saturating_sub(a.right() + trim), a.height);
        info.masked_areas = [left, right].into_iter().filter(|r| !r.is_empty()).collect();
        info.black_from_masked = !info.masked_areas.is_empty();
    }
    info.color_matrices = db_color(&meta, None);

    let raw = Naked { data, offset: offset as usize, raw_w: rw, raw_h: rh, out_w, out_h, cpp, loader };
    let mut dec = SimpleDecoder::new(RawFormat::Nakedraw, meta, info, Box::new(move |ctx| raw.decode(ctx)));
    dec.random_access = RandomAccess::Rows;
    Ok(Box::new(dec))
}

impl Naked<'_> {
    fn decode(&self, ctx: &DecodeContext) -> Result<RawImage> {
        let mut img = RawImage::new_u16(self.out_w, self.out_h, self.cpp, &ctx.limits)?;
        let (w, h) = (self.raw_w, self.raw_h);
        let src = self.data.get(self.offset..).unwrap_or(&[]);
        let out = img.u16_mut();
        let par = ctx.parallel;
        ctx.profiler.time(Stage::Unpack, || -> Result<()> {
            match &self.loader {
                Loader::Packed(p) => misc_packed::packed_rows(self.data, self.offset, *p, out, par)?,
                Loader::Words { big_endian, shift, bottom_up } => {
                    misc_packed::u16_rows(src, w, h, *big_endian, *shift, *bottom_up, out, par)
                }
                Loader::Tight10 => {
                    let stride = (5 * w).div_ceil(32) * 8;
                    misc_packed::mipi10_rows(src, w, h, stride, out, par)
                }
                Loader::Loose10 => misc_packed::loose10_rows(src, w, h, out, par),
                Loader::EightBit(curve) => {
                    par::for_each_row_chunk(&mut out[..w * h], w, 64, par, |first, chunk| {
                        for (i, row) in chunk.chunks_exact_mut(w).enumerate() {
                            let Some(s) = src.get((first + i) * w..) else { break };
                            for (o, &b) in row.iter_mut().zip(s) {
                                *o = curve.get(b as usize).copied().unwrap_or(0);
                            }
                        }
                    });
                }
                Loader::Rd175 => rd175(src, out),
                Loader::KodakYcc { c330, skip_32, curve } => {
                    kodak_ycc(src, w, self.out_w as usize, self.out_h as usize, *c330, *skip_32, curve, out)
                }
            }
            Ok(())
        })?;
        Ok(img)
    }
}

fn is_canon_s2is(data: &[u8]) -> bool {
    (0..100usize).any(|row| data.get(row * 3340 + 3284).is_some_and(|&b| b > 15))
}

/// Minolta RD-175: three CCDs' worth of half-width lines interleaved into a
fn rd175(src: &[u8], out: &mut [u16]) {
    const W: usize = 1534;
    let mut put = |row: usize, col: usize, v: u16| {
        if let Some(o) = out.get_mut(row * W + col) {
            *o = v;
        }
    };
    for irow in 0..1481usize {
        let Some(pixel) = src.get(irow * 768..irow * 768 + 768) else { return };
        let mut bx = irow / 82;
        let mut row = irow % 82 * 12 + if bx < 12 { bx | 1 } else { (bx - 12) * 2 };
        match irow {
            1477 | 1479 => continue,
            1476 => row = 984,
            1480 => row = 985,
            1478 => {
                row = 985;
                bx = 1;
            }
            _ => {}
        }
        let p = |i: usize| pixel[i] as u16;
        if bx < 12 && bx & 1 == 1 {
            for col in 0..1533usize {
                if col != 1 {
                    let v = if (col + 1) & 2 != 0 { p(col / 2 - 1) + p(col / 2 + 1) } else { p(col / 2) << 1 };
                    put(row, col, v);
                }
                row ^= 1;
            }
            put(row, 1, p(1) << 1);
            put(row, 1533, p(765) << 1);
        } else {
            let mut col = row & 1;
            while col < W {
                put(row, col, p(col / 2) << 1);
                col += 2;
            }
        }
    }
}

/// Kodak C330 (4:2:2 YCbYCr pairs, optional 32-row gaps) and C603/12MP (two Y rows
/// kodak c330 loader / kodak c603 loader).
#[allow(clippy::too_many_arguments)]
fn kodak_ycc(src: &[u8], raw_w: usize, w: usize, h: usize, c330: bool, skip_32: bool, curve: &[u16], out: &mut [u16]) {
    let lut = |v: i32| curve.get(v.clamp(0, 255) as usize).copied().unwrap_or(0);
    let mut pos = 0usize;
    let mut buf = vec![0u8; raw_w * 3 + 4];
    for row in 0..h {
        let line = &mut out[row * w * 3..(row + 1) * w * 3];
        if c330 {
            let n = raw_w * 2;
            fill(&mut buf[..n], src, pos);
            pos += n;
            if skip_32 && row & 31 == 31 {
                pos += raw_w * 32;
            }
            for col in 0..w {
                let y = buf[col * 2] as i32;
                let cb = buf[((col * 2) & !3) | 1] as i32 - 128;
                let cr = buf[((col * 2) & !3) | 3] as i32 - 128;
                ycc_px(&mut line[col * 3..col * 3 + 3], y, cb, cr, &lut);
            }
        } else {
            if row & 1 == 0 {
                let n = raw_w * 3;
                fill(&mut buf[..n], src, pos);
                pos += n;
            }
            for col in 0..w {
                let y = buf.get(w * 2 * (row & 1) + col).copied().unwrap_or(0) as i32;
                let cb = buf.get(w + (col & !1)).copied().unwrap_or(128) as i32 - 128;
                let cr = buf.get(w + (col & !1) + 1).copied().unwrap_or(128) as i32 - 128;
                ycc_px(&mut line[col * 3..col * 3 + 3], y, cb, cr, &lut);
            }
        }
    }
}

fn fill(dst: &mut [u8], src: &[u8], pos: usize) {
    let s = src.get(pos..).unwrap_or(&[]);
    let n = dst.len().min(s.len());
    dst[..n].copy_from_slice(&s[..n]);
    dst[n..].fill(0);
}

#[inline]
fn ycc_px(px: &mut [u16], y: i32, cb: i32, cr: i32, lut: &impl Fn(i32) -> u16) {
    let g = y - ((cb + cr + 2) >> 2);
    px[0] = lut(g + cr);
    px[1] = lut(g);
    px[2] = lut(g + cb);
}

fn open_size_matched<'a>(data: &'a [u8], cam: &'static Camera, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let w = hint_u(cam, "full_width").unwrap_or(0) as u32;
    let h = hint_u(cam, "full_height").unwrap_or(0) as u32;
    let offset = hint_u(cam, "offset").unwrap_or(0) as usize;
    if w == 0 || h == 0 || offset >= data.len() {
        return Err(Error::invalid_metadata("CHDK entry without geometry"));
    }
    opts.limits.check_dims(w, h, 1)?;
    let bits = hint_u(cam, "bits")
        .map(|b| b as u32)
        .unwrap_or(((data.len() - offset) as u64 * 8 / w as u64 / h as u64) as u32);
    if !(1..=16).contains(&bits) {
        return Err(Error::unsupported_compression("headerless raw", format!("{bits} bits per sample")));
    }
    let order = match cam.hint("order") {
        Some("plain") => PackOrder::LsbBytes,
        Some("jpeg") => PackOrder::MsbBytes,
        Some("jpeg32") => PackOrder::Msb32,
        Some("jpeg16") | None => PackOrder::Msb16,
        Some(o) => return Err(Error::invalid_metadata(format!("unknown packing order {o}"))),
    };
    let order = match (bits, order) {
        (16, PackOrder::Msb16 | PackOrder::LsbBytes) => PackOrder::LsbBytes,
        (8, _) => PackOrder::MsbBytes,
        (_, o) => o,
    };
    let mut meta = RawMetadata { make: cam.make.to_string(), model: cam.model.to_string(), ..Default::default() };
    let db = db_lookup(&mut meta, cam.mode);
    if meta.canonical_model.is_empty() {
        meta.canonical_model = cam.canonical_model.to_string();
    }
    meta.set_extra("identified_by", "file size");
    let mut info = FrameInfo::new(w, h, bits as u8);
    info.compression = Compression::Uncompressed;
    apply_db(&mut info, &meta, Some(db.unwrap_or(cam)), Known::default());
    let layout = flashraw_codecs::PackedLayout::tight(w as usize, h as usize, bits, order);
    let payload = &data[offset..];
    let simd_payload = PackedPayload { data: payload, width: w, height: h, bits, order, row_stride: layout.row_stride };
    let mut dec = SimpleDecoder::new(
        RawFormat::Nakedraw,
        meta,
        info,
        Box::new(move |ctx| {
            let mut img = RawImage::new_u16(w, h, 1, &ctx.limits)?;
            ctx.profiler.time(Stage::Unpack, || {
                flashraw_codecs::unpack(payload, layout, img.u16_mut(), w as usize, ctx.parallel, ctx.simd, true)
            })?;
            Ok(img)
        }),
    );
    dec.random_access = RandomAccess::Rows;
    dec.packed = Some(simd_payload);
    Ok(Box::new(dec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_curve_is_monotonic() {
        let c = gamma_curve(0.0, 3.875, 255);
        assert_eq!(c.len(), 256);
        assert_eq!(c[0], 0);
        assert!(c.windows(2).all(|w| w[0] <= w[1]));
        assert!(c[254] > 60000);
    }

    #[test]
    fn rd175_handles_short_input() {
        let mut out = vec![0u16; 1534 * 986];
        rd175(&[1u8; 5000], &mut out);
        assert!(out.iter().any(|&v| v != 0));
    }

    #[test]
    fn ycc_conversion() {
        let lut = |v: i32| v.clamp(0, 255) as u16;
        let mut px = [0u16; 3];
        ycc_px(&mut px, 100, 0, 0, &lut);
        assert_eq!(px, [100, 100, 100]);
        ycc_px(&mut px, 100, 40, -40, &lut);
        assert_eq!(px, [60, 100, 140]);
    }
}
