//! The EXIF and XMP metadata of a RAW file as bytes, for other metadata parsers.
//!
//! [`exif`] returns a standalone TIFF stream (`II*\0` or `MM\0*`, what a JPEG's APP1 segment holds
//! after `Exif\0\0`): IFD0 with the EXIF, GPS and interoperability directories and the maker note.
//! Where it comes from:
//!
//! - TIFF-based files (DNG, CR2, NEF, ARW, ORF, RW2, PEF and most others) and the TIFF inside an
//!   MRW: the directories are rebuilt into a new stream. Pointers to image data (strips, tiles,
//!   sub-images, embedded JPEGs) are left out. The maker note is copied at the offset it had in
//!   the file, relative to the TIFF header, because several vendors (Canon, Sony, Panasonic,
//!   Pentax, older Olympus) store offsets relative to that header inside it. The bytes in front
//!   of it are zero, or hold the rebuilt directories when they fit.
//! - CR3: the four TIFF streams of the Canon metadata box (IFD0, EXIF, maker note, GPS) merged
//!   into one, with the maker note's own TIFF at the start so its offsets stay valid.
//! - Other files (RAF, X3F, ...): the APP1 EXIF segment of the embedded JPEG preview, verbatim.
//!
//! [`xmp`] returns the XMP packet: TIFF tag 700, the CR3 XMP box or a JPEG preview's XMP segment.

use flashraw_core::bmff;
use flashraw_core::bytes::{ByteOrder, ReadAt};
use flashraw_core::metadata::{PreviewEncoding, PreviewInfo};
use flashraw_core::tiff::{self, Entry, Ifd, Tiff, TiffOptions, tags, type_size, types};
use flashraw_core::types::RawFormat;

/// Interoperability IFD pointer (EXIF IFD).
const INTEROP_IFD: u16 = 0xA005;
/// JPEG preview stored as an IFD0 value by Panasonic RW2 (`JpgFromRaw`).
const PANASONIC_JPG_FROM_RAW: u16 = 0x002E;
/// The XMP box of CR3 files (top level).
const XMP_UUID: [u8; 16] =
    [0xbe, 0x7a, 0xcf, 0xcb, 0x97, 0xa9, 0x42, 0xe8, 0x9c, 0x71, 0x99, 0x94, 0x91, 0xe3, 0xaf, 0xac];
/// A maker note further than this from the TIFF header is moved next to the directories instead
/// of being kept at its offset (which would mean that many zero bytes in front of it).
const MAX_MAKER_NOTE_OFFSET: usize = 4 << 20;

/// The file's EXIF as a standalone TIFF stream, or `None` when the file has none.
pub fn exif(data: &[u8], format: RawFormat, previews: &[PreviewInfo]) -> Option<Vec<u8>> {
    let built = match format {
        RawFormat::Cr3 => cr3_exif(data),
        RawFormat::Mrw => crate::minolta::parse_blocks(data, 0)
            .ok()
            .and_then(|b| b.ttw)
            .and_then(|(o, l)| data.get(o..o.checked_add(l)?))
            .and_then(tiff_exif),
        RawFormat::Raf | RawFormat::X3f | RawFormat::Crw => None,
        _ => tiff_exif(data),
    };
    built.or_else(|| jpeg_previews(data, previews).find_map(|j| jpeg_segment(j, b"Exif\0\0")).map(<[u8]>::to_vec))
}

/// The file's XMP packet (UTF-8 XML), or `None` when the file has none.
pub fn xmp<'a>(data: &'a [u8], format: RawFormat, previews: &[PreviewInfo]) -> Option<&'a [u8]> {
    let found = match format {
        RawFormat::Cr3 => bmff::boxes(data).map_while(|b| b.ok()).find(|b| b.uuid == Some(XMP_UUID)).map(|b| b.data),
        RawFormat::Raf | RawFormat::X3f | RawFormat::Crw | RawFormat::Mrw => None,
        _ => tiff_xmp(data),
    };
    found
        .or_else(|| jpeg_previews(data, previews).find_map(|j| jpeg_segment(j, b"http://ns.adobe.com/xap/1.0/\0")))
        .map(trim_packet)
        .filter(|x| !x.is_empty())
}

fn trim_packet(x: &[u8]) -> &[u8] {
    let end = x.iter().rposition(|&b| b != 0 && !b.is_ascii_whitespace()).map_or(0, |p| p + 1);
    &x[..end]
}

/// The JPEG previews, largest first.
fn jpeg_previews<'a>(data: &'a [u8], previews: &[PreviewInfo]) -> impl Iterator<Item = &'a [u8]> {
    let mut p: Vec<&PreviewInfo> = previews.iter().filter(|p| p.encoding == PreviewEncoding::Jpeg).collect();
    p.sort_by_key(|p| std::cmp::Reverse(p.width as u64 * p.height as u64));
    let slices: Vec<&'a [u8]> =
        p.into_iter().filter_map(|p| data.get(p.offset as usize..(p.offset.checked_add(p.length)? as usize))).collect();
    slices.into_iter()
}

/// The body of the first APP1 segment of `jpeg` that starts with `prefix`, without the prefix.
fn jpeg_segment<'a>(jpeg: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if jpeg.get(..2) != Some(&[0xFF, 0xD8][..]) {
        return None;
    }
    let mut pos = 2usize;
    while pos + 4 <= jpeg.len() {
        if jpeg[pos] != 0xFF {
            return None;
        }
        let marker = jpeg[pos + 1];
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let len = u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]) as usize;
        if len < 2 {
            return None;
        }
        let body = jpeg.get(pos + 4..pos + 2 + len)?;
        if marker == 0xE1 && body.starts_with(prefix) {
            let b = &body[prefix.len()..];
            return (!b.is_empty()).then_some(b);
        }
        pos += 2 + len;
    }
    None
}

fn tiff_order(data: &[u8]) -> Option<ByteOrder> {
    match data.get(0..2)? {
        b"II" => Some(ByteOrder::Little),
        b"MM" => Some(ByteOrder::Big),
        _ => None,
    }
}

/// The IFD of the top-level chain that describes the photo: the first with an EXIF or GPS
/// pointer, else the first with a camera make.
fn main_ifd<'t, 'a>(t: &'t Tiff<'a>) -> Option<&'t Ifd<'a>> {
    t.ifds.iter().find(|i| i.exif.is_some() || i.gps.is_some()).or_else(|| t.ifds.iter().find(|i| i.has(tags::MAKE)))
}

fn tiff_xmp(data: &[u8]) -> Option<&[u8]> {
    tiff_order(data)?;
    let opts = TiffOptions { lenient: true, ..TiffOptions::default() };
    let t = Tiff::parse_with(data, opts).ok()?;
    t.ifds.iter().find_map(|i| i.get(tags::XMP)).map(|e| e.bytes())
}

fn tiff_exif(data: &[u8]) -> Option<Vec<u8>> {
    tiff_order(data)?;
    let opts = TiffOptions { lenient: true, ..TiffOptions::default() };
    let t = Tiff::parse_with(data, opts).ok()?;
    let ifd0 = main_ifd(&t)?;
    let exif = ifd0.exif.as_deref();
    let maker_note = exif.and_then(|e| e.get(tags::MAKER_NOTE)).map(|mn| MakerNote {
        space: data,
        at: mn.value_pos,
        len: mn.bytes().len(),
    });
    let interop = exif.and_then(|e| interop(data, e));
    Some(write(t.order, ifd0, exif, ifd0.gps.as_deref(), interop.as_ref(), maker_note))
}

fn interop<'a>(data: &'a [u8], exif: &Ifd<'a>) -> Option<Ifd<'a>> {
    let o = exif.u32(INTEROP_IFD)?;
    let opts = TiffOptions { follow_sub_ifds: false, lenient: true, ..TiffOptions::default() };
    tiff::parse_ifd_at(data, exif.order, o, opts).ok()
}

fn cr3_exif(data: &[u8]) -> Option<Vec<u8>> {
    let cr3 = crate::canon::cr3::container::Cr3::parse(data).ok()?;
    let opts = TiffOptions { follow_sub_ifds: false, lenient: true, ..TiffOptions::default() };
    let first =
        |i: usize| cr3.cmt[i].and_then(|d| Tiff::parse_with(d, opts).ok()).and_then(|t| t.ifds.into_iter().next());
    let ifd0 = first(0)?;
    let exif = first(1);
    let gps = first(3);
    let interop = exif.as_ref().and_then(|e| interop(cr3.cmt[1]?, e));
    // The maker note is a TIFF of its own whose offsets count from its header: keep that header
    // at the start of the output, the maker note IFD where its header points.
    let maker_note = cr3.cmt[2].and_then(|d| {
        let order = tiff_order(d)?;
        let at = d.u32_at(order, 4).ok()? as usize;
        (at >= 8 && at < d.len()).then_some(MakerNote { space: d, at, len: d.len() - at })
    });
    Some(write(ifd0.order, &ifd0, exif.as_ref(), gps.as_ref(), interop.as_ref(), maker_note))
}

/// A maker note: `len` bytes at `at` of `space`, the bytes its internal offsets count from.
struct MakerNote<'a> {
    space: &'a [u8],
    at: usize,
    len: usize,
}

/// One entry of an output directory.
struct Field<'a> {
    tag: u16,
    typ: u16,
    count: u32,
    bytes: std::borrow::Cow<'a, [u8]>,
    /// The value is the offset of another directory or of the maker note, set at layout time.
    pointer: Option<Target>,
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Exif,
    Gps,
    Interop,
    MakerNote,
}

/// Tags of IFD0 that point to image data or to directories handled here.
fn dropped_from_ifd0(e: &Entry) -> bool {
    matches!(
        e.tag,
        tags::STRIP_OFFSETS
            | tags::STRIP_BYTE_COUNTS
            | tags::TILE_OFFSETS
            | tags::TILE_BYTE_COUNTS
            | tags::SUB_IFDS
            | tags::EXIF_IFD
            | tags::GPS_IFD
            | INTEROP_IFD
            | 0x0201 // JPEGInterchangeFormat
            | 0x0202 // JPEGInterchangeFormatLength
            | PANASONIC_JPG_FROM_RAW
    ) || e.typ == types::IFD
        // DNGPrivateData is opaque bytes in DNG; Sony stores a pointer to its private IFD there.
        || (e.tag == 0xC634 && !matches!(e.typ, types::BYTE | types::UNDEFINED))
}

fn fields<'a>(ifd: &Ifd<'a>, out: ByteOrder, keep: impl Fn(&Entry) -> bool) -> Vec<Field<'a>> {
    let mut f: Vec<Field<'a>> = Vec::new();
    for e in ifd.entries.iter().filter(|e| keep(e)) {
        let sz = type_size(e.typ);
        let n = e.len();
        if sz == 0 || n == 0 || f.iter().any(|x| x.tag == e.tag) {
            continue;
        }
        let raw = &e.data[..n * sz];
        let bytes =
            if e.order == out { std::borrow::Cow::Borrowed(raw) } else { std::borrow::Cow::Owned(swap(raw, e.typ)) };
        f.push(Field { tag: e.tag, typ: e.typ, count: n as u32, bytes, pointer: None });
    }
    f
}

/// Reverse the byte order of every value of TIFF type `typ` in `raw`.
fn swap(raw: &[u8], typ: u16) -> Vec<u8> {
    let unit = match typ {
        types::SHORT | types::SSHORT => 2,
        types::LONG | types::SLONG | types::FLOAT | types::IFD | types::RATIONAL | types::SRATIONAL => 4,
        types::DOUBLE | types::LONG8 | 17 | 18 => 8,
        _ => 1,
    };
    let mut v = raw.to_vec();
    for c in v.chunks_exact_mut(unit) {
        c.reverse();
    }
    v
}

fn pointer<'a>(tag: u16, to: Target, typ: u16, count: u32) -> Field<'a> {
    Field { tag, typ, count, bytes: std::borrow::Cow::Owned(vec![0; 4]), pointer: Some(to) }
}

/// Bytes of a directory and its out-of-line values.
fn dir_size(f: &[Field]) -> usize {
    2 + 12 * f.len()
        + 4
        + f.iter()
            .filter(|x| x.pointer.is_none() && x.bytes.len() > 4)
            .map(|x| x.bytes.len().next_multiple_of(2))
            .sum::<usize>()
}

fn write(
    order: ByteOrder,
    ifd0: &Ifd,
    exif: Option<&Ifd>,
    gps: Option<&Ifd>,
    interop: Option<&Ifd>,
    maker_note: Option<MakerNote>,
) -> Vec<u8> {
    let maker_note = maker_note.filter(|m| m.len > 0 && m.at.checked_add(m.len).is_some_and(|e| e <= m.space.len()));
    let mut d0 = fields(ifd0, order, |e| !dropped_from_ifd0(e) && !is_embedded_jpeg(e));
    let mut dx =
        exif.map(|e| fields(e, order, |e| e.tag != tags::MAKER_NOTE && e.tag != INTEROP_IFD)).unwrap_or_default();
    let dg = gps.map(|g| fields(g, order, |_| true)).unwrap_or_default();
    let di = interop.map(|i| fields(i, order, |_| true)).unwrap_or_default();
    if let Some(m) = &maker_note {
        dx.push(pointer(tags::MAKER_NOTE, Target::MakerNote, types::UNDEFINED, m.len as u32));
    }
    if !di.is_empty() {
        dx.push(pointer(INTEROP_IFD, Target::Interop, types::LONG, 1));
    }
    if !dx.is_empty() {
        d0.push(pointer(tags::EXIF_IFD, Target::Exif, types::LONG, 1));
    }
    if !dg.is_empty() {
        d0.push(pointer(tags::GPS_IFD, Target::Gps, types::LONG, 1));
    }
    for d in [&mut d0, &mut dx] {
        d.sort_by_key(|f| f.tag);
    }
    let dg = sorted(dg);
    let di = sorted(di);

    // Directories go in one block; the maker note keeps its offset when it can.
    let block = dir_size(&d0) + [&dx, &dg, &di].iter().filter(|d| !d.is_empty()).map(|d| dir_size(d)).sum::<usize>();
    let (block_at, mn_at) = match &maker_note {
        Some(m) if m.at >= 8 && m.at <= MAX_MAKER_NOTE_OFFSET => {
            if 8 + block <= m.at {
                (8, Some(m.at))
            } else {
                ((m.at + m.len).next_multiple_of(2), Some(m.at))
            }
        }
        Some(_) => (8, Some((8 + block).next_multiple_of(2))),
        None => (8, None),
    };
    let mut pos = block_at;
    let mut at = |d: &Vec<Field>| {
        if d.is_empty() {
            return 0;
        }
        let p = pos;
        pos += dir_size(d);
        p
    };
    let (o0, ox, og, oi) = (at(&d0), at(&dx), at(&dg), at(&di));
    let end = pos.max(mn_at.zip(maker_note.as_ref()).map_or(0, |(a, m)| a + m.len));
    let mut out = vec![0u8; end];
    out[..2].copy_from_slice(if order == ByteOrder::Little { b"II" } else { b"MM" });
    put16(&mut out, 2, order, 42);
    put32(&mut out, 4, order, o0 as u32);
    let target = |t: Target| match t {
        Target::Exif => ox,
        Target::Gps => og,
        Target::Interop => oi,
        Target::MakerNote => mn_at.unwrap_or(0),
    } as u32;
    for (d, o) in [(&d0, o0), (&dx, ox), (&dg, og), (&di, oi)] {
        if !d.is_empty() {
            put_dir(&mut out, o, order, d, &target);
        }
    }
    if let (Some(m), Some(a)) = (&maker_note, mn_at) {
        out[a..a + m.len].copy_from_slice(&m.space[m.at..m.at + m.len]);
    }
    out
}

fn sorted(mut d: Vec<Field>) -> Vec<Field> {
    d.sort_by_key(|f| f.tag);
    d
}

/// A JPEG image stored as a tag value (not metadata).
fn is_embedded_jpeg(e: &Entry) -> bool {
    e.data.len() > 64 << 10 && e.data.starts_with(&[0xFF, 0xD8])
}

fn put16(out: &mut [u8], p: usize, o: ByteOrder, v: u16) {
    out[p..p + 2].copy_from_slice(&if o == ByteOrder::Little { v.to_le_bytes() } else { v.to_be_bytes() });
}

fn put32(out: &mut [u8], p: usize, o: ByteOrder, v: u32) {
    out[p..p + 4].copy_from_slice(&if o == ByteOrder::Little { v.to_le_bytes() } else { v.to_be_bytes() });
}

fn put_dir(out: &mut [u8], at: usize, order: ByteOrder, d: &[Field], target: &impl Fn(Target) -> u32) {
    put16(out, at, order, d.len() as u16);
    let mut values = at + 2 + 12 * d.len() + 4;
    for (i, f) in d.iter().enumerate() {
        let p = at + 2 + 12 * i;
        put16(out, p, order, f.tag);
        put16(out, p + 2, order, f.typ);
        put32(out, p + 4, order, f.count);
        if let Some(t) = f.pointer {
            put32(out, p + 8, order, target(t));
        } else if f.bytes.len() <= 4 {
            out[p + 8..p + 8 + f.bytes.len()].copy_from_slice(&f.bytes);
        } else {
            put32(out, p + 8, order, values as u32);
            out[values..values + f.bytes.len()].copy_from_slice(&f.bytes);
            values += f.bytes.len().next_multiple_of(2);
        }
    }
    put32(out, at + 2 + 12 * d.len(), order, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian TIFF with an IFD0 (Make, Model, Orientation, StripOffsets, EXIF pointer)
    /// and an EXIF IFD (ISO, maker note), magic `magic`. Returns the file and the maker note offset.
    fn sample(magic: u16) -> (Vec<u8>, usize) {
        let mut f = vec![b'I', b'I'];
        f.extend_from_slice(&magic.to_le_bytes());
        f.extend_from_slice(&8u32.to_le_bytes());
        let entry = |f: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32| {
            for b in [&tag.to_le_bytes()[..], &typ.to_le_bytes(), &count.to_le_bytes(), &value.to_le_bytes()] {
                f.extend_from_slice(b);
            }
        };
        // IFD0 at 8: 5 entries, values from 8 + 2 + 60 + 4 = 74.
        f.extend_from_slice(&5u16.to_le_bytes());
        entry(&mut f, tags::MAKE, types::ASCII, 6, 74);
        entry(&mut f, tags::MODEL, types::ASCII, 8, 80);
        entry(&mut f, tags::STRIP_OFFSETS, types::LONG, 2, 88);
        entry(&mut f, tags::ORIENTATION, types::SHORT, 1, 6);
        entry(&mut f, tags::EXIF_IFD, types::LONG, 1, 96);
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(b"Maker\0Model 1\0");
        f.extend_from_slice(&[0xE8, 3, 0, 0, 0xD0, 7, 0, 0]);
        assert_eq!(f.len(), 96);
        // EXIF IFD at 96: ISO and a 6-byte maker note at 126.
        f.extend_from_slice(&2u16.to_le_bytes());
        entry(&mut f, 0x8827, types::SHORT, 1, 400);
        entry(&mut f, tags::MAKER_NOTE, types::UNDEFINED, 6, 126);
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(b"VENDOR");
        (f, 126)
    }

    #[test]
    fn rebuilt_stream_keeps_tags_and_the_maker_note_offset() {
        // 0x55 is the RW2 magic: the output has the standard one.
        for magic in [42u16, 0x55] {
            let (src, mn_at) = sample(magic);
            let out = tiff_exif(&src).expect("exif");
            assert_eq!(&out[..4], b"II*\0");
            let t = Tiff::parse(&out).unwrap();
            let i = &t.ifds[0];
            assert_eq!(i.string(tags::MAKE).as_deref(), Some("Maker"));
            assert_eq!(i.string(tags::MODEL).as_deref(), Some("Model 1"));
            assert_eq!(i.u32(tags::ORIENTATION), Some(6));
            assert!(!i.has(tags::STRIP_OFFSETS));
            let e = i.exif.as_ref().expect("exif ifd");
            assert_eq!(e.u32(0x8827), Some(400));
            let mn = e.get(tags::MAKER_NOTE).unwrap();
            assert_eq!(mn.bytes(), b"VENDOR");
            assert_eq!(mn.value_pos, mn_at, "the maker note keeps its offset");
        }
    }

    #[test]
    fn byte_order_swap_per_value() {
        assert_eq!(swap(&[1, 2, 3, 4], types::SHORT), vec![2, 1, 4, 3]);
        assert_eq!(swap(&[1, 2, 3, 4, 5, 6, 7, 8], types::RATIONAL), vec![4, 3, 2, 1, 8, 7, 6, 5]);
        assert_eq!(swap(&[1, 2, 3], types::ASCII), vec![1, 2, 3]);
    }

    #[test]
    fn jpeg_app1_segments() {
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1, 0, 12];
        j.extend_from_slice(b"Exif\0\0II*\0");
        j.extend_from_slice(&[0xFF, 0xDA]);
        assert_eq!(jpeg_segment(&j, b"Exif\0\0"), Some(&b"II*\0"[..]));
        assert_eq!(jpeg_segment(&j, b"http://ns.adobe.com/xap/1.0/\0"), None);
    }
}
