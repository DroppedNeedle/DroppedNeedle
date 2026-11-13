//! Discovery sections on artist and album pages: similar artists, top
//! songs, top albums, similar albums, and more by the same artist.
//!
//! ListenBrainz answers by default; when it has nothing, Last.fm fills in
//! with the user's Last.fm key or the instance key, so the sections do not
//! go blank while ListenBrainz popularity is switched off upstream.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::FutureExt as _;
use serde::{Deserialize, Serialize};

use droppedneedle_kernel::db::fold_text;
use droppedneedle_kernel::runtime_config::sections::ListenSource;
use droppedneedle_providers::musicbrainz::{
    Criticality, RecordingRelease, RecordingSearchHit, ReleaseGroupRef,
};
use droppedneedle_providers::{
    CacheKey, Fetched, IntegrationStatus, Policy, ProviderError, RequestPriority, Source, lastfm,
    listenbrainz, record_current,
};

use super::artist::checked_mbid;
use super::artist::{ReleaseGroupItem, ReleaseGroupList};
use super::error::CatalogError;
use super::mapping;
use super::models::{
    AlbumRef, ArtistRef, MoreByArtistResponse, SimilarAlbumsResponse, SimilarArtistsResponse,
    TopAlbumsResponse, TopSong, TopSongsResponse,
};
use super::{Catalog, is_mbid, mb_error, mb_search_error, record_mb_down};
use crate::artwork::warm::WarmTargets;
use crate::cache_hold::CacheHold;
use crate::discover::listening::shared;
use crate::metadata::library::LocalArtistSong;

/// Releases whose tracklists top songs look up, at most.
const MAX_TOP_SONG_RELEASES: usize = 10;
/// MusicBrainz requests one artist page makes to find its songs' albums,
/// at most: a search per song, and a lookup of the recording the service
/// names.
const MAX_SONG_LOOKUPS: usize = 20;
/// How long one request spends resolving Last.fm releases.
const LASTFM_RESOLVE_BUDGET: Duration = Duration::from_secs(4);
/// Similar artists consulted for similar albums.
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
    /// Whether ListenBrainz answers the section only with a token.
    fn needs_token(self) -> bool {
        matches!(self, Self::TopSongs | Self::TopAlbums)
    }

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
    /// The source the caller asked for, or the preferred one.
    asked: ListenSource,
    /// Sources that failed, or are switched off, while the section was
    /// built.
    unavailable: Vec<ListenSource>,
    /// Sources that answer the section only for an account the person
    /// has not linked.
    needs_account: Vec<ListenSource>,
}

/// The listening service that stands in for `source`.
fn other_source(source: ListenSource) -> ListenSource {
    match source {
        ListenSource::Listenbrainz => ListenSource::Lastfm,
        ListenSource::Lastfm => ListenSource::Listenbrainz,
    }
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

/// The answer of `source` with `rows`, saying it needs the person's
/// account when it was `refused` for want of one and has nothing.
fn answer_from(source: ListenSource, rows: Option<SectionRows>, refused: &AtomicBool) -> Answer {
    let rows = rows.unwrap_or_default();
    let needs_account = if refused.load(Ordering::Relaxed) && rows.is_empty() {
        vec![source]
    } else {
        Vec::new()
    };
    Answer {
        unavailable: Vec::new(),
        needs_account,
        rows,
        source,
        configured: true,
        asked: source,
    }
}

impl Catalog {
    fn preferred_source(&self, asked: Option<ListenSource>) -> ListenSource {
        asked.unwrap_or_default()
    }

    /// Build one artist section from the preferred source, and from the
    /// other one when the preferred is switched off, not set up or has
    /// nothing. The answer names the source asked for and the ones that
    /// failed or are switched off, so the page can say why it shows another
    /// service.
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
        let mut first = self
            .section_from(source, user_id, &mbid, count, section, priority)
            .await;
        // A service switched off cannot be asked either.
        if !first.configured {
            first.unavailable.push(source);
        }
        if first.rows.is_empty() {
            let mut fallback = self
                .section_from(
                    other_source(source),
                    user_id,
                    &mbid,
                    count,
                    section,
                    priority,
                )
                .await;
            let mut unavailable = std::mem::take(&mut first.unavailable);
            unavailable.append(&mut fallback.unavailable);
            let mut needs_account = std::mem::take(&mut first.needs_account);
            needs_account.append(&mut fallback.needs_account);
            if fallback.configured && !fallback.rows.is_empty() {
                fallback.asked = source;
                fallback.unavailable = unavailable;
                fallback.needs_account = needs_account;
                return Ok(fallback);
            }
            first.unavailable = unavailable;
            first.needs_account = needs_account;
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
            // `titles2`: rows matched since titles fold accents and
            // punctuation; rows matched before would keep their gaps.
            (ListenSource::Lastfm, Section::TopAlbums) => {
                let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
                CacheKey::new(
                    Source::LastFm,
                    &[&namespace, section.key_part(), "titles2", mbid, &count_part],
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
            asked: source,
            unavailable: Vec::new(),
            needs_account: Vec::new(),
        };
        // Set when the read was not sent for want of the person's account.
        let refused = Arc::new(AtomicBool::new(false));
        let rows = match source {
            ListenSource::Listenbrainz => {
                let Some(client) = self.upstream().listenbrainz() else {
                    return unconfigured;
                };
                let lender = self.upstream().lender(user_id);
                let cache = self.upstream().cache();
                if section.needs_token()
                    && !cache.fresh(&key).await
                    && !shared::has_token(&lender.credentials().await)
                {
                    // Not sent without the person's account: what is held
                    // answers, and no fetch is started or joined.
                    refused.store(true, Ordering::Relaxed);
                    return answer_from(source, cache.peek(&key).await.flatten(), &refused);
                }
                let catalog = self.clone();
                let mbid = mbid.to_owned();
                let refused = Arc::clone(&refused);
                self.cached(
                    &key,
                    Policy::LISTENBRAINZ_ARTIST,
                    priority,
                    move |priority| async move {
                        let creds = lender.credentials().await;
                        let mut hold = CacheHold::default();
                        let rows = catalog
                            .listenbrainz_rows(
                                &client,
                                &creds,
                                (&mbid, count, section, priority),
                                &mut hold,
                            )
                            .await;
                        match rows {
                            // Nothing was sent: the person has no account
                            // for this read. What is held answers; nothing
                            // is kept, and it is no failure.
                            Err(ProviderError::NotConfigured { .. }) => {
                                refused.store(true, Ordering::Relaxed);
                                Ok(Fetched::Partial(SectionRows::default()))
                            }
                            rows => section_answer(rows.ok(), hold),
                        }
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
        match rows {
            Ok(rows) => answer_from(source, rows, &refused),
            Err(_) => Answer {
                unavailable: vec![source],
                ..answer_from(source, None, &refused)
            },
        }
    }

    /// Whether the artist's similar artists, top songs and top albums, at
    /// these counts, are all held, fresh or stale, for a user who can
    /// (`lastfm`) or cannot read Last.fm, in the order the user follows
    /// (`lastfm_first`). A section the first source has nothing for counts
    /// once the other's answer, which then fills in, is held too.
    pub(super) async fn artist_sections_held(
        &self,
        lastfm: bool,
        lastfm_first: bool,
        mbid: &str,
        (similar, songs, albums): (u32, u32, u32),
    ) -> bool {
        let listenbrainz = self.upstream().listenbrainz().is_some();
        let order = if lastfm_first {
            [ListenSource::Lastfm, ListenSource::Listenbrainz]
        } else {
            [ListenSource::Listenbrainz, ListenSource::Lastfm]
        };
        for (section, count) in [
            (Section::Similar, similar),
            (Section::TopSongs, songs),
            (Section::TopAlbums, albums),
        ] {
            for source in order {
                let readable = match source {
                    ListenSource::Listenbrainz => listenbrainz,
                    ListenSource::Lastfm => lastfm,
                };
                if !readable {
                    continue;
                }
                let key = self.section_key(source, mbid, count, section);
                match self.upstream().cache().peek::<SectionRows>(&key).await {
                    None => return false,
                    Some(Some(_)) => break,
                    Some(None) => {}
                }
            }
        }
        true
    }

    /// Whether `user_id` can read Last.fm (their own key or the
    /// instance's). Decrypts the user's key, so callers ask once.
    pub(super) async fn lastfm_readable(&self, user_id: &str) -> bool {
        self.upstream().lastfm(user_id).await.is_some()
    }

    /// ListenBrainz rows for one section, or the failure of its read. A
    /// secondary read that failed is noted in `hold`.
    async fn listenbrainz_rows(
        &self,
        client: &super::upstream::CatalogListenBrainz,
        creds: &listenbrainz::ListenBrainzCredentials,
        (mbid, count, section, priority): (&str, u32, Section, RequestPriority),
        hold: &mut CacheHold,
    ) -> Result<SectionRows, ProviderError> {
        let count = count as usize;
        match section {
            Section::Similar => {
                let similar = client.similar_artists(mbid, count, creds, priority).await?;
                Ok(SectionRows {
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
                    .await?;
                let ids: Vec<String> = recordings
                    .iter()
                    .filter_map(|recording| recording.recording_mbid.clone())
                    .collect();
                let groups = client
                    .cached_recording_groups(self.upstream().cache(), &ids, creds, priority)
                    .await
                    .unwrap_or_else(|error| {
                        hold.provider(&error);
                        HashMap::new()
                    });
                Ok(SectionRows {
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
                            listen_count: Some(recording.listen_count),
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
                    // Without an account the recordings are refused too.
                    Err(error @ ProviderError::NotConfigured { .. }) => return Err(error),
                    // No popularity rows, or the popularity read failed: the
                    // list is rebuilt from the top recordings instead.
                    other => {
                        if let Err(error) = &other {
                            hold.provider(error);
                        }
                        return Ok(SectionRows {
                            albums: albums_from_recordings(
                                self.upstream().cache(),
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
                Ok(SectionRows {
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
            asked_source: answer.asked,
            unavailable_sources: answer.unavailable,
            needs_account: answer.needs_account,
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
            asked_source: answer.asked,
            unavailable_sources: answer.unavailable,
            needs_account: answer.needs_account,
        })
    }

    /// The artist page's top songs: [`Self::top_songs`] at user priority,
    /// with an album for songs that name none, so they show a cover, and
    /// songs the library holds pointed at the user's copy, so they play.
    /// Only the page looks songs' albums up; radio and the precache do not.
    pub async fn artist_page_top_songs(
        &self,
        user_id: &str,
        mbid: &str,
        count: u32,
        source: Option<ListenSource>,
    ) -> Result<TopSongsResponse, CatalogError> {
        let artist = checked_mbid(mbid, "artist")?;
        let mut answer = self
            .top_songs(
                user_id,
                &artist,
                count,
                source,
                RequestPriority::UserInitiated,
            )
            .await?;
        self.fill_song_groups(&artist, &mut answer.songs).await;
        self.prefer_owned_songs(&artist, &mut answer.songs).await;
        Ok(answer)
    }

    /// Disc and track numbers for top songs. Numbers come from the cached
    /// tracklist of the release the listens point at, so songs sharing a
    /// release share one lookup and album pages share the cache. The request
    /// only reads the cache; releases not cached yet are fetched once in the
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

    /// Release groups for top songs that name none, as Last.fm's do, so
    /// they get a cover. Read from the cache, a stale answer included; a
    /// song with no fresh answer is looked up again in the background, at
    /// most [`MAX_SONG_LOOKUPS`] requests a page, so the page answers at
    /// once and carries the covers from the next load on.
    async fn fill_song_groups(&self, artist: &str, songs: &mut [TopSong]) {
        let cache = self.upstream().cache();
        let mut missing: Vec<(CacheKey, SongLookup)> = Vec::new();
        for song in songs
            .iter_mut()
            .filter(|song| song.release_group_mbid.is_none())
        {
            let lookup = SongLookup {
                artist: artist.to_owned(),
                title: song.title.clone(),
                recording: song
                    .recording_mbid
                    .as_deref()
                    .filter(|id| is_mbid(id))
                    .map(str::to_ascii_lowercase),
            };
            let key = self.song_group_key(&lookup);
            if let Some(group) = cache.peek::<String>(&key).await {
                song.release_group_mbid = group;
            }
            if !cache.fresh(&key).await {
                missing.push((key, lookup));
            }
        }
        let mut requests = 0;
        for (key, lookup) in missing {
            requests += lookup.requests();
            if requests > MAX_SONG_LOOKUPS {
                break;
            }
            self.warm_song_group(key, lookup);
        }
    }

    /// Songs the library holds show with the user's own copy: its album
    /// and its place there, so the row shows that cover and plays the
    /// file. A library that cannot be read changes nothing.
    async fn prefer_owned_songs(&self, artist: &str, songs: &mut [TopSong]) {
        match self.local().artist_songs(artist).await {
            Ok(owned) => own_copies(songs, &owned),
            Err(error) => {
                tracing::warn!(%error, "library songs failed; showing the songs as listed");
            }
        }
    }

    /// The cache key of a song's release group: the MusicBrainz server,
    /// the artist, the recording the service names and the folded title,
    /// each of which changes the answer.
    fn song_group_key(&self, song: &SongLookup) -> CacheKey {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        CacheKey::hashed(
            Source::MusicBrainz,
            "song_to_rg",
            &[
                &namespace,
                &song.artist,
                song.recording.as_deref().unwrap_or(""),
                &title_key(&song.title),
            ],
        )
    }

    /// Look a song's release group up into the cache at background
    /// priority, unless it is being looked up already: the recording the
    /// service names, and a search for the title among the artist's
    /// recordings (the named recording may be unknown to MusicBrainz, or a
    /// late re-recording).
    fn warm_song_group(&self, key: CacheKey, song: SongLookup) {
        let catalog = self.clone();
        self.warm_once(format!("song:{}", key.as_str()), async move {
            let upstream = catalog.clone();
            let title = song.title.clone();
            let looked_up = catalog
                .cached(
                    &key,
                    Policy::SONG_TO_GROUP,
                    RequestPriority::BackgroundSync,
                    move |priority| async move {
                        let (client, _) = upstream.upstream().musicbrainz(priority);
                        let named = match &song.recording {
                            Some(recording) => client
                                .lookup_recording(
                                    recording,
                                    &["releases", "release-groups"],
                                    Criticality::IdentityCritical,
                                )
                                .await
                                .map_err(mb_error)?
                                .map(|found| found.entity.releases)
                                .unwrap_or_default(),
                            None => Vec::new(),
                        };
                        // Words, not a phrase: titles differ in punctuation
                        // ("été, L'Après-midi" against "été : L’Après‐midi");
                        // the folded title comparison then keeps the same
                        // song only. Folded words hold no Lucene syntax. A
                        // title with no words is not searched.
                        let words = title_key(&song.title);
                        let hits = if words.is_empty() {
                            Vec::new()
                        } else {
                            let query = format!("recording:({words}) AND arid:{}", song.artist);
                            client
                                .search_recordings_query(
                                    &query,
                                    SONG_SEARCH_LIMIT,
                                    Criticality::IdentityCritical,
                                )
                                .await
                                .map_err(mb_search_error)?
                                .items
                        };
                        Ok(song_group(&named, &song.title, &hits)
                            .map_or(Fetched::Missing, Fetched::Found))
                    },
                )
                .await;
            if let Err(error) = looked_up {
                tracing::warn!(song = %title, %error, "song release group warm failed");
            }
        });
    }

    /// Fetch release tracklists into the cache at background priority, one
    /// task per release not already being fetched.
    fn warm_releases(&self, releases: Vec<String>) {
        for release in releases {
            let catalog = self.clone();
            self.warm_once(format!("release:{release}"), async move {
                if let Err(error) = catalog
                    .release_detail_at(&release, RequestPriority::BackgroundSync)
                    .await
                {
                    tracing::warn!(release = %release, %error, "release tracklist warm failed");
                }
            });
        }
    }

    /// Run `work` in the background unless a warm under `key` is running.
    /// Each warm logs its own failure; a panic is logged here, and the key
    /// is let go either way so a later page can warm it again.
    fn warm_once(&self, key: String, work: impl Future<Output = ()> + Send + 'static) {
        {
            let mut warming = self
                .inner
                .warming
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if !warming.insert(key.clone()) {
                return;
            }
        }
        let catalog = self.clone();
        tokio::spawn(async move {
            if AssertUnwindSafe(work).catch_unwind().await.is_err() {
                tracing::error!(warm = %key, "a catalog warm panicked");
            }
            catalog
                .inner
                .warming
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&key);
        });
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
            asked_source: answer.asked,
            unavailable_sources: answer.unavailable,
            needs_account: answer.needs_account,
        })
    }

    /// `GET /albums/{album_id}/similar`: top albums of artists similar to
    /// this album's artist, from the service the person prefers, then from
    /// the other one when the first is switched off, not set up or has
    /// nothing. The answer names the service that answered, and is
    /// unconfigured only when neither can be asked.
    pub async fn similar_albums(
        &self,
        user_id: &str,
        raw_album: &str,
        raw_artist: &str,
        count: u32,
    ) -> Result<SimilarAlbumsResponse, CatalogError> {
        let album = checked_mbid(raw_album, "album")?;
        let artist = checked_mbid(raw_artist, "artist")?;
        let preferred = self.upstream().preferred_source(user_id).await;
        let mut answered = None;
        for source in [preferred, other_source(preferred)] {
            let found = match source {
                ListenSource::Listenbrainz => {
                    self.similar_albums_listenbrainz(user_id, &album, &artist, count)
                        .await
                }
                ListenSource::Lastfm => {
                    self.similar_albums_from_lastfm(user_id, &album, &artist, count)
                        .await
                }
            };
            let Some(mut albums) = found else {
                continue;
            };
            if !albums.is_empty() {
                self.flag_discovery_albums(&mut albums).await;
                return Ok(SimilarAlbumsResponse {
                    albums,
                    source,
                    configured: true,
                });
            }
            answered.get_or_insert(source);
        }
        Ok(SimilarAlbumsResponse {
            albums: Vec::new(),
            // Neither can be asked only while ListenBrainz is off.
            source: answered.unwrap_or(ListenSource::Listenbrainz),
            configured: answered.is_some(),
        })
    }

    /// ListenBrainz's similar albums: its similar artists and their most
    /// played albums. `None` while ListenBrainz reads are off.
    async fn similar_albums_listenbrainz(
        &self,
        user_id: &str,
        album: &str,
        artist: &str,
        count: u32,
    ) -> Option<Vec<AlbumRef>> {
        let client = self.upstream().listenbrainz()?;
        let lender = self.upstream().lender(user_id);
        let key = CacheKey::new(
            Source::ListenBrainz,
            &["similar_albums", album, artist, &count.to_string()],
        );
        let (lb_album, lb_artist) = (album.to_owned(), artist.to_owned());
        let shared_rows = self.upstream().cache().clone();
        // Album pages and radio wait on this.
        let priority = RequestPriority::UserInitiated;
        let rows = self
            .cached(
                &key,
                Policy::LISTENBRAINZ_ARTIST,
                priority,
                move |priority| async move {
                    let similar = shared::similar_artists(
                        &shared_rows,
                        &client,
                        &lb_artist,
                        SIMILAR_ALBUM_ARTISTS,
                        &lender,
                        priority,
                    )
                    .await
                    .map_err(CatalogError::Unavailable)?;
                    let similar: Vec<(String, String)> = similar
                        .into_iter()
                        .filter_map(|row| Some((row.artist.mbid?, row.artist.name)))
                        .filter(|(mbid, _)| is_mbid(mbid))
                        .take(SIMILAR_ALBUM_ARTISTS)
                        .collect();
                    let mut albums = Vec::new();
                    let mut hold = CacheHold::default();
                    for (similar_mbid, similar_name) in &similar {
                        let groups = match shared::artist_albums(
                            &shared_rows,
                            &client,
                            similar_mbid,
                            ALBUMS_PER_SIMILAR_ARTIST,
                            &lender,
                            priority,
                        )
                        .await
                        {
                            Ok(groups) => groups,
                            // Not sent without the person's account: what is
                            // held is shown and nothing is kept for others.
                            Err(ProviderError::NotConfigured { .. }) => {
                                hold.local();
                                continue;
                            }
                            Err(error) => {
                                hold.provider(&error);
                                continue;
                            }
                        };
                        for group in groups.into_iter().map(|row| row.album) {
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
                                ..AlbumRef::titled(group.title)
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
        Some(rows.ok().flatten().unwrap_or_default().albums)
    }

    /// Last.fm's similar albums: the artist's similar artists, as the
    /// artist page reads them, and their top albums. `None` while Last.fm
    /// is switched off or no key can read it.
    async fn similar_albums_from_lastfm(
        &self,
        user_id: &str,
        album: &str,
        artist: &str,
        count: u32,
    ) -> Option<Vec<AlbumRef>> {
        let answer = self
            .section_from(
                ListenSource::Lastfm,
                user_id,
                artist,
                SIMILAR_ALBUM_ARTISTS as u32,
                Section::Similar,
                RequestPriority::UserInitiated,
            )
            .await;
        if !answer.configured {
            return None;
        }
        let similar: Vec<(String, String)> = answer
            .rows
            .similar
            .into_iter()
            .filter_map(|row| Some((row.musicbrainz_id?, row.name)))
            .filter(|(mbid, _)| is_mbid(mbid))
            .take(SIMILAR_ALBUM_ARTISTS)
            .collect();
        if similar.is_empty() {
            return Some(Vec::new());
        }
        Some(
            self.similar_albums_lastfm(user_id, album, artist, &similar, count)
                .await,
        )
    }

    /// Last.fm top albums of the similar artists, resolved to release
    /// groups. Last.fm names releases, so each album resolves through
    /// MusicBrainz once and the answer is cached a week; resolution runs
    /// under a short deadline; a partial list is shown but not kept, and
    /// the next visit fills in from the resolutions already made.
    async fn similar_albums_lastfm(
        &self,
        user_id: &str,
        album: &str,
        artist: &str,
        similar: &[(String, String)],
        count: u32,
    ) -> Vec<AlbumRef> {
        let Some((client, creds)) = self.upstream().lastfm(user_id).await else {
            return Vec::new();
        };
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        // The similar artists are Last.fm's for `artist`, so the key names
        // both; rows keyed by the album alone were built from ListenBrainz's.
        let key = CacheKey::new(
            Source::LastFm,
            &[
                &namespace,
                "similar_albums_of",
                album,
                artist,
                &count.to_string(),
            ],
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
    /// groups, in MusicBrainz order. A dead MusicBrainz reads as empty, which
    /// keeps the album page up.
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

/// Rebuild an artist's top albums from their most played recordings: listens
/// add up per release group (or per release title when the group is
/// unknown), most played first. The failure of the recordings read when
/// ListenBrainz failed; a failed release group lookup is noted in `hold`.
async fn albums_from_recordings(
    cache: &droppedneedle_providers::ProviderCacheStore,
    client: &super::upstream::CatalogListenBrainz,
    creds: &listenbrainz::ListenBrainzCredentials,
    (mbid, count, priority): (&str, usize, RequestPriority),
    hold: &mut CacheHold,
) -> Result<Vec<AlbumRef>, ProviderError> {
    let recordings = client
        .artist_top_recordings(mbid, (count * 8).max(80), creds, priority)
        .await?;
    let ids: Vec<String> = recordings
        .iter()
        .filter_map(|recording| recording.recording_mbid.clone())
        .collect();
    let groups = client
        .cached_recording_groups(cache, &ids, creds, priority)
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
    Ok(albums)
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

/// A title folded for matching across services: accents and case folded,
/// and every run of punctuation one space, so Last.fm's `d'Amelie` meets
/// MusicBrainz's `d’Amélie`.
fn title_key(title: &str) -> String {
    fold_text(title)
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// How well a release group fits a title shared by several: a plain album
/// first, then any album, an EP, a single, anything else.
fn type_rank(group: &ReleaseGroupItem) -> u8 {
    match group
        .primary_type
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("album") if group.secondary_types.is_empty() => 0,
        Some("album") => 1,
        Some("ep") => 2,
        Some("single") => 3,
        _ => 4,
    }
}

/// One top song to find a release group for.
#[derive(Debug, Clone)]
struct SongLookup {
    /// Artist MBID, lowercased.
    artist: String,
    /// Song title as the listening service names it.
    title: String,
    /// Recording MBID the service names, when it is one.
    recording: Option<String>,
}

impl SongLookup {
    /// MusicBrainz requests the lookup makes: the recording lookup, and
    /// the search when the title has words to search for.
    fn requests(&self) -> usize {
        usize::from(self.recording.is_some()) + usize::from(!title_key(&self.title).is_empty())
    }
}

/// Point each song the library holds at its own copy. A copy counts only
/// on one of the artist's own albums or on the album the song is shown
/// with, never by its title alone: another artist's album may hold a cover
/// of the song. Among those, a copy matches by the song's recording or its
/// folded title; the copy on the shown album stays, then the copy of the
/// recording, then the first (the artist's own oldest album).
fn own_copies(songs: &mut [TopSong], owned: &[LocalArtistSong]) {
    for song in songs {
        let wanted = title_key(&song.title);
        let recording = song.recording_mbid.as_deref();
        let shown = song.release_group_mbid.as_deref();
        let on_shown = |copy: &LocalArtistSong| {
            shown.is_some_and(|group| copy.release_group_mbid.eq_ignore_ascii_case(group))
        };
        let same_recording = |copy: &LocalArtistSong| {
            recording.is_some_and(|id| {
                copy.recording_mbid
                    .as_deref()
                    .is_some_and(|own| own.eq_ignore_ascii_case(id))
            })
        };
        let copies: Vec<&LocalArtistSong> = owned
            .iter()
            .filter(|copy| copy.own_album || on_shown(copy))
            .filter(|copy| same_recording(copy) || title_key(&copy.title) == wanted)
            .collect();
        let Some(copy) = copies
            .iter()
            .find(|copy| on_shown(copy))
            .or_else(|| copies.iter().find(|copy| same_recording(copy)))
            .or_else(|| copies.first())
        else {
            continue;
        };
        song.release_group_mbid = Some(copy.release_group_mbid.clone());
        song.release_name = Some(copy.album_title.clone());
        song.disc_number = Some(copy.disc_number);
        song.track_number = Some(copy.track_number);
    }
}

/// Recordings a song title search reads.
const SONG_SEARCH_LIMIT: u32 = 25;

/// The release group a song is best shown with: official releases first,
/// then where it first came out (an album, a soundtrack included, then an
/// EP, a single; compilations, live and remix albums last), then the
/// earliest. A later album that is a compilation in all but name still
/// loses to the original on date. `None` when no candidate names a group.
fn best_group<'a>(
    candidates: impl Iterator<Item = (bool, &'a ReleaseGroupRef, Option<&'a str>)>,
) -> Option<String> {
    candidates
        .map(|(official, group, release_date)| {
            let gathered = group.secondary_types.iter().any(|kind| {
                ["compilation", "live", "dj-mix", "remix", "mixtape/street"]
                    .contains(&kind.to_ascii_lowercase().as_str())
            });
            let rank = match group
                .primary_type
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                _ if gathered => 3,
                Some("album") => 0,
                Some("ep") => 1,
                Some("single") => 2,
                _ => 3,
            };
            let date = group
                .first_release_date
                .as_deref()
                .or(release_date)
                .filter(|date| !date.is_empty())
                .unwrap_or("9999")
                .to_owned();
            ((!official, rank, date), group.id.to_ascii_lowercase())
        })
        .min()
        .map(|(_, group)| group)
}

/// The best release group for a song among the releases of the recording
/// the service named and the search hits with the song's title (folded).
fn song_group(
    named: &[RecordingRelease],
    title: &str,
    hits: &[RecordingSearchHit],
) -> Option<String> {
    let wanted = title_key(title);
    let official =
        |status: Option<&str>| status.is_some_and(|status| status.eq_ignore_ascii_case("official"));
    let from_named = named.iter().filter_map(|release| {
        Some((
            official(release.status.as_deref()),
            release.release_group.as_ref()?,
            release.date.as_deref(),
        ))
    });
    let from_search = hits
        .iter()
        .filter(|hit| {
            hit.title
                .as_deref()
                .is_some_and(|found| title_key(found) == wanted)
        })
        .flat_map(|hit| {
            hit.releases.iter().filter_map(|release| {
                Some((
                    official(release.status.as_deref()),
                    release.release_group.as_ref()?,
                    release.date.as_deref(),
                ))
            })
        });
    best_group(from_named.chain(from_search))
}

/// Match Last.fm's top albums to the artist's release groups: a Last.fm id
/// that already is one of the artist's release groups wins, else a title
/// match. Titles compare folded (accents, case, punctuation), also without
/// a trailing bracket like `(Bande originale du film)` or
/// `[Deluxe Edition]`; a title several groups share goes to the
/// best-fitting type, and a tie of the same type stays unlinked. One row
/// per group.
fn match_lastfm_albums(
    top: Vec<lastfm::TopItem>,
    groups: Option<&ReleaseGroupList>,
) -> Vec<AlbumRef> {
    let mut ids: HashSet<String> = HashSet::new();
    let mut by_title: HashMap<String, (u8, Option<String>)> = HashMap::new();
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
        let rank = type_rank(group);
        by_title
            .entry(title)
            .and_modify(|(best, known)| {
                if rank < *best {
                    (*best, *known) = (rank, Some(id.clone()));
                } else if rank == *best && known.as_deref() != Some(id.as_str()) {
                    *known = None;
                }
            })
            .or_insert((rank, Some(id)));
    }
    let mut seen = HashSet::new();
    let mut albums = Vec::new();
    for item in top {
        let raw = item.mbid.as_deref().map(str::to_ascii_lowercase);
        let by_name = title_candidates(&item.name)
            .into_iter()
            .find_map(|key| by_title.get(&key).map(|(_, id)| id.clone()))
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
            listen_count: item.playcount,
            ..AlbumRef::titled(item.name)
        });
    }
    albums
}

/// The folded title and, when it ends in a bracket, the title without it.
fn title_candidates(title: &str) -> Vec<String> {
    let key = title_key(title);
    if key.is_empty() {
        return Vec::new();
    }
    let mut candidates = vec![key];
    let trimmed = title.trim_end();
    if let Some(open) = match trimmed.chars().last() {
        Some(')') => trimmed.rfind('('),
        Some(']') => trimmed.rfind('['),
        _ => None,
    } {
        let stripped = title_key(&trimmed[..open]);
        if !stripped.is_empty() {
            candidates.push(stripped);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: &str, title: &str, primary: &str, secondary: &[&str]) -> ReleaseGroupItem {
        ReleaseGroupItem {
            id: id.to_owned(),
            title: Some(title.to_owned()),
            primary_type: Some(primary.to_owned()),
            secondary_types: secondary.iter().map(|value| (*value).to_owned()).collect(),
            first_release_date: None,
            artist_name: None,
        }
    }

    fn top(name: &str) -> lastfm::TopItem {
        lastfm::TopItem {
            name: name.to_owned(),
            artist_name: "Yann Tiersen".to_owned(),
            mbid: None,
            playcount: Some(1),
            image_url: String::new(),
        }
    }

    /// Yann Tiersen's Last.fm top albums against his discography
    /// (MusicBrainz, 2026-10-31): before, 2 of 10 rows found a release
    /// group, so 8 showed no cover.
    #[test]
    fn last_fm_titles_find_their_release_groups() {
        let groups = ReleaseGroupList {
            items: vec![
                group(
                    "amelie",
                    "Le Fabuleux Destin d’Amélie Poulain",
                    "Album",
                    &["Soundtrack"],
                ),
                group("cascades-ep", "Rue des cascades", "EP", &[]),
                group("cascades", "Rue des cascades", "Album", &[]),
                group("lenin", "Good Bye Lenin!", "Album", &["Soundtrack"]),
                group("lenin-ep", "Good Bye Lenin!", "EP", &[]),
                group("twin-a", "Twin", "Single", &[]),
                group("twin-b", "Twin", "Single", &[]),
            ],
            complete: true,
            total: 7,
        };
        let albums = match_lastfm_albums(
            vec![
                top("Le Fabuleux Destin d'Amelie Poulain"),
                top("Le fabuleux destin d'Amélie Poulain (Bande originale du film)"),
                top("Rue Des Cascades"),
                top("Good Bye Lenin!"),
                top("Twin"),
                top("Amelie From Montmartre"),
            ],
            Some(&groups),
        );
        let found: Vec<(&str, Option<&str>)> = albums
            .iter()
            .map(|album| (album.title.as_str(), album.musicbrainz_id.as_deref()))
            .collect();
        assert_eq!(
            found,
            [
                ("Le Fabuleux Destin d'Amelie Poulain", Some("amelie")),
                ("Rue Des Cascades", Some("cascades")),
                ("Good Bye Lenin!", Some("lenin")),
                ("Twin", None),
                ("Amelie From Montmartre", None),
            ],
            "accents and apostrophes fold, a trailing bracket drops (its row is the \
             same group, so it goes), a shared title takes the album, a true tie and \
             an unknown title stay unlinked"
        );
    }

    /// A song the library holds points at the user's copy, so the row
    /// plays it, but only a copy on the artist's own album or on the album
    /// the song is shown with: "Sur le fil" shown with "Le Phare" and owned
    /// on the "Amélie" soundtrack goes there by its recording; "La Dispute",
    /// owned on both, stays with "Le Phare"; "La Noyée" on a pianist's album
    /// is a cover, not the user's copy, unless the song is shown with it.
    #[test]
    fn a_song_the_library_holds_shows_with_its_own_copy() {
        let song = |title: &str, recording: Option<&str>, group: &str| TopSong {
            title: title.to_owned(),
            artist_name: "Yann Tiersen".to_owned(),
            recording_mbid: recording.map(str::to_owned),
            release_group_mbid: Some(group.to_owned()),
            original_release_mbid: None,
            release_name: None,
            listen_count: Some(1),
            disc_number: None,
            track_number: None,
        };
        let copy =
            |title: &str, recording: Option<&str>, group: &str, track: u32| LocalArtistSong {
                title: title.to_owned(),
                recording_mbid: recording.map(str::to_owned),
                release_group_mbid: group.to_owned(),
                album_title: group.to_owned(),
                disc_number: 1,
                track_number: track,
                own_album: group != "piano",
            };
        let mut songs = vec![
            song("Sur le fil", Some("REC-SUR"), "phare"),
            song("La Dispute", None, "phare"),
            song("La Noyée", None, "phare"),
            song("La Noyée", None, "piano"),
        ];
        let owned = [
            copy("La Noyee", None, "piano", 7),
            copy("La Dispute", None, "amelie", 5),
            copy("Le fil", Some("rec-sur"), "amelie", 3),
            copy("La dispute", None, "phare", 2),
        ];
        own_copies(&mut songs, &owned);
        let placed: Vec<(&str, Option<&str>, Option<u32>)> = songs
            .iter()
            .map(|song| {
                (
                    song.title.as_str(),
                    song.release_group_mbid.as_deref(),
                    song.track_number,
                )
            })
            .collect();
        assert_eq!(
            placed,
            [
                ("Sur le fil", Some("amelie"), Some(3)),
                ("La Dispute", Some("phare"), Some(2)),
                ("La Noyée", Some("phare"), None),
                ("La Noyée", Some("piano"), Some(7)),
            ]
        );
    }
}
