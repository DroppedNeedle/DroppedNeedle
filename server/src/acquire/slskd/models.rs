//! Wire structs mirroring slskd 0.25.1 JSON shapes.
//!
//! Ported from v2's slskd models (shapes verified
//! against a live slskd 0.25.1.0 instance). Unknown fields are ignored on
//! decode so newer slskd versions stay readable; only the fields the
//! repository uses are modeled.

use serde::{Deserialize, Serialize};

/// One file in a peer's search response.
///
/// `bit_rate` is absent for lossless files (v2 models: "left None, do not
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
/// is no batch GUID: each file becomes its own transfer.
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
/// (v2 PR #222); left `None` there.
/// `id` and `filename` are required: a record without them fails decoding.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlskdTransfer {
    pub id: String,
    #[serde(default)]
    pub username: String,
    pub filename: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub bytes_transferred: i64,
    #[serde(default)]
    pub bytes_remaining: i64,
    #[serde(default)]
    pub percent_complete: f64,
    #[serde(default)]
    pub average_speed: f64,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub direction: String,
    #[serde(default)]
    pub place_in_queue: Option<i64>,
    #[serde(default)]
    pub exception: Option<String>,
    #[serde(default)]
    pub requested_at: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
}

/// One directory of a peer's transfers.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SlskdTransferDirectory {
    pub directory: String,
    pub files: Vec<SlskdTransfer>,
}

/// One peer's transfers, grouped by directory (GET
/// /api/v0/transfers/downloads/{username}, and each element of GET
/// /api/v0/transfers/downloads).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlskdUserTransfers {
    pub username: String,
    #[serde(default)]
    pub directories: Vec<SlskdTransferDirectory>,
}

impl SlskdUserTransfers {
    /// Every transfer, each carrying the peer's username.
    #[must_use]
    pub fn into_transfers(self) -> Vec<SlskdTransfer> {
        let username = self.username;
        self.directories
            .into_iter()
            .flat_map(|directory| directory.files)
            .map(|mut transfer| {
                if transfer.username.is_empty() {
                    transfer.username.clone_from(&username);
                }
                transfer
            })
            .collect()
    }
}

/// `directories` block of GET /api/v0/options: where slskd saves files
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
