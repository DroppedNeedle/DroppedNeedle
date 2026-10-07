//! Types for catalog corrections: the requests, what a preview shows, what
//! an apply returns, and the typed refusal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::library::operations::reasons::Reason;

/// Which album membership change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipKind {
    /// Selected tracks of one album become a new album (or join another).
    Split,
    /// Whole albums fold into one album.
    Merge,
    /// Selected tracks join another album.
    Move,
    /// Selected tracks go back to where an automatic scan files them.
    Reset,
}

impl MembershipKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Merge => "merge",
            Self::Move => "move",
            Self::Reset => "reset",
        }
    }
}

/// What the receiving album keeps when the albums being combined name
/// different editions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityChoice {
    /// Drop every competing edition; the album is identified again.
    #[default]
    Detach,
    /// The receiving album keeps its own edition.
    RetainManual,
}

/// What the surviving artist keeps when the merged artists name different
/// MusicBrainz artists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderChoice {
    /// The survivor loses its MusicBrainz link.
    Detach,
    /// The survivor keeps its link (or takes the only one on offer).
    #[default]
    RetainSurvivor,
}

/// One album membership change, as previewed and as applied.
#[derive(Debug, Clone)]
pub struct MembershipRequest {
    pub kind: MembershipKind,
    /// The album the route names (split and reset).
    pub album_id: Option<String>,
    pub track_ids: Vec<String>,
    /// Revisions the page showed; zero or missing means "not known", and
    /// the preview token then guards the change on its own.
    pub expected_album_revisions: BTreeMap<String, i64>,
    pub target_album_id: Option<String>,
    /// Split only: the new album's title and album artist.
    pub title: Option<String>,
    pub album_artist_name: Option<String>,
    pub identity_choice: IdentityChoice,
}

/// One artist merge, as previewed and as applied.
#[derive(Debug, Clone)]
pub struct ArtistMergeRequest {
    pub source_artist_ids: Vec<String>,
    pub surviving_artist_id: String,
    pub expected_revisions: BTreeMap<String, i64>,
    pub provider_choice: ProviderChoice,
}

/// Where a set of tracks ends up.
#[derive(Debug, Clone, Serialize)]
pub struct AlbumGroup {
    /// The receiving album; a new album's id is only known once applied.
    pub album_id: String,
    pub title: String,
    pub album_artist_name: String,
    pub track_ids: Vec<String>,
    /// The album is made by this change.
    pub created: bool,
    pub reason_code: &'static str,
}

/// What happens to one album's edition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditionChangeKind {
    Kept,
    Moved,
    Cleared,
    RemapQueued,
}

impl EditionChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Moved => "moved",
            Self::Cleared => "cleared",
            Self::RemapQueued => "remap_queued",
        }
    }
}

/// One album's edition outcome, with the plain reason.
#[derive(Debug, Clone)]
pub struct EditionChange {
    pub album_id: String,
    pub album_title: String,
    pub change: EditionChangeKind,
    /// For a moved edition, the album it came from.
    pub from_album_id: Option<String>,
    pub release_mbid: Option<String>,
    pub reason: Reason,
    /// Editions this change dropped, so they can be put back.
    pub dropped: Vec<DroppedEdition>,
}

/// An edition a change dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DroppedEdition {
    /// The album that held it.
    pub album_id: String,
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub decision_source: String,
}

/// What a membership change does.
#[derive(Debug, Clone, Default)]
pub struct MembershipOutcome {
    pub track_ids: Vec<String>,
    pub source_album_ids: Vec<String>,
    pub target_album_id: Option<String>,
    pub groups: Vec<AlbumGroup>,
    /// Albums emptied by the change, each with the album it now points to.
    pub retired: Vec<(String, Option<String>)>,
    /// Competing editions (release or release group MBIDs).
    pub identity_conflicts: Vec<String>,
    pub edition_changes: Vec<EditionChange>,
}

/// What an artist merge does.
#[derive(Debug, Clone, Default)]
pub struct ArtistMergeOutcome {
    pub surviving_artist_id: String,
    pub retired_artist_ids: Vec<String>,
    /// Competing MusicBrainz artist ids.
    pub identity_conflicts: Vec<String>,
    /// References that will point at the survivor, by kind.
    pub reference_counts: BTreeMap<String, i64>,
}

/// A preview and the token that applies exactly it.
#[derive(Debug, Clone)]
pub struct Previewed<T> {
    pub token: String,
    pub outcome: T,
}

/// How an apply is identified and who asks.
#[derive(Debug, Clone)]
pub struct ApplyMeta {
    pub preview_token: String,
    /// Replaying a key returns the first result instead of applying again.
    pub idempotency_key: Option<String>,
    pub actor: String,
}

/// An applied correction, as recorded in the audit row and returned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Applied {
    pub kind: String,
    #[serde(default)]
    pub track_ids: Vec<String>,
    #[serde(default)]
    pub source_album_ids: Vec<String>,
    #[serde(default)]
    pub target_album_id: Option<String>,
    #[serde(default)]
    pub surviving_artist_id: Option<String>,
    #[serde(default)]
    pub retired_artist_ids: Vec<String>,
    #[serde(default)]
    pub catalog_revision: i64,
}

/// Why a correction did not happen. Handlers map it to a status.
#[derive(Debug)]
pub enum CorrectionError {
    NotFound(Reason),
    Invalid(Reason),
    /// The catalog moved since the page or the preview.
    Conflict(Reason),
    /// A store fault; the cause is for the log.
    Store(String),
}

impl From<rusqlite::Error> for CorrectionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.to_string())
    }
}
