//! Error messages follow one style (see `common/error_style.rs`): lower case at the start
//! unless the first word is a proper noun, acronym, format name or code, no trailing
//! period, no stray whitespace, and no repeated kind prefix.
//!
//! Two checks: a scan of the sources for the messages passed to the error constructors
//! and macros (Rust crates, the npm package and the Python package), and the errors
//! produced by hostile inputs at run time (the malformed-input and truncation suites run
//! the same check on everything they decode).

mod common;

use common::error_style::{check_decode, check_error, error_violation, violation};
use flashraw::{CorruptionPolicy, DecodeOptions, Error, FlashRaw, Limits};
use std::path::{Path, PathBuf};

// --- Source scan ----------------------------------------------------------------------

/// Calls whose message is a string literal argument: (callee, arguments to skip).
const RUST_CALLS: &[(&str, usize)] = &[
    // flashraw-core `Error` constructors and macros.
    ("corrupt", 0),
    ("corrupt_at", 0),
    ("invalid_metadata", 0),
    ("unsupported_compression", 1),
    ("bail_corrupt", 0),
    ("ensure_corrupt", 1),
    ("UnsupportedFormat", 0),
    ("InvalidMetadata", 0),
    ("GpuUnavailable", 0),
    ("OutOfMemory", 0),
    ("LimitExceeded", 0),
    ("NotAvailable", 0),
    ("InvalidArgument", 0),
    // Codec helpers.
    ("require", 1),
    ("u32_checked", 1),
    // Bindings: WASM `ApiError`, C `FfiError`, Python, the C compatibility layer, the CLI.
    ("invalid", 0),
    ("not_available", 0),
    ("internal", 0),
    ("make_error", 2),
    ("set_error", 0),
    ("Err", 0),
];

/// Struct fields that hold a message.
const RUST_FIELDS: &[&str] = &["message", "reason"];

#[derive(Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String, usize),
    Punct(char),
}

/// A small Rust lexer: identifiers, string literals (with their line) and punctuation;
/// comments, char literals, lifetimes and numbers are dropped.
fn lex_rust(src: &str) -> Vec<Tok> {
    let s: Vec<char> = src.chars().collect();
    let mut out = vec![];
    let (mut i, mut line) = (0, 1);
    let count_lines = |a: &[char]| a.iter().filter(|&&c| c == '\n').count();
    while i < s.len() {
        let c = s[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && s.get(i + 1) == Some(&'/') {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && s.get(i + 1) == Some(&'*') {
            let (start, mut depth) = (i, 0);
            while i < s.len() {
                if s[i] == '/' && s.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if s[i] == '*' && s.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            line += count_lines(&s[start..i.min(s.len())]);
        } else if c == '"' {
            let start = i + 1;
            i += 1;
            while i < s.len() && s[i] != '"' {
                i += if s[i] == '\\' { 2 } else { 1 };
            }
            out.push(Tok::Str(s[start..i.min(s.len())].iter().collect(), line));
            line += count_lines(&s[start..i.min(s.len())]);
            i += 1;
        } else if c == '\'' {
            // Char literal or lifetime.
            if s.get(i + 1) == Some(&'\\') {
                i += 2;
                while i < s.len() && s[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if s.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                i += 1;
                while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                    i += 1;
                }
            }
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                i += 1;
            }
            let ident: String = s[start..i].iter().collect();
            let prefix = matches!(ident.as_str(), "b" | "c" | "r" | "br" | "cr");
            if prefix && i < s.len() && (s[i] == '"' || (ident.ends_with('r') && s[i] == '#')) {
                if ident.ends_with('r') {
                    // Raw string: r#*"..."#*
                    let hashes = s[i..].iter().take_while(|&&c| c == '#').count();
                    i += hashes + 1;
                    let start = i;
                    let close: Vec<char> = std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
                    while i < s.len() && !s[i..].starts_with(&close) {
                        i += 1;
                    }
                    out.push(Tok::Str(s[start..i.min(s.len())].iter().collect(), line));
                    line += count_lines(&s[start..i.min(s.len())]);
                    i += close.len();
                }
                // Byte and C strings are lexed as plain strings on the next iteration.
                continue;
            }
            out.push(Tok::Ident(ident));
        } else if c.is_ascii_digit() {
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_' || s[i] == '.') {
                if s[i] == '.' && s.get(i + 1) == Some(&'.') {
                    break;
                }
                i += 1;
            }
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    out
}

/// Messages (line, text) passed to the known calls and fields in a Rust source.
fn rust_messages(src: &str) -> Vec<(usize, String)> {
    let toks = lex_rust(src);
    let mut out = vec![];
    for (k, t) in toks.iter().enumerate() {
        let Tok::Ident(name) = t else { continue };
        if k > 0 && toks[k - 1] == Tok::Ident("fn".into()) {
            continue;
        }
        // Calls: `name(...)` or `name!(...)`.
        if let Some(&(_, skip)) = RUST_CALLS.iter().find(|(n, _)| n == name) {
            let mut j = k + 1;
            if toks.get(j) == Some(&Tok::Punct('!')) {
                j += 1;
            }
            if toks.get(j) != Some(&Tok::Punct('(')) {
                continue;
            }
            let (mut depth, mut arg) = (0usize, 0usize);
            for t in &toks[j..] {
                match t {
                    Tok::Punct('(' | '[' | '{') => depth += 1,
                    Tok::Punct(')' | ']' | '}') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Tok::Punct(',') if depth == 1 => arg += 1,
                    Tok::Str(s, line) if arg >= skip => {
                        out.push((*line, s.clone()));
                        break;
                    }
                    _ => {}
                }
            }
        }
        // Fields: `message: "..."` or `message: format!("...")`, up to the next `,` / `}`.
        if RUST_FIELDS.contains(&name.as_str())
            && toks.get(k + 1) == Some(&Tok::Punct(':'))
            && toks.get(k + 2) != Some(&Tok::Punct(':'))
            && (k == 0 || toks[k - 1] != Tok::Punct(':'))
        {
            let mut depth = 0usize;
            for t in &toks[k + 2..] {
                match t {
                    Tok::Punct('(' | '[' | '{') => depth += 1,
                    Tok::Punct(')' | ']' | '}') if depth == 0 => break,
                    Tok::Punct(')' | ']' | '}') => depth -= 1,
                    Tok::Punct(',' | ';') if depth == 0 => break,
                    Tok::Str(s, line) => {
                        out.push((*line, s.clone()));
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

/// Messages passed to `...Error(` constructors in JavaScript or Python: the literal
/// first argument (quoted, template or f-string).
fn script_messages(src: &str) -> Vec<(usize, String)> {
    let s: Vec<char> = src.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i + 6 <= s.len() {
        if s[i..i + 6] != ['E', 'r', 'r', 'o', 'r', '('] {
            i += 1;
            continue;
        }
        let line = s[..i].iter().filter(|&&c| c == '\n').count() + 1;
        i += 6;
        while i < s.len() && s[i].is_whitespace() {
            i += 1;
        }
        if s.get(i) == Some(&'f') {
            i += 1; // Python f-string
        }
        let Some(&q) = s.get(i).filter(|c| matches!(c, '\'' | '"' | '`')) else { continue };
        i += 1;
        let start = i;
        let mut depth = 0; // `${...}` nesting in templates
        while i < s.len() && (depth > 0 || s[i] != q) {
            match s[i] {
                '\\' => i += 1,
                '$' if q == '`' && s.get(i + 1) == Some(&'{') => {
                    depth += 1;
                    i += 1;
                }
                '}' if depth > 0 => depth -= 1,
                '`' if depth > 0 => {
                    // A nested template inside `${...}`.
                    i += 1;
                    while i < s.len() && s[i] != '`' {
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        out.push((line, s[start..i.min(s.len())].iter().collect()));
        i += 1;
    }
    out
}

fn workspace_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.join("crates/flashraw-core/src/error.rs").exists().then_some(root)
}

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            files(&p, ext, out);
        } else if p.extension().is_some_and(|e| e == ext) {
            out.push(p);
        }
    }
}

#[test]
fn source_error_messages_follow_the_style() {
    let Some(root) = workspace_root() else {
        eprintln!("workspace sources not present; skipping");
        return;
    };
    let mut sources = vec![];
    for c in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        files(&c.path().join("src"), "rs", &mut sources);
    }
    let rust_count = sources.len();
    files(&root.join("npm/src"), "js", &mut sources);
    files(&root.join("crates/flashraw-python/python"), "py", &mut sources);
    let (mut checked, mut failures) = (0, vec![]);
    for (n, path) in sources.iter().enumerate() {
        let src = std::fs::read_to_string(path).unwrap();
        let messages = if n < rust_count { rust_messages(&src) } else { script_messages(&src) };
        for (line, msg) in messages {
            checked += 1;
            // Rust string continuations (`\` + newline + indentation) are one space.
            let msg = msg.split("\\\n").map(str::trim_start).collect::<Vec<_>>().join("");
            if let Some(v) = violation(&msg) {
                let rel = path.strip_prefix(&root).unwrap_or(path);
                failures.push(format!("{}:{line}: {msg:?}: {v}", rel.display()));
            }
        }
    }
    eprintln!("{checked} messages checked in {} files", sources.len());
    assert!(checked > 500, "the scan found only {checked} messages; is the lexer broken?");
    assert!(failures.is_empty(), "{} messages break the style:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn the_scanner_finds_messages() {
    let src = r##"
        // Error::corrupt("Commented out.")
        fn corrupt(what: &str) -> Error { Error::corrupt(format!("ARW6: {what}")) }
        let c = '"'; let l: &'static str = "x";
        bail_corrupt!("Bad {x}.", x = 1);
        ensure_corrupt!(a == "b", "truncated header");
        Error::unsupported_compression("NEF", format!("{b}-bit"));
        Err(ApiError { corrupt: None, kind: "Closed", message: r#"Raw "string""#.into() })
        make_error(py, "Internal", &format!("internal error: {m}"), None, None)
    "##;
    let found: Vec<String> = rust_messages(src).into_iter().map(|(_, m)| m).collect();
    assert_eq!(
        found,
        ["ARW6: {what}", "Bad {x}.", "truncated header", "{b}-bit", "Closed", r#"Raw "string""#, "internal error: {m}"]
    );
    let js =
        "throw new InvalidArgumentError(`unknown ${a ? `x` : y} op`); new Error('Oops.'); raise ValueError(f\"a {b}\")";
    let found: Vec<String> = script_messages(js).into_iter().map(|(_, m)| m).collect();
    assert_eq!(found, ["unknown ${a ? `x` : y} op", "Oops.", "a {b}"]);
}

#[test]
fn style_rules() {
    for ok in [
        "truncated header",
        "NEF has no raw image IFD",
        "CR2: zero slice width",
        "Huffman table with code lengths > 16",
        "`GpuRawU16` of floating-point data is not supported",
        "{b}-bit uncompressed",
        "YCbCr sampling 2x2",
        "X-Trans layout...",
    ] {
        assert_eq!(violation(ok), None, "{ok:?}");
    }
    for bad in ["Truncated header", "truncated header.", "truncated header ", " truncated", ""] {
        assert!(violation(bad).is_some(), "{bad:?}");
    }
    assert_eq!(error_violation("CorruptFile", "corrupt file: truncated header"), None);
    assert!(error_violation("CorruptFile", "corrupt file: Truncated header").is_some());
    assert!(error_violation("CorruptFile", "corrupt file: corrupt header").is_some());
    assert!(error_violation("InvalidMetadata", "invalid metadata: invalid metadata").is_some());
    assert_eq!(error_violation("InvalidMetadata", "invalid metadata: invalid DNG CFAPattern"), None);
}

// --- Run time -------------------------------------------------------------------------

#[test]
fn error_kinds_display_in_the_style() {
    let errors = [
        Error::UnsupportedFormat(String::new()),
        Error::UnsupportedFormat("not a TIFF header".into()),
        Error::unsupported_compression("DNG", "TIFF compression 7"),
        Error::corrupt("truncated header"),
        Error::corrupt_at("read beyond end of data", 0x40),
        Error::invalid_metadata("missing TIFF tag 0x0111"),
        Error::CameraUnsupported { make: "Canon".into(), model: "EOS".into(), reason: String::new() },
        Error::GpuUnavailable("no GPU adapter".into()),
        Error::OutOfMemory("failed to allocate 8 bytes".into()),
        Error::LimitExceeded("dimension 9x9 exceeds 8".into()),
        Error::NotAvailable("no tiles".into()),
        Error::InvalidArgument("empty region".into()),
        Error::Io(std::io::Error::other("disk on fire")),
    ];
    for e in &errors {
        check_error(e).unwrap();
    }
    let p: flashraw::ParseOptionError = "nope".parse::<flashraw::Backend>().unwrap_err();
    assert_eq!(violation(&p.to_string()), None);
}

/// Decode every way the API allows and check each error and warning.
fn check_all(data: &[u8]) -> Result<(), String> {
    let limits = Limits::default().with_max_samples(64 << 20).with_max_dimension(65535);
    let raw = match FlashRaw::open_with(data, limits) {
        Ok(raw) => raw,
        Err(e) => return check_error(&e),
    };
    for p in raw.previews() {
        if let Err(e) = raw.preview_bytes(p) {
            check_error(&e)?;
        }
    }
    for policy in [CorruptionPolicy::Strict, CorruptionPolicy::Recover] {
        let opts = DecodeOptions { limits, threads: Some(2), corruption_policy: policy, ..Default::default() };
        check_decode(&raw.decode(opts.clone()))?;
        check_decode(&raw.decode_region(flashraw::Rect::new(1, 1, 7, 5), opts.clone()))?;
        check_decode(&raw.decode_region(flashraw::Rect::new(0, 0, u32::MAX / 2, 4), opts))?;
    }
    Ok(())
}

#[test]
fn hostile_inputs_produce_styled_errors() {
    let mut inputs: Vec<Vec<u8>> = vec![vec![], vec![0; 16], b"II*\0".to_vec(), b"MM\0*\0\0\0\x08".to_vec()];
    // Magic prefixes of the supported containers, alone and padded with garbage.
    let magics: &[&[u8]] = &[
        b"II*\0\x08\0\0\0",
        b"MM\0*\0\0\0\x08",
        b"IIRO\x08\0\0\0",
        b"IIU\0\x08\0\0\0",
        b"FUJIFILMCCD-RAW ",
        b"\0MRM\0\0\0\0",
        b"\0\0\0\x18ftypcrx ",
        b"HEAPCCDR",
        b"FOVb",
        b"IIII\x01\0\0\0",
        b"ARRI\x12\x34\x56\x78",
        b"\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    ];
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for m in magics {
        inputs.push(m.to_vec());
        for len in [64, 600, 5000] {
            let mut v = m.to_vec();
            while v.len() < len {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                v.push(x as u8);
            }
            inputs.push(v);
        }
    }
    let failures: Vec<String> = inputs
        .iter()
        .enumerate()
        .filter_map(|(i, d)| check_all(d).err().map(|e| format!("input {i} ({} bytes): {e}", d.len())))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Truncated copies of corpus files (one per extension) when the corpus is present.
#[test]
fn truncated_corpus_files_produce_styled_errors() {
    let corpus = std::env::var("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    if !corpus.exists() {
        eprintln!("corpus not present; skipping");
        return;
    }
    let mut by_ext: std::collections::BTreeMap<String, (u64, PathBuf)> = Default::default();
    let mut stack = vec![corpus];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let Some(ext) = p.extension().map(|x| x.to_string_lossy().to_lowercase()) else { continue };
            if matches!(ext.as_str(), "jpg" | "jpeg" | "txt" | "md" | "xmp" | "json" | "tsv" | "png" | "pp3" | "html") {
                continue;
            }
            let size = e.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
            if size < 40_000_000 && by_ext.get(&ext).is_none_or(|(s, _)| *s > size) {
                by_ext.insert(ext, (size, p));
            }
        }
    }
    let failures: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = by_ext
            .values()
            .map(|(_, p)| {
                s.spawn(move || {
                    let data = std::fs::read(p).unwrap();
                    let mut out = vec![];
                    for frac in [0.0005, 0.005, 0.05, 0.3, 0.7, 0.99] {
                        let cut = (data.len() as f64 * frac) as usize;
                        if let Err(e) = check_all(&data[..cut]) {
                            out.push(format!("{} cut at {cut}: {e}", p.display()));
                        }
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    eprintln!("{} formats checked", by_ext.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
