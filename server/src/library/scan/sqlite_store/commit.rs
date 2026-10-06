//! The index-window commit: catalog rows built from tags, the inventory
//! marks for every row the window covered, the run counters, and the
//! identify offers for the albums it touched, all in one transaction. A
//! crash either lands the whole window or none of it, so a resumed run
//! picks up at the first unprocessed row with nothing counted twice.
//!
//! Catalog rows follow v2's indexer: display names come from tags, then
//! the path parse, then placeholders, each with its provenance; albums
//! group by directory (disc folders folded) plus album title and album
//! artist; artists key by name and sort name.

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

/// Write one file's catalog rows. Returns the track id and its album id.
fn write_item(
    tx: &Connection,
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

    let grouping_key = format!(
        "{directory}\0{}\0{}",
        fold_text(&album_title),
        fold_text(&album_artist)
    );
    let album_id = stable_id(&format!("album:{}:{grouping_key}", item.root_id));
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
         ON CONFLICT (id) DO UPDATE SET title = excluded.title, \
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

    // The previous row (if any) keeps its id; a new path takes a stable
    // id unless a moved track already holds it.
    let previous: Option<(String, String)> = tx
        .prepare_cached(
            "SELECT id, local_album_id FROM local_tracks WHERE root_id = ?1 AND relative_path = ?2",
        )?
        .query_row(params![item.root_id, item.relative_path], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
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
        item.relative_path,
        item.relative_path,
        sha256_hex(&item.relative_path),
        item.size_bytes as i64,
        item.mtime_ns,
        exact_stat_revision(item.size_bytes, item.mtime_ns),
        tag_revision,
        now,
        title,
        fold_text(&title),
        track_artist,
        fold_text(&track_artist),
        album_title,
        fold_text(&album_title),
        album_artist,
        fold_text(&album_artist),
        raw_album,
        tag.album_artist.as_deref().unwrap_or("").trim(),
        tag.disc_number.max(1) as i64,
        track_number as i64,
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
    Ok((track_id, stored_album, moved_from))
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
            match write_item(tx, item) {
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
            write_item(tx, item)
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
    for album_id in emptied {
        drop_if_empty(tx, &album_id)?;
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
