//! Where an image's orientation comes from, and how competing records are resolved.
//!
//! A RAW file can state its orientation in more than one place: the standard
//! TIFF/EXIF `Orientation` tag and a vendor record (a MakerNote field such as Canon's
//! CameraOrientation, the rotation saved by a vendor's own editing software, or the
//! EXIF block of the camera's own embedded JPEG). The records do not always agree,
//! and which one describes the image depends on how the camera (or software) wrote
//! them, so the choice is made per camera with an [`OrientationPolicy`].
//!
//! [`OrientationMetadata`] keeps both records, the chosen value and why it was
//! chosen. [`RawMetadata::orientation`](crate::RawMetadata::orientation) stays the
//! resolved value.

use crate::types::Orientation;

/// Which record the resolved orientation was taken from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum OrientationSource {
    /// The standard TIFF/EXIF `Orientation` tag (or the container's own standard
    /// orientation field for non-TIFF containers).
    Exif,
    /// A vendor record: MakerNote, vendor edit data or the camera's embedded JPEG.
    MakerNote,
    /// A fixed per-camera rule (e.g. a sensor that is mounted rotated in the body).
    CameraProfileOverride,
    /// Nothing usable was recorded; the image is shown as stored.
    #[default]
    Default,
}

impl OrientationSource {
    /// Stable lower-case name (`"exif"`, `"maker-note"`, `"camera-profile"`, `"default"`).
    pub fn name(self) -> &'static str {
        match self {
            OrientationSource::Exif => "exif",
            OrientationSource::MakerNote => "maker-note",
            OrientationSource::CameraProfileOverride => "camera-profile",
            OrientationSource::Default => "default",
        }
    }
}

/// How the standard and vendor orientation records of a camera are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum OrientationPolicy {
    /// Only the standard tag is used; vendor records are informational.
    #[default]
    ExifOnly,
    /// The vendor record wins whenever it is present.
    MakerNotePreferred,
    /// The vendor record is used when the standard tag is Normal, missing or invalid
    /// (for cameras that only write the rotation into the vendor record).
    MakerNoteWhenExifNormal,
    /// Canon: the camera writes the standard tag from its orientation sensor only when
    /// its Auto Rotate setting is on, and always records the sensor in the MakerNote
    /// (CameraOrientation). A Normal standard tag is therefore what the camera chose
    /// unless the recorded Auto Rotate setting says it was on; the MakerNote replaces
    /// the standard tag only when that tag is missing or invalid, or when Auto Rotate
    /// was on and the tag still says Normal.
    CanonAutoRotateAware,
}

impl OrientationPolicy {
    /// Stable kebab-case name, as used in the camera database.
    pub fn name(self) -> &'static str {
        match self {
            OrientationPolicy::ExifOnly => "exif-only",
            OrientationPolicy::MakerNotePreferred => "makernote-preferred",
            OrientationPolicy::MakerNoteWhenExifNormal => "makernote-when-exif-normal",
            OrientationPolicy::CanonAutoRotateAware => "canon-auto-rotate-aware",
        }
    }

    /// Parse a name produced by [`OrientationPolicy::name`].
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "exif-only" => OrientationPolicy::ExifOnly,
            "makernote-preferred" => OrientationPolicy::MakerNotePreferred,
            "makernote-when-exif-normal" => OrientationPolicy::MakerNoteWhenExifNormal,
            "canon-auto-rotate-aware" => OrientationPolicy::CanonAutoRotateAware,
            _ => return None,
        })
    }
}

/// Orientation records of a file and the value resolved from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct OrientationMetadata {
    /// The standard TIFF/EXIF `Orientation` tag; `None` when missing or not a valid
    /// value (1-8).
    pub exif: Option<Orientation>,
    /// The vendor orientation record (MakerNote or equivalent), when present.
    pub maker_note: Option<Orientation>,
    /// The camera's Auto Rotate setting (`Some(true)` = on), when recorded.
    pub auto_rotate: Option<bool>,
    /// The orientation to apply to the image (equal to `RawMetadata::orientation`).
    pub resolved: Orientation,
    /// Which record `resolved` comes from.
    pub source: OrientationSource,
    /// The policy that chose `resolved`.
    pub policy: OrientationPolicy,
}

impl OrientationMetadata {
    /// Combine the records of a file under `policy`.
    pub fn resolve(
        exif: Option<Orientation>,
        maker_note: Option<Orientation>,
        auto_rotate: Option<bool>,
        policy: OrientationPolicy,
    ) -> Self {
        use OrientationSource as S;
        let normal = Orientation::Normal;
        let from_exif = || exif.map(|o| (o, S::Exif));
        let from_mn = || maker_note.map(|o| (o, S::MakerNote));
        let chosen = match policy {
            OrientationPolicy::ExifOnly => from_exif(),
            OrientationPolicy::MakerNotePreferred => from_mn().or_else(from_exif),
            OrientationPolicy::MakerNoteWhenExifNormal => match exif {
                Some(o) if o != normal => from_exif(),
                _ => from_mn().or_else(from_exif),
            },
            OrientationPolicy::CanonAutoRotateAware => match (exif, maker_note) {
                (Some(o), _) if o != normal => from_exif(),
                (None, _) => from_mn(),
                (Some(_), Some(m)) if m != normal && auto_rotate == Some(true) => from_mn(),
                (Some(_), _) => from_exif(),
            },
        };
        let (resolved, source) = chosen.unwrap_or((normal, S::Default));
        OrientationMetadata { exif, maker_note, auto_rotate, resolved, source, policy }
    }

    /// Only the container's standard orientation field is known (`Normal` is treated
    /// as "nothing recorded").
    pub fn from_container(orientation: Orientation) -> Self {
        let exif = (orientation != Orientation::Normal).then_some(orientation);
        Self::resolve(exif, None, None, OrientationPolicy::ExifOnly)
    }

    /// A fixed per-camera orientation that replaces whatever the file records.
    pub fn camera_override(mut self, orientation: Orientation) -> Self {
        self.resolved = orientation;
        self.source = OrientationSource::CameraProfileOverride;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Orientation::*;
    use OrientationPolicy as P;
    use OrientationSource as S;

    fn r(e: Option<Orientation>, m: Option<Orientation>, a: Option<bool>, p: P) -> (Orientation, S) {
        let o = OrientationMetadata::resolve(e, m, a, p);
        assert_eq!((o.exif, o.maker_note, o.auto_rotate, o.policy), (e, m, a, p));
        (o.resolved, o.source)
    }

    #[test]
    fn exif_only_ignores_vendor_records() {
        assert_eq!(r(Some(Normal), Some(Rotate90), None, P::ExifOnly), (Normal, S::Exif));
        assert_eq!(r(Some(Rotate270), Some(Rotate90), None, P::ExifOnly), (Rotate270, S::Exif));
        assert_eq!(r(None, Some(Rotate90), None, P::ExifOnly), (Normal, S::Default));
        assert_eq!(r(None, None, None, P::ExifOnly), (Normal, S::Default));
    }

    #[test]
    fn maker_note_preferred() {
        assert_eq!(r(Some(Rotate270), Some(Rotate90), None, P::MakerNotePreferred), (Rotate90, S::MakerNote));
        assert_eq!(r(Some(Rotate270), Some(Normal), None, P::MakerNotePreferred), (Normal, S::MakerNote));
        assert_eq!(r(Some(Rotate180), None, None, P::MakerNotePreferred), (Rotate180, S::Exif));
        assert_eq!(r(None, None, None, P::MakerNotePreferred), (Normal, S::Default));
    }

    #[test]
    fn maker_note_when_exif_normal() {
        let p = P::MakerNoteWhenExifNormal;
        assert_eq!(r(Some(Normal), Some(Rotate90), None, p), (Rotate90, S::MakerNote));
        assert_eq!(r(None, Some(Rotate270), None, p), (Rotate270, S::MakerNote));
        assert_eq!(r(Some(Rotate180), Some(Rotate90), None, p), (Rotate180, S::Exif));
        assert_eq!(r(Some(Normal), None, None, p), (Normal, S::Exif));
        assert_eq!(r(Some(Normal), Some(Normal), None, p), (Normal, S::MakerNote));
    }

    #[test]
    fn canon_auto_rotate_aware() {
        let p = P::CanonAutoRotateAware;
        // The camera tagged the rotation itself.
        assert_eq!(r(Some(Rotate270), Some(Rotate270), Some(true), p), (Rotate270, S::Exif));
        assert_eq!(r(Some(Rotate90), Some(Normal), None, p), (Rotate90, S::Exif));
        // Auto Rotate off: the camera deliberately left the image unrotated.
        assert_eq!(r(Some(Normal), Some(Rotate270), Some(false), p), (Normal, S::Exif));
        // Setting not recorded: a Normal tag is the camera's choice.
        assert_eq!(r(Some(Normal), Some(Rotate90), None, p), (Normal, S::Exif));
        // Auto Rotate on but the tag says Normal: the sensor record is authoritative.
        assert_eq!(r(Some(Normal), Some(Rotate90), Some(true), p), (Rotate90, S::MakerNote));
        // Missing or invalid tag: the MakerNote fills in.
        assert_eq!(r(None, Some(Rotate270), None, p), (Rotate270, S::MakerNote));
        assert_eq!(r(None, Some(Normal), Some(false), p), (Normal, S::MakerNote));
        assert_eq!(r(None, None, None, p), (Normal, S::Default));
    }

    #[test]
    fn container_and_override() {
        let c = OrientationMetadata::from_container(Rotate90);
        assert_eq!((c.exif, c.resolved, c.source), (Some(Rotate90), Rotate90, S::Exif));
        let c = OrientationMetadata::from_container(Normal);
        assert_eq!((c.exif, c.resolved, c.source), (None, Normal, S::Default));
        let o = c.camera_override(Rotate90);
        assert_eq!((o.resolved, o.source), (Rotate90, S::CameraProfileOverride));
    }

    #[test]
    fn policy_names_round_trip() {
        for p in [P::ExifOnly, P::MakerNotePreferred, P::MakerNoteWhenExifNormal, P::CanonAutoRotateAware] {
            assert_eq!(P::parse(p.name()), Some(p));
        }
        assert_eq!(P::parse("bogus"), None);
    }
}
