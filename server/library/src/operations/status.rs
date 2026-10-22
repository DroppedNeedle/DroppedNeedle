//! Reads of an album's edition: where it stands (chosen by a person,
//! confirmed, an unconfirmed best guess, or unmatched), why, and the
//! albums whose match still waits for a look.

use rusqlite::{Connection, OptionalExtension as _, params};

use super::reasons::{self, Reason};
use super::store::{self, PROVIDER};
use crate::identify::models::CandidateEvidence;
use crate::identity::reads::undo_available;

/// Where an album's edition stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditionState {
    /// A person chose it; nothing automatic changes it.
    Chosen,
    /// The matcher is sure of it.
    Confirmed,
    /// The matcher's best guess, waiting for a look.
    Unconfirmed,
    /// Nothing on MusicBrainz fits; the album keeps its own tags.
    Unmatched,
    /// Not identified yet.
    Unidentified,
}

/// What kind of edition the album has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditionKind {
    /// A MusicBrainz release.
    Release,
    /// A custom edition built from the album's own files.
    Custom,
    /// A release group with no release chosen within it.
    GroupOnly,
    /// No edition at all.
    None,
}

/// One album's edition, with the reason behind it.
#[derive(Debug, Clone)]
pub struct EditionStatus {
    pub local_album_id: String,
    pub state: EditionState,
    pub release_group_mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub chosen_by_user_id: Option<String>,
    pub chosen_at: Option<f64>,
    /// Why the edition is what it is, in words.
    pub reason: Reason,
    /// The closest candidates the matcher scored, best first.
    pub candidates: Vec<CandidateEvidence>,
    pub undo_available: bool,
    /// The identity's row revision, 0 when there is none.
    pub identity_revision: i64,
    pub edition_kind: EditionKind,
    /// Tries so far, while the chosen edition's files are still being
    /// placed on it.
    pub placing_attempts: Option<u32>,
    /// An identification of the album is queued or running.
    pub identifying: bool,
}

/// Release group, release, decision source, chooser, chosen at, revision.
type IdentityRow = (String, Option<String>, String, Option<String>, f64, i64);

/// One album's edition, or `None` for an album the library does not hold.
pub fn edition_status(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Option<EditionStatus>> {
    let known: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1)",
        params![album_id],
        |row| row.get(0),
    )?;
    if !known {
        return Ok(None);
    }
    let identity: Option<IdentityRow> = conn
        .query_row(
            "SELECT release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
             selected_at, row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2",
            params![album_id, PROVIDER],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let flagged: Option<(String, String, String)> = conn
        .query_row(
            "SELECT state, reason_code, candidates_json FROM library_album_match_state \
             WHERE local_album_id = ?1",
            params![album_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let candidates: Vec<CandidateEvidence> = flagged
        .as_ref()
        .and_then(|(_, _, json)| serde_json::from_str(json).ok())
        .unwrap_or_default();
    let protected = identity.as_ref().is_some_and(|(_, _, source, _, _, _)| {
        crate::identity::DecisionSource::parse(source)
            .is_some_and(crate::identity::DecisionSource::is_persons_choice)
    });
    let custom: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1)",
        params![album_id],
        |row| row.get(0),
    )?;
    let (state, reason) = match (&identity, &flagged) {
        // A chosen edition stays chosen; a flag on it explains a problem
        // with the choice (it fits none of the files).
        (_, Some((_, code, _))) if protected => (EditionState::Chosen, reasons::match_reason(code)),
        _ if protected => (EditionState::Chosen, reasons::match_reason("CHOSEN")),
        (_, Some((state, code, _))) if state == "unmatched" => {
            (EditionState::Unmatched, reasons::match_reason(code))
        }
        (Some(_), Some((_, code, _))) => (EditionState::Unconfirmed, reasons::match_reason(code)),
        // The album is matched to a release group but no release in it yet.
        (Some((_, None, _, _, _, _)), None) if !custom => (
            EditionState::Confirmed,
            reasons::match_reason("EDITION_NOT_CHOSEN"),
        ),
        (Some(_), None) => (EditionState::Confirmed, reasons::match_reason("SUPPORTED")),
        (None, _) if keeps_own_tags(conn, album_id)? => (
            EditionState::Unidentified,
            reasons::match_reason("FOLDER_KEEPS_TAGS"),
        ),
        (None, _) => (
            EditionState::Unidentified,
            reasons::match_reason("UNIDENTIFIED"),
        ),
    };
    let (group, release, chosen_by, chosen_at, identity_revision) = match identity {
        Some((group, release, _, user, at, revision)) => (
            Some(group),
            release,
            protected.then_some(user).flatten(),
            protected.then_some(at),
            revision,
        ),
        None => (None, None, None, None, 0),
    };
    // A custom edition counts only where no release is chosen, as
    // automatic management reads it.
    let edition_kind = match (&group, &release) {
        (_, Some(_)) => EditionKind::Release,
        _ if custom => EditionKind::Custom,
        (Some(_), None) => EditionKind::GroupOnly,
        (None, None) => EditionKind::None,
    };
    let placing_attempts = placing_attempts(conn, album_id)?;
    let identifying = identifying(conn, album_id)?;
    Ok(Some(EditionStatus {
        local_album_id: album_id.to_owned(),
        state,
        release_group_mbid: group,
        release_mbid: release,
        chosen_by_user_id: chosen_by,
        chosen_at,
        reason,
        candidates,
        undo_available: undo_available(conn, album_id)?,
        identity_revision,
        edition_kind,
        placing_attempts,
        identifying,
    }))
}

/// Tries so far while the album's chosen edition is still being placed
/// (its `library_edition_remap_queue` row); `None` once it is placed.
pub(crate) fn placing_attempts(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<u32>> {
    conn.query_row(
        "SELECT attempts FROM library_edition_remap_queue WHERE local_album_id = ?1",
        params![album_id],
        |row| row.get(0),
    )
    .optional()
}

/// Whether an identification of the album is open. Open jobs are
/// `queued`, `running` and `deferred` (waiting out a backoff); `attention`
/// waits on a person, and the rest have ended.
pub(crate) fn identifying(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_identify_jobs WHERE local_album_id = ?1 \
         AND state IN ('queued', 'running', 'deferred'))",
        params![album_id],
        |row| row.get(0),
    )
}

/// Whether every indexed file of the album sits in a folder that keeps its
/// own tags, so identification never runs for it by itself.
fn keeps_own_tags(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    Ok(
        store::album_revisions(conn, album_id)?.is_some_and(|album| {
            album
                .policies
                .iter()
                .all(|policy| policy == "local_metadata")
        }),
    )
}

/// One album whose match waits for a look.
#[derive(Debug, Clone)]
pub struct WaitingAlbum {
    pub local_album_id: String,
    pub title: String,
    pub artist_name: String,
    pub state: EditionState,
    pub release_mbid: Option<String>,
    pub release_group_mbid: Option<String>,
    pub reason: Reason,
    pub updated_at: f64,
}

/// Albums whose match is unconfirmed (or, with `unmatched`, matches
/// nothing), newest first, with the total.
pub fn waiting_albums(
    conn: &Connection,
    unmatched: bool,
    limit: u32,
    offset: u32,
) -> rusqlite::Result<(Vec<WaitingAlbum>, u64)> {
    let state = if unmatched {
        "unmatched"
    } else {
        "unconfirmed"
    };
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM library_album_match_state s \
         JOIN local_albums a ON a.id = s.local_album_id \
         WHERE s.state = ?1 AND a.retired_into_album_id IS NULL",
        params![state],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT s.local_album_id, a.title, COALESCE(a.album_artist_name, ''), s.reason_code, \
         e.release_mbid, e.release_group_mbid, s.updated_at \
         FROM library_album_match_state s \
         JOIN local_albums a ON a.id = s.local_album_id \
         LEFT JOIN local_album_external_identities e \
           ON e.local_album_id = s.local_album_id AND e.provider = ?2 \
         WHERE s.state = ?1 AND a.retired_into_album_id IS NULL \
         ORDER BY s.updated_at DESC, s.local_album_id LIMIT ?3 OFFSET ?4",
    )?;
    let rows = stmt
        .query_map(params![state, PROVIDER, limit, offset], |row| {
            let code: String = row.get(3)?;
            Ok(WaitingAlbum {
                local_album_id: row.get(0)?,
                title: row.get(1)?,
                artist_name: row.get(2)?,
                state: if unmatched {
                    EditionState::Unmatched
                } else {
                    EditionState::Unconfirmed
                },
                reason: reasons::match_reason(&code),
                release_mbid: row.get(4)?,
                release_group_mbid: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, u64::try_from(total).unwrap_or(0)))
}
