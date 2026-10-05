//! Conversions between FlashRaw's descriptions and the LibRaw data model: CFA bit masks,
//! black-level arrays, orientation codes, colour matrices and the output transfer curve.

use flashraw::{BlackLevel, CfaColor, CfaPattern, ColorMatrices, Illuminant, Orientation};

/// EXIF orientation -> LibRaw `flip` (0 none, 3 = 180 deg, 5 = 90 deg CCW, 6 = 90 deg CW;
/// bit 0 mirrors columns, bit 1 mirrors rows, bit 2 transposes).
pub fn flip_from_orientation(o: Orientation) -> i32 {
    match o.exif() {
        2 => 1,
        3 => 3,
        4 => 2,
        5 => 4,
        6 => 6,
        7 => 7,
        8 => 5,
        _ => 0,
    }
}

/// How the sensor's colours map onto LibRaw colour indices.
#[derive(Debug, Clone)]
pub struct CfaModel {
    /// `idata.filters`: a 2-bit index per site of an 8x2 tile, `LIBRAW_XTRANS` (9) for 6x6
    /// patterns, 0 without a usable CFA.
    pub filters: u32,
    /// `idata.colors`.
    pub colors: i32,
    /// `idata.cdesc` (colour letters by index).
    pub cdesc: String,
    /// The pattern (sensor-anchored) and the index of each colour, when there is a CFA.
    pattern: Option<(CfaPattern, Vec<(CfaColor, u8)>)>,
}

/// Colour index of `color` in `map`.
fn index_of(map: &[(CfaColor, u8)], color: CfaColor) -> u8 {
    map.iter().find(|(c, _)| *c == color).map_or(0, |(_, i)| *i)
}

impl CfaModel {
    /// Model for a sensor with `cfa` (anchored at sensor (0, 0)), `cpp` samples per pixel,
    /// whose visible area starts at (`left`, `top`).
    pub fn new(cfa: Option<CfaPattern>, cpp: u32, left: u32, top: u32) -> Self {
        let Some(p) = cfa else {
            let colors = if cpp >= 3 { cpp.min(4) as i32 } else { 1 };
            let cdesc = if colors == 3 { "RGBG" } else { "GMCY" };
            return CfaModel { filters: 0, colors, cdesc: cdesc.into(), pattern: None };
        };
        let distinct = p.distinct_colors();
        use CfaColor::*;
        let rgb = distinct.iter().all(|c| matches!(c, Red | Green | Blue));
        let (cdesc, map): (String, Vec<(CfaColor, u8)>) = if rgb {
            ("RGBG".into(), vec![(Red, 0), (Green, 1), (Blue, 2)])
        } else if distinct.iter().all(|c| matches!(c, Red | Green | Blue | Emerald)) {
            ("RGBE".into(), vec![(Red, 0), (Green, 1), (Blue, 2), (Emerald, 3)])
        } else if distinct.iter().all(|c| matches!(c, Cyan | Magenta | Yellow | Green)) {
            ("GMCY".into(), vec![(Green, 0), (Magenta, 1), (Cyan, 2), (Yellow, 3)])
        } else {
            let letters: String = distinct.iter().take(4).map(|c| c.letter()).collect();
            (letters, distinct.iter().take(4).enumerate().map(|(i, c)| (*c, i as u8)).collect())
        };
        let colors = if rgb { 3 } else { map.len().clamp(1, 4) as i32 };
        let (w, h) = (p.width(), p.height());
        let filters = if w == 6 && h == 6 {
            flashraw_libraw_xtrans()
        } else if w <= 2 && 8 % h == 0 {
            let mut f = 0u32;
            for row in 0..8usize {
                for col in 0..2usize {
                    let c = p.color_at(col + left as usize, row + top as usize);
                    f |= (index_of(&map, c) as u32) << ((((row << 1) & 14) | (col & 1)) << 1);
                }
            }
            if colors == 3 {
                // Number the second green of each 2x2 cell 3 (cdesc "RGBG").
                f |= ((f >> 2 & 0x2222_2222) | (f << 2 & 0x8888_8888)) & f << 1;
            }
            f
        } else {
            0
        };
        CfaModel { filters, colors, cdesc, pattern: Some((p, map)) }
    }

    /// LibRaw colour index at visible-area coordinates (as `libraw_COLOR`).
    pub fn color(&self, row: i64, col: i64, left: u32, top: u32) -> i32 {
        match (&self.pattern, self.filters) {
            (_, 0) | (None, _) => 6,
            (Some((p, map)), 9) => {
                let c =
                    p.color_at((col + left as i64).rem_euclid(6) as usize, (row + top as i64).rem_euclid(6) as usize);
                index_of(map, c) as i32
            }
            (Some(_), f) => {
                let (r, c) = (row.rem_euclid(8) as u32, col.rem_euclid(2) as u32);
                (f >> ((((r << 1) & 14) | (c & 1)) << 1) & 3) as i32
            }
        }
    }

    /// 6x6 X-Trans maps relative to the visible area and to the sensor origin.
    pub fn xtrans(&self, left: u32, top: u32) -> ([[i8; 6]; 6], [[i8; 6]; 6]) {
        let mut rel = [[0i8; 6]; 6];
        let mut abs = [[0i8; 6]; 6];
        if let (Some((p, map)), 9) = (&self.pattern, self.filters) {
            for r in 0..6 {
                for c in 0..6 {
                    rel[r][c] = index_of(map, p.color_at(c + left as usize, r + top as usize)) as i8;
                    abs[r][c] = index_of(map, p.color_at(c, r)) as i8;
                }
            }
        }
        (rel, abs)
    }

    /// Pattern dimensions (1x1 without a CFA).
    pub fn dims(&self) -> (usize, usize) {
        self.pattern.as_ref().map_or((1, 1), |(p, _)| (p.width(), p.height()))
    }
}

const fn flashraw_libraw_xtrans() -> u32 {
    crate::types::LIBRAW_XTRANS
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn lcm(a: usize, b: usize) -> usize {
    a / gcd(a, b) * b
}

/// LibRaw black representation: `black` plus `cblack[0..4]` per colour index, and an
/// optional `cblack[4] x cblack[5]` pattern in `cblack[6..]` (visible-area anchored), with
/// the common minimum moved into `black`. Fractional levels are truncated.
pub fn black_to_libraw(b: &BlackLevel, cfa: &CfaModel, cpp: u32, left: u32, top: u32) -> (u32, Vec<u32>) {
    let mut cblack = vec![0u32; crate::types::LIBRAW_CBLACK_SIZE];
    let round = |v: f32| v.max(0.0).floor() as u32;
    let samples = b.samples.max(1) as usize;
    if cpp > 1 || cfa.filters == 0 {
        if samples > 1 {
            let vals: Vec<u32> = (0..samples.min(4)).map(|c| round(b.at(left as usize, top as usize, c))).collect();
            let base = vals.iter().copied().min().unwrap_or(0);
            for (i, v) in vals.iter().enumerate() {
                cblack[i] = v - base;
            }
            return (base, cblack);
        }
        let (rows, cols) = (b.rows.max(1) as usize, b.cols.max(1) as usize);
        if rows * cols <= 1 || b.is_uniform() {
            return (round(b.values.first().copied().unwrap_or(0.0)), cblack);
        }
        return pattern_black(b, rows, cols, left, top, cblack);
    }
    // CFA: per colour index when each colour has one black, else a pattern.
    let (cw, ch) = cfa.dims();
    let pr = lcm(b.rows.max(1) as usize, ch.max(1));
    let pc = lcm(b.cols.max(1) as usize, cw.max(1));
    let mut per_index: [Option<u32>; 4] = [None; 4];
    let mut consistent = pr <= 64 && pc <= 64;
    if consistent {
        'outer: for r in 0..pr {
            for c in 0..pc {
                let v = round(b.at(c + left as usize, r + top as usize, 0));
                let i = cfa.color(r as i64, c as i64, left, top);
                let Some(slot) = per_index.get_mut(i as usize) else {
                    consistent = false;
                    break 'outer;
                };
                match slot {
                    Some(x) if *x != v => {
                        consistent = false;
                        break 'outer;
                    }
                    _ => *slot = Some(v),
                }
            }
        }
    }
    if consistent {
        let base = per_index.iter().flatten().copied().min().unwrap_or(0);
        for (i, v) in per_index.iter().enumerate() {
            cblack[i] = v.map_or(0, |v| v - base);
        }
        return (base, cblack);
    }
    pattern_black(b, b.rows.max(1) as usize, b.cols.max(1) as usize, left, top, cblack)
}

fn pattern_black(
    b: &BlackLevel,
    rows: usize,
    cols: usize,
    left: u32,
    top: u32,
    mut cblack: Vec<u32>,
) -> (u32, Vec<u32>) {
    let round = |v: f32| v.max(0.0).floor() as u32;
    if rows * cols + 6 > cblack.len() {
        return (round(b.mean()), cblack);
    }
    let vals: Vec<u32> =
        (0..rows * cols).map(|i| round(b.at(i % cols + left as usize, i / cols + top as usize, 0))).collect();
    let base = vals.iter().copied().min().unwrap_or(0);
    cblack[4] = rows as u32;
    cblack[5] = cols as u32;
    for (i, v) in vals.iter().enumerate() {
        cblack[6 + i] = v - base;
    }
    (base, cblack)
}

/// The black level LibRaw fields describe, as a FlashRaw pattern anchored at sensor (0, 0).
pub fn black_from_libraw(black: u32, cblack: &[u32], cfa: &CfaModel, cpp: u32, left: u32, top: u32) -> BlackLevel {
    let (pr, pc) = (cblack[4] as usize, cblack[5] as usize);
    let pat = pr > 0 && pc > 0 && pr * pc + 6 <= cblack.len();
    if cpp > 1 {
        let vals: Vec<f32> = (0..cpp as usize).map(|c| (black + cblack[c.min(3)]) as f32).collect();
        return BlackLevel {
            rows: 1,
            cols: 1,
            samples: cpp as u8,
            values: vals,
            row_deltas: vec![],
            col_deltas: vec![],
        };
    }
    let (cw, ch) = cfa.dims();
    let rows = if pat { lcm(pr, ch) } else { ch };
    let cols = if pat { lcm(pc, cw) } else { cw };
    if rows > 64 || cols > 64 {
        return BlackLevel::uniform(black as f32);
    }
    let mut values = Vec::with_capacity(rows * cols);
    for y in 0..rows {
        for x in 0..cols {
            // Sensor (x, y) -> visible coordinates.
            let (vr, vc) = (y as i64 - top as i64, x as i64 - left as i64);
            let mut v = black;
            if cfa.filters != 0 {
                v += cblack[cfa.color(vr, vc, left, top).clamp(0, 3) as usize];
            } else {
                v += cblack[0];
            }
            if pat {
                v += cblack[6 + vr.rem_euclid(pr as i64) as usize * pc + vc.rem_euclid(pc as i64) as usize];
            }
            values.push(v as f32);
        }
    }
    BlackLevel::pattern(rows, cols, values)
}

/// sRGB (D65) -> XYZ.
const XYZ_RGB: [[f64; 3]; 3] =
    [[0.412_453, 0.357_580, 0.180_423], [0.212_671, 0.715_160, 0.072_169], [0.019_334, 0.119_193, 0.950_227]];

/// The XYZ -> camera matrix LibRaw calls `cam_xyz` (D65 calibration preferred), rows per
/// camera colour.
pub fn cam_xyz(m: &ColorMatrices) -> Option<Vec<[f64; 3]>> {
    let x = m.color.iter().find(|c| c.illuminant == Illuminant::D65).or_else(|| m.color.last())?;
    let rows = x.rows as usize;
    if rows < 3 || x.values.len() < rows * 3 {
        return None;
    }
    Some(
        (0..rows.min(4))
            .map(|r| [x.values[r * 3] as f64, x.values[r * 3 + 1] as f64, x.values[r * 3 + 2] as f64])
            .collect(),
    )
}

fn inv3(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let [[a, b, c], [d, e, f], [g, h, i]] = m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    let k = 1.0 / det;
    Some([
        [(e * i - f * h) * k, (c * h - b * i) * k, (b * f - c * e) * k],
        [(f * g - d * i) * k, (a * i - c * g) * k, (c * d - a * f) * k],
        [(d * h - e * g) * k, (b * g - a * h) * k, (a * e - b * d) * k],
    ])
}

/// Camera -> sRGB (`rgb_cam`, 3 x colours) and daylight multipliers (`pre_mul`) from an
/// XYZ -> camera matrix: camera-from-sRGB rows are normalised to sum to one (their sums
/// give the daylight multipliers), and the result is pseudo-inverted.
pub fn rgb_cam_and_pre_mul(cam_xyz: &[[f64; 3]]) -> Option<([[f32; 4]; 3], [f32; 4])> {
    let n = cam_xyz.len();
    let mut cam_rgb = vec![[0f64; 3]; n];
    let mut pre_mul = [0f32; 4];
    for i in 0..n {
        for j in 0..3 {
            cam_rgb[i][j] = (0..3).map(|k| cam_xyz[i][k] * XYZ_RGB[k][j]).sum();
        }
        let s: f64 = cam_rgb[i].iter().sum();
        if s.abs() < 1e-9 || !s.is_finite() {
            return None;
        }
        cam_rgb[i].iter_mut().for_each(|v| *v /= s);
        pre_mul[i] = (1.0 / s) as f32;
    }
    // Pseudo-inverse (A^T A)^-1 A^T: 3 x n.
    let mut ata = [[0f64; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            ata[r][c] = (0..n).map(|k| cam_rgb[k][r] * cam_rgb[k][c]).sum();
        }
    }
    let inv = inv3(ata)?;
    let mut rgb_cam = [[0f32; 4]; 3];
    for r in 0..3 {
        for c in 0..n {
            rgb_cam[r][c] = (0..3).map(|k| inv[r][k] * cam_rgb[c][k]).sum::<f64>() as f32;
        }
    }
    Some((rgb_cam, pre_mul))
}

/// Linear sRGB -> output colour space for `params.output_color` (None: unsupported).
pub fn output_matrix(output_color: i32) -> Option<[[f32; 3]; 3]> {
    const ID: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let xyz = XYZ_RGB.map(|r| r.map(|v| v as f32));
    let from_xyz = |m: [[f32; 3]; 3]| -> [[f32; 3]; 3] {
        let mut r = [[0f32; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                r[i][j] = (0..3).map(|k| m[i][k] * xyz[k][j]).sum();
            }
        }
        r
    };
    match output_color {
        0 | 1 => Some(ID),
        // Adobe RGB (1998), D65.
        2 => Some(from_xyz([
            [2.041_369, -0.564_946_4, -0.344_694_4],
            [-0.969_266, 1.876_010_8, 0.041_556],
            [0.013_447_4, -0.118_389_7, 1.015_409_8],
        ])),
        // CIE XYZ (D65).
        5 => Some(xyz),
        // ITU-R BT.2020, D65.
        8 => Some(from_xyz([
            [1.716_651_2, -0.355_670_8, -0.253_366_3],
            [-0.666_684_4, 1.616_481_2, 0.015_768_5],
            [0.017_639_9, -0.042_770_6, 0.942_103_1],
        ])),
        _ => None,
    }
}

/// A power-law transfer curve with a linear toe, continuous in value and slope:
/// `y = ts * x` below the breakpoint, `(1 + a) x^p - a` above (BT.709 for p = 0.45,
/// ts = 4.5). Returns `(breakpoint, a)`; `(0, 0)` means a pure power curve.
pub fn solve_toe(p: f64, ts: f64) -> (f64, f64) {
    if !(p > 0.0 && p < 1.0 && ts > 1.0) {
        return (0.0, 0.0);
    }
    // f(x0) = 1 + ts x0 (1/p - 1) - ts x0^(1-p) / p; root in (0, 1).
    let f = |x: f64| 1.0 + ts * x * (1.0 / p - 1.0) - ts * x.powf(1.0 - p) / p;
    let (mut lo, mut hi) = (1e-12f64, 1.0f64);
    if f(lo).signum() == f(hi).signum() {
        return (0.0, 0.0);
    }
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        if f(mid).signum() == f(lo).signum() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let x0 = 0.5 * (lo + hi);
    (x0, ts * x0 * (1.0 / p - 1.0))
}

/// `params.gamm[2..6]` derived from `gamm[0..2]`.
pub fn gamma_params(p: f64, ts: f64) -> [f64; 4] {
    let (x0, a) = solve_toe(p, ts);
    [ts * x0, x0, a, 0.0]
}

/// 16-bit output curve for linear 16-bit input: values at or above `imax` saturate.
pub fn gamma_curve(p: f64, ts: f64, imax: f64) -> Vec<u16> {
    let (x0, a) = solve_toe(p, ts);
    let linear = (p - 1.0).abs() < 1e-9 || p <= 0.0;
    (0..0x10000u32)
        .map(|i| {
            let r = i as f64 / imax.max(1.0);
            if r >= 1.0 {
                return 0xffff;
            }
            let y = if linear {
                r
            } else if r < x0 {
                r * ts
            } else {
                r.powf(p) * (1.0 + a) - a
            };
            (y * 65536.0).clamp(0.0, 65535.0) as u16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bt709_toe() {
        let (x0, a) = solve_toe(0.45, 4.5);
        assert!((x0 - 0.018).abs() < 1e-3, "{x0}");
        assert!((a - 0.099).abs() < 1e-3, "{a}");
        let c = gamma_curve(0.45, 4.5, 65536.0);
        assert_eq!(c[0], 0);
        assert!(c[65535] > 65000);
        assert!(c.windows(2).all(|w| w[0] <= w[1]));
        let lin = gamma_curve(1.0, 1.0, 65536.0);
        assert_eq!(lin[32768], 32768);
    }

    #[test]
    fn rggb_filters_mark_second_green() {
        let p = CfaPattern::parse("RGGB").unwrap();
        let m = CfaModel::new(Some(p), 1, 0, 0);
        assert_eq!(m.filters, 0xB4B4_B4B4);
        assert_eq!((m.color(0, 0, 0, 0), m.color(0, 1, 0, 0), m.color(1, 0, 0, 0), m.color(1, 1, 0, 0)), (0, 1, 3, 2));
        // An odd visible-area offset shifts the pattern.
        let m = CfaModel::new(Some(p), 1, 1, 0);
        assert_eq!(m.color(0, 0, 1, 0), 1);
    }

    #[test]
    fn black_round_trip() {
        let p = CfaPattern::parse("RGGB").unwrap();
        let m = CfaModel::new(Some(p), 1, 0, 0);
        let b = BlackLevel::pattern2x2([512.0, 514.0, 514.0, 516.0]);
        let (black, cb) = black_to_libraw(&b, &m, 1, 0, 0);
        assert_eq!((black, &cb[..4]), (512, &[0u32, 2, 4, 2][..]));
        let back = black_from_libraw(black, &cb, &m, 1, 0, 0);
        assert_eq!(back.values, vec![512.0, 514.0, 514.0, 516.0]);
        let (black, cb) = black_to_libraw(&BlackLevel::uniform(64.0), &m, 1, 0, 0);
        assert_eq!((black, cb[0], cb[4]), (64, 0, 0));
    }

    #[test]
    fn pre_mul_identity_matrix() {
        // cam_xyz = sRGB->XYZ inverse gives rgb_cam = identity and equal multipliers.
        let inv = inv3(XYZ_RGB).unwrap();
        let (rgb_cam, pre) = rgb_cam_and_pre_mul(&inv).unwrap();
        for i in 0..3 {
            assert!((rgb_cam[i][i] - 1.0).abs() < 1e-4);
            assert!((pre[i] - 1.0).abs() < 1e-4);
        }
    }
}
