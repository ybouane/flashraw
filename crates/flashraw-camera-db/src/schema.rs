// Shared between build.rs (via `include!`) and the `tooling` module: the schema of
// the single camera database file, camera-db/cameras.json.

/// The whole camera database.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DbFile {
    /// Schema version of this file.
    #[serde(default)]
    pub version: u32,
    /// Per-camera (and per decoder mode) entries.
    #[serde(default)]
    pub cameras: Vec<DbCamera>,
    /// Fallback colour data matched by canonical make + model prefix (table order).
    #[serde(default)]
    pub matrices: Vec<DbMatrix>,
    /// Model-name aliases: rebadged / regional names that map to a reference model.
    #[serde(default)]
    pub model_aliases: Vec<DbAliasGroup>,
    /// Sensor geometry selected by raw buffer size (older bodies without margin metadata).
    #[serde(default)]
    pub size_rules: Vec<DbSizeRule>,
}

/// Rebadged or regional model names that share a reference model's data.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DbAliasGroup {
    pub canonical_make: String,
    /// The reference model name.
    pub model: String,
    /// Other names written by cameras sharing the reference model's data.
    pub aliases: Vec<String>,
}

/// Geometry for a canonical make and exact raw buffer size: active area left/top and
/// width/height deltas relative to the buffer.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DbSizeRule {
    pub canonical_make: String,
    pub raw_width: u32,
    pub raw_height: u32,
    pub left: i32,
    pub top: i32,
    pub width_delta: i32,
    pub height_delta: i32,
}

/// How an entry's values were confirmed against sample files.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DbVerification {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub corpus_files: u32,
    /// Largest sample value actually present in the decoded corpus files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_data_max: Option<u32>,
    /// Real files contain samples more than 5% above the entry's white level (beyond any
    /// deliberate conservative clip point): the value needs attention.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub white_below_data: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DbMatrix {
    pub canonical_make: String,
    pub model_prefix: String,
    #[serde(default)]
    pub black: i32,
    #[serde(default)]
    pub white: i32,
    pub color_matrix: Vec<i32>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DbCamera {
    /// Make string exactly as written by the camera (EXIF Make).
    pub make: String,
    /// Model string exactly as written by the camera (EXIF Model).
    pub model: String,
    /// Decoder-specific mode ("" = default). See docs/camera-db.md for the mode vocabulary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    pub canonical_make: String,
    pub canonical_model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<DbAlias>,
    /// CFA pattern letters, row-major; rows separated by '/' for patterns wider than 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfa: Option<String>,
    /// Crop `[x, y, width, height]`; width/height <= 0 are relative to the right/bottom edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<[i32; 4]>,
    /// Vertical masked areas `[x, width]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub black_areas_v: Vec<[u32; 2]>,
    /// Horizontal masked areas `[y, height]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub black_areas_h: Vec<[u32; 2]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sensor: Vec<DbSensor>,
    /// XYZ(D65) → camera matrix scaled by 10000, row-major (9 or 12 values).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub color_matrix: Vec<i32>,
    /// Decoder quirks / hints (key → value).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub hints: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_size_match: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<DbVerification>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DbAlias {
    pub model: String,
    pub canonical: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DbSensor {
    pub black: i32,
    pub white: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub black_colors: Vec<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iso_min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iso_max: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iso_list: Vec<u32>,
}
