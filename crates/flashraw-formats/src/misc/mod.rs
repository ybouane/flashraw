//! The long tail: formats without a vendor module of their own.
//!
//! - [`naked`]: headerless sensor dumps identified by exact file size (Canon CHDK
//!   `.CRW`, action cameras, phone dumps, old compacts, Kodak C-series YCbCr dumps,
//!   Minolta RD-175), using the camera database's `filesize` entries.
//! - [`rpi`]: Raspberry Pi camera JPEGs with an appended Broadcom (`BRCM`) raw.
//! - [`arri`]: ARRI ALEXA `.ari` frames.
//! - [`tiff_ep`]: standards-based fallback for TIFF/EP CFA files from unknown makes.
//!
//! Dispatch: magic-identified formats via [`probe`]/[`open`], TIFF files with
//! unrecognised makes via [`open_tiff`], everything else via [`open_headerless`].

pub mod arri;
pub mod naked;
pub mod rpi;
pub mod tiff_ep;

use crate::common::OpenOptions;
use flashraw_core::tiff::Tiff;
use flashraw_core::*;

/// Magic-signature probe for formats handled here.
pub fn probe(data: &[u8]) -> bool {
    arri::probe(data) || is_light_lri(data)
}

/// Light L16 `.lri` container ("LELR" blocks): recognised so that it is reported
/// precisely instead of being misidentified by size.
fn is_light_lri(data: &[u8]) -> bool {
    data.len() >= 32 && &data[..4] == b"LELR"
}

pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if arri::probe(data) {
        return arri::open(data, opts);
    }
    if is_light_lri(data) {
        return Err(Error::UnsupportedFormat(
            "Light L16 LRI (multi-module capture with proprietary per-module packing) is not supported".into(),
        ));
    }
    Err(Error::UnsupportedFormat("misc: unrecognised signature".into()))
}

/// TIFF files whose make has no dedicated module.
pub fn open_tiff<'a>(data: &'a [u8], tiff: Tiff<'a>, opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    tiff_ep::open(data, tiff, opts)
}

/// Files without a recognisable header (identified by size or trailing signatures).
pub fn open_headerless<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if let Some(r) = rpi::open(data, opts) {
        return r;
    }
    if let Some(r) = naked::open(data, opts) {
        return r;
    }
    Err(Error::UnsupportedFormat("no recognisable header".into()))
}

pub(crate) fn orientation_from_flip(flip: u32) -> Orientation {
    match flip {
        3 => Orientation::Rotate180,
        5 => Orientation::Rotate270,
        6 => Orientation::Rotate90,
        1 => Orientation::FlipH,
        2 => Orientation::FlipV,
        4 => Orientation::Transpose,
        7 => Orientation::Transverse,
        _ => Orientation::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_data_never_panics() {
        let p = Profiler::default();
        let mut s = 7u32;
        for len in [0usize, 1, 15, 16, 100, 307_200, 4096 + 7] {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    s = s.wrapping_mul(1103515245).wrapping_add(12345);
                    (s >> 16) as u8
                })
                .collect();
            if let Ok(d) = open_headerless(&data, &OpenOptions::default()) {
                let _ = d.decode(&DecodeContext::new(&p));
            }
            let _ = open(&data, &OpenOptions::default());
        }
    }
}
