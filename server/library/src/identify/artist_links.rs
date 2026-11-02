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
//! Every change to a link, the app's or a person's, writes one audit row.
//! A person may link a row by hand or clear the link they set
//! ([`set_by_person`], [`clear_by_person`]); a change that would group the
//! row with another record is refused, so a hand-set link never starts a
//! merge.
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
use super::stores::StoreError;
use crate::catalog_revision::bump_catalog;
use crate::corrections::artist_convergence::{CREDITS_MOVED, MOVED_BY_NAME, MOVED_WITH_ID};
use crate::corrections::continuation::accepted_release;
use crate::identify::sql::ROW_IDS;
use crate::identify::sql::STALE_ALBUMS;
use crate::identify::sql::WAITS_FOR_DOCUMENT;
use crate::identify::sql::{
    AlbumReleaseRevision, ArtistEligibility, ArtistLinkRow, ArtistLinkSource, CreditRow,
    MappedTrack, NamedArtist, StoredProof,
};
use crate::identity::DecisionSource;
use crate::identity::reads::release_input_hash;
use crate::matching::model::{ArtistNames, CreditedArtist, Release, VARIOUS_ARTISTS_MBID};
use crate::operations::decisions::{ArtistAction, record_artist_action};
use crate::scan::sqlite_store::artists::SENTINEL_ARTIST_IDS;
use droppedneedle_kernel::db::statement::Statement;
use droppedneedle_kernel::db::{DbError, fold_text, savepoint};

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
}

crate::scan::models::spelled!(CreditState {
    WaitingForIdentity => "waiting_for_identity",
    AmbiguousCreditStructure => "ambiguous_credit_structure",
    Projected => "projected",
});

/// Have the next sweep work album `album` through again: which rows its
/// credits point at, the names they are credited as or an artist id they
/// hold changed in a way the stamp does not see.
pub(crate) fn mark_stale(conn: &Connection, album: &str) -> rusqlite::Result<()> {
    conn.prepare_cached(crate::identify::sql::MARK_STALE.text())?
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

/// Whether row `artist` is credited on a live album that waits for its
/// release's document. Such an album proves nothing yet and contradicts
/// nothing, so the app's link of a row nothing proves stays meanwhile: a
/// link carried from v2 outlives the first sweep after the upgrade.
fn waits_for_document(conn: &Connection, artist: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(WAITS_FOR_DOCUMENT.text())?.query_row(
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
    conn.prepare_cached(crate::identify::sql::SETTLED_REASON.text())?
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
    conn.prepare_cached(crate::identify::sql::MARK_RELEASE_STALE.text())?
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
    conn.prepare_cached(crate::identify::sql::RELEASES_WAITING.text())?
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
    conn.prepare_cached(crate::identify::sql::RELEASE_SETTLED.text())?
        .query_map(
            params![RELEASE_DOCUMENT_MISSING, reason, now_ms, release_mbid],
            |row| row.get(0),
        )?
        .collect()
}

/// Up to `limit` albums a sweep should work through again.
pub fn stale_albums(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(STALE_ALBUMS.text())?
        .query_map(params![limit as i64, release_input_hash()], |row| {
            row.get(0)
        })?
        .collect()
}

/// Up to `limit` rows the app linked, after `cursor` in id order.
pub fn automatic_links_after(
    conn: &Connection,
    cursor: &str,
    limit: usize,
) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(crate::identify::sql::AUTOMATIC_LINKS_FROM.text())?
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
    statement: Statement,
    subject: &str,
) -> rusqlite::Result<Vec<LocalCredit>> {
    conn.prepare_cached(statement.text())?
        .query_map(
            params![subject, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
            |row| {
                let credit = CreditRow::read(row)?;
                let name = credit
                    .credited_name
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or(credit.display_name);
                Ok(LocalCredit {
                    position: credit.position,
                    artist: credit.local_artist_id,
                    name,
                    provable: credit.provable,
                    file_id: credit.artist_mbid,
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
    let album_side = side_credits(conn, crate::identify::sql::ALBUM_CREDIT_ROWS, album)?;
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
        .prepare_cached(crate::identify::sql::PROVE.text())?
        .query_map(params![album, release], |row| {
            let track = MappedTrack::read(row)?;
            Ok((track.id, track.release_track_mbid, track.row_revision))
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
        let side = side_credits(conn, crate::identify::sql::TRACK_CREDIT_ROWS, track)?;
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
    let existing: Vec<StoredProof> = tx
        .prepare_cached(crate::identify::sql::STORED_PROOFS.text())?
        .query_map(params![album], StoredProof::read)?
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
    for stored in existing {
        let key: Key = (
            stored.subject_kind,
            stored.subject_id,
            stored.credit_position,
        );
        let kept = wanted
            .get(&key)
            .is_some_and(|(_, wanted)| *wanted == stored.evidence_hash);
        if kept {
            continue;
        }
        if let (Some(from), Some((proof, _))) = (&stored.source_local_artist_id, wanted.get(&key))
            && *from != proof.artist
            && stored.artist_mbid == proof.mbid
        {
            moved.push(MovedProof {
                from: from.clone(),
                to: proof.artist.clone(),
                mbid: stored.artist_mbid,
            });
        }
        lost.extend(stored.source_local_artist_id);
        lost.insert(stored.local_artist_id);
        if !wanted.contains_key(&key) {
            tx.prepare_cached(crate::identify::sql::DROP_PROOF.text())?
                .execute(params![key.0, key.1, key.2])?;
        }
    }
    for (proof, hash) in wanted.values() {
        tx.prepare_cached(crate::identify::sql::WRITE_PROOF.text())?
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
        tx.prepare_cached(crate::identify::sql::CLEAR_RECONCILIATION_STATE.text())?
            .execute(params![album])?;
        return Ok(());
    };
    let stamp: String = tx
        .prepare_cached(crate::identify::sql::STORE_STATE.text())?
        .query_row(params![album], |row| row.get(0))?;
    tx.prepare_cached(crate::identify::sql::WRITE_RECONCILIATION_STATE.text())?
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
        .prepare_cached(crate::identify::sql::ALBUM_IS_LIVE.text())?
        .query_row(params![album], |row| row.get(0))
        .optional()?;
    let Some(live) = live else {
        return Ok(Linked::default());
    };
    let identity = if live {
        tx.prepare_cached(crate::identify::sql::ALBUM_RELEASE_AND_REVISION.text())?
            .query_row(params![album], AlbumReleaseRevision::read)
            .optional()?
    } else {
        None
    };
    let (proofs, state) = match identity {
        None => (Vec::new(), None),
        Some(identity) => match accepted_release(tx, album)? {
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
                        identity.release_mbid.as_deref().unwrap_or(&release),
                        identity.row_revision,
                    )?;
                    (proofs, Some((state, reason)))
                }
            },
        },
    };
    let (mut rows, moved) = store_proofs(tx, album, &proofs, now_ms)?;
    store_state(tx, album, state, now_ms)?;
    let credited: Vec<String> = tx
        .prepare_cached(crate::identify::sql::ARTISTS_CREDITED_ON_ALBUM.text())?
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
    Unlink {
        artist: String,
        reason: UnlinkReason,
    },
}

/// Why the app's link of a row goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlinkReason {
    /// Nothing proves the row is the id it is linked to any more.
    NotProven,
    /// Another row holds the id by a person's or v2's link.
    GaveWay,
}

impl UnlinkReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotProven => LINK_NOT_PROVEN,
            Self::GaveWay => LINK_GAVE_WAY,
        }
    }
}

/// The audit's action kinds for a change to an artist's link: one set
/// where there was none, one moved to another id, one removed.
pub const LINK_SET: &str = "artist_link_set";
pub const LINK_MOVED: &str = "artist_link_moved";
pub const LINK_REMOVED: &str = "artist_link_removed";
/// The audit's reasons: the app linked a row its proofs and files name,
/// the app's link went because nothing proves it any more or gave way to
/// a person's link on another row, or a person set or cleared the link.
pub const LINK_PROVEN: &str = "ARTIST_LINK_PROVEN";
pub const LINK_NOT_PROVEN: &str = "ARTIST_LINK_NOT_PROVEN";
pub const LINK_GAVE_WAY: &str = "ARTIST_LINK_GAVE_WAY";
pub const LINK_SET_BY_PERSON: &str = "ARTIST_LINK_SET_BY_PERSON";
pub const LINK_CLEARED_BY_PERSON: &str = "ARTIST_LINK_CLEARED_BY_PERSON";

/// The ids row `artist` is proven to be, when it is a row that can be:
/// live, not a sentinel and not a Various Artists row.
fn row_ids(conn: &Connection, artist: &str) -> rusqlite::Result<BTreeSet<String>> {
    conn.prepare_cached(ROW_IDS.text())?
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
    let mut unlinks: BTreeMap<String, UnlinkReason> = BTreeMap::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut conflicts: BTreeSet<String> = BTreeSet::new();
    for artist in rows {
        let row = conn
            .prepare_cached(crate::identify::sql::ARTIST_CREATED_AND_ELIGIBLE.text())?
            .query_row(
                params![artist, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
                ArtistEligibility::read,
            )
            .optional()?;
        let Some(ArtistEligibility {
            created_at: created,
            provable,
        }) = row
        else {
            continue;
        };
        let link: Option<(String, DecisionSource)> = conn
            .prepare_cached(crate::identify::sql::ARTIST_LINK_AND_SOURCE.text())?
            .query_row(params![artist], ArtistLinkSource::read)
            .map(|link| (link.provider_artist_id, link.decision_source))
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
                    unlinks.insert(artist.clone(), UnlinkReason::GaveWay);
                }
            }
            Some(mbid) => candidates.push(Candidate {
                created,
                artist: artist.clone(),
                mbid,
                linked: link.is_some(),
            }),
            None if link.is_some() && !(unproven && waits_for_document(conn, artist)?) => {
                unlinks.insert(artist.clone(), UnlinkReason::NotProven);
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
        .map(|(artist, reason)| LinkChange::Unlink {
            artist: artist.clone(),
            reason: *reason,
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
        // Any other row keeping a link to the id, in whatever spelling,
        // owns it: a second link to it would clash.
        let holders: Vec<String> = conn
            .prepare_cached(crate::identify::sql::ARTISTS_LINKED_TO_MBID.text())?
            .query_map(params![candidate.mbid], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let owned = holders.iter().any(|holder| {
            *holder != candidate.artist && !unlinks.contains_key(holder) && !moving.contains(holder)
        });
        if owned || !taken.insert(candidate.mbid.clone()) {
            if candidate.linked {
                changes.push(LinkChange::Unlink {
                    artist: candidate.artist,
                    reason: UnlinkReason::NotProven,
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
    conn.prepare_cached(crate::identify::sql::PERSON_HOLDS.text())?
        .query_row(params![mbid, artist], |row| row.get(0))
}

/// Move the artist's `updated_at` (seconds) and revision once.
pub(crate) fn touch_artist(tx: &Transaction<'_>, artist: &str, now: f64) -> rusqlite::Result<()> {
    tx.prepare_cached(crate::identify::sql::TOUCH_ARTIST.text())?
        .execute(params![artist, crate::clock::ms(now)])?;
    Ok(())
}

/// Settle every row with an app link that holds one of `ids` through its
/// link, a credit's file id or a current proof, so a row whose files are
/// one artist but whose link is another is settled before that artist's
/// group is judged. A failure is rolled back and logged; the caller goes
/// on, and a later pass tries again.
pub(crate) fn settle_holders(
    tx: &Transaction<'_>,
    ids: &[String],
    now_ms: i64,
) -> rusqlite::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let settled = savepoint(tx, "artist_links", || {
        let mut rows: BTreeSet<String> = BTreeSet::new();
        for id in ids {
            let held = tx
                .prepare_cached(crate::identify::sql::SETTLE_HOLDERS.text())?
                .query_map(params![id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.extend(held);
        }
        let rows: Vec<String> = rows.into_iter().collect();
        settle(tx, &rows, now_ms)
    })?;
    if let Err(error) = settled {
        tracing::warn!(%error, "the app's artist links could not be settled; a later pass tries again");
    }
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
    for change in &changes {
        if let LinkChange::Unlink { artist, reason } = change
            && drop_app_link(tx, artist, reason.code(), now)?
        {
            changed = true;
        }
    }
    // A row taking an id another row of this settle is moving off can
    // only be linked once that row's old link is gone: remove every
    // moving row's old link before linking any.
    let mut before: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for change in &changes {
        if let LinkChange::Link { artist, mbid } = change {
            before.insert(artist, app_link(tx, artist)?);
            tx.prepare_cached(crate::identify::sql::DROP_REVISABLE_LINK.text())?
                .execute(params![artist, mbid])?;
        }
    }
    for change in &changes {
        if let LinkChange::Link { artist, mbid } = change {
            let written = tx
                .prepare_cached(crate::identify::sql::WRITE_SETTLED_LINK.text())?
                .execute(params![artist, mbid, crate::clock::ms(now)])?;
            if written > 0 {
                touch_artist(tx, artist, now)?;
                let old = before.get(artist.as_str()).cloned().flatten();
                audit_link(
                    tx,
                    "system",
                    artist,
                    old.as_deref(),
                    Some(mbid),
                    LINK_PROVEN,
                    now,
                )?;
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

/// The id of row `artist`'s link when it is the app's own.
fn app_link(conn: &Connection, artist: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(crate::identify::sql::APP_LINK.text())?
        .query_row(params![artist], |row| row.get(0))
        .optional()
}

/// Write the audit row of a change to row `artist`'s link from `before`
/// to `after` (`None` for no link), by `actor` (`system` for the app).
pub(crate) fn audit_link(
    tx: &Transaction<'_>,
    actor: &str,
    artist: &str,
    before: Option<&str>,
    after: Option<&str>,
    reason: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let kind = match (before, after) {
        (None, _) => LINK_SET,
        (Some(old), Some(new)) if old == new => LINK_SET,
        (Some(_), Some(_)) => LINK_MOVED,
        (Some(_), None) => LINK_REMOVED,
    };
    record_artist_action(
        tx,
        ArtistAction {
            actor,
            kind,
            artist_id: artist,
            before: serde_json::json!({ "provider_artist_id": before }),
            after: serde_json::json!({ "provider_artist_id": after }),
            reason,
        },
        now,
    )?;
    Ok(())
}

/// Remove the app's link of row `artist`, if it has one: the row's
/// revision moves and the audit records why (`reason`). The one way the
/// app's link of a row goes. Returns whether a link went.
pub(crate) fn drop_app_link(
    tx: &Transaction<'_>,
    artist: &str,
    reason: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let removed: Option<String> = tx
        .prepare_cached(crate::identify::sql::DROP_APP_LINK.text())?
        .query_row(params![artist], |row| row.get(0))
        .optional()?;
    let Some(removed) = removed else {
        return Ok(false);
    };
    touch_artist(tx, artist, now)?;
    audit_link(tx, "system", artist, Some(&removed), None, reason, now)?;
    Ok(true)
}

/// Who decided an artist's link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSource {
    /// The app, from the album matches and the files.
    App,
    /// A person.
    Person,
    /// A person's choice carried over from v2.
    V2,
}

impl From<DecisionSource> for LinkSource {
    fn from(source: DecisionSource) -> Self {
        match source {
            DecisionSource::Manual => Self::Person,
            DecisionSource::LegacyImport => Self::V2,
            DecisionSource::Automatic | DecisionSource::Embedded => Self::App,
        }
    }
}

/// An artist's MusicBrainz link, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredLink {
    pub mbid: String,
    pub source: LinkSource,
    /// The person who set it, while their account exists.
    pub selected_by: Option<String>,
    /// Unix milliseconds.
    pub selected_at_ms: i64,
}

/// An artist record's link and the revision a change must name.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistLink {
    pub artist_revision: i64,
    pub link: Option<StoredLink>,
}

/// Why a person's change to an artist's link was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRefusal {
    /// No artist record with that id.
    NotFound,
    /// Not a MusicBrainz artist id.
    InvalidId,
    /// One of MusicBrainz's special-purpose artists.
    SpecialPurpose,
    /// A sentinel, a Various Artists record or one merged into another.
    NotLinkable,
    /// The record changed since it was read.
    Stale,
    /// The change would put the record in a group with these others (id,
    /// name): another record holds the id, or would be proven it. Records
    /// are merged by an admin, never by a person's link.
    Shared(Vec<(String, String)>),
    /// Only a person's or v2's link can be cleared.
    NotPersons,
}

/// A person's change to an artist's link that did not land.
#[derive(Debug)]
pub enum ArtistLinkError {
    Refused(LinkRefusal),
    Store(StoreError),
}

impl std::fmt::Display for ArtistLinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => write!(f, "artist link refused: {refusal:?}"),
            Self::Store(error) => error.fmt(f),
        }
    }
}

impl From<DbError> for ArtistLinkError {
    fn from(error: DbError) -> Self {
        Self::Store(error.into())
    }
}

impl From<rusqlite::Error> for ArtistLinkError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.into())
    }
}

impl From<StoreError> for ArtistLinkError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// Row `artist`'s link and revision; `None` when there is no such row.
pub fn artist_link(conn: &Connection, artist: &str) -> rusqlite::Result<Option<ArtistLink>> {
    let revision: Option<i64> = conn
        .prepare_cached(crate::identify::sql::ARTIST_REVISION.text())?
        .query_row(params![artist], |row| row.get(0))
        .optional()?;
    let Some(artist_revision) = revision else {
        return Ok(None);
    };
    let link = conn
        .prepare_cached(crate::identify::sql::ARTIST_LINK_WITH_SELECTOR.text())?
        .query_row(params![artist], ArtistLinkRow::read)
        .map(|link| StoredLink {
            mbid: link.provider_artist_id,
            source: LinkSource::from(link.decision_source),
            selected_by: link.id,
            selected_at_ms: link.selected_at.0,
        })
        .optional()?;
    Ok(Some(ArtistLink {
        artist_revision,
        link,
    }))
}

/// Row `artist` as a person's change finds it: its link, or a refusal
/// when it is not there or changed since `expected_revision`.
fn person_target(
    tx: &Transaction<'_>,
    artist: &str,
    expected_revision: i64,
) -> Result<(ArtistLink, bool), ArtistLinkError> {
    let provable: Option<bool> = tx
        .prepare_cached(crate::identify::sql::PERSON_TARGET.text())?
        .query_row(
            params![artist, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
            |row| row.get(0),
        )
        .optional()?;
    let (Some(provable), Some(current)) = (provable, artist_link(tx, artist)?) else {
        return Err(ArtistLinkError::Refused(LinkRefusal::NotFound));
    };
    if current.artist_revision != expected_revision {
        return Err(ArtistLinkError::Refused(LinkRefusal::Stale));
    }
    Ok((current, provable))
}

fn named_artist(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String)> {
    let found = NamedArtist::read(row)?;
    Ok((found.id, found.display_name))
}

/// The live records other than `artist` that a link of `artist` to
/// `mbid` would group it with, as (id, name), at most ten: any record
/// holding `mbid` by a link, and any record its credits' files or a
/// current proof name as `mbid`, unless a person or v2 linked that record
/// to another id.
fn sharing(conn: &Connection, artist: &str, mbid: &str) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare_cached(crate::identify::sql::SHARING.text())?
        .query_map(
            params![mbid, artist, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
            named_artist,
        )?
        .collect()
}

/// The live rows an open recorded move of `artist`'s credits points at,
/// as (id, name), at most ten. While `artist` holds a link, the merge that
/// follows such a move is held back; clearing the link would let it run.
fn follows_out(conn: &Connection, artist: &str) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare_cached(crate::identify::sql::FOLLOWS_OUT.text())?
        .query_map(
            params![artist, MOVED_BY_NAME, MOVED_WITH_ID, CREDITS_MOVED],
            named_artist,
        )?
        .collect()
}

/// The live rows waiting to follow their credits into `artist`, those
/// proven to be `mbid` when one is given, as (id, name), at most ten. A
/// link of `artist` to that id, or clearing its link, could let such a
/// merge run.
fn follows_in(
    conn: &Connection,
    artist: &str,
    mbid: Option<&str>,
) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare_cached(crate::identify::sql::FOLLOWS_IN.text())?
        .query_map(params![artist, CREDITS_MOVED, mbid], named_artist)?
        .collect()
}

/// The records a person's link of `artist` to `mbid` would group it with
/// ([`sharing`]) or let a waiting merge fold into it ([`follows_in`]), as
/// (id, name), at most ten. A link by hand is never written while any is
/// left: merging records is an admin's.
pub(crate) fn would_group(
    conn: &Connection,
    artist: &str,
    mbid: &str,
) -> rusqlite::Result<Vec<(String, String)>> {
    let mut others: BTreeSet<(String, String)> = sharing(conn, artist, mbid)?.into_iter().collect();
    others.extend(follows_in(conn, artist, Some(mbid))?);
    Ok(others.into_iter().take(10).collect())
}

/// A person links row `artist` to MusicBrainz artist `raw_mbid`. The id
/// is read lowercased, an id MusicBrainz merged as the one it was merged
/// into, and stored as the person's (`manual`), which the app's settling
/// never moves. A link that would group the row with another record
/// ([`would_group`]) is refused: merging records is an admin's. Returns the row's link after.
pub fn set_by_person(
    tx: &Transaction<'_>,
    artist: &str,
    raw_mbid: &str,
    expected_revision: i64,
    user_id: &str,
    now_ms: i64,
) -> Result<ArtistLink, ArtistLinkError> {
    let refused = |refusal| Err(ArtistLinkError::Refused(refusal));
    let mbid = raw_mbid.trim().to_ascii_lowercase();
    if mbid.len() != 36 || uuid::Uuid::parse_str(&mbid).is_err() {
        return refused(LinkRefusal::InvalidId);
    }
    let mbid = merged_ids::current_id(tx, &mbid)?;
    if placeholder(&mbid) {
        return refused(LinkRefusal::SpecialPurpose);
    }
    let (current, provable) = person_target(tx, artist, expected_revision)?;
    if !provable {
        return refused(LinkRefusal::NotLinkable);
    }
    let before = current.link.as_ref();
    if before.is_some_and(|link| link.mbid == mbid && link.source != LinkSource::App) {
        return Ok(current);
    }
    let others = would_group(tx, artist, &mbid)?;
    if !others.is_empty() {
        return refused(LinkRefusal::Shared(others));
    }
    let now = now_ms as f64 / 1000.0;
    tx.prepare_cached(crate::identify::sql::SET_BY_PERSON.text())?
        .execute(params![artist, mbid, user_id, crate::clock::ms(now)])?;
    touch_artist(tx, artist, now)?;
    audit_link(
        tx,
        user_id,
        artist,
        before.map(|link| link.mbid.as_str()),
        Some(&mbid),
        LINK_SET_BY_PERSON,
        now,
    )?;
    bump_catalog(tx)?;
    artist_link(tx, artist)?.ok_or(ArtistLinkError::Refused(LinkRefusal::NotFound))
}

/// A person clears row `artist`'s link, set by a person or carried from
/// v2, and the row is settled again at once, so the app's own link comes
/// back where the album matches and files prove one. A clear after which
/// the row would be proven an id another record holds, or that its files
/// or proofs name, or that would free a recorded move of credits to merge
/// it into another record or another record into it, is refused: merging
/// records is an admin's. Returns the row's link after.
pub fn clear_by_person(
    tx: &Transaction<'_>,
    artist: &str,
    expected_revision: i64,
    user_id: &str,
    now_ms: i64,
) -> Result<ArtistLink, ArtistLinkError> {
    let (current, provable) = person_target(tx, artist, expected_revision)?;
    let Some(link) = current.link.filter(|link| link.source != LinkSource::App) else {
        return Err(ArtistLinkError::Refused(LinkRefusal::NotPersons));
    };
    let mut others: BTreeSet<(String, String)> = follows_out(tx, artist)?.into_iter().collect();
    others.extend(follows_in(tx, artist, None)?);
    if provable {
        for id in row_ids(tx, artist)? {
            if !placeholder(&id) {
                others.extend(sharing(tx, artist, &id)?);
            }
        }
    }
    if !others.is_empty() {
        return Err(ArtistLinkError::Refused(LinkRefusal::Shared(
            others.into_iter().take(10).collect(),
        )));
    }
    let now = now_ms as f64 / 1000.0;
    tx.prepare_cached(crate::identify::sql::CLEAR_BY_PERSON.text())?
        .execute(params![artist])?;
    touch_artist(tx, artist, now)?;
    audit_link(
        tx,
        user_id,
        artist,
        Some(&link.mbid),
        None,
        LINK_CLEARED_BY_PERSON,
        now,
    )?;
    bump_catalog(tx)?;
    settle(tx, &[artist.to_owned()], now_ms)?;
    artist_link(tx, artist)?.ok_or(ArtistLinkError::Refused(LinkRefusal::NotFound))
}
