//! # flashraw-formats
//!
//! Container parsing and per-vendor decoders. [`open`] detects the format and
//! returns a [`RawDecoder`] that exposes metadata immediately and decodes pixels on
//! demand.
//!
//! ## Module conventions
//! Each vendor module exposes some of:
//! - `probe(data) -> bool` for formats identified by a magic signature, with
//!   `open(data, opts)`;
//! - `open_tiff(data, tiff, opts)` for TIFF-based formats dispatched by EXIF Make.
//!
//! Decoders fill [`FrameInfo`] from file metadata first and use the camera database
//! ([`common::apply_db`]) only to fill gaps, so unknown cameras still decode.

pub mod common;
pub mod dng;

pub mod canon;
pub mod fuji;
pub mod hasselblad;
pub mod kodak;
pub mod medium;
pub mod minolta;
pub mod misc;
pub mod nikon;
pub mod olympus;
pub mod panasonic;
pub mod pentax;
pub mod phaseone;
pub mod samsung;
pub mod sigma;
pub mod sony;

pub use common::OpenOptions;
use flashraw_core::tiff::{Tiff, tags};
use flashraw_core::*;

/// Parse `data` and return a decoder for it.
pub fn open<'a>(data: &'a [u8], opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if data.len() < 16 {
        return Err(Error::UnsupportedFormat("file too small".into()));
    }
    // Containers identified by magic signatures.
    if canon::cr3::probe(data) {
        return canon::cr3::open(data, opts);
    }
    if canon::crw::probe(data) {
        return canon::crw::open(data, opts);
    }
    if fuji::probe(data) {
        return fuji::open(data, opts);
    }
    if panasonic::probe(data) {
        return panasonic::open(data, opts);
    }
    if olympus::probe(data) {
        return olympus::open(data, opts);
    }
    if minolta::probe(data) {
        return minolta::open(data, opts);
    }
    if sigma::probe(data) {
        return sigma::open(data, opts);
    }
    if phaseone::probe(data) {
        return phaseone::open(data, opts);
    }
    if kodak::probe(data) {
        return kodak::open(data, opts);
    }
    if misc::probe(data) {
        return misc::open(data, opts);
    }
    let tiff = match Tiff::parse(data) {
        Ok(t) => t,
        Err(_) => {
            // Size-identified Kodak dumps first, then the generic header-less table.
            return kodak::open_headerless(data, opts).unwrap_or_else(|| misc::open_headerless(data, opts));
        }
    };
    let make = tiff.all_ifds().into_iter().find_map(|i| i.string(tags::MAKE)).unwrap_or_default();
    let brand = flashraw_camera_db::canonical_make(&make);
    if dng::is_dng(&tiff) {
        return dng::open(data, tiff, opts);
    }
    // Leaf MOS files often carry no Make tag; the PKTS metadata tag identifies them.
    if tiff.find_entry(phaseone::TAG_LEAF_PKTS).is_some() {
        return phaseone::open_tiff(data, tiff, opts);
    }
    match brand {
        "Canon" => canon::open_tiff(data, tiff, opts),
        "Nikon" => nikon::open_tiff(data, tiff, opts),
        "Sony" => sony::open_tiff(data, tiff, opts),
        "Pentax" | "Ricoh" => pentax::open_tiff(data, tiff, opts),
        "Samsung" => samsung::open_tiff(data, tiff, opts),
        "Hasselblad" => hasselblad::open_tiff(data, tiff, opts),
        "Kodak" => kodak::open_tiff(data, tiff, opts),
        "Leaf" | "Phase One" | "Mamiya" => phaseone::open_tiff(data, tiff, opts),
        "Minolta" | "Konica Minolta" => minolta::open_tiff(data, tiff, opts),
        "Olympus" | "OM Digital Solutions" => olympus::open_tiff(data, tiff, opts),
        "Panasonic" | "Leica" => panasonic::open_tiff(data, tiff, opts),
        "Fujifilm" => fuji::open_tiff(data, tiff, opts),
        "Sigma" => sigma::open_tiff(data, tiff, opts),
        "Epson" => medium::epson::open_tiff(data, tiff, opts),
        "Sinar" => medium::sinar::open_tiff(data, tiff, opts),
        _ => misc::open_tiff(data, tiff, opts),
    }
}

/// Cheap format detection without full parsing.
pub fn detect(data: &[u8]) -> Option<RawFormat> {
    if data.len() < 16 {
        return None;
    }
    if canon::cr3::probe(data) {
        return Some(RawFormat::Cr3);
    }
    if canon::crw::probe(data) {
        return Some(RawFormat::Crw);
    }
    if fuji::probe(data) {
        return Some(RawFormat::Raf);
    }
    if panasonic::probe(data) {
        return Some(RawFormat::Rw2);
    }
    if olympus::probe(data) {
        return Some(RawFormat::Orf);
    }
    if minolta::probe(data) {
        return Some(RawFormat::Mrw);
    }
    if sigma::probe(data) {
        return Some(RawFormat::X3f);
    }
    if phaseone::probe(data) {
        return Some(RawFormat::Iiq);
    }
    if kodak::probe(data) {
        return Some(RawFormat::Other);
    }
    if misc::probe(data) {
        return Some(RawFormat::Other);
    }
    let tiff = Tiff::parse(data).ok()?;
    if dng::is_dng(&tiff) {
        return Some(RawFormat::Dng);
    }
    if tiff.find_entry(phaseone::TAG_LEAF_PKTS).is_some() {
        return Some(RawFormat::Mos);
    }
    let make = tiff.all_ifds().into_iter().find_map(|i| i.string(tags::MAKE))?;
    Some(match flashraw_camera_db::canonical_make(&make) {
        "Canon" if data.get(8..10) == Some(b"CR") => RawFormat::Cr2,
        "Canon" => RawFormat::Other,
        "Nikon" => RawFormat::Nef,
        "Sony" => RawFormat::Arw,
        "Pentax" | "Ricoh" => RawFormat::Pef,
        "Samsung" => RawFormat::Srw,
        "Hasselblad" => RawFormat::ThreeFr,
        "Kodak" => RawFormat::Kdc,
        "Leaf" | "Mamiya" => RawFormat::Mos,
        "Phase One" => RawFormat::Iiq,
        "Epson" => RawFormat::Erf,
        _ => RawFormat::Other,
    })
}
