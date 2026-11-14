//! Orientation resolution shared by the format decoders: the standard TIFF/EXIF tag,
//! vendor records, the per-camera policy from the camera database, and the
//! orientation of each embedded preview.
//!
//! The RAW image and its previews are resolved independently: a preview may carry its
//! own EXIF orientation (the camera's embedded JPEG), or may have been rendered
//! already rotated by the software that saved the file.

use flashraw_camera_db::Camera;
use flashraw_core::tiff::{Ifd, Tiff, TiffOptions, tags};
use flashraw_core::{Orientation, OrientationMetadata, OrientationPolicy, PreviewEncoding, PreviewInfo, RawMetadata};

/// The orientation policy for a camera: its database override, else the make's rule.
pub fn policy_for(meta: &RawMetadata, cam: Option<&Camera>) -> OrientationPolicy {
    let make = match (cam, meta.canonical_make.as_str()) {
        (Some(c), _) => c.canonical_make,
        (None, "") => flashraw_camera_db::canonical_make(&meta.make),
        (None, m) => m,
    };
    let model = match (cam, meta.canonical_model.as_str()) {
        (Some(c), _) => c.canonical_model,
        (None, "") => meta.model.as_str(),
        (None, m) => m,
    };
    OrientationPolicy::parse(flashraw_camera_db::orientation_policy(make, model, cam)).unwrap_or_default()
}

/// The standard `Orientation` tag across `ifds` (in order): the first valid non-Normal
/// value, else `Normal` when some IFD records a valid `1`, else `None` (no tag, or only
/// invalid values such as 0).
pub fn tiff_orientation<'a, 'b: 'a>(ifds: impl IntoIterator<Item = &'a Ifd<'b>>) -> Option<Orientation> {
    let mut normal = false;
    for ifd in ifds {
        match ifd.u32(tags::ORIENTATION).and_then(Orientation::from_exif_checked) {
            Some(Orientation::Normal) => normal = true,
            Some(o) => return Some(o),
            None => {}
        }
    }
    normal.then_some(Orientation::Normal)
}

/// The EXIF `Orientation` stored inside a JPEG's APP1 Exif segment, when it has one.
pub fn jpeg_exif_orientation(jpeg: &[u8]) -> Option<Orientation> {
    if jpeg.get(..2) != Some(&[0xFF, 0xD8][..]) {
        return None;
    }
    let mut pos = 2usize;
    // Metadata segments precede the image data; stop at SOS or the first non-marker.
    while pos + 4 <= jpeg.len() {
        if jpeg[pos] != 0xFF {
            return None;
        }
        let marker = jpeg[pos + 1];
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let len = u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]) as usize;
        if len < 2 {
            return None;
        }
        let body = jpeg.get(pos + 4..pos + 2 + len)?;
        if marker == 0xE1 && body.starts_with(b"Exif\0\0") {
            let opts = TiffOptions { follow_sub_ifds: false, max_ifds: 2, ..TiffOptions::default() };
            let tiff = Tiff::parse_with(&body[6..], opts).ok()?;
            return tiff.ifds.first()?.u32(tags::ORIENTATION).and_then(Orientation::from_exif_checked);
        }
        pos += 2 + len;
    }
    None
}

/// Resolve each preview's orientation independently of the RAW image:
///
/// - a JPEG preview with its own EXIF orientation uses it (it describes those pixels);
/// - a preview whose aspect already matches the displayed (rotated) image was rendered
///   rotated, so it needs no further rotation;
/// - otherwise the preview shares the RAW image's resolved orientation.
///
/// `width`/`height` are the RAW image dimensions as stored (before orientation).
pub fn resolve_previews(data: &[u8], previews: &mut [PreviewInfo], raw: Orientation, width: u32, height: u32) {
    for p in previews.iter_mut() {
        let own = if p.encoding == PreviewEncoding::Jpeg {
            usize::try_from(p.offset)
                .ok()
                .zip(usize::try_from(p.length).ok())
                .and_then(|(o, l)| data.get(o..o.checked_add(l)?))
                .and_then(jpeg_exif_orientation)
        } else {
            None
        };
        p.orientation = match own {
            Some(o) => o,
            None if raw.swaps_dimensions() && pre_rotated(p.width, p.height, width, height) => Orientation::Normal,
            None => raw,
        };
    }
}

/// True when a `pw x ph` preview is portrait while the `w x h` image is landscape (or
/// the reverse), i.e. it was stored already rotated by 90°.
fn pre_rotated(pw: u32, ph: u32, w: u32, h: u32) -> bool {
    pw > 0 && ph > 0 && pw != ph && w != h && (pw > ph) != (w > h)
}

/// Resolve the RAW orientation of `meta` from its records and store it in
/// `meta.orientation_info` / `meta.orientation`; returns the resolved value.
pub fn apply(
    meta: &mut RawMetadata,
    exif: Option<Orientation>,
    maker_note: Option<Orientation>,
    auto_rotate: Option<bool>,
    policy: OrientationPolicy,
) -> Orientation {
    let info = OrientationMetadata::resolve(exif, maker_note, auto_rotate, policy);
    meta.orientation_info = info;
    meta.orientation = info.resolved;
    info.resolved
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use flashraw_core::PreviewKind;

    /// A minimal JPEG: SOI, an APP1 Exif segment whose IFD0 holds `Orientation = o`, EOI.
    pub(crate) fn jpeg_with_orientation(o: u16) -> Vec<u8> {
        let mut tiff = b"II*\0\x08\0\0\0".to_vec();
        tiff.extend(1u16.to_le_bytes());
        tiff.extend(0x0112u16.to_le_bytes());
        tiff.extend(3u16.to_le_bytes());
        tiff.extend(1u32.to_le_bytes());
        tiff.extend((o as u32).to_le_bytes());
        tiff.extend(0u32.to_le_bytes());
        let mut seg = b"Exif\0\0".to_vec();
        seg.extend(tiff);
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xE1];
        j.extend(((seg.len() + 2) as u16).to_be_bytes());
        j.extend(seg);
        j.extend([0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        j
    }

    fn preview(offset: u64, length: u64, w: u32, h: u32) -> PreviewInfo {
        PreviewInfo::new(offset, length, w, h, PreviewEncoding::Jpeg, Orientation::Normal, PreviewKind::Preview)
    }

    #[test]
    fn reads_jpeg_exif_orientation() {
        assert_eq!(jpeg_exif_orientation(&jpeg_with_orientation(8)), Some(Orientation::Rotate270));
        assert_eq!(jpeg_exif_orientation(&jpeg_with_orientation(1)), Some(Orientation::Normal));
        assert_eq!(jpeg_exif_orientation(&jpeg_with_orientation(0)), None);
        assert_eq!(jpeg_exif_orientation(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]), None);
        assert_eq!(jpeg_exif_orientation(b"not a jpeg"), None);
        // Truncated segment lengths must not panic.
        let mut j = jpeg_with_orientation(6);
        j.truncate(20);
        assert_eq!(jpeg_exif_orientation(&j), None);
    }

    #[test]
    fn previews_are_resolved_independently() {
        let own = jpeg_with_orientation(8);
        let mut data = vec![0u8; 16];
        let at = data.len() as u64;
        data.extend(&own);
        let mut p = vec![
            // Own EXIF orientation wins over the RAW orientation.
            preview(at, own.len() as u64, 4000, 3000),
            // Portrait preview of a landscape sensor: rendered already rotated.
            preview(0, 4, 2000, 3008),
            // Landscape preview without EXIF: shares the RAW orientation.
            preview(0, 4, 3008, 2000),
        ];
        resolve_previews(&data, &mut p, Orientation::Rotate90, 3008, 2000);
        assert_eq!(p[0].orientation, Orientation::Rotate270);
        assert_eq!(p[1].orientation, Orientation::Normal);
        assert_eq!(p[2].orientation, Orientation::Rotate90);
        // A 180° RAW orientation cannot be detected from the aspect: kept.
        resolve_previews(&data, &mut p[1..], Orientation::Rotate180, 3008, 2000);
        assert_eq!(p[1].orientation, Orientation::Rotate180);
    }

    #[test]
    fn tiff_orientation_skips_invalid_values() {
        // One SHORT entry: Orientation = v, or ImageWidth when `v` is None.
        let ifd = |v: Option<u32>| {
            let mut b = b"II*\0\x08\0\0\0".to_vec();
            b.extend(1u16.to_le_bytes());
            b.extend(if v.is_some() { 0x0112u16 } else { 0x0100 }.to_le_bytes());
            b.extend(3u16.to_le_bytes());
            b.extend(1u32.to_le_bytes());
            b.extend(v.unwrap_or(64).to_le_bytes());
            b.extend(0u32.to_le_bytes());
            b
        };
        let (a, b, c, d) = (ifd(Some(0)), ifd(Some(1)), ifd(Some(6)), ifd(None));
        fn t(d: &[u8]) -> Tiff<'_> {
            Tiff::parse(d).unwrap()
        }
        let (ta, tb, tc, td) = (t(&a), t(&b), t(&c), t(&d));
        assert_eq!(tiff_orientation(&ta.ifds), None);
        assert_eq!(tiff_orientation(&td.ifds), None);
        assert_eq!(tiff_orientation(ta.ifds.iter().chain(&tb.ifds)), Some(Orientation::Normal));
        assert_eq!(tiff_orientation(tb.ifds.iter().chain(&tc.ifds)), Some(Orientation::Rotate90));
    }
}
