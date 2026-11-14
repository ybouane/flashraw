//! Phase One IIQ (also Leaf and Mamiya-Leaf backs in IIQ form) and Leaf MOS containers.
//!
//! Two unrelated container families live here:
//!
//! * [`iiq`]: a vendor directory of numbered 16-byte entries (geometry, payload location and
//!   format, row index, black references, white balance, colour matrix), either bare or
//!   embedded after the 8-byte header of a TIFF wrapper;
//! * [`mos`]: a plain TIFF whose raw image is a lossless-JPEG stream or 16-bit samples, plus a
//!   tree of binary packets ([`pkts`], TIFF tag 0x8606) with the back type, neutrals, mosaic
//!   pattern, rotation and a colour matrix.
//!
//! The entropy decoders live in `flashraw_codecs`; this module only locates and parameterises them.

mod iiq;
mod mos;
mod pkts;
#[cfg(test)]
mod tests;

use crate::common::OpenOptions;
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;

/// TIFF tag that carries a Leaf PKTS packet tree (Leaf MOS files).
pub const TAG_LEAF_PKTS: u16 = 0x8606;

/// Magic-signature probe: does `data` start with a Phase One IIQ directory header (bare or TIFF-wrapped)?
pub fn probe(data: &[u8]) -> bool {
    iiq::find_header(data).is_some()
}

/// Open a Phase One IIQ file (bare or TIFF-wrapped).
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    let base = iiq::find_header(data).ok_or_else(|| Error::UnsupportedFormat("not an IIQ file".into()))?;
    iiq::open(data, base, None, opts)
}

/// Open a TIFF-based file routed here by make or by the PKTS tag: a Phase One IIQ in TIFF clothing, a Leaf MOS file
/// or (no PKTS tag) a Mamiya file that is handed on to the Mamiya reader.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if let Some(base) = iiq::find_header(data) {
        return iiq::open(data, base, Some(tiff), opts);
    }
    if tiff.find_entry(TAG_LEAF_PKTS).is_some() {
        return mos::open(data, &tiff, opts);
    }
    let make = tiff.all_ifds().iter().find_map(|i| i.string(tags::MAKE)).unwrap_or_default();
    if flashraw_camera_db::canonical_make(&make).eq_ignore_ascii_case("Mamiya") {
        return crate::medium::mamiya::open_tiff(data, tiff, opts);
    }
    Err(Error::UnsupportedFormat(format!("{make}: TIFF without Phase One / Leaf raw data")))
}
