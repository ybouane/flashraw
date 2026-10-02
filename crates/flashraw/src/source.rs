//! Input sources: borrowed slices, owned buffers, shared buffers, memory maps.

use std::sync::Arc;

/// Bytes a [`crate::FlashRaw`] can decode from.
pub enum Source<'a> {
    /// Zero-copy borrow of caller memory.
    Borrowed(&'a [u8]),
    /// Owned buffer.
    Owned(Vec<u8>),
    /// Shared, reference-counted buffer.
    Shared(Arc<[u8]>),
    /// Memory-mapped file.
    #[cfg(feature = "mmap")]
    Mmap(memmap2::Mmap),
}

impl Source<'_> {
    /// The source bytes. For every owning variant the returned slice points into a
    /// heap allocation or mapping whose address does not change when `Source` moves.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Source::Borrowed(b) => b,
            Source::Owned(v) => v,
            Source::Shared(a) => a,
            #[cfg(feature = "mmap")]
            Source::Mmap(m) => m,
        }
    }
}

impl<'a> From<&'a [u8]> for Source<'a> {
    fn from(b: &'a [u8]) -> Self {
        Source::Borrowed(b)
    }
}
impl<'a> From<&'a Vec<u8>> for Source<'a> {
    fn from(b: &'a Vec<u8>) -> Self {
        Source::Borrowed(b)
    }
}
impl<'a, const N: usize> From<&'a [u8; N]> for Source<'a> {
    fn from(b: &'a [u8; N]) -> Self {
        Source::Borrowed(b)
    }
}
impl From<Vec<u8>> for Source<'static> {
    fn from(v: Vec<u8>) -> Self {
        Source::Owned(v)
    }
}
impl From<Arc<[u8]>> for Source<'static> {
    fn from(v: Arc<[u8]>) -> Self {
        Source::Shared(v)
    }
}
#[cfg(feature = "mmap")]
impl From<memmap2::Mmap> for Source<'static> {
    fn from(m: memmap2::Mmap) -> Self {
        Source::Mmap(m)
    }
}
