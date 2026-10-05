//! Canon CRW: the CIFF ("Camera Image File Format") container used by Canon cameras
//! from 1998 to 2004 (PowerShot Pro70/Pro90/G1..G6/Pro1/S30..S70, EOS D30/D60/10D/300D).
//!
//! ## Container
//!
//! `II`/`MM` byte order, a u32 header length and the `HEAPCCDR` signature. The rest
//! of the file is a *heap*: records are listed in a table whose offset is stored in
//! the heap's last 4 bytes (`u16 count`, then 10-byte `type, length, offset` records).
//! Record types `0x28xx`/`0x30xx` are sub-heaps; types with bit 14 set store their
//! value in the record itself (the length/offset fields).
//!
//! ## Payload variants
//!
//! - **Compressed** (all cameras since the PowerShot G1/Pro90 and EOS D30): Canon's
//!   block Huffman coder (`flashraw_codecs::canon_crw`), 10-bit samples, optionally
//!   with an uncompressed plane of the 2 low bits giving 12-bit samples. The plane's
//!   presence is detected with a byte-stuffing heuristic; the compressed stream
//!   starts 514 bytes after the raw record (plus the low-bit plane when present).
//! - **Packed 10-bit** (PowerShot A5, A50, Pro70): 16-bit little-endian words,
//!   MSB-first, CMYG sensors.
//! - **PowerShot 600**: 10-bit samples in 10-byte groups, rows interleaved
//!   (even rows first), CMYG sensor.
//!
//! ## Geometry, levels, colour
//!
//! Buffer dimensions come from the sensor-info record (0x1031) and the visible
//! geometry from a table keyed by the raw size (the files do not describe their borders). The
//! black level is measured from the left/right optical-black columns after decoding; the white
//! level is 1023 for 10-bit files, 4095 for 12-bit files unless the camera's table
//! value is lower (EOS D60/10D/300D: 4000). As-shot white balance comes from per-generation
//! sources: ColorInfo2 (0x102c; G1/Pro90, G2/S30/S40), ColorBalance
//! (0x10a9; EOS D60/10D/300D) and the ColorInfo1 white-balance tables (0x0032),
//! selected by the white-balance index of the shot-info record (0x102a).

use crate::common::*;
use flashraw_codecs::canon_crw;
use flashraw_codecs::packed::{self, PackedLayout};
use flashraw_core::bytes::{Cursor, ascii_field};
use flashraw_core::*;

const SIGNATURE: &[u8; 8] = b"HEAPCCDR";
/// Bytes between the raw record start and the compressed stream (plus the low-bit plane).
const COMPRESSED_SKIP: usize = 514;
const MAX_DEPTH: u32 = 16;
const MAX_HEAPS: u32 = 1024;

pub fn probe(data: &[u8]) -> bool {
    data.len() >= 26 && (data.starts_with(b"II") || data.starts_with(b"MM")) && &data[6..14] == SIGNATURE
}

// Standard white-balance indices used by Canon's tables.
const WBI_UNKNOWN: u8 = 0;
const WBI_DAYLIGHT: u8 = 1;
const WBI_FLUORESCENT: u8 = 2;
const WBI_TUNGSTEN: u8 = 3;
const WBI_FLASH: u8 = 4;
const WBI_CLOUDY: u8 = 10;
const WBI_SHADE: u8 = 11;
const WBI_FL_W: u8 = 14;
const WBI_UNDERWATER: u8 = 65;
const WBI_FLUORESCENT_HIGH: u8 = 66;
const WBI_AUTO: u8 = 82;
const WBI_CUSTOM: u8 = 83;
const WBI_AUTO1: u8 = 85;
const WBI_CUSTOM1: u8 = 90;
const WBI_CUSTOM2: u8 = 91;
const WBI_CUSTOM3: u8 = 92;
const WBI_CUSTOM4: u8 = 93;
const WBI_PC_SET1: u8 = 96;
const WBI_PC_SET2: u8 = 97;
const WBI_PC_SET3: u8 = 98;
const WBI_PC_SET4: u8 = 99;
const WBI_PC_SET5: u8 = 100;
const WBI_BW: u8 = 120;
const WBI_KELVIN: u8 = 254;

/// Canon white-balance setting (shot info) → standard index.
const CANON_WBI2STD: [u8; 24] = [
    WBI_AUTO,
    WBI_DAYLIGHT,
    WBI_CLOUDY,
    WBI_TUNGSTEN,
    WBI_FLUORESCENT,
    WBI_FLASH,
    WBI_CUSTOM,
    WBI_BW,
    WBI_SHADE,
    WBI_KELVIN,
    WBI_PC_SET1,
    WBI_PC_SET2,
    WBI_PC_SET3,
    WBI_UNKNOWN,
    WBI_FLUORESCENT_HIGH,
    WBI_CUSTOM1,
    WBI_CUSTOM2,
    WBI_UNDERWATER,
    WBI_CUSTOM3,
    WBI_CUSTOM4,
    WBI_PC_SET4,
    WBI_PC_SET5,
    WBI_UNKNOWN,
    WBI_AUTO1,
];

/// Out-of-range indices (including "no shot info seen", -1) map to Auto.
fn wbi2std(wbi: i32) -> u8 {
    usize::try_from(wbi).ok().and_then(|i| CANON_WBI2STD.get(i).copied()).unwrap_or(WBI_AUTO)
}

/// Line layouts of the ColorInfo1 (0x0032) white-balance tables.
const LINES_KEY0_LEN2048: &[u8] =
    &[WBI_CUSTOM1, WBI_CUSTOM2, WBI_DAYLIGHT, WBI_CLOUDY, WBI_TUNGSTEN, WBI_FLUORESCENT, WBI_UNKNOWN, WBI_FLASH];
const LINES_KEY410_LEN3072: &[u8] = &[
    WBI_CUSTOM1,
    WBI_CUSTOM2,
    WBI_DAYLIGHT,
    WBI_CLOUDY,
    WBI_TUNGSTEN,
    WBI_FLUORESCENT,
    WBI_FLUORESCENT_HIGH,
    WBI_UNKNOWN,
    WBI_UNDERWATER,
    WBI_UNKNOWN,
    WBI_FLASH,
];
const LINES_KEY410_LEN2048: &[u8] = &[
    WBI_CUSTOM1,
    WBI_CUSTOM2,
    WBI_DAYLIGHT,
    WBI_CLOUDY,
    WBI_TUNGSTEN,
    WBI_FLUORESCENT,
    WBI_UNKNOWN,
    WBI_FLASH,
    WBI_UNKNOWN,
    WBI_UNKNOWN,
    WBI_UNKNOWN,
];
const LINES_D30: &[u8] = &[WBI_DAYLIGHT, WBI_CLOUDY, WBI_TUNGSTEN, WBI_FLUORESCENT, WBI_FLASH, WBI_CUSTOM];

/// Index maps from file channel order to `[R, G, B, G2]`.
fn rggb(c: usize) -> usize {
    c ^ (c >> 1)
}
fn grbg(c: usize) -> usize {
    c ^ (c >> 1) ^ 1
}
fn bg2rg1(c: usize) -> usize {
    c ^ 2
}

/// Everything extracted from the CIFF heaps.
#[derive(Debug, Clone)]
struct Ciff {
    order: ByteOrder,
    make: String,
    model: String,
    artist: Option<String>,
    firmware: Option<String>,
    serial: Option<String>,
    /// Image info (0x1810): output image size, pixel aspect, rotation in degrees.
    image_size: (u32, u32),
    pixel_aspect: f32,
    rotation: i32,
    /// Sensor info (0x1031): full sensor size and the default crop `(l, t, r, b)`.
    sensor: Option<(u32, u32)>,
    default_crop: Option<[u16; 4]>,
    /// Decoder table (0x1835).
    table: u32,
    raw_offset: Option<usize>,
    previews: Vec<(usize, usize)>,
    iso: Option<f32>,
    shutter: Option<f32>,
    aperture: Option<f32>,
    focal: Option<f32>,
    ev: Option<f32>,
    timestamp: Option<u32>,
    shot_order: Option<u32>,
    unique_id: Option<u32>,
    color_space: Option<&'static str>,
    /// As-shot multipliers `[R, G, B, G2]` (`[0] <= 0` = unknown).
    cam_mul: [f32; 4],
    /// White-balance presets by standard index.
    presets: Vec<(u8, [f32; 4])>,
    /// Per-channel black from ColorBalance (0x10a9), `[R, G, B, G2]`.
    channel_black: Option<[u16; 4]>,
    heaps: u32,
}

/// Per-heap white-balance parsing state (local to each heap: a sub-heap does not inherit it).
struct HeapWb {
    wbi: i32,
    appendix_a: bool,
    got_as_shot: bool,
    table_offset: i64,
}

impl Ciff {
    fn new(order: ByteOrder) -> Self {
        Ciff {
            order,
            make: String::new(),
            model: String::new(),
            artist: None,
            firmware: None,
            serial: None,
            image_size: (0, 0),
            pixel_aspect: 1.0,
            rotation: 0,
            sensor: None,
            default_crop: None,
            table: 0,
            raw_offset: None,
            previews: Vec::new(),
            iso: None,
            shutter: None,
            aperture: None,
            focal: None,
            ev: None,
            timestamp: None,
            shot_order: None,
            unique_id: None,
            color_space: None,
            cam_mul: [0.0, 1.0, 0.0, 0.0],
            presets: Vec::new(),
            channel_black: None,
            heaps: 0,
        }
    }

    fn preset(&self, idx: u8) -> [f32; 4] {
        self.presets.iter().find(|(i, _)| *i == idx).map(|(_, v)| *v).unwrap_or([0.0; 4])
    }
    fn set_preset(&mut self, idx: u8, v: [f32; 4]) {
        if let Some(p) = self.presets.iter_mut().find(|(i, _)| *i == idx) {
            p.1 = v;
        } else {
            self.presets.push((idx, v));
        }
    }

    fn cursor<'a>(&self, data: &'a [u8], pos: usize) -> Cursor<'a> {
        Cursor::at(data, pos, self.order)
    }

    /// Parse the heap at `off..off + len`.
    fn parse_heap(&mut self, data: &[u8], off: usize, len: usize, depth: u32) -> Result<()> {
        self.heaps += 1;
        if self.heaps > MAX_HEAPS {
            return Err(Error::corrupt("CIFF: too many heaps"));
        }
        if depth > MAX_DEPTH || len < 4 {
            return Ok(());
        }
        let end =
            off.checked_add(len).filter(|&e| e <= data.len()).ok_or_else(|| Error::corrupt("CIFF heap beyond EOF"))?;
        let table = off.saturating_add(self.cursor(data, end - 4).u32()? as usize);
        let mut c = self.cursor(data, table);
        let nrecs = c.u16()? as usize;
        // Plausibility limits: reject absurd record counts, nesting depth and overruns.
        if nrecs == 0 || (nrecs as u32 | depth) > 127 || off + nrecs * 10 > data.len() {
            return Ok(());
        }
        let mut wb = HeapWb { wbi: -1, appendix_a: false, got_as_shot: false, table_offset: 0 };
        for _ in 0..nrecs {
            let Ok(typ) = c.u16() else { break };
            let Ok(rlen) = c.u32() else { break };
            let Ok(roff) = c.u32() else { break };
            let see = off.saturating_add(roff as usize);
            let in_record = typ >> 14 == 1;
            if !in_record {
                if see >= data.len() {
                    continue;
                }
                if (((typ >> 8) + 8) | 8) == 0x38 {
                    self.parse_heap(data, see, (rlen as usize).min(data.len() - see), depth + 1)?;
                    continue;
                }
            }
            // A malformed record only loses that record.
            let _ = self.record(data, typ, rlen, see, &mut wb);
        }
        Ok(())
    }

    fn record(&mut self, data: &[u8], typ: u16, len: u32, see: usize, wb: &mut HeapWb) -> Result<()> {
        let mut c = self.cursor(data, see);
        match typ {
            0x0810 => self.artist = Some(ascii_field(slice_upto(data, see, 64))).filter(|s| !s.is_empty()),
            0x080a => {
                let field = slice_upto(data, see, 64);
                let mlen = field.iter().take(63).position(|&b| b == 0).unwrap_or(63);
                self.make = ascii_field(&field[..mlen]);
                self.model = ascii_field(slice_upto(data, see + mlen + 1, 63));
            }
            0x080b => {
                let s = ascii_field(slice_upto(data, see, (len as usize).min(64)));
                let s = match s.get(..16) {
                    Some(p) if p.eq_ignore_ascii_case("Firmware Version") => s[16..].trim().to_string(),
                    _ => s,
                };
                self.firmware = Some(s).filter(|s| !s.is_empty());
            }
            0x1810 => {
                let (w, h) = (c.u32()?, c.u32()?);
                let aspect = f32::from_bits(c.u32()?);
                let rot = c.u32()? as i32;
                self.image_size = (w, h);
                self.pixel_aspect = aspect;
                self.rotation = rot;
            }
            0x1835 => self.table = c.u32()?,
            0x2005 => self.raw_offset = Some(see),
            0x2007 | 0x2008 => self.previews.push((see, len as usize)),
            0x1818 => {
                c.skip(4)?;
                let tv = f32::from_bits(c.u32()?);
                let av = f32::from_bits(c.u32()?);
                self.shutter = Some(2f32.powf(-tv));
                self.aperture = Some(2f32.powf(av / 2.0));
            }
            0x102a => self.shot_info(&mut c, wb)?,
            0x102c => {
                let kind = c.u16()?;
                if kind > 512 {
                    // Pro90 IS, G1.
                    c.skip(118)?;
                    for i in 0..4 {
                        self.cam_mul[bg2rg1(i)] = c.u16()? as f32;
                    }
                } else if kind != 276 {
                    // G2, S30, S40 ("Appendix A" layout).
                    wb.appendix_a = true;
                    wb.table_offset = -14;
                    c.skip(98)?;
                    for i in 0..4 {
                        self.cam_mul[grbg(i)] = c.u16()? as f32;
                    }
                    if self.cam_mul[0] > 0.001 {
                        wb.got_as_shot = true;
                    }
                }
            }
            0x10a9 => self.color_balance(&mut c, wb)?,
            0x10b4 => {
                self.color_space = match c.u16()? {
                    1 => Some("sRGB"),
                    2 => Some("AdobeRGB"),
                    _ => None,
                }
            }
            0x580b => {
                self.serial = Some(if self.model.eq_ignore_ascii_case("Canon EOS D30") {
                    format!("{:x}-{:05}", len >> 16, len & 0xffff)
                } else {
                    len.to_string()
                })
            }
            0x0032 => self.color_info1(data, see, len, wb)?,
            0x1031 => {
                c.skip(2)?;
                let (w, h) = (c.u16()?, c.u16()?);
                c.skip(4)?;
                let crop = [c.u16()?, c.u16()?, c.u16()?, c.u16()?];
                self.sensor = Some((w as u32, h as u32));
                self.default_crop = Some(crop);
            }
            0x501c => self.iso = Some((len & 0xffff) as f32),
            0x5029 => {
                let mut f = (len >> 16) as f32;
                if len & 0xffff == 2 {
                    // Zoom lens: focal length in 1/32 mm.
                    f /= 32.0;
                }
                self.focal = Some(f);
            }
            0x5814 => self.ev = Some(f32::from_bits(len)),
            0x5817 => self.shot_order = Some(len),
            0x5834 => self.unique_id = Some(len),
            0x580e => self.timestamp = Some(len),
            0x180e => self.timestamp = Some(c.u32()?),
            _ => {}
        }
        Ok(())
    }

    /// Canon shot info (0x102a): ISO, aperture, shutter and the white-balance setting.
    fn shot_info(&mut self, c: &mut Cursor, wb: &mut HeapWb) -> Result<()> {
        let v: Vec<u16> = (0..8).map(|_| c.u16()).collect::<Result<_>>()?;
        self.iso = Some(2f32.powf((v[1] as f32 + v[2] as f32) / 32.0 - 5.0) * 100.0);
        self.aperture = (v[4] != 0xffe0 && v[4] != 0x7fff).then(|| 2f32.powf(v[4] as f32 / 64.0));
        let mut shutter = 2f32.powf(-(v[5] as i16 as f32) / 32.0);
        wb.wbi = v[7] as i32;
        if wb.wbi as usize >= CANON_WBI2STD.len() {
            wb.wbi = 0;
        }
        if shutter > 1e6 {
            c.skip(32)?;
            shutter = c.u16()? as f32 / 10.0;
        }
        self.shutter = Some(shutter);
        Ok(())
    }

    /// ColorBalance (0x10a9): EOS D60/10D/300D presets, as-shot by WB setting, channel blacks.
    fn color_balance(&mut self, c: &mut Cursor, wb: &HeapWb) -> Result<()> {
        let n_wb = (c.u16()? as i32 - 2) / 8 - 1;
        let read4 = |c: &mut Cursor| -> Result<[f32; 4]> {
            let mut v = [0f32; 4];
            for i in 0..4 {
                v[rggb(i)] = c.u16()? as f32;
            }
            Ok(v)
        };
        if n_wb != 0 {
            let auto = read4(c)?;
            self.set_preset(WBI_AUTO, auto);
        }
        if n_wb >= 7 {
            for idx in [WBI_DAYLIGHT, WBI_SHADE, WBI_CLOUDY, WBI_TUNGSTEN, WBI_FL_W, WBI_FLASH] {
                let v = read4(c)?;
                self.set_preset(idx, v);
            }
        } else {
            self.cam_mul = self.preset(WBI_AUTO);
        }
        if n_wb == 7 {
            // EOS D60 and some 300D firmware.
            self.cam_mul = if (0..9).contains(&wb.wbi) && wb.wbi != 6 {
                self.preset(wbi2std(wb.wbi))
            } else {
                self.preset(WBI_AUTO)
            };
        } else if n_wb == 9 {
            // EOS 10D, 300D.
            let custom = read4(c)?;
            self.set_preset(WBI_CUSTOM, custom);
            let kelvin = read4(c)?;
            self.set_preset(WBI_KELVIN, kelvin);
            self.cam_mul = if (0..10).contains(&wb.wbi) { self.preset(wbi2std(wb.wbi)) } else { self.preset(WBI_AUTO) };
        }
        let mut bl = [0u16; 4];
        for i in 0..4 {
            bl[rggb(i)] = c.u16()?;
        }
        self.channel_black = Some(bl);
        Ok(())
    }

    /// ColorInfo1 (0x0032): white-balance tables of the PowerShots and the EOS D30.
    fn color_info1(&mut self, data: &[u8], see: usize, len: u32, wb: &mut HeapWb) -> Result<()> {
        let mut c = self.cursor(data, see);
        if len == 768 {
            // EOS D30: reciprocal coefficients.
            c.skip(4)?;
            for &idx in LINES_D30 {
                let mut v = [0f32; 4];
                for i in 0..4 {
                    v[rggb(i)] = (1_024_000.0 / c.u16()?.max(1) as f32).round();
                }
                self.set_preset(idx, v);
            }
            c.skip(68 - LINES_D30.len() * 8)?;
            for i in 0..4 {
                self.cam_mul[rggb(i)] = 1024.0 / c.u16()?.max(1) as f32;
            }
            if wb.wbi == 0 {
                // Camera was on auto WB: the table holds no usable as-shot value, so report none.
                self.cam_mul[0] = -1.0;
            }
            return Ok(());
        }
        if !(self.cam_mul[0] <= 0.001 || wb.appendix_a) {
            return Ok(());
        }
        let key0 = c.u16()?;
        let mut key = [0x410u16, 0x45f3];
        let mut use_table = true;
        let lines = if key0 == key[0] && len == 2048 {
            wb.table_offset = 8; // Pro1
            LINES_KEY410_LEN2048
        } else if key0 == key[0] && len == 3072 {
            wb.table_offset = 16; // G6, S60, S70
            LINES_KEY410_LEN3072
        } else if key0 == 0 && len == 2048 {
            // G2, S30, S40, G3, G5, S45, S50: unencrypted; old firmware stores no as-shot line.
            key = [0, 0];
            if leading_float(self.firmware.as_deref().unwrap_or("")) < 1.02 {
                use_table = false;
            }
            LINES_KEY0_LEN2048
        } else {
            return Ok(());
        };
        let std = wbi2std(wb.wbi);
        if std == WBI_AUTO || std == WBI_UNKNOWN || wb.got_as_shot {
            use_table = false;
        }
        let mut as_shot_line = lines.len();
        if use_table {
            let wanted = if std == WBI_CUSTOM { wbi2std(WBI_DAYLIGHT as i32) } else { std };
            as_shot_line = lines.iter().position(|&l| l == wanted).unwrap_or(lines.len());
        }
        let start = (see as i64 + 2 + 78 + wb.table_offset).max(0) as usize;
        let mut c = self.cursor(data, start);
        for (n, &idx) in lines.iter().enumerate() {
            if idx == WBI_UNKNOWN {
                c.skip(8)?;
                continue;
            }
            let mut v = [0f32; 4];
            for i in 0..4 {
                v[grbg(i)] = (c.u16()? ^ key[i & 1]) as f32;
            }
            self.set_preset(idx, v);
            if use_table && as_shot_line == n {
                self.cam_mul = v;
                wb.got_as_shot = true;
            }
        }
        if !wb.got_as_shot {
            self.cam_mul[0] = -1.0;
        }
        Ok(())
    }
}

/// `data[pos..pos + len]` clamped to the file.
fn slice_upto(data: &[u8], pos: usize, len: usize) -> &[u8] {
    let s = data.get(pos..).unwrap_or(&[]);
    &s[..len.min(s.len())]
}

/// `atof`-style parse of the leading number of a string ("1.0.2.0" → 1.0).
fn leading_float(s: &str) -> f32 {
    let s = s.trim_start();
    let mut end = 0;
    let mut dot = false;
    for (i, ch) in s.char_indices() {
        if ch.is_ascii_digit() || (i == 0 && (ch == '+' || ch == '-')) {
            end = i + 1;
        } else if ch == '.' && !dot {
            dot = true;
            end = i + 1;
        } else {
            break;
        }
    }
    s[..end].trim_end_matches('.').parse().unwrap_or(0.0)
}

/// Geometry of the CRW-era sensors, keyed by the full sensor size: `raw_w, raw_h, left, top, right trim, bottom trim, and the
/// left/right masked-column adjustments`.
struct SensorGeom {
    raw: (u32, u32),
    left: u32,
    top: u32,
    trim: (u32, u32),
    /// `(left mask start, left mask end trim, right mask start, right mask end trim)`.
    mask: (u32, u32, u32, u32),
}

const SENSORS: &[SensorGeom] = &[
    // PowerShot Pro90 IS
    SensorGeom { raw: (1944, 1416), left: 0, top: 0, trim: (48, 0), mask: (0, 0, 0, 0) },
    // PowerShot S30, G1
    SensorGeom { raw: (2144, 1560), left: 4, top: 8, trim: (52, 2), mask: (0, 0, 0, 25) },
    // EOS D30
    SensorGeom { raw: (2224, 1456), left: 48, top: 6, trim: (0, 2), mask: (0, 0, 0, 0) },
    // PowerShot G2, S40, G3, S45
    SensorGeom { raw: (2376, 1728), left: 12, top: 6, trim: (52, 2), mask: (0, 0, 0, 0) },
    // PowerShot G5, S50, S60
    SensorGeom { raw: (2672, 1968), left: 12, top: 6, trim: (44, 2), mask: (0, 0, 0, 0) },
    // EOS D60, 10D, 300D
    SensorGeom { raw: (3152, 2068), left: 64, top: 12, trim: (0, 0), mask: (16, 0, 0, 0) },
    // PowerShot G6, S70
    SensorGeom { raw: (3160, 2344), left: 44, top: 12, trim: (4, 4), mask: (0, 0, 0, 0) },
    // PowerShot Pro1
    SensorGeom { raw: (3344, 2484), left: 4, top: 6, trim: (52, 6), mask: (0, 0, 0, 0) },
];

/// How the payload is stored.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Payload {
    /// Huffman blocks; `lowbits`: a 2-bit low plane precedes the stream.
    Compressed { lowbits: bool },
    /// 10-bit samples in MSB-first 16-bit little-endian words (A5, A50, Pro70).
    Packed10,
    /// PowerShot 600: 10-byte groups of 8 samples, even rows first.
    Canon600,
}

/// 32-bit CFA descriptor (`filters`, two bits per cell over 8 rows x 2 columns) + colour letters (at the
/// visible-area origin) → a pattern
/// anchored at the buffer origin.
fn cfa_from_filters(filters: u32, desc: [CfaColor; 4], left: u32, top: u32) -> Option<CfaPattern> {
    let fc = |row: usize, col: usize| (filters >> ((((row << 1) & 14) | (col & 1)) << 1)) & 3;
    let two_rows = (0..8).all(|r| (0..2).all(|c| fc(r, c) == fc(r % 2, c)));
    let ph = if two_rows { 2 } else { 8 };
    let mut colors = Vec::with_capacity(ph * 2);
    for y in 0..ph {
        for x in 0..2 {
            let r = (y + ph - top as usize % ph) % ph;
            let c = (x + 2 - left as usize % 2) % 2;
            colors.push(desc[fc(r, c) as usize]);
        }
    }
    CfaPattern::new(2, ph, &colors)
}

/// Seconds since 1970 → `YYYY-MM-DDTHH:MM:SS` (camera local time, no zone).
fn format_timestamp(t: u32) -> String {
    let days = (t / 86_400) as i64;
    let secs = t % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

pub fn open<'a>(data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if !probe(data) {
        return Err(Error::UnsupportedFormat("not a CIFF file".into()));
    }
    let order = if data.starts_with(b"II") { ByteOrder::Little } else { ByteOrder::Big };
    let hlen = Cursor::at(data, 2, order).u32()? as usize;
    if hlen < 14 || hlen >= data.len() {
        return Err(Error::corrupt("CIFF header length out of range"));
    }
    let mut ciff = Ciff::new(order);
    ciff.parse_heap(data, hlen, data.len() - hlen, 0)?;
    if ciff.make.is_empty() && ciff.model.is_empty() {
        return Err(Error::invalid_metadata("CIFF without make/model record"));
    }

    let mut meta = RawMetadata::new(ciff.make.clone(), ciff.model.clone());
    let cam = db_lookup(&mut meta, "");
    let short_model = strip_make_prefix(&ciff.model, "Canon");
    let model_is = |names: &[&str]| names.iter().any(|n| short_model.eq_ignore_ascii_case(n));

    // ---- Geometry ----
    let (mut raw_w, mut raw_h) = ciff.sensor.unwrap_or((0, 0));
    let (mut width, mut height) = ciff.image_size;
    let (mut left, mut top) = (0u32, 0u32);
    let mut mask = (0u32, 0u32, 0u32, 0u32);
    let mut from_table = false;
    if let Some(g) = SENSORS.iter().find(|g| g.raw == (raw_w, raw_h)) {
        left = g.left;
        top = g.top;
        width = raw_w - g.left - g.trim.0;
        height = raw_h - g.top - g.trim.1;
        mask = g.mask;
        from_table = true;
    }
    // Unknown sensor with per-channel blacks in the metadata: use the camera's crop.
    let metadata_black = (!from_table)
        .then_some(ciff.channel_black)
        .flatten()
        .filter(|b| b.iter().map(|&v| v as u32).sum::<u32>() / 4 != 0);
    if metadata_black.is_some() && ciff.sensor == Some((raw_w, raw_h)) {
        if let Some([l, t, r, b]) = ciff.default_crop {
            left = (l as u32 + 1) & !1;
            top = (t as u32 + 1) & !1;
            width = (r as u32).saturating_sub(left);
            height = (b as u32).saturating_sub(top);
        }
    }

    let mut payload = Payload::Compressed { lowbits: false };
    let mut filters = 0x9494_9494u32;
    let mut desc = [CfaColor::Red, CfaColor::Green, CfaColor::Blue, CfaColor::Green];
    let cmyg = [CfaColor::Green, CfaColor::Magenta, CfaColor::Cyan, CfaColor::Yellow];
    let mut pixel_aspect = ciff.pixel_aspect;
    if model_is(&["PowerShot 600"]) {
        (height, width, raw_w) = (613, 854, 896);
        filters = 0xe1e4_e1e4;
        desc = cmyg;
        payload = Payload::Canon600;
    } else if model_is(&["PowerShot A5", "PowerShot A5 Zoom"]) {
        (height, width, raw_w) = (773, 960, 992);
        pixel_aspect = 256.0 / 235.0;
        filters = 0x1e4e_1e4e;
        desc = cmyg;
        payload = Payload::Packed10;
    } else if model_is(&["PowerShot A50"]) {
        (height, width, raw_w) = (968, 1290, 1320);
        filters = 0x1b4e_4b1e;
        desc = cmyg;
        payload = Payload::Packed10;
    } else if model_is(&["PowerShot Pro70"]) {
        (height, width) = (1024, 1552);
        filters = 0x1e4b_4e1b;
        desc = cmyg;
        payload = Payload::Packed10;
    } else if model_is(&["PowerShot Pro90 IS", "PowerShot G1"]) {
        filters = 0xb4b4_b4b4;
        desc = cmyg;
    }
    raw_w = raw_w.max(width);
    raw_h = raw_h.max(height);
    if !(22..=64000).contains(&raw_w) || !(22..=64000).contains(&raw_h) || width < 22 || height < 22 {
        return Err(Error::invalid_metadata(format!("CRW: implausible dimensions {raw_w}x{raw_h} / {width}x{height}")));
    }
    if left >= raw_w || top >= raw_h {
        return Err(Error::invalid_metadata("CRW: margins outside the sensor"));
    }
    if !(0.1..=10.0).contains(&pixel_aspect) {
        pixel_aspect = 1.0;
    }

    let raw_off = ciff.raw_offset.unwrap_or(hlen);
    let total = raw_w as usize * raw_h as usize;
    if let Payload::Compressed { .. } = payload {
        payload = Payload::Compressed { lowbits: canon_crw::has_lowbits(data) };
    }
    let bits: u8 = if payload == (Payload::Compressed { lowbits: true }) { 12 } else { 10 };

    // ---- Levels ----
    // Colour/level table, by canonical model; regional aliases (e.g.
    // "EOS Digital Rebel") fall back to the base model's entry ("EOS 300D").
    let matrix = flashraw_camera_db::find_matrix(&meta.canonical_make, &meta.canonical_model)
        .or_else(|| cam.and_then(|c| flashraw_camera_db::find_matrix(c.canonical_make, c.canonical_model)));
    let mut info = FrameInfo::new(raw_w, raw_h, bits);
    info.active_area = Rect::new(left, top, width.min(raw_w - left), height.min(raw_h - top));
    info.crop = info.active_area;
    info.pixel_aspect = pixel_aspect;
    info.compression = match payload {
        Payload::Compressed { .. } => Compression::CanonCrw,
        _ => Compression::Uncompressed,
    };
    let table_white = matrix.map(|m| m.white).filter(|&w| w > 0).map(|w| w as u32);
    info.white_level = WhiteLevel::uniform(match bits {
        12 => table_white.unwrap_or(4095),
        _ => 1023,
    });
    let table_black = matrix.map(|m| m.black.max(0) as f32).unwrap_or(0.0);
    info.black_level = BlackLevel::uniform(table_black);
    let cfa = cfa_from_filters(filters, desc, left, top).ok_or_else(|| Error::invalid_metadata("CRW: bad CFA"))?;
    info.layout = layout_for(&cfa);
    info.cfa = Some(cfa);

    let (aa_top, aa_bottom) = (top, top + info.active_area.height);
    let mut masks = Vec::new();
    let mut masked = |x0: u32, x1: u32| {
        if x1 > x0 && aa_bottom > aa_top {
            masks.push(Rect::new(x0, aa_top, x1 - x0, aa_bottom - aa_top));
        }
    };
    match payload {
        Payload::Compressed { .. } => {
            if let Some(b) = metadata_black {
                // Per-channel black from ColorBalance, anchored at the visible origin (RGGB).
                let at = |x: u32, y: u32| {
                    let (vx, vy) = ((x + left) % 2, (y + top) % 2);
                    let v = match (vy, vx) {
                        (0, 0) => b[0],
                        (0, 1) => b[1],
                        (1, 0) => b[3],
                        _ => b[2],
                    };
                    v as f32
                };
                info.black_level = BlackLevel::pattern2x2([at(0, 0), at(1, 0), at(0, 1), at(1, 1)]);
            } else {
                // Optical-black columns left and right of the image, two columns in from
                // each edge.
                // The table's first mask value is superseded by the second.
                let (_, m03, m11, m13) = mask;
                let start = m11 + 2;
                masked(start, left.saturating_sub(m03 + 2));
                masked(start + left + width, raw_w.saturating_sub(m13));
            }
        }
        Payload::Packed10 => masked(left + width, raw_w),
        Payload::Canon600 => {}
    }
    info.masked_areas = masks;
    info.black_from_masked = !info.masked_areas.is_empty();
    if let Payload::Canon600 = payload {
        if let Some(b) = canon600_black(data, raw_off, raw_w, height, width) {
            info.black_level = BlackLevel::uniform(b as f32);
        }
    }

    // ---- White balance (`cam_mul[0] <= 0` = unknown) ----
    let colors = if desc == cmyg { 4 } else { 3 };
    let m = ciff.cam_mul;
    let wb_ok = m[..colors].iter().all(|&v| v > 0.001) && {
        let mn = m[..colors].iter().copied().fold(f32::MAX, f32::min);
        let n: Vec<f32> = m[..colors].iter().map(|v| v / mn).collect();
        let (lo, hi) = n.iter().fold((f32::MAX, 0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        lo > 0.01 && hi <= 100.0
    };
    if wb_ok {
        info.white_balance = WhiteBalance::from_multipliers(m[0], m[1], m[2], m[3]);
    }

    // ---- Colour ----
    if let Some(mx) = matrix.filter(|m| !m.color_matrix.is_empty()) {
        let rows = mx.color_matrix.len() / 3;
        if let Some(cm) =
            ColorMatrix::new(Illuminant::D65, rows, mx.color_matrix.iter().map(|&v| v as f32 / 10000.0).collect())
        {
            info.color_matrices.color.push(cm);
            info.color_matrices.source = ColorSource::CameraDb;
        }
    }
    if info.color_matrices.color.is_empty() {
        info.color_matrices = db_color(&meta, cam);
    }

    info.orientation = match (ciff.rotation % 360 + 360) % 360 {
        90 => Orientation::Rotate90,
        180 => Orientation::Rotate180,
        270 => Orientation::Rotate270,
        _ => Orientation::Normal,
    };

    // ---- Metadata ----
    meta.artist = ciff.artist.clone();
    meta.firmware = ciff.firmware.clone();
    meta.serial = ciff.serial.clone();
    meta.exposure.iso = ciff.iso.filter(|v| v.is_finite() && *v > 0.0).map(|v| v.round() as u32);
    meta.exposure.exposure_time = ciff.shutter.filter(|v| v.is_finite() && *v > 0.0).map(|v| v as f64);
    meta.exposure.f_number = ciff.aperture.filter(|v| v.is_finite() && *v > 0.0);
    meta.exposure.focal_length = ciff.focal.filter(|v| *v > 0.0);
    meta.exposure.exposure_bias = ciff.ev.filter(|v| v.is_finite());
    meta.capture_time = ciff.timestamp.filter(|&t| t != 0).map(format_timestamp);
    meta.orientation = info.orientation;
    if let Some(s) = ciff.shot_order {
        meta.set_extra("shot_order", s);
    }
    if let Some(u) = ciff.unique_id {
        meta.set_extra("canon_model_id", format!("0x{u:08x}"));
    }
    if let Some(cs) = ciff.color_space {
        meta.set_extra("color_space", cs);
    }
    meta.set_extra("crw_table", ciff.table);
    meta.set_extra(
        "mode",
        match payload {
            Payload::Compressed { lowbits: true } => "compressed-12bit",
            Payload::Compressed { lowbits: false } => "compressed-10bit",
            Payload::Packed10 => "packed-10bit",
            Payload::Canon600 => "canon600-10bit",
        },
    );
    for &(o, l) in &ciff.previews {
        push_jpeg(data, o, l, info.orientation, &mut meta.previews);
    }
    classify_previews(&mut meta.previews);

    // ---- Payload ----
    let (w, h) = (raw_w as usize, raw_h as usize);
    let dec = match payload {
        Payload::Compressed { lowbits } => {
            let low_len = if lowbits { total / 4 } else { 0 };
            // Data past the end of the file fails when decoded (metadata and previews stay
            // available).
            let start = raw_off.saturating_add(COMPRESSED_SKIP + low_len);
            let stream = data.get(start..).unwrap_or(&[]);
            let low = lowbits.then(|| slice_upto(data, raw_off, low_len));
            let params = canon_crw::CrwParams {
                width: w,
                height: h,
                table: ciff.table,
                lowbits: low,
                low_offset_quirk: raw_w == 2672,
            };
            let unit = DecodeUnit {
                rect: Rect::full(raw_w, raw_h),
                offset: raw_off as u64,
                length: data.len().saturating_sub(raw_off) as u64,
            };
            let mut d = SimpleDecoder::new(
                RawFormat::Crw,
                meta,
                info,
                Box::new(move |ctx: &DecodeContext| {
                    // One entropy-coded stream (its length is not stored): fails under
                    // either corruption policy; data starting past the end of the file is
                    // reported as truncation.
                    if stream.is_empty() {
                        let t = Truncation::new(DataUnit::Payload, start as u64, 1, data.len() as u64);
                        return Err(Error::truncated(t));
                    }
                    let mut img = RawImage::new_u16(raw_w, raw_h, 1, &ctx.limits)?;
                    ctx.profiler.time(Stage::EntropyDecode, || canon_crw::decode(stream, &params, img.u16_mut()))?;
                    Ok(img)
                }),
            );
            d.units = vec![unit];
            d
        }
        Payload::Packed10 => {
            let layout = PackedLayout::tight(w, h, 10, PackOrder::Msb16);
            let src = slice_upto(data, raw_off, layout.row_stride * h);
            let mut d = SimpleDecoder::new(
                RawFormat::Crw,
                meta,
                info,
                Box::new(move |ctx: &DecodeContext| {
                    let geometry = RowGeometry::packed(h, w, 10, layout.row_stride);
                    let src = ctx.sensor_rows(data, DataUnit::Payload, raw_off, geometry)?;
                    let mut img = RawImage::new_u16(raw_w, raw_h, 1, &ctx.limits)?;
                    ctx.profiler.time(Stage::Unpack, || {
                        packed::unpack(src, layout, img.u16_mut(), w, ctx.parallel, ctx.simd, ctx.recover)
                    })?;
                    Ok(img)
                }),
            );
            d.random_access = RandomAccess::Rows;
            d.packed = Some(PackedPayload {
                data: src,
                width: raw_w,
                height: raw_h,
                bits: 10,
                order: PackOrder::Msb16,
                row_stride: layout.row_stride,
            });
            d
        }
        Payload::Canon600 => {
            let rows = height as usize;
            let mut d = SimpleDecoder::new(
                RawFormat::Crw,
                meta,
                info,
                Box::new(move |ctx: &DecodeContext| {
                    // Rows are whole 10-byte groups; a missing row stays zero when recovering.
                    let geometry =
                        RowGeometry { rows, stride: CANON600_ROW_BYTES, row_bytes: CANON600_ROW_BYTES, samples: w };
                    let src = ctx.sensor_rows(data, DataUnit::Payload, raw_off, geometry)?;
                    let mut img = RawImage::new_u16(raw_w, raw_h, 1, &ctx.limits)?;
                    ctx.profiler.time(Stage::Unpack, || canon600_unpack(src, w, rows, img.u16_mut()))?;
                    Ok(img)
                }),
            );
            d.random_access = RandomAccess::Rows;
            d
        }
    };
    Ok(Box::new(dec))
}

const CANON600_ROW_BYTES: usize = 1120;

/// Buffer row of stored row `irow`: even rows first, then odd rows.
fn canon600_row(irow: usize, rows: usize) -> usize {
    let even = rows.div_ceil(2);
    if irow < even { irow * 2 } else { (irow - even) * 2 + 1 }
}

/// Unpack one PowerShot 600 row: groups of 10 bytes hold 8 samples (high 8 bits in
/// bytes 0, 2..=8; low bit pairs in bytes 1 and 9).
fn canon600_row_samples(src: &[u8], dst: &mut [u16]) {
    for (g, o) in src.chunks_exact(10).zip(dst.chunks_exact_mut(8)) {
        let (lo1, lo2) = (g[1] as u16, g[9] as u16);
        o[0] = ((g[0] as u16) << 2) | (lo1 >> 6);
        o[1] = ((g[2] as u16) << 2) | ((lo1 >> 4) & 3);
        o[2] = ((g[3] as u16) << 2) | ((lo1 >> 2) & 3);
        o[3] = ((g[4] as u16) << 2) | (lo1 & 3);
        o[4] = ((g[5] as u16) << 2) | (lo2 & 3);
        o[5] = ((g[6] as u16) << 2) | ((lo2 >> 2) & 3);
        o[6] = ((g[7] as u16) << 2) | ((lo2 >> 4) & 3);
        o[7] = ((g[8] as u16) << 2) | (lo2 >> 6);
    }
}

/// Unpack the rows present in `src` (the caller checks the range; rows missing from a
/// recovered truncated payload stay zero).
fn canon600_unpack(src: &[u8], w: usize, rows: usize, out: &mut [u16]) -> Result<()> {
    if w * 10 / 8 < CANON600_ROW_BYTES {
        return Err(Error::corrupt("PowerShot 600 row narrower than its data"));
    }
    for (irow, s) in src.chunks_exact(CANON600_ROW_BYTES).take(rows).enumerate() {
        let row = canon600_row(irow, rows);
        if let Some(dst) = out.get_mut(row * w..(row + 1) * w) {
            canon600_row_samples(s, dst);
        }
    }
    Ok(())
}

/// PowerShot 600 black: mean of the masked columns right of the image, minus 4 (a fixed
/// calibration offset for this sensor).
fn canon600_black(data: &[u8], raw_off: usize, raw_w: u32, rows: u32, width: u32) -> Option<u32> {
    let (w, rows) = (raw_w as usize, rows as usize);
    let src = data.get(raw_off..raw_off.checked_add(rows * CANON600_ROW_BYTES)?)?;
    let mut line = vec![0u16; w];
    let (mut sum, mut n) = (0u64, 0u64);
    for s in src.chunks_exact(CANON600_ROW_BYTES) {
        canon600_row_samples(s, &mut line);
        for &v in &line[width as usize..] {
            sum += v as u64;
            n += 1;
        }
    }
    (n > 0).then(|| ((sum / n) as u32).saturating_sub(4))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a little-endian CIFF heap from `(type, payload)` records.
    fn heap(records: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut table = Vec::new();
        for (typ, payload) in records {
            table.extend_from_slice(&typ.to_le_bytes());
            table.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            table.extend_from_slice(&(body.len() as u32).to_le_bytes());
            body.extend_from_slice(payload);
        }
        let toff = body.len() as u32;
        body.extend_from_slice(&(records.len() as u16).to_le_bytes());
        body.extend_from_slice(&table);
        body.extend_from_slice(&toff.to_le_bytes());
        body
    }

    fn file(records: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut f = b"II".to_vec();
        f.extend_from_slice(&26u32.to_le_bytes());
        f.extend_from_slice(b"HEAPCCDR");
        f.extend_from_slice(&[0x02, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        f.extend_from_slice(&heap(records));
        f
    }

    fn le32(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }
    fn le16(v: &[u16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn probe_signature() {
        let f = file(&[]);
        assert!(probe(&f));
        let mut g = f.clone();
        g[6] = b'X';
        assert!(!probe(&g));
        assert!(!probe(b"II*\0"));
    }

    #[test]
    fn parses_nested_heaps_and_values() {
        let mut name = b"Canon\0Canon PowerShot G9000\0".to_vec();
        name.resize(32, 0);
        let sub = heap(&[(0x080a, name), (0x1810, le32(&[1600, 1200, 1.0f32.to_bits(), 90]))]);
        let mut shot = le16(&[68, 0, 128, 0, 128, 0x60, 0, 1]);
        shot.resize(68, 0);
        let exif = heap(&[
            (0x102a, shot),
            (0x1835, le32(&[2, 0, 0, 0])),
            (0x1031, le16(&[34, 2400, 1800, 1, 1, 10, 6, 2390, 1795])),
        ]);
        let f = file(&[(0x2005, vec![0; 64]), (0x300a, sub), (0x300b, exif)]);
        let mut ciff = Ciff::new(ByteOrder::Little);
        ciff.parse_heap(&f, 26, f.len() - 26, 0).unwrap();
        assert_eq!(ciff.make, "Canon");
        assert_eq!(ciff.model, "Canon PowerShot G9000");
        assert_eq!(ciff.image_size, (1600, 1200));
        assert_eq!(ciff.rotation, 90);
        assert_eq!(ciff.table, 2);
        assert_eq!(ciff.sensor, Some((2400, 1800)));
        assert_eq!(ciff.default_crop, Some([10, 6, 2390, 1795]));
        assert_eq!(ciff.raw_offset, Some(26));
        assert_eq!(ciff.iso.map(|v| v.round() as u32), Some(50));
        assert!((ciff.aperture.unwrap() - 4.0).abs() < 1e-4);
        assert!((ciff.shutter.unwrap() - 2f32.powf(-3.0)).abs() < 1e-6);
    }

    #[test]
    fn value_in_record_tags() {
        // Type 0x5029 (bit 14 set): value in the length field (focal 0x146/32 mm, zoom lens).
        let mut f = b"II".to_vec();
        f.extend_from_slice(&26u32.to_le_bytes());
        f.extend_from_slice(b"HEAPCCDR");
        f.extend_from_slice(&[0; 12]);
        let mut body = vec![];
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&0x5029u16.to_le_bytes());
        body.extend_from_slice(&0x0146_0002u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(&body);
        let mut ciff = Ciff::new(ByteOrder::Little);
        ciff.parse_heap(&f, 26, f.len() - 26, 0).unwrap();
        assert_eq!(ciff.focal, Some(326.0 / 32.0));
    }

    #[test]
    fn malformed_heaps_do_not_panic() {
        let base = file(&[(0x300a, heap(&[(0x300b, heap(&[(0x1810, le32(&[1, 2, 3, 4]))]))]))]);
        for i in 0..base.len() {
            for v in [0u8, 0xff, 0x30, 0x7f] {
                let mut f = base.clone();
                f[i] = v;
                let _ = open(&f, &OpenOptions::default());
                let mut ciff = Ciff::new(ByteOrder::Little);
                let _ = ciff.parse_heap(&f, 26, f.len() - 26, 0);
            }
        }
        for n in 0..base.len() {
            let _ = open(&base[..n], &OpenOptions::default());
        }
        // A heap that contains itself: bounded by the depth/heap limits.
        let mut f = file(&[(0x300a, vec![0; 16])]);
        let len = f.len();
        // Point the sub-heap record at the root heap.
        let table = 26 + u32::from_le_bytes(f[len - 4..].try_into().unwrap()) as usize;
        f[table + 2 + 2..table + 2 + 6].copy_from_slice(&((len - 26) as u32).to_le_bytes());
        f[table + 2 + 6..table + 2 + 10].copy_from_slice(&0u32.to_le_bytes());
        let mut ciff = Ciff::new(ByteOrder::Little);
        let _ = ciff.parse_heap(&f, 26, f.len() - 26, 0);
        assert!(ciff.heaps <= MAX_HEAPS + 1);
    }

    #[test]
    fn cfa_from_filters_shifts() {
        let rgb = [CfaColor::Red, CfaColor::Green, CfaColor::Blue, CfaColor::Green];
        assert_eq!(cfa_from_filters(0xb4b4_b4b4, rgb, 0, 0).unwrap().to_string(), "RGGB");
        assert_eq!(cfa_from_filters(0xb4b4_b4b4, rgb, 1, 0).unwrap().to_string(), "GRBG");
        let cmyg = [CfaColor::Green, CfaColor::Magenta, CfaColor::Cyan, CfaColor::Yellow];
        assert_eq!(cfa_from_filters(0xb4b4_b4b4, cmyg, 0, 0).unwrap().to_string(), "GMYC");
        let p = cfa_from_filters(0x1e4b_4e1b, cmyg, 0, 0).unwrap();
        assert_eq!((p.width(), p.height()), (2, 8));
        let rows: Vec<String> = (0..8).map(|y| (0..2).map(|x| p.color_at(x, y).letter()).collect::<String>()).collect();
        assert_eq!(rows.join("/"), "YC/MG/CY/GM/YC/GM/CY/MG");
    }

    #[test]
    fn timestamps_and_floats() {
        assert_eq!(format_timestamp(0), "1970-01-01T00:00:00");
        assert_eq!(format_timestamp(1_105_175_848), "2005-01-08T09:17:28");
        assert_eq!(leading_float("1.0.2.0"), 1.0);
        assert_eq!(leading_float("1.02"), 1.02);
        assert_eq!(leading_float("x"), 0.0);
    }

    #[test]
    fn canon600_row_order() {
        let order: Vec<usize> = (0..5).map(|i| canon600_row(i, 5)).collect();
        assert_eq!(order, vec![0, 2, 4, 1, 3]);
        let mut out = [0u16; 8];
        canon600_row_samples(&[1, 0b11_10_01_00, 2, 3, 4, 5, 6, 7, 8, 0b00_01_10_11], &mut out);
        assert_eq!(out, [4 | 3, 8 | 2, 12 | 1, 16, 20 | 3, 24 | 2, 28 | 1, 32]);
    }

    /// Real samples: truncation and byte corruption must give `Ok` or `Err`, never a panic.
    #[test]
    fn corpus_corruption_does_not_panic() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/Canon");
        let mut files = vec![];
        for model in ["PowerShot G2", "EOS D30", "PowerShot Pro70"] {
            let Ok(rd) = std::fs::read_dir(dir.join(model)) else { continue };
            files.extend(
                rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("crw"))),
            );
        }
        if files.is_empty() {
            eprintln!("corpus not available; skipping");
            return;
        }
        let profiler = Profiler::default();
        let ctx = DecodeContext::new(&profiler);
        for path in files {
            let data = std::fs::read(&path).unwrap();
            let check = |d: &[u8]| {
                if let Ok(dec) = open(d, &OpenOptions::default()) {
                    let _ = dec.decode(&ctx);
                }
            };
            check(&data);
            for frac in [2usize, 3, 5, 10, 100] {
                check(&data[..data.len() * (frac - 1) / frac]);
            }
            check(&data[..600.min(data.len())]);
            let mut x = 0x9e37_79b9u32;
            for _ in 0..12 {
                let mut d = data.clone();
                for _ in 0..16 {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    let i = x as usize % d.len();
                    d[i] = (x >> 8) as u8;
                }
                // Corrupt the heap tables near the end too.
                let n = d.len();
                d[n - 1 - (x as usize % 64.min(n))] ^= 0x5a;
                check(&d);
            }
        }
    }
}
