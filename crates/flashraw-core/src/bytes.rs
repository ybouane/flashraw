//! Bounds-checked, zero-copy byte access.
//!
//! Every container parser works on `&[u8]` slices of the (usually memory-mapped)
//! input. All offset arithmetic is checked: a malformed offset yields
//! [`Error::CorruptFile`] instead of a panic or out-of-bounds read.

use crate::{Error, Result};

/// Byte order of multi-byte integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ByteOrder {
    Little,
    Big,
}

impl ByteOrder {
    #[inline(always)]
    pub fn u16(self, b: [u8; 2]) -> u16 {
        match self {
            ByteOrder::Little => u16::from_le_bytes(b),
            ByteOrder::Big => u16::from_be_bytes(b),
        }
    }
    #[inline(always)]
    pub fn u32(self, b: [u8; 4]) -> u32 {
        match self {
            ByteOrder::Little => u32::from_le_bytes(b),
            ByteOrder::Big => u32::from_be_bytes(b),
        }
    }
    #[inline(always)]
    pub fn u64(self, b: [u8; 8]) -> u64 {
        match self {
            ByteOrder::Little => u64::from_le_bytes(b),
            ByteOrder::Big => u64::from_be_bytes(b),
        }
    }
}

/// Return `data[offset..offset+len]` or a corruption error.
#[inline]
pub fn slice_at(data: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    offset.checked_add(len).and_then(|end| data.get(offset..end)).ok_or_else(|| {
        Error::corrupt_at(format!("range of {len} bytes exceeds file size {}", data.len()), offset as u64)
    })
}

/// Return `data[offset..]` clamped to at most `len` bytes. Used for payloads where
/// vendors routinely declare slightly more bytes than the file contains (truncated
#[inline]
pub fn slice_at_lenient(data: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    if offset >= data.len() {
        return Err(Error::corrupt_at("payload offset beyond end of file", offset as u64));
    }
    let end = offset.saturating_add(len).min(data.len());
    Ok(&data[offset..end])
}

#[inline(always)]
fn arr<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    match offset.checked_add(N).and_then(|end| data.get(offset..end)) {
        Some(s) => {
            let mut a = [0u8; N];
            a.copy_from_slice(s);
            Ok(a)
        }
        None => Err(Error::corrupt_at("read beyond end of data", offset as u64)),
    }
}

/// Random-access endian-aware reads on a byte slice.
pub trait ReadAt {
    fn u8_at(&self, offset: usize) -> Result<u8>;
    fn u16_at(&self, order: ByteOrder, offset: usize) -> Result<u16>;
    fn u32_at(&self, order: ByteOrder, offset: usize) -> Result<u32>;
    fn u64_at(&self, order: ByteOrder, offset: usize) -> Result<u64>;
    fn u16le(&self, offset: usize) -> Result<u16> {
        self.u16_at(ByteOrder::Little, offset)
    }
    fn u16be(&self, offset: usize) -> Result<u16> {
        self.u16_at(ByteOrder::Big, offset)
    }
    fn u32le(&self, offset: usize) -> Result<u32> {
        self.u32_at(ByteOrder::Little, offset)
    }
    fn u32be(&self, offset: usize) -> Result<u32> {
        self.u32_at(ByteOrder::Big, offset)
    }
}

impl ReadAt for [u8] {
    #[inline]
    fn u8_at(&self, offset: usize) -> Result<u8> {
        self.get(offset).copied().ok_or_else(|| Error::corrupt_at("read beyond end of data", offset as u64))
    }
    #[inline]
    fn u16_at(&self, order: ByteOrder, offset: usize) -> Result<u16> {
        Ok(order.u16(arr(self, offset)?))
    }
    #[inline]
    fn u32_at(&self, order: ByteOrder, offset: usize) -> Result<u32> {
        Ok(order.u32(arr(self, offset)?))
    }
    #[inline]
    fn u64_at(&self, order: ByteOrder, offset: usize) -> Result<u64> {
        Ok(order.u64(arr(self, offset)?))
    }
}

/// A sequential cursor over a byte slice.
#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    order: ByteOrder,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8], order: ByteOrder) -> Self {
        Cursor { data, pos: 0, order }
    }
    pub fn at(data: &'a [u8], pos: usize, order: ByteOrder) -> Self {
        Cursor { data, pos, order }
    }
    #[inline]
    pub fn data(&self) -> &'a [u8] {
        self.data
    }
    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }
    #[inline]
    pub fn order(&self) -> ByteOrder {
        self.order
    }
    pub fn set_order(&mut self, order: ByteOrder) {
        self.order = order;
    }
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }
    #[inline]
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        let np = self.pos.checked_add(n).ok_or_else(|| Error::corrupt("cursor overflow"))?;
        if np > self.data.len() {
            return Err(Error::corrupt_at("skip beyond end of data", self.pos as u64));
        }
        self.pos = np;
        Ok(())
    }
    #[inline]
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = slice_at(self.data, self.pos, n)?;
        self.pos += n;
        Ok(s)
    }
    #[inline]
    pub fn u8(&mut self) -> Result<u8> {
        let v = self.data.u8_at(self.pos)?;
        self.pos += 1;
        Ok(v)
    }
    #[inline]
    pub fn u16(&mut self) -> Result<u16> {
        let v = self.data.u16_at(self.order, self.pos)?;
        self.pos += 2;
        Ok(v)
    }
    #[inline]
    pub fn u32(&mut self) -> Result<u32> {
        let v = self.data.u32_at(self.order, self.pos)?;
        self.pos += 4;
        Ok(v)
    }
    #[inline]
    pub fn u64(&mut self) -> Result<u64> {
        let v = self.data.u64_at(self.order, self.pos)?;
        self.pos += 8;
        Ok(v)
    }
    #[inline]
    pub fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
    #[inline]
    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    pub fn u16be(&mut self) -> Result<u16> {
        let v = self.data.u16be(self.pos)?;
        self.pos += 2;
        Ok(v)
    }
    pub fn u32be(&mut self) -> Result<u32> {
        let v = self.data.u32be(self.pos)?;
        self.pos += 4;
        Ok(v)
    }
}

/// Decode a fixed-length, NUL-padded ASCII field into a trimmed `String`.
pub fn ascii_field(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let s: String = bytes[..end].iter().map(|&b| if b.is_ascii() && b >= 0x20 { b as char } else { ' ' }).collect();
    s.trim().to_string()
}

/// Find the first occurrence of `needle` in `hay` (simple, used for marker scans in headers only).
pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_reads() {
        let d = [1u8, 2, 3, 4];
        assert_eq!(d.u16le(0).unwrap(), 0x0201);
        assert_eq!(d.u16be(2).unwrap(), 0x0304);
        assert_eq!(d.u32be(0).unwrap(), 0x01020304);
        assert!(d.u32le(1).is_err());
        assert!(d.u16le(usize::MAX).is_err());
        assert!(slice_at(&d, usize::MAX - 1, 4).is_err());
        assert_eq!(slice_at_lenient(&d, 2, 100).unwrap(), &[3, 4]);
    }

    #[test]
    fn cursor() {
        let d = [0u8, 1, 0, 0, 0, 2];
        let mut c = Cursor::new(&d, ByteOrder::Big);
        assert_eq!(c.u16().unwrap(), 1);
        assert_eq!(c.u32().unwrap(), 2);
        assert!(c.u8().is_err());
    }

    #[test]
    fn ascii() {
        assert_eq!(ascii_field(b"NIKON \0garbage"), "NIKON");
    }
}
