//! Sensor-domain value types: geometry, CFA layouts, levels, colour calibration.
//!
//! Coordinate convention (used everywhere in FlashRaw): all rectangles, CFA
//! patterns and black-level patterns are expressed in the coordinate system of the
//! *full decoded sensor buffer* — i.e. including masked/optical-black borders.
//! `(0, 0)` is the first stored sample.

use std::fmt;

/// An axis-aligned rectangle in sensor-buffer coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Rect { x, y, width, height }
    }
    /// Rect from left/top/right/bottom edges (exclusive right/bottom). Returns `None` if inverted.
    pub fn from_ltrb(left: u32, top: u32, right: u32, bottom: u32) -> Option<Self> {
        (right >= left && bottom >= top).then(|| Rect::new(left, top, right - left, bottom - top))
    }
    pub const fn full(width: u32, height: u32) -> Self {
        Rect { x: 0, y: 0, width, height }
    }
    #[inline]
    pub fn right(&self) -> u32 {
        self.x.saturating_add(self.width)
    }
    #[inline]
    pub fn bottom(&self) -> u32 {
        self.y.saturating_add(self.height)
    }
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
    pub fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let l = self.x.max(o.x);
        let t = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        (r > l && b > t).then(|| Rect::new(l, t, r - l, b - t))
    }
    pub fn contains_rect(&self, o: &Rect) -> bool {
        o.x >= self.x && o.y >= self.y && o.right() <= self.right() && o.bottom() <= self.bottom()
    }
    /// Clamp to a `width x height` buffer, returning an empty rect if fully outside.
    pub fn clamp_to(&self, width: u32, height: u32) -> Rect {
        self.intersect(&Rect::full(width, height)).unwrap_or_default()
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}+{}+{}", self.width, self.height, self.x, self.y)
    }
}

/// Colour of a single CFA site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum CfaColor {
    #[default]
    Red = 0,
    Green = 1,
    Blue = 2,
    Cyan = 3,
    Magenta = 4,
    Yellow = 5,
    White = 6,
    Emerald = 7,
}

impl CfaColor {
    /// DNG `CFAPattern` colour index (0=R 1=G 2=B 3=C 4=M 5=Y 6=W).
    pub fn from_dng(v: u8) -> Option<Self> {
        Some(match v {
            0 => CfaColor::Red,
            1 => CfaColor::Green,
            2 => CfaColor::Blue,
            3 => CfaColor::Cyan,
            4 => CfaColor::Magenta,
            5 => CfaColor::Yellow,
            6 => CfaColor::White,
            _ => return None,
        })
    }
    pub fn letter(self) -> char {
        match self {
            CfaColor::Red => 'R',
            CfaColor::Green => 'G',
            CfaColor::Blue => 'B',
            CfaColor::Cyan => 'C',
            CfaColor::Magenta => 'M',
            CfaColor::Yellow => 'Y',
            CfaColor::White => 'W',
            CfaColor::Emerald => 'E',
        }
    }
    pub fn from_letter(c: char) -> Option<Self> {
        Some(match c.to_ascii_uppercase() {
            'R' => CfaColor::Red,
            'G' => CfaColor::Green,
            'B' => CfaColor::Blue,
            'C' => CfaColor::Cyan,
            'M' => CfaColor::Magenta,
            'Y' => CfaColor::Yellow,
            'W' => CfaColor::White,
            'E' => CfaColor::Emerald,
            _ => return None,
        })
    }
}

/// Maximum CFA repeat size (X-Trans is 6x6; some exotic sensors use 8x8).
pub const MAX_CFA: usize = 8;

/// A repeating colour filter array pattern anchored at sensor-buffer `(0, 0)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CfaPattern {
    width: u8,
    height: u8,
    colors: [[CfaColor; MAX_CFA]; MAX_CFA],
}

impl CfaPattern {
    /// Build from row-major colours. Returns `None` for invalid dimensions.
    pub fn new(width: usize, height: usize, colors: &[CfaColor]) -> Option<Self> {
        if width == 0 || height == 0 || width > MAX_CFA || height > MAX_CFA || colors.len() != width * height {
            return None;
        }
        let mut c = [[CfaColor::Red; MAX_CFA]; MAX_CFA];
        for y in 0..height {
            for x in 0..width {
                c[y][x] = colors[y * width + x];
            }
        }
        Some(CfaPattern { width: width as u8, height: height as u8, colors: c })
    }

    /// Parse a pattern string such as `"RGGB"` (2x2) or a 36-letter X-Trans string (6x6).
    /// Square patterns are inferred from length; use [`CfaPattern::parse_dims`] otherwise.
    pub fn parse(s: &str) -> Option<Self> {
        let n = s.chars().filter(|c| !c.is_whitespace() && *c != '/').count();
        let side = (n as f64).sqrt() as usize;
        if side * side != n {
            return None;
        }
        Self::parse_dims(s, side, side)
    }

    pub fn parse_dims(s: &str, width: usize, height: usize) -> Option<Self> {
        let colors: Option<Vec<CfaColor>> =
            s.chars().filter(|c| !c.is_whitespace() && *c != '/').map(CfaColor::from_letter).collect();
        Self::new(width, height, &colors?)
    }

    pub const RGGB: CfaPattern = Self::bayer2(CfaColor::Red, CfaColor::Green, CfaColor::Green, CfaColor::Blue);
    pub const BGGR: CfaPattern = Self::bayer2(CfaColor::Blue, CfaColor::Green, CfaColor::Green, CfaColor::Red);
    pub const GRBG: CfaPattern = Self::bayer2(CfaColor::Green, CfaColor::Red, CfaColor::Blue, CfaColor::Green);
    pub const GBRG: CfaPattern = Self::bayer2(CfaColor::Green, CfaColor::Blue, CfaColor::Red, CfaColor::Green);

    const fn bayer2(a: CfaColor, b: CfaColor, c: CfaColor, d: CfaColor) -> Self {
        let mut colors = [[CfaColor::Red; MAX_CFA]; MAX_CFA];
        colors[0][0] = a;
        colors[0][1] = b;
        colors[1][0] = c;
        colors[1][1] = d;
        CfaPattern { width: 2, height: 2, colors }
    }

    #[inline]
    pub fn width(&self) -> usize {
        self.width as usize
    }
    #[inline]
    pub fn height(&self) -> usize {
        self.height as usize
    }
    /// Colour at sensor-buffer position `(x, y)`.
    #[inline(always)]
    pub fn color_at(&self, x: usize, y: usize) -> CfaColor {
        self.colors[y % self.height as usize][x % self.width as usize]
    }
    /// The pattern as seen from a new origin at `(dx, dy)` (e.g. the active-area origin).
    pub fn shifted(&self, dx: usize, dy: usize) -> CfaPattern {
        let mut c = [[CfaColor::Red; MAX_CFA]; MAX_CFA];
        for y in 0..self.height() {
            for x in 0..self.width() {
                c[y][x] = self.color_at(x + dx, y + dy);
            }
        }
        CfaPattern { width: self.width, height: self.height, colors: c }
    }
    /// Row-major colour list.
    pub fn colors(&self) -> Vec<CfaColor> {
        (0..self.height())
            .flat_map(|y| (0..self.width()).map(move |x| (x, y)))
            .map(|(x, y)| self.colors[y][x])
            .collect()
    }
    pub fn is_bayer(&self) -> bool {
        if self.width != 2 || self.height != 2 {
            return false;
        }
        let mut cnt = [0; 8];
        for c in self.colors() {
            cnt[c as usize] += 1;
        }
        cnt[0] == 1 && cnt[1] == 2 && cnt[2] == 1
    }
    pub fn is_xtrans(&self) -> bool {
        self.width == 6 && self.height == 6
    }
    /// Distinct colours present, in `CfaColor` order.
    pub fn distinct_colors(&self) -> Vec<CfaColor> {
        let mut seen = [false; 8];
        for c in self.colors() {
            seen[c as usize] = true;
        }
        (0..8u8).filter(|&i| seen[i as usize]).map(CfaColor::from_index).collect()
    }
}

impl CfaColor {
    /// Inverse of `self as u8`; out-of-range values map to `Emerald`.
    pub fn from_index(i: u8) -> CfaColor {
        match i {
            0 => CfaColor::Red,
            1 => CfaColor::Green,
            2 => CfaColor::Blue,
            3 => CfaColor::Cyan,
            4 => CfaColor::Magenta,
            5 => CfaColor::Yellow,
            6 => CfaColor::White,
            _ => CfaColor::Emerald,
        }
    }
}

impl fmt::Display for CfaPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for y in 0..self.height() {
            if y > 0 && (self.width() > 2 || self.height() > 2) {
                f.write_str("/")?;
            }
            for x in 0..self.width() {
                write!(f, "{}", self.colors[y][x].letter())?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for CfaPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CfaPattern({self})")
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for CfaPattern {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

/// How samples are arranged on the sensor / in the decoded buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SensorLayout {
    /// 2x2 Bayer CFA, one sample per pixel.
    Bayer,
    /// Fujifilm X-Trans 6x6 CFA, one sample per pixel.
    XTrans,
    /// Quad-Bayer / Tetracell (2x2 same-colour blocks in a 4x4 repeat).
    QuadBayer,
    /// Any other repeating CFA (CYGM, RGBE, ...).
    OtherCfa,
    /// No CFA: one luminance sample per pixel.
    Monochrome,
    /// Three or more samples per pixel already in camera space (linear DNG, sRAW/mRAW after
    /// YCbCr conversion, multi-shot composites).
    Linear,
    /// Layered (Foveon X3) sensor, three samples per pixel.
    Foveon,
}

/// Black level description, anchored at sensor-buffer `(0, 0)`.
///
/// The effective black for sample `c` of pixel `(x, y)` is
/// `pattern[(y % rows) * cols + (x % cols)][c] + row_delta[y] + col_delta[x]`.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BlackLevel {
    pub rows: u8,
    pub cols: u8,
    /// Samples per pixel covered by `values` (1 for CFA data).
    pub samples: u8,
    /// `rows * cols * samples` values.
    pub values: Vec<f32>,
    /// Optional per-row additive deltas (DNG BlackLevelDeltaV), length = buffer height.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
    pub row_deltas: Vec<f32>,
    /// Optional per-column additive deltas (DNG BlackLevelDeltaH), length = buffer width.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
    pub col_deltas: Vec<f32>,
}

impl BlackLevel {
    pub fn uniform(v: f32) -> Self {
        BlackLevel { rows: 1, cols: 1, samples: 1, values: vec![v], row_deltas: vec![], col_deltas: vec![] }
    }
    /// A 2x2 pattern (row-major) aligned with a 2x2 CFA at `(0, 0)`.
    pub fn pattern2x2(v: [f32; 4]) -> Self {
        BlackLevel { rows: 2, cols: 2, samples: 1, values: v.to_vec(), row_deltas: vec![], col_deltas: vec![] }
    }
    pub fn pattern(rows: usize, cols: usize, values: Vec<f32>) -> Self {
        BlackLevel { rows: rows as u8, cols: cols as u8, samples: 1, values, row_deltas: vec![], col_deltas: vec![] }
    }
    /// Black for sample `c` at `(x, y)`.
    #[inline]
    pub fn at(&self, x: usize, y: usize, c: usize) -> f32 {
        let (rows, cols, s) = (self.rows.max(1) as usize, self.cols.max(1) as usize, self.samples.max(1) as usize);
        let idx = ((y % rows) * cols + (x % cols)) * s + (c % s);
        let mut v = self.values.get(idx).copied().unwrap_or(0.0);
        if let Some(d) = self.row_deltas.get(y) {
            v += d;
        }
        if let Some(d) = self.col_deltas.get(x) {
            v += d;
        }
        v
    }
    /// The pattern re-anchored at `(dx, dy)` (deltas are dropped).
    pub fn shifted(&self, dx: usize, dy: usize) -> Self {
        let (rows, cols, s) = (self.rows.max(1) as usize, self.cols.max(1) as usize, self.samples.max(1) as usize);
        let mut values = Vec::with_capacity(self.values.len());
        for y in 0..rows {
            for x in 0..cols {
                for c in 0..s {
                    values.push(
                        self.at(x + dx, y + dy, c)
                            - self.row_deltas.get(y + dy).copied().unwrap_or(0.0)
                            - self.col_deltas.get(x + dx).copied().unwrap_or(0.0),
                    );
                }
            }
        }
        BlackLevel {
            rows: rows as u8,
            cols: cols as u8,
            samples: s as u8,
            values,
            row_deltas: vec![],
            col_deltas: vec![],
        }
    }
    pub fn is_uniform(&self) -> bool {
        self.row_deltas.is_empty() && self.col_deltas.is_empty() && self.values.windows(2).all(|w| w[0] == w[1])
    }
    /// Mean of the repeating pattern (convenience for display).
    pub fn mean(&self) -> f32 {
        if self.values.is_empty() { 0.0 } else { self.values.iter().sum::<f32>() / self.values.len() as f32 }
    }
}

/// Saturation (white) level per sample plane / CFA colour.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WhiteLevel {
    pub values: Vec<u32>,
}

impl WhiteLevel {
    pub fn uniform(v: u32) -> Self {
        WhiteLevel { values: vec![v] }
    }
    pub fn max(&self) -> u32 {
        self.values.iter().copied().max().unwrap_or(0)
    }
    pub fn min(&self) -> u32 {
        self.values.iter().copied().min().unwrap_or(0)
    }
    pub fn at(&self, c: usize) -> u32 {
        self.values.get(c).or(self.values.first()).copied().unwrap_or(0)
    }
}

/// As-shot white-balance multipliers in `[R, G, B, G2/4th]` order, normalised so G = 1.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WhiteBalance {
    pub coeffs: [f32; 4],
}

impl WhiteBalance {
    /// Build from raw multipliers, normalising by the green coefficient. Returns `None`
    /// if the values are not usable (zero/NaN green, non-positive values).
    pub fn from_multipliers(r: f32, g: f32, b: f32, g2: f32) -> Option<Self> {
        if !(g.is_finite() && g > 0.0 && r.is_finite() && r > 0.0 && b.is_finite() && b > 0.0) {
            return None;
        }
        let g2 = if g2.is_finite() && g2 > 0.0 { g2 / g } else { 1.0 };
        Some(WhiteBalance { coeffs: [r / g, 1.0, b / g, g2] })
    }
    /// From a DNG-style "as shot neutral" (camera response to neutral), i.e. reciprocal multipliers.
    pub fn from_neutral(n: &[f32]) -> Option<Self> {
        if n.len() < 3 || n.iter().any(|&v| !(v.is_finite() && v > 0.0)) {
            return None;
        }
        Self::from_multipliers(1.0 / n[0], 1.0 / n[1], 1.0 / n[2], n.get(3).map(|v| 1.0 / v).unwrap_or(1.0 / n[1]))
    }
}

/// EXIF LightSource / DNG CalibrationIlluminant code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Illuminant(pub u16);

impl Illuminant {
    pub const UNKNOWN: Illuminant = Illuminant(0);
    pub const STANDARD_A: Illuminant = Illuminant(17);
    pub const D50: Illuminant = Illuminant(23);
    pub const D65: Illuminant = Illuminant(21);
    pub const D55: Illuminant = Illuminant(20);

    /// Approximate correlated colour temperature, when known.
    pub fn temperature(&self) -> Option<f32> {
        Some(match self.0 {
            1 | 4 | 9 | 18 | 20 => 5500.0,
            2 | 14 => 4150.0,
            3 | 17 => 2856.0,
            10 => 6500.0,
            11 | 22 | 21 => 6500.0,
            12 => 6430.0,
            13 => 5000.0,
            15 => 3450.0,
            16 => 2940.0,
            19 => 4874.0,
            23 => 5003.0,
            24 => 3200.0,
            _ => return None,
        })
    }
}

/// A colour matrix in DNG `ColorMatrix` convention: maps CIE XYZ to camera space,
/// `rows` (camera channels, usually 3) x 3.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ColorMatrix {
    pub illuminant: Illuminant,
    pub rows: u8,
    pub values: Vec<f32>,
}

impl ColorMatrix {
    pub fn new(illuminant: Illuminant, rows: usize, values: Vec<f32>) -> Option<Self> {
        (values.len() == rows * 3 && rows >= 3).then_some(ColorMatrix { illuminant, rows: rows as u8, values })
    }
    pub fn get(&self, r: usize, c: usize) -> f32 {
        self.values[r * 3 + c]
    }
}

/// Where colour calibration came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ColorSource {
    #[default]
    None,
    /// Embedded in the file (DNG tags, maker notes).
    File,
    /// From the FlashRaw camera database.
    CameraDb,
}

/// Colour calibration data.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ColorMatrices {
    /// XYZ → camera matrices (DNG ColorMatrix1/2/3 or database matrix, usually D65).
    pub color: Vec<ColorMatrix>,
    /// Camera → XYZ(D50) forward matrices (DNG ForwardMatrix1/2/3).
    pub forward: Vec<ColorMatrix>,
    /// Per-camera calibration (DNG CameraCalibration1/2/3), `rows x rows` stored row-major in `values`.
    pub calibration: Vec<ColorMatrix>,
    /// DNG AnalogBalance.
    pub analog_balance: Vec<f32>,
    pub source: ColorSource,
}

impl ColorMatrices {
    /// Preferred XYZ→camera matrix: D65 if present, otherwise the last one listed.
    pub fn preferred(&self) -> Option<&ColorMatrix> {
        self.color.iter().find(|m| m.illuminant == Illuminant::D65).or_else(|| self.color.last())
    }
}

/// EXIF orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Orientation {
    #[default]
    Normal = 1,
    FlipH = 2,
    Rotate180 = 3,
    FlipV = 4,
    Transpose = 5,
    Rotate90 = 6,
    Transverse = 7,
    Rotate270 = 8,
}

impl Orientation {
    pub fn from_exif(v: u32) -> Self {
        match v {
            2 => Orientation::FlipH,
            3 => Orientation::Rotate180,
            4 => Orientation::FlipV,
            5 => Orientation::Transpose,
            6 => Orientation::Rotate90,
            7 => Orientation::Transverse,
            8 => Orientation::Rotate270,
            _ => Orientation::Normal,
        }
    }
    pub fn exif(self) -> u32 {
        self as u32
    }
    /// True if width and height swap when applied.
    pub fn swaps_dimensions(self) -> bool {
        matches!(
            self,
            Orientation::Transpose | Orientation::Rotate90 | Orientation::Transverse | Orientation::Rotate270
        )
    }
}

/// RAW container formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum RawFormat {
    Dng,
    Cr2,
    Cr3,
    Crw,
    Nef,
    Nrw,
    Arw,
    Sr2,
    Srf,
    Raf,
    Rw2,
    Orf,
    Pef,
    Iiq,
    Mos,
    ThreeFr,
    Fff,
    Mrw,
    Srw,
    X3f,
    Kdc,
    Dcr,
    Dcs,
    Erf,
    Mef,
    Mdc,
    Nakedraw,
    Ari,
    Other,
}

impl RawFormat {
    pub fn name(self) -> &'static str {
        match self {
            RawFormat::Dng => "DNG",
            RawFormat::Cr2 => "CR2",
            RawFormat::Cr3 => "CR3",
            RawFormat::Crw => "CRW",
            RawFormat::Nef => "NEF",
            RawFormat::Nrw => "NRW",
            RawFormat::Arw => "ARW",
            RawFormat::Sr2 => "SR2",
            RawFormat::Srf => "SRF",
            RawFormat::Raf => "RAF",
            RawFormat::Rw2 => "RW2",
            RawFormat::Orf => "ORF",
            RawFormat::Pef => "PEF",
            RawFormat::Iiq => "IIQ",
            RawFormat::Mos => "MOS",
            RawFormat::ThreeFr => "3FR",
            RawFormat::Fff => "FFF",
            RawFormat::Mrw => "MRW",
            RawFormat::Srw => "SRW",
            RawFormat::X3f => "X3F",
            RawFormat::Kdc => "KDC",
            RawFormat::Dcr => "DCR",
            RawFormat::Dcs => "DCS",
            RawFormat::Erf => "ERF",
            RawFormat::Mef => "MEF",
            RawFormat::Mdc => "MDC",
            RawFormat::Nakedraw => "RAW",
            RawFormat::Ari => "ARI",
            RawFormat::Other => "other",
        }
    }
}

impl fmt::Display for RawFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Payload compression / packing scheme (normalised across vendors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Compression {
    /// Uncompressed samples, possibly bit-packed.
    Uncompressed,
    /// ITU T.81 lossless JPEG (process 14), incl. Canon/DNG/Sony/Hasselblad variants.
    LosslessJpeg,
    /// Baseline (lossy) JPEG, e.g. lossy DNG.
    LossyJpeg,
    /// Deflate (DNG floating point / zip).
    Deflate,
    /// JPEG XL (DNG 1.7).
    JpegXl,
    CanonCrw,
    /// Canon CR3 "CRX" wavelet codec (RAW and C-RAW).
    CanonCrx,
    /// Canon sRAW/mRAW (YCbCr lossless JPEG).
    CanonSraw,
    NikonHuffman,
    /// Nikon "high efficiency" (TicoRAW).
    NikonHighEfficiency,
    /// Sony ARW v1 (A100 era) curve/huffman.
    SonyArw1,
    /// Sony ARW v2 lossy 7/8-bit delta blocks with tone curve.
    SonyArw2,
    /// Sony lossless (lossless JPEG tiles).
    SonyLossless,
    Panasonic,
    PanasonicV5,
    PanasonicV6,
    PanasonicV7,
    PanasonicV8,
    FujiCompressed,
    OlympusCompressed,
    PentaxHuffman,
    SamsungV0,
    SamsungV1,
    SamsungV2,
    PhaseOne,
    PhaseOneS,
    HasselbladLjpeg,
    LeafLjpeg,
    KodakDc120,
    Kodak65000,
    KodakRgb,
    KodakYcbcr,
    KodakSegmented,
    SigmaX3f,
    MinoltaPacked,
    Other,
}

impl Compression {
    pub fn name(self) -> &'static str {
        match self {
            Compression::Uncompressed => "uncompressed",
            Compression::LosslessJpeg => "lossless-jpeg",
            Compression::LossyJpeg => "lossy-jpeg",
            Compression::Deflate => "deflate",
            Compression::JpegXl => "jpeg-xl",
            Compression::CanonCrw => "canon-crw",
            Compression::CanonCrx => "canon-crx",
            Compression::CanonSraw => "canon-sraw",
            Compression::NikonHuffman => "nikon-huffman",
            Compression::NikonHighEfficiency => "nikon-he",
            Compression::SonyArw1 => "sony-arw1",
            Compression::SonyArw2 => "sony-arw2",
            Compression::SonyLossless => "sony-lossless",
            Compression::Panasonic => "panasonic-v4",
            Compression::PanasonicV5 => "panasonic-v5",
            Compression::PanasonicV6 => "panasonic-v6",
            Compression::PanasonicV7 => "panasonic-v7",
            Compression::PanasonicV8 => "panasonic-v8",
            Compression::FujiCompressed => "fuji-compressed",
            Compression::OlympusCompressed => "olympus",
            Compression::PentaxHuffman => "pentax-huffman",
            Compression::SamsungV0 => "samsung-v0",
            Compression::SamsungV1 => "samsung-v1",
            Compression::SamsungV2 => "samsung-v2",
            Compression::PhaseOne => "phaseone",
            Compression::PhaseOneS => "phaseone-s",
            Compression::HasselbladLjpeg => "hasselblad",
            Compression::LeafLjpeg => "leaf",
            Compression::KodakDc120 => "kodak-dc120",
            Compression::Kodak65000 => "kodak-65000",
            Compression::KodakRgb => "kodak-rgb",
            Compression::KodakYcbcr => "kodak-ycbcr",
            Compression::KodakSegmented => "kodak-segmented",
            Compression::SigmaX3f => "sigma-x3f",
            Compression::MinoltaPacked => "minolta-packed",
            Compression::Other => "other",
        }
    }
}

impl fmt::Display for Compression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfa_parse_and_shift() {
        let p = CfaPattern::parse("RGGB").unwrap();
        assert_eq!(p, CfaPattern::RGGB);
        assert!(p.is_bayer());
        assert_eq!(p.shifted(1, 0), CfaPattern::GRBG);
        assert_eq!(p.shifted(1, 1), CfaPattern::BGGR);
        assert_eq!(p.shifted(0, 1), CfaPattern::GBRG);
        let x = CfaPattern::parse("GGRGGB GGBGGR BRGRBG GGBGGR GGRGGB RBGBRG").unwrap();
        assert!(x.is_xtrans());
        assert_eq!(x.to_string().len(), 36 + 5);
    }

    #[test]
    fn black_level_pattern() {
        let b = BlackLevel::pattern2x2([1.0, 2.0, 3.0, 4.0]);
        assert_eq!(b.at(0, 0, 0), 1.0);
        assert_eq!(b.at(3, 1, 0), 4.0);
        let s = b.shifted(1, 0);
        assert_eq!(s.values, vec![2.0, 1.0, 4.0, 3.0]);
    }

    #[test]
    fn rect_ops() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Some(Rect::new(5, 5, 5, 5)));
        assert_eq!(Rect::new(20, 0, 5, 5).clamp_to(10, 10), Rect::default());
    }

    #[test]
    fn wb_normalise() {
        let wb = WhiteBalance::from_multipliers(2.0, 1.0, 1.5, 1.0).unwrap();
        assert_eq!(wb.coeffs, [2.0, 1.0, 1.5, 1.0]);
        assert!(WhiteBalance::from_multipliers(0.0, 1.0, 1.0, 1.0).is_none());
        let n = WhiteBalance::from_neutral(&[0.5, 1.0, 0.8]).unwrap();
        assert!((n.coeffs[0] - 2.0).abs() < 1e-6);
    }
}
