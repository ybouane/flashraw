//! ISO Base Media File Format (ISO/IEC 14496-12) box parsing, used by CR3.

use crate::bytes::ReadAt;
use crate::{Error, Result};

/// A parsed box header with its payload slice.
#[derive(Clone, Copy)]
pub struct BmffBox<'a> {
    pub typ: [u8; 4],
    /// Absolute offset of the box start in the root slice.
    pub offset: usize,
    /// Payload (after the size/type/uuid header).
    pub data: &'a [u8],
    /// Absolute offset of `data` in the root slice.
    pub data_offset: usize,
    pub uuid: Option<[u8; 16]>,
}

impl std::fmt::Debug for BmffBox<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Box({}, off={}, len={})", String::from_utf8_lossy(&self.typ), self.offset, self.data.len())
    }
}

impl<'a> BmffBox<'a> {
    pub fn is(&self, t: &[u8; 4]) -> bool {
        &self.typ == t
    }
    /// Iterate child boxes starting `skip` bytes into the payload (e.g. 4 for full boxes,
    /// 8 for `stsd`).
    pub fn children(&self, skip: usize) -> BoxIter<'a> {
        BoxIter { data: self.data, pos: skip.min(self.data.len()), base: self.data_offset, failed: false }
    }
    /// First child of type `t`.
    pub fn child(&self, skip: usize, t: &[u8; 4]) -> Option<BmffBox<'a>> {
        self.children(skip).filter_map(|b| b.ok()).find(|b| b.is(t))
    }
    /// Child by uuid.
    pub fn child_uuid(&self, skip: usize, uuid: &[u8; 16]) -> Option<BmffBox<'a>> {
        self.children(skip).filter_map(|b| b.ok()).find(|b| b.uuid.as_ref() == Some(uuid))
    }
}

/// Iterator over sibling boxes in a byte range.
pub struct BoxIter<'a> {
    data: &'a [u8],
    pos: usize,
    base: usize,
    failed: bool,
}

/// Iterate the top-level boxes of a file.
pub fn boxes(data: &[u8]) -> BoxIter<'_> {
    BoxIter { data, pos: 0, base: 0, failed: false }
}

impl<'a> Iterator for BoxIter<'a> {
    type Item = Result<BmffBox<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos + 8 > self.data.len() {
            return None;
        }
        let r = parse_box(self.data, self.pos, self.base);
        match &r {
            Ok((_, next)) => self.pos = *next,
            Err(_) => self.failed = true,
        }
        Some(r.map(|(b, _)| b))
    }
}

fn parse_box<'a>(data: &'a [u8], pos: usize, base: usize) -> Result<(BmffBox<'a>, usize)> {
    let size32 = data.u32be(pos)? as u64;
    let mut typ = [0u8; 4];
    typ.copy_from_slice(&data[pos + 4..pos + 8]);
    let mut hdr = 8usize;
    let size = match size32 {
        0 => (data.len() - pos) as u64,
        1 => {
            hdr = 16;
            data.u64_at(crate::ByteOrder::Big, pos + 8)?
        }
        s => s,
    };
    let mut uuid = None;
    if &typ == b"uuid" {
        let u = data.get(pos + hdr..pos + hdr + 16).ok_or_else(|| Error::corrupt("truncated BMFF uuid box"))?;
        let mut a = [0u8; 16];
        a.copy_from_slice(u);
        uuid = Some(a);
        hdr += 16;
    }
    let end = (pos as u64).checked_add(size).filter(|&e| size >= hdr as u64 && e <= data.len() as u64);
    let Some(end) = end else {
        return Err(Error::corrupt_at(format!("invalid BMFF box size {size}"), (base + pos) as u64));
    };
    let end = end as usize;
    let b = BmffBox { typ, offset: base + pos, data: &data[pos + hdr..end], data_offset: base + pos + hdr, uuid };
    Ok((b, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(t: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(t);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn nested_boxes() {
        let inner = mk(b"abcd", &[1, 2, 3]);
        let outer = mk(b"moov", &inner);
        let mut file = mk(b"ftyp", b"crx ");
        file.extend(&outer);
        let top: Vec<_> = boxes(&file).map(|b| b.unwrap()).collect();
        assert_eq!(top.len(), 2);
        assert!(top[1].is(b"moov"));
        let c = top[1].child(0, b"abcd").unwrap();
        assert_eq!(c.data, &[1, 2, 3]);
        assert_eq!(c.data_offset, file.len() - 3);
    }

    #[test]
    fn bad_sizes() {
        let mut b = mk(b"moov", &[0; 4]);
        b[3] = 200; // size beyond data
        assert!(boxes(&b).next().unwrap().is_err());
        let mut b = mk(b"moov", &[0; 4]);
        b[3] = 3; // smaller than header
        assert!(boxes(&b).next().unwrap().is_err());
    }
}
