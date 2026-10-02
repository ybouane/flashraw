//! Vendor module: phaseone (implementation in progress).

use crate::common::OpenOptions;
use flashraw_core::tiff::Tiff;
use flashraw_core::*;

/// Magic-signature probe.
pub fn probe(_data: &[u8]) -> bool {
    false
}

pub fn open<'a>(_data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("phaseone: not implemented".into()))
}

pub fn open_tiff<'a>(_data: &'a [u8], tiff: Tiff<'a>, _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let _ = tiff;
    Err(Error::UnsupportedFormat("phaseone: not implemented".into()))
}
