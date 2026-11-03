//! Reads of an album's edition: where it stands (chosen by a person,
//! confirmed, an unconfirmed best guess, or unmatched), why, and the
//! albums whose match still waits for a look.

use rusqlite::{Connection, OptionalExtension as _, params};

use crate::edition::PROVIDER;
use crate::edition::sql::AlbumFactsRow;
use crate::edition::sql::EditionIdentity;
use crate::edition::sql::LooksWrongAlbumRow;
use crate::edition::sql::MatchStateRow;
use crate::edition::sql::WaitingAlbumRow;
use crate::identify::models::CandidateEvidence;
use crate::identity::reads::undo_available;
use crate::matching::score::FINGERPRINT_DISPUTED;
use crate::reasons;
use crate::reasons::Reason;
use crate::reidentify::store;

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
    /// The album's own title, as its files have it.
    pub album_title: String,
    /// The release group every file's tags name, when the guess fits
    /// another (`NAMED_GROUP_DIFFERS`).
    pub named_release_group_mbid: Option<String>,
    /// Titles of the files whose audio sounds like another track of the
    /// guess (`AUDIO_DISAGREES`).
    pub disputed_tracks: Vec<String>,
}

/// The candidate the album's identity names: the guess a person is asked
/// to confirm.
pub fn guess_of<'a>(
    candidates: &'a [CandidateEvidence],
    release_mbid: Option<&str>,
) -> Option<&'a CandidateEvidence> {
    let release = release_mbid?;
    candidates.iter().find(|candidate| {
        candidate
            .release_mbid
            .as_deref()
            .is_some_and(|mbid| mbid.eq_ignore_ascii_case(release))
    })
}

/// Local track ids of the guess's files whose audio names another track.
fn disputed_ids(guess: Option<&CandidateEvidence>) -> Vec<String> {
    guess
        .map(|guess| {
            guess
                .track_evidence
                .iter()
                .filter(|track| {
                    track
                        .evidence_kinds
                        .iter()
                        .any(|kind| kind == FINGERPRINT_DISPUTED)
                })
                .map(|track| track.local_track_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Release group, release, decision source, chooser, chosen at, revision.
type IdentityRow = (String, Option<String>, String, Option<String>, f64, i64);

/// One album's edition, or `None` for an album the library does not hold.
pub fn edition_status(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Option<EditionStatus>> {
    let known: bool = conn.query_row(
        crate::edition::sql::ALBUM_EXISTS.text(),
        params![album_id],
        |row| row.get(0),
    )?;
    if !known {
        return Ok(None);
    }
    let identity: Option<IdentityRow> = conn
        .query_row(
            crate::edition::sql::ALBUM_EDITION_IDENTITY.text(),
            params![album_id, PROVIDER],
            EditionIdentity::read,
        )
        .optional()?
        .map(|row| {
            (
                row.release_group_mbid,
                row.release_mbid,
                row.decision_source,
                row.selected_by_user_id,
                crate::clock::at_secs(row.selected_at),
                row.row_revision,
            )
        });
    let flagged: Option<(String, String, String)> = conn
        .query_row(
            crate::edition::sql::ALBUM_MATCH_STATE.text(),
            params![album_id],
            MatchStateRow::read,
        )
        .optional()?
        .map(|row| (row.state, row.reason_code, row.candidates_json));
    let candidates: Vec<CandidateEvidence> = flagged
        .as_ref()
        .and_then(|(_, _, json)| serde_json::from_str(json).ok())
        .unwrap_or_default();
    let protected = identity.as_ref().is_some_and(|(_, _, source, _, _, _)| {
        crate::identity::DecisionSource::parse(source)
            .is_some_and(crate::identity::DecisionSource::is_persons_choice)
    });
    let custom: bool = conn.query_row(
        crate::edition::sql::CUSTOM_ACTIVE.text(),
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
    let facts = conn
        .query_row(
            crate::edition::sql::ALBUM_FACTS.text(),
            params![album_id],
            AlbumFactsRow::read,
        )
        .optional()?;
    let named_release_group_mbid = if reason.code == crate::identify::service::NAMED_GROUP_DIFFERS {
        conn.query_row(
            crate::edition::sql::NAMED_GROUP.text(),
            params![album_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
    } else {
        None
    };
    let mut disputed_tracks = Vec::new();
    if reason.code == crate::identify::service::AUDIO_DISAGREES {
        for id in disputed_ids(guess_of(&candidates, release.as_deref())) {
            let title: Option<String> = conn
                .query_row(
                    crate::edition::sql::TRACK_TITLE.text(),
                    params![id, album_id],
                    |row| row.get(0),
                )
                .optional()?;
            disputed_tracks.extend(title);
        }
    }
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
        album_title: facts.map(|facts| facts.title).unwrap_or_default(),
        named_release_group_mbid,
        disputed_tracks,
    }))
}

/// Tries so far while the album's chosen edition is still being placed
/// (its `library_edition_remap_queue` row); `None` once it is placed.
pub(crate) fn placing_attempts(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<u32>> {
    conn.query_row(
        crate::edition::sql::PLACING_ATTEMPTS.text(),
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
        crate::edition::sql::IDENTIFYING.text(),
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
    /// The identity's row revision, 0 when the album has none.
    pub identity_revision: i64,
    /// The guess the identity names, for a best guess.
    pub guess: Option<CandidateEvidence>,
    /// Indexed files the album holds.
    pub file_count: u32,
    /// People who marked the album's match "Looks wrong".
    pub looks_wrong: u32,
}

/// Which albums the waiting list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    Unconfirmed,
    Unmatched,
    LooksWrong,
}

/// Albums whose match is unconfirmed, that match nothing, or that someone
/// marked "Looks wrong", newest first, with the total.
pub fn waiting_albums(
    conn: &Connection,
    which: Waiting,
    limit: u32,
    offset: u32,
) -> rusqlite::Result<(Vec<WaitingAlbum>, u64)> {
    let state = match which {
        Waiting::Unconfirmed => "unconfirmed",
        Waiting::Unmatched => "unmatched",
        Waiting::LooksWrong => return looks_wrong_albums(conn, limit, offset),
    };
    let unmatched = which == Waiting::Unmatched;
    let total: i64 = conn.query_row(
        crate::edition::sql::WAITING_ALBUM_COUNT.text(),
        params![state],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(crate::edition::sql::WAITING_ALBUM_PAGE.text())?;
    let rows = stmt
        .query_map(
            params![state, PROVIDER, limit, offset],
            WaitingAlbumRow::read,
        )?
        .map(|row| {
            row.map(|row| {
                let candidates: Vec<CandidateEvidence> =
                    match serde_json::from_str(&row.candidates_json) {
                        Ok(candidates) => candidates,
                        Err(error) => {
                            tracing::warn!(
                                album_id = %row.local_album_id,
                                %error,
                                "an album's stored candidates do not read; its row names no guess"
                            );
                            Vec::new()
                        }
                    };
                let guess = if unmatched {
                    None
                } else {
                    guess_of(&candidates, row.release_mbid.as_deref()).cloned()
                };
                WaitingAlbum {
                    local_album_id: row.local_album_id,
                    title: row.title,
                    artist_name: row.artist_name,
                    state: if unmatched {
                        EditionState::Unmatched
                    } else {
                        EditionState::Unconfirmed
                    },
                    reason: reasons::match_reason(&row.reason_code),
                    release_mbid: row.release_mbid,
                    release_group_mbid: row.release_group_mbid,
                    updated_at: crate::clock::at_secs(row.updated_at),
                    identity_revision: row.identity_revision,
                    guess,
                    file_count: u32::try_from(row.file_count).unwrap_or(0),
                    looks_wrong: u32::try_from(row.looks_wrong).unwrap_or(0),
                }
            })
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, u64::try_from(total).unwrap_or(0)))
}

/// Albums someone marked "Looks wrong", newest flag first, each with its
/// edition as it stands.
fn looks_wrong_albums(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> rusqlite::Result<(Vec<WaitingAlbum>, u64)> {
    let total: i64 = conn.query_row(
        crate::edition::sql::LOOKS_WRONG_ALBUM_COUNT.text(),
        [],
        |row| row.get(0),
    )?;
    let flagged = conn
        .prepare(crate::edition::sql::LOOKS_WRONG_PAGE.text())?
        .query_map(params![limit, offset], LooksWrongAlbumRow::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut albums = Vec::with_capacity(flagged.len());
    for row in flagged {
        let Some(status) = edition_status(conn, &row.local_album_id)? else {
            continue;
        };
        let guess = guess_of(&status.candidates, status.release_mbid.as_deref()).cloned();
        let artist_name: String = conn.query_row(
            crate::edition::sql::ALBUM_ARTIST_NAME.text(),
            params![row.local_album_id],
            |row| row.get(0),
        )?;
        let file_count = conn
            .query_row(
                crate::edition::sql::ALBUM_FACTS.text(),
                params![row.local_album_id],
                AlbumFactsRow::read,
            )
            .map(|facts| u32::try_from(facts.file_count).unwrap_or(0))?;
        albums.push(WaitingAlbum {
            local_album_id: row.local_album_id,
            title: status.album_title,
            artist_name,
            state: status.state,
            release_mbid: status.release_mbid,
            release_group_mbid: status.release_group_mbid,
            reason: status.reason,
            updated_at: crate::clock::at_secs(row.flagged_at),
            identity_revision: status.identity_revision,
            guess,
            file_count,
            looks_wrong: u32::try_from(row.looks_wrong).unwrap_or(0),
        });
    }
    Ok((albums, u64::try_from(total).unwrap_or(0)))
}

/// Whether `user_id` marked the album "Looks wrong", and how many did.
pub fn looks_wrong_of(
    conn: &Connection,
    album_id: &str,
    user_id: &str,
) -> rusqlite::Result<(bool, u32)> {
    conn.query_row(
        crate::edition::sql::LOOKS_WRONG_OF.text(),
        params![album_id, user_id],
        |row| {
            Ok((
                row.get(0)?,
                u32::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
            ))
        },
    )
}
