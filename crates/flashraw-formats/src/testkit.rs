//! Helpers shared by the unit tests of the format readers: corpus access, fingerprints of what a
//! reader reports about a file, a deterministic generator and a small TIFF writer.

/// Read a corpus file (`$FLASHRAW_CORPUS` or `<workspace>/corpus`); `None` when it is absent.
pub fn corpus(rel: &str) -> Option<Vec<u8>> {
    let root = std::env::var_os("FLASHRAW_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    std::fs::read(root.join(rel)).ok()
}

/// FNV-1a, 64 bit.
pub fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// One number for everything a reader reports without decoding: format, metadata (previews and
/// extras included), frame description and packed payload description. An error is reduced to
/// its kind.
pub fn fingerprint(data: &[u8]) -> String {
    match crate::open(data, &crate::OpenOptions::default()) {
        Ok(d) => {
            let packed = d.packed_payload().map(|p| (p.data.len(), p.width, p.height, p.bits, p.order, p.row_stride));
            let text = format!(
                "{:?}|{:?}|{:?}|{:?}|{:?}",
                d.format(),
                d.metadata(),
                d.frame_info(),
                d.random_access(),
                packed
            );
            format!("{:016x}", fnv(text.as_bytes()))
        }
        Err(e) => format!("error:{}", e.kind()),
    }
}

/// Compare fingerprints of corpus files with the pinned values (files missing from the corpus are
/// skipped). Set `FLASHRAW_PRINT_GOLDEN=1` to print the current values instead of asserting.
pub fn check_golden(pinned: &[(&str, &str)]) {
    let print = std::env::var_os("FLASHRAW_PRINT_GOLDEN").is_some();
    let mut seen = 0;
    for (rel, expected) in pinned {
        let Some(data) = corpus(rel) else { continue };
        seen += 1;
        let got = fingerprint(&data);
        if print {
            println!("(\"{rel}\", \"{got}\"),");
        } else {
            assert_eq!(&got, expected, "{rel}");
        }
    }
    if seen == 0 {
        eprintln!("corpus not present; golden fingerprints skipped");
    }
}

/// Deterministic pseudo-random numbers.
pub struct Lcg(pub u64);

impl Lcg {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 24
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// One TIFF directory entry to write: tag, type and the value bytes (little-endian).
pub struct Tag {
    pub tag: u16,
    pub typ: u16,
    pub count: u32,
    pub value: Vec<u8>,
}

pub fn short(tag: u16, v: &[u16]) -> Tag {
    Tag { tag, typ: 3, count: v.len() as u32, value: v.iter().flat_map(|x| x.to_le_bytes()).collect() }
}

pub fn long(tag: u16, v: &[u32]) -> Tag {
    Tag { tag, typ: 4, count: v.len() as u32, value: v.iter().flat_map(|x| x.to_le_bytes()).collect() }
}

pub fn ascii(tag: u16, s: &str) -> Tag {
    let mut value = s.as_bytes().to_vec();
    value.push(0);
    Tag { tag, typ: 2, count: value.len() as u32, value }
}

pub fn bytes(tag: u16, v: &[u8]) -> Tag {
    Tag { tag, typ: 1, count: v.len() as u32, value: v.to_vec() }
}

pub fn rationals(tag: u16, v: &[(u32, u32)]) -> Tag {
    Tag {
        tag,
        typ: 5,
        count: v.len() as u32,
        value: v.iter().flat_map(|&(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes())).collect(),
    }
}

/// A little-endian TIFF under construction. Directories and out-of-line values are appended to the
/// buffer; the header's first-IFD offset is patched by [`TiffWriter::finish`].
pub struct TiffWriter {
    pub buf: Vec<u8>,
}

impl TiffWriter {
    pub fn new() -> Self {
        TiffWriter { buf: b"II*\0\0\0\0\0".to_vec() }
    }

    /// Append a directory (entries are sorted by tag); returns its offset.
    pub fn ifd(&mut self, mut tags: Vec<Tag>) -> u32 {
        tags.sort_by_key(|t| t.tag);
        let start = self.buf.len();
        let values_at = start + 2 + 12 * tags.len() + 4;
        let mut table = (tags.len() as u16).to_le_bytes().to_vec();
        let mut tail = Vec::new();
        for t in &tags {
            table.extend(t.tag.to_le_bytes());
            table.extend(t.typ.to_le_bytes());
            table.extend(t.count.to_le_bytes());
            if t.value.len() <= 4 {
                table.extend(&t.value);
                table.extend(std::iter::repeat_n(0, 4 - t.value.len()));
            } else {
                table.extend(((values_at + tail.len()) as u32).to_le_bytes());
                tail.extend(&t.value);
                if t.value.len() % 2 == 1 {
                    tail.push(0);
                }
            }
        }
        table.extend(0u32.to_le_bytes());
        self.buf.extend(table);
        self.buf.extend(tail);
        start as u32
    }

    /// Point the header at `first_ifd` and return the file.
    pub fn finish(mut self, first_ifd: u32) -> Vec<u8> {
        self.buf[4..8].copy_from_slice(&first_ifd.to_le_bytes());
        self.buf
    }
}
