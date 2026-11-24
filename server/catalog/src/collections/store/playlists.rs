//! Playlist rows over `catalog_playlists`, `catalog_playlist_tracks` and
//! `catalog_playlist_covers`.
//!
//! One playlist model serves the native routes, both compat protocols and
//! remote playlist imports. Entries that point at a library file carry its
//! local track id in `library_file_id`; compat clients see only those.
//!
//! Every call names whose it is. [`PlaylistStore::owned_by`] reads and
//! writes one user's own playlists: each write checks the owner in its own
//! statement or transaction, so a write to another user's playlist changes
//! nothing and answers [`Written::Missing`]. [`PlaylistStore::editable_by`]
//! changes the songs of a playlist the user owns or is shared with to
//! edit, checked the same way. [`PlaylistStore::visible_to`]
//! reads what one user may see: their own playlists, public ones and those
//! shared with them, with the line drawn in SQL, and lets them leave one
//! shared with them. [`PlaylistStore::strays`] lets an admin delete a
//! playlist with no owner, and nothing else.
//!
//! Positions are dense and zero based. `UNIQUE(playlist_id, position)`
//! is checked row by row during an UPDATE, so every reorder writes the new
//! order in two passes: first to negative slots, then to the final ones.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::super::db::{CollectionsDb, StoreError};
use super::super::entry_cover::entry_cover;
use super::super::identify_job::{IDENTIFY, IdentifyJob, identify_key};
use super::super::models::{AlbumPick, FiledKind, PlaylistMember, PlaylistTrack, SourceAlbumKind};
use super::super::sql::{self, PlaylistExportRow};
use droppedneedle_auth::users::roles::Admin;
use droppedneedle_kernel::db::{DbError, fold_text};
use droppedneedle_kernel::jobs::queue::JobQueue;

/// How many track covers a summary carries.
const SUMMARY_COVER_COUNT: i64 = 4;

/// One playlist with its list aggregates.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistRow {
    /// Playlist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Owning user id. A playlist from v2 with no owner has none.
    pub owner_id: Option<String>,
    /// Owner display name, when the owner still exists.
    pub owner_name: Option<String>,
    /// Public playlists are visible to every user.
    pub is_public: bool,
    /// Import provenance (`<source>:<id>`).
    pub source_ref: Option<String>,
    /// Creation time, epoch seconds.
    pub created_at: u64,
    /// Last mutation time, epoch seconds.
    pub updated_at: u64,
    /// True when an uploaded cover exists.
    pub has_cover: bool,
    /// Every entry.
    pub track_count: usize,
    /// Summed entry durations, seconds, when any entry has one.
    pub total_duration: Option<f64>,
    /// Entries pointing at a library file.
    pub streamable_count: usize,
    /// Summed durations of those entries, seconds.
    pub streamable_duration: f64,
    /// True when the playlist is shared with the user reading it.
    pub shared_with_caller: bool,
    /// True when it is shared with the user reading it to edit.
    pub shared_to_edit: bool,
    /// True when the playlist is shared with anyone.
    pub has_members: bool,
    /// Up to four entry covers in position order.
    pub cover_urls: Vec<String>,
}

/// One row of the full list: a playlist the caller may read, or only the
/// existence of another user's private one.
#[derive(Debug, Clone, PartialEq)]
pub enum Listed {
    /// A playlist the caller may read.
    Readable(PlaylistRow),
    /// Another user's private playlist, or a private one with no owner.
    Hidden {
        /// Playlist id.
        id: String,
        /// Owning user id, when it has an owner.
        owner_id: Option<String>,
        /// Every entry.
        track_count: usize,
        /// Owner display name, when the owner still exists.
        owner_name: Option<String>,
    },
}

/// One entry to insert.
#[derive(Debug, Clone, Default)]
pub struct NewEntry {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album name.
    pub album_name: String,
    /// Album id.
    pub album_id: Option<String>,
    /// Artist id.
    pub artist_id: Option<String>,
    /// Source-specific track id.
    pub track_source_id: Option<String>,
    /// Cover URL; stored only when `entry_cover` keeps it.
    pub cover_url: Option<String>,
    /// Preferred source type.
    pub source_type: String,
    /// Sources carrying the track.
    pub available_sources: Option<Vec<String>>,
    /// Audio format.
    pub format: Option<String>,
    /// Track number.
    pub track_number: Option<i32>,
    /// Disc number.
    pub disc_number: Option<i32>,
    /// Duration, seconds.
    pub duration: Option<f64>,
    /// Plex rating key.
    pub plex_rating_key: Option<String>,
    /// Local track id, when the entry is a library file.
    pub library_file_id: Option<String>,
    /// The recording `source_type` gave for this entry, lowercase, when
    /// known.
    pub recording_mbid: Option<String>,
    /// The ISRC a playlist file gave for an entry the library lacked,
    /// uppercased.
    pub isrc: Option<String>,
}

/// What a playlist's source said about one song's album, kept with the
/// song so its album can be looked for again without asking the source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceAlbum {
    /// The source's album id.
    pub source_id: Option<String>,
    /// Album title.
    pub title: String,
    /// The album's first artist.
    pub artist: String,
    /// What the source files the album as.
    pub kind: Option<SourceAlbumKind>,
    /// Tracks on the album, as the source counts them.
    pub tracks: Option<u32>,
}

/// One song taken from a playlist's source and the entry it becomes.
#[derive(Debug, Clone)]
pub struct SourceEntry {
    /// The source's id for the song.
    pub song_id: String,
    /// What the source said about its album.
    pub album: SourceAlbum,
    /// The entry.
    pub entry: NewEntry,
}

/// One song a playlist took from its source that a sync has not followed
/// up, with its entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSourceSong {
    /// The source's id for the song.
    pub song_id: String,
    /// Its entry.
    pub entry_id: String,
    /// Track title.
    pub track_name: String,
    /// Artist text, every credit included.
    pub artist_name: String,
    /// Album name on the entry.
    pub album_name: String,
    /// The release group the entry was given, when one was.
    pub album_id: Option<String>,
    /// The recording the entry carries, when it carries one.
    pub recording_mbid: Option<String>,
    /// The song's ISRC.
    pub isrc: Option<String>,
    /// Length, milliseconds.
    pub duration_ms: Option<i64>,
    /// The entry plays already: a library file or a source of its own.
    pub playable: bool,
    /// What the source said about its album.
    pub album: SourceAlbum,
    /// How its album was chosen, when not as the source files it.
    pub pick: Option<AlbumPick>,
    /// Looks for an album made so far.
    pub looks: i64,
}

/// Where a sync puts the songs it adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePlace {
    /// After every entry, in the source's order.
    End,
    /// Before every entry, in the source's order: Liked Songs, newest
    /// first.
    Top,
}

/// What one sync write took from the source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceTaken {
    /// The new entries, in the order they were given.
    pub added: Vec<String>,
    /// Songs found already on the playlist, from an import made before its
    /// songs were kept.
    pub seeded: usize,
}

/// Keep that playlist `playlist_id` took `song` as `entry_id` at `now`.
fn insert_source_song(
    tx: &Transaction,
    playlist_id: &str,
    song: &SourceEntry,
    entry_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::INSERT_SOURCE_SONG.text(),
        params![
            playlist_id,
            song.song_id,
            entry_id,
            now,
            song.album.source_id,
            song.album.title,
            song.album.artist,
            song.album.kind,
            song.album.tracks,
        ],
    )?;
    Ok(())
}

/// The entries of a playlist imported before its songs were kept, each
/// paired with the source song it already is: the first entry in playlist
/// order with exactly that title and artist text, one to one. Names are
/// compared only to tell which entry a song already is, never to pick an
/// album. Answers the songs still to add, in order.
fn seed_source_songs<'a>(
    tx: &Transaction,
    playlist_id: &str,
    songs: &'a [SourceEntry],
    now: i64,
) -> rusqlite::Result<(Vec<&'a SourceEntry>, usize)> {
    let mut unclaimed: Vec<sql::EntryNamed> = tx
        .prepare(sql::ENTRY_NAMES.text())?
        .query_map(params![playlist_id], sql::EntryNamed::read)?
        .collect::<rusqlite::Result<_>>()?;
    let mut new = Vec::new();
    let mut seeded = 0;
    for song in songs {
        let same = unclaimed.iter().position(|named| {
            named.track_name == song.entry.track_name && named.artist_name == song.entry.artist_name
        });
        match same {
            Some(at) => {
                let named = unclaimed.remove(at);
                insert_source_song(tx, playlist_id, song, &named.entry_id, now)?;
                seeded += 1;
            }
            None => new.push(song),
        }
    }
    Ok((new, seeded))
}

/// A song an import kept that still waits for its album: no source, no
/// file and no album, with the names the file gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptEntry {
    /// Entry id.
    pub entry_id: String,
    /// Track title.
    pub title: String,
    /// Artist text, every credit included.
    pub artist: String,
    /// Album name.
    pub album: String,
    /// The ISRC the file gave, uppercased.
    pub isrc: Option<String>,
}

/// Uploaded cover bytes.
#[derive(Debug, Clone)]
pub struct CoverImage {
    /// Mime type.
    pub content_type: String,
    /// Image bytes.
    pub bytes: Vec<u8>,
}

/// A playlist as one write left it, without its entries. Times are epoch
/// seconds.
#[derive(Debug, Clone)]
pub struct SavedPlaylist {
    /// Playlist id.
    pub id: String,
    /// Name as stored.
    pub name: String,
    /// Public flag.
    pub is_public: bool,
    /// When it was made.
    pub created_at: u64,
    /// When it last changed.
    pub updated_at: u64,
}

/// One track a membership check asks about, by exact names.
#[derive(Debug, Clone)]
pub struct EntryNames<'a> {
    /// Track title.
    pub track_name: &'a str,
    /// Artist name.
    pub artist_name: &'a str,
    /// Album name.
    pub album_name: &'a str,
}

/// Outcome of a write that names a playlist or entry by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written<T> {
    /// The write happened.
    Done(T),
    /// The playlist is gone or not the caller's, or the entry is gone.
    Missing,
}

/// The playlist store. Every read and write goes through
/// [`Self::owned_by`] or [`Self::visible_to`].
#[derive(Clone, Debug)]
pub struct PlaylistStore {
    db: CollectionsDb,
}

/// One user's own playlists.
#[derive(Clone, Copy, Debug)]
pub struct OwnedPlaylists<'a> {
    store: &'a PlaylistStore,
    user_id: &'a str,
}

/// The playlists one user may change the songs of: their own, and those
/// shared with them to edit. Everything else about a playlist stays its
/// owner's, through [`OwnedPlaylists`].
#[derive(Clone, Copy, Debug)]
pub struct EditablePlaylists<'a> {
    store: &'a PlaylistStore,
    user_id: &'a str,
    /// Only the owner's own playlists, for work done for the owner with
    /// no request (Weekly Mix, Spotify import): editors never widen it.
    owner_only: bool,
}

/// What one user may read: their own playlists, public ones and those
/// shared with them.
#[derive(Clone, Copy, Debug)]
pub struct VisiblePlaylists<'a> {
    store: &'a PlaylistStore,
    user_id: &'a str,
}

/// Playlists with no owner, as an admin may act on them.
#[derive(Clone, Copy, Debug)]
pub struct StrayPlaylists<'a> {
    store: &'a PlaylistStore,
}

fn epoch(value: i64) -> u64 {
    value.max(0) as u64
}

fn count(value: i64) -> usize {
    value.max(0) as usize
}

fn playlist_from_row(row: &Row<'_>) -> rusqlite::Result<PlaylistRow> {
    Ok(PlaylistRow {
        id: row.get(0)?,
        name: row.get(1)?,
        owner_id: row.get(2)?,
        owner_name: row.get(3)?,
        is_public: row.get::<_, i64>(4)? != 0,
        source_ref: row.get(5)?,
        created_at: epoch(row.get(6)?),
        updated_at: epoch(row.get(7)?),
        has_cover: row.get::<_, i64>(8)? != 0,
        track_count: count(row.get(9)?),
        total_duration: row.get(10)?,
        streamable_count: count(row.get(11)?),
        streamable_duration: row.get(12)?,
        shared_with_caller: row.get::<_, Option<i64>>(13)?.is_some(),
        shared_to_edit: row.get::<_, Option<i64>>(13)? == Some(1),
        has_members: row.get::<_, i64>(14)? != 0,
        cover_urls: Vec::new(),
    })
}

fn track_from_row(row: &Row<'_>) -> rusqlite::Result<PlaylistTrack> {
    let sources: Option<String> = row.get(10)?;
    Ok(PlaylistTrack {
        id: row.get(0)?,
        position: count(row.get(1)?),
        track_name: row.get(2)?,
        artist_name: row.get(3)?,
        album_name: row.get(4)?,
        album_id: row.get(5)?,
        artist_id: row.get(6)?,
        track_source_id: row.get(7)?,
        cover_url: row
            .get::<_, Option<String>>(8)?
            .as_deref()
            .and_then(entry_cover),
        source_type: row.get(9)?,
        available_sources: sources.as_deref().and_then(decode_sources),
        format: row.get(11)?,
        track_number: row.get(12)?,
        disc_number: row.get(13)?,
        duration: row.get(14)?,
        created_at: epoch(row.get(15)?),
        plex_rating_key: row.get(16)?,
        library_file_id: row.get(17)?,
        recording_mbid: row.get(18)?,
    })
}

fn cover_from_row(row: &Row<'_>) -> rusqlite::Result<CoverImage> {
    Ok(CoverImage {
        content_type: row.get(0)?,
        bytes: row.get(1)?,
    })
}

/// Stored sources are a JSON string list (v2 shape). A malformed value
/// reads as unknown rather than failing the playlist.
fn decode_sources(raw: &str) -> Option<Vec<String>> {
    match serde_json::from_str::<Vec<String>>(raw) {
        Ok(sources) => Some(sources),
        Err(error) => {
            tracing::warn!(%error, "playlist entry has unreadable available_sources");
            None
        }
    }
}

fn encode_sources(sources: Option<&Vec<String>>) -> Option<String> {
    sources.map(|list| serde_json::Value::from(list.clone()).to_string())
}

/// A title as linking compares it: folded, typographic apostrophes read as
/// plain ones, and dashes and brackets as spaces, so "Opener - Live" keys
/// as "Opener (Live)" does.
fn title_key(title: &str) -> String {
    let spaced: String = fold_text(title)
        .chars()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{02bc}' => '\'',
            '-' | '\u{2010}'..='\u{2015}' | '\u{2212}' | '(' | ')' | '[' | ']' => ' ',
            other => other,
        })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// [`title_key`] for a title at an entry's own place on its album, where
/// the place narrows which song it is: "&" reads as "and", "/" as a space,
/// apostrophes as nothing, and a bracketed credit ("(feat. B)") is
/// dropped, so "A & B (feat. C)" keys as "A and B" does. Folds first, so
/// fullwidth forms read as their plain ones. Also says whether a credit
/// was dropped.
fn place_key(title: &str) -> (String, bool) {
    let folded = fold_text(title);
    let mut kept = String::with_capacity(folded.len());
    let mut dropped = false;
    let mut rest = folded.as_str();
    while let Some(open) = rest.find(['(', '[']) {
        let close = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(end) = rest[open..].find(close).map(|at| open + at) else {
            break;
        };
        kept.push_str(&rest[..open]);
        if is_credit(&rest[open + 1..end]) {
            dropped = true;
        } else {
            kept.push_str(&rest[open..=end]);
        }
        kept.push(' ');
        rest = &rest[end + 1..];
    }
    kept.push_str(rest);
    let plain: String = kept
        .replace('&', " and ")
        .chars()
        .filter(|character| !matches!(character, '\'' | '\u{2018}' | '\u{2019}' | '\u{02bc}'))
        .map(|character| if character == '/' { ' ' } else { character })
        .collect();
    (title_key(&plain), dropped)
}

/// Whether a bracket's text credits artists: it holds the word "feat",
/// "ft" or "featuring".
fn is_credit(text: &str) -> bool {
    text.split(|character: char| !character.is_alphanumeric())
        .any(|word| ["feat", "ft", "featuring"].contains(&word.to_lowercase().as_str()))
}

/// The shorter key is the longer or a whole-word start of it.
fn key_starts(short: &str, long: &str) -> bool {
    !short.is_empty()
        && long
            .strip_prefix(short)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// An entry's title and the name of the library track at its place the
/// same song: the shorter [`title_key`] is the longer or a whole-word start
/// of it, so "Song" agrees with "Song - 2011 Remaster" and "Go" does not
/// with "Gone".
fn titles_agree(entry: &str, track: &str) -> bool {
    let (entry, track) = (title_key(entry), title_key(track));
    let (short, long) = if entry.len() <= track.len() {
        (entry, track)
    } else {
        (track, entry)
    };
    key_starts(&short, &long)
}

/// [`titles_agree`] by [`place_key`], for when nothing on the album agrees
/// strictly: a side whose credit was dropped is the shorter one only when
/// the keys are equal, so "Opener (feat. B)" does not agree with "Opener
/// (Acoustic)".
fn titles_agree_at_place(entry: &str, track: &str) -> bool {
    let (entry, track) = (place_key(entry), place_key(track));
    let (short, long) = if entry.0.len() <= track.0.len() {
        (entry, track)
    } else {
        (track, entry)
    };
    !(short.1 && short.0 != long.0) && key_starts(&short.0, &long.0)
}

/// The library track each entry links to: the one at its disc and track
/// number whose title agrees, else one titled the same elsewhere on the
/// album, as another edition orders it, else the one at its place whose
/// title agrees past credits and symbols. `candidates` are grouped by
/// entry, the preferred copy's tracks first.
fn library_links(candidates: &[sql::LibraryCopyCandidate]) -> Vec<(String, String)> {
    candidates
        .chunk_by(|a, b| a.entry_id == b.entry_id)
        .filter_map(|copies| {
            let at_place = |agree: fn(&str, &str) -> bool| {
                copies.iter().find(|copy| {
                    (copy.entry_disc, copy.entry_track)
                        == (copy.track_disc, Some(copy.track_number))
                        && agree(&copy.entry_title, &copy.track_title)
                })
            };
            let same_title = || {
                copies
                    .iter()
                    .find(|copy| title_key(&copy.entry_title) == title_key(&copy.track_title))
            };
            at_place(titles_agree)
                .or_else(same_title)
                .or_else(|| at_place(titles_agree_at_place))
                .map(|copy| (copy.entry_id.clone(), copy.track_id.clone()))
        })
        .collect()
}

/// The caller's playlist's entries with no source yet and their candidate
/// library tracks.
fn library_copy_candidates(
    conn: &Connection,
    user_id: &str,
    playlist_id: &str,
) -> rusqlite::Result<Vec<sql::LibraryCopyCandidate>> {
    conn.prepare(sql::LIBRARY_COPY_CANDIDATES.text())?
        .query_map(
            params![user_id, playlist_id],
            sql::LibraryCopyCandidate::read,
        )?
        .collect()
}

/// One playlist row and its first entry covers, or None.
fn one_playlist(
    conn: &Connection,
    statement: droppedneedle_kernel::db::statement::Statement,
    user_id: &str,
    playlist_id: &str,
) -> Result<Option<PlaylistRow>, DbError> {
    let Some(mut row) = conn
        .query_row(
            statement.text(),
            params![user_id, playlist_id],
            playlist_from_row,
        )
        .optional()?
    else {
        return Ok(None);
    };
    row.cover_urls = conn
        .prepare(sql::PLAYLIST_COVERS.text())?
        .query_map(params![playlist_id, SUMMARY_COVER_COUNT], |row| {
            row.get::<_, String>(0)
        })?
        .filter_map(|url| url.map(|url| entry_cover(&url)).transpose())
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(row))
}

/// Fill each row's cover tiles from one read of the caller's readable
/// playlists' first entry covers.
fn fill_covers<'a>(
    conn: &Connection,
    user_id: &str,
    rows: impl Iterator<Item = &'a mut PlaylistRow>,
) -> Result<(), DbError> {
    let mut by_playlist: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut stmt = conn.prepare(sql::LIST_COVERS.text())?;
    let covers = stmt.query_map(params![user_id, SUMMARY_COVER_COUNT], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for cover in covers {
        let (id, url) = cover?;
        if let Some(url) = entry_cover(&url) {
            by_playlist.entry(id).or_default().push(url);
        }
    }
    for row in rows {
        if let Some(urls) = by_playlist.remove(&row.id) {
            row.cover_urls = urls;
        }
    }
    Ok(())
}

/// Whether `playlist_id` is `user_id`'s.
fn owns(tx: &Transaction, playlist_id: &str, user_id: &str) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row(
            sql::OWNED_GUARD.text(),
            params![playlist_id, user_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Bump `updated_at` of the user's playlist to `now`, milliseconds; false
/// when it is gone or not theirs.
fn touch(tx: &Transaction, playlist_id: &str, user_id: &str, now: i64) -> rusqlite::Result<bool> {
    let changed = tx.execute(sql::TOUCH.text(), params![now, playlist_id, user_id])?;
    Ok(changed > 0)
}

/// Whether `user_id` may change the songs of playlist `playlist_id`: as
/// its owner, or also as an editor unless `owner_only`.
fn may_edit(
    tx: &Transaction,
    playlist_id: &str,
    user_id: &str,
    owner_only: bool,
) -> rusqlite::Result<bool> {
    if owner_only {
        return owns(tx, playlist_id, user_id);
    }
    Ok(tx
        .query_row(
            sql::EDIT_GUARD.text(),
            params![user_id, playlist_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Bump `updated_at` of a playlist the user may edit to `now`,
/// milliseconds; false when it is gone or they may not.
fn touch_editable(
    tx: &Transaction,
    playlist_id: &str,
    user_id: &str,
    owner_only: bool,
    now: i64,
) -> rusqlite::Result<bool> {
    if !may_edit(tx, playlist_id, user_id, owner_only)? {
        return Ok(false);
    }
    tx.execute(sql::STAMP.text(), params![now, playlist_id])?;
    Ok(true)
}

/// Insert one empty playlist row made at `now`, milliseconds.
fn insert_playlist(
    tx: &Transaction,
    id: &str,
    name: &str,
    now: i64,
    source_ref: Option<&str>,
    owner_id: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::INSERT_PLAYLIST.text(),
        params![id, name, now, source_ref, owner_id],
    )?;
    Ok(())
}

/// Entry ids in position order.
fn entry_order(tx: &Transaction, playlist_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(sql::ENTRY_ORDER.text())?;
    let rows = stmt.query_map(params![playlist_id], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// Write `order` as the dense positions 0..n, in two passes so the unique
/// position index never sees a collision.
fn write_order(tx: &Transaction, order: &[String]) -> rusqlite::Result<()> {
    let mut park = tx.prepare(sql::SET_POSITION.text())?;
    for (index, id) in order.iter().enumerate() {
        park.execute(params![-(index as i64) - 1, id])?;
    }
    for (index, id) in order.iter().enumerate() {
        park.execute(params![index as i64, id])?;
    }
    Ok(())
}

/// Insert `entries` at `position` (clamped; None appends) and renumber.
/// The caller has checked the playlist is the user's.
fn insert_entries(
    tx: &Transaction,
    playlist_id: &str,
    position: Option<usize>,
    entries: &[NewEntry],
    now: i64,
) -> rusqlite::Result<Vec<String>> {
    let mut order = entry_order(tx, playlist_id)?;
    let at = position.unwrap_or(order.len()).min(order.len());
    let tail = order.len() as i64;
    let mut stmt = tx.prepare(sql::INSERT_ENTRY.text())?;
    let mut added = Vec::with_capacity(entries.len());
    for (offset, entry) in entries.iter().enumerate() {
        let id = uuid::Uuid::new_v4().to_string();
        // Parked past the tail; `write_order` renumbers below.
        stmt.execute(params![
            id,
            playlist_id,
            tail + offset as i64,
            entry.track_name,
            entry.artist_name,
            entry.album_name,
            entry.album_id,
            entry.artist_id,
            entry.track_source_id,
            entry.cover_url.as_deref().and_then(entry_cover),
            entry.source_type,
            encode_sources(entry.available_sources.as_ref()),
            entry.format,
            entry.track_number,
            entry.disc_number,
            entry
                .duration
                .map(|seconds| (seconds * 1000.0).round() as i64),
            now,
            entry.plex_rating_key,
            entry.library_file_id,
            entry.recording_mbid,
            entry.isrc,
        ])?;
        added.push(id);
    }
    drop(stmt);
    order.splice(at..at, added.iter().cloned());
    write_order(tx, &order)?;
    Ok(added)
}

impl PlaylistStore {
    /// Store over one database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// Every playlist, as (owner, playlist), holding an entry with no source
    /// that an album now in the library owns.
    pub async fn to_link(&self) -> Result<Vec<(String, String)>, StoreError> {
        self.db
            .read("playlists.to_link", |conn| {
                let rows = conn
                    .prepare(sql::PLAYLISTS_TO_LINK.text())?
                    .query_map([], sql::PlaylistToLink::read)?
                    .map(|row| row.map(|row| (row.user_id, row.playlist_id)))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
    }

    /// Where the library's catalog and the playlists stand: a link pass
    /// has nothing new to do while neither moved.
    pub async fn link_mark(&self) -> Result<(i64, i64), StoreError> {
        self.db
            .read("playlists.link_mark", |conn| {
                let mark = conn.query_row(sql::LINK_MARK.text(), [], sql::LinkMark::read)?;
                Ok((mark.catalog_revision, mark.playlists_changed))
            })
            .await
    }

    /// The songs of `playlist_id` an import kept that still wait for an
    /// album, in playlist order: no source, no file and no album, and a
    /// title, an artist and an album named. A playlist that is gone has
    /// none.
    pub async fn entries_to_identify(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<KeptEntry>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .read("playlists.entries_to_identify", move |conn| {
                let rows = conn
                    .prepare(sql::ENTRIES_TO_IDENTIFY.text())?
                    .query_map(params![playlist_id], sql::EntryToIdentify::read)?
                    .map(|row| {
                        row.map(|row| KeptEntry {
                            entry_id: row.entry_id,
                            title: row.track_name,
                            artist: row.artist_name,
                            album: row.album_name,
                            isrc: row.isrc,
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
    }

    /// Give `entries` of `playlist_id`, each with the recording found for
    /// it when one was, the release group `group` and its cover, in one
    /// write. An entry that has meanwhile gained a source, a file or an
    /// album is left alone. A change stamps the playlist, so the link pass
    /// looks at it again. Answers how many entries changed.
    pub async fn set_entries_album(
        &self,
        playlist_id: &str,
        group: &str,
        entries: Vec<(String, Option<String>)>,
    ) -> Result<usize, StoreError> {
        let group = group.to_ascii_lowercase();
        let cover = entry_cover(&format!("/api/v3/covers/release-group/{group}?size=250"));
        let playlist_id = playlist_id.to_owned();
        let now = self.db.now_ms();
        self.db
            .write("playlists.set_entries_album", move |tx| {
                let mut changed = 0;
                for (entry_id, recording) in &entries {
                    changed += tx.execute(
                        sql::SET_ENTRY_ALBUM.text(),
                        params![playlist_id, entry_id, group, cover, recording],
                    )?;
                }
                if changed > 0 {
                    tx.execute(sql::STAMP.text(), params![now, playlist_id])?;
                }
                Ok(changed)
            })
            .await
    }

    /// The songs `playlist_id` took from its source that a sync has not
    /// followed up and whose next look for an album is due by `now` (unix
    /// ms), in playlist order; with `albumless_only`, only those with no
    /// album yet.
    pub async fn open_source_songs(
        &self,
        playlist_id: &str,
        now: i64,
        albumless_only: bool,
    ) -> Result<Vec<OpenSourceSong>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .read("playlists.open_source_songs", move |conn| {
                let rows = conn
                    .prepare(sql::OPEN_SOURCE_SONGS.text())?
                    .query_map(
                        params![playlist_id, now, albumless_only],
                        sql::OpenSourceSongColumns::read,
                    )?
                    .map(|row| {
                        row.map(|row| OpenSourceSong {
                            song_id: row.song_id,
                            entry_id: row.entry_id,
                            track_name: row.track_name,
                            artist_name: row.artist_name,
                            album_name: row.album_name,
                            album_id: row.album_id,
                            recording_mbid: row.recording_mbid,
                            isrc: row.isrc,
                            duration_ms: row.duration_ms,
                            playable: row.playable,
                            album: SourceAlbum {
                                source_id: row.album_source_id,
                                title: row.album_title,
                                artist: row.album_artist,
                                kind: row.album_kind,
                                tracks: row.album_tracks,
                            },
                            pick: row.album_pick,
                            looks: row.looks,
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
    }

    /// The release group each source album of `playlist_id` was given on
    /// one of its songs' entries, the first in playlist order; none an
    /// album moved to an artist's own album.
    pub async fn placed_albums(
        &self,
        playlist_id: &str,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .read("playlists.placed_albums", move |conn| {
                let mut placed = std::collections::HashMap::new();
                for row in conn
                    .prepare(sql::PLACED_ALBUMS.text())?
                    .query_map(params![playlist_id], sql::PlacedAlbum::read)?
                {
                    let row = row?;
                    placed.entry(row.album_source_id).or_insert(row.album_id);
                }
                Ok(placed)
            })
            .await
    }

    /// The titles and ISRCs of `playlist_id`'s songs on source album
    /// `album_source_id`, in playlist order.
    pub async fn source_album_songs(
        &self,
        playlist_id: &str,
        album_source_id: &str,
    ) -> Result<Vec<(String, Option<String>)>, StoreError> {
        let (playlist_id, album_source_id) = (playlist_id.to_owned(), album_source_id.to_owned());
        self.db
            .read("playlists.source_album_songs", move |conn| {
                let rows = conn
                    .prepare(sql::SOURCE_ALBUM_SONGS.text())?
                    .query_map(
                        params![playlist_id, album_source_id],
                        sql::SourceAlbumSong::read,
                    )?
                    .map(|row| row.map(|row| (row.track_name, row.isrc)))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
    }

    /// A sync is done with `song_ids` of `playlist_id` at `now`: requested,
    /// or nothing to ask for.
    pub async fn mark_followed_up(
        &self,
        playlist_id: &str,
        song_ids: Vec<String>,
        now: i64,
    ) -> Result<(), StoreError> {
        if song_ids.is_empty() {
            return Ok(());
        }
        let playlist_id = playlist_id.to_owned();
        self.db
            .write("playlists.mark_followed_up", move |tx| {
                let mut statement = tx.prepare(sql::FOLLOW_UP_SOURCE_SONG.text())?;
                for song_id in &song_ids {
                    statement.execute(params![playlist_id, song_id, now])?;
                }
                Ok(())
            })
            .await
    }

    /// A sync filed a request of `kind` for MusicBrainz id `key` that
    /// covers `song_ids` of `playlist_id`, at `now`: they are followed up,
    /// and each keeps what was filed for it.
    pub async fn mark_filed(
        &self,
        playlist_id: &str,
        song_ids: Vec<String>,
        (kind, key): (FiledKind, &str),
        now: i64,
    ) -> Result<(), StoreError> {
        if song_ids.is_empty() {
            return Ok(());
        }
        let (playlist_id, key) = (playlist_id.to_owned(), key.to_ascii_lowercase());
        self.db
            .write("playlists.mark_filed", move |tx| {
                let mut statement = tx.prepare(sql::FILE_SOURCE_SONG.text())?;
                for song_id in &song_ids {
                    statement.execute(params![playlist_id, song_id, now, kind.as_str(), key])?;
                }
                Ok(())
            })
            .await
    }

    /// Nothing proved the album of `playlist_id`'s song `song_id`: count
    /// the look and look again at `at` (unix ms).
    pub async fn look_again(
        &self,
        playlist_id: &str,
        song_id: &str,
        at: i64,
    ) -> Result<(), StoreError> {
        let (playlist_id, song_id) = (playlist_id.to_owned(), song_id.to_owned());
        self.db
            .write("playlists.look_again", move |tx| {
                tx.execute(
                    sql::LOOK_AGAIN_SOURCE_SONG.text(),
                    params![playlist_id, song_id, at],
                )?;
                Ok(())
            })
            .await
    }

    /// The entries of `playlist_id` whose songs a sync has not followed up.
    pub async fn open_source_entries(
        &self,
        playlist_id: &str,
    ) -> Result<std::collections::HashSet<String>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .read("playlists.open_source_entries", move |conn| {
                let rows = conn
                    .prepare(sql::OPEN_SOURCE_ENTRIES.text())?
                    .query_map(params![playlist_id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok(rows)
            })
            .await
    }

    /// Move `playlist_id`'s entry `entry_id` (source song `song_id`) from
    /// release group `from` to the artist's own album `to`, titled `title`,
    /// with `recording` when known, and keep that the sync moved it, in one
    /// write. An entry that has meanwhile gained a file or a source, or
    /// another album, is left alone. Answers whether it moved.
    pub async fn move_to_own_album(
        &self,
        playlist_id: &str,
        song_id: &str,
        entry_id: &str,
        (from, to): (&str, &str),
        title: &str,
        recording: Option<&str>,
    ) -> Result<bool, StoreError> {
        let (playlist_id, song_id, entry_id) = (
            playlist_id.to_owned(),
            song_id.to_owned(),
            entry_id.to_owned(),
        );
        let (from, to) = (from.to_ascii_lowercase(), to.to_ascii_lowercase());
        let cover = entry_cover(&format!("/api/v3/covers/release-group/{to}?size=250"));
        let (title, recording) = (title.to_owned(), recording.map(str::to_ascii_lowercase));
        let now = self.db.now_ms();
        self.db
            .write("playlists.move_to_own_album", move |tx| {
                let moved = tx.execute(
                    sql::MOVE_TO_OWN_ALBUM.text(),
                    params![playlist_id, entry_id, from, to, cover, title, recording],
                )? > 0;
                if moved {
                    tx.execute(sql::PICK_OWN_ALBUM.text(), params![playlist_id, song_id])?;
                    tx.execute(sql::STAMP.text(), params![now, playlist_id])?;
                }
                Ok(moved)
            })
            .await
    }

    /// The album the source lists each of `playlist_id`'s entries on that
    /// a sync moved to the artist's own album, by entry.
    pub async fn listed_on(
        &self,
        playlist_id: &str,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .read("playlists.listed_on", move |conn| {
                let rows = conn
                    .prepare(sql::LISTED_ON.text())?
                    .query_map(params![playlist_id], sql::ListedOn::read)?
                    .map(|row| row.map(|row| (row.entry_id, row.album_title)))
                    .collect::<rusqlite::Result<_>>()?;
                Ok(rows)
            })
            .await
    }

    /// The owner of `playlist_id` and where it came from, when it has an
    /// owner.
    pub async fn source_of(
        &self,
        playlist_id: &str,
    ) -> Result<Option<(String, Option<String>)>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .read("playlists.source_of", move |conn| {
                Ok(conn
                    .query_row(
                        sql::PLAYLIST_SOURCE.text(),
                        params![playlist_id],
                        sql::PlaylistSource::read,
                    )
                    .optional()?
                    .map(|row| (row.user_id, row.source_ref)))
            })
            .await
    }

    /// How many songs `playlist_id` took from its source still wait for a
    /// sync: not followed up, not playable, and with an album or a look for
    /// one due by `now` (unix ms).
    pub async fn open_source_song_count(
        &self,
        playlist_id: &str,
        now: i64,
    ) -> Result<u32, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .read("playlists.open_source_song_count", move |conn| {
                let count: i64 = conn.query_row(
                    sql::OPEN_SOURCE_SONG_COUNT.text(),
                    params![playlist_id, now],
                    |row| row.get(0),
                )?;
                Ok(u32::try_from(count).unwrap_or(u32::MAX))
            })
            .await
    }

    /// The source songs `playlist_id` has taken, whether or not their
    /// entries are still there.
    pub async fn source_song_ids(
        &self,
        playlist_id: &str,
    ) -> Result<std::collections::HashSet<String>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .read("playlists.source_song_ids", move |conn| {
                let ids = conn
                    .prepare(sql::SOURCE_SONG_IDS.text())?
                    .query_map(params![playlist_id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok(ids)
            })
            .await
    }

    /// One user's own playlists, to read and change.
    pub fn owned_by<'a>(&'a self, user_id: &'a str) -> OwnedPlaylists<'a> {
        OwnedPlaylists {
            store: self,
            user_id,
        }
    }

    /// The playlists one user may change the songs of.
    pub fn editable_by<'a>(&'a self, user_id: &'a str) -> EditablePlaylists<'a> {
        EditablePlaylists {
            store: self,
            user_id,
            owner_only: false,
        }
    }

    /// What one user may read.
    pub fn visible_to<'a>(&'a self, user_id: &'a str) -> VisiblePlaylists<'a> {
        VisiblePlaylists {
            store: self,
            user_id,
        }
    }

    /// Playlists with no owner, which only an admin may delete.
    pub fn strays(&self, _admin: &Admin) -> StrayPlaylists<'_> {
        StrayPlaylists { store: self }
    }
}

impl EditablePlaylists<'_> {
    /// Insert entries at `position` (clamped; None appends). Returns the
    /// new entry ids.
    pub async fn insert(
        &self,
        playlist_id: &str,
        position: Option<usize>,
        entries: Vec<NewEntry>,
    ) -> Result<Written<Vec<String>>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let (now, owner_only) = (self.store.db.now_ms(), self.owner_only);
        self.store
            .db
            .write("playlists.insert", move |tx| {
                if !touch_editable(tx, &playlist_id, &user_id, owner_only, now)? {
                    return Ok(Written::Missing);
                }
                let added = insert_entries(tx, &playlist_id, position, &entries, now)?;
                Ok(Written::Done(added))
            })
            .await
    }

    /// Replace every entry with `entries`, in order, in one transaction: a
    /// write that fails leaves the old entries in place.
    pub async fn replace_entries(
        &self,
        playlist_id: &str,
        entries: Vec<NewEntry>,
    ) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let (now, owner_only) = (self.store.db.now_ms(), self.owner_only);
        self.store
            .db
            .write("playlists.replace_entries", move |tx| {
                if !touch_editable(tx, &playlist_id, &user_id, owner_only, now)? {
                    return Ok(Written::Missing);
                }
                tx.execute(sql::CLEAR_ENTRIES.text(), params![playlist_id])?;
                insert_entries(tx, &playlist_id, None, &entries, now)?;
                Ok(Written::Done(()))
            })
            .await
    }

    /// Remove entries by id, skipping unknown ids. Returns how many went.
    pub async fn remove(
        &self,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> Result<Written<usize>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let entry_ids = entry_ids.to_vec();
        let (now, owner_only) = (self.store.db.now_ms(), self.owner_only);
        self.store
            .db
            .write("playlists.remove", move |tx| {
                if !touch_editable(tx, &playlist_id, &user_id, owner_only, now)? {
                    return Ok(Written::Missing);
                }
                let mut removed = 0;
                {
                    let mut stmt = tx.prepare(sql::REMOVE_ENTRY.text())?;
                    for id in &entry_ids {
                        removed += stmt.execute(params![playlist_id, id])?;
                    }
                }
                let order = entry_order(tx, &playlist_id)?;
                write_order(tx, &order)?;
                Ok(Written::Done(removed))
            })
            .await
    }

    /// Move one entry to `position`, clamped. Returns where it landed, or
    /// `Missing` when the playlist or entry is gone.
    pub async fn move_entry(
        &self,
        playlist_id: &str,
        entry_id: &str,
        position: usize,
    ) -> Result<Written<usize>, StoreError> {
        let (user_id, playlist_id, entry_id) = (
            self.user_id.to_owned(),
            playlist_id.to_owned(),
            entry_id.to_owned(),
        );
        let (now, owner_only) = (self.store.db.now_ms(), self.owner_only);
        self.store
            .db
            .write("playlists.move", move |tx| {
                if !may_edit(tx, &playlist_id, &user_id, owner_only)? {
                    return Ok(Written::Missing);
                }
                let mut order = entry_order(tx, &playlist_id)?;
                let Some(from) = order.iter().position(|id| *id == entry_id) else {
                    return Ok(Written::Missing);
                };
                let id = order.remove(from);
                let at = position.min(order.len());
                order.insert(at, id);
                write_order(tx, &order)?;
                tx.execute(sql::STAMP.text(), params![now, playlist_id])?;
                Ok(Written::Done(at))
            })
            .await
    }

    /// Replace one entry's source fields.
    pub async fn update_sources(
        &self,
        playlist_id: &str,
        entry_id: &str,
        source_type: Option<String>,
        available_sources: Option<Vec<String>>,
    ) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id, entry_id) = (
            self.user_id.to_owned(),
            playlist_id.to_owned(),
            entry_id.to_owned(),
        );
        let (now, owner_only) = (self.store.db.now_ms(), self.owner_only);
        self.store
            .db
            .write("playlists.update_sources", move |tx| {
                if !may_edit(tx, &playlist_id, &user_id, owner_only)? {
                    return Ok(Written::Missing);
                }
                let exists = tx
                    .query_row(
                        sql::ENTRY_EXISTS.text(),
                        params![playlist_id, entry_id],
                        |_| Ok(()),
                    )
                    .optional()?;
                if exists.is_none() {
                    return Ok(Written::Missing);
                }
                if let Some(source_type) = &source_type {
                    tx.execute(
                        sql::SET_SOURCE_TYPE.text(),
                        params![source_type, entry_id, playlist_id],
                    )?;
                }
                if let Some(sources) = &available_sources {
                    tx.execute(
                        sql::SET_AVAILABLE_SOURCES.text(),
                        params![encode_sources(Some(sources)), entry_id, playlist_id],
                    )?;
                }
                tx.execute(sql::STAMP.text(), params![now, playlist_id])?;
                Ok(Written::Done(()))
            })
            .await
    }
}

impl StrayPlaylists<'_> {
    /// Delete a playlist with no owner, with its entries and cover. A
    /// playlist that has an owner is `Missing` and stays.
    pub async fn delete(&self, playlist_id: &str) -> Result<Written<()>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.store
            .db
            .write("playlists.delete_stray", move |tx| {
                let changed = tx.execute(sql::DELETE_STRAY.text(), params![playlist_id])?;
                Ok(if changed == 0 {
                    Written::Missing
                } else {
                    Written::Done(())
                })
            })
            .await
    }
}

impl VisiblePlaylists<'_> {
    /// Stop sharing playlist `playlist_id` with the user; false when it
    /// was not shared with them.
    pub async fn leave(&self, playlist_id: &str) -> Result<bool, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .write("playlists.leave", move |tx| {
                Ok(tx.execute(sql::LEAVE.text(), params![playlist_id, user_id])? > 0)
            })
            .await
    }

    /// Every playlist in name order: the readable ones with aggregates and
    /// cover tiles, other users' private ones as existence only. A private
    /// playlist with no owner is listed only when `with_private_strays`.
    pub async fn list(&self, with_private_strays: bool) -> Result<Vec<Listed>, StoreError> {
        let user_id = self.user_id.to_owned();
        self.store
            .db
            .read("playlists.list", move |conn| {
                let mut stmt = conn.prepare(sql::LIST_ALL.text())?;
                let mut listed = stmt
                    .query_map(params![user_id, with_private_strays], |row| {
                        if row.get::<_, i64>(15)? != 0 {
                            Ok(Listed::Readable(playlist_from_row(row)?))
                        } else {
                            Ok(Listed::Hidden {
                                id: row.get(0)?,
                                owner_id: row.get(2)?,
                                track_count: count(row.get(9)?),
                                owner_name: row.get(3)?,
                            })
                        }
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                fill_covers(
                    conn,
                    &user_id,
                    listed.iter_mut().filter_map(|item| match item {
                        Listed::Readable(row) => Some(row),
                        Listed::Hidden { .. } => None,
                    }),
                )?;
                Ok(listed)
            })
            .await
    }

    /// The playlists the caller may read, name order, with aggregates and
    /// cover tiles.
    pub async fn readable(&self) -> Result<Vec<PlaylistRow>, StoreError> {
        let user_id = self.user_id.to_owned();
        self.store
            .db
            .read("playlists.readable", move |conn| {
                let mut stmt = conn.prepare(sql::LIST_READABLE.text())?;
                let mut rows = stmt
                    .query_map(params![user_id], playlist_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                fill_covers(conn, &user_id, rows.iter_mut())?;
                Ok(rows)
            })
            .await
    }

    /// One playlist the caller may read, with aggregates, or None.
    pub async fn get(&self, playlist_id: &str) -> Result<Option<PlaylistRow>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.get", move |conn| {
                one_playlist(conn, sql::GET_READABLE, &user_id, &playlist_id)
            })
            .await
    }

    /// Whether the caller owns a playlist they may read; None when they
    /// may not read it or it is gone.
    pub async fn is_owner(&self, playlist_id: &str) -> Result<Option<bool>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.is_owner", move |conn| {
                Ok(conn
                    .query_row(
                        sql::READABLE_OWNER.text(),
                        params![user_id, playlist_id],
                        |row| Ok(row.get::<_, i64>(0)? != 0),
                    )
                    .optional()?)
            })
            .await
    }

    /// Whether the user may change the songs of a playlist they may read;
    /// None when they may not read it.
    pub async fn may_edit(&self, playlist_id: &str) -> Result<Option<bool>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.may_edit", move |conn| {
                Ok(conn
                    .query_row(
                        sql::READABLE_EDITOR.text(),
                        params![user_id, playlist_id],
                        |row| Ok(row.get::<_, i64>(0)? != 0),
                    )
                    .optional()?)
            })
            .await
    }

    /// Entries of a playlist the caller may read, in position order; none
    /// when they may not read it.
    pub async fn tracks(&self, playlist_id: &str) -> Result<Vec<PlaylistTrack>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.tracks", move |conn| {
                Ok(conn
                    .prepare(sql::TRACKS_READABLE.text())?
                    .query_map(params![user_id, playlist_id], track_from_row)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Entries of a playlist the caller may read, in position order, each
    /// with its live local file's album artist and root-relative path.
    pub(crate) async fn export_entries(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<PlaylistExportRow>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.export_entries", move |conn| {
                Ok(conn
                    .prepare(sql::EXPORT_ENTRIES.text())?
                    .query_map(params![user_id, playlist_id], PlaylistExportRow::read)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Uploaded cover bytes of a playlist the caller may read, or None.
    pub async fn cover(&self, playlist_id: &str) -> Result<Option<CoverImage>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.cover", move |conn| {
                Ok(conn
                    .query_row(
                        sql::COVER_READABLE.text(),
                        params![user_id, playlist_id],
                        cover_from_row,
                    )
                    .optional()?)
            })
            .await
    }

    /// For each asked track, by its index, the ids of the playlists the
    /// caller may read holding an entry with exactly its names, in id
    /// order. One statement for the whole list.
    pub async fn holding(
        &self,
        tracks: &[EntryNames<'_>],
    ) -> Result<Vec<(usize, String)>, StoreError> {
        if tracks.is_empty() {
            return Ok(Vec::new());
        }
        let asked = serde_json::Value::from(
            tracks
                .iter()
                .map(|track| {
                    serde_json::json!([track.track_name, track.artist_name, track.album_name])
                })
                .collect::<Vec<_>>(),
        )
        .to_string();
        let user_id = self.user_id.to_owned();
        self.store
            .db
            .read("playlists.holding", move |conn| {
                Ok(conn
                    .prepare(sql::MEMBERSHIP.text())?
                    .query_map(params![user_id, asked], |row| {
                        Ok((count(row.get(0)?), row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }
}

impl OwnedPlaylists<'_> {
    /// Song writes on the user's own playlists only, for work done for
    /// them with no request: a playlist shared with them to edit is not
    /// theirs to rebuild.
    pub fn songs(&self) -> EditablePlaylists<'_> {
        EditablePlaylists {
            store: self.store,
            user_id: self.user_id,
            owner_only: true,
        }
    }

    /// One of the user's playlists with aggregates, or None.
    pub async fn get(&self, playlist_id: &str) -> Result<Option<PlaylistRow>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.get_owned", move |conn| {
                one_playlist(conn, sql::GET_OWNED, &user_id, &playlist_id)
            })
            .await
    }

    /// Entries of one of the user's playlists, in position order; none
    /// when it is not theirs.
    pub async fn tracks(&self, playlist_id: &str) -> Result<Vec<PlaylistTrack>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.tracks_owned", move |conn| {
                Ok(conn
                    .prepare(sql::TRACKS_OWNED.text())?
                    .query_map(params![user_id, playlist_id], track_from_row)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Uploaded cover bytes of one of the user's playlists, or None.
    pub async fn cover(&self, playlist_id: &str) -> Result<Option<CoverImage>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.cover_owned", move |conn| {
                Ok(conn
                    .query_row(
                        sql::COVER_OWNED.text(),
                        params![user_id, playlist_id],
                        cover_from_row,
                    )
                    .optional()?)
            })
            .await
    }

    /// The playlist the user imported from one source, or None.
    pub async fn find_by_source(&self, source_ref: &str) -> Result<Option<String>, StoreError> {
        let (user_id, source_ref) = (self.user_id.to_owned(), source_ref.to_owned());
        self.store
            .db
            .read("playlists.find_by_source", move |conn| {
                Ok(conn
                    .query_row(
                        sql::FIND_BY_SOURCE.text(),
                        params![user_id, source_ref],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
    }

    /// Create a playlist; returns its id. A duplicate import provenance for
    /// the same owner reports `None`.
    pub async fn create(
        &self,
        name: &str,
        source_ref: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (owner_id, name, source_ref) = (
            self.user_id.to_owned(),
            name.to_owned(),
            source_ref.map(str::to_owned),
        );
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.create", move |tx| {
                if let Some(source_ref) = &source_ref {
                    let taken: Option<String> = tx
                        .query_row(
                            sql::FIND_BY_SOURCE.text(),
                            params![owner_id, source_ref],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if taken.is_some() {
                        return Ok(None);
                    }
                }
                insert_playlist(tx, &id, &name, now, source_ref.as_deref(), &owner_id)?;
                Ok(Some(id))
            })
            .await
    }

    /// Create a playlist holding `entries`, in order, in one transaction:
    /// a write that fails leaves nothing behind. Returns its id and when it
    /// was made, epoch seconds.
    pub async fn create_with(
        &self,
        name: &str,
        entries: Vec<NewEntry>,
    ) -> Result<(String, u64), StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (owner_id, name) = (self.user_id.to_owned(), name.to_owned());
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.create_with", move |tx| {
                insert_playlist(tx, &id, &name, now, None, &owner_id)?;
                insert_entries(tx, &id, None, &entries, now)?;
                Ok((id, epoch(now / 1000)))
            })
            .await
    }

    /// [`Self::create_with`] for an import that kept songs naming an album:
    /// the same write queues the job on `queue` that finds those albums.
    pub async fn create_identifying(
        &self,
        name: &str,
        entries: Vec<NewEntry>,
        queue: &JobQueue,
    ) -> Result<(String, u64), StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (owner_id, name) = (self.user_id.to_owned(), name.to_owned());
        let now = self.store.db.now_ms();
        let queue = queue.clone();
        self.store
            .db
            .write_queueing("playlists.create_identifying", move |tx| {
                insert_playlist(tx, &id, &name, now, None, &owner_id)?;
                insert_entries(tx, &id, None, &entries, now)?;
                let job = IdentifyJob {
                    playlist_id: id.clone(),
                };
                queue.enqueue(tx, IDENTIFY, &identify_key(&id), &job)?;
                Ok((id, epoch(now / 1000)))
            })
            .await
    }

    /// Rename a playlist.
    pub async fn rename(&self, playlist_id: &str, name: &str) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id, name) = (
            self.user_id.to_owned(),
            playlist_id.to_owned(),
            name.to_owned(),
        );
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.rename", move |tx| {
                let changed =
                    tx.execute(sql::RENAME.text(), params![name, playlist_id, user_id])?;
                if changed == 0 {
                    return Ok(Written::Missing);
                }
                touch(tx, &playlist_id, &user_id, now)?;
                Ok(Written::Done(()))
            })
            .await
    }

    /// Flip visibility.
    pub async fn set_public(
        &self,
        playlist_id: &str,
        public: bool,
    ) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.set_public", move |tx| {
                let changed = tx.execute(
                    sql::SET_PUBLIC.text(),
                    params![i64::from(public), playlist_id, user_id],
                )?;
                if changed == 0 {
                    return Ok(Written::Missing);
                }
                touch(tx, &playlist_id, &user_id, now)?;
                Ok(Written::Done(()))
            })
            .await
    }

    /// Delete a playlist with its entries and cover.
    pub async fn delete(&self, playlist_id: &str) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .write("playlists.delete", move |tx| {
                let changed =
                    tx.execute(sql::DELETE_PLAYLIST.text(), params![playlist_id, user_id])?;
                Ok(if changed == 0 {
                    Written::Missing
                } else {
                    Written::Done(())
                })
            })
            .await
    }

    /// The people one of the user's playlists is shared with, first
    /// shared first; none when it is not theirs.
    pub async fn members(&self, playlist_id: &str) -> Result<Vec<PlaylistMember>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.members", move |conn| {
                Ok(conn
                    .prepare(sql::MEMBERS.text())?
                    .query_map(params![user_id, playlist_id], |row| {
                        let member = sql::MemberColumns::read(row)?;
                        Ok(PlaylistMember {
                            user_id: member.user_id,
                            display_name: member.display_name,
                            can_edit: member.can_edit,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Share the playlist with exactly `members`, each `(user id, may
    /// edit)`, keeping when each person was first shared it. False, and
    /// nothing changes, when an id names no user.
    pub async fn set_members(
        &self,
        playlist_id: &str,
        members: Vec<(String, bool)>,
    ) -> Result<Written<bool>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.set_members", move |tx| {
                if !owns(tx, &playlist_id, &user_id)? {
                    return Ok(Written::Missing);
                }
                let ids: Vec<&String> = members.iter().map(|(id, _)| id).collect();
                let listed = serde_json::to_string(&ids)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
                let known: usize = tx.query_row(
                    sql::KNOWN_USERS.text(),
                    params![listed, playlist_id],
                    |row| row.get(0),
                )?;
                if known != members.len() {
                    return Ok(Written::Done(false));
                }
                tx.execute(sql::DROP_MEMBERS.text(), params![playlist_id, listed])?;
                for (member, can_edit) in &members {
                    tx.execute(
                        sql::ADD_MEMBER.text(),
                        params![playlist_id, member, now, can_edit],
                    )?;
                }
                Ok(Written::Done(true))
            })
            .await
    }

    /// Replace every entry with `entries` and, when `name` is given, rename
    /// the playlist, in one transaction. Returns the playlist as the write
    /// left it.
    pub async fn replace_named(
        &self,
        playlist_id: &str,
        name: Option<&str>,
        entries: Vec<NewEntry>,
    ) -> Result<Written<SavedPlaylist>, StoreError> {
        let (user_id, playlist_id, name) = (
            self.user_id.to_owned(),
            playlist_id.to_owned(),
            name.map(str::to_owned),
        );
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.replace_entries", move |tx| {
                if !touch(tx, &playlist_id, &user_id, now)? {
                    return Ok(Written::Missing);
                }
                if let Some(name) = &name {
                    tx.execute(sql::RENAME.text(), params![name, playlist_id, user_id])?;
                }
                tx.execute(sql::CLEAR_ENTRIES.text(), params![playlist_id])?;
                insert_entries(tx, &playlist_id, None, &entries, now)?;
                let (name, is_public, created_at) =
                    tx.query_row(sql::SAVED.text(), params![playlist_id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)? != 0,
                            epoch(row.get(2)?),
                        ))
                    })?;
                Ok(Written::Done(SavedPlaylist {
                    id: playlist_id,
                    name,
                    is_public,
                    created_at,
                    updated_at: epoch(now / 1000),
                }))
            })
            .await
    }

    /// Replace every entry with the songs an import took from the
    /// playlist's source, in order, and keep which source song each entry
    /// is, in one transaction: a write that fails leaves the old entries
    /// and songs in place.
    pub async fn replace_from_source(
        &self,
        playlist_id: &str,
        songs: Vec<SourceEntry>,
    ) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.replace_from_source", move |tx| {
                if !touch(tx, &playlist_id, &user_id, now)? {
                    return Ok(Written::Missing);
                }
                tx.execute(sql::CLEAR_SOURCE_SONGS.text(), params![playlist_id])?;
                tx.execute(sql::CLEAR_ENTRIES.text(), params![playlist_id])?;
                let entries: Vec<NewEntry> = songs.iter().map(|song| song.entry.clone()).collect();
                let added = insert_entries(tx, &playlist_id, None, &entries, now)?;
                for (song, entry_id) in songs.iter().zip(&added) {
                    insert_source_song(tx, &playlist_id, song, entry_id, now)?;
                }
                Ok(Written::Done(()))
            })
            .await
    }

    /// Add the songs of the playlist's source it has not taken before, at
    /// `place`, and keep that it took them, in one transaction. A song
    /// taken before is skipped, whether or not its entry is still there, so
    /// a repeated write adds nothing and a song removed by hand is never
    /// added back. A playlist that has taken no song yet (one imported
    /// before its songs were kept) first pairs its entries with the songs
    /// they already are ([`seed_source_songs`]). The source gives each song
    /// once.
    pub async fn append_from_source(
        &self,
        playlist_id: &str,
        songs: Vec<SourceEntry>,
        place: SourcePlace,
    ) -> Result<Written<SourceTaken>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.append_from_source", move |tx| {
                if !owns(tx, &playlist_id, &user_id)? {
                    return Ok(Written::Missing);
                }
                let any: bool =
                    tx.query_row(sql::HAS_SOURCE_SONGS.text(), params![playlist_id], |row| {
                        row.get(0)
                    })?;
                let (candidates, seeded) = if any {
                    (songs.iter().collect(), 0)
                } else {
                    seed_source_songs(tx, &playlist_id, &songs, now)?
                };
                let mut new = Vec::with_capacity(candidates.len());
                for song in candidates {
                    let seen = tx
                        .query_row(
                            sql::SOURCE_SONG_SEEN.text(),
                            params![playlist_id, song.song_id],
                            |_| Ok(()),
                        )
                        .optional()?;
                    if seen.is_none() {
                        new.push(song);
                    }
                }
                if new.is_empty() {
                    return Ok(Written::Done(SourceTaken {
                        added: Vec::new(),
                        seeded,
                    }));
                }
                let entries: Vec<NewEntry> = new.iter().map(|song| song.entry.clone()).collect();
                let position = match place {
                    SourcePlace::End => None,
                    SourcePlace::Top => Some(0),
                };
                let added = insert_entries(tx, &playlist_id, position, &entries, now)?;
                for (song, entry_id) in new.iter().zip(&added) {
                    insert_source_song(tx, &playlist_id, song, entry_id, now)?;
                }
                touch(tx, &playlist_id, &user_id, now)?;
                Ok(Written::Done(SourceTaken { added, seeded }))
            })
            .await
    }

    /// Point each entry with no source yet whose album the library owns at
    /// the library's copy (see [`library_links`]); when the album is held
    /// twice, the copy a lookup by its group opens comes first. Answers how
    /// many entries that linked; none when the playlist is not the user's.
    /// Opening a playlist runs this, and so does an import once its rows
    /// are written (an importer that stores rows with no source should
    /// too), so music apps list the songs before anyone opens the page.
    /// The links are worked out on a read; the writer is taken only when
    /// there is something to link, and each link checks again that the
    /// entry has no source, the track is indexed and the playlist is the
    /// user's.
    pub async fn link_library_copies(&self, playlist_id: &str) -> Result<usize, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let links = {
            let (user_id, playlist_id) = (user_id.clone(), playlist_id.clone());
            self.store
                .db
                .read("playlists.library_copy_candidates", move |conn| {
                    let candidates = library_copy_candidates(conn, &user_id, &playlist_id)?;
                    Ok(library_links(&candidates))
                })
                .await?
        };
        if links.is_empty() {
            return Ok(0);
        }
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.link_library_copies", move |tx| {
                let mut linked = 0;
                for (entry_id, track_id) in &links {
                    linked += tx.execute(
                        sql::LINK_LIBRARY_COPY.text(),
                        params![user_id, playlist_id, entry_id, track_id],
                    )?;
                }
                if linked > 0 {
                    touch(tx, &playlist_id, &user_id, now)?;
                }
                Ok(linked)
            })
            .await
    }

    /// Store or replace the uploaded cover.
    pub async fn set_cover(
        &self,
        playlist_id: &str,
        cover: CoverImage,
    ) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.set_cover", move |tx| {
                if !touch(tx, &playlist_id, &user_id, now)? {
                    return Ok(Written::Missing);
                }
                tx.execute(
                    sql::UPSERT_COVER.text(),
                    params![playlist_id, cover.content_type, cover.bytes, now],
                )?;
                Ok(Written::Done(()))
            })
            .await
    }

    /// Delete the uploaded cover. `Missing` when there was none.
    pub async fn remove_cover(&self, playlist_id: &str) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let now = self.store.db.now_ms();
        self.store
            .db
            .write("playlists.remove_cover", move |tx| {
                if !owns(tx, &playlist_id, &user_id)? {
                    return Ok(Written::Missing);
                }
                let changed = tx.execute(sql::DELETE_COVER.text(), params![playlist_id])?;
                if changed == 0 {
                    return Ok(Written::Missing);
                }
                touch(tx, &playlist_id, &user_id, now)?;
                Ok(Written::Done(()))
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::{library_links, titles_agree, titles_agree_at_place};
    use crate::collections::sql::LibraryCopyCandidate;

    fn candidate(entry: (&str, i64), track: (&str, &str, i64)) -> LibraryCopyCandidate {
        LibraryCopyCandidate {
            entry_id: "entry".to_owned(),
            entry_title: entry.0.to_owned(),
            entry_disc: 1,
            entry_track: Some(entry.1),
            track_id: track.0.to_owned(),
            track_title: track.1.to_owned(),
            track_disc: 1,
            track_number: track.2,
        }
    }

    /// A credit dropped at the place never beats the same title elsewhere,
    /// on the same copy or another edition's, and links only what is left.
    #[test]
    fn a_loose_match_at_the_place_links_only_when_nothing_strict_does() {
        let entry = ("Opener (feat. Aurora)", 3);
        let one_copy = [
            candidate(entry, ("t3", "Opener (feat. Nightfall)", 3)),
            candidate(entry, ("t9", "Opener (feat. Aurora)", 9)),
        ];
        assert_eq!(
            library_links(&one_copy),
            [("entry".to_owned(), "t9".to_owned())]
        );
        let two_editions = [
            candidate(entry, ("a3", "Opener (feat. Nightfall)", 3)),
            candidate(entry, ("b3", "Opener (feat. Aurora) - Remaster", 3)),
        ];
        assert_eq!(
            library_links(&two_editions),
            [("entry".to_owned(), "b3".to_owned())]
        );
        let loose_only = [
            candidate(("Night & Day", 2), ("t1", "Opener", 1)),
            candidate(("Night & Day", 2), ("t2", "Night and Day", 2)),
        ];
        assert_eq!(
            library_links(&loose_only),
            [("entry".to_owned(), "t2".to_owned())]
        );
    }

    #[test]
    fn titles_agree_across_typography_and_suffixes() {
        for (entry, track) in [
            ("Opener", "Opener"),
            ("Don’t Stop", "Don't Stop"),
            ("Re-Entry", "Re‐Entry"),
            ("Song", "Song - 2011 Remaster"),
            ("Opener - Live", "Opener (live)"),
            ("CAFÉ", "cafe"),
            ("Song 2011 Remaster", "Song"),
            ("Song (feat. X)", "Song"),
            ("Song (feat. X)", "Song (feat. X) - Remaster"),
        ] {
            assert!(titles_agree(entry, track), "{entry:?} / {track:?}");
        }
        for (entry, track) in [
            ("Opener (feat. Aurora) (Live)", "Opener (live)"),
            ("Opener [feat. Aurora] - Radio Version", "Opener (radio)"),
            (
                "Second Song (Aurora & Nightfall feat. Opener) - Remix",
                "Second Song (remix)",
            ),
            ("Night & Day", "Night and Day"),
            ("Rock N Roll", "Rock 'n' Roll"),
            ("Dawn / Dusk 12/24", "Dawn/Dusk 12/24"),
            ("Song (feat. A)", "Song (feat. B)"),
            ("Night ＆ Day（feat. Aurora）", "Night and Day"),
        ] {
            assert!(titles_agree_at_place(entry, track), "{entry:?} / {track:?}");
        }
        for (entry, track) in [
            ("Opener", "Second Song"),
            ("", "Second Song"),
            ("Intro", ""),
            ("Go", "Gone"),
            ("Love", "Lovesong"),
            ("Lovesong", "Love"),
            ("The Song", "Song"),
            ("Opener (Acoustic)", "Opener (Live)"),
            ("Opener (with Strings)", "Opener (Live)"),
            ("Opener (feat. Aurora)", "Opener (Acoustic)"),
        ] {
            assert!(!titles_agree(entry, track), "{entry:?} / {track:?}");
            assert!(
                !titles_agree_at_place(entry, track),
                "{entry:?} / {track:?}"
            );
        }
    }
}
