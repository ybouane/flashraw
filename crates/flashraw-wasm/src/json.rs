//! JSON views of FlashRaw metadata for JavaScript.
//!
//! The shapes here are the contract documented in `npm/src/types.d.ts`: camelCase keys,
//! plain JSON values (no tuples, enums as lowercase strings), so the JS side only has to
//! `JSON.parse` once per call instead of crossing the WASM boundary per field.

use flashraw::{
    BlackLevel, CfaPattern, ColorMatrices, ColorMatrix, ColorSource, DecodeUnit, FlashRaw, FrameInfo,
    OrientationMetadata, PreviewEncoding, PreviewInfo, PreviewKind, RandomAccess, RawFrame, RawMetadata, Rect,
    SampleFormat, SensorLayout, Stage, WhiteBalance, WhiteLevel,
};
use serde_json::{Map, Value, json};

/// Finite numbers only: JSON has no NaN/Infinity, and garbage metadata may contain them.
fn num(v: f64) -> Value {
    if v.is_finite() { json!(v) } else { Value::Null }
}

/// f32 → JSON via its shortest decimal representation (so 0.6719 stays 0.6719, not 0.67189997).
fn f32v(v: f32) -> Value {
    if !v.is_finite() {
        return Value::Null;
    }
    v.to_string().parse::<f64>().map(|d| json!(d)).unwrap_or(Value::Null)
}

fn f32s(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|&x| f32v(x)).collect())
}

pub fn rect(r: Rect) -> Value {
    json!({ "x": r.x, "y": r.y, "width": r.width, "height": r.height })
}

pub fn cfa(p: Option<CfaPattern>) -> Value {
    match p {
        None => Value::Null,
        Some(p) => json!({
            "pattern": p.colors().iter().map(|c| c.letter()).collect::<String>(),
            "width": p.width(),
            "height": p.height(),
            "colors": p.colors().iter().map(|c| c.letter().to_string()).collect::<Vec<_>>(),
        }),
    }
}

pub fn layout(l: SensorLayout) -> &'static str {
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

pub fn black(b: &BlackLevel) -> Value {
    let mut m = Map::new();
    m.insert("rows".into(), json!(b.rows));
    m.insert("cols".into(), json!(b.cols));
    m.insert("samples".into(), json!(b.samples));
    m.insert("values".into(), f32s(&b.values));
    m.insert("mean".into(), f32v(b.mean()));
    if !b.row_deltas.is_empty() {
        m.insert("rowDeltas".into(), f32s(&b.row_deltas));
    }
    if !b.col_deltas.is_empty() {
        m.insert("colDeltas".into(), f32s(&b.col_deltas));
    }
    Value::Object(m)
}

pub fn white(w: &WhiteLevel) -> Value {
    json!(w.values)
}

pub fn wb(w: Option<WhiteBalance>) -> Value {
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

pub fn colors(c: &ColorMatrices) -> Value {
    json!({
        "color": c.color.iter().map(matrix).collect::<Vec<_>>(),
        "forward": c.forward.iter().map(matrix).collect::<Vec<_>>(),
        "calibration": c.calibration.iter().map(matrix).collect::<Vec<_>>(),
        "analogBalance": f32s(&c.analog_balance),
        "source": match c.source {
            ColorSource::None => "none",
            ColorSource::File => "file",
            ColorSource::CameraDb => "camera-db",
            _ => "unknown",
        },
    })
}

pub fn preview_mime(e: PreviewEncoding) -> &'static str {
    match e {
        PreviewEncoding::Jpeg => "image/jpeg",
        PreviewEncoding::JpegXl => "image/jxl",
        PreviewEncoding::Heif => "image/heif",
        PreviewEncoding::Rgb8 => "application/x-flashraw-rgb8",
        PreviewEncoding::Ycbcr422 => "application/x-flashraw-ycbcr422",
        _ => "application/octet-stream",
    }
}

fn preview(i: usize, p: &PreviewInfo) -> Value {
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
        "mimeType": preview_mime(p.encoding),
        "width": p.width,
        "height": p.height,
        "byteLength": p.length,
        "offset": p.offset,
        "orientation": p.orientation.exif(),
    })
}

pub fn previews(m: &RawMetadata) -> Value {
    Value::Array(m.previews.iter().enumerate().map(|(i, p)| preview(i, p)).collect())
}

pub fn frame_info(f: &FrameInfo) -> Value {
    json!({
        "width": f.width,
        "height": f.height,
        "channels": f.cpp,
        "bitsPerSample": f.bits_per_sample,
        "sampleFormat": match f.sample_format { SampleFormat::U16 => "u16", SampleFormat::F32 => "f32", _ => "unknown" },
        "cfa": cfa(f.cfa),
        "layout": layout(f.layout),
        "activeArea": rect(f.active_area),
        "crop": rect(f.crop),
        "maskedAreas": f.masked_areas.iter().map(|r| rect(*r)).collect::<Vec<_>>(),
        "blackLevel": black(&f.black_level),
        "blackFromMasked": f.black_from_masked,
        "whiteLevel": white(&f.white_level),
        "whiteBalance": wb(f.white_balance),
        "colorMatrices": colors(&f.color_matrices),
        "orientation": f.orientation.exif(),
        "pixelAspect": f32v(f.pixel_aspect),
        "compression": f.compression.name(),
    })
}

fn opt_str(s: &Option<String>) -> Value {
    s.as_deref().map(|s| json!(s)).unwrap_or(Value::Null)
}

fn pair(p: Option<(f32, f32)>) -> Value {
    p.map(|(a, b)| json!([f32v(a), f32v(b)])).unwrap_or(Value::Null)
}

fn opt_f32(v: Option<f32>) -> Value {
    v.map(f32v).unwrap_or(Value::Null)
}

/// Orientation records and how `orientation` was chosen (`RawMetadata.orientationInfo`).
pub fn orientation_info(o: &OrientationMetadata) -> Value {
    json!({
        "exif": o.exif.map(|v| v.exif()),
        "makerNote": o.maker_note.map(|v| v.exif()),
        "autoRotate": o.auto_rotate,
        "resolved": o.resolved.exif(),
        "source": o.source.name(),
        "policy": o.policy.name(),
    })
}

/// The full metadata object (`FlashRaw.metadata` in JS).
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
            "canonicalMake": m.canonical_make,
            "canonicalModel": m.canonical_model,
            "uniqueModel": opt_str(&m.unique_camera_model),
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
            "focalRange": pair(m.lens.focal_range),
            "apertureRange": pair(m.lens.aperture_range),
        },
        "exposure": {
            "iso": e.iso,
            "exposureTime": e.exposure_time.map(num).unwrap_or(Value::Null),
            "fNumber": opt_f32(e.f_number),
            "focalLength": opt_f32(e.focal_length),
            "focalLength35mm": e.focal_length_35mm,
            "exposureBias": opt_f32(e.exposure_bias),
            "flash": e.flash,
            "meteringMode": e.metering_mode,
            "exposureProgram": e.exposure_program,
        },
        "captureTime": opt_str(&m.capture_time),
        "gps": m.gps.as_ref().map(|g| json!({
            "latitude": g.latitude.map(num),
            "longitude": g.longitude.map(num),
            "altitude": g.altitude.map(num),
            "timestamp": opt_str(&g.timestamp),
        })).unwrap_or(Value::Null),
        "orientation": m.orientation.exif(),
        "orientationInfo": orientation_info(&m.orientation_info),
        "width": m.width,
        "height": m.height,
        "rawWidth": m.raw_width,
        "rawHeight": m.raw_height,
        "bitsPerSample": m.bits_per_sample,
        "compression": m.compression.unwrap_or(f.compression).name(),
        "dngVersion": m.dng_version.map(|v| format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3])),
        "cfa": cfa(f.cfa),
        "layout": layout(f.layout),
        "blackLevel": black(&f.black_level),
        "whiteLevel": white(&f.white_level),
        "whiteBalance": wb(f.white_balance),
        "colorMatrices": colors(&f.color_matrices),
        "previews": previews(m),
        "extra": extra,
        "dng": m.dng.as_ref().map(dng).unwrap_or(Value::Null),
        "opcodes": opcodes(f),
    })
}

fn opt_num(v: Option<f64>) -> Value {
    v.map(num).unwrap_or(Value::Null)
}

fn nums(v: &[f64]) -> Value {
    Value::Array(v.iter().map(|&x| num(x)).collect())
}

/// DNG rendering hints (`metadata.dng`).
fn dng(x: &flashraw::DngExtras) -> Value {
    let p = &x.profile;
    json!({
        "baselineExposure": opt_num(x.baseline_exposure),
        "baselineNoise": opt_num(x.baseline_noise),
        "baselineSharpness": opt_num(x.baseline_sharpness),
        "linearResponseLimit": opt_num(x.linear_response_limit),
        "asShotNeutral": x.as_shot_neutral.as_deref().map(nums).unwrap_or(Value::Null),
        "asShotWhiteXY": x.as_shot_white_xy.map(|(a, b)| json!([num(a), num(b)])).unwrap_or(Value::Null),
        "defaultUserCrop": x.default_user_crop.map(|c| nums(&c)).unwrap_or(Value::Null),
        "defaultScale": x.default_scale.map(|(a, b)| json!([num(a), num(b)])).unwrap_or(Value::Null),
        "noiseProfile": x.noise_profile.as_deref().map(nums).unwrap_or(Value::Null),
        "profile": {
            "name": opt_str(&p.name),
            "embedPolicy": p.embed_policy,
            "hueSatMapDims": p.hue_sat_map_dims.map(|d| json!(d)).unwrap_or(Value::Null),
            "hueSatMapCount": p.hue_sat_map_count,
            "lookTableDims": p.look_table_dims.map(|d| json!(d)).unwrap_or(Value::Null),
            "toneCurve": p.tone_curve,
            "baselineExposureOffset": opt_num(p.baseline_exposure_offset),
        },
    })
}

/// The DNG opcode lists with parameters of the opcodes an editor applies itself
/// (`metadata.opcodes`). Unparsable lists are left out (decoding reports them).
pub fn opcodes(f: &FrameInfo) -> Value {
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
                "previewOnly": e.preview_only,
                "applied": e.op.is_pixel_opcode(),
                "params": params,
            }));
        }
    }
    Value::Array(out)
}

pub fn random_access(r: RandomAccess) -> Value {
    match r {
        RandomAccess::Sequential => json!({ "kind": "sequential" }),
        RandomAccess::Rows => json!({ "kind": "rows" }),
        RandomAccess::Strips { rows } => json!({ "kind": "strips", "rows": rows }),
        RandomAccess::Tiles { width, height } => json!({ "kind": "tiles", "width": width, "height": height }),
        RandomAccess::Blocks => json!({ "kind": "blocks" }),
        _ => json!({ "kind": "unknown" }),
    }
}

pub fn capabilities(raw: &FlashRaw<'_>) -> Value {
    let c = raw.capabilities();
    json!({
        "randomAccess": random_access(c.random_access),
        "tiles": c.units,
        "previews": c.previews,
        "gpuUnpack": c.gpu_unpack,
        "float": c.float,
    })
}

pub fn units(u: &[DecodeUnit]) -> Value {
    Value::Array(
        u.iter()
            .enumerate()
            .map(|(i, u)| json!({ "index": i, "rect": rect(u.rect), "offset": u.offset, "byteLength": u.length }))
            .collect(),
    )
}

pub fn stage_key(s: Stage) -> &'static str {
    match s {
        Stage::ContainerParse => "containerParse",
        Stage::Metadata => "metadata",
        Stage::EntropyDecode => "entropyDecode",
        Stage::Unpack => "unpack",
        Stage::Predictor => "predictor",
        Stage::Calibration => "calibration",
        Stage::Postprocess => "postprocess",
        Stage::GpuUpload => "gpuUpload",
        Stage::GpuCompute => "gpuCompute",
        Stage::GpuDownload => "gpuDownload",
        Stage::Preview => "preview",
        _ => "unknown",
    }
}

pub fn output_name(o: flashraw::OutputFormat) -> &'static str {
    use flashraw::OutputFormat::*;
    match o {
        RawU16 => "raw-u16",
        RawF32 => "raw-f32",
        NormalizedF32 => "normalized",
        LinearRgbF32 => "linear-rgb",
        Rgba8 => "rgba8",
        GpuRawU16 => "gpu-raw-u16",
        GpuNormalizedF32 => "gpu-normalized",
        GpuLinearRgbF32 => "gpu-linear-rgb",
        GpuRgba8 => "gpu-rgba8",
        _ => "unknown",
    }
}

/// Everything about a decoded frame except its pixels.
pub fn frame(f: &RawFrame, requested_backend: &str) -> Value {
    let s = &f.stats;
    let mut stages = Map::new();
    for (st, ms) in &s.stages {
        stages.insert(stage_key(*st).into(), num(*ms));
    }
    json!({
        "width": f.width,
        "height": f.height,
        "channels": f.cpp,
        "output": output_name(f.output),
        "bitsPerSample": f.bits_per_sample,
        "cfa": cfa(f.cfa),
        "layout": layout(f.sensor_layout),
        "activeArea": rect(f.active_area),
        "crop": rect(f.crop),
        "origin": { "x": f.origin.0, "y": f.origin.1 },
        "maskedAreas": f.masked_areas.iter().map(|r| rect(*r)).collect::<Vec<_>>(),
        "blackLevel": black(&f.black_level),
        "whiteLevel": white(&f.white_level),
        "whiteBalance": wb(f.white_balance),
        "colorMatrices": colors(&f.color_matrices),
        "orientation": f.orientation.exif(),
        "backend": {
            "requested": requested_backend,
            "kind": if s.backend.gpu_adapter.is_some() { "gpu" } else { "cpu" },
            "decode": s.backend.decode,
            "postprocess": s.backend.postprocess,
            "simd": s.backend.simd,
            "threads": s.backend.threads,
            "gpuAdapter": s.backend.gpu_adapter,
        },
        "timings": { "totalMs": num(s.total_ms), "stages": stages },
        "inputBytes": s.input_bytes,
        "outputBytes": s.output_bytes,
        "opcodes": s.opcodes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashraw::{Orientation, OrientationPolicy};

    #[test]
    fn orientation_info_fields() {
        let o = OrientationMetadata::resolve(
            Some(Orientation::Normal),
            Some(Orientation::Rotate90),
            None,
            OrientationPolicy::MakerNoteWhenExifNormal,
        );
        let v = orientation_info(&o);
        assert_eq!(v["exif"], 1);
        assert_eq!(v["makerNote"], 6);
        assert_eq!(v["autoRotate"], Value::Null);
        assert_eq!(v["resolved"], 6);
        assert_eq!(v["source"], "maker-note");
        assert_eq!(v["policy"], "makernote-when-exif-normal");
        let v = orientation_info(&OrientationMetadata::default());
        assert_eq!(v["exif"], Value::Null);
        assert_eq!(v["resolved"], 1);
        assert_eq!(v["source"], "default");
    }

    #[test]
    fn metadata_carries_orientation_info() {
        let root = std::env::var("FLASHRAW_CORPUS").unwrap_or_else(|_| "../../corpus".into());
        let path = std::path::Path::new(&root).join("Canon/EOS Digital Rebel XS/RAW_CANON_1000D.CR2");
        let Ok(bytes) = std::fs::read(path) else { return };
        let raw = FlashRaw::open(bytes).unwrap();
        let m = metadata(&raw);
        assert_eq!(m["orientation"], 8);
        let o = &m["orientationInfo"];
        assert_eq!((&o["exif"], &o["makerNote"], &o["resolved"]), (&json!(8), &json!(8), &json!(8)));
        assert_eq!(o["source"], "exif");
        assert_eq!(o["policy"], "canon-auto-rotate-aware");
        assert!(m["previews"].as_array().unwrap().iter().all(|p| p["orientation"] == 8));
    }
}
