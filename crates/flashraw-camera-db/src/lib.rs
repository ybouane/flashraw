//! # flashraw-camera-db
//!
//! Static camera database compiled from `camera-db/cameras/*.toml` at build time
//! (see `build.rs`). Lookups are binary searches over a sorted, normalised key
//! index — no parsing happens at runtime.
//!
//! The database adds per-camera knowledge (crops, masked areas, black/white levels,
//! CFA layout, colour matrices, decoder hints). Decoders must not require an entry
//! to decode a file: standards-based metadata comes first, the database refines it.
//!
//! Data notes: see `camera-db/README.md`.

#[cfg(feature = "tooling")]
pub mod tooling;

/// Per-ISO sensor levels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SensorInfo {
    pub black: i32,
    pub white: i32,
    /// Per-CFA-colour black levels (in CFA pattern order), if specified.
    pub black_colors: &'static [i32],
    /// ISO range this entry applies to (0 = unbounded).
    pub iso_min: u32,
    pub iso_max: u32,
    pub iso_list: &'static [u32],
}

impl SensorInfo {
    pub fn applies_to(&self, iso: u32) -> bool {
        if !self.iso_list.is_empty() {
            return self.iso_list.contains(&iso);
        }
        (self.iso_min == 0 || iso >= self.iso_min) && (self.iso_max == 0 || iso <= self.iso_max)
    }
}

/// A camera database entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub make: &'static str,
    pub model: &'static str,
    pub mode: &'static str,
    pub canonical_make: &'static str,
    pub canonical_model: &'static str,
    /// `(model string as written by camera, canonical alias name)`.
    pub aliases: &'static [(&'static str, &'static str)],
    pub cfa: Option<&'static str>,
    /// `[x, y, width, height]`; width/height <= 0 are relative to the right/bottom edge.
    pub crop: Option<[i32; 4]>,
    pub black_areas_v: &'static [[u32; 2]],
    pub black_areas_h: &'static [[u32; 2]],
    pub sensors: &'static [SensorInfo],
    /// XYZ(D65) → camera, scaled by 10000.
    pub color_matrix: &'static [i32],
    pub hints: &'static [(&'static str, &'static str)],
    pub no_size_match: bool,
}

/// Fallback colour data (prefix matched), used when no exact camera entry has a matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FallbackMatrix {
    pub canonical_make: &'static str,
    pub model_prefix: &'static str,
    /// Black level hint (0 = unknown).
    pub black: i32,
    /// White level hint (0 = unknown).
    pub white: i32,
    /// XYZ(D65) → camera, scaled by 10000.
    pub color_matrix: &'static [i32],
}

include!(concat!(env!("OUT_DIR"), "/cameras.rs"));

/// Fallback matrix lookup by canonical make and model (first prefix match in table order,
/// preferring the longest prefix on ties of the same make).
pub fn find_matrix(canonical_make: &str, canonical_model: &str) -> Option<&'static FallbackMatrix> {
    let model = norm(canonical_model);
    let make = norm(canonical_make);
    MATRICES
        .iter()
        .filter(|m| norm(m.canonical_make) == make)
        .filter(|m| {
            let p = norm(m.model_prefix);
            // Whole-word prefix only: "EOS R" must not match "EOS REBEL SL3".
            model.starts_with(&p) && (model.len() == p.len() || !model.as_bytes()[p.len()].is_ascii_alphanumeric())
        })
        .max_by_key(|m| m.model_prefix.len())
}

fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w.to_uppercase());
    }
    out
}

impl Camera {
    /// Sensor levels for an ISO value (falls back to the first/default entry).
    pub fn sensor(&self, iso: u32) -> Option<&'static SensorInfo> {
        let specific = self
            .sensors
            .iter()
            .find(|s| (s.iso_min != 0 || s.iso_max != 0 || !s.iso_list.is_empty()) && s.applies_to(iso));
        specific
            .or_else(|| self.sensors.iter().find(|s| s.iso_min == 0 && s.iso_max == 0 && s.iso_list.is_empty()))
            .or(self.sensors.first())
    }
    pub fn hint(&self, key: &str) -> Option<&'static str> {
        self.hints.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
    /// Crop resolved against an uncropped `width x height` buffer as `(x, y, w, h)`.
    pub fn resolve_crop(&self, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        let [x, y, w, h] = self.crop?;
        if x < 0 || y < 0 || x as u32 >= width || y as u32 >= height {
            return None;
        }
        let w = if w <= 0 { width as i64 - x as i64 + w as i64 } else { w as i64 };
        let h = if h <= 0 { height as i64 - y as i64 + h as i64 } else { h as i64 };
        if w <= 0 || h <= 0 {
            return None;
        }
        let w = (w as u32).min(width - x as u32);
        let h = (h as u32).min(height - y as u32);
        Some((x as u32, y as u32, w, h))
    }
    /// Canonical model name for the given raw model string (alias aware).
    pub fn canonical_model_for(&self, model: &str) -> &'static str {
        let n = norm(model);
        self.aliases.iter().find(|(m, _)| norm(m) == n).map(|(_, c)| *c).unwrap_or(self.canonical_model)
    }
}

/// Look up a camera by EXIF make/model and decoder mode. Falls back to the
/// default mode (`""`) when no mode-specific entry exists.
pub fn find(make: &str, model: &str, mode: &str) -> Option<&'static Camera> {
    let (mk, md) = (norm(make), norm(model));
    let key = |mode: &str| format!("{mk}\t{md}\t{}", norm(mode));
    let lookup = |k: &str| INDEX.binary_search_by(|(e, _)| (*e).cmp(k)).ok().map(|i| &CAMERAS[INDEX[i].1 as usize]);
    lookup(&key(mode)).or_else(|| if mode.is_empty() { None } else { lookup(&key("")) })
}

/// Look up a camera ignoring mode: returns the first entry for make/model.
pub fn find_any(make: &str, model: &str) -> Option<&'static Camera> {
    let prefix = format!("{}\t{}\t", norm(make), norm(model));
    let i = INDEX.partition_point(|(e, _)| *e < prefix.as_str());
    INDEX.get(i).filter(|(e, _)| e.starts_with(&prefix)).map(|(_, idx)| &CAMERAS[*idx as usize])
}

/// All entries.
pub fn all() -> &'static [Camera] {
    CAMERAS
}

/// Normalise a maker string to its canonical brand name.
pub fn canonical_make(make: &str) -> &'static str {
    let m = make.trim().to_ascii_uppercase();
    const TABLE: &[(&str, &str)] = &[
        ("AGFA", "AgfaPhoto"),
        ("ALCATEL", "Alcatel"),
        ("APPLE", "Apple"),
        ("ASUS", "Asus"),
        ("BLACKMAGIC", "Blackmagic"),
        ("CANON", "Canon"),
        ("CASIO", "Casio"),
        ("DJI", "DJI"),
        ("EPSON", "Epson"),
        ("SEIKO EPSON", "Epson"),
        ("FUJI", "Fujifilm"),
        ("GOOGLE", "Google"),
        ("GOPRO", "GoPro"),
        ("HASSELBLAD", "Hasselblad"),
        ("HTC", "HTC"),
        ("HUAWEI", "Huawei"),
        ("KODAK", "Kodak"),
        ("EASTMAN KODAK", "Kodak"),
        ("KONICA", "Konica Minolta"),
        ("MINOLTA", "Minolta"),
        ("LEAF", "Leaf"),
        ("LEICA", "Leica"),
        ("LG", "LG"),
        ("MAMIYA", "Mamiya"),
        ("MOTOROLA", "Motorola"),
        ("NIKON", "Nikon"),
        ("NOKIA", "Nokia"),
        ("OLYMPUS", "Olympus"),
        ("OM DIGITAL", "OM Digital Solutions"),
        ("OM SYSTEM", "OM Digital Solutions"),
        ("ONEPLUS", "OnePlus"),
        ("PANASONIC", "Panasonic"),
        ("PENTAX", "Pentax"),
        ("RICOH", "Ricoh"),
        ("PHASE ONE", "Phase One"),
        ("SAMSUNG", "Samsung"),
        ("SIGMA", "Sigma"),
        ("SINAR", "Sinar"),
        ("SONY", "Sony"),
        ("XIAOMI", "Xiaomi"),
        ("XIAOYI", "Xiaoyi"),
        ("YI TECHNOLOGY", "Xiaoyi"),
        ("PARROT", "Parrot"),
        ("RASPBERRYPI", "Raspberry Pi"),
        ("ZEISS", "Zeiss"),
        ("ARRI", "Arri"),
        ("YUNEEC", "Yuneec"),
        ("LEICA CAMERA", "Leica"),
        ("PHASEONE", "Phase One"),
    ];
    let mut best: Option<(&str, &str)> = None;
    for &(k, v) in TABLE {
        if m.starts_with(k) && best.is_none_or(|(bk, _)| k.len() > bk.len()) {
            best = Some((k, v));
        }
    }
    best.map(|(_, v)| v).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_is_sorted_and_resolvable() {
        assert!(INDEX.windows(2).all(|w| w[0].0 < w[1].0));
        for (k, i) in INDEX {
            let parts: Vec<&str> = k.split('\t').collect();
            let c = &CAMERAS[*i as usize];
            assert_eq!(norm(c.make), parts[0]);
            assert!(find(parts[0], parts[1], parts[2]).is_some());
        }
    }

    #[test]
    fn matrix_prefix_is_word_bounded() {
        if let Some(m) = find_matrix("Canon", "EOS Rebel SL3") {
            assert_ne!(m.model_prefix, "EOS R");
        }
        assert!(find_matrix("Canon", "EOS 5D Mark IV").is_some_and(|m| m.model_prefix == "EOS 5D Mark IV"));
    }

    #[test]
    fn make_normalisation() {
        assert_eq!(canonical_make("NIKON CORPORATION"), "Nikon");
        assert_eq!(canonical_make("OM Digital Solutions"), "OM Digital Solutions");
        assert_eq!(canonical_make("LEICA CAMERA AG"), "Leica");
        assert_eq!(canonical_make("FUJIFILM"), "Fujifilm");
    }

    #[test]
    fn crop_resolution() {
        let c = Camera {
            make: "",
            model: "",
            mode: "",
            canonical_make: "",
            canonical_model: "",
            aliases: &[],
            cfa: None,
            crop: Some([10, 4, -8, 0]),
            black_areas_v: &[],
            black_areas_h: &[],
            sensors: &[],
            color_matrix: &[],
            hints: &[],
            no_size_match: false,
        };
        assert_eq!(c.resolve_crop(100, 50), Some((10, 4, 82, 46)));
    }
}
