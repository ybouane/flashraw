//! Canon maker note parsing shared by CR2, CR3 and Canon TIFF raws.
//!
//! The maker note is a TIFF directory whose entries are mostly arrays of 16-bit words. Only the
//! fields that decoding needs are extracted; everything is read with bounds-checked accessors, so a
//! damaged entry loses its fields and nothing else.

use flashraw_core::tiff::{Entry, Ifd, types};
use flashraw_core::{BlackLevel, ByteOrder, CfaColor, CfaPattern, Orientation, Rect};

/// Rectangle in Canon's inclusive `left, top, right, bottom` convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CanonArea {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl CanonArea {
    /// Width of the inclusive area (0 if inverted).
    pub fn width(&self) -> u32 {
        (self.right as u32 + 1).saturating_sub(self.left as u32)
    }
    /// Height of the inclusive area (0 if inverted).
    pub fn height(&self) -> u32 {
        (self.bottom as u32 + 1).saturating_sub(self.top as u32)
    }
}

/// Maker note tag `0x00e0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SensorInfo {
    pub width: u16,
    pub height: u16,
    pub crop: CanonArea,
    pub left_ob: CanonArea,
}

/// Parsed maker note tag `0x4001`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorData {
    /// Element count of the tag (selects the layout).
    pub len: usize,
    /// Layout family 1..=12 (0 = unknown).
    pub version: u8,
    /// First word of the block.
    pub sub_version: u16,
    /// As-shot multipliers in file order `R, G1, G2, B`.
    pub as_shot: Option<[u16; 4]>,
    /// sRAW multipliers in `R, G, B, G2` order, already scaled.
    pub sraw_mul: Option<[u16; 4]>,
    /// Per-channel black level in file order `R, G1, G2, B`.
    pub black: Option<[u16; 4]>,
    pub normal_white: Option<u16>,
    pub specular_white: Option<u16>,
}

/// Fields of the Canon maker note relevant to decoding.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CanonMakernote {
    pub model_id: Option<u32>,
    pub firmware: Option<String>,
    pub serial: Option<u32>,
    pub lens_model: Option<String>,
    pub quality: Option<u16>,
    pub record_mode: Option<u16>,
    pub sraw_quality: Option<u16>,
    pub focal_range: Option<(f32, f32)>,
    pub wbi: Option<u16>,
    pub sensor: Option<SensorInfo>,
    pub aspect: Option<(u32, u32)>,
    pub color: Option<ColorData>,
    pub wb_table_1d: Option<[u16; 3]>,
    pub wb_g9: Option<[u32; 4]>,
    pub raw_1d: Option<u32>,
    pub multishot: bool,
    pub camera_info: Vec<u8>,
    pub camera_info_bytes: bool,
    pub order: Option<ByteOrder>,
}

// ---------------------------------------------------------------------------------------------
// ColorData layouts (tag 0x4001). DATA tables: the layout is chosen by the element count of the tag
// and, for some counts, by the first word of the block (the "sub-version"). Offsets are in 16-bit
// words from the start of the block. Evidence per family is in the specification's tables
// (`colordata_layouts`); every family below has at least one corpus file unless noted.
// ---------------------------------------------------------------------------------------------

/// Word offsets of the fields of one ColorData layout (`None`: the layout has no such field).
#[derive(Debug, Clone, Copy)]
struct Fields {
    version: u8,
    /// As-shot multipliers, four words `R, G1, G2, B`.
    as_shot: usize,
    /// sRAW multiplier quad.
    sraw: Option<usize>,
    /// Per-channel black quad.
    black: Option<usize>,
    /// Normal / specular white pair.
    white: Option<usize>,
    /// Black quad used when the primary one is missing or zero.
    alt_black: Option<usize>,
}

const fn fields(
    version: u8,
    as_shot: usize,
    sraw: Option<usize>,
    black: Option<usize>,
    white: Option<usize>,
    alt_black: Option<usize>,
) -> Fields {
    Fields { version, as_shot, sraw, black, white, alt_black }
}

/// Families selected by the element count alone.
///
/// Flags (see the specification's `colordata_layouts`): the as-shot quad of each family is a plausible
/// white balance on every corpus file and the black quad agrees with the optical black measured on the
/// decoded data to within 2.5 (`verified`); the sRAW quad and the white pair are positions that hold
/// plausible values with no numeric ground truth (`partly`).
///
/// * 582 (20D, 350D; 4 files), 653 (1D II, 1Ds II; 2), 796 (5D, 30D, 400D, 1D II N; 5): verified.
/// * 1273, 1275 (600D, 1200D; 4): verified; sRAW and white positions partly.
/// * 1816, 1820, 1824 (M50, R, RP, 90D, 250D, G7X III, SX70; 61): verified; the alternative black quad is
///   used only when the primary one is zero.
/// * 1770, 2024, 3656 (1D X III, R5, R6; 14): verified, same remark.
/// * 4528 (R5 Mark II; 4): verified.
const BY_COUNT: &[(&[usize], Fields)] = &[
    (&[582], fields(1, 0x19, None, Some(0xa6), None, None)),
    (&[653], fields(2, 0x22, None, Some(0x11e), None, None)),
    (&[796], fields(3, 0x3f, None, Some(0xc4), None, None)),
    (&[1273, 1275], fields(6, 0x3f, Some(0x62), Some(0x1df), Some(0x1e3), None)),
    (&[1816, 1820, 1824], fields(9, 0x47, None, Some(0x318), Some(0x31c), Some(0x149))),
    (&[1770, 2024, 3656], fields(10, 0x55, None, Some(0x326), Some(0x32a), Some(0x157))),
    (&[4528], fields(12, 0x69, None, Some(0x290), Some(0x294), Some(0x17f))),
];

/// Families whose positions depend on the first word (the "sub-version").
///
/// * 674 .. 1346, family 4 (1D III, 40D, 1Ds III, 450D/1000D, 5D II, 500D, 1D IV, 550D, 60D; 34 files):
///   verified, except that the 12-bit 1000D / Rebel XS express the black level in 14-bit units (4x the
///   sensor value). Sub-versions seen: 2, 3, 4, 5, 6, 7, 9.
/// * 5120 (PowerShots, EOS M; 28 files): sub-versions 0xfffc and 0xfffd verified for black; the as-shot
///   quad has no colour-temperature cross-check; other sub-versions yield the as-shot quad only.
/// * 1312 .. 1506, family 7 (5D III, 6D, 70D, 1D X, 650D/700D, 7D II, 750D; 26 files): verified for
///   sub-versions 10 and 11; others have no black.
/// * 1353 .. 1602, family 8 (1300D/2000D, 5DS, 5D IV, 80D, 1D X II, 6D II; 34 files): verified; sub-version
///   14 and the others use different black positions.
/// * 3778, 3973 (R3, R7, R10, R6 II, R8, R50 below 64; R5 II, R6 III, R50 V, V1 from 64): verified. A 3973
///   block from 64 on has no layout.
fn by_count_and_version(count: usize, w0: u16) -> Option<Fields> {
    let f = match count {
        // 1D III, 40D, 1Ds III, 450D/1000D, 5D II, 500D, 1D IV, 550D, 60D.
        674 | 692 | 702 | 1227 | 1250 | 1251 | 1337 | 1338 | 1346 => {
            let (black, white) = match w0 {
                4 | 5 => (0x2b4, Some(0x2b8)),
                6 | 7 => (0x2cb, Some(0x2cf)),
                9 => (0x2cf, Some(0x2d3)),
                _ => (0xe7, None),
            };
            fields(4, 0x3f, Some(0x4e), Some(black), white, None)
        }
        // PowerShots and EOS M (bytes counted, 5120).
        5120 => {
            let (black, white) = match w0 {
                0xfffc => (Some(0x14d), Some(0x569)),
                0xfffd => (Some(0x108), None),
                _ => (None, None),
            };
            fields(5, 0x47, None, black, white, None)
        }
        // 5D III, 6D, 70D, 1D X, 650D/700D, 7D II, 750D.
        1312 | 1313 | 1316 | 1506 => {
            let (black, white) = match w0 {
                10 => (Some(0x1f8), Some(0x1fc)),
                11 => (Some(0x2d8), Some(0x2dc)),
                _ => (None, None),
            };
            fields(7, 0x3f, Some(0x7b), black, white, None)
        }
        // 1300D/2000D, 5DS, 5D IV, 80D, 1D X II, 6D II, 77D.
        1353 | 1560 | 1592 | 1602 => {
            let (black, white) = if w0 == 14 { (0x22c, 0x230) } else { (0x30a, 0x30e) };
            fields(8, 0x3f, Some(0x80), Some(black), Some(white), None)
        }
        // R3, R7, R10, R6 II, R8, R50 (versions below 64) and R5 II, R6 III, V1 (64 and above).
        3778 | 3973 => {
            if w0 < 64 {
                fields(11, 0x69, None, Some(0x27c), Some(0x280), Some(0x16b))
            } else if count == 3778 {
                fields(12, 0x69, None, Some(0x290), Some(0x294), Some(0x17f))
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(f)
}

fn layout_for(count: usize, w0: u16) -> Option<Fields> {
    for (counts, f) in BY_COUNT {
        if counts.contains(&count) {
            return Some(*f);
        }
    }
    by_count_and_version(count, w0)
}

fn quad(w: &[u16], at: usize) -> Option<[u16; 4]> {
    let s = w.get(at..at.checked_add(4)?)?;
    Some([s[0], s[1], s[2], s[3]])
}

fn pair(w: &[u16], at: usize) -> Option<(u16, u16)> {
    let s = w.get(at..at.checked_add(2)?)?;
    Some((s[0], s[1]))
}

impl ColorData {
    /// Parse a ColorData block from its 16-bit words; `len` is the element count of the tag.
    pub fn parse(w: &[u16], len: usize) -> Option<ColorData> {
        if len <= 500 || w.is_empty() {
            return None;
        }
        let mut cd = ColorData { len, sub_version: w[0], ..ColorData::default() };
        let Some(f) = layout_for(len, w[0]) else {
            return Some(cd);
        };
        cd.version = f.version;
        cd.as_shot = quad(w, f.as_shot);
        cd.sraw_mul = f.sraw.and_then(|at| quad(w, at)).map(|m| {
            // File order R, G1, G2, B becomes R, G1, B, G2, scaled so the largest entry maps to its
            // own value times max / 1024.
            let r = [m[0], m[1], m[3], m[2]];
            let scale = r.iter().copied().max().unwrap_or(0) as f32 / 1024.0;
            r.map(|v| (v as f32 * scale) as u16)
        });
        cd.black = f.black.and_then(|at| quad(w, at));
        if let Some((n, s)) = f.white.and_then(|at| pair(w, at)) {
            cd.normal_white = Some(n);
            cd.specular_white = Some(s);
        }
        if cd.average_black() == 0 {
            if let Some(alt) = f.alt_black.and_then(|at| quad(w, at)) {
                cd.black = Some(alt);
            }
        }
        Some(cd)
    }

    /// Integer average of the four black levels (0 when absent).
    pub fn average_black(&self) -> u32 {
        self.black.map_or(0, |b| b.iter().map(|&v| v as u32).sum::<u32>() / 4)
    }
}

// ---------------------------------------------------------------------------------------------
// PowerShot G9 white-balance table (tag 0x0029). DATA: Canon's white-balance setting number to the
// line of the table. Verified only for setting 15 (Custom 1, line 8); the other entries follow
// Canon's published enumeration and have no sample file.
// ---------------------------------------------------------------------------------------------

/// Line of the G9 table for a Canon white-balance setting (0 Auto, 1 Daylight, 2 Cloudy,
/// 3 Tungsten, 4 Fluorescent, 5 Flash ... see the specification, section 3.4).
fn g9_line(wbi: u16) -> usize {
    match wbi {
        0..=4 => wbi as usize,
        5 => 6,
        14 => 5,
        15 => 8,
        16 => 9,
        17 => 7,
        _ => 0,
    }
}

fn g9_multipliers(e: &Entry, wbi: u16) -> Option<[u32; 4]> {
    let int = |i: usize| -> Option<u32> {
        let at = i.checked_mul(4)?;
        let b = e.data.get(at..at.checked_add(4)?)?;
        Some(e.order.u32([b[0], b[1], b[2], b[3]]))
    };
    let line = |l: usize| -> Option<[u32; 4]> {
        let first = 2usize.checked_add(l.checked_mul(8)?)?;
        // File order of a line: G, R, B, G2.
        let (g, r, b, g2) = (int(first)?, int(first + 1)?, int(first + 2)?, int(first + 3)?);
        Some([r, g, b, g2])
    };
    line(g9_line(wbi)).or_else(|| line(0))
}

impl CanonMakernote {
    /// The camera's image area in a `raw_w x raw_h` buffer (spec section 3.6).
    pub fn image_area(&self, raw_w: u32, raw_h: u32, scaled: bool) -> Option<Rect> {
        let s = self.sensor.as_ref()?;
        if !scaled && (s.width as u32, s.height as u32) != (raw_w, raw_h) {
            return None;
        }
        let c = &s.crop;
        let r = Rect::new(c.left as u32, c.top as u32, c.width(), c.height());
        if r.is_empty() || r.right() > raw_w || r.bottom() > raw_h {
            return None;
        }
        if !scaled {
            if let Some((w, h)) = self.aspect {
                if w <= r.width && h <= r.height {
                    return Some(crate::common::centred_crop(r, w, h));
                }
            }
        }
        Some(r)
    }

    /// Parse a Canon maker note IFD.
    pub fn parse(ifd: &Ifd) -> CanonMakernote {
        let mut mn = CanonMakernote::default();
        for e in &ifd.entries {
            match e.tag {
                0x0001 => mn.camera_settings(&entry_words(e)),
                0x0004 => mn.wbi = entry_words(e).get(7).copied(),
                0x0007 => {
                    let s = e.string();
                    mn.firmware = (!s.is_empty()).then_some(s);
                }
                0x000c => mn.serial = e.u32(0),
                0x000d => {
                    mn.camera_info = e.data.to_vec();
                    mn.camera_info_bytes = e.typ == types::UNDEFINED;
                    mn.order = Some(e.order);
                }
                0x0010 => mn.model_id = e.u32(0),
                0x0095 => {
                    let s = e.string();
                    mn.lens_model = (!s.is_empty()).then_some(s);
                }
                0x00e0 => mn.sensor = sensor_info(&entry_words(e)),
                0x009a => {
                    mn.aspect = match (e.u32(1), e.u32(2)) {
                        (Some(a), Some(b)) if a > 0 && b > 0 => Some((a, b)),
                        _ => None,
                    }
                }
                0x4001 => mn.color = ColorData::parse(&entry_words(e), e.count as usize),
                0x4021 => mn.multishot = e.u32(0).is_some_and(|v| v != 0) && e.u32(1).is_some_and(|v| v != 0),
                _ => {}
            }
        }
        // Tables that depend on the final white-balance setting.
        mn.raw_1d = ifd.get(0x0081).and_then(|e| e.u32(0));
        if let Some(e) = ifd.get(0x00a4) {
            let w = entry_words(e);
            let at = mn.wbi.unwrap_or(0) as usize * 24;
            mn.wb_table_1d = w.get(at..at + 3).map(|s| [s[0], s[1], s[2]]);
        }
        if let Some(e) = ifd.get(0x0029) {
            mn.wb_g9 = g9_multipliers(e, mn.wbi.unwrap_or(0));
        }
        mn
    }

    fn camera_settings(&mut self, w: &[u16]) {
        self.quality = w.get(3).copied();
        self.record_mode = w.get(9).copied();
        self.sraw_quality = w.get(46).copied();
        if let (Some(&max), Some(&min), Some(&units)) = (w.get(23), w.get(24), w.get(25)) {
            let u = if units > 1 { units as f32 } else { 1.0 };
            if min > 0 && max > 0 {
                self.focal_range = Some((min as f32 / u, max as f32 / u));
            }
        }
    }

    /// As-shot multipliers `(R, G, B, G2)`, spec section 3.5.
    pub fn as_shot_wb(&self) -> Option<[f32; 4]> {
        if self.multishot {
            return Some([1024.0; 4]);
        }
        if let Some([r, g1, g2, b]) = self.color.and_then(|c| c.as_shot) {
            if r > 0 && g1 > 0 && g2 > 0 && b > 0 {
                return Some([r as f32, g1 as f32, b as f32, g2 as f32]);
            }
        }
        if let Some([r, g, b]) = self.wb_table_1d {
            if r > 0 && g > 0 && b > 0 {
                return Some([r as f32, g as f32, b as f32, g as f32]);
            }
        }
        if let Some(m) = self.wb_g9 {
            if m.iter().all(|&v| v > 0) {
                return Some(m.map(|v| v as f32));
            }
        }
        None
    }

    /// Orientation-sensor reading and Auto Rotate setting from CameraInfo, spec section 3.7.
    pub fn camera_orientation(
        &self,
        orientation_offset: Option<u32>,
        auto_rotate_offset: Option<u32>,
    ) -> (Option<Orientation>, Option<bool>) {
        let byte = |off: u32| self.camera_info.get(off as usize).copied().filter(|&v| v < 3);
        let reading = match orientation_offset {
            Some(off) => byte(off),
            None => self.scan_orientation_record(),
        };
        let sensor = reading.map(|v| match v {
            1 => Orientation::Rotate90,
            2 => Orientation::Rotate270,
            _ => Orientation::Normal,
        });
        (sensor, auto_rotate_offset.and_then(byte).map(|v| v != 0))
    }

    /// EOS CameraInfo without a database offset: a `0xAAAA` header, a `0xBBBB` marker, then 4-byte
    /// aligned records of which the one with identifier 0x101 holds the reading at +8.
    fn scan_orientation_record(&self) -> Option<u8> {
        let order = self.order?;
        let ci = &self.camera_info;
        if !self.camera_info_bytes || ci.get(..2)? != [0xAA, 0xAA] {
            return None;
        }
        let marker = (2..ci.len().saturating_sub(1)).find(|&i| ci[i] == 0xBB && ci[i + 1] == 0xBB)?;
        let mut p = marker + 2;
        while p + 9 <= ci.len() {
            let id = order.u32([ci[p], ci[p + 1], ci[p + 2], ci[p + 3]]);
            if id == 0x101 && ci[p + 8] < 3 {
                return Some(ci[p + 8]);
            }
            p += 4;
        }
        None
    }

    /// Firmware version as one comparable integer, spec section 3.8.
    pub fn firmware_number(&self) -> Option<u32> {
        let s = self.firmware.as_deref()?;
        let from = s.find(|c: char| c.is_ascii_digit())?;
        let mut parts = [0u32; 3];
        let runs = s[from..].split(|c: char| !c.is_ascii_digit()).filter(|r| !r.is_empty()).take(3);
        for (slot, run) in parts.iter_mut().zip(runs) {
            match run.parse::<u32>() {
                Ok(v) => *slot = v,
                Err(_) => break,
            }
        }
        Some(parts[0].saturating_mul(1000).saturating_add(parts[1]).saturating_mul(1000).saturating_add(parts[2]))
    }
}

fn sensor_info(v: &[u16]) -> Option<SensorInfo> {
    if v.len() < 13 {
        return None;
    }
    let area = |i: usize| CanonArea { left: v[i], top: v[i + 1], right: v[i + 2], bottom: v[i + 3] };
    Some(SensorInfo { width: v[1], height: v[2], crop: area(5), left_ob: area(9) })
}

/// Per-channel black level of the maker note (file order R, G1, G2, B) laid out on the 2x2 visible
/// pattern and moved to the buffer origin by the margin phase.
pub fn black_pattern(black: [u16; 4], visible: &CfaPattern, sx: usize, sy: usize) -> BlackLevel {
    let mut greens = 0;
    let mut v = [0f32; 4];
    for (i, slot) in v.iter_mut().enumerate() {
        let b = match visible.color_at(i % 2, i / 2) {
            CfaColor::Red => black[0],
            CfaColor::Blue => black[3],
            _ => {
                greens += 1;
                if greens == 1 { black[1] } else { black[2] }
            }
        };
        *slot = b as f32;
    }
    BlackLevel::pattern2x2(v).shifted(sx, sy)
}
/// The 16-bit words of an entry's value bytes, in the entry's byte order.
pub fn entry_words(e: &Entry) -> Vec<u16> {
    e.data.chunks_exact(2).map(|c| e.order.u16([c[0], c[1]])).collect()
}

/// A 16-bit value of a byte buffer in the given order.
pub fn read_u16(order: ByteOrder, d: &[u8], at: usize) -> Option<u16> {
    let b = d.get(at..at.checked_add(2)?)?;
    Some(order.u16([b[0], b[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(count: usize, w0: u16) -> Vec<u16> {
        let mut w = vec![0u16; count];
        w[0] = w0;
        w
    }

    #[test]
    fn color_data_family_seven_offsets() {
        let mut w = block(1312, 10);
        w[0x3f..0x43].copy_from_slice(&[2000, 1024, 1024, 1500]);
        w[0x7b..0x7f].copy_from_slice(&[2000, 1024, 1024, 1500]);
        w[0x1f8..0x1fc].copy_from_slice(&[512, 513, 514, 515]);
        let cd = ColorData::parse(&w, 1312).unwrap();
        assert_eq!(cd.version, 7);
        assert_eq!(cd.as_shot, Some([2000, 1024, 1024, 1500]));

        assert_eq!(cd.black, Some([512, 513, 514, 515]));
        assert_eq!(cd.average_black(), 513);
    }

    #[test]
    fn sraw_quad_is_reordered_and_scaled() {
        let mut w = block(1312, 10);
        w[0x7b..0x7f].copy_from_slice(&[2000, 1024, 1024, 1500]);
        let cd = ColorData::parse(&w, 1312).unwrap();
        // Reordered to [2000, 1024, 1500, 1024], factor 2000 / 1024.
        assert_eq!(cd.sraw_mul, Some([3906, 2000, 2929, 2000]));
    }

    #[test]
    fn long_blocks_split_on_the_first_word() {
        let low = ColorData::parse(&block(3778, 5), 3778).unwrap();
        let high = ColorData::parse(&block(3778, 70), 3778).unwrap();
        assert_eq!((low.version, high.version), (11, 12));
        assert!(ColorData::parse(&block(3973, 64), 3973).unwrap().as_shot.is_none());
    }

    #[test]
    fn short_and_unknown_blocks() {
        assert!(ColorData::parse(&block(600, 1), 500).is_none());
        let unknown = ColorData::parse(&block(600, 1), 601).unwrap();
        assert_eq!((unknown.version, unknown.as_shot, unknown.black), (0, None, None));
    }

    #[test]
    fn alternative_black_replaces_a_zero_one() {
        let mut w = block(1816, 1);
        w[0x149..0x14d].copy_from_slice(&[100, 101, 102, 103]);
        assert_eq!(ColorData::parse(&w, 1816).unwrap().black, Some([100, 101, 102, 103]));
    }

    #[test]
    fn firmware_numbers() {
        let mn = |s: &str| CanonMakernote { firmware: Some(s.into()), ..Default::default() };
        assert_eq!(mn("Firmware Version 2.0.7").firmware_number(), Some(2_000_007));
        assert_eq!(mn("Firmware 1.1").firmware_number(), Some(1_001_000));
        assert_eq!(mn("none").firmware_number(), None);
    }

    #[test]
    fn camera_info_orientation() {
        let mut info = vec![0u8; 0x400];
        info[0x83] = 2;
        info[0x3b2] = 1;
        let mn = CanonMakernote { camera_info: info, ..Default::default() };
        assert_eq!(mn.camera_orientation(Some(0x83), Some(0x3b2)), (Some(Orientation::Rotate270), Some(true)));
        assert_eq!(mn.camera_orientation(Some(0x10000), Some(0x83)), (None, Some(true)));
        let mut info = vec![0u8; 0x40];
        info[0x20] = 7;
        let mn = CanonMakernote { camera_info: info, ..Default::default() };
        assert_eq!(mn.camera_orientation(Some(0x20), None), (None, None));
    }

    #[test]
    fn orientation_record_scan() {
        let mut info = vec![0xAA, 0xAA, 0, 0, 0xBB, 0xBB];
        info.extend_from_slice(&0x101u32.to_le_bytes());
        info.extend_from_slice(&[0, 0, 0, 0, 2, 0, 0, 0]);
        let mn = CanonMakernote {
            camera_info: info,
            camera_info_bytes: true,
            order: Some(ByteOrder::Little),
            ..Default::default()
        };
        assert_eq!(mn.camera_orientation(None, None).0, Some(Orientation::Rotate270));
        let plain = CanonMakernote { camera_info_bytes: false, ..mn.clone() };
        assert_eq!(plain.camera_orientation(None, None).0, None);
    }

    #[test]
    fn as_shot_priority() {
        let mut mn = CanonMakernote::default();
        assert_eq!(mn.as_shot_wb(), None);
        mn.wb_g9 = Some([1, 2, 3, 4]);
        assert_eq!(mn.as_shot_wb(), Some([1.0, 2.0, 3.0, 4.0]));
        mn.wb_table_1d = Some([5, 6, 7]);
        assert_eq!(mn.as_shot_wb(), Some([5.0, 6.0, 7.0, 6.0]));
        mn.multishot = true;
        assert_eq!(mn.as_shot_wb(), Some([1024.0; 4]));
    }

    #[test]
    fn image_area_uses_the_aspect_when_unscaled() {
        let mn = CanonMakernote {
            sensor: Some(SensorInfo {
                width: 5920,
                height: 3950,
                crop: CanonArea { left: 140, top: 96, right: 5899, bottom: 3935 },
                left_ob: CanonArea::default(),
            }),
            ..Default::default()
        };
        assert_eq!(mn.image_area(5920, 3950, false), Some(Rect::new(140, 96, 5760, 3840)));
        assert_eq!(mn.image_area(5000, 3950, false), None);
        assert_eq!(mn.image_area(5920, 3950, true), Some(Rect::new(140, 96, 5760, 3840)));
    }
}
