//! What choosing or undoing an edition did, and an edition-finder page.

use crate::identity::AutomaticUndone;

/// The result of undoing an automatic edition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoOutcome {
    pub local_album_id: String,
    pub outcome: AutomaticUndone,
    pub review_id: Option<String>,
}

/// One file an edition choice placed on the release: what a retag would
/// rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedFile {
    pub local_track_id: String,
    pub root_id: String,
    pub relative_path: String,
}

/// What choosing an edition did to one album.
#[derive(Debug, Clone, PartialEq)]
pub struct EditionChoice {
    pub local_album_id: String,
    pub release_group_mbid: String,
    pub release_mbid: String,
    /// Files placed on a track of the edition.
    pub placed: Vec<PlacedFile>,
    /// Files the edition has no track for. They keep their recording.
    pub extra_track_ids: Vec<String>,
    /// Titles of the edition's tracks no file holds.
    pub missing_titles: Vec<String>,
}

/// One edition-finder page with the album's current identity marked.
#[derive(Debug, Clone)]
pub struct ReleaseSearch {
    pub title_query: String,
    pub artist_query: String,
    /// Set when the page lists one release group's releases.
    pub release_group_query: Option<String>,
    pub current_release_group_mbid: Option<String>,
    pub current_release_mbid: Option<String>,
    pub page: crate::identify::sources::EditionPage,
    pub limit: u32,
}
