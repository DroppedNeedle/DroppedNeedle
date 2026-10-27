//! Types for catalog corrections: the requests, what a preview shows, what
//! an apply returns, and the typed refusal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::operations::reasons::Reason;

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = IdentityChoiceBody)]
pub enum IdentityChoice {
    /// Drop every competing edition; the album is identified again.
    #[default]
    Detach,
    /// The receiving album keeps its own edition.
    RetainManual,
}

/// What the surviving artist keeps when the merged artists name different
/// MusicBrainz artists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ProviderChoiceBody)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
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

/// Which correction was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionKind {
    Split,
    Merge,
    Move,
    Reset,
    MergeArtist,
}

impl From<MembershipKind> for CorrectionKind {
    fn from(kind: MembershipKind) -> Self {
        match kind {
            MembershipKind::Split => Self::Split,
            MembershipKind::Merge => Self::Merge,
            MembershipKind::Move => Self::Move,
            MembershipKind::Reset => Self::Reset,
        }
    }
}

/// An applied correction, as recorded in the audit row and returned.
// A recorded `kind` outside `CorrectionKind` does not read: its replay
// fails as an unreadable row.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = CatalogCorrectionResponse)]
pub struct Applied {
    pub kind: CorrectionKind,
    #[serde(default)]
    #[schema(required = true)]
    pub track_ids: Vec<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub source_album_ids: Vec<String>,
    #[serde(default)]
    pub target_album_id: Option<String>,
    #[serde(default)]
    pub surviving_artist_id: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub retired_artist_ids: Vec<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub catalog_revision: i64,
}

impl Applied {
    /// A correction of `kind` with nothing recorded yet.
    pub fn new(kind: CorrectionKind) -> Self {
        Self {
            kind,
            track_ids: Vec::new(),
            source_album_ids: Vec::new(),
            target_album_id: None,
            surviving_artist_id: None,
            retired_artist_ids: Vec::new(),
            catalog_revision: 0,
        }
    }
}

/// One album of a retag pair, as the card shows it.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = RetagPairAlbumView)]
pub struct PairSide {
    pub id: String,
    pub title: String,
    pub album_artist_name: String,
    /// Indexed tracks.
    pub track_count: i64,
    pub edition: Option<PairEdition>,
    /// The edition's revision, 0 for none. Send it back with a join.
    pub identity_revision: i64,
}

/// An album's edition on a retag pair.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = RetagPairEditionView)]
pub struct PairEdition {
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    /// Chosen by a person or carried over from v2.
    pub chosen: bool,
    /// A custom edition is active. It does not carry over a join.
    pub custom: bool,
}

/// Why the scan left a retag pair for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = RetagPairReasonView)]
pub enum PairReason {
    /// Each album holds a different edition a person chose.
    ChosenMatchesDiffer,
    /// The album was split against a rename: the old album's edition
    /// goes to the named album only.
    TracksElsewhere,
    /// The old album holds a custom edition, which does not carry over.
    CustomEdition,
    /// The old album was left unmanaged, kept as a custom edition or
    /// stepped back from, and lost its files in the scan in which the new
    /// album's files appeared beside it. Nothing shows they are one
    /// album, so the new album is left alone until "Manage it again".
    ReplacedHandsOff,
}

/// Two album rows that look like one retagged album, which the scan would
/// not join by itself.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = RetagPairView)]
pub struct RetagPair {
    /// The row the files left. It holds the favorites, plays and links.
    pub old_album: PairSide,
    /// The row the files sit on now, or the album it was merged into
    /// since. A join keeps this one.
    pub new_album: PairSide,
    pub reason: PairReason,
    /// Two different chosen editions remain once a custom edition on the
    /// old album is left out: the join must name the one to keep.
    pub editions_differ: bool,
    /// When the scan recorded the pair (Unix milliseconds).
    pub recorded_at: i64,
}

/// The open retag pairs, newest first (at most 100), and how many there
/// are.
#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
#[schema(as = RetagPairListResponse)]
pub struct RetagPairList {
    pub pairs: Vec<RetagPair>,
    pub total: i64,
}

/// A person joining a retag pair.
#[derive(Debug, Clone)]
pub struct PairJoinRequest {
    pub old_album_id: String,
    pub new_album_id: String,
    /// Which album's chosen edition stays, when the two differ.
    pub keep_edition_of: Option<String>,
    /// The edition revisions the card showed, by album id (0 for none).
    pub expected_identity_revisions: BTreeMap<String, i64>,
    pub actor: String,
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
    /// The database was too busy to answer, or is shutting down; the
    /// cause is for the log.
    Busy(String),
}

impl std::fmt::Display for CorrectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(reason) | Self::Invalid(reason) | Self::Conflict(reason) => {
                write!(f, "{}: {}", reason.code, reason.message)
            }
            Self::Store(cause) | Self::Busy(cause) => write!(f, "{cause}"),
        }
    }
}

impl From<droppedneedle_kernel::db::DbError> for CorrectionError {
    fn from(error: droppedneedle_kernel::db::DbError) -> Self {
        use droppedneedle_kernel::db::DbError;
        match error {
            DbError::Busy { .. } | DbError::LaneClosed => Self::Busy(error.to_string()),
            other => Self::Store(other.to_string()),
        }
    }
}

impl From<rusqlite::Error> for CorrectionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.to_string())
    }
}
