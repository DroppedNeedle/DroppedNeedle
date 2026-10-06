//! Album pages: header, tracklist, editions, artwork, Last.fm and store
//! links.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::providers::degradation::scoped;
use crate::providers::itunes::ITunesClient;
use crate::providers::lastfm;
use crate::providers::musicbrainz::{Criticality, MbRelease, MbReleaseGroup};
use crate::providers::{IntegrationStatus, RequestPriority, record_current};

use super::artist::{LASTFM_TTL, checked_mbid};
use super::error::CatalogError;
use super::mapping;
use super::models::GroupEditionPinResponse;
use super::models::{
    AlbumBasicInfo, AlbumEditionItem, AlbumEditionsResponse, AlbumImages, AlbumInfo, AlbumTrack,
    AlbumTracksInfo, CatalogSource, LastFmAlbumEnrichment, LastFmTag, PurchaseKind, PurchaseLink,
    PurchaseOptionsResponse,
};
use super::ports::PurchaseQuery;
use super::{Catalog, MISS_TTL, mb_error, mb_retry, record_mb_down, secs};

/// Includes for the release-group lookup: one call serves the header, the
/// edition list (with track counts) and the group's store links.
const GROUP_INCLUDES: [&str; 4] = ["artist-credits", "media", "releases", "url-rels"];
/// Includes for one release: tracklist, label, and its store links.
const RELEASE_INCLUDES: [&str; 3] = ["labels", "recordings", "url-rels"];
/// Releases consulted for store links (v2 `_MAX_RELEASE_LOOKUPS`).
const MAX_PURCHASE_LOOKUPS: usize = 2;
/// Purchase options are kept a week (v2 `_CACHE_TTL_SECONDS`).
const PURCHASE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
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
    relations: &[crate::providers::musicbrainz::Relation],
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

/// One cached TheAudioDB album lookup: images, or a remembered miss.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AudioDbAlbumEntry {
    images: Option<AlbumImages>,
}

fn audiodb_album_key(mbid: &str) -> String {
    format!("audiodb_album:{}", mbid.to_ascii_lowercase())
}

fn album_ttl(catalog: &Catalog, in_library: bool) -> Duration {
    let advanced = catalog.upstream().settings().advanced();
    secs(if in_library {
        advanced.cache_ttl_album_library
    } else {
        advanced.cache_ttl_album_non_library
    })
}

/// The edition an album page shows and why.
struct Selection {
    selected: Option<String>,
    basis: Option<&'static str>,
    owned: Option<String>,
    pinned: Option<String>,
}

impl Catalog {
    fn group_key(&self, mbid: &str) -> String {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        format!("mb:rg:detail:{namespace}:{mbid}")
    }

    fn release_key(&self, mbid: &str) -> String {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        format!("mb:release:detail:{namespace}:{mbid}")
    }

    /// The cached release-group detail. Sources that hand over a release
    /// MBID where a group belongs (v2 issue #78) resolve through the
    /// release before the miss is final.
    pub(super) async fn group_detail(&self, mbid: &str) -> Result<GroupCore, CatalogError> {
        let catalog = self.clone();
        let id = mbid.to_owned();
        self.cached(
            &self.inner.flights.group,
            self.group_key(mbid),
            move || async move {
                let (client, _) = catalog
                    .upstream()
                    .musicbrainz(RequestPriority::UserInitiated);
                let mut found = mb_retry(|| {
                    client.lookup_release_group(&id, &GROUP_INCLUDES, Criticality::IdentityCritical)
                })
                .await
                .map_err(mb_error)?;
                if found.is_none() {
                    let resolved = mb_retry(|| {
                        client.resolve_release_to_release_group(&id, Criticality::IdentityCritical)
                    })
                    .await
                    .map_err(mb_error)?;
                    if let Some(group_id) = resolved.filter(|group_id| *group_id != id) {
                        found = mb_retry(|| {
                            client.lookup_release_group(
                                &group_id,
                                &GROUP_INCLUDES,
                                Criticality::IdentityCritical,
                            )
                        })
                        .await
                        .map_err(mb_error)?;
                    }
                }
                match found {
                    None => Ok((None, Some(MISS_TTL))),
                    Some(lookup) => {
                        let detail = GroupDetail::from_wire(lookup.entity);
                        let (owned, _) = catalog
                            .album_flags(std::slice::from_ref(&detail.mbid))
                            .await;
                        let ttl = album_ttl(&catalog, !owned.is_empty());
                        Ok((Some(detail), Some(ttl)))
                    }
                }
            },
        )
        .await
    }

    /// The cached release detail.
    async fn release_detail(&self, mbid: &str) -> Result<ReleaseCore, CatalogError> {
        self.release_detail_at(mbid, RequestPriority::UserInitiated)
            .await
    }

    /// The cached release detail, fetching at `priority` on a miss.
    pub(super) async fn release_detail_at(
        &self,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<ReleaseCore, CatalogError> {
        let catalog = self.clone();
        let id = mbid.to_owned();
        self.cached(
            &self.inner.flights.release,
            self.release_key(mbid),
            move || async move {
                let (client, _) = catalog.upstream().musicbrainz(priority);
                let found = mb_retry(|| {
                    client.lookup_release(&id, &RELEASE_INCLUDES, Criticality::IdentityCritical)
                })
                .await
                .map_err(mb_error)?;
                Ok(match found {
                    None => (None, Some(MISS_TTL)),
                    Some(lookup) => (
                        Some(ReleaseDetail::from_wire(lookup.entity)),
                        Some(album_ttl(&catalog, false)),
                    ),
                })
            },
        )
        .await
    }

    /// A release detail already in the cache; never dials out.
    pub(super) async fn cached_release_detail(&self, mbid: &str) -> Option<ReleaseDetail> {
        let bytes = self
            .upstream()
            .cache()
            .get_bytes(&self.release_key(mbid))
            .await?;
        serde_json::from_slice::<ReleaseCore>(&bytes).ok().flatten()
    }

    /// `GET /albums/{album_id}/basic`: the header. A dead MusicBrainz falls
    /// back to the library's copy.
    pub async fn album_basic(&self, raw_id: &str) -> Result<AlbumBasicInfo, CatalogError> {
        let id = checked_mbid(raw_id, "album")?;
        let (result, context) = scoped(self.build_basic(&id)).await;
        result.map(|mut basic| {
            basic.service_status = mapping::service_status(&context);
            basic
        })
    }

    async fn build_basic(&self, id: &str) -> Result<AlbumBasicInfo, CatalogError> {
        match self.group_detail(id).await {
            Ok(Some(group)) => Ok(self.basic_from_group(&group).await),
            Ok(None) => Err(CatalogError::NotFound),
            Err(error) => {
                record_mb_down(&error);
                if matches!(error, CatalogError::Unavailable(_))
                    && let Some(local) = self.local_basic(id).await?
                {
                    return Ok(local);
                }
                Err(error)
            }
        }
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

    async fn local_basic(&self, id: &str) -> Result<Option<AlbumBasicInfo>, CatalogError> {
        let Some(local) = self
            .local()
            .album(id)
            .await
            .map_err(CatalogError::database)?
        else {
            return Ok(None);
        };
        tracing::warn!(album = %id, "musicbrainz unavailable; album page from the library");
        Ok(Some(AlbumBasicInfo {
            title: local.title,
            musicbrainz_id: id.to_owned(),
            artist_name: local.artist_name,
            artist_id: local.artist_mbid.unwrap_or_default(),
            release_date: None,
            year: local.year,
            album_type: None,
            disambiguation: None,
            in_library: true,
            requested: false,
            cover_url: mapping::release_group_cover_url(id),
            album_thumb_url: self
                .cached_album_images(id)
                .await
                .and_then(|images| images.album_thumb_url),
            source: CatalogSource::Library,
            service_status: None,
        }))
    }

    /// Pick the edition: a valid pin, then the owned identification, then
    /// the release closest to the library's file count, then MusicBrainz's
    /// best (v2 `_effective_release_id`, minus the embedded-tag vote the
    /// v3 identity tables already settle).
    async fn select_edition(&self, group: &GroupDetail) -> Result<Selection, CatalogError> {
        let evidence = self
            .local()
            .edition_evidence(&group.mbid)
            .await
            .map_err(CatalogError::database)?;
        let listed = |candidate: &Option<String>| {
            candidate.as_ref().and_then(|wanted| {
                group
                    .releases
                    .iter()
                    .find(|release| release.id.eq_ignore_ascii_case(wanted))
                    .map(|release| release.id.clone())
            })
        };
        let ranked = group.ranked();
        let (selected, basis) = if let Some(pin) = listed(&evidence.pinned_release) {
            (Some(pin), Some("pin"))
        } else if let Some(owned) = listed(&evidence.owned_release) {
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
        } else if let Some(first) = ranked.first() {
            (Some(first.id.clone()), Some("ranked"))
        } else {
            (None, None)
        };
        Ok(Selection {
            selected,
            basis,
            owned: evidence.owned_release,
            pinned: evidence.pinned_release,
        })
    }

    async fn tracks_for_group(&self, group: &GroupDetail) -> Result<AlbumTracksInfo, CatalogError> {
        let selection = self.select_edition(group).await?;
        let mut candidates: Vec<String> = selection.selected.iter().cloned().collect();
        for release in group.ranked().into_iter().take(TRACKLIST_CANDIDATES) {
            if !candidates.contains(&release.id) {
                candidates.push(release.id.clone());
            }
        }
        for candidate in candidates {
            let Some(release) = self.release_detail(&candidate).await? else {
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

    async fn local_tracks(&self, id: &str) -> Result<Option<AlbumTracksInfo>, CatalogError> {
        let Some(local) = self
            .local()
            .album(id)
            .await
            .map_err(CatalogError::database)?
        else {
            return Ok(None);
        };
        let tracks: Vec<AlbumTrack> = local
            .tracks
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
        Ok(Some(AlbumTracksInfo {
            total_tracks: u32::try_from(tracks.len()).unwrap_or(u32::MAX),
            total_length: (total > 0).then_some(total),
            tracks,
            ..AlbumTracksInfo::default()
        }))
    }

    /// `GET /albums/{album_id}/tracks`: the shown edition's tracklist. A
    /// dead MusicBrainz falls back to the library's tracks.
    pub async fn album_tracks(&self, raw_id: &str) -> Result<AlbumTracksInfo, CatalogError> {
        let id = checked_mbid(raw_id, "album")?;
        match self.group_detail(&id).await {
            Ok(Some(group)) => self.tracks_for_group(&group).await,
            Ok(None) => Err(CatalogError::NotFound),
            Err(error) => {
                if matches!(error, CatalogError::Unavailable(_))
                    && let Some(local) = self.local_tracks(&id).await?
                {
                    return Ok(local);
                }
                Err(error)
            }
        }
    }

    /// `GET /albums/{album_id}`: header, tracklist and TheAudioDB artwork.
    pub async fn album(&self, raw_id: &str) -> Result<AlbumInfo, CatalogError> {
        let id = checked_mbid(raw_id, "album")?;
        let (result, context) = scoped(self.build_album(&id)).await;
        result.map(|mut info| {
            info.basic.service_status = mapping::service_status(&context);
            info
        })
    }

    async fn build_album(&self, id: &str) -> Result<AlbumInfo, CatalogError> {
        let group = match self.group_detail(id).await {
            Ok(Some(group)) => group,
            Ok(None) => return Err(CatalogError::NotFound),
            Err(error) => {
                record_mb_down(&error);
                if matches!(error, CatalogError::Unavailable(_))
                    && let Some(basic) = self.local_basic(id).await?
                {
                    let tracks = self.local_tracks(id).await?.unwrap_or_default();
                    let images = self.cached_album_images(id).await.unwrap_or_default();
                    return Ok(AlbumInfo {
                        basic,
                        tracks,
                        images,
                    });
                }
                return Err(error);
            }
        };
        let (basic, tracks, images) = tokio::join!(
            self.basic_from_group(&group),
            self.tracks_for_group(&group),
            self.fetch_album_images(&group)
        );
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

    async fn cached_album_images(&self, mbid: &str) -> Option<AlbumImages> {
        let bytes = self
            .upstream()
            .cache()
            .get_bytes(&audiodb_album_key(mbid))
            .await?;
        serde_json::from_slice::<AudioDbAlbumEntry>(&bytes)
            .ok()?
            .images
    }

    async fn fetch_album_images(&self, group: &GroupDetail) -> Option<AlbumImages> {
        let advanced = self.upstream().settings().advanced();
        let client = self.upstream().audiodb(&advanced)?;
        let key = audiodb_album_key(&group.mbid);
        if let Some(bytes) = self.upstream().cache().get_bytes(&key).await
            && let Ok(entry) = serde_json::from_slice::<AudioDbAlbumEntry>(&bytes)
        {
            return entry.images;
        }
        let mut outcome = client.album_by_mbid(&group.mbid).await;
        if matches!(outcome, crate::providers::audiodb::Outcome::Missing)
            && advanced.audiodb_name_search_fallback
        {
            outcome = client.search_album(&group.artist_name, &group.title).await;
        }
        let (images, ttl) = match outcome {
            crate::providers::audiodb::Outcome::Found(album) => (
                Some(AlbumImages {
                    album_thumb_url: album.thumb,
                    album_back_url: album.back,
                    album_cdart_url: album.cdart,
                    album_spine_url: album.spine,
                    album_3d_case_url: album.case_3d,
                    album_3d_flat_url: album.flat_3d,
                    album_3d_face_url: album.face_3d,
                    album_3d_thumb_url: album.thumb_3d,
                }),
                secs(advanced.cache_ttl_audiodb_found),
            ),
            crate::providers::audiodb::Outcome::Missing => {
                (None, secs(advanced.cache_ttl_audiodb_not_found))
            }
            crate::providers::audiodb::Outcome::Unavailable { .. } => return None,
        };
        if let Ok(bytes) = serde_json::to_vec(&AudioDbAlbumEntry {
            images: images.clone(),
        }) {
            self.upstream().cache().set_bytes(&key, bytes, ttl).await;
        }
        images
    }

    /// `GET /albums/{album_id}/editions`: every MusicBrainz release of the
    /// album, flagged owned and pinned, with the one the page shows.
    pub async fn album_editions(
        &self,
        raw_id: &str,
    ) -> Result<AlbumEditionsResponse, CatalogError> {
        let id = checked_mbid(raw_id, "album")?;
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
        Ok(AlbumEditionsResponse {
            items: group
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
                    is_owned: matches(&selection.owned, &release.id),
                    is_pinned: matches(&selection.pinned, &release.id),
                })
                .collect(),
            pinned_release_mbid: selection.pinned,
            owned_release_mbid: selection.owned,
            selected_release_mbid: selection.selected,
            selected_basis: selection.basis.map(str::to_owned),
        })
    }

    /// `POST /albums/{album_id}/refresh`: drop the album's cached
    /// MusicBrainz, artwork and store answers, then rebuild the header.
    pub async fn album_refresh(&self, raw_id: &str) -> Result<AlbumBasicInfo, CatalogError> {
        let id = checked_mbid(raw_id, "album")?;
        let mut keys = vec![self.group_key(&id), audiodb_album_key(&id)];
        if let Ok(Some(group)) = self.group_detail(&id).await {
            keys.push(self.group_key(&group.mbid));
            keys.push(audiodb_album_key(&group.mbid));
            keys.extend(
                group
                    .releases
                    .iter()
                    .map(|release| self.release_key(&release.id)),
            );
            keys.push(self.purchase_key(&group.mbid));
        }
        self.forget(&keys).await;
        self.album_basic(&id).await
    }

    /// Whether the user may pin editions: admins and trusted users (v2
    /// curators). A user who is gone reads as not allowed.
    async fn require_curator(&self, user_id: &str) -> Result<(), CatalogError> {
        use crate::auth::users::roles::Role;
        let user = self
            .upstream()
            .users()
            .users
            .get_by_id(user_id)
            .await
            .map_err(|error| CatalogError::Internal(format!("user read: {error:?}")))?;
        match user.map(|user| user.role) {
            Some(Role::Admin | Role::Trusted) => Ok(()),
            _ => Err(CatalogError::Forbidden),
        }
    }

    /// The one library copy of a release group an edition pin applies to.
    /// v2 pinned per release group; v3 pins per library copy, so a group
    /// held twice must be pinned from the copy's own library page.
    async fn pin_target(&self, group: &str) -> Result<String, CatalogError> {
        let mut albums = self
            .local()
            .albums_for_group(group)
            .await
            .map_err(CatalogError::database)?;
        match albums.len() {
            0 => Err(CatalogError::Missing(
                "This album is not in the library, so there is no copy to pin".to_owned(),
            )),
            1 => Ok(albums.remove(0)),
            _ => Err(CatalogError::Conflict(
                "The library holds this album more than once; pin the edition from the copy's own page"
                    .to_owned(),
            )),
        }
    }

    /// Drop the cached answers an edition change alters.
    async fn forget_album(&self, group: &GroupDetail) {
        let mut keys = vec![self.group_key(&group.mbid), self.purchase_key(&group.mbid)];
        keys.extend(
            group
                .releases
                .iter()
                .map(|release| self.release_key(&release.id)),
        );
        self.forget(&keys).await;
    }

    /// `PUT /albums/{album_id}/edition`: pin an edition by release group,
    /// as v2 did, for the library's one copy of the album. Curators only;
    /// the release must be one of the album's editions.
    pub async fn set_group_edition_pin(
        &self,
        user_id: &str,
        raw_id: &str,
        release_mbid: &str,
    ) -> Result<GroupEditionPinResponse, CatalogError> {
        self.require_curator(user_id).await?;
        let id = checked_mbid(raw_id, "album")?;
        let group = self
            .group_detail(&id)
            .await?
            .ok_or(CatalogError::NotFound)?;
        let release = group
            .releases
            .iter()
            .find(|release| release.id.eq_ignore_ascii_case(release_mbid.trim()))
            .map(|release| release.id.clone())
            .ok_or_else(|| {
                CatalogError::Missing("That edition does not belong to this album".to_owned())
            })?;
        let album = self.pin_target(&group.mbid).await?;
        self.pins
            .set(&album, &group.mbid, &release, user_id)
            .await
            .map_err(CatalogError::Internal)?;
        self.forget_album(&group).await;
        Ok(GroupEditionPinResponse {
            pinned_release_mbid: Some(release),
        })
    }

    /// `DELETE /albums/{album_id}/edition`: clear the pin and go back to
    /// automatic edition choice. Curators only.
    pub async fn clear_group_edition_pin(
        &self,
        user_id: &str,
        raw_id: &str,
    ) -> Result<GroupEditionPinResponse, CatalogError> {
        self.require_curator(user_id).await?;
        let id = checked_mbid(raw_id, "album")?;
        let group = self
            .group_detail(&id)
            .await?
            .ok_or(CatalogError::NotFound)?;
        let album = self.pin_target(&group.mbid).await?;
        self.pins
            .clear(&album)
            .await
            .map_err(CatalogError::Internal)?;
        self.forget_album(&group).await;
        Ok(GroupEditionPinResponse {
            pinned_release_mbid: None,
        })
    }

    /// The purchase cache key: per MusicBrainz source, store region and set
    /// of extra link providers.
    fn purchase_key(&self, mbid: &str) -> String {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let region = self.upstream().settings().store_region();
        crate::providers::digest_key(
            "getit:",
            &[
                &namespace,
                &mbid.to_ascii_lowercase(),
                &region.to_ascii_uppercase(),
                &self.purchase_links.token(),
            ],
            "v1",
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
        let id = checked_mbid(raw_id, "album")?;
        let region = self.upstream().settings().store_region();
        let catalog = self.clone();
        let key = self.purchase_key(&id);
        let value = self
            .cached(&self.inner.flights.other, key, move || async move {
                let (options, complete) = catalog.build_purchase_options(&id, &region).await?;
                let value = serde_json::to_value(&options)
                    .map_err(|error| CatalogError::Internal(format!("purchase encode: {error}")))?;
                Ok((value, complete.then_some(PURCHASE_TTL)))
            })
            .await?;
        serde_json::from_value(value)
            .map_err(|error| CatalogError::Internal(format!("purchase decode: {error}")))
    }

    /// The options plus whether every source answered (only then cached).
    async fn build_purchase_options(
        &self,
        id: &str,
        region: &str,
    ) -> Result<(PurchaseOptionsResponse, bool), CatalogError> {
        let Some(group) = self.group_detail(id).await? else {
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
            let http = self.upstream().http_get();
            let itunes = ITunesClient::with_search_url(&http, &self.upstream().endpoints().itunes);
            match itunes.find_album(artist, &group.title, region).await {
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
                    tracing::warn!(album = %group.mbid, ?error, "itunes lookup failed");
                    record_current("itunes", IntegrationStatus::Error, false);
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

    /// `GET /albums/{album_id}/lastfm`: Last.fm summary and tags with the
    /// user's own key, looked up by id and then by names.
    pub async fn album_lastfm(
        &self,
        user_id: &str,
        raw_id: &str,
        artist_name: &str,
        album_name: &str,
    ) -> Result<LastFmAlbumEnrichment, CatalogError> {
        let id = checked_mbid(raw_id, "album")?;
        let Some((client, creds)) = self.upstream().lastfm(user_id).await else {
            return Ok(LastFmAlbumEnrichment::default());
        };
        let key =
            crate::providers::digest_key("lfm_album_info:", &[&id, artist_name, album_name], "v1");
        let artist = artist_name.to_owned();
        let album = album_name.to_owned();
        let value = self
            .cached(&self.inner.flights.other, key, move || async move {
                let mut outcome = client.album_info(&creds, &artist, &album, Some(&id)).await;
                if matches!(outcome, lastfm::Outcome::Missing)
                    && !artist.trim().is_empty()
                    && !album.trim().is_empty()
                {
                    outcome = client.album_info(&creds, &artist, &album, None).await;
                }
                let info = match outcome {
                    lastfm::Outcome::Found(info) => info,
                    lastfm::Outcome::Missing => {
                        return Ok((serde_json::Value::Null, Some(LASTFM_TTL)));
                    }
                    lastfm::Outcome::Unavailable { .. } => {
                        return Ok((serde_json::Value::Null, None));
                    }
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
                let value = serde_json::to_value(&enrichment)
                    .map_err(|error| CatalogError::Internal(format!("lastfm encode: {error}")))?;
                Ok((value, Some(LASTFM_TTL)))
            })
            .await?;
        Ok(serde_json::from_value(value).unwrap_or_default())
    }
}

fn bandcamp_album_search(term: &str) -> String {
    format!(
        "https://bandcamp.com/search?q={}&item_type=a",
        mapping::quote_plus(term.trim())
    )
}
