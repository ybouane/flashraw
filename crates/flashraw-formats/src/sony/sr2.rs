//! The encrypted SR2 private directory of older Sony bodies (DSLR-A700 to A99, NEX, first RX generation).
//!
//! IFD0's DNGPrivateData (0xC634) starts with the offset of a small private IFD. Three of its tags (0x7200
//! block offset, 0x7201 block length, 0x7221 key) locate a block of the file that is enciphered with the
//! stream of [`super::crypt`] and, once decrypted, is a TIFF directory without header whose values carry
//! the black levels and white balance.

use super::crypt;
use flashraw_core::tiff::{Tiff, TiffOptions, parse_ifd_at, tags, type_size};

const TAG_BLOCK_OFFSET: u16 = 0x7200;
const TAG_BLOCK_LENGTH: u16 = 0x7201;
const TAG_KEY: u16 = 0x7221;

/// Real blocks are about a kilobyte; anything this large is not one.
const MAX_BLOCK: usize = 10_240_000;
/// Directories with more entries than this are not read.
const MAX_ENTRIES: usize = 1000;
/// Values kept per entry (only the first four are ever used).
const MAX_VALUES: usize = 16;

/// A decrypted directory entry: its tag and the leading 16-bit words of its value.
pub(super) type Entry = (u16, Vec<u16>);

/// The entries of the private directory in directory order; empty when the file has none.
pub(super) fn entries(data: &[u8], tiff: &Tiff) -> Vec<Entry> {
    let Some(block) = locate(data, tiff) else { return Vec::new() };
    let mut buf = data[block.offset..block.offset + block.len].to_vec();
    crypt::decrypt(&mut buf, block.key);
    walk(&buf, block.offset, tiff.order)
}

struct Block {
    offset: usize,
    len: usize,
    key: u32,
}

fn locate(data: &[u8], tiff: &Tiff) -> Option<Block> {
    let order = tiff.order;
    let private = tiff.ifds.first()?.get(tags::DNG_PRIVATE_DATA)?.bytes();
    let at = order.u32(private.get(..4)?.try_into().ok()?);
    let opts = TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() };
    let ifd = parse_ifd_at(data, order, at, opts).ok()?;
    let offset = ifd.u32(TAG_BLOCK_OFFSET)? as usize;
    let len = ifd.u32(TAG_BLOCK_LENGTH)? as usize;
    let key = order.u32(ifd.get(TAG_KEY)?.bytes().get(..4)?.try_into().ok()?);
    if len == 0 || len >= MAX_BLOCK || offset.checked_add(len)? > data.len() {
        return None;
    }
    Some(Block { offset, len, key })
}

/// Read the entries of a decrypted directory. `base` is the file offset the buffer was taken from:
/// values longer than four bytes are addressed by absolute file offset.
fn walk(buf: &[u8], base: usize, order: flashraw_core::ByteOrder) -> Vec<Entry> {
    let word = |pos: usize| buf.get(pos..pos.checked_add(2)?).map(|b| order.u16([b[0], b[1]]));
    let long = |pos: usize| buf.get(pos..pos.checked_add(4)?).map(|b| order.u32([b[0], b[1], b[2], b[3]]));
    let Some(count) = word(0).map(usize::from).filter(|&n| n <= MAX_ENTRIES) else { return Vec::new() };
    let mut out = Vec::new();
    for i in 0..count {
        let pos = 2 + 12 * i;
        if buf.len() < pos + 12 {
            break;
        }
        let (tag, typ) = (word(pos).unwrap_or(0), word(pos + 2).unwrap_or(0));
        let n = long(pos + 4).unwrap_or(0) as usize;
        let unit = type_size(typ).max(1);
        let start = if unit.saturating_mul(n) <= 4 {
            pos + 8
        } else {
            match (long(pos + 8).unwrap_or(0) as usize).checked_sub(base) {
                Some(s) => s,
                None => continue,
            }
        };
        let values = (0..n.min(MAX_VALUES)).filter_map(|c| word(start.checked_add(unit * c)?)).collect();
        out.push((tag, values));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashraw_core::ByteOrder;

    fn entry(tag: u16, typ: u16, count: u32, value: u32) -> Vec<u8> {
        let mut e = Vec::new();
        e.extend(tag.to_le_bytes());
        e.extend(typ.to_le_bytes());
        e.extend(count.to_le_bytes());
        e.extend(value.to_le_bytes());
        e
    }

    #[test]
    fn walks_inline_and_remote_values() {
        let base = 1000;
        let mut buf = 2u16.to_le_bytes().to_vec();
        buf.extend(entry(0x7310, 3, 4, (base + 30) as u32));
        buf.extend(entry(0x7000, 3, 1, 7));
        buf.resize(30, 0);
        for v in [512u16, 513, 514, 515] {
            buf.extend(v.to_le_bytes());
        }
        let got = walk(&buf, base, ByteOrder::Little);
        assert_eq!(got, vec![(0x7310, vec![512, 513, 514, 515]), (0x7000, vec![7])]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(walk(&[], 0, ByteOrder::Little).is_empty());
        assert!(walk(&[0xff, 0xff], 0, ByteOrder::Little).is_empty());
        // An offset below the block start skips the entry; a short entry ends the walk.
        let mut buf = 2u16.to_le_bytes().to_vec();
        buf.extend(entry(0x7310, 3, 4, 5));
        buf.extend([0; 5]);
        assert!(walk(&buf, 100, ByteOrder::Little).is_empty());
    }
}
