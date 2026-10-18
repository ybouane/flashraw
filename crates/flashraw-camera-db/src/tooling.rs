//! Camera database tooling: validation, generation, comparison.
//!
//! Exposed through `flashraw camera-db {validate,generate,compare,stats}`.
//! `camera-db/cameras.json` is the source of truth that `build.rs` compiles; never edit
//! the generated Rust tables.

use std::collections::BTreeMap;
use std::path::Path;

include!("schema.rs");

/// Orientation policy names accepted in `orientation.policy` / `orientation_rules`.
pub const ORIENTATION_POLICIES: &[&str] =
    &["exif-only", "makernote-preferred", "makernote-when-exif-normal", "canon-auto-rotate-aware"];

/// Errors from tooling operations.
pub type ToolResult<T> = Result<T, String>;

/// Write the database as pretty-printed JSON (entries sorted for stable diffs).
pub fn write_db(path: &Path, db: &DbFile) -> ToolResult<()> {
    let mut db = db.clone();
    db.version = db.version.max(1);
    db.cameras.sort_by(|a, b| {
        (crate::canonical_make(&a.make), a.make.to_lowercase(), a.model.to_lowercase(), a.mode.to_lowercase()).cmp(&(
            crate::canonical_make(&b.make),
            b.make.to_lowercase(),
            b.model.to_lowercase(),
            b.mode.to_lowercase(),
        ))
    });
    let value = serde_json::to_value(&db).map_err(|e| e.to_string())?;
    let mut text = String::new();
    format_json(&value, 0, &mut text);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text + "\n").map_err(|e| e.to_string())
}

/// Pretty-print JSON with 2-space indentation, keeping short arrays of scalars (matrices,
/// crops, levels, alias lists) on one line so the file stays reviewable.
fn format_json(v: &serde_json::Value, indent: usize, out: &mut String) {
    use serde_json::Value;
    let pad = |n: usize| " ".repeat(n);
    match v {
        Value::Object(m) if m.is_empty() => out.push_str("{}"),
        Value::Object(m) => {
            out.push_str("{\n");
            for (i, (k, val)) in m.iter().enumerate() {
                out.push_str(&pad(indent + 2));
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push_str(": ");
                format_json(val, indent + 2, out);
                out.push_str(if i + 1 < m.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
        Value::Array(a) if a.is_empty() => out.push_str("[]"),
        Value::Array(a) => {
            let scalars = a.iter().all(|x| !x.is_object() && !x.is_array());
            let items: Vec<String> = a.iter().map(|x| serde_json::to_string(x).unwrap_or_default()).collect();
            let inline = format!("[{}]", items.join(", "));
            if scalars && inline.len() <= 162 {
                out.push_str(&inline);
                return;
            }
            out.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                out.push_str(&pad(indent + 2));
                format_json(x, indent + 2, out);
                out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// Load the JSON database.
pub fn load_db(path: &Path) -> ToolResult<DbFile> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Validation problems found in a database.
pub fn validate(db: &DbFile) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = BTreeMap::new();
    for c in &db.cameras {
        let id = format!("{} / {} [{}]", c.make, c.model, c.mode);
        if c.make.trim().is_empty() || (c.model.trim().is_empty() && !c.hints.contains_key("filesize")) {
            problems.push(format!("{id}: empty make or model"));
        }
        let key = (c.make.to_uppercase(), c.model.to_uppercase(), c.mode.to_uppercase());
        if seen.insert(key, ()).is_some() {
            problems.push(format!("{id}: duplicate entry"));
        }
        if let Some(cfa) = &c.cfa {
            let letters: String = cfa.chars().filter(|c| *c != '/').collect();
            let n = letters.len();
            let rows = cfa.split('/').count();
            let valid_dims = if rows > 1 { n % rows == 0 } else { (n as f64).sqrt().fract() == 0.0 };
            if n == 0 || n > 64 || !valid_dims || !letters.chars().all(|l| "RGBCMYWE".contains(l)) {
                problems.push(format!("{id}: invalid CFA {cfa:?}"));
            }
        }
        if !c.color_matrix.is_empty() && c.color_matrix.len() != 9 && c.color_matrix.len() != 12 {
            problems.push(format!("{id}: colour matrix with {} values", c.color_matrix.len()));
        }
        for s in &c.sensor {
            if s.white != 0 && s.white <= s.black {
                problems.push(format!("{id}: white {} <= black {}", s.white, s.black));
            }
        }
        if let Some([x, y, _, _]) = c.crop {
            if x < 0 || y < 0 {
                problems.push(format!("{id}: negative crop origin"));
            }
        }
        let mut crops = BTreeMap::new();
        for r in &c.image_crops {
            let [aw, ah] = r.area;
            let [x, y, w, h] = r.crop;
            if aw == 0 || ah == 0 || w == 0 || h == 0 {
                problems.push(format!("{id}: image crop with a zero dimension"));
            } else if x + w > aw || y + h > ah {
                problems.push(format!("{id}: image crop {:?} outside its {aw}x{ah} area", r.crop));
            }
            if crops.insert((aw, ah, w, h), ()).is_some() {
                problems.push(format!("{id}: duplicate image crop {w}x{h} for area {aw}x{ah}"));
            }
        }
        if let Some(o) = &c.orientation {
            if !o.policy.is_empty() && !ORIENTATION_POLICIES.contains(&o.policy.as_str()) {
                problems.push(format!("{id}: unknown orientation policy {:?}", o.policy));
            }
            if o.auto_rotate_offset.is_some() && o.maker_note_offset.is_none() {
                problems.push(format!("{id}: orientation auto_rotate_offset without maker_note_offset"));
            }
        }
    }
    let mut rules = BTreeMap::new();
    for r in &db.orientation_rules {
        let id = format!("orientation rule {} {:?}", r.canonical_make, r.model_prefix);
        if r.canonical_make.trim().is_empty() {
            problems.push(format!("{id}: empty make"));
        }
        if !ORIENTATION_POLICIES.contains(&r.policy.as_str()) {
            problems.push(format!("{id}: unknown policy {:?}", r.policy));
        }
        if rules.insert((r.canonical_make.to_lowercase(), r.model_prefix.to_lowercase()), ()).is_some() {
            problems.push(format!("{id}: duplicate"));
        }
    }
    for g in &db.model_aliases {
        if g.model.trim().is_empty() || g.aliases.is_empty() {
            problems.push(format!("alias group {} {:?}: empty", g.canonical_make, g.model));
        }
    }
    let mut sizes = BTreeMap::new();
    for r in &db.size_rules {
        if sizes.insert((r.canonical_make.to_lowercase(), r.raw_width, r.raw_height), ()).is_some() {
            problems.push(format!("size rule {} {}x{}: duplicate", r.canonical_make, r.raw_width, r.raw_height));
        }
        if r.raw_width == 0 || r.raw_height == 0 {
            problems.push(format!("size rule {}: zero dimension", r.canonical_make));
        }
    }
    for m in &db.matrices {
        let id = format!("matrix {} {}", m.canonical_make, m.model_prefix);
        if m.color_matrix.len() != 9 && m.color_matrix.len() != 12 {
            problems.push(format!("{id}: {} values", m.color_matrix.len()));
        }
    }
    problems
}

/// Coverage report: per canonical make, how many entries carry each kind of data, and how
/// many have sample files in the verification corpus. One line per row.
pub fn coverage(db: &DbFile) -> Vec<String> {
    // entries, samples, sensor levels, crop, masked areas, colour matrix, image crops, orientation
    let mut by_make: BTreeMap<String, [usize; 8]> = BTreeMap::new();
    for c in &db.cameras {
        let row = by_make.entry(c.canonical_make.clone()).or_default();
        let flags = [
            true,
            c.verification.as_ref().is_some_and(|v| v.corpus_files > 0),
            !c.sensor.is_empty(),
            c.crop.is_some(),
            !c.black_areas_v.is_empty() || !c.black_areas_h.is_empty(),
            !c.color_matrix.is_empty(),
            !c.image_crops.is_empty(),
            c.orientation.is_some(),
        ];
        for (slot, on) in row.iter_mut().zip(flags) {
            *slot += on as usize;
        }
    }
    let head = |name: &str| {
        format!(
            "{name:22} {:>7} {:>7} {:>7} {:>6} {:>7} {:>7} {:>7} {:>7}",
            "entries", "samples", "levels", "crop", "masked", "matrix", "imgcrop", "orient"
        )
    };
    let mut out = vec![head("make")];
    let mut tot = [0usize; 8];
    for (m, r) in &by_make {
        out.push(format!(
            "{m:22} {:7} {:7} {:7} {:6} {:7} {:7} {:7} {:7}",
            r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]
        ));
        for i in 0..8 {
            tot[i] += r[i];
        }
    }
    out.push(format!(
        "{:22} {:7} {:7} {:7} {:6} {:7} {:7} {:7} {:7}",
        "all", tot[0], tot[1], tot[2], tot[3], tot[4], tot[5], tot[6], tot[7]
    ));
    out.push(format!(
        "fallback matrices {}, alias groups {}, size rules {}, orientation rules {}",
        db.matrices.len(),
        db.model_aliases.len(),
        db.size_rules.len(),
        db.orientation_rules.len()
    ));
    out
}

/// Compare two databases, reporting entries added/removed/changed.
pub fn compare(a: &DbFile, b: &DbFile) -> Vec<String> {
    let key = |c: &DbCamera| format!("{} / {} [{}]", c.make, c.model, c.mode);
    let ma: BTreeMap<String, &DbCamera> = a.cameras.iter().map(|c| (key(c), c)).collect();
    let mb: BTreeMap<String, &DbCamera> = b.cameras.iter().map(|c| (key(c), c)).collect();
    let mut out = Vec::new();
    for (k, ca) in &ma {
        match mb.get(k) {
            None => out.push(format!("- {k}")),
            Some(cb) if cb != ca => {
                let mut diffs = Vec::new();
                if ca.crop != cb.crop {
                    diffs.push(format!("crop {:?} -> {:?}", ca.crop, cb.crop));
                }
                if ca.cfa != cb.cfa {
                    diffs.push(format!("cfa {:?} -> {:?}", ca.cfa, cb.cfa));
                }
                if ca.sensor != cb.sensor {
                    diffs.push("sensor levels".into());
                }
                if ca.color_matrix != cb.color_matrix {
                    diffs.push("colour matrix".into());
                }
                if ca.hints != cb.hints {
                    diffs.push("hints".into());
                }
                if ca.orientation != cb.orientation {
                    diffs.push("orientation".into());
                }
                if ca.image_crops != cb.image_crops {
                    diffs.push("image crops".into());
                }
                if diffs.is_empty() {
                    diffs.push("other fields".into());
                }
                out.push(format!("~ {k}: {}", diffs.join(", ")));
            }
            _ => {}
        }
    }
    for k in mb.keys() {
        if !ma.contains_key(k) {
            out.push(format!("+ {k}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::format_json;

    #[test]
    fn unknown_entry_keys_are_rejected() {
        let entry = |extra: &str| {
            format!(r#"{{"cameras":[{{"make":"A","model":"B","canonical_make":"A","canonical_model":"B"{extra}}}]}}"#)
        };
        assert!(serde_json::from_str::<super::DbFile>(&entry("")).is_ok());
        assert!(serde_json::from_str::<super::DbFile>(&entry(r#","extra_key":"x""#)).is_err());
    }

    #[test]
    fn orientation_entries_are_validated() {
        let mut db = super::DbFile::default();
        db.orientation_rules.push(super::DbOrientationRule {
            canonical_make: "Canon".into(),
            policy: "sideways".into(),
            ..Default::default()
        });
        db.cameras.push(super::DbCamera {
            make: "Canon".into(),
            model: "Canon EOS X".into(),
            canonical_make: "Canon".into(),
            canonical_model: "EOS X".into(),
            orientation: Some(super::DbOrientation { auto_rotate_offset: Some(4), ..Default::default() }),
            ..Default::default()
        });
        let p = super::validate(&db);
        assert!(p.iter().any(|m| m.contains("unknown policy")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("auto_rotate_offset without")), "{p:?}");
    }

    #[test]
    fn inline_arrays_keep_string_contents() {
        let v: serde_json::Value = serde_json::json!({ "aliases": ["C70Z,C7000Z", "X"], "m": [1, -2, 3] });
        let mut once = String::new();
        format_json(&v, 0, &mut once);
        assert!(once.contains(r#"["C70Z,C7000Z", "X"]"#), "{once}");
        assert!(once.contains("[1, -2, 3]"), "{once}");
        let mut twice = String::new();
        format_json(&serde_json::from_str(&once).unwrap(), 0, &mut twice);
        assert_eq!(once, twice);
    }
}
