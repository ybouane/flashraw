//! Output helpers for the CLI.

use flashraw::{
    CorruptReason, CorruptionPolicy, DecodeOptions, DecodeWarning, FlashRaw, FrameInfo, OrientationMetadata,
    OutputFormat, PreviewEncoding, PreviewInfo, RawFrame, Rect, Stage,
};
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;

pub fn frame_info_json(i: &FrameInfo) -> Value {
    json!({
        "width": i.width,
        "height": i.height,
        "cpp": i.cpp,
        "bits_per_sample": i.bits_per_sample,
        "sample_format": format!("{:?}", i.sample_format),
        "cfa": i.cfa.map(|c| c.to_string()),
        "layout": format!("{:?}", i.layout),
        "active_area": i.active_area,
        "crop": i.crop,
        "masked_areas": i.masked_areas,
        "black_level": i.black_level,
        "black_from_masked": i.black_from_masked,
        "white_level": i.white_level,
        "white_balance": i.white_balance,
        "color_matrices": i.color_matrices,
        "orientation": i.orientation,
        "compression": i.compression.name(),
        "opcodes": opcode_lists_json(i),
    })
}

/// Orientation records and how the resolved value was chosen (EXIF codes 1-8).
pub fn orientation_info_json(o: &OrientationMetadata) -> Value {
    json!({
        "exif": o.exif.map(|v| v.exif()),
        "maker_note": o.maker_note.map(|v| v.exif()),
        "auto_rotate": o.auto_rotate,
        "resolved": o.resolved.exif(),
        "source": o.source.name(),
        "policy": o.policy.name(),
    })
}

/// Opcode names per DNG opcode list (`null` without lists).
fn opcode_lists_json(i: &FrameInfo) -> Value {
    use flashraw::low_level::codecs::dng_opcodes;
    let Some(l) = &i.opcodes else { return Value::Null };
    let names = |b: &[u8]| -> Value {
        if b.len() <= 4 {
            return json!([]);
        }
        match dng_opcodes::parse(b) {
            Ok(ops) => json!(ops.iter().map(|o| o.name()).collect::<Vec<_>>()),
            Err(e) => json!(format!("unparsable: {e}")),
        }
    };
    json!({ "list1": names(&l.lists[0]), "list2": names(&l.lists[1]), "list3": names(&l.lists[2]) })
}

pub fn info_json(path: &Path, raw: &FlashRaw) -> Value {
    let m = raw.metadata();
    json!({
        "file": path,
        "format": raw.format().name(),
        "camera": m.camera(),
        "make": m.make,
        "model": m.model,
        "canonical_make": m.canonical_make,
        "canonical_model": m.canonical_model,
        "mode": m.extra("mode"),
        "compression": m.compression.map(|c| c.name()),
        "bits_per_sample": m.bits_per_sample,
        "raw_width": m.raw_width,
        "raw_height": m.raw_height,
        "width": m.width,
        "height": m.height,
        "orientation": m.orientation.exif(),
        "orientation_info": orientation_info_json(&m.orientation_info),
        "frame": frame_info_json(raw.frame_info()),
        "iso": m.exposure.iso,
        "exposure_time": m.exposure.exposure_time,
        "f_number": m.exposure.f_number,
        "focal_length": m.exposure.focal_length,
        "lens": m.lens.model,
        "capture_time": m.capture_time,
        "previews": m.previews,
        "capabilities": raw.capabilities(),
        "parse_ms": raw.parse_ms(),
    })
}

pub fn print_info(path: &Path, raw: &FlashRaw) {
    let m = raw.metadata();
    let i = raw.frame_info();
    println!("{}", path.display());
    println!("  Format:        {} ({})", raw.format().name(), i.compression.name());
    println!(
        "  Camera:        {}   [make={:?} model={:?}{}]",
        m.camera(),
        m.make,
        m.model,
        m.extra("mode").map(|v| format!(" mode={v}")).unwrap_or_default()
    );
    println!(
        "  Sensor buffer: {}x{} x{} @ {} bit{}",
        i.width,
        i.height,
        i.cpp,
        i.bits_per_sample,
        if i.sample_format == flashraw::SampleFormat::F32 { " float" } else { "" }
    );
    println!("  Active area:   {}", i.active_area);
    println!("  Default crop:  {}", i.crop);
    println!("  Layout:        {:?} {}", i.layout, i.cfa.map(|c| c.to_string()).unwrap_or_default());
    println!(
        "  Black/White:   {}{} / {:?}",
        if i.black_level.is_uniform() {
            format!("{}", i.black_level.mean())
        } else {
            format!("{:?}", i.black_level.values)
        },
        if i.black_from_masked { " (measured from masked area at decode)" } else { "" },
        i.white_level.values
    );
    if let Some(wb) = i.white_balance {
        println!("  WB (as shot):  {:.4} {:.4} {:.4} {:.4}", wb.coeffs[0], wb.coeffs[1], wb.coeffs[2], wb.coeffs[3]);
    }
    if let Some(cm) = i.color_matrices.preferred() {
        println!("  Color matrix:  {:?} illuminant {} ({:?})", cm.values, cm.illuminant.0, i.color_matrices.source);
    }
    let o = &m.orientation_info;
    let rec = |v: Option<flashraw::Orientation>| v.map(|v| v.exif().to_string()).unwrap_or("-".into());
    println!(
        "  Orientation:   {:?} (from {}; EXIF {}, maker note {}{}; policy {})",
        i.orientation,
        o.source.name(),
        rec(o.exif),
        rec(o.maker_note),
        o.auto_rotate.map(|a| format!(", auto-rotate {}", if a { "on" } else { "off" })).unwrap_or_default(),
        o.policy.name()
    );
    let x = &m.exposure;
    println!(
        "  Exposure:      ISO {} {} f/{} {}mm",
        x.iso.map(|v| v.to_string()).unwrap_or("-".into()),
        x.exposure_time
            .map(|t| if t < 1.0 { format!("1/{:.0}s", 1.0 / t) } else { format!("{t}s") })
            .unwrap_or("-".into()),
        x.f_number.map(|v| format!("{v:.1}")).unwrap_or("-".into()),
        x.focal_length.map(|v| format!("{v:.0}")).unwrap_or("-".into())
    );
    if let Some(l) = &m.lens.model {
        println!("  Lens:          {l}");
    }
    if let Some(t) = &m.capture_time {
        println!("  Captured:      {t}");
    }
    for p in &m.previews {
        println!(
            "  Preview:       {:?} {}x{} {:?} ({} bytes @ {:#x})",
            p.kind, p.width, p.height, p.encoding, p.length, p.offset
        );
    }
    let c = raw.capabilities();
    println!("  Random access: {:?} ({} units)", c.random_access, c.units);
    println!("  Parse time:    {:.2} ms", raw.parse_ms());
}

pub fn print_profile(f: &RawFrame) {
    let s = &f.stats;
    let mut accounted = 0.0;
    for (stage, ms) in &s.stages {
        println!("{:<24}{:>9.2} ms", format!("{}:", stage.label()), ms);
        accounted += ms;
    }
    if s.total_ms > accounted + 0.05 {
        println!("{:<24}{:>9.2} ms", "Other:", s.total_ms - accounted);
    }
    println!("{:<24}{:>9.2} ms", "Total:", s.total_ms);
    println!();
    println!("Backend:");
    println!("  decode: {}", s.backend.decode);
    if let Some(p) = &s.backend.postprocess {
        println!("  postprocess: {p}");
    }
    if let Some(g) = &s.backend.gpu_adapter {
        println!("  gpu: {g}");
    }
}

fn u16_samples(f: &RawFrame) -> Result<std::borrow::Cow<'_, [u16]>, String> {
    if let Some(v) = f.as_u16() {
        return Ok(v.into());
    }
    if let Some(v) = f.as_f16_bits() {
        let to16 = |h: u16| (flashraw::process::f16_to_f32(h) * 65535.0).round().clamp(0.0, 65535.0) as u16;
        return Ok(v.iter().map(|&h| to16(h)).collect::<Vec<_>>().into());
    }
    if let Some(v) = f.as_f32() {
        let scale = if f.output == OutputFormat::RawF32 { 1.0 } else { 65535.0 };
        return Ok(v.iter().map(|x| (x * scale).round().clamp(0.0, 65535.0) as u16).collect::<Vec<_>>().into());
    }
    if let Some(v) = f.as_u8() {
        return Ok(v.iter().map(|&x| x as u16 * 257).collect::<Vec<_>>().into());
    }
    Err("frame has no host data".into())
}

pub fn write_frame(f: &RawFrame, out: &Path, fmt: &str) -> Result<(), String> {
    let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let mut w = std::io::BufWriter::new(file);
    let e = |e: std::io::Error| e.to_string();
    match fmt {
        "raw" | "u16" | "bin" => {
            // Raw dumps keep half floats as their bit patterns.
            let s = match f.as_f16_bits() {
                Some(v) => v.into(),
                None => u16_samples(f)?,
            };
            for v in s.iter() {
                w.write_all(&v.to_le_bytes()).map_err(e)?;
            }
        }
        "pgm" | "ppm" | "pnm" => {
            let s = u16_samples(f)?;
            let (magic, cpp) = match f.cpp {
                1 => ("P5", 1),
                3 => ("P6", 3),
                4 => ("P6", 4),
                c => return Err(format!("cannot write {c}-component frame as Netpbm")),
            };
            write!(w, "{magic}\n{} {}\n65535\n", f.width, f.height).map_err(e)?;
            for px in s.chunks(cpp) {
                for v in px.iter().take(3) {
                    w.write_all(&v.to_be_bytes()).map_err(e)?;
                }
            }
        }
        "tif" | "tiff" => {
            let s = u16_samples(f)?;
            write_tiff16(&mut w, f.width, f.height, f.cpp.min(3), f.cpp, &s).map_err(e)?;
        }
        "jpg" | "jpeg" => {
            let px = f.as_u8().ok_or("JPEG output requires --output rgba8")?;
            let enc = jpeg_encoder::Encoder::new(&mut w, 92);
            enc.encode(px, f.width as u16, f.height as u16, jpeg_encoder::ColorType::Rgba)
                .map_err(|e| e.to_string())?;
        }
        other => return Err(format!("unknown output format {other:?}")),
    }
    w.flush().map_err(e)
}

/// Minimal uncompressed 16-bit TIFF writer (grey or RGB).
fn write_tiff16(w: &mut impl Write, width: u32, height: u32, spp: u32, src_cpp: u32, s: &[u16]) -> std::io::Result<()> {
    let data_len = width as u64 * height as u64 * spp as u64 * 2;
    let n_entries: u16 = 10;
    let ifd_off: u32 = 8;
    let extra_off = ifd_off + 2 + n_entries as u32 * 12 + 4;
    let bps_off = extra_off;
    let data_off = bps_off + 8;
    w.write_all(b"II*\0")?;
    w.write_all(&ifd_off.to_le_bytes())?;
    w.write_all(&n_entries.to_le_bytes())?;
    let mut entry = |tag: u16, typ: u16, count: u32, val: u32| -> std::io::Result<()> {
        w.write_all(&tag.to_le_bytes())?;
        w.write_all(&typ.to_le_bytes())?;
        w.write_all(&count.to_le_bytes())?;
        w.write_all(&val.to_le_bytes())
    };
    entry(256, 4, 1, width)?;
    entry(257, 4, 1, height)?;
    if spp == 1 {
        entry(258, 3, 1, 16)?
    } else {
        entry(258, 3, 3, bps_off)?
    }
    entry(259, 3, 1, 1)?;
    entry(262, 3, 1, if spp == 1 { 1 } else { 2 })?;
    entry(273, 4, 1, data_off)?;
    entry(277, 3, 1, spp)?;
    entry(278, 4, 1, height)?;
    entry(279, 4, 1, data_len as u32)?;
    entry(284, 3, 1, 1)?;
    w.write_all(&0u32.to_le_bytes())?;
    for _ in 0..3 {
        w.write_all(&16u16.to_le_bytes())?;
    }
    w.write_all(&0u16.to_le_bytes())?;
    for px in s.chunks(src_cpp as usize) {
        for v in px.iter().take(spp as usize) {
            w.write_all(&v.to_le_bytes())?;
        }
    }
    Ok(())
}

pub fn write_preview(raw: &FlashRaw, p: &PreviewInfo, out: &Path) -> Result<(), String> {
    match p.encoding {
        PreviewEncoding::Jpeg | PreviewEncoding::Heif | PreviewEncoding::JpegXl => {
            let b = raw.preview_bytes(p).map_err(|e| e.to_string())?;
            std::fs::write(out, b).map_err(|e| e.to_string())
        }
        PreviewEncoding::Rgb8 | PreviewEncoding::Ycbcr422 => {
            let img = raw.decode_preview_rgb(p).map_err(|e| e.to_string())?;
            let (w, h, rgb) = (img.width, img.height, img.rgb);
            let f = std::fs::File::create(out).map_err(|e| e.to_string())?;
            let enc = jpeg_encoder::Encoder::new(std::io::BufWriter::new(f), 92);
            enc.encode(&rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb).map_err(|e| e.to_string())
        }
        e => Err(format!("cannot write a {e:?} preview")),
    }
}

fn stats(v: &mut [f64]) -> (f64, f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = v[v.len() / 2];
    (v[0], med, v[v.len() - 1])
}

pub fn benchmark(path: &Path, opts: &DecodeOptions, iterations: usize, as_json: bool) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut open_ms = Vec::new();
    let mut decode_ms = Vec::new();
    let mut stage_sum: std::collections::BTreeMap<Stage, f64> = Default::default();
    let mut last = None;
    // Warm-up (page cache, thread pool).
    {
        let raw = FlashRaw::open(&bytes[..]).map_err(|e| e.to_string())?;
        raw.decode(opts.clone()).map_err(|e| e.to_string())?;
    }
    for _ in 0..iterations {
        // Release the previous frame first so peak RSS reflects a single decode.
        drop(last.take());
        let t = std::time::Instant::now();
        let raw = FlashRaw::open(&bytes[..]).map_err(|e| e.to_string())?;
        open_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = std::time::Instant::now();
        let f = raw.decode(opts.clone()).map_err(|e| e.to_string())?;
        decode_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        for (s, ms) in &f.stats.stages {
            *stage_sum.entry(*s).or_default() += ms / iterations as f64;
        }
        last = Some(f);
    }
    let f = last.unwrap();
    let total: Vec<f64> = open_ms.iter().zip(&decode_ms).map(|(a, b)| a + b).collect();
    let (omin, omed, _) = stats(&mut open_ms.clone());
    let (dmin, dmed, dmax) = stats(&mut decode_ms.clone());
    let (tmin, tmed, _) = stats(&mut total.clone());
    let mp = f.width as f64 * f.height as f64 / 1e6;
    if as_json {
        println!(
            "{}",
            json!({
                "file": path, "format": f.metadata.format.map(|x| x.name()), "camera": f.metadata.camera(),
                "compression": f.metadata.compression.map(|c| c.name()), "megapixels": mp,
                "open_ms_median": omed, "open_ms_min": omin, "decode_ms_median": dmed, "decode_ms_min": dmin, "decode_ms_max": dmax,
                "total_ms_median": tmed, "total_ms_min": tmin, "mp_per_s": mp / (tmed / 1000.0),
                "maxrss_kb": maxrss_kb(),
                "backend": f.stats.backend, "opcodes": f.stats.opcodes, "stages": stage_sum.iter().map(|(s, v)| (s.label(), v)).collect::<std::collections::BTreeMap<_, _>>(),
            })
        );
    } else {
        println!(
            "{} — {} {} ({:.1} MP)",
            path.display(),
            f.metadata.camera(),
            f.metadata.compression.map(|c| c.name()).unwrap_or(""),
            mp
        );
        println!("  open    median {omed:8.2} ms   min {omin:8.2} ms");
        println!("  decode  median {dmed:8.2} ms   min {dmin:8.2} ms   max {dmax:8.2} ms");
        println!("  total   median {tmed:8.2} ms   ({:.0} MP/s)", mp / (tmed / 1000.0));
        for (s, v) in &stage_sum {
            println!("    {:<22}{v:8.2} ms", format!("{}:", s.label()));
        }
        println!("  backend: {}", f.stats.backend.decode);
        for n in &f.stats.opcodes {
            println!("  {n}");
        }
    }
    Ok(())
}

/// Hashes and geometry of the decoded data, for regression testing.
/// A structured corruption cause as JSON (`kind`, `unit`, `index`, and for truncation the
/// byte range: `offset`, `expected`, `available`, `file_len`).
pub fn reason_json(r: &CorruptReason) -> Value {
    let u = r.unit();
    let mut v = json!({"unit": u.kind(), "index": u.index()});
    match r {
        CorruptReason::Truncated(t) => {
            v["kind"] = json!("truncated");
            v["offset"] = json!(t.offset);
            v["expected"] = json!(t.expected);
            v["available"] = json!(t.available);
            v["file_len"] = json!(t.file_len);
        }
        CorruptReason::InvalidUnit { message, .. } | CorruptReason::PreviewFallback { message } => {
            v["kind"] = json!(r.kind());
            v["message"] = json!(message);
        }
        _ => v["kind"] = json!(r.kind()),
    }
    v
}

/// Recovery warnings as JSON: the reason fields plus `samples_filled` and `message`.
pub fn warnings_json(w: &[DecodeWarning]) -> Value {
    Value::Array(
        w.iter()
            .map(|w| {
                let mut v = reason_json(&w.reason);
                v["samples_filled"] = json!(w.samples_filled);
                v["message"] = json!(w.to_string());
                v
            })
            .collect(),
    )
}

/// Error JSON: `error`, `error_kind`, and `corrupt` (the structured cause) when known.
fn error_json(path: &Path, e: &flashraw::Error) -> Value {
    let mut v = json!({"file": path, "error": e.to_string(), "error_kind": e.kind()});
    if let Some(r) = e.corrupt_reason() {
        v["corrupt"] = reason_json(r);
    }
    v
}

pub fn verify(path: &Path, dump_dir: Option<&Path>, apply_opcodes: bool, policy: CorruptionPolicy) -> Value {
    let t0 = std::time::Instant::now();
    let raw = match FlashRaw::open_path(path) {
        Ok(r) => r,
        Err(e) => return error_json(path, &e),
    };
    let open_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let m = raw.metadata().clone();
    let info = raw.frame_info().clone();
    let t1 = std::time::Instant::now();
    // By default sensor data as stored, without DNG opcode lists applied, so the hashes
    // describe the payload decode alone.
    let frame = match raw.decode(DecodeOptions { apply_opcodes, corruption_policy: policy, ..Default::default() }) {
        Ok(f) => f,
        Err(e) => {
            let mut v = error_json(path, &e);
            v["format"] = json!(raw.format().name());
            v["make"] = json!(m.make);
            v["model"] = json!(m.model);
            v["frame"] = frame_info_json(&info);
            return v;
        }
    };
    let decode_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let full = Rect::full(frame.width, frame.height);
    let (mut mn, mut mx) = (u16::MAX, 0u16);
    // Active-area sample statistics: maximum and 99.99th percentile (robust to hot pixels),
    // used to cross-check white levels against real data.
    let (mut active_max, mut active_p9999) = (0u16, 0u16);
    if let Some(d) = frame.as_u16() {
        for &v in d {
            mn = mn.min(v);
            mx = mx.max(v);
        }
        let mut hist = vec![0u64; 65536];
        let a = info.active_area.clamp_to(frame.width, frame.height);
        let (w, cpp) = (frame.width as usize, frame.cpp as usize);
        for y in a.y as usize..a.bottom() as usize {
            for &v in &d[(y * w + a.x as usize) * cpp..(y * w + a.right() as usize) * cpp] {
                hist[v as usize] += 1;
            }
        }
        let total: u64 = hist.iter().sum();
        let mut acc = 0u64;
        for (v, &n) in hist.iter().enumerate() {
            if n > 0 {
                active_max = v as u16;
            }
            acc += n;
            if active_p9999 == 0 && total > 0 && acc * 10000 >= total * 9999 {
                active_p9999 = v as u16;
            }
        }
        if let Some(dir) = dump_dir {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let mut b = Vec::with_capacity(d.len() * 2);
            for v in d {
                b.extend_from_slice(&v.to_le_bytes());
            }
            let _ = std::fs::write(dir.join(format!("{name}.flashraw.u16")), b);
        }
    }
    // Per-colour black (R, G, B, G2) for comparisons.
    let mut black4 = [0f64; 4];
    let mut cnt4 = [0f64; 4];
    if let Some(cfa) = info.cfa {
        let mut seen_g = false;
        for y in 0..cfa.height().min(2) {
            for x in 0..cfa.width().min(2) {
                let c = cfa.color_at(info.active_area.x as usize + x, info.active_area.y as usize + y) as usize;
                let idx = if c == 1 && seen_g { 3 } else { c.min(3) };
                if c == 1 {
                    seen_g = true;
                }
                black4[idx] +=
                    frame.black_level.at(info.active_area.x as usize + x, info.active_area.y as usize + y, 0) as f64;
                cnt4[idx] += 1.0;
            }
        }
    }
    for i in 0..4 {
        if cnt4[i] > 0.0 {
            black4[i] /= cnt4[i];
        } else {
            black4[i] = frame.black_level.mean() as f64;
        }
    }
    json!({
        "file": path,
        "format": raw.format().name(),
        "make": m.make, "model": m.model,
        "canonical_make": m.canonical_make, "canonical_model": m.canonical_model,
        "mode": m.extra("mode"),
        "compression": info.compression.name(),
        "raw_width": frame.width, "raw_height": frame.height, "cpp": frame.cpp,
        "active_area": info.active_area, "crop": info.crop,
        "cfa_visible": info.cfa.map(|c| c.shifted(info.active_area.x as usize, info.active_area.y as usize).to_string()),
        "cfa": info.cfa.map(|c| c.to_string()),
        "layout": format!("{:?}", info.layout),
        "black4": black4,
        // 2x2 black pattern at the active-area origin (row-major), for positional comparison.
        "black_visible": (0..4).map(|i| frame.black_level.at(info.active_area.x as usize + i % 2, info.active_area.y as usize + i / 2, 0)).collect::<Vec<_>>(),
        "black_level": frame.black_level,
        // From the frame, like the black level: float data converted to integers report
        // both in the converted units.
        "white": frame.white_level.max(),
        "wb": info.white_balance.map(|w| w.coeffs),
        "color_matrix": info.color_matrices.preferred().map(|c| c.values.clone()),
        "orientation": info.orientation.exif(),
        "orientation_info": orientation_info_json(&m.orientation_info),
        "preview_orientations": m.previews.iter().map(|p| p.orientation.exif()).collect::<Vec<_>>(),
        "bits": info.bits_per_sample,
        "iso": m.exposure.iso,
        "previews": m.previews.len(),
        "min": mn, "max": mx, "active_max": active_max, "active_p9999": active_p9999,
        "hash_full": frame.hash_u16(full).map(|h| format!("{h:016x}")),
        "hash_active": frame.hash_u16(info.active_area).map(|h| format!("{h:016x}")),
        "open_ms": open_ms, "decode_ms": decode_ms,
        "incomplete": frame.is_incomplete(),
        "warnings": warnings_json(frame.warnings()),
    })
}

pub fn supported(as_json: bool) {
    let rows = flashraw_support_matrix();
    if as_json {
        println!("{}", serde_json::to_string_pretty(&rows).unwrap());
        return;
    }
    println!("{:<8} {:<28} {:<26} STATUS", "FORMAT", "MODE", "COMPRESSION");
    for r in &rows {
        println!(
            "{:<8} {:<28} {:<26} {}",
            r["format"].as_str().unwrap_or(""),
            r["mode"].as_str().unwrap_or(""),
            r["compression"].as_str().unwrap_or(""),
            r["status"].as_str().unwrap_or("")
        );
    }
    println!(
        "\n{} camera database entries. See docs/formats.md for the full support matrix.",
        flashraw_camera_db::all().len()
    );
}

fn flashraw_support_matrix() -> Vec<Value> {
    let text = include_str!("../../../docs/support-matrix.tsv");
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            json!({"format": c.first(), "mode": c.get(1), "compression": c.get(2), "status": c.get(3), "notes": c.get(4)})
        })
        .collect()
}

/// Peak resident set size of this process in KiB (0 where unavailable).
pub fn maxrss_kb() -> i64 {
    #[cfg(unix)]
    {
        // SAFETY: getrusage writes into the zero-initialised struct we pass; no other invariants.
        unsafe {
            let mut ru: libc::rusage = std::mem::zeroed();
            if libc::getrusage(libc::RUSAGE_SELF, &mut ru) == 0 {
                // `ru_maxrss` is in bytes on Apple platforms and in KiB elsewhere.
                #[allow(clippy::useless_conversion)]
                let v = i64::from(ru.ru_maxrss);
                return if cfg!(target_vendor = "apple") { v / 1024 } else { v };
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Little-endian 8x6 RGGB DNG with 32-bit float samples in [0, 0.47], white 1.0 and
    /// black 0.01.
    fn float_dng() -> Vec<u8> {
        let (w, h) = (8u32, 6u32);
        let payload: Vec<u8> = (0..w * h).flat_map(|i| (i as f32 / 100.0).to_le_bytes()).collect();
        let short = |v: u16| (3u16, 1u32, (v as u32).to_le_bytes().to_vec());
        let long = |v: u32| (4u16, 1u32, v.to_le_bytes().to_vec());
        let mut entries = vec![
            (0x00FEu16, long(0)),
            (0x0100, long(w)),
            (0x0101, long(h)),
            (0x0102, short(32)),
            (0x0103, short(1)),
            (0x0106, short(32803)),
            (0x0115, short(1)),
            (0x0116, long(h)),
            (0x0117, long(payload.len() as u32)),
            (0x0153, short(3)),
            (0x828D, (3, 2, [2u16.to_le_bytes(), 2u16.to_le_bytes()].concat())),
            (0x828E, (1, 4, vec![0, 1, 1, 2])),
            (0xC612, (1, 4, vec![1, 4, 0, 0])),
            (0xC61A, (5, 1, [1u32.to_le_bytes(), 100u32.to_le_bytes()].concat())),
            (0xC61D, long(1)),
        ];
        let ifd_len = 2 + (entries.len() + 1) * 12 + 4;
        let extra: Vec<u8> = entries.iter().filter(|e| e.1.2.len() > 4).flat_map(|e| e.1.2.clone()).collect();
        let strip = (8 + ifd_len + extra.len()) as u32;
        entries.push((0x0111, long(strip)));
        entries.sort_by_key(|e| e.0);
        let mut out = b"II*\0\x08\0\0\0".to_vec();
        out.extend((entries.len() as u16).to_le_bytes());
        let mut at = 8 + ifd_len;
        for (tag, (typ, count, data)) in &entries {
            out.extend(tag.to_le_bytes());
            out.extend(typ.to_le_bytes());
            out.extend(count.to_le_bytes());
            if data.len() <= 4 {
                let mut d = data.clone();
                d.resize(4, 0);
                out.extend(d);
            } else {
                out.extend((at as u32).to_le_bytes());
                at += data.len();
            }
        }
        out.extend(0u32.to_le_bytes());
        out.extend(extra);
        assert_eq!(out.len(), strip as usize);
        out.extend(payload);
        out
    }

    #[test]
    fn verify_reports_float_levels_in_frame_units() {
        let dir = std::env::temp_dir().join(format!("flashraw-cli-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("float.dng");
        std::fs::write(&path, float_dng()).unwrap();
        let v = verify(&path, None, false, CorruptionPolicy::default());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(v.get("error").is_none(), "{v}");
        // Floats in [0, 1] are scaled to 16383 for the integer output; white and black follow.
        let white = v["white"].as_u64().unwrap();
        assert_eq!(white, 16383, "{v}");
        let black = v["black4"][0].as_f64().unwrap();
        assert!((black - 163.0).abs() <= 1.0, "{v}");
        assert!(v["max"].as_u64().unwrap() <= white, "{v}");
    }
}
