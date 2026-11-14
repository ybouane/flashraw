//! Canon sensor geometry keyed by the raw buffer size.
//!
//! Canon files do not describe their optical-black borders reliably, so the readers keep a table
//! keyed by the decoded buffer size: where the image starts, how much is cut off on the right and
//! bottom, which column bands inside the optical-black margins the black level is measured from and,
//! rarely, the colour filter phase at the active-area origin.
//!
//! The rows of [`LAYOUTS`] are measured properties of the sensors, not something a file states.
//! Each row carries one of these flags in the comment above it:
//!
//! * `verified`: the decoded sensor data of at least one corpus file shows the dark-to-image
//!   transition within two pixels of the stored margin;
//! * `verified-partly`: as above, but the other corpus files of that size are scene limited;
//! * `contradicted-by-pixels`: a corpus file shows the transition elsewhere; the stored value is kept;
//! * `unverified`: no sample file; the value is kept as it has always been applied.
//!
//! Only the first eight rows (the CRW-era sensors) are used by the CRW reader.

/// How the optical-black column bands of a layout are placed.
///
/// The left band is `[left_start, left - left_guard)`, the right band is
/// `[left + width + right_gap, raw_width - right_trim)`; the usual values are 2, 2, 2, 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BandParams {
    /// First column of the left band.
    pub left_start: u32,
    /// Columns kept between the end of the left band and the active area.
    pub left_guard: u32,
    /// Columns between the active area and the start of the right band.
    pub right_gap: u32,
    /// Columns cut off the right edge of the buffer for the end of the right band.
    pub right_trim: u32,
}

/// One sensor layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorLayout {
    pub raw_width: u32,
    pub raw_height: u32,
    pub left: u32,
    pub top: u32,
    /// Columns / rows between the active area and the right / bottom edge of the buffer.
    pub right_trim: u32,
    pub bottom_trim: u32,
    pub band: BandParams,
    /// Colour filter pattern at the active-area origin when the table forces it (`"GBRG"`), else `None`.
    pub forced_cfa: Option<&'static str>,
}

/// Visible geometry of a layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

/// Build one table row: buffer size, origin, right / bottom trim, band parameters
/// `[left_start, left_guard, right_gap, right_trim]`, forced pattern.
#[allow(clippy::too_many_arguments)]
const fn row(
    raw_width: u32,
    raw_height: u32,
    left: u32,
    top: u32,
    right_trim: u32,
    bottom_trim: u32,
    band: [u32; 4],
    forced_cfa: Option<&'static str>,
) -> SensorLayout {
    SensorLayout {
        raw_width,
        raw_height,
        left,
        top,
        right_trim,
        bottom_trim,
        band: BandParams { left_start: band[0], left_guard: band[1], right_gap: band[2], right_trim: band[3] },
        forced_cfa,
    }
}

/// The known layouts, in the order of the specification's table (57 rows).
///
/// DATA: edit the rows, not the code around them.
pub static LAYOUTS: &[SensorLayout] = &[
    // unverified
    row(1944, 1416, 0, 0, 48, 0, [2, 2, 2, 0], None),
    // verified
    row(2144, 1560, 4, 8, 52, 2, [2, 2, 2, 25], None),
    // verified
    row(2224, 1456, 48, 6, 0, 2, [2, 2, 2, 0], None),
    // verified
    row(2376, 1728, 12, 6, 52, 2, [2, 2, 2, 0], None),
    // verified
    row(2672, 1968, 12, 6, 44, 2, [2, 2, 2, 0], None),
    // verified
    row(3152, 2068, 64, 12, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(3160, 2344, 44, 12, 4, 4, [2, 2, 2, 0], None),
    // verified
    row(3344, 2484, 4, 6, 52, 6, [2, 2, 2, 0], None),
    // verified
    row(3516, 2328, 42, 14, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(3596, 2360, 74, 12, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(3744, 2784, 52, 12, 8, 12, [2, 2, 2, 0], None),
    // verified
    row(3944, 2622, 30, 18, 6, 2, [2, 2, 2, 0], None),
    // verified
    row(3948, 2622, 42, 18, 0, 2, [2, 2, 2, 0], None),
    // verified
    row(3984, 2622, 76, 20, 0, 2, [2, 2, 2, 0], None),
    // verified
    row(4032, 2656, 112, 44, 10, 0, [2, 2, 2, 0], None),
    // verified
    row(4104, 3048, 48, 12, 24, 12, [2, 2, 2, 0], None),
    // unverified
    row(4116, 2178, 4, 2, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(4152, 2772, 192, 12, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(4160, 3124, 104, 11, 8, 65, [2, 2, 2, 0], None),
    // verified
    row(4176, 3062, 96, 17, 8, 0, [2, 18, 2, 7], Some("GBRG")),
    // contradicted-by-pixels
    row(4192, 3062, 96, 17, 24, 0, [2, 18, 2, 0], Some("GBRG")),
    // verified
    row(4312, 2876, 22, 18, 0, 2, [2, 2, 2, 0], None),
    // verified
    row(4352, 2850, 144, 46, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(4352, 2874, 62, 18, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(4476, 2954, 90, 34, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(4480, 3348, 12, 10, 36, 12, [2, 2, 2, 18], Some("GBRG")),
    // verified
    row(4480, 3366, 80, 50, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(4496, 3366, 80, 50, 12, 0, [2, 2, 2, 0], None),
    // verified
    row(4768, 3516, 96, 16, 0, 0, [2, 18, 2, 0], None),
    // verified-partly
    row(4832, 3204, 62, 26, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(4832, 3228, 62, 51, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(5108, 3349, 98, 13, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(5120, 3318, 142, 45, 62, 0, [2, 2, 2, 0], None),
    // verified
    row(5280, 3528, 72, 52, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(5344, 3516, 142, 51, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(5344, 3584, 126, 100, 0, 2, [2, 2, 2, 0], None),
    // verified
    row(5344, 3950, 98, 18, 0, 0, [2, 26, 2, 0], None),
    // verified
    row(5360, 3516, 158, 51, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(5568, 3708, 72, 38, 0, 0, [2, 2, 2, 0], None),
    // verified-partly
    row(5632, 3710, 96, 17, 0, 0, [2, 18, 2, 0], Some("GBRG")),
    // verified
    row(5712, 3774, 62, 20, 10, 2, [2, 2, 2, 0], None),
    // verified
    row(5792, 3804, 158, 51, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(5920, 3950, 122, 80, 2, 0, [2, 2, 2, 0], None),
    // verified
    row(6096, 4051, 76, 35, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(6096, 4056, 72, 34, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(6288, 4056, 264, 36, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(6384, 4224, 120, 44, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(6880, 4544, 136, 42, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(6888, 4546, 146, 48, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(7128, 4732, 144, 72, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(8896, 5920, 160, 64, 0, 0, [2, 2, 2, 0], None),
    // verified
    row(6192, 4152, 160, 120, 0, 0, [2, 2, 2, 0], None),
    // contradicted-by-pixels
    row(6192, 4060, 168, 52, 24, 8, [34, 50, 34, 0], None),
    // verified
    row(6188, 4120, 154, 96, 12, 0, [34, 50, 34, 0], None),
    // verified
    row(8480, 5650, 258, 162, 0, 0, [130, 4, 130, 0], None),
    // verified
    row(5376, 3574, 258, 162, 0, 0, [130, 4, 130, 0], None),
    // unverified
    row(6160, 4144, 128, 120, 0, 0, [2, 2, 2, 0], None),
];

/// The layout whose buffer size is `raw_width` x `raw_height`.
pub fn find(raw_width: u32, raw_height: u32) -> Option<&'static SensorLayout> {
    LAYOUTS.iter().find(|l| l.raw_width == raw_width && l.raw_height == raw_height)
}

impl SensorLayout {
    /// Active rectangle before the even-origin rounding the readers apply.
    pub fn geometry(&self) -> Geometry {
        Geometry {
            left: self.left,
            top: self.top,
            width: self.raw_width.saturating_sub(self.left).saturating_sub(self.right_trim),
            height: self.raw_height.saturating_sub(self.top).saturating_sub(self.bottom_trim),
        }
    }
}

/// The optical-black column bands `[x0, x1)` of an (already even-rounded) active area `g` in a buffer
/// `raw_width` wide, as `[left band, right band]`; a band that is empty or inverted has `x1 <= x0`.
pub fn masked_columns(g: &Geometry, raw_width: u32, band: &BandParams) -> [(u32, u32); 2] {
    let left = (band.left_start, g.left.saturating_sub(band.left_guard));
    let right =
        (g.left.saturating_add(g.width).saturating_add(band.right_gap), raw_width.saturating_sub(band.right_trim));
    [left, right]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_57_unique_sizes() {
        assert_eq!(LAYOUTS.len(), 57);
        for (i, a) in LAYOUTS.iter().enumerate() {
            assert!(LAYOUTS[i + 1..].iter().all(|b| (a.raw_width, a.raw_height) != (b.raw_width, b.raw_height)));
        }
    }

    #[test]
    fn five_d_mark_iii_row() {
        let l = find(5920, 3950).unwrap();
        let g = l.geometry();
        assert_eq!((g.left, g.top, g.width, g.height), (122, 80, 5796, 3870));
        let [lb, rb] = masked_columns(&g, 5920, &l.band);
        assert_eq!(lb, (2, 120));
        assert!(rb.1 <= rb.0);
        assert!(find(5921, 3950).is_none());
    }

    #[test]
    fn right_band_of_a_g1_sized_buffer() {
        let l = find(2144, 1560).unwrap();
        let g = l.geometry();
        assert_eq!((g.width, g.height), (2088, 1550));
        let [_, rb] = masked_columns(&g, 2144, &l.band);
        assert_eq!(rb, (2094, 2119));
    }

    #[test]
    fn forced_phase_rows() {
        let n = LAYOUTS.iter().filter(|l| l.forced_cfa == Some("GBRG")).count();
        assert_eq!(n, 4);
    }
}
