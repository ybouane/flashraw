//! Canon cr3 (implementation in progress).

use crate::common::OpenOptions;
use flashraw_core::*;

pub fn probe(_data: &[u8]) -> bool {
    false
}

pub fn open<'a>(_data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("cr3: not implemented".into()))
}
