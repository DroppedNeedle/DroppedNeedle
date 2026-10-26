//! The index-window commit: catalog rows built from tags, the inventory
//! marks for every row the window covered, the run counters, and the
//! identify offers for the albums it touched, all in one transaction. A
//! crash either lands the whole window or none of it, so a resumed run
//! picks up at the first unprocessed row with nothing counted twice.
//!
//! Catalog rows follow v2's indexer: display names come from tags, then
//! the path parse, then placeholders, each with its provenance; artists
//! key by name and sort name.
//!
//! Ids survive moves (the Navidrome approach). An album carries a
//! persistent key (release MBID, else the tagged album artist and title),
//! so when its files move or are organized the album row follows them
//! with its identity and reviews. Copies of one album in different
//! folders still stay separate albums, as in v2. A file that appears where a
//! track's file just went away continues that track (same album names,
//! disc and track number, plus the same recording MBID or the same title
//! and duration), so
//! favorites, history and playlists follow a moved file. When every track
//! of an album is retagged into a new album in one run, the old row takes
//! the new name instead of being left behind. When the retag is seen by
//! more than one run, the end of the run that sees the rest merges the old
//! row into the new one. Other music at a track's path (a length more than
//! five seconds off, and a recording id or tagged title that differs) is a
//! new track: the old row stays on its album, missing, with its history.
//! A new FLAC copy of the same recording at a track's path (same album,
//! disc and track, and another sample rate, bit depth, channel count or a
//! length more than a second off) makes the app forget its record of the
//! old file, so the copy is managed as a file the app never wrote.

use std::collections::BTreeMap;

use super::artists::{self, Credit, Own, upsert_artist};
use super::hands_off::hold_replacements;
use super::held_scopes::{self, held_back};
use super::retag_pairs::{self, RetagPairReason, RetagPairState};
use super::*;
use crate::catalog_revision::bump_catalog;
use crate::corrections::artist_convergence::{converge_artists, record_move};
use crate::corrections::continuation::{Scope, continue_tracks};
use crate::corrections::placement::place_scanned;
use crate::corrections::release_join::join_replaced;
use crate::corrections::retag_merge::{JoinCause, MergeOutcome, album_has_record, merge_retagged};
use crate::identify::artist_links::{mark_stale, placeholder};
use crate::identify::queue::offer_again;
use crate::identify::sqlite::{
    chosen_matches_differ, match_chosen, offer_album, release_chosen, retarget_references,
    settle_followed_match,
};
use crate::management::{automatic, takeover};
use crate::matching::model::EditionHints;
use crate::operations::decisions::{CatalogAction, record_action};
use crate::publish::HIDDEN_PREFIX;
use crate::scan::naming::{
    album_grouping_key, album_name_key, grouping_directory, parse_names_for_row,
};
use crate::scan::store::{EditionFill, IndexWindow, WindowOutcome};
use crate::tags::{AudioArtistCredit, AudioTag};
use droppedneedle_kernel::db::savepoint;

pub(super) use crate::scan::naming::{UNKNOWN_ARTIST, UNKNOWN_ARTIST_ID, stable_id};

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// How one file names its album.
struct AlbumNames<'a> {
    release_mbid: Option<&'a str>,
    /// Both the album title and the album artist came from tags.
    from_tags: bool,
    directory: &'a str,
    title_folded: String,
    artist_folded: String,
}

impl AlbumNames<'_> {
    /// The album's persistent key (see [`album_grouping_key`]).
    fn key(&self) -> String {
        album_grouping_key(
            self.release_mbid,
            self.from_tags,
            self.directory,
            &self.title_folded,
            &self.artist_folded,
        )
    }

    fn name_key(&self) -> String {
        album_name_key(
            self.from_tags,
            self.directory,
            &self.title_folded,
            &self.artist_folded,
        )
    }
}

/// Where a track's group lives: its root and grouping directory. Without
/// a run (an import adding files), nothing counts as walked this run.
struct Place<'a> {
    run_id: Option<&'a str>,
    root_id: &'a str,
    directory: &'a str,
}

/// Journal row `j` keeps scope `s` from judging its track: it is still
/// open, or it moved after the scope's walk began. The states listed are
/// the publisher's terminal ones (`JournalState::is_terminal`).
macro_rules! fenced_by {
    () => {
        "(j.state NOT IN ('cleaned', 'compensated', 'needs_attention') \
          OR j.fence_revision > s.walk_revision)"
    };
}
pub(super) use fenced_by;

/// The one rule for "this run walked past the track" (aliased `t`; the
/// run id is the parameter `$run`): the walk did not see the file, and a
/// scope of the run covering its path finished its walk with a recorded
/// walk revision, while no library write naming the track is still open
/// or moved after that walk began. A write names a track by its journal
/// row's `track_id`, or, for a removal or a replaced file, by the row's
/// replacement. The journal probe runs last: it is the rarest to match.
macro_rules! walked_past {
    ($run:literal) => {
        concat!(
            "(NOT EXISTS (SELECT 1 FROM library_scan_inventory i WHERE i.run_id = ",
            $run,
            " AND i.root_id = t.root_id AND i.relative_path = t.relative_path) \
             AND EXISTS (SELECT 1 FROM library_scan_run_scopes s WHERE s.run_id = ",
            $run,
            " AND s.root_id = t.root_id AND s.discovery_state = 'completed' \
              AND s.walk_revision IS NOT NULL \
              AND (s.relative_path = '.' OR t.relative_path = s.relative_path \
                OR substr(t.relative_path, 1, length(s.relative_path) + 1) = s.relative_path || '/') \
              AND NOT EXISTS (SELECT 1 FROM library_publish_journal j \
                WHERE j.track_id = t.id AND ",
            $crate::scan::sqlite_store::commit::fenced_by!(),
            ") AND NOT EXISTS (SELECT 1 FROM library_publish_replacements r \
                JOIN library_publish_journal j ON j.id = r.journal_id \
                WHERE r.replaced_track_id = t.id AND ",
            $crate::scan::sqlite_store::commit::fenced_by!(),
            ")))"
        )
    };
}
pub(super) use walked_past;

/// A track still counts as present unless it is missing or this run
/// walked its folder to the end without seeing it, and did not hold back a
/// folder covering it.
const TRACK_PRESENT: &str = concat!(
    "(t.availability = 'indexed' AND NOT (",
    walked_past!("?1"),
    " AND NOT ",
    held_back!("?1", "t"),
    "))"
);

/// Where an album's tracks are, seen from one folder.
#[derive(Default)]
struct Presence {
    /// Some present tracks are in this folder.
    here: bool,
    /// Some present tracks are anywhere else.
    elsewhere: bool,
    /// Some tracks this run found gone (not ones missing since earlier).
    gone_this_run: bool,
}

fn album_presence(
    tx: &Connection,
    place: &Place<'_>,
    album_id: &str,
) -> rusqlite::Result<Presence> {
    let mut stmt = tx.prepare_cached(&format!(
        "SELECT t.root_id, t.relative_path, {TRACK_PRESENT} FROM local_tracks t \
         WHERE t.local_album_id = ?2 AND t.availability = 'indexed'"
    ))?;
    let mut rows = stmt.query(params![place.run_id, album_id])?;
    let mut presence = Presence::default();
    while let Some(row) = rows.next()? {
        let root: String = row.get(0)?;
        let path: String = row.get(1)?;
        let present: bool = row.get(2)?;
        if !present {
            presence.gone_this_run = true;
        } else if root == place.root_id && grouping_directory(&path) == place.directory {
            presence.here = true;
        } else {
            presence.elsewhere = true;
        }
    }
    Ok(presence)
}

/// The album a track files under. Copies of one album in different
/// folders stay different albums (duplicate resolution depends on it),
/// so the persistent key only carries an album along when it moved or
/// was retagged:
///
/// 1. the album the track was in, while it still answers to the key and
///    has no present tracks outside this folder;
/// 2. an album with the key that already has present tracks in this
///    folder (for tagged names, also a same-named album where only some
///    files carry the release MBID, so a partly tagged album stays
///    whole);
/// 3. an album with the key that has no present tracks anywhere and
///    whose files this run found gone, so they moved here (the oldest
///    first);
/// 4. else a new album.
///
/// A track whose grouping came from an earlier DroppedNeedle version
/// (`legacy`: v2 grouped it, and v3 has not moved it since) first stays in
/// the album it was in while the file still names that album, wherever the
/// album's other tracks sit: v2 grouped differently in places, and its
/// album ids must not change under the people using them.
fn resolve_album(
    tx: &Connection,
    names: &AlbumNames<'_>,
    place: &Place<'_>,
    previous: Option<&str>,
    legacy: bool,
) -> rusqlite::Result<String> {
    if legacy && let Some(previous) = previous {
        let named: bool = tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 \
                 AND retired_into_album_id IS NULL AND title_folded = ?2 \
                 AND COALESCE(album_artist_name_folded, '') = ?3)",
            )?
            .query_row(
                params![previous, names.title_folded, names.artist_folded],
                |row| row.get(0),
            )?;
        if named {
            return Ok(previous.to_owned());
        }
    }
    let key = names.key();
    if let Some(previous) = previous {
        let same: bool = tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 AND grouping_key = ?2)",
            )?
            .query_row(params![previous, key], |row| row.get(0))?;
        if same && !album_presence(tx, place, previous)?.elsewhere {
            return Ok(previous.to_owned());
        }
    }
    // Tagged names: with an MBID, a same-named album nobody tagged yet;
    // without one, a same-named album known by its MBID. Same folder only.
    let same_folder_only = names.from_tags.then(|| {
        if names.release_mbid.is_some() {
            names.name_key()
        } else {
            "mbid:".to_owned()
        }
    });
    let candidates: Vec<(String, bool)> = {
        let mut stmt = tx.prepare_cached(
            "SELECT id, grouping_key = ?1 FROM local_albums \
             WHERE retired_into_album_id IS NULL AND (grouping_key = ?1 \
             OR (?2 IS NOT NULL AND title_folded = ?3 AND album_artist_name_folded = ?4 \
               AND (grouping_key = ?2 OR (?2 = 'mbid:' AND grouping_key LIKE 'mbid:%')))) \
             ORDER BY created_at, id",
        )?;
        stmt.query_map(
            params![
                key,
                same_folder_only,
                names.title_folded,
                names.artist_folded
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<rusqlite::Result<_>>()?
    };
    let mut takeover: Option<String> = None;
    for (id, exact) in candidates {
        let presence = album_presence(tx, place, &id)?;
        if presence.here {
            return Ok(id);
        }
        // An album missing since an earlier run may be a different
        // release that happens to share the names; only one this run
        // emptied is taken over.
        if exact && !presence.elsewhere && presence.gone_this_run && takeover.is_none() {
            takeover = Some(id);
        }
    }
    if let Some(id) = takeover {
        return Ok(id);
    }
    for seed in [
        format!("album:{key}"),
        format!("album:{key}:{}:{}", place.root_id, place.directory),
    ] {
        let stable = stable_id(&seed);
        let taken: bool = tx
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1)")?
            .query_row(params![stable], |row| row.get(0))?;
        if !taken {
            return Ok(stable);
        }
    }
    Ok(uuid::Uuid::new_v4().to_string())
}

/// What a newly seen file is matched on against tracks whose files went
/// away.
struct TrackKey<'a> {
    recording_mbid: Option<&'a str>,
    album_title_folded: &'a str,
    album_artist_folded: &'a str,
    title_folded: &'a str,
    disc_number: i64,
    track_number: i64,
    duration_seconds: Option<f64>,
}

/// A track this run walked past, whether or not the mass-missing guard
/// held its scope back.
pub(super) const WALKED_PAST: &str = walked_past!("?1");

/// A track is gone when it is already marked missing, or when this run
/// walked its folder to the end without seeing the file. A folder the
/// mass-missing guard held back (an unmounted share looks like that), a
/// scope or a mount found empty, does not count as walked.
pub(super) const TRACK_GONE: &str = concat!(
    "(t.availability = 'missing' OR (t.availability = 'indexed' AND ",
    walked_past!("?1"),
    " AND NOT ",
    held_back!("?1", "t"),
    "))"
);

/// Durations closer than this are the same recording.
pub(super) const DURATION_TOLERANCE_SECONDS: f64 = 1.0;

/// A file at a track's path whose length moved by more than this may be
/// other music. A retag or re-encode moves it by less than the tolerance
/// above; a re-rip by the gap between tracks the ripper put on one side or
/// the other, 2 seconds on most discs.
const OTHER_MUSIC_SECONDS: f64 = 5.0;

/// Audit kind of the tracks whose paths other music took.
const OTHER_MUSIC_KIND: &str = "other_music_at_path";

/// Audit kind of the tracks whose files were replaced by new copies of
/// the same recordings.
const NEW_COPY_KIND: &str = "new_copy_at_path";

/// The row at a file's path, as read before the file is written.
struct AtPath {
    id: String,
    album_id: String,
    availability: String,
    duration_seconds: Option<f64>,
    recording_mbid: Option<String>,
    title_folded: String,
    title_provenance: String,
    disc_number: i64,
    track_number: i64,
    sample_rate: Option<i64>,
    bit_depth: Option<i64>,
    channels: Option<i64>,
}

/// A file's length and stream properties, as stored or as read.
#[derive(Clone, Copy)]
struct Sound {
    seconds: Option<f64>,
    sample_rate: Option<i64>,
    bit_depth: Option<i64>,
    channels: Option<i64>,
}

impl Sound {
    fn of_row(row: &AtPath) -> Self {
        Self {
            seconds: row.duration_seconds,
            sample_rate: row.sample_rate,
            bit_depth: row.bit_depth,
            channels: row.channels,
        }
    }

    fn of_file(item: &CommitIndexedItem) -> Self {
        let header = &item.tags.header;
        Self {
            seconds: header.duration_seconds,
            sample_rate: header.sample_rate.map(i64::from),
            bit_depth: header.bit_depth.map(i64::from),
            channels: header.channels.map(i64::from),
        }
    }

    fn json(self) -> serde_json::Value {
        serde_json::json!({
            "seconds": self.seconds,
            "sample_rate": self.sample_rate,
            "bit_depth": self.bit_depth,
            "channels": self.channels,
        })
    }
}

/// A track whose file was replaced by a new copy of its recording.
struct NewCopy {
    track_id: String,
    album_id: String,
    before: Sound,
    after: Sound,
}

/// A track whose path now holds other music, and the track that took it.
struct OtherMusic {
    old_track_id: String,
    old_album_id: String,
    new_track_id: String,
    old_seconds: Option<f64>,
    new_seconds: Option<f64>,
}

/// What writing one file did.
struct Written {
    track_id: String,
    album_id: String,
    /// The album the track left, if it changed albums.
    moved_from: Option<String>,
    /// The track whose path the file took, when it is other music.
    other_music: Option<OtherMusic>,
    /// The track whose record the scan forgot, when the file is a new
    /// copy of its recording.
    new_copy: Option<NewCopy>,
}

/// Whether the file now at `row`'s path is other music, not the track the
/// row describes: the row is indexed or missing, both lengths are known and
/// differ by more than [`OTHER_MUSIC_SECONDS`], and the file contradicts the
/// row. It does when both carry a recording id and the ids differ, or, when
/// either has none, both titles came from tags and they differ. A title
/// read from a path or a file name is never a contradiction, and nothing
/// else counts: not album names, artists, numbers, format or a match.
fn other_music(row: &AtPath, item: &CommitIndexedItem) -> bool {
    if row.availability != "indexed" && row.availability != "missing" {
        return false;
    }
    let lengths_apart = match (row.duration_seconds, item.tags.header.duration_seconds) {
        (Some(old), Some(new)) if old > 0.0 && new > 0.0 => (old - new).abs() > OTHER_MUSIC_SECONDS,
        _ => false,
    };
    if !lengths_apart {
        return false;
    }
    let recording = |mbid: Option<&str>| {
        mbid.map(|mbid| mbid.trim().to_lowercase())
            .filter(|mbid| !mbid.is_empty())
    };
    let tag = &item.tags.tag;
    match (
        recording(row.recording_mbid.as_deref()),
        recording(tag.musicbrainz_recording_id.as_deref()),
    ) {
        (Some(old), Some(new)) => old != new,
        _ => {
            let title = tag.title.trim();
            row.title_provenance == "tag"
                && !title.is_empty()
                && row.title_folded != fold_text(title)
        }
    }
}

/// Whether the file a scan just wrote over `row` is a new copy of the
/// row's recording, not a hand edit: the row was indexed or missing, the
/// file is a FLAC, it stayed on the row's album with the row's disc and
/// track number, it is the same recording, and its audio changed. The same
/// recording is the same recording id on both sides or, when either has
/// none, the same title read from both files' tags with lengths within
/// [`OTHER_MUSIC_SECONDS`] (other music's contradiction turned around).
/// The audio changed when the sample rate, bit depth or channel count is
/// known on both sides and differs, or the length moved by more than
/// [`DURATION_TOLERANCE_SECONDS`].
///
/// FLAC only: its length and stream properties come from STREAMINFO,
/// which every reader reads alike and no tag write touches. Verified
/// 2026-10-17 on lofty 0.25.4: the scan's reader gave 44100 Hz, 16 bits,
/// 2 channels and 0.3 s for copies of `management_full.flac` and
/// `flac_full_01.flac`, and the same four values after `save_tags` wrote a
/// title and a 783 KB front cover.
fn new_copy(
    row: &AtPath,
    item: &CommitIndexedItem,
    stored_album: &str,
    disc_number: i64,
    track_number: i64,
) -> bool {
    if row.availability != "indexed" && row.availability != "missing" {
        return false;
    }
    let flac = matches!(
        crate::tags::format_for_path(std::path::Path::new(&item.relative_path)),
        Ok(crate::tags::AudioFormat::Flac)
    );
    if !flac
        || stored_album != row.album_id
        || disc_number != row.disc_number
        || track_number != row.track_number
    {
        return false;
    }
    let (old, new) = (Sound::of_row(row), Sound::of_file(item));
    let lengths = match (old.seconds, new.seconds) {
        (Some(old), Some(new)) if old > 0.0 && new > 0.0 => Some((old - new).abs()),
        _ => None,
    };
    let recording = |mbid: Option<&str>| {
        mbid.map(|mbid| mbid.trim().to_lowercase())
            .filter(|mbid| !mbid.is_empty())
    };
    let tag = &item.tags.tag;
    let same_recording = match (
        recording(row.recording_mbid.as_deref()),
        recording(tag.musicbrainz_recording_id.as_deref()),
    ) {
        (Some(old), Some(new)) => old == new,
        _ => {
            let title = tag.title.trim();
            row.title_provenance == "tag"
                && !title.is_empty()
                && row.title_folded == fold_text(title)
                && lengths.is_some_and(|apart| apart <= OTHER_MUSIC_SECONDS)
        }
    };
    if !same_recording {
        return false;
    }
    let differs = |old: Option<i64>, new: Option<i64>| match (old, new) {
        (Some(old), Some(new)) if old > 0 && new > 0 => old != new,
        _ => false,
    };
    differs(old.sample_rate, new.sample_rate)
        || differs(old.bit_depth, new.bit_depth)
        || differs(old.channels, new.channels)
        || lengths.is_some_and(|apart| apart > DURATION_TOLERANCE_SECONDS)
}

/// Retire the track whose path now holds other music. It is missing from
/// the file's read time, or keeps the time it went missing, and gives its
/// path up for the hidden name the publisher gives a row whose file is
/// gone (`gone/{id}/{path}`), so the file can take the real one. Its album,
/// matches, history and management record stay on it.
fn give_up_path(tx: &Connection, track_id: &str, item: &CommitIndexedItem) -> rusqlite::Result<()> {
    let hidden = format!("{HIDDEN_PREFIX}gone/{track_id}/{}", item.relative_path);
    let depth = std::path::Path::new(&item.relative_path)
        .components()
        .count();
    let root_dir = std::path::Path::new(&item.absolute_path)
        .ancestors()
        .nth(depth)
        .ok_or_else(|| rusqlite::Error::InvalidPath(item.absolute_path.clone().into()))?;
    let file_path = root_dir.join(&hidden).to_string_lossy().into_owned();
    tx.prepare_cached(
        "UPDATE local_tracks SET availability = 'missing', \
         missing_since = CASE WHEN availability = 'missing' THEN missing_since ELSE ?2 END, \
         relative_path = ?3, file_path = ?4, path_hash = ?5, row_revision = row_revision + 1 \
         WHERE id = ?1",
    )?
    .execute(params![
        track_id,
        item.tags_read_at,
        hidden,
        file_path,
        sha256_hex(&hidden)
    ])?;
    Ok(())
}

/// The gone track a newly seen file continues, as (track id, album id):
/// the same album names, disc and track number, plus the same recording
/// MBID, or else the same title and duration. Only an unambiguous match
/// counts.
fn find_moved_track(
    tx: &Connection,
    run_id: &str,
    key: &TrackKey<'_>,
) -> rusqlite::Result<Option<(String, String)>> {
    struct Candidate {
        id: String,
        album_id: String,
        same_place: bool,
    }
    let columns = "SELECT t.id, t.local_album_id, \
         (t.album_title_folded = ?3 AND COALESCE(t.album_artist_name_folded, '') = ?4 \
          AND t.disc_number = ?5 AND t.track_number = ?6), t.duration_seconds \
         FROM local_tracks t";
    let sql = match key.recording_mbid {
        Some(_) => format!("{columns} WHERE t.embedded_recording_mbid = ?2 AND {TRACK_GONE}"),
        None => format!(
            "{columns} WHERE t.title_folded = ?2 AND t.album_title_folded = ?3 \
             AND COALESCE(t.album_artist_name_folded, '') = ?4 AND t.disc_number = ?5 \
             AND t.track_number = ?6 AND {TRACK_GONE}"
        ),
    };
    let first = key.recording_mbid.unwrap_or(key.title_folded);
    let mut candidates = Vec::new();
    let mut stmt = tx.prepare_cached(&sql)?;
    let mut rows = stmt.query(params![
        run_id,
        first,
        key.album_title_folded,
        key.album_artist_folded,
        key.disc_number,
        key.track_number
    ])?;
    while let Some(row) = rows.next()? {
        let duration: Option<f64> = row.get(3)?;
        let same_length = match (duration, key.duration_seconds) {
            (Some(old), Some(new)) => (old - new).abs() < DURATION_TOLERANCE_SECONDS,
            (None, None) => true,
            _ => false,
        };
        if key.recording_mbid.is_none() && !same_length {
            continue;
        }
        candidates.push(Candidate {
            id: row.get(0)?,
            album_id: row.get(1)?,
            same_place: row.get(2)?,
        });
    }
    // The same recording on another album (or disc, or track number) is
    // another release's track: its release-specific identity must not
    // follow, so it never matches.
    candidates.retain(|candidate| candidate.same_place);
    Ok(match candidates.as_slice() {
        [only] => Some((only.id.clone(), only.album_id.clone())),
        _ => None,
    })
}

/// The album facts one file gives, beyond its credits.
struct AlbumRow<'a> {
    grouping_key: &'a str,
    album_title: &'a str,
    album_artist: &'a str,
    raw_album: &'a str,
    year: Option<i32>,
    now: f64,
}

/// Which side of a file's credits [`credit_rows`] reads.
#[derive(Clone, Copy)]
enum CreditSide {
    Album,
    Track,
}

/// Each credit of an album or a track as its artist links read it: the
/// position, the artist row, the name it is credited as and the id its
/// file holds. When they change, the album's artist links are worked out
/// again ([`mark_stale`]): its proofs name the rows the credits pointed at
/// and rest on those names and ids.
type CreditRow = (i64, String, Option<String>, Option<String>);

fn credit_rows(
    tx: &Connection,
    side: CreditSide,
    subject: &str,
) -> rusqlite::Result<Vec<CreditRow>> {
    let sql = match side {
        CreditSide::Album => {
            "SELECT position, local_artist_id, credited_name, artist_mbid \
             FROM local_album_artists WHERE local_album_id = ?1 ORDER BY position"
        }
        CreditSide::Track => {
            "SELECT position, local_artist_id, credited_name, artist_mbid \
             FROM local_track_artists WHERE local_track_id = ?1 ORDER BY position"
        }
    };
    tx.prepare_cached(sql)?
        .query_map(params![subject], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect()
}

/// Record each credit of `subject` that moved to another artist row
/// ([`record_move`]): the same position on another row, the old row no
/// longer on the subject at all. Lists of different lengths record
/// nothing, so a credit dropped or split in two is never read as a move.
/// A move involves an id when the file held one before or after, or a
/// proof sat at the old credit.
fn record_moves(
    tx: &Connection,
    side: CreditSide,
    subject: &str,
    before: &[CreditRow],
    after: &[CreditRow],
    now: f64,
) -> rusqlite::Result<()> {
    if before.len() != after.len() {
        return Ok(());
    }
    let kind = match side {
        CreditSide::Album => "album",
        CreditSide::Track => "track",
    };
    for ((position, from, _, from_mbid), (to_position, to, _, to_mbid)) in before.iter().zip(after)
    {
        if position != to_position || from == to || after.iter().any(|row| &row.1 == from) {
            continue;
        }
        let with_id = from_mbid.is_some()
            || to_mbid.is_some()
            || tx
                .prepare_cached(
                    "SELECT EXISTS (SELECT 1 FROM library_artist_credit_proofs \
                     WHERE subject_kind = ?1 AND subject_id = ?2 AND credit_position = ?3 \
                     AND source_local_artist_id = ?4)",
                )?
                .query_row(params![kind, subject, position, from], |row| row.get(0))?;
        record_move(tx, from, to, with_id, now)?;
    }
    Ok(())
}

/// Insert or refresh the album row and its artist credits from one file.
fn write_album(
    tx: &Connection,
    album_id: &str,
    item: &CommitIndexedItem,
    album_credits: &[Credit],
    album_artist_ids: &[String],
    row: &AlbumRow<'_>,
) -> rusqlite::Result<()> {
    let tag = &item.tags.tag;
    let (grouping_key, album_title, album_artist, raw_album, year, now) = (
        row.grouping_key,
        row.album_title,
        row.album_artist,
        row.raw_album,
        row.year,
        row.now,
    );
    let primary_artist = album_artist_ids
        .first()
        .cloned()
        .unwrap_or_else(|| UNKNOWN_ARTIST_ID.to_owned());
    tx.prepare_cached(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, tag_album_title, tag_album_artist_name, \
         album_artist_id, album_artist_sort_name, year, original_release_date, primary_genre, \
         is_compilation, grouping_source, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
         'automatic', ?16, ?16) \
         ON CONFLICT (id) DO UPDATE SET root_id = excluded.root_id, title = excluded.title, \
         title_folded = excluded.title_folded, album_artist_name = excluded.album_artist_name, \
         album_artist_name_folded = excluded.album_artist_name_folded, \
         tag_album_title = excluded.tag_album_title, \
         tag_album_artist_name = excluded.tag_album_artist_name, \
         album_artist_id = excluded.album_artist_id, \
         album_artist_sort_name = excluded.album_artist_sort_name, \
         year = COALESCE(excluded.year, local_albums.year), \
         original_release_date = COALESCE(excluded.original_release_date, \
         local_albums.original_release_date), \
         primary_genre = COALESCE(excluded.primary_genre, local_albums.primary_genre), \
         is_compilation = excluded.is_compilation, updated_at = excluded.updated_at",
    )?
    .execute(params![
        album_id,
        item.root_id,
        grouping_key,
        album_title,
        fold_text(album_title),
        album_artist,
        fold_text(album_artist),
        raw_album,
        tag.album_artist.as_deref().unwrap_or("").trim(),
        primary_artist,
        tag.album_artist_sort,
        year,
        tag.original_release_date,
        tag.genre,
        tag.compilation,
        now,
    ])?;
    let rows_before = credit_rows(tx, CreditSide::Album, album_id)?;
    let mut album_artist_rows = tx.prepare_cached(
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, \
         credited_name, join_phrase, artist_mbid) VALUES (?1, ?2, ?3, 'main', ?4, ?5, ?6) \
         ON CONFLICT (local_album_id, position) DO UPDATE SET \
         local_artist_id = excluded.local_artist_id, credited_name = excluded.credited_name, \
         join_phrase = excluded.join_phrase, artist_mbid = excluded.artist_mbid",
    )?;
    for (position, (credit, id)) in album_credits.iter().zip(album_artist_ids).enumerate() {
        album_artist_rows.execute(params![
            album_id,
            position as i64,
            id,
            credit.credited_name,
            credit.join_phrase,
            credit.mbid,
        ])?;
    }
    let rows_after = credit_rows(tx, CreditSide::Album, album_id)?;
    if rows_after != rows_before {
        mark_stale(tx, album_id)?;
        record_moves(
            tx,
            CreditSide::Album,
            album_id,
            &rows_before,
            &rows_after,
            now,
        )?;
    }

    Ok(())
}

/// How an import adds its file: the track id its plan pinned, and the
/// album it joins, if any.
pub(crate) struct Adopted<'a> {
    pub track_id: &'a str,
    /// The album the file was fetched for. The file joins it as it is:
    /// the album row keeps its own names and key.
    pub album_id: Option<&'a str>,
}

/// A file's tag revision: a digest of its tag values, each under its name
/// in a fixed order. Pending contributions, previews and edition decisions
/// compare it to see a tag change. An absent value (no text, zero, an
/// empty list, an unset flag) adds nothing, so a field added here later
/// moves only the revisions of files that carry it. Edition hints are kept
/// beside the row and left out, so reading them is never a tag change.
fn tag_revision(tag: &AudioTag) -> String {
    let AudioTag {
        title,
        artist,
        album,
        track_number,
        album_artist,
        disc_number,
        year,
        genre,
        musicbrainz_release_group_id,
        musicbrainz_release_id,
        musicbrainz_recording_id,
        musicbrainz_release_track_id,
        musicbrainz_artist_id,
        musicbrainz_album_artist_id,
        acoustid_id,
        compilation,
        release_type,
        title_sort,
        artist_sort,
        album_sort,
        album_artist_sort,
        disc_subtitle,
        original_release_date,
        replaygain_track_gain,
        replaygain_album_gain,
        replaygain_track_peak,
        replaygain_album_peak,
        genres,
        artists,
        album_artists,
        musicbrainz_artist_ids,
        musicbrainz_album_artist_ids,
        artists_listed,
        album_artists_listed,
        edition: _,
    } = tag;
    let mut fields = TagFields::default();
    fields.text("title", title);
    fields.text("artist", artist);
    fields.text("album", album);
    fields.number("track_number", *track_number);
    fields.optional("album_artist", album_artist);
    fields.number("disc_number", *disc_number);
    if let Some(year) = year {
        fields.put("year", &year.to_le_bytes());
    }
    fields.optional("genre", genre);
    fields.optional("musicbrainz_release_group_id", musicbrainz_release_group_id);
    fields.optional("musicbrainz_release_id", musicbrainz_release_id);
    fields.optional("musicbrainz_recording_id", musicbrainz_recording_id);
    fields.optional("musicbrainz_release_track_id", musicbrainz_release_track_id);
    fields.optional("musicbrainz_artist_id", musicbrainz_artist_id);
    fields.optional("musicbrainz_album_artist_id", musicbrainz_album_artist_id);
    fields.optional("acoustid_id", acoustid_id);
    fields.flag("compilation", *compilation);
    fields.optional("release_type", release_type);
    fields.optional("title_sort", title_sort);
    fields.optional("artist_sort", artist_sort);
    fields.optional("album_sort", album_sort);
    fields.optional("album_artist_sort", album_artist_sort);
    fields.optional("disc_subtitle", disc_subtitle);
    fields.optional("original_release_date", original_release_date);
    fields.real("replaygain_track_gain", *replaygain_track_gain);
    fields.real("replaygain_album_gain", *replaygain_album_gain);
    fields.real("replaygain_track_peak", *replaygain_track_peak);
    fields.real("replaygain_album_peak", *replaygain_album_peak);
    fields.texts("genres", genres);
    fields.credits("artists", artists);
    fields.credits("album_artists", album_artists);
    fields.texts("musicbrainz_artist_ids", musicbrainz_artist_ids);
    fields.texts("musicbrainz_album_artist_ids", musicbrainz_album_artist_ids);
    fields.flag("artists_listed", *artists_listed);
    fields.flag("album_artists_listed", *album_artists_listed);
    Sha256::digest(&fields.0)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Named, length-prefixed values, the input of a tag revision.
#[derive(Default)]
struct TagFields(Vec<u8>);

impl TagFields {
    fn put(&mut self, name: &str, value: &[u8]) {
        self.0.extend_from_slice(name.as_bytes());
        self.0.push(0);
        self.0
            .extend_from_slice(&(value.len() as u64).to_le_bytes());
        self.0.extend_from_slice(value);
    }

    fn text(&mut self, name: &str, value: &str) {
        if !value.is_empty() {
            self.put(name, value.as_bytes());
        }
    }

    fn optional(&mut self, name: &str, value: &Option<String>) {
        if let Some(value) = value {
            self.put(name, value.as_bytes());
        }
    }

    fn number(&mut self, name: &str, value: u32) {
        if value != 0 {
            self.put(name, &value.to_le_bytes());
        }
    }

    fn real(&mut self, name: &str, value: Option<f64>) {
        if let Some(value) = value {
            self.put(name, &value.to_bits().to_le_bytes());
        }
    }

    fn flag(&mut self, name: &str, value: bool) {
        if value {
            self.put(name, &[1]);
        }
    }

    fn texts(&mut self, name: &str, values: &[String]) {
        if values.is_empty() {
            return;
        }
        let mut list = TagFields::default();
        for value in values {
            list.put("", value.as_bytes());
        }
        self.put(name, &list.0);
    }

    fn credits(&mut self, name: &str, credits: &[AudioArtistCredit]) {
        if credits.is_empty() {
            return;
        }
        let mut list = TagFields::default();
        for credit in credits {
            let AudioArtistCredit {
                name,
                credited_name,
                sort_name,
                musicbrainz_artist_id,
                join_phrase,
            } = credit;
            let mut one = TagFields::default();
            one.text("name", name);
            one.optional("credited_name", credited_name);
            one.optional("sort_name", sort_name);
            one.optional("musicbrainz_artist_id", musicbrainz_artist_id);
            one.text("join_phrase", join_phrase);
            list.put("", &one.0);
        }
        self.put(name, &list.0);
    }
}

/// Write one file's catalog rows.
fn write_item(
    tx: &Connection,
    run_id: Option<&str>,
    item: &CommitIndexedItem,
    adopted: Option<&Adopted<'_>>,
) -> rusqlite::Result<Written> {
    let tag = &item.tags.tag;
    let header = &item.tags.header;
    let now = item.tags_read_at;
    let directory = grouping_directory(&item.relative_path);
    let raw_album = tag.album.trim();
    let raw_album_artist = tag
        .album_artist
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| tag.artist.trim());
    let raw_title = tag.title.trim();
    let parsed = (raw_album.is_empty() || raw_album_artist.is_empty() || raw_title.is_empty())
        .then(|| parse_names_for_row(&item.relative_path));
    let file_stem = std::path::Path::new(&item.relative_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("")
        .to_owned();
    let parsed_title = parsed
        .as_ref()
        .and_then(|names| names.title.clone())
        .filter(|title| *title != file_stem);
    let (title, title_provenance) = if !raw_title.is_empty() {
        (raw_title.to_owned(), "tag")
    } else if let Some(parsed) = parsed_title {
        (parsed, "parsed")
    } else {
        (file_stem.clone(), "placeholder")
    };
    let (album_title, album_title_provenance) = if !raw_album.is_empty() {
        (raw_album.to_owned(), "tag")
    } else if let Some(album) = parsed
        .as_ref()
        .and_then(|names| names.album.clone())
        .filter(|album| *album != file_stem)
    {
        (album, "parsed")
    } else {
        (file_stem.clone(), "placeholder")
    };
    let (album_artist, album_artist_provenance) = if !raw_album_artist.is_empty() {
        (raw_album_artist.to_owned(), "tag")
    } else if let Some(artist) = parsed.as_ref().and_then(|names| names.artist.clone()) {
        (artist, "parsed")
    } else {
        (UNKNOWN_ARTIST.to_owned(), "placeholder")
    };
    let track_artist = non_empty(&tag.artist).unwrap_or_else(|| album_artist.clone());
    let track_number = if tag.track_number > 0 {
        tag.track_number
    } else {
        parsed
            .as_ref()
            .and_then(|names| names.track_number)
            .unwrap_or(0)
    };
    let year = tag
        .year
        .or_else(|| parsed.as_ref().and_then(|names| names.year));

    let album_credits = artists::credits(
        tx,
        &tag.album_artists,
        &tag.musicbrainz_album_artist_ids,
        tag.album_artists_listed,
        &album_artist,
        tag.album_artist_sort.as_deref(),
    )?;
    let track_credits = artists::credits(
        tx,
        &tag.artists,
        &tag.musicbrainz_artist_ids,
        tag.artists_listed,
        &track_artist,
        tag.artist_sort.as_deref(),
    )?;

    let release_mbid = tag
        .musicbrainz_release_id
        .as_deref()
        .map(str::trim)
        .filter(|mbid| !mbid.is_empty());
    let names = AlbumNames {
        release_mbid,
        from_tags: album_title_provenance == "tag" && album_artist_provenance == "tag",
        directory: &directory,
        title_folded: fold_text(&album_title),
        artist_folded: fold_text(&album_artist),
    };
    let grouping_key = names.key();
    let disc_number = i64::from(tag.disc_number.max(1));
    let title_folded = fold_text(&title);

    // The row at this path keeps its id, unless the scan reads other music
    // there: that row is retired with everything attached to it, its
    // record included, and the file is indexed as at a new path. A new
    // path continues a track whose file went away when the keys agree:
    // the old row moves here and the upsert below refreshes it.
    let at_path: Option<AtPath> = tx
        .prepare_cached(
            "SELECT id, local_album_id, availability, duration_seconds, \
             embedded_recording_mbid, title_folded, title_provenance, disc_number, \
             track_number, sample_rate, bit_depth, channels \
             FROM local_tracks WHERE root_id = ?1 AND relative_path = ?2",
        )?
        .query_row(params![item.root_id, item.relative_path], |row| {
            Ok(AtPath {
                id: row.get(0)?,
                album_id: row.get(1)?,
                availability: row.get(2)?,
                duration_seconds: row.get(3)?,
                recording_mbid: row.get(4)?,
                title_folded: row.get(5)?,
                title_provenance: row.get(6)?,
                disc_number: row.get(7)?,
                track_number: row.get(8)?,
                sample_rate: row.get(9)?,
                bit_depth: row.get(10)?,
                channels: row.get(11)?,
            })
        })
        .optional()?;
    let mut replaced: Option<AtPath> = None;
    // The row the file continues at its path.
    let mut continued: Option<AtPath> = None;
    let mut previous: Option<(String, String)> = match at_path {
        Some(row) if run_id.is_some() && adopted.is_none() && other_music(&row, item) => {
            give_up_path(tx, &row.id, item)?;
            replaced = Some(row);
            None
        }
        Some(row) => {
            let previous = Some((row.id.clone(), row.album_id.clone()));
            continued = Some(row);
            previous
        }
        None => None,
    };
    if previous.is_none()
        && let Some(run_id) = run_id
    {
        let recording = tag
            .musicbrainz_recording_id
            .as_deref()
            .map(str::trim)
            .filter(|mbid| !mbid.is_empty());
        let key = TrackKey {
            recording_mbid: recording,
            album_title_folded: &names.title_folded,
            album_artist_folded: &names.artist_folded,
            title_folded: &title_folded,
            disc_number,
            track_number: i64::from(track_number),
            duration_seconds: header.duration_seconds,
        };
        if let Some((moved_id, moved_album)) = find_moved_track(tx, run_id, &key)? {
            tx.prepare_cached(
                "UPDATE local_tracks SET root_id = ?2, relative_path = ?3, path_hash = ?4, \
                 file_path = ?5, row_revision = row_revision + 1 WHERE id = ?1",
            )?
            .execute(params![
                moved_id,
                item.root_id,
                item.relative_path,
                sha256_hex(&item.relative_path),
                item.absolute_path,
            ])?;
            previous = Some((moved_id, moved_album));
        }
    }
    // How the stored row holds its album: a curator's lock keeps it as is
    // (album row included), and a grouping carried from v2 holds while the
    // file still names the album.
    let membership: Option<(String, bool)> = match &previous {
        Some((id, _)) => tx
            .prepare_cached(
                "SELECT membership_source, membership_locked = 1 FROM local_tracks WHERE id = ?1",
            )?
            .query_row(params![id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?,
        None => None,
    };
    let locked = membership.as_ref().is_some_and(|(_, locked)| *locked);
    let legacy = membership
        .as_ref()
        .is_some_and(|(source, _)| source == "legacy_import");
    let joined = adopted.and_then(|adopted| adopted.album_id);
    let album_id = match (&previous, locked, joined) {
        (_, _, Some(joined)) => joined.to_owned(),
        (Some((_, album)), true, None) => album.clone(),
        _ => resolve_album(
            tx,
            &names,
            &Place {
                run_id,
                root_id: &item.root_id,
                directory: &directory,
            },
            previous.as_ref().map(|(_, album)| album.as_str()),
            legacy,
        )?,
    };
    let (album_artist_ids, track_artist_ids) = artists::resolve(
        tx,
        &album_credits,
        &track_credits,
        Own {
            run_id,
            track_id: previous.as_ref().map(|(id, _)| id.as_str()),
            album_id: &album_id,
        },
        now,
    )?;
    if !locked && joined.is_none() {
        write_album(
            tx,
            &album_id,
            item,
            &album_credits,
            &album_artist_ids,
            &AlbumRow {
                grouping_key: &grouping_key,
                album_title: &album_title,
                album_artist: &album_artist,
                raw_album,
                year,
                now,
            },
        )?;
    }

    // A new path takes a stable id unless a moved track already holds it;
    // an import takes the id its plan pinned.
    let track_id = match (&previous, adopted) {
        (Some((id, _)), _) => id.clone(),
        (None, Some(adopted)) => adopted.track_id.to_owned(),
        (None, None) => {
            let stable = stable_id(&format!("track:{}:{}", item.root_id, item.relative_path));
            let taken: bool = tx
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM local_tracks WHERE id = ?1)")?
                .query_row(params![stable], |row| row.get(0))?;
            if taken {
                uuid::Uuid::new_v4().to_string()
            } else {
                stable
            }
        }
    };
    let tag_revision = tag_revision(tag);
    let file_format = crate::tags::format_for_path(std::path::Path::new(&item.relative_path))
        .map(|format| format.as_str().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned());
    let policy = item.effective_policy.as_str();
    tx.prepare_cached(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, \
         tag_revision, tags_read_at, title, title_folded, artist_name, artist_name_folded, \
         album_title, album_title_folded, album_artist_name, album_artist_name_folded, \
         tag_album_title, tag_album_artist_name, disc_number, track_number, year, genre, \
         genre_folded, release_type, title_sort, artist_sort, album_sort, album_artist_sort, \
         disc_subtitle, is_compilation, embedded_release_group_mbid, embedded_release_mbid, \
         embedded_recording_mbid, embedded_release_track_mbid, embedded_artist_mbid, \
         embedded_album_artist_mbid, duration_seconds, file_format, bit_rate, sample_rate, \
         bit_depth, channels, replaygain_track_gain, replaygain_album_gain, \
         replaygain_track_peak, replaygain_album_peak, availability, ingest_source, \
         imported_at, membership_source, desired_policy_revision, applied_policy_revision, \
         applied_policy, title_provenance, album_title_provenance, album_artist_provenance) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'exact', ?10, ?11, ?12, ?13, ?14, ?15, \
         ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, \
         ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45, ?46, ?47, ?48, ?49, \
         'indexed', 'scan', ?11, 'automatic', ?50, ?50, ?51, ?52, ?53, ?54) \
         ON CONFLICT (root_id, relative_path) DO UPDATE SET \
         local_album_id = CASE WHEN local_tracks.membership_locked = 1 \
           THEN local_tracks.local_album_id ELSE excluded.local_album_id END, \
         membership_source = CASE WHEN local_tracks.membership_locked = 1 \
           OR local_tracks.local_album_id = excluded.local_album_id \
           THEN local_tracks.membership_source ELSE 'automatic' END, \
         file_path = excluded.file_path, file_size_bytes = excluded.file_size_bytes, \
         file_mtime_ns = excluded.file_mtime_ns, stat_revision = excluded.stat_revision, \
         stat_revision_kind = excluded.stat_revision_kind, tag_revision = excluded.tag_revision, \
         tags_read_at = excluded.tags_read_at, title = excluded.title, \
         title_folded = excluded.title_folded, artist_name = excluded.artist_name, \
         artist_name_folded = excluded.artist_name_folded, album_title = excluded.album_title, \
         album_title_folded = excluded.album_title_folded, \
         album_artist_name = excluded.album_artist_name, \
         album_artist_name_folded = excluded.album_artist_name_folded, \
         tag_album_title = excluded.tag_album_title, \
         tag_album_artist_name = excluded.tag_album_artist_name, \
         disc_number = excluded.disc_number, track_number = excluded.track_number, \
         year = excluded.year, genre = excluded.genre, genre_folded = excluded.genre_folded, \
         release_type = excluded.release_type, title_sort = excluded.title_sort, \
         artist_sort = excluded.artist_sort, album_sort = excluded.album_sort, \
         album_artist_sort = excluded.album_artist_sort, \
         disc_subtitle = excluded.disc_subtitle, is_compilation = excluded.is_compilation, \
         embedded_release_group_mbid = excluded.embedded_release_group_mbid, \
         embedded_release_mbid = excluded.embedded_release_mbid, \
         embedded_recording_mbid = excluded.embedded_recording_mbid, \
         embedded_release_track_mbid = excluded.embedded_release_track_mbid, \
         embedded_artist_mbid = excluded.embedded_artist_mbid, \
         embedded_album_artist_mbid = excluded.embedded_album_artist_mbid, \
         duration_seconds = excluded.duration_seconds, file_format = excluded.file_format, \
         bit_rate = excluded.bit_rate, sample_rate = excluded.sample_rate, \
         bit_depth = excluded.bit_depth, channels = excluded.channels, \
         replaygain_track_gain = excluded.replaygain_track_gain, \
         replaygain_album_gain = excluded.replaygain_album_gain, \
         replaygain_track_peak = excluded.replaygain_track_peak, \
         replaygain_album_peak = excluded.replaygain_album_peak, \
         desired_policy_revision = excluded.desired_policy_revision, \
         applied_policy_revision = excluded.applied_policy_revision, \
         applied_policy = excluded.applied_policy, \
         title_provenance = excluded.title_provenance, \
         album_title_provenance = excluded.album_title_provenance, \
         album_artist_provenance = excluded.album_artist_provenance, \
         availability = CASE WHEN local_tracks.manual_excluded = 1 THEN 'excluded' \
           ELSE 'indexed' END, missing_since = NULL, \
         excluded_at = CASE WHEN local_tracks.manual_excluded = 1 \
           THEN COALESCE(local_tracks.excluded_at, excluded.tags_read_at) END",
    )?
    .execute(params![
        track_id,
        album_id,
        item.root_id,
        item.absolute_path,
        item.relative_path,
        sha256_hex(&item.relative_path),
        item.size_bytes as i64,
        item.mtime_ns,
        exact_stat_revision(item.size_bytes, item.mtime_ns),
        tag_revision,
        now,
        title,
        title_folded,
        track_artist,
        fold_text(&track_artist),
        album_title,
        fold_text(&album_title),
        album_artist,
        fold_text(&album_artist),
        raw_album,
        tag.album_artist.as_deref().unwrap_or("").trim(),
        disc_number,
        i64::from(track_number),
        year,
        tag.genre,
        tag.genre.as_deref().map(fold_text),
        tag.release_type,
        tag.title_sort,
        tag.artist_sort,
        tag.album_sort,
        tag.album_artist_sort,
        tag.disc_subtitle,
        tag.compilation,
        tag.musicbrainz_release_group_id,
        tag.musicbrainz_release_id,
        tag.musicbrainz_recording_id,
        tag.musicbrainz_release_track_id,
        tag.musicbrainz_artist_id,
        tag.musicbrainz_album_artist_id,
        header.duration_seconds,
        file_format,
        header.bitrate_kbps,
        header.sample_rate,
        header.bit_depth,
        header.channels,
        tag.replaygain_track_gain,
        tag.replaygain_album_gain,
        tag.replaygain_track_peak,
        tag.replaygain_album_peak,
        item.policy_revision,
        policy,
        title_provenance,
        album_title_provenance,
        album_artist_provenance,
    ])?;
    // The row's id and album as stored (a locked membership keeps its album).
    let (track_id, stored_album): (String, String) = tx
        .prepare_cached(
            "SELECT id, local_album_id FROM local_tracks WHERE root_id = ?1 AND relative_path = ?2",
        )?
        .query_row(params![item.root_id, item.relative_path], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    // A new copy of the recording the row describes is a file the app
    // never wrote: its record goes, unless the app has stepped back from
    // the album, where only handing it back clears anything.
    let new_copy = match continued {
        Some(row)
            if run_id.is_some()
                && adopted.is_none()
                && new_copy(
                    &row,
                    item,
                    &stored_album,
                    disc_number,
                    i64::from(track_number),
                )
                && crate::publish::record::record(tx, &row.id)?.exists()
                && !takeover::is_marked(tx, &row.album_id)?
                && crate::publish::record::forget(tx, &row.id)? =>
        {
            Some(NewCopy {
                before: Sound::of_row(&row),
                after: Sound::of_file(item),
                track_id: row.id,
                album_id: row.album_id,
            })
        }
        _ => None,
    };

    let edition = &tag.edition;
    let blank = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    tx.prepare_cached(
        "INSERT INTO local_track_edition_tags (local_track_id, media, barcode, catalog_number, \
         release_country, total_discs, release_date) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT (local_track_id) DO UPDATE SET media = excluded.media, \
         barcode = excluded.barcode, catalog_number = excluded.catalog_number, \
         release_country = excluded.release_country, total_discs = excluded.total_discs, \
         release_date = excluded.release_date",
    )?
    .execute(params![
        track_id,
        blank(&edition.media),
        blank(&edition.barcode),
        blank(&edition.catalog_number),
        blank(&edition.release_country),
        edition.total_discs,
        edition.release_date,
    ])?;
    let rows_before = credit_rows(tx, CreditSide::Track, &track_id)?;
    tx.prepare_cached("DELETE FROM local_track_artists WHERE local_track_id = ?1")?
        .execute(params![track_id])?;
    let mut track_artist_rows = tx.prepare_cached(
        "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role, \
         credited_name, join_phrase, artist_mbid) VALUES (?1, ?2, ?3, 'main', ?4, ?5, ?6)",
    )?;
    for (position, (credit, id)) in track_credits.iter().zip(&track_artist_ids).enumerate() {
        track_artist_rows.execute(params![
            track_id,
            position as i64,
            id,
            credit.credited_name,
            credit.join_phrase,
            credit.mbid,
        ])?;
    }
    let rows_after = credit_rows(tx, CreditSide::Track, &track_id)?;
    if rows_after != rows_before {
        let album: String = tx
            .prepare_cached("SELECT local_album_id FROM local_tracks WHERE id = ?1")?
            .query_row(params![track_id], |row| row.get(0))?;
        mark_stale(tx, &album)?;
        record_moves(
            tx,
            CreditSide::Track,
            &track_id,
            &rows_before,
            &rows_after,
            now,
        )?;
    }
    tx.prepare_cached(
        "DELETE FROM local_track_genres WHERE local_track_id = ?1 AND source = 'local'",
    )?
    .execute(params![track_id])?;
    let genres: Vec<&String> = if tag.genres.is_empty() {
        tag.genre.iter().collect()
    } else {
        tag.genres.iter().collect()
    };
    let mut genre_rows = tx.prepare_cached(
        "INSERT OR IGNORE INTO local_track_genres (local_track_id, position, name, folded_name, \
         source, source_document_revision) VALUES (?1, ?2, ?3, ?4, 'local', ?5)",
    )?;
    for (position, genre) in genres
        .into_iter()
        .filter(|genre| !genre.trim().is_empty())
        .enumerate()
    {
        genre_rows.execute(params![
            track_id,
            position as i64,
            genre.trim(),
            fold_text(genre),
            tag_revision
        ])?;
    }
    let moved_from = previous
        .map(|(_, album)| album)
        .filter(|album| *album != stored_album);
    if let Some(left) = &moved_from
        && let Some(run_id) = run_id
    {
        // Recorded now, not with the window's marks: a later item in this
        // window may empty the album this one left.
        tx.prepare_cached(
            "INSERT INTO library_scan_album_moves (run_id, root_id, relative_path, \
             local_track_id, previous_album_id) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (run_id, root_id, relative_path) DO UPDATE SET \
             local_track_id = excluded.local_track_id, \
             previous_album_id = excluded.previous_album_id",
        )?
        .execute(params![
            run_id,
            item.root_id,
            item.relative_path,
            track_id,
            left
        ])?;
    }
    let other_music = replaced.map(|row| OtherMusic {
        old_track_id: row.id,
        old_album_id: row.album_id,
        new_track_id: track_id.clone(),
        old_seconds: row.duration_seconds,
        new_seconds: header.duration_seconds,
    });
    Ok(Written {
        track_id,
        album_id: stored_album,
        moved_from,
        other_music,
        new_copy,
    })
}

/// Whether a track row still holds its album, as SQL over the track alias
/// `track` and the run id `run`. A track row is never deleted, so an
/// album keeps the rows of files removed years ago; they must not keep it
/// from being followed when the files that are left are retagged.
///
/// A row holds its album while its file is there, and for the one scan
/// in which the file went away: the row is still `indexed` (the run
/// walked past its file, or has not reached it), or it was marked
/// `missing` since this run began. A share that drops out half way must
/// not hand an album to the few of its files that were retagged, so the
/// follow waits one scan. The next scan knows the file is still gone: its
/// row is then `missing` from before that run began.
///
/// A row that is `excluded`, or `missing` since before the run began,
/// holds nothing. When the rows left behind are half the album or more,
/// the follow also waits until each of them has been gone a day
/// ([`LONG_GONE_SECONDS`]).
pub(crate) fn holds(track: &str, run: &str) -> String {
    format!(
        "({track}.availability = 'indexed' OR ({track}.availability = 'missing' \
         AND {track}.missing_since >= (SELECT started_at FROM library_scan_runs \
         WHERE id = {run})))"
    )
}

/// How long the rows a retag left behind must have been missing, when
/// they are half the album or more, before the album is followed. In
/// seconds, like `missing_since`. A share that drops out for a few scans
/// must not hand the album's row and chosen edition to the few files that
/// were retagged meanwhile. A missing row with no time counts as long
/// gone.
const LONG_GONE_SECONDS: f64 = 86_400.0;

/// An album a track left in this run. It is followed when its files were
/// retagged together:
///
/// - no track row holds it any more ([`holds`]);
/// - no track that left it in an earlier run sits on a piece of it
///   ([`has_counted_departure`]): an album retagged over two runs, or split
///   over two, is settled when the run ends ([`join_across_scans`]);
/// - every track that left it in this run sits on one album;
/// - that album holds no other track row, or only rows this run first
///   indexed on a row this run made: files added with the retag, which
///   come along;
/// - when no more of its tracks moved than stayed behind (moved counts
///   the tracks that left it in this run, never the added files), each
///   row that stayed is excluded or has been missing a day
///   ([`LONG_GONE_SECONDS`]). Until then nothing is done and nothing is
///   recorded.
///
/// The old row then takes over the new name and key, keeping its id,
/// reviews and pins, and the new row goes, its tracks, plays, favorites
/// and playlist entries passing to the old one. Rows left behind (missing
/// or excluded tracks) are not written: they stay on the old row with
/// their ids, plays and track matches. An album with an edition someone
/// chose and no file the app wrote is stamped for automatic management
/// to leave alone ([`stamp_if_retagged_by_hand`]). Returns the album that
/// went.
///
/// A match or a review that landed on the new row while the scan was half
/// way through the album is settled by `settle_followed_match`: a
/// person's match is never replaced, and two people's different choices
/// leave both rows as they are. Work a person started on the new row (a
/// preview, a draft, an override, anything the audit log names) is never
/// rewritten to name another row: it keeps the new row from being
/// deleted, so both rows stay.
///
/// When the two rows look like one retagged album and both stay, the pair
/// is recorded with the reason and the log names the two, once
/// ([`RetagPair::leave_apart`]): two chosen matches differ, or the new row
/// could not be deleted. A pair on record is left as it is here, unless
/// the store failed the last time and the pair is still open: one a
/// person joined or dismissed is theirs. The end of the run tries the
/// open pairs again as a merge that keeps the new row
/// ([`join_across_scans`]): after a store error, live work, or work on the
/// new row. Nothing is recorded when the
/// tracks went to an album that was there before or that holds tracks
/// from elsewhere (a person joining albums), or to more than one album
/// (a split).
fn follow_regrouped(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<Option<String>> {
    if held(tx, run_id, album_id)? || has_counted_departure(tx, run_id, album_id, now)? {
        return Ok(None);
    }
    let destinations: Vec<String> = tx
        .prepare_cached(
            "SELECT DISTINCT t.local_album_id FROM library_scan_album_moves m \
             JOIN local_tracks t ON t.id = m.local_track_id \
             WHERE m.run_id = ?1 AND m.previous_album_id = ?2",
        )?
        .query_map(params![run_id, album_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let [target] = destinations.as_slice() else {
        return Ok(None);
    };
    if target == album_id {
        return Ok(None);
    }
    let pair = RetagPair {
        old: album_id,
        new: target,
        now,
    };
    // A pair on record is left as it is. Only one the store failed on,
    // and that no person has settled since, is tried again.
    let retry = (RetagPairReason::StoreError, RetagPairState::Open);
    if retag_pairs::recorded(tx, album_id, target)?.is_some_and(|recorded| recorded != retry) {
        return Ok(None);
    }
    // The new row holds tracks that did not come from this album. When
    // the row or one of those tracks was there before this run, a person
    // retagged the album into another one: nothing to say about it. When
    // this run made the row and first indexed every one of those tracks,
    // files were added along with the retag, and they come along.
    if !holds_only_its_retag(tx, run_id, target, album_id)? {
        return Ok(None);
    }
    // Files added with the retag never left the album: only the tracks
    // that moved from it count against those that stayed.
    let (moved, stayed): (i64, i64) = tx
        .prepare_cached(
            "SELECT (SELECT COUNT(DISTINCT m.local_track_id) FROM library_scan_album_moves m \
               JOIN local_tracks t ON t.id = m.local_track_id \
               WHERE m.run_id = ?1 AND m.previous_album_id = ?2 AND t.local_album_id = ?3), \
             (SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?2)",
        )?
        .query_row(params![run_id, album_id, target], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    if waits_for_the_rest(tx, album_id, moved, stayed, &[], now)? {
        return Ok(None);
    }
    if chosen_matches_differ(tx, target, album_id)? {
        pair.leave_apart(tx, RetagPairReason::ChosenMatchesDiffer, None)?;
        return Ok(None);
    }
    let followed = savepoint(tx, "follow_album", || {
        tx.execute(
            "UPDATE local_albums SET (root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, tag_album_title, \
             tag_album_artist_name, album_artist_id, album_artist_sort_name, year, \
             original_release_date, primary_genre, is_compilation, updated_at) = \
             (SELECT root_id, grouping_key, title, title_folded, album_artist_name, \
             album_artist_name_folded, tag_album_title, tag_album_artist_name, \
             album_artist_id, album_artist_sort_name, year, original_release_date, \
             primary_genre, is_compilation, updated_at FROM local_albums WHERE id = ?2), \
             row_revision = row_revision + 1 WHERE id = ?1",
            params![album_id, target],
        )?;
        tx.execute(
            "UPDATE local_tracks SET local_album_id = ?1 WHERE local_album_id = ?2",
            params![album_id, target],
        )?;
        tx.execute(
            "DELETE FROM local_album_artists WHERE local_album_id = ?1",
            params![album_id],
        )?;
        tx.execute(
            "UPDATE local_album_artists SET local_album_id = ?1 WHERE local_album_id = ?2",
            params![album_id, target],
        )?;
        mark_stale(tx, album_id)?;
        tx.execute(
            "DELETE FROM local_album_artwork WHERE local_album_id = ?1",
            params![target],
        )?;
        tx.execute(
            "DELETE FROM library_identify_jobs WHERE local_album_id = ?1",
            params![target],
        )?;
        settle_followed_match(tx, target, album_id, now)?;
        // What people made of the new row since an earlier window of this
        // run created it. Every play filed under the new row names one of
        // the tracks moved above, or no track, so each lands on the kept
        // row. Written by track all the same: a play must never be parted
        // from its track should the new row ever hold another album's.
        tx.execute(
            "UPDATE library_play_history SET local_album_id = COALESCE( \
             (SELECT t.local_album_id FROM local_tracks t \
              WHERE t.id = library_play_history.local_track_id), ?1) \
             WHERE local_album_id = ?2",
            params![album_id, target],
        )?;
        retarget_references(tx, "album", target, album_id)?;
        tx.execute(
            "UPDATE playlist_tracks SET album_id = ?1 WHERE album_id = ?2",
            params![album_id, target],
        )?;
        tx.execute("DELETE FROM local_albums WHERE id = ?1", params![target])?;
        stamp_if_retagged_by_hand(tx, album_id, album_id, now)
    })?;
    match followed {
        Ok(_) => Ok(Some(target.clone())),
        Err(error) => {
            let reason = if foreign_key_refused(&error) {
                RetagPairReason::WorkOnNewRow
            } else {
                RetagPairReason::StoreError
            };
            pair.leave_apart(tx, reason, Some(&error))?;
            Ok(None)
        }
    }
}

/// After a follow, and for the half of an album retagged in a first
/// sitting: when `edition_of` has an edition someone chose (a release a
/// person picked or v2 carried, or a custom edition) and the app has no
/// record of any file on `album_id`, stamp `album_id` for automatic
/// management, which then leaves it unmanaged instead of writing the old
/// edition's name back over the retag. An album with a file the app wrote
/// is left to the check of those bytes, which tells the app's own retag
/// from a person's. Returns whether it stamped.
fn stamp_if_retagged_by_hand(
    tx: &rusqlite::Transaction<'_>,
    edition_of: &str,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let custom: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1)",
        )?
        .query_row(params![edition_of], |row| row.get(0))?;
    if !custom && !release_chosen(tx, edition_of)? {
        return Ok(false);
    }
    if album_has_record(tx, album_id)? {
        return Ok(false);
    }
    automatic::stamp_retag_followed(tx, album_id, now)?;
    Ok(true)
}

/// Whether `target` holds only the tracks that left `album_id` in this
/// run, or besides them only rows this run first indexed on a row this run
/// made: files added with the retag. Rows that were there before the run,
/// or a row made earlier, mean a person put the tracks into another album.
fn holds_only_its_retag(
    tx: &Connection,
    run_id: &str,
    target: &str,
    album_id: &str,
) -> rusqlite::Result<bool> {
    let (foreign, older): (bool, bool) = tx
        .prepare_cached(
            "WITH run AS (SELECT started_at FROM library_scan_runs WHERE id = ?1), \
             other AS (SELECT t.imported_at, t.root_id, t.relative_path FROM local_tracks t \
               WHERE t.local_album_id = ?2 AND NOT EXISTS ( \
                 SELECT 1 FROM library_scan_album_moves m WHERE m.run_id = ?1 \
                 AND m.local_track_id = t.id AND m.previous_album_id = ?3)) \
             SELECT EXISTS(SELECT 1 FROM other), \
             EXISTS(SELECT 1 FROM local_albums a, run WHERE a.id = ?2 \
               AND a.created_at < run.started_at) \
             OR EXISTS(SELECT 1 FROM other, run WHERE other.imported_at < run.started_at \
               OR NOT EXISTS (SELECT 1 FROM library_scan_inventory i WHERE i.run_id = ?1 \
                 AND i.root_id = other.root_id AND i.relative_path = other.relative_path))",
        )?
        .query_row(params![run_id, target, album_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    Ok(!(foreign && older))
}

/// Whether joining an album to the row its tracks moved to waits for the
/// rest of it: when no more of its tracks `moved` than `stayed` on it,
/// every row that stayed must be excluded or have been missing a day
/// ([`LONG_GONE_SECONDS`]; a missing row with no time counts as long gone).
/// The rows in `moved_ids` moved but still sit on the album, and are not
/// read.
///
/// A row whose path other music took stays behind for good: an
/// `other_music_at_path` action of the album names it, and it is still at
/// its hidden name. It is never read as excluded or long gone, so the
/// album goes ahead only when more of its tracks moved than stayed or were
/// replaced by other music. A row put back at a real path since no longer
/// counts.
pub(crate) fn waits_for_the_rest(
    tx: &Connection,
    album_id: &str,
    moved: i64,
    stayed: i64,
    moved_ids: &[String],
    now: f64,
) -> rusqlite::Result<bool> {
    if moved > stayed {
        return Ok(false);
    }
    let moved_ids = serde_json::to_string(moved_ids)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    tx.prepare_cached(&format!(
        "SELECT EXISTS(SELECT 1 FROM local_tracks t WHERE t.local_album_id = ?1 \
         AND t.id NOT IN (SELECT value FROM json_each(?3)) \
         AND ((substr(t.relative_path, 1, length(?4) + length(t.id) + 1) = ?4 || t.id || '/' \
           AND EXISTS (SELECT 1 FROM library_catalog_actions a, \
             json_each(a.after_json, '$.pairs') p \
             WHERE a.local_album_id = ?1 AND a.action_kind = '{OTHER_MUSIC_KIND}' \
             AND json_extract(p.value, '$.old_track_id') = t.id)) \
         OR (t.availability != 'excluded' AND NOT (t.availability = 'missing' \
           AND (t.missing_since IS NULL OR t.missing_since <= ?2)))))"
    ))?
    .query_row(
        params![
            album_id,
            now - LONG_GONE_SECONDS,
            moved_ids,
            format!("{HIDDEN_PREFIX}gone/")
        ],
        |row| row.get(0),
    )
}

/// Whether a statement failed because a row elsewhere still names the row
/// it would delete or re-point. SQLite reports a plain foreign key as
/// `SQLITE_CONSTRAINT_FOREIGNKEY` and one with `ON DELETE RESTRICT` as
/// `SQLITE_CONSTRAINT_TRIGGER` with the message "FOREIGN KEY constraint
/// failed". Probed on SQLite 3.46.1; the build bundles 3.46.0, where the
/// brief with a plan on the new row sees the same.
fn foreign_key_refused(error: &rusqlite::Error) -> bool {
    let rusqlite::Error::SqliteFailure(failure, message) = error else {
        return false;
    };
    failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
        || (failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
            && message
                .as_deref()
                .is_some_and(|message| message.contains("FOREIGN KEY")))
}

/// An album's old row and the new row its retagged files sit on, when the
/// two are not joined.
pub(crate) struct RetagPair<'a> {
    pub(crate) old: &'a str,
    pub(crate) new: &'a str,
    pub(crate) now: f64,
}

impl RetagPair<'_> {
    /// Record the pair with the reason both rows stay, and say so in the
    /// log, once: a pair already on record is neither written nor logged
    /// again. Runs in the window's transaction, outside the follow's
    /// savepoint, so it lands with the rows it describes.
    pub(crate) fn leave_apart(
        &self,
        tx: &Connection,
        reason: RetagPairReason,
        error: Option<&rusqlite::Error>,
    ) -> rusqlite::Result<()> {
        let now_ms = (self.now * 1000.0) as i64;
        if retag_pairs::record(tx, self.old, self.new, reason, now_ms)? {
            tracing::warn!(
                album_id = self.old,
                target = self.new,
                reason = reason.as_str(),
                error = error.map(tracing::field::display),
                "retagged album was not joined with its new row; both kept"
            );
        }
        Ok(())
    }
}

/// Drop an album left with no tracks, with its scan-owned joins. Rows that
/// reference the album elsewhere (identities, reviews) keep it, and so do
/// a favorite, a rating and a playlist entry, which name it without a
/// foreign key:
/// the album may be the old half of a retag that a later scan joins, and
/// they go with it then.
fn drop_if_empty(tx: &Connection, album_id: &str) -> rusqlite::Result<()> {
    let kept: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE local_album_id = ?1) \
             OR EXISTS(SELECT 1 FROM library_user_favorites \
               WHERE item_kind = 'album' AND item_id = ?1) \
             OR EXISTS(SELECT 1 FROM library_user_ratings \
               WHERE item_kind = 'album' AND item_id = ?1) \
             OR EXISTS(SELECT 1 FROM playlist_tracks WHERE album_id = ?1)",
        )?
        .query_row(params![album_id], |row| row.get(0))?;
    if kept {
        return Ok(());
    }
    let dropped = savepoint(tx, "drop_album", || {
        tx.execute(
            "DELETE FROM local_album_artists WHERE local_album_id = ?1",
            params![album_id],
        )?;
        tx.execute(
            "DELETE FROM local_album_artwork WHERE local_album_id = ?1",
            params![album_id],
        )?;
        tx.execute("DELETE FROM local_albums WHERE id = ?1", params![album_id])
    })?;
    if let Err(error) = dropped {
        tracing::debug!(%error, album_id, "emptied album still referenced; kept");
    }
    Ok(())
}

/// An album that received tracks in this run from an album a track still
/// holds ([`holds`]) may be half of a retag the scan has not finished
/// reading. It is not offered for identification yet: a job would judge
/// half an album, and a match written on this row keeps the old row from
/// taking it over. Read from the run's move rows, so a resumed run holds
/// the same albums.
fn held_back(tx: &Connection, run_id: &str, album_id: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(&format!(
        "SELECT EXISTS(SELECT 1 FROM local_tracks t \
         CROSS JOIN library_scan_album_moves m ON m.run_id = ?1 AND m.root_id = t.root_id \
           AND m.relative_path = t.relative_path AND m.local_track_id = t.id \
         WHERE t.local_album_id = ?2 AND m.previous_album_id <> ?2 \
         AND EXISTS (SELECT 1 FROM local_tracks left_behind \
           WHERE left_behind.local_album_id = m.previous_album_id AND {}))",
        holds("left_behind", "?1")
    ))?
    .query_row(params![run_id, album_id], |row| row.get(0))
}

/// Whether a scan that has not ended moved a track onto this album from
/// another album that a track still holds ([`holds`]): the album may be
/// the new row of a retag the scan is half way through, which the old row
/// takes over when the retag has been read to the end. Catalog cleanup
/// asks before it retires a row into the album, which would stop that.
/// The surviving row of a retag already followed answers no.
pub(crate) fn awaits_retag(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM library_scan_runs run \
             CROSS JOIN local_tracks t \
             CROSS JOIN library_scan_album_moves m ON m.run_id = run.id \
               AND m.root_id = t.root_id AND m.relative_path = t.relative_path \
               AND m.local_track_id = t.id \
             WHERE run.terminal_at IS NULL AND t.local_album_id = ?1 \
             AND m.previous_album_id <> ?1 \
             AND EXISTS (SELECT 1 FROM local_tracks left_behind \
               WHERE left_behind.local_album_id = m.previous_album_id AND {}))",
            holds("left_behind", "run.id")
        ),
        params![album_id],
        |row| row.get(0),
    )
}

/// The automatic-policy albums this run moved tracks to from one album,
/// when no track holds that album any more ([`holds`]): nothing more can
/// arrive from it, so they are due their offer.
fn released_by(tx: &Connection, run_id: &str, album_id: &str) -> rusqlite::Result<Vec<String>> {
    tx.prepare_cached(&format!(
        "SELECT DISTINCT t.local_album_id FROM library_scan_album_moves m \
         JOIN local_tracks t ON t.id = m.local_track_id \
         JOIN library_scan_inventory i ON i.run_id = m.run_id AND i.root_id = m.root_id \
           AND i.relative_path = m.relative_path \
         WHERE m.run_id = ?1 AND m.previous_album_id = ?2 AND t.local_album_id <> ?2 \
         AND i.effective_policy = 'automatic' \
         AND NOT EXISTS (SELECT 1 FROM local_tracks left_behind \
           WHERE left_behind.local_album_id = ?2 AND {})",
        holds("left_behind", "?1")
    ))?
    .query_map(params![run_id, album_id], |row| row.get(0))?
    .collect()
}

/// One pass of what a run settles once it has ended: its name, which is
/// also its lane turns' name, and the work.
pub(super) struct EndPass {
    pub(super) name: &'static str,
    pub(super) work: EndWork,
}

/// How a pass's work is cut into lane turns. Each takes the run and the
/// time it ended.
pub(super) enum EndWork {
    /// One turn, answering how many albums it offered for identification.
    Whole(fn(&rusqlite::Transaction<'_>, &str, f64) -> rusqlite::Result<i64>),
    /// One album the run moved tracks off at a time, a chunk of albums a
    /// turn, then `tidy` in a turn of its own. Each album is a complete
    /// change and safe to run twice, so a stop resumes by running the pass
    /// again.
    ByAlbum {
        each: fn(&Connection, &str, &str, f64) -> rusqlite::Result<()>,
        tidy: fn(&Connection, &str, f64) -> rusqlite::Result<()>,
    },
}

/// Albums the run moved tracks off that are still live.
pub(super) fn albums_left(conn: &Connection, run_id: &str) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(
        "SELECT DISTINCT m.previous_album_id FROM library_scan_album_moves m \
         JOIN local_albums a ON a.id = m.previous_album_id \
         WHERE m.run_id = ?1 AND a.retired_into_album_id IS NULL ORDER BY 1",
    )?
    .query_map(params![run_id], |row| row.get(0))?
    .collect()
}

/// What a run settles once it has ended, however it ended, while its move
/// and inventory rows are still there. Each pass is its own write after
/// the run's state has changed, so a slow or failing pass never keeps the
/// run from ending; `SqliteScanStore::settle_ended_runs` runs them. In
/// order:
///
/// 1. albums retagged across scans ([`join_across_scans`]);
/// 2. the half of an album retagged in a first sitting takes the old
///    album's "leave unmanaged", its taken-over mark and the stamp
///    ([`carry_to_first_half`]);
/// 3. albums whose files were replaced by another album's on the same
///    edition are joined to it ([`join_replaced`]);
/// 4. the tracks that left an album are kept on record past the run
///    ([`keep_departures_of`], [`tidy_departures`]);
/// 5. files filed onto an album with an edition are placed on its tracks
///    by their own ids ([`place_scanned`]);
/// 6. missing tracks continue into the tracks that took their place
///    ([`continue_tracks`]);
/// 7. artist rows MusicBrainz proves to be one artist are merged, and a
///    row a retag emptied follows its credits where the rule allows
///    ([`converge_artists`]);
/// 8. new files that took the place of a hands-off album's are left
///    alone ([`hold_replacements`]);
/// 9. the albums the run held back are offered ([`offer_held_back`]);
/// 10. the folders the mass-missing guard held back are recorded, and
///     those the run walked as expected are let go ([`held_scopes::settle`]).
pub(super) const END_PASSES: [EndPass; 10] = [
    EndPass {
        name: "scan_end_join_across_scans",
        work: EndWork::Whole(join_across_scans),
    },
    EndPass {
        name: "scan_end_carry_to_first_half",
        work: EndWork::Whole(|tx, run_id, now| carry_to_first_half(tx, run_id, now).map(|()| 0)),
    },
    EndPass {
        name: "scan_end_join_replaced",
        work: EndWork::Whole(|tx, _, now| join_replaced(tx, Scope::Library, now).map(|()| 0)),
    },
    EndPass {
        name: "scan_end_keep_departures",
        work: EndWork::ByAlbum {
            each: keep_departures_of,
            tidy: tidy_departures,
        },
    },
    EndPass {
        name: "scan_end_place_scanned",
        work: EndWork::Whole(|tx, _, now| place_scanned(tx, now).map(|()| 0)),
    },
    EndPass {
        name: "scan_end_continue_tracks",
        work: EndWork::Whole(|tx, _, now| continue_tracks(tx, Scope::Library, now).map(|()| 0)),
    },
    EndPass {
        name: "scan_end_converge_artists",
        work: EndWork::Whole(|tx, _, now| converge_artists(tx, now).map(|()| 0)),
    },
    EndPass {
        name: "scan_end_hold_replacements",
        work: EndWork::Whole(|tx, run_id, now| hold_replacements(tx, run_id, now).map(|()| 0)),
    },
    EndPass {
        name: "scan_end_offer_held_back",
        work: EndWork::Whole(|tx, run_id, now| offer_held_back(tx, run_id, now)),
    },
    EndPass {
        name: "scan_end_held_scopes",
        work: EndWork::Whole(|tx, run_id, now| held_scopes::settle(tx, run_id, now).map(|()| 0)),
    },
];

/// Mark a run that just ended as owing its end passes, starting with the
/// first. Written in the same transaction as the run's terminal state.
pub(super) fn owe_end_passes(tx: &Connection, run_id: &str, now: f64) -> rusqlite::Result<()> {
    tx.prepare_cached(crate::scan::sql::OWE_END_PASSES.text())?
        .execute(params![
            run_id,
            END_PASSES[0].name,
            FailureCode::RunEndPending,
            now
        ])?;
    Ok(())
}

/// A track that left an album, by an earlier run (a departure on record)
/// or by this one, and sits on another album now.
struct Departed {
    track_id: String,
    /// The album it sits on.
    album_id: String,
    /// When the scan that saw it reach that album began, in milliseconds.
    arrived_ms: i64,
    /// It left the album in the run now ending.
    this_run: bool,
}

/// The tracks that left `album_id` and sit on another album now.
fn departed_from(
    tx: &Connection,
    run_id: &str,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<Vec<Departed>> {
    tx.prepare_cached(
        "WITH run AS (SELECT CAST(COALESCE(started_at, ?3) * 1000 AS INTEGER) AS start_ms \
           FROM library_scan_runs WHERE id = ?1), \
         moved_now AS (SELECT m.local_track_id, m.previous_album_id \
           FROM library_scan_album_moves m JOIN local_tracks t ON t.id = m.local_track_id \
           WHERE m.run_id = ?1 AND t.local_album_id <> m.previous_album_id), \
         left_it AS (SELECT local_track_id FROM library_album_departures \
           WHERE previous_album_id = ?2 \
           UNION SELECT local_track_id FROM library_scan_album_moves \
           WHERE run_id = ?1 AND previous_album_id = ?2) \
         SELECT t.id, t.local_album_id, \
         CASE WHEN EXISTS (SELECT 1 FROM moved_now WHERE moved_now.local_track_id = t.id) \
           THEN (SELECT start_ms FROM run) \
           ELSE COALESCE((SELECT MAX(d.arrived_at) FROM library_album_departures d \
             WHERE d.local_track_id = t.id), (SELECT start_ms FROM run)) END, \
         EXISTS (SELECT 1 FROM moved_now WHERE moved_now.local_track_id = t.id \
           AND moved_now.previous_album_id = ?2) \
         FROM left_it JOIN local_tracks t ON t.id = left_it.local_track_id \
         WHERE t.local_album_id <> ?2 ORDER BY t.id",
    )?
    .query_map(params![run_id, album_id, now], |row| {
        Ok(Departed {
            track_id: row.get(0)?,
            album_id: row.get(1)?,
            arrived_ms: row.get(2)?,
            this_run: row.get(3)?,
        })
    })?
    .collect()
}

/// The tracks that left `album_id` and sit on a piece of it, by the piece.
/// A row is a piece of the album when every other track row on it left
/// the album too, or is a file first indexed after the first of them
/// arrived there: the album split off, with files added since. A track
/// that went into an album with tracks of its own has changed albums and
/// says nothing about this one, so it is left out, for good. A track is
/// judged by when it reached the row it sits on, not by when it left the
/// album: one that moved on again does not make an older album look new.
fn pieces(
    tx: &Connection,
    run_id: &str,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<BTreeMap<String, Vec<Departed>>> {
    let mut by_album: BTreeMap<String, Vec<Departed>> = BTreeMap::new();
    for track in departed_from(tx, run_id, album_id, now)? {
        by_album
            .entry(track.album_id.clone())
            .or_default()
            .push(track);
    }
    let mut kept = BTreeMap::new();
    for (piece, tracks) in by_album {
        let first_ms = tracks.iter().map(|track| track.arrived_ms).min();
        let foreign: bool = tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM local_tracks o WHERE o.local_album_id = ?3 \
                 AND o.id NOT IN (SELECT local_track_id FROM library_album_departures \
                   WHERE previous_album_id = ?2 \
                   UNION SELECT local_track_id FROM library_scan_album_moves \
                   WHERE run_id = ?1 AND previous_album_id = ?2) \
                 AND (o.imported_at IS NULL OR o.imported_at * 1000 < ?4))",
            )?
            .query_row(params![run_id, album_id, piece, first_ms], |row| row.get(0))?;
        if !foreign {
            kept.insert(piece, tracks);
        }
    }
    Ok(kept)
}

/// Whether a track that left the album in an earlier run sits on a piece
/// of it ([`pieces`]). The follow within a run leaves such an album to the
/// end of the run: the album was split over two runs, or is being retagged
/// in two sittings.
fn has_counted_departure(
    tx: &Connection,
    run_id: &str,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let earlier: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM library_album_departures WHERE previous_album_id = ?1)",
        )?
        .query_row(params![album_id], |row| row.get(0))?;
    Ok(earlier
        && pieces(tx, run_id, album_id, now)?
            .values()
            .flatten()
            .any(|track| !track.this_run))
}

/// Whether a track row still holds the album in the run now ending.
fn held(tx: &Connection, run_id: &str, album_id: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(&format!(
        "SELECT EXISTS(SELECT 1 FROM local_tracks t WHERE t.local_album_id = ?2 AND {})",
        holds("t", "?1")
    ))?
    .query_row(params![run_id, album_id], |row| row.get(0))
}

fn retired(tx: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 \
         AND retired_into_album_id IS NOT NULL)",
    )?
    .query_row(params![album_id], |row| row.get(0))
}

/// Albums retagged across scans, decided once when a run ends: every album
/// a departure on record or a move of this run names, that is not retired
/// and that no track row holds ([`holds`]), read in one query.
///
/// - A pair already on record is left as it is, unless it is still open
///   and waiting to be tried again (a store error, live work, work on the
///   new row): a merge that succeeds removes it, a refusal gives it the
///   merge's reason.
/// - No track that left the album sits on a piece of it ([`pieces`]):
///   nothing.
/// - They all sit on one piece: the album is merged into it
///   ([`merge_retagged`]), files indexed there since coming along, and so
///   are the rows its tracks passed through on the way
///   ([`join_passed_through`]). When no more of them moved than stayed
///   behind, the merge waits until the rest are excluded or long gone
///   ([`waits_for_the_rest`]), and nothing is done or recorded until then.
///   A refusal is recorded and said once.
/// - They sit on more than one piece: when every track that left it in
///   this run sits on one of them, the album and that piece are recorded
///   as a pair (`tracks_elsewhere`); a set split in two over two scans, or
///   one track taken out and the rest renamed later. Otherwise nothing.
///
/// Returns how many albums the merges offered for identification.
fn join_across_scans(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    now: f64,
) -> rusqlite::Result<i64> {
    let albums: Vec<String> = tx
        .prepare_cached(&format!(
            "SELECT a.id FROM local_albums a WHERE a.retired_into_album_id IS NULL \
             AND a.id IN (SELECT previous_album_id FROM library_album_departures \
               UNION SELECT previous_album_id FROM library_scan_album_moves WHERE run_id = ?1) \
             AND NOT EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id AND {}) \
             ORDER BY a.id",
            holds("t", "?1")
        ))?
        .query_map(params![run_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut offered = 0;
    for album_id in &albums {
        if retired(tx, album_id)? {
            continue;
        }
        let pieces = pieces(tx, run_id, album_id, now)?;
        let (target, merge) = match pieces.len() {
            0 => continue,
            1 => {
                let Some((piece, tracks)) = pieces.iter().next() else {
                    continue;
                };
                let stayed: i64 = tx
                    .prepare_cached("SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?1")?
                    .query_row(params![album_id], |row| row.get(0))?;
                if waits_for_the_rest(tx, album_id, tracks.len() as i64, stayed, &[], now)? {
                    continue;
                }
                (piece.clone(), true)
            }
            _ => {
                let mut left_now = pieces
                    .iter()
                    .filter(|(_, tracks)| tracks.iter().any(|track| track.this_run))
                    .map(|(piece, _)| piece);
                match (left_now.next(), left_now.next()) {
                    (Some(piece), None) => (piece.clone(), false),
                    _ => continue,
                }
            }
        };
        if !merge {
            if tried(tx, album_id, &target)? {
                RetagPair {
                    old: album_id,
                    new: &target,
                    now,
                }
                .leave_apart(tx, RetagPairReason::TracksElsewhere, None)?;
            }
            continue;
        }
        if join_one(tx, album_id, &target, now, &mut offered)? {
            let tracks: Vec<&str> = pieces
                .values()
                .flatten()
                .map(|track| track.track_id.as_str())
                .collect();
            join_passed_through(tx, run_id, album_id, &target, &tracks, now, &mut offered)?;
        }
    }
    Ok(offered)
}

/// Whether the pair may be decided: it is not on record, or it is still
/// open and waiting to be tried again.
pub(crate) fn tried(tx: &Connection, old: &str, new: &str) -> rusqlite::Result<bool> {
    Ok(match retag_pairs::recorded(tx, old, new)? {
        None => true,
        Some((reason, state)) => state == RetagPairState::Open && reason.tried_again(),
    })
}

/// Merge `old` into `new` unless the pair is settled already, recording a
/// refusal. An album a person said is not the same as another (a pair they
/// dismissed) is merged nowhere by itself: a later rename of the other
/// album must not get round their answer. Returns whether it merged.
fn join_one(
    tx: &rusqlite::Transaction<'_>,
    old: &str,
    new: &str,
    now: f64,
    offered: &mut i64,
) -> rusqlite::Result<bool> {
    let dismissed: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM library_retag_pairs \
             WHERE old_album_id = ?1 AND state = ?2)",
        )?
        .query_row(params![old, RetagPairState::Dismissed.as_str()], |row| {
            row.get(0)
        })?;
    if dismissed || !tried(tx, old, new)? {
        return Ok(false);
    }
    match merge_retagged(tx, old, new, None, now, JoinCause::AcrossScans)? {
        MergeOutcome::Merged { offered: queued } => {
            *offered += i64::from(queued);
            Ok(true)
        }
        MergeOutcome::Refused { reason, error } => {
            RetagPair { old, new, now }.leave_apart(tx, reason, error.as_ref())?;
            Ok(false)
        }
    }
}

/// After `album_id` was merged into `piece`: the rows its tracks passed
/// through on the way there (the half of a retag seen by an earlier scan,
/// which then went on with the rest to a third name) are merged into
/// `piece` too, when they hold no track row at all and every track that
/// left them is one of `tracks`, sits on `piece` and had left `album_id`
/// before it left them. Otherwise such a row would stay behind empty, with
/// what was set on it between the sittings. An album the tracks were on
/// before they joined `album_id` is another album, and stays as it is.
fn join_passed_through(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    album_id: &str,
    piece: &str,
    tracks: &[&str],
    now: f64,
    offered: &mut i64,
) -> rusqlite::Result<()> {
    let mut rows = std::collections::BTreeSet::new();
    for track in tracks {
        let passed: Vec<String> = tx
            .prepare_cached(
                "SELECT previous_album_id FROM library_album_departures \
                 WHERE local_track_id = ?2 \
                 UNION SELECT previous_album_id FROM library_scan_album_moves \
                 WHERE run_id = ?1 AND local_track_id = ?2",
            )?
            .query_map(params![run_id, track], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows.extend(passed);
    }
    for row in rows {
        if row == album_id || row == piece || retired(tx, &row)? {
            continue;
        }
        let empty: bool = tx
            .prepare_cached(
                "SELECT NOT EXISTS(SELECT 1 FROM local_tracks WHERE local_album_id = ?1)",
            )?
            .query_row(params![row], |found| found.get(0))?;
        if !empty {
            continue;
        }
        let mut passed_through = true;
        for track in departed_from(tx, run_id, &row, now)? {
            let left_album = left_at(tx, run_id, &track.track_id, album_id, now)?;
            let left_row = left_at(tx, run_id, &track.track_id, &row, now)?;
            passed_through &= track.album_id == piece
                && tracks.contains(&track.track_id.as_str())
                && matches!((left_album, left_row), (Some(first), Some(then)) if first < then);
        }
        if passed_through {
            join_one(tx, &row, piece, now, offered)?;
        }
    }
    Ok(())
}

/// When the scan that saw the track leave `album_id` began, in
/// milliseconds: this run, or a departure on record.
fn left_at(
    tx: &Connection,
    run_id: &str,
    track_id: &str,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<Option<i64>> {
    tx.prepare_cached(
        "SELECT CASE WHEN EXISTS (SELECT 1 FROM library_scan_album_moves \
           WHERE run_id = ?1 AND local_track_id = ?2 AND previous_album_id = ?3) \
         THEN (SELECT CAST(COALESCE(started_at, ?4) * 1000 AS INTEGER) \
           FROM library_scan_runs WHERE id = ?1) \
         ELSE (SELECT departed_at FROM library_album_departures \
           WHERE local_track_id = ?2 AND previous_album_id = ?3) END",
    )?
    .query_row(params![run_id, track_id, album_id, now], |row| row.get(0))
}

/// The half of an album retagged in a first sitting, while the rest still
/// holds the old album, is left the way the old album is until the second
/// sitting joins the two: it takes the old album's taken-over mark, and,
/// unless a person chose a match for it, the old album's "leave unmanaged"
/// and the stamp when the old album has an edition someone chose and the
/// app has no record of its files ([`stamp_if_retagged_by_hand`]).
/// Otherwise automatic management could identify and write half an album
/// while it is being retagged by hand. A match a person chose for the new
/// half is their word for those files.
///
/// Once per pair, read before this run's departures are kept: for an album
/// that took tracks from an old album in this run, holds no track that
/// left that album in an earlier run, and holds nothing else but files this
/// run first indexed on a row this run made ([`holds_only_its_retag`]).
/// Lifted or handed back there, it is not put back. One audit action on
/// the receiving album says what it took.
///
/// A store error is recorded as a pair (`store_error`) and said once; the
/// end of each later run tries the pair again while the old album is still
/// held, and removes the pair when it lands. Once the old album is no
/// longer held, the merge decides the stamp instead.
fn carry_to_first_half(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let failed: Vec<(String, String)> = tx
        .prepare_cached(
            "SELECT p.old_album_id, p.new_album_id FROM library_retag_pairs p \
             JOIN local_albums old ON old.id = p.old_album_id \
             JOIN local_albums new ON new.id = p.new_album_id \
             WHERE p.state = ?1 AND p.reason = ?2 \
             AND old.retired_into_album_id IS NULL AND new.retired_into_album_id IS NULL \
             ORDER BY 1, 2",
        )?
        .query_map(
            params![
                RetagPairState::Open.as_str(),
                RetagPairReason::StoreError.as_str()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<rusqlite::Result<_>>()?;
    for (old, new) in &failed {
        if held(tx, run_id, old)? && carry_in_savepoint(tx, old, new, now)?.is_none() {
            tx.execute(
                "DELETE FROM library_retag_pairs WHERE old_album_id = ?1 AND new_album_id = ?2",
                params![old, new],
            )?;
        }
    }
    let pairs: Vec<(String, String)> = tx
        .prepare_cached(
            "SELECT DISTINCT m.previous_album_id, t.local_album_id \
             FROM library_scan_album_moves m JOIN local_tracks t ON t.id = m.local_track_id \
             JOIN local_albums old ON old.id = m.previous_album_id \
             JOIN local_albums new ON new.id = t.local_album_id \
             WHERE m.run_id = ?1 AND t.local_album_id <> m.previous_album_id \
             AND old.retired_into_album_id IS NULL AND new.retired_into_album_id IS NULL \
             AND NOT EXISTS (SELECT 1 FROM library_album_departures d \
               JOIN local_tracks earlier ON earlier.id = d.local_track_id \
               WHERE d.previous_album_id = m.previous_album_id \
               AND earlier.local_album_id = t.local_album_id) \
             ORDER BY 1, 2",
        )?
        .query_map(params![run_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (old, new) in &pairs {
        if !holds_only_its_retag(tx, run_id, new, old)? {
            continue;
        }
        if let Some(error) = carry_in_savepoint(tx, old, new, now)?
            && retag_pairs::record(
                tx,
                old,
                new,
                RetagPairReason::StoreError,
                (now * 1000.0) as i64,
            )?
        {
            tracing::warn!(
                album_id = old.as_str(),
                target = new.as_str(),
                %error,
                "first half of a retagged album not yet left as the old album was; \
                 tried again when a scan ends"
            );
        }
    }
    Ok(())
}

/// [`carry_one`] in a savepoint: the store's error, with nothing written,
/// or `None` when it landed.
fn carry_in_savepoint(
    tx: &rusqlite::Transaction<'_>,
    old: &str,
    new: &str,
    now: f64,
) -> rusqlite::Result<Option<rusqlite::Error>> {
    Ok(savepoint(tx, "first_half", || carry_one(tx, old, new, now))?.err())
}

/// Give `new` what [`carry_to_first_half`] says, with one audit action for
/// what it took.
fn carry_one(
    tx: &rusqlite::Transaction<'_>,
    old: &str,
    new: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let chosen_there = match_chosen(tx, new)?;
    let excluded = !chosen_there
        && tx.execute(
            "INSERT OR IGNORE INTO library_management_exclusions (local_album_id, reason, \
             excluded_by_user_id, excluded_at) SELECT ?2, reason, excluded_by_user_id, \
             excluded_at FROM library_management_exclusions WHERE local_album_id = ?1",
            params![old, new],
        )? > 0;
    let unmarked = !takeover::is_marked(tx, new)?;
    takeover::copy_mark(tx, old, new)?;
    let marked = unmarked && takeover::is_marked(tx, new)?;
    let stamped = !chosen_there && stamp_if_retagged_by_hand(tx, old, new, now)?;
    if excluded || marked || stamped {
        record_action(
            tx,
            CatalogAction {
                actor: "system",
                kind: "retag_first_half",
                album_id: new,
                job_id: None,
                before: serde_json::json!({ "from_album_id": old }),
                after: serde_json::json!({
                    "excluded": excluded,
                    "taken_over": marked,
                    "stamped_unmanaged": stamped,
                }),
                reason: "RETAG_FIRST_HALF_LEFT_AS_IT_WAS",
            },
            now,
        )?;
        bump_catalog(tx)?;
    }
    Ok(())
}

/// Keep on record, past the run, each track this run moved off
/// `album_id` (an album still there, [`albums_left`]), when it sits on a
/// piece of that album ([`pieces`]); a row already on record keeps the
/// time it left. Whether the row a track arrived on is a piece can only be
/// told now: a track that went into an album with tracks of its own has
/// changed albums and never counts for the album it left again, and once
/// it moves on, the next scan can no longer see that it did.
fn keep_departures_of(
    tx: &Connection,
    run_id: &str,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<()> {
    for track in pieces(tx, run_id, album_id, now)?.values().flatten() {
        if track.this_run {
            tx.prepare_cached(
                "INSERT OR IGNORE INTO library_album_departures (local_track_id, \
                 previous_album_id, departed_at, arrived_at) SELECT ?2, ?3, \
                 CAST(COALESCE(started_at, ?4) * 1000 AS INTEGER), \
                 CAST(COALESCE(started_at, ?4) * 1000 AS INTEGER) \
                 FROM library_scan_runs WHERE id = ?1",
            )?
            .execute(params![run_id, track.track_id, album_id, now])?;
        }
    }
    Ok(())
}

/// After [`keep_departures_of`] for every album: every row of a track this
/// run moved takes this run as the time it arrived where it is. Then, in
/// one statement, drop the rows whose track is back on the album it left,
/// and those of an album merged into another.
fn tidy_departures(tx: &Connection, run_id: &str, now: f64) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "UPDATE library_album_departures SET arrived_at = ( \
           SELECT CAST(COALESCE(started_at, ?2) * 1000 AS INTEGER) FROM library_scan_runs \
           WHERE id = ?1) \
         WHERE local_track_id IN (SELECT m.local_track_id FROM library_scan_album_moves m \
           JOIN local_tracks t ON t.id = m.local_track_id \
           WHERE m.run_id = ?1 AND t.local_album_id <> m.previous_album_id)",
    )?
    .execute(params![run_id, now])?;
    tx.prepare_cached(
        "DELETE FROM library_album_departures WHERE EXISTS (SELECT 1 FROM local_tracks t \
           WHERE t.id = library_album_departures.local_track_id \
           AND t.local_album_id = library_album_departures.previous_album_id) \
         OR EXISTS (SELECT 1 FROM local_albums a \
           WHERE a.id = library_album_departures.previous_album_id \
           AND a.retired_into_album_id IS NOT NULL)",
    )?
    .execute([])?;
    Ok(())
}

/// Offer the albums a run was still holding back when it ended, however
/// it ended: the retag was partial, the run stopped half way through it,
/// or a person emptied the old album by hand in between. Every album that
/// took a track from another in this run is offered; one offered already
/// is not queued twice. Returns how many were queued.
fn offer_held_back(conn: &Connection, run_id: &str, now: f64) -> rusqlite::Result<i64> {
    let albums: Vec<String> = conn
        .prepare_cached(
            "SELECT DISTINCT t.local_album_id FROM library_scan_album_moves m \
             JOIN local_tracks t ON t.id = m.local_track_id \
             JOIN library_scan_inventory i ON i.run_id = m.run_id AND i.root_id = m.root_id \
               AND i.relative_path = m.relative_path \
             WHERE m.run_id = ?1 AND t.local_album_id <> m.previous_album_id \
             AND i.effective_policy = 'automatic'",
        )?
        .query_map(params![run_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut enqueued = 0i64;
    for album_id in &albums {
        if offer_album(conn, album_id, (now * 1000.0) as i64)? {
            enqueued += 1;
        }
    }
    Ok(enqueued)
}

/// Inventory and counter part of a window: specific marks for the rows
/// the window wrote or failed, then every other pending row in the
/// window's key range marks skipped. Counter deltas land with the marks.
fn mark_window(
    tx: &Connection,
    window: &IndexWindow,
    indexed: &[(String, String, String)],
    failed: &[(String, String, &'static str)],
    extra_counters: &[(Counter, i64)],
) -> rusqlite::Result<()> {
    let mut mark_indexed = tx.prepare_cached(
        "UPDATE library_scan_inventory SET processing_state = 'indexed', local_track_id = ?4 \
         WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
    )?;
    for (root, path, track_id) in indexed {
        mark_indexed.execute(params![window.run_id, root, path, track_id])?;
    }
    let mut mark_failed = tx.prepare_cached(
        "UPDATE library_scan_inventory SET processing_state = ?4 \
         WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
    )?;
    for (root, path, state) in failed {
        mark_failed.execute(params![window.run_id, root, path, state])?;
    }
    let (after_root, after_path) = window
        .after
        .as_ref()
        .map(|(root, path)| (root.as_str(), path.as_str()))
        .unwrap_or(("", ""));
    tx.prepare_cached(
        "UPDATE library_scan_inventory SET processing_state = 'skipped' \
         WHERE run_id = ?1 AND processing_state = 'pending' \
         AND (root_id, relative_path) > (?2, ?3) AND (root_id, relative_path) <= (?4, ?5)",
    )?
    .execute(params![
        window.run_id,
        after_root,
        after_path,
        window.through.0,
        window.through.1
    ])?;
    let mut summed: HashMap<&'static str, i64> = HashMap::new();
    let window_counters = window
        .counters
        .iter()
        .map(|(_, counter, delta)| (counter, delta));
    for (counter, delta) in window_counters.chain(
        extra_counters
            .iter()
            .map(|(counter, delta)| (counter, delta)),
    ) {
        *summed.entry(counter.column()).or_insert(0) += delta;
    }
    for (column, delta) in summed {
        if delta != 0 {
            tx.execute(
                &format!("UPDATE library_scan_runs SET {column} = {column} + ?1 WHERE id = ?2"),
                params![delta, window.run_id],
            )?;
        }
    }
    Ok(())
}

/// Record the tracks of one album whose paths now hold other music: one
/// audit action naming each old track and the new track at its path, and
/// one log line.
fn record_other_music(
    tx: &rusqlite::Transaction<'_>,
    album_id: &str,
    pairs: &[OtherMusic],
    now: f64,
) -> rusqlite::Result<()> {
    let listed: Vec<serde_json::Value> = pairs
        .iter()
        .map(|pair| {
            serde_json::json!({
                "old_track_id": pair.old_track_id,
                "new_track_id": pair.new_track_id,
                "old_seconds": pair.old_seconds,
                "new_seconds": pair.new_seconds,
            })
        })
        .collect();
    record_action(
        tx,
        CatalogAction {
            actor: "system",
            kind: OTHER_MUSIC_KIND,
            album_id,
            job_id: None,
            before: serde_json::json!({}),
            after: serde_json::json!({ "pairs": listed }),
            reason: "OTHER_MUSIC_AT_PATH",
        },
        now,
    )?;
    tracing::info!(
        album_id,
        tracks = pairs.len(),
        "other music took the paths of an album's tracks; they stay on it as missing"
    );
    Ok(())
}

/// Record the tracks of one album whose files were replaced by new copies
/// of the same recordings: one audit action naming each track with its
/// length and stream before and after, and one log line.
fn record_new_copies(
    tx: &rusqlite::Transaction<'_>,
    album_id: &str,
    copies: &[NewCopy],
    now: f64,
) -> rusqlite::Result<()> {
    let listed: Vec<serde_json::Value> = copies
        .iter()
        .map(|copy| {
            serde_json::json!({
                "track_id": copy.track_id,
                "before": copy.before.json(),
                "after": copy.after.json(),
            })
        })
        .collect();
    record_action(
        tx,
        CatalogAction {
            actor: "system",
            kind: NEW_COPY_KIND,
            album_id,
            job_id: None,
            before: serde_json::json!({}),
            after: serde_json::json!({ "tracks": listed }),
            reason: "NEW_COPY_AT_PATH",
        },
        now,
    )?;
    tracing::info!(
        album_id,
        tracks = copies.len(),
        "new copies of the same recordings replaced files the app wrote; it forgot its record of them"
    );
    Ok(())
}

/// Write items, offer albums, and mark the window. With `isolate`, each
/// item runs in its own savepoint and a failing one is reported instead of
/// failing the window.
fn write_window(
    tx: &rusqlite::Transaction<'_>,
    window: &IndexWindow,
    isolate: bool,
) -> rusqlite::Result<WindowOutcome> {
    let mut outcome = WindowOutcome::default();
    let mut indexed: Vec<(String, String, String)> = Vec::new();
    let mut failed: Vec<(String, String, &'static str)> = window
        .failed
        .iter()
        .map(|(root, path, state)| (root.clone(), path.clone(), *state))
        .collect();
    let mut adjust: Vec<(Counter, i64)> = Vec::new();
    let mut offered: Vec<String> = Vec::new();
    // Albums with a changed file the app has a record of: automatic
    // management looks at them soon, to see who changed it.
    let mut changed: Vec<String> = Vec::new();
    let mut emptied: Vec<String> = Vec::new();
    let mut other_music: BTreeMap<String, Vec<OtherMusic>> = BTreeMap::new();
    let mut new_copies: BTreeMap<String, Vec<NewCopy>> = BTreeMap::new();
    for item in &window.items {
        let written = if isolate {
            savepoint(tx, "item", || {
                write_item(tx, Some(&window.run_id), item, None)
            })?
        } else {
            write_item(tx, Some(&window.run_id), item, None)
        };
        match written {
            Ok(Written {
                track_id,
                album_id,
                moved_from,
                other_music: replaced,
                new_copy,
            }) => {
                let changed_on_record = item.verdict_counter == Counter::Changed.column()
                    && takeover::has_record(tx, &track_id)?;
                // Other music on the album of a track the app wrote: the
                // album's files changed, as when the file itself changed.
                let replaced_on_record = match &replaced {
                    Some(old) => {
                        old.old_album_id == album_id && takeover::has_record(tx, &old.old_track_id)?
                    }
                    None => false,
                };
                if changed_on_record || replaced_on_record {
                    changed.push(album_id.clone());
                }
                if let Some(old) = replaced {
                    other_music
                        .entry(old.old_album_id.clone())
                        .or_default()
                        .push(old);
                }
                if let Some(copy) = new_copy {
                    new_copies
                        .entry(copy.album_id.clone())
                        .or_default()
                        .push(copy);
                }
                indexed.push((item.root_id.clone(), item.relative_path.clone(), track_id));
                if item.effective_policy == EffectivePolicy::Automatic {
                    offered.push(album_id);
                }
                emptied.extend(moved_from);
                outcome.committed += 1;
            }
            Err(error) if isolate => {
                // The coordinator counted this file as indexed; it was not.
                adjust.push((Counter::Indexed, -1));
                adjust.extend(Counter::parse(item.verdict_counter).map(|verdict| (verdict, -1)));
                adjust.push((Counter::Errored, 1));
                failed.push((item.root_id.clone(), item.relative_path.clone(), "failed"));
                outcome.failed.push((
                    item.root_id.clone(),
                    item.relative_path.clone(),
                    error.to_string(),
                ));
            }
            Err(error) => return Err(error),
        }
    }
    // Written before the follow below, so it and the join at the run end
    // read the same marker (`waits_for_the_rest`).
    for (album_id, pairs) in &other_music {
        record_other_music(tx, album_id, pairs, window.now)?;
    }
    // The record that would have queued these albums is gone.
    for (album_id, copies) in new_copies {
        record_new_copies(tx, &album_id, &copies, window.now)?;
        changed.push(album_id);
    }
    emptied.sort();
    emptied.dedup();
    for album_id in emptied {
        match follow_regrouped(tx, &window.run_id, &album_id, window.now)? {
            Some(gone) => {
                for offer in offered
                    .iter_mut()
                    .chain(changed.iter_mut())
                    .filter(|offer| **offer == gone)
                {
                    offer.clone_from(&album_id);
                }
            }
            None => {
                drop_if_empty(tx, &album_id)?;
                offered.extend(released_by(tx, &window.run_id, &album_id)?);
            }
        }
    }
    offered.sort();
    offered.dedup();
    let now_ms = (window.now * 1000.0) as i64;
    for album_id in &offered {
        if !held_back(tx, &window.run_id, album_id)? && offer_album(tx, album_id, now_ms)? {
            outcome.enqueued += 1;
        }
    }
    let (filled, relook) = fill_edition_tags(tx, &window.edition_fills)?;
    for album_id in &relook {
        if !held_back(tx, &window.run_id, album_id)? && offer_again(tx, album_id, now_ms)? {
            outcome.enqueued += 1;
        }
    }
    adjust.push((Counter::IdentificationEnqueued, outcome.enqueued as i64));
    // Every file the catalog did not take is on the run's failure list
    // with the window that gave it up, so a stop cannot lose the row.
    outcome.failed.extend(window.refused.iter().cloned());
    let mut record = tx.prepare_cached(crate::scan::sql::RECORD_FAILURE.text())?;
    for (root, path, error) in &outcome.failed {
        record.execute(params![
            window.run_id,
            root,
            path,
            FailureCode::CatalogCommitFailed,
            format!("The catalog could not store this file: {error}"),
            ScanPhase::Indexing.as_str(),
            window.now,
        ])?;
    }
    changed.sort();
    changed.dedup();
    for album_id in &changed {
        automatic::queue(tx, album_id, window.now)?;
    }
    // New music shows as the scan goes: a window that wrote a catalog row
    // moves the revision with it.
    if outcome.committed > 0 || filled > 0 {
        bump_catalog(tx)?;
    }
    mark_window(tx, window, &indexed, &failed, &adjust)?;
    Ok(outcome)
}

/// Commit one window: all rows in one bulk turn, or, when that fails on
/// a row, row by row so one bad file costs that file only. A turn the
/// database was too busy for is tried again until the run is asked to
/// stop.
pub(super) fn commit_window(
    db: &Db,
    stop: &RunStop,
    window: &IndexWindow,
) -> Result<WindowOutcome, ScanStoreError> {
    let write = |isolate: bool, name: &'static str| {
        retry_busy(stop, name, || {
            db.write_blocking(Lane::Bulk, name, |tx| {
                Ok::<_, ScanStoreError>(write_window(tx, window, isolate)?)
            })
        })
    };
    match write(false, "scan_index_window") {
        Err(ScanStoreError::Internal { message }) => {
            tracing::warn!(
                error = message,
                "index window failed whole; committing row by row"
            );
            write(true, "scan_index_window_rows")
        }
        other => other,
    }
}

/// Store the edition hints read from unchanged files whose rows held none.
/// Only those rows are written: the rest of a carried row stays as it is.
/// Returns how many rows were stored, and the albums without a settled
/// match that gained a hint, which identification looks at again.
fn fill_edition_tags(
    tx: &Connection,
    fills: &[EditionFill],
) -> rusqlite::Result<(usize, Vec<String>)> {
    let blank = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let mut filled = 0;
    let mut relook: Vec<String> = Vec::new();
    for fill in fills {
        let edition = &fill.edition;
        let (media, barcode, catalog_number, release_country) = (
            blank(&edition.media),
            blank(&edition.barcode),
            blank(&edition.catalog_number),
            blank(&edition.release_country),
        );
        let stored = tx
            .prepare_cached(crate::scan::sql::FILL_EDITION_TAGS.text())?
            .execute(params![
                fill.track_id,
                fill.stat_revision,
                media,
                barcode,
                catalog_number,
                release_country,
                edition.total_discs,
                edition.release_date,
            ])?;
        filled += stored;
        // Only what edition matching reads counts: it takes no release
        // date, and a disc total only when there is more than one disc.
        let hinted = !EditionHints::of_file(
            media.as_deref(),
            barcode.as_deref(),
            catalog_number.as_deref(),
            release_country.as_deref(),
            edition.total_discs,
            fill.disc_number,
        )
        .is_empty();
        if stored == 0 || !hinted || fill.effective_policy != EffectivePolicy::Automatic {
            continue;
        }
        if let Some(album_id) = tx
            .prepare_cached(crate::scan::sql::UNSETTLED_ALBUM_OF_TRACK.text())?
            .query_row(params![fill.track_id], |row| row.get::<_, String>(0))
            .optional()?
        {
            relook.push(album_id);
        }
    }
    relook.sort();
    relook.dedup();
    Ok((filled, relook))
}

/// The two halves of a window, split by path: each carries only its own
/// rows, their items, failed marks and counters, and its own range. `None`
/// for a window of one row.
pub(super) fn split_window(window: &IndexWindow) -> Option<(IndexWindow, IndexWindow)> {
    let mut rows: Vec<&(String, String)> = window.counters.iter().map(|(row, _, _)| row).collect();
    rows.sort_unstable();
    rows.dedup();
    if rows.len() < 2 {
        return None;
    }
    let middle = rows[rows.len() / 2 - 1].clone();
    let first_half = |root: &String, path: &String| (root, path) <= (&middle.0, &middle.1);
    let half = |first: bool| IndexWindow {
        run_id: window.run_id.clone(),
        after: if first {
            window.after.clone()
        } else {
            Some(middle.clone())
        },
        through: if first {
            middle.clone()
        } else {
            window.through.clone()
        },
        items: window
            .items
            .iter()
            .filter(|item| first_half(&item.root_id, &item.relative_path) == first)
            .cloned()
            .collect(),
        failed: window
            .failed
            .iter()
            .filter(|(root, path, _)| first_half(root, path) == first)
            .cloned()
            .collect(),
        refused: window
            .refused
            .iter()
            .filter(|(root, path, _)| first_half(root, path) == first)
            .cloned()
            .collect(),
        counters: window
            .counters
            .iter()
            .filter(|((root, path), _, _)| first_half(root, path) == first)
            .cloned()
            .collect(),
        edition_fills: window
            .edition_fills
            .iter()
            .filter(|fill| first_half(&fill.root_id, &fill.relative_path) == first)
            .cloned()
            .collect(),
        now: window.now,
    };
    Some((half(true), half(false)))
}

/// A window of one file whose catalog write ran out of time, given up:
/// it writes no catalog row, marks the file's inventory row failed and
/// moves its counts from indexed to errored, as a refused row does.
/// `None` when the window holds no file to give up.
pub(super) fn give_up_window(window: &IndexWindow) -> Option<IndexWindow> {
    let item = window.items.first()?;
    let row = (item.root_id.clone(), item.relative_path.clone());
    let mut given_up = IndexWindow {
        items: Vec::new(),
        ..window.clone()
    };
    given_up
        .failed
        .push((row.0.clone(), row.1.clone(), "failed"));
    given_up.refused.push((
        row.0.clone(),
        row.1.clone(),
        "the catalog write ran past its time budget".to_owned(),
    ));
    given_up.counters.push((row.clone(), Counter::Indexed, -1));
    if let Some(verdict) = Counter::parse(item.verdict_counter) {
        given_up.counters.push((row.clone(), verdict, -1));
    }
    given_up.counters.push((row, Counter::Errored, 1));
    Some(given_up)
}

/// Where a scan would file a stored track if no manual grouping held it,
/// read from the names the catalog row already carries. Used to reset a
/// manual grouping. Returns the album id and whether that album was
/// created here; `None` for an unknown track.
///
/// The album is found the way a scan finds it ([`resolve_album`]). When no
/// live album fits, an album with the track's key that a manual change
/// retired into the track's current album comes back (so a split followed
/// by a reset restores the original album id); otherwise a new album row
/// is made from the track's names, which the next scan refreshes.
pub(crate) fn automatic_album_for(
    tx: &Connection,
    track_id: &str,
    now: f64,
) -> rusqlite::Result<Option<(String, bool)>> {
    struct Stored {
        album_id: String,
        root_id: String,
        relative_path: String,
        release_mbid: Option<String>,
        album_title: String,
        album_artist: String,
        from_tags: bool,
        album_artist_sort: Option<String>,
        year: Option<i64>,
        genre: Option<String>,
        is_compilation: i64,
    }
    let stored = tx
        .query_row(
            "SELECT local_album_id, root_id, relative_path, embedded_release_mbid, album_title, \
             COALESCE(album_artist_name, ?2), \
             album_title_provenance = 'tag' AND album_artist_provenance = 'tag', \
             album_artist_sort, year, genre, is_compilation FROM local_tracks WHERE id = ?1",
            params![track_id, UNKNOWN_ARTIST],
            |row| {
                Ok(Stored {
                    album_id: row.get(0)?,
                    root_id: row.get(1)?,
                    relative_path: row.get(2)?,
                    release_mbid: row.get(3)?,
                    album_title: row.get(4)?,
                    album_artist: row.get(5)?,
                    from_tags: row.get(6)?,
                    album_artist_sort: row.get(7)?,
                    year: row.get(8)?,
                    genre: row.get(9)?,
                    is_compilation: row.get(10)?,
                })
            },
        )
        .optional()?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let directory = grouping_directory(&stored.relative_path);
    let names = AlbumNames {
        release_mbid: stored
            .release_mbid
            .as_deref()
            .map(str::trim)
            .filter(|mbid| !mbid.is_empty()),
        from_tags: stored.from_tags,
        directory: &directory,
        title_folded: fold_text(&stored.album_title),
        artist_folded: fold_text(&stored.album_artist),
    };
    let place = Place {
        run_id: None,
        root_id: &stored.root_id,
        directory: &directory,
    };
    let album_id = resolve_album(tx, &names, &place, Some(&stored.album_id), false)?;
    let exists: bool = tx
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1)")?
        .query_row(params![album_id], |row| row.get(0))?;
    if exists {
        return Ok(Some((album_id, false)));
    }
    let key = names.key();
    let retired: Option<String> = tx
        .query_row(
            "SELECT id FROM local_albums WHERE grouping_key = ?1 AND retired_into_album_id = ?2 \
             ORDER BY created_at, id LIMIT 1",
            params![key, stored.album_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(revived) = retired {
        tx.execute(
            "UPDATE local_albums SET retired_into_album_id = NULL, grouping_source = 'automatic', \
             grouping_locked = 0, updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1",
            params![revived, now],
        )?;
        tx.execute(
            "DELETE FROM local_album_aliases WHERE alias = ?1 AND kind = 'merged_album'",
            params![revived],
        )?;
        return Ok(Some((revived, false)));
    }
    // No artist id: the stored one is only the file's first album artist
    // id, which for a joined credit names one artist of it. The next scan
    // writes the ids the file holds.
    let credit = Credit {
        name: stored.album_artist.clone(),
        sort_name: stored.album_artist_sort.clone(),
        credited_name: stored.album_artist.clone(),
        join_phrase: String::new(),
        mbid: None,
    };
    let artist_id = album_artist_row(tx, track_id, &credit, now)?;
    tx.execute(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, album_artist_sort_name, \
         year, primary_genre, is_compilation, grouping_source, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'automatic', ?13, ?13)",
        params![
            album_id,
            stored.root_id,
            key,
            stored.album_title,
            fold_text(&stored.album_title),
            stored.album_artist,
            fold_text(&stored.album_artist),
            artist_id,
            stored.album_artist_sort,
            stored.year,
            stored.genre,
            stored.is_compilation,
            now,
        ],
    )?;
    tx.execute(
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, \
         credited_name, join_phrase) VALUES (?1, 0, ?2, 'main', ?3, '')",
        params![album_id, artist_id, stored.album_artist],
    )?;
    Ok(Some((album_id, true)))
}

/// The row a reset album's credit, which carries no id, lands on, as a
/// scan of the track's file would key it ([`artists::resolve`]): the row
/// of a credit of the track holding an id under the same folded name, else
/// the name row.
fn album_artist_row(
    tx: &Connection,
    track_id: &str,
    credit: &Credit,
    now: f64,
) -> rusqlite::Result<String> {
    let folded = fold_text(&credit.name);
    let shared = tx
        .prepare_cached(
            "SELECT c.local_artist_id, c.artist_mbid, c.credited_name \
             FROM local_track_artists c JOIN local_artists a ON a.id = c.local_artist_id \
             WHERE c.local_track_id = ?1 AND c.artist_mbid IS NOT NULL \
             AND a.retired_into_artist_id IS NULL ORDER BY c.position",
        )?
        .query_map(params![track_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .find(|(_, mbid, name)| !placeholder(mbid) && fold_text(name) == folded);
    match shared {
        Some((row, _, _)) => Ok(row),
        None => upsert_artist(tx, credit, now),
    }
}

/// The track id a file imported to `(root, relative path)` will get: the
/// row already at that path (a track whose file went missing) when
/// `reuse` allows it, else the stable id a scan would give a new path.
/// `None` when that stable id is taken by another row: the row at the
/// path when it is not reused, otherwise only a hash collision.
pub(crate) fn adoptable_track_id(
    conn: &Connection,
    root_id: &str,
    relative_path: &str,
    reuse: bool,
) -> rusqlite::Result<Option<String>> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM local_tracks WHERE root_id = ?1 AND relative_path = ?2",
            params![root_id, relative_path],
            |row| row.get(0),
        )
        .optional()?;
    if reuse && existing.is_some() {
        return Ok(existing);
    }
    let stable = stable_id(&format!("track:{root_id}:{relative_path}"));
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE id = ?1)",
        params![stable],
        |row| row.get(0),
    )?;
    Ok((!taken).then_some(stable))
}

/// Index one file an import just published, inside the publisher's
/// catalog transaction: the same rows a scan writes for a new file, so
/// the next scan finds nothing to change. A new row takes the id the plan
/// pinned; a row still at the path keeps its own, which the caller
/// checks. A file with a joined album goes there, not where its tags
/// point. Returns the track id.
pub(crate) fn adopt_file(
    tx: &Connection,
    item: &CommitIndexedItem,
    adopted: &Adopted<'_>,
) -> rusqlite::Result<String> {
    write_item(tx, None, item, Some(adopted)).map(|written| written.track_id)
}
