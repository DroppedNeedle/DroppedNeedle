//! Before-state snapshot document for undo and baseline restore.
//!
//! [`TagDocument`] is the semantic tag snapshot the publisher pins
//! before every Apply: managed fields replay through staging on
//! undo and baseline restore, so anything stored here must
//! round-trip through the real save wrapper (`tags::save` via
//! `staging`). Custom tags and unknown native frames stay preserved
//! by the save wrapper's byte-level handling, not by this document.
//!
//! The staged writer contract lives in `staging` now: managed-field
//! names outside the save wrapper's surface block at preview time,
//! and unknown frames round-trip untouched through the real tag
//! stack. There is no document-level simulation of a save here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::PublishError;

/// A semantic tag document: managed fields, custom tags, and opaque
/// unknown native frames (raw ID3 frames, stray atoms, and similar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagDocument {
    /// Managed semantic fields; each holds an ordered value list so
    /// multi-valued tags keep their boundaries (E21).
    pub managed: BTreeMap<String, Vec<String>>,
    /// Custom and unknown-but-named tags, preserved by default (D4).
    pub custom: BTreeMap<String, Vec<String>>,
    /// Unknown native frames as opaque bytes, keyed by native
    /// descriptor. The writer never inspects these.
    #[serde(default)]
    pub unknown_frames: BTreeMap<String, Vec<u8>>,
}

impl TagDocument {
    /// Empty document.
    pub fn empty() -> Self {
        Self {
            managed: BTreeMap::new(),
            custom: BTreeMap::new(),
            unknown_frames: BTreeMap::new(),
        }
    }

    /// Serialize for snapshot blobs.
    pub fn to_bytes(&self) -> Result<Vec<u8>, PublishError> {
        serde_json::to_vec(self).map_err(PublishError::from)
    }

    /// Parse back from a snapshot blob.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PublishError> {
        serde_json::from_slice(bytes).map_err(PublishError::from)
    }
}
