//!
//! Generated TOML is the source of truth that `build.rs` compiles; never edit the
//! generated Rust tables.

use std::collections::BTreeMap;
use std::path::Path;

include!("schema.rs");

/// Errors from tooling operations.
pub type ToolResult<T> = Result<T, String>;

fn file_key(make: &str) -> String {
    let c = crate::canonical_make(make);
    let base = if c.is_empty() { make } else { c };
    base.to_ascii_lowercase().replace(|c: char| !c.is_ascii_alphanumeric(), "-").trim_matches('-').to_string()
}

/// Write cameras and matrices as one TOML file per canonical make into `dir`.
pub fn write_db(dir: &Path, cameras: &[DbCamera], matrices: &[DbMatrix]) -> ToolResult<usize> {
    let mut files: BTreeMap<String, DbFile> = BTreeMap::new();
    for c in cameras {
        files.entry(file_key(&c.make)).or_default().cameras.push(c.clone());
    }
    for m in matrices {
        files.entry(file_key(&m.canonical_make)).or_default().matrices.push(m.clone());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for (k, f) in &files {
        let mut text = String::from(
            "# FlashRaw camera database; see camera-db/README.md.

",
        );
        text.push_str(&toml::to_string(f).map_err(|e| e.to_string())?);
        std::fs::write(dir.join(format!("{k}.toml")), text).map_err(|e| e.to_string())?;
    }
    Ok(files.len())
}

/// Load all TOML files from a directory.
pub fn load_db(dir: &Path) -> ToolResult<DbFile> {
    let mut all = DbFile::default();
    let mut entries: Vec<_> =
        std::fs::read_dir(dir).map_err(|e| e.to_string())?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.extension().is_some_and(|x| x == "toml") {
            let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
            let f: DbFile = toml::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
            all.cameras.extend(f.cameras);
            all.matrices.extend(f.matrices);
        }
    }
    Ok(all)
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
