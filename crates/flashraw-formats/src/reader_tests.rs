//! Container-level tests of the Hasselblad, Sony SRF, Raspberry Pi and Epson readers:
//! synthetic files built in memory, and (ignored, corpus needed) digests of the metadata of
//! real files.

use crate::OpenOptions;
use flashraw_core::*;

// ---------------------------------------------------------------------------------------------
// A minimal TIFF writer
// ---------------------------------------------------------------------------------------------

enum Value {
    Bytes(Vec<u8>),
    /// The 4-byte value field as is (offset of a child IFD, or of a blob inside the file).
    Field(u32),
}

struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    value: Value,
}

struct Tb {
    le: bool,
    buf: Vec<u8>,
}

impl Tb {
    fn new(le: bool) -> Self {
        let mut buf = if le { b"II*\0".to_vec() } else { b"MM\0*".to_vec() };
        buf.extend([0; 4]);
        Tb { le, buf }
    }
    fn u16b(&self, v: u16) -> [u8; 2] {
        if self.le { v.to_le_bytes() } else { v.to_be_bytes() }
    }
    fn u32b(&self, v: u32) -> [u8; 4] {
        if self.le { v.to_le_bytes() } else { v.to_be_bytes() }
    }
    fn short(&self, tag: u16, v: &[u16]) -> Entry {
        Entry {
            tag,
            typ: 3,
            count: v.len() as u32,
            value: Value::Bytes(v.iter().flat_map(|&x| self.u16b(x)).collect()),
        }
    }
    fn long(&self, tag: u16, v: &[u32]) -> Entry {
        Entry {
            tag,
            typ: 4,
            count: v.len() as u32,
            value: Value::Bytes(v.iter().flat_map(|&x| self.u32b(x)).collect()),
        }
    }
    fn ascii(&self, tag: u16, s: &str) -> Entry {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        Entry { tag, typ: 2, count: b.len() as u32, value: Value::Bytes(b) }
    }
    fn undefined(&self, tag: u16, b: Vec<u8>) -> Entry {
        Entry { tag, typ: 7, count: b.len() as u32, value: Value::Bytes(b) }
    }
    /// `n` signed rationals `1/1`.
    fn srationals(&self, tag: u16, n: u32) -> Entry {
        let b = (0..n).flat_map(|_| [self.u32b(1), self.u32b(1)].concat()).collect();
        Entry { tag, typ: 10, count: n, value: Value::Bytes(b) }
    }
    fn child(&self, tag: u16, typ: u16, offset: u32) -> Entry {
        Entry { tag, typ, count: 1, value: Value::Field(offset) }
    }
    fn append(&mut self, bytes: &[u8]) -> u32 {
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
        let at = self.buf.len() as u32;
        self.buf.extend(bytes);
        at
    }
    /// Append an IFD (entries sorted by tag, values after the table); returns its offset.
    fn ifd(&mut self, mut entries: Vec<Entry>) -> u32 {
        entries.sort_by_key(|e| e.tag);
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
        let off = self.buf.len();
        let table = 2 + 12 * entries.len() + 4;
        let mut tail: Vec<u8> = Vec::new();
        let mut out = self.u16b(entries.len() as u16).to_vec();
        for e in &entries {
            out.extend(self.u16b(e.tag));
            out.extend(self.u16b(e.typ));
            out.extend(self.u32b(e.count));
            match &e.value {
                Value::Field(v) => out.extend(self.u32b(*v)),
                Value::Bytes(b) if b.len() <= 4 => {
                    out.extend(b);
                    out.extend(vec![0; 4 - b.len()]);
                }
                Value::Bytes(b) => {
                    out.extend(self.u32b((off + table + tail.len()) as u32));
                    tail.extend(b);
                    if tail.len() % 2 == 1 {
                        tail.push(0);
                    }
                }
            }
        }
        out.extend([0; 4]);
        out.extend(tail);
        self.buf.extend(out);
        off as u32
    }
    fn finish(mut self, first_ifd: u32) -> Vec<u8> {
        let f = self.u32b(first_ifd);
        self.buf[4..8].copy_from_slice(&f);
        self.buf
    }
}

fn open(data: &[u8]) -> Result<Box<dyn RawDecoder + '_>> {
    crate::open(data, &OpenOptions::default())
}

/// Decode with the strict or the recover policy; the image and the warnings.
fn decode(d: &dyn RawDecoder, recover: bool) -> (Result<RawImage>, Vec<DecodeWarning>) {
    let p = Profiler::default();
    let mut ctx = DecodeContext::new(&p);
    ctx.recover = recover;
    let r = d.decode(&ctx);
    (r, ctx.take_warnings())
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

// ---------------------------------------------------------------------------------------------
// Hasselblad
// ---------------------------------------------------------------------------------------------

mod hasselblad {
    use super::*;
    use flashraw_core::tiff::tags;

    struct Spec {
        le: bool,
        model: &'static str,
        width: u32,
        height: u32,
        compression: u16,
        bits: u16,
        samples_per_pixel: u16,
        payload: Vec<u8>,
        matrix1: bool,
        matrix2: bool,
        /// Maker note: sensor code, coating code, model name, CCD profile.
        note: Option<(u32, u32, &'static str, bool)>,
        extra_raw: Vec<u16>,
        /// Put the payload after the directories, so that cutting the file cuts the payload.
        payload_last: bool,
    }

    impl Spec {
        fn new(le: bool, width: u32, height: u32, samples: &[u16]) -> Self {
            let t = Tb::new(le);
            Spec {
                le,
                model: "Hasselblad H3D",
                width,
                height,
                compression: 1,
                bits: 16,
                samples_per_pixel: 1,
                payload: samples.iter().flat_map(|&s| t.u16b(s)).collect(),
                matrix1: true,
                matrix2: false,
                note: None,
                extra_raw: vec![],
                payload_last: false,
            }
        }

        fn build(&self) -> Vec<u8> {
            if !self.payload_last {
                return self.build_at(None);
            }
            let end = self.build_at(Some(0)).len() as u32;
            let mut file = self.build_at(Some(end));
            file.extend(&self.payload);
            file
        }

        fn build_at(&self, payload_at: Option<u32>) -> Vec<u8> {
            let mut t = Tb::new(self.le);
            let strip = payload_at.unwrap_or_else(|| t.append(&self.payload));
            let mut raw = vec![
                t.long(tags::IMAGE_WIDTH, &[self.width]),
                t.long(tags::IMAGE_LENGTH, &[self.height]),
                t.short(tags::BITS_PER_SAMPLE, &[self.bits]),
                t.short(tags::COMPRESSION, &[self.compression]),
                t.short(tags::PHOTOMETRIC, &[32803]),
                t.short(tags::SAMPLES_PER_PIXEL, &[self.samples_per_pixel]),
                t.long(tags::STRIP_OFFSETS, &[strip]),
                t.long(tags::STRIP_BYTE_COUNTS, &[self.payload.len() as u32]),
            ];
            if !self.extra_raw.is_empty() {
                raw.push(t.long(tags::MASKED_AREAS, &self.extra_raw.iter().map(|&v| v as u32).collect::<Vec<_>>()));
            }
            let raw = t.ifd(raw);
            let mut ifd0 = vec![
                t.ascii(tags::MAKE, "Hasselblad"),
                t.ascii(tags::MODEL, self.model),
                t.short(tags::ORIENTATION, &[1]),
                t.child(tags::SUB_IFDS, 4, raw),
            ];
            if let Some((sensor, coating, name, profile)) = self.note {
                let m = t.buf.len() + (t.buf.len() % 2);
                // The maker note is a plain IFD starting at its data.
                let mut entries = vec![t.long(0x11, &[sensor]), t.long(0x16, &[coating]), t.ascii(0x15, name)];
                if profile {
                    entries.push(t.srationals(0x2A, 12));
                }
                let at = t.ifd(entries);
                assert_eq!(at as usize, m);
                let len = (t.buf.len() - m) as u32;
                let exif = t.ifd(vec![Entry { tag: tags::MAKER_NOTE, typ: 7, count: len, value: Value::Field(at) }]);
                ifd0.push(t.child(tags::EXIF_IFD, 4, exif));
            }
            if self.matrix1 {
                ifd0.push(t.srationals(tags::COLOR_MATRIX1, 9));
            }
            if self.matrix2 {
                ifd0.push(t.srationals(tags::COLOR_MATRIX2, 9));
            }
            let first = t.ifd(ifd0);
            t.finish(first)
        }
    }

    const SAMPLES: [u16; 8] = [1, 2, 3, 0x1234, 0xFFFF, 0, 7, 0x8000];

    #[test]
    fn raw16_both_byte_orders() {
        for le in [true, false] {
            let data = Spec::new(le, 4, 2, &SAMPLES).build();
            let d = open(&data).unwrap();
            assert_eq!(d.format(), RawFormat::ThreeFr);
            assert_eq!(d.frame_info().compression, Compression::Uncompressed);
            assert_eq!(d.random_access(), RandomAccess::Rows);
            let p = d.packed_payload().unwrap();
            assert_eq!((p.width, p.height, p.bits, p.row_stride, p.data.len()), (4, 2, 16, 8, 16));
            assert_eq!(p.order, if le { PackOrder::LsbBytes } else { PackOrder::MsbBytes });
            let (img, warnings) = decode(d.as_ref(), false);
            assert_eq!(img.unwrap().u16(), &SAMPLES);
            assert!(warnings.is_empty());
        }
    }

    #[test]
    fn truncated_payload_by_policy() {
        // A 4x3 image whose payload holds two rows and ends the file.
        let mut spec = Spec::new(true, 4, 3, &SAMPLES);
        spec.payload_last = true;
        let data2 = spec.build();
        let d = open(&data2).unwrap();
        let (r, _) = decode(d.as_ref(), false);
        assert_eq!(r.unwrap_err().kind(), "CorruptFile");
        let (r, w) = decode(d.as_ref(), true);
        assert_eq!(r.unwrap().u16(), &[1, 2, 3, 0x1234, 0xFFFF, 0, 7, 0x8000, 0, 0, 0, 0]);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].samples_filled, 4);
    }

    #[test]
    fn container_kind() {
        let mut s = Spec::new(true, 4, 2, &SAMPLES);
        assert_eq!(open(&s.build()).unwrap().format(), RawFormat::ThreeFr);
        s.matrix2 = true;
        let data = s.build();
        let d = open(&data).unwrap();
        assert_eq!(d.format(), RawFormat::Fff);
        assert_eq!(d.metadata().extra[0], ("hasselblad_container".to_string(), "FFF".to_string()));
        s.matrix2 = false;
        s.matrix1 = false;
        assert_eq!(open(&s.build()).unwrap().format(), RawFormat::Fff);
        // A maker note with a sane model name and a CCD profile makes it an FFF too.
        s.matrix1 = true;
        s.note = Some((9, 4, "Hasselblad H3D", true));
        assert_eq!(open(&s.build()).unwrap().format(), RawFormat::Fff);
        s.note = Some((9, 4, "Hasselblad H3D", false));
        assert_eq!(open(&s.build()).unwrap().format(), RawFormat::ThreeFr);
        s.note = Some((9, 4, "bad;name", true));
        assert_eq!(open(&s.build()).unwrap().format(), RawFormat::ThreeFr);
    }

    #[test]
    fn unsupported_variants() {
        let mut s = Spec::new(true, 4, 2, &SAMPLES);
        s.samples_per_pixel = 2;
        let e = open(&s.build()).err().unwrap();
        assert_eq!(e.kind(), "UnsupportedCompression");
        let mut s = Spec::new(true, 4, 2, &SAMPLES);
        s.bits = 12;
        assert_eq!(open(&s.build()).err().unwrap().kind(), "UnsupportedCompression");
        s.compression = 5;
        s.bits = 16;
        assert_eq!(open(&s.build()).err().unwrap().kind(), "UnsupportedCompression");
        // The lossless JPEG variants open (whatever the sample depth); a bad stream fails to decode.
        for c in [7, 99] {
            let mut s = Spec::new(true, 4, 2, &SAMPLES);
            s.compression = c;
            s.bits = 8;
            let data = s.build();
            let d = open(&data).unwrap();
            assert_eq!(d.frame_info().compression, Compression::HasselbladLjpeg);
            assert_eq!(d.random_access(), RandomAccess::Sequential);
            assert!(d.packed_payload().is_none());
            assert!(decode(d.as_ref(), true).0.is_err());
        }
    }

    #[test]
    fn x1d_ii_from_maker_note() {
        // A full-size X1D II 3FR whose payload lies beyond the end of the file: the
        // metadata is available, decoding fails.
        let mut s = Spec::new(false, 8384, 6304, &[]);
        s.model = "X1D II 50C";
        s.note = Some((15, 5, "X1D II 50C", false));
        let data = s.build();
        let d = open(&data).unwrap();
        let (m, f) = (d.metadata(), d.frame_info());
        assert_eq!(d.format(), RawFormat::ThreeFr);
        let extra: Vec<(&str, &str)> = m.extra.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(
            extra,
            [
                ("hasselblad_container", "3FR"),
                ("hasselblad_sensor_code", "15"),
                ("hasselblad_sensor", "50-15-Coated5-II-3FR")
            ]
        );
        assert_eq!(f.active_area, Rect::new(48, 96, 8278, 6208));
        assert_eq!(f.crop, f.active_area);
        assert_eq!(f.cfa, Some(CfaPattern::RGGB));
        assert_eq!(f.black_level.values, vec![0.0]);
        assert_eq!(f.white_level.values, vec![65535]);
        assert_eq!(f.color_matrices.source, ColorSource::CameraDb);
        assert!(decode(d.as_ref(), false).0.is_err());
    }

    #[test]
    fn sixty_megapixel_conventions_and_masks() {
        let mut s = Spec::new(true, 9044, 6732, &[]);
        s.note = Some((12, 4, "H4D-60", false));
        s.model = "Hasselblad H4D-60";
        s.extra_raw = vec![12, 76, 50, 11820, 52, 76, 90, 11820, 5, 5, 2, 2, 1, 2];
        let data = s.build();
        let d = open(&data).unwrap();
        let f = d.frame_info();
        assert_eq!(f.active_area, Rect::new(40, 8, 8964, 6716));
        assert_eq!((f.black_level.values.clone(), f.white_level.values.clone()), (vec![256.0], vec![33025]));
        // The inverted group and the incomplete tail are skipped; the rectangles are clamped to the buffer.
        assert_eq!(f.masked_areas, vec![Rect::new(76, 12, 8968, 38), Rect::new(76, 52, 8968, 38)]);
        assert!(f.black_from_masked);
    }

    #[test]
    fn size_identification_without_maker_note() {
        let s = Spec::new(true, 7262, 5456, &[]);
        let data = s.build();
        let d = open(&data).unwrap();
        let extra = &d.metadata().extra;
        assert_eq!(extra[1], ("hasselblad_sensor_code".to_string(), "9".to_string()));
        assert_eq!(extra[2].1, "39-Uncoated-3FR");
        assert_eq!(d.frame_info().active_area, Rect::new(8, 4, 7247, 5444));
        assert_eq!(d.frame_info().cfa, Some(CfaPattern::RGGB));
        // Unknown size: no family, whole buffer.
        let data = Spec::new(true, 6, 4, &[0; 24]).build();
        let d = open(&data).unwrap();
        assert_eq!(d.metadata().extra.len(), 2);
        assert_eq!(d.frame_info().active_area, Rect::full(6, 4));
    }

    #[test]
    fn no_raw_image() {
        let mut t = Tb::new(true);
        let first = t.ifd(vec![t.ascii(tags::MAKE, "Hasselblad")]);
        let data = t.finish(first);
        let tiff = flashraw_core::tiff::Tiff::parse(&data).unwrap();
        let e = crate::hasselblad::open_tiff(&data, tiff, &OpenOptions::default()).err().unwrap();
        assert_eq!(e.kind(), "InvalidMetadata");
        assert!(crate::hasselblad::open(&data, &OpenOptions::default()).is_err());
        assert!(!crate::hasselblad::probe(&data));
    }
}

// ---------------------------------------------------------------------------------------------
// Epson
// ---------------------------------------------------------------------------------------------

mod epson {
    use super::*;
    use flashraw_core::tiff::tags;

    /// Samples as 12-bit MSB-first pairs.
    fn pack12(samples: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in samples.chunks(2) {
            let (a, b) = (p[0], p.get(1).copied().unwrap_or(0));
            out.extend([(a >> 4) as u8, ((a & 15) << 4 | b >> 8) as u8, b as u8]);
        }
        out.truncate((samples.len() * 12).div_ceil(8));
        out
    }

    fn image(width: usize, height: usize) -> Vec<Vec<u16>> {
        (0..height).map(|y| (0..width).map(|x| ((x * 37 + y * 101 + 5) % 4096) as u16).collect()).collect()
    }

    /// Padded rows: groups of ten samples in 15 bytes plus a padding byte.
    fn padded_payload(rows: &[Vec<u16>]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in rows {
            for g in r.chunks(10) {
                let mut b = pack12(g);
                b.resize(16, 0xAA);
                out.extend(b);
            }
        }
        out
    }

    fn tight_payload(rows: &[Vec<u16>]) -> Vec<u8> {
        rows.iter().flat_map(|r| pack12(r)).collect()
    }

    fn build(width: u32, compression: u16, payload: &[u8], rows: u32, note: Option<(Vec<u32>, Vec<u8>)>) -> Vec<u8> {
        build_at(width, compression, payload, rows, note, None)
    }

    /// Like `build`, with the payload after the directories (cutting the file cuts it).
    fn build_last(width: u32, compression: u16, payload: &[u8], rows: u32) -> Vec<u8> {
        let end = build_at(width, compression, payload, rows, None, Some(0)).len() as u32;
        let mut file = build_at(width, compression, payload, rows, None, Some(end));
        file.extend(payload);
        file
    }

    fn build_at(
        width: u32,
        compression: u16,
        payload: &[u8],
        rows: u32,
        note: Option<(Vec<u32>, Vec<u8>)>,
        payload_at: Option<u32>,
    ) -> Vec<u8> {
        let mut t = Tb::new(false);
        let strip = payload_at.unwrap_or_else(|| t.append(payload));
        let raw = t.ifd(vec![
            t.long(tags::IMAGE_WIDTH, &[width]),
            t.long(tags::IMAGE_LENGTH, &[rows]),
            t.short(tags::BITS_PER_SAMPLE, &[12]),
            t.short(tags::COMPRESSION, &[compression]),
            t.short(tags::PHOTOMETRIC, &[32803]),
            t.long(tags::STRIP_OFFSETS, &[strip]),
            t.long(tags::STRIP_BYTE_COUNTS, &[payload.len() as u32]),
        ]);
        let mut ifd0 = vec![
            t.ascii(tags::MAKE, "SEIKO EPSON CORP."),
            t.ascii(tags::MODEL, "R-D1"),
            t.child(tags::SUB_IFDS, 4, raw),
        ];
        if let Some((black, wb)) = note {
            if t.buf.len() % 2 == 1 {
                t.buf.push(0);
            }
            let m = t.buf.len();
            t.buf.extend(b"EPSON\0\x01\0");
            let mut entries = vec![];
            if !black.is_empty() {
                entries.push(t.long(0x0401, &black));
            }
            entries.push(t.undefined(0x0E80, wb));
            t.ifd(entries);
            let len = (t.buf.len() - m) as u32;
            let exif = t.ifd(vec![Entry { tag: tags::MAKER_NOTE, typ: 7, count: len, value: Value::Field(m as u32) }]);
            ifd0.push(t.child(tags::EXIF_IFD, 4, exif));
        }
        let first = t.ifd(ifd0);
        t.finish(first)
    }

    #[test]
    fn padded_and_tight_rows() {
        let rows = image(13, 3);
        let flat: Vec<u16> = rows.iter().flatten().copied().collect();
        for (c, payload) in [(32769u16, padded_payload(&rows)), (1, tight_payload(&rows))] {
            let data = build(13, c, &payload, 3, None);
            let d = open(&data).unwrap();
            assert_eq!(d.format(), RawFormat::Erf);
            assert_eq!(d.random_access(), RandomAccess::Rows);
            assert_eq!(d.frame_info().bits_per_sample, 12);
            for parallel in [false, true] {
                let p = Profiler::default();
                let mut ctx = DecodeContext::new(&p);
                ctx.parallel = parallel;
                ctx.recover = false;
                assert_eq!(d.decode(&ctx).unwrap().u16(), &flat[..], "compression {c}");
            }
        }
    }

    #[test]
    fn truncation_keeps_complete_samples() {
        let rows = image(13, 3);
        let payload = padded_payload(&rows);
        // Cut inside the second row: group 0 of row 1 is complete, group 1 has bytes for 1 sample.
        let cut = 32 + 16 + 2;
        let data = build_last(13, 32769, &payload[..cut], 3);
        let d = open(&data).unwrap();
        assert_eq!(decode(d.as_ref(), false).0.unwrap_err().kind(), "CorruptFile");
        let (img, w) = decode(d.as_ref(), true);
        let img = img.unwrap();
        let s = img.u16();
        assert_eq!(&s[..13], &rows[0][..]);
        assert_eq!(&s[13..13 + 10], &rows[1][..10]);
        assert_eq!(s[23], rows[1][10]);
        assert!(s[24..].iter().all(|&v| v == 0));
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].samples_filled, 2 + 13);
        // Nothing at all.
        let data = build_last(13, 32769, &[], 3);
        let d = open(&data).unwrap();
        let (img, w) = decode(d.as_ref(), true);
        assert!(img.unwrap().u16().iter().all(|&v| v == 0));
        assert_eq!(w[0].samples_filled, 39);
    }

    #[test]
    fn maker_note_black_and_white_balance() {
        let rows = image(10, 2);
        let mut wb = vec![0u8; 52];
        wb[48..50].copy_from_slice(&29589u16.to_be_bytes());
        wb[50..52].copy_from_slice(&0x8000u16.to_be_bytes());
        let data = build(10, 32769, &padded_payload(&rows), 2, Some((vec![61, 64, 63, 60], wb.clone())));
        let d = open(&data).unwrap();
        let f = d.frame_info();
        assert_eq!(f.black_level.values, vec![61.0, 64.0, 63.0, 60.0]);
        let c = f.white_balance.as_ref().unwrap().coeffs;
        assert_eq!(c[0], 29589.0f32 * 567.0 / 65536.0);
        assert_eq!(c[2], 0x8000 as f32 * 431.0 / 65536.0);
        assert_eq!((c[1], c[3]), (1.0, 1.0));
        // A short blob or too few black levels leave those values out.
        let data = build(10, 32769, &padded_payload(&rows), 2, Some((vec![1, 2, 3], wb[..51].to_vec())));
        let d = open(&data).unwrap();
        assert!(d.frame_info().white_balance.is_none());
        assert_ne!(d.frame_info().black_level.values, vec![1.0, 2.0, 3.0]);
        // Wrong maker note header.
        let mut bad = build(10, 32769, &padded_payload(&rows), 2, Some((vec![61, 64, 63, 60], wb)));
        let at = bad.windows(6).position(|w| w == b"EPSON\0").unwrap();
        bad[at + 4] = b'M';
        let d = open(&bad).unwrap();
        assert!(d.frame_info().white_balance.is_none());
    }

    #[test]
    fn unsupported_packing() {
        let rows = image(10, 1);
        let mut data = build(10, 32769, &padded_payload(&rows), 1, None);
        // Compression 5 instead of 32769.
        let at = data.windows(2).rposition(|w| w == 32769u16.to_be_bytes()).unwrap();
        data[at..at + 2].copy_from_slice(&5u16.to_be_bytes());
        assert_eq!(open(&data).err().unwrap().kind(), "UnsupportedCompression");
    }
}

// ---------------------------------------------------------------------------------------------
// Raspberry Pi
// ---------------------------------------------------------------------------------------------

mod rpi {
    use super::*;

    const DISTANCE: usize = 445_440; // 640x480 IMX219 mode

    /// A JPEG prefix, then a BRCM block with a 10-bit raster of `width` x `height`.
    fn build(width: u16, height: u16, pad: u16, format: u16, order: u8, bayer_format: u8) -> Vec<u8> {
        let mut data = vec![0u8; 0x9000];
        data[..4].copy_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        data[6..10].copy_from_slice(b"Exif");
        let brcm = vec![0u8; DISTANCE];
        let mut file = data;
        file.extend(brcm);
        let at = file.len() - DISTANCE;
        file[at..at + 4].copy_from_slice(b"BRCM");
        let h = at + 0xB0;
        file[h + 32..h + 34].copy_from_slice(&width.to_le_bytes());
        file[h + 34..h + 36].copy_from_slice(&height.to_le_bytes());
        file[h + 36..h + 38].copy_from_slice(&pad.to_le_bytes());
        file[h + 66..h + 68].copy_from_slice(&format.to_le_bytes());
        file[h + 68] = order;
        file[h + 69] = bayer_format;
        file
    }

    #[test]
    fn detection() {
        let ok = build(640, 480, 0, 33, 2, 3);
        assert!(open(&ok).is_ok());
        // Not a Pi file: wrong magic, short, wrong tag, unknown bayer format, raster outside the file.
        let mut bad = ok.clone();
        bad[2] = 0;
        assert!(crate::misc::rpi::open(&bad, &OpenOptions::default()).is_none());
        assert!(crate::misc::rpi::open(&ok[..0x8FFF], &OpenOptions::default()).is_none());
        let at = ok.len() - DISTANCE;
        let mut bad = ok.clone();
        bad[at] = b'X';
        assert!(crate::misc::rpi::open(&bad, &OpenOptions::default()).is_none());
        assert!(crate::misc::rpi::open(&build(640, 480, 0, 33, 2, 9), &OpenOptions::default()).is_none());
        assert!(crate::misc::rpi::open(&build(640, 481, 0, 33, 2, 3), &OpenOptions::default()).is_some());
        assert!(crate::misc::rpi::open(&build(640, 600, 0, 33, 2, 3), &OpenOptions::default()).is_none());
        assert!(crate::misc::rpi::open(&build(0, 480, 0, 33, 2, 3), &OpenOptions::default()).is_none());
    }

    #[test]
    fn ten_bit_raster_and_frame() {
        let mut file = build(640, 480, 0, 33, 1, 3);
        let raster = file.len() - DISTANCE + 0x8000;
        // Row y: groups of 5 bytes with samples y, y+1, ... (high bits in the first 4 bytes).
        for y in 0..480usize {
            for g in 0..160usize {
                let o = raster + y * 800 + g * 5;
                for c in 0..4 {
                    file[o + c] = (y + g + c) as u8;
                }
                file[o + 4] = 0b11_10_01_00;
            }
        }
        let d = open(&file).unwrap();
        let f = d.frame_info();
        assert_eq!((f.width, f.height, f.bits_per_sample), (640, 480, 10));
        assert_eq!(f.cfa, Some(CfaPattern::GBRG));
        assert_eq!(f.white_level.values, vec![1023]);
        assert_eq!(f.active_area, Rect::new(0, 0, 640, 480));
        assert_eq!(d.format(), RawFormat::Other);
        let m = d.metadata();
        assert_eq!(
            (m.make.as_str(), m.canonical_make.as_str(), m.model.as_str()),
            ("RaspberryPi", "Raspberry Pi", "Pi")
        );
        assert_eq!(f.black_level.values, vec![0.0]);
        let (img, w) = decode(d.as_ref(), false);
        let img = img.unwrap();
        assert!(w.is_empty());
        let s = img.u16();
        assert_eq!(&s[..4], &[(0 << 2), (1 << 2) | 1, (2 << 2) | 2, (3 << 2) | 3]);
        let row = 7usize;
        assert_eq!(s[row * 640 + 4], (((row + 1) as u16) << 2));
        // Truncation: the raster is checked at open, the file cannot be cut below it.
        let cut = &file[..file.len() - 100];
        assert!(crate::misc::rpi::open(cut, &OpenOptions::default()).is_none());
    }

    #[test]
    fn other_depths() {
        // 12 bit, 16 bit, 8 bit, 14 bit rasters of the same geometry decode their first row.
        for (fmt, bits) in [(2u8, 8u32), (4, 12), (5, 14), (6, 16)] {
            let mut file = build(64, 4, 0, 33, 0, fmt);
            let raster = file.len() - DISTANCE + 0x8000;
            for i in 0..512 {
                file[raster + i] = (i * 7 + 1) as u8;
            }
            let d = open(&file).unwrap();
            assert_eq!(d.frame_info().bits_per_sample as u32, bits);
            assert_eq!(d.frame_info().cfa, Some(CfaPattern::RGGB));
            let img = decode(d.as_ref(), false).0.unwrap();
            assert_eq!(img.width, 64);
            if bits == 16 {
                assert_eq!(img.u16()[0], u16::from_le_bytes([1, 8]));
            }
            if bits == 8 {
                assert_eq!(img.u16()[1], 8);
            }
        }
        // Without the Bayer format marker the data is 10-bit.
        let d_file = build(64, 4, 0, 0, 3, 6);
        let d = open(&d_file).unwrap();
        assert_eq!(d.frame_info().bits_per_sample, 10);
        assert_eq!(d.frame_info().cfa, Some(CfaPattern::GRBG));
    }
}

// ---------------------------------------------------------------------------------------------
// Sony SRF
// ---------------------------------------------------------------------------------------------

mod srf {
    use super::*;
    use crate::sony::crypt::{SonyCipher, keystream};
    use flashraw_core::tiff::tags;

    fn build(model: &str, width: u32, height: u32, samples: &[u16], pointer: u8, data_offset: usize) -> Vec<u8> {
        let mut t = Tb::new(true);
        let first = t.ifd(vec![
            t.ascii(tags::MAKE, "SONY"),
            t.ascii(tags::MODEL, model),
            t.long(tags::IMAGE_WIDTH, &[width]),
            t.long(tags::IMAGE_LENGTH, &[height]),
        ]);
        let mut file = t.finish(first);
        let master = 0xDEAD_BEEFu32;
        let data_key = 0x1357_9BDFu32;
        file.resize(data_offset + samples.len() * 2, 0);
        file[200_896] = pointer;
        let at = 200_896 + 4 * pointer as usize;
        file[at..at + 4].copy_from_slice(&master.to_be_bytes());
        let mut header = [0u8; 40];
        header[22..26].copy_from_slice(&data_key.to_le_bytes());
        SonyCipher::new(master).apply(&mut header);
        file[164_600..164_640].copy_from_slice(&header);
        let ks = keystream(data_key, samples.len() / 2);
        for (i, (p, k)) in samples.chunks_exact(2).zip(ks).enumerate() {
            let v = ((p[0] as u32) << 16 | p[1] as u32) ^ k;
            file[data_offset + 4 * i..data_offset + 4 * i + 4].copy_from_slice(&v.to_be_bytes());
        }
        file
    }

    fn samples(n: usize) -> Vec<u16> {
        (0..n).map(|i| ((i * 29 + 3) % 0x4000) as u16).collect()
    }

    #[test]
    fn f828_frame_and_decode() {
        let s = samples(16 * 6);
        let data = build("DSC-F828", 16, 6, &s, 3, 862_144);
        let d = open(&data).unwrap();
        let f = d.frame_info();
        assert_eq!(d.format(), RawFormat::Arw);
        assert_eq!((f.width, f.height, f.bits_per_sample), (16, 6, 14));
        assert_eq!(f.cfa, CfaPattern::new(2, 2, &[CfaColor::Emerald, CfaColor::Red, CfaColor::Blue, CfaColor::Green]));
        assert_eq!(f.layout, SensorLayout::OtherCfa);
        assert_eq!(f.white_level.values, vec![0x3ff0]);
        assert_eq!(f.active_area, Rect::new(6, 0, 10, 6));
        // The empty right strip is dropped when the frame is finished.
        assert_eq!(f.masked_areas, vec![Rect::new(0, 0, 6, 6)]);
        assert!(f.black_from_masked);
        assert_eq!(d.metadata().extra, vec![("mode".to_string(), "srf".to_string())]);
        for parallel in [false, true] {
            let p = Profiler::default();
            let mut ctx = DecodeContext::new(&p);
            ctx.parallel = parallel;
            ctx.recover = false;
            assert_eq!(d.decode(&ctx).unwrap().u16(), &s[..]);
        }
    }

    #[test]
    fn v3_geometry() {
        let s = samples(100 * 2);
        let data = build("DSC-V3", 100, 2, &s, 3, 787_392);
        let d = open(&data).unwrap();
        let f = d.frame_info();
        assert_eq!(f.cfa, Some(CfaPattern::GRBG));
        assert_eq!(f.layout, SensorLayout::Bayer);
        assert_eq!(f.active_area, Rect::new(60, 0, 40, 2));
        assert_eq!(f.masked_areas, vec![Rect::new(9, 0, 51, 2)]);
        assert_eq!(decode(d.as_ref(), false).0.unwrap().u16(), &s[..]);
    }

    #[test]
    fn truncation_and_plausibility() {
        let s = samples(16 * 6);
        let data = build("DSC-F828", 16, 6, &s, 3, 862_144);
        // Lose the last row and three bytes.
        let cut = &data[..data.len() - 32 - 3];
        let d = open(cut).unwrap();
        let (r, _) = decode(d.as_ref(), false);
        assert_eq!(r.unwrap_err().kind(), "CorruptFile");
        let (r, w) = decode(d.as_ref(), true);
        let img = r.unwrap();
        // Whole words only; the rest stays zero.
        let present = (16 * 6 * 2 - 35) / 4 * 2;
        assert_eq!(&img.u16()[..present], &s[..present]);
        assert!(img.u16()[present..].iter().all(|&v| v == 0));
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].samples_filled, (16 * 6 - present) as u64);
        // A wrong pointer byte moves the key: the samples are noise and are rejected.
        let mut bad = data.clone();
        bad[200_896] = 4;
        let d = open(&bad).unwrap();
        assert_eq!(decode(d.as_ref(), true).0.unwrap_err().kind(), "CorruptFile");
        // A sixteenth of the samples out of range is accepted, one more is not.
        let mut s2 = samples(16 * 8);
        for v in s2.iter_mut().take(8) {
            *v = 0x4000;
        }
        let d = build("DSC-F828", 16, 8, &s2, 3, 862_144);
        let dec = open(&d).unwrap();
        assert_eq!(decode(dec.as_ref(), true).0.unwrap().u16(), &s2[..]);
        s2[8] = 0xFFFF;
        let d = build("DSC-F828", 16, 8, &s2, 3, 862_144);
        let dec = open(&d).unwrap();
        assert_eq!(decode(dec.as_ref(), true).0.unwrap_err().kind(), "CorruptFile");
    }

    #[test]
    fn rejected_files() {
        let s = samples(16 * 6);
        // Odd width, oversize, zero size.
        for (w, h) in [(15u32, 6u32), (3362, 6), (16, 2461), (0, 6)] {
            let data = build("DSC-F828", w, h, &[], 3, 862_144);
            assert_eq!(open(&data).err().unwrap().kind(), "InvalidMetadata", "{w}x{h}");
        }
        // Key material outside the file.
        let mut data = build("DSC-F828", 16, 6, &s, 3, 862_144);
        data.truncate(200_000);
        assert_eq!(open(&data).err().unwrap().kind(), "CorruptFile");
        // Pointer byte pointing past the end.
        let mut data = build("DSC-F828", 16, 6, &s, 3, 862_144);
        data.truncate(200_900);
        assert!(open(&data).is_err());
    }
}

// ---------------------------------------------------------------------------------------------
// Real files: digests of everything the readers decide (ignored; needs the corpus)
// ---------------------------------------------------------------------------------------------

mod golden {
    use super::*;

    fn corpus() -> std::path::PathBuf {
        match std::env::var_os("FLASHRAW_CORPUS") {
            Some(p) => p.into(),
            None => std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"),
        }
    }

    /// Digest of the metadata and frame description a reader produced.
    fn digest(d: &dyn RawDecoder) -> u64 {
        let (m, f) = (d.metadata(), d.frame_info());
        let s = format!(
            "{:?}|{}|{}|{}|{:?}|{:?}|{}x{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            d.format(),
            m.make,
            m.model,
            m.canonical_model,
            m.extra,
            m.compression,
            f.width,
            f.height,
            f.bits_per_sample,
            f.cfa,
            f.layout,
            f.active_area,
            f.crop,
            f.masked_areas,
            f.black_from_masked,
            f.black_level,
            f.white_level,
            f.white_balance,
            f.color_matrices,
            f.orientation,
            m.previews,
            d.random_access(),
            d.packed_payload().map(|p| (p.width, p.height, p.bits, p.row_stride, p.data.len())),
        );
        fnv(&s)
    }

    /// File (under the corpus) and the digest of its decoder description.
    const FILES: &[(&str, u64)] = &[
        ("Hasselblad/CF132/RAW_HASSELBLAD_IXPRESS_CF132.3FR", 0x5fbf8b1ec3e634a3),
        ("Hasselblad/CFV/RAW_HASSELBLAD_CFV.3FR", 0x473ba527575d89f0),
        ("Hasselblad/H3D/A9391120.3FR", 0xe6f116aa7bf0f7e0),
        ("Hasselblad/Hasselblad H4D/A0000902.3FR", 0xb8fab9c656bd5d17),
        ("Hasselblad/CFV-50c/20200215_BERLIN_OldNew_WestHafen_B0000420.3FR", 0x231e584d9c0584af),
        ("Hasselblad/CFV-50/20170401_CCSG_on_Hasselblad_CFV-50-0002297.fff", 0x51847c6159fb9e4e),
        ("Hasselblad/H4D-50/2-9-2017_street_0012.fff", 0x922490d24f14500e),
        ("Hasselblad/CFV-50c/B0002648.fff", 0x2b63f7e9ef741d13),
        ("Hasselblad/H5D-40/20151114_CCSG_on_Hasselblad_H5D40-0726.fff", 0xfa2cf94a610d346e),
        ("Hasselblad/Hasselblad H4D-40/Job_3583.fff", 0x33fbc0b252adbb6b),
        ("Hasselblad/Hasselblad H6D-100cMS/h6d400c_1shot_0007.fff", 0x3f313c01094c2d4b),
        ("Hasselblad/X1D/B9999910.3FR", 0xf5499b9e2d091c26),
        ("Hasselblad/CFV 100CElectronic Shutter/B0003499.3FR", 0x8845315091de972e),
        ("Epson/R-D1/_EPS0672.ERF", 0x4fc5976505c1b417),
        ("Epson/R-D1s/EPSN3575.ERF", 0xa2c9e685d51e6461),
        ("Epson/R-D1x/_EPS1919.ERF", 0xea2bfb67dbe6e2ff),
        ("RaspberryPi/RP_imx219/Pi_V2.1.raw", 0x928f9bf0b9ff0c1d),
        ("RaspberryPi/RP_imx477/pi.raw", 0x472a231de376e65a),
        ("RaspberryPi/RP_ov5647/Pi_V1.3.raw", 0x6ef3feef31ba8b5f),
        ("RaspberryPi/RP_ov5647/Pi_V1.3_picamera.raw", 0xdeafdfd2b9a0d856),
        ("Sony/DSC-F828/DSC06227.SRF", 0x3c3a9ef9ab99ab89),
    ];

    #[test]
    #[ignore = "needs the corpus (FLASHRAW_CORPUS)"]
    fn real_files_keep_their_description() {
        let root = corpus();
        let mut seen = 0;
        let mut wrong = Vec::new();
        for (rel, want) in FILES {
            let Ok(data) = std::fs::read(root.join(rel)) else { continue };
            seen += 1;
            let d = open(&data).unwrap();
            let got = digest(d.as_ref());
            if std::env::var_os("FLASHRAW_PRINT_DIGESTS").is_some() {
                println!("        (\"{rel}\", {got:#018x}),");
            } else if got != *want {
                wrong.push(format!("{rel}: {got:#018x}"));
            }
        }
        assert!(seen > 0 || std::env::var_os("FLASHRAW_ALLOW_NO_CORPUS").is_some(), "corpus not found");
        assert!(wrong.is_empty(), "description changed: {wrong:#?}");
    }
}
