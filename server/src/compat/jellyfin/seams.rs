//! Boundary traits to code the Jellyfin routes do not own, plus in-memory
//! fakes. The router is generic over every trait here; `compat::setup`
//! binds the production adapters.
//!
//! - Auth: [`crate::auth::compat_auth::jellyfin::JellyfinPasswordStore`]
//!   (from `compat_auth`). This file only adds the thin [`Principal`]
//!   view over its `JellyfinUser`. Login bodies reuse `compat_auth`'s
//!   `login_echo_json`, so the Finamp/Manet login contract has one owner.
//! - Library reads: [`LibraryRead`]. Real view services (to be bound)
//!   (paged, user-scoped, real search); the router's in-memory filtering,
//!   sorting, and paging over the snapshot only pins the quirk contract.
//! - Streaming: [`StreamEngine`]. Production binds the stream engine
//!   (real `stream_track` byte contract, ffmpeg pipe, concurrency leases).
//!   [`MemoryEngine`] replays the same range/status contract over seeded
//!   bytes so the goldens pin it; [`decide`] ports v2's transcode policy rules
//!   verbatim for the PlaybackInfo direct/transcode fork.
//! - Ids: [`IdMap`]. A persisted compat id map is the intended binding;
//!   [`MemoryIds`] ports the deterministic `sha256("kind:internal")[:32]`
//!   derivation with an in-memory reverse table.
//! - Playback sessions: [`PlaybackSessions`]. Production binds the
//!   scrobble adapter (presence + scrobble forwarding).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::auth::compat_auth::jellyfin::{JellyfinUser, effective_name};
use sha2::{Digest, Sha256};

use super::models::UserDto;

// ===== Settings =====

/// The connect-apps settings subset the shim reads (v2
/// `get_connect_apps_settings`).
#[derive(Debug, Clone)]
pub struct JellyfinSettings {
    /// Kill switch, default off (v2 `jellyfin_enabled`). Disabled → HTTP 404
    /// on every route, before handler lookup (no method enumeration).
    pub enabled: bool,
    /// User-editable advertised name, default "DroppedNeedle".
    pub server_name: String,
    /// User-editable advertised version, default "10.10.6".
    pub server_version: String,
    /// Transcoding master switch.
    pub transcoding_enabled: bool,
    /// Quality ceiling in kbps, default 320: a ceiling, never a trigger.
    pub transcode_max_bitrate_kbps: u32,
    /// Default transcode output (`mp3` or `opus`).
    pub transcode_default_format: String,
    /// Whether ffmpeg is present (silent direct fallback when absent).
    pub ffmpeg_available: bool,
}

impl Default for JellyfinSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            server_name: "DroppedNeedle".to_owned(),
            server_version: "10.10.6".to_owned(),
            transcoding_enabled: true,
            transcode_max_bitrate_kbps: 320,
            transcode_default_format: "mp3".to_owned(),
            ffmpeg_available: true,
        }
    }
}

// ===== Auth seam =====

/// Authenticated caller: id, display name, role, and the presented token
/// (needed to embed `api_key` in `DirectStreamUrl`).
#[derive(Debug, Clone)]
pub struct Principal {
    pub id: String,
    pub name: String,
    pub role: String,
    pub token: String,
}

impl Principal {
    /// Build from `compat_auth`'s user (display-name `or`-chain parity via
    /// `effective_name`).
    pub fn from_user(user: &JellyfinUser, token: &str) -> Self {
        Self {
            id: user.id.clone(),
            name: effective_name(user).to_owned(),
            role: user.role.clone(),
            token: token.to_owned(),
        }
    }

    /// The full non-null user object strict clients hard-cast on (Finamp
    /// #144/#376, Manet `EnableAllFolders`).
    pub fn user_dto(&self, server_id: &str) -> UserDto {
        UserDto {
            id: self.id.clone(),
            name: self.name.clone(),
            server_id: server_id.to_owned(),
            has_password: true,
            has_configured_password: true,
            has_configured_easy_password: false,
            configuration: super::models::UserConfiguration::default(),
            policy: super::models::UserPolicy::permissive(self.role == "admin"),
        }
    }
}

// ===== Id map seam =====

/// Opaque Jellyfin id <-> (kind, internal id). Kinds: `artist`, `album`,
/// `track`, `playlist`, `genre`, `library` (v2 `VALID_KINDS`).
pub trait IdMap: Clone + Send + Sync + 'static {
    /// Stable 32-hex id for a (kind, internal) pair.
    fn to_jf(&self, kind: &str, internal: &str) -> impl Future<Output = String> + Send;
    /// Reverse lookup; accepts dashed or undashed input, `None` when unknown.
    /// (`from_*` takes `self` by v2-naming parity, not by convention.)
    #[allow(clippy::wrong_self_convention)]
    fn from_jf(&self, jf_id: &str) -> impl Future<Output = Option<(String, String)>> + Send;
}

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

// ===== Library seam =====

/// Ticks per second (v2 `JELLYFIN_TICKS_PER_SECOND`).
pub const TICKS_PER_SECOND: i64 = 10_000_000;

/// Track row the builders shape into an `Audio` DTO (v2 `ViewTrack` subset).
#[derive(Debug, Clone, Default)]
pub struct TrackView {
    pub file_id: String,
    pub title: String,
    pub duration_seconds: Option<f64>,
    pub year: Option<i32>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub album_title: Option<String>,
    pub rg_mbid: Option<String>,
    pub artist_name: Option<String>,
    pub artist_mbid: Option<String>,
    pub album_artist_name: Option<String>,
    pub album_artist_mbid: Option<String>,
    pub genre: Option<String>,
    pub file_format: Option<String>,
    /// Source bitrate in kbps (v2 `ViewTrack.bitrate`).
    pub bitrate: Option<u32>,
    pub channels: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub file_size_bytes: Option<u64>,
    /// Unix seconds added.
    pub created_at: Option<f64>,
    pub recording_mbid: Option<String>,
    /// Caller-scoped favorite flag, filled by the read methods.
    pub starred: bool,
    pub play_count: u64,
    /// Unix seconds of the last play, for the history sorts.
    pub last_played: Option<f64>,
}

/// Album row (v2 `ViewAlbum` subset).
#[derive(Debug, Clone, Default)]
pub struct AlbumView {
    pub rg_mbid: String,
    pub title: String,
    pub artist_name: Option<String>,
    pub artist_mbid: Option<String>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub track_count: usize,
    pub total_duration_seconds: Option<f64>,
    /// Unix seconds added.
    pub date_added: Option<f64>,
    pub starred: bool,
    pub play_count: u64,
    pub last_played: Option<f64>,
}

/// Artist row (v2 `ViewArtist` subset).
#[derive(Debug, Clone, Default)]
pub struct ArtistView {
    pub artist_mbid: String,
    pub name: String,
    pub album_count: usize,
    /// Unix seconds added.
    pub date_added: Option<f64>,
    pub starred: bool,
}

/// Genre row (v2 `ViewGenre` subset).
#[derive(Debug, Clone, Default)]
pub struct GenreView {
    pub name: String,
    pub song_count: usize,
}

/// Playlist browse row (v2 `ViewPlaylist` subset). Counts are streamable
/// entries only, matching the served `/Items` listing (v2 issue #181).
#[derive(Debug, Clone, Default)]
pub struct PlaylistView {
    pub id: String,
    pub name: String,
    pub track_count: usize,
    pub total_duration_seconds: Option<f64>,
}

/// One playlist entry: the per-entry remove/reorder handle plus the linked
/// library file (entries without one are never served).
#[derive(Debug, Clone)]
pub struct PlaylistEntry {
    pub id: String,
    pub file_id: Option<String>,
}

/// Playlist detail for the Items/add/remove/move routes.
#[derive(Debug, Clone, Default)]
pub struct PlaylistDetail {
    pub id: String,
    pub name: String,
    pub entries: Vec<PlaylistEntry>,
}

/// Served image bytes plus content type.
#[derive(Debug, Clone)]
pub struct CoverBytes {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

/// Artist listing scope: every artist vs album artists (v2
/// `/Artists` vs `/Artists/AlbumArtists`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistScope {
    All,
    Album,
}

/// Library reads behind the browse routes. List methods take the caller so
/// the adapter can fill caller-scoped `starred` flags (v2 `user=user`
/// parity); the router does filtering, sorting, and paging over the
/// returned snapshot.
pub trait LibraryRead: Clone + Send + Sync + 'static {
    fn tracks(&self, user_id: &str) -> impl Future<Output = Vec<TrackView>> + Send;
    fn track(&self, user_id: &str, file_id: &str)
    -> impl Future<Output = Option<TrackView>> + Send;
    fn albums(&self, user_id: &str) -> impl Future<Output = Vec<AlbumView>> + Send;
    fn album(&self, user_id: &str, rg_mbid: &str)
    -> impl Future<Output = Option<AlbumView>> + Send;
    fn artists(
        &self,
        user_id: &str,
        scope: ArtistScope,
    ) -> impl Future<Output = Vec<ArtistView>> + Send;
    fn artist(&self, user_id: &str, mbid: &str) -> impl Future<Output = Option<ArtistView>> + Send;
    fn genres(&self) -> impl Future<Output = Vec<GenreView>> + Send;
    fn playlists(&self, user_id: &str) -> impl Future<Output = Vec<PlaylistView>> + Send;
    fn playlist(
        &self,
        user_id: &str,
        id: &str,
    ) -> impl Future<Output = Option<PlaylistDetail>> + Send;
    fn create_playlist(&self, user_id: &str, name: &str) -> impl Future<Output = String> + Send;
    fn add_playlist_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        file_id: &str,
    ) -> impl Future<Output = ()> + Send;
    fn remove_playlist_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> impl Future<Output = ()> + Send;
    fn move_playlist_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_id: &str,
        index: usize,
    ) -> impl Future<Output = ()> + Send;
    /// Favorited internal ids of one kind for one caller.
    fn favorites(&self, user_id: &str, kind: &str) -> impl Future<Output = Vec<String>> + Send;
    fn set_favorite(
        &self,
        user_id: &str,
        kind: &str,
        internal: &str,
        add: bool,
    ) -> impl Future<Output = ()> + Send;
    /// Release art bytes (`size` is the 250/500/1200 bucket); `None` renders
    /// 404 with no placeholder, unlike Subsonic (v2 `_image`).
    fn cover(&self, rg_mbid: &str, size: &str) -> impl Future<Output = Option<CoverBytes>> + Send;
    fn artist_image(&self, mbid: &str) -> impl Future<Output = Option<CoverBytes>> + Send;
    /// Etags for `ImageTags.Primary` / `AlbumPrimaryImageTag`.
    fn cover_tag(&self, rg_mbid: &str) -> impl Future<Output = Option<String>> + Send;
    fn artist_tag(&self, mbid: &str) -> impl Future<Output = Option<String>> + Send;
}

// ===== In-memory library =====

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
pub struct MemoryLibrary {
    rows: std::sync::Arc<Mutex<MemoryRows>>,
}

impl MemoryLibrary {
    /// Empty library.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed one track.
    #[cfg(any(test, feature = "test-support"))]
    pub fn add_track(&self, track: TrackView) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.tracks.push(track);
        }
    }

    /// Seed one album.
    #[cfg(any(test, feature = "test-support"))]
    pub fn add_album(&self, album: AlbumView) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.albums.push(album);
        }
    }

    /// Seed one artist (`album_artist` lists it under `/Artists/AlbumArtists`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn add_artist(&self, artist: ArtistView, album_artist: bool) {
        if let Ok(mut rows) = self.rows.lock() {
            if album_artist {
                rows.album_artist_mbids.insert(artist.artist_mbid.clone());
            }
            rows.artists.push(artist);
        }
    }

    /// Seed one genre.
    #[cfg(any(test, feature = "test-support"))]
    pub fn add_genre(&self, genre: GenreView) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.genres.push(genre);
        }
    }

    /// Seed release art for one size bucket (`250`, `500`, `1200`).
    #[cfg(any(test, feature = "test-support"))]
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
    #[cfg(any(test, feature = "test-support"))]
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

impl LibraryRead for MemoryLibrary {
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

// ===== Streaming seam =====

/// Inputs to the direct-vs-transcode policy (v2 `decide()`).
#[derive(Debug, Clone)]
pub struct DecideInput<'a> {
    /// Lowercase source container (`track.file_format`).
    pub src_format: Option<&'a str>,
    /// Source bitrate in kbps (`track.bitrate or 0`).
    pub src_bitrate_kbps: u32,
    /// Client-requested output (`requested_format`), already codec-mapped.
    pub requested: Option<&'a str>,
    /// Client ceiling in kbps (`max_bitrate_kbps`).
    pub ceiling_kbps: Option<u32>,
    pub force_original: bool,
    pub start_seconds: f64,
    pub transcoding_enabled: bool,
    /// Server quality ceiling, default 320: a ceiling, never a trigger.
    pub server_max_kbps: u32,
    /// Default output when the request names none usable.
    pub default_format: &'a str,
    pub ffmpeg: bool,
}

/// The policy verdict (v2 `StreamPlan` subset).
#[derive(Debug, Clone, PartialEq)]
pub enum StreamPlan {
    Direct,
    Transcode {
        format: String,
        bitrate_kbps: u32,
        start_seconds: f64,
    },
}

/// Direct-vs-transcode policy, v2 `transcode_service.decide()` rules in
/// order: silent direct fallback first; a transcode needs an explicit client
/// request (codec mismatch or client ceiling below source); the server max
/// only caps quality once transcoding.
pub fn decide(input: &DecideInput) -> StreamPlan {
    const HUGE: u32 = 1_000_000_000;
    const MIN_BITRATE_KBPS: u32 = 64;
    if input.force_original || !input.transcoding_enabled || !input.ffmpeg {
        return StreamPlan::Direct;
    }
    let ceiling = match input.ceiling_kbps {
        Some(c) if c > 0 => c,
        _ => HUGE,
    };
    let src = input.src_format.unwrap_or("").to_lowercase();
    let req = input.requested.unwrap_or("").to_lowercase();
    let codec_mismatch = !req.is_empty() && req != "raw" && req != src;
    let over_ceiling = ceiling < input.src_bitrate_kbps;
    if !codec_mismatch && !over_ceiling {
        return StreamPlan::Direct;
    }
    let out = if req == "mp3" || req == "opus" {
        req
    } else {
        input.default_format.to_lowercase()
    };
    StreamPlan::Transcode {
        format: out,
        bitrate_kbps: ceiling.min(input.server_max_kbps).max(MIN_BITRATE_KBPS),
        start_seconds: input.start_seconds.max(0.0),
    }
}

/// Served audio bytes: status, headers, body.
#[derive(Debug, Clone)]
pub struct ByteOutcome {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Audio bytes behind the `/Audio` routes. Production binds the stream
/// engine (range-capable file streams, ffmpeg pipe, concurrency leases with
/// 429 + `Retry-After: 1`); the router only maps outcomes to responses.
pub trait StreamEngine: Clone + Send + Sync + 'static {
    /// The engine scoped to one authenticated caller, so stream leases
    /// count against that user. Engines without leases return themselves.
    fn for_caller(&self, _user_id: &str) -> Self {
        self.clone()
    }

    /// Direct bytes with range support (200/206/416 contract).
    fn direct(
        &self,
        file_id: &str,
        range: Option<&str>,
    ) -> impl Future<Output = ByteOutcome> + Send;
    /// HEAD outcome for a file: the same status and headers a `direct`
    /// GET would answer (200/206/416, unknown ids 404), always with an
    /// empty body. The router maps it straight onto the response.
    fn head(&self, file_id: &str, range: Option<&str>) -> impl Future<Output = ByteOutcome> + Send;
    /// Transcoded bytes (estimate off on Jellyfin: never a Content-Length).
    /// The real adapter must serve an unsized streaming
    /// body (ffmpeg pipe). Axum auto-adds `Content-Length` to sized bodies,
    /// so a sized adapter response would violate the v2 contract on the wire;
    /// only an unknown size hint keeps the header off.
    fn transcode(
        &self,
        file_id: &str,
        format: &str,
        bitrate_kbps: u32,
        start_seconds: f64,
    ) -> impl Future<Output = ByteOutcome> + Send;
}

/// Content-Type per extension, v2's list verbatim.
pub fn content_type_for_format(format: Option<&str>) -> &'static str {
    match format.unwrap_or("").to_lowercase().as_str() {
        "flac" => "audio/flac",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "wma" => "audio/x-ms-wma",
        "opus" => "audio/opus",
        _ => "application/octet-stream",
    }
}

/// In-memory [`StreamEngine`]: seeded bytes served under the v2 range
/// contract (single-range `bytes=` only; open and suffix ranges 206;
/// multi-range, malformed, empty, or unsatisfiable → 416 with
/// `Content-Range: bytes */N` and no body).
/// Seeded file bytes plus lowercase container per file id.
#[cfg(any(test, feature = "test-support"))]
type SeededFiles = std::sync::Arc<Mutex<HashMap<String, (Vec<u8>, Option<String>)>>>;

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Default)]
pub struct MemoryEngine {
    files: SeededFiles,
}

#[cfg(any(test, feature = "test-support"))]
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
#[cfg(any(test, feature = "test-support"))]
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

#[cfg(any(test, feature = "test-support"))]
fn unsatisfied(size: usize) -> ByteOutcome {
    ByteOutcome {
        status: 416,
        headers: vec![("Content-Range".to_owned(), format!("bytes */{size}"))],
        body: Vec::new(),
    }
}

#[cfg(any(test, feature = "test-support"))]
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

// ===== Playback sessions seam =====

/// Presence + scrobble calls behind the `/Sessions/Playing*` routes.
/// Production binds the compat scrobble adapter.
pub trait PlaybackSessions: Clone + Send + Sync + 'static {
    fn mark_started(&self, user_id: &str, key: &str) -> impl Future<Output = ()> + Send;
    fn pop_started(&self, user_id: &str, key: &str) -> impl Future<Output = Option<String>> + Send;
    fn now_playing(
        &self,
        user_id: &str,
        file_id: &str,
        client: Option<&str>,
    ) -> impl Future<Output = ()> + Send;
    fn progress(
        &self,
        user_id: &str,
        file_id: &str,
        position_ms: Option<i64>,
        paused: bool,
    ) -> impl Future<Output = ()> + Send;
    fn clear_presence(
        &self,
        user_id: &str,
        client: Option<&str>,
    ) -> impl Future<Output = ()> + Send;
    fn scrobble(
        &self,
        user_id: &str,
        file_id: &str,
        client: Option<&str>,
    ) -> impl Future<Output = ()> + Send;
}

/// Whether a stop report counts as a play (v2 `_should_scrobble`): an
/// omitted position counts; otherwise past 90% or within a second of the
/// end. `Failed` stops never reach this check.
pub fn should_scrobble(position_ticks: Option<i64>, runtime_ticks: Option<i64>) -> bool {
    let Some(position) = position_ticks else {
        return true;
    };
    if let Some(runtime) = runtime_ticks
        && runtime > 0
    {
        if position as f64 / runtime as f64 * 100.0 > 90.0 {
            return true;
        }
        if position >= runtime - TICKS_PER_SECOND {
            return true;
        }
    }
    false
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
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Default)]
pub struct MemorySessions {
    started: std::sync::Arc<Mutex<HashSet<(String, String)>>>,
    calls: std::sync::Arc<Mutex<Vec<SessionCall>>>,
}

#[cfg(any(test, feature = "test-support"))]
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

#[cfg(any(test, feature = "test-support"))]
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
