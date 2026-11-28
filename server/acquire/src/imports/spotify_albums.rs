//! Which MusicBrainz release group a Spotify album is.
//!
//! An album is matched only on proof, never on its name alone: a release
//! counts when it carries a recording with an ISRC Spotify gave for a
//! track of that album, under one of the album's title forms. The
//! releases come first from a recording search by those ISRCs, which
//! finds every edition whatever a title search would rank first, then,
//! when MusicBrainz knows none of them, from a release search by title
//! and the album's own artist, read with `inc=recordings+isrcs`. A
//! multi-track release (two or more tracks) MusicBrainz lists no ISRCs
//! for at all can't be proven that way, so it counts by its listing
//! instead: the album's title, an artist of it, Spotify's track count
//! and known album type, and every song of the album on the playlist on
//! it. One-track singles still require ISRC proof, a release listing any
//! ISRC must prove itself by one, and an ISRC proof wins over a listing.
//! Two proven groups (an album and a same-named single carrying the same
//! recording) are told apart by Spotify's album type, then its track
//! count. No proven release, or a tie left after that, leaves the album
//! unresolved, so an import never points a request at the wrong music.
//! Lookups run at background priority, paced by the executor, and each
//! album's answer is kept in the metadata cache.
//!
//! A playlist file's album ([`FileAlbum`]) is proven the same way by an
//! ISRC the file gives, else by its listing, read from one recording
//! search a song ([`AlbumResolver::resolve_file_album`]).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use droppedneedle_kernel::db::fold_text;
use droppedneedle_kernel::runtime_config::{Live, sections::MusicBrainzSettings};
use droppedneedle_library::identify::artist_links::placeholder;
use droppedneedle_providers::cache::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};
use droppedneedle_providers::musicbrainz::models::{
    ArtistCreditName, MbRelease, RecordingSearchHit, ReleaseGroupRef, ReleaseSearchHit, Track,
};
use droppedneedle_providers::musicbrainz::{
    Criticality, MusicBrainzClient, MusicBrainzView, SOURCE, escape_lucene_phrase, is_valid_mbid,
    normalize_mb_id,
};
use droppedneedle_providers::{ProviderError, Providers, RequestPriority};

/// Releases read from one search. Editions of a proven group cost no
/// lookup, so this bounds the lookups to the distinct candidates.
const SEARCH_LIMIT: u32 = 5;

/// Recordings read from one ISRC search, where every hit carries its
/// releases inline with no extra lookups.
const RECORDING_SEARCH_LIMIT: u32 = 25;

/// An album's ISRCs asked by recording search, in order, until one
/// proves a group: one is enough when MusicBrainz knows the album.
const ISRC_SEARCHES: usize = 2;

/// The includes a candidate is proven with.
const PROOF_INCLUDES: [&str; 3] = ["isrcs", "recordings", "release-groups"];

/// Songs of one file album the listing proof searches for, in file order.
const FILE_SONGS: usize = 5;

/// How long a playlist file album's answer is kept, found or not: a week.
/// A miss outlasts the identify job's three days of retries, so a retry
/// asks MusicBrainz only about albums it has not answered yet.
const FILE_ALBUM_POLICY: Policy = Policy {
    fresh: Duration::from_secs(7 * 24 * 3600),
    miss: Duration::from_secs(7 * 24 * 3600),
};

/// How long the recordings an ISRC names are kept: a month when MusicBrainz
/// knows the ISRC, a day when it does not.
const ISRC_POLICY: Policy = Policy::RECORDING_TO_GROUP;

/// How long a Spotify album's proof is kept: a week when MusicBrainz places
/// it, a day when it does not, so an album it cannot place is asked about
/// once a day.
const RESOLVE_POLICY: Policy = Policy {
    fresh: Duration::from_secs(7 * 24 * 3600),
    miss: Duration::from_secs(24 * 3600),
};

/// One song a playlist file filed under an album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSong {
    /// The title as the file gives it.
    pub title: String,
    /// The ISRC the file gives, uppercased and well formed.
    pub isrc: Option<String>,
}

/// An album a playlist file names for songs the library lacks: its title
/// and artist as the file gives them, and its songs in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAlbum {
    /// Album title.
    pub title: String,
    /// The artist text of its first song, every credit included.
    pub artist: String,
    /// Its songs, in file order.
    pub songs: Vec<FileSong>,
}

/// The release group a file album was proven to be, and for each song
/// whose title only one recording on that group carries, that recording,
/// keyed by the song's title as the file gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundAlbum {
    /// The release group, lowercased.
    pub group: String,
    /// Song title key ([`title_key`]) to recording id, lowercased.
    pub recordings: BTreeMap<String, String>,
}

/// One Spotify album as the import knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyAlbum {
    /// Album title.
    pub title: String,
    /// The album's first artist, else the first credited artist of a
    /// track on it.
    pub artist: String,
    /// Spotify's album type: `album`, `single` or `compilation`.
    pub kind: Option<String>,
    /// Tracks on the album, as Spotify counts them.
    pub total_tracks: Option<u32>,
    /// The titles of its tracks on the playlist.
    pub songs: BTreeSet<String>,
    /// The ISRCs of its tracks on the playlist, uppercased and well formed.
    pub isrcs: BTreeSet<String>,
}

impl SpotifyAlbum {
    /// The album with `isrc` added when it is a well-formed ISRC.
    pub fn add_isrc(&mut self, isrc: &str) {
        let isrc = isrc.trim().to_ascii_uppercase();
        if is_isrc(&isrc) {
            self.isrcs.insert(isrc);
        }
    }
}

/// The one cached read of the recordings an ISRC names, shared by the
/// album proof and the compilation rule: one MusicBrainz call per ISRC a
/// month at most.
#[derive(Clone)]
struct IsrcReads {
    view: MusicBrainzView,
    cache: ProviderCacheStore,
    namespace: String,
    priority: RequestPriority,
}

/// Compact release proof kept in `ProviderCacheStore` for `by_search` so
/// repeated searches across playlists do not re-fetch the same MusicBrainz
/// release document.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProofTrack {
    title: Option<String>,
    isrcs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProofRelease {
    title: Option<String>,
    group_id: String,
    primary_type: Option<String>,
    secondary_types: Vec<String>,
    tracks: Vec<ProofTrack>,
}

impl ProofRelease {
    fn from_wire(release: MbRelease) -> Option<Self> {
        let group = release.release_group?;
        let group_id = normalize_mb_id(&group.id);
        if !is_valid_mbid(&group_id) {
            return None;
        }
        let tracks = release
            .media
            .iter()
            .flat_map(|medium| &medium.tracks)
            .map(|track: &Track| ProofTrack {
                title: track.display_title().map(str::to_owned),
                isrcs: track
                    .recording
                    .as_ref()
                    .map(|recording| {
                        recording
                            .isrcs
                            .iter()
                            .map(|isrc| isrc.trim().to_ascii_uppercase())
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();
        Some(Self {
            title: release.title,
            group_id,
            primary_type: group.primary_type,
            secondary_types: group.secondary_types,
            tracks,
        })
    }
}

impl IsrcReads {
    /// The recordings a search for `isrc` finds, up to
    /// [`RECORDING_SEARCH_LIMIT`].
    async fn hits(&self, isrc: &str) -> Result<Vec<RecordingSearchHit>, ProviderError> {
        let key = CacheKey::hashed(
            Source::MusicBrainz,
            "isrc_recordings",
            &[&self.namespace, isrc],
        );
        let (view, query) = (self.view.clone(), format!("isrc:{isrc}"));
        let found = self
            .cache
            .get_or_fetch(&key, ISRC_POLICY, self.priority, move |_| async move {
                let page = view
                    .search_recordings_query(
                        &query,
                        RECORDING_SEARCH_LIMIT,
                        Criticality::IdentityCritical,
                    )
                    .await?;
                Ok(if page.items.is_empty() {
                    Fetched::Missing
                } else {
                    Fetched::Found(page.items)
                })
            })
            .await?;
        Ok(found.unwrap_or_default())
    }

    /// One release's proof fields, cached in `ProviderCacheStore` under the
    /// MusicBrainz source namespace.
    async fn proof_release(&self, release_id: &str) -> Result<Option<ProofRelease>, ProviderError> {
        let id = normalize_mb_id(release_id);
        let key = CacheKey::new(
            Source::MusicBrainz,
            &[&self.namespace, "spotify_proof_release", &id],
        );
        let view = self.view.clone();
        self.cache
            .get_or_fetch(
                &key,
                Policy::MUSICBRAINZ_ENTITY,
                self.priority,
                move |_| async move {
                    let found = view
                        .lookup_release(&id, &PROOF_INCLUDES, Criticality::IdentityCritical)
                        .await?;
                    Ok(
                        match found.and_then(|r| ProofRelease::from_wire(r.entity)) {
                            Some(proof) => Fetched::Found(proof),
                            None => Fetched::Missing,
                        },
                    )
                },
            )
            .await
    }
}

/// Finds the release group of Spotify albums through MusicBrainz.
#[derive(Clone)]
pub struct AlbumResolver {
    musicbrainz: MusicBrainzClient,
    settings: Live<MusicBrainzSettings>,
    cache: ProviderCacheStore,
}

impl AlbumResolver {
    /// The resolver over the shared providers, reading the MusicBrainz
    /// source from `settings` at each album.
    pub fn new(providers: &Providers, settings: Live<MusicBrainzSettings>) -> Self {
        Self {
            musicbrainz: providers.musicbrainz.clone(),
            settings,
            cache: providers.cache.clone(),
        }
    }

    /// The album's release group id, `None` when nothing proves one, with
    /// MusicBrainz asked at `priority`.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure, so the caller can stop asking while the
    /// service is down. Nothing is cached for it.
    pub async fn resolve(
        &self,
        album: &SpotifyAlbum,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        self.resolve_for(None, album, priority).await
    }

    /// Resolve a Spotify album, keying first by Spotify's stable album ID
    /// (`spotify_id`) when present so the same album appearing across multiple
    /// playlists with different track subsets reuses the proven release group
    /// with 0 MusicBrainz calls.
    pub async fn resolve_for(
        &self,
        spotify_id: Option<&str>,
        album: &SpotifyAlbum,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let can_list = !album.songs.is_empty() && album.total_tracks.is_some_and(|n| n >= 2);
        if album.title.trim().is_empty() || (album.isrcs.is_empty() && !can_list) {
            return Ok(None);
        }
        let reads = self.isrc_reads(priority)?;
        let namespace = reads.namespace.clone();
        let clean_spotify_id = spotify_id.map(str::trim).filter(|id| !id.is_empty());
        if let Some(id) = clean_spotify_id {
            let id_key = CacheKey::new(Source::MusicBrainz, &[&namespace, "spotify_album_id", id]);
            if let Some(Some(group)) = self.cache.peek::<String>(&id_key).await {
                return Ok(Some(group));
            }
        }
        let isrcs: Vec<&str> = album.isrcs.iter().map(String::as_str).collect();
        let joined = isrcs.join(",");
        let tracks = album
            .total_tracks
            .map(|n| n.to_string())
            .unwrap_or_default();
        let songs: Vec<&str> = album.songs.iter().map(String::as_str).collect();
        // ASCII unit separator survives CacheKey::hashed's whitespace collapse,
        // keeping {"A B", "C"} distinct from {"A", "B C"}.
        let songs = songs.join("\x1f");
        let key = CacheKey::hashed(
            Source::MusicBrainz,
            "spotify_album",
            &[
                &namespace,
                &album.title,
                &album.artist,
                album.kind.as_deref().unwrap_or_default(),
                &tracks,
                &joined,
                &songs,
            ],
        );
        let album = album.clone();
        let resolved = self
            .cache
            .get_or_fetch(&key, RESOLVE_POLICY, priority, move |_| async move {
                Ok(match prove(&reads, &album).await? {
                    Some(group) => Fetched::Found(group),
                    None => Fetched::Missing,
                })
            })
            .await?;
        if let (Some(id), Some(group)) = (clean_spotify_id, resolved.as_ref()) {
            let id_key = CacheKey::new(Source::MusicBrainz, &[&namespace, "spotify_album_id", id]);
            if let Ok(bytes) = serde_json::to_vec(group) {
                self.cache.set_bytes(&id_key, bytes, RESOLVE_POLICY.fresh);
            }
        }
        Ok(resolved)
    }

    /// Whether a previously resolved release group is cached as a plain studio
    /// Album, EP, or Single with no secondary types (so the compilation rule
    /// cannot move tracks off it). Never dials out.
    pub async fn is_known_studio_group(&self, group_mbid: &str) -> bool {
        let Some(settings) = self.settings.read() else {
            return false;
        };
        let group = normalize_mb_id(group_mbid);
        if !is_valid_mbid(&group) {
            return false;
        }
        let namespace = format!("{}:g{}", settings.source_id, settings.generation);
        let key = CacheKey::new(Source::MusicBrainz, &[&namespace, "rg_studio", &group]);
        self.cache.peek::<bool>(&key).await == Some(Some(true))
    }

    /// The cached ISRC reads over the MusicBrainz source the settings name
    /// now, at `priority`.
    fn isrc_reads(&self, priority: RequestPriority) -> Result<IsrcReads, ProviderError> {
        let Some(settings) = self.settings.read() else {
            return Err(ProviderError::NotConfigured {
                provider: SOURCE,
                message: "the musicbrainz settings cannot be read".to_owned(),
            });
        };
        Ok(IsrcReads {
            namespace: format!("{}:g{}", settings.source_id, settings.generation),
            view: self
                .musicbrainz
                .fixed(self.musicbrainz.source_from(&settings), priority),
            cache: self.cache.clone(),
            priority,
        })
    }

    /// The artist's own album or single that a song Spotify files on
    /// `album` is on, by its ISRC ([`pick_own_group`]); none when the rule
    /// does not move it.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure, or settings that cannot be read.
    pub async fn own_group(
        &self,
        album: &SpotifyAlbum,
        isrc: &str,
    ) -> Result<Option<OwnGroup>, ProviderError> {
        let Some(isrc) = clean_isrc(isrc) else {
            return Ok(None);
        };
        let hits = self
            .isrc_reads(RequestPriority::BackgroundSync)?
            .hits(&isrc)
            .await?;
        Ok(pick_own_group(album, &hits))
    }

    /// The one recording an ISRC names on release group `group`, when
    /// exactly one is there, with MusicBrainz asked at `priority`.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure, or settings that cannot be read.
    pub async fn recording_on_group(
        &self,
        isrc: &str,
        group: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let Some(isrc) = clean_isrc(isrc) else {
            return Ok(None);
        };
        let hits = self.isrc_reads(priority)?.hits(&isrc).await?;
        let on_group: BTreeSet<String> = hits
            .iter()
            .filter(|hit| {
                hit.releases.iter().any(|release| {
                    release
                        .release_group
                        .as_ref()
                        .is_some_and(|found| normalize_mb_id(&found.id) == group)
                })
            })
            .map(|hit| normalize_mb_id(&hit.id))
            .filter(|id| is_valid_mbid(id))
            .collect();
        let mut on_group = on_group.into_iter();
        Ok(match (on_group.next(), on_group.next()) {
            (Some(recording), None) => Some(recording),
            _ => None,
        })
    }

    /// The release group a playlist file's album is, by proof only: an
    /// ISRC the file gives, else the album's listing. `None` when nothing
    /// proves exactly one group.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure, or settings that cannot be read
    /// ([`ProviderError::NotConfigured`]). Nothing is cached for it.
    pub async fn resolve_file_album(
        &self,
        album: &FileAlbum,
    ) -> Result<Option<FoundAlbum>, ProviderError> {
        if non_empty_forms(&album.title).is_empty()
            || title_key(&album.artist).is_empty()
            || album.songs.is_empty()
        {
            return Ok(None);
        }
        let reads = self.isrc_reads(RequestPriority::BackgroundSync)?;
        let namespace = reads.namespace.clone();
        // The unit separator survives CacheKey::hashed's whitespace
        // collapse, as in `resolve`.
        let songs: Vec<&str> = album.songs.iter().map(|song| song.title.as_str()).collect();
        let isrcs: Vec<&str> = album
            .songs
            .iter()
            .map(|song| song.isrc.as_deref().unwrap_or_default())
            .collect();
        let key = CacheKey::hashed(
            Source::MusicBrainz,
            "playlist_file_album",
            &[
                &namespace,
                &album.title,
                &album.artist,
                &songs.join("\x1f"),
                &isrcs.join("\x1f"),
            ],
        );
        let album = album.clone();
        self.cache
            .get_or_fetch(
                &key,
                FILE_ALBUM_POLICY,
                RequestPriority::BackgroundSync,
                move |_| async move {
                    Ok(match prove_file(&reads, &album).await? {
                        Some(found) => Fetched::Found(found),
                        None => Fetched::Missing,
                    })
                },
            )
            .await
    }
}

/// A file album's group: by its ISRCs when the file gives any; by its
/// listing when it gives none, or when its ISRCs prove no group. ISRCs
/// that prove two groups leave it unknown.
async fn prove_file(
    reads: &IsrcReads,
    album: &FileAlbum,
) -> Result<Option<FoundAlbum>, ProviderError> {
    let view = &reads.view;
    let isrcs: BTreeSet<String> = album
        .songs
        .iter()
        .filter_map(|song| song.isrc.clone())
        .filter(|isrc| is_isrc(isrc))
        .collect();
    if !isrcs.is_empty() {
        let as_listed = SpotifyAlbum {
            title: album.title.clone(),
            artist: album.artist.clone(),
            kind: None,
            total_tracks: None,
            songs: BTreeSet::new(),
            isrcs,
        };
        let (proven, hits) = isrc_search(reads, &as_listed).await?;
        if proven.is_empty() {
            return by_listing(view, album).await;
        }
        let Some(group) = pick(&as_listed, proven) else {
            return Ok(None);
        };
        let songs: Vec<(&FileSong, Vec<&RecordingSearchHit>)> = album
            .songs
            .iter()
            .map(|song| {
                let carrying = hits
                    .iter()
                    .filter(|(isrc, _)| song.isrc.as_deref() == Some(isrc.as_str()))
                    .map(|(_, hit)| hit)
                    .collect();
                (song, carrying)
            })
            .collect();
        return Ok(Some(found_on(&group, &songs)));
    }
    by_listing(view, album).await
}

/// The group of the one release every searched song is on by its listing:
/// for each song, a recording with its title, credited to the album's
/// artist by exact name, on an official release of two or more tracks
/// under one of the album's title forms. A recording MusicBrainz lists
/// under ISRCs none of which is the song's file ISRC never counts for it.
/// A search answering more than it shows proves nothing, and the songs
/// stop once no group is left.
async fn by_listing(
    view: &MusicBrainzView,
    album: &FileAlbum,
) -> Result<Option<FoundAlbum>, ProviderError> {
    let album_forms = non_empty_forms(&album.title);
    let first_credit = album
        .artist
        .split([',', ';'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    let artists: BTreeSet<String> = [title_key(&album.artist), title_key(&first_credit)]
        .into_iter()
        .filter(|key| !key.is_empty())
        .collect();
    let release = title_forms(&album.title).pop().unwrap_or_default();
    let mut seen = BTreeSet::new();
    let songs: Vec<&FileSong> = album
        .songs
        .iter()
        .filter(|song| {
            let key = title_key(&song.title);
            !key.is_empty() && seen.insert(key)
        })
        .take(FILE_SONGS)
        .collect();
    let mut left: Option<BTreeSet<String>> = None;
    let mut pages = Vec::with_capacity(songs.len());
    for song in songs {
        let query = format!(
            r#"recording:"{}" AND artist:"{}" AND release:"{}""#,
            escape_lucene_phrase(song.title.trim()),
            escape_lucene_phrase(&first_credit),
            escape_lucene_phrase(&release),
        );
        let page = view
            .search_recordings_query(
                &query,
                RECORDING_SEARCH_LIMIT,
                Criticality::IdentityCritical,
            )
            .await?;
        if page.count > page.items.len() as u64 {
            return Ok(None);
        }
        let song_forms = non_empty_forms(&song.title);
        let other_isrcs = |hit: &RecordingSearchHit| {
            song.isrc.as_deref().is_some_and(|isrc| {
                !hit.isrcs.is_empty()
                    && !hit
                        .isrcs
                        .iter()
                        .any(|listed| listed.trim().eq_ignore_ascii_case(isrc))
            })
        };
        let hits: Vec<RecordingSearchHit> = page
            .items
            .into_iter()
            .filter(|hit| {
                hit.video != Some(true)
                    && !other_isrcs(hit)
                    && hit
                        .title
                        .as_deref()
                        .is_some_and(|title| !non_empty_forms(title).is_disjoint(&song_forms))
                    && hit.artist_credit.iter().any(|credit| {
                        artists.contains(&title_key(&credit.name))
                            || artists.contains(&title_key(&credit.artist.name))
                    })
            })
            .collect();
        let groups: BTreeSet<String> = hits
            .iter()
            .flat_map(|hit| listed_groups(hit, &album_forms))
            .collect();
        let narrowed: BTreeSet<String> = match left {
            Some(left) => left.intersection(&groups).cloned().collect(),
            None => groups,
        };
        if narrowed.is_empty() {
            return Ok(None);
        }
        left = Some(narrowed);
        pages.push((song, hits));
    }
    let Some(left) = left else {
        return Ok(None);
    };
    let mut groups = left.into_iter();
    let (Some(group), None) = (groups.next(), groups.next()) else {
        tracing::debug!("a playlist file album matches more than one release group");
        return Ok(None);
    };
    let songs: Vec<(&FileSong, Vec<&RecordingSearchHit>)> = pages
        .iter()
        .map(|(song, hits)| (*song, hits.iter().collect()))
        .collect();
    Ok(Some(found_on(&group, &songs)))
}

/// The groups a hit's releases put it on by their listing: official, two
/// or more tracks, under one of the album's title forms.
fn listed_groups(hit: &RecordingSearchHit, album_forms: &BTreeSet<String>) -> Vec<String> {
    hit.releases
        .iter()
        .filter(|release| {
            release
                .status
                .as_deref()
                .is_some_and(|status| status.eq_ignore_ascii_case("official"))
                && release.track_count.is_some_and(|tracks| tracks >= 2)
                && release
                    .title
                    .as_deref()
                    .is_some_and(|title| !non_empty_forms(title).is_disjoint(album_forms))
        })
        .filter_map(|release| release.release_group.as_ref())
        .map(|group| normalize_mb_id(&group.id))
        .filter(|id| is_valid_mbid(id))
        .collect()
}

/// The found album: `group`, and each song's recording when exactly one
/// of its hits is on that group.
fn found_on(group: &str, songs: &[(&FileSong, Vec<&RecordingSearchHit>)]) -> FoundAlbum {
    let mut recordings = BTreeMap::new();
    for (song, hits) in songs {
        let song_forms = non_empty_forms(&song.title);
        let on_group: BTreeSet<String> = hits
            .iter()
            .filter(|hit| {
                hit.title
                    .as_deref()
                    .is_some_and(|title| !non_empty_forms(title).is_disjoint(&song_forms))
                    && hit.releases.iter().any(|release| {
                        release
                            .release_group
                            .as_ref()
                            .is_some_and(|ref_group| normalize_mb_id(&ref_group.id) == group)
                    })
            })
            .map(|hit| normalize_mb_id(&hit.id))
            .filter(|id| is_valid_mbid(id))
            .collect();
        let mut ids = on_group.into_iter();
        if let (Some(recording), None) = (ids.next(), ids.next()) {
            recordings.insert(title_key(&song.title), recording);
        }
    }
    FoundAlbum {
        group: group.to_owned(),
        recordings,
    }
}

/// A release group a release proved, with what a tie is broken by.
struct Proven {
    primary_type: Option<String>,
    secondary_types: Vec<String>,
    /// The track counts of its proving releases.
    track_counts: BTreeSet<u32>,
}

impl Proven {
    fn new(group: &ReleaseGroupRef) -> Self {
        Self {
            primary_type: group.primary_type.clone(),
            secondary_types: group.secondary_types.clone(),
            track_counts: BTreeSet::new(),
        }
    }

    /// Whether the group is the known kind of release Spotify says the album
    /// is. Returns `None` when `kind` is absent or not one of Spotify's
    /// `album`, `single` or `compilation` types.
    fn fits_known(&self, kind: Option<&str>) -> Option<bool> {
        let has_secondary = |want: &str| {
            self.secondary_types
                .iter()
                .any(|secondary| secondary.eq_ignore_ascii_case(want))
        };
        let compilation = has_secondary("Compilation");
        let soundtrack = has_secondary("Soundtrack");
        match kind.map(str::to_ascii_lowercase).as_deref() {
            Some("compilation") => Some(compilation || soundtrack),
            Some("single") => Some(matches!(
                self.primary_type.as_deref(),
                Some("Single" | "EP")
            )),
            Some("album") => Some(self.primary_type.as_deref() == Some("Album") && !compilation),
            _ => None,
        }
    }

    /// Whether the group is the kind of release Spotify says the album is;
    /// any group fits an album Spotify gave no type.
    fn fits(&self, kind: Option<&str>) -> bool {
        self.fits_known(kind).unwrap_or(true)
    }

    /// Whether the group is a plain Album, EP, or Single with no secondary
    /// types (so it is not a compilation, soundtrack, remix, or DJ-mix).
    fn is_studio_own_album(&self) -> bool {
        self.secondary_types.is_empty()
            && matches!(
                self.primary_type.as_deref(),
                Some("Album" | "EP" | "Single")
            )
    }
}

/// The proven group, by the album's ISRCs first and its title and
/// artist second. Also caches whether the proven group is a plain studio
/// album/EP/single so `own_album` can skip redundant per-track ISRC lookups.
async fn prove(reads: &IsrcReads, album: &SpotifyAlbum) -> Result<Option<String>, ProviderError> {
    let mut proven = by_isrc(reads, album).await?;
    if proven.is_empty() {
        proven = by_search_cached(reads, album).await?;
    }
    let picked = pick_with_studio(album, proven);
    if let Some((ref group, is_studio)) = picked {
        let key = CacheKey::new(Source::MusicBrainz, &[&reads.namespace, "rg_studio", group]);
        if let Ok(bytes) = serde_json::to_vec(&is_studio) {
            reads.cache.set_bytes(&key, bytes, RESOLVE_POLICY.fresh);
        }
    }
    Ok(picked.map(|(group, _)| group))
}

/// The groups of the releases a recording search by the album's ISRCs
/// finds under one of its title forms.
async fn by_isrc(
    reads: &IsrcReads,
    album: &SpotifyAlbum,
) -> Result<BTreeMap<String, Proven>, ProviderError> {
    Ok(isrc_search(reads, album).await?.0)
}

/// [`by_isrc`]'s groups, with every hit its searches read and the ISRC
/// each was searched by. Each ISRC's hits come through the one cached read
/// ([`IsrcReads::hits`]).
async fn isrc_search(
    reads: &IsrcReads,
    album: &SpotifyAlbum,
) -> Result<(BTreeMap<String, Proven>, Vec<(String, RecordingSearchHit)>), ProviderError> {
    let forms = non_empty_forms(&album.title);
    let mut hits = Vec::new();
    if forms.is_empty() {
        return Ok((BTreeMap::new(), hits));
    }
    let mut proven: BTreeMap<String, Proven> = BTreeMap::new();
    for isrc in album
        .isrcs
        .iter()
        .filter(|isrc| is_isrc(isrc))
        .take(ISRC_SEARCHES)
    {
        let items = reads.hits(isrc).await?;
        for release in items.iter().flat_map(|hit| &hit.releases) {
            let (Some(title), Some(group)) = (&release.title, &release.release_group) else {
                continue;
            };
            let id = normalize_mb_id(&group.id);
            if is_valid_mbid(&id) && !non_empty_forms(title).is_disjoint(&forms) {
                proven
                    .entry(id)
                    .or_insert_with(|| Proven::new(group))
                    .track_counts
                    .extend(release.track_count);
            }
        }
        hits.extend(items.into_iter().map(|hit| (isrc.clone(), hit)));
        if !proven.is_empty() {
            break;
        }
    }
    Ok((proven, hits))
}

/// The groups proven by a release search for each title form, until one
/// form proves any by ISRC; if none does, the first form's listing
/// matches are returned. Release lookups are cached in `ProviderCacheStore`.
async fn by_search_cached(
    reads: &IsrcReads,
    album: &SpotifyAlbum,
) -> Result<BTreeMap<String, Proven>, ProviderError> {
    let mut fallback: Option<BTreeMap<String, Proven>> = None;
    for title in title_forms(&album.title) {
        let page = reads
            .view
            .search_releases(
                &title,
                &album.artist,
                SEARCH_LIMIT,
                Criticality::IdentityCritical,
            )
            .await?;
        let mut proven: BTreeMap<String, Proven> = BTreeMap::new();
        let mut by_listing: BTreeMap<String, Proven> = BTreeMap::new();
        for hit in &page.items {
            let listed = hit
                .release_group
                .as_ref()
                .map(|group| normalize_mb_id(&group.id));
            if listed.as_ref().is_some_and(|group| {
                proven.contains_key(group)
                    || (album.isrcs.is_empty() && by_listing.contains_key(group))
            }) {
                continue;
            }
            let Some(release) = reads.proof_release(&hit.id).await? else {
                continue;
            };
            let id = release.group_id.clone();
            let carries = release
                .tracks
                .iter()
                .flat_map(|track| &track.isrcs)
                .any(|isrc| album.isrcs.contains(isrc));
            let candidate = Proven {
                primary_type: release.primary_type.clone(),
                secondary_types: release.secondary_types.clone(),
                track_counts: BTreeSet::new(),
            };
            let found = if carries {
                &mut proven
            } else if lists_the_album(album, hit, &release, &candidate) {
                &mut by_listing
            } else {
                continue;
            };
            found
                .entry(id)
                .or_insert(candidate)
                .track_counts
                .extend(u32::try_from(release.tracks.len()).ok());
        }
        if !proven.is_empty() {
            return Ok(proven);
        }
        if !by_listing.is_empty() {
            if album.isrcs.is_empty() {
                return Ok(by_listing);
            }
            if fallback.is_none() {
                fallback = Some(by_listing);
            }
        }
    }
    Ok(fallback.unwrap_or_default())
}

#[cfg(test)]
async fn by_search(
    view: &MusicBrainzView,
    album: &SpotifyAlbum,
) -> Result<BTreeMap<String, Proven>, ProviderError> {
    let reads = IsrcReads {
        view: view.clone(),
        cache: ProviderCacheStore::new(
            droppedneedle_kernel::db::cache::CacheDb::scratch(),
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
        namespace: "test".to_owned(),
        priority: RequestPriority::BackgroundSync,
    };
    by_search_cached(&reads, album).await
}

/// The one proven group, after a tie is narrowed by the album's type and
/// then its track count; a rule that would rule out every group is skipped.
fn pick(album: &SpotifyAlbum, proven: BTreeMap<String, Proven>) -> Option<String> {
    pick_with_studio(album, proven).map(|(group, _)| group)
}

fn pick_with_studio(
    album: &SpotifyAlbum,
    proven: BTreeMap<String, Proven>,
) -> Option<(String, bool)> {
    let mut left: Vec<(&String, &Proven)> = proven.iter().collect();
    let rules: [&dyn Fn(&Proven) -> bool; 2] =
        [&|group| group.fits(album.kind.as_deref()), &|group| {
            album
                .total_tracks
                .is_none_or(|tracks| group.track_counts.contains(&tracks))
        }];
    for rule in rules {
        if left.len() <= 1 {
            break;
        }
        let kept: Vec<_> = left
            .iter()
            .copied()
            .filter(|(_, group)| rule(group))
            .collect();
        if !kept.is_empty() {
            left = kept;
        }
    }
    match left.as_slice() {
        [(group, info)] => Some(((*group).clone(), info.is_studio_own_album())),
        [] => None,
        _ => {
            tracing::debug!(
                groups = left.len(),
                "spotify album matches more than one release group; left unresolved"
            );
            None
        }
    }
}

/// Whether a multi-track release MusicBrainz lists no ISRCs for is the
/// album by its listing: its title is one of the album's title forms, the
/// album's artist is among its credited artists, it has at least two
/// tracks and as many as Spotify counts, its group is the known kind
/// Spotify gives, and every song of the album on the playlist is on it.
fn lists_the_album(
    album: &SpotifyAlbum,
    hit: &ReleaseSearchHit,
    release: &ProofRelease,
    group: &Proven,
) -> bool {
    let artist = title_key(&album.artist);
    let album_forms = non_empty_forms(&album.title);
    if artist.is_empty() || album_forms.is_empty() || album.songs.is_empty() {
        return false;
    }
    let on_release: BTreeSet<String> = release
        .tracks
        .iter()
        .filter_map(|track| track.title.as_deref())
        .flat_map(non_empty_forms)
        .collect();
    let release_title = release
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .or(hit.title.as_deref());
    album
        .total_tracks
        .is_some_and(|n| n >= 2 && Some(n) == u32::try_from(release.tracks.len()).ok())
        && release.tracks.iter().all(|track| track.isrcs.is_empty())
        && release_title.is_some_and(|title| !non_empty_forms(title).is_disjoint(&album_forms))
        && hit.artist_credit.iter().any(|credit| {
            title_key(&credit.name) == artist || title_key(&credit.artist.name) == artist
        })
        && group.fits_known(album.kind.as_deref()) == Some(true)
        && album.songs.iter().all(|song| {
            let song_forms = non_empty_forms(song);
            !song_forms.is_empty() && !song_forms.is_disjoint(&on_release)
        })
}

/// The artist's own album or single a compilation's song is on, by its
/// ISRC: the release group, the title of its earliest release among the
/// hits, and the recording on it when only one hit recording is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnGroup {
    /// The release group, lowercased.
    pub group: String,
    /// The title of its earliest release among the hits.
    pub title: String,
    /// The recording on it, when exactly one of the hits is.
    pub recording: Option<String>,
}

/// How an own album ranks: an album before an EP before a single.
fn own_kind_rank(group: &ReleaseGroupRef) -> Option<u8> {
    if !group.secondary_types.is_empty() {
        return None;
    }
    match group.primary_type.as_deref() {
        Some("Album") => Some(0),
        Some("EP") => Some(1),
        Some("Single") => Some(2),
        _ => None,
    }
}

/// Whether a group carries the secondary type `want`.
fn has_secondary(group: &ReleaseGroupRef, want: &str) -> bool {
    group
        .secondary_types
        .iter()
        .any(|secondary| secondary.eq_ignore_ascii_case(want))
}

/// The first credited artist with a real MusicBrainz id, lowercased; a
/// special-purpose artist (Various Artists, unknown) counts as none.
fn first_real_artist(credit: &[ArtistCreditName]) -> Option<String> {
    let id = credit
        .iter()
        .map(|name| normalize_mb_id(&name.artist.id))
        .find(|id| is_valid_mbid(id))?;
    (!placeholder(&id)).then_some(id)
}

/// Whether Spotify itself files `album` as a compilation: its type, or a
/// "Various Artists" album artist.
#[must_use]
pub fn filed_as_compilation(album: &SpotifyAlbum) -> bool {
    album
        .kind
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("compilation"))
        || title_key(&album.artist) == "various artists"
}

/// The compilation rule, on the recordings an ISRC search found. A song
/// Spotify files on `album` as a compilation (its type, a "Various
/// Artists" album artist, or every group the hits list under the album's
/// title typed Compilation or Soundtrack) is on the artist's own album
/// when the hits put the same recording on an official release in a group
/// typed Album, EP or Single with no secondary type, credited first to the
/// recordings' first artist, and not one of the groups Spotify's filing
/// is. Album beats EP beats Single, then the earliest release; two groups
/// on the earliest date, a group of the best kind with no dated release,
/// artists that disagree or no artist leave the song as Spotify files it.
/// Names never decide it: the ISRC is the proof of the same recording.
pub fn pick_own_group(album: &SpotifyAlbum, hits: &[RecordingSearchHit]) -> Option<OwnGroup> {
    let forms = non_empty_forms(&album.title);
    let filing: BTreeMap<String, &ReleaseGroupRef> = hits
        .iter()
        .flat_map(|hit| &hit.releases)
        .filter(|release| {
            release
                .title
                .as_deref()
                .is_some_and(|title| !non_empty_forms(title).is_disjoint(&forms))
        })
        .filter_map(|release| release.release_group.as_ref())
        .map(|group| (normalize_mb_id(&group.id), group))
        .collect();
    let compilation = filed_as_compilation(album)
        || (!filing.is_empty()
            && filing.values().all(|group| {
                has_secondary(group, "Compilation") || has_secondary(group, "Soundtrack")
            }));
    if !compilation {
        return None;
    }
    let artists: BTreeSet<Option<String>> = hits
        .iter()
        .map(|hit| first_real_artist(&hit.artist_credit))
        .collect();
    let artist = match artists.into_iter().collect::<Vec<_>>().as_slice() {
        [Some(artist)] => artist.clone(),
        _ => return None,
    };
    // Group, rank, its earliest dated release's date and title, whether
    // any of its releases is undated, and the recordings on it.
    struct Candidate {
        rank: u8,
        earliest: Option<(String, String)>,
        recordings: BTreeSet<String>,
    }
    let mut candidates: BTreeMap<String, Candidate> = BTreeMap::new();
    for hit in hits {
        for release in &hit.releases {
            let Some(group) = release.release_group.as_ref() else {
                continue;
            };
            let id = normalize_mb_id(&group.id);
            let Some(rank) = own_kind_rank(group) else {
                continue;
            };
            let official = release
                .status
                .as_deref()
                .is_some_and(|status| status.eq_ignore_ascii_case("Official"));
            if !official
                || !is_valid_mbid(&id)
                || filing.contains_key(&id)
                || first_real_artist(&release.artist_credit).as_deref() != Some(artist.as_str())
            {
                continue;
            }
            let candidate = candidates.entry(id).or_insert(Candidate {
                rank,
                earliest: None,
                recordings: BTreeSet::new(),
            });
            candidate.recordings.insert(normalize_mb_id(&hit.id));
            let dated = release
                .date
                .as_deref()
                .map(str::trim)
                .filter(|date| !date.is_empty());
            if let Some(date) = dated {
                let title = release.title.clone().unwrap_or_default();
                if candidate
                    .earliest
                    .as_ref()
                    .is_none_or(|(earliest, _)| date < earliest.as_str())
                {
                    candidate.earliest = Some((date.to_owned(), title));
                }
            }
        }
    }
    let best = candidates.values().map(|candidate| candidate.rank).min()?;
    let mut kind: Vec<(&String, &Candidate)> = candidates
        .iter()
        .filter(|(_, candidate)| candidate.rank == best)
        .collect();
    if kind
        .iter()
        .any(|(_, candidate)| candidate.earliest.is_none())
    {
        return None;
    }
    kind.sort_by(|a, b| a.1.earliest.cmp(&b.1.earliest));
    let (group, chosen) = kind.first()?;
    let date = chosen
        .earliest
        .as_ref()
        .map_or("", |(date, _)| date.as_str());
    // A year and a full date in that year may be the same day: a tie.
    if kind.get(1).is_some_and(|(_, next)| {
        next.earliest
            .as_ref()
            .is_some_and(|(at, _)| at.starts_with(date))
    }) {
        return None;
    }
    let mut recordings = chosen.recordings.iter();
    let recording = match (recordings.next(), recordings.next()) {
        (Some(recording), None) => Some(recording.clone()),
        _ => None,
    };
    Some(OwnGroup {
        group: (*group).clone(),
        title: chosen
            .earliest
            .as_ref()
            .map(|(_, title)| title.clone())
            .unwrap_or_default(),
        recording,
    })
}

/// A title as the proof compares it, and the key a found album's
/// recordings are kept under: folded, with punctuation as spaces,
/// so "Awesome Mix, Vol. 1" reads as "Awesome Mix Vol. 1". Titles made only
/// of symbols (`+`, `÷`, `( )`) keep their folded symbols instead of
/// collapsing to the empty string.
pub(crate) fn title_key(title: &str) -> String {
    let folded = fold_text(title);
    let words = folded
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if words.is_empty() {
        folded.trim().to_owned()
    } else {
        words
    }
}

/// Whether two titles share a title form, as the proofs compare them.
pub(crate) fn same_title(left: &str, right: &str) -> bool {
    !non_empty_forms(left).is_disjoint(&non_empty_forms(right))
}

/// Non-empty title keys for `title` and its stripped edition form, so an
/// empty or whitespace-only string never matches anything.
fn non_empty_forms(title: &str) -> BTreeSet<String> {
    title_forms(title)
        .iter()
        .map(|form| title_key(form))
        .filter(|key| !key.is_empty())
        .collect()
}

/// The title as Spotify gives it, then without a trailing edition note
/// (`Abbey Road (Remastered)`, `Glass Tides - Single`), which MusicBrainz
/// keeps out of the title.
fn title_forms(title: &str) -> Vec<String> {
    let title = title.trim();
    let mut forms = vec![title.to_owned()];
    let base = if let Some(stripped) = title
        .strip_suffix(')')
        .and_then(|rest| rest.rfind('(').map(|at| &rest[..at]))
    {
        stripped
    } else if let Some(stripped) = title
        .strip_suffix(']')
        .and_then(|rest| rest.rfind('[').map(|at| &rest[..at]))
    {
        stripped
    } else if let Some(at) = title.rfind(" - ") {
        &title[..at]
    } else {
        title
    };
    let base = base.trim();
    if !base.is_empty() && base != title {
        forms.push(base.to_owned());
    }
    forms
}

/// `raw` uppercased when it is a well-formed ISRC.
pub(crate) fn clean_isrc(raw: &str) -> Option<String> {
    let isrc = raw.trim().to_ascii_uppercase();
    is_isrc(&isrc).then_some(isrc)
}

/// `CC XXX YY NNNNN`: two letters, three letters or digits, then seven
/// digits.
fn is_isrc(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 12
        && bytes[..2].iter().all(u8::is_ascii_uppercase)
        && bytes[2..5]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && bytes[5..].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use droppedneedle_kernel::runtime_config::ConfigStore;
    use droppedneedle_kernel::runtime_config::crypto::Crypto;
    use droppedneedle_kernel::runtime_config::sections::MbSourceMode;
    use droppedneedle_kernel::test_support::ScratchDir;
    use droppedneedle_providers::test_support::ScriptedWire;

    use super::*;

    const MB_ROOT: &str = "http://127.0.0.1:9/musicbrainz/ws/2";
    const GROUP_A: &str = "5907a1b0-0000-4000-8000-000000000101";
    const GROUP_B: &str = "5907a1b0-0000-4000-8000-000000000102";
    const REL_1: &str = "5907a1b0-0000-4000-8000-000000000301";
    const REL_2: &str = "5907a1b0-0000-4000-8000-000000000302";
    const REL_3: &str = "5907a1b0-0000-4000-8000-000000000303";
    const JSON_HEADER: &[(&str, &str)] = &[("content-type", "application/json")];

    fn scripted_view(wire: Arc<ScriptedWire>) -> MusicBrainzView {
        let providers = Providers::unpaced(wire);
        let settings = MusicBrainzSettings {
            source_mode: MbSourceMode::Official,
            selected_source_mode: MbSourceMode::Official,
            ..MusicBrainzSettings::default()
        };
        providers.musicbrainz.fixed(
            providers.musicbrainz.source_from(&settings),
            RequestPriority::BackgroundSync,
        )
    }

    /// The cached ISRC reads over `view`, on a cache of their own.
    fn reads(view: &MusicBrainzView) -> IsrcReads {
        IsrcReads {
            view: view.clone(),
            cache: Providers::unpaced(Arc::new(ScriptedWire::new())).cache,
            namespace: "test".to_owned(),
            priority: RequestPriority::BackgroundSync,
        }
    }

    fn group_ref(primary_type: Option<&str>, secondary_types: &[&str]) -> ReleaseGroupRef {
        ReleaseGroupRef {
            id: GROUP_A.to_owned(),
            title: Some("Sample".to_owned()),
            first_release_date: None,
            primary_type: primary_type.map(str::to_owned),
            primary_type_id: None,
            secondary_types: secondary_types.iter().map(|s| (*s).to_owned()).collect(),
            artist_credit: Vec::new(),
            disambiguation: None,
            genres: Vec::new(),
        }
    }

    fn search_hit(rel_id: &str, group_id: &str, title: &str, artist: &str, kind: &str) -> String {
        format!(
            r#"{{"id":"{rel_id}","title":"{title}","artist-credit":[{{"name":"{artist}","artist":{{"id":"5907a1b0-0000-4000-8000-000000000401","name":"{artist}"}}}}],"release-group":{{"id":"{group_id}","title":"{title}","primary-type":"{kind}","secondary-types":[]}}}}"#
        )
    }

    fn release_doc(
        rel_id: &str,
        group_id: &str,
        title: &str,
        kind: &str,
        tracks: &[(&str, Option<&str>, &[&str])],
    ) -> String {
        let track_json: Vec<String> = tracks
            .iter()
            .enumerate()
            .map(|(index, (rec_title, track_title, isrcs))| {
                let isrcs_json: Vec<String> = isrcs.iter().map(|i| format!(r#""{i}""#)).collect();
                let title_field = match track_title {
                    Some(t) => format!(r#""title":"{t}","#),
                    None => String::new(),
                };
                format!(
                    r#"{{"id":"5907a1b0-0000-4000-8000-00000000050{index}",{title_field}"recording":{{"id":"5907a1b0-0000-4000-8000-00000000060{index}","title":"{rec_title}","isrcs":[{}]}}}}"#,
                    isrcs_json.join(",")
                )
            })
            .collect();
        format!(
            r#"{{"id":"{rel_id}","title":"{title}","release-group":{{"id":"{group_id}","title":"{title}","primary-type":"{kind}","secondary-types":[]}},"media":[{{"position":1,"tracks":[{}]}}]}}"#,
            track_json.join(",")
        )
    }

    #[tokio::test]
    async fn title_key_keeps_symbols_distinct_and_blanks_never_match() {
        assert_eq!(title_key("+"), "+");
        assert_eq!(title_key("÷"), "÷");
        assert_ne!(title_key("+"), title_key("÷"));
        assert_eq!(title_key("( )"), "( )");
        assert_eq!(title_key("   "), "");

        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/recording"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"recordings":[{{"id":"5907a1b0-0000-4000-8000-000000000201","title":"Song","releases":[
                    {{"id":"{REL_1}","title":"÷","track-count":12,"release-group":{{"id":"{GROUP_A}","title":"÷","primary-type":"Album","secondary-types":[]}}}},
                    {{"id":"{REL_2}","title":"","track-count":12,"release-group":{{"id":"{GROUP_A}","title":"","primary-type":"Album","secondary-types":[]}}}}
                ]}}]}}"#
            ),
        );
        let view = scripted_view(wire);

        let mut plus = SpotifyAlbum {
            title: "+".to_owned(),
            artist: "Ed Sheeran".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(12),
            songs: BTreeSet::new(),
            isrcs: BTreeSet::new(),
        };
        plus.add_isrc("QZAAA2600001");
        assert!(
            by_isrc(&reads(&view), &plus)
                .await
                .expect("mb answers")
                .is_empty(),
            "+ must not match ÷"
        );

        let mut blank = plus.clone();
        blank.title = "   ".to_owned();
        assert!(
            by_isrc(&reads(&view), &blank)
                .await
                .expect("mb answers")
                .is_empty(),
            "blank title must never match"
        );
    }

    #[tokio::test]
    async fn by_isrc_strips_musicbrainz_edition_suffix_and_validates_isrcs() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/recording"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"recordings":[{{"id":"5907a1b0-0000-4000-8000-000000000201","title":"Signal","releases":[
                    {{"id":"{REL_1}","title":"Night Signals (Original Soundtrack)","track-count":12,"release-group":{{"id":"{GROUP_A}","title":"Night Signals","primary-type":"Album","secondary-types":["Soundtrack"]}}}}
                ]}}]}}"#
            ),
        );
        let view = scripted_view(wire.clone());

        let mut album = SpotifyAlbum {
            title: "Night Signals".to_owned(),
            artist: "Various Artists".to_owned(),
            kind: Some("compilation".to_owned()),
            total_tracks: Some(12),
            songs: BTreeSet::new(),
            isrcs: BTreeSet::new(),
        };
        // Invalid value inserted directly into the pub field must be skipped.
        album.isrcs.insert("BAD OR artist:x".to_owned());
        album.add_isrc("QZAAA2600001");

        let proven = by_isrc(&reads(&view), &album).await.expect("mb answers");
        assert!(
            proven.contains_key(GROUP_A),
            "MB release with trailing edition note should match on ISRC lookup"
        );

        let sent = wire.sent();
        assert_eq!(sent.len(), 1, "invalid ISRC skipped, valid ISRC hits once");
        assert_eq!(sent[0].query("query").as_deref(), Some("isrc:QZAAA2600001"));
        assert_eq!(sent[0].query("limit").as_deref(), Some("25"));
    }

    #[test]
    fn proven_fits_handles_case_and_soundtracks() {
        let soundtrack = Proven::new(&group_ref(Some("Album"), &["Soundtrack"]));
        assert!(soundtrack.fits(Some("compilation")));
        assert!(soundtrack.fits(Some("COMPILATION")));
        assert!(soundtrack.fits(Some("album")));
        assert!(soundtrack.fits(Some("ALBUM")));
        assert!(!soundtrack.fits(Some("single")));
        assert!(!soundtrack.fits(Some("SINGLE")));

        let compilation = Proven::new(&group_ref(Some("Album"), &["Compilation"]));
        assert!(compilation.fits(Some("Compilation")));
        assert!(!compilation.fits(Some("album")));
        assert!(!compilation.fits(Some("ALBUM")));

        let single = Proven::new(&group_ref(Some("Single"), &[]));
        assert!(single.fits(Some("SINGLE")));
        assert!(!single.fits(Some("ALBUM")));
    }

    #[tokio::test]
    async fn listing_rejects_one_track_single_without_isrcs() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Solo Drop", "Aurora Current", "Single")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Solo Drop",
                "Single",
                &[("Solo Drop", Some("Solo Drop"), &[])],
            ),
        );
        let view = scripted_view(wire);

        let album = SpotifyAlbum {
            title: "Solo Drop".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("single".to_owned()),
            total_tracks: Some(1),
            songs: BTreeSet::from(["Solo Drop".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            prove(&reads(&view), &album).await.expect("mb answers"),
            None,
            "1-track single with zero ISRCs must stay unresolved"
        );
    }

    #[tokio::test]
    async fn listing_requires_known_album_kind() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Quiet Harbor", "Aurora Current", "Album")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Quiet Harbor",
                "Album",
                &[
                    ("Low Tide", Some("Low Tide"), &[]),
                    ("Undertow", Some("Undertow"), &[]),
                ],
            ),
        );
        let view = scripted_view(wire);

        for kind in [None, Some("unknown".to_owned())] {
            let album = SpotifyAlbum {
                title: "Quiet Harbor".to_owned(),
                artist: "Aurora Current".to_owned(),
                kind: kind.clone(),
                total_tracks: Some(2),
                songs: BTreeSet::from(["Low Tide".to_owned()]),
                isrcs: BTreeSet::new(),
            };
            assert_eq!(
                prove(&reads(&view), &album).await.expect("mb answers"),
                None,
                "kind {kind:?} must not match by listing"
            );
        }
    }

    #[tokio::test]
    async fn listing_rejects_blank_and_symbol_mismatches() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{},{}]}}"#,
                search_hit(REL_1, GROUP_A, "Quiet Harbor", "", "Album"),
                search_hit(REL_2, GROUP_B, "÷", "÷", "Album")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Quiet Harbor",
                "Album",
                &[("", Some(""), &[]), ("Track Two", Some("Track Two"), &[])],
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_2}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_2,
                GROUP_B,
                "÷",
                "Album",
                &[("÷", Some("÷"), &[]), ("Two", Some("Two"), &[])],
            ),
        );
        let view = scripted_view(wire);

        let blank_artist = SpotifyAlbum {
            title: "Quiet Harbor".to_owned(),
            artist: "   ".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["Track Two".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            prove(&reads(&view), &blank_artist)
                .await
                .expect("mb answers"),
            None,
            "blank artist must not match empty MB artist credit"
        );

        let blank_song = SpotifyAlbum {
            title: "Quiet Harbor".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["   ".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        let wire_song = Arc::new(ScriptedWire::new());
        wire_song.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Quiet Harbor", "Aurora Current", "Album")
            ),
        );
        wire_song.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Quiet Harbor",
                "Album",
                &[("", Some(""), &[]), ("Track Two", Some("Track Two"), &[])],
            ),
        );
        assert_eq!(
            prove(&reads(&scripted_view(wire_song)), &blank_song)
                .await
                .expect("mb answers"),
            None,
            "blank song must not match empty MB track title"
        );

        let symbol_mismatch = SpotifyAlbum {
            title: "+".to_owned(),
            artist: "+".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["+".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            prove(&reads(&view), &symbol_mismatch)
                .await
                .expect("mb answers"),
            None,
            "punctuation-only + must not match ÷ in listing"
        );
    }

    #[tokio::test]
    async fn by_search_does_not_let_listing_suppress_isrc_proof() {
        // Part A: Hit 1 of Group A matches by listing, Hit 2 of Group A carries
        // the ISRC, Hit 3 of Group B (a Single) also carries the ISRC.
        // Group A must not be skipped on Hit 2.
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{},{},{}]}}"#,
                search_hit(REL_1, GROUP_A, "Glass Bloom", "Aurora Current", "Album"),
                search_hit(REL_2, GROUP_A, "Glass Bloom", "Aurora Current", "Album"),
                search_hit(REL_3, GROUP_B, "Glass Bloom", "Aurora Current", "Single")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Glass Bloom",
                "Album",
                &[
                    ("Track 1", Some("Track 1"), &[]),
                    ("Track 2", Some("Track 2"), &[]),
                ],
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_2}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_2,
                GROUP_A,
                "Glass Bloom",
                "Album",
                &[
                    ("Track 1", Some("Track 1"), &["QZAAA2600099"]),
                    ("Track 2", Some("Track 2"), &[]),
                ],
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_3}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_3,
                GROUP_B,
                "Glass Bloom",
                "Single",
                &[("Track 1", Some("Track 1"), &["QZAAA2600099"])],
            ),
        );
        let view = scripted_view(wire);

        let mut album = SpotifyAlbum {
            title: "Glass Bloom".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["Track 1".to_owned(), "Track 2".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        album.add_isrc("QZAAA2600099");

        let proven = by_search(&view, &album).await.expect("mb answers");
        assert!(
            proven.contains_key(GROUP_A),
            "Group A must not be suppressed by an earlier listing match"
        );
        assert_eq!(pick(&album, proven).as_deref(), Some(GROUP_A));

        // Part B: Form 1 ("Glass Bloom (Deluxe)") matches Group B by listing,
        // while Form 2 ("Glass Bloom") proves Group A by ISRC.
        let wire_forms = Arc::new(ScriptedWire::new());
        wire_forms.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(
                    REL_1,
                    GROUP_B,
                    "Glass Bloom (Deluxe)",
                    "Aurora Current",
                    "Album"
                )
            ),
        );
        wire_forms.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_2, GROUP_A, "Glass Bloom", "Aurora Current", "Album")
            ),
        );
        wire_forms.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_B,
                "Glass Bloom (Deluxe)",
                "Album",
                &[
                    ("Track 1", Some("Track 1"), &[]),
                    ("Track 2", Some("Track 2"), &[]),
                ],
            ),
        );
        wire_forms.answer(
            &format!("{MB_ROOT}/release/{REL_2}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_2,
                GROUP_A,
                "Glass Bloom",
                "Album",
                &[
                    ("Track 1", Some("Track 1"), &["QZAAA2600099"]),
                    ("Track 2", Some("Track 2"), &[]),
                ],
            ),
        );

        let mut deluxe = album.clone();
        deluxe.title = "Glass Bloom (Deluxe)".to_owned();
        let proven_forms = by_search(&scripted_view(wire_forms), &deluxe)
            .await
            .expect("mb answers");
        assert_eq!(
            pick(&deluxe, proven_forms).as_deref(),
            Some(GROUP_A),
            "ISRC proof on a later title form must win over a listing match on the first"
        );
    }

    #[tokio::test]
    async fn listing_uses_canonical_release_and_track_display_titles() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Search Alias", "Aurora Current", "Album")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Canonical Album",
                "Album",
                &[("First Song", None, &[]), ("Second Song", None, &[])],
            ),
        );
        let view = scripted_view(wire);

        let album = SpotifyAlbum {
            title: "Canonical Album".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["First Song".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            prove(&reads(&view), &album)
                .await
                .expect("mb answers")
                .as_deref(),
            Some(GROUP_A),
            "listing should use release.title and track.display_title()"
        );
    }

    #[tokio::test]
    async fn cache_key_distinguishes_song_boundaries() {
        let dir = ScratchDir::new("spotify-albums-cache").expect("scratch dir");
        let crypto = Crypto::from_key_bytes(&[7; 32]).expect("crypto");
        let store = ConfigStore::scratch(dir.path(), crypto).expect("config store");
        store
            .save(MusicBrainzSettings {
                source_mode: MbSourceMode::Official,
                selected_source_mode: MbSourceMode::Official,
                ..MusicBrainzSettings::default()
            })
            .await
            .expect("mb settings save");

        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Boundary Album", "Aurora Current", "Album")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Boundary Album",
                "Album",
                &[("A B", Some("A B"), &[]), ("C", Some("C"), &[])],
            ),
        );
        let providers = Providers::unpaced(wire);
        let resolver = AlbumResolver::new(&providers, store.live());

        let first = SpotifyAlbum {
            title: "Boundary Album".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["A B".to_owned(), "C".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            resolver
                .resolve(&first, RequestPriority::BackgroundSync)
                .await
                .expect("mb answers")
                .as_deref(),
            Some(GROUP_A)
        );

        let second = SpotifyAlbum {
            songs: BTreeSet::from(["A".to_owned(), "B C".to_owned()]),
            ..first
        };
        assert_eq!(
            resolver
                .resolve(&second, RequestPriority::BackgroundSync)
                .await
                .expect("mb answers"),
            None,
            "songs {{\"A B\", \"C\"}} and {{\"A\", \"B C\"}} must not share a cache key"
        );
    }

    #[tokio::test]
    async fn resolve_for_shares_spotify_album_id_and_caches_studio_flag() {
        use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto, sections::MbSourceMode};
        use droppedneedle_kernel::test_support::ScratchDir;

        let dir = ScratchDir::new("spotify-albums-id-cache").expect("scratch dir");
        let crypto = Crypto::from_key_bytes(&[9; 32]).expect("crypto");
        let store = ConfigStore::scratch(dir.path(), crypto).expect("config store");
        store
            .save(MusicBrainzSettings {
                source_mode: MbSourceMode::Official,
                selected_source_mode: MbSourceMode::Official,
                ..MusicBrainzSettings::default()
            })
            .await
            .expect("mb settings save");

        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Shared Album", "Aurora Current", "Album")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Shared Album",
                "Album",
                &[
                    ("Track One", Some("Track One"), &[]),
                    ("Track Two", Some("Track Two"), &[]),
                ],
            ),
        );
        let providers = Providers::unpaced(wire.clone());
        let resolver = AlbumResolver::new(&providers, store.live());

        let playlist_one = SpotifyAlbum {
            title: "Shared Album".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["Track One".to_owned(), "Track Two".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            resolver
                .resolve_for(
                    Some("sp-album-1"),
                    &playlist_one,
                    RequestPriority::BackgroundSync
                )
                .await
                .expect("mb answers")
                .as_deref(),
            Some(GROUP_A)
        );
        let calls_after_first = wire.sent().len();
        assert_eq!(calls_after_first, 2, "search + proof release lookup");
        assert!(
            resolver.is_known_studio_group(GROUP_A).await,
            "studio album must be cached so own_album skips per-track ISRC lookups"
        );

        let playlist_two = SpotifyAlbum {
            songs: BTreeSet::from(["Track One".to_owned()]),
            ..playlist_one
        };
        assert_eq!(
            resolver
                .resolve_for(
                    Some("sp-album-1"),
                    &playlist_two,
                    RequestPriority::BackgroundSync
                )
                .await
                .expect("mb answers")
                .as_deref(),
            Some(GROUP_A)
        );
        assert_eq!(
            wire.sent().len(),
            calls_after_first,
            "same Spotify album ID on a second playlist must hit cache with 0 MB calls"
        );
    }
}
