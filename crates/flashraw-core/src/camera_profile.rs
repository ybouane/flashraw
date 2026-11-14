//! DNG camera profiles: the colour matrices, hue/saturation/value tables and tone curve of
//! a DNG Camera Profile (`.dcp` file) or of the profile tags embedded in a DNG.
//!
//! This module only holds and parses the data; the `flashraw` crate renders with it. A DCP is
//! a TIFF-like file with the magic `IIRC` in place of the TIFF version, always little-endian,
//! whose single IFD uses the DNG profile tags.

use crate::tiff::{Ifd, Tiff, TiffOptions, tags};
use crate::types::{ColorMatrix, Illuminant};
use crate::{Error, Result};

/// Tags that exist only in the DNG tag range and are not yet in [`tags`].
mod extra {
    pub const PROFILE_COPYRIGHT: u16 = 0xC6FE;
    pub const PROFILE_HUE_SAT_MAP_ENCODING: u16 = 0xC7A3;
    pub const PROFILE_LOOK_TABLE_ENCODING: u16 = 0xC7A4;
    pub const DEFAULT_BLACK_RENDER: u16 = 0xC7A6;
}

/// Most table entries accepted (hue x sat x val divisions).
const MAX_TABLE_ENTRIES: usize = 1 << 22;
/// Most tone curve points accepted.
const MAX_CURVE_POINTS: usize = 1 << 14;

/// How the RGB values are encoded before a table converts them to HSV
/// (`ProfileHueSatMapEncoding`, `ProfileLookTableEncoding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum TableEncoding {
    /// Linear (the default).
    #[default]
    Linear,
    /// The sRGB transfer curve: values are encoded before the HSV conversion and decoded
    /// after the table, which spaces the value dimension perceptually.
    Srgb,
}

/// A hue/saturation/value table (`ProfileHueSatMapData`, `ProfileLookTableData`).
///
/// Entries are `(hue shift in degrees, saturation scale, value scale)` triples, stored with
/// the saturation division varying fastest, then the hue division, then the value division.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ProfileTable {
    /// Divisions `[hue, saturation, value]`, each at least 1.
    pub dims: [u32; 3],
    /// First table: `3 * hue * sat * val` floats. For a hue/sat map this is the table for
    /// the first calibration illuminant.
    pub data: Vec<f32>,
    /// Hue/sat maps only: the table for the second illuminant (same dimensions).
    pub data2: Option<Vec<f32>>,
    /// Encoding of the RGB values the table is indexed by.
    pub encoding: TableEncoding,
}

impl ProfileTable {
    /// A table with its first data set. `None` unless `data.len() == 3 * dims product`, every
    /// dimension is at least 1, the table is not absurdly large and every value is finite.
    pub fn new(dims: [u32; 3], data: Vec<f32>, encoding: TableEncoding) -> Option<Self> {
        let n = dims.iter().try_fold(1usize, |a, &d| a.checked_mul(d as usize))?;
        let ok = dims.iter().all(|&d| d >= 1) && n <= MAX_TABLE_ENTRIES / 3 && data.len() == n * 3;
        (ok && data.iter().all(|v| v.is_finite())).then_some(ProfileTable { dims, data, data2: None, encoding })
    }

    /// Number of `(hue, sat, val)` entries per data set.
    pub fn len(&self) -> usize {
        self.data.len() / 3
    }

    /// Whether the table has no entries (never true for a validated table).
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// A DNG camera profile.
///
/// Build one from a `.dcp` file ([`CameraProfile::from_dcp`]) or take the one embedded in a
/// DNG (`FlashRaw::embedded_camera_profile` in the `flashraw` crate). Every field is optional
/// data; a profile with neither matrices nor tables does nothing.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct CameraProfile {
    /// ProfileName.
    pub name: Option<String>,
    /// UniqueCameraModel: the camera the profile was made for.
    pub unique_camera_model: Option<String>,
    /// ProfileCopyright.
    pub copyright: Option<String>,
    /// ProfileEmbedPolicy (0 allow copying, 1 embed if used, 2 embed never, 3 no restrictions).
    pub embed_policy: Option<u32>,
    /// ColorMatrix1/2 (XYZ to camera, 3 x 3) with their calibration illuminants.
    pub color_matrices: Vec<ColorMatrix>,
    /// ForwardMatrix1/2 (white balanced camera to XYZ D50, 3 x 3) with their illuminants.
    pub forward_matrices: Vec<ColorMatrix>,
    /// ProfileHueSatMapDims / ProfileHueSatMapData1/2 / ProfileHueSatMapEncoding.
    pub hue_sat_map: Option<ProfileTable>,
    /// ProfileLookTableDims / ProfileLookTableData / ProfileLookTableEncoding.
    pub look_table: Option<ProfileTable>,
    /// ProfileToneCurve as `(input, output)` points in `[0, 1]`, ascending in input.
    pub tone_curve: Option<Vec<[f32; 2]>>,
    /// BaselineExposureOffset in stops.
    pub baseline_exposure_offset: f64,
    /// DefaultBlackRender (0 auto, 1 none). Recorded, not applied.
    pub default_black_render: u32,
}

impl CameraProfile {
    /// Parse a DNG Camera Profile (`.dcp`) file.
    ///
    /// # Errors
    /// [`Error::UnsupportedFormat`] without the `IIRC` magic, [`Error::InvalidMetadata`] for
    /// tables whose size does not match their dimensions or contain non-finite values.
    pub fn from_dcp(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 8 || &bytes[0..4] != b"IIRC" {
            return Err(Error::UnsupportedFormat("not a DNG camera profile (missing IIRC magic)".into()));
        }
        let tiff = Tiff::parse_with(bytes, TiffOptions { follow_sub_ifds: false, ..TiffOptions::default() })?;
        let ifd = tiff.ifds.first().ok_or_else(|| Error::corrupt("camera profile without an IFD"))?;
        let p = Self::from_ifd(ifd, true)?;
        if p.is_empty() {
            return Err(Error::invalid_metadata("camera profile without matrices, tables or a tone curve"));
        }
        Ok(p)
    }

    /// The profile tags of a DNG's first IFD. Returns `None` when the file carries no profile
    /// data (no forward matrix, hue/sat map, look table or tone curve);
    /// ordinary colour matrices alone do not make a profile. Malformed tables are skipped.
    pub fn from_dng_ifd(ifd: &Ifd) -> Option<Self> {
        let p = Self::from_ifd(ifd, false).ok()?;
        let has_profile_data = !p.forward_matrices.is_empty()
            || p.hue_sat_map.is_some()
            || p.look_table.is_some()
            || p.tone_curve.is_some();
        has_profile_data.then_some(p)
    }

    fn from_ifd(ifd: &Ifd, strict: bool) -> Result<Self> {
        let mut p = CameraProfile {
            name: ifd.string(tags::PROFILE_NAME),
            unique_camera_model: ifd.string(tags::UNIQUE_CAMERA_MODEL),
            copyright: ifd.string(extra::PROFILE_COPYRIGHT),
            embed_policy: ifd.u32(tags::PROFILE_EMBED_POLICY),
            baseline_exposure_offset: ifd.f64(tags::BASELINE_EXPOSURE_OFFSET).filter(|v| v.is_finite()).unwrap_or(0.0),
            default_black_render: ifd.u32(extra::DEFAULT_BLACK_RENDER).unwrap_or(0),
            ..Default::default()
        };
        let illuminant = |tag: u16| Illuminant(ifd.u32(tag).unwrap_or(0) as u16);
        for (m, i) in
            [(tags::COLOR_MATRIX1, tags::CALIBRATION_ILLUMINANT1), (tags::COLOR_MATRIX2, tags::CALIBRATION_ILLUMINANT2)]
        {
            if let Some(e) = ifd.get(m) {
                let v = e.f32s();
                if v.len() == 9 && v.iter().all(|x| x.is_finite()) {
                    p.color_matrices.extend(ColorMatrix::new(illuminant(i), 3, v));
                }
            }
        }
        for (m, i) in [
            (tags::FORWARD_MATRIX1, tags::CALIBRATION_ILLUMINANT1),
            (tags::FORWARD_MATRIX2, tags::CALIBRATION_ILLUMINANT2),
        ] {
            if let Some(e) = ifd.get(m) {
                let v = e.f32s();
                if v.len() == 9 && v.iter().all(|x| x.is_finite()) {
                    p.forward_matrices.extend(ColorMatrix::new(illuminant(i), 3, v));
                }
            }
        }
        let encoding = |tag: u16| match ifd.u32(tag) {
            Some(1) => TableEncoding::Srgb,
            _ => TableEncoding::Linear,
        };
        let dims = |tag: u16| -> Option<[u32; 3]> {
            let v = ifd.get(tag)?.u32s();
            (v.len() >= 3).then(|| [v[0], v[1], v[2]])
        };
        let mut bad = None;
        if let (Some(d), Some(e1)) = (dims(tags::PROFILE_HUE_SAT_MAP_DIMS), ifd.get(tags::PROFILE_HUE_SAT_MAP_DATA1)) {
            let enc = encoding(extra::PROFILE_HUE_SAT_MAP_ENCODING);
            match ProfileTable::new(d, e1.f32s(), enc) {
                Some(mut t) => {
                    if let Some(e2) = ifd.get(tags::PROFILE_HUE_SAT_MAP_DATA2) {
                        let v = e2.f32s();
                        if v.len() == t.data.len() && v.iter().all(|x| x.is_finite()) {
                            t.data2 = Some(v);
                        } else {
                            bad = Some("ProfileHueSatMapData2 does not match ProfileHueSatMapDims");
                        }
                    }
                    p.hue_sat_map = Some(t);
                }
                None => bad = Some("ProfileHueSatMapData1 does not match ProfileHueSatMapDims"),
            }
        }
        if let (Some(d), Some(e)) = (dims(tags::PROFILE_LOOK_TABLE_DIMS), ifd.get(tags::PROFILE_LOOK_TABLE_DATA)) {
            match ProfileTable::new(d, e.f32s(), encoding(extra::PROFILE_LOOK_TABLE_ENCODING)) {
                Some(t) => p.look_table = Some(t),
                None => bad = Some("ProfileLookTableData does not match ProfileLookTableDims"),
            }
        }
        if let Some(e) = ifd.get(tags::PROFILE_TONE_CURVE) {
            let v = e.f32s();
            let pts: Vec<[f32; 2]> = v.chunks_exact(2).map(|c| [c[0], c[1]]).collect();
            let sane = pts.len() >= 2
                && pts.len() <= MAX_CURVE_POINTS
                && v.len() % 2 == 0
                && pts.iter().all(|c| c[0].is_finite() && c[1].is_finite())
                && pts.windows(2).all(|w| w[1][0] > w[0][0]);
            if sane {
                p.tone_curve = Some(pts);
            } else {
                bad = Some("ProfileToneCurve is not an ascending list of (input, output) pairs");
            }
        }
        match (bad, strict) {
            (Some(why), true) => Err(Error::invalid_metadata(why)),
            _ => Ok(p),
        }
    }

    /// Whether the profile carries nothing to apply.
    pub fn is_empty(&self) -> bool {
        self.color_matrices.is_empty()
            && self.forward_matrices.is_empty()
            && self.hue_sat_map.is_none()
            && self.look_table.is_none()
            && self.tone_curve.is_none()
            && self.baseline_exposure_offset == 0.0
    }

    /// Serialise as a `.dcp` file (little-endian TIFF with the `IIRC` magic), the inverse of
    /// [`CameraProfile::from_dcp`]. Used by tests and tools; matrices are written as floats.
    pub fn to_dcp(&self) -> Vec<u8> {
        type Entry = (u16, u16, u32, Vec<u8>);
        fn floats(e: &mut Vec<Entry>, tag: u16, v: &[f32]) {
            e.push((tag, 11, v.len() as u32, v.iter().flat_map(|x| x.to_le_bytes()).collect()));
        }
        fn longs(e: &mut Vec<Entry>, tag: u16, v: &[u32]) {
            e.push((tag, 4, v.len() as u32, v.iter().flat_map(|x| x.to_le_bytes()).collect()));
        }
        fn ascii(e: &mut Vec<Entry>, tag: u16, s: &Option<String>) {
            if let Some(s) = s {
                let mut b = s.as_bytes().to_vec();
                b.push(0);
                e.push((tag, 2, b.len() as u32, b));
            }
        }
        let mut entries: Vec<Entry> = Vec::new();
        ascii(&mut entries, tags::UNIQUE_CAMERA_MODEL, &self.unique_camera_model);
        ascii(&mut entries, tags::PROFILE_NAME, &self.name);
        ascii(&mut entries, extra::PROFILE_COPYRIGHT, &self.copyright);
        let color = [tags::COLOR_MATRIX1, tags::COLOR_MATRIX2];
        let forward = [tags::FORWARD_MATRIX1, tags::FORWARD_MATRIX2];
        let illuminants = [tags::CALIBRATION_ILLUMINANT1, tags::CALIBRATION_ILLUMINANT2];
        for (i, m) in self.color_matrices.iter().take(2).enumerate() {
            floats(&mut entries, color[i], &m.values);
            entries.push((illuminants[i], 3, 1, m.illuminant.0.to_le_bytes().to_vec()));
        }
        for (i, m) in self.forward_matrices.iter().take(2).enumerate() {
            floats(&mut entries, forward[i], &m.values);
            if self.color_matrices.len() <= i {
                entries.push((illuminants[i], 3, 1, m.illuminant.0.to_le_bytes().to_vec()));
            }
        }
        if let Some(t) = &self.hue_sat_map {
            longs(&mut entries, tags::PROFILE_HUE_SAT_MAP_DIMS, &t.dims);
            floats(&mut entries, tags::PROFILE_HUE_SAT_MAP_DATA1, &t.data);
            if let Some(d2) = &t.data2 {
                floats(&mut entries, tags::PROFILE_HUE_SAT_MAP_DATA2, d2);
            }
            if t.encoding == TableEncoding::Srgb {
                longs(&mut entries, extra::PROFILE_HUE_SAT_MAP_ENCODING, &[1]);
            }
        }
        if let Some(t) = &self.look_table {
            longs(&mut entries, tags::PROFILE_LOOK_TABLE_DIMS, &t.dims);
            floats(&mut entries, tags::PROFILE_LOOK_TABLE_DATA, &t.data);
            if t.encoding == TableEncoding::Srgb {
                longs(&mut entries, extra::PROFILE_LOOK_TABLE_ENCODING, &[1]);
            }
        }
        if let Some(c) = &self.tone_curve {
            let flat: Vec<f32> = c.iter().flatten().copied().collect();
            floats(&mut entries, tags::PROFILE_TONE_CURVE, &flat);
        }
        if let Some(e) = self.embed_policy {
            longs(&mut entries, tags::PROFILE_EMBED_POLICY, &[e]);
        }
        if self.baseline_exposure_offset != 0.0 {
            entries.push((tags::BASELINE_EXPOSURE_OFFSET, 12, 1, self.baseline_exposure_offset.to_le_bytes().to_vec()));
        }
        if self.default_black_render != 0 {
            longs(&mut entries, extra::DEFAULT_BLACK_RENDER, &[self.default_black_render]);
        }
        entries.sort_by_key(|e| e.0);
        let n = entries.len();
        let mut out = b"IIRC\x08\0\0\0".to_vec();
        out.extend((n as u16).to_le_bytes());
        let mut extra_off = 8 + 2 + n * 12 + 4;
        let mut extra: Vec<u8> = Vec::new();
        for (tag, typ, count, data) in &entries {
            out.extend(tag.to_le_bytes());
            out.extend(typ.to_le_bytes());
            out.extend(count.to_le_bytes());
            if data.len() <= 4 {
                let mut d = data.clone();
                d.resize(4, 0);
                out.extend(d);
            } else {
                out.extend((extra_off as u32).to_le_bytes());
                extra_off += data.len();
                extra.extend(data);
            }
        }
        out.extend(0u32.to_le_bytes());
        out.extend(extra);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CameraProfile {
        let mut hs = ProfileTable::new([2, 2, 1], (0..12).map(|i| i as f32 * 0.5).collect(), TableEncoding::Linear)
            .expect("table");
        hs.data2 = Some((0..12).map(|i| i as f32).collect());
        CameraProfile {
            name: Some("Test profile".into()),
            unique_camera_model: Some("Test Camera".into()),
            copyright: Some("CC0".into()),
            embed_policy: Some(3),
            color_matrices: vec![
                ColorMatrix::new(Illuminant::STANDARD_A, 3, vec![1.0, 0.1, 0.0, 0.0, 1.0, 0.1, 0.1, 0.0, 1.0]).unwrap(),
                ColorMatrix::new(Illuminant::D65, 3, vec![0.9, 0.0, 0.1, 0.0, 0.9, 0.1, 0.0, 0.1, 0.9]).unwrap(),
            ],
            forward_matrices: vec![
                ColorMatrix::new(Illuminant::STANDARD_A, 3, vec![0.8, 0.1, 0.1, 0.3, 0.6, 0.1, 0.0, 0.1, 0.7]).unwrap(),
                ColorMatrix::new(Illuminant::D65, 3, vec![0.7, 0.2, 0.1, 0.3, 0.7, 0.0, 0.0, 0.0, 0.8]).unwrap(),
            ],
            hue_sat_map: Some(hs),
            look_table: Some(
                ProfileTable::new([1, 2, 2], (0..12).map(|i| i as f32 + 0.25).collect(), TableEncoding::Srgb).unwrap(),
            ),
            tone_curve: Some(vec![[0.0, 0.0], [0.5, 0.7], [1.0, 1.0]]),
            baseline_exposure_offset: 0.5,
            default_black_render: 1,
        }
    }

    #[test]
    fn dcp_round_trips() {
        let p = sample();
        let bytes = p.to_dcp();
        assert_eq!(&bytes[..4], b"IIRC");
        let q = CameraProfile::from_dcp(&bytes).expect("parse");
        assert_eq!(q, p);
    }

    #[test]
    fn dcp_rejects_other_files() {
        let p = sample().to_dcp();
        let mut tiff = p.clone();
        tiff[..4].copy_from_slice(b"II*\0");
        assert!(matches!(CameraProfile::from_dcp(&tiff), Err(Error::UnsupportedFormat(_))));
        assert!(CameraProfile::from_dcp(b"IIRC").is_err());
        // Truncations never panic.
        for n in 0..p.len() {
            let _ = CameraProfile::from_dcp(&p[..n]);
        }
    }

    #[test]
    fn mismatched_table_is_an_error() {
        let mut p = sample();
        p.hue_sat_map.as_mut().unwrap().dims = [3, 3, 3];
        // Written with dimensions that do not match the data.
        let err = CameraProfile::from_dcp(&p.to_dcp()).unwrap_err();
        assert!(err.to_string().contains("ProfileHueSatMapData1"), "{err}");
    }

    #[test]
    fn table_validation() {
        assert!(ProfileTable::new([0, 1, 1], vec![], TableEncoding::Linear).is_none());
        assert!(ProfileTable::new([1, 1, 1], vec![0.0, 1.0], TableEncoding::Linear).is_none());
        assert!(ProfileTable::new([1, 1, 1], vec![0.0, f32::NAN, 1.0], TableEncoding::Linear).is_none());
        assert!(ProfileTable::new([u32::MAX, u32::MAX, 2], vec![0.0; 3], TableEncoding::Linear).is_none());
        assert!(ProfileTable::new([1, 1, 1], vec![0.0, 1.0, 1.0], TableEncoding::Linear).is_some());
    }
}
