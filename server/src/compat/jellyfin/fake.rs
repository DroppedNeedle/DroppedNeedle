//! Test doubles for the Jellyfin seams: a seedable library, an id map, a
//! byte engine that replays the range contract and a recording session
//! sink. Compiled only for tests.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use super::params::SortKey;
use super::seams::*;
use crate::compat::body::AudioBody;

/// Deterministic `sha256("kind:internal")[:32]` with an in-memory reverse
/// table (v2 `CompatIdMapService` derivation; the persisted table is not
/// bound yet).
#[derive(Debug, Clone, Default)]
pub struct MemoryIds {
    reverse: std::sync::Arc<Mutex<HashMap<String, (String, String)>>>,
}

impl MemoryIds {
    /// Empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// The deterministic derivation, exposed so tests can precompute ids.
    pub fn derive(kind: &str, internal: &str) -> String {
        format!(
            "{:x}",
            Sha256::digest(format!("{kind}:{internal}").as_bytes())
        )[..32]
            .to_owned()
    }

    fn normalize(jf_id: &str) -> String {
        jf_id.replace('-', "").trim().to_lowercase()
    }
}

impl IdMap for MemoryIds {
    async fn to_jf(&self, kind: &str, internal: &str) -> String {
        let jf_id = Self::derive(kind, internal);
        if let Ok(mut reverse) = self.reverse.lock() {
            reverse.insert(jf_id.clone(), (kind.to_owned(), internal.to_owned()));
        }
        jf_id
    }

    async fn from_jf(&self, jf_id: &str) -> Option<(String, String)> {
        self.reverse
            .lock()
            .ok()
            .and_then(|reverse| reverse.get(&Self::normalize(jf_id)).cloned())
    }
}

#[derive(Debug, Clone)]
struct StoredPlaylist {
    id: String,
    name: String,
    owner: String,
    entries: Vec<PlaylistEntry>,
    next_entry: usize,
}

#[derive(Debug, Default)]
struct MemoryRows {
    tracks: Vec<TrackView>,
    albums: Vec<AlbumView>,
    artists: Vec<ArtistView>,
    album_artist_mbids: HashSet<String>,
    genres: Vec<GenreView>,
    playlists: Vec<StoredPlaylist>,
    next_playlist: usize,
    favorites: HashSet<(String, String, String)>,
    covers: HashMap<(String, String), CoverBytes>,
    artist_images: HashMap<String, CoverBytes>,
}

/// In-memory [`LibraryRead`] for tests and the empty production library.
#[derive(Debug, Clone, Default)]
pub struct FakeLibrary {
    rows: std::sync::Arc<Mutex<MemoryRows>>,
}

impl FakeLibrary {
    /// Empty library.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed one track.
    pub fn add_track(&self, track: TrackView) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.tracks.push(track);
        }
    }

    /// Seed one album.
    pub fn add_album(&self, album: AlbumView) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.albums.push(album);
        }
    }

    /// Seed one artist (`album_artist` lists it under `/Artists/AlbumArtists`).
    pub fn add_artist(&self, artist: ArtistView, album_artist: bool) {
        if let Ok(mut rows) = self.rows.lock() {
            if album_artist {
                rows.album_artist_mbids.insert(artist.artist_mbid.clone());
            }
            rows.artists.push(artist);
        }
    }

    /// Seed one genre.
    pub fn add_genre(&self, genre: GenreView) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.genres.push(genre);
        }
    }

    /// Seed release art for one size bucket (`250`, `500`, `1200`).
    pub fn add_cover(&self, rg_mbid: &str, size: &str, bytes: Vec<u8>, content_type: &str) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.covers.insert(
                (rg_mbid.to_owned(), size.to_owned()),
                CoverBytes {
                    bytes,
                    content_type: content_type.to_owned(),
                },
            );
        }
    }

    /// Seed one artist image.
    pub fn add_artist_image(&self, mbid: &str, bytes: Vec<u8>, content_type: &str) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.artist_images.insert(
                mbid.to_owned(),
                CoverBytes {
                    bytes,
                    content_type: content_type.to_owned(),
                },
            );
        }
    }

    fn with_rows<T>(&self, f: impl FnOnce(&MemoryRows) -> T) -> Option<T> {
        self.rows.lock().ok().map(|rows| f(&rows))
    }

    fn mutate(&self, f: impl FnOnce(&mut MemoryRows)) {
        if let Ok(mut rows) = self.rows.lock() {
            f(&mut rows);
        }
    }
}

/// `tag-{rg}` when any cover size is seeded for the album.
fn cover_tag(rows: &MemoryRows, rg_mbid: &str) -> Option<String> {
    rows.covers
        .keys()
        .any(|(rg, _)| rg == rg_mbid)
        .then(|| format!("tag-{rg_mbid}"))
}

fn is_favorite(rows: &MemoryRows, user_id: &str, kind: &str, id: &str) -> bool {
    rows.favorites
        .contains(&(user_id.to_owned(), kind.to_owned(), id.to_owned()))
}

fn track_for(rows: &MemoryRows, user_id: &str, track: &TrackView) -> TrackView {
    let mut track = track.clone();
    track.starred = is_favorite(rows, user_id, "track", &track.file_id);
    track.album_image_tag = track.rg_mbid.as_deref().and_then(|rg| cover_tag(rows, rg));
    track
}

fn album_for(rows: &MemoryRows, user_id: &str, album: &AlbumView) -> AlbumView {
    let mut album = album.clone();
    album.starred = is_favorite(rows, user_id, "album", &album.rg_mbid);
    album.image_tag = cover_tag(rows, &album.rg_mbid);
    album
}

fn artist_for(rows: &MemoryRows, user_id: &str, artist: &ArtistView) -> ArtistView {
    let mut artist = artist.clone();
    artist.starred = is_favorite(rows, user_id, "artist", &artist.artist_mbid);
    artist.image_tag = rows
        .artist_images
        .contains_key(&artist.artist_mbid)
        .then(|| format!("tag-{}", artist.artist_mbid));
    artist
}

/// `[start, start + limit)` of an already-ordered list, plus its length.
fn slice<T>(mut items: Vec<T>, start: usize, limit: usize) -> (Vec<T>, usize) {
    let total = items.len();
    if start >= total {
        return (Vec::new(), total);
    }
    let end = start.saturating_add(limit).min(total);
    items.truncate(end);
    (items.split_off(start), total)
}

fn matches(haystacks: &[Option<&str>], needle: &str) -> bool {
    let needle = needle.to_lowercase();
    haystacks
        .iter()
        .flatten()
        .any(|h| h.to_lowercase().contains(&needle))
}

fn flip(desc: bool, ord: std::cmp::Ordering) -> std::cmp::Ordering {
    if desc { ord.reverse() } else { ord }
}

/// Stable stand-in shuffle for `SortBy=Random`: tests pin stability and
/// completeness, not randomness.
fn fnv(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn sort_tracks(tracks: &mut [TrackView], key: SortKey, desc: bool) {
    tracks.sort_by(|a, b| {
        let ord = match key {
            SortKey::Recent => a
                .created_at
                .partial_cmp(&b.created_at)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            SortKey::Year | SortKey::PremiereDate => a.year.cmp(&b.year),
            SortKey::Random => fnv(&a.file_id).cmp(&fnv(&b.file_id)),
            SortKey::DatePlayed => a
                .last_played
                .partial_cmp(&b.last_played)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::PlayCount => a.play_count.cmp(&b.play_count),
        };
        flip(desc, ord.then_with(|| a.file_id.cmp(&b.file_id)))
    });
}

fn sort_albums(albums: &mut [AlbumView], key: SortKey, desc: bool) {
    albums.sort_by(|a, b| {
        let ord = match key {
            SortKey::Recent => a
                .date_added
                .partial_cmp(&b.date_added)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            SortKey::Year | SortKey::PremiereDate => a.year.cmp(&b.year),
            SortKey::Random => fnv(&a.rg_mbid).cmp(&fnv(&b.rg_mbid)),
            SortKey::DatePlayed => a
                .last_played
                .partial_cmp(&b.last_played)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::PlayCount => a.play_count.cmp(&b.play_count),
        };
        flip(desc, ord.then_with(|| a.rg_mbid.cmp(&b.rg_mbid)))
    });
}

impl LibraryRead for FakeLibrary {
    async fn track_page(
        &self,
        user_id: &str,
        filter: &TrackFilter,
        sort: ItemSort,
        start: usize,
        limit: usize,
    ) -> (Vec<TrackView>, usize) {
        let mut tracks = self
            .with_rows(|rows| {
                rows.tracks
                    .iter()
                    .filter(|t| {
                        filter
                            .album
                            .as_ref()
                            .is_none_or(|album| t.rg_mbid.as_ref() == Some(album))
                            && (filter.artists.is_empty()
                                || t.artist_mbid
                                    .as_ref()
                                    .is_some_and(|m| filter.artists.contains(m)))
                            && (filter.album_artists.is_empty()
                                || t.album_artist_mbid
                                    .as_ref()
                                    .is_some_and(|m| filter.album_artists.contains(m)))
                            && filter.search.as_deref().is_none_or(|needle| {
                                matches(
                                    &[
                                        Some(t.title.as_str()),
                                        t.artist_name.as_deref(),
                                        t.album_title.as_deref(),
                                    ],
                                    needle,
                                )
                            })
                    })
                    .map(|t| track_for(rows, user_id, t))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        match sort {
            ItemSort::Catalog => {}
            ItemSort::Disc => tracks.sort_by(|a, b| {
                (
                    a.disc_number.unwrap_or(0),
                    a.track_number.unwrap_or(0),
                    &a.file_id,
                )
                    .cmp(&(
                        b.disc_number.unwrap_or(0),
                        b.track_number.unwrap_or(0),
                        &b.file_id,
                    ))
            }),
            ItemSort::By(key, desc) => {
                // History sorts page from play history: unplayed drop out.
                if key == SortKey::DatePlayed {
                    tracks.retain(|t| t.last_played.is_some());
                } else if key == SortKey::PlayCount {
                    tracks.retain(|t| t.play_count > 0);
                }
                sort_tracks(&mut tracks, key, desc);
            }
        }
        slice(tracks, start, limit)
    }

    async fn tracks_by_ids(&self, user_id: &str, ids: &[String]) -> Vec<TrackView> {
        self.with_rows(|rows| {
            ids.iter()
                .filter_map(|id| rows.tracks.iter().find(|t| &t.file_id == id))
                .map(|t| track_for(rows, user_id, t))
                .collect()
        })
        .unwrap_or_default()
    }

    async fn track(&self, user_id: &str, file_id: &str) -> Option<TrackView> {
        self.with_rows(|rows| {
            rows.tracks
                .iter()
                .find(|t| t.file_id == file_id)
                .map(|t| track_for(rows, user_id, t))
        })
        .flatten()
    }

    async fn album_page(
        &self,
        user_id: &str,
        filter: &AlbumFilter,
        sort: ItemSort,
        start: usize,
        limit: usize,
    ) -> (Vec<AlbumView>, usize) {
        let mut albums = self
            .with_rows(|rows| {
                let appears_on: HashSet<&str> = rows
                    .tracks
                    .iter()
                    .filter(|t| {
                        t.artist_mbid
                            .as_ref()
                            .is_some_and(|m| filter.appears_on.contains(m))
                            && t.album_artist_mbid
                                .as_ref()
                                .is_none_or(|m| !filter.appears_on.contains(m))
                    })
                    .filter_map(|t| t.rg_mbid.as_deref())
                    .collect();
                rows.albums
                    .iter()
                    .filter(|a| {
                        (filter.artists.is_empty()
                            || a.artist_mbid
                                .as_ref()
                                .is_some_and(|m| filter.artists.contains(m)))
                            && (filter.appears_on.is_empty()
                                || appears_on.contains(a.rg_mbid.as_str()))
                            && filter.search.as_deref().is_none_or(|needle| {
                                matches(&[Some(a.title.as_str()), a.artist_name.as_deref()], needle)
                            })
                    })
                    .map(|a| album_for(rows, user_id, a))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let ItemSort::By(key, desc) = sort {
            if key == SortKey::DatePlayed {
                albums.retain(|a| a.last_played.is_some());
            } else if key == SortKey::PlayCount {
                albums.retain(|a| a.play_count > 0);
            }
            sort_albums(&mut albums, key, desc);
        }
        slice(albums, start, limit)
    }

    async fn albums_by_ids(&self, user_id: &str, ids: &[String]) -> Vec<AlbumView> {
        self.with_rows(|rows| {
            ids.iter()
                .filter_map(|id| rows.albums.iter().find(|a| &a.rg_mbid == id))
                .map(|a| album_for(rows, user_id, a))
                .collect()
        })
        .unwrap_or_default()
    }

    async fn album(&self, user_id: &str, rg_mbid: &str) -> Option<AlbumView> {
        self.with_rows(|rows| {
            rows.albums
                .iter()
                .find(|a| a.rg_mbid == rg_mbid)
                .map(|a| album_for(rows, user_id, a))
        })
        .flatten()
    }

    async fn artist_page(
        &self,
        user_id: &str,
        scope: ArtistScope,
        search: Option<&str>,
        start: usize,
        limit: usize,
    ) -> (Vec<ArtistView>, usize) {
        let artists = self
            .with_rows(|rows| {
                rows.artists
                    .iter()
                    .filter(|a| {
                        (scope == ArtistScope::All
                            || rows.album_artist_mbids.contains(&a.artist_mbid))
                            && search.is_none_or(|needle| matches(&[Some(a.name.as_str())], needle))
                    })
                    .map(|a| artist_for(rows, user_id, a))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        slice(artists, start, limit)
    }

    async fn artist(&self, user_id: &str, mbid: &str) -> Option<ArtistView> {
        self.with_rows(|rows| {
            rows.artists
                .iter()
                .find(|a| a.artist_mbid == mbid)
                .map(|a| artist_for(rows, user_id, a))
        })
        .flatten()
    }

    async fn genres(&self) -> Vec<GenreView> {
        self.with_rows(|rows| rows.genres.clone())
            .unwrap_or_default()
    }

    async fn playlists(&self, user_id: &str) -> Vec<PlaylistView> {
        let durations: HashMap<String, Option<f64>> = self
            .with_rows(|rows| {
                rows.tracks
                    .iter()
                    .map(|t| (t.file_id.clone(), t.duration_seconds))
                    .collect()
            })
            .unwrap_or_default();
        self.with_rows(|rows| {
            rows.playlists
                .iter()
                .filter(|p| p.owner == user_id)
                .map(|p| {
                    let streamable: Vec<&PlaylistEntry> =
                        p.entries.iter().filter(|e| e.file_id.is_some()).collect();
                    let total: f64 = streamable
                        .iter()
                        .filter_map(|e| {
                            e.file_id
                                .as_ref()
                                .and_then(|f| durations.get(f).copied().flatten())
                        })
                        .sum();
                    PlaylistView {
                        id: p.id.clone(),
                        name: p.name.clone(),
                        track_count: streamable.len(),
                        total_duration_seconds: if total > 0.0 { Some(total) } else { None },
                    }
                })
                .collect()
        })
        .unwrap_or_default()
    }

    async fn playlist(&self, user_id: &str, id: &str) -> Option<PlaylistDetail> {
        self.with_rows(|rows| {
            rows.playlists
                .iter()
                .find(|p| p.id == id && p.owner == user_id)
                .map(|p| PlaylistDetail {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    entries: p.entries.clone(),
                })
        })
        .flatten()
    }

    async fn create_playlist(&self, user_id: &str, name: &str) -> Result<String, WriteRefusal> {
        let mut id = String::new();
        self.mutate(|rows| {
            rows.next_playlist += 1;
            id = format!("pl-{}", rows.next_playlist);
            rows.playlists.push(StoredPlaylist {
                id: id.clone(),
                name: name.to_owned(),
                owner: user_id.to_owned(),
                entries: Vec::new(),
                next_entry: 0,
            });
        });
        Ok(id)
    }

    async fn add_playlist_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        file_id: &str,
    ) -> Result<(), WriteRefusal> {
        self.mutate(|rows| {
            if let Some(p) = rows
                .playlists
                .iter_mut()
                .find(|p| p.id == playlist_id && p.owner == user_id)
            {
                p.next_entry += 1;
                p.entries.push(PlaylistEntry {
                    id: format!("e{}", p.next_entry),
                    file_id: Some(file_id.to_owned()),
                });
            }
        });
        Ok(())
    }

    async fn remove_playlist_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> Result<(), WriteRefusal> {
        self.mutate(|rows| {
            if let Some(p) = rows
                .playlists
                .iter_mut()
                .find(|p| p.id == playlist_id && p.owner == user_id)
            {
                p.entries.retain(|e| !entry_ids.contains(&e.id));
            }
        });
        Ok(())
    }

    async fn move_playlist_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_id: &str,
        index: usize,
    ) -> Result<(), WriteRefusal> {
        self.mutate(|rows| {
            if let Some(p) = rows
                .playlists
                .iter_mut()
                .find(|p| p.id == playlist_id && p.owner == user_id)
                && let Some(pos) = p.entries.iter().position(|e| e.id == entry_id)
            {
                let entry = p.entries.remove(pos);
                let at = index.min(p.entries.len());
                p.entries.insert(at, entry);
            }
        });
        Ok(())
    }

    async fn favorites(&self, user_id: &str, kind: &str) -> Vec<String> {
        self.with_rows(|rows| {
            let mut out: Vec<String> = rows
                .favorites
                .iter()
                .filter(|(u, k, _)| u == user_id && k == kind)
                .map(|(_, _, internal)| internal.clone())
                .collect();
            out.sort();
            out
        })
        .unwrap_or_default()
    }

    async fn set_favorite(
        &self,
        user_id: &str,
        kind: &str,
        internal: &str,
        add: bool,
    ) -> Result<(), WriteRefusal> {
        self.mutate(|rows| {
            let key = (user_id.to_owned(), kind.to_owned(), internal.to_owned());
            if add {
                rows.favorites.insert(key);
            } else {
                rows.favorites.remove(&key);
            }
        });
        Ok(())
    }

    async fn cover(&self, rg_mbid: &str, size: &str) -> Option<CoverBytes> {
        self.with_rows(|rows| {
            rows.covers
                .get(&(rg_mbid.to_owned(), size.to_owned()))
                .cloned()
        })
        .flatten()
    }

    async fn artist_image(&self, mbid: &str) -> Option<CoverBytes> {
        self.with_rows(|rows| rows.artist_images.get(mbid).cloned())
            .flatten()
    }
}

/// In-memory [`StreamEngine`]: seeded bytes served under the v2 range
/// contract (single-range `bytes=` only; open and suffix ranges 206;
/// multi-range, malformed, empty, or unsatisfiable → 416 with
/// `Content-Range: bytes */N` and no body).
/// Seeded file bytes plus lowercase container per file id.
type SeededFiles = std::sync::Arc<Mutex<HashMap<String, (Vec<u8>, Option<String>)>>>;

#[derive(Debug, Clone, Default)]
pub struct MemoryEngine {
    files: SeededFiles,
}

impl MemoryEngine {
    /// Empty engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed one file's bytes plus its lowercase container (`mp3`, `flac`…).
    pub fn add_file(&self, file_id: &str, bytes: Vec<u8>, format: Option<&str>) {
        if let Ok(mut files) = self.files.lock() {
            files.insert(file_id.to_owned(), (bytes, format.map(str::to_owned)));
        }
    }

    fn lookup(&self, file_id: &str) -> Option<(Vec<u8>, Option<String>)> {
        self.files.lock().ok()?.get(file_id).cloned()
    }
}

/// Resolve a `Range` header against `size`: `Ok(None)` is the full body,
/// `Ok(Some((start, end)))` an inclusive byte span, `Err(())` a 416.
fn resolve_range(range: Option<&str>, size: usize) -> Result<Option<(usize, usize)>, ()> {
    let Some(header) = range else {
        return Ok(None);
    };
    if size == 0 {
        return Err(());
    }
    // Surrounding whitespace is insignificant on every path (the engine
    // parser and the Subsonic code trim too).
    let spec = header.trim().strip_prefix("bytes=").ok_or(())?;
    if spec.is_empty() || spec.contains(',') {
        return Err(());
    }
    let (start_s, end_s) = spec.split_once('-').ok_or(())?;
    if start_s.is_empty() {
        // Suffix range: the last N bytes. `bytes=-0` is unsatisfiable.
        let n: usize = end_s.parse().map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        let n = n.min(size);
        return Ok(Some((size - n, size - 1)));
    }
    let start: usize = start_s.parse().map_err(|_| ())?;
    if start >= size {
        return Err(());
    }
    if end_s.is_empty() {
        return Ok(Some((start, size - 1)));
    }
    let end: usize = end_s.parse().map_err(|_| ())?;
    if end < start {
        return Err(());
    }
    Ok(Some((start, end.min(size - 1))))
}

fn unsatisfied(size: usize) -> ByteOutcome {
    ByteOutcome {
        status: 416,
        headers: vec![("Content-Range".to_owned(), format!("bytes */{size}"))],
        body: AudioBody::empty(),
    }
}

impl StreamEngine for MemoryEngine {
    async fn direct(&self, file_id: &str, range: Option<&str>) -> ByteOutcome {
        let Some((bytes, format)) = self.lookup(file_id) else {
            return ByteOutcome {
                status: 404,
                headers: Vec::new(),
                body: AudioBody::empty(),
            };
        };
        let size = bytes.len();
        let span = match resolve_range(range, size) {
            Ok(span) => span,
            Err(()) => return unsatisfied(size),
        };
        // `Content-Encoding: identity` on every audio response: gzip drops
        // Content-Length and breaks seeking (as v2 found).
        let content_type = content_type_for_format(format.as_deref()).to_owned();
        match span {
            None => ByteOutcome {
                status: 200,
                headers: vec![
                    ("Content-Type".to_owned(), content_type),
                    ("Content-Length".to_owned(), size.to_string()),
                    ("Accept-Ranges".to_owned(), "bytes".to_owned()),
                    ("Content-Encoding".to_owned(), "identity".to_owned()),
                ],
                body: AudioBody::Bytes(bytes),
            },
            Some((start, end)) => ByteOutcome {
                status: 206,
                headers: vec![
                    ("Content-Type".to_owned(), content_type),
                    ("Content-Length".to_owned(), (end - start + 1).to_string()),
                    (
                        "Content-Range".to_owned(),
                        format!("bytes {start}-{end}/{size}"),
                    ),
                    ("Accept-Ranges".to_owned(), "bytes".to_owned()),
                    ("Content-Encoding".to_owned(), "identity".to_owned()),
                ],
                body: AudioBody::Bytes(bytes[start..=end].to_vec()),
            },
        }
    }

    async fn head(&self, file_id: &str, range: Option<&str>) -> ByteOutcome {
        let Some((bytes, format)) = self.lookup(file_id) else {
            return ByteOutcome {
                status: 404,
                headers: Vec::new(),
                body: AudioBody::empty(),
            };
        };
        let size = bytes.len();
        let span = match resolve_range(range, size) {
            Ok(span) => span,
            Err(()) => return unsatisfied(size),
        };
        // GET-equivalent headers, empty body: HEAD honors Range (206 +
        // Content-Range) exactly like the stream engine does.
        let content_type = content_type_for_format(format.as_deref()).to_owned();
        match span {
            None => ByteOutcome {
                status: 200,
                headers: vec![
                    ("Content-Type".to_owned(), content_type),
                    ("Content-Length".to_owned(), size.to_string()),
                    ("Accept-Ranges".to_owned(), "bytes".to_owned()),
                    ("Content-Encoding".to_owned(), "identity".to_owned()),
                ],
                body: AudioBody::empty(),
            },
            Some((start, end)) => ByteOutcome {
                status: 206,
                headers: vec![
                    ("Content-Type".to_owned(), content_type),
                    ("Content-Length".to_owned(), (end - start + 1).to_string()),
                    (
                        "Content-Range".to_owned(),
                        format!("bytes {start}-{end}/{size}"),
                    ),
                    ("Accept-Ranges".to_owned(), "bytes".to_owned()),
                    ("Content-Encoding".to_owned(), "identity".to_owned()),
                ],
                body: AudioBody::empty(),
            },
        }
    }

    async fn transcode(
        &self,
        file_id: &str,
        format: &str,
        bitrate_kbps: u32,
        start_seconds: f64,
    ) -> ByteOutcome {
        // Marker bytes only: production binds the real ffmpeg pipe. The
        // header set is the real v2 contract (200, no ranges, no store,
        // identity encoding, never a Content-Length on Jellyfin). Transcoded
        // opus rides an ogg container (`-f ogg`), hence `audio/ogg`, unlike
        // direct .opus files, which serve as `audio/opus` (as in v2).
        let _ = self.lookup(file_id);
        let content_type = if format.eq_ignore_ascii_case("opus") {
            "audio/ogg"
        } else {
            content_type_for_format(Some(format))
        };
        ByteOutcome {
            status: 200,
            headers: vec![
                ("Content-Type".to_owned(), content_type.to_owned()),
                ("Accept-Ranges".to_owned(), "none".to_owned()),
                ("Cache-Control".to_owned(), "no-store".to_owned()),
                ("Content-Encoding".to_owned(), "identity".to_owned()),
            ],
            body: AudioBody::Bytes(
                format!("transcoded:{format}:{bitrate_kbps}:{start_seconds:.3}").into_bytes(),
            ),
        }
    }
}

/// Observed session call, for test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCall {
    MarkStarted {
        user_id: String,
        key: String,
    },
    NowPlaying {
        user_id: String,
        file_id: String,
    },
    Progress {
        user_id: String,
        file_id: String,
        position_ms: Option<i64>,
        paused: bool,
    },
    ClearPresence {
        user_id: String,
    },
    Scrobble {
        user_id: String,
        file_id: String,
    },
}

/// Recording [`PlaybackSessions`] for tests.
#[derive(Debug, Clone, Default)]
pub struct MemorySessions {
    started: std::sync::Arc<Mutex<HashSet<(String, String)>>>,
    calls: std::sync::Arc<Mutex<Vec<SessionCall>>>,
}

impl MemorySessions {
    /// Empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call observed so far.
    pub fn calls(&self) -> Vec<SessionCall> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }

    fn record(&self, call: SessionCall) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call);
        }
    }
}

impl PlaybackSessions for MemorySessions {
    async fn mark_started(&self, user_id: &str, key: &str) {
        if let Ok(mut started) = self.started.lock() {
            started.insert((user_id.to_owned(), key.to_owned()));
        }
        self.record(SessionCall::MarkStarted {
            user_id: user_id.to_owned(),
            key: key.to_owned(),
        });
    }

    async fn pop_started(&self, user_id: &str, key: &str) -> Option<String> {
        self.started.lock().ok().and_then(|mut started| {
            started
                .remove(&(user_id.to_owned(), key.to_owned()))
                .then(|| "started-at".to_owned())
        })
    }

    async fn now_playing(&self, user_id: &str, file_id: &str, _client: Option<&str>) {
        self.record(SessionCall::NowPlaying {
            user_id: user_id.to_owned(),
            file_id: file_id.to_owned(),
        });
    }

    async fn progress(&self, user_id: &str, file_id: &str, position_ms: Option<i64>, paused: bool) {
        self.record(SessionCall::Progress {
            user_id: user_id.to_owned(),
            file_id: file_id.to_owned(),
            position_ms,
            paused,
        });
    }

    async fn clear_presence(&self, user_id: &str, _client: Option<&str>) {
        self.record(SessionCall::ClearPresence {
            user_id: user_id.to_owned(),
        });
    }

    async fn scrobble(&self, user_id: &str, file_id: &str, _client: Option<&str>) {
        self.record(SessionCall::Scrobble {
            user_id: user_id.to_owned(),
            file_id: file_id.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, title: &str, year: i32) -> TrackView {
        TrackView {
            file_id: id.to_owned(),
            title: title.to_owned(),
            year: Some(year),
            ..TrackView::default()
        }
    }

    #[test]
    fn sorts_break_ties_by_id_and_flip_whole_order() {
        let mut tracks = vec![track("b", "Same", 2000), track("a", "Same", 1990)];
        sort_tracks(&mut tracks, SortKey::Title, false);
        assert_eq!(tracks[0].file_id, "a", "ties fall back to the id");
        sort_tracks(&mut tracks, SortKey::Year, true);
        assert_eq!(tracks[0].year, Some(2000));
    }

    #[test]
    fn slices_clamp_to_the_list() {
        assert_eq!(slice(vec![1, 2, 3, 4], 1, 2), (vec![2, 3], 4));
        assert_eq!(slice(vec![1, 2, 3, 4], 1, ALL), (vec![2, 3, 4], 4));
        assert_eq!(slice(vec![1, 2], 5, 1), (Vec::new(), 2));
    }
}
