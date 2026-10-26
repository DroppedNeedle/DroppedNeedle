//! Artists linked to MusicBrainz when their album is matched.
//!
//! While an album's match is accepted and the release's document is on
//! file, each credit of the album and of its mapped tracks is compared,
//! position by position, with the release's. A side whose credits line up
//! with the release's one for one proves, at each position, that the row
//! credited there is the MusicBrainz artist the release credits there,
//! when the names agree (as credited, or one of the artist's MusicBrainz
//! names when those were fetched). A name never picks a row: only the row
//! already at that position can be proven. Placeholder artists, Various
//! Artists rows and the sentinels prove nothing. A track whose file
//! credits only the lead of a release credit with guests is proven by the
//! album's proof of its one artist (`stand_in`).
//!
//! Proofs are stored per album and count only while the album, its match
//! and the credit they prove stay as they were ([`CURRENT_PROOF`]). A row
//! is then linked to `x` when its current proofs and its files' ids name
//! `x` and nothing else, and no other row holds `x`. These links are the
//! app's own (`automatic`): they follow the match and the files and give
//! way to a person. A person's or v2's link is never touched, except to
//! follow an artist MusicBrainz merged into another ([`merged_ids`]). A
//! release id MusicBrainz merged counts as the id it was merged into, and
//! the ids that disagree on a row are recorded to be asked about.
//!
//! An album whose release document is not on file yet (an upgrade carries
//! matches but no documents) changes nothing: its proofs stay, and the
//! app's link of a row nothing proves stays while such an album credits
//! it. Identification fetches the missing documents when it is idle, and
//! a document that lands sends its albums through the next sweep.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use sha2::{Digest as _, Sha256};

use super::merged_ids;
use super::sqlite::stored_release;
use crate::catalog_revision::bump_catalog;
use crate::corrections::continuation::accepted_release;
use crate::identity::DecisionSource;
use crate::identity::reads::release_input_hash;
use crate::identity::sql::{PERSONS, REVISABLE};
use crate::matching::model::{ArtistNames, CreditedArtist, Release, VARIOUS_ARTISTS_MBID};
use crate::scan::sqlite_store::artists::SENTINEL_ARTIST_IDS;
use droppedneedle_kernel::db::fold_text;

/// How many albums, and how many linked rows, one sweep looks at.
pub const SWEEP_BATCH: usize = 50;

/// MusicBrainz's special-purpose artists, which stand for no one artist.
/// Each was looked up on the MusicBrainz web service
/// (`/ws/2/artist/<id>`) on 2026-10-16: Various Artists, `[unknown]`,
/// `[anonymous]`, `[traditional]`, `[no artist]`, `[data]` and
/// `[dialogue]`, all of type "Other" and described as special purpose
/// artists. `41ece0f7-91f6-4c87-982c-3a39c5a02586`, which the catalog
/// hides, answered as "/v/", a real group, so it is not listed here.
pub(crate) const PLACEHOLDER_ARTISTS: [&str; 7] = [
    VARIOUS_ARTISTS_MBID,
    "125ec42a-7229-4250-afc5-e057484327fe",
    "f731ccc4-e22a-43af-a747-64213329e088",
    "9be7f096-97ec-4615-8957-8d40b5dcbc41",
    "eec63d3c-3b81-4ad4-b1e4-7c147d4d2b61",
    "33cf029c-63b0-41a0-9855-be2a3665fb3b",
    "314e1c25-dde7-4e4d-b2f4-0a7b9f7c56dc",
];

/// Where proving an album's artists stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreditState {
    /// The album's match is not an accepted one, or its release's
    /// document is not on file.
    WaitingForIdentity,
    /// The album's credits do not line up with the release's one for one.
    AmbiguousCreditStructure,
    /// The album's credits line up with the release's.
    Projected,
}

impl CreditState {
    pub const ALL: [Self; 3] = [
        Self::WaitingForIdentity,
        Self::AmbiguousCreditStructure,
        Self::Projected,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingForIdentity => "waiting_for_identity",
            Self::AmbiguousCreditStructure => "ambiguous_credit_structure",
            Self::Projected => "projected",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == raw)
    }
}

/// SQL that is true while the proof row `p` still counts: its album is
/// live and its match accepted and unchanged, the credit it proves still
/// points at its row, and for a track, the track is indexed on the album
/// and still mapped to the same release track.
pub const CURRENT_PROOF: &str = "EXISTS (SELECT 1 FROM local_albums proof_album \
     JOIN local_album_external_identities proof_identity \
       ON proof_identity.local_album_id = proof_album.id \
      AND proof_identity.provider = 'musicbrainz' \
     WHERE proof_album.id = p.local_album_id AND proof_album.retired_into_album_id IS NULL \
     AND proof_identity.row_revision = p.album_identity_revision \
     AND lower(trim(COALESCE(proof_identity.release_mbid, ''))) = lower(trim(p.release_mbid)) \
     AND NOT EXISTS (SELECT 1 FROM library_album_match_state s \
       WHERE s.local_album_id = proof_album.id) \
     AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
       WHERE c.local_album_id = proof_album.id)) \
     AND CASE p.subject_kind \
     WHEN 'album' THEN EXISTS (SELECT 1 FROM local_album_artists c \
       WHERE c.local_album_id = p.local_album_id AND c.position = p.credit_position \
       AND c.local_artist_id = p.source_local_artist_id) \
     ELSE EXISTS (SELECT 1 FROM local_tracks t \
       JOIN local_track_external_identities ti \
         ON ti.local_track_id = t.id AND ti.provider = 'musicbrainz' \
       JOIN local_track_artists c ON c.local_track_id = t.id AND c.position = p.credit_position \
       WHERE t.id = p.local_track_id AND t.local_album_id = p.local_album_id \
       AND t.availability = 'indexed' AND c.local_artist_id = p.source_local_artist_id \
       AND ti.row_revision = p.track_identity_revision \
       AND lower(trim(COALESCE(ti.release_mbid, ''))) = lower(trim(p.release_mbid)) \
       AND lower(trim(COALESCE(ti.release_track_mbid, ''))) = lower(trim(p.release_track_mbid))) \
     END";

/// SQL for the stamp of album `a`: the version of the rules below, its
/// match (release, who made it, its revision and when), whether the match
/// is accepted, and the count, revisions and latest time of its tracks'
/// mappings. Any change to them moves it. A change to the credits, which
/// it leaves out, clears it instead ([`mark_stale`]).
///
/// Any change to what a pass proves bumps the leading version, so every
/// album is worked through again once.
const STAMP: &str = "'rules-2|' || COALESCE((SELECT lower(trim(COALESCE(e.release_mbid, ''))) || '|' \
     || e.decision_source || '|' || e.row_revision || '|' || e.selected_at || '|' \
     || (EXISTS (SELECT 1 FROM library_album_match_state s WHERE s.local_album_id = a.id) \
      OR EXISTS (SELECT 1 FROM library_custom_edition_active c WHERE c.local_album_id = a.id)) \
     FROM local_album_external_identities e \
     WHERE e.local_album_id = a.id AND e.provider = 'musicbrainz'), '-') \
     || '|' || (SELECT count(*) || '|' || COALESCE(sum(i.row_revision), 0) || '|' \
     || COALESCE(max(i.selected_at), 0) FROM local_tracks t \
     JOIN local_track_external_identities i ON i.local_track_id = t.id \
      AND i.provider = 'musicbrainz' WHERE t.local_album_id = a.id)";

/// The albums whose stamp moved since their last pass (or that have none),
/// the albums with a state but no match any more, and the albums waiting
/// for a release document that is now on file (`DOCUMENT_HASH`, the
/// current document version's hash), at most `limit`.
const STALE_ALBUMS: &str = "SELECT a.id FROM local_album_external_identities e \
     JOIN local_albums a ON a.id = e.local_album_id \
     LEFT JOIN library_artist_reconciliation_state s ON s.local_album_id = a.id \
     WHERE e.provider = 'musicbrainz' AND a.retired_into_album_id IS NULL \
     AND s.input_revision IS NOT (STAMP) \
     UNION ALL SELECT s.local_album_id FROM library_artist_reconciliation_state s \
     WHERE NOT EXISTS (SELECT 1 FROM local_albums a \
      JOIN local_album_external_identities e ON e.local_album_id = a.id \
       AND e.provider = 'musicbrainz' \
      WHERE a.id = s.local_album_id AND a.retired_into_album_id IS NULL) \
     UNION ALL SELECT s.local_album_id FROM library_artist_reconciliation_state s \
     JOIN local_album_external_identities e ON e.local_album_id = s.local_album_id \
      AND e.provider = 'musicbrainz' \
     WHERE s.state = 'waiting_for_identity' AND s.reason_code = 'RELEASE_DOCUMENT_MISSING' \
     AND EXISTS (SELECT 1 FROM library_management_metadata_snapshots m \
      WHERE m.provider = 'musicbrainz' AND m.entity_kind = 'release' \
      AND m.entity_id = lower(trim(e.release_mbid)) AND m.input_hash = 'DOCUMENT_HASH') \
     LIMIT ?1";

/// The rows the app linked, in id order after `?1`, at most `?2`. Every
/// link is MusicBrainz's (a CHECK), so the walk goes by the primary key.
fn automatic_links_after_sql() -> String {
    format!(
        "SELECT local_artist_id FROM local_artist_external_identities \
         WHERE decision_source IN {REVISABLE} AND local_artist_id > ?1 \
         ORDER BY local_artist_id LIMIT ?2"
    )
}

/// The ids row `?1` is proven to be: up to two of its current proofs'
/// ids, and the lowest and highest ids its credits' files hold.
const ROW_IDS: &str = "SELECT DISTINCT mbid FROM ( \
     SELECT (SELECT min(artist_mbid) FROM local_track_artists \
      WHERE local_artist_id = ?1 AND artist_mbid IS NOT NULL) AS mbid \
     UNION ALL SELECT (SELECT max(artist_mbid) FROM local_track_artists \
      WHERE local_artist_id = ?1 AND artist_mbid IS NOT NULL) \
     UNION ALL SELECT (SELECT min(artist_mbid) FROM local_album_artists \
      WHERE local_artist_id = ?1 AND artist_mbid IS NOT NULL) \
     UNION ALL SELECT (SELECT max(artist_mbid) FROM local_album_artists \
      WHERE local_artist_id = ?1 AND artist_mbid IS NOT NULL) \
     UNION ALL SELECT mbid FROM (SELECT DISTINCT p.artist_mbid AS mbid \
      FROM library_artist_credit_proofs p WHERE p.source_local_artist_id = ?1 \
      AND (CURRENT_PROOF) LIMIT 2)) WHERE mbid IS NOT NULL";

/// Have the next sweep work album `album` through again: which rows its
/// credits point at, the names they are credited as or an artist id they
/// hold changed in a way the stamp does not see.
pub(crate) fn mark_stale(conn: &Connection, album: &str) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "UPDATE library_artist_reconciliation_state SET input_revision = '' \
         WHERE local_album_id = ?1 AND input_revision <> ''",
    )?
    .execute(params![album])?;
    Ok(())
}

/// The reason of an album whose accepted release has no document on file.
pub(crate) const RELEASE_DOCUMENT_MISSING: &str = "RELEASE_DOCUMENT_MISSING";
/// The reason of an album whose accepted release MusicBrainz answered it
/// does not have. The album is offered for identification again, and its
/// document is not asked for while the reason stands; the album's next
/// pass, after its match or credits change, sets it back to missing.
pub(crate) const RELEASE_NOT_FOUND: &str = "RELEASE_NOT_FOUND";
/// The reason of an album whose accepted release's document MusicBrainz
/// refused in a way asking again will not change (an id it refuses, a
/// redirect not followed, a document that does not read). Its document is
/// not asked for while the reason stands, as for [`RELEASE_NOT_FOUND`].
pub(crate) const RELEASE_DOCUMENT_REFUSED: &str = "RELEASE_DOCUMENT_REFUSED";

/// Whether row `?1` is credited on a live album waiting for the document
/// of its accepted release (reasons `?2`, `?3` and `?4`).
const WAITS_FOR_DOCUMENT: &str = "SELECT EXISTS (SELECT 1 FROM local_album_artists c \
     JOIN library_artist_reconciliation_state s ON s.local_album_id = c.local_album_id \
     JOIN local_albums a ON a.id = c.local_album_id \
     WHERE c.local_artist_id = ?1 AND a.retired_into_album_id IS NULL \
     AND s.state = 'waiting_for_identity' AND s.reason_code IN (?2, ?3, ?4)) \
     OR EXISTS (SELECT 1 FROM local_track_artists c \
     JOIN local_tracks t ON t.id = c.local_track_id \
     JOIN library_artist_reconciliation_state s ON s.local_album_id = t.local_album_id \
     JOIN local_albums a ON a.id = t.local_album_id \
     WHERE c.local_artist_id = ?1 AND t.availability = 'indexed' \
     AND a.retired_into_album_id IS NULL \
     AND s.state = 'waiting_for_identity' AND s.reason_code IN (?2, ?3, ?4))";

/// Whether row `artist` is credited on a live album that waits for its
/// release's document. Such an album proves nothing yet and contradicts
/// nothing, so the app's link of a row nothing proves stays meanwhile: a
/// link carried from v2 outlives the first sweep after the upgrade.
fn waits_for_document(conn: &Connection, artist: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(WAITS_FOR_DOCUMENT)?.query_row(
        params![
            artist,
            RELEASE_DOCUMENT_MISSING,
            RELEASE_NOT_FOUND,
            RELEASE_DOCUMENT_REFUSED
        ],
        |row| row.get(0),
    )
}

/// Why album `album`'s document will not come ([`RELEASE_NOT_FOUND`] or
/// [`RELEASE_DOCUMENT_REFUSED`]), while its stamp is the one that answer
/// was recorded under: a pass over an unchanged match does not ask again.
fn settled_reason(conn: &Connection, album: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(&format!(
        "SELECT s.reason_code FROM library_artist_reconciliation_state s \
         JOIN local_albums a ON a.id = s.local_album_id \
         WHERE s.local_album_id = ?1 AND s.reason_code IN (?2, ?3) \
         AND s.input_revision = ({STAMP})"
    ))?
    .query_row(
        params![album, RELEASE_NOT_FOUND, RELEASE_DOCUMENT_REFUSED],
        |row| row.get(0),
    )
    .optional()
}

/// Have the next sweep work through again every live album matched to
/// release `release_mbid` (lowercased), once its document is on file.
/// Release ids are stored as MusicBrainz gives them, with no spaces, so
/// the lookup goes through the index on `lower(release_mbid)`.
pub(crate) fn mark_release_stale(conn: &Connection, release_mbid: &str) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "UPDATE library_artist_reconciliation_state SET input_revision = '' \
         WHERE input_revision <> '' AND local_album_id IN (SELECT e.local_album_id \
         FROM local_album_external_identities e JOIN local_albums a ON a.id = e.local_album_id \
         WHERE e.provider = 'musicbrainz' AND a.retired_into_album_id IS NULL \
         AND lower(e.release_mbid) = ?1)",
    )?
    .execute(params![release_mbid])?;
    Ok(())
}

/// Up to `limit` releases whose document a live album waits for, the
/// longest wait first, leaving out any whose current document (`?2`, the
/// document version's hash) is on file already.
pub(crate) fn releases_waiting(
    conn: &Connection,
    input_hash: &str,
    limit: usize,
) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(
        "SELECT lower(trim(e.release_mbid)) AS release FROM library_artist_reconciliation_state s \
         JOIN local_albums a ON a.id = s.local_album_id \
         JOIN local_album_external_identities e ON e.local_album_id = a.id \
          AND e.provider = 'musicbrainz' \
         WHERE s.state = 'waiting_for_identity' AND s.reason_code = ?1 \
         AND a.retired_into_album_id IS NULL AND trim(COALESCE(e.release_mbid, '')) <> '' \
         AND NOT EXISTS (SELECT 1 FROM library_management_metadata_snapshots m \
          WHERE m.provider = 'musicbrainz' AND m.entity_kind = 'release' \
          AND m.entity_id = lower(trim(e.release_mbid)) AND m.input_hash = ?2) \
         GROUP BY release ORDER BY min(s.updated_at), release LIMIT ?3",
    )?
    .query_map(
        params![RELEASE_DOCUMENT_MISSING, input_hash, limit as i64],
        |row| row.get(0),
    )?
    .collect()
}

/// Record on the albums waiting for release `release_mbid`'s (lowercased)
/// document why it will not come: `reason` is [`RELEASE_NOT_FOUND`] or
/// [`RELEASE_DOCUMENT_REFUSED`]. Returns those albums.
pub(crate) fn release_settled(
    conn: &Connection,
    release_mbid: &str,
    reason: &str,
    now_ms: i64,
) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(
        "UPDATE library_artist_reconciliation_state SET reason_code = ?2, updated_at = ?3 \
         WHERE state = 'waiting_for_identity' AND reason_code = ?1 \
         AND local_album_id IN (SELECT local_album_id FROM local_album_external_identities \
         WHERE provider = 'musicbrainz' AND lower(release_mbid) = ?4) \
         RETURNING local_album_id",
    )?
    .query_map(
        params![RELEASE_DOCUMENT_MISSING, reason, now_ms, release_mbid],
        |row| row.get(0),
    )?
    .collect()
}

fn stale_albums_sql() -> String {
    STALE_ALBUMS
        .replace("STAMP", STAMP)
        .replace("DOCUMENT_HASH", &release_input_hash())
}

fn row_ids_sql() -> String {
    ROW_IDS.replace("CURRENT_PROOF", CURRENT_PROOF)
}

/// Up to `limit` albums a sweep should work through again.
pub fn stale_albums(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(&stale_albums_sql())?
        .query_map(params![limit as i64], |row| row.get(0))?
        .collect()
}

/// Up to `limit` rows the app linked, after `cursor` in id order.
pub fn automatic_links_after(
    conn: &Connection,
    cursor: &str,
    limit: usize,
) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(&automatic_links_after_sql())?
        .query_map(params![cursor, limit as i64], |row| row.get(0))?
        .collect()
}

/// One credit of a side, as the catalog holds it.
struct LocalCredit {
    position: i64,
    artist: String,
    /// The name it is credited as, else the row's name.
    name: String,
    /// A live row that is neither a sentinel nor a Various Artists row.
    provable: bool,
    /// The id the credit's file holds, if any.
    file_id: Option<String>,
}

fn side_credits(
    conn: &Connection,
    table: &str,
    key: &str,
    subject: &str,
) -> rusqlite::Result<Vec<LocalCredit>> {
    conn.prepare_cached(&format!(
        "SELECT c.position, c.local_artist_id, c.credited_name, a.display_name, \
         a.retired_into_artist_id IS NULL AND a.kind <> 'various_artists' \
         AND a.id NOT IN (?2, ?3), c.artist_mbid \
         FROM {table} c JOIN local_artists a ON a.id = c.local_artist_id \
         WHERE c.{key} = ?1 ORDER BY c.position"
    ))?
    .query_map(
        params![subject, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
        |row| {
            let credited: Option<String> = row.get(2)?;
            let display: String = row.get(3)?;
            let name = credited
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(display);
            Ok(LocalCredit {
                position: row.get(0)?,
                artist: row.get(1)?,
                name,
                provable: row.get(4)?,
                file_id: row.get(5)?,
            })
        },
    )?
    .collect()
}

/// The release credit's ids (lowercased, MusicBrainz's current ones),
/// when the side lines up with them: as many credits, positions in order,
/// distinct UUIDs, and no row twice.
fn lined_up(side: &[LocalCredit], ids: Vec<String>) -> Option<Vec<String>> {
    if side.is_empty() || side.len() != ids.len() {
        return None;
    }
    if side
        .iter()
        .enumerate()
        .any(|(index, credit)| credit.position != index as i64)
    {
        return None;
    }
    let rows: BTreeSet<&str> = side.iter().map(|credit| credit.artist.as_str()).collect();
    let distinct: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    let valid = ids
        .iter()
        .all(|id| id.len() == 36 && uuid::Uuid::parse_str(id).is_ok());
    (valid && distinct.len() == ids.len() && rows.len() == side.len()).then_some(ids)
}

/// Whether the credit's name is the release's for `mbid`: as credited
/// there, or one of the artist's MusicBrainz names when those are stored.
fn names_agree(
    credit: &str,
    release: &CreditedArtist,
    mbid: &str,
    names: Option<&ArtistNames>,
) -> bool {
    let folded = fold_text(credit);
    if folded.is_empty() {
        return false;
    }
    if fold_text(&release.name) == folded {
        return true;
    }
    names
        .and_then(|names| names.artists.get(mbid))
        .is_some_and(|artist| {
            fold_text(&artist.name) == folded
                || artist
                    .aliases
                    .iter()
                    .flatten()
                    .any(|alias| fold_text(&alias.name) == folded)
        })
}

pub(crate) fn placeholder(mbid: &str) -> bool {
    PLACEHOLDER_ARTISTS.contains(&mbid)
}

/// One proven position, as stored.
struct Proof {
    track: Option<String>,
    subject: String,
    position: i64,
    artist: String,
    mbid: String,
    canonical: String,
    credited: String,
    sort: String,
    join: String,
    release: String,
    release_track: Option<String>,
    album_revision: i64,
    track_revision: Option<i64>,
}

impl Proof {
    fn kind(&self) -> &'static str {
        if self.track.is_some() {
            "track"
        } else {
            "album"
        }
    }

    fn hash(&self, album: &str) -> String {
        let parts = [
            self.kind(),
            &self.subject,
            album,
            &self.position.to_string(),
            &self.artist,
            &self.mbid,
            &self.canonical,
            &self.credited,
            &self.sort,
            &self.join,
            &self.release,
            self.release_track.as_deref().unwrap_or(""),
            &self.album_revision.to_string(),
            &self
                .track_revision
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ];
        let digest = Sha256::digest(parts.join("\u{1f}").as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// What one side proves, position by position: `false` when it does not
/// line up. A release id MusicBrainz merged counts as the id it was merged
/// into; the names are still looked up by the document's own id.
#[allow(clippy::too_many_arguments)]
fn prove_side(
    conn: &Connection,
    side: &[LocalCredit],
    credits: &[CreditedArtist],
    names: Option<&ArtistNames>,
    subject: &str,
    track: Option<(&str, &str, i64)>,
    release: &str,
    album_revision: i64,
    out: &mut Vec<Proof>,
) -> rusqlite::Result<bool> {
    let current = credits
        .iter()
        .map(|artist| merged_ids::current_id(conn, &artist.id))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let Some(ids) = lined_up(side, current) else {
        return Ok(false);
    };
    for ((credit, artist), mbid) in side.iter().zip(credits).zip(ids) {
        let own = artist.id.trim().to_ascii_lowercase();
        if placeholder(&mbid) || !credit.provable || !names_agree(&credit.name, artist, &own, names)
        {
            continue;
        }
        out.push(Proof::of(
            credit,
            artist,
            mbid,
            names,
            subject,
            track,
            release,
            album_revision,
        ));
    }
    Ok(true)
}

impl Proof {
    /// The proof that `credit` is `artist` of the release, as `mbid`.
    #[allow(clippy::too_many_arguments)]
    fn of(
        credit: &LocalCredit,
        artist: &CreditedArtist,
        mbid: String,
        names: Option<&ArtistNames>,
        subject: &str,
        track: Option<(&str, &str, i64)>,
        release: &str,
        album_revision: i64,
    ) -> Self {
        let own = artist.id.trim().to_ascii_lowercase();
        let canonical = names
            .and_then(|names| names.artists.get(&own))
            .map(|known| known.name.clone())
            .unwrap_or_else(|| artist.name.clone());
        Self {
            track: track.map(|(id, _, _)| id.to_owned()),
            subject: subject.to_owned(),
            position: credit.position,
            artist: credit.artist.clone(),
            mbid,
            canonical,
            credited: artist.name.clone(),
            sort: artist.sort_name.clone().unwrap_or_default(),
            join: artist.join.clone(),
            release: release.to_owned(),
            release_track: track.map(|(_, release_track, _)| release_track.to_owned()),
            album_revision,
            track_revision: track.map(|(_, _, revision)| revision),
        }
    }
}

/// The album's proof of its one artist `album_artist` standing in for a
/// track side that does not line up: the file credits one artist where
/// the release credits a lead and others (guests, a duet). The credit is
/// proven to be the lead when the lead is `album_artist`, the credit's
/// name agrees with the lead and no other credit of the track, it holds
/// none of the others' names (a combined tag such as "A feat. B" proves
/// nothing), and its file holds no other id.
#[allow(clippy::too_many_arguments)]
fn stand_in(
    conn: &Connection,
    side: &[LocalCredit],
    credits: &[CreditedArtist],
    names: Option<&ArtistNames>,
    album_artist: &str,
    track: (&str, &str, i64),
    release: &str,
    album_revision: i64,
    out: &mut Vec<Proof>,
) -> rusqlite::Result<()> {
    let [credit] = side else {
        return Ok(());
    };
    let Some((lead, others)) = credits.split_first() else {
        return Ok(());
    };
    if credit.position != 0
        || !credit.provable
        || credit
            .file_id
            .as_deref()
            .is_some_and(|id| !id.eq_ignore_ascii_case(album_artist))
        || merged_ids::current_id(conn, &lead.id)? != album_artist
    {
        return Ok(());
    }
    let agrees = |artist: &CreditedArtist| {
        names_agree(
            &credit.name,
            artist,
            &artist.id.trim().to_ascii_lowercase(),
            names,
        )
    };
    let folded = fold_text(&credit.name);
    let combined = others.iter().any(|other| {
        let name = fold_text(&other.name);
        !name.is_empty() && folded.contains(&name)
    });
    if !agrees(lead) || others.iter().any(agrees) || combined {
        return Ok(());
    }
    out.push(Proof::of(
        credit,
        lead,
        album_artist.to_owned(),
        names,
        track.0,
        Some(track),
        release,
        album_revision,
    ));
    Ok(())
}

/// What an album's accepted match proves: its proofs and its state.
fn prove(
    conn: &Connection,
    album: &str,
    document: &Release,
    release: &str,
    identity_release: &str,
    album_revision: i64,
) -> rusqlite::Result<(Vec<Proof>, CreditState, &'static str)> {
    let names = document.artist_names.as_ref();
    let mut proofs = Vec::new();
    let album_side = side_credits(conn, "local_album_artists", "local_album_id", album)?;
    let lines_up = prove_side(
        conn,
        &album_side,
        &document.artists,
        names,
        album,
        None,
        identity_release,
        album_revision,
        &mut proofs,
    )?;
    // The album's one artist, when its credit lines up and is proven.
    let album_artist = (lines_up && album_side.len() == 1)
        .then(|| {
            proofs
                .iter()
                .find(|proof| proof.track.is_none() && proof.position == 0)
                .map(|proof| proof.mbid.clone())
        })
        .flatten();
    let tracks: Vec<(String, String, i64)> = conn
        .prepare_cached(
            "SELECT t.id, i.release_track_mbid, i.row_revision FROM local_tracks t \
             JOIN local_track_external_identities i \
               ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
             AND lower(trim(COALESCE(i.release_mbid, ''))) = ?2 \
             AND trim(COALESCE(i.release_track_mbid, '')) <> '' ORDER BY t.id",
        )?
        .query_map(params![album, release], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (track, release_track, revision) in &tracks {
        let Some(on_release) = document.tracks.iter().find(|candidate| {
            candidate
                .id
                .trim()
                .eq_ignore_ascii_case(release_track.trim())
        }) else {
            continue;
        };
        let side = side_credits(conn, "local_track_artists", "local_track_id", track)?;
        let mapped = (track.as_str(), release_track.as_str(), *revision);
        let track_lines_up = prove_side(
            conn,
            &side,
            &on_release.artists,
            names,
            track,
            Some(mapped),
            identity_release,
            album_revision,
            &mut proofs,
        )?;
        if !track_lines_up && let Some(album_artist) = &album_artist {
            stand_in(
                conn,
                &side,
                &on_release.artists,
                names,
                album_artist,
                mapped,
                identity_release,
                album_revision,
                &mut proofs,
            )?;
        }
    }
    Ok(if lines_up {
        (proofs, CreditState::Projected, "CREDITS_LINE_UP")
    } else {
        (
            proofs,
            CreditState::AmbiguousCreditStructure,
            "ALBUM_CREDITS_DO_NOT_LINE_UP",
        )
    })
}

/// A proof that moved, at the same credit, from one row to another and
/// still proves the same artist: the credit's row changed under it (a
/// changed sort name, say), not who the credit is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedProof {
    pub from: String,
    pub to: String,
    pub mbid: String,
}

/// What [`link_album`] found: the ids worth judging for a merge, and the
/// proofs that moved to another row.
#[derive(Debug, Default)]
pub struct Linked {
    pub ids: Vec<String>,
    pub moved: Vec<MovedProof>,
}

/// Replace the album's proofs with `proofs`, writing only what differs.
/// Returns the rows of every proof removed or changed, and the proofs that
/// moved to another row with the same id.
fn store_proofs(
    tx: &Transaction<'_>,
    album: &str,
    proofs: &[Proof],
    now_ms: i64,
) -> rusqlite::Result<(BTreeSet<String>, Vec<MovedProof>)> {
    type Key = (String, String, i64);
    #[allow(clippy::type_complexity)]
    let existing: Vec<(Key, String, Option<String>, String, String)> = tx
        .prepare_cached(
            "SELECT subject_kind, subject_id, credit_position, evidence_hash, \
             source_local_artist_id, local_artist_id, artist_mbid \
             FROM library_artist_credit_proofs WHERE local_album_id = ?1",
        )?
        .query_map(params![album], |row| {
            Ok((
                (row.get(0)?, row.get(1)?, row.get(2)?),
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let wanted: BTreeMap<Key, (&Proof, String)> = proofs
        .iter()
        .map(|proof| {
            (
                (
                    proof.kind().to_owned(),
                    proof.subject.clone(),
                    proof.position,
                ),
                (proof, proof.hash(album)),
            )
        })
        .collect();
    let mut lost = BTreeSet::new();
    let mut moved = Vec::new();
    for (key, hash, source, artist, mbid) in &existing {
        let kept = wanted.get(key).is_some_and(|(_, wanted)| wanted == hash);
        if kept {
            continue;
        }
        if let (Some(from), Some((proof, _))) = (source, wanted.get(key))
            && *from != proof.artist
            && *mbid == proof.mbid
        {
            moved.push(MovedProof {
                from: from.clone(),
                to: proof.artist.clone(),
                mbid: mbid.clone(),
            });
        }
        lost.extend(source.iter().cloned());
        lost.insert(artist.clone());
        if !wanted.contains_key(key) {
            tx.prepare_cached(
                "DELETE FROM library_artist_credit_proofs \
                 WHERE subject_kind = ?1 AND subject_id = ?2 AND credit_position = ?3",
            )?
            .execute(params![key.0, key.1, key.2])?;
        }
    }
    for (proof, hash) in wanted.values() {
        tx.prepare_cached(
            "INSERT INTO library_artist_credit_proofs (subject_kind, subject_id, \
             local_album_id, local_track_id, credit_position, source_local_artist_id, \
             local_artist_id, artist_mbid, canonical_name, credited_name, sort_name, \
             join_phrase, release_mbid, release_track_mbid, album_identity_revision, \
             track_identity_revision, evidence_hash, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
             ?16, ?17, ?17) \
             ON CONFLICT (subject_kind, subject_id, credit_position) DO UPDATE SET \
             local_album_id = excluded.local_album_id, local_track_id = excluded.local_track_id, \
             source_local_artist_id = excluded.source_local_artist_id, \
             local_artist_id = excluded.local_artist_id, artist_mbid = excluded.artist_mbid, \
             canonical_name = excluded.canonical_name, credited_name = excluded.credited_name, \
             sort_name = excluded.sort_name, join_phrase = excluded.join_phrase, \
             release_mbid = excluded.release_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             album_identity_revision = excluded.album_identity_revision, \
             track_identity_revision = excluded.track_identity_revision, \
             evidence_hash = excluded.evidence_hash, updated_at = excluded.updated_at, \
             row_revision = library_artist_credit_proofs.row_revision + 1 \
             WHERE library_artist_credit_proofs.evidence_hash IS NOT excluded.evidence_hash",
        )?
        .execute(params![
            proof.kind(),
            proof.subject,
            album,
            proof.track,
            proof.position,
            proof.artist,
            proof.mbid,
            proof.canonical,
            proof.credited,
            proof.sort,
            proof.join,
            proof.release,
            proof.release_track,
            proof.album_revision,
            proof.track_revision,
            hash,
            now_ms,
        ])?;
    }
    Ok((lost, moved))
}

/// Write the album's state with its stamp, or remove it (`None`), only
/// where it differs.
fn store_state(
    tx: &Transaction<'_>,
    album: &str,
    state: Option<(CreditState, &str)>,
    now_ms: i64,
) -> rusqlite::Result<()> {
    let Some((state, reason)) = state else {
        tx.prepare_cached(
            "DELETE FROM library_artist_reconciliation_state WHERE local_album_id = ?1",
        )?
        .execute(params![album])?;
        return Ok(());
    };
    let stamp: String = tx
        .prepare_cached(&format!(
            "SELECT {STAMP} FROM local_albums a WHERE a.id = ?1"
        ))?
        .query_row(params![album], |row| row.get(0))?;
    tx.prepare_cached(
        "INSERT INTO library_artist_reconciliation_state (local_album_id, input_revision, state, \
         reason_code, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (local_album_id) DO UPDATE SET input_revision = excluded.input_revision, \
         state = excluded.state, reason_code = excluded.reason_code, \
         updated_at = excluded.updated_at \
         WHERE library_artist_reconciliation_state.input_revision IS NOT excluded.input_revision \
         OR library_artist_reconciliation_state.state IS NOT excluded.state \
         OR library_artist_reconciliation_state.reason_code IS NOT excluded.reason_code",
    )?
    .execute(params![album, stamp, state.as_str(), reason, now_ms])?;
    Ok(())
}

/// Work out what the album's match proves about its artists, store it,
/// and settle the links of every row credited on it and of every row that
/// lost a proof. Returns the ids the album proves and the ids newly
/// linked (the groups worth judging for a merge), and the proofs that
/// moved to another row. One transaction, no file read, nothing asked of
/// any outside service.
pub fn link_album(tx: &Transaction<'_>, album: &str, now_ms: i64) -> rusqlite::Result<Linked> {
    let live: Option<bool> = tx
        .prepare_cached("SELECT retired_into_album_id IS NULL FROM local_albums WHERE id = ?1")?
        .query_row(params![album], |row| row.get(0))
        .optional()?;
    let Some(live) = live else {
        return Ok(Linked::default());
    };
    let identity: Option<(Option<String>, i64)> = if live {
        tx.prepare_cached(
            "SELECT release_mbid, row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = 'musicbrainz'",
        )?
        .query_row(params![album], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?
    } else {
        None
    };
    let (proofs, state) = match identity {
        None => (Vec::new(), None),
        Some((identity_release, revision)) => match accepted_release(tx, album)? {
            None => (
                Vec::new(),
                Some((CreditState::WaitingForIdentity, "MATCH_NOT_ACCEPTED")),
            ),
            Some(release) => match stored_release(tx, &release, 0.0)? {
                // A document not on file proves nothing and contradicts
                // nothing: the album's proofs and its rows' links stay as
                // they are until it lands ([`waits_for_document`]).
                None => {
                    let reason = settled_reason(tx, album)?;
                    store_state(
                        tx,
                        album,
                        Some((
                            CreditState::WaitingForIdentity,
                            reason.as_deref().unwrap_or(RELEASE_DOCUMENT_MISSING),
                        )),
                        now_ms,
                    )?;
                    return Ok(Linked::default());
                }
                Some(document) => {
                    let (proofs, state, reason) = prove(
                        tx,
                        album,
                        &document,
                        &release,
                        identity_release.as_deref().unwrap_or(&release),
                        revision,
                    )?;
                    (proofs, Some((state, reason)))
                }
            },
        },
    };
    let (mut rows, moved) = store_proofs(tx, album, &proofs, now_ms)?;
    store_state(tx, album, state, now_ms)?;
    let credited: Vec<String> = tx
        .prepare_cached(
            "SELECT local_artist_id FROM local_album_artists WHERE local_album_id = ?1 \
             UNION SELECT c.local_artist_id FROM local_track_artists c \
             JOIN local_tracks t ON t.id = c.local_track_id WHERE t.local_album_id = ?1",
        )?
        .query_map(params![album], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    rows.extend(credited);
    let mut ids: BTreeSet<String> = proofs.into_iter().map(|proof| proof.mbid).collect();
    ids.extend(settle(tx, &rows.into_iter().collect::<Vec<_>>(), now_ms)?);
    Ok(Linked {
        ids: ids.into_iter().collect(),
        moved,
    })
}

/// A change to one row's link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkChange {
    /// The row is linked to this id, replacing the app's link it had.
    Link { artist: String, mbid: String },
    /// The app's link of the row goes.
    Unlink { artist: String },
}

/// The ids row `artist` is proven to be, when it is a row that can be:
/// live, not a sentinel and not a Various Artists row.
fn row_ids(conn: &Connection, artist: &str) -> rusqlite::Result<BTreeSet<String>> {
    conn.prepare_cached(&row_ids_sql())?
        .query_map(params![artist], |row| row.get(0))?
        .collect()
}

/// What settling `rows` would change, by the link rule: a row with a
/// person's or v2's link is left alone; a row proven to be exactly one
/// non-placeholder id `x` is linked to it unless another row holds `x`
/// (the oldest row wins among rows settled together); any other row loses
/// the app's link, except a row proven nothing while an album crediting it
/// waits for its release's document. Reads only.
pub fn plan(conn: &Connection, rows: &[String]) -> rusqlite::Result<Vec<LinkChange>> {
    Ok(plan_rows(conn, rows)?.changes)
}

/// What settling some rows would change, and the ids that disagree on
/// them: every id of a row proven to be more than one, and for a row a
/// person or v2 linked, its link's id and ids when they name another.
struct Planned {
    changes: Vec<LinkChange>,
    conflicts: BTreeSet<String>,
}

fn plan_rows(conn: &Connection, rows: &[String]) -> rusqlite::Result<Planned> {
    struct Candidate {
        created: f64,
        artist: String,
        mbid: String,
        linked: bool,
    }
    let rows: BTreeSet<&String> = rows.iter().collect();
    let mut unlinks: BTreeSet<String> = BTreeSet::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut conflicts: BTreeSet<String> = BTreeSet::new();
    for artist in rows {
        let row: Option<(f64, bool)> = conn
            .prepare_cached(
                "SELECT created_at, retired_into_artist_id IS NULL \
                 AND kind <> 'various_artists' AND id NOT IN (?2, ?3) \
                 FROM local_artists WHERE id = ?1",
            )?
            .query_row(
                params![artist, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((created, provable)) = row else {
            continue;
        };
        let link: Option<(String, DecisionSource)> = conn
            .prepare_cached(
                "SELECT lower(provider_artist_id), decision_source \
                 FROM local_artist_external_identities \
                 WHERE local_artist_id = ?1 AND provider = 'musicbrainz'",
            )?
            .query_row(params![artist], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let ids = if provable {
            row_ids(conn, artist)?
        } else {
            BTreeSet::new()
        };
        if let Some((linked, source)) = &link
            && source.is_persons_choice()
        {
            if ids.iter().any(|id| id != linked) {
                conflicts.insert(linked.clone());
                conflicts.extend(ids);
            }
            continue;
        }
        if ids.len() > 1 {
            conflicts.extend(ids.iter().cloned());
        }
        let unproven = provable && ids.is_empty();
        let only = (ids.len() == 1)
            .then(|| ids.into_iter().next())
            .flatten()
            .filter(|mbid| !placeholder(mbid));
        match only {
            // The app's link gives way to a person's or v2's link to the
            // same id on another row.
            Some(mbid) if link.as_ref().is_some_and(|(linked, _)| *linked == mbid) => {
                if person_holds(conn, &mbid, artist)? {
                    unlinks.insert(artist.clone());
                }
            }
            Some(mbid) => candidates.push(Candidate {
                created,
                artist: artist.clone(),
                mbid,
                linked: link.is_some(),
            }),
            None if link.is_some() && !(unproven && waits_for_document(conn, artist)?) => {
                unlinks.insert(artist.clone());
            }
            None => {}
        }
    }
    candidates.sort_by(|a, b| {
        a.mbid
            .cmp(&b.mbid)
            .then(a.created.total_cmp(&b.created))
            .then_with(|| a.artist.cmp(&b.artist))
    });
    let mut changes: Vec<LinkChange> = unlinks
        .iter()
        .map(|artist| LinkChange::Unlink {
            artist: artist.clone(),
        })
        .collect();
    // Every candidate's own link changes either way, so none of them
    // holds its old id any more.
    let moving: BTreeSet<String> = candidates
        .iter()
        .map(|candidate| candidate.artist.clone())
        .collect();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for candidate in candidates {
        // A person's or v2's link first: it never gives way.
        let owner: Option<String> = conn
            .prepare_cached(&format!(
                "SELECT local_artist_id FROM local_artist_external_identities \
                 WHERE provider = 'musicbrainz' AND lower(provider_artist_id) = ?1 \
                 ORDER BY decision_source IN {PERSONS} DESC, local_artist_id LIMIT 1"
            ))?
            .query_row(params![candidate.mbid], |row| row.get(0))
            .optional()?;
        let owned =
            owner.is_some_and(|owner| !unlinks.contains(&owner) && !moving.contains(&owner));
        if owned || !taken.insert(candidate.mbid.clone()) {
            if candidate.linked {
                changes.push(LinkChange::Unlink {
                    artist: candidate.artist,
                });
            }
        } else {
            changes.push(LinkChange::Link {
                artist: candidate.artist,
                mbid: candidate.mbid,
            });
        }
    }
    Ok(Planned { changes, conflicts })
}

/// Whether a row other than `artist` holds `mbid` by a person's or v2's
/// link.
fn person_holds(conn: &Connection, mbid: &str, artist: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(&format!(
        "SELECT EXISTS (SELECT 1 FROM local_artist_external_identities \
         WHERE provider = 'musicbrainz' AND decision_source IN {PERSONS} \
         AND lower(provider_artist_id) = ?1 AND local_artist_id <> ?2)"
    ))?
    .query_row(params![mbid, artist], |row| row.get(0))
}

/// Move the artist's `updated_at` (seconds) and revision once.
pub(crate) fn touch_artist(tx: &Transaction<'_>, artist: &str, now: f64) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "UPDATE local_artists SET updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1",
    )?
    .execute(params![artist, now])?;
    Ok(())
}

/// Settle the links of `rows` ([`plan`]) and apply the changes: the app's
/// links only, removals first, after a person's or v2's link to an id
/// MusicBrainz merged follows the merge. Each row whose link changed moves
/// its revision once, and the catalog moves once when anything changed.
/// The ids that disagree on a row are recorded to be checked. Returns the
/// ids newly linked.
pub fn settle(tx: &Transaction<'_>, rows: &[String], now_ms: i64) -> rusqlite::Result<Vec<String>> {
    let mut changed = merged_ids::follow_person_links(tx, rows, now_ms)?;
    let Planned { changes, conflicts } = plan_rows(tx, rows)?;
    merged_ids::nominate(tx, &conflicts, now_ms)?;
    let now = now_ms as f64 / 1000.0;
    let mut linked = Vec::new();
    let touch = |artist: &str| touch_artist(tx, artist, now);
    for change in &changes {
        if let LinkChange::Unlink { artist } = change {
            let removed = tx
                .prepare_cached(&format!(
                    "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?1 \
                     AND provider = 'musicbrainz' AND decision_source IN {REVISABLE}"
                ))?
                .execute(params![artist])?;
            if removed > 0 {
                touch(artist)?;
                changed = true;
            }
        }
    }
    // A row taking an id another row of this settle is moving off can
    // only be linked once that row's old link is gone: remove every
    // moving row's old link before linking any.
    for change in &changes {
        if let LinkChange::Link { artist, mbid } = change {
            tx.prepare_cached(&format!(
                "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?1 \
                 AND provider = 'musicbrainz' AND decision_source IN {REVISABLE} \
                 AND lower(provider_artist_id) <> ?2"
            ))?
            .execute(params![artist, mbid])?;
        }
    }
    for change in &changes {
        if let LinkChange::Link { artist, mbid } = change {
            let written = tx
                .prepare_cached(&format!(
                    "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                     provider_artist_id, decision_source, selected_at) \
                     VALUES (?1, 'musicbrainz', ?2, 'automatic', ?3) \
                     ON CONFLICT (local_artist_id, provider) DO UPDATE SET \
                     provider_artist_id = excluded.provider_artist_id, \
                     decision_source = 'automatic', attempt_id = NULL, \
                     selected_by_user_id = NULL, selected_at = excluded.selected_at, \
                     row_revision = local_artist_external_identities.row_revision + 1 \
                     WHERE local_artist_external_identities.decision_source \
                     IN {REVISABLE}"
                ))?
                .execute(params![artist, mbid, now])?;
            if written > 0 {
                touch(artist)?;
                changed = true;
                linked.push(mbid.clone());
            }
        }
    }
    if changed {
        bump_catalog(tx)?;
    }
    Ok(linked)
}
