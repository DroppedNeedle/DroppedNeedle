//! Test doubles for the Jellyfin seams: a seedable library, an id map, a
//! byte engine that replays the range contract and a recording session
//! sink. Compiled only for tests.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use super::seams::*;

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

impl LibraryRead for FakeLibrary {
    async fn tracks(&self, user_id: &str) -> Vec<TrackView> {
        self.with_rows(|rows| {
            rows.tracks
                .iter()
                .map(|t| {
                    let mut t = t.clone();
                    t.starred = rows.favorites.contains(&(
                        user_id.to_owned(),
                        "track".to_owned(),
                        t.file_id.clone(),
                    ));
                    t
                })
                .collect()
        })
        .unwrap_or_default()
    }

    async fn track(&self, user_id: &str, file_id: &str) -> Option<TrackView> {
        self.with_rows(|rows| {
            rows.tracks.iter().find(|t| t.file_id == file_id).map(|t| {
                let mut t = t.clone();
                t.starred = rows.favorites.contains(&(
                    user_id.to_owned(),
                    "track".to_owned(),
                    t.file_id.clone(),
                ));
                t
            })
        })
        .flatten()
    }

    async fn albums(&self, user_id: &str) -> Vec<AlbumView> {
        self.with_rows(|rows| {
            rows.albums
                .iter()
                .map(|a| {
                    let mut a = a.clone();
                    a.starred = rows.favorites.contains(&(
                        user_id.to_owned(),
                        "album".to_owned(),
                        a.rg_mbid.clone(),
                    ));
                    a
                })
                .collect()
        })
        .unwrap_or_default()
    }

    async fn album(&self, user_id: &str, rg_mbid: &str) -> Option<AlbumView> {
        self.with_rows(|rows| {
            rows.albums.iter().find(|a| a.rg_mbid == rg_mbid).map(|a| {
                let mut a = a.clone();
                a.starred = rows.favorites.contains(&(
                    user_id.to_owned(),
                    "album".to_owned(),
                    a.rg_mbid.clone(),
                ));
                a
            })
        })
        .flatten()
    }

    async fn artists(&self, user_id: &str, scope: ArtistScope) -> Vec<ArtistView> {
        self.with_rows(|rows| {
            rows.artists
                .iter()
                .filter(|a| {
                    scope == ArtistScope::All || rows.album_artist_mbids.contains(&a.artist_mbid)
                })
                .map(|a| {
                    let mut a = a.clone();
                    a.starred = rows.favorites.contains(&(
                        user_id.to_owned(),
                        "artist".to_owned(),
                        a.artist_mbid.clone(),
                    ));
                    a
                })
                .collect()
        })
        .unwrap_or_default()
    }

    async fn artist(&self, user_id: &str, mbid: &str) -> Option<ArtistView> {
        self.with_rows(|rows| {
            rows.artists
                .iter()
                .find(|a| a.artist_mbid == mbid)
                .map(|a| {
                    let mut a = a.clone();
                    a.starred = rows.favorites.contains(&(
                        user_id.to_owned(),
                        "artist".to_owned(),
                        a.artist_mbid.clone(),
                    ));
                    a
                })
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

    async fn create_playlist(&self, user_id: &str, name: &str) -> String {
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
        id
    }

    async fn add_playlist_entry(&self, user_id: &str, playlist_id: &str, file_id: &str) {
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
    }

    async fn remove_playlist_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_ids: &[String],
    ) {
        self.mutate(|rows| {
            if let Some(p) = rows
                .playlists
                .iter_mut()
                .find(|p| p.id == playlist_id && p.owner == user_id)
            {
                p.entries.retain(|e| !entry_ids.contains(&e.id));
            }
        });
    }

    async fn move_playlist_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_id: &str,
        index: usize,
    ) {
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

    async fn set_favorite(&self, user_id: &str, kind: &str, internal: &str, add: bool) {
        self.mutate(|rows| {
            let key = (user_id.to_owned(), kind.to_owned(), internal.to_owned());
            if add {
                rows.favorites.insert(key);
            } else {
                rows.favorites.remove(&key);
            }
        });
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

    async fn cover_tag(&self, rg_mbid: &str) -> Option<String> {
        self.with_rows(|rows| {
            rows.covers
                .keys()
                .any(|(rg, _)| rg == rg_mbid)
                .then(|| format!("tag-{rg_mbid}"))
        })
        .flatten()
    }

    async fn artist_tag(&self, mbid: &str) -> Option<String> {
        self.with_rows(|rows| {
            rows.artist_images
                .contains_key(mbid)
                .then(|| format!("tag-{mbid}"))
        })
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
        body: Vec::new(),
    }
}

impl StreamEngine for MemoryEngine {
    async fn direct(&self, file_id: &str, range: Option<&str>) -> ByteOutcome {
        let Some((bytes, format)) = self.lookup(file_id) else {
            return ByteOutcome {
                status: 404,
                headers: Vec::new(),
                body: Vec::new(),
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
                body: bytes,
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
                body: bytes[start..=end].to_vec(),
            },
        }
    }

    async fn head(&self, file_id: &str, range: Option<&str>) -> ByteOutcome {
        let Some((bytes, format)) = self.lookup(file_id) else {
            return ByteOutcome {
                status: 404,
                headers: Vec::new(),
                body: Vec::new(),
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
                body: Vec::new(),
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
                body: Vec::new(),
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
            body: format!("transcoded:{format}:{bitrate_kbps}:{start_seconds:.3}").into_bytes(),
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
