//! Discovery sections on artist and album pages: similar artists, top
//! songs, top albums, similar albums, and more by the same artist.
//!
//! ListenBrainz answers by default (or whichever source the primary music
//! source setting names); when it has nothing, Last.fm fills in with the
//! user's Last.fm key or the instance key, the way v2 kept these sections from going blank while
//! ListenBrainz popularity was switched off upstream.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::providers::musicbrainz::Criticality;
use crate::providers::{IntegrationStatus, RequestPriority, lastfm, listenbrainz, record_current};
use crate::runtime_config::sections::MusicSource;

use super::artist::ReleaseGroupList;
use super::artist::checked_mbid;
use super::error::CatalogError;
use super::mapping;
use super::models::{
    DiscoveryAlbum, DiscoverySource, MoreByArtistResponse, SimilarAlbumsResponse, SimilarArtist,
    SimilarArtistsResponse, TopAlbum, TopAlbumsResponse, TopSong, TopSongsResponse,
};
use super::{Catalog, MISS_TTL, is_mbid, mb_error, mb_retry, record_mb_down, secs};

/// Empty answers are kept ten minutes (v2 `DISCOVERY_EMPTY_CACHE_TTL`).
const EMPTY_TTL: Duration = Duration::from_secs(600);
/// Partial answers (a deadline hit) are kept a minute.
const PARTIAL_TTL: Duration = Duration::from_secs(60);
/// Releases whose tracklists top songs look up, at most.
const MAX_TOP_SONG_RELEASES: usize = 10;
/// Release-to-group answers are kept a week: they never change.
const RELEASE_GROUP_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// How long one request spends resolving Last.fm releases.
const LASTFM_RESOLVE_BUDGET: Duration = Duration::from_secs(4);
/// Similar artists consulted for similar albums (v2 uses five).
const SIMILAR_ALBUM_ARTISTS: usize = 5;
/// Albums taken per similar artist.
const ALBUMS_PER_SIMILAR_ARTIST: usize = 3;

/// Which section is being built.
#[derive(Debug, Clone, Copy)]
enum Section {
    Similar,
    TopSongs,
    TopAlbums,
}

impl Section {
    fn key_prefix(self) -> &'static str {
        match self {
            Self::Similar => "lb_similar_artists:",
            Self::TopSongs => "artist_discovery:top_songs:",
            Self::TopAlbums => "artist_discovery:top_albums:",
        }
    }
}

/// One section's raw rows before library flags, as cached.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SectionRows {
    similar: Vec<SimilarArtist>,
    songs: Vec<TopSong>,
    albums: Vec<TopAlbum>,
}

impl SectionRows {
    fn is_empty(&self) -> bool {
        self.similar.is_empty() && self.songs.is_empty() && self.albums.is_empty()
    }
}

/// A section answer: rows, who answered, and whether that source is set up.
struct Answer {
    rows: SectionRows,
    source: DiscoverySource,
    configured: bool,
}

impl Catalog {
    fn discovery_ttl(&self, in_library: bool, empty: bool) -> Duration {
        if empty {
            return EMPTY_TTL;
        }
        let advanced = self.upstream().settings().advanced();
        secs(if in_library {
            advanced.cache_ttl_artist_discovery_library
        } else {
            advanced.cache_ttl_artist_discovery_non_library
        })
    }

    fn preferred_source(&self, asked: Option<DiscoverySource>) -> DiscoverySource {
        asked.unwrap_or_else(|| match self.upstream().settings().primary_source() {
            MusicSource::Listenbrainz => DiscoverySource::Listenbrainz,
            MusicSource::Lastfm => DiscoverySource::Lastfm,
        })
    }

    /// Build one artist section from the preferred source, falling back
    /// from ListenBrainz to Last.fm when ListenBrainz has nothing.
    async fn artist_section(
        &self,
        user_id: &str,
        raw_mbid: &str,
        count: u32,
        asked: Option<DiscoverySource>,
        section: Section,
    ) -> Result<Answer, CatalogError> {
        let mbid = checked_mbid(raw_mbid, "artist")?;
        let source = self.preferred_source(asked);
        let first = self
            .section_from(source, user_id, &mbid, count, section)
            .await;
        if source == DiscoverySource::Listenbrainz && first.rows.is_empty() {
            let fallback = self
                .section_from(DiscoverySource::Lastfm, user_id, &mbid, count, section)
                .await;
            if fallback.configured && !fallback.rows.is_empty() {
                return Ok(fallback);
            }
        }
        Ok(first)
    }

    async fn section_from(
        &self,
        source: DiscoverySource,
        user_id: &str,
        mbid: &str,
        count: u32,
        section: Section,
    ) -> Answer {
        let key = format!(
            "{}{}:{}:{}",
            section.key_prefix(),
            match source {
                DiscoverySource::Listenbrainz => "lb",
                DiscoverySource::Lastfm => "lfm",
            },
            mbid,
            count
        );
        let unconfigured = Answer {
            rows: SectionRows::default(),
            source,
            configured: false,
        };
        let rows = match source {
            DiscoverySource::Listenbrainz => {
                let Some((client, creds)) = self.upstream().listenbrainz() else {
                    return unconfigured;
                };
                let catalog = self.clone();
                let mbid = mbid.to_owned();
                self.cached(&self.inner.flights.other, key, move || async move {
                    let rows = catalog
                        .listenbrainz_rows(&client, &creds, &mbid, count, section)
                        .await;
                    catalog.cacheable(rows, &mbid).await
                })
                .await
            }
            DiscoverySource::Lastfm => {
                let Some((client, creds)) = self.upstream().lastfm(user_id).await else {
                    return unconfigured;
                };
                let catalog = self.clone();
                let mbid = mbid.to_owned();
                self.cached(&self.inner.flights.other, key, move || async move {
                    // Top albums match Last.fm titles against the artist's
                    // (cached) discography to find release groups.
                    let groups = match section {
                        Section::TopAlbums => catalog.release_groups(&mbid).await.ok(),
                        _ => None,
                    };
                    let rows =
                        lastfm_rows(&client, &creds, &mbid, count, section, groups.as_ref()).await;
                    catalog.cacheable(rows, &mbid).await
                })
                .await
            }
        };
        let rows = rows
            .ok()
            .and_then(|value| serde_json::from_value::<SectionRows>(value).ok())
            .unwrap_or_default();
        Answer {
            rows,
            source,
            configured: true,
        }
    }

    /// Wrap a section answer for the cache: failures are never cached,
    /// empty answers briefly, real answers for the discovery lifetime.
    async fn cacheable(
        &self,
        rows: Option<SectionRows>,
        mbid: &str,
    ) -> Result<(serde_json::Value, Option<Duration>), CatalogError> {
        let Some(rows) = rows else {
            return Ok((
                serde_json::to_value(SectionRows::default()).unwrap_or_default(),
                None,
            ));
        };
        let in_library = !self.artist_flags(&[mbid.to_owned()]).await.is_empty();
        let ttl = self.discovery_ttl(in_library, rows.is_empty());
        let value = serde_json::to_value(&rows)
            .map_err(|error| CatalogError::Internal(format!("discovery encode: {error}")))?;
        Ok((value, Some(ttl)))
    }

    /// ListenBrainz rows for one section; `None` when ListenBrainz failed.
    async fn listenbrainz_rows(
        &self,
        client: &super::upstream::CatalogListenBrainz,
        creds: &listenbrainz::ListenBrainzCredentials,
        mbid: &str,
        count: u32,
        section: Section,
    ) -> Option<SectionRows> {
        let count = count as usize;
        match section {
            Section::Similar => {
                let similar = client
                    .similar_artists(mbid, count, creds)
                    .await
                    .into_option()?;
                Some(SectionRows {
                    similar: similar
                        .into_iter()
                        .filter(|artist| is_mbid(&artist.artist_mbid))
                        .take(count)
                        .map(|artist| SimilarArtist {
                            name: artist.artist_name,
                            musicbrainz_id: artist.artist_mbid,
                            listen_count: artist.listen_count,
                            in_library: false,
                        })
                        .collect(),
                    ..SectionRows::default()
                })
            }
            Section::TopSongs => {
                let recordings = client
                    .artist_top_recordings(mbid, count, creds)
                    .await
                    .into_option()?;
                let ids: Vec<String> = recordings
                    .iter()
                    .filter_map(|recording| recording.recording_mbid.clone())
                    .collect();
                let groups = client
                    .recording_release_groups(&ids, creds)
                    .await
                    .into_option()
                    .unwrap_or_default();
                Some(SectionRows {
                    songs: recordings
                        .into_iter()
                        .map(|recording| TopSong {
                            release_group_mbid: recording
                                .recording_mbid
                                .as_ref()
                                .and_then(|id| groups.get(id).cloned()),
                            title: recording.title,
                            artist_name: recording.artist_name,
                            recording_mbid: recording.recording_mbid,
                            original_release_mbid: recording.release_mbid,
                            release_name: recording.release_name,
                            listen_count: recording.listen_count,
                            disc_number: None,
                            track_number: None,
                        })
                        .collect(),
                    ..SectionRows::default()
                })
            }
            Section::TopAlbums => {
                let groups = match client.artist_top_release_groups(mbid, count, creds).await {
                    listenbrainz::Outcome::Found(groups) if !groups.is_empty() => groups,
                    // No popularity rows, or the popularity read failed: v2
                    // rebuilt the list from the top recordings instead.
                    _ => {
                        return Some(SectionRows {
                            albums: albums_from_recordings(client, creds, mbid, count).await?,
                            ..SectionRows::default()
                        });
                    }
                };
                Some(SectionRows {
                    albums: groups
                        .into_iter()
                        .map(|group| TopAlbum {
                            title: group.name,
                            artist_name: group.artist_name,
                            release_group_mbid: Some(group.release_group_mbid),
                            listen_count: group.listen_count,
                            in_library: false,
                            requested: false,
                            cover_url: None,
                        })
                        .collect(),
                    ..SectionRows::default()
                })
            }
        }
    }

    /// `GET /artists/{artist_mbid}/similar`.
    pub async fn similar_artists(
        &self,
        user_id: &str,
        mbid: &str,
        count: u32,
        source: Option<DiscoverySource>,
    ) -> Result<SimilarArtistsResponse, CatalogError> {
        let answer = self
            .artist_section(user_id, mbid, count, source, Section::Similar)
            .await?;
        let mut similar = answer.rows.similar;
        let mut seen = std::collections::HashSet::new();
        similar.retain(|artist| seen.insert(artist.musicbrainz_id.to_ascii_lowercase()));
        let ids: Vec<String> = similar
            .iter()
            .map(|artist| artist.musicbrainz_id.clone())
            .collect();
        let owned = self.artist_flags(&ids).await;
        for artist in &mut similar {
            artist.in_library = owned.contains(&artist.musicbrainz_id.to_ascii_lowercase());
        }
        Ok(SimilarArtistsResponse {
            similar_artists: similar,
            source: answer.source,
            configured: answer.configured,
        })
    }

    /// `GET /artists/{artist_mbid}/top-songs`.
    pub async fn top_songs(
        &self,
        user_id: &str,
        mbid: &str,
        count: u32,
        source: Option<DiscoverySource>,
    ) -> Result<TopSongsResponse, CatalogError> {
        let answer = self
            .artist_section(user_id, mbid, count, source, Section::TopSongs)
            .await?;
        let mut songs = answer.rows.songs;
        self.fill_track_numbers(&mut songs).await;
        Ok(TopSongsResponse {
            songs,
            source: answer.source,
            configured: answer.configured,
        })
    }

    /// Disc and track numbers for top songs (v2 looked each song up on
    /// MusicBrainz in the request). Numbers come from the cached tracklist
    /// of the release the listens point at, so songs sharing a release
    /// share one lookup and album pages share the cache. The request only
    /// reads the cache; releases not cached yet are fetched once in the
    /// background at background priority, so the page answers at once and
    /// carries the numbers from the next load on.
    async fn fill_track_numbers(&self, songs: &mut [TopSong]) {
        let mut releases: Vec<String> = Vec::new();
        for song in songs.iter() {
            if let Some(release) = song.original_release_mbid.as_deref()
                && is_mbid(release)
                && !releases
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(release))
            {
                releases.push(release.to_ascii_lowercase());
            }
        }
        releases.truncate(MAX_TOP_SONG_RELEASES);
        let mut missing = Vec::new();
        for release in releases {
            let Some(detail) = self.cached_release_detail(&release).await else {
                missing.push(release);
                continue;
            };
            for song in songs.iter_mut().filter(|song| {
                song.original_release_mbid
                    .as_deref()
                    .is_some_and(|id| id.eq_ignore_ascii_case(&release))
            }) {
                let Some(recording) = song.recording_mbid.as_deref() else {
                    continue;
                };
                if let Some(track) = detail.tracks.iter().find(|track| {
                    track
                        .recording_id
                        .as_deref()
                        .is_some_and(|id| id.eq_ignore_ascii_case(recording))
                }) {
                    song.disc_number = Some(track.disc_number);
                    song.track_number = Some(track.position);
                }
            }
        }
        self.warm_releases(missing);
    }

    /// Fetch release tracklists into the cache at background priority, one
    /// task per release not already being fetched.
    fn warm_releases(&self, releases: Vec<String>) {
        for release in releases {
            let key = format!("release:{release}");
            {
                let mut warming = self
                    .inner
                    .warming
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if !warming.insert(key.clone()) {
                    continue;
                }
            }
            let catalog = self.clone();
            tokio::spawn(async move {
                if let Err(error) = catalog
                    .release_detail_at(&release, RequestPriority::BackgroundSync)
                    .await
                {
                    tracing::warn!(release = %release, %error, "release tracklist warm failed");
                }
                catalog
                    .inner
                    .warming
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(&key);
            });
        }
    }

    /// `GET /artists/{artist_mbid}/top-albums`.
    pub async fn top_albums(
        &self,
        user_id: &str,
        mbid: &str,
        count: u32,
        source: Option<DiscoverySource>,
    ) -> Result<TopAlbumsResponse, CatalogError> {
        let answer = self
            .artist_section(user_id, mbid, count, source, Section::TopAlbums)
            .await?;
        let mut albums = answer.rows.albums;
        let ids: Vec<String> = albums
            .iter()
            .filter_map(|album| album.release_group_mbid.clone())
            .collect();
        let (owned, requested) = self.album_flags(&ids).await;
        for album in &mut albums {
            let id = album
                .release_group_mbid
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase();
            album.in_library = owned.contains(&id);
            album.requested = !album.in_library && requested.contains(&id);
            album.cover_url = mapping::release_group_cover_url(&id);
        }
        Ok(TopAlbumsResponse {
            albums,
            source: answer.source,
            configured: answer.configured,
        })
    }

    /// `GET /albums/{album_id}/similar`: top albums of artists similar to
    /// this album's artist. ListenBrainz names the similar artists and their
    /// albums; when it has no albums for them, Last.fm's top albums for the
    /// same artists fill in with the user's or the instance Last.fm key (v2
    /// `_similar_albums_lastfm`).
    pub async fn similar_albums(
        &self,
        user_id: &str,
        raw_album: &str,
        raw_artist: &str,
        count: u32,
    ) -> Result<SimilarAlbumsResponse, CatalogError> {
        let album = checked_mbid(raw_album, "album")?;
        let artist = checked_mbid(raw_artist, "artist")?;
        let Some((client, creds)) = self.upstream().listenbrainz() else {
            return Ok(SimilarAlbumsResponse {
                albums: Vec::new(),
                source: DiscoverySource::Listenbrainz,
                configured: false,
            });
        };
        let key = format!("lb_similar_albums:{album}:{artist}:{count}");
        let catalog = self.clone();
        let (lb_album, lb_artist) = (album.clone(), artist.clone());
        let value = self
            .cached(&self.inner.flights.other, key, move || async move {
                let Some(similar) = client
                    .similar_artists(&lb_artist, SIMILAR_ALBUM_ARTISTS, &creds)
                    .await
                    .into_option()
                else {
                    return Ok((serde_json::Value::Null, None));
                };
                let similar: Vec<(String, String)> = similar
                    .into_iter()
                    .filter(|artist| is_mbid(&artist.artist_mbid))
                    .take(SIMILAR_ALBUM_ARTISTS)
                    .map(|artist| (artist.artist_mbid, artist.artist_name))
                    .collect();
                let mut albums = Vec::new();
                for (similar_mbid, similar_name) in &similar {
                    let Some(groups) = client
                        .artist_top_release_groups(similar_mbid, ALBUMS_PER_SIMILAR_ARTIST, &creds)
                        .await
                        .into_option()
                    else {
                        continue;
                    };
                    for group in groups {
                        if group.release_group_mbid.eq_ignore_ascii_case(&lb_album) {
                            continue;
                        }
                        albums.push(DiscoveryAlbum {
                            musicbrainz_id: group.release_group_mbid,
                            title: group.name,
                            artist_name: if group.artist_name.is_empty() {
                                similar_name.clone()
                            } else {
                                group.artist_name
                            },
                            artist_id: Some(similar_mbid.clone()),
                            year: None,
                            in_library: false,
                            requested: false,
                            cover_url: None,
                        });
                    }
                    if albums.len() >= count as usize {
                        break;
                    }
                }
                albums.truncate(count as usize);
                let ttl = catalog.discovery_ttl(false, albums.is_empty());
                let value = serde_json::to_value(SimilarAlbumRows { albums, similar }).map_err(
                    |error| CatalogError::Internal(format!("discovery encode: {error}")),
                )?;
                Ok((value, Some(ttl)))
            })
            .await?;
        let rows: SimilarAlbumRows = serde_json::from_value(value).unwrap_or_default();
        let mut albums = rows.albums;
        if albums.is_empty() && !rows.similar.is_empty() {
            albums = self
                .similar_albums_lastfm(user_id, &album, &rows.similar, count)
                .await;
        }
        self.flag_discovery_albums(&mut albums).await;
        Ok(SimilarAlbumsResponse {
            albums,
            source: DiscoverySource::Listenbrainz,
            configured: true,
        })
    }

    /// Last.fm top albums of the similar artists, resolved to release
    /// groups. Last.fm names releases, so each album resolves through
    /// MusicBrainz once and the answer is cached a week; resolution runs
    /// under a short deadline, and a partial list is cached only a minute
    /// so the next visit fills in from the resolutions already made.
    async fn similar_albums_lastfm(
        &self,
        user_id: &str,
        album: &str,
        similar: &[(String, String)],
        count: u32,
    ) -> Vec<DiscoveryAlbum> {
        let Some((client, creds)) = self.upstream().lastfm(user_id).await else {
            return Vec::new();
        };
        let key = format!("lfm_similar_albums:{album}:{count}");
        let catalog = self.clone();
        let album = album.to_owned();
        let similar = similar.to_vec();
        let value = self
            .cached(&self.inner.flights.other, key, move || async move {
                let mut albums: Vec<DiscoveryAlbum> = Vec::new();
                let mut seen: HashSet<String> = HashSet::from([album.clone()]);
                let mut complete = true;
                let deadline = tokio::time::Instant::now() + LASTFM_RESOLVE_BUDGET;
                'artists: for (artist_mbid, artist_name) in &similar {
                    let top = match client
                        .artist_top_albums(&creds, artist_name, Some(artist_mbid), 3)
                        .await
                    {
                        lastfm::Outcome::Found(top) => top,
                        lastfm::Outcome::Missing => continue,
                        lastfm::Outcome::Unavailable { .. } => {
                            complete = false;
                            continue;
                        }
                    };
                    for item in top {
                        if albums.len() >= count as usize {
                            break 'artists;
                        }
                        let Some(release) = item.mbid.filter(|id| is_mbid(id)) else {
                            continue;
                        };
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        let group = match tokio::time::timeout(
                            remaining,
                            catalog.release_to_group(&release),
                        )
                        .await
                        {
                            Ok(Some(group)) => group,
                            Ok(None) => continue,
                            Err(_) => {
                                complete = false;
                                break 'artists;
                            }
                        };
                        if !seen.insert(group.to_ascii_lowercase()) {
                            continue;
                        }
                        albums.push(DiscoveryAlbum {
                            musicbrainz_id: group,
                            title: item.name,
                            artist_name: if item.artist_name.is_empty() {
                                artist_name.clone()
                            } else {
                                item.artist_name
                            },
                            artist_id: Some(artist_mbid.clone()),
                            year: None,
                            in_library: false,
                            requested: false,
                            cover_url: None,
                        });
                    }
                }
                if !complete {
                    record_current("lastfm", IntegrationStatus::Degraded, false);
                }
                let ttl = if complete {
                    catalog.discovery_ttl(false, albums.is_empty())
                } else {
                    PARTIAL_TTL
                };
                let value = serde_json::to_value(&albums).map_err(|error| {
                    CatalogError::Internal(format!("discovery encode: {error}"))
                })?;
                Ok((value, Some(ttl)))
            })
            .await;
        value
            .ok()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    /// The release group of one release, cached a week (misses ten
    /// minutes). `None` when MusicBrainz has no such release or failed.
    pub(super) async fn release_to_group(&self, release: &str) -> Option<String> {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let key = format!(
            "mb:release_to_rg:{namespace}:{}",
            release.to_ascii_lowercase()
        );
        let catalog = self.clone();
        let release = release.to_owned();
        let found = self
            .cached(&self.inner.flights.other, key, move || async move {
                let (client, _) = catalog
                    .upstream()
                    .musicbrainz(RequestPriority::UserInitiated);
                let group = mb_retry(|| {
                    client.resolve_release_to_release_group(&release, Criticality::IdentityCritical)
                })
                .await
                .map_err(mb_error)?;
                let ttl = if group.is_some() {
                    RELEASE_GROUP_TTL
                } else {
                    MISS_TTL
                };
                Ok((serde_json::to_value(group).unwrap_or_default(), Some(ttl)))
            })
            .await;
        match found {
            Ok(value) => serde_json::from_value(value).ok().flatten(),
            Err(error) => {
                record_mb_down(&error);
                None
            }
        }
    }

    /// `GET /albums/{album_id}/more-by-artist`: the artist's other release
    /// groups, in MusicBrainz order. A dead MusicBrainz reads as empty, the
    /// way v2 kept the album page up.
    pub async fn more_by_artist(
        &self,
        raw_album: &str,
        raw_artist: &str,
        count: u32,
    ) -> Result<MoreByArtistResponse, CatalogError> {
        let artist = checked_mbid(raw_artist, "artist")?;
        let album = raw_album.trim().to_ascii_lowercase();
        let list = match self.release_groups(&artist).await {
            Ok(list) => list,
            Err(error) => {
                tracing::warn!(artist = %artist, %error, "more-by-artist unavailable");
                return Ok(MoreByArtistResponse {
                    albums: Vec::new(),
                    artist_name: String::new(),
                });
            }
        };
        let artist_name = list
            .items
            .iter()
            .find_map(|item| item.artist_name.clone())
            .unwrap_or_default();
        let mut albums: Vec<DiscoveryAlbum> = list
            .items
            .iter()
            .filter(|item| item.id.to_ascii_lowercase() != album)
            .take(count as usize)
            .map(|item| DiscoveryAlbum {
                musicbrainz_id: item.id.clone(),
                title: item.title.clone().unwrap_or_else(|| "Unknown".to_owned()),
                artist_name: item
                    .artist_name
                    .clone()
                    .unwrap_or_else(|| artist_name.clone()),
                artist_id: Some(artist.clone()),
                year: mapping::year_of(item.first_release_date.as_deref()),
                in_library: false,
                requested: false,
                cover_url: None,
            })
            .collect();
        self.flag_discovery_albums(&mut albums).await;
        Ok(MoreByArtistResponse {
            albums,
            artist_name,
        })
    }

    async fn flag_discovery_albums(&self, albums: &mut [DiscoveryAlbum]) {
        let ids: Vec<String> = albums
            .iter()
            .map(|album| album.musicbrainz_id.clone())
            .collect();
        let (owned, requested) = self.album_flags(&ids).await;
        for album in albums {
            let id = album.musicbrainz_id.to_ascii_lowercase();
            album.in_library = owned.contains(&id);
            album.requested = !album.in_library && requested.contains(&id);
            album.cover_url = mapping::release_group_cover_url(&id);
        }
    }
}

/// Last.fm rows for one section; `None` when Last.fm failed.
async fn lastfm_rows(
    client: &super::upstream::CatalogLastFm,
    creds: &lastfm::LastFmCredentials,
    mbid: &str,
    count: u32,
    section: Section,
    groups: Option<&ReleaseGroupList>,
) -> Option<SectionRows> {
    let found = |outcome: lastfm::Outcome<Vec<lastfm::TopItem>>| match outcome {
        lastfm::Outcome::Found(items) => Some(items),
        lastfm::Outcome::Missing => Some(Vec::new()),
        lastfm::Outcome::Unavailable { .. } => None,
    };
    match section {
        Section::Similar => {
            let similar = match client.similar_artists(creds, "", Some(mbid), count).await {
                lastfm::Outcome::Found(similar) => similar,
                lastfm::Outcome::Missing => Vec::new(),
                lastfm::Outcome::Unavailable { .. } => return None,
            };
            Some(SectionRows {
                similar: similar
                    .into_iter()
                    .filter_map(|artist| {
                        Some(SimilarArtist {
                            musicbrainz_id: artist.mbid.filter(|id| is_mbid(id))?,
                            name: artist.name,
                            listen_count: 0,
                            in_library: false,
                        })
                    })
                    .collect(),
                ..SectionRows::default()
            })
        }
        Section::TopSongs => Some(SectionRows {
            songs: found(client.artist_top_tracks(creds, "", Some(mbid), count).await)?
                .into_iter()
                .map(|track| TopSong {
                    title: track.name,
                    artist_name: track.artist_name,
                    recording_mbid: track.mbid,
                    release_group_mbid: None,
                    original_release_mbid: None,
                    release_name: None,
                    listen_count: track.playcount,
                    disc_number: None,
                    track_number: None,
                })
                .collect(),
            ..SectionRows::default()
        }),
        Section::TopAlbums => {
            let top = found(client.artist_top_albums(creds, "", Some(mbid), count).await)?;
            Some(SectionRows {
                albums: match_lastfm_albums(top, groups),
                ..SectionRows::default()
            })
        }
    }
}

/// Rebuild an artist's top albums from their most played recordings (v2
/// `_get_top_albums_from_recordings_fallback`): listens add up per release
/// group (or per release title when the group is unknown), most played
/// first. `None` when ListenBrainz failed.
async fn albums_from_recordings(
    client: &super::upstream::CatalogListenBrainz,
    creds: &listenbrainz::ListenBrainzCredentials,
    mbid: &str,
    count: usize,
) -> Option<Vec<TopAlbum>> {
    let recordings = client
        .artist_top_recordings(mbid, (count * 8).max(80), creds)
        .await
        .into_option()?;
    let ids: Vec<String> = recordings
        .iter()
        .filter_map(|recording| recording.recording_mbid.clone())
        .collect();
    let groups = client
        .recording_release_groups(&ids, creds)
        .await
        .into_option()
        .unwrap_or_default();
    let mut order: Vec<String> = Vec::new();
    let mut totals: HashMap<String, TopAlbum> = HashMap::new();
    for recording in recordings {
        let group = recording
            .recording_mbid
            .as_ref()
            .and_then(|id| groups.get(id))
            .map(|group| group.to_ascii_lowercase());
        let title = recording.release_name.clone().unwrap_or_default();
        let key = match (&group, title.trim()) {
            (Some(group), _) => group.clone(),
            (None, "") => continue,
            (None, title) => format!("name:{}", title.to_lowercase()),
        };
        let entry = totals.entry(key.clone()).or_insert_with(|| {
            order.push(key);
            TopAlbum {
                title: if title.trim().is_empty() {
                    "Unknown".to_owned()
                } else {
                    title.trim().to_owned()
                },
                artist_name: recording.artist_name.clone(),
                release_group_mbid: group.clone(),
                listen_count: 0,
                in_library: false,
                requested: false,
                cover_url: None,
            }
        });
        entry.listen_count += recording.listen_count;
    }
    let mut albums: Vec<TopAlbum> = order
        .into_iter()
        .filter_map(|key| totals.remove(&key))
        .collect();
    albums.sort_by(|left, right| right.listen_count.cmp(&left.listen_count));
    albums.truncate(count);
    Some(albums)
}

/// Collapse a title for matching: casefolded, whitespace collapsed.
fn title_key(title: &str) -> String {
    title
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Match Last.fm's top albums to the artist's release groups (v2
/// `_get_top_albums_lastfm`): a Last.fm id that already is one of the
/// artist's release groups wins, else a unique title match (deluxe
/// suffixes dropped), else the row stays unlinked. One row per group.
fn match_lastfm_albums(
    top: Vec<lastfm::TopItem>,
    groups: Option<&ReleaseGroupList>,
) -> Vec<TopAlbum> {
    let mut ids: HashSet<String> = HashSet::new();
    let mut by_title: HashMap<String, Option<String>> = HashMap::new();
    for group in groups.map(|list| list.items.as_slice()).unwrap_or_default() {
        let id = group.id.to_ascii_lowercase();
        ids.insert(id.clone());
        let Some(title) = group
            .title
            .as_deref()
            .map(title_key)
            .filter(|key| !key.is_empty())
        else {
            continue;
        };
        by_title
            .entry(title)
            .and_modify(|known| {
                if known.as_deref() != Some(id.as_str()) {
                    *known = None;
                }
            })
            .or_insert(Some(id));
    }
    let mut seen = HashSet::new();
    let mut albums = Vec::new();
    for item in top {
        let raw = item.mbid.as_deref().map(str::to_ascii_lowercase);
        let by_name = title_candidates(&item.name)
            .into_iter()
            .find_map(|key| by_title.get(&key).cloned())
            .flatten();
        let group = raw.filter(|id| ids.contains(id)).or(by_name);
        if let Some(group) = &group
            && !seen.insert(group.clone())
        {
            continue;
        }
        albums.push(TopAlbum {
            title: item.name,
            artist_name: item.artist_name,
            release_group_mbid: group,
            listen_count: item.playcount,
            in_library: false,
            requested: false,
            cover_url: None,
        });
    }
    albums
}

/// The title and, for deluxe editions, the title without the suffix.
fn title_candidates(title: &str) -> Vec<String> {
    let key = title_key(title);
    if key.is_empty() {
        return Vec::new();
    }
    let mut candidates = vec![key.clone()];
    for suffix in [
        " (deluxe)",
        " (deluxe edition)",
        " [deluxe]",
        " [deluxe edition]",
    ] {
        if let Some(stripped) = key.strip_suffix(suffix) {
            candidates.push(stripped.trim_end().to_owned());
            break;
        }
    }
    candidates
}

/// Cached ListenBrainz similar-album rows plus the similar artists, so a
/// Last.fm fallback can reuse them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SimilarAlbumRows {
    albums: Vec<DiscoveryAlbum>,
    similar: Vec<(String, String)>,
}
