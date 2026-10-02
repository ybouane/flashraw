// Shared between build.rs (via `include!`) and the `tooling` module: the on-disk
// TOML schema of camera-db/cameras/*.toml.

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DbFile {
    #[serde(default, rename = "camera", skip_serializing_if = "Vec::is_empty")]
    pub cameras: Vec<DbCamera>,
    /// Fallback colour data matched by canonical make + model prefix (table order).
    #[serde(default, rename = "matrix", skip_serializing_if = "Vec::is_empty")]
    pub matrices: Vec<DbMatrix>,
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
