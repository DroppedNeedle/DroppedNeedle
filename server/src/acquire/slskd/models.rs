//! Wire structs mirroring slskd 0.25.1 JSON shapes.
//!
//! Ported from `backend/repositories/slskd/slskd_models.py` (shapes verified
//! against a live slskd 0.25.1.0 instance). Unknown fields are ignored on
//! decode so newer slskd versions stay readable; only the fields the
//! repository uses are modeled.

use serde::{Deserialize, Serialize};

/// One file in a peer's search response.
///
/// `bit_rate` is ABSENT for lossless files (v2 models: "left None, do not
/// coerce to 0"). `extension` can be empty even for a real file, so the
/// repository parses the extension from `filename` instead (v2 C6a).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SlskdFile {
    pub filename: String,
    pub size: i64,
    pub extension: String,
    pub bit_depth: Option<i32>,
    pub sample_rate: Option<i32>,
    /// Track duration in seconds.
    pub length: Option<f64>,
    /// Absent (`None`) for lossless; must never be coerced to 0.
    pub bit_rate: Option<i32>,
    pub code: Option<i32>,
    pub is_locked: bool,
}

/// One peer's answer to a search.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SlskdUserSearchResponse {
    pub username: String,
    pub has_free_upload_slot: bool,
    pub upload_speed: i64,
    pub queue_length: i64,
    pub file_count: i64,
    pub locked_file_count: i64,
    pub files: Vec<SlskdFile>,
    pub locked_files: Vec<SlskdFile>,
    pub token: Option<i64>,
}

/// Search state. `state` is a comma-joined flags string such as
/// `"Completed, Succeeded"` (v2 models).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SlskdSearchResponse {
    pub id: String,
    pub state: String,
    pub is_complete: bool,
    pub search_text: String,
    pub file_count: i64,
    pub response_count: i64,
    pub locked_file_count: i64,
    pub token: Option<i64>,
}

/// 201 body from enqueue. Keys are PascalCase (`Enqueued` / `Failed`); there
/// is no batch GUID — each file becomes its own transfer (v2 C2).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SlskdEnqueueResponse {
    #[serde(rename = "Enqueued")]
    pub enqueued: Vec<serde_json::Value>,
    #[serde(rename = "Failed")]
    pub failed: Vec<serde_json::Value>,
}

/// One transfer record. `requested_at` / `started_at` are retry-attempt
/// timestamps (v2 #131/#253): slskd appends one record per attempt per file,
/// so recency-ordering records needs these. Absent on some slskd versions
/// (v2 PR #222) — left `None` there.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SlskdTransfer {
    pub id: String,
    pub username: String,
    pub filename: String,
    pub size: i64,
    pub bytes_transferred: i64,
    pub bytes_remaining: i64,
    pub percent_complete: f64,
    pub average_speed: f64,
    pub state: String,
    pub direction: String,
    pub place_in_queue: Option<i64>,
    pub exception: Option<String>,
    pub requested_at: Option<String>,
    pub started_at: Option<String>,
}

/// `directories` block of GET /api/v0/options — where slskd saves files
/// (verified keys `downloads` and `incomplete`; both are slskd's
/// in-container paths, v2 models).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SlskdDirectories {
    pub downloads: String,
    pub incomplete: String,
}

/// Subset of GET /api/v0/options the repository uses: just the directories
/// block, so DroppedNeedle can tell the user the exact path slskd
/// downloads to. Unknown fields ignored.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SlskdOptions {
    pub directories: SlskdDirectories,
}
