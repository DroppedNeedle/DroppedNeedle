//! NZBGet wire models for its JSON-RPC API. Every answer the client reads
//! decodes into one of these; a job without its `NZBID`, or a config entry
//! without its name, fails decoding instead of matching as an empty id.
//! Sizes come split into 32-bit halves (`...Lo`/`...Hi`).
//!
//! Shapes checked on 2026-11-16 against NZBGet 26.3 (the
//! `nzbgetcom/nzbget` image): `version`, `config`, `listgroups`, `history`,
//! `append` and `editqueue`, and the in-repo `NzbgetMock` serves the same.

use serde::Deserialize;

/// One JSON-RPC answer: a `result`, or an `error` with NZBGet's message.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct RpcReply {
    /// The method's answer, absent or null on failure.
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    /// The failure, when NZBGet reports one.
    #[serde(default)]
    pub error: Option<RpcError>,
}

/// NZBGet's JSON-RPC error object.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct RpcError {
    /// NZBGet's own wording.
    #[serde(default)]
    pub message: String,
}

/// One `config` entry (`MainDir`, `DestDir`, `Category1.Name`, ...).
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigEntry {
    /// Option name, as NZBGet spells it.
    #[serde(rename = "Name")]
    pub name: String,
    /// Option value; `${Option}` references are left unexpanded.
    #[serde(rename = "Value", default)]
    pub value: String,
}

/// One job still in the download queue (`listgroups`).
#[derive(Debug, Clone, Deserialize)]
pub struct Group {
    /// The job id; required.
    #[serde(rename = "NZBID")]
    pub nzb_id: i64,
    /// The job name: the added file's name without `.nzb`.
    #[serde(rename = "NZBName", default)]
    pub nzb_name: String,
    /// The dupe key DroppedNeedle sets to the job name.
    #[serde(rename = "DupeKey", default)]
    pub dupe_key: String,
    /// `QUEUED`, `PAUSED`, `DOWNLOADING`, `FETCHING` or a
    /// post-processing state.
    #[serde(rename = "Status", default)]
    pub status: String,
    /// Low 32 bits of the job's size.
    #[serde(rename = "FileSizeLo", default)]
    pub file_size_lo: u32,
    /// High 32 bits of the job's size.
    #[serde(rename = "FileSizeHi", default)]
    pub file_size_hi: u32,
    /// Low 32 bits of what is left to download.
    #[serde(rename = "RemainingSizeLo", default)]
    pub remaining_size_lo: u32,
    /// High 32 bits of what is left to download.
    #[serde(rename = "RemainingSizeHi", default)]
    pub remaining_size_hi: u32,
}

impl Group {
    /// The job's size in bytes.
    pub fn size(&self) -> u64 {
        join(self.file_size_lo, self.file_size_hi)
    }

    /// Bytes still to download.
    pub fn remaining(&self) -> u64 {
        join(self.remaining_size_lo, self.remaining_size_hi)
    }
}

/// One finished job (`history`).
#[derive(Debug, Clone, Deserialize)]
pub struct HistoryItem {
    /// The job id; required.
    #[serde(rename = "NZBID")]
    pub nzb_id: i64,
    /// The display name.
    #[serde(rename = "Name", default)]
    pub name: String,
    /// The job name: the added file's name without `.nzb`.
    #[serde(rename = "NZBName", default)]
    pub nzb_name: String,
    /// The dupe key DroppedNeedle sets to the job name.
    #[serde(rename = "DupeKey", default)]
    pub dupe_key: String,
    /// `CATEGORY/DETAIL`, such as `SUCCESS/ALL` or `FAILURE/PAR`.
    #[serde(rename = "Status", default)]
    pub status: String,
    /// Where the job was written while it ran.
    #[serde(rename = "DestDir", default)]
    pub dest_dir: String,
    /// Where a script moved it afterwards, when one did.
    #[serde(rename = "FinalDir", default)]
    pub final_dir: String,
    /// Repair outcome.
    #[serde(rename = "ParStatus", default)]
    pub par_status: String,
    /// Unpack outcome.
    #[serde(rename = "UnpackStatus", default)]
    pub unpack_status: String,
    /// Move outcome.
    #[serde(rename = "MoveStatus", default)]
    pub move_status: String,
    /// Post-processing script outcome.
    #[serde(rename = "ScriptStatus", default)]
    pub script_status: String,
    /// Low 32 bits of the job's size.
    #[serde(rename = "FileSizeLo", default)]
    pub file_size_lo: u32,
    /// High 32 bits of the job's size.
    #[serde(rename = "FileSizeHi", default)]
    pub file_size_hi: u32,
}

impl HistoryItem {
    /// The job's size in bytes.
    pub fn size(&self) -> u64 {
        join(self.file_size_lo, self.file_size_hi)
    }

    /// The job's folder on NZBGet's side: `FinalDir` when a script moved
    /// it, else `DestDir`.
    pub fn storage(&self) -> &str {
        if self.final_dir.is_empty() {
            &self.dest_dir
        } else {
            &self.final_dir
        }
    }

    /// The part of `Status` before the slash, upper-cased.
    pub fn outcome(&self) -> String {
        self.status
            .split('/')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase()
    }
}

fn join(lo: u32, hi: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}
