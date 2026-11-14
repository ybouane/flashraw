//! Canon CRW: the CIFF container of the 1998-2004 cameras.
//!
//! A CIFF file is a tree of "heaps": a heap ends with the offset of its record table, and a record is
//! either a value stored in the heap, a value held by the record itself, or a sub-heap. The reader
//! walks the tree once, collects the records it knows into plain values, and then derives the
//! geometry, colour filter pattern, white balance and payload description from them.
//!
//! Everything here is bounds-checked against the file. Tables are separated from the code and carry
//! their evidence in comments: `verified` means a corpus file exercises the entry, `unverified` means
//! the entry follows Canon's published lists (or has always been applied) and has no sample file.

use super::geometry::{self, BandParams, Geometry};
use crate::common::*;
use flashraw_codecs::PackedLayout;
use flashraw_codecs::canon_crw::{self, CrwParams};
use flashraw_core::bytes::{Cursor, ascii_field};
use flashraw_core::*;

/// True when `data` starts with a CIFF header (`II`/`MM`, header length, `HEAPCCDR`).
pub fn probe(data: &[u8]) -> bool {
    data.len() >= 26 && (data.starts_with(b"II") || data.starts_with(b"MM")) && &data[6..14] == b"HEAPCCDR"
}

// ---------------------------------------------------------------------------------------------
// Bounds that keep a damaged tree from costing more than the file is worth
// ---------------------------------------------------------------------------------------------

/// Records per heap above which the heap is ignored.
const MAX_RECORDS: usize = 127;
/// Nesting depth below which sub-heaps are followed.
const MAX_DEPTH: u32 = 16;
/// Heaps visited before the file is rejected.
const MAX_HEAPS: usize = 1024;
/// Sensor dimensions that can be real.
const DIMENSIONS: std::ops::RangeInclusive<u32> = 22..=64000;

// ---------------------------------------------------------------------------------------------
// White-balance presets (DATA)
// ---------------------------------------------------------------------------------------------

/// A white-balance preset of a colour table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Preset {
    Auto,
    Auto2,
    Daylight,
    Cloudy,
    Tungsten,
    Fluorescent,
    /// The preset stored by the colour balance record; Canon's setting `Fluorescent` does not map to it.
    FluorescentWhite,
    FluorescentHigh,
    Flash,
    Custom,
    Custom1,
    Custom2,
    Custom3,
    Custom4,
    BlackWhite,
    Shade,
    Kelvin,
    PcSet1,
    PcSet2,
    PcSet3,
    PcSet4,
    PcSet5,
    Underwater,
    /// Canon's unassigned slots 13 and 22.
    Unassigned,
}

const PRESET_COUNT: usize = Preset::Unassigned as usize + 1;

/// Canon's white-balance setting number (published enumeration) to the preset it selects.
/// Settings 0, 4 and 5 are exercised by corpus files (Auto, Fluorescent, Flash); the others follow
/// the published list (unverified).
const WB_SETTING_PRESET: [Preset; 24] = [
    Preset::Auto,
    Preset::Daylight,
    Preset::Cloudy,
    Preset::Tungsten,
    Preset::Fluorescent,
    Preset::Flash,
    Preset::Custom,
    Preset::BlackWhite,
    Preset::Shade,
    Preset::Kelvin,
    Preset::PcSet1,
    Preset::PcSet2,
    Preset::PcSet3,
    Preset::Unassigned,
    Preset::FluorescentHigh,
    Preset::Custom1,
    Preset::Custom2,
    Preset::Underwater,
    Preset::Custom3,
    Preset::Custom4,
    Preset::PcSet4,
    Preset::PcSet5,
    Preset::Unassigned,
    Preset::Auto2,
];

/// The preset a setting number selects; unknown numbers and "no shot info seen" (-1) mean Auto.
fn preset_of_setting(wbi: i32) -> Preset {
    usize::try_from(wbi).ok().and_then(|i| WB_SETTING_PRESET.get(i)).copied().unwrap_or(Preset::Auto)
}

/// Presets of the colour balance record `0x10a9` after the optional first one: `n >= 7` stores these
/// six in this order (verified on D60 / 10D / 300D files, which have Auto as the setting).
const BALANCE_PRESETS: [Preset; 6] =
    [Preset::Daylight, Preset::Shade, Preset::Cloudy, Preset::Tungsten, Preset::FluorescentWhite, Preset::Flash];
/// Presets stored in addition when `n == 9`.
const BALANCE_EXTRA_PRESETS: [Preset; 2] = [Preset::Custom, Preset::Kelvin];
/// The six lines of the EOS D30 colour record `0x0032` (length 768).
const D30_PRESETS: [Preset; 6] =
    [Preset::Daylight, Preset::Cloudy, Preset::Tungsten, Preset::Fluorescent, Preset::Flash, Preset::Custom];

/// Layout of the white-balance lines inside a PowerShot colour record `0x0032` (DATA; checked on the
/// 11 RGB PowerShot corpus tables, see the specification). `None` entries are lines that are skipped.
struct LineTable {
    lines: &'static [Option<Preset>],
    /// Offset added to the start of the lines for this table; `None` keeps the heap's current value.
    offset: Option<i32>,
    /// XOR keys applied alternately to successive words.
    keys: [u16; 2],
}

/// First word 0, record length 2048: G2, S30, S40, G3, G5, S45, S50 (unencrypted).
const TABLE_PLAIN: LineTable = LineTable {
    lines: &[
        Some(Preset::Custom1),
        Some(Preset::Custom2),
        Some(Preset::Daylight),
        Some(Preset::Cloudy),
        Some(Preset::Tungsten),
        Some(Preset::Fluorescent),
        None,
        Some(Preset::Flash),
    ],
    offset: None,
    keys: [0, 0],
};
/// First word 0x0410, record length 2048: Pro1.
const TABLE_PRO1: LineTable = LineTable {
    lines: &[
        Some(Preset::Custom1),
        Some(Preset::Custom2),
        Some(Preset::Daylight),
        Some(Preset::Cloudy),
        Some(Preset::Tungsten),
        Some(Preset::Fluorescent),
        None,
        Some(Preset::Flash),
        None,
        None,
        None,
    ],
    offset: Some(8),
    keys: [0x410, 0x45f3],
};
/// First word 0x0410, record length 3072: G6, S60, S70.
const TABLE_G6: LineTable = LineTable {
    lines: &[
        Some(Preset::Custom1),
        Some(Preset::Custom2),
        Some(Preset::Daylight),
        Some(Preset::Cloudy),
        Some(Preset::Tungsten),
        Some(Preset::Fluorescent),
        Some(Preset::FluorescentHigh),
        None,
        Some(Preset::Underwater),
        None,
        Some(Preset::Flash),
    ],
    offset: Some(16),
    keys: [0x410, 0x45f3],
};

/// Bytes between the record start and the first line, besides the table's own offset.
const LINES_START: i64 = 2 + 78;
/// Heap offset adjustment after a `0x102c` record of the 272 .. 274 layout.
const APPENDIX_OFFSET: i32 = -14;
/// Firmware below this number stores no as-shot line in the unencrypted table.
const FIRMWARE_WITH_AS_SHOT: f64 = 1.02;

// ---------------------------------------------------------------------------------------------
// Camera-specific sensor facts (DATA)
// ---------------------------------------------------------------------------------------------

/// How the sensor data is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Payload {
    Compressed,
    /// 10-bit samples, most significant bit first, in 16-bit words.
    Packed10,
    /// PowerShot 600: 10-byte groups of eight samples, even rows first.
    Canon600,
}

/// Colour letters of the filter descriptor's 2-bit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    Rgbg,
    /// Cyan, magenta, yellow, green: codes 0..3 are G, M, C, Y.
    Cmyg,
}

impl Filter {
    fn colours(self) -> [CfaColor; 4] {
        match self {
            Filter::Rgbg => [CfaColor::Red, CfaColor::Green, CfaColor::Blue, CfaColor::Green],
            Filter::Cmyg => [CfaColor::Green, CfaColor::Magenta, CfaColor::Cyan, CfaColor::Yellow],
        }
    }
    fn channels(self) -> usize {
        if self == Filter::Cmyg { 4 } else { 3 }
    }
}

/// A model whose geometry or storage differs from the table-driven default.
struct ModelRule {
    /// Model name without the make, compared ignoring case.
    model: &'static str,
    /// Image height and width, and the buffer width when the rule sets one (`None`: the sensor decides).
    size: Option<(u32, u32, Option<u32>)>,
    descriptor: u32,
    filter: Filter,
    payload: Payload,
    /// Pixel aspect override.
    aspect: Option<f32>,
}

/// The 8 colour-filter rows x 2 columns descriptor of the default Bayer sensors.
const DESCRIPTOR_BAYER: u32 = 0x9494_9494;

/// Per-model rules. Pro70 and G1 have sample files (Pro70 1552x1024, G1 2144x1560); the Pro90 IS,
/// 600, A5, A5 Zoom and A50 rows are unverified (no file).
const MODEL_RULES: &[ModelRule] = &[
    ModelRule {
        model: "PowerShot 600",
        size: Some((613, 854, Some(896))),
        descriptor: 0xe1e4_e1e4,
        filter: Filter::Cmyg,
        payload: Payload::Canon600,
        aspect: None,
    },
    ModelRule {
        model: "PowerShot A5",
        size: Some((773, 960, Some(992))),
        descriptor: 0x1e4e_1e4e,
        filter: Filter::Cmyg,
        payload: Payload::Packed10,
        aspect: Some(256.0 / 235.0),
    },
    ModelRule {
        model: "PowerShot A5 Zoom",
        size: Some((773, 960, Some(992))),
        descriptor: 0x1e4e_1e4e,
        filter: Filter::Cmyg,
        payload: Payload::Packed10,
        aspect: Some(256.0 / 235.0),
    },
    ModelRule {
        model: "PowerShot A50",
        size: Some((968, 1290, Some(1320))),
        descriptor: 0x1b4e_4b1e,
        filter: Filter::Cmyg,
        payload: Payload::Packed10,
        aspect: None,
    },
    ModelRule {
        model: "PowerShot Pro70",
        size: Some((1024, 1552, None)),
        descriptor: 0x1e4b_4e1b,
        filter: Filter::Cmyg,
        payload: Payload::Packed10,
        aspect: None,
    },
    ModelRule {
        model: "PowerShot Pro90 IS",
        size: None,
        descriptor: 0xb4b4_b4b4,
        filter: Filter::Cmyg,
        payload: Payload::Compressed,
        aspect: None,
    },
    ModelRule {
        model: "PowerShot G1",
        size: None,
        descriptor: 0xb4b4_b4b4,
        filter: Filter::Cmyg,
        payload: Payload::Compressed,
        aspect: None,
    },
];

/// The CRW era sensors are the first rows of the shared layout table.
const CRW_LAYOUT_ROWS: usize = 8;

// ---------------------------------------------------------------------------------------------
// Parsing the heap tree
// ---------------------------------------------------------------------------------------------

/// Everything collected from the records.
#[derive(Debug)]
struct Ciff {
    make: String,
    model: String,
    artist: Option<String>,
    firmware: Option<String>,
    serial: Option<String>,
    /// Image width, height, pixel aspect and rotation from the image info record.
    image: Option<(u32, u32, f32, i32)>,
    table: Option<u32>,
    raw_offset: Option<usize>,
    /// `(position, length)` of the JPEG previews.
    jpegs: Vec<(usize, usize)>,
    shutter: Option<f32>,
    aperture: Option<f32>,
    iso: Option<f32>,
    focal: Option<f32>,
    bias: Option<f32>,
    time: Option<u32>,
    color_space: Option<&'static str>,
    shot_order: Option<u32>,
    unique_id: Option<u32>,
    /// Sensor size and default crop (left, top, right, bottom).
    sensor: Option<(u32, u32, [u32; 4])>,
    /// Camera multipliers `R, G, B, G2`; `[0, 1, 0, 0]` until a record sets them.
    cam_mul: [f32; 4],
    /// Per-channel black `R, G, B, G2`.
    black: Option<[u16; 4]>,
    heaps: usize,
}

/// State shared by the records of one heap (sub-heaps start afresh).
struct HeapState {
    /// White-balance setting from the shot info record; -1 until one is seen.
    wbi: i32,
    /// A `0x102c` record of the 272 .. 274 layout was seen.
    appendix: bool,
    got_as_shot: bool,
    table_offset: i32,
}

impl HeapState {
    fn new() -> Self {
        HeapState { wbi: -1, appendix: false, got_as_shot: false, table_offset: 0 }
    }
}

/// A record of a heap table.
#[derive(Clone, Copy)]
struct Record {
    ty: u16,
    length: u32,
    /// Position of the value in the file (heap start + offset).
    pos: usize,
}

/// Four words in the order `R, G, G2, B` as `[R, G, B, G2]`.
fn quad_rggb(c: &mut Cursor) -> Result<[f32; 4]> {
    let (r, g, g2, b) = (c.u16()?, c.u16()?, c.u16()?, c.u16()?);
    Ok([r as f32, g as f32, b as f32, g2 as f32])
}

/// Four words in the order `G, R, B, G2` as `[R, G, B, G2]`.
fn quad_grbg(c: &mut Cursor) -> Result<[f32; 4]> {
    let (g, r, b, g2) = (c.u16()?, c.u16()?, c.u16()?, c.u16()?);
    Ok([r as f32, g as f32, b as f32, g2 as f32])
}

/// The number at the start of a text like C's `atof`: optional sign, digits, one dot.
fn leading_number(s: &str) -> f64 {
    let b = s.trim_start().as_bytes();
    let mut end = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        end = 1;
    }
    let mut dot = false;
    while let Some(&c) = b.get(end) {
        if c.is_ascii_digit() || (c == b'.' && !dot) {
            dot |= c == b'.';
            end += 1;
        } else {
            break;
        }
    }
    std::str::from_utf8(&b[..end]).ok().and_then(|t| t.parse().ok()).unwrap_or(0.0)
}

/// `YYYY-MM-DDTHH:MM:SS` of a count of seconds since 1970 (proleptic Gregorian calendar).
fn format_time(secs: u32) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Civil date from a day count (days since 1970-01-01).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

impl Ciff {
    fn new() -> Self {
        Ciff {
            make: String::new(),
            model: String::new(),
            artist: None,
            firmware: None,
            serial: None,
            image: None,
            table: None,
            raw_offset: None,
            jpegs: Vec::new(),
            shutter: None,
            aperture: None,
            iso: None,
            focal: None,
            bias: None,
            time: None,
            color_space: None,
            shot_order: None,
            unique_id: None,
            sensor: None,
            cam_mul: [0.0, 1.0, 0.0, 0.0],
            black: None,
            heaps: 0,
        }
    }

    /// Walk the heap `data[start..start + len]` and every heap below it.
    fn heap(&mut self, data: &[u8], order: ByteOrder, start: usize, len: usize, depth: u32) -> Result<()> {
        if depth > MAX_DEPTH {
            return Ok(());
        }
        self.heaps += 1;
        if self.heaps > MAX_HEAPS {
            return Err(Error::corrupt("CIFF: too many heaps"));
        }
        let Some(end) = start.checked_add(len).filter(|&e| len >= 4 && e <= data.len()) else {
            return Ok(());
        };
        let table = start.saturating_add(data.u32_at(order, end - 4)? as usize);
        let count = data.u16_at(order, table)? as usize;
        let table_end = table.saturating_add(2).saturating_add(count * 10);
        if count == 0 || count > MAX_RECORDS || table_end > data.len() {
            return Ok(());
        }
        let mut st = HeapState::new();
        for i in 0..count {
            let at = table + 2 + i * 10;
            let (ty, length, offset) =
                (data.u16_at(order, at)?, data.u32_at(order, at + 2)?, data.u32_at(order, at + 6)?);
            let pos = start.saturating_add(offset as usize);
            let in_record = ty >> 14 == 1;
            if !in_record && pos >= data.len() {
                continue;
            }
            if matches!(ty >> 8, 0x28 | 0x30) {
                self.heap(data, order, pos, (length as usize).min(data.len() - pos), depth + 1)?;
                continue;
            }
            // A record that cannot be read loses itself and nothing else.
            let _ = self.record(data, order, &mut st, Record { ty, length, pos });
        }
        Ok(())
    }

    fn record(&mut self, data: &[u8], order: ByteOrder, st: &mut HeapState, r: Record) -> Result<()> {
        let mut c = Cursor::at(data, r.pos, order);
        match r.ty {
            0x080a => {
                let field = &data[r.pos..r.pos.saturating_add(64).min(data.len())];
                let head = &field[..field.len().min(63)];
                let nul = head.iter().position(|&b| b == 0);
                self.make = ascii_field(&head[..nul.unwrap_or(head.len())]);
                let from = nul.unwrap_or(63) + 1;
                self.model = ascii_field(field.get(from..(from + 63).min(field.len())).unwrap_or(&[]));
            }
            0x0810 => {
                let text = ascii_field(&data[r.pos..r.pos.saturating_add(64).min(data.len())]);
                if !text.is_empty() {
                    self.artist = Some(text);
                }
            }
            0x080b => {
                let n = (r.length as usize).min(64);
                let text = ascii_field(&data[r.pos..r.pos.saturating_add(n).min(data.len())]);
                let text = match text.get(..16) {
                    Some(p) if p.eq_ignore_ascii_case("Firmware Version") => text[16..].trim().to_string(),
                    _ => text,
                };
                if !text.is_empty() {
                    self.firmware = Some(text);
                }
            }
            0x1810 => {
                let (w, h) = (c.u32()?, c.u32()?);
                let aspect = f32::from_bits(c.u32()?);
                self.image = Some((w, h, aspect, c.u32()? as i32));
            }
            0x1835 => self.table = Some(c.u32()?),
            0x2005 => self.raw_offset = Some(r.pos),
            0x2007 | 0x2008 => self.jpegs.push((r.pos, r.length as usize)),
            0x1818 => {
                c.skip(4)?;
                let (tv, av) = (f32::from_bits(c.u32()?), f32::from_bits(c.u32()?));
                self.shutter = Some((-tv).exp2());
                self.aperture = Some((av / 2.0).exp2());
            }
            0x102a => self.shot_info(&mut c, st)?,
            0x102c => self.colour_info2(&mut c, st)?,
            0x10a9 => self.colour_balance(&mut c, st)?,
            0x0032 => self.colour_info1(data, order, st, &r)?,
            0x10b4 => {
                self.color_space = match c.u16()? {
                    1 => Some("sRGB"),
                    2 => Some("AdobeRGB"),
                    _ => None,
                }
            }
            0x580b => {
                self.serial = Some(if self.model.eq_ignore_ascii_case("Canon EOS D30") {
                    format!("{:x}-{:05}", r.length >> 16, r.length & 0xffff)
                } else {
                    r.length.to_string()
                });
            }
            0x1031 => {
                c.skip(2)?;
                let (w, h) = (c.u16()? as u32, c.u16()? as u32);
                c.skip(4)?;
                let crop = [c.u16()? as u32, c.u16()? as u32, c.u16()? as u32, c.u16()? as u32];
                self.sensor = Some((w, h, crop));
            }
            0x501c => self.iso = Some((r.length & 0xffff) as f32),
            0x5029 => {
                let mm = (r.length >> 16) as f32;
                self.focal = Some(if r.length & 0xffff == 2 { mm / 32.0 } else { mm });
            }
            0x5814 => self.bias = Some(f32::from_bits(r.length)),
            0x5817 => self.shot_order = Some(r.length),
            0x5834 => self.unique_id = Some(r.length),
            0x580e => self.time = Some(r.length),
            0x180e => self.time = Some(c.u32()?),
            _ => {}
        }
        Ok(())
    }

    /// `0x102a`: ISO, aperture, shutter and the white-balance setting.
    fn shot_info(&mut self, c: &mut Cursor, st: &mut HeapState) -> Result<()> {
        let mut v = [0u16; 8];
        for w in &mut v {
            *w = c.u16()?;
        }
        self.iso = Some(((v[1] as u32 + v[2] as u32) as f32 / 32.0 - 5.0).exp2() * 100.0);
        self.aperture = (v[4] != 0xFFE0 && v[4] != 0x7FFF).then(|| (v[4] as f32 / 64.0).exp2());
        let mut shutter = (-(v[5] as i16 as f32) / 32.0).exp2();
        if shutter > 1e6 {
            c.skip(32)?;
            shutter = c.u16()? as f32 / 10.0;
        }
        self.shutter = Some(shutter);
        st.wbi = if v[7] >= 24 { 0 } else { v[7] as i32 };
        Ok(())
    }

    /// `0x102c`: camera multipliers of the PowerShot generations that store them here.
    fn colour_info2(&mut self, c: &mut Cursor, st: &mut HeapState) -> Result<()> {
        let kind = c.u16()?;
        if kind > 512 {
            // Pro90 IS, G1: words in the order B, G2, R, G.
            c.skip(118)?;
            let (b, g2, r, g) = (c.u16()?, c.u16()?, c.u16()?, c.u16()?);
            self.cam_mul = [r as f32, g as f32, b as f32, g2 as f32];
        } else if kind != 276 {
            // 272 .. 274: G2, S40, S30. The colour record that follows has an appendix.
            st.appendix = true;
            st.table_offset = APPENDIX_OFFSET;
            c.skip(98)?;
            self.cam_mul = quad_grbg(c)?;
            if self.cam_mul[0] > 0.001 {
                st.got_as_shot = true;
            }
        }
        Ok(())
    }

    /// `0x10a9`: colour balance of the D60, 10D and 300D.
    fn colour_balance(&mut self, c: &mut Cursor, st: &mut HeapState) -> Result<()> {
        let n = (c.u16()? as i32 - 2) / 8 - 1;
        let mut presets = [[0f32; 4]; PRESET_COUNT];
        if n != 0 {
            presets[Preset::Auto as usize] = quad_rggb(c)?;
        }
        let auto = presets[Preset::Auto as usize];
        if n >= 7 {
            for p in BALANCE_PRESETS {
                presets[p as usize] = quad_rggb(c)?;
            }
        } else if n != 0 {
            self.cam_mul = auto;
        }
        if n == 7 {
            let usable = (0..9).contains(&st.wbi) && st.wbi != 6;
            self.cam_mul = if usable { presets[preset_of_setting(st.wbi) as usize] } else { auto };
        }
        if n == 9 {
            for p in BALANCE_EXTRA_PRESETS {
                presets[p as usize] = quad_rggb(c)?;
            }
            self.cam_mul = if (0..10).contains(&st.wbi) { presets[preset_of_setting(st.wbi) as usize] } else { auto };
        }
        let black = quad_rggb(c)?;
        self.black = Some(black.map(|v| v as u16));
        Ok(())
    }

    /// `0x0032`: the EOS D30's colour table, or the PowerShots' table of white-balance lines.
    fn colour_info1(&mut self, data: &[u8], order: ByteOrder, st: &mut HeapState, r: &Record) -> Result<()> {
        let mut c = Cursor::at(data, r.pos, order);
        if r.length == 768 {
            c.skip(4)?;
            // Six preset lines (not used for output), then the camera multipliers.
            for _ in D30_PRESETS {
                for _ in 0..4 {
                    c.u16()?;
                }
            }
            c.skip(68 - 6 * 8)?;
            let (r_, g, g2, b) = (c.u16()?, c.u16()?, c.u16()?, c.u16()?);
            let gain = |w: u16| 1024.0 / w.max(1) as f32;
            self.cam_mul = [gain(r_), gain(g), gain(b), gain(g2)];
            if st.wbi == 0 {
                self.cam_mul[0] = -1.0;
            }
            return Ok(());
        }
        if !(self.cam_mul[0] <= 0.001 || st.appendix) {
            return Ok(());
        }
        let key0 = c.u16()?;
        let table = match (key0, r.length) {
            (0x0410, 2048) => &TABLE_PRO1,
            (0x0410, 3072) => &TABLE_G6,
            (0, 2048) => &TABLE_PLAIN,
            _ => return Ok(()),
        };
        let offset = table.offset.unwrap_or(st.table_offset);
        let firmware_old =
            table.keys == [0, 0] && leading_number(self.firmware.as_deref().unwrap_or("")) < FIRMWARE_WITH_AS_SHOT;
        let std = preset_of_setting(st.wbi);
        let wanted = if matches!(std, Preset::Auto | Preset::Unassigned) || st.got_as_shot || firmware_old {
            None
        } else if std == Preset::Custom {
            Some(Preset::Daylight)
        } else {
            Some(std)
        };
        let line = wanted.and_then(|w| table.lines.iter().position(|l| *l == Some(w)));
        // Every line is read in turn (a short record loses the whole record); only the as-shot line is kept.
        let base = r.pos as i64 + LINES_START + offset as i64;
        let base = usize::try_from(base).map_err(|_| Error::corrupt("CIFF: colour table before the file"))?;
        let mut as_shot = None;
        c.seek(base);
        for (i, l) in table.lines.iter().enumerate() {
            if l.is_none() {
                c.skip(8)?;
                continue;
            }
            let mut w = [c.u16()?, c.u16()?, c.u16()?, c.u16()?];
            for (j, v) in w.iter_mut().enumerate() {
                *v ^= table.keys[j & 1];
            }
            if Some(i) == line {
                as_shot = Some([w[1] as f32, w[0] as f32, w[2] as f32, w[3] as f32]);
            }
        }
        if let Some(m) = as_shot {
            self.cam_mul = m;
            st.got_as_shot = true;
        }
        if !st.got_as_shot {
            self.cam_mul[0] = -1.0;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

/// Colour filter pattern of a descriptor (8 rows x 2 columns of 2-bit codes), anchored at the buffer origin.
fn filter_pattern(descriptor: u32, filter: Filter, left: u32, top: u32) -> Result<CfaPattern> {
    let code = |row: u32, col: u32| ((descriptor >> ((((row << 1) & 14) | (col & 1)) << 1)) & 3) as usize;
    let colours = filter.colours();
    let two_rows = (2..8).all(|r| (0..2).all(|c| code(r, c) == code(r % 2, c)));
    let ph: u32 = if two_rows { 2 } else { 8 };
    let mut cells = Vec::with_capacity(2 * ph as usize);
    for y in 0..ph {
        for x in 0..2u32 {
            cells.push(colours[code((y + ph - top % ph) % ph, (x + 2 - left % 2) % 2)]);
        }
    }
    CfaPattern::new(2, ph as usize, &cells).ok_or_else(|| Error::invalid_metadata("CRW: invalid CFA"))
}

/// Orientation of a rotation in degrees.
fn rotation_orientation(degrees: i32) -> Orientation {
    match degrees.rem_euclid(360) {
        90 => Orientation::Rotate90,
        180 => Orientation::Rotate180,
        270 => Orientation::Rotate270,
        _ => Orientation::Normal,
    }
}

/// The decode closure handed to the shared decoder.
type DecodeFn<'a> = Box<dyn Fn(&DecodeContext) -> Result<RawImage> + Send + Sync + 'a>;

/// Bytes of one stored PowerShot 600 row.
const CANON600_ROW: usize = 1120;

/// Unpack one PowerShot 600 row (112 groups of 10 bytes, eight samples each) into `dst`.
fn unpack_canon600_row(src: &[u8], dst: &mut [u16]) {
    for (g, out) in src.chunks_exact(10).zip(dst.chunks_mut(8)) {
        let s = [
            (g[0] as u16) << 2 | (g[1] as u16) >> 6,
            (g[2] as u16) << 2 | (g[1] as u16 >> 4) & 3,
            (g[3] as u16) << 2 | (g[1] as u16 >> 2) & 3,
            (g[4] as u16) << 2 | (g[1] as u16) & 3,
            (g[5] as u16) << 2 | (g[9] as u16) & 3,
            (g[6] as u16) << 2 | (g[9] as u16 >> 2) & 3,
            (g[7] as u16) << 2 | (g[9] as u16 >> 4) & 3,
            (g[8] as u16) << 2 | (g[9] as u16) >> 6,
        ];
        out.copy_from_slice(&s[..out.len()]);
    }
}

/// Buffer row that stored row `i` of `rows` goes to: even rows come first.
fn canon600_row(i: usize, rows: usize) -> usize {
    let evens = rows.div_ceil(2);
    if i < evens { 2 * i } else { 2 * (i - evens) + 1 }
}

/// Open a CRW file.
pub fn open<'a>(data: &'a [u8], _opts: &OpenOptions) -> Result<Box<dyn RawDecoder + 'a>> {
    if !probe(data) {
        return Err(Error::UnsupportedFormat("not a CIFF file".into()));
    }
    let order = if data.starts_with(b"II") { ByteOrder::Little } else { ByteOrder::Big };
    let hlen = data.u32_at(order, 2)? as usize;
    if hlen < 14 || hlen >= data.len() {
        return Err(Error::corrupt("CIFF header length out of range"));
    }
    let mut ciff = Ciff::new();
    ciff.heap(data, order, hlen, data.len() - hlen, 0)?;
    if ciff.make.is_empty() && ciff.model.is_empty() {
        return Err(Error::invalid_metadata("CIFF without make/model record"));
    }

    let mut meta = RawMetadata::new(ciff.make.clone(), ciff.model.clone());
    let cam = db_lookup(&mut meta, "");
    let model = ciff.model.trim();
    let model = model.strip_prefix("Canon ").unwrap_or(model);
    let rule = MODEL_RULES.iter().find(|m| m.model.eq_ignore_ascii_case(model));

    // Geometry. The sensor record gives the buffer, the image record the picture; a few sensors are in
    // the layout table, a few models are special.
    let (mut raw_w, mut raw_h) = ciff.sensor.map_or((0, 0), |s| (s.0, s.1));
    let (mut width, mut height) = ciff.image.map_or((0, 0), |i| (i.0, i.1));
    let mut aspect = ciff.image.map_or(1.0, |i| i.2);
    let (mut left, mut top) = (0u32, 0u32);
    let mut band = BandParams { left_start: 2, left_guard: 2, right_gap: 2, right_trim: 0 };
    let row = geometry::LAYOUTS.iter().take(CRW_LAYOUT_ROWS).find(|l| (l.raw_width, l.raw_height) == (raw_w, raw_h));
    if let Some(l) = row {
        let g = l.geometry();
        (left, top, width, height) = (g.left, g.top, g.width, g.height);
        band = l.band;
    }
    let metadata_black = ciff.black.filter(|b| b.iter().map(|&v| v as u32).sum::<u32>() / 4 != 0);
    let from_metadata = row.is_none() && metadata_black.is_some() && ciff.sensor.is_some();
    if from_metadata {
        if let Some((_, _, crop)) = ciff.sensor {
            left = (crop[0] + 1) & !1;
            top = (crop[1] + 1) & !1;
            // The right and bottom edges are exclusive here, unlike the maker note's inclusive crop.
            width = crop[2].saturating_sub(left);
            height = crop[3].saturating_sub(top);
        }
    }
    let (descriptor, filter, payload) = match rule {
        Some(m) => {
            if let Some((h, w, rw)) = m.size {
                (height, width) = (h, w);
                raw_w = rw.unwrap_or(raw_w);
            }
            if let Some(a) = m.aspect {
                aspect = a;
            }
            (m.descriptor, m.filter, m.payload)
        }
        None => (DESCRIPTOR_BAYER, Filter::Rgbg, Payload::Compressed),
    };
    raw_w = raw_w.max(width);
    raw_h = raw_h.max(height);
    if !DIMENSIONS.contains(&raw_w) || !DIMENSIONS.contains(&raw_h) || width < 22 || height < 22 {
        return Err(Error::invalid_metadata(format!(
            "CRW: implausible dimensions {width}x{height} in {raw_w}x{raw_h}"
        )));
    }
    if left >= raw_w || top >= raw_h {
        return Err(Error::invalid_metadata("CRW: active area outside the buffer"));
    }
    if !(0.1..=10.0).contains(&aspect) {
        aspect = 1.0;
    }
    let active = Rect::new(left, top, width.min(raw_w - left), height.min(raw_h - top));
    let cfa = filter_pattern(descriptor, filter, left, top)?;

    let raw_off = ciff.raw_offset.unwrap_or(hlen);
    let lowbits = payload == Payload::Compressed && canon_crw::has_lowbits(data);
    let bits: u8 = if payload == Payload::Compressed && lowbits { 12 } else { 10 };
    let orientation = rotation_orientation(ciff.image.map_or(0, |i| i.3));

    let mut info = FrameInfo::new(raw_w, raw_h, bits);
    info.pixel_aspect = aspect;
    info.compression = if payload == Payload::Compressed { Compression::CanonCrw } else { Compression::Uncompressed };
    info.layout = layout_for(&cfa);
    info.cfa = Some(cfa);
    info.orientation = orientation;
    info.active_area = active;
    info.crop = active;

    let matrix = flashraw_camera_db::find_matrix(&meta.canonical_make, &meta.canonical_model)
        .or_else(|| cam.and_then(|c| flashraw_camera_db::find_matrix(c.canonical_make, c.canonical_model)));
    let white = matrix.map_or(0, |m| m.white);
    info.white_level = WhiteLevel::uniform(match (payload, lowbits) {
        (Payload::Compressed, true) if white > 0 => white as u32,
        (Payload::Compressed, true) => 4095,
        _ => 1023,
    });
    info.black_level = BlackLevel::uniform(matrix.map_or(0, |m| m.black.max(0)) as f32);

    // Black level.
    let mut masked = Vec::new();
    let band_rect =
        |x0: u32, x1: u32| (x1 > x0 && active.height > 0).then(|| Rect::new(x0, active.y, x1 - x0, active.height));
    match payload {
        Payload::Compressed => {
            if let (true, Some(b)) = (from_metadata, metadata_black) {
                let v = |row: u32, col: u32| match (row, col) {
                    (0, 0) => b[0],
                    (0, _) => b[1],
                    (_, 0) => b[3],
                    _ => b[2],
                } as f32;
                let at = |x: u32, y: u32| v((y + top) % 2, (x + left) % 2);
                info.black_level = BlackLevel::pattern2x2([at(0, 0), at(1, 0), at(0, 1), at(1, 1)]);
            } else {
                let g = Geometry { left, top, width, height };
                masked.extend(
                    geometry::masked_columns(&g, raw_w, &band).into_iter().filter_map(|(a, b)| band_rect(a, b)),
                );
            }
        }
        Payload::Packed10 => masked.extend(band_rect(active.right(), raw_w)),
        Payload::Canon600 => {
            if let Some(mean) = canon600_black(data, raw_off, raw_w, raw_h, width) {
                info.black_level = BlackLevel::uniform(mean);
            }
        }
    }
    info.black_from_masked = !masked.is_empty();
    info.masked_areas = masked;

    // White balance: the multipliers must all be plausible and within a factor of 100 of each other.
    let channels = filter.channels();
    let m = &ciff.cam_mul[..channels];
    let lowest = m.iter().copied().fold(f32::INFINITY, f32::min);
    let highest = m.iter().copied().fold(0.0, f32::max);
    if m.iter().all(|&v| v > 0.001) && lowest > 0.01 && highest / lowest <= 100.0 {
        let c = ciff.cam_mul;
        info.white_balance = WhiteBalance::from_multipliers(c[0], c[1], c[2], c[3]);
    }
    info.color_matrices = match matrix.and_then(|m| {
        ColorMatrix::new(
            Illuminant::D65,
            m.color_matrix.len() / 3,
            m.color_matrix.iter().map(|&v| v as f32 / 10000.0).collect(),
        )
    }) {
        Some(cm) => ColorMatrices { color: vec![cm], source: ColorSource::CameraDb, ..Default::default() },
        None => db_color(&meta, cam),
    };

    // Metadata.
    meta.orientation = orientation;
    meta.artist = ciff.artist.clone();
    meta.firmware = ciff.firmware.clone();
    meta.serial = ciff.serial.clone();
    meta.capture_time = ciff.time.map(format_time);
    let x = &mut meta.exposure;
    x.iso = ciff.iso.filter(|v| v.is_finite() && *v > 0.0).map(|v| v.round() as u32);
    x.exposure_time = ciff.shutter.map(f64::from);
    x.f_number = ciff.aperture.filter(|v| v.is_finite() && *v > 0.0);
    x.focal_length = ciff.focal.filter(|v| *v > 0.0);
    x.exposure_bias = ciff.bias.filter(|v| v.is_finite());
    if let Some(v) = ciff.shot_order {
        meta.set_extra("shot_order", v);
    }
    if let Some(id) = ciff.unique_id {
        meta.set_extra("canon_model_id", format!("0x{id:08x}"));
    }
    if let Some(cs) = ciff.color_space {
        meta.set_extra("color_space", cs);
    }
    // Every CRW reports its Huffman table selector; files without the record use selector 0.
    meta.set_extra("crw_table", ciff.table.unwrap_or(0));
    meta.set_extra(
        "mode",
        match (payload, lowbits) {
            (Payload::Compressed, true) => "compressed-12bit",
            (Payload::Compressed, false) => "compressed-10bit",
            (Payload::Packed10, _) => "packed-10bit",
            (Payload::Canon600, _) => "canon600-10bit",
        },
    );
    for &(pos, len) in &ciff.jpegs {
        push_jpeg(data, pos, len, orientation, &mut meta.previews);
    }
    classify_previews(&mut meta.previews);

    // Decoding.
    let (w, h) = (raw_w as usize, raw_h as usize);
    let file_len = data.len();
    let unit = DecodeUnit {
        rect: Rect::full(raw_w, raw_h),
        offset: raw_off as u64,
        length: file_len.saturating_sub(raw_off) as u64,
    };
    let table = ciff.table.unwrap_or(0);
    let decode: DecodeFn<'a> = match payload {
        Payload::Compressed => Box::new(move |ctx| {
            let plane_len = if lowbits { w * h / 4 } else { 0 };
            let start = raw_off.saturating_add(514).saturating_add(plane_len);
            let stream = data.get(start..).filter(|s| !s.is_empty());
            let Some(stream) = stream else {
                return Err(Error::truncated(Truncation::new(DataUnit::Payload, start as u64, 1, file_len as u64)));
            };
            let plane = if lowbits { data.get(raw_off..raw_off + plane_len) } else { None };
            let params = CrwParams { width: w, height: h, table, lowbits: plane, low_offset_quirk: raw_w == 2672 };
            let mut img = RawImage::new_u16(raw_w, raw_h, 1, &ctx.limits)?;
            ctx.profiler.time(Stage::EntropyDecode, || canon_crw::decode(stream, &params, img.u16_mut()))?;
            Ok(img)
        }),
        Payload::Packed10 => {
            let layout = PackedLayout::tight(w, h, 10, PackOrder::Msb16);
            Box::new(move |ctx| {
                let geometry = RowGeometry::packed(h, w, 10, layout.row_stride);
                let src = ctx.sensor_rows(data, DataUnit::Payload, raw_off, geometry)?;
                let mut img = RawImage::new_u16(raw_w, raw_h, 1, &ctx.limits)?;
                ctx.profiler.time(Stage::Unpack, || {
                    flashraw_codecs::unpack(src, layout, img.u16_mut(), w, ctx.parallel, ctx.simd, ctx.recover)
                })?;
                Ok(img)
            })
        }
        Payload::Canon600 => Box::new(move |ctx| {
            if w * 10 / 8 < CANON600_ROW {
                return Err(Error::corrupt("PowerShot 600 row narrower than its data"));
            }
            let geometry = RowGeometry { rows: h, stride: CANON600_ROW, row_bytes: CANON600_ROW, samples: w };
            let src = ctx.sensor_rows(data, DataUnit::Payload, raw_off, geometry)?;
            let mut img = RawImage::new_u16(raw_w, raw_h, 1, &ctx.limits)?;
            let out = img.u16_mut();
            ctx.profiler.time(Stage::Unpack, || {
                for (i, stored) in src.chunks_exact(CANON600_ROW).take(h).enumerate() {
                    let r = canon600_row(i, h);
                    unpack_canon600_row(stored, &mut out[r * w..(r + 1) * w]);
                }
            });
            Ok(img)
        }),
    };
    let mut dec = SimpleDecoder::new(RawFormat::Crw, meta, info, decode);
    if payload == Payload::Compressed {
        dec.units = vec![unit];
    } else {
        dec.random_access = RandomAccess::Rows;
    }
    if payload == Payload::Packed10 {
        let layout = PackedLayout::tight(w, h, 10, PackOrder::Msb16);
        let needed = RowGeometry::packed(h, w, 10, layout.row_stride).needed();
        let rest = data.get(raw_off..).unwrap_or(&[]);
        dec.packed = Some(PackedPayload {
            data: &rest[..rest.len().min(needed)],
            width: raw_w,
            height: raw_h,
            bits: 10,
            order: PackOrder::Msb16,
            row_stride: layout.row_stride,
        });
    }
    Ok(Box::new(dec))
}

/// PowerShot 600 black level: the mean of the columns beyond the picture, minus 4, when the file holds
/// every row (unverified, no sample file).
fn canon600_black(data: &[u8], raw_off: usize, raw_w: u32, raw_h: u32, width: u32) -> Option<f32> {
    let (w, h) = (raw_w as usize, raw_h as usize);
    let need = h.checked_mul(CANON600_ROW)?;
    let rows = data.get(raw_off..raw_off.checked_add(need)?)?;
    if w * 10 / 8 < CANON600_ROW || width >= raw_w {
        return None;
    }
    let mut row = vec![0u16; w];
    let (mut sum, mut n) = (0u64, 0u64);
    for stored in rows.chunks_exact(CANON600_ROW) {
        unpack_canon600_row(stored, &mut row);
        sum += row[width as usize..].iter().map(|&v| v as u64).sum::<u64>();
        n += (w - width as usize) as u64;
    }
    (n > 0).then(|| (sum as f64 / n as f64) as f32 - 4.0)
}
