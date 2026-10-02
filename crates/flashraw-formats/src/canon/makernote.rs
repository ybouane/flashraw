//! Canon maker note parsing shared by CR2, CR3 (CMT3) and Canon TIFF raws.
//!
//! The Canon maker note is a plain TIFF IFD (no header) whose value offsets are
//! relative to the enclosing TIFF structure, so [`flashraw_core::tiff`] resolves the
//! entries for us. Only the fields that drive decoding and calibration are parsed:
//!
//! | Tag      | Name            | Used for                                                  |
//! |----------|-----------------|-----------------------------------------------------------|
//! | `0x0001` | CameraSettings  | quality / RecordMode / sRAW quality, lens focal range     |
//! | `0x0004` | ShotInfo        | white-balance index (`wbi`) for old WB tables             |
//! | `0x0007` | FirmwareVersion | sRAW hue offset selection (5D Mark II firmware split)      |
//! | `0x000c` | SerialNumber    | metadata                                                  |
//! | `0x0010` | ModelID         | camera identification (sRAW colour formulas, quirks)      |
//! | `0x0029` | G9 WB table     | as-shot WB of the PowerShot G9                            |
//! | `0x0081` | 1D raw pointer  | raw data location of EOS-1D / 1Ds TIFF raws               |
//! | `0x0095` | LensModel       | metadata                                                  |
//! | `0x00a4` | WB table (1D)   | as-shot WB of EOS-1D / 1Ds                                |
//! | `0x00e0` | SensorInfo      | sensor size, default crop, left optical black             |
//! | `0x4001` | ColorData       | as-shot WB, sRAW WB, per-channel black, white levels      |
//!
//! 16-bit words in the tag; see [`ColorData::parse`].

use flashraw_core::ByteOrder;
use flashraw_core::tiff::{Entry, Ifd};

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
    /// Default crop (the camera's "valid image" area), inclusive.
    pub crop: CanonArea,
    /// Left optical-black area, inclusive.
    pub left_ob: CanonArea,
}

/// Parsed maker note tag `0x4001`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorData {
    /// Number of 16-bit words (selects the layout).
    pub len: usize,
    pub version: u8,
    /// First word of the tag (a reference tool "ColorDataVersion", e.g. 10 for 5D Mark III, -3/-4 for PowerShots).
    pub sub_version: u16,
    /// As-shot WB multipliers in file order `R, G1, G2, B` (1024 ≈ unity for EOS).
    pub as_shot: Option<[u16; 4]>,
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
    /// CameraSettings[3].
    pub quality: Option<u16>,
    /// CameraSettings[9] (1 = JPEG, 2 = CRW+THM, 3 = AVI+THM, 4 = TIF, 5 = TIF+JPEG, 6 = CR2, 7 = CR2+JPEG, 9 = MOV...).
    pub record_mode: Option<u16>,
    /// CameraSettings[46].
    pub sraw_quality: Option<u16>,
    /// Lens focal range (min, max) in mm from CameraSettings.
    pub focal_range: Option<(f32, f32)>,
    /// White-balance index from ShotInfo[7].
    pub wbi: Option<u16>,
    pub sensor: Option<SensorInfo>,
    pub color: Option<ColorData>,
    /// As-shot WB from the EOS-1D/1Ds table (tag 0x00a4), `R, G, B`.
    pub wb_table_1d: Option<[u16; 3]>,
    /// As-shot WB from the PowerShot G9 table (tag 0x0029), `R, G, B, G2`.
    pub wb_g9: Option<[u32; 4]>,
    /// EOS-1D/1Ds raw data pointer (tag 0x0081): offset of the raw image header.
    pub raw_1d: Option<u32>,
    /// Tag 0x4021 marks an in-camera multi-shot composite whose data is already white balanced.
    pub multishot: bool,
}

fn u16s(e: &Entry) -> Vec<u16> {
    let n = e.data.len() / 2;
    (0..n).map(|i| e.order.u16([e.data[2 * i], e.data[2 * i + 1]])).collect()
}

/// Read `count` u16 values at word index `at` (None if out of range).
fn words<const N: usize>(w: &[u16], at: usize) -> Option<[u16; N]> {
    let s = w.get(at..at.checked_add(N)?)?;
    let mut out = [0u16; N];
    out.copy_from_slice(s);
    Some(out)
}

/// Word offsets within ColorData: as-shot WB, sRAW WB, black, white, alternate black.
type Offsets = (Option<usize>, Option<usize>, Option<usize>, Option<usize>, Option<usize>);

impl ColorData {
    /// Parse a ColorData block from its 16-bit words. `len` is the tag's value count,
    /// which identifies the layout (PowerShots store the block as UNDEFINED bytes, so
    /// their count is in bytes while the payload is still read as 16-bit words).
    pub fn parse(w: &[u16], len: usize) -> Option<ColorData> {
        if len <= 500 || w.is_empty() {
            return None;
        }
        let mut cd = ColorData { len, sub_version: w[0], ..Default::default() };
        // (as-shot WB word offset, sRAW WB offset, black offset, white offset, alt black offset)
        let (wb, sraw, black, white, black2): Offsets = match len {
            582 => {
                cd.version = 1; // 20D, 350D
                (Some(0x19), None, Some(0xa6), None, None)
            }
            653 => {
                cd.version = 2; // 1D Mark II, 1Ds Mark II
                (Some(0x22), None, Some(0x11e), None, None)
            }
            796 => {
                cd.version = 3; // 1D Mark II N, 5D, 30D, 400D
                (Some(0x3f), None, Some(0xc4), None, None)
            }
            674 | 692 | 702 | 1227 | 1250 | 1251 | 1337 | 1338 | 1346 => {
                cd.version = 4;
                let (b, wl) = match cd.sub_version {
                    4 | 5 => (0x2b4, Some(0x2b8)),
                    6 | 7 => (0x2cb, Some(0x2cf)),
                    9 => (0x2cf, Some(0x2d3)),
                    _ => (0xe7, None),
                };
                (Some(0x3f), Some(0x4e), Some(b), wl, None)
            }
            5120 => {
                cd.version = 5; // PowerShots, EOS M (early)
                match cd.sub_version {
                    0xfffc => (Some(0x47), None, Some(0x14d), Some(0x569), None),
                    0xfffd => (Some(0x47), None, Some(0x108), None, None),
                    _ => (Some(0x47), None, None, None, None),
                }
            }
            1273 | 1275 => {
                cd.version = 6; // 600D, 1200D
                (Some(0x3f), Some(0x62), Some(0x1df), Some(0x1e3), None)
            }
            1312 | 1313 | 1316 | 1506 => {
                cd.version = 7; // 5D Mark III, 6D, 70D, 1D X, 7D Mark II, 750D...
                let (b, wl) = match cd.sub_version {
                    10 => (Some(0x1f8), Some(0x1fc)),
                    11 => (Some(0x2d8), Some(0x2dc)),
                    _ => (None, None),
                };
                (Some(0x3f), Some(0x7b), b, wl, None)
            }
            1560 | 1592 | 1353 | 1602 => {
                cd.version = 8; // 5DS, 5D Mark IV, 80D, 1D X Mark II, 1300D, 200D, 6D Mark II...
                let (b, wl) = if cd.sub_version == 14 { (0x22c, 0x230) } else { (0x30a, 0x30e) };
                (Some(0x3f), Some(0x80), Some(b), Some(wl), None)
            }
            1820 | 1824 | 1816 => {
                cd.version = 9; // M50, R, RP, 250D, 90D, M6 Mark II, G7 X Mark III...
                let o = 0x47;
                (Some(o), None, Some(o + 0x2d1), Some(o + 0x2d5), Some(o + 0x102))
            }
            1770 | 2024 | 3656 => {
                cd.version = 10; // R5, R6, 1D X Mark III
                let o = 0x55;
                (Some(o), None, Some(o + 0x2d1), Some(o + 0x2d5), Some(o + 0x102))
            }
            3973 | 3778 => {
                cd.version = 11; // R3, R7, R10, R6 Mark II, R8, R50...
                let o = 0x69;
                (Some(o), None, Some(o + 0x213), Some(o + 0x217), Some(o + 0x102))
            }
            4528 => {
                cd.version = 12; // R1, R5 Mark II
                let o = 0x69;
                (Some(o), None, Some(o + 0x227), Some(o + 0x22b), Some(o + 0x116))
            }
            _ => (None, None, None, None, None),
        };
        if let Some(o) = wb {
            cd.as_shot = words::<4>(w, o);
        }
        if let Some(o) = sraw {
            // truncates back to u16; the sRAW decoder then applies `v * mul >> 10`.
            if let Some(m) = words::<4>(w, o) {
                let rgbg = [m[0], m[1], m[3], m[2]];
                let max = rgbg.iter().copied().max().unwrap_or(0) as f32 / 1024.0;
                cd.sraw_mul = Some(rgbg.map(|v| (v as f32 * max) as u16));
            }
        }
        let mut avg_black = 0u32;
        if let Some(o) = black {
            if let Some(b) = words::<4>(w, o) {
                avg_black = b.iter().map(|&v| v as u32).sum::<u32>() / 4;
                cd.black = Some(b);
            }
        }
        if let Some(o) = white {
            // adjacent, which is the same position.
            if let Some([n, s]) = words::<2>(w, o) {
                cd.normal_white = Some(n);
                cd.specular_white = Some(s);
            }
        }
        if avg_black == 0 {
            if let Some(b) = black2.and_then(|o| words::<4>(w, o)) {
                cd.black = Some(b);
            }
        }
        Some(cd)
    }

    pub fn average_black(&self) -> u32 {
        self.black.map(|b| b.iter().map(|&v| v as u32).sum::<u32>() / 4).unwrap_or(0)
    }
}

impl CanonMakernote {
    /// Parse a Canon maker note IFD.
    pub fn parse(ifd: &Ifd) -> CanonMakernote {
        let mut mn = CanonMakernote::default();
        for e in &ifd.entries {
            match e.tag {
                0x0001 => mn.parse_camera_settings(&u16s(e)),
                0x0004 => {
                    let w = u16s(e);
                    mn.wbi = w.get(7).copied();
                }
                0x0007 => mn.firmware = Some(e.string()).filter(|s| !s.is_empty()),
                0x000c => mn.serial = e.u32(0),
                0x0010 => mn.model_id = e.u32(0),
                0x0095 => mn.lens_model = Some(e.string()).filter(|s| !s.is_empty()),
                0x00e0 => {
                    let w = u16s(e);
                    if let Some(v) = words::<13>(&w, 0) {
                        let area =
                            |i: usize| CanonArea { left: v[i], top: v[i + 1], right: v[i + 2], bottom: v[i + 3] };
                        mn.sensor = Some(SensorInfo { width: v[1], height: v[2], crop: area(5), left_ob: area(9) });
                    }
                }
                0x4001 => mn.color = ColorData::parse(&u16s(e), e.count as usize),
                // Multi-shot (HDR / multiple exposure) composites are already white balanced.
                0x4021 if e.u32(0).is_some_and(|v| v != 0) && e.u32(1).is_some_and(|v| v != 0) => {
                    mn.multishot = true;
                }
                _ => {}
            }
        }
        // Tags whose interpretation depends on others (wbi).
        for e in &ifd.entries {
            match e.tag {
                0x00a4 => {
                    let w = u16s(e);
                    let i = mn.wbi.unwrap_or(0) as usize * 24;
                    mn.wb_table_1d = words::<3>(&w, i);
                }
                0x0029 => mn.wb_g9 = parse_g9_wb(e, mn.wbi.unwrap_or(0)),
                // Pointer to the raw image header of EOS-1D/1Ds TIFF raws.
                0x0081 => mn.raw_1d = e.u32(0),
                _ => {}
            }
        }
        mn
    }

    fn parse_camera_settings(&mut self, w: &[u16]) {
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

    /// ColorData first, then the EOS-1D table, then the PowerShot G9 table.
    pub fn as_shot_wb(&self) -> Option<[f32; 4]> {
        if self.multishot {
            return Some([1024.0; 4]);
        }
        if let Some(m) = self.color.and_then(|c| c.as_shot) {
            if m.iter().all(|&v| v > 0) {
                return Some([m[0] as f32, m[1] as f32, m[3] as f32, m[2] as f32]);
            }
        }
        if let Some(m) = self.wb_table_1d {
            if m.iter().all(|&v| v > 0) {
                return Some([m[0] as f32, m[1] as f32, m[2] as f32, m[1] as f32]);
            }
        }
        if let Some(m) = self.wb_g9 {
            if m.iter().all(|&v| v > 0) {
                return Some(m.map(|v| v as f32));
            }
        }
        None
    }

    /// Firmware version as a single comparable integer (`a.b.c` → `a*10^6 + b*10^3 + c`).
    pub fn firmware_number(&self) -> Option<u32> {
        let s = self.firmware.as_deref()?;
        let start = s.find(|c: char| c.is_ascii_digit())?;
        let mut parts = s[start..].split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
        let mut v = [0u32; 3];
        for slot in v.iter_mut() {
            match parts.next().and_then(|p| p.parse::<u32>().ok()) {
                Some(n) => *slot = n,
                None => break,
            }
        }
        Some((v[0].saturating_mul(1000).saturating_add(v[1])).saturating_mul(1000).saturating_add(v[2]))
    }
}

/// PowerShot G9 WB table (tag 0x0029): an 8-byte header, then 32-byte lines holding
/// four u32 multipliers in `G, R, B, G2` order followed by 16 unused bytes. The
/// as-shot entry is the line matching `wbi`, falling back to Auto (line 0).
fn parse_g9_wb(e: &Entry, wbi: u16) -> Option<[u32; 4]> {
    let d = e.data;
    let get = |i: usize| -> Option<u32> {
        let o = i.checked_mul(4)?;
        let b = d.get(o..o + 4)?;
        Some(e.order.u32([b[0], b[1], b[2], b[3]]))
    };
    let line = |n: usize| -> Option<[u32; 4]> {
        let base = 2 + n * 8; // in u32 units
        let g = [get(base)?, get(base + 1)?, get(base + 2)?, get(base + 3)?];
        Some([g[1], g[0], g[2], g[3]])
    };
    // Canon wbi → G9 line (Auto, Daylight, Cloudy, Tungsten, Fluorescent, FluorescentHigh,
    // Flash, Underwater, Custom1, Custom2); unmatched indices fall back to Auto.
    let n = match wbi {
        0..=4 => wbi as usize,
        5 => 6,  // Flash
        14 => 5, // Fluorescent high
        15 => 8, // Custom 1
        16 => 9, // Custom 2
        17 => 7, // Underwater
        _ => 0,
    };
    line(n).or_else(|| line(0))
}

/// Read 16-bit words of an entry (public helper for format modules).
pub fn entry_words(e: &Entry) -> Vec<u16> {
    u16s(e)
}

/// Convenience: byte order independent u16 reader for raw maker-note blobs.
pub fn read_u16(order: ByteOrder, d: &[u8], at: usize) -> Option<u16> {
    let b = d.get(at..at + 2)?;
    Some(order.u16([b[0], b[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colordata_v7_offsets() {
        let mut w = vec![0u16; 1312];
        w[0] = 10;
        w[0x3f..0x43].copy_from_slice(&[2000, 1024, 1024, 1500]);
        w[0x7b..0x7f].copy_from_slice(&[2048, 1024, 1024, 1536]);
        w[0x1f8..0x1fc].copy_from_slice(&[2047, 2048, 2049, 2046]);
        w[0x1fc..0x1fe].copy_from_slice(&[15000, 15488]);
        let cd = ColorData::parse(&w, w.len()).unwrap();
        assert_eq!(cd.version, 7);
        assert_eq!(cd.as_shot, Some([2000, 1024, 1024, 1500]));
        assert_eq!(cd.black, Some([2047, 2048, 2049, 2046]));
        assert_eq!(cd.specular_white, Some(15488));
        // sRAW: RGBG order scaled by max/1024 = 2.
        assert_eq!(cd.sraw_mul, Some([4096, 2048, 3072, 2048]));
        assert_eq!(cd.average_black(), 2047);
    }

    #[test]
    fn colordata_rejects_short_and_unknown() {
        assert!(ColorData::parse(&[0u16; 100], 100).is_none());
        let cd = ColorData::parse(&[7u16; 999], 999).unwrap();
        assert_eq!(cd.version, 0);
        assert!(cd.as_shot.is_none());
    }

    #[test]
    fn firmware_numbers() {
        let mn = CanonMakernote { firmware: Some("Firmware Version 2.0.7".into()), ..Default::default() };
        assert_eq!(mn.firmware_number(), Some(2_000_007));
        let mn = CanonMakernote { firmware: Some("Firmware 1.1".into()), ..Default::default() };
        assert_eq!(mn.firmware_number(), Some(1_001_000));
    }
}
