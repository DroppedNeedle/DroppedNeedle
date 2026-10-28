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

use droppedneedle_kernel::runtime_config::sections::ListenSource;
use droppedneedle_providers::musicbrainz::Criticality;
use droppedneedle_providers::{
    CacheKey, Fetched, IntegrationStatus, Policy, RequestPriority, Source, lastfm, listenbrainz,
    record_current,
};

use super::artist::ReleaseGroupList;
use super::artist::checked_mbid;
use super::error::CatalogError;
use super::mapping;
use super::models::{
    AlbumRef, ArtistRef, MoreByArtistResponse, SimilarAlbumsResponse, SimilarArtistsResponse,
    TopAlbumsResponse, TopSong, TopSongsResponse,
};
use super::{Catalog, is_mbid, mb_error, record_mb_down};
use crate::reads::cache_hold::CacheHold;
use crate::reads::platform::artwork::warm::WarmTargets;

/// Releases whose tracklists top songs look up, at most.
const MAX_TOP_SONG_RELEASES: usize = 10;
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
    /// The cache key part. A part names its row shape too, so a cached
    /// answer in an older shape is never read as the new one.
    fn key_part(self) -> &'static str {
        match self {
            Self::Similar => "similar_artist_refs",
            Self::TopSongs => "top_songs",
            Self::TopAlbums => "top_album_refs",
        }
    }
}

/// One section's raw rows before library flags, as cached.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SectionRows {
    similar: Vec<ArtistRef>,
    songs: Vec<TopSong>,
    albums: Vec<AlbumRef>,
}

impl SectionRows {
    fn is_empty(&self) -> bool {
        self.similar.is_empty() && self.songs.is_empty() && self.albums.is_empty()
    }
}

/// A section answer: rows, who answered, and whether that source is set up.
struct Answer {
    rows: SectionRows,
    source: ListenSource,
    configured: bool,
}

/// What a section fetch found: rows, kept when every read behind them
/// answered, "nothing there" when empty, and `Err` when the source failed
/// (a kept answer then stands in).
fn section_answer(
    rows: Option<SectionRows>,
    hold: CacheHold,
) -> Result<Fetched<SectionRows>, CatalogError> {
    let Some(rows) = rows else {
        return Err(CatalogError::Internal("discovery source failed".to_owned()));
    };
    Ok(if hold != CacheHold::default() {
        Fetched::Partial(rows)
    } else if rows.is_empty() {
        Fetched::Missing
    } else {
        Fetched::Found(rows)
    })
}

impl Catalog {
    fn preferred_source(&self, asked: Option<ListenSource>) -> ListenSource {
        asked.unwrap_or_else(|| self.upstream().settings().primary_source())
    }

    /// Build one artist section from the preferred source, falling back
    /// from ListenBrainz to Last.fm when ListenBrainz has nothing.
    async fn artist_section(
        &self,
        user_id: &str,
        raw_mbid: &str,
        count: u32,
        asked: Option<ListenSource>,
        section: Section,
        priority: RequestPriority,
    ) -> Result<Answer, CatalogError> {
        let mbid = checked_mbid(raw_mbid, "artist")?;
        let source = self.preferred_source(asked);
        let first = self
            .section_from(source, user_id, &mbid, count, section, priority)
            .await;
        if source == ListenSource::Listenbrainz && first.rows.is_empty() {
            let fallback = self
                .section_from(
                    ListenSource::Lastfm,
                    user_id,
                    &mbid,
                    count,
                    section,
                    priority,
                )
                .await;
            if fallback.configured && !fallback.rows.is_empty() {
                return Ok(fallback);
            }
        }
        Ok(first)
    }

    /// The cache key of one section from one source.
    fn section_key(
        &self,
        source: ListenSource,
        mbid: &str,
        count: u32,
        section: Section,
    ) -> CacheKey {
        let count_part = count.to_string();
        match (source, section) {
            (ListenSource::Listenbrainz, _) => CacheKey::new(
                Source::ListenBrainz,
                &[section.key_part(), mbid, &count_part],
            ),
            // Last.fm titles are matched against the MusicBrainz
            // discography, so the MusicBrainz source is in the key.
            (ListenSource::Lastfm, Section::TopAlbums) => {
                let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
                CacheKey::new(
                    Source::LastFm,
                    &[&namespace, section.key_part(), mbid, &count_part],
                )
            }
            (ListenSource::Lastfm, _) => {
                CacheKey::new(Source::LastFm, &[section.key_part(), mbid, &count_part])
            }
        }
    }

    async fn section_from(
        &self,
        source: ListenSource,
        user_id: &str,
        mbid: &str,
        count: u32,
        section: Section,
        priority: RequestPriority,
    ) -> Answer {
        let key = self.section_key(source, mbid, count, section);
        let unconfigured = Answer {
            rows: SectionRows::default(),
            source,
            configured: false,
        };
        let rows = match source {
            ListenSource::Listenbrainz => {
                let Some((client, creds)) = self.upstream().listenbrainz() else {
                    return unconfigured;
                };
                let catalog = self.clone();
                let mbid = mbid.to_owned();
                self.cached(
                    &key,
                    Policy::DISCOVERY,
                    priority,
                    move |priority| async move {
                        let mut hold = CacheHold::default();
                        let rows = catalog
                            .listenbrainz_rows(
                                &client,
                                &creds,
                                (&mbid, count, section, priority),
                                &mut hold,
                            )
                            .await;
                        section_answer(rows, hold)
                    },
                )
                .await
            }
            ListenSource::Lastfm => {
                let Some((client, creds)) = self.upstream().lastfm(user_id).await else {
                    return unconfigured;
                };
                let catalog = self.clone();
                let mbid = mbid.to_owned();
                self.cached(
                    &key,
                    Policy::DISCOVERY,
                    priority,
                    move |priority| async move {
                        // Top albums match Last.fm titles against the artist's
                        // (cached) discography to find release groups.
                        let mut hold = CacheHold::default();
                        let groups = match section {
                            Section::TopAlbums => {
                                match catalog.release_groups(&mbid, priority).await {
                                    Ok(groups) => Some(groups),
                                    Err(error) => {
                                        note_catalog_failure(&mut hold, &error);
                                        None
                                    }
                                }
                            }
                            _ => None,
                        };
                        let rows = lastfm_rows(
                            &client,
                            &creds,
                            (&mbid, count, section, priority),
                            groups.as_ref(),
                        )
                        .await;
                        // Rows matched against a missing or partial discography
                        // are not kept, so links fill in once it lands.
                        let partial = matches!(section, Section::TopAlbums)
                            && !groups.as_ref().is_some_and(|groups| groups.complete);
                        Ok(match section_answer(rows, hold)? {
                            Fetched::Found(rows) if partial => Fetched::Partial(rows),
                            answer => answer,
                        })
                    },
                )
                .await
            }
        };
        Answer {
            rows: rows.ok().flatten().unwrap_or_default(),
            source,
            configured: true,
        }
    }

    /// Whether the artist's similar artists, top songs and top albums, at
    /// these counts, are all held, fresh or stale, as `user_id` reads them.
    /// A section ListenBrainz has nothing for counts once Last.fm's answer,
    /// which then fills in, is held too.
    pub(super) async fn artist_sections_held(
        &self,
        user_id: &str,
        mbid: &str,
        (similar, songs, albums): (u32, u32, u32),
    ) -> bool {
        let listenbrainz = self.preferred_source(None) == ListenSource::Listenbrainz
            && self.upstream().listenbrainz().is_some();
        let mut lastfm = None;
        for (section, count) in [
            (Section::Similar, similar),
            (Section::TopSongs, songs),
            (Section::TopAlbums, albums),
        ] {
            if listenbrainz {
                let key = self.section_key(ListenSource::Listenbrainz, mbid, count, section);
                match self.upstream().cache().peek::<SectionRows>(&key).await {
                    None => return false,
                    Some(Some(_)) => continue,
                    Some(None) => {}
                }
            }
            let configured = match lastfm {
                Some(configured) => configured,
                None => *lastfm.insert(self.upstream().lastfm(user_id).await.is_some()),
            };
            if configured
                && !self
                    .held(&self.section_key(ListenSource::Lastfm, mbid, count, section))
                    .await
            {
                return false;
            }
        }
        true
    }

    /// ListenBrainz rows for one section; `None` when ListenBrainz failed.
    /// A secondary read that failed is noted in `hold`.
    async fn listenbrainz_rows(
        &self,
        client: &super::upstream::CatalogListenBrainz,
        creds: &listenbrainz::ListenBrainzCredentials,
        (mbid, count, section, priority): (&str, u32, Section, RequestPriority),
        hold: &mut CacheHold,
    ) -> Option<SectionRows> {
        let count = count as usize;
        match section {
            Section::Similar => {
                let similar = client
                    .similar_artists(mbid, count, creds, priority)
                    .await
                    .ok()?;
                Some(SectionRows {
                    similar: similar
                        .into_iter()
                        .filter(|artist| is_mbid(&artist.artist_mbid))
                        .take(count)
                        .map(|artist| ArtistRef {
                            musicbrainz_id: Some(artist.artist_mbid),
                            listen_count: Some(artist.listen_count),
                            ..ArtistRef::named(artist.artist_name)
                        })
                        .collect(),
                    ..SectionRows::default()
                })
            }
            Section::TopSongs => {
                let recordings = client
                    .artist_top_recordings(mbid, count, creds, priority)
                    .await
                    .ok()?;
                let ids: Vec<String> = recordings
                    .iter()
                    .filter_map(|recording| recording.recording_mbid.clone())
                    .collect();
                let groups = client
                    .recording_release_groups(&ids, creds, priority)
                    .await
                    .unwrap_or_else(|error| {
                        hold.provider(&error);
                        HashMap::new()
                    });
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
                let groups = match client
                    .artist_top_release_groups(mbid, count, creds, priority)
                    .await
                {
                    Ok(groups) if !groups.is_empty() => groups,
                    // No popularity rows, or the popularity read failed: v2
                    // rebuilt the list from the top recordings instead.
                    other => {
                        if let Err(error) = &other {
                            hold.provider(error);
                        }
                        return Some(SectionRows {
                            albums: albums_from_recordings(
                                client,
                                creds,
                                (mbid, count, priority),
                                hold,
                            )
                            .await?,
                            ..SectionRows::default()
                        });
                    }
                };
                Some(SectionRows {
                    albums: groups
                        .into_iter()
                        .map(|group| AlbumRef {
                            artist_name: Some(group.artist_name),
                            musicbrainz_id: Some(group.release_group_mbid),
                            listen_count: Some(group.listen_count),
                            ..AlbumRef::titled(group.name)
                        })
                        .collect(),
                    ..SectionRows::default()
                })
            }
        }
    }

    /// `GET /artists/{artist_mbid}/similar`. A page reads at user
    /// priority, the precache at background priority.
    pub async fn similar_artists(
        &self,
        user_id: &str,
        mbid: &str,
        count: u32,
        source: Option<ListenSource>,
        priority: RequestPriority,
    ) -> Result<SimilarArtistsResponse, CatalogError> {
        let answer = self
            .artist_section(user_id, mbid, count, source, Section::Similar, priority)
            .await?;
        let mut similar = answer.rows.similar;
        let mut seen = std::collections::HashSet::new();
        similar.retain(|artist| {
            seen.insert(
                artist
                    .musicbrainz_id
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            )
        });
        let ids: Vec<String> = similar
            .iter()
            .filter_map(|artist| artist.musicbrainz_id.clone())
            .collect();
        let owned = self.artist_flags(&ids).await;
        for artist in &mut similar {
            artist.in_library = artist
                .musicbrainz_id
                .as_deref()
                .is_some_and(|id| owned.contains(&id.to_ascii_lowercase()));
        }
        if priority.is_user_facing() {
            let mut targets = WarmTargets::default();
            for id in &ids {
                targets.artist(Some(id));
            }
            self.warm_covers("similar", targets);
        }
        Ok(SimilarArtistsResponse {
            similar_artists: similar,
            source: answer.source,
            configured: answer.configured,
        })
    }

    /// `GET /artists/{artist_mbid}/top-songs`. A page reads at user
    /// priority, the precache at background priority.
    pub async fn top_songs(
        &self,
        user_id: &str,
        mbid: &str,
        count: u32,
        source: Option<ListenSource>,
        priority: RequestPriority,
    ) -> Result<TopSongsResponse, CatalogError> {
        let answer = self
            .artist_section(user_id, mbid, count, source, Section::TopSongs, priority)
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

    /// `GET /artists/{artist_mbid}/top-albums`. A page reads at user
    /// priority, the precache at background priority.
    pub async fn top_albums(
        &self,
        user_id: &str,
        mbid: &str,
        count: u32,
        source: Option<ListenSource>,
        priority: RequestPriority,
    ) -> Result<TopAlbumsResponse, CatalogError> {
        let answer = self
            .artist_section(user_id, mbid, count, source, Section::TopAlbums, priority)
            .await?;
        let mut albums = answer.rows.albums;
        let ids: Vec<String> = albums
            .iter()
            .filter_map(|album| album.musicbrainz_id.clone())
            .collect();
        let (owned, requested) = self.album_flags(&ids).await;
        for album in &mut albums {
            let id = album
                .musicbrainz_id
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
                source: ListenSource::Listenbrainz,
                configured: false,
            });
        };
        let key = CacheKey::new(
            Source::ListenBrainz,
            &["similar_albums", &album, &artist, &count.to_string()],
        );
        let (lb_album, lb_artist) = (album.clone(), artist.clone());
        // Album pages and radio wait on this.
        let priority = RequestPriority::UserInitiated;
        let rows = self
            .cached(
                &key,
                Policy::DISCOVERY,
                priority,
                move |priority| async move {
                    let similar = client
                        .similar_artists(&lb_artist, SIMILAR_ALBUM_ARTISTS, &creds, priority)
                        .await
                        .map_err(CatalogError::Unavailable)?;
                    let similar: Vec<(String, String)> = similar
                        .into_iter()
                        .filter(|artist| is_mbid(&artist.artist_mbid))
                        .take(SIMILAR_ALBUM_ARTISTS)
                        .map(|artist| (artist.artist_mbid, artist.artist_name))
                        .collect();
                    let mut albums = Vec::new();
                    let mut hold = CacheHold::default();
                    for (similar_mbid, similar_name) in &similar {
                        let groups = match client
                            .artist_top_release_groups(
                                similar_mbid,
                                ALBUMS_PER_SIMILAR_ARTIST,
                                &creds,
                                priority,
                            )
                            .await
                        {
                            Ok(groups) => groups,
                            Err(error) => {
                                hold.provider(&error);
                                continue;
                            }
                        };
                        for group in groups {
                            if group.release_group_mbid.eq_ignore_ascii_case(&lb_album) {
                                continue;
                            }
                            albums.push(AlbumRef {
                                musicbrainz_id: Some(group.release_group_mbid),
                                artist_name: Some(if group.artist_name.is_empty() {
                                    similar_name.clone()
                                } else {
                                    group.artist_name
                                }),
                                artist_id: Some(similar_mbid.clone()),
                                ..AlbumRef::titled(group.name)
                            });
                        }
                        if albums.len() >= count as usize {
                            break;
                        }
                    }
                    albums.truncate(count as usize);
                    let rows = SimilarAlbumRows { albums, similar };
                    Ok(if hold != CacheHold::default() {
                        Fetched::Partial(rows)
                    } else if rows.albums.is_empty() && rows.similar.is_empty() {
                        Fetched::Missing
                    } else {
                        Fetched::Found(rows)
                    })
                },
            )
            .await;
        // A failure was recorded where it happened; the section is empty.
        let rows = rows.ok().flatten().unwrap_or_default();
        let mut albums = rows.albums;
        if albums.is_empty() && !rows.similar.is_empty() {
            albums = self
                .similar_albums_lastfm(user_id, &album, &rows.similar, count)
                .await;
        }
        self.flag_discovery_albums(&mut albums).await;
        Ok(SimilarAlbumsResponse {
            albums,
            source: ListenSource::Listenbrainz,
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
    ) -> Vec<AlbumRef> {
        let Some((client, creds)) = self.upstream().lastfm(user_id).await else {
            return Vec::new();
        };
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let key = CacheKey::new(
            Source::LastFm,
            &[&namespace, "similar_albums", album, &count.to_string()],
        );
        let catalog = self.clone();
        let album = album.to_owned();
        let similar = similar.to_vec();
        let priority = RequestPriority::UserInitiated;
        let albums = self
            .cached(
                &key,
                Policy::DISCOVERY,
                priority,
                move |priority| async move {
                    let mut albums: Vec<AlbumRef> = Vec::new();
                    let mut seen: HashSet<String> = HashSet::from([album.clone()]);
                    let mut complete = true;
                    let mut hold = CacheHold::default();
                    let deadline = tokio::time::Instant::now() + LASTFM_RESOLVE_BUDGET;
                    'artists: for (artist_mbid, artist_name) in &similar {
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        let top = match tokio::time::timeout(
                            remaining,
                            client.artist_top_albums(
                                &creds,
                                artist_name,
                                Some(artist_mbid),
                                3,
                                priority,
                            ),
                        )
                        .await
                        {
                            Ok(Ok(top)) => top,
                            Ok(Err(error)) => {
                                hold.provider(&error);
                                complete = false;
                                continue;
                            }
                            Err(_) => {
                                complete = false;
                                break 'artists;
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
                                Ok(Ok(Some(group))) => group,
                                Ok(Ok(None)) => continue,
                                Ok(Err(error)) => {
                                    note_catalog_failure(&mut hold, &error);
                                    complete = false;
                                    continue;
                                }
                                Err(_) => {
                                    complete = false;
                                    break 'artists;
                                }
                            };
                            if !seen.insert(group.to_ascii_lowercase()) {
                                continue;
                            }
                            albums.push(AlbumRef {
                                musicbrainz_id: Some(group),
                                artist_name: Some(if item.artist_name.is_empty() {
                                    artist_name.clone()
                                } else {
                                    item.artist_name
                                }),
                                artist_id: Some(artist_mbid.clone()),
                                ..AlbumRef::titled(item.name)
                            });
                        }
                    }
                    if !complete {
                        record_current("lastfm", IntegrationStatus::Degraded, false);
                    }
                    Ok(if !complete || hold != CacheHold::default() {
                        Fetched::Partial(albums)
                    } else if albums.is_empty() {
                        Fetched::Missing
                    } else {
                        Fetched::Found(albums)
                    })
                },
            )
            .await;
        albums.ok().flatten().unwrap_or_default()
    }

    /// The release group of one release, cached a week (misses ten
    /// minutes). `None` when MusicBrainz has no such release.
    pub(super) async fn release_to_group(
        &self,
        release: &str,
    ) -> Result<Option<String>, CatalogError> {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let key = CacheKey::new(
            Source::MusicBrainz,
            &[&namespace, "release_to_rg", &release.to_ascii_lowercase()],
        );
        let catalog = self.clone();
        let release = release.to_owned();
        let found = self
            .cached(
                &key,
                Policy::RELEASE_TO_GROUP,
                RequestPriority::UserInitiated,
                move |priority| async move {
                    let (client, _) = catalog.upstream().musicbrainz(priority);
                    let group = client
                        .resolve_release_to_release_group(&release, Criticality::IdentityCritical)
                        .await
                        .map_err(mb_error)?;
                    Ok(group.map_or(Fetched::Missing, Fetched::Found))
                },
            )
            .await;
        match found {
            Ok(group) => Ok(group),
            Err(error) => {
                record_mb_down(&error);
                Err(error)
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
        let list = match self
            .release_groups(&artist, RequestPriority::UserInitiated)
            .await
        {
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
        let mut albums: Vec<AlbumRef> = list
            .items
            .iter()
            .filter(|item| item.id.to_ascii_lowercase() != album)
            .take(count as usize)
            .map(|item| AlbumRef {
                musicbrainz_id: Some(item.id.clone()),
                artist_name: Some(
                    item.artist_name
                        .clone()
                        .unwrap_or_else(|| artist_name.clone()),
                ),
                artist_id: Some(artist.clone()),
                year: mapping::year_of(item.first_release_date.as_deref()),
                ..AlbumRef::titled(item.title.clone().unwrap_or_else(|| "Unknown".to_owned()))
            })
            .collect();
        self.flag_discovery_albums(&mut albums).await;
        Ok(MoreByArtistResponse {
            albums,
            artist_name,
        })
    }

    async fn flag_discovery_albums(&self, albums: &mut [AlbumRef]) {
        let ids: Vec<String> = albums
            .iter()
            .filter_map(|album| album.musicbrainz_id.clone())
            .collect();
        let (owned, requested) = self.album_flags(&ids).await;
        for album in albums {
            let Some(id) = album.musicbrainz_id.as_deref().map(str::to_ascii_lowercase) else {
                continue;
            };
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
    (mbid, count, section, priority): (&str, u32, Section, RequestPriority),
    groups: Option<&ReleaseGroupList>,
) -> Option<SectionRows> {
    match section {
        Section::Similar => {
            let similar = client
                .similar_artists(creds, "", Some(mbid), count, priority)
                .await
                .ok()?;
            Some(SectionRows {
                similar: similar
                    .into_iter()
                    .filter_map(|artist| {
                        Some(ArtistRef {
                            musicbrainz_id: Some(artist.mbid.filter(|id| is_mbid(id))?),
                            ..ArtistRef::named(artist.name)
                        })
                    })
                    .collect(),
                ..SectionRows::default()
            })
        }
        Section::TopSongs => Some(SectionRows {
            songs: client
                .artist_top_tracks(creds, "", Some(mbid), count, priority)
                .await
                .ok()?
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
            let top = client
                .artist_top_albums(creds, "", Some(mbid), count, priority)
                .await
                .ok()?;
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
/// first. `None` when ListenBrainz failed; a failed release group lookup
/// is noted in `hold`.
async fn albums_from_recordings(
    client: &super::upstream::CatalogListenBrainz,
    creds: &listenbrainz::ListenBrainzCredentials,
    (mbid, count, priority): (&str, usize, RequestPriority),
    hold: &mut CacheHold,
) -> Option<Vec<AlbumRef>> {
    let recordings = client
        .artist_top_recordings(mbid, (count * 8).max(80), creds, priority)
        .await
        .ok()?;
    let ids: Vec<String> = recordings
        .iter()
        .filter_map(|recording| recording.recording_mbid.clone())
        .collect();
    let groups = client
        .recording_release_groups(&ids, creds, priority)
        .await
        .unwrap_or_else(|error| {
            hold.provider(&error);
            HashMap::new()
        });
    let mut order: Vec<String> = Vec::new();
    let mut totals: HashMap<String, AlbumRef> = HashMap::new();
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
            AlbumRef {
                artist_name: Some(recording.artist_name.clone()),
                musicbrainz_id: group.clone(),
                listen_count: Some(0),
                ..AlbumRef::titled(if title.trim().is_empty() {
                    "Unknown".to_owned()
                } else {
                    title.trim().to_owned()
                })
            }
        });
        entry.listen_count = Some(entry.listen_count.unwrap_or(0) + recording.listen_count);
    }
    let mut albums: Vec<AlbumRef> = order
        .into_iter()
        .filter_map(|key| totals.remove(&key))
        .collect();
    albums.sort_by(|left, right| right.listen_count.cmp(&left.listen_count));
    albums.truncate(count);
    Some(albums)
}

/// Note a catalog read an answer was built past: a MusicBrainz outage
/// holds for its wait, a local failure is not cached, and absence or a
/// rejected id is a real answer.
fn note_catalog_failure(hold: &mut CacheHold, error: &CatalogError) {
    match error {
        CatalogError::Unavailable(error) => hold.provider(error),
        CatalogError::Internal(_) => hold.local(),
        CatalogError::Invalid(_) | CatalogError::NotFound | CatalogError::Missing(_) => {}
    }
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
) -> Vec<AlbumRef> {
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
        albums.push(AlbumRef {
            artist_name: Some(item.artist_name),
            musicbrainz_id: group,
            listen_count: Some(item.playcount),
            ..AlbumRef::titled(item.name)
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
    albums: Vec<AlbumRef>,
    similar: Vec<(String, String)>,
}
