//! Canon: CR2 and Canon TIFF raws ([`cr2`]), CR3 (ISO BMFF + CRX, [`cr3`]) and CRW
//! (CIFF, [`crw`]).
//!
//! Shared Canon knowledge lives in [`makernote`] (maker note parsing: SensorInfo,
//! ColorData white balance / black / white levels, model IDs) and [`geometry`]
//! (per-sensor margins and optical-black layout).

pub mod cr2;
pub mod cr3;
pub mod crw;
pub mod geometry;
pub mod makernote;

use crate::common::OpenOptions;
use flashraw_core::tiff::Tiff;
use flashraw_core::*;

/// TIFF-based Canon files (CR2 and the TIFF raws of the EOS-1D/1Ds/D2000).
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    cr2::open(data, tiff, opts)
}
