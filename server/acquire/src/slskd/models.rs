//! Wire structs mirroring slskd 0.25.1 JSON shapes.
//!
//! Shapes were verified against a live slskd 0.25.1.0 instance under v2;
//! that run's date is not recorded. Each
//! model was checked again on 2026-10-21 against the in-repo `MockSlskd`
//! in the testkit crate, the recording of those 0.25.1.0 answers. Unknown
//! fields are ignored on decode so newer slskd versions stay readable;
//! only the fields the repository uses are modeled. Identity fields are
//! required: an answer without one fails decoding instead of reading as
//! an empty id.

use serde::{Deserialize, Serialize};

/// One file in a peer's search response (`GET /searches/{id}/responses`,
/// each peer's `files`; checked against `MockSlskd`'s canned peers).
///
/// `filename` and `size` are required: they are what an enqueue names.
/// `bit_rate` is absent for lossless files: left `None`,
/// never coerced to 0. `extension` can be empty even for a real file, so the
/// repository parses the extension from `filename` instead.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlskdFile {
    pub filename: String,
    pub size: i64,
    #[serde(default)]
    pub extension: String,
    #[serde(default)]
    pub bit_depth: Option<i32>,
    #[serde(default)]
    pub sample_rate: Option<i32>,
    /// Track duration in seconds.
    #[serde(default)]
    pub length: Option<f64>,
    /// Absent (`None`) for lossless; must never be coerced to 0.
    #[serde(default)]
    pub bit_rate: Option<i32>,
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub is_locked: bool,
}

/// One peer's answer to a search (`GET /searches/{id}/responses`; checked
/// against `MockSlskd`). `username` is required: it is the peer an enqueue
/// goes to.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlskdUserSearchResponse {
    pub username: String,
    #[serde(default)]
    pub has_free_upload_slot: bool,
    #[serde(default)]
    pub upload_speed: i64,
    #[serde(default)]
    pub queue_length: i64,
    #[serde(default)]
    pub file_count: i64,
    #[serde(default)]
    pub locked_file_count: i64,
    #[serde(default)]
    pub files: Vec<SlskdFile>,
    #[serde(default)]
    pub locked_files: Vec<SlskdFile>,
    #[serde(default)]
    pub token: Option<i64>,
}

/// Search state (`POST /searches` and `GET /searches/{id}`; checked
/// against `MockSlskd`, whose state read carries only `id`, `state`,
/// `isComplete` and `responseCount`). `id` is required: the poll reads
/// the search by it. `state` is a comma-joined flags string such as
/// `"Completed, Succeeded"`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlskdSearchResponse {
    pub id: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub is_complete: bool,
    #[serde(default)]
    pub search_text: String,
    #[serde(default)]
    pub file_count: i64,
    #[serde(default)]
    pub response_count: i64,
    #[serde(default)]
    pub locked_file_count: i64,
    #[serde(default)]
    pub token: Option<i64>,
}

/// 201 body from enqueue (checked against `MockSlskd`). Keys are
/// PascalCase (`Enqueued` / `Failed`); there is no batch GUID: each file
/// becomes its own transfer, matched later by username and filename. Both
/// lists default to empty on purpose: the repository then falls back to
/// the files it asked for.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SlskdEnqueueResponse {
    #[serde(rename = "Enqueued")]
    pub enqueued: Vec<serde_json::Value>,
    #[serde(rename = "Failed")]
    pub failed: Vec<serde_json::Value>,
}

/// One transfer record (`GET /transfers/downloads[/{username}]`, each
/// directory's `files`; checked against `MockSlskd`). `requested_at` / `started_at` are retry-attempt
/// timestamps (issues #131/#253): slskd appends one record per attempt per
/// file, so recency-ordering records needs these. Absent on some slskd versions
/// (PR #222); left `None` there. `id` and `filename` are required: a record
/// without them fails decoding.
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

/// `directories` block of GET /api/v0/options (checked against
/// `MockSlskd`): where slskd saves files
/// (verified keys `downloads` and `incomplete`; both are slskd's
/// in-container paths).
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
