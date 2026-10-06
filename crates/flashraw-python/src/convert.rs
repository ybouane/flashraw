//! Plain-data views of FlashRaw metadata for Python.
//!
//! Everything is built as a `serde_json::Value` (snake_case keys, enums as lowercase strings,
//! no tuples) and converted to Python dicts/lists in one pass by [`to_py`]. The Python layer
//! (`python/flashraw/_types.py`) turns these dicts into typed, frozen dataclasses.

use flashraw::{
    BlackLevel, CfaPattern, ColorMatrices, ColorMatrix, ColorSource, CorruptReason, DecodeUnit, DecodeWarning,
    DngExtras, FlashRaw, FrameInfo, OrientationMetadata, OutputFormat, PreviewEncoding, PreviewInfo, PreviewKind,
    RandomAccess, RawFrame, RawMetadata, Rect, SampleFormat, SensorLayout, Stage, WhiteBalance, WhiteLevel,
};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde_json::{Map, Value, json};

/// Convert a JSON value to the equivalent Python object (dict / list / int / float / str /
/// bool / None).
pub fn to_py<'py>(py: Python<'py>, v: &Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match v {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => b.into_pyobject(py)?.to_owned().into_any(),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                u.into_pyobject(py)?.into_any()
            } else if let Some(i) = n.as_i64() {
                i.into_pyobject(py)?.into_any()
            } else {
                n.as_f64().unwrap_or(f64::NAN).into_pyobject(py)?.into_any()
            }
        }
        Value::String(s) => s.into_pyobject(py)?.into_any(),
        Value::Array(a) => {
            let items = a.iter().map(|x| to_py(py, x)).collect::<PyResult<Vec<_>>>()?;
            PyList::new(py, items)?.into_any()
        }
        Value::Object(m) => {
            let d = PyDict::new(py);
            for (k, x) in m {
                d.set_item(k, to_py(py, x)?)?;
            }
            d.into_any()
        }
    })
}

/// Finite numbers only (garbage metadata may contain NaN/Infinity).
fn num(v: f64) -> Value {
    if v.is_finite() { json!(v) } else { Value::Null }
}

/// f32 via its shortest decimal representation (0.6719 stays 0.6719, not 0.67189997).
fn f32v(v: f32) -> Value {
    if !v.is_finite() {
        return Value::Null;
    }
    v.to_string().parse::<f64>().map(|d| json!(d)).unwrap_or(Value::Null)
}

fn f32s(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|&x| f32v(x)).collect())
}

fn nums(v: &[f64]) -> Value {
    Value::Array(v.iter().map(|&x| num(x)).collect())
}

fn opt_num(v: Option<f64>) -> Value {
    v.map(num).unwrap_or(Value::Null)
}

fn opt_f32(v: Option<f32>) -> Value {
    v.map(f32v).unwrap_or(Value::Null)
}

fn opt_str(s: &Option<String>) -> Value {
    s.as_deref().map(|s| json!(s)).unwrap_or(Value::Null)
}

fn pair(p: Option<(f32, f32)>) -> Value {
    p.map(|(a, b)| json!([f32v(a), f32v(b)])).unwrap_or(Value::Null)
}

pub fn rect(r: Rect) -> Value {
    json!({ "x": r.x, "y": r.y, "width": r.width, "height": r.height })
}

fn rects(r: &[Rect]) -> Value {
    Value::Array(r.iter().map(|r| rect(*r)).collect())
}

fn cfa(p: Option<CfaPattern>) -> Value {
    match p {
        None => Value::Null,
        Some(p) => json!({
            "pattern": p.colors().iter().map(|c| c.letter()).collect::<String>(),
            "width": p.width(),
            "height": p.height(),
        }),
    }
}

fn layout(l: SensorLayout) -> &'static str {
    match l {
        SensorLayout::Bayer => "bayer",
        SensorLayout::XTrans => "x-trans",
        SensorLayout::QuadBayer => "quad-bayer",
        SensorLayout::OtherCfa => "other-cfa",
        SensorLayout::Monochrome => "monochrome",
        SensorLayout::Linear => "linear",
        SensorLayout::Foveon => "foveon",
        _ => "unknown",
    }
}

fn black(b: &BlackLevel) -> Value {
    json!({
        "rows": b.rows,
        "cols": b.cols,
        "samples": b.samples,
        "values": f32s(&b.values),
        "mean": f32v(b.mean()),
        "row_deltas": f32s(&b.row_deltas),
        "col_deltas": f32s(&b.col_deltas),
    })
}

fn white(w: &WhiteLevel) -> Value {
    json!(w.values)
}

fn wb(w: Option<WhiteBalance>) -> Value {
    w.map(|w| f32s(&w.coeffs)).unwrap_or(Value::Null)
}

fn matrix(m: &ColorMatrix) -> Value {
    json!({
        "illuminant": m.illuminant.0,
        "temperature": m.illuminant.temperature().map(f32v).unwrap_or(Value::Null),
        "rows": m.rows,
        "values": f32s(&m.values),
    })
}

fn colors(c: &ColorMatrices) -> Value {
    json!({
        "color": c.color.iter().map(matrix).collect::<Vec<_>>(),
        "forward": c.forward.iter().map(matrix).collect::<Vec<_>>(),
        "calibration": c.calibration.iter().map(matrix).collect::<Vec<_>>(),
        "analog_balance": f32s(&c.analog_balance),
        "source": match c.source {
            ColorSource::None => "none",
            ColorSource::File => "file",
            ColorSource::CameraDb => "camera-db",
            _ => "unknown",
        },
    })
}

fn preview_mime(e: PreviewEncoding) -> &'static str {
    match e {
        PreviewEncoding::Jpeg => "image/jpeg",
        PreviewEncoding::JpegXl => "image/jxl",
        PreviewEncoding::Heif => "image/heif",
        PreviewEncoding::Rgb8 => "application/x-flashraw-rgb8",
        PreviewEncoding::Ycbcr422 => "application/x-flashraw-ycbcr422",
        _ => "application/octet-stream",
    }
}

pub fn preview(i: usize, p: &PreviewInfo) -> Value {
    json!({
        "index": i,
        "kind": match p.kind {
            PreviewKind::Thumbnail => "thumbnail",
            PreviewKind::Preview => "preview",
            PreviewKind::FullSize => "full-size",
            _ => "unknown",
        },
        "encoding": match p.encoding {
            PreviewEncoding::Jpeg => "jpeg",
            PreviewEncoding::Rgb8 => "rgb8",
            PreviewEncoding::Ycbcr422 => "ycbcr422",
            PreviewEncoding::JpegXl => "jpeg-xl",
            PreviewEncoding::Heif => "heif",
            _ => "unknown",
        },
        "mime_type": preview_mime(p.encoding),
        "width": p.width,
        "height": p.height,
        "byte_length": p.length,
        "offset": p.offset,
        "orientation": p.orientation.exif(),
    })
}

fn previews(m: &RawMetadata) -> Value {
    Value::Array(m.previews.iter().enumerate().map(|(i, p)| preview(i, p)).collect())
}

fn compression_name(f: &FrameInfo, m: &RawMetadata) -> &'static str {
    m.compression.unwrap_or(f.compression).name()
}

/// Geometry and calibration of the sensor buffer (`RawFile.frame_info`).
pub fn frame_info(f: &FrameInfo) -> Value {
    json!({
        "width": f.width,
        "height": f.height,
        "channels": f.cpp,
        "bits_per_sample": f.bits_per_sample,
        "sample_format": match f.sample_format { SampleFormat::U16 => "u16", SampleFormat::F32 => "f32", _ => "unknown" },
        "cfa": cfa(f.cfa),
        "layout": layout(f.layout),
        "active_area": rect(f.active_area),
        "crop": rect(f.crop),
        "masked_areas": rects(&f.masked_areas),
        "black_level": black(&f.black_level),
        "black_from_masked": f.black_from_masked,
        "white_level": white(&f.white_level),
        "white_balance": wb(f.white_balance),
        "color_matrices": colors(&f.color_matrices),
        "orientation": f.orientation.exif(),
        "pixel_aspect": f32v(f.pixel_aspect),
        "compression": f.compression.name(),
    })
}

fn orientation_info(o: &OrientationMetadata) -> Value {
    json!({
        "exif": o.exif.map(|v| v.exif()),
        "maker_note": o.maker_note.map(|v| v.exif()),
        "auto_rotate": o.auto_rotate,
        "resolved": o.resolved.exif(),
        "source": o.source.name(),
        "policy": o.policy.name(),
    })
}

/// The full metadata object (`RawFile.metadata`).
pub fn metadata(raw: &FlashRaw<'_>) -> Value {
    let m = raw.metadata();
    let f = raw.frame_info();
    let e = &m.exposure;
    let mut extra = Map::new();
    for (k, v) in &m.extra {
        extra.insert(k.clone(), json!(v));
    }
    json!({
        "format": raw.format().name(),
        "camera": {
            "name": m.camera(),
            "make": m.make,
            "model": m.model,
            "canonical_make": m.canonical_make,
            "canonical_model": m.canonical_model,
            "unique_model": opt_str(&m.unique_camera_model),
            "serial": opt_str(&m.serial),
            "firmware": opt_str(&m.firmware),
        },
        "software": opt_str(&m.software),
        "artist": opt_str(&m.artist),
        "copyright": opt_str(&m.copyright),
        "lens": {
            "make": opt_str(&m.lens.make),
            "model": opt_str(&m.lens.model),
            "serial": opt_str(&m.lens.serial),
            "focal_range": pair(m.lens.focal_range),
            "aperture_range": pair(m.lens.aperture_range),
        },
        "exposure": {
            "iso": e.iso,
            "exposure_time": opt_num(e.exposure_time),
            "f_number": opt_f32(e.f_number),
            "focal_length": opt_f32(e.focal_length),
            "focal_length_35mm": e.focal_length_35mm,
            "exposure_bias": opt_f32(e.exposure_bias),
            "flash": e.flash,
            "metering_mode": e.metering_mode,
            "exposure_program": e.exposure_program,
        },
        "capture_time": opt_str(&m.capture_time),
        "gps": m.gps.as_ref().map(|g| json!({
            "latitude": opt_num(g.latitude),
            "longitude": opt_num(g.longitude),
            "altitude": opt_num(g.altitude),
            "timestamp": opt_str(&g.timestamp),
        })).unwrap_or(Value::Null),
        "orientation": m.orientation.exif(),
        "orientation_info": orientation_info(&m.orientation_info),
        "width": m.width,
        "height": m.height,
        "raw_width": m.raw_width,
        "raw_height": m.raw_height,
        "bits_per_sample": m.bits_per_sample,
        "compression": compression_name(f, m),
        "dng_version": m.dng_version.map(|v| format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3])),
        "cfa": cfa(f.cfa),
        "layout": layout(f.layout),
        "black_level": black(&f.black_level),
        "white_level": white(&f.white_level),
        "white_balance": wb(f.white_balance),
        "color_matrices": colors(&f.color_matrices),
        "previews": previews(m),
        "extra": extra,
        "dng": m.dng.as_ref().map(dng).unwrap_or(Value::Null),
        "opcodes": opcodes(f),
    })
}

/// DNG rendering hints (`metadata.dng`).
fn dng(x: &DngExtras) -> Value {
    let p = &x.profile;
    json!({
        "baseline_exposure": opt_num(x.baseline_exposure),
        "baseline_noise": opt_num(x.baseline_noise),
        "baseline_sharpness": opt_num(x.baseline_sharpness),
        "linear_response_limit": opt_num(x.linear_response_limit),
        "as_shot_neutral": x.as_shot_neutral.as_deref().map(nums).unwrap_or(Value::Null),
        "as_shot_white_xy": x.as_shot_white_xy.map(|(a, b)| json!([num(a), num(b)])).unwrap_or(Value::Null),
        "default_user_crop": x.default_user_crop.map(|c| nums(&c)).unwrap_or(Value::Null),
        "default_scale": x.default_scale.map(|(a, b)| json!([num(a), num(b)])).unwrap_or(Value::Null),
        "noise_profile": x.noise_profile.as_deref().map(nums).unwrap_or(Value::Null),
        "profile": {
            "name": opt_str(&p.name),
            "embed_policy": p.embed_policy,
            "hue_sat_map_dims": p.hue_sat_map_dims.map(|d| json!(d)).unwrap_or(Value::Null),
            "hue_sat_map_count": p.hue_sat_map_count,
            "look_table_dims": p.look_table_dims.map(|d| json!(d)).unwrap_or(Value::Null),
            "tone_curve": p.tone_curve,
            "baseline_exposure_offset": opt_num(p.baseline_exposure_offset),
        },
    })
}

/// The DNG opcode lists, with the parameters of the opcodes an editor applies itself.
/// Unparsable lists are left out (decoding reports them).
fn opcodes(f: &FrameInfo) -> Value {
    use flashraw::low_level::codecs::dng_opcodes::{Opcode, parse_entries};
    let Some(lists) = &f.opcodes else { return json!([]) };
    let mut out = Vec::new();
    for (i, bytes) in lists.lists.iter().enumerate() {
        if bytes.len() <= 4 {
            continue;
        }
        let Ok(entries) = parse_entries(bytes) else { continue };
        for e in entries {
            let params = match &e.op {
                Opcode::WarpRectilinear { planes, cx, cy } if !planes.is_empty() => json!({
                    "kind": "WarpRectilinear",
                    "planes": planes.iter().map(|(kr, kt)| json!({ "kr": nums(kr), "kt": nums(kt) })).collect::<Vec<_>>(),
                    "center": [num(*cx), num(*cy)],
                }),
                Opcode::FixVignetteRadial { k, cx, cy } => json!({
                    "kind": "FixVignetteRadial",
                    "k": nums(k),
                    "center": [num(*cx), num(*cy)],
                }),
                _ => Value::Null,
            };
            out.push(json!({
                "list": i + 1,
                "id": e.id,
                "name": match &e.op { Opcode::Other { .. } => format!("Opcode{}", e.id), op => op.name().to_string() },
                "optional": e.optional,
                "preview_only": e.preview_only,
                "applied": e.op.is_pixel_opcode(),
                "params": params,
            }));
        }
    }
    Value::Array(out)
}

fn random_access(r: RandomAccess) -> Value {
    match r {
        RandomAccess::Sequential => json!({ "kind": "sequential" }),
        RandomAccess::Rows => json!({ "kind": "rows" }),
        RandomAccess::Strips { rows } => json!({ "kind": "strips", "rows": rows }),
        RandomAccess::Tiles { width, height } => json!({ "kind": "tiles", "width": width, "height": height }),
        RandomAccess::Blocks => json!({ "kind": "blocks" }),
        _ => json!({ "kind": "unknown" }),
    }
}

/// What the file supports (`RawFile.capabilities`).
pub fn capabilities(raw: &FlashRaw<'_>) -> Value {
    let c = raw.capabilities();
    json!({
        "random_access": random_access(c.random_access),
        "tiles": c.units,
        "previews": c.previews,
        "gpu_unpack": c.gpu_unpack,
        "float": c.float,
    })
}

/// Independently decodable tiles/strips (`RawFile.tiles`).
pub fn units(u: &[DecodeUnit]) -> Value {
    Value::Array(
        u.iter()
            .enumerate()
            .map(|(i, u)| json!({ "index": i, "rect": rect(u.rect), "offset": u.offset, "byte_length": u.length }))
            .collect(),
    )
}

fn stage_key(s: Stage) -> &'static str {
    match s {
        Stage::ContainerParse => "container_parse",
        Stage::Metadata => "metadata",
        Stage::EntropyDecode => "entropy_decode",
        Stage::Unpack => "unpack",
        Stage::Predictor => "predictor",
        Stage::Calibration => "calibration",
        Stage::Postprocess => "postprocess",
        Stage::GpuUpload => "gpu_upload",
        Stage::GpuCompute => "gpu_compute",
        Stage::GpuDownload => "gpu_download",
        Stage::Preview => "preview",
        _ => "unknown",
    }
}

fn output_name(o: OutputFormat) -> &'static str {
    o.host_equivalent().name()
}

/// Everything about a decoded frame except its pixels.
pub fn frame(f: &RawFrame, requested_backend: &str) -> Value {
    let s = &f.stats;
    let mut stages = Map::new();
    for (st, ms) in &s.stages {
        let key = stage_key(*st);
        let prev = stages.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        stages.insert(key.into(), num(prev + *ms));
    }
    json!({
        "width": f.width,
        "height": f.height,
        "channels": f.cpp,
        "output": output_name(f.output),
        "bits_per_sample": f.bits_per_sample,
        "cfa": cfa(f.cfa),
        "layout": layout(f.sensor_layout),
        "active_area": rect(f.active_area),
        "crop": rect(f.crop),
        "origin": [f.origin.0, f.origin.1],
        "masked_areas": rects(&f.masked_areas),
        "black_level": black(&f.black_level),
        "white_level": white(&f.white_level),
        "white_balance": wb(f.white_balance),
        "color_matrices": colors(&f.color_matrices),
        "orientation": f.orientation.exif(),
        "stats": {
            "backend": {
                "requested": requested_backend,
                "kind": if s.backend.gpu_adapter.is_some() { "gpu" } else { "cpu" },
                "decode": s.backend.decode,
                "postprocess": s.backend.postprocess,
                "simd": s.backend.simd,
                "threads": s.backend.threads,
                "gpu_adapter": s.backend.gpu_adapter,
            },
            "total_ms": num(s.total_ms),
            "stages": stages,
            "input_bytes": s.input_bytes,
            "output_bytes": s.output_bytes,
            "opcodes": s.opcodes,
        },
        "source": f.source.name(),
        "warnings": f.warnings().iter().map(warning).collect::<Vec<_>>(),
    })
}

/// A structured corruption cause: `kind` (`truncated`, `invalid-unit`, `preview-fallback`), `unit` (`strip`,
/// `tile`, `plane`, `payload`), `index`, and for truncation the byte range (`offset`,
/// `expected`, `available`, `file_length`).
pub fn corrupt_reason(r: &CorruptReason) -> Value {
    let u = r.unit();
    let mut v = json!({
        "kind": "other",
        "unit": u.kind(),
        "index": u.index(),
        "offset": null,
        "expected": null,
        "available": null,
        "file_length": null,
        "message": r.to_string(),
    });
    match r {
        CorruptReason::Truncated(t) => {
            v["kind"] = json!("truncated");
            v["offset"] = json!(t.offset);
            v["expected"] = json!(t.expected);
            v["available"] = json!(t.available);
            v["file_length"] = json!(t.file_len);
        }
        _ => v["kind"] = json!(r.kind()),
    }
    v
}

/// A recovery warning: the cause plus `samples_filled` and a readable `message`.
fn warning(w: &DecodeWarning) -> Value {
    let mut v = corrupt_reason(&w.reason);
    v["samples_filled"] = json!(w.samples_filled);
    v["message"] = json!(w.to_string());
    v
}
