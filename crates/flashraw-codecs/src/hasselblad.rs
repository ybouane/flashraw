//! Hasselblad 3FR/FFF compressed sensor data (a lossless-JPEG variant).
//!
//! A JPEG marker header (SOF3, DHT, SOS) is followed by one bit stream of pixel pairs; see the
//! specification for the exact syntax.
//!
//! The stream is a run of 32-bit little-endian words read most significant bit first, with no
//! byte stuffing and no row alignment. Every pair of horizontally adjacent pixels is coded as
//! two prefix-coded difference lengths followed by the two difference values. Even and odd
//! columns form two independent lanes, each predicted from the pixel two columns to its left.

use crate::ljpeg_par::PARALLEL_MIN_SAMPLES;
use crate::par_entropy::{self, EndCheck, SymbolStream};
use flashraw_core::bits::{BitPumpMsb32, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, Result, par};

/// Parsed header of a Hasselblad stream.
#[derive(Debug, Clone)]
pub struct HasselbladHeader {
    /// Prefix code for the difference lengths.
    pub table: HuffTable,
    /// Predictor selector from the SOS header.
    pub predictor: u32,
    /// Frame width declared by SOF3 (informational; the container decides).
    pub width: u32,
    /// Frame height declared by SOF3 (informational; the container decides).
    pub height: u32,
    /// Offset of the first scan byte.
    pub scan_offset: usize,
}

const MARKER_SOF3: u8 = 0xC3;
const MARKER_DHT: u8 = 0xC4;
const MARKER_SOS: u8 = 0xDA;
const MARKER_EOI: u8 = 0xD9;

/// Predictor selector that switches to the half-gradient scheme.
const PREDICTOR_GRADIENT: u32 = 11;

impl HasselbladHeader {
    /// Parse the markers up to the start of the scan.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if !data.starts_with(&[0xFF, 0xD8]) {
            return Err(Error::corrupt("Hasselblad: missing SOI marker"));
        }
        let mut pos = 2usize;
        let (mut width, mut height) = (0u32, 0u32);
        // The table with id 0 defined last wins; without one, the first table defined.
        let mut first: Option<HuffTable> = None;
        let mut id0: Option<HuffTable> = None;
        loop {
            // Stray bytes, then fill bytes, then the marker code.
            while data.get(pos).is_some_and(|&b| b != 0xFF) {
                pos += 1;
            }
            while data.get(pos) == Some(&0xFF) {
                pos += 1;
            }
            let marker = *data.get(pos).ok_or_else(|| Error::corrupt("Hasselblad: no scan header"))?;
            pos += 1;
            if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) {
                continue;
            }
            if marker == MARKER_EOI {
                return Err(Error::corrupt("Hasselblad: end of image before the scan"));
            }
            let len_bytes = data.get(pos..pos + 2).ok_or_else(|| Error::corrupt("Hasselblad: truncated header"))?;
            let seg_len = u16::from_be_bytes([len_bytes[0], len_bytes[1]]) as usize;
            if seg_len < 2 {
                return Err(Error::corrupt("Hasselblad: bad segment length"));
            }
            let end = pos + seg_len;
            let body = data.get(pos + 2..end).ok_or_else(|| Error::corrupt("Hasselblad: segment past the data"))?;
            match marker {
                MARKER_SOF3 if body.len() >= 5 => {
                    height = u32::from(u16::from_be_bytes([body[1], body[2]]));
                    width = u32::from(u16::from_be_bytes([body[3], body[4]]));
                }
                MARKER_DHT => {
                    let mut rest = body;
                    while rest.len() >= 17 {
                        let id = rest[0] & 0x0F;
                        let counts = &rest[1..17];
                        let symbols: usize = counts.iter().map(|&c| usize::from(c)).sum();
                        let list = rest
                            .get(17..17 + symbols)
                            .ok_or_else(|| Error::corrupt("Hasselblad: Huffman table past its segment"))?;
                        let table = HuffTable::from_counts(counts, list, false)?;
                        if id == 0 {
                            id0 = Some(table.clone());
                        }
                        first.get_or_insert(table);
                        rest = &rest[17 + symbols..];
                    }
                }
                MARKER_SOS => {
                    let components = *body.first().ok_or_else(|| Error::corrupt("Hasselblad: short scan header"))?;
                    let ss = *body
                        .get(1 + 2 * usize::from(components))
                        .ok_or_else(|| Error::corrupt("Hasselblad: short scan header"))?;
                    let table =
                        id0.or(first).ok_or_else(|| Error::corrupt("Hasselblad: scan without a Huffman table"))?;
                    return Ok(HasselbladHeader { table, predictor: u32::from(ss), width, height, scan_offset: end });
                }
                _ => {}
            }
            pos = end;
        }
    }
}

/// Read one pair: two difference lengths, then the two differences (type 4 in the spec).
/// `None` for a bit pattern that is no code, or a length above 16.
#[inline(always)]
fn read_pair(bits: &mut BitPumpMsb32<'_>, table: &HuffTable) -> Option<(i32, i32)> {
    let n0 = table.decode_symbol(bits).ok()?;
    let n1 = table.decode_symbol(bits).ok()?;
    if n0.max(n1) > 16 {
        return None;
    }
    bits.fill();
    Some((read_diff(bits, n0), read_diff(bits, n1)))
}

/// `n` raw bits as a T.81 signed difference; the 16-bit all-ones pattern means -32768.
#[inline(always)]
fn read_diff(bits: &mut BitPumpMsb32<'_>, n: u32) -> i32 {
    let v = bits.peek_no_fill(n) as i32;
    bits.skip_no_fill(n);
    // The top bit clear means negative: subtract 2^n - 1 (zero for n = 0, where v = 0).
    let top = (v >> (n.max(1) - 1)) & 1;
    let d = v - ((top ^ 1) * ((1i32 << n) - 1));
    if d == 0xFFFF { -0x8000 } else { d }
}

fn validate(data: &[u8], hdr: &HasselbladHeader, out: &[u16], width: usize, height: usize) -> Result<Option<usize>> {
    if width % 2 != 0 {
        return Err(Error::unsupported_compression("Hasselblad", "odd raster width"));
    }
    let total = width.checked_mul(height).filter(|&t| t <= out.len());
    let Some(total) = total else {
        return Err(Error::InvalidArgument("Hasselblad: output buffer too small".into()));
    };
    if hdr.scan_offset > data.len() {
        return Err(Error::corrupt("Hasselblad: scan starts past the data"));
    }
    Ok((total != 0).then_some(total))
}

/// Decode the stream into `out` (`width x height`, row-major). `base` is added to the
/// row-start prediction `0x8000`.
pub fn decode(
    data: &[u8],
    hdr: &HasselbladHeader,
    out: &mut [u16],
    width: usize,
    height: usize,
    base: i32,
) -> Result<()> {
    let Some(total) = validate(data, hdr, out, width, height)? else { return Ok(()) };
    let scan = &data[hdr.scan_offset..];
    let start = 0x8000i32.wrapping_add(base);
    if hdr.predictor == PREDICTOR_GRADIENT {
        decode_gradient(scan, &hdr.table, &mut out[..total], width, start)
    } else {
        decode_lanes(scan, &hdr.table, &mut out[..total], width, start as u16)
    }
}

fn truncated() -> Error {
    Error::corrupt("Hasselblad: data ends inside the image")
}

/// Plain prediction: each lane of each row is a running sum from `start`.
fn decode_lanes(scan: &[u8], table: &HuffTable, out: &mut [u16], width: usize, start: u16) -> Result<()> {
    let mut bits = BitPumpMsb32::new(scan);
    for row in out.chunks_exact_mut(width) {
        let (mut even, mut odd) = (start, start);
        for px in row.chunks_exact_mut(2) {
            let (d0, d1) = read_pair(&mut bits, table).ok_or_else(truncated)?;
            even = even.wrapping_add(d0 as u16);
            odd = odd.wrapping_add(d1 as u16);
            px[0] = even;
            px[1] = odd;
        }
        if bits.exhausted() {
            return Err(truncated());
        }
    }
    Ok(())
}

/// Predictor 11: the prediction adds half the vertical gradient two rows up to the left
/// neighbour. The history is kept unreduced, in three rolling rows.
fn decode_gradient(scan: &[u8], table: &HuffTable, out: &mut [u16], width: usize, start: i32) -> Result<()> {
    let mut bits = BitPumpMsb32::new(scan);
    let mut hist = vec![0i32; 3 * width];
    for (r, row) in out.chunks_exact_mut(width).enumerate() {
        // Row r-2 shares a slot with row r+1 (mod 3).
        let cur_at = (r % 3) * width;
        let up_at = ((r + 1) % 3) * width;
        for col in (0..width).step_by(2) {
            let (n0, n1) = read_pair(&mut bits, table).ok_or_else(truncated)?;
            for (k, d) in [n0, n1].into_iter().enumerate() {
                let x = col + k;
                let pred = if x < 2 {
                    start
                } else if r < 2 {
                    hist[cur_at + x - 2]
                } else {
                    hist[cur_at + x - 2].wrapping_add(hist[up_at + x] / 2).wrapping_sub(hist[up_at + x - 2] / 2)
                };
                let h = pred.wrapping_add(d);
                hist[cur_at + x] = h;
                row[x] = h as u16;
            }
        }
        if bits.exhausted() {
            return Err(truncated());
        }
    }
    Ok(())
}

/// The scan as a speculatively decodable stream of pairs.
struct PairScan<'a> {
    scan: &'a [u8],
    table: &'a HuffTable,
}

impl SymbolStream for PairScan<'_> {
    type Pump<'p>
        = BitPumpMsb32<'p>
    where
        Self: 'p;
    const WIDTH: usize = 2;
    fn cycle(&self) -> usize {
        1
    }
    fn period(&self) -> usize {
        1
    }
    fn total_bits(&self) -> u64 {
        self.scan.len() as u64 * 8
    }
    fn end_check(&self, pos: u64) -> EndCheck {
        // Sequential decoding is exhausted once it has consumed more than 8 padding bytes;
        // up to 8 bytes of look-ahead are ordinary, the 64 bits between are history-dependent.
        let end = self.total_bits();
        if pos <= end {
            EndCheck::Valid
        } else if pos > end + 64 {
            EndCheck::Invalid
        } else {
            EndCheck::Unknown
        }
    }
    fn pump_at(&self, bit: u64) -> BitPumpMsb32<'_> {
        BitPumpMsb32::at_bit(self.scan, bit)
    }
    fn position(p: &BitPumpMsb32<'_>) -> u64 {
        p.bit_position()
    }
    #[inline(always)]
    fn decode(&self, p: &mut BitPumpMsb32<'_>, _phase: usize, out: &mut [u16]) -> bool {
        let before = p.bit_position();
        match read_pair(p, self.table) {
            Some((d0, d1)) => {
                out[0] = d0 as u16;
                out[1] = d1 as u16;
                true
            }
            None => {
                if p.bit_position() == before {
                    p.fill();
                    p.skip_no_fill(1);
                }
                false
            }
        }
    }
}

/// Try the multi-threaded path; `false` leaves `out` for the sequential decoder to fill.
fn decode_speculative(scan: &[u8], table: &HuffTable, out: &mut [u16], width: usize, start: u16) -> bool {
    let Some(chunk) = par_entropy::default_chunk(scan.len()) else { return false };
    let stream = PairScan { scan, table };
    let Some(diffs) = par_entropy::decode(&stream, out.len() / 2, chunk) else { return false };
    let rows_per_task = (out.len() / width).div_ceil(par::threads().max(1) * 4).max(1);
    par::for_each_row_chunk(out, width, rows_per_task, true, |r0, rows| {
        for (i, row) in rows.chunks_exact_mut(width).enumerate() {
            diffs.copy_to((r0 + i) * width, row);
            let (mut even, mut odd) = (start, start);
            for px in row.chunks_exact_mut(2) {
                even = even.wrapping_add(px[0]);
                odd = odd.wrapping_add(px[1]);
                px[0] = even;
                px[1] = odd;
            }
        }
    });
    true
}

/// [`decode`], using all threads for large images when `parallel` is set. Output and errors
/// are identical to [`decode`].
pub fn decode_par(
    data: &[u8],
    hdr: &HasselbladHeader,
    out: &mut [u16],
    width: usize,
    height: usize,
    base: i32,
    parallel: bool,
) -> Result<()> {
    let Some(total) = validate(data, hdr, out, width, height)? else { return Ok(()) };
    if parallel && hdr.predictor != PREDICTOR_GRADIENT && total >= PARALLEL_MIN_SAMPLES {
        let scan = &data[hdr.scan_offset..];
        let start = 0x8000i32.wrapping_add(base) as u16;
        if decode_speculative(scan, &hdr.table, &mut out[..total], width, start) {
            return Ok(());
        }
    }
    decode(data, hdr, out, width, height, base)
}
