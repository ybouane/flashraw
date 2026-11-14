//! CR3 container: ISO BMFF with Canon boxes.
//!
//! A CR3 file is an ISO base media file (ISO/IEC 14496-12). The movie box holds one `trak` per
//! stored image (a JPEG, a small and a full-size CRX raw, in dual-pixel files a second raw) plus a
//! timed-metadata track, and a Canon `uuid` box with the EXIF directories (`CMT1`..`CMT4`) and the
//! thumbnail. The medium preview sits in a second Canon `uuid` box at the top level.

use flashraw_codecs::canon_crx::CrxHeader;
use flashraw_core::bmff::{BmffBox, boxes};
use flashraw_core::{Error, Result};

/// UUID of the Canon metadata box (`CNCV`, `CMT1`..`CMT4`, `THMB`).
pub const CANON_UUID: [u8; 16] =
    [0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
/// UUID of the box that holds the `PRVW` medium preview.
pub const PREVIEW_UUID: [u8; 16] =
    [0xea, 0xf4, 0x2b, 0x5e, 0x1c, 0x98, 0x4b, 0x88, 0xb9, 0xfb, 0xb7, 0xdc, 0x40, 0x6e, 0x4d, 0x16];

/// Sample tables with more entries than this are ignored: no real file has more than a handful, and
/// the bound keeps a damaged table from costing memory.
const MAX_TABLE_ENTRIES: usize = 1_000_000;

/// What a track carries.
#[derive(Debug, Clone, PartialEq)]
pub enum Media {
    Raw(CrxHeader),
    Jpeg,
    Heif,
    TimedMetadata,
    Other,
}

/// One `trak` of the movie box.
#[derive(Debug, Clone)]
pub struct Track {
    pub media: Media,
    /// Width and height of the visual sample entry.
    pub entry_width: u16,
    pub entry_height: u16,
    /// Per-sample sizes (`stsz`); empty when `fixed_size` is non-zero.
    pub sample_sizes: Vec<u32>,
    pub fixed_size: u32,
    pub sample_count: u32,
    pub chunk_offsets: Vec<u64>,
    /// `(first_chunk, samples_per_chunk)` pairs of `stsc`.
    pub stsc: Vec<(u32, u32)>,
}

impl Track {
    /// Size of sample `index`.
    fn size(&self, index: usize) -> Option<u64> {
        if self.fixed_size != 0 {
            (index < self.sample_count as usize).then_some(self.fixed_size as u64)
        } else {
            self.sample_sizes.get(index).map(|&s| s as u64)
        }
    }

    /// `(file offset, size)` of sample `index`, or `None`.
    ///
    /// Samples are numbered from 0 across the chunks. Chunk `c` (counted from 1) holds the
    /// `samples_per_chunk` of the last `stsc` entry whose `first_chunk <= c` (chunks before the first
    /// entry use the first one; without any entry a chunk holds one sample). A sample's offset is its
    /// chunk's offset plus the sizes of the samples before it in that chunk. Sample 0 is always at the
    /// first chunk offset.
    pub fn sample(&self, index: usize) -> Option<(u64, u64)> {
        let size = self.size(index)?;
        if index == 0 {
            return Some((*self.chunk_offsets.first()?, size));
        }
        let mut entry = 0usize;
        let mut per_chunk = self.stsc.first().map_or(1, |e| e.1);
        let mut first_in_chunk = 0usize;
        for (c, &chunk_offset) in self.chunk_offsets.iter().enumerate() {
            while self.stsc.get(entry).is_some_and(|e| e.0 as usize <= c + 1) {
                per_chunk = self.stsc[entry].1;
                entry += 1;
            }
            let next_first = first_in_chunk.saturating_add(per_chunk as usize);
            if index < next_first {
                let mut offset = chunk_offset;
                for earlier in first_in_chunk..index {
                    offset = offset.checked_add(self.size(earlier)?)?;
                }
                return Some((offset, size));
            }
            first_in_chunk = next_first;
        }
        None
    }
}

/// Parsed CR3 structure (borrowing the file).
#[derive(Debug, Default)]
pub struct Cr3<'a> {
    /// Payloads of `CMT1`..`CMT4` (TIFF streams: IFD0, EXIF, Canon maker note, GPS).
    pub cmt: [Option<&'a [u8]>; 4],
    /// `THMB`: `(width, height, jpeg)`.
    pub thumbnail: Option<(u16, u16, &'a [u8])>,
    /// `PRVW`: `(width, height, jpeg)`.
    pub preview: Option<(u16, u16, &'a [u8])>,
    pub tracks: Vec<Track>,
}

/// True for an ISO BMFF file whose `ftyp` major brand is `crx `.
pub fn is_cr3(data: &[u8]) -> bool {
    data.get(4..12) == Some(b"ftypcrx ")
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn be64(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// Children of a box up to the first one that cannot be parsed.
fn children<'a>(b: &BmffBox<'a>, skip: usize) -> impl Iterator<Item = BmffBox<'a>> {
    b.children(skip).map_while(|c| c.ok())
}

/// The big-endian table of `count` entries read from `at`, as long as the data lasts.
fn table<T>(d: &[u8], at: usize, step: usize, read: impl Fn(&[u8], usize) -> Option<T>) -> Vec<T> {
    let count = be32(d, 4).unwrap_or(0) as usize;
    if count > MAX_TABLE_ENTRIES {
        return Vec::new();
    }
    let available = d.len().saturating_sub(at) / step;
    (0..count.min(available)).map_while(|i| read(d, at + i * step)).collect()
}

impl<'a> Cr3<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Cr3<'a>> {
        if !is_cr3(data) {
            return Err(Error::UnsupportedFormat("not a CR3 file".into()));
        }
        let mut moov = None;
        let mut cr3 = Cr3::default();
        for b in boxes(data).map_while(|b| b.ok()) {
            if b.is(b"moov") {
                moov = Some(b);
            } else if b.uuid == Some(PREVIEW_UUID) {
                cr3.preview = preview_box(b.data);
            }
        }
        let moov = moov.ok_or_else(|| Error::corrupt("CR3 without moov box"))?;
        for b in children(&moov, 0) {
            if b.uuid == Some(CANON_UUID) {
                cr3.canon_box(&b);
            } else if b.is(b"trak") {
                cr3.tracks.push(track(&b));
            }
        }
        Ok(cr3)
    }

    /// `CMT1`..`CMT4` and `THMB` of the Canon metadata box.
    fn canon_box(&mut self, canon: &BmffBox<'a>) {
        for b in children(canon, 0) {
            match &b.typ {
                b"CMT1" => self.cmt[0] = Some(b.data),
                b"CMT2" => self.cmt[1] = Some(b.data),
                b"CMT3" => self.cmt[2] = Some(b.data),
                b"CMT4" => self.cmt[3] = Some(b.data),
                // Version and flags, width and height, JPEG size and four reserved bytes, then the JPEG.
                b"THMB" if b.data.len() > 16 => {
                    if let (Some(w), Some(h)) = (be16(b.data, 4), be16(b.data, 6)) {
                        self.thumbnail = Some((w, h, &b.data[16..]));
                    }
                }
                _ => {}
            }
        }
    }

    /// The raw track to decode and its index: the first with the largest `bits * width * height`.
    pub fn main_raw(&self) -> Option<(usize, &Track, &CrxHeader)> {
        let mut best: Option<(u64, usize, &Track, &CrxHeader)> = None;
        for (i, t) in self.tracks.iter().enumerate() {
            if let Media::Raw(h) = &t.media {
                let size = h.bits as u64 * h.width as u64 * h.height as u64;
                if best.is_none_or(|(s, ..)| size > s) {
                    best = Some((size, i, t, h));
                }
            }
        }
        best.map(|(_, i, t, h)| (i, t, h))
    }

    /// Number of raw images in the file (dual-pixel / burst).
    pub fn raw_count(&self) -> usize {
        let Some((_, track, best)) = self.main_raw() else { return 0 };
        if track.sample_count > 1 {
            return track.sample_count as usize;
        }
        let size = best.bits as u64 * best.width as u64 * best.height as u64;
        self.tracks
            .iter()
            .filter(|t| matches!(&t.media, Media::Raw(h) if h.bits as u64 * h.width as u64 * h.height as u64 == size))
            .count()
    }
}

/// The medium preview: eight bytes, a `PRVW` box (size, name, zero, one, width, height, two, JPEG size)
/// and the JPEG from byte 32.
fn preview_box(payload: &[u8]) -> Option<(u16, u16, &[u8])> {
    if payload.len() <= 32 || payload.get(12..16) != Some(b"PRVW") {
        return None;
    }
    Some((be16(payload, 22)?, be16(payload, 24)?, &payload[32..]))
}

/// One `trak`: its handler, the sample entry and the sample tables.
fn track(trak: &BmffBox) -> Track {
    let mut t = Track {
        media: Media::Other,
        entry_width: 0,
        entry_height: 0,
        sample_sizes: Vec::new(),
        fixed_size: 0,
        sample_count: 0,
        chunk_offsets: Vec::new(),
        stsc: Vec::new(),
    };
    let Some(mdia) = trak.child(0, b"mdia") else { return t };
    // The handler type is bytes 8..12 of the `hdlr` payload: `vide` for images, `meta` for timed metadata.
    let handler = mdia.child(0, b"hdlr").and_then(|h| h.data.get(8..12).map(|b| [b[0], b[1], b[2], b[3]]));
    let Some(stbl) = mdia.child(0, b"minf").and_then(|m| m.child(0, b"stbl")) else { return t };
    for b in children(&stbl, 0) {
        match &b.typ {
            b"stsd" => sample_entry(&mut t, &b, handler == Some(*b"meta")),
            b"stsz" => {
                t.fixed_size = be32(b.data, 4).unwrap_or(0);
                t.sample_count = be32(b.data, 8).unwrap_or(0);
                t.sample_sizes = if t.fixed_size != 0 || t.sample_count as usize > MAX_TABLE_ENTRIES {
                    Vec::new()
                } else {
                    // Per-sample sizes follow the count.
                    let available = b.data.len().saturating_sub(12) / 4;
                    (0..(t.sample_count as usize).min(available)).filter_map(|i| be32(b.data, 12 + i * 4)).collect()
                };
            }
            b"stsc" => {
                t.stsc = table(b.data, 8, 12, |d, at| Some((be32(d, at)?, be32(d, at + 4)?)));
            }
            b"co64" => t.chunk_offsets = table(b.data, 8, 8, be64),
            b"stco" => t.chunk_offsets = table(b.data, 8, 4, |d, at| be32(d, at).map(u64::from)),
            _ => {}
        }
    }
    t
}

/// The sample description: the first `CRAW` entry gives the picture size and, through its children, the
/// kind of image; a `CTMD` entry of a `meta` track marks the timed metadata.
fn sample_entry(t: &mut Track, stsd: &BmffBox, meta_handler: bool) {
    for entry in children(stsd, 8) {
        if entry.is(b"CTMD") && meta_handler {
            t.media = Media::TimedMetadata;
        } else if entry.is(b"CRAW") {
            t.entry_width = be16(entry.data, 24).unwrap_or(0);
            t.entry_height = be16(entry.data, 26).unwrap_or(0);
            for child in children(&entry, 82) {
                match &child.typ {
                    b"CMP1" => {
                        if let Ok(h) = CrxHeader::parse(child.data) {
                            t.media = Media::Raw(h);
                        }
                    }
                    b"JPEG" => t.media = Media::Jpeg,
                    b"HEVC" | b"hvcC" => t.media = Media::Heif,
                    _ => {}
                }
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(t: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(t);
        v.extend_from_slice(payload);
        v
    }

    fn track_with(chunks: &[u64], stsc: &[(u32, u32)], sizes: &[u32]) -> Track {
        Track {
            media: Media::Other,
            entry_width: 0,
            entry_height: 0,
            sample_sizes: sizes.to_vec(),
            fixed_size: 0,
            sample_count: sizes.len() as u32,
            chunk_offsets: chunks.to_vec(),
            stsc: stsc.to_vec(),
        }
    }

    #[test]
    fn samples_follow_chunks_and_stsc() {
        let t = track_with(&[100, 1000], &[(1, 2), (2, 1)], &[10, 20, 30]);
        assert_eq!(t.sample(0), Some((100, 10)));
        assert_eq!(t.sample(1), Some((110, 20)));
        assert_eq!(t.sample(2), Some((1000, 30)));
        assert_eq!(t.sample(3), None);
    }

    #[test]
    fn one_sample_per_chunk_without_stsc_and_fixed_sizes() {
        let t = track_with(&[5, 50, 500], &[], &[1, 2, 3]);
        assert_eq!(t.sample(2), Some((500, 3)));
        let fixed = Track { fixed_size: 7, sample_sizes: vec![], sample_count: 2, ..t };
        assert_eq!(fixed.sample(1), Some((50, 7)));
        assert_eq!(fixed.sample(2), None);
    }

    #[test]
    fn rejects_non_cr3() {
        assert!(!is_cr3(b"\0\0\0\x18ftypheic"));
        assert!(matches!(Cr3::parse(b"not a cr3 file at all"), Err(Error::UnsupportedFormat(_))));
    }

    #[test]
    fn needs_a_moov_box() {
        let mut f = bx(b"ftyp", b"crx \0\0\0\x01crx ");
        f.extend(bx(b"mdat", &[0; 16]));
        assert!(is_cr3(&f));
        assert!(matches!(Cr3::parse(&f), Err(Error::CorruptFile { .. })));
    }

    #[test]
    fn canon_box_and_preview_are_read() {
        let mut thmb = vec![0, 0, 0, 0, 0, 0xa0, 0, 0x78, 0, 0, 0, 4, 0, 0, 0, 0];
        thmb.extend([0xff, 0xd8, 0xff, 0xd9]);
        let mut canon = bx(b"CMT1", b"II*\0");
        canon.extend(bx(b"THMB", &thmb));
        let mut uuid = CANON_UUID.to_vec();
        uuid.extend(&canon);
        let moov = bx(b"moov", &bx(b"uuid", &uuid));
        let mut f = bx(b"ftyp", b"crx \0\0\0\x01crx ");
        f.extend(&moov);
        let cr3 = Cr3::parse(&f).unwrap();
        assert_eq!(cr3.cmt[0], Some(&b"II*\0"[..]));
        assert_eq!(cr3.thumbnail.map(|t| (t.0, t.1, t.2.len())), Some((160, 120, 4)));
        assert!(cr3.preview.is_none() && cr3.tracks.is_empty());
        assert_eq!(cr3.raw_count(), 0);
    }

    #[test]
    fn every_truncation_and_flip_of_a_container_is_survived() {
        let mut thmb = vec![0u8; 16];
        thmb.extend([0xff, 0xd8, 0xff, 0xd9]);
        let mut uuid = CANON_UUID.to_vec();
        uuid.extend(bx(b"THMB", &thmb));
        let trak = bx(
            b"trak",
            &bx(b"mdia", &[bx(b"hdlr", &[0; 20]), bx(b"minf", &bx(b"stbl", &bx(b"stsz", &[0; 16])))].concat()),
        );
        let mut f = bx(b"ftyp", b"crx \0\0\0\x01crx ");
        f.extend(bx(b"moov", &[bx(b"uuid", &uuid), trak].concat()));
        for n in 0..f.len() {
            let _ = Cr3::parse(&f[..n]);
            let mut g = f.clone();
            g[n] ^= 0xff;
            let _ = Cr3::parse(&g);
        }
    }
}
