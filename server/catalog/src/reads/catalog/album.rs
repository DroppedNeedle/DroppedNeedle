//! Album pages: header, tracklist, editions, artwork, Last.fm and store
//! links.

use serde::{Deserialize, Serialize};

use droppedneedle_providers::degradation::scoped;
use droppedneedle_providers::musicbrainz::{Criticality, MbRelease, MbReleaseGroup};
use droppedneedle_providers::{CacheKey, Fetched, Policy, ProviderError, RequestPriority, Source};

use super::artist::checked_mbid;
use super::error::CatalogError;
use super::library::{LocalAlbum, LocalTrack};
use super::mapping;
use super::models::{
    AlbumBasicInfo, AlbumEditionItem, AlbumEditionsResponse, AlbumImages, AlbumInfo, AlbumTrack,
    AlbumTracksInfo, CatalogSource, LastFmAlbumEnrichment, LastFmTag, PluginDetails, PurchaseKind,
    PurchaseLink, PurchaseOptionsResponse,
};
use super::ports::PurchaseQuery;
use super::{
    Answer, Catalog, LASTFM_DETAILS, PURCHASE, is_mbid, mb_error, mb_search_error, record_mb_down,
};

/// Includes for the release-group lookup: one call serves the header, the
/// edition list (with track counts) and the group's store links.
const GROUP_INCLUDES: [&str; 4] = ["artist-credits", "media", "releases", "url-rels"];
/// Includes for one release: tracklist, label, and its store links.
const RELEASE_INCLUDES: [&str; 3] = ["labels", "recordings", "url-rels"];
/// Releases consulted for store links (v2 `_MAX_RELEASE_LOOKUPS`).
const MAX_PURCHASE_LOOKUPS: usize = 2;
/// Ranked editions tried when the chosen one has no tracklist.
const TRACKLIST_CANDIDATES: usize = 3;

/// The cached projection of one release-group lookup. `None` is a miss.
pub type GroupCore = Option<GroupDetail>;

/// What album pages keep from a release group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupDetail {
    /// Canonical release-group MBID.
    pub mbid: String,
    /// Title.
    pub title: String,
    /// First credited artist.
    pub artist_name: String,
    /// That artist's MBID.
    pub artist_id: String,
    /// The whole credit as printed, for store searches.
    pub credit: String,
    /// First release date.
    pub first_release_date: Option<String>,
    /// Primary type.
    pub primary_type: Option<String>,
    /// Disambiguation.
    pub disambiguation: Option<String>,
    /// Every release (edition).
    pub releases: Vec<ReleaseSummary>,
    /// Store links on the group itself.
    pub store_links: Vec<PurchaseLink>,
}

/// One edition as the group lookup lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseSummary {
    /// Release MBID.
    pub id: String,
    /// Title.
    pub title: Option<String>,
    /// Disambiguation.
    pub disambiguation: Option<String>,
    /// Date.
    pub date: Option<String>,
    /// Country.
    pub country: Option<String>,
    /// Packaging.
    pub packaging: Option<String>,
    /// Status.
    pub status: Option<String>,
    /// Tracks across all media.
    pub track_count: u32,
    /// Media formats, in disc order, without repeats.
    #[serde(default)]
    pub formats: Vec<String>,
    /// Rank among the group's releases, best first.
    pub rank: usize,
}

impl GroupDetail {
    fn from_wire(group: MbReleaseGroup) -> Self {
        let ranked: Vec<String> = mapping::ranked_releases(&group.releases)
            .into_iter()
            .map(|release| release.id.clone())
            .collect();
        let releases = group
            .releases
            .iter()
            .map(|release| ReleaseSummary {
                id: release.id.clone(),
                title: release.title.clone(),
                disambiguation: release
                    .disambiguation
                    .clone()
                    .filter(|text| !text.is_empty()),
                date: release.date.clone().filter(|text| !text.is_empty()),
                country: release.country.clone(),
                packaging: release.packaging.clone(),
                status: release.status.clone(),
                track_count: mapping::media_track_count(release),
                formats: release
                    .media
                    .iter()
                    .fold(Vec::new(), |mut formats, medium| {
                        if let Some(format) = medium.format.clone()
                            && !formats.contains(&format)
                        {
                            formats.push(format);
                        }
                        formats
                    }),
                rank: ranked
                    .iter()
                    .position(|id| *id == release.id)
                    .unwrap_or(usize::MAX),
            })
            .collect();
        let mut store_links = Vec::new();
        collect_links(&group.relations, &mut store_links);
        let (artist_name, artist_id) = mapping::first_credit(&group.artist_credit);
        Self {
            title: group
                .title
                .clone()
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| "Unknown Album".to_owned()),
            credit: mapping::full_credit(&group.artist_credit),
            artist_name,
            artist_id,
            first_release_date: group
                .first_release_date
                .clone()
                .filter(|date| !date.is_empty()),
            primary_type: group.primary_type.clone(),
            disambiguation: group.disambiguation.clone().filter(|text| !text.is_empty()),
            releases,
            store_links,
            mbid: group.id,
        }
    }

    /// Releases in rank order.
    fn ranked(&self) -> Vec<&ReleaseSummary> {
        let mut ranked: Vec<&ReleaseSummary> = self
            .releases
            .iter()
            .filter(|release| release.rank != usize::MAX)
            .collect();
        ranked.sort_by_key(|release| release.rank);
        ranked
    }
}

/// The cached projection of one release lookup. `None` is a miss.
pub type ReleaseCore = Option<ReleaseDetail>;

/// What album pages keep from one release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseDetail {
    /// Release MBID.
    pub id: String,
    /// Tracklist.
    pub tracks: Vec<AlbumTrack>,
    /// Total length in milliseconds.
    pub total_length: u64,
    /// First label.
    pub label: Option<String>,
    /// Barcode.
    pub barcode: Option<String>,
    /// Country.
    pub country: Option<String>,
    /// Store links on this release.
    pub store_links: Vec<PurchaseLink>,
}

impl ReleaseDetail {
    fn from_wire(release: MbRelease) -> Self {
        let (tracks, total_length) = mapping::extract_tracks(&release);
        let mut store_links = Vec::new();
        collect_links(&release.relations, &mut store_links);
        Self {
            label: mapping::first_label(&release),
            barcode: release.barcode.clone().filter(|code| !code.is_empty()),
            country: release.country.clone(),
            tracks,
            total_length,
            store_links,
            id: release.id,
        }
    }
}

fn collect_links(
    relations: &[droppedneedle_providers::musicbrainz::Relation],
    into: &mut Vec<PurchaseLink>,
) {
    for relation in relations {
        if let Some(link) = mapping::purchase_link(relation, &mapping::RELEASE_STORE_RELATIONS)
            && !into.iter().any(|known| known.url == link.url)
        {
            into.push(link);
        }
    }
}

/// The edition an album page shows and why.
struct Selection {
    selected: Option<String>,
    basis: Option<&'static str>,
    owned: Option<String>,
    /// The edition a person chose for the library's copy.
    chosen: Option<String>,
}

/// Releases read per page when listing every edition of a group.
const EDITION_PAGE: u32 = 100;
/// Pages read at most when listing every edition (500 editions).
const EDITION_PAGES: u32 = 5;

impl Catalog {
    pub(super) fn group_key(&self, mbid: &str) -> CacheKey {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        CacheKey::new(Source::MusicBrainz, &[&namespace, "rg", mbid])
    }

    fn release_key(&self, mbid: &str) -> CacheKey {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        CacheKey::new(Source::MusicBrainz, &[&namespace, "release", mbid])
    }

    /// Cache key of TheAudioDB's album art. A name match may be another
    /// album's art, so whether the name search is on is in the key.
    fn audiodb_album_key(&self, mbid: &str) -> CacheKey {
        let fallback = self
            .upstream()
            .settings()
            .advanced()
            .audiodb_name_search_fallback;
        CacheKey::new(
            Source::AudioDb,
            &[
                if fallback { "by_name" } else { "by_id" },
                "album",
                &mbid.to_ascii_lowercase(),
            ],
        )
    }

    /// The cached release-group detail. Sources that hand over a release
    /// MBID where a group belongs (v2 issue #78) resolve through the
    /// release before the miss is final.
    pub(super) async fn group_detail(&self, mbid: &str) -> Result<GroupCore, CatalogError> {
        self.group_detail_at(mbid, RequestPriority::UserInitiated)
            .await
    }

    /// The cached release-group detail, fetching at `priority` when none
    /// is fresh.
    pub(super) async fn group_detail_at(
        &self,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<GroupCore, CatalogError> {
        let catalog = self.clone();
        let id = mbid.to_owned();
        self.cached(
            &self.group_key(mbid),
            Policy::MUSICBRAINZ_ENTITY,
            priority,
            move |priority| async move {
                let (client, _) = catalog.upstream().musicbrainz(priority);
                let mut found = client
                    .lookup_release_group(&id, &GROUP_INCLUDES, Criticality::IdentityCritical)
                    .await
                    .map_err(mb_error)?;
                if found.is_none() {
                    let resolved = client
                        .resolve_release_to_release_group(&id, Criticality::IdentityCritical)
                        .await
                        .map_err(mb_error)?;
                    if let Some(group_id) = resolved.filter(|group_id| *group_id != id) {
                        found = client
                            .lookup_release_group(
                                &group_id,
                                &GROUP_INCLUDES,
                                Criticality::IdentityCritical,
                            )
                            .await
                            .map_err(mb_error)?;
                    }
                }
                Ok(match found {
                    None => Fetched::Missing,
                    Some(lookup) => Fetched::Found(GroupDetail::from_wire(lookup.entity)),
                })
            },
        )
        .await
    }

    /// The cached release detail.
    async fn release_detail(&self, mbid: &str) -> Result<ReleaseCore, CatalogError> {
        self.release_detail_at(mbid, RequestPriority::UserInitiated)
            .await
    }

    /// The cached release detail, fetching at `priority` on a miss. A page
    /// never joins a background fetch of the same release: it would wait
    /// behind every user request in the queue.
    pub(super) async fn release_detail_at(
        &self,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<ReleaseCore, CatalogError> {
        let catalog = self.clone();
        let id = mbid.to_owned();
        self.cached(
            &self.release_key(mbid),
            Policy::MUSICBRAINZ_ENTITY,
            priority,
            move |priority| async move {
                let (client, _) = catalog.upstream().musicbrainz(priority);
                let found = client
                    .lookup_release(&id, &RELEASE_INCLUDES, Criticality::IdentityCritical)
                    .await
                    .map_err(mb_error)?;
                Ok(match found {
                    None => Fetched::Missing,
                    Some(lookup) => Fetched::Found(ReleaseDetail::from_wire(lookup.entity)),
                })
            },
        )
        .await
    }

    /// A release detail already held, fresh or stale; never dials out.
    pub(super) async fn cached_release_detail(&self, mbid: &str) -> Option<ReleaseDetail> {
        self.upstream()
            .cache()
            .peek(&self.release_key(mbid))
            .await
            .flatten()
    }

    /// The release-group MBID a route's album id names. A library album
    /// id resolves to the group it is identified as, as in v2; anything
    /// else that is not an MBID is rejected.
    async fn album_id(&self, raw_id: &str) -> Result<String, CatalogError> {
        if let Ok(id) = checked_mbid(raw_id, "album") {
            return Ok(id);
        }
        let local = raw_id.trim();
        if !local.is_empty() && local.len() <= 128 {
            let found = self
                .local()
                .group_for_album(local)
                .await
                .map_err(CatalogError::database)?;
            if let Some(group) = found.filter(|group| is_mbid(group)) {
                return Ok(group.to_ascii_lowercase());
            }
        }
        checked_mbid(raw_id, "album")
    }

    /// `GET /albums/{album_id}/basic`: the header. A dead MusicBrainz falls
    /// back to the library's copy.
    pub async fn album_basic(&self, raw_id: &str) -> Result<AlbumBasicInfo, CatalogError> {
        let id = self.album_id(raw_id).await?;
        let (result, context) = scoped(self.build_basic(&id)).await;
        result.map(|mut basic| {
            basic.service_status = mapping::service_status(&context);
            basic
        })
    }

    async fn build_basic(&self, id: &str) -> Result<AlbumBasicInfo, CatalogError> {
        let answer = self
            .musicbrainz_or_library(self.group_detail(id).await, || self.local_album(id))
            .await?;
        Ok(match answer {
            Answer::MusicBrainz(group) => self.basic_from_group(&group).await,
            Answer::Library(local) => self.basic_from_local(id, &local).await,
        })
    }

    async fn basic_from_group(&self, group: &GroupDetail) -> AlbumBasicInfo {
        let (owned, requested) = self.album_flags(std::slice::from_ref(&group.mbid)).await;
        let key = group.mbid.to_ascii_lowercase();
        let in_library = owned.contains(&key);
        AlbumBasicInfo {
            title: group.title.clone(),
            musicbrainz_id: group.mbid.clone(),
            artist_name: group.artist_name.clone(),
            artist_id: group.artist_id.clone(),
            release_date: group.first_release_date.clone(),
            year: mapping::year_of(group.first_release_date.as_deref()),
            album_type: group.primary_type.clone(),
            disambiguation: group.disambiguation.clone(),
            in_library,
            requested: !in_library && requested.contains(&key),
            cover_url: mapping::release_group_cover_url(&group.mbid),
            album_thumb_url: self
                .cached_album_images(&group.mbid)
                .await
                .and_then(|images| images.album_thumb_url),
            source: CatalogSource::Musicbrainz,
            service_status: None,
        }
    }

    /// The library's copy of an album, read once per page while
    /// MusicBrainz is down.
    async fn local_album(&self, id: &str) -> Result<Option<LocalAlbum>, CatalogError> {
        let found = self
            .local()
            .album(id)
            .await
            .map_err(CatalogError::database)?;
        if found.is_some() {
            tracing::warn!(album = %id, "musicbrainz unavailable; album page from the library");
        }
        Ok(found)
    }

    async fn basic_from_local(&self, id: &str, local: &LocalAlbum) -> AlbumBasicInfo {
        AlbumBasicInfo {
            title: local.title.clone(),
            musicbrainz_id: id.to_owned(),
            artist_name: local.artist_name.clone(),
            artist_id: local.artist_mbid.clone().unwrap_or_default(),
            release_date: None,
            year: local.year,
            album_type: None,
            disambiguation: None,
            in_library: local.owned,
            requested: false,
            cover_url: mapping::release_group_cover_url(id),
            album_thumb_url: self
                .cached_album_images(id)
                .await
                .and_then(|images| images.album_thumb_url),
            source: CatalogSource::Library,
            service_status: None,
        }
    }

    /// Pick the edition: the library copy's edition (a person's choice or
    /// the matcher's best fit for the files; the identity row is the one
    /// record of it), then the release closest to the library's file count,
    /// then the saved edition preferences. The library's edition stands even when the
    /// group lookup does not list it (MusicBrainz lists at most 25).
    async fn select_edition(&self, group: &GroupDetail) -> Result<Selection, CatalogError> {
        let evidence = self
            .local()
            .edition_evidence(&group.mbid)
            .await
            .map_err(CatalogError::database)?;
        let ranked = group.ranked();
        let (selected, basis) = if let Some(chosen) = evidence.chosen_release.clone() {
            (Some(chosen), Some("chosen"))
        } else if let Some(owned) = evidence.owned_release.clone() {
            (Some(owned), Some("owned"))
        } else if let Some(closest) = evidence.file_count.and_then(|files| {
            ranked
                .iter()
                .enumerate()
                .filter(|(_, release)| release.track_count > 0)
                .min_by_key(|(rank, release)| (release.track_count.abs_diff(files), *rank))
                .map(|(_, release)| release.id.clone())
        }) {
            (Some(closest), Some("file_count"))
        } else if let Some(first) = self.preferred(&ranked) {
            (Some(first), Some("preferred"))
        } else {
            (None, None)
        };
        Ok(Selection {
            selected,
            basis,
            owned: evidence.owned_release,
            chosen: evidence.chosen_release,
        })
    }

    /// The edition the saved edition preferences pick, MusicBrainz's rank
    /// breaking ties: what the page shows, and what a request fetches, for
    /// an album the library does not hold.
    fn preferred(&self, ranked: &[&ReleaseSummary]) -> Option<String> {
        // No pick while the preferences do not read: a default pick is
        // not the admin's choice.
        let prefs = self.upstream().settings().edition_preferences()?;
        ranked
            .iter()
            .enumerate()
            .min_by_key(|(rank, release)| {
                let facts = droppedneedle_library::edition_prefs::EditionFacts {
                    status: release.status.as_deref(),
                    formats: release.formats.iter().map(String::as_str).collect(),
                    country: release.country.as_deref(),
                    date: release.date.as_deref(),
                    text: release
                        .title
                        .iter()
                        .chain(release.disambiguation.iter())
                        .map(String::as_str)
                        .collect(),
                    types: Vec::new(),
                };
                (prefs.key(&facts), *rank)
            })
            .map(|(_, release)| release.id.clone())
    }

    async fn tracks_for_group(
        &self,
        group: &GroupDetail,
        priority: RequestPriority,
    ) -> Result<AlbumTracksInfo, CatalogError> {
        let selection = self.select_edition(group).await?;
        let mut candidates: Vec<String> = selection.selected.iter().cloned().collect();
        for release in group.ranked().into_iter().take(TRACKLIST_CANDIDATES) {
            if !candidates.contains(&release.id) {
                candidates.push(release.id.clone());
            }
        }
        for candidate in candidates {
            let Some(release) = self.release_detail_at(&candidate, priority).await? else {
                continue;
            };
            if release.tracks.is_empty() {
                continue;
            }
            let basis = if Some(&candidate) == selection.selected.as_ref() {
                selection.basis
            } else {
                Some("ranked")
            };
            return Ok(AlbumTracksInfo {
                total_tracks: u32::try_from(release.tracks.len()).unwrap_or(u32::MAX),
                total_length: (release.total_length > 0).then_some(release.total_length),
                tracks: release.tracks,
                label: release.label,
                barcode: release.barcode,
                country: release.country,
                selected_release_mbid: Some(candidate),
                pick_basis: basis.map(str::to_owned),
            });
        }
        Ok(AlbumTracksInfo::default())
    }

    /// `GET /albums/{album_id}/tracks`: the shown edition's tracklist. A
    /// dead MusicBrainz falls back to the library's tracks.
    pub async fn album_tracks(&self, raw_id: &str) -> Result<AlbumTracksInfo, CatalogError> {
        let id = self.album_id(raw_id).await?;
        let answer = self
            .musicbrainz_or_library(self.group_detail(&id).await, || self.local_album(&id))
            .await?;
        match answer {
            Answer::MusicBrainz(group) => {
                self.tracks_for_group(&group, RequestPriority::UserInitiated)
                    .await
            }
            Answer::Library(local) => Ok(local_tracks(local.tracks)),
        }
    }

    /// `GET /albums/{album_id}`: header, tracklist and TheAudioDB artwork.
    pub async fn album(&self, raw_id: &str) -> Result<AlbumInfo, CatalogError> {
        let id = self.album_id(raw_id).await?;
        let (result, context) = scoped(self.build_album(&id, RequestPriority::UserInitiated)).await;
        result.map(|mut info| {
            info.basic.service_status = mapping::service_status(&context);
            info
        })
    }

    /// The album page with its providers asked at `priority`.
    pub(super) async fn build_album(
        &self,
        id: &str,
        priority: RequestPriority,
    ) -> Result<AlbumInfo, CatalogError> {
        let answer = self
            .musicbrainz_or_library(self.group_detail_at(id, priority).await, || {
                self.local_album(id)
            })
            .await?;
        let group = match answer {
            Answer::MusicBrainz(group) => group,
            Answer::Library(local) => {
                let basic = self.basic_from_local(id, &local).await;
                let images = self.cached_album_images(id).await.unwrap_or_default();
                return Ok(AlbumInfo {
                    basic,
                    tracks: local_tracks(local.tracks),
                    images,
                });
            }
        };
        let (basic, tracks, images) = tokio::join!(
            self.basic_from_group(&group),
            self.tracks_for_group(&group, priority),
            self.album_images_lookup(&group, priority)
        );
        let images = images.flatten();
        let tracks = match tracks {
            Ok(tracks) => tracks,
            Err(error) => {
                record_mb_down(&error);
                tracing::warn!(album = %group.mbid, %error, "tracklist unavailable; header only");
                AlbumTracksInfo::default()
            }
        };
        let mut basic = basic;
        if basic.album_thumb_url.is_none() {
            basic.album_thumb_url = images
                .as_ref()
                .and_then(|images| images.album_thumb_url.clone());
        }
        Ok(AlbumInfo {
            basic,
            tracks,
            images: images.unwrap_or_default(),
        })
    }

    /// TheAudioDB images already held, fresh or stale; never dials out.
    async fn cached_album_images(&self, mbid: &str) -> Option<AlbumImages> {
        self.upstream()
            .cache()
            .peek(&self.audiodb_album_key(mbid))
            .await
            .flatten()
    }

    /// TheAudioDB images as a settled answer: `Some(None)` when AudioDB has
    /// none (or is off), `None` when it could not answer. AudioDB is asked
    /// at `priority`.
    pub(super) async fn album_images_lookup(
        &self,
        group: &GroupDetail,
        priority: RequestPriority,
    ) -> Option<Option<AlbumImages>> {
        let advanced = self.upstream().settings().advanced();
        let Some(client) = self.upstream().audiodb(&advanced) else {
            return Some(None);
        };
        let fallback = advanced.audiodb_name_search_fallback;
        let (mbid, artist, title) = (
            group.mbid.clone(),
            group.artist_name.clone(),
            group.title.clone(),
        );
        let answer = self
            .upstream()
            .cache()
            .get_or_fetch(
                &self.audiodb_album_key(&group.mbid),
                Policy::AUDIODB,
                priority,
                move |priority| async move {
                    let mut found = client.album_by_mbid(&mbid, priority).await?;
                    if found.is_none() && fallback {
                        found = client.search_album(&artist, &title, priority).await?;
                    }
                    Ok::<_, ProviderError>(match found {
                        Some(album) => Fetched::Found(AlbumImages {
                            album_thumb_url: album.thumb,
                            album_back_url: album.back,
                            album_cdart_url: album.cdart,
                            album_spine_url: album.spine,
                            album_3d_case_url: album.case_3d,
                            album_3d_flat_url: album.flat_3d,
                            album_3d_face_url: album.face_3d,
                            album_3d_thumb_url: album.thumb_3d,
                        }),
                        None => Fetched::Missing,
                    })
                },
            )
            .await;
        // A failure is recorded where it happened and stores nothing, so
        // the next visit tries again.
        answer.ok()
    }

    /// `GET /albums/{album_id}/editions`: every MusicBrainz release of the
    /// album (paged through the release index, so groups with more than 25
    /// editions list them all), flagged owned and chosen, with the one the
    /// page shows.
    pub async fn album_editions(
        &self,
        raw_id: &str,
    ) -> Result<AlbumEditionsResponse, CatalogError> {
        let id = self.album_id(raw_id).await?;
        let group = self
            .group_detail(&id)
            .await?
            .ok_or(CatalogError::NotFound)?;
        let selection = self.select_edition(&group).await?;
        let matches = |wanted: &Option<String>, id: &str| {
            wanted
                .as_deref()
                .is_some_and(|wanted| wanted.eq_ignore_ascii_case(id))
        };
        let mut items = match self.every_edition(&group.mbid).await {
            Ok(items) if !items.is_empty() => items,
            Ok(_) => listed_editions(&group),
            Err(error) => {
                tracing::info!(album = %group.mbid, %error, "edition index unavailable; listing the group lookup's editions");
                listed_editions(&group)
            }
        };
        for item in &mut items {
            item.is_owned = matches(&selection.owned, &item.release_mbid);
            item.is_pinned = matches(&selection.chosen, &item.release_mbid);
        }
        Ok(AlbumEditionsResponse {
            items,
            pinned_release_mbid: selection.chosen,
            owned_release_mbid: selection.owned,
            selected_release_mbid: selection.selected,
            selected_basis: selection.basis.map(str::to_owned),
        })
    }

    fn editions_key(&self, mbid: &str) -> CacheKey {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        CacheKey::new(
            Source::MusicBrainz,
            &[&namespace, "rg_editions", &mbid.to_ascii_lowercase()],
        )
    }

    /// Every release of one group from MusicBrainz's release index, cached
    /// like the group detail.
    async fn every_edition(&self, mbid: &str) -> Result<Vec<AlbumEditionItem>, CatalogError> {
        let catalog = self.clone();
        let group = mbid.to_owned();
        let items = self
            .cached(
                &self.editions_key(mbid),
                Policy::MUSICBRAINZ_ENTITY,
                RequestPriority::UserInitiated,
                move |priority| async move {
                    let (client, _) = catalog.upstream().musicbrainz(priority);
                    let mut items: Vec<AlbumEditionItem> = Vec::new();
                    for page in 0..EDITION_PAGES {
                        let offset = page * EDITION_PAGE;
                        let found = client
                            .search_release_group_editions(
                                &group,
                                EDITION_PAGE,
                                offset,
                                Criticality::IdentityCritical,
                            )
                            .await
                            .map_err(mb_search_error)?;
                        let total = found.count;
                        let got = found.items.len();
                        items.extend(
                            found
                                .items
                                .into_iter()
                                .filter_map(
                                    droppedneedle_library::identify::sources::edition_from_hit,
                                )
                                .filter(|edition| {
                                    edition.release_group_mbid.eq_ignore_ascii_case(&group)
                                })
                                .map(edition_item),
                        );
                        if got == 0 || u64::from(offset) + got as u64 >= total {
                            break;
                        }
                    }
                    Ok(Fetched::Found(items))
                },
            )
            .await?;
        Ok(items.unwrap_or_default())
    }

    /// `GET /albums/{album_id}/editions/{release_mbid}/tracks`: one
    /// release's tracklist (cached like every release lookup).
    pub async fn edition_tracks(&self, raw_release: &str) -> Result<AlbumTracksInfo, CatalogError> {
        let id = checked_mbid(raw_release, "release")?;
        let release = self
            .release_detail(&id)
            .await?
            .ok_or(CatalogError::NotFound)?;
        Ok(AlbumTracksInfo {
            total_tracks: u32::try_from(release.tracks.len()).unwrap_or(u32::MAX),
            total_length: (release.total_length > 0).then_some(release.total_length),
            tracks: release.tracks,
            label: release.label,
            barcode: release.barcode,
            country: release.country,
            selected_release_mbid: Some(id),
            pick_basis: None,
        })
    }

    /// `POST /albums/{album_id}/refresh`: drop the album's cached
    /// MusicBrainz, artwork and store answers, then rebuild the header.
    pub async fn album_refresh(&self, raw_id: &str) -> Result<AlbumBasicInfo, CatalogError> {
        let id = self.album_id(raw_id).await?;
        let region = self.upstream().settings().store_region();
        let mut keys = vec![self.group_key(&id), self.audiodb_album_key(&id)];
        keys.extend(region.iter().map(|region| self.purchase_key(&id, region)));
        if let Ok(Some(group)) = self.group_detail(&id).await {
            keys.push(self.group_key(&group.mbid));
            keys.push(self.audiodb_album_key(&group.mbid));
            keys.extend(
                group
                    .releases
                    .iter()
                    .map(|release| self.release_key(&release.id)),
            );
            keys.extend(
                region
                    .iter()
                    .map(|region| self.purchase_key(&group.mbid, region)),
            );
        }
        self.forget(&keys).await;
        self.album_basic(&id).await
    }

    /// The purchase cache key: per MusicBrainz source, store region and set
    /// of extra link providers.
    fn purchase_key(&self, mbid: &str, region: &str) -> CacheKey {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        CacheKey::hashed(
            Source::Purchase,
            "links",
            &[
                &namespace,
                &mbid.to_ascii_lowercase(),
                &region.to_ascii_uppercase(),
                &self.purchase_links.token(),
            ],
        )
    }

    /// `GET /albums/{album_id}/purchase-options`: store links from
    /// MusicBrainz (group first, then up to two releases, official first),
    /// extra links from purchase-link providers, an iTunes match when
    /// nothing so far is a download store, and a Bandcamp search as the
    /// floor. Kept a week; an answer missing iTunes because iTunes failed
    /// is not kept.
    pub async fn album_purchase_options(
        &self,
        raw_id: &str,
    ) -> Result<PurchaseOptionsResponse, CatalogError> {
        let id = self.album_id(raw_id).await?;
        // Keyed by the resolved group, so refresh and pin changes (which
        // drop the group's key) reach answers asked for by a release id.
        let group = self.group_detail(&id).await?;
        let region = self
            .upstream()
            .settings()
            .store_region()
            .ok_or_else(|| CatalogError::Internal("store region cannot be read".to_owned()))?;
        let catalog = self.clone();
        let key = self.purchase_key(
            group.as_ref().map_or(id.as_str(), |group| &group.mbid),
            &region,
        );
        let options = self
            .cached(
                &key,
                PURCHASE,
                RequestPriority::UserInitiated,
                move |_| async move {
                    let (options, complete) =
                        catalog.build_purchase_options(group, &region).await?;
                    Ok(if complete {
                        Fetched::Found(options)
                    } else {
                        Fetched::Partial(options)
                    })
                },
            )
            .await?;
        Ok(options.unwrap_or_default())
    }

    /// The options plus whether every source answered (only then cached).
    async fn build_purchase_options(
        &self,
        group: GroupCore,
        region: &str,
    ) -> Result<(PurchaseOptionsResponse, bool), CatalogError> {
        let Some(group) = group else {
            return Ok((
                PurchaseOptionsResponse {
                    bandcamp_search_url: bandcamp_album_search(""),
                    ..PurchaseOptionsResponse::default()
                },
                true,
            ));
        };
        let mut links = group.store_links.clone();
        let mut candidates: Vec<&ReleaseSummary> = group.releases.iter().collect();
        candidates.sort_by_key(|release| release.status.as_deref() != Some("Official"));
        for release in candidates.into_iter().take(MAX_PURCHASE_LOOKUPS) {
            if links.iter().any(|link| link.kind == PurchaseKind::Digital) {
                break;
            }
            if let Some(detail) = self.release_detail(&release.id).await? {
                for link in detail.store_links {
                    if !links.iter().any(|known| known.url == link.url) {
                        links.push(link);
                    }
                }
            }
        }
        let artist = group.credit.as_str();
        if !artist.is_empty() || !group.title.is_empty() {
            let query = PurchaseQuery {
                artist: artist.to_owned(),
                title: group.title.clone(),
                release_group_mbid: group.mbid.clone(),
            };
            for extra in self.purchase_links.links(&query).await {
                let url = extra.url.trim();
                if !(url.starts_with("https://") || url.starts_with("http://"))
                    || links.iter().any(|known| known.url == url)
                {
                    continue;
                }
                let store = mapping::store_for(url);
                links.push(PurchaseLink {
                    store: store.to_owned(),
                    label: extra
                        .label
                        .filter(|label| !label.trim().is_empty())
                        .or_else(|| mapping::store_label(store).map(str::to_owned))
                        .unwrap_or_else(|| mapping::host_of(url)),
                    url: url.to_owned(),
                    kind: extra.kind.unwrap_or(PurchaseKind::Digital),
                });
            }
        }
        let mut complete = true;
        let has_digital = links.iter().any(|link| link.kind == PurchaseKind::Digital);
        if !has_digital && !artist.is_empty() && !group.title.is_empty() {
            match self
                .upstream()
                .itunes()
                .find_album(artist, &group.title, region, RequestPriority::UserInitiated)
                .await
            {
                Ok(Some(found)) => links.push(PurchaseLink {
                    store: "itunes".to_owned(),
                    label: mapping::store_label("itunes")
                        .unwrap_or("iTunes")
                        .to_owned(),
                    url: found.url,
                    kind: PurchaseKind::Digital,
                }),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(album = %group.mbid, %error, "itunes lookup failed");
                    complete = false;
                }
            }
        }
        let split = |kind: PurchaseKind| {
            let mut chosen: Vec<PurchaseLink> = links
                .iter()
                .filter(|link| link.kind == kind)
                .cloned()
                .collect();
            mapping::sort_links(&mut chosen);
            chosen
        };
        Ok((
            PurchaseOptionsResponse {
                digital: split(PurchaseKind::Digital),
                physical: split(PurchaseKind::Physical),
                free: split(PurchaseKind::Free),
                bandcamp_search_url: bandcamp_album_search(&format!("{artist} {}", group.title)),
            },
            complete,
        ))
    }

    /// `GET /albums/{album_id}/plugin-details`: what metadata plugins add
    /// about the album, kept for an hour per set of plugins. With no
    /// plugin installed it answers empty at once.
    pub async fn album_plugin_details(&self, raw_id: &str) -> Result<PluginDetails, CatalogError> {
        let id = self.album_id(raw_id).await?;
        let Some(group) = self.group_detail(&id).await? else {
            return Ok(PluginDetails::default());
        };
        let (artist, title, mbid) = (
            group.credit.clone(),
            group.title.clone(),
            group.mbid.clone(),
        );
        self.plugin_details("album", &group.mbid, move |catalog| async move {
            catalog.details.album(&artist, &title, &mbid).await
        })
        .await
    }

    /// `GET /albums/{album_id}/lastfm`: Last.fm summary and tags with the
    /// user's Last.fm key (or the instance key), looked up by id and then
    /// by names.
    pub async fn album_lastfm(
        &self,
        user_id: &str,
        raw_id: &str,
        artist_name: &str,
        album_name: &str,
    ) -> Result<LastFmAlbumEnrichment, CatalogError> {
        let id = self.album_id(raw_id).await?;
        let Some((client, creds)) = self.upstream().lastfm(user_id).await else {
            return Ok(LastFmAlbumEnrichment::default());
        };
        let key = CacheKey::hashed(
            Source::LastFm,
            "album_info",
            &[&id, artist_name, album_name],
        );
        let artist = artist_name.to_owned();
        let album = album_name.to_owned();
        let priority = RequestPriority::UserInitiated;
        let enrichment = self
            .cached(&key, LASTFM_DETAILS, priority, move |priority| async move {
                let mut found = client
                    .album_info(&creds, &artist, &album, Some(&id), priority)
                    .await
                    .map_err(CatalogError::Unavailable)?;
                if found.is_none() && !artist.trim().is_empty() && !album.trim().is_empty() {
                    found = client
                        .album_info(&creds, &artist, &album, None, priority)
                        .await
                        .map_err(CatalogError::Unavailable)?;
                }
                let Some(info) = found else {
                    return Ok(Fetched::Missing);
                };
                let enrichment = LastFmAlbumEnrichment {
                    summary: mapping::clean_lastfm_bio(&info.summary),
                    tags: info
                        .tags
                        .into_iter()
                        .map(|tag| LastFmTag {
                            name: tag.name,
                            url: Some(tag.url).filter(|url| !url.is_empty()),
                        })
                        .collect(),
                    listeners: info.listeners,
                    playcount: info.playcount,
                    url: Some(info.url).filter(|url| !url.is_empty()),
                };
                Ok(Fetched::Found(enrichment))
            })
            .await;
        // A failure was recorded where it happened; the page shows nothing.
        Ok(enrichment.ok().flatten().unwrap_or_default())
    }
}

/// The library's tracks as a tracklist.
fn local_tracks(tracks: Vec<LocalTrack>) -> AlbumTracksInfo {
    let tracks: Vec<AlbumTrack> = tracks
        .into_iter()
        .map(|track| AlbumTrack {
            position: track.track_number,
            title: track.title,
            disc_number: track.disc_number,
            length: track.length_ms,
            recording_id: track.recording_mbid,
            release_track_id: None,
            media_format: None,
        })
        .collect();
    let total: u64 = tracks.iter().filter_map(|track| track.length).sum();
    AlbumTracksInfo {
        total_tracks: u32::try_from(tracks.len()).unwrap_or(u32::MAX),
        total_length: (total > 0).then_some(total),
        tracks,
        ..AlbumTracksInfo::default()
    }
}

fn bandcamp_album_search(term: &str) -> String {
    format!(
        "https://bandcamp.com/search?q={}&item_type=a",
        mapping::quote_plus(term.trim())
    )
}

/// The editions the group lookup itself lists.
fn listed_editions(group: &GroupDetail) -> Vec<AlbumEditionItem> {
    group
        .releases
        .iter()
        .map(|release| AlbumEditionItem {
            release_mbid: release.id.clone(),
            track_count: release.track_count,
            title: release.title.clone(),
            disambiguation: release.disambiguation.clone(),
            date: release.date.clone(),
            country: release.country.clone(),
            packaging: release.packaging.clone(),
            status: release.status.clone(),
            media_formats: release.formats.clone(),
            ..AlbumEditionItem::default()
        })
        .collect()
}

/// One release from the release index as an edition row.
fn edition_item(edition: droppedneedle_library::identify::sources::Edition) -> AlbumEditionItem {
    AlbumEditionItem {
        release_mbid: edition.release_mbid,
        track_count: edition.track_count,
        title: Some(edition.title).filter(|title| !title.is_empty()),
        disambiguation: edition.disambiguation,
        date: edition.date,
        country: edition.country,
        packaging: edition.packaging,
        status: edition.status,
        media_formats: edition.media_formats,
        disc_count: edition.disc_count,
        barcode: edition.barcode,
        label: edition.label,
        catalog_number: edition.catalogue_number,
        is_owned: false,
        is_pinned: false,
    }
}
