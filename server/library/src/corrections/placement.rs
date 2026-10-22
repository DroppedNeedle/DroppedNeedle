//! A file a scan adds to an album with an edition is placed on the
//! edition's track by its own MusicBrainz ids.
//!
//! An album A is looked at when its match names a release R (any decision
//! source), it has no custom edition, no files queued to be placed, and no
//! match state but `CHOSEN_EDITION_FITS_NO_FILE`. A candidate is an
//! indexed row N of A whose identity names no release track. The edition's
//! tracks are R's stored document, or else the release tracks the album's
//! own identities name. The evidence is N's embedded ids only, never
//! names:
//!
//! - E0: N's release id, when set, answers to R, or nothing is placed;
//! - E1: N's release-track id is a track X of R. N is placed on X when its
//!   recording id is empty, equal to X's, or N's disc and track number are
//!   X's (MusicBrainz merges recordings); otherwise N is not placed;
//! - E2: otherwise, N's release id answers to R and exactly one track X of
//!   R sits at N's disc and track number with N's recording id.
//!
//! A track an indexed row of A holds is not taken, and a track two
//! candidates claim is given to neither. A placement writes the track
//! identity only, never a file; the run end's continuation then pairs the
//! missing row that held X with N.
//!
//! Placing a row is what lets automatic management write it. On an album
//! whose edition a person chose and that the app never wrote, nothing
//! would read a placed file as changed by hand, so A is marked taken over
//! in the same savepoint when a placed row N took the place of a missing
//! row O of A on its track and
//! 1. the edition was chosen before N was first indexed;
//! 2. O was replaced by N (the album join's test, [`judge`]), or N was
//!    indexed before O went missing and N's album title, album artist,
//!    disc or track number differ from O's (a copy retagged while the
//!    original was still there);
//! 3. neither N nor any missing row of A has a record of its bytes (where
//!    one has, the check of those bytes tells the hand edit).
//!
//! A pair the join's test says to wait on holds the album's placements
//! back to a later run end.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, Transaction, params};

use super::continuation::Pair;
use super::release_join::{Verdict, judge};
use crate::catalog_revision::bump_catalog;
use crate::identify::sqlite::{release_chosen, stored_release};
use crate::identity;
use crate::management::{automatic, takeover};
use crate::matching::Release;
use crate::publish::record::record_sql;
use droppedneedle_kernel::db::savepoint;

/// Indexed rows with an embedded release-track or recording id and no
/// identity naming a release track, on albums whose match names a release
/// and that have no custom edition, no queued placement and no match state
/// but a choice that fitted no file. A row whose embedded release-track id
/// an indexed row of the album already holds (a second copy) is left out.
/// Starts from the albums' identities and reads each album's indexed rows
/// by album.
const CANDIDATES: &str = "SELECT t.local_album_id, t.id, \
    lower(trim(COALESCE(t.embedded_release_track_mbid, ''))), \
    lower(trim(COALESCE(t.embedded_recording_mbid, ''))), \
    lower(trim(COALESCE(t.embedded_release_mbid, ''))), \
    t.disc_number, t.track_number, t.imported_at, \
    t.album_title_folded, COALESCE(t.album_artist_name_folded, '') \
    FROM local_album_external_identities ae \
    JOIN local_tracks t ON t.local_album_id = ae.local_album_id AND t.availability = 'indexed' \
    WHERE ae.provider = 'musicbrainz' AND trim(COALESCE(ae.release_mbid, '')) <> '' \
    AND (trim(COALESCE(t.embedded_release_track_mbid, '')) <> '' \
      OR trim(COALESCE(t.embedded_recording_mbid, '')) <> '') \
    AND NOT EXISTS (SELECT 1 FROM local_track_external_identities i \
      WHERE i.local_track_id = t.id AND i.provider = 'musicbrainz' \
      AND trim(COALESCE(i.release_track_mbid, '')) <> '') \
    AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
      WHERE c.local_album_id = ae.local_album_id) \
    AND NOT EXISTS (SELECT 1 FROM library_edition_remap_queue q \
      WHERE q.local_album_id = ae.local_album_id) \
    AND NOT EXISTS (SELECT 1 FROM library_album_match_state s \
      WHERE s.local_album_id = ae.local_album_id \
      AND s.reason_code IS NOT 'CHOSEN_EDITION_FITS_NO_FILE') \
    AND NOT EXISTS (SELECT 1 FROM local_tracks h \
      JOIN local_track_external_identities hi \
        ON hi.local_track_id = h.id AND hi.provider = 'musicbrainz' \
      WHERE h.local_album_id = t.local_album_id AND h.availability = 'indexed' \
      AND trim(COALESCE(t.embedded_release_track_mbid, '')) <> '' \
      AND lower(trim(hi.release_track_mbid)) = lower(trim(t.embedded_release_track_mbid))) \
    ORDER BY t.local_album_id, t.id";

/// One candidate row and what the scan read from its file.
struct Candidate {
    id: String,
    release_track: String,
    recording: String,
    release: String,
    disc: i64,
    track: i64,
    imported_at: Option<f64>,
    album_title: String,
    album_artist: String,
}

/// One track of the edition. Keys are lowercased; `id` and `recording`
/// keep their spelling for the write.
struct EditionTrack {
    key: String,
    id: String,
    recording: String,
    disc: Option<u32>,
    position: Option<u32>,
}

/// The edition an album is matched to, as far as the database knows it.
struct Edition {
    document: Option<Release>,
    release: String,
    tracks: Vec<EditionTrack>,
}

impl Edition {
    fn answers_to(&self, mbid: &str) -> bool {
        match &self.document {
            Some(document) => document.answers_to(mbid),
            None => self.release.trim().eq_ignore_ascii_case(mbid),
        }
    }

    fn by_key(&self, key: &str) -> Option<&EditionTrack> {
        self.tracks.iter().find(|track| track.key == key)
    }

    /// The track the candidate's own ids name, if they name one.
    fn evidence(&self, row: &Candidate) -> Option<&EditionTrack> {
        if !row.release.is_empty() && !self.answers_to(&row.release) {
            return None;
        }
        let disc = u32::try_from(row.disc.max(1)).ok();
        let position = u32::try_from(row.track).ok().filter(|&n| n > 0);
        if let Some(track) = self.by_key(&row.release_track) {
            let same_place = disc.is_some()
                && position.is_some()
                && track.disc == disc
                && track.position == position;
            let same_recording =
                row.recording.is_empty() || row.recording == track.recording.to_ascii_lowercase();
            return (same_recording || same_place).then_some(track);
        }
        if row.release.is_empty() || row.recording.is_empty() || position.is_none() {
            return None;
        }
        let mut there = self
            .tracks
            .iter()
            .filter(|track| track.disc == disc && track.position == position);
        match (there.next(), there.next()) {
            (Some(track), None) if track.recording.eq_ignore_ascii_case(&row.recording) => {
                Some(track)
            }
            _ => None,
        }
    }
}

/// Place every file a scan filed onto an album with an edition and that
/// its own ids put on one of the edition's tracks, each album in a
/// savepoint of the caller's transaction. A store error in one album rolls
/// that album back with one warning; the next run end tries again. Call it
/// after the run end's joins and before the continuation, which then
/// carries what people made onto the placed rows.
pub(crate) fn place_scanned(tx: &Transaction<'_>, now: f64) -> rusqlite::Result<()> {
    let mut albums: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    {
        let mut statement = tx.prepare_cached(CANDIDATES)?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Candidate {
                    id: row.get(1)?,
                    release_track: row.get(2)?,
                    recording: row.get(3)?,
                    release: row.get(4)?,
                    disc: row.get(5)?,
                    track: row.get(6)?,
                    imported_at: row.get(7)?,
                    album_title: row.get(8)?,
                    album_artist: row.get(9)?,
                },
            ))
        })?;
        for row in rows {
            let (album, candidate) = row?;
            albums.entry(album).or_default().push(candidate);
        }
    }
    let mut placed_any = false;
    for (album, candidates) in &albums {
        match savepoint(tx, "place_scanned", || {
            place_album(tx, album, candidates, now)
        })? {
            Ok(placed) => placed_any |= placed,
            Err(error) => {
                tracing::warn!(
                    %error,
                    album_id = album.as_str(),
                    "scanned files not placed on the album's edition; the next run end tries again"
                );
            }
        }
    }
    if placed_any {
        bump_catalog(tx)?;
    }
    Ok(())
}

/// Place one album's candidates; whether anything was placed.
fn place_album(
    tx: &Transaction<'_>,
    album: &str,
    candidates: &[Candidate],
    now: f64,
) -> rusqlite::Result<bool> {
    let (release, chosen_at): (String, f64) = tx
        .prepare_cached(
            "SELECT release_mbid, selected_at FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = 'musicbrainz'",
        )?
        .query_row(params![album], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let edition = edition(tx, album, &release)?;
    let held = held_tracks(tx, album)?;

    let mut claims: BTreeMap<&str, Vec<&Candidate>> = BTreeMap::new();
    let mut held_count = 0usize;
    for row in candidates {
        let Some(track) = edition.evidence(row) else {
            continue;
        };
        if held.contains(&track.key) {
            held_count += 1;
            continue;
        }
        claims.entry(track.key.as_str()).or_default().push(row);
    }
    let claimed_twice = claims.values().filter(|rows| rows.len() > 1).count();
    let placements: Vec<(&EditionTrack, &Candidate)> = claims
        .iter()
        .filter_map(|(key, rows)| match rows.as_slice() {
            [only] => edition.by_key(key).map(|track| (track, *only)),
            _ => None,
        })
        .collect();
    if placements.is_empty() {
        return Ok(false);
    }

    let mut mark = false;
    if release_chosen(tx, album)? && !missing_rows_have_record(tx, album)? {
        for (track, row) in &placements {
            match retagged_by_hand(tx, album, track, row, chosen_at, now)? {
                Retag::Yes => mark = true,
                Retag::No => {}
                Retag::Wait => return Ok(false),
            }
        }
    }

    if mark {
        takeover::mark(tx, album, takeover::Cause::RetagFollowed, &[], None, now)?;
    }
    let spots: Vec<identity::Spot<'_>> = placements
        .iter()
        .map(|(track, row)| identity::Spot {
            local_track_id: &row.id,
            recording_mbid: &track.recording,
            release_track_mbid: &track.id,
            disc: track.disc,
            position: track.position,
        })
        .collect();
    identity::place_scanned(tx, album, &release, &spots, now)?;
    if !mark {
        automatic::queue(tx, album, now)?;
    }
    tracing::info!(
        album_id = album,
        placed = placements.len(),
        held = held_count,
        claimed_twice,
        marked = mark,
        "scanned files placed on the album's edition by their own ids"
    );
    Ok(true)
}

/// The edition's tracks: the stored release document, or else the release
/// tracks the album's own identities name (naming R or no release). A
/// release track two of them give different recordings is left out.
fn edition(tx: &Connection, album: &str, release: &str) -> rusqlite::Result<Edition> {
    if let Some(document) = stored_release(tx, release, 0.0)? {
        let tracks = document
            .tracks
            .iter()
            .map(|track| EditionTrack {
                key: track.id.trim().to_ascii_lowercase(),
                id: track.id.clone(),
                recording: track.recording_id.clone(),
                disc: Some(track.disc),
                position: Some(track.position),
            })
            .collect();
        return Ok(Edition {
            document: Some(document),
            release: release.to_owned(),
            tracks,
        });
    }
    let mut tracks: Vec<EditionTrack> = Vec::new();
    let mut conflicting = BTreeSet::new();
    let mut statement = tx.prepare_cached(
        "SELECT i.release_track_mbid, i.recording_mbid, i.medium_position, \
         i.release_track_position FROM local_tracks t \
         JOIN local_track_external_identities i \
           ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
         WHERE t.local_album_id = ?1 AND trim(COALESCE(i.release_track_mbid, '')) <> '' \
         AND (trim(COALESCE(i.release_mbid, '')) = '' \
           OR lower(trim(i.release_mbid)) = lower(trim(?2))) \
         ORDER BY t.id",
    )?;
    let rows = statement.query_map(params![album, release], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<u32>>(2)?,
            row.get::<_, Option<u32>>(3)?,
        ))
    })?;
    for row in rows {
        let (id, recording, disc, position) = row?;
        let key = id.trim().to_ascii_lowercase();
        match tracks.iter_mut().find(|track| track.key == key) {
            Some(known) => {
                if !known.recording.eq_ignore_ascii_case(recording.trim()) {
                    conflicting.insert(key);
                }
                if known.disc.is_none() || known.position.is_none() {
                    known.disc = known.disc.or(disc);
                    known.position = known.position.or(position);
                }
            }
            None => tracks.push(EditionTrack {
                key,
                id: id.trim().to_owned(),
                recording: recording.trim().to_owned(),
                disc,
                position,
            }),
        }
    }
    tracks.retain(|track| !conflicting.contains(&track.key));
    Ok(Edition {
        document: None,
        release: release.to_owned(),
        tracks,
    })
}

/// The release tracks indexed rows of the album hold, lowercased.
fn held_tracks(tx: &Connection, album: &str) -> rusqlite::Result<BTreeSet<String>> {
    tx.prepare_cached(
        "SELECT lower(trim(i.release_track_mbid)) FROM local_tracks t \
         JOIN local_track_external_identities i \
           ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
         WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
         AND trim(COALESCE(i.release_track_mbid, '')) <> ''",
    )?
    .query_map(params![album], |row| row.get(0))?
    .collect()
}

/// Whether a missing row of the album has a record of its bytes.
fn missing_rows_have_record(tx: &Connection, album: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(&format!(
        "SELECT EXISTS(SELECT 1 FROM local_tracks t \
         JOIN library_track_management_state s ON s.local_track_id = t.id \
         WHERE t.local_album_id = ?1 AND t.availability = 'missing' AND {})",
        record_sql()
    ))?
    .query_row(params![album], |row| row.get(0))
}

/// Whether one placement is a person's retag the app must not write over.
enum Retag {
    Yes,
    No,
    /// The join's test waits a scan: so does the album.
    Wait,
}

/// Rule 1 to 3 of the module doc for a row placed on `track`, on an album
/// whose edition a person chose at `chosen_at` and whose missing rows have
/// no record.
fn retagged_by_hand(
    tx: &Transaction<'_>,
    album: &str,
    track: &EditionTrack,
    row: &Candidate,
    chosen_at: f64,
    now: f64,
) -> rusqlite::Result<Retag> {
    let Some(arrived) = row.imported_at else {
        return Ok(Retag::No);
    };
    if chosen_at >= arrived {
        return Ok(Retag::No);
    }
    let has_record: bool = tx
        .prepare_cached(&format!(
            "SELECT EXISTS(SELECT 1 FROM library_track_management_state s \
             WHERE s.local_track_id = ?1 AND {})",
            record_sql()
        ))?
        .query_row(params![row.id], |r| r.get(0))?;
    if has_record {
        return Ok(Retag::No);
    }
    let olds: Vec<(String, Option<f64>, String, String, i64, i64)> = tx
        .prepare_cached(
            "SELECT t.id, t.missing_since, t.album_title_folded, \
             COALESCE(t.album_artist_name_folded, ''), t.disc_number, t.track_number \
             FROM local_tracks t JOIN local_track_external_identities i \
               ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
             WHERE t.local_album_id = ?1 AND t.availability = 'missing' \
             AND lower(trim(i.release_track_mbid)) = ?2 ORDER BY t.id",
        )?
        .query_map(params![album, track.key], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut waits = false;
    for (old, missing_since, title, artist, disc, number) in olds {
        let retitled_copy = missing_since.is_some_and(|gone| arrived < gone)
            && (title != row.album_title
                || artist != row.album_artist
                || disc != row.disc
                || number != row.track);
        if retitled_copy {
            return Ok(Retag::Yes);
        }
        let pair = Pair {
            old,
            old_album: album.to_owned(),
            new: row.id.clone(),
            new_album: album.to_owned(),
        };
        match judge(tx, &pair, now)? {
            Verdict::Replaced => return Ok(Retag::Yes),
            Verdict::Wait => waits = true,
            Verdict::Stayed | Verdict::Veto => {}
        }
    }
    Ok(if waits { Retag::Wait } else { Retag::No })
}
