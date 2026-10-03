//! Canon sensor geometry knowledge keyed by the raw buffer size.
//!
//! Canon CR2/CRW files do not reliably describe their optical-black borders, so
//! FlashRaw keeps a table of sensor layouts indexed by the raw buffer dimensions. It
//! gives the active area and the masked areas used for black-level measurement.

/// One sensor layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorLayout {
    pub raw_width: u32,
    pub raw_height: u32,
    pub left: u32,
    pub top: u32,
    /// Columns/rows to drop at the right/bottom after the margins.
    pub right_trim: u32,
    pub bottom_trim: u32,
    /// Extra insets of the left/right masked column bands (see [`SensorLayout::masked_columns`]).
    pub mask_left_start: u32,
    pub mask_left_end_inset: u32,
    pub mask_right_start: u32,
    pub mask_right_end_inset: u32,
    /// CFA override as a 2x2 descriptor byte, two bits per position (0 = default RGGB).
    pub filters: u8,
}

#[allow(clippy::too_many_arguments)]
const fn l(
    raw_width: u32,
    raw_height: u32,
    left: u32,
    top: u32,
    right_trim: u32,
    bottom_trim: u32,
    masks: [u32; 4],
    filters: u8,
) -> SensorLayout {
    SensorLayout {
        raw_width,
        raw_height,
        left,
        top,
        right_trim,
        bottom_trim,
        mask_left_start: masks[0],
        mask_left_end_inset: masks[1],
        mask_right_start: masks[2],
        mask_right_end_inset: masks[3],
        filters,
    }
}

const Z: [u32; 4] = [0; 4];

/// Known layouts (raw width x height → margins), shared by CRW, CR2 and CR3 bodies.
pub static LAYOUTS: &[SensorLayout] = &[
    l(1944, 1416, 0, 0, 48, 0, Z, 0),                   // PowerShot Pro90 IS
    l(2144, 1560, 4, 8, 52, 2, [0, 0, 0, 25], 0),       // PowerShot S30, G1
    l(2224, 1456, 48, 6, 0, 2, Z, 0),                   // EOS D30
    l(2376, 1728, 12, 6, 52, 2, Z, 0),                  // PowerShot G2, S40, G3, S45
    l(2672, 1968, 12, 6, 44, 2, Z, 0),                  // PowerShot G5, S50, S60
    l(3152, 2068, 64, 12, 0, 0, [16, 0, 0, 0], 0),      // EOS D60, 10D, 300D
    l(3160, 2344, 44, 12, 4, 4, Z, 0),                  // PowerShot G6, S70
    l(3344, 2484, 4, 6, 52, 6, Z, 0),                   // PowerShot Pro1
    l(3516, 2328, 42, 14, 0, 0, Z, 0),                  // EOS 350D
    l(3596, 2360, 74, 12, 0, 0, Z, 0),                  // EOS-1D Mark II, 20D, 1D Mark II N, 30D
    l(3744, 2784, 52, 12, 8, 12, Z, 0),                 // PowerShot G11, S90, G12, S95
    l(3944, 2622, 30, 18, 6, 2, Z, 0),                  // EOS 40D
    l(3948, 2622, 42, 18, 0, 2, Z, 0),                  // EOS 400D, 1000D
    l(3984, 2622, 76, 20, 0, 2, [14, 0, 0, 0], 0),      // EOS-1D Mark III
    l(4032, 2656, 112, 44, 10, 0, Z, 0),                // APS-C crop modes (6D Mark II, RP)
    l(4104, 3048, 48, 12, 24, 12, Z, 0),                // PowerShot G9
    l(4116, 2178, 4, 2, 0, 0, Z, 0),                    // ?
    l(4152, 2772, 192, 12, 0, 0, Z, 0),                 // PowerShot SX1 IS
    l(4160, 3124, 104, 11, 8, 65, Z, 0),                // PowerShot S100, S100V, G15, S110
    l(4176, 3062, 96, 17, 8, 0, [0, 16, 0, 7], 0x49),   // PowerShot SX50 HS
    l(4192, 3062, 96, 17, 24, 0, [0, 16, 0, 0], 0x49),  // PowerShot G16, S120
    l(4312, 2876, 22, 18, 0, 2, Z, 0),                  // EOS 450D
    l(4352, 2850, 144, 46, 0, 0, Z, 0),                 // APS-C crop mode (EOS R)
    l(4352, 2874, 62, 18, 0, 0, Z, 0),                  // EOS 1100D
    l(4476, 2954, 90, 34, 0, 0, Z, 0),                  // EOS 5D
    l(4480, 3348, 12, 10, 36, 12, [0, 0, 0, 18], 0x49), // PowerShot G10
    l(4480, 3366, 80, 50, 0, 0, Z, 0),                  // PowerShot G1 X Mark II
    l(4496, 3366, 80, 50, 12, 0, Z, 0),                 // PowerShot G1 X
    l(4768, 3516, 96, 16, 0, 0, [0, 16, 0, 0], 0),      // PowerShot SX60 HS
    l(4832, 3204, 62, 26, 0, 0, Z, 0),                  // EOS 500D
    l(4832, 3228, 62, 51, 0, 0, Z, 0),                  // EOS 50D
    l(5108, 3349, 98, 13, 0, 0, Z, 0),                  // EOS-1Ds Mark II
    l(5120, 3318, 142, 45, 62, 0, Z, 0),                // EOS-1D Mark IV
    l(5280, 3528, 72, 52, 0, 0, Z, 0),                  // EOS M, M2, M10, 650D, 700D, 100D
    l(5344, 3516, 142, 51, 0, 0, Z, 0),                 // EOS 550D, 600D, 60D, 1200D, 1300D, 3000D
    l(5344, 3584, 126, 100, 0, 2, Z, 0),                // EOS-1D X, 1D C
    l(5344, 3950, 98, 18, 0, 0, [0, 24, 0, 0], 0),      // PowerShot SX70 HS
    l(5360, 3516, 158, 51, 0, 0, Z, 0),                 // EOS 7D
    l(5568, 3708, 72, 38, 0, 0, Z, 0),                  // EOS 7D Mark II, 6D, 70D, 1D X Mark II
    l(5632, 3710, 96, 17, 0, 0, [0, 16, 0, 0], 0x49),   // PowerShot G7 X, G3 X, G9 X, G5 X (+ Mark II)
    l(5712, 3774, 62, 20, 10, 2, Z, 0),                 // EOS-1Ds Mark III
    l(5792, 3804, 158, 51, 0, 0, Z, 0),                 // EOS 5D Mark II
    l(5920, 3950, 122, 80, 2, 0, Z, 0),                 // EOS 5D Mark III
    l(6096, 4051, 76, 35, 0, 0, Z, 0),                  // EOS 1500D
    l(6096, 4056, 72, 34, 0, 0, Z, 0),                  // EOS M3, 760D, 750D
    l(6288, 4056, 264, 36, 0, 0, Z, 0),                 // EOS M5, M100, M6, 80D, 800D, 77D, 200D, G1 X Mark III...
    l(6384, 4224, 120, 44, 0, 0, Z, 0),                 // EOS 6D Mark II, RP
    l(6880, 4544, 136, 42, 0, 0, Z, 0),                 // EOS 5D Mark IV
    l(6888, 4546, 146, 48, 0, 0, Z, 0),                 // EOS R
    l(7128, 4732, 144, 72, 0, 0, Z, 0),                 // EOS M6 Mark II, 90D
    l(8896, 5920, 160, 64, 0, 0, Z, 0),                 // EOS 5DS, 5DS R
    l(6192, 4152, 160, 120, 0, 0, Z, 0),                // EOS R3
    l(6192, 4060, 168, 52, 24, 8, [16, 48, 32, 0], 0),  // EOS R10
    l(6188, 4120, 154, 96, 12, 0, [16, 48, 32, 0], 0),  // EOS R6 Mark II
    l(8480, 5650, 258, 162, 0, 0, [128, 2, 128, 0], 0), // EOS R5 Mark II (full frame)
    l(5376, 3574, 258, 162, 0, 0, [128, 2, 128, 0], 0), // EOS R5 Mark II (APS-C)
    l(6160, 4144, 128, 120, 0, 0, Z, 0),                // EOS R1
];

/// Look up a layout by raw dimensions. When several entries share the dimensions the
/// last one wins.
pub fn find(raw_width: u32, raw_height: u32) -> Option<&'static SensorLayout> {
    LAYOUTS.iter().rev().find(|l| l.raw_width == raw_width && l.raw_height == raw_height)
}

/// Visible geometry `(left, top, width, height)` plus the masked column bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

impl SensorLayout {
    pub fn geometry(&self) -> Geometry {
        Geometry {
            left: self.left,
            top: self.top,
            width: self.raw_width.saturating_sub(self.left).saturating_sub(self.right_trim),
            height: self.raw_height.saturating_sub(self.top).saturating_sub(self.bottom_trim),
        }
    }
}

/// Optical-black column bands `[x0, x1)` (rows `top..top+height`) the black level is
/// measured from for Canon files: the left border minus a 2-pixel
/// guard band on each side, and the right border after a 2-pixel guard band.
///
/// `insets` are the per-layout adjustments (`mask_left_start`, `mask_left_end_inset`,
/// `mask_right_start`, `mask_right_end_inset`). Note the left band's start uses the right
/// band's start offset (`insets[2]`), not `insets[0]`: the table values are tuned for that.
pub fn masked_columns(g: &Geometry, raw_width: u32, insets: [u32; 4]) -> [(u32, u32); 2] {
    let left_start = insets[2] + 2;
    let left_end = g.left.saturating_sub(insets[1] + 2);
    let right_start = insets[2] + 2 + g.left + g.width;
    let right_end = raw_width.saturating_sub(insets[3]);
    [(left_start, left_end), (right_start, right_end)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_masks() {
        let l = find(5920, 3950).unwrap();
        let g = l.geometry();
        assert_eq!((g.left, g.top, g.width, g.height), (122, 80, 5796, 3870));
        let m = masked_columns(
            &g,
            5920,
            [l.mask_left_start, l.mask_left_end_inset, l.mask_right_start, l.mask_right_end_inset],
        );
        assert_eq!(m, [(2, 120), (5920, 5920)]);
        assert!(find(1, 1).is_none());
    }
}
