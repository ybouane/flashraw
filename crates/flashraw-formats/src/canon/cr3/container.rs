//! CR3 container: ISO BMFF with Canon boxes.
//!
//! ```text
//! ftyp 'crx '
//! moov
//!   uuid 85c0b687-820f-11e0-8111-f4ce462b6a48   Canon metadata
//!     CNCV (version string) CCTP CTBO
//!     CMT1 (TIFF: IFD0)  CMT2 (TIFF: EXIF)  CMT3 (TIFF: Canon maker note)  CMT4 (TIFF: GPS)
//!     THMB (small JPEG thumbnail)
//!   trak* → mdia → hdlr, minf → stbl → stsd (sample entry 'CRAW' with CMP1 / JPEG / HEVC
//!                                             child, or 'CTMD' timed metadata), stsz, stsc, co64
//! uuid eaf42b5e-1c98-4b88-b9fb-b7dc406e4d16    PRVW (medium JPEG preview)
//! mdat
//! ```
//!
//! Track order is typically: full-size JPEG (or HEVC), small raw, full raw, CTMD,
//! and a second full raw for dual-pixel files.

use flashraw_codecs::canon_crx::CrxHeader;
use flashraw_core::bmff::{self, BmffBox};
use flashraw_core::{Error, Result};

pub const CANON_UUID: [u8; 16] =
    [0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
pub const PREVIEW_UUID: [u8; 16] =
    [0xea, 0xf4, 0x2b, 0x5e, 0x1c, 0x98, 0x4b, 0x88, 0xb9, 0xfb, 0xb7, 0xdc, 0x40, 0x6e, 0x4d, 0x16];

/// Upper bound on table entries accepted from a track (guards against corrupt sample tables).
const MAX_ENTRIES: u32 = 1_000_000;

/// What a track carries.
#[derive(Debug, Clone, PartialEq)]
pub enum Media {
    Raw(CrxHeader),
    Jpeg,
    Heif,
    TimedMetadata,
    Other,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub media: Media,
    /// Sample entry dimensions (CRAW entries).
    pub entry_width: u16,
    pub entry_height: u16,
    /// Per-sample sizes (`stsz`); empty with `fixed_size` when constant.
    pub sample_sizes: Vec<u32>,
    pub fixed_size: u32,
    pub sample_count: u32,
    pub chunk_offsets: Vec<u64>,
    /// `(first_chunk, samples_per_chunk)` (`stsc`).
    pub stsc: Vec<(u32, u32)>,
}

impl Track {
    fn new() -> Self {
        Track {
            media: Media::Other,
            entry_width: 0,
            entry_height: 0,
            sample_sizes: Vec::new(),
            fixed_size: 0,
            sample_count: 0,
            chunk_offsets: Vec::new(),
            stsc: Vec::new(),
        }
    }

    fn sample_size(&self, i: usize) -> Option<u64> {
        if self.fixed_size > 0 { Some(self.fixed_size as u64) } else { self.sample_sizes.get(i).map(|&s| s as u64) }
    }

    /// `(offset, size)` of sample `index` (chunk/sample-to-chunk mapping as in `stsc`).
    pub fn sample(&self, index: usize) -> Option<(u64, u64)> {
        if index == 0 {
            // The first sample starts its first chunk regardless of stsc.
            return Some((*self.chunk_offsets.first()?, self.sample_size(0)?));
        }
        let mut sample = 0usize;
        let mut entry = 0usize;
        for (ci, &chunk_off) in self.chunk_offsets.iter().enumerate() {
            while entry + 1 < self.stsc.len() && self.stsc[entry + 1].0 as usize == ci + 1 {
                entry += 1;
            }
            let per_chunk = self.stsc.get(entry).map(|e| e.1).unwrap_or(1) as usize;
            let mut off = chunk_off;
            for _ in 0..per_chunk {
                let size = self.sample_size(sample)?;
                if sample == index {
                    return Some((off, size));
                }
                off = off.checked_add(size)?;
                sample += 1;
            }
        }
        None
    }
}

/// Parsed CR3 structure (borrowing the file).
#[derive(Debug, Default)]
pub struct Cr3<'a> {
    pub cmt: [Option<&'a [u8]>; 4],
    /// THMB: `(width, height, jpeg)`.
    pub thumbnail: Option<(u16, u16, &'a [u8])>,
    /// PRVW: `(width, height, jpeg)`.
    pub preview: Option<(u16, u16, &'a [u8])>,
    pub tracks: Vec<Track>,
}

fn be16(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
}
fn be32(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}
fn be64(d: &[u8], o: usize) -> Option<u64> {
    d.get(o..o + 8).map(|b| u64::from_be_bytes(b.try_into().unwrap_or([0; 8])))
}

pub fn is_cr3(data: &[u8]) -> bool {
    data.len() >= 12 && &data[4..8] == b"ftyp" && &data[8..12] == b"crx "
}

impl<'a> Cr3<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Cr3<'a>> {
        if !is_cr3(data) {
            return Err(Error::UnsupportedFormat("not a CR3 file".into()));
        }
        let mut cr3 = Cr3::default();
        let mut moov = None;
        for b in bmff::boxes(data) {
            // A broken trailing box (e.g. truncated mdat) does not invalidate what precedes it.
            let Ok(b) = b else { break };
            if b.is(b"moov") {
                moov = Some(b);
            } else if b.uuid == Some(PREVIEW_UUID) {
                cr3.preview = parse_prvw(b.data);
            }
        }
        let moov = moov.ok_or_else(|| Error::corrupt("CR3 without moov box"))?;
        for b in moov.children(0).map_while(|b| b.ok()) {
            if b.uuid == Some(CANON_UUID) {
                cr3.parse_canon(&b);
            } else if b.is(b"trak") {
                cr3.tracks.push(parse_trak(&b));
            }
        }
        Ok(cr3)
    }

    fn parse_canon(&mut self, b: &BmffBox<'a>) {
        for c in b.children(0).map_while(|b| b.ok()) {
            match &c.typ {
                b"CMT1" => self.cmt[0] = Some(c.data),
                b"CMT2" => self.cmt[1] = Some(c.data),
                b"CMT3" => self.cmt[2] = Some(c.data),
                b"CMT4" => self.cmt[3] = Some(c.data),
                b"THMB" if c.data.len() > 16 => {
                    // version/flags, u16 width, u16 height, u32 size, 4 reserved, JPEG.
                    let (w, h) = (be16(c.data, 4).unwrap_or(0), be16(c.data, 6).unwrap_or(0));
                    self.thumbnail = Some((w, h, &c.data[16..]));
                }
                _ => {}
            }
        }
    }

    /// The raw track to decode (the primary image): the first of the tracks with
    /// the largest `bits * width * height`.
    pub fn main_raw(&self) -> Option<(usize, &Track, &CrxHeader)> {
        let mut best: Option<(usize, &Track, &CrxHeader, u64)> = None;
        for (i, t) in self.tracks.iter().enumerate() {
            if let Media::Raw(h) = &t.media {
                let bc = h.bits as u64 * h.width as u64 * h.height as u64;
                if best.as_ref().is_none_or(|b| bc > b.3) {
                    best = Some((i, t, h, bc));
                }
            }
        }
        best.map(|(i, t, h, _)| (i, t, h))
    }

    /// Number of raw images in the file (dual-pixel / burst).
    pub fn raw_count(&self) -> usize {
        let Some((_, t, h)) = self.main_raw() else { return 0 };
        let bc = h.bits as u64 * h.width as u64 * h.height as u64;
        let same: usize = self
            .tracks
            .iter()
            .filter(|t| matches!(&t.media, Media::Raw(x) if x.bits as u64 * x.width as u64 * x.height as u64 == bc))
            .count();
        if t.sample_count > 1 { t.sample_count as usize } else { same }
    }
}

/// PRVW in the preview uuid box: 8 bytes, then the PRVW box (`size, 'PRVW', u32,
/// u16, u16 width, u16 height, u16, u32 size`) followed by the JPEG.
fn parse_prvw(d: &[u8]) -> Option<(u16, u16, &[u8])> {
    if d.get(12..16)? != b"PRVW" || d.len() <= 32 {
        return None;
    }
    Some((be16(d, 22)?, be16(d, 24)?, &d[32..]))
}

fn parse_trak<'a>(trak: &BmffBox<'a>) -> Track {
    let mut t = Track::new();
    let mut handler = [0u8; 4];
    let Some(mdia) = trak.child(0, b"mdia") else { return t };
    if let Some(hdlr) = mdia.child(0, b"hdlr") {
        if let Some(h) = hdlr.data.get(8..12) {
            handler.copy_from_slice(h);
        }
    }
    let Some(stbl) = mdia.child(0, b"minf").and_then(|m| m.child(0, b"stbl")) else { return t };
    for b in stbl.children(0).map_while(|b| b.ok()) {
        match &b.typ {
            b"stsd" => parse_stsd(&b, &handler, &mut t),
            b"stsz" => {
                let (Some(size), Some(count)) = (be32(b.data, 4), be32(b.data, 8)) else { continue };
                t.sample_count = count;
                if size > 0 {
                    t.fixed_size = size;
                } else if count <= MAX_ENTRIES {
                    t.sample_sizes = (0..count as usize).map_while(|i| be32(b.data, 12 + 4 * i)).collect();
                }
            }
            b"stsc" => {
                let Some(count) = be32(b.data, 4).filter(|&c| c <= MAX_ENTRIES) else { continue };
                t.stsc = (0..count as usize)
                    .map_while(|i| Some((be32(b.data, 8 + 12 * i)?, be32(b.data, 12 + 12 * i)?)))
                    .collect();
            }
            b"co64" | b"stco" => {
                let Some(count) = be32(b.data, 4).filter(|&c| c <= MAX_ENTRIES) else { continue };
                t.chunk_offsets = if &b.typ == b"co64" {
                    (0..count as usize).map_while(|i| be64(b.data, 8 + 8 * i)).collect()
                } else {
                    (0..count as usize).map_while(|i| be32(b.data, 8 + 4 * i).map(u64::from)).collect()
                };
            }
            _ => {}
        }
    }
    t
}

fn parse_stsd(stsd: &BmffBox, handler: &[u8; 4], t: &mut Track) {
    for entry in stsd.children(8).map_while(|b| b.ok()) {
        if entry.is(b"CTMD") && handler == b"meta" {
            t.media = Media::TimedMetadata;
        }
        if !entry.is(b"CRAW") {
            continue;
        }
        // VisualSampleEntry: width/height at 24/26; Canon children after 82 bytes.
        t.entry_width = be16(entry.data, 24).unwrap_or(0);
        t.entry_height = be16(entry.data, 26).unwrap_or(0);
        for c in entry.children(82).map_while(|b| b.ok()) {
            match &c.typ {
                b"CMP1" => {
                    if let Ok(h) = CrxHeader::parse(c.data) {
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn bx(t: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(t);
        v.extend_from_slice(payload);
        v
    }

    fn full(t: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 4];
        p.extend_from_slice(payload);
        bx(t, &p)
    }

    /// A minimal CR3 with one raw track whose sample is `sample` at the end of the file.
    pub fn synthetic(cmp1: &[u8], sample: &[u8]) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
        let mut craw = vec![0u8; 82];
        craw[24..26].copy_from_slice(&64u16.to_be_bytes());
        craw[26..28].copy_from_slice(&32u16.to_be_bytes());
        craw.extend(bx(b"CMP1", cmp1));
        let mut stsd_p = 1u32.to_be_bytes().to_vec();
        stsd_p.extend(bx(b"CRAW", &craw));
        let stsd = full(b"stsd", &stsd_p);
        let mut stsz_p = 0u32.to_be_bytes().to_vec();
        stsz_p.extend(1u32.to_be_bytes());
        stsz_p.extend((sample.len() as u32).to_be_bytes());
        let stsz = full(b"stsz", &stsz_p);
        let build = |off: u64| {
            let mut co = 1u32.to_be_bytes().to_vec();
            co.extend(off.to_be_bytes());
            let co64 = full(b"co64", &co);
            let stbl = bx(b"stbl", &[stsd.clone(), stsz.clone(), co64].concat());
            let minf = bx(b"minf", &stbl);
            let hdlr = full(b"hdlr", b"\0\0\0\0vide\0\0\0\0\0\0\0\0\0\0\0\0\0");
            let mdia = bx(b"mdia", &[hdlr, minf].concat());
            let trak = bx(b"trak", &mdia);
            let mut canon = CANON_UUID.to_vec();
            canon.extend(bx(b"CMT1", b"II*\0\x08\0\0\0\0\0\0\0\0\0"));
            let moov = bx(b"moov", &[bx(b"uuid", &canon), trak].concat());
            [ftyp.clone(), moov].concat()
        };
        let head_len = build(0).len() as u64 + 8;
        let mut file = build(head_len);
        file.extend(bx(b"mdat", sample));
        file
    }

    #[test]
    fn probe_and_parse_synthetic() {
        let mut cmp1 = vec![0u8; 52];
        cmp1[4..6].copy_from_slice(&0x100u16.to_be_bytes());
        cmp1[8..12].copy_from_slice(&64u32.to_be_bytes());
        cmp1[12..16].copy_from_slice(&64u32.to_be_bytes());
        cmp1[16..20].copy_from_slice(&64u32.to_be_bytes());
        cmp1[20..24].copy_from_slice(&64u32.to_be_bytes());
        cmp1[24] = 14;
        cmp1[25] = 0x40;
        cmp1[28..32].copy_from_slice(&112u32.to_be_bytes());
        let file = synthetic(&cmp1, &[1, 2, 3, 4]);
        assert!(is_cr3(&file));
        let cr3 = Cr3::parse(&file).unwrap();
        assert!(cr3.cmt[0].is_some());
        let (_, t, h) = cr3.main_raw().unwrap();
        assert_eq!(h.width, 64);
        assert_eq!((t.entry_width, t.entry_height), (64, 32));
        let (off, size) = t.sample(0).unwrap();
        assert_eq!(size, 4);
        assert_eq!(&file[off as usize..off as usize + 4], &[1, 2, 3, 4]);
        assert!(t.sample(1).is_none());
    }

    #[test]
    fn malformed_boxes() {
        assert!(Cr3::parse(b"\0\0\0\x0cftypcrx ").is_err()); // no moov
        let mut bad = bx(b"ftyp", b"crx ");
        bad.extend(&[0, 0, 0, 200]);
        bad.extend(b"moov");
        assert!(Cr3::parse(&bad).is_err());
        // Every truncation of a valid synthetic file parses or errors without panicking.
        let mut cmp1 = vec![0u8; 52];
        cmp1[4..6].copy_from_slice(&0x100u16.to_be_bytes());
        let file = synthetic(&cmp1, &[0; 16]);
        for n in 0..file.len() {
            let _ = Cr3::parse(&file[..n]);
        }
        for i in 0..file.len() {
            let mut f = file.clone();
            f[i] ^= 0xFF;
            if let Ok(c) = Cr3::parse(&f) {
                for t in &c.tracks {
                    let _ = t.sample(0);
                    let _ = t.sample(3);
                }
            }
        }
    }

    #[test]
    fn stsc_sample_mapping() {
        let mut t = Track::new();
        t.chunk_offsets = vec![100, 1000];
        t.stsc = vec![(1, 2), (2, 1)];
        t.sample_sizes = vec![10, 20, 30];
        assert_eq!(t.sample(0), Some((100, 10)));
        assert_eq!(t.sample(1), Some((110, 20)));
        assert_eq!(t.sample(2), Some((1000, 30)));
        assert_eq!(t.sample(3), None);
    }
}
