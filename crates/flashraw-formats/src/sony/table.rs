//! Fallback colour-table lookup shared by the Sony, Olympus and Minolta decoders.
//!
//! The camera database's fallback table (`[[matrix]]` entries, in table order) is matched
//! by prefix: the first entry of
//! the maker whose model prefix matches case-insensitively wins. Entries also carry
//! table black/white overrides (`black > 0` replaces the file black, `black < 0`
//! applies only when the file has none, `white > 0` replaces the white level).

use crate::common::db_color;
use flashraw_camera_db::FallbackMatrix;
use flashraw_core::*;

/// First table entry for `make` whose prefix matches `model`.
pub fn lookup(make: &str, model: &str) -> Option<&'static FallbackMatrix> {
    let mb = model.as_bytes();
    flashraw_camera_db::MATRICES.iter().find(|m| {
        let p = m.model_prefix.as_bytes();
        m.canonical_make.eq_ignore_ascii_case(make) && mb.len() >= p.len() && mb[..p.len()].eq_ignore_ascii_case(p)
    })
}

/// Colour matrices from a table entry, falling back to the generic database lookup.
pub fn color(entry: Option<&'static FallbackMatrix>, meta: &RawMetadata) -> ColorMatrices {
    if let Some(m) = entry.filter(|m| m.color_matrix.iter().any(|&v| v != 0)) {
        let rows = m.color_matrix.len() / 3;
        if let Some(cm) =
            ColorMatrix::new(Illuminant::D65, rows, m.color_matrix.iter().map(|&v| v as f32 / 10000.0).collect())
        {
            return ColorMatrices { color: vec![cm], source: ColorSource::CameraDb, ..Default::default() };
        }
    }
    // Fall back to the exact camera-db entry (e.g. curated rebadged bodies), then the
    // generic prefix table.
    db_color(meta, flashraw_camera_db::find_any(&meta.make, &meta.model))
}

/// Apply the table's black/white overrides to `(black, per-colour deltas, white)`.
pub fn apply_levels(entry: Option<&FallbackMatrix>, black: &mut u32, cblack: &mut [u32; 4], white: &mut Option<u32>) {
    let Some(t) = entry else { return };
    let rblack = *black + cblack.iter().sum::<u32>() / 4;
    if t.black > 0 {
        *black = t.black as u32;
        *cblack = [0; 4];
    } else if t.black < 0 && rblack == 0 {
        *black = t.black.unsigned_abs();
        *cblack = [0; 4];
    }
    if t.white > 0 {
        *white = Some(t.white as u32);
    }
}
