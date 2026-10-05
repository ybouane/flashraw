//! Sony maker-note model id (tag 0xB001): names the body when IFD0 Model is a
//! placeholder, as in pre-production firmware ("MODEL-NAME", "DSLR-A000", ...).

use flashraw_core::tiff::{Tiff, TiffOptions, tags};

/// Maker-note tag holding the model id.
const TAG_MODEL_ID: u16 = 0xB001;

/// Model id -> IFD0 Model of the production body. Every id was observed in production
/// files of that body (rebadged twins share an id; the first-sold name is listed).
const MODELS: &[(u16, &str)] = &[
    (2, "DSC-R1"),
    (256, "DSLR-A100"),
    (257, "DSLR-A900"),
    (258, "DSLR-A700"),
    (259, "DSLR-A200"),
    (260, "DSLR-A350"),
    (261, "DSLR-A300"),
    (263, "DSLR-A380"),
    (264, "DSLR-A330"),
    (265, "DSLR-A230"),
    (266, "DSLR-A290"),
    (269, "DSLR-A850"),
    (273, "DSLR-A550"),
    (274, "DSLR-A500"),
    (275, "DSLR-A450"),
    (278, "NEX-5"),
    (279, "NEX-3"),
    (280, "SLT-A33"),
    (281, "SLT-A55V"),
    (282, "DSLR-A560"),
    (283, "DSLR-A580"),
    (284, "NEX-C3"),
    (285, "SLT-A35"),
    (286, "SLT-A65V"),
    (287, "SLT-A77V"),
    (288, "NEX-5N"),
    (289, "NEX-7"),
    (291, "SLT-A37"),
    (292, "SLT-A57"),
    (293, "NEX-F3"),
    (294, "SLT-A99V"),
    (295, "NEX-6"),
    (296, "NEX-5R"),
    (297, "DSC-RX100"),
    (298, "DSC-RX1"),
    (302, "ILCE-3000"),
    (303, "SLT-A58"),
    (305, "NEX-3N"),
    (306, "ILCE-7"),
    (307, "NEX-5T"),
    (308, "DSC-RX100M2"),
    (309, "DSC-RX10"),
    (310, "DSC-RX1R"),
    (311, "ILCE-7R"),
    (312, "ILCE-6000"),
    (313, "ILCE-5000"),
    (317, "DSC-RX100M3"),
    (318, "ILCE-7S"),
    (319, "ILCA-77M2"),
    (339, "ILCE-5100"),
    (340, "ILCE-7M2"),
    (341, "DSC-RX100M4"),
    (342, "DSC-RX10M2"),
    (344, "DSC-RX1RM2"),
    (346, "ILCE-QX1"),
    (347, "ILCE-7RM2"),
    (350, "ILCE-7SM2"),
    (353, "ILCA-68"),
    (354, "ILCA-99M2"),
    (355, "DSC-RX10M3"),
    (356, "DSC-RX100M5"),
    (357, "ILCE-6300"),
    (358, "ILCE-9"),
    (360, "ILCE-6500"),
    (362, "ILCE-7RM3"),
    (363, "ILCE-7M3"),
    (364, "DSC-RX0"),
    (365, "DSC-RX10M4"),
    (366, "DSC-RX100M6"),
    (367, "DSC-HX99"),
    (369, "DSC-RX100M5A"),
    (371, "ILCE-6400"),
    (372, "DSC-RX0M2"),
    (373, "DSC-HX95"),
    (374, "DSC-RX100M7"),
    (375, "ILCE-7RM4"),
    (376, "ILCE-9M2"),
    (378, "ILCE-6600"),
    (379, "ILCE-6100"),
    (380, "ZV-1"),
    (381, "ILCE-7C"),
    (382, "ZV-E10"),
    (383, "ILCE-7SM3"),
    (384, "ILCE-1"),
    (385, "ILME-FX3"),
    (386, "ILCE-7RM3A"),
    (387, "ILCE-7RM4A"),
    (388, "ILCE-7M4"),
    (390, "ILCE-7RM5"),
    (391, "ILME-FX30"),
    (392, "ILCE-9M3"),
    (393, "ZV-E1"),
    (394, "ILCE-6700"),
    (395, "ZV-1M2"),
    (396, "ILCE-7CR"),
    (397, "ILCE-7CM2"),
    (399, "ZV-E10M2"),
    (400, "ILCE-1M2"),
    (401, "DSC-RX1RM3"),
    (402, "ILCE-6400A"),
    (404, "DSC-RX100M7A"),
    (406, "ILME-FX2"),
    (407, "ILCE-7M5"),
];

/// Production model name for a maker-note model id.
pub(crate) fn model_for_id(id: u16) -> Option<&'static str> {
    MODELS.binary_search_by_key(&id, |&(i, _)| i).ok().map(|i| MODELS[i].1)
}

/// IFD0 Model strings that do not name a body: "MODEL-NAME", or a model whose digits
/// are a run of zeros ("DSLR-A000", "SLT-A00", "NEX-00").
pub(crate) fn is_placeholder(model: &str) -> bool {
    let digits: Vec<u8> = model.bytes().filter(u8::is_ascii_digit).collect();
    model.trim().is_empty()
        || model.eq_ignore_ascii_case("MODEL-NAME")
        || (digits.len() >= 2 && digits.iter().all(|&d| d == b'0'))
}

/// The model id from the Sony maker note ("SONY DSC"/"SONY CAM" header + IFD, or a bare
/// IFD), with value offsets relative to the TIFF start.
pub(crate) fn makernote_model_id(data: &[u8], tiff: &Tiff) -> Option<u16> {
    let e = tiff.find_entry(tags::MAKER_NOTE)?;
    let start = e.value_pos;
    let header = data.get(start..start + 12)?;
    let off = if header.starts_with(b"SONY DSC ") || header.starts_with(b"SONY CAM ") { start + 12 } else { start };
    let opts = TiffOptions { follow_sub_ifds: false, ..Default::default() };
    let ifd = flashraw_core::tiff::parse_ifd_at(data, tiff.order, u32::try_from(off).ok()?, opts).ok()?;
    ifd.u32(TAG_MODEL_ID).and_then(|v| u16::try_from(v).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_unique() {
        assert!(MODELS.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(model_for_id(388), Some("ILCE-7M4"));
        assert_eq!(model_for_id(312), Some("ILCE-6000"));
        assert_eq!(model_for_id(1), None);
    }

    #[test]
    fn placeholders() {
        for m in ["MODEL-NAME", "model-name", "DSLR-A000", "SLT-A00", "NEX-00", ""] {
            assert!(is_placeholder(m), "{m}");
        }
        for m in ["ILCE-7M4", "DSC-RX0", "ILCE-1", "NEX-5", "DSLR-A100", "SLT-A99V"] {
            assert!(!is_placeholder(m), "{m}");
        }
    }

    /// Little-endian TIFF: IFD0 (Make, Model, ExifIFD) -> Exif IFD (MakerNote) -> Sony
    /// maker note with a "SONY DSC" header and tag 0xB001.
    fn tiff_with_model_id(model: &str, id: u16) -> Vec<u8> {
        let mut out = b"II*\0\x08\0\0\0".to_vec();
        let mut model_b = model.as_bytes().to_vec();
        model_b.push(0);
        model_b.resize(model_b.len().max(5).next_multiple_of(2), 0);
        let ifd0_len = 2 + 3 * 12 + 4;
        let make_off = 8 + ifd0_len;
        let model_off = make_off + 6;
        let exif_off = model_off + model_b.len();
        let exif_len = 2 + 12 + 4;
        let mn_off = exif_off + exif_len;
        let mut mn = b"SONY DSC \0\0\0".to_vec();
        mn.extend(1u16.to_le_bytes());
        mn.extend(TAG_MODEL_ID.to_le_bytes());
        mn.extend(3u16.to_le_bytes());
        mn.extend(1u32.to_le_bytes());
        mn.extend(id.to_le_bytes());
        mn.extend([0, 0, 0, 0, 0, 0]);
        let entry = |out: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32| {
            out.extend(tag.to_le_bytes());
            out.extend(typ.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value.to_le_bytes());
        };
        out.extend(3u16.to_le_bytes());
        entry(&mut out, tags::MAKE, 2, 5, make_off as u32);
        entry(&mut out, tags::MODEL, 2, model_b.len() as u32, model_off as u32);
        entry(&mut out, tags::EXIF_IFD, 4, 1, exif_off as u32);
        out.extend(0u32.to_le_bytes());
        out.extend(b"SONY\0\0");
        out.extend(&model_b);
        out.extend(1u16.to_le_bytes());
        entry(&mut out, tags::MAKER_NOTE, 7, mn.len() as u32, mn_off as u32);
        out.extend(0u32.to_le_bytes());
        out.extend(&mn);
        out
    }

    /// A corpus A7 IV whose IFD0 Model is overwritten with a placeholder opens with the
    /// same model, crop and colour matrix as the original.
    #[test]
    fn placeholder_model_resolves_like_the_production_body() {
        use crate::nikon::conventions::robustness::corpus_file;
        let rel = "Sony/ILCE-7M4/ILCE-7M4_DSC06676_FullFrame-LossLess-Compressed-Small.ARW";
        let Some(orig) = corpus_file(rel) else { return };
        let pos = Tiff::parse(&orig).unwrap().ifds[0].get(tags::MODEL).unwrap().value_pos;
        let mut patched = orig.clone();
        assert_eq!(&patched[pos..pos + 8], b"ILCE-7M4");
        patched[pos..pos + 8].copy_from_slice(b"ILCE-000");
        let opts = crate::common::OpenOptions::default();
        let (a, b) = (crate::open(&orig, &opts).unwrap(), crate::open(&patched, &opts).unwrap());
        assert_eq!(b.metadata().model, "ILCE-7M4");
        assert_eq!(b.metadata().extra("ifd0_model"), Some("ILCE-000"));
        assert_eq!(a.metadata().canonical_model, b.metadata().canonical_model);
        let (fa, fb) = (a.frame_info(), b.frame_info());
        assert_eq!((fa.active_area, fa.crop), (fb.active_area, fb.crop));
        assert_eq!(fa.color_matrices, fb.color_matrices);
        assert_eq!(fa.white_level, fb.white_level);
    }

    #[test]
    fn model_id_from_makernote() {
        let f = tiff_with_model_id("MODEL-NAME", 388);
        let tiff = Tiff::parse(&f).unwrap();
        assert_eq!(makernote_model_id(&f, &tiff), Some(388));
        // Truncated maker note: no id, no panic.
        let cut = &f[..f.len() - 10];
        if let Ok(t) = Tiff::parse(cut) {
            assert_eq!(makernote_model_id(cut, &t), None);
        }
    }
}
