//! Discovery sections on artist and album pages: similar artists, top
//! songs, top albums, similar albums, and more by the same artist.
//!
//! ListenBrainz answers by default (or whichever source the primary music
//! source setting names); when it has nothing, Last.fm fills in with the
//! user's own key, the way v2 kept these sections from going blank while
//! ListenBrainz popularity was switched off upstream.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::providers::{lastfm, listenbrainz};
use crate::runtime_config::sections::MusicSource;

use super::artist::checked_mbid;
use super::error::CatalogError;
use super::mapping;
use super::models::{
    DiscoveryAlbum, DiscoverySource, MoreByArtistResponse, SimilarAlbumsResponse, SimilarArtist,
    SimilarArtistsResponse, TopAlbum, TopAlbumsResponse, TopSong, TopSongsResponse,
};
use super::{Catalog, secs};

/// Empty answers are kept ten minutes (v2 `DISCOVERY_EMPTY_CACHE_TTL`).
const EMPTY_TTL: Duration = Duration::from_secs(600);
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
                    let rows = lastfm_rows(&client, &creds, &mbid, count, section).await;
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
                        })
                        .collect(),
                    ..SectionRows::default()
                })
            }
            Section::TopAlbums => {
                let groups = client
                    .artist_top_release_groups(mbid, count, creds)
                    .await
                    .into_option()?;
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
        Ok(TopSongsResponse {
            songs: answer.rows.songs,
            source: answer.source,
            configured: answer.configured,
        })
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
    /// this album's artist, from ListenBrainz.
    pub async fn similar_albums(
        &self,
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
        let value = self
            .cached(&self.inner.flights.other, key, move || async move {
                let Some(similar) = client
                    .similar_artists(&artist, SIMILAR_ALBUM_ARTISTS, &creds)
                    .await
                    .into_option()
                else {
                    return Ok((serde_json::Value::Array(Vec::new()), None));
                };
                let mut albums = Vec::new();
                for similar_artist in similar.into_iter().take(SIMILAR_ALBUM_ARTISTS) {
                    let Some(groups) = client
                        .artist_top_release_groups(
                            &similar_artist.artist_mbid,
                            ALBUMS_PER_SIMILAR_ARTIST,
                            &creds,
                        )
                        .await
                        .into_option()
                    else {
                        continue;
                    };
                    for group in groups {
                        if group.release_group_mbid.eq_ignore_ascii_case(&album) {
                            continue;
                        }
                        albums.push(DiscoveryAlbum {
                            musicbrainz_id: group.release_group_mbid,
                            title: group.name,
                            artist_name: if group.artist_name.is_empty() {
                                similar_artist.artist_name.clone()
                            } else {
                                group.artist_name
                            },
                            artist_id: Some(similar_artist.artist_mbid.clone()),
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
                let value = serde_json::to_value(&albums).map_err(|error| {
                    CatalogError::Internal(format!("discovery encode: {error}"))
                })?;
                Ok((value, Some(ttl)))
            })
            .await?;
        let mut albums: Vec<DiscoveryAlbum> = serde_json::from_value(value).unwrap_or_default();
        self.flag_discovery_albums(&mut albums).await;
        Ok(SimilarAlbumsResponse {
            albums,
            source: DiscoverySource::Listenbrainz,
            configured: true,
        })
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
                            musicbrainz_id: artist.mbid?,
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
                })
                .collect(),
            ..SectionRows::default()
        }),
        Section::TopAlbums => Some(SectionRows {
            albums: found(client.artist_top_albums(creds, "", Some(mbid), count).await)?
                .into_iter()
                .map(|album| TopAlbum {
                    title: album.name,
                    artist_name: album.artist_name,
                    // Last.fm album ids are release ids, not release groups.
                    release_group_mbid: None,
                    listen_count: album.playcount,
                    in_library: false,
                    requested: false,
                    cover_url: None,
                })
                .collect(),
            ..SectionRows::default()
        }),
    }
}
