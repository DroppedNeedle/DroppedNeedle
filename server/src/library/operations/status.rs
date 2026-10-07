//! Reads of an album's edition: where it stands (chosen by a person,
//! confirmed, an unconfirmed best guess, or unmatched), why, and the
//! albums whose match still waits for a look.

use rusqlite::{Connection, OptionalExtension as _, params};

use super::choice::undo_available;
use super::reasons::{self, Reason};
use super::store::PROVIDER;
use crate::library::identify::models::CandidateEvidence;

/// Where an album's edition stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl EditionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chosen => "chosen",
            Self::Confirmed => "confirmed",
            Self::Unconfirmed => "unconfirmed",
            Self::Unmatched => "unmatched",
            Self::Unidentified => "unidentified",
        }
    }
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
}

/// Release group, release, decision source, chooser, chosen at.
type IdentityRow = (String, Option<String>, String, Option<String>, f64);

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
             selected_at FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2",
            params![album_id, PROVIDER],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
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
    let protected = identity
        .as_ref()
        .is_some_and(|(_, _, source, _, _)| source == "manual" || source == "legacy_import");
    let (state, reason) = match (&identity, &flagged) {
        _ if protected => (EditionState::Chosen, reasons::match_reason("CHOSEN")),
        (_, Some((state, code, _))) if state == "unmatched" => {
            (EditionState::Unmatched, reasons::match_reason(code))
        }
        (Some(_), Some((_, code, _))) => (EditionState::Unconfirmed, reasons::match_reason(code)),
        (Some(_), None) => (EditionState::Confirmed, reasons::match_reason("SUPPORTED")),
        (None, _) => (
            EditionState::Unidentified,
            reasons::match_reason("UNIDENTIFIED"),
        ),
    };
    let (group, release, chosen_by, chosen_at) = match identity {
        Some((group, release, _, user, at)) => (
            Some(group),
            release,
            protected.then_some(user).flatten(),
            protected.then_some(at),
        ),
        None => (None, None, None, None),
    };
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
    }))
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
