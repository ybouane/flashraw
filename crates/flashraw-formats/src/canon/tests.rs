//! Tests of the Canon readers that need no corpus: files built in memory (CIFF, CR2-like TIFF, CR3
//! container), the properties the format description promises, and survival of damaged input.
//! The ignored test at the end pins what the readers report for real files.

use crate::OpenOptions;
use crate::testkit::{self, Lcg, Tag, TiffWriter, ascii, long, short};
use flashraw_core::*;

fn open(data: &[u8]) -> Result<Box<dyn RawDecoder + '_>> {
    crate::open(data, &OpenOptions::default())
}

fn decode(d: &dyn RawDecoder) -> Result<RawImage> {
    let prof = Profiler::new();
    let mut ctx = DecodeContext::new(&prof);
    ctx.parallel = false;
    d.decode(&ctx)
}

// ---------------------------------------------------------------------------------------------
// CIFF (CRW)
// ---------------------------------------------------------------------------------------------

enum Rec {
    /// Value stored in the heap.
    Heap(u16, Vec<u8>),
    /// Value held by the record itself (type bit 14 set by the caller).
    Inline(u16, u32),
    Sub(u16, Vec<Rec>),
}

fn heap(recs: &[Rec]) -> Vec<u8> {
    let mut body = Vec::new();
    let mut table = Vec::new();
    for r in recs {
        let (typ, len, off) = match r {
            Rec::Heap(t, p) => {
                let off = body.len() as u32;
                body.extend_from_slice(p);
                (*t, p.len() as u32, off)
            }
            Rec::Inline(t, v) => (*t, *v, 0),
            Rec::Sub(t, sub) => {
                let off = body.len() as u32;
                let h = heap(sub);
                body.extend_from_slice(&h);
                (*t, h.len() as u32, off)
            }
        };
        table.extend(typ.to_le_bytes());
        table.extend(len.to_le_bytes());
        table.extend(off.to_le_bytes());
    }
    let table_at = body.len() as u32;
    body.extend((recs.len() as u16).to_le_bytes());
    body.extend(table);
    body.extend(table_at.to_le_bytes());
    body
}

fn crw_file(recs: &[Rec]) -> Vec<u8> {
    let mut f = b"II".to_vec();
    f.extend(26u32.to_le_bytes());
    f.extend(b"HEAPCCDR");
    f.extend([0x02, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    f.extend(heap(recs));
    f
}

fn le16(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn le32(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// A CRW file of `model` whose sensor record says `sensor` and whose image record says `image`.
fn crw(model: &str, sensor: (u16, u16), image: (u32, u32), colour: Vec<Rec>, raw: Vec<u8>) -> Vec<u8> {
    let mut desc = b"Canon\0".to_vec();
    desc.extend(model.as_bytes());
    desc.resize(40, 0);
    let mut shot = le16(&[68, 0, 0, 0, 128, 0, 0, 0]);
    shot.resize(68, 0);
    let mut props = vec![Rec::Heap(0x102a, shot)];
    props.extend(colour);
    let (sw, sh) = sensor;
    props.push(Rec::Heap(0x1031, le16(&[34, sw, sh, 1, 1, 24, 14, sw - 40, sh - 30, 0, 0, 0, 0])));
    let recs = vec![
        Rec::Heap(0x2005, raw),
        Rec::Heap(0x2007, {
            let mut j = vec![0xff, 0xd8, 0xff, 0xdb, 0, 2, 0xff, 0xd9];
            j.resize(16, 0);
            j
        }),
        Rec::Sub(
            0x300a,
            vec![
                Rec::Heap(0x080a, desc),
                Rec::Heap(0x1810, le32(&[image.0, image.1, 1.0f32.to_bits(), 90])),
                Rec::Inline(0x580b, 123_456),
                Rec::Inline(0x501c, 400),
                Rec::Inline(0x5029, 35 << 16),
                Rec::Inline(0x5834, 0x8000_0168),
                Rec::Heap(0x180e, le32(&[1_105_175_848])),
            ],
        ),
        Rec::Sub(0x300b, props),
    ];
    crw_file(&recs)
}

/// A colour balance record (`0x10a9`) of a 10D: `n = 9`, the Auto quad first, then the presets and the black.
fn colour_balance(auto: [u16; 4]) -> Rec {
    let n = 9;
    let mut v = vec![(2 + 8 * (n + 1)) as u16];
    v.extend(auto);
    v.extend(std::iter::repeat_n(1000u16, (6 + 2) * 4));
    v.extend([128, 129, 130, 131]);
    Rec::Heap(0x10a9, le16(&v))
}

#[test]
fn crw_geometry_black_and_metadata_of_a_table_sensor() {
    // A G5-sized buffer: the 2672 x 1968 row of the layout table.
    let f = crw("Canon PowerShot G5", (2672, 1968), (2592, 1944), vec![], vec![0; 700]);
    assert!(super::crw::probe(&f));
    let d = open(&f).unwrap();
    let (m, i) = (d.metadata(), d.frame_info());
    assert_eq!(d.format(), RawFormat::Crw);
    assert_eq!((m.make.as_str(), m.model.as_str()), ("Canon", "Canon PowerShot G5"));
    assert_eq!((i.width, i.height), (2672, 1968));
    assert_eq!(i.active_area, Rect::new(12, 6, 2616, 1960));
    assert_eq!(i.crop, i.active_area);
    assert_eq!(i.cfa, CfaPattern::parse("RGGB"));
    assert_eq!(i.compression, Compression::CanonCrw);
    // The bit depth follows the low-bit plane the payload does or does not carry.
    let twelve = m.extra("mode") == Some("compressed-12bit");
    assert_eq!(i.bits_per_sample, if twelve { 12 } else { 10 });
    // Left band [2, 10) and right band [2630, 2672) over the rows of the active area.
    assert_eq!(i.masked_areas, vec![Rect::new(2, 6, 8, 1960), Rect::new(2630, 6, 42, 1960)]);
    assert!(i.black_from_masked);
    assert_eq!(i.orientation, Orientation::Rotate90);
    assert_eq!(m.serial.as_deref(), Some("123456"));
    // The shot info record (ISO 3 with all-zero words) comes after the ISO of the first heap and wins.
    assert_eq!(m.exposure.iso, Some(3));
    assert_eq!(m.exposure.focal_length, Some(35.0));
    assert_eq!(m.capture_time.as_deref(), Some("2005-01-08T09:17:28"));
    assert_eq!(m.extra("canon_model_id"), Some("0x80000168"));
    assert_eq!(m.previews.len(), 1);
}

#[test]
fn crw_colour_balance_gives_the_auto_quad_as_white_balance() {
    let f =
        crw("Canon EOS 10D", (3152, 2068), (3088, 2056), vec![colour_balance([2048, 1024, 1024, 1536])], vec![0; 700]);
    let d = open(&f).unwrap();
    let wb = d.frame_info().white_balance.expect("white balance");
    assert_eq!(wb.coeffs, [2.0, 1.0, 1.5, 1.0]);
    // The record's last quad is the camera's black level, used only for sensors outside the table.
    assert_eq!(d.frame_info().active_area, Rect::new(64, 12, 3088, 2056));
}

#[test]
fn crw_unknown_sensor_takes_the_crop_and_the_black_of_the_metadata() {
    let f =
        crw("Canon EOS 10D", (3000, 2000), (2900, 1900), vec![colour_balance([2048, 1024, 1024, 1536])], vec![0; 700]);
    let i = open(&f).unwrap().frame_info().clone();
    // Crop (24, 14, 2960, 1970): the origin is rounded up to even, the far edges are exclusive.
    assert_eq!(i.active_area, Rect::new(24, 14, 2936, 1956));
    assert!(i.masked_areas.is_empty());
    assert_eq!(i.black_level.values.len(), 4);
}

#[test]
fn crw_packed_pro70_decodes_to_the_declared_size() {
    let f = crw("Canon PowerShot Pro70", (1552, 1024), (1552, 1024), vec![], vec![0x55; 1940 * 1024 + 8]);
    let d = open(&f).unwrap();
    let i = d.frame_info();
    assert_eq!((i.width, i.height, i.bits_per_sample), (1552, 1024, 10));
    assert_eq!(i.compression, Compression::Uncompressed);
    assert_eq!(i.cfa.as_ref().map(|c| (c.width(), c.height())), Some((2, 8)));
    assert_eq!(d.packed_payload().map(|p| (p.width, p.height, p.bits)), Some((1552, 1024, 10)));
    let img = decode(&*d).unwrap();
    assert_eq!((img.width, img.height), (1552, 1024));
}

#[test]
fn crw_rejects_what_is_not_ciff() {
    assert!(!super::crw::probe(b"II*\0\x08\0\0\0CR\x02\0"));
    assert!(!super::crw::probe(&[0; 20]));
    let mut f = crw("Canon EOS 10D", (3152, 2068), (3088, 2056), vec![], vec![0; 700]);
    f[2..6].copy_from_slice(&5u32.to_le_bytes());
    assert!(matches!(open(&f), Err(Error::CorruptFile { .. })));
}

#[test]
fn crw_survives_damage() {
    let f = crw("Canon PowerShot G5", (2672, 1968), (2592, 1944), vec![colour_balance([1, 2, 3, 4])], vec![0; 700]);
    for n in 0..f.len() {
        let _ = open(&f[..n]);
    }
    for at in 0..f.len().min(400) {
        for v in [0u8, 0xff, 0x30, 0x7f] {
            let mut g = f.clone();
            g[at] = v;
            if let Ok(d) = open(&g) {
                let _ = decode(&*d);
            }
        }
    }
    // A sub-heap that contains itself ends at the nesting limit.
    let mut own = vec![1, 0, 0x0a, 0x30];
    own.extend(16u32.to_le_bytes());
    own.extend([0; 8]);
    let mut file = crw_file(&[])[..26].to_vec();
    file.extend(&own);
    let _ = open(&file);
}

// ---------------------------------------------------------------------------------------------
// CR2-like TIFF
// ---------------------------------------------------------------------------------------------

/// A lossless-JPEG header (`precision` bits, `w` x `h` MCUs, components `(h, v)` sampling) followed by noise.
fn ljpeg(precision: u8, w: u16, h: u16, comps: &[(u8, u8)], body: usize) -> Vec<u8> {
    let mut g = Lcg(7);
    let mut d = vec![0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x14, 0x00, 1];
    d.extend([0u8; 15]);
    d.push(0);
    let n = comps.len() as u8;
    d.extend([0xFF, 0xC3, 0, 8 + 3 * n, precision]);
    d.extend(h.to_be_bytes());
    d.extend(w.to_be_bytes());
    d.push(n);
    for (i, (hs, vs)) in comps.iter().enumerate() {
        d.extend([i as u8 + 1, hs << 4 | vs, 0]);
    }
    d.extend([0xFF, 0xDA, 0, 6 + 2 * n, n]);
    for i in 0..n {
        d.extend([i + 1, 0]);
    }
    d.extend([1, 0, 0]);
    d.extend(g.bytes(body).into_iter().map(|b| if b == 0xFF { 0xFE } else { b }));
    d
}

/// A CR2-like file: IFD0 with make and model, an EXIF IFD holding the maker note, and the raw stream in a
/// sub-IFD (compression 6, one strip).
fn cr2(model: &str, jpeg: &[u8], maker_note: Vec<Tag>, raw_tags: Vec<Tag>) -> Vec<u8> {
    let mut w = TiffWriter::new();
    let raw_off = w.buf.len() as u32;
    w.buf.extend(jpeg);
    if w.buf.len() % 2 == 1 {
        w.buf.push(0);
    }
    let mn = w.ifd(maker_note);
    let exif = w.ifd(vec![long(0x927C, &[mn]), short(0x8827, &[200])]);
    let mut raw = vec![short(0x103, &[6]), long(0x111, &[raw_off]), long(0x117, &[jpeg.len() as u32])];
    raw.extend(raw_tags);
    let raw_ifd = w.ifd(raw);
    let ifd0 = w.ifd(vec![ascii(0x10f, "Canon"), ascii(0x110, model), long(0x8769, &[exif]), long(0x14A, &[raw_ifd])]);
    w.finish(ifd0)
}

fn two_component_jpeg(w: u32, h: u32) -> Vec<u8> {
    ljpeg(14, (w / 2) as u16, h as u16, &[(1, 1), (1, 1)], 300)
}

fn sensor_info(w: u16, h: u16, crop: [u16; 4]) -> Tag {
    short(0x00e0, &[34, w, h, 1, 1, crop[0], crop[1], crop[2], crop[3], 0, 0, 0, 0])
}

/// ColorData words of the 5D Mark III family (count 1312, first word 10) with the given as-shot and black quads.
fn colour_data_7(as_shot: [u16; 4], black: [u16; 4]) -> Tag {
    let mut w = vec![0u16; 1312];
    w[0] = 10;
    w[0x3f..0x43].copy_from_slice(&as_shot);
    w[0x1f8..0x1fc].copy_from_slice(&black);
    short(0x4001, &w)
}

#[test]
fn cr2_table_buffer_geometry_and_black_bands() {
    let f = cr2("Canon EOS 5D Mark III", &two_component_jpeg(5920, 3950), vec![], vec![]);
    let d = open(&f).unwrap();
    let i = d.frame_info();
    assert_eq!(d.format(), RawFormat::Cr2);
    assert_eq!((i.width, i.height), (5920, 3950));
    assert_eq!(i.active_area, Rect::new(122, 80, 5796, 3870));
    assert_eq!(i.cfa, CfaPattern::parse("RGGB"));
    assert_eq!(i.masked_areas, vec![Rect::new(2, 80, 118, 3870)]);
    assert!(i.black_from_masked);
    assert_eq!(i.compression, Compression::LosslessJpeg);
    assert_eq!(d.units().len(), 1);
}

#[test]
fn cr2_odd_margins_move_the_phase_and_round_the_origin() {
    // The 4160 x 3124 row starts at row 11: the pattern at the buffer origin is shifted, the area starts at 12.
    let f = cr2("Canon PowerShot G15", &two_component_jpeg(4160, 3124), vec![], vec![]);
    let i = open(&f).unwrap().frame_info().clone();
    assert_eq!(i.active_area, Rect::new(104, 12, 4048, 3047));
    assert_eq!(i.cfa, CfaPattern::parse("GBRG"));
}

#[test]
fn cr2_maker_note_describes_a_buffer_without_a_table_row() {
    let mn = vec![
        sensor_info(6000, 3000, [100, 50, 5899, 2949]),
        colour_data_7([2000, 1024, 1024, 1500], [512, 513, 514, 515]),
        long(0x0010, &[0x8000_0285]),
    ];
    let f = cr2("Canon EOS Test", &two_component_jpeg(6000, 3000), mn, vec![]);
    let d = open(&f).unwrap();
    let (m, i) = (d.metadata(), d.frame_info());
    assert_eq!(i.active_area, Rect::new(100, 50, 5800, 2900));
    assert!(i.masked_areas.is_empty());
    // Per-channel black laid out on the RGGB pattern: R, G1, G2, B.
    assert_eq!(i.black_level.values, vec![512.0, 513.0, 514.0, 515.0]);
    assert_eq!(
        i.white_balance.map(|w| w.coeffs),
        WhiteBalance::from_multipliers(2000.0, 1024.0, 1500.0, 1024.0).map(|w| w.coeffs)
    );
    assert_eq!(m.extra("canon_model_id"), Some("0x80000285"));
}

#[test]
fn cr2_sliced_streams_report_their_slices() {
    let f =
        cr2("Canon EOS 5D Mark III", &two_component_jpeg(5920, 3950), vec![], vec![short(0xC640, &[3, 1980, 1960])]);
    assert_eq!(open(&f).unwrap().metadata().extra("cr2_slices"), Some("3x1980+1960"));
}

#[test]
fn cr2_srow_frames_keep_the_valid_columns() {
    // EOS 5D Mark II sRAW1: a 3872-wide 4:2:0 frame holds 3866 valid columns.
    let jpeg = ljpeg(15, 1936, 2574, &[(2, 2), (1, 1), (1, 1)], 300);
    let f = cr2("Canon EOS 5D Mark II", &jpeg, vec![], vec![]);
    let d = open(&f).unwrap();
    let (m, i) = (d.metadata(), d.frame_info());
    assert_eq!(i.compression, Compression::CanonSraw);
    assert_eq!((i.cpp, i.bits_per_sample), (3, 15));
    assert_eq!(m.extra("mode"), Some("medium-raw"));
    assert_eq!(m.extra("sraw_subsampling"), Some("4:2:0"));
}

#[test]
fn cr2_without_a_lossless_stream_is_unsupported() {
    let f = cr2("Canon EOS 5D Mark III", &[0xff, 0xd8, 0xff, 0xd9], vec![], vec![]);
    assert!(matches!(open(&f), Err(Error::CameraUnsupported { .. })));
}

#[test]
fn cr2_survives_damage() {
    let mn = vec![sensor_info(6000, 3000, [100, 50, 5899, 2949]), colour_data_7([1, 2, 3, 4], [5, 6, 7, 8])];
    let f = cr2("Canon EOS Test", &two_component_jpeg(6000, 3000), mn, vec![]);
    let mut g = Lcg(3);
    for n in (0..f.len()).step_by(7) {
        let _ = open(&f[..n]);
    }
    for _ in 0..600 {
        let mut h = f.clone();
        let at = g.below(h.len() as u64) as usize;
        h[at] = g.next() as u8;
        if let Ok(d) = open(&h) {
            let _ = d.frame_info();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// CR3
// ---------------------------------------------------------------------------------------------

fn bx(t: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(t);
    v.extend_from_slice(payload);
    v
}

fn full_box(t: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    bx(t, &[&[0u8; 4][..], payload].concat())
}

/// The `CMP1` payload of a CRX header: version 0x100, `w` x `h`, one tile, `bits`, colour layout `cfa`.
fn cmp1(w: u32, h: u32, bits: u8, cfa: u8) -> Vec<u8> {
    let mut c = vec![0u8; 52];
    c[4..6].copy_from_slice(&0x100u16.to_be_bytes());
    c[8..12].copy_from_slice(&w.to_be_bytes());
    c[12..16].copy_from_slice(&h.to_be_bytes());
    c[16..20].copy_from_slice(&w.to_be_bytes());
    c[20..24].copy_from_slice(&h.to_be_bytes());
    c[24] = bits;
    c[25] = 0x40 | cfa;
    c[28..32].copy_from_slice(&112u32.to_be_bytes());
    c
}

fn track(handler: &[u8; 4], entry: Vec<u8>, sample_at: u64, sample_len: u32) -> Vec<u8> {
    let stsd = full_box(b"stsd", &[&1u32.to_be_bytes()[..], &entry].concat());
    let stsz = full_box(b"stsz", &[&0u32.to_be_bytes()[..], &1u32.to_be_bytes(), &sample_len.to_be_bytes()].concat());
    let stsc = full_box(b"stsc", &[&1u32.to_be_bytes()[..], &[0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1]].concat());
    let co64 = full_box(b"co64", &[&1u32.to_be_bytes()[..], &sample_at.to_be_bytes()].concat());
    let stbl = bx(b"stbl", &[stsd, stsz, stsc, co64].concat());
    // Version and flags (in full_box), a predefined word, then the handler type.
    let mut hdlr = vec![0u8; 4];
    hdlr.extend(handler);
    hdlr.resize(20, 0);
    bx(b"trak", &bx(b"mdia", &[full_box(b"hdlr", &hdlr), bx(b"minf", &stbl)].concat()))
}

fn tiff_stream(tags: Vec<Tag>) -> Vec<u8> {
    let mut w = TiffWriter::new();
    let first = w.ifd(tags);
    w.finish(first)
}

/// A CR3 file with one full-size raw track and a timed-metadata track whose first sample carries a
/// maker note with ColorData; `sensor` is the full-size buffer.
fn cr3(sensor: (u32, u32), bits: u8, cfa: u8) -> Vec<u8> {
    let ftyp = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xdb, 0, 2, 0xff, 0xd9];
    jpeg.resize(40, 0);
    let raw_sample = vec![0u8; 300];
    // Timed metadata: one type-8 record with one 0x927C item.
    let mn = tiff_stream(vec![colour_data_7([2000, 1024, 1024, 1500], [512, 512, 512, 512])]);
    let mut record = vec![1, 1, 0, 1, 0xff, 0xff];
    record.extend(((mn.len() + 8) as u32).to_le_bytes());
    record.extend(0x927Cu32.to_le_bytes());
    record.extend(&mn);
    let mut ctmd = ((record.len() + 6) as u32).to_le_bytes().to_vec();
    ctmd.extend(8u16.to_le_bytes());
    ctmd.extend(&record);

    let build = |base: u64| {
        let mut craw = vec![0u8; 82];
        craw[24..26].copy_from_slice(&(sensor.0.min(65535) as u16).to_be_bytes());
        craw[26..28].copy_from_slice(&(sensor.1.min(65535) as u16).to_be_bytes());
        craw.extend(bx(b"CMP1", &cmp1(sensor.0, sensor.1, bits, cfa)));
        let mut jcraw = vec![0u8; 82];
        jcraw.extend(bx(b"JPEG", &[0; 4]));
        let mut canon = super::cr3::container::CANON_UUID.to_vec();
        canon.extend(bx(b"CMT1", &tiff_stream(vec![ascii(0x10f, "Canon"), ascii(0x110, "Canon EOS Test")])));
        let mut thmb = vec![0u8; 4];
        thmb.extend(160u16.to_be_bytes());
        thmb.extend(120u16.to_be_bytes());
        thmb.extend([0, 0, 0, 40, 0, 2, 0, 0]);
        thmb.extend(&jpeg);
        canon.extend(bx(b"THMB", &thmb));
        let traks = [
            track(b"vide", bx(b"CRAW", &jcraw), base, jpeg.len() as u32),
            track(b"vide", bx(b"CRAW", &craw), base + jpeg.len() as u64, raw_sample.len() as u32),
            track(b"meta", bx(b"CTMD", &[0; 8]), base + (jpeg.len() + raw_sample.len()) as u64, ctmd.len() as u32),
        ];
        bx(b"moov", &[bx(b"uuid", &canon), traks.concat()].concat())
    };
    let mdat_at = (ftyp.len() + build(0).len() + 8) as u64;
    let mut f = ftyp;
    f.extend(build(mdat_at));
    f.extend(bx(b"mdat", &[jpeg.clone(), raw_sample, ctmd].concat()));
    f
}

#[test]
fn cr3_container_geometry_and_maker_note_from_the_timed_track() {
    // 6288 x 4056 is a row of the layout table (EOS 77D / 800D generation).
    let f = cr3((6288, 4056), 14, 0);
    assert!(super::cr3::probe(&f));
    let d = open(&f).unwrap();
    let (m, i) = (d.metadata(), d.frame_info());
    assert_eq!(d.format(), RawFormat::Cr3);
    assert_eq!((i.width, i.height, i.bits_per_sample), (6288, 4056, 14));
    assert_eq!(i.compression, Compression::CanonCrx);
    assert_eq!(i.active_area, Rect::new(264, 36, 6024, 4020));
    assert_eq!(i.cfa, CfaPattern::parse("RGGB"));
    assert_eq!(m.extra("crx_version"), Some("0x100"));
    assert_eq!(m.extra("mode"), Some("raw"));
    assert_eq!(m.extra("raw_images"), None);
    let wb = i.white_balance.expect("as-shot white balance from the timed-metadata maker note");
    assert_eq!(wb.coeffs, WhiteBalance::from_multipliers(2000.0, 1024.0, 1500.0, 1024.0).unwrap().coeffs);
    // The thumbnail; the JPEG track has no readable size and is skipped.
    assert_eq!(m.previews.len(), 1);
}

#[test]
fn cr3_header_pattern_and_forced_phase() {
    let grbg = open(&cr3((6288, 4056), 14, 1)).unwrap().frame_info().cfa;
    assert_eq!(grbg, CfaPattern::parse("GRBG"));
    // The 5632 x 3710 row forces GBRG at the active-area origin (row 17 is odd: the buffer origin is shifted).
    let forced = open(&cr3((5632, 3710), 14, 0)).unwrap().frame_info().cfa;
    assert_eq!(forced, CfaPattern::parse("GBRG").map(|p| p.shifted(0, 1)));
}

#[test]
fn cr3_decode_reports_what_the_codec_finds() {
    let f = cr3((6288, 4056), 14, 0);
    let d = open(&f).unwrap();
    assert!(decode(&*d).is_err());
}

#[test]
fn cr3_sample_beyond_the_file_is_a_decode_time_truncation() {
    let mut f = cr3((6288, 4056), 14, 0);
    let keep = f.len() - 400;
    f.truncate(keep);
    if let Ok(d) = open(&f) {
        let _ = decode(&*d);
    }
}

#[test]
fn cr3_survives_damage() {
    let f = cr3((6288, 4056), 14, 0);
    let mut g = Lcg(11);
    for n in 0..f.len() {
        let _ = open(&f[..n]);
    }
    for _ in 0..2000 {
        let mut h = f.clone();
        let at = g.below(h.len() as u64) as usize;
        h[at] ^= 1 << g.below(8);
        if let Ok(d) = open(&h) {
            let _ = d.frame_info();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Real files
// ---------------------------------------------------------------------------------------------

/// Fingerprints of what the readers report for corpus files (see `testkit::check_golden`).
#[test]
#[ignore = "needs the corpus"]
fn golden_metadata_of_corpus_files() {
    testkit::check_golden(GOLDEN);
}

const GOLDEN: &[(&str, &str)] = &[
    ("Canon/EOS 5D Mark III/5G4A9394.CR2", "85af61625afcc49a"),
    ("Canon/EOS 5D Mark II/08.canon.raw.cr2", "3c20f23143b43bc6"),
    ("Canon/EOS 5D Mark II/09.canon.sraw1.cr2", "2730d4f60bee8390"),
    ("Canon/EOS 5D Mark II/10.canon.sraw2.cr2", "40d876fe61996c32"),
    ("Canon/EOS 5D Mark IV/B13A0732.CR2", "d5fc3f01a4070c76"),
    ("Canon/EOS 6D Mark II/6DII-mRAW.CR2", "731806e5673d4a1f"),
    ("Canon/EOS 6D Mark II/6DII-sRAW.CR2", "d4168a77566410a1"),
    ("Canon/EOS-1D Mark III/_K5R7350.CR2", "6461d558fe4c1962"),
    ("Canon/EOS-1D/B53A0011-20171118-ss200-ap2.8-fl50-iso200.TIF", "facb736930e7240e"),
    ("Canon/EOS D2000C/RAW_CANON_D2000.TIF", "cec1540fc1410ee7"),
    ("Canon/PowerShot S120/IMG_1995.CR2", "36459fbc22a3e6c8"),
    ("Canon/PowerShot SX50 HS/IMG_4059.CR2", "5098fbcb99dd1286"),
    ("Canon/EOS 10D/CRW_7673.CRW", "3686f8929b216f38"),
    ("Canon/EOS D30/CRW_2444.CRW", "2b051b7ae32f7561"),
    ("Canon/EOS D60/CRW_0099.CRW", "1716da0d91626eef"),
    ("Canon/PowerShot G2/RAW_CANON_G2.CRW", "bae4793418ff0bb9"),
    ("Canon/PowerShot G6/CRW_2097.CRW", "67da1dea21e70f03"),
    ("Canon/PowerShot Pro1/RAW_CANON_PRO1_ARGB.CRW", "5e70405ff6e330d4"),
    ("Canon/PowerShot Pro70/RAW_CANON_PRO70_SRGB.CRW", "7fd1f098818beb00"),
    ("Canon/EOS M50/Canon_EOS_M50_CRAW_ISO_100_nocrop_nodual.CR3", "0b953fb52c5d7b36"),
    ("Canon/EOS M50/Canon_EOS_M50_RAW_ISO_100_nocrop_nodual.CR3", "4994b74a3d8117ea"),
    ("Canon/EOS R/Canon_EOS_R_CRAW_ISO_100.CR3", "bba6743b3b9de3f7"),
    ("Canon/EOS R3/_C3A1200.CR3", "177c865783765dd0"),
    ("Canon/EOS R5/Canon_EOS_R5_RAW_ISO_100_crop_dual.CR3", "5b74076dbfeadadf"),
    ("Canon/EOS R6/Canon_EOS_R6_CRAW_ISO_100_nocrop_nodual.CR3", "5dabcf412ea6172b"),
    ("Canon/EOS RP/Canon_EOS_RP_RAW_ISO_100_nocrop_nodual.CR3", "cafc65d38cd55543"),
];
