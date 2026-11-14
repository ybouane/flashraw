//! Container-level tests: synthetic IIQ and MOS files built here, driven through the crate's dispatcher.

use super::*;
use crate::OpenOptions;

// ----------------------------------------------------------------------------------- tiny writers

fn p16(big: bool, v: u16) -> [u8; 2] {
    if big { v.to_be_bytes() } else { v.to_le_bytes() }
}
fn p32(big: bool, v: u32) -> [u8; 4] {
    if big { v.to_be_bytes() } else { v.to_le_bytes() }
}

/// TIFF entry: tag, type, count, value bytes in the file's byte order.
type Ent = (u16, u16, u32, Vec<u8>);

fn ascii(tag: u16, s: &str) -> Ent {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    (tag, 2, v.len() as u32, v)
}
fn short(big: bool, tag: u16, v: u16) -> Ent {
    (tag, 3, 1, p16(big, v).to_vec())
}
fn long(big: bool, tag: u16, v: u32) -> Ent {
    (tag, 4, 1, p32(big, v).to_vec())
}

/// `II*\0` / `MM\0*` header, `body` at offset 8, then one IFD (values that do not fit follow it).
fn tiff_file(big: bool, body: &[u8], mut entries: Vec<Ent>) -> Vec<u8> {
    entries.sort_by_key(|e| e.0);
    let mut f = if big { b"MM\0*".to_vec() } else { b"II*\0".to_vec() };
    let ifd_at = (8 + body.len() + 1) & !1;
    f.extend(p32(big, ifd_at as u32));
    f.extend(body);
    f.resize(ifd_at, 0);
    f.extend(p16(big, entries.len() as u16));
    let mut extra = Vec::new();
    let extra_at = ifd_at + 2 + 12 * entries.len() + 4;
    for (tag, ty, count, val) in &entries {
        f.extend(p16(big, *tag));
        f.extend(p16(big, *ty));
        f.extend(p32(big, *count));
        if val.len() <= 4 {
            f.extend(val);
            f.resize(f.len() + 4 - val.len(), 0);
        } else {
            f.extend(p32(big, (extra_at + extra.len()) as u32));
            extra.extend(val);
            if extra.len() % 2 == 1 {
                extra.push(0);
            }
        }
    }
    f.extend([0u8; 4]);
    f.extend(extra);
    f
}

/// A bare IIQ file under construction: header, payload and tables first, directory at the end.
struct Iiq {
    big: bool,
    blob: Vec<u8>,
    entries: Vec<(u32, u32, u32)>,
}

impl Iiq {
    fn new(big: bool) -> Self {
        let mut blob = Vec::new();
        blob.extend(if big { b"MMMMRawC" } else { b"IIIICwaR" });
        blob.extend([0u8; 4]);
        Iiq { big, blob, entries: Vec::new() }
    }
    fn put(&mut self, bytes: &[u8]) -> u32 {
        let at = self.blob.len() as u32;
        self.blob.extend(bytes);
        at
    }
    fn inline(&mut self, tag: u32, v: u32) -> &mut Self {
        self.entries.push((tag, 4, v));
        self
    }
    fn table(&mut self, tag: u32, bytes: &[u8]) -> &mut Self {
        let at = self.put(bytes);
        self.entries.push((tag, bytes.len() as u32, at));
        self
    }
    fn geometry(&mut self, w: u32, h: u32) -> &mut Self {
        self.inline(0x108, w).inline(0x109, h)
    }
    fn finish(mut self) -> Vec<u8> {
        let big = self.big;
        let dir = self.blob.len() as u32;
        self.blob.extend(p32(big, self.entries.len() as u32));
        self.blob.extend([0u8; 4]);
        for &(tag, len, data) in &self.entries {
            for v in [tag, 4, len, data] {
                self.blob.extend(p32(big, v));
            }
        }
        self.blob[8..12].copy_from_slice(&p32(big, dir));
        self.blob
    }
}

fn samples(w: u32, h: u32) -> Vec<u16> {
    (0..w * h).map(|i| (i * 7 + 3) as u16).collect()
}

fn le_bytes(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn open(data: &[u8]) -> Result<Box<dyn RawDecoder + '_>> {
    crate::open(data, &OpenOptions::default())
}

fn decode(d: &dyn RawDecoder, recover: bool) -> Result<RawImage> {
    let prof = Profiler::default();
    let mut ctx = DecodeContext::new(&prof);
    ctx.recover = recover;
    ctx.parallel = false;
    d.decode(&ctx)
}

fn region(d: &dyn RawDecoder, r: Rect) -> Result<RawImage> {
    let prof = Profiler::default();
    d.decode_region(&DecodeContext::new(&prof), r)
}

/// Uncompressed 16-bit IIQ of `w x h` ramp samples.
fn raw_iiq(w: u32, h: u32, format: Option<u32>) -> Vec<u8> {
    let mut f = Iiq::new(false);
    let at = f.put(&le_bytes(&samples(w, h)));
    f.geometry(w, h).entries.push((0x10f, w * h * 2, at));
    if let Some(format) = format {
        f.inline(0x10e, format);
    }
    f.finish()
}

// ------------------------------------------------------------------------------------------- IIQ

#[test]
fn probe_looks_at_header_positions_up_to_28() {
    let iiq = raw_iiq(4, 2, None);
    for pad in 0..40 {
        let mut f = vec![0u8; pad];
        f.extend(&iiq);
        assert_eq!(probe(&f), pad <= 28, "pad {pad}");
    }
    // The magic must match too, from either end.
    let mut near = iiq.clone();
    near[5] = b'x';
    near[6] = b'x';
    assert!(!probe(&near));
    near[5] = b'w';
    near[6] = b'a';
    assert!(probe(&near));
    assert!(!probe(&iiq[..7]));
    // A TIFF that merely contains the signature later on is not an IIQ.
    let mut later = vec![0u8; 64];
    later.extend(&iiq);
    assert!(!probe(&later));
}

#[test]
fn uncompressed_round_trip_region_and_packed_payload() {
    let (w, h) = (6, 5);
    let f = raw_iiq(w, h, None);
    let d = open(&f).unwrap();
    assert_eq!(d.format(), RawFormat::Iiq);
    assert_eq!(d.random_access(), RandomAccess::Rows);
    assert!(d.units().is_empty());
    assert_eq!(d.frame_info().compression, Compression::Uncompressed);
    assert_eq!(d.frame_info().bits_per_sample, 16);
    assert_eq!(d.frame_info().white_level.values, [65535]);
    let p = d.packed_payload().expect("format 0 exposes its payload");
    assert_eq!((p.width, p.height, p.bits, p.order, p.row_stride), (w, h, 16, PackOrder::LsbBytes, 12));
    let full = decode(&*d, true).unwrap();
    assert_eq!(full.u16(), &samples(w, h)[..]);
    let r = Rect::new(2, 1, 3, 2);
    let part = region(&*d, r).unwrap();
    assert_eq!((part.width, part.height), (3, 2));
    let all = samples(w, h);
    let expect: Vec<u16> = (1..3).flat_map(|y| all[(y * 6 + 2) as usize..(y * 6 + 5) as usize].to_vec()).collect();
    assert_eq!(part.u16(), &expect[..]);
    assert!(matches!(region(&*d, Rect::new(10, 10, 2, 2)), Err(Error::InvalidArgument(_))));
}

#[test]
fn scrambled_regions_equal_crops_of_the_full_decode() {
    for format in [1, 2] {
        for (w, h) in [(6, 5), (7, 6)] {
            let mut f = Iiq::new(false);
            let at = f.put(&le_bytes(&samples(w, h)));
            f.geometry(w, h).inline(0x10e, format).inline(0x112, 0x1234_5678).entries.push((0x10f, w * h * 2, at));
            let file = f.finish();
            let d = open(&file).unwrap();
            let full = decode(&*d, true).unwrap();
            // (With an odd width the pairs straddle rows: regions must end on a pair boundary.)
            for r in [Rect::new(0, 0, w, 2), Rect::new(1, 2, 3, 3), Rect::new(0, 3, w, 2), Rect::new(2, 1, 4, h - 1)] {
                if w % 2 == 1 && r.bottom() != h && (r.bottom() * w) % 2 == 1 {
                    continue;
                }
                let part = region(&*d, r).unwrap();
                assert_eq!(part.u16(), full.crop(r).unwrap().u16(), "format {format} {w}x{h} {r}");
            }
        }
    }
}

/// Rows of 16-bit literals (all that an L row narrower than eight columns holds), packed as 32-bit
/// little-endian words read most significant bit first.
fn literal_row(vals: &[u16]) -> Vec<u8> {
    let mut bytes: Vec<u8> = vals.iter().flat_map(|v| v.to_be_bytes()).collect();
    bytes.resize(bytes.len().div_ceil(4) * 4, 0);
    bytes.chunks_exact(4).flat_map(|c| [c[3], c[2], c[1], c[0]]).collect()
}

#[test]
fn narrow_l_rows_follow_the_row_index_in_file_order() {
    let (w, h) = (5u32, 4u32);
    let rows: Vec<Vec<u16>> = (0..h).map(|y| (0..w).map(|x| (y * 1000 + x * 11 + 5) as u16).collect()).collect();
    // Rows stored in reverse order.
    let mut payload = Vec::new();
    let mut starts = vec![0u32; h as usize];
    for y in (0..h as usize).rev() {
        starts[y] = payload.len() as u32;
        payload.extend(literal_row(&rows[y]));
    }
    let mut f = Iiq::new(false);
    let at = f.put(&payload);
    let index: Vec<u8> = starts.iter().flat_map(|s| s.to_le_bytes()).collect();
    f.geometry(w, h).inline(0x10e, 8).table(0x21c, &index).entries.push((0x10f, payload.len() as u32, at));
    let file = f.finish();
    let d = open(&file).unwrap();
    assert_eq!(d.frame_info().compression, Compression::PhaseOne);
    assert_eq!(d.units().len(), h as usize);
    assert_eq!(d.units()[0].rect, Rect::new(0, 0, w, 1));
    let img = decode(&*d, true).unwrap();
    let want: Vec<u16> = rows.concat();
    assert_eq!(img.u16(), &want[..]);
    let part = region(&*d, Rect::new(1, 2, 3, 2)).unwrap();
    assert_eq!(part.u16(), &[rows[2][1], rows[2][2], rows[2][3], rows[3][1], rows[3][2], rows[3][3]][..]);
}

#[test]
fn compressed_formats_need_their_row_index() {
    for format in [3, 5, 6, 8] {
        let mut f = Iiq::new(false);
        let at = f.put(&[0; 64]);
        f.geometry(8, 4).inline(0x10e, format).entries.push((0x10f, 64, at));
        let e = open(&f.finish()).map(|_| ()).unwrap_err();
        assert_eq!(e.kind(), "InvalidMetadata", "{e}");
    }
    let mut f = Iiq::new(false);
    f.geometry(8, 4).inline(0x10e, 4).inline(0x10f, 0);
    assert_eq!(open(&f.finish()).map(|_| ()).unwrap_err().kind(), "UnsupportedCompression");
}

#[test]
fn row_index_outside_the_file_fails_every_decode() {
    let mut f = Iiq::new(false);
    let at = f.put(&[0; 32]);
    f.geometry(5, 4).inline(0x10e, 8).entries.push((0x10f, 32, at));
    f.entries.push((0x21c, 16, 1 << 20));
    let file = f.finish();
    let d = open(&file).unwrap();
    assert!(d.units().is_empty());
    for recover in [true, false] {
        let e = decode(&*d, recover).unwrap_err();
        assert!(e.truncation().is_some(), "{e}");
    }
    assert!(region(&*d, Rect::new(0, 0, 2, 2)).is_err());
}

#[test]
fn directory_errors() {
    let mut f = raw_iiq(4, 2, None);
    let at = f.len() - 4 * 16 - 8;
    f[8..12].copy_from_slice(&0x0bad_0badu32.to_le_bytes());
    assert_eq!(open(&f).map(|_| ()).unwrap_err().kind(), "CorruptFile");
    f[8..12].copy_from_slice(&(at as u32).to_le_bytes());
    f[at..at + 4].copy_from_slice(&1_000_000u32.to_le_bytes());
    assert_eq!(open(&f).map(|_| ()).unwrap_err().kind(), "CorruptFile");
    f[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(open(&f).map(|_| ()).unwrap_err().kind(), "CorruptFile");
    let mut g = Iiq::new(false);
    g.inline(0x108, 4);
    assert_eq!(open(&g.finish()).map(|_| ()).unwrap_err().kind(), "InvalidMetadata");
}

/// Geometry of a `w x h` IIQ with the given visible rectangle: (cfa, active area).
fn geometry(lm: u32, tm: u32, vw: u32, vh: u32) -> (CfaPattern, Rect) {
    let (w, h) = (60, 40);
    let mut f = Iiq::new(false);
    let at = f.put(&le_bytes(&samples(w, h)));
    f.geometry(w, h).inline(0x10a, lm).inline(0x10b, tm).inline(0x10c, vw).inline(0x10d, vh);
    f.entries.push((0x10f, w * h * 2, at));
    let file = f.finish();
    let d = open(&file).unwrap();
    let i = d.frame_info();
    assert_eq!(i.crop, i.active_area);
    (i.cfa.unwrap(), i.active_area)
}

#[test]
fn odd_margins_shift_the_pattern_and_shrink_the_area() {
    assert_eq!(geometry(36, 6, 20, 20), (CfaPattern::RGGB, Rect::new(36, 6, 20, 20)));
    assert_eq!(geometry(26, 21, 20, 10), (CfaPattern::GBRG, Rect::new(26, 22, 20, 9)));
    assert_eq!(geometry(41, 22, 16, 10), (CfaPattern::GRBG, Rect::new(42, 22, 15, 10)));
    assert_eq!(geometry(26, 22, 20, 10), (CfaPattern::RGGB, Rect::new(26, 22, 20, 10)));
    // A visible rectangle sticking out of the buffer is cut to it.
    assert_eq!(geometry(50, 30, 40, 40).1, Rect::new(50, 30, 10, 10));
    let _ = geometry(70, 0, 5, 5);
}

#[test]
fn black_references_become_negated_weighted_means() {
    let (w, h) = (8u32, 3u32);
    let mut f = Iiq::new(false);
    let at = f.put(&le_bytes(&samples(w, h)));
    let rows: Vec<u8> = [(4i16, 8i16), (0, 0), (-3, 5)]
        .iter()
        .flat_map(|&(a, b)| [a.to_le_bytes(), b.to_le_bytes()].concat())
        .collect();
    f.geometry(w, h).inline(0x21d, 1024).inline(0x222, 2).table(0x223, &rows);
    f.entries.push((0x10f, w * h * 2, at));
    let file = f.finish();
    let d = open(&file).unwrap();
    let b = &d.frame_info().black_level;
    assert_eq!(b.values, [1024.0]);
    let want = |a: f32, bb: f32| -(a * 2.0 + bb * 6.0) / 8.0;
    assert_eq!(b.row_deltas[0].to_bits(), want(4.0, 8.0).to_bits());
    // A zero sum keeps its sign: the negative zero is part of the reported values.
    assert_eq!(b.row_deltas[1].to_bits(), (-0.0f32).to_bits());
    assert_eq!(b.row_deltas[2].to_bits(), want(-3.0, 5.0).to_bits());
    assert!(b.col_deltas.is_empty());
}

#[test]
fn all_zero_references_are_dropped_and_short_tables_ignored() {
    for outside in [false, true] {
        let mut f = Iiq::new(false);
        let at = f.put(&le_bytes(&samples(8, 3)));
        f.geometry(8, 3);
        if outside {
            f.entries.push((0x223, 12, 1 << 20));
        } else {
            f.table(0x223, &[0u8; 12]);
        }
        f.entries.push((0x10f, 48, at));
        let file = f.finish();
        let d = open(&file).unwrap();
        assert!(d.frame_info().black_level.row_deltas.is_empty());
    }
}

#[test]
fn directory_values_reach_the_metadata() {
    let mut f = Iiq::new(false);
    let at = f.put(&le_bytes(&samples(8, 4)));
    let wb: Vec<u8> = [1.83f32, 1.0, 1.42].iter().flat_map(|v| v.to_le_bytes()).collect();
    f.geometry(8, 4).table(0x107, &wb).table(0x301, b"IQ4 150MP, v7.00.7 (F1)\0").table(0x102, b"SN123\0");
    f.inline(0x210, 22.625f32.to_bits()).table(0x110, &[0; 9]).inline(0x100, 1);
    f.entries.push((0x10f, 64, at));
    let file = f.finish();
    let d = open(&file).unwrap();
    let m = d.metadata();
    assert_eq!((m.make.as_str(), m.model.as_str()), ("Phase One", "IQ4 150MP"));
    assert_eq!(m.serial.as_deref(), Some("SN123"));
    assert_eq!(m.firmware.as_deref(), Some("IQ4 150MP, v7.00.7 (F1)"));
    let extra: Vec<(&str, &str)> = m.extra.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let keys: Vec<&str> = extra.iter().map(|e| e.0).collect();
    assert_eq!(&keys[..3], ["sensor_temperature", "calibration_bytes", "iiq_format"]);
    assert!(extra.contains(&("sensor_temperature", "22.625")) && extra.contains(&("calibration_bytes", "9")));
    let i = d.frame_info();
    assert_eq!(i.orientation, Orientation::Rotate90);
    let wb = i.white_balance.unwrap().coeffs;
    assert_eq!(wb, [1.83, 1.0, 1.42, 1.0]);
}

#[test]
fn achromatic_backs_have_no_filter() {
    let mut f = Iiq::new(false);
    let at = f.put(&le_bytes(&samples(8, 4)));
    f.geometry(8, 4).table(0x301, b"IQ3 100MP Achromatic, User\0");
    f.entries.push((0x10f, 64, at));
    let file = f.finish();
    let d = open(&file).unwrap();
    assert_eq!((d.frame_info().cfa, d.frame_info().layout), (None, SensorLayout::Monochrome));
}

#[test]
fn big_endian_and_tiff_wrapped_files() {
    let (w, h) = (6u32, 3u32);
    let mut f = Iiq::new(true);
    let bytes: Vec<u8> = samples(w, h).iter().flat_map(|s| s.to_be_bytes()).collect();
    let at = f.put(&bytes);
    f.geometry(w, h).entries.push((0x10f, w * h * 2, at));
    let be = f.finish();
    let d = open(&be).unwrap();
    assert_eq!(decode(&*d, true).unwrap().u16(), &samples(w, h)[..]);
    assert_eq!(d.packed_payload().unwrap().order, PackOrder::MsbBytes);

    // The same blob after an 8-byte TIFF header: offsets stay relative to the blob.
    let mut f = Iiq::new(false);
    let at = f.put(&le_bytes(&samples(w, h)));
    f.geometry(w, h).entries.push((0x10f, w * h * 2, at));
    let blob = f.finish();
    let ifd = tiff_file(false, &blob, vec![ascii(0x10f, "Leaf"), ascii(0x110, "Credo 40")]);
    assert!(probe(&ifd));
    let d = open(&ifd).unwrap();
    assert_eq!(d.format(), RawFormat::Iiq);
    assert_eq!((d.metadata().make.as_str(), d.metadata().model.as_str()), ("Leaf", "Credo 40"));
    assert_eq!(decode(&*d, true).unwrap().u16(), &samples(w, h)[..]);
}

#[test]
fn truncations_and_byte_flips_never_panic() {
    let mut f = Iiq::new(false);
    let rows: Vec<u8> = (0..3u32).flat_map(|y| (y * 20).to_le_bytes()).collect();
    let at = f.put(&[0x55; 80]);
    f.geometry(6, 3).inline(0x10e, 6).table(0x21c, &rows).table(0x223, &[1; 12]);
    f.entries.push((0x10f, 80, at));
    let file = f.finish();
    for n in 0..file.len() {
        if let Ok(d) = open(&file[..n]) {
            let _ = decode(&*d, true);
            let _ = decode(&*d, false);
            let _ = region(&*d, Rect::new(1, 1, 3, 2));
        }
    }
    for i in 0..file.len() {
        for v in [0x00, 0xff, 0x80] {
            let mut g = file.clone();
            g[i] ^= v;
            if let Ok(d) = open(&g) {
                let _ = decode(&*d, true);
                let _ = region(&*d, Rect::new(0, 1, 6, 2));
            }
        }
    }
}

// ------------------------------------------------------------------------------------------- MOS

fn pkt(name: &str, payload: &[u8]) -> Vec<u8> {
    let mut v = b"PKTS".to_vec();
    v.extend(1u32.to_be_bytes());
    let mut n = name.as_bytes().to_vec();
    n.resize(40, 0);
    v.extend(n);
    v.extend((payload.len() as u32).to_be_bytes());
    v.extend(payload);
    v
}

fn container(name: &str, children: &[Vec<u8>]) -> Vec<u8> {
    let mut inner = children.concat();
    inner.push(0);
    pkt(name, &inner)
}

/// A big-endian MOS file with uncompressed 16-bit strips of `band` rows.
fn mos(w: u32, h: u32, band: u32, tree: &[u8], extra: Vec<Ent>) -> Vec<u8> {
    let big = true;
    let pixels: Vec<u16> = samples(w, h);
    let mut body = Vec::new();
    let mut offsets = Vec::new();
    let mut counts = Vec::new();
    for rows in pixels.chunks((w * band) as usize) {
        offsets.push(8 + body.len() as u32);
        let b: Vec<u8> = rows.iter().flat_map(|s| s.to_be_bytes()).collect();
        counts.push(b.len() as u32);
        body.extend(b);
    }
    let pkts_at = 8 + body.len() as u32;
    body.extend(tree);
    let longs = |v: &[u32]| v.iter().flat_map(|x| p32(big, *x)).collect::<Vec<u8>>();
    let mut entries = vec![
        long(big, 256, w),
        long(big, 257, h),
        short(big, 258, 16),
        short(big, 259, 1),
        (273, 4, offsets.len() as u32, longs(&offsets)),
        long(big, 278, band),
        (279, 4, counts.len() as u32, longs(&counts)),
        ascii(271, "Leaf"),
        ascii(272, "Aptus"),
        (super::TAG_LEAF_PKTS, 2, tree.len() as u32, p32(big, pkts_at).to_vec()),
    ];
    entries.retain(|e| extra.iter().all(|x| x.0 != e.0));
    entries.extend(extra);
    tiff_file(big, &body, entries)
}

fn capture(rotation: &str, pattern: &str, image_rotation: &str) -> Vec<u8> {
    capture_back(b"12", rotation, pattern, image_rotation)
}

fn capture_back(back: &[u8], rotation: &str, pattern: &str, image_rotation: &str) -> Vec<u8> {
    container(
        "root",
        &[
            pkt("ShootObj_back_type", back),
            pkt("NeutObj_neutrals", b"3270 2703 3270 2232"),
            pkt("back_serial_number", b"L123456\0\0"),
            pkt("CameraObj_camera_type", b"Mamiya 645\0"),
            container(
                "cap",
                &[
                    pkt("CaptProf_raw_data_rotation", rotation.as_bytes()),
                    pkt("CaptProf_number_of_planes", b"1"),
                    pkt("CaptProf_mosaic_pattern", pattern.as_bytes()),
                    pkt("CaptProf_color_matrix", b"1.2 -0.1 -0.1 -0.1 1.2 -0.1 -0.1 -0.1 1.2"),
                ],
            ),
            container("img", &[pkt("ImgProf_rotation_angle", image_rotation.as_bytes())]),
        ],
    )
}

#[test]
fn mos_metadata_from_the_packet_tree() {
    let file = mos(8, 6, 6, &capture("0", "1 3 2 0", "90"), vec![]);
    assert_eq!(crate::detect(&file), Some(RawFormat::Mos));
    let d = open(&file).unwrap();
    let (m, i) = (d.metadata(), d.frame_info());
    assert_eq!((m.make.as_str(), m.model.as_str(), m.serial.as_deref()), ("Leaf", "Aptus 22", Some("L123456")));
    assert!(m.extra.iter().any(|(k, v)| k == "body" && v == "Mamiya 645"));
    assert_eq!(i.cfa, Some(CfaPattern::RGGB));
    assert_eq!(i.orientation, Orientation::Rotate90);
    assert_eq!((i.bits_per_sample, i.white_level.values.clone()), (14, vec![16383]));
    let wb = i.white_balance.unwrap().coeffs;
    assert!((wb[0] - 3270.0 / 2703.0).abs() < 1e-6 && (wb[2] - 3270.0 / 2232.0).abs() < 1e-6);
    assert_eq!(decode(&*d, true).unwrap().u16(), &samples(8, 6)[..]);

    // A back the camera database does not know keeps the matrix of the packet.
    let unknown = mos(8, 6, 6, &capture_back(b"1", "0", "1 3 2 0", "0"), vec![]);
    let d = open(&unknown).unwrap();
    assert_eq!(d.metadata().model, "DCB2");
    assert_eq!(d.frame_info().color_matrices.source, ColorSource::File);

    let gbrg = mos(8, 6, 6, &capture("0", "2 0 1 3", "0"), vec![]);
    let d = open(&gbrg).unwrap();
    assert_eq!(d.frame_info().cfa, Some(CfaPattern::GBRG));
    assert_eq!(d.frame_info().orientation, Orientation::Normal);
}

#[test]
fn mos_bands_decode_in_groups_and_missing_bands_fail() {
    let tree = capture("0", "1 3 2 0", "0");
    let file = mos(8, 6, 2, &tree, vec![]);
    let d = open(&file).unwrap();
    assert_eq!(d.random_access(), RandomAccess::Strips { rows: 2 });
    assert_eq!(decode(&*d, true).unwrap().u16(), &samples(8, 6)[..]);
    // Declare more rows per strip than strips provided: the third band is missing.
    let short = mos(8, 6, 2, &tree, vec![long(true, 278, 1)]);
    let d = open(&short).unwrap();
    let e = decode(&*d, true).unwrap_err();
    assert!(e.to_string().contains("missing band"), "{e}");
}

#[test]
fn mos_rejects_what_it_cannot_decode() {
    let tree = capture("0", "1 3 2 0", "0");
    let kind = |f: &[u8]| open(f).map(|_| ()).unwrap_err().kind();
    assert_eq!(kind(&mos(8, 6, 6, &tree, vec![long(true, 259, 5)])), "UnsupportedCompression");
    assert_eq!(kind(&mos(8, 6, 6, &tree, vec![short(true, 258, 12)])), "UnsupportedCompression");
    assert_eq!(kind(&mos(8, 6, 6, &tree, vec![short(true, 277, 3)])), "UnsupportedCompression");
    let planes = container("root", &[container("cap", &[pkt("CaptProf_number_of_planes", b"4")])]);
    assert_eq!(kind(&mos(8, 6, 6, &planes, vec![])), "UnsupportedCompression");
}

#[test]
fn mos_truncations_never_panic() {
    let file = mos(8, 6, 3, &capture("0", "1 3 2 0", "90"), vec![]);
    for n in 0..file.len() {
        if let Ok(d) = open(&file[..n]) {
            let _ = decode(&*d, true);
            let _ = decode(&*d, false);
        }
    }
    for i in 0..file.len() {
        let mut g = file.clone();
        g[i] ^= 0xff;
        if let Ok(d) = open(&g) {
            let _ = decode(&*d, true);
        }
    }
}

// ------------------------------------------------------------------------- real files (corpus)

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// Hashes of the metadata and frame description (Debug text) of representative corpus files, taken from
/// the build that matches the previous reader file for file. Needs `FLASHRAW_CORPUS`.
#[test]
#[ignore = "needs the corpus"]
fn golden_metadata_of_real_files() {
    let Ok(root) = std::env::var("FLASHRAW_CORPUS") else { return };
    let files: &[(&str, u64)] = GOLDEN;
    let mut seen = 0;
    for (rel, want) in files {
        let Ok(data) = std::fs::read(std::path::Path::new(&root).join(rel)) else { continue };
        let d = open(&data).unwrap();
        let got = fnv(&format!("{:#?}\n{:#?}\n{:?}", d.metadata(), d.frame_info(), d.units().len()));
        if std::env::var("GOLDEN_PRINT").is_ok() {
            println!("    (\"{rel}\", {got:#018x}),");
        } else {
            assert_eq!(got, *want, "{rel}");
        }
        seen += 1;
    }
    eprintln!("checked {seen} of {} files", files.len());
}

const GOLDEN: &[(&str, u64)] = &[
    ("Leaf/Aptus 22/L_003172.mos", 0x53aaa16f4daf3235),
    ("Leaf/Aptus 22/L_003173.mos", 0x94492adf8dfa6749),
    ("Leaf/Aptus 75/RAW_APTUS_75.MOS", 0x551c8de30ef7097a),
    ("Leaf/AFi-II 12/L_000859.mos", 0xe5e6dac62773e7c2),
    ("Leaf/Credo 40/0001_20_32.IIQ", 0xf6354e3eaa1ab6ce),
    ("Leaf/Credo 40/0002_20_32.IIQ", 0xe5a085db11ee5095),
    ("Phase One/P25+/CF028662.IIQ", 0x67d699281d4caafd),
    ("Phase One/P20+/CF051545.IIQ", 0x76b70df231537dd1),
    ("Phase One/P30/CF098736.TIF", 0x380382e3a5f066b8),
    ("Phase One/P45/230215_3810.TIF", 0x1a85cda1506cc901),
    ("Phase One/H 25/H25_IT8.7-2_Card.TIF", 0x8f2a1c453eba5f98),
    ("Phase One/H 25/H25_Outdoor_.IIQ", 0xfafa56e6ace9e75a),
    ("Phase One/IQ180/230214_9562.IIQ", 0xbca820e22675bfde),
    ("Phase One/IQ180/230214_9565.IIQ", 0xae0fa391559ba8d0),
    ("Phase One/IQ3 100MP/Phase_One_IQ3_100MP_IIQ_S.IIQ", 0xc9437de01c81c042),
    ("Phase One/IQ4 150MP/IQ4_150MP_L_16_Ex.iiq", 0xd729cd56a63787c2),
    ("Phase One/iXM-100/_849.IIQ", 0x50bd5ee68303bee4),
    ("Phase One/iXU180/cap_22908.IIQ", 0x414ebed1e62a694b),
    ("Phase One/iXM-RS150F/RGB_86_0_4.IIQ", 0xe2b19c35ba439a41),
    ("Phase One AS/P40+/CF100585.IIQ", 0xe694ca18b2abf8a8),
];
