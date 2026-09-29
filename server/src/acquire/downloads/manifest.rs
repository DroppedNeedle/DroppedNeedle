//! Per-task staging manifest, ported from `models/download_manifest.py`.
//!
//! slskd has no batch id and no per-request external id, so a task is tied
//! to its transfers only by source identity plus the exact filenames it
//! enqueued. The manifest persists that pair to
//! `staging/{task_id}/manifest.json` at enqueue time so a restart can
//! re-correlate the task and finish the import. Staging holds only this
//! file; the audio itself lives in the client's download directory.
//!
//! Decoding is lenient: unknown fields are ignored and a legacy manifest
//! carrying only `source_username` gets a soulseek handle back-filled, so
//! a download mid-flight at upgrade survives the deploy.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One file the task enqueued. `filename` is the correlation key; the
/// optional duration feeds the post-download duration check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedFile {
    /// Exact client-side filename.
    pub filename: String,
    /// Advertised size in bytes.
    pub size: i64,
    /// Expected audio duration in seconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

/// One track from the task's exact edition, keyed by disc/position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedTrack {
    /// Position within the disc (1-based).
    pub track_number: i64,
    /// Disc number (1-based).
    #[serde(default = "default_disc")]
    pub disc_number: i64,
    /// Canonical length in seconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// Recording MBID for corroborating evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_mbid: Option<String>,
    /// Track title for corroborating evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Release-track MBID; makes attribution edition-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_track_mbid: Option<String>,
}

fn default_disc() -> i64 {
    1
}

/// Client correlation handle. Soulseek fills username plus filenames (no
/// batch id); usenet fills the job name before enqueue so it survives a
/// crash between enqueue and journaling.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TaskHandle {
    /// `soulseek`, `usenet`, or `plugin:<key>`.
    #[serde(default)]
    pub source: String,
    /// Soulseek peer username.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// Exact enqueued filenames.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filenames: Vec<String>,
    /// Client job name (`droppedneedle-{task_id}[-{n}]` for usenet).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub job_name: String,
}

/// Durable per-task import and correlation record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadManifest {
    /// Owning task id.
    pub task_id: String,
    /// Release-group MBID being fetched.
    #[serde(default)]
    pub release_group_mbid: String,
    /// Artist display name for the importer.
    #[serde(default)]
    pub artist_name: String,
    /// Album title for the importer.
    #[serde(default)]
    pub album_title: String,
    /// Naming template the import must render with.
    #[serde(default)]
    pub naming_template: String,
    /// Files the task enqueued.
    #[serde(default)]
    pub target_files: Vec<ExpectedFile>,
    /// Legacy slskd correlation (pre-handle manifests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_username: Option<String>,
    /// Generalised correlation handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<TaskHandle>,
    /// Complete selected-edition track map.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_tracks: Vec<ExpectedTrack>,
    /// Selected edition MBID, when pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_mbid: Option<String>,
    /// Artist MBID, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    /// Release year, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    /// Single-track download whose duration is canonical: a mismatch means
    /// "wrong track", not a corrupt file to quarantine.
    #[serde(default)]
    pub is_track: bool,
    /// Set by the last-resort re-pull: hold (never silently import or
    /// discard) on a repeat gate failure.
    #[serde(default)]
    pub hold_on_wrong_track: bool,
    /// Owning task's origin (`user`, `retry`, `upgrade`).
    #[serde(default = "default_origin")]
    pub origin: String,
    /// Explicit administrator for conversion-held items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by_user_id: Option<String>,
    /// Candidate-attempt journal identity; old manifests carry none and
    /// are linked conservatively by their exact client job at startup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
}

fn default_origin() -> String {
    "user".to_string()
}

impl DownloadManifest {
    /// In-flight back-fill: a legacy manifest decodes with `handle` unset
    /// and `source_username` set; synthesise the soulseek handle so poll,
    /// cancel, and import re-correlate exactly as before.
    pub fn backfill_handle(&mut self) {
        if self.handle.is_none()
            && let Some(username) = self.source_username.clone()
        {
            self.handle = Some(TaskHandle {
                source: "soulseek".to_string(),
                username,
                filenames: self
                    .target_files
                    .iter()
                    .map(|file| file.filename.clone())
                    .collect(),
                job_name: String::new(),
            });
        }
    }
}

/// Encode/decode manifests to the on-disk JSON bytes.
#[derive(Debug, Clone, Copy, Default)]
pub struct ManifestCodec;

impl ManifestCodec {
    /// Whether a task id is safe to join under the staging root: the minted
    /// 32-hex shape, or at minimum separator-free with no parent markers.
    pub fn valid_task_id(task_id: &str) -> bool {
        if task_id.len() == 32 && task_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return true;
        }
        !task_id.is_empty()
            && !task_id.contains(['/', '\\'])
            && task_id != ".."
            && !task_id.split('/').any(|part| part == "..")
    }

    /// Path of a task's manifest under the staging root.
    pub fn path(staging_root: &Path, task_id: &str) -> PathBuf {
        staging_root.join(task_id).join("manifest.json")
    }

    /// Checked path: `None` when the task id could escape the staging root.
    pub fn checked_path(staging_root: &Path, task_id: &str) -> Option<PathBuf> {
        Self::valid_task_id(task_id).then(|| Self::path(staging_root, task_id))
    }

    /// Serialize a manifest. Unknown fields are never written.
    pub fn encode(&self, manifest: &DownloadManifest) -> Result<Vec<u8>, ManifestError> {
        serde_json::to_vec(manifest).map_err(ManifestError::Encode)
    }

    /// Deserialize, ignoring unknown fields and back-filling a legacy
    /// soulseek handle when only `source_username` is present.
    pub fn decode(&self, data: &[u8]) -> Result<DownloadManifest, ManifestError> {
        let mut manifest: DownloadManifest =
            serde_json::from_slice(data).map_err(ManifestError::Decode)?;
        manifest.backfill_handle();
        Ok(manifest)
    }

    /// Read a task's manifest from the staging root.
    pub fn read(
        &self,
        staging_root: &Path,
        task_id: &str,
    ) -> Result<DownloadManifest, ManifestError> {
        let Some(path) = Self::checked_path(staging_root, task_id) else {
            return Err(ManifestError::InvalidTaskId {
                task_id: task_id.to_owned(),
            });
        };
        let bytes = std::fs::read(&path).map_err(|source| ManifestError::Io {
            path: path.clone(),
            detail: source.to_string(),
        })?;
        self.decode(&bytes)
    }

    /// Write a task's manifest, creating the task directory first.
    pub fn write(
        &self,
        staging_root: &Path,
        manifest: &DownloadManifest,
    ) -> Result<PathBuf, ManifestError> {
        let Some(path) = Self::checked_path(staging_root, &manifest.task_id) else {
            return Err(ManifestError::InvalidTaskId {
                task_id: manifest.task_id.clone(),
            });
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ManifestError::Io {
                path: parent.to_path_buf(),
                detail: source.to_string(),
            })?;
        }
        let bytes = self.encode(manifest)?;
        std::fs::write(&path, bytes).map_err(|source| ManifestError::Io {
            path: path.clone(),
            detail: source.to_string(),
        })?;
        Ok(path)
    }
}

/// Manifest failures: encode, decode, or staging I/O.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Serialization failed.
    #[error("manifest encode failed: {0}")]
    Encode(#[source] serde_json::Error),
    /// Deserialization failed.
    #[error("manifest decode failed: {0}")]
    Decode(#[source] serde_json::Error),
    /// Staging read or write failed.
    #[error("manifest I/O failed for {}: {detail}", path.display())]
    Io {
        /// Path being read or written.
        path: PathBuf,
        /// Underlying OS error text.
        detail: String,
    },
    /// The task id could escape the staging root.
    #[error("manifest task id is not a safe path component: {task_id}")]
    InvalidTaskId {
        /// Offending task id.
        task_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_manifest_backfills_soulseek_handle() {
        let codec = ManifestCodec;
        let raw = br#"{
            "task_id": "t1", "release_group_mbid": "rg",
            "artist_name": "a", "album_title": "b", "naming_template": "t",
            "target_files": [{"filename": "01.flac", "size": 10}],
            "source_username": "peer", "unknown_future_field": 42
        }"#;
        let manifest = codec.decode(raw).unwrap();
        let handle = manifest.handle.unwrap();
        assert_eq!(handle.source, "soulseek");
        assert_eq!(handle.username, "peer");
        assert_eq!(handle.filenames, vec!["01.flac".to_string()]);
        assert_eq!(manifest.origin, "user");
    }
}
