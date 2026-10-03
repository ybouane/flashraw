//!
//! Generated TOML is the source of truth that `build.rs` compiles; never edit the
//! generated Rust tables.

use std::collections::BTreeMap;
use std::path::Path;

include!("schema.rs");

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
            let inline = serde_json::to_string(v).unwrap_or_default();
            if scalars && inline.len() <= 160 {
                out.push_str(&inline.replace(',', ", "));
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
        if m.color_matrix.len() != 9 && m.color_matrix.len() != 12 {
            problems.push(format!("matrix {} {}: {} values", m.canonical_make, m.model_prefix, m.color_matrix.len()));
        }
    }
    problems
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
