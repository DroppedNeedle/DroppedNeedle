//! Before-state snapshot document for undo and baseline restore, and the
//! tag updates a plan writes.
//!
//! [`TagUpdates`] is what crosses the seam from management to the
//! publisher and the tag writer, and its serde form is the one reader of
//! the stored shape in bundles and before-states.
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

use crate::tags::fields::TagField;

/// A semantic tag document: managed fields, custom tags, and opaque
/// unknown native frames (raw ID3 frames, stray atoms, and similar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagDocument {
    /// Managed semantic fields; each holds an ordered value list so
    /// multi-valued tags keep their boundaries. A document made for a write
    /// also holds the native items and pictures the write replaces.
    pub managed: TagUpdates,
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
    /// 3 adds ISRC, version 4 adds ReplayGain, lyrics, pictures and
    /// native custom items, version 5 the credit, work and AcoustID
    /// fields, and version 6 engineer, mixer, DJ-mixer, writer and
    /// director (see [`recorded_since`]).
    #[serde(default)]
    pub version: u32,
    /// The embedded pictures, in file order, by the hash of their bytes
    /// in the blob store. `None` when the document did not record them
    /// (documents before version 4, v2 baselines): nothing may then be
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
    /// FLAC/Ogg picture-block facts (width, height, colour depth,
    /// colours used) as the file held them; `None` for other formats and
    /// for art a run embeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<[u32; 4]>,
}

impl PictureRef {
    /// A picture as a document records it. A tab in the description
    /// becomes a space, as the stored record cannot hold one.
    pub fn new(
        kind: u8,
        mime: String,
        description: &str,
        sha256: String,
        info: Option<[u32; 4]>,
    ) -> Self {
        Self {
            kind,
            mime,
            description: description.replace('\t', " "),
            sha256,
            info,
        }
    }

    /// The same picture with its description as [`new`](Self::new) keeps
    /// it.
    pub fn normalised(&self) -> Self {
        Self::new(
            self.kind,
            self.mime.clone(),
            &self.description,
            self.sha256.clone(),
            self.info,
        )
    }

    /// The stored record (`kind<TAB>mime<TAB>description<TAB>sha256`, then
    /// `<TAB>width,height,depth,colours` when the facts are known), or
    /// `None` when a value holds a tab: such a record would read back as
    /// another picture.
    fn record(&self) -> Option<String> {
        if [&self.mime, &self.description, &self.sha256]
            .iter()
            .any(|text| text.contains('\t'))
        {
            return None;
        }
        let mut out = format!(
            "{}\t{}\t{}\t{}",
            self.kind, self.mime, self.description, self.sha256
        );
        if let Some([width, height, depth, colours]) = self.info {
            out.push_str(&format!("\t{width},{height},{depth},{colours}"));
        }
        Some(out)
    }

    /// The picture can be stored in a plan or a before-state.
    #[must_use]
    pub fn recordable(&self) -> bool {
        self.record().is_some()
    }

    /// Parse [`Self::record`].
    fn parse(value: &str) -> Option<Self> {
        let mut parts = value.splitn(5, '\t');
        let kind = parts.next()?.parse().ok()?;
        let mime = parts.next()?.to_owned();
        let description = parts.next()?.to_owned();
        let sha256 = parts.next()?.to_owned();
        let info = match parts.next() {
            None => None,
            Some(facts) => {
                let numbers: Vec<u32> = facts
                    .split(',')
                    .map(|part| part.parse().ok())
                    .collect::<Option<_>>()?;
                Some(<[u32; 4]>::try_from(numbers).ok()?)
            }
        };
        Some(Self {
            kind,
            mime,
            description,
            sha256,
            info,
        })
    }

    /// The same picture in the file: type, MIME type, description and
    /// bytes (picture-block facts are not compared).
    #[must_use]
    pub fn same_picture(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.mime == other.mime
            && self.description == other.description
            && self.sha256 == other.sha256
    }
}

/// The stored name of the update that replaces a file's embedded pictures.
const PICTURES_KEY: &str = "pictures";

/// The stored prefix of a native item outside the writable fields
/// (`native:vorbis:COMPOSER`).
const NATIVE_PREFIX: &str = "native:";

/// What one tag update writes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagTarget {
    /// A writable field, in every spelling the container has.
    Field(TagField),
    /// One Vorbis spelling of a field (`total_tracks:TRACKTOTAL`). Only
    /// documents read from a file carry these; requests cannot.
    Spelling(TagField, &'static str),
    /// A native item outside the writable fields (`vorbis:MOOD`): a scrub
    /// removes it, and undo or a baseline restore writes it back.
    Native(String),
}

impl TagTarget {
    /// The stored name in pieces, so names compare without building them.
    fn pieces(&self) -> [&str; 3] {
        match self {
            Self::Field(field) => [field.name(), "", ""],
            Self::Spelling(field, spelling) => [field.name(), ":", spelling],
            Self::Native(key) => [NATIVE_PREFIX, key, ""],
        }
    }

    /// The stored name: `title`, `total_tracks:TRACKTOTAL`,
    /// `native:vorbis:MOOD`.
    #[must_use]
    pub fn name(&self) -> String {
        self.pieces().concat()
    }

    fn parse(name: &str) -> Option<Self> {
        if let Some(key) = name.strip_prefix(NATIVE_PREFIX) {
            return Some(Self::Native(key.to_owned()));
        }
        match TagField::from_managed_name(name)? {
            (field, None) => Some(Self::Field(field)),
            (field, Some(spelling)) => Some(Self::Spelling(field, spelling)),
        }
    }
}

/// Targets sort by their stored name, so updates replay in the order they
/// always have: a field's own entry before its spellings, which then
/// override what it wrote to every spelling.
impl Ord for TagTarget {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let bytes = |target: &Self| -> Vec<u8> { target.pieces().concat().into_bytes() };
        match (self, other) {
            (Self::Field(left), Self::Field(right)) => left.name().cmp(right.name()),
            _ => bytes(self).cmp(&bytes(other)),
        }
    }
}

impl PartialOrd for TagTarget {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// One update, as [`TagUpdates::iter`] yields it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagUpdate<'a> {
    Field {
        field: TagField,
        values: &'a [String],
    },
    Spelling {
        field: TagField,
        spelling: &'static str,
        values: &'a [String],
    },
    Native {
        key: &'a str,
        values: &'a [String],
    },
    /// The file's embedded pictures become these, in order; an empty list
    /// removes them all.
    Pictures(&'a [PictureRef]),
}

/// The tag updates of one plan item, and the managed fields of a
/// before-state document. Undo replays a document's fields as the next
/// plan's updates, so the two are one type with one stored form: a JSON
/// object of stored names to value lists, `pictures` holding one record
/// per picture. An empty value list removes the target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagUpdates {
    values: BTreeMap<TagTarget, Vec<String>>,
    pictures: Option<Vec<PictureRef>>,
}

impl TagUpdates {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates naming writable fields only, from a map of field names (the
    /// tags an import or a release document plans). A name that is not a
    /// field is an error, never skipped.
    pub fn from_fields(
        fields: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Result<Self, super::PublishError> {
        let mut updates = Self::new();
        for (name, values) in fields {
            let field = TagField::from_name(&name).ok_or_else(|| {
                super::PublishError::Store(format!("tag field {name} is not a writable field"))
            })?;
            updates.insert(TagTarget::Field(field), values);
        }
        Ok(updates)
    }

    /// Add one entry by its stored name. False, adding nothing, for an
    /// unknown name or a picture record that does not read.
    pub fn insert_named(&mut self, name: &str, values: Vec<String>) -> bool {
        if name == PICTURES_KEY {
            let Some(pictures) = values
                .iter()
                .map(|value| PictureRef::parse(value))
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            self.pictures = Some(pictures);
            return true;
        }
        let Some(target) = TagTarget::parse(name) else {
            return false;
        };
        self.values.insert(target, values);
        true
    }

    /// Add a requested entry that is not a field: the removal of a native
    /// item, or pictures that read. False, adding nothing, for anything
    /// else.
    pub fn insert_requested_extra(&mut self, name: &str, values: &[String]) -> bool {
        let native_removal = name.starts_with(NATIVE_PREFIX) && values.is_empty();
        (native_removal || name == PICTURES_KEY) && self.insert_named(name, values.to_vec())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.pictures.is_none()
    }

    pub fn insert(&mut self, target: TagTarget, values: Vec<String>) -> Option<Vec<String>> {
        self.values.insert(target, values)
    }

    /// Add every update of `other`, its entries and pictures winning over
    /// these.
    pub fn extend(&mut self, other: TagUpdates) {
        self.values.extend(other.values);
        if other.pictures.is_some() {
            self.pictures = other.pictures;
        }
    }

    /// Insert `values` unless the target already has an entry.
    pub fn insert_missing(&mut self, target: TagTarget, values: Vec<String>) {
        self.values.entry(target).or_insert(values);
    }

    pub fn remove(&mut self, target: &TagTarget) -> Option<Vec<String>> {
        self.values.remove(target)
    }

    #[must_use]
    pub fn get(&self, target: &TagTarget) -> Option<&Vec<String>> {
        self.values.get(target)
    }

    /// A field's own entry.
    #[must_use]
    pub fn field(&self, field: TagField) -> Option<&Vec<String>> {
        self.values.get(&TagTarget::Field(field))
    }

    /// A field's values: its own entry, or its first Vorbis spelling's.
    #[must_use]
    pub fn field_or_spelling(&self, field: TagField) -> Option<&Vec<String>> {
        self.field(field).or_else(|| {
            self.values.iter().find_map(|(target, values)| {
                matches!(target, TagTarget::Spelling(of, _) if *of == field).then_some(values)
            })
        })
    }

    /// The targets with entries, in replay order.
    pub fn targets(&self) -> impl Iterator<Item = &TagTarget> {
        self.values.keys()
    }

    /// The pictures update, when there is one.
    #[must_use]
    pub fn pictures(&self) -> Option<&[PictureRef]> {
        self.pictures.as_deref()
    }

    /// Replace the file's embedded pictures with `pictures`.
    pub fn set_pictures(&mut self, pictures: Vec<PictureRef>) {
        self.pictures = Some(pictures);
    }

    /// Every update: the values in replay order, then the pictures.
    pub fn iter(&self) -> impl Iterator<Item = TagUpdate<'_>> {
        self.values
            .iter()
            .map(|(target, values)| match target {
                TagTarget::Field(field) => TagUpdate::Field {
                    field: *field,
                    values,
                },
                TagTarget::Spelling(field, spelling) => TagUpdate::Spelling {
                    field: *field,
                    spelling,
                    values,
                },
                TagTarget::Native(key) => TagUpdate::Native { key, values },
            })
            .chain(self.pictures.as_deref().map(TagUpdate::Pictures))
    }

    /// The updates by stored name, for pages that list them. A picture
    /// that cannot be recorded shows with its tabs as spaces.
    #[must_use]
    pub fn named(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = self
            .values
            .iter()
            .map(|(target, values)| (target.name(), values.clone()))
            .collect();
        if let Some(pictures) = &self.pictures {
            let shown = |picture: &PictureRef| {
                picture.record().unwrap_or_else(|| {
                    PictureRef {
                        mime: picture.mime.replace('\t', " "),
                        description: picture.description.replace('\t', " "),
                        sha256: picture.sha256.replace('\t', " "),
                        ..picture.clone()
                    }
                    .record()
                    .unwrap_or_default()
                })
            };
            out.insert(
                PICTURES_KEY.to_owned(),
                pictures.iter().map(shown).collect(),
            );
        }
        out
    }
}

impl Serialize for TagUpdates {
    /// Refuses a picture that cannot be recorded, so a plan or a
    /// before-state holding one is never stored and nothing it would write
    /// moves.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error as _, SerializeMap as _};
        let pictures = self
            .pictures
            .as_ref()
            .map(|pictures| {
                pictures
                    .iter()
                    .map(|picture| {
                        picture.record().ok_or_else(|| {
                            S::Error::custom(
                                "a picture's MIME type, hash or description holds a tab",
                            )
                        })
                    })
                    .collect::<Result<Vec<String>, S::Error>>()
            })
            .transpose()?;
        let mut entries: Vec<(String, &Vec<String>)> = self
            .values
            .iter()
            .map(|(target, values)| (target.name(), values))
            .collect();
        if let Some(records) = &pictures {
            entries.push((PICTURES_KEY.to_owned(), records));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (name, values) in entries {
            map.serialize_entry(&name, values)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for TagUpdates {
    /// The one reader of the stored form: an unknown name or a picture
    /// record that does not read is an error.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let stored = BTreeMap::<String, Vec<String>>::deserialize(deserializer)?;
        let mut updates = Self::new();
        for (name, values) in stored {
            if !updates.insert_named(&name, values) {
                return Err(D::Error::custom(format!("unknown tag update {name}")));
            }
        }
        Ok(updates)
    }
}

/// The current [`TagDocument`] shape.
pub const TAG_DOCUMENT_VERSION: u32 = 6;

/// The first shape that records every writable field it knew of.
pub const FULL_FIELDS_VERSION: u32 = 2;

/// The first document shape that records `field`. A restore from an older
/// document leaves the field alone: it cannot tell absent from unread.
pub fn recorded_since(field: TagField) -> u32 {
    match field {
        TagField::Isrc => 3,
        TagField::ReplayGainTrackGain
        | TagField::ReplayGainTrackPeak
        | TagField::ReplayGainAlbumGain
        | TagField::ReplayGainAlbumPeak
        | TagField::Lyrics => RICH_FIELDS_VERSION,
        TagField::AlbumArtists
        | TagField::Work
        | TagField::MusicBrainzWorkId
        | TagField::Movement
        | TagField::MovementNumber
        | TagField::MovementCount
        | TagField::Composer
        | TagField::Lyricist
        | TagField::Conductor
        | TagField::Performer
        | TagField::Arranger
        | TagField::Remixer
        | TagField::Producer
        | TagField::Comment
        | TagField::Copyright
        | TagField::EncodedBy
        | TagField::AcoustIdId
        | TagField::AcoustIdFingerprint => CREDIT_FIELDS_VERSION,
        TagField::Engineer
        | TagField::Mixer
        | TagField::DjMixer
        | TagField::Writer
        | TagField::Director => OTHER_ROLES_VERSION,
        _ => FULL_FIELDS_VERSION,
    }
}

/// The first shape that records ReplayGain, lyrics, pictures and native
/// custom items. A restore from an older document leaves pictures and
/// native items alone.
pub const RICH_FIELDS_VERSION: u32 = 4;

/// The first shape that records the credit, work, movement, comment,
/// copyright, encoder and AcoustID fields. Older documents held some of
/// them as native items and some not at all, so a restore from one
/// leaves these fields as the file holds them.
pub const CREDIT_FIELDS_VERSION: u32 = 5;

/// The first shape that records engineer, mixer, DJ-mixer, writer and
/// director. Older documents held their Vorbis, `TXXX` and MP4 tags as
/// native items and their `TIPL` pairs not at all, so a restore from one
/// leaves these fields as the file holds them.
pub const OTHER_ROLES_VERSION: u32 = 6;

impl TagDocument {
    /// Empty document.
    pub fn empty() -> Self {
        Self {
            managed: TagUpdates::new(),
            custom: BTreeMap::new(),
            unknown_frames: BTreeMap::new(),
            opaque: Vec::new(),
            version: TAG_DOCUMENT_VERSION,
            pictures: Some(Vec::new()),
            picture_bytes: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored picture record reads back as the picture it was made from,
    /// and a picture no record can hold is never stored: the plan or
    /// before-state naming it fails to serialize, before any file moves.
    #[test]
    fn picture_records_read_back_as_written_or_are_refused() {
        let picture = PictureRef::new(3, "image/png".to_owned(), "Front\tcover", "ab".into(), None);
        assert_eq!(picture.description, "Front cover");
        let mut updates = TagUpdates::new();
        updates.set_pictures(vec![picture.clone()]);
        let stored = serde_json::to_string(&updates).expect("stored");
        let read: TagUpdates = serde_json::from_str(&stored).expect("read");
        assert_eq!(read.pictures(), Some(&[picture][..]));

        for odd in [
            PictureRef::new(3, "image/\tpng".to_owned(), "", "ab".into(), None),
            PictureRef::new(3, "image/png".to_owned(), "", "a\tb".into(), None),
        ] {
            assert!(!odd.recordable());
            let mut updates = TagUpdates::new();
            updates.set_pictures(vec![odd]);
            assert!(serde_json::to_string(&updates).is_err());
        }
    }
}
