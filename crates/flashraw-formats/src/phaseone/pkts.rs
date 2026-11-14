//! The Leaf "PKTS" packet tree (TIFF tag 0x8606).
//!
//! A packet is `"PKTS"`, a u32 version, a 40-byte NUL-padded name, a u32 payload size and the
//! payload; all integers are big-endian whatever the TIFF byte order. A payload that itself
//! starts with `"PKTS"` is a container of further packets. Siblings form a "run"; the Bayer
//! phase of the sensor is decided when the run that holds the plane count ends.

use flashraw_core::bytes::ascii_field;

/// Back-type number (packet `ShootObj_back_type`) to marketing name; empty entries have no name.
const BACK_NAMES: [&str; 39] = [
    "",
    "DCB2",
    "Volare",
    "Cantare",
    "CMost",
    "Valeo 6",
    "Valeo 11",
    "Valeo 22",
    "Valeo 11p",
    "Valeo 17",
    "",
    "Aptus 17",
    "Aptus 22",
    "Aptus 75",
    "Aptus 65",
    "Aptus 54S",
    "Aptus 65S",
    "Aptus 75S",
    "AFi 5",
    "AFi 6",
    "AFi 7",
    "AFi-II 7",
    "Aptus-II 7",
    "",
    "Aptus-II 6",
    "AFi-II 10",
    "",
    "Aptus-II 10",
    "Aptus-II 5",
    "",
    "DM33",
    "",
    "",
    "Aptus-II 10R",
    "Aptus-II 8",
    "",
    "Aptus-II 12",
    "",
    "AFi-II 12",
];

const MAGIC: &[u8; 4] = b"PKTS";
const HEADER_LEN: usize = 52;
const NAME_AT: usize = 8;
const NAME_LEN: usize = 40;
const SIZE_AT: usize = 48;
/// Bounds of a walk (real files: 6 levels, 130-190 packets).
const MAX_DEPTH: u32 = 16;
const MAX_PACKETS: u32 = 8192;

/// Colour-filter phase announced by the capture profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mosaic {
    /// No run of packets announced a plane count.
    Undecided,
    /// Single plane; index into RGGB, GRBG, BGGR, GBRG.
    Phase(u8),
    /// A capture with this many (not one) planes per exposure.
    MultiPlane,
}

/// What the packet tree says about the capture.
#[derive(Debug)]
pub(super) struct Packets {
    pub back_model: Option<&'static str>,
    pub neutrals: Option<[i32; 4]>,
    pub matrix: Option<[f32; 9]>,
    /// Accumulated rotation in degrees (or a raw code); -1 while unknown.
    pub rotation: i32,
    pub mosaic: Mosaic,
    pub planes: i32,
    pub rows_flags: u32,
    /// `(offset, size)` of the embedded JPEG preview.
    pub preview: Option<(usize, usize)>,
    pub body: String,
    pub serial: Option<String>,
}

impl Packets {
    fn new() -> Self {
        Packets {
            back_model: None,
            neutrals: None,
            matrix: None,
            rotation: -1,
            mosaic: Mosaic::Undecided,
            planes: 0,
            rows_flags: 0,
            preview: None,
            body: String::new(),
            serial: None,
        }
    }
}

struct Walker<'a> {
    data: &'a [u8],
    out: Packets,
    seen: u32,
}

/// Walk the tree that starts at `start` in `data`.
pub(super) fn parse(data: &[u8], start: usize) -> Packets {
    let mut w = Walker { data, out: Packets::new(), seen: 0 };
    w.run(start, data.len(), 0);
    w.out
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// Per-run facts that decide the mosaic.
#[derive(Default)]
struct Run {
    planes: i32,
    red_site: u8,
}

impl Walker<'_> {
    fn run(&mut self, start: usize, end: usize, depth: u32) {
        let end = end.min(self.data.len());
        let mut run = Run::default();
        let mut pos = start;
        while self.seen < MAX_PACKETS {
            let Some(head_end) = pos.checked_add(HEADER_LEN).filter(|&e| e <= end) else { break };
            if &self.data[pos..pos + 4] != MAGIC {
                break;
            }
            let name = ascii_field(&self.data[pos + NAME_AT..pos + NAME_AT + NAME_LEN]);
            let size = be32(self.data, pos + SIZE_AT).unwrap_or(0) as usize;
            let payload_end = head_end.saturating_add(size).min(end);
            self.seen += 1;
            self.packet(&name, head_end, size, payload_end, &mut run);
            if depth < MAX_DEPTH && self.data[head_end..payload_end].starts_with(MAGIC) {
                self.run(head_end, payload_end, depth + 1);
            }
            pos = head_end.saturating_add(size);
        }
        if run.planes != 0 {
            self.out.mosaic = if run.planes == 1 {
                // The rotation held at the end of the run (-1, unknown, truncates to no turn).
                let turns = self.out.rotation / 90;
                Mosaic::Phase((turns + i32::from(run.red_site)).rem_euclid(4) as u8)
            } else {
                Mosaic::MultiPlane
            };
        }
    }

    fn packet(&mut self, name: &str, at: usize, size: usize, end: usize, run: &mut Run) {
        let payload = &self.data[at..end];
        let o = &mut self.out;
        match name {
            "ShootObj_back_type" => {
                if let Some(&i) = ints(payload, 1).first() {
                    if let Some(n) = usize::try_from(i).ok().and_then(|i| BACK_NAMES.get(i)).filter(|n| !n.is_empty()) {
                        o.back_model = Some(n);
                    }
                }
            }
            "NeutObj_neutrals" if o.neutrals.is_none() => {
                if let Ok(n) = <[i32; 4]>::try_from(ints(payload, 4)) {
                    o.neutrals = Some(n);
                }
            }
            "CaptProf_color_matrix" if o.matrix.is_none() => {
                if let Ok(m) = <[f32; 9]>::try_from(numbers(payload, 9)) {
                    o.matrix = Some(m);
                }
            }
            "CaptProf_number_of_planes" => {
                if let Some(&n) = ints(payload, 1).first() {
                    run.planes = n;
                    o.planes = n;
                }
            }
            "CaptProf_raw_data_rotation" => {
                if let Some(&n) = ints(payload, 1).first() {
                    o.rotation = n;
                }
            }
            "CaptProf_mosaic_pattern" => {
                // Four sites in row-major order; the site holding 1 is red.
                let red = ints(payload, 4).iter().rposition(|&v| v == 1);
                run.red_site = match red {
                    Some(1) => 1,
                    Some(2) => 3,
                    Some(3) => 2,
                    _ => 0,
                };
            }
            "ImgProf_rotation_angle" => {
                if let Some(&n) = ints(payload, 1).first() {
                    o.rotation = n.wrapping_sub(o.rotation);
                }
            }
            "JPEG_preview_data" => o.preview = Some((at, size)),
            "Rows_data" => o.rows_flags = be32(payload, 0).unwrap_or(0),
            "CameraObj_camera_type" => {
                let s = ascii_field(payload);
                if !s.is_empty() {
                    o.body = s;
                }
            }
            "back_serial_number" => {
                if let Some(tok) = words(payload).next() {
                    let s = ascii_field(tok);
                    if !s.is_empty() {
                        o.serial = Some(s);
                    }
                }
            }
            _ => {}
        }
    }
}

fn words(payload: &[u8]) -> impl Iterator<Item = &[u8]> {
    payload.split(|&b| b == 0 || b.is_ascii_whitespace()).filter(|p| !p.is_empty())
}

/// The leading integers of a textual payload (at most `max`); a piece may carry trailing junk.
fn ints(payload: &[u8], max: usize) -> Vec<i32> {
    let mut out = Vec::new();
    for mut piece in words(payload) {
        if out.len() == max {
            break;
        }
        while piece.last().is_some_and(|b| !b.is_ascii_digit()) {
            piece = &piece[..piece.len() - 1];
        }
        match std::str::from_utf8(piece).ok().and_then(|s| s.parse::<i32>().ok()) {
            Some(v) => out.push(v),
            None => break,
        }
    }
    out
}

/// The leading decimal numbers of a textual payload (at most `max`).
fn numbers(payload: &[u8], max: usize) -> Vec<f32> {
    let mut out = Vec::new();
    for piece in words(payload) {
        if out.len() == max {
            break;
        }
        match std::str::from_utf8(piece).ok().and_then(|s| s.parse::<f64>().ok()) {
            Some(v) => out.push(v as f32),
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn packet(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut v = b"PKTS".to_vec();
        v.extend(1u32.to_be_bytes());
        let mut n = name.as_bytes().to_vec();
        n.resize(40, 0);
        v.extend(n);
        v.extend((payload.len() as u32).to_be_bytes());
        v.extend(payload);
        v
    }

    fn container(children: &[Vec<u8>]) -> Vec<u8> {
        let mut inner: Vec<u8> = children.concat();
        inner.push(0);
        packet("c", &inner)
    }

    #[test]
    fn integer_reading() {
        assert_eq!(ints(b"12 -3\0 7,", 4), [12, -3, 7]);
        assert_eq!(ints(b"1 x 3", 4), [1]);
        assert_eq!(ints(b"1 2 3 4 5", 4), [1, 2, 3, 4]);
        assert!(ints(b"", 1).is_empty());
        assert_eq!(numbers(b"1.5 -2e1 .25 zz 4", 9), [1.5, -20.0, 0.25]);
    }

    #[test]
    fn back_type_and_neutrals() {
        let tree = container(&[
            packet("ShootObj_back_type", b"12\0"),
            packet("NeutObj_neutrals", b"3270 2703 3270 2232"),
            packet("NeutObj_neutrals", b"1 1 1 1"),
            packet("ShootObj_back_type", b"10"),
            packet("ShootObj_back_type", b"99"),
        ]);
        let p = parse(&tree, 0);
        assert_eq!(p.back_model, Some("Aptus 22"));
        assert_eq!(p.neutrals, Some([3270, 2703, 3270, 2232]));
    }

    #[test]
    fn mosaic_is_decided_when_the_run_ends() {
        let capture = container(&[
            packet("CaptProf_raw_data_rotation", b"0"),
            packet("CaptProf_number_of_planes", b"1"),
            packet("CaptProf_mosaic_pattern", b"2 0 1 3"),
        ]);
        let tree = container(&[capture, packet("ImgProf_rotation_angle", b"90")]);
        let p = parse(&tree, 0);
        assert_eq!(p.mosaic, Mosaic::Phase(3));
        assert_eq!(p.rotation, 90);
        let multi = container(&[container(&[packet("CaptProf_number_of_planes", b"4")])]);
        assert_eq!(parse(&multi, 0).mosaic, Mosaic::MultiPlane);
    }

    #[test]
    fn truncated_and_hostile_trees_end_quietly() {
        let tree = container(&[packet("Rows_data", &[0, 0, 0, 1])]);
        for n in 0..tree.len() {
            let _ = parse(&tree[..n], 0);
        }
        let mut bomb = packet("x", &[]);
        bomb[48..52].copy_from_slice(&u32::MAX.to_be_bytes());
        let _ = parse(&bomb, 0);
        assert_eq!(parse(&tree, 0).rows_flags, 1);
    }
}
