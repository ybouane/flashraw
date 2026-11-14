//! The error-message style, shared by the tests that check it.
//!
//! A message starts in lower case (unless its first word is a proper noun, an acronym, a
//! format or camera name, or code in backticks), has no trailing period and no leading or
//! trailing whitespace. `Error`'s Display adds a prefix per kind ("corrupt file: ...");
//! the text after it follows the same rules and does not repeat the prefix.

#![allow(dead_code)] // each test binary uses part of this module

/// Capitalised words a message may start with. Words with another capital letter or a
/// digit after the first character (NEF, CR2, YCbCr, FlashRaw, GpuRawU16...) are accepted
/// as acronyms, format names or identifiers without being listed.
const PROPER_NOUNS: &[&str] = &[
    "Bayer",
    "Canon",
    "Epson",
    "Fuji",
    "Fujifilm",
    "Hasselblad",
    "Huffman",
    "Kodak",
    "Leaf",
    "Leica",
    "Light",
    "Mamiya",
    "Minolta",
    "Nikon",
    "Node",
    "Olympus",
    "Panasonic",
    "Pentax",
    "Phase",
    "Pillow",
    "Python",
    "Samsung",
    "Sigma",
    "Sinar",
    "Sony",
    "Web",
];

/// Display prefix of each `Error` kind (`Io` is left out: it carries the OS message), and
/// the words the text after it must not start with (they would repeat the prefix).
const KIND_PREFIXES: &[(&str, &str, &str)] = &[
    ("UnsupportedFormat", "unsupported or unrecognised file format", "unsupported"),
    ("UnsupportedCompression", "unsupported compression in ", "unsupported"),
    ("CorruptFile", "corrupt file: ", "corrupt"),
    ("InvalidMetadata", "invalid metadata: ", "invalid metadata"),
    ("CameraUnsupported", "unsupported camera: ", "unsupported"),
    ("GpuUnavailable", "GPU unavailable: ", "GPU unavailable"),
    ("OutOfMemory", "out of memory: ", "out of memory"),
    ("LimitExceeded", "limit exceeded: ", "limit exceeded"),
    ("NotAvailable", "not available: ", "not available"),
    ("InvalidArgument", "invalid argument: ", "invalid argument"),
];

/// Why the first word of `s` may not start a message, if it may not.
fn leading_word(s: &str) -> Option<String> {
    let first = s.chars().next()?;
    if !first.is_uppercase() {
        return None; // lower case, a digit, a placeholder (`{`), code (`` ` ``) or a quote
    }
    let word: String = s.chars().take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '/').collect();
    let rest = &word[first.len_utf8()..];
    if rest.chars().any(|c| c.is_uppercase() || c.is_ascii_digit()) || word.chars().count() == 1 {
        return None; // acronym, format name, model code or identifier
    }
    if PROPER_NOUNS.contains(&word.as_str()) {
        return None;
    }
    Some(format!("starts with a capital letter ({word:?})"))
}

/// The style violation in a message, if any.
pub fn violation(msg: &str) -> Option<String> {
    if msg.is_empty() {
        return Some("empty message".into());
    }
    if msg.trim_start() != msg {
        return Some("leading whitespace".into());
    }
    if msg.trim_end() != msg || msg.ends_with("\\n") {
        return Some("trailing whitespace".into());
    }
    if msg.ends_with('.') && !msg.ends_with("...") {
        return Some("trailing period".into());
    }
    leading_word(msg)
}

/// The style violation in an error's `to_string()`, if any: the general rules, plus the
/// text after the kind's prefix follows them and does not repeat the prefix.
pub fn error_violation(kind: &str, msg: &str) -> Option<String> {
    if let Some(v) = violation(msg) {
        return Some(v);
    }
    let &(_, prefix, repeat) = KIND_PREFIXES.iter().find(|(k, _, _)| *k == kind)?;
    let Some(inner) = msg.strip_prefix(prefix) else {
        return Some(format!("{kind} message does not start with {prefix:?}"));
    };
    let inner = inner.strip_prefix(": ").unwrap_or(inner);
    if inner.is_empty() {
        return None;
    }
    if let Some(v) = violation(inner) {
        return Some(format!("after {prefix:?}: {v}"));
    }
    let lower = inner.to_lowercase();
    if lower == repeat || lower.starts_with(&format!("{} ", repeat.to_lowercase())) {
        return Some(format!("repeats the {repeat:?} prefix"));
    }
    None
}

/// Check an error; `Err` describes the violation.
pub fn check_error(e: &flashraw::Error) -> Result<(), String> {
    let msg = e.to_string();
    match error_violation(e.kind(), &msg) {
        Some(v) => Err(format!("error message {msg:?}: {v}")),
        None => Ok(()),
    }
}

/// Check a decode warning; `Err` describes the violation.
pub fn check_warning(w: &flashraw::DecodeWarning) -> Result<(), String> {
    let msg = w.to_string();
    match violation(&msg) {
        Some(v) => Err(format!("warning message {msg:?}: {v}")),
        None => Ok(()),
    }
}

/// Check the outcome of a decode: its error, or its warnings.
pub fn check_decode(r: &flashraw::Result<flashraw::RawFrame>) -> Result<(), String> {
    match r {
        Ok(f) => f.warnings().iter().try_for_each(check_warning),
        Err(e) => check_error(e),
    }
}
