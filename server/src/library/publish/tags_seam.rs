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
    /// Text items outside the writable fields, keyed as
    /// TagEdit::native takes them. Recorded so a restore can put back what
    /// a scrub removed; the writer itself never touches them otherwise.
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
    /// Document shape. Version 2 records every writable field the file
    /// held (and its opaque fields); older documents held a subset, so
    /// a restore never infers from them that a field was absent. Version
    /// 3 adds ISRC and version 4 adds ReplayGain, lyrics, pictures and
    /// native custom items (see [`recorded_since`]).
    #[serde(default)]
    pub version: u32,
    /// The embedded pictures, in file order, by the hash of their bytes
    /// in the blob store. `None` when the document did not record them
    /// (documents before version 3, v2 baselines): nothing may then be
    /// inferred about the file's pictures.
    #[serde(default)]
    pub pictures: Option<Vec<PictureRef>>,
    /// The picture bytes behind [`Self::pictures`], carried from the file
    /// to the publisher, which stores them with the before-state. Never
    /// serialized.
    #[serde(skip)]
    pub picture_bytes: BTreeMap<String, Vec<u8>>,
}

/// One embedded picture in a [`TagDocument`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PictureRef {
    pub kind: u8,
    pub mime: String,
    pub description: String,
    pub sha256: String,
}

impl PictureRef {
    /// The managed-update value for this picture
    /// (`kind<TAB>mime<TAB>description<TAB>sha256`).
    pub fn encode(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}",
            self.kind,
            self.mime,
            self.description.replace('\t', " "),
            self.sha256
        )
    }

    /// Parse [`Self::encode`].
    pub fn decode(value: &str) -> Option<Self> {
        let mut parts = value.splitn(4, '\t');
        Some(Self {
            kind: parts.next()?.parse().ok()?,
            mime: parts.next()?.to_owned(),
            description: parts.next()?.to_owned(),
            sha256: parts.next()?.to_owned(),
        })
    }
}

/// The managed-update name that replaces a file's embedded pictures with
/// the listed ones (each [`PictureRef::encode`]d); an empty list removes
/// them all.
pub const PICTURES_KEY: &str = "pictures";

/// The current [`TagDocument`] shape.
pub const TAG_DOCUMENT_VERSION: u32 = 4;

/// The first shape that records every writable field it knew of.
pub const FULL_FIELDS_VERSION: u32 = 2;

/// The first document shape that records `field`. A restore from an older
/// document leaves the field alone: it cannot tell absent from unread.
pub fn recorded_since(field: crate::library::tags::fields::TagField) -> u32 {
    use crate::library::tags::fields::TagField;
    match field {
        TagField::Isrc => 3,
        TagField::ReplayGainTrackGain
        | TagField::ReplayGainTrackPeak
        | TagField::ReplayGainAlbumGain
        | TagField::ReplayGainAlbumPeak
        | TagField::Lyrics => RICH_FIELDS_VERSION,
        _ => FULL_FIELDS_VERSION,
    }
}

/// The first shape that records ReplayGain, lyrics, pictures and native
/// custom items. A restore from an older document leaves pictures and
/// native items alone.
pub const RICH_FIELDS_VERSION: u32 = 4;

impl TagDocument {
    /// Empty document.
    pub fn empty() -> Self {
        Self {
            managed: BTreeMap::new(),
            custom: BTreeMap::new(),
            unknown_frames: BTreeMap::new(),
            opaque: Vec::new(),
            version: TAG_DOCUMENT_VERSION,
            pictures: Some(Vec::new()),
            picture_bytes: BTreeMap::new(),
        }
    }
}
