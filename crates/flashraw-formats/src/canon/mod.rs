//! Canon: CR2 (TIFF), CR3 (ISO BMFF + CRX), CRW (CIFF).

pub mod cr3;
pub mod crw;

use crate::common::OpenOptions;
use flashraw_core::tiff::Tiff;
use flashraw_core::*;

/// TIFF-based Canon files (CR2 and early TIF raws).
pub fn open_tiff<'a>(_data: &'a [u8], _tiff: Tiff<'a>, _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    Err(Error::UnsupportedFormat("canon: not implemented".into()))
}
