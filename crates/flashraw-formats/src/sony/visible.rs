//! The part of a Sony sensor buffer that is image: the columns and rows on the right and bottom edge
//! that hold padding or dark reference pixels are left out.
//!
//! The files do not state this rectangle (their crop tags describe the image area inside it), so it is
//! a table of camera facts keyed by body, buffer width and, for a few sizes, buffer height. The left and
//! top margins of Sony raws are always zero.
//!
//! Every [`RULES`] row carries how well it is known ([`Basis`]):
//! * `Verified`: the decoded sample data itself ends at the stated size (replicated or zero padding on
//!   the right/bottom of the buffer, measured on sample files);
//! * `FileConsistent`: a sample file decodes with the value and the area contains the file's own crop
//!   rectangle, but the margin is dark reference data that padding statistics cannot see;
//! * `Unverified`: no sample file reaches the row; kept as it was.
//!
//! Rows are tried in order and the first match decides, so the order encodes the precedence between the
//! body-specific and the width-generic rows.

/// Which bodies (IFD0 Model after placeholder resolution, case sensitive) a rule covers.
#[derive(Clone, Copy)]
enum Bodies {
    Any,
    Exact(&'static [&'static str]),
    Prefix(&'static [&'static str]),
    Contains(&'static [&'static str]),
}

impl Bodies {
    fn matches(self, model: &str) -> bool {
        match self {
            Bodies::Any => true,
            Bodies::Exact(list) => list.contains(&model),
            Bodies::Prefix(list) => list.iter().any(|p| model.starts_with(p)),
            Bodies::Contains(list) => list.iter().any(|p| model.contains(p)),
        }
    }
}

/// Which buffer sizes a rule covers.
#[derive(Clone, Copy)]
enum Sizes {
    Any,
    Width(u32),
    Exactly(u32, u32),
    /// Width, and height strictly below the limit.
    WidthShorterThan(u32, u32),
    /// Width, and height at least the limit.
    WidthAtLeast(u32, u32),
}

impl Sizes {
    fn matches(self, w: u32, h: u32) -> bool {
        match self {
            Sizes::Any => true,
            Sizes::Width(x) => w == x,
            Sizes::Exactly(x, y) => w == x && h == y,
            Sizes::WidthShorterThan(x, y) => w == x && h < y,
            Sizes::WidthAtLeast(x, y) => w == x && h >= y,
        }
    }
}

/// One side of the result.
#[derive(Clone, Copy)]
enum Side {
    /// The whole buffer side.
    Full,
    /// The buffer side minus a margin (never below 0).
    Less(u32),
    /// A fixed length.
    Is(u32),
}

impl Side {
    fn apply(self, full: u32) -> u32 {
        match self {
            Side::Full => full,
            Side::Less(m) => full.saturating_sub(m),
            Side::Is(v) => v,
        }
    }
}

/// How well a rule is known (see the module documentation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub(super) enum Basis {
    Verified,
    FileConsistent,
    Unverified,
}

#[allow(dead_code)] // `basis` documents how well the row is known
struct Rule {
    bodies: Bodies,
    sizes: Sizes,
    width: Side,
    height: Side,
    basis: Basis,
}

const fn rule(bodies: Bodies, sizes: Sizes, width: Side, height: Side, basis: Basis) -> Rule {
    Rule { bodies, sizes, width, height, basis }
}

use Basis::{FileConsistent as Fc, Unverified as Un, Verified as Ve};
use Bodies::{Any as AnyBody, Contains, Exact, Prefix};
use Side::{Full, Is, Less};
use Sizes::{Any as AnySize, Exactly, Width, WidthAtLeast, WidthShorterThan};

/// The 61 MP bodies.
const FAMILY_61MP: Bodies = Exact(&["ILCE-7RM5", "ILCE-7CR", "ILX-LR1"]);
/// The 33 MP / 26 MP generation of the full-frame bodies introduced in 2021 and later.
const FAMILY_7M4: Bodies = Exact(&["ILCE-7M4", "ILCE-7CM2", "ILME-FX2", "ILCE-7M5"]);
/// The APS-C bodies of the 2022 generation.
const FAMILY_6700: Bodies = Exact(&["ILCE-6700", "ZV-E10M2", "ILME-FX30"]);
const A1_FAMILY: Bodies = Exact(&["ILCE-1", "ILCE-1M2"]);

/// Visible-area rules in precedence order: the 61 MP family decides every width; then the widths that do
/// not depend on the body (3984 to 5504); then the global-shutter body; then the wider generic widths; then
/// the remaining bodies by size; rows left over are not trimmed.
#[rustfmt::skip]
const RULES: &[Rule] = &[
    // 61 MP family (every width is decided here).
    rule(FAMILY_61MP, Width(6304),           Full,     Full, Ve),
    rule(FAMILY_61MP, Width(6656),           Is(6272), Is(4180), Fc),
    rule(FAMILY_61MP, Width(9728),           Is(9566), Is(6374), Fc),
    rule(FAMILY_61MP, Width(5120),           Is(4776), Is(3180), Fc),
    rule(FAMILY_61MP, Width(9600),           Less(36), Full, Fc),
    // Unverified: no sample has another width.
    rule(FAMILY_61MP, AnySize,                   Less(32), Full, Un),

    // Widths independent of the body.
    rule(AnyBody, Width(3984),                   Is(3925), Full, Fc),
    rule(AnyBody, Width(4288),                   Less(32), Full, Ve),
    rule(Exact(&["DSLR-A350"]), Width(4600), Full,     Less(4), Ve),
    rule(AnyBody, Width(4600),                   Full,     Full, Ve),
    rule(AnyBody, WidthShorterThan(4928, 3280),  Less(8),  Full, Ve),
    rule(AnyBody, Width(4928),                   Full,     Full, Ve),
    rule(AnyBody, WidthAtLeast(5504, 3665),      Less(8),  Full, Ve),
    rule(AnyBody, Width(5504),                   Less(32), Full, Ve),

    // ILCE-9M3 (global shutter), ahead of the generic wide rows.
    rule(Exact(&["ILCE-9M3"]), Width(6048),  Less(36), Full, Fc),
    rule(Exact(&["ILCE-9M3"]), Width(6144),  Is(6012), Is(4020), Fc),
    rule(Exact(&["ILCE-9M3"]), Width(4096),  Is(3944), Is(2644), Fc),
    rule(Exact(&["ILCE-9M3"]), Width(3072),  Is(3016), Is(2008), Ve),
    rule(Exact(&["ILCE-9M3"]), Width(3968),  Is(3948), Full, Fc),
    // Unverified: no sample has another width.
    rule(Exact(&["ILCE-9M3"]), AnySize,          Less(32), Full, Un),

    // Wide widths independent of the body.
    rule(Contains(&["RX1", "A99"]), Width(6048), Less(30), Full, Fc),
    rule(AnyBody, Width(6048),                   Less(24), Full, Ve),
    rule(AnyBody, Width(7392),                   Less(30), Full, Fc),
    rule(AnyBody, Width(8000),                   Less(32), Full, Ve),
    rule(AnyBody, Width(9600),                   Less(32), Full, Fc),

    // Bodies by size.
    rule(Exact(&["ZV-E1"]), Exactly(4608, 3072), Is(4256), Is(2846), Fc),
    // Unverified: no sample has another size.
    rule(Exact(&["ZV-E1"]), AnySize,             Full, Full, Un),

    rule(A1_FAMILY, Exactly(8704, 6144),     Is(8660), Is(5784), Fc),
    rule(A1_FAMILY, Width(8672),             Less(12), Full, Fc),
    rule(A1_FAMILY, Exactly(6144, 4096),     Is(5636), Is(3768), Fc),
    rule(A1_FAMILY, Width(5664),             Less(28), Full, Fc),
    rule(A1_FAMILY, Width(5632),             Less(4),  Is(3756), Fc),
    rule(A1_FAMILY, Width(4608),             Is(4332), Is(2892), Fc),
    // Unverified: no sample has another size.
    rule(A1_FAMILY, AnySize,                     Full, Full, Un),

    rule(FAMILY_7M4, Exactly(7168, 5120),    Is(7028), Is(4688), Fc),
    rule(FAMILY_7M4, Width(4736),            Is(4692), Full, Fc),
    rule(FAMILY_7M4, Width(5120),            Is(4624), Is(3080), Fc),
    rule(FAMILY_7M4, Width(3584),            Is(3516), Is(2344), Fc),
    rule(FAMILY_7M4, Width(7040),            Less(12), Full, Fc),
    rule(FAMILY_7M4, AnySize,                    Full, Full, Ve),

    rule(FAMILY_6700, Width(6656),           Is(6240), Is(4168), Fc),
    rule(FAMILY_6700, AnySize,                   Less(32), Full, Fc),

    // Everything else is not trimmed.
    rule(AnyBody, AnySize,                           Full, Full, Ve),
];

/// APS-C crop modes of the full-frame bodies: the buffer is a cropped readout whose right 32 columns are
/// not image. Applied after the table above.
#[allow(dead_code)] // `basis` documents how well the row is known
struct CropMode {
    bodies: Bodies,
    /// Buffer widths in `[from, to)`.
    from: u32,
    to: u32,
    basis: Basis,
}

#[rustfmt::skip]
const CROP_MODES: &[CropMode] = &[
    // 61 MP bodies in APS-C mode (width 6304 has samples; 5216 has none).
    CropMode { bodies: Prefix(&["ILCE-7RM"]),  from: 5216, to: 5217, basis: Un },
    CropMode { bodies: Prefix(&["ILCE-7RM"]),  from: 6304, to: 6305, basis: Fc },
    CropMode { bodies: Exact(&["ILCA-99M2"]),  from: 5216, to: 5217, basis: Un },
    CropMode { bodies: Exact(&["ILCA-99M2"]),  from: 6304, to: 6305, basis: Un },
    // Unverified: no samples of these bodies in crop mode.
    CropMode { bodies: Exact(&["ILCE-7R"]),    from: 4580, to: 5020, basis: Un },
    CropMode { bodies: Exact(&["ILCE-7"]),     from: 3968, to: 3969, basis: Un },
    CropMode { bodies: Prefix(&["ILCE-7M", "SLT-A99"]), from: 3751, to: 4120, basis: Un },
    CropMode { bodies: Exact(&["ILCE-9"]),     from: 3751, to: 4120, basis: Un },
    CropMode { bodies: Prefix(&["ILCE-7S"]),   from: 2816, to: 2817, basis: Fc },
];

/// Width and height of the visible area of a `w x h` buffer of body `model`. An ARQ buffer is
/// trimmed by a fixed margin and nothing else applies.
pub(super) fn visible_size(model: &str, w: u32, h: u32, arq: bool) -> (u32, u32) {
    if arq {
        return (w.saturating_sub(if w > 12_000 { 64 } else { 32 }), h);
    }
    let rule =
        RULES.iter().find(|r| r.bodies.matches(model) && r.sizes.matches(w, h)).expect("RULES ends with a catch-all");
    let (mut vw, vh) = (rule.width.apply(w), rule.height.apply(h));
    if CROP_MODES.iter().any(|c| c.bodies.matches(model) && (c.from..c.to).contains(&w)) {
        vw = w.saturating_sub(32);
    }
    (vw.min(w), vh.min(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_of_known_bodies() {
        assert_eq!(visible_size("ILCE-7M4", 7168, 5120, false), (7028, 4688));
        assert_eq!(visible_size("DSLR-A350", 4600, 3072, false), (4600, 3068));
        assert_eq!(visible_size("DSC-RX100M3", 5504, 3648, false), (5472, 3648));
        assert_eq!(visible_size("DSC-RX100M3", 5504, 3680, false), (5496, 3680));
        assert_eq!(visible_size("ILCE-9M3", 3072, 2048, false), (3016, 2008));
        assert_eq!(visible_size("ILCE-6700", 5504, 3648, false), (5472, 3648)); // the generic width wins
        assert_eq!(visible_size("ILCE-7RM5", 6304, 4180, false), (6272, 4180)); // crop mode
        assert_eq!(visible_size("ILCE-7RM3", 8000, 5320, true), (7968, 5320));
        assert_eq!(visible_size("ILCE-7RM3", 12_800, 5320, true), (12_736, 5320));
        assert_eq!(visible_size("ILCE-7M3", 6000, 4000, false), (6000, 4000));
    }

    #[test]
    fn never_exceeds_the_buffer() {
        for w in [0, 1, 31, 32, 33, 2816, 6048, 100_000] {
            for m in ["", "ILCE-9M3", "ILCE-7RM5", "ILCE-6700", "ILCE-7R"] {
                let (vw, vh) = visible_size(m, w, 7, false);
                assert!(vw <= w && vh <= 7);
            }
        }
    }

    #[test]
    fn last_rule_is_the_catch_all() {
        let last = RULES.last().unwrap();
        assert!(matches!(last.bodies, Bodies::Any) && matches!(last.sizes, Sizes::Any));
        assert!(RULES.iter().any(|r| r.basis == Basis::Unverified));
    }
}
