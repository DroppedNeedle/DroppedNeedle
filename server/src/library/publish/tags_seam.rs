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

/// A semantic tag document: managed fields, custom tags, and opaque
/// unknown native frames (raw ID3 frames, stray atoms, and similar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagDocument {
    /// Managed semantic fields; each holds an ordered value list so
    /// multi-valued tags keep their boundaries.
    pub managed: BTreeMap<String, Vec<String>>,
    /// Custom and unknown-but-named tags, preserved by default.
    pub custom: BTreeMap<String, Vec<String>>,
    /// Unknown native frames as opaque bytes, keyed by native
    /// descriptor. The writer never inspects these.
    #[serde(default)]
    pub unknown_frames: BTreeMap<String, Vec<u8>>,
    /// Managed fields the file held in a shape that cannot be written
    /// back (see `tags::FieldDocument`). No write touches them, and a
    /// restore never removes them.
    #[serde(default)]
    pub opaque: Vec<String>,
}

impl TagDocument {
    /// Empty document.
    pub fn empty() -> Self {
        Self {
            managed: BTreeMap::new(),
            custom: BTreeMap::new(),
            unknown_frames: BTreeMap::new(),
            opaque: Vec::new(),
        }
    }
}
