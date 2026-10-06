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
//! track's file just went away continues that track (same recording MBID,
//! or same album key, disc, track number, title and duration), so
//! favorites, history and playlists follow a moved file. When every track
//! of an album is retagged into a new album in one run, the old row takes
//! the new name instead of being left behind.

use super::*;
use crate::library::identify::sqlite::offer_album;
use crate::library::scan::naming::{grouping_directory, parse_names_for_row};
use crate::library::scan::store::{IndexWindow, WindowOutcome};

/// The catalog's "Unknown Artist" sentinel (seeded by the baseline).
pub(super) const UNKNOWN_ARTIST_ID: &str = "00000000-0000-4000-8000-000000000002";
const UNKNOWN_ARTIST: &str = "Unknown Artist";

/// A stable UUID-shaped id from a namespaced key.
fn stable_id(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_custom_bytes(bytes)
        .into_uuid()
        .to_string()
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// One artist credit as the catalog stores it.
struct Credit {
    name: String,
    sort_name: Option<String>,
    credited_name: String,
    join_phrase: String,
}

fn artist_id(credit: &Credit) -> String {
    if credit.name == UNKNOWN_ARTIST {
        return UNKNOWN_ARTIST_ID.to_owned();
    }
    stable_id(&format!(
        "artist:{}:{}",
        credit.name,
        credit.sort_name.as_deref().unwrap_or("")
    ))
}

fn credits(
    from_tags: &[crate::library::tags::AudioArtistCredit],
    fallback: &str,
    fallback_sort: Option<&str>,
) -> Vec<Credit> {
    let mut out: Vec<Credit> = from_tags
        .iter()
        .filter(|credit| !credit.name.trim().is_empty())
        .map(|credit| Credit {
            name: credit.name.trim().to_owned(),
            sort_name: credit.sort_name.clone(),
            credited_name: credit
                .credited_name
                .clone()
                .unwrap_or_else(|| credit.name.trim().to_owned()),
            join_phrase: credit.join_phrase.clone(),
        })
        .collect();
    if out.is_empty() {
        out.push(Credit {
            name: fallback.to_owned(),
            sort_name: fallback_sort.map(str::to_owned),
            credited_name: fallback.to_owned(),
            join_phrase: String::new(),
        });
    }
    out
}

fn upsert_artist(tx: &Connection, credit: &Credit, now: f64) -> rusqlite::Result<String> {
    let id = artist_id(credit);
    let folded = fold_text(&credit.name);
    tx.prepare_cached(
        "INSERT INTO local_artists (id, display_name, sort_name, folded_name, normalized_name, \
         kind, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4, 'group', ?5, ?5) \
         ON CONFLICT (id) DO NOTHING",
    )?
    .execute(params![id, credit.name, credit.sort_name, folded, now])?;
    Ok(id)
}

/// Separator inside name-based album keys; never part of a folded name.
const KEY_SEPARATOR: char = '\u{1f}';

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
    /// The key an album is found by wherever its files sit: the release
    /// MBID when the file carries one, else the folded album artist and
    /// title from tags. Names parsed from the path (or placeholders) only
    /// mean something next to that path, so those keys keep the folder.
    /// Migration 0013 writes the same keys for albums indexed before it.
    fn key(&self) -> String {
        match self.release_mbid {
            Some(mbid) => format!("mbid:{}", mbid.to_lowercase()),
            None => self.name_key(),
        }
    }

    fn name_key(&self) -> String {
        if self.from_tags {
            format!(
                "tag:{}{KEY_SEPARATOR}{}",
                self.artist_folded, self.title_folded
            )
        } else {
            format!(
                "{}\0{}\0{}",
                self.directory, self.title_folded, self.artist_folded
            )
        }
    }
}

/// Where a track's group lives: its root and grouping directory.
struct Place<'a> {
    run_id: &'a str,
    root_id: &'a str,
    directory: &'a str,
}

/// A track still counts as present unless it is missing or this run
/// walked its folder to the end without seeing it.
const TRACK_PRESENT: &str = "t.availability = 'indexed' \
    AND NOT (EXISTS (SELECT 1 FROM library_scan_run_scopes s WHERE s.run_id = ?1 \
      AND s.root_id = t.root_id AND s.discovery_state = 'completed' \
      AND (s.relative_path = '.' OR t.relative_path = s.relative_path \
        OR substr(t.relative_path, 1, length(s.relative_path) + 1) = s.relative_path || '/')) \
    AND NOT EXISTS (SELECT 1 FROM library_scan_inventory i WHERE i.run_id = ?1 \
      AND i.root_id = t.root_id AND i.relative_path = t.relative_path))";

/// Where an album's present tracks are, seen from one folder: (some are
/// in this folder, some are anywhere else).
fn album_presence(
    tx: &Connection,
    place: &Place<'_>,
    album_id: &str,
) -> rusqlite::Result<(bool, bool)> {
    let mut stmt = tx.prepare_cached(&format!(
        "SELECT t.root_id, t.relative_path FROM local_tracks t \
         WHERE t.local_album_id = ?2 AND {TRACK_PRESENT}"
    ))?;
    let mut rows = stmt.query(params![place.run_id, album_id])?;
    let (mut here, mut elsewhere) = (false, false);
    while let Some(row) = rows.next()? {
        let root: String = row.get(0)?;
        let path: String = row.get(1)?;
        if root == place.root_id && grouping_directory(&path) == place.directory {
            here = true;
        } else {
            elsewhere = true;
        }
    }
    Ok((here, elsewhere))
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
/// 3. an album with the key that has no present tracks anywhere, whose
///    files moved here or were retagged (the oldest first);
/// 4. else a new album.
fn resolve_album(
    tx: &Connection,
    names: &AlbumNames<'_>,
    place: &Place<'_>,
    previous: Option<&str>,
) -> rusqlite::Result<String> {
    let key = names.key();
    if let Some(previous) = previous {
        let same: bool = tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 AND grouping_key = ?2)",
            )?
            .query_row(params![previous, key], |row| row.get(0))?;
        if same && !album_presence(tx, place, previous)?.1 {
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
        let (here, elsewhere) = album_presence(tx, place, &id)?;
        if here {
            return Ok(id);
        }
        if exact && !elsewhere && takeover.is_none() {
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

/// A track is gone when it is already marked missing, or when this run
/// walked its folder to the end without seeing the file.
const TRACK_GONE: &str = "(t.availability = 'missing' OR (t.availability = 'indexed' \
    AND EXISTS (SELECT 1 FROM library_scan_run_scopes s WHERE s.run_id = ?1 \
      AND s.root_id = t.root_id AND s.discovery_state = 'completed' \
      AND (s.relative_path = '.' OR t.relative_path = s.relative_path \
        OR substr(t.relative_path, 1, length(s.relative_path) + 1) = s.relative_path || '/')) \
    AND NOT EXISTS (SELECT 1 FROM library_scan_inventory i WHERE i.run_id = ?1 \
      AND i.root_id = t.root_id AND i.relative_path = t.relative_path)))";

/// Durations closer than this are the same recording.
const DURATION_TOLERANCE_SECONDS: f64 = 1.0;

/// The gone track a newly seen file continues, as (track id, album id):
/// same recording MBID, or else same album names, disc, track number,
/// title and duration. Only an unambiguous match counts; among several
/// recording matches, the one on the same album, disc and track wins.
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
    if candidates.len() > 1 {
        candidates.retain(|candidate| candidate.same_place);
    }
    Ok(match candidates.as_slice() {
        [only] => Some((only.id.clone(), only.album_id.clone())),
        _ => None,
    })
}

/// Write one file's catalog rows. Returns the track id, its album id, and
/// the album it left, if it changed albums.
fn write_item(
    tx: &Connection,
    run_id: &str,
    item: &CommitIndexedItem,
) -> rusqlite::Result<(String, String, Option<String>)> {
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

    let album_credits = credits(
        &tag.album_artists,
        &album_artist,
        tag.album_artist_sort.as_deref(),
    );
    let track_credits = credits(&tag.artists, &track_artist, tag.artist_sort.as_deref());
    let mut album_artist_ids = Vec::with_capacity(album_credits.len());
    for credit in &album_credits {
        album_artist_ids.push(upsert_artist(tx, credit, now)?);
    }
    let mut track_artist_ids = Vec::with_capacity(track_credits.len());
    for credit in &track_credits {
        // A track credit naming an album artist reuses that artist row.
        let shared = album_credits
            .iter()
            .position(|album| fold_text(&album.name) == fold_text(&credit.name));
        let id = match shared.and_then(|index| album_artist_ids.get(index)) {
            Some(id) => id.clone(),
            None => upsert_artist(tx, credit, now)?,
        };
        track_artist_ids.push(id);
    }

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

    // The row at this path keeps its id. A new path continues a track
    // whose file went away when the keys agree: the old row moves here
    // and the upsert below refreshes it.
    let mut previous: Option<(String, String)> = tx
        .prepare_cached(
            "SELECT id, local_album_id FROM local_tracks WHERE root_id = ?1 AND relative_path = ?2",
        )?
        .query_row(params![item.root_id, item.relative_path], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    if previous.is_none() {
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
    let album_id = resolve_album(
        tx,
        &names,
        &Place {
            run_id,
            root_id: &item.root_id,
            directory: &directory,
        },
        previous.as_ref().map(|(_, album)| album.as_str()),
    )?;
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
        fold_text(&album_title),
        album_artist,
        fold_text(&album_artist),
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
    let mut album_artist_rows = tx.prepare_cached(
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, \
         credited_name, join_phrase) VALUES (?1, ?2, ?3, 'main', ?4, ?5) \
         ON CONFLICT (local_album_id, position) DO UPDATE SET \
         local_artist_id = excluded.local_artist_id, credited_name = excluded.credited_name, \
         join_phrase = excluded.join_phrase",
    )?;
    for (position, (credit, id)) in album_credits.iter().zip(&album_artist_ids).enumerate() {
        album_artist_rows.execute(params![
            album_id,
            position as i64,
            id,
            credit.credited_name,
            credit.join_phrase,
        ])?;
    }

    // A new path takes a stable id unless a moved track already holds it.
    let track_id = match &previous {
        Some((id, _)) => id.clone(),
        None => {
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
    let tag_revision = {
        let digest = Sha256::digest(format!("{tag:?}").as_bytes());
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let file_format =
        crate::library::tags::format_for_path(std::path::Path::new(&item.relative_path))
            .map(|format| format.as_str().to_owned())
            .unwrap_or_else(|_| "unknown".to_owned());
    let policy = policy_to_str(item.effective_policy);
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
         availability = 'indexed', missing_since = NULL, excluded_at = NULL",
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

    tx.prepare_cached("DELETE FROM local_track_artists WHERE local_track_id = ?1")?
        .execute(params![track_id])?;
    let mut track_artist_rows = tx.prepare_cached(
        "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role, \
         credited_name, join_phrase) VALUES (?1, ?2, ?3, 'main', ?4, ?5)",
    )?;
    for (position, (credit, id)) in track_credits.iter().zip(&track_artist_ids).enumerate() {
        track_artist_rows.execute(params![
            track_id,
            position as i64,
            id,
            credit.credited_name,
            credit.join_phrase,
        ])?;
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
    if let Some(left) = &moved_from {
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
    Ok((track_id, stored_album, moved_from))
}

/// An album this run emptied: when every track it lost went to one album,
/// and that album holds nothing but those tracks and nothing of its own
/// (identity, reviews), the whole album was retagged together. The old
/// row then takes over the new name and key, keeping its id, identity,
/// reviews and pins, and the new row goes. Returns the album that went.
fn follow_regrouped(
    tx: &Connection,
    run_id: &str,
    album_id: &str,
) -> rusqlite::Result<Option<String>> {
    let remaining: i64 = tx
        .prepare_cached("SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?1")?
        .query_row(params![album_id], |row| row.get(0))?;
    if remaining > 0 {
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
    let foreign: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM local_tracks t WHERE t.local_album_id = ?2 \
             AND NOT EXISTS (SELECT 1 FROM library_scan_album_moves m WHERE m.run_id = ?1 \
             AND m.local_track_id = t.id AND m.previous_album_id = ?3)) \
             OR EXISTS(SELECT 1 FROM local_album_external_identities WHERE local_album_id = ?2) \
             OR EXISTS(SELECT 1 FROM library_identify_reviews WHERE local_album_id = ?2)",
        )?
        .query_row(params![run_id, target, album_id], |row| row.get(0))?;
    if foreign {
        return Ok(None);
    }
    tx.execute("SAVEPOINT follow_album", [])?;
    let followed = (|| {
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
        tx.execute(
            "DELETE FROM local_album_artwork WHERE local_album_id = ?1",
            params![target],
        )?;
        tx.execute(
            "DELETE FROM library_identify_jobs WHERE local_album_id = ?1",
            params![target],
        )?;
        tx.execute("DELETE FROM local_albums WHERE id = ?1", params![target])
    })();
    match followed {
        Ok(_) => {
            tx.execute("RELEASE follow_album", [])?;
            Ok(Some(target.clone()))
        }
        Err(error) => {
            tracing::debug!(%error, album_id, target, "regrouped album still referenced; kept both");
            tx.execute("ROLLBACK TO follow_album", [])?;
            tx.execute("RELEASE follow_album", [])?;
            Ok(None)
        }
    }
}

/// Drop an album left with no tracks, with its scan-owned joins. Rows that
/// reference the album elsewhere (identities, reviews) keep it.
fn drop_if_empty(tx: &Connection, album_id: &str) -> rusqlite::Result<()> {
    let remaining: i64 = tx
        .prepare_cached("SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?1")?
        .query_row(params![album_id], |row| row.get(0))?;
    if remaining > 0 {
        return Ok(());
    }
    tx.execute("SAVEPOINT drop_album", [])?;
    let dropped = (|| {
        tx.execute(
            "DELETE FROM local_album_artists WHERE local_album_id = ?1",
            params![album_id],
        )?;
        tx.execute(
            "DELETE FROM local_album_artwork WHERE local_album_id = ?1",
            params![album_id],
        )?;
        tx.execute("DELETE FROM local_albums WHERE id = ?1", params![album_id])
    })();
    match dropped {
        Ok(_) => tx.execute("RELEASE drop_album", []).map(|_| ()),
        Err(error) => {
            tracing::debug!(%error, album_id, "emptied album still referenced; kept");
            tx.execute("ROLLBACK TO drop_album", [])?;
            tx.execute("RELEASE drop_album", []).map(|_| ())
        }
    }
}

/// Inventory and counter part of a window: specific marks for the rows
/// the window wrote or failed, then every other pending row in the
/// window's key range marks skipped. Counter deltas land with the marks.
fn mark_window(
    tx: &Connection,
    window: &IndexWindow,
    indexed: &[(String, String, String)],
    failed: &[(String, String, &'static str)],
    extra_counters: &[(&'static str, i64)],
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
    for (name, delta) in window.counters.iter().chain(extra_counters) {
        if let Some(column) = counter_column(name) {
            *summed.entry(column).or_insert(0) += delta;
        }
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

/// Write items, offer albums, and mark the window. With `isolate`, each
/// item runs in its own savepoint and a failing one is reported instead of
/// failing the window.
fn write_window(
    tx: &Connection,
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
    let mut adjust: Vec<(&'static str, i64)> = Vec::new();
    let mut offered: Vec<String> = Vec::new();
    let mut emptied: Vec<String> = Vec::new();
    for item in &window.items {
        let written = if isolate {
            tx.execute("SAVEPOINT item", [])?;
            match write_item(tx, &window.run_id, item) {
                Ok(written) => {
                    tx.execute("RELEASE item", [])?;
                    Ok(written)
                }
                Err(error) => {
                    tx.execute("ROLLBACK TO item", [])?;
                    tx.execute("RELEASE item", [])?;
                    Err(error)
                }
            }
        } else {
            write_item(tx, &window.run_id, item)
        };
        match written {
            Ok((track_id, album_id, moved_from)) => {
                indexed.push((item.root_id.clone(), item.relative_path.clone(), track_id));
                if item.effective_policy == EffectivePolicy::Automatic {
                    offered.push(album_id);
                }
                emptied.extend(moved_from);
                outcome.committed += 1;
            }
            Err(error) if isolate => {
                // The coordinator counted this file as indexed; it was not.
                adjust.push((counter_names::INDEXED, -1));
                adjust.push((item.verdict_counter, -1));
                adjust.push((counter_names::ERRORED, 1));
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
    emptied.sort();
    emptied.dedup();
    for album_id in emptied {
        match follow_regrouped(tx, &window.run_id, &album_id)? {
            Some(gone) => {
                for offer in offered.iter_mut().filter(|offer| **offer == gone) {
                    offer.clone_from(&album_id);
                }
            }
            None => drop_if_empty(tx, &album_id)?,
        }
    }
    offered.sort();
    offered.dedup();
    let now_ms = (window.now * 1000.0) as i64;
    for album_id in &offered {
        if offer_album(tx, album_id, now_ms)? {
            outcome.enqueued += 1;
        }
    }
    adjust.push((
        counter_names::IDENTIFICATION_ENQUEUED,
        outcome.enqueued as i64,
    ));
    mark_window(tx, window, &indexed, &failed, &adjust)?;
    Ok(outcome)
}

/// Commit one window: all rows in one transaction, or, when that fails
/// on a row, row by row so one bad file costs that file only.
pub(super) fn commit_window(
    conn: &mut Connection,
    window: &IndexWindow,
) -> rusqlite::Result<WindowOutcome> {
    let whole = retry_on_busy("commit_window", || {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let outcome = write_window(&tx, window, false)?;
        tx.commit()?;
        Ok(outcome)
    });
    match whole {
        Ok(outcome) => Ok(outcome),
        Err(error) if is_transient(&error) => Err(error),
        Err(error) => {
            tracing::warn!(%error, "index window failed whole; committing row by row");
            retry_on_busy("commit_window_rows", || {
                let tx =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let outcome = write_window(&tx, window, true)?;
                tx.commit()?;
                Ok(outcome)
            })
        }
    }
}
