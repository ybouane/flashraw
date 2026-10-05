//! JSON documents returned by `flashraw_file_metadata_json()` and `flashraw_frame_json()`.
//!
//! These are the escape hatch for fields without a dedicated getter: new metadata appears
//! here first, without an ABI change. Key names are snake_case and only ever added.

use flashraw::{CorruptReason, DecodeWarning, FlashRaw, FrameInfo, OrientationMetadata, RawFrame, SensorLayout};
use serde_json::{Value, json};

/// A structured corruption cause (`kind`, `unit`, `index`, and for truncation the byte range).
pub fn corrupt_reason(r: &CorruptReason) -> Value {
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
        CorruptReason::InvalidUnit { message, .. } => {
            v["kind"] = json!("invalid-unit");
            v["message"] = json!(message);
        }
        _ => v["kind"] = json!("other"),
    }
    v
}

fn warning(w: &DecodeWarning) -> Value {
    let mut v = corrupt_reason(&w.reason);
    v["samples_filled"] = json!(w.samples_filled);
    v["message"] = json!(w.to_string());
    v
}

fn layout_name(l: SensorLayout) -> &'static str {
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

fn opcode_lists(i: &FrameInfo) -> Value {
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

/// Geometry and calibration of the sensor buffer.
pub fn frame_info(i: &FrameInfo) -> Value {
    json!({
        "width": i.width,
        "height": i.height,
        "cpp": i.cpp,
        "bits_per_sample": i.bits_per_sample,
        "sample_format": format!("{:?}", i.sample_format).to_lowercase(),
        "cfa": i.cfa.map(|c| c.to_string()),
        "layout": layout_name(i.layout),
        "active_area": i.active_area,
        "crop": i.crop,
        "masked_areas": i.masked_areas,
        "black_level": i.black_level,
        "black_from_masked": i.black_from_masked,
        "white_level": i.white_level,
        "white_balance": i.white_balance,
        "color_matrices": i.color_matrices,
        "orientation": i.orientation.exif(),
        "pixel_aspect": i.pixel_aspect,
        "compression": i.compression.name(),
        "opcodes": opcode_lists(i),
    })
}

/// Everything known about an opened file, without decoding pixels.
pub fn file(raw: &FlashRaw<'_>) -> Value {
    let m = raw.metadata();
    let mut metadata = serde_json::to_value(m).unwrap_or(Value::Null);
    if let Value::Object(o) = &mut metadata {
        // Plain EXIF codes instead of enum names, like `orientation` elsewhere.
        o.insert("orientation".into(), json!(m.orientation.exif()));
        o.insert("orientation_info".into(), orientation_info(&m.orientation_info));
    }
    json!({
        "abi_version": crate::FLASHRAW_ABI_VERSION,
        "library_version": env!("CARGO_PKG_VERSION"),
        "format": raw.format().name(),
        "camera": m.camera(),
        "metadata": metadata,
        "frame_info": frame_info(raw.frame_info()),
        "capabilities": raw.capabilities(),
        "parse_ms": raw.parse_ms(),
    })
}

/// A decoded frame's description (no pixel data).
pub fn frame(f: &RawFrame) -> Value {
    json!({
        "width": f.width,
        "height": f.height,
        "cpp": f.cpp,
        "output": f.output.name(),
        "active_area": f.active_area,
        "crop": f.crop,
        "origin": [f.origin.0, f.origin.1],
        "bits_per_sample": f.bits_per_sample,
        "cfa": f.cfa.map(|c| c.to_string()),
        "layout": layout_name(f.sensor_layout),
        "masked_areas": f.masked_areas,
        "black_level": f.black_level,
        "white_level": f.white_level,
        "white_balance": f.white_balance,
        "color_matrices": f.color_matrices,
        "orientation": f.orientation.exif(),
        "stats": f.stats,
        "incomplete": f.is_incomplete(),
        "warnings": f.warnings.iter().map(warning).collect::<Vec<_>>(),
    })
}
