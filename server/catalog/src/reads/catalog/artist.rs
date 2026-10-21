//! Artist pages: header, biography, discography, Last.fm and store links.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use droppedneedle_providers::degradation::scoped;
use droppedneedle_providers::musicbrainz::{
    Criticality, MAX_PAGE_LIMIT, MbArtist, ReleaseGroupRef,
};
use droppedneedle_providers::{CacheKey, Fetched, Policy, ProviderError, RequestPriority, Source};

use super::error::CatalogError;
use super::mapping;
use super::models::{
    ArtistExtendedInfo, ArtistImages, ArtistInfo, ArtistPurchaseOptionsResponse, ArtistReleases,
    CatalogSource, ExternalLink, LastFmArtistEnrichment, LastFmSimilarArtist, LastFmTag, LifeSpan,
    PluginDetails, PurchaseLink, ReleaseItem,
};
use super::{
    Answer, BIOGRAPHY, Catalog, LASTFM_DETAILS, is_mbid, mb_error, mb_search_error, record_mb_down,
};
use crate::reads::collections::state::AutoDownloadState;
use crate::reads::platform::artwork::ArtistImageCandidate;
use crate::reads::platform::artwork::cache::ImageCredit;
use crate::reads::platform::artwork::warm::WarmTargets;

/// Most discography pages fetched per artist (v2 `_MAX_RG_PAGES`).
const MAX_RELEASE_GROUP_PAGES: u32 = 10;
/// Wikipedia edition for biographies.
const BIO_LANGUAGE: &str = "en";
/// Tags and aliases shown on the header (v2 limits).
const MAX_TAGS: usize = 10;

/// The cached projection of one MusicBrainz artist lookup. `None` is a
/// definitive miss.
pub type ArtistCore = Option<ArtistDetail>;

/// What the artist pages keep from MusicBrainz.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistDetail {
    /// Canonical MBID (after any merge redirect).
    pub mbid: String,
    /// Name.
    pub name: String,
    /// Disambiguation comment.
    pub disambiguation: Option<String>,
    /// Artist type.
    pub artist_type: Option<String>,
    /// Country code.
    pub country: Option<String>,
    /// Life span.
    pub life_span: Option<LifeSpan>,
    /// Tags, most voted first.
    pub tags: Vec<String>,
    /// Aliases.
    pub aliases: Vec<String>,
    /// Known links.
    pub external_links: Vec<ExternalLink>,
    /// The artist's own store pages.
    pub store_links: Vec<PurchaseLink>,
    /// Wikidata id, for the portrait.
    pub wikidata_id: Option<String>,
    /// Wikipedia or Wikidata URL, for the biography.
    pub wiki_url: Option<String>,
}

impl ArtistDetail {
    fn from_wire(artist: MbArtist) -> Self {
        let mut tags: Vec<_> = artist.tags.iter().collect();
        tags.sort_by(|left, right| right.count.unwrap_or(0).cmp(&left.count.unwrap_or(0)));
        let mut seen = HashSet::new();
        let tags = tags
            .into_iter()
            .map(|tag| tag.name.clone())
            .filter(|name| !name.is_empty() && seen.insert(name.clone()))
            .take(MAX_TAGS)
            .collect();
        let aliases = artist
            .aliases
            .iter()
            .map(|alias| alias.name.clone())
            .filter(|name| !name.is_empty())
            .take(MAX_TAGS)
            .collect();
        let (wikidata_id, wiki_url) = mapping::wiki_info(&artist.relations);
        let mut store_links: Vec<PurchaseLink> = Vec::new();
        for relation in &artist.relations {
            if let Some(link) = mapping::purchase_link(relation, &mapping::ARTIST_STORE_RELATIONS)
                && !store_links.iter().any(|known| known.url == link.url)
            {
                store_links.push(link);
            }
        }
        mapping::sort_links(&mut store_links);
        Self {
            name: artist
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Unknown Artist".to_owned()),
            disambiguation: artist
                .disambiguation
                .clone()
                .filter(|text| !text.is_empty()),
            artist_type: artist.artist_type.clone(),
            country: artist.country.clone(),
            life_span: artist.life_span.as_ref().map(|span| LifeSpan {
                begin: span.begin.clone(),
                end: span.end.clone(),
                ended: span.ended,
            }),
            tags,
            aliases,
            external_links: mapping::external_links(&artist.relations),
            store_links,
            wikidata_id,
            wiki_url,
            mbid: artist.id,
        }
    }
}

/// One release group kept for discography pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseGroupItem {
    /// Release-group MBID.
    pub id: String,
    /// Title.
    pub title: Option<String>,
    /// Primary type.
    pub primary_type: Option<String>,
    /// Secondary types.
    pub secondary_types: Vec<String>,
    /// First release date.
    pub first_release_date: Option<String>,
    /// Credited artist name, for "more by" rows.
    pub artist_name: Option<String>,
}

impl ReleaseGroupItem {
    fn from_wire(group: ReleaseGroupRef) -> Self {
        let artist_name = group
            .artist_credit
            .first()
            .map(|_| mapping::first_credit(&group.artist_credit).0);
        Self {
            id: group.id,
            title: group.title,
            primary_type: group.primary_type,
            secondary_types: group.secondary_types,
            first_release_date: group.first_release_date,
            artist_name,
        }
    }
}

/// An artist's release groups. `complete` is false while the rest of a
/// large discography is still being fetched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseGroupList {
    /// Release groups in MusicBrainz order, duplicates dropped.
    pub items: Vec<ReleaseGroupItem>,
    /// Whether every page has been fetched.
    pub complete: bool,
    /// Release groups MusicBrainz lists for the artist, before filtering.
    #[serde(default)]
    pub total: u64,
}

/// A discography split into the three sections the pages show, each
/// newest first, filtered by the release-type preferences and flagged
/// against the library.
#[derive(Debug, Clone, Default)]
pub struct Discography {
    /// Albums.
    pub albums: Vec<ReleaseItem>,
    /// EPs.
    pub eps: Vec<ReleaseItem>,
    /// Singles.
    pub singles: Vec<ReleaseItem>,
}

/// The cached biography and portrait.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Biography {
    /// Wikipedia introduction.
    pub description: Option<String>,
    /// Wikidata/Commons portrait.
    pub image: Option<String>,
    /// The portrait's Commons credit. Absent in an answer cached before
    /// credits were kept.
    #[serde(default)]
    pub image_credit: Option<ImageCredit>,
    /// The portrait lookup failed, so "no image" is not an answer. Never
    /// cached: a failed fetch is not stored.
    #[serde(skip)]
    pub image_failed: bool,
}

/// The artist's own image and the discography's covers, as the page
/// lists them.
fn artist_targets(info: &ArtistInfo) -> WarmTargets {
    let mut targets = WarmTargets::default();
    targets.artist(Some(&info.musicbrainz_id));
    for release in info.albums.iter().chain(&info.eps).chain(&info.singles) {
        targets.cover(Some(&release.id));
    }
    targets
}

/// Validate and lowercase an MBID from a path.
pub(super) fn checked_mbid(raw: &str, what: &str) -> Result<String, CatalogError> {
    let mbid = raw.trim().to_ascii_lowercase();
    if is_mbid(&mbid) {
        Ok(mbid)
    } else {
        Err(CatalogError::Invalid(format!(
            "Invalid or unknown {what} ID: {raw}"
        )))
    }
}

impl Catalog {
    /// Cache key of the MusicBrainz artist detail.
    pub(super) fn artist_detail_key(&self, mbid: &str) -> CacheKey {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        CacheKey::new(Source::MusicBrainz, &[&namespace, "artist", mbid])
    }

    /// Cache key of the artist's release-group list.
    pub(super) fn release_groups_key(&self, mbid: &str) -> CacheKey {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        CacheKey::new(Source::MusicBrainz, &[&namespace, "artist_rgs", mbid])
    }

    /// Cache key of TheAudioDB's artist art. A name match may be another
    /// artist's picture, so whether the name search is on is in the key.
    pub(super) fn audiodb_artist_key(&self, mbid: &str) -> CacheKey {
        let fallback = self
            .upstream()
            .settings()
            .advanced()
            .audiodb_name_search_fallback;
        CacheKey::new(
            Source::AudioDb,
            &[
                if fallback { "by_name" } else { "by_id" },
                "artist",
                &mbid.to_ascii_lowercase(),
            ],
        )
    }

    /// The cached MusicBrainz artist detail at `priority`, `None` for a
    /// definitive miss. A page joins only a fetch a page started, so it
    /// never waits behind an image resolve or a warm-up queued at a lower
    /// priority.
    pub(super) async fn artist_detail(
        &self,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<ArtistCore, CatalogError> {
        let key = self.artist_detail_key(mbid);
        let catalog = self.clone();
        let mbid = mbid.to_owned();
        self.cached(
            &key,
            Policy::MUSICBRAINZ_ENTITY,
            priority,
            move |priority| async move {
                let (client, _) = catalog.upstream().musicbrainz(priority);
                let found = client
                    .lookup_artist(
                        &mbid,
                        &["aliases", "tags", "url-rels"],
                        Criticality::IdentityCritical,
                    )
                    .await
                    .map_err(mb_error)?;
                Ok(match found {
                    None => Fetched::Missing,
                    Some(lookup) => Fetched::Found(ArtistDetail::from_wire(lookup.entity)),
                })
            },
        )
        .await
    }

    /// `GET /artists/{artist_mbid}`: the header, the discography in three
    /// sections (as v2's full artist read), and the caller's follow state.
    /// A dead MusicBrainz falls back to the library's own rows.
    pub async fn artist(&self, user_id: &str, raw_mbid: &str) -> Result<ArtistInfo, CatalogError> {
        let mbid = checked_mbid(raw_mbid, "artist")?;
        let (result, context) =
            scoped(self.build_artist(&mbid, RequestPriority::UserInitiated)).await;
        let mut info = result?;
        self.warm_covers("artist", artist_targets(&info));
        info.service_status = mapping::service_status(&context);
        if let Some(follow) = self.follows().status(user_id, &info.musicbrainz_id).await {
            info.followed = follow.followed;
            info.auto_download = follow.auto_download;
            info.auto_download_state = follow.auto_download_state;
        }
        Ok(info)
    }

    pub(super) async fn build_artist(
        &self,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<ArtistInfo, CatalogError> {
        let (detail, list) = tokio::join!(
            self.artist_detail(mbid, priority),
            self.release_groups(mbid, priority)
        );
        let answer = self
            .musicbrainz_or_library(detail, || async {
                self.local()
                    .artist(mbid)
                    .await
                    .map_err(CatalogError::database)
            })
            .await?;
        let detail = match answer {
            Answer::MusicBrainz(detail) => detail,
            Answer::Library(local) => {
                tracing::warn!(artist = %mbid, "musicbrainz unavailable; artist page from the library");
                let discography = self.local_discography(mbid).await?.unwrap_or_default();
                let (in_library, appears_in_library) = self.artist_relationship(mbid).await;
                return Ok(ArtistInfo {
                    name: local.name,
                    musicbrainz_id: mbid.to_owned(),
                    disambiguation: None,
                    artist_type: None,
                    country: None,
                    life_span: None,
                    tags: Vec::new(),
                    aliases: Vec::new(),
                    external_links: Vec::new(),
                    images: self.cached_artist_images(mbid).await.unwrap_or_default(),
                    in_library,
                    appears_in_library,
                    release_group_count: discography.albums.len() as u64,
                    albums: discography.albums,
                    singles: discography.singles,
                    eps: discography.eps,
                    followed: false,
                    auto_download: false,
                    auto_download_state: AutoDownloadState::None,
                    source: CatalogSource::Library,
                    service_status: None,
                });
            }
        };
        let (in_library, appears_in_library) = self.artist_relationship(&detail.mbid).await;
        let images = self
            .cached_artist_images(&detail.mbid)
            .await
            .unwrap_or_default();
        // The discography decorates the header; a failed browse leaves the
        // sections empty with the outage noted, never fails the header.
        let (discography, release_group_count) = match list {
            Ok(list) => (self.discography(&list).await, list.total),
            Err(error) => {
                record_mb_down(&error);
                tracing::warn!(artist = %mbid, %error, "artist discography unavailable");
                (Discography::default(), 0)
            }
        };
        Ok(ArtistInfo {
            name: detail.name,
            musicbrainz_id: detail.mbid,
            disambiguation: detail.disambiguation,
            artist_type: detail.artist_type,
            country: detail.country,
            life_span: detail.life_span,
            tags: detail.tags,
            aliases: detail.aliases,
            external_links: detail.external_links,
            images,
            in_library,
            appears_in_library,
            albums: discography.albums,
            singles: discography.singles,
            eps: discography.eps,
            release_group_count,
            followed: false,
            auto_download: false,
            auto_download_state: AutoDownloadState::None,
            source: CatalogSource::Musicbrainz,
            service_status: None,
        })
    }

    /// The artist's library relationship: (owned, appears only on tracks).
    /// A failed read logs and reads as neither.
    async fn artist_relationship(&self, mbid: &str) -> (bool, bool) {
        let owned = !self.artist_flags(&[mbid.to_owned()]).await.is_empty();
        if owned {
            return (true, false);
        }
        let appears = self
            .local()
            .artist_appears(mbid)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "library appearance read failed; showing none");
                false
            });
        (false, appears)
    }

    /// TheAudioDB images already in the cache; never dials out, so the
    /// header stays fast.
    pub(super) async fn cached_artist_images(&self, mbid: &str) -> Option<ArtistImages> {
        self.cached_audiodb_answer(&self.audiodb_artist_key(mbid))
            .await
            .flatten()
    }

    /// TheAudioDB's held answer under `key`, fresh or stale: `Some(None)`
    /// for a remembered miss, `None` when nothing is held.
    async fn cached_audiodb_answer(&self, key: &CacheKey) -> Option<Option<ArtistImages>> {
        self.upstream().cache().peek(key).await
    }

    /// TheAudioDB images for a page, from the cache only: the free key
    /// allows one call every two seconds, too slow for a page to wait on.
    /// With nothing cached, a background fill for `queue_for` (the artist
    /// as the page asked for it) is queued and the answer reads as none.
    /// Off reads as none and queues nothing.
    pub(super) async fn page_artist_images(
        &self,
        mbid: &str,
        queue_for: &str,
    ) -> Option<ArtistImages> {
        let advanced = self.upstream().settings().advanced();
        self.upstream().audiodb(&advanced)?;
        let cached = self
            .cached_audiodb_answer(&self.audiodb_artist_key(mbid))
            .await;
        if cached.is_none() {
            let mut targets = WarmTargets::default();
            targets.audiodb_artist(Some(queue_for));
            self.warm_covers("audiodb", targets);
        }
        cached.flatten()
    }

    /// TheAudioDB images, fetched and cached with v2's found/miss
    /// lifetimes. Off or failing reads as no images.
    async fn fetch_artist_images(&self, mbid: &str, name: &str) -> Option<ArtistImages> {
        self.artist_images_lookup(mbid, name, RequestPriority::UserInitiated)
            .await
            .flatten()
    }

    /// Whether TheAudioDB's answer for the artist is held, fresh or stale.
    pub(super) async fn artist_images_known(&self, mbid: &str) -> bool {
        self.held(&self.audiodb_artist_key(mbid)).await
    }

    /// TheAudioDB images as a settled answer: `Some(None)` when AudioDB has
    /// none (or is off), `None` when it could not answer, so nothing was
    /// cached and the caller may ask again soon. AudioDB is asked at
    /// `priority`.
    pub(super) async fn artist_images_lookup(
        &self,
        mbid: &str,
        name: &str,
        priority: RequestPriority,
    ) -> Option<Option<ArtistImages>> {
        let advanced = self.upstream().settings().advanced();
        let Some(client) = self.upstream().audiodb(&advanced) else {
            return Some(None);
        };
        let fallback = advanced.audiodb_name_search_fallback;
        let (mbid, name) = (mbid.to_owned(), name.to_owned());
        let answer = self
            .upstream()
            .cache()
            .get_or_fetch(
                &self.audiodb_artist_key(&mbid),
                Policy::AUDIODB,
                priority,
                move |priority| async move {
                    let mut found = client.artist_by_mbid(&mbid, priority).await?;
                    if found.is_none() && fallback && !name.is_empty() {
                        found = client.search_artist(&name, priority).await?;
                    }
                    Ok::<_, ProviderError>(match found {
                        Some(artist) => Fetched::Found(ArtistImages {
                            thumb_url: artist.thumb,
                            fanart_url: artist.fanart,
                            fanart_url_2: artist.fanart_2,
                            fanart_url_3: artist.fanart_3,
                            fanart_url_4: artist.fanart_4,
                            wide_thumb_url: artist.wide_thumb,
                            banner_url: artist.banner,
                            logo_url: artist.logo,
                            clearart_url: artist.clearart,
                            cutout_url: artist.cutout,
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

    /// `GET /artists/{artist_mbid}/extended`: biography and portrait from
    /// Wikipedia/Wikidata, plus TheAudioDB images. Every failure reads as
    /// absent fields (v2 answered empty rather than erroring).
    pub async fn artist_extended(
        &self,
        raw_mbid: &str,
    ) -> Result<ArtistExtendedInfo, CatalogError> {
        let mbid = checked_mbid(raw_mbid, "artist")?;
        let detail = match self
            .artist_detail(&mbid, RequestPriority::UserInitiated)
            .await
        {
            Ok(Some(detail)) => detail,
            Ok(None) => return Ok(ArtistExtendedInfo::default()),
            Err(error) => {
                tracing::warn!(artist = %mbid, %error, "extended artist info unavailable");
                return Ok(ArtistExtendedInfo::default());
            }
        };
        let (biography, images, plugins) = tokio::join!(
            self.biography(&detail, RequestPriority::UserInitiated),
            self.fetch_artist_images(&detail.mbid, &detail.name),
            self.artist_plugin_details(&detail.name, &detail.mbid)
        );
        // Plugin details sit below Wikipedia and Wikidata: they only fill
        // what those left empty.
        let filled = |first: Option<String>, plugin: Option<String>| {
            first
                .filter(|text| !text.trim().is_empty())
                .or(plugin.filter(|text| !text.trim().is_empty()))
        };
        Ok(ArtistExtendedInfo {
            description: filled(biography.description, plugins.biography),
            image: filled(biography.image, plugins.image_url),
            images: images.unwrap_or_default(),
        })
    }

    /// Plugin details for the artist; a failure reads as none.
    async fn artist_plugin_details(&self, name: &str, mbid: &str) -> PluginDetails {
        let (name, owned_mbid) = (name.to_owned(), mbid.to_owned());
        self.plugin_details("artist", mbid, move |catalog| async move {
            catalog.details.artist(&name, &owned_mbid).await
        })
        .await
        .unwrap_or_default()
    }

    /// The artist's Wikipedia intro and Wikidata portrait at `priority`. A
    /// page joins only a flight a page started, so it never waits on a
    /// background fetch. A failed fetch is not kept; the wiki call that
    /// met it noted it for the request.
    pub(super) async fn biography(
        &self,
        detail: &ArtistDetail,
        priority: RequestPriority,
    ) -> Biography {
        if detail.wiki_url.is_none() && detail.wikidata_id.is_none() {
            return Biography::default();
        }
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let key = CacheKey::new(Source::Wikidata, &[&namespace, "bio", &detail.mbid]);
        let catalog = self.clone();
        let detail = detail.clone();
        let fetched = self
            .cached(&key, BIOGRAPHY, priority, move |priority| async move {
                let wiki = catalog.upstream().wikidata();
                let description = match &detail.wiki_url {
                    Some(url) => wiki.get_bio_extract(url, BIO_LANGUAGE, priority).await,
                    None => Ok(None),
                };
                let image = match &detail.wikidata_id {
                    Some(id) => wiki.get_artist_image(id, priority).await,
                    None => Ok(None),
                };
                let failed = description.is_err() || image.is_err();
                if let Some(error) = description.as_ref().err().or(image.as_ref().err()) {
                    tracing::debug!(artist = %detail.mbid, %error, "artist biography incomplete");
                }
                let image_failed = image.is_err();
                let portrait = image.ok().flatten().map(ArtistImageCandidate::commons);
                let biography = Biography {
                    description: description.ok().flatten().filter(|text| !text.is_empty()),
                    image_failed,
                    image_credit: portrait.as_ref().and_then(|found| found.credit.clone()),
                    image: portrait.map(|found| found.url),
                };
                Ok(if failed {
                    Fetched::Partial(biography)
                } else {
                    Fetched::Found(biography)
                })
            })
            .await;
        fetched.ok().flatten().unwrap_or_default()
    }

    /// An artist's release groups at `priority`: the first page right away,
    /// the rest by a background walker (cached when complete).
    pub(super) async fn release_groups(
        &self,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<ReleaseGroupList, CatalogError> {
        let key = self.release_groups_key(mbid);
        let catalog = self.clone();
        let artist = mbid.to_owned();
        let store_key = key.clone();
        self.cached(
            &key,
            Policy::DISCOGRAPHY,
            priority,
            move |priority| async move {
                let (client, _) = catalog.upstream().musicbrainz(priority);
                let page = client
                    .browse_artist_release_groups(
                        &artist,
                        MAX_PAGE_LIMIT,
                        0,
                        Criticality::IdentityCritical,
                    )
                    .await
                    .map_err(mb_search_error)?;
                let total = page.count;
                let mut seen = HashSet::new();
                let items: Vec<ReleaseGroupItem> = page
                    .items
                    .into_iter()
                    .filter(|group| seen.insert(group.id.to_ascii_lowercase()))
                    .map(ReleaseGroupItem::from_wire)
                    .collect();
                if total <= items.len() as u64 {
                    return Ok(Fetched::Found(ReleaseGroupList {
                        items,
                        complete: true,
                        total,
                    }));
                }
                // The first page stands in, unstored, until the walker
                // stores the whole list.
                catalog.spawn_discography_walker(artist, store_key, items.clone(), total);
                Ok(Fetched::Partial(ReleaseGroupList {
                    items,
                    complete: false,
                    total,
                }))
            },
        )
        .await
        .map(Option::unwrap_or_default)
    }

    /// Fetch the rest of a large discography at background priority and
    /// cache the whole list. One walker per artist; a failure logs and
    /// stores nothing, so the next visit tries again.
    fn spawn_discography_walker(
        &self,
        artist: String,
        key: CacheKey,
        seed: Vec<ReleaseGroupItem>,
        total: u64,
    ) {
        {
            let mut warming = self
                .inner
                .warming
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if !warming.insert(key.as_str().to_owned()) {
                return;
            }
        }
        let catalog = self.clone();
        tokio::spawn(async move {
            let outcome = catalog.walk_discography(&artist, &key, seed, total).await;
            if let Err(error) = outcome {
                tracing::warn!(artist = %artist, %error, "discography walk failed; partial list stays");
            }
            catalog
                .inner
                .warming
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(key.as_str());
        });
    }

    async fn walk_discography(
        &self,
        artist: &str,
        key: &CacheKey,
        mut items: Vec<ReleaseGroupItem>,
        total: u64,
    ) -> Result<(), CatalogError> {
        let (client, _) = self.upstream().musicbrainz(RequestPriority::BackgroundSync);
        let mut seen: HashSet<String> = items
            .iter()
            .map(|item| item.id.to_ascii_lowercase())
            .collect();
        let mut offset = MAX_PAGE_LIMIT;
        for _ in 1..MAX_RELEASE_GROUP_PAGES {
            if u64::from(offset) >= total {
                break;
            }
            let page = client
                .browse_artist_release_groups(
                    artist,
                    MAX_PAGE_LIMIT,
                    offset,
                    Criticality::IdentityCritical,
                )
                .await
                .map_err(mb_search_error)?;
            if page.items.is_empty() {
                break;
            }
            items.extend(
                page.items
                    .into_iter()
                    .filter(|group| seen.insert(group.id.to_ascii_lowercase()))
                    .map(ReleaseGroupItem::from_wire),
            );
            offset += MAX_PAGE_LIMIT;
        }
        let list = ReleaseGroupList {
            items,
            complete: true,
            total,
        };
        let bytes = serde_json::to_vec(&list)
            .map_err(|error| CatalogError::Internal(format!("discography encode: {error}")))?;
        self.upstream()
            .cache()
            .set_bytes(key, bytes, Policy::DISCOGRAPHY.fresh);
        Ok(())
    }

    /// `GET /artists/{artist_mbid}/releases`: one filtered page of the
    /// discography. A dead MusicBrainz falls back to the library's albums.
    pub async fn artist_releases(
        &self,
        raw_mbid: &str,
        offset: u32,
        limit: u32,
    ) -> Result<ArtistReleases, CatalogError> {
        let mbid = checked_mbid(raw_mbid, "artist")?;
        let (result, context) = scoped(self.build_releases(&mbid, offset, limit)).await;
        result.map(|mut page| {
            page.service_status = mapping::service_status(&context);
            page
        })
    }

    async fn build_releases(
        &self,
        mbid: &str,
        offset: u32,
        limit: u32,
    ) -> Result<ArtistReleases, CatalogError> {
        let listed = self
            .release_groups(mbid, RequestPriority::UserInitiated)
            .await
            .map(Some);
        let answer = self
            .musicbrainz_or_library(listed, || self.local_discography(mbid))
            .await?;
        let list = match answer {
            Answer::MusicBrainz(list) => list,
            Answer::Library(local) => {
                tracing::warn!(artist = %mbid, "musicbrainz unavailable; discography from the library");
                return Ok(paged(local, offset, limit, true, CatalogSource::Library));
            }
        };
        let discography = self.discography(&list).await;
        Ok(paged(
            discography,
            offset,
            limit,
            list.complete,
            CatalogSource::Musicbrainz,
        ))
    }

    /// Split a release-group list into sections, filtered by the
    /// release-type preferences (v2 `categorize_release_groups`) and
    /// flagged against the library and the request list.
    pub(super) async fn discography(&self, list: &ReleaseGroupList) -> Discography {
        let preferences = self.upstream().settings().preferences();
        let primary = mapping::type_set(&preferences.primary_types);
        let secondary = mapping::type_set(&preferences.secondary_types);
        if primary.is_empty() {
            return Discography::default();
        }
        let kept: Vec<&ReleaseGroupItem> = list
            .items
            .iter()
            .filter(|item| {
                mapping::should_include_release(
                    item.primary_type.as_deref(),
                    &item.secondary_types,
                    Some(&primary),
                    Some(&secondary),
                    false,
                )
            })
            .collect();
        let ids: Vec<String> = kept.iter().map(|item| item.id.clone()).collect();
        let (owned, requested) = self.album_flags(&ids).await;
        let mut sections = Discography::default();
        for item in kept {
            let id = item.id.to_ascii_lowercase();
            let in_library = owned.contains(&id);
            let release = ReleaseItem {
                id: item.id.clone(),
                title: item.title.clone(),
                release_type: item.primary_type.clone(),
                first_release_date: item.first_release_date.clone(),
                year: mapping::year_of(item.first_release_date.as_deref()),
                in_library,
                requested: !in_library && requested.contains(&id),
            };
            match item
                .primary_type
                .as_deref()
                .map(str::to_lowercase)
                .as_deref()
            {
                Some("album") => sections.albums.push(release),
                Some("ep") => sections.eps.push(release),
                Some("single") => sections.singles.push(release),
                _ => {}
            }
        }
        for section in [
            &mut sections.albums,
            &mut sections.eps,
            &mut sections.singles,
        ] {
            section.sort_by_key(|item| (item.year.is_none(), std::cmp::Reverse(item.year)));
        }
        sections
    }

    /// The library's identified albums by an artist, as a discography,
    /// for when MusicBrainz is down. `None` when the library has none.
    async fn local_discography(&self, mbid: &str) -> Result<Option<Discography>, CatalogError> {
        let local = self
            .local()
            .artist_albums(mbid)
            .await
            .map_err(CatalogError::database)?;
        if local.is_empty() {
            return Ok(None);
        }
        Ok(Some(Discography {
            albums: local
                .into_iter()
                .map(|album| ReleaseItem {
                    id: album.release_group_mbid,
                    title: Some(album.title),
                    release_type: None,
                    first_release_date: None,
                    year: album.year,
                    in_library: true,
                    requested: false,
                })
                .collect(),
            ..Discography::default()
        }))
    }

    /// `GET /artists/{artist_mbid}/lastfm`: Last.fm biography, tags and
    /// similar artists with the user's Last.fm key or the instance key.
    /// Empty when Last.fm is off, no key is usable,
    /// or Last.fm does not know the artist.
    pub async fn artist_lastfm(
        &self,
        user_id: &str,
        raw_mbid: &str,
        artist_name: &str,
    ) -> Result<LastFmArtistEnrichment, CatalogError> {
        let mbid = checked_mbid(raw_mbid, "artist")?;
        let Some((client, creds)) = self.upstream().lastfm(user_id).await else {
            return Ok(LastFmArtistEnrichment::default());
        };
        let key = CacheKey::new(Source::LastFm, &["artist_info", &mbid]);
        let name = artist_name.to_owned();
        let priority = RequestPriority::UserInitiated;
        let enrichment = self
            .cached(&key, LASTFM_DETAILS, priority, move |priority| async move {
                let Some(info) = client
                    .artist_info(&creds, &name, Some(&mbid), priority)
                    .await
                    .map_err(CatalogError::Unavailable)?
                else {
                    return Ok(Fetched::Missing);
                };
                let enrichment = LastFmArtistEnrichment {
                    bio: mapping::clean_lastfm_bio(if info.bio_content.is_empty() {
                        &info.bio_summary
                    } else {
                        &info.bio_content
                    }),
                    summary: mapping::clean_lastfm_bio(&info.bio_summary),
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
                    similar_artists: info
                        .similar
                        .into_iter()
                        .map(|similar| LastFmSimilarArtist {
                            name: similar.name,
                            mbid: similar.mbid,
                            match_score: similar.score,
                            url: Some(similar.url).filter(|url| !url.is_empty()),
                        })
                        .collect(),
                    url: Some(info.url).filter(|url| !url.is_empty()),
                };
                Ok(Fetched::Found(enrichment))
            })
            .await;
        // A failure was recorded where it happened; the page shows nothing.
        Ok(enrichment.ok().flatten().unwrap_or_default())
    }

    /// `GET /artists/{artist_mbid}/purchase-options`: the artist's own
    /// store pages from MusicBrainz, plus a Bandcamp search. A dead
    /// MusicBrainz leaves only the search.
    pub async fn artist_purchase_options(
        &self,
        raw_mbid: &str,
        fallback_name: &str,
    ) -> Result<ArtistPurchaseOptionsResponse, CatalogError> {
        let mbid = checked_mbid(raw_mbid, "artist")?;
        let detail = match self
            .artist_detail(&mbid, RequestPriority::UserInitiated)
            .await
        {
            Ok(detail) => detail,
            Err(error) => {
                tracing::warn!(artist = %mbid, %error, "artist store links unavailable");
                None
            }
        };
        let name = detail
            .as_ref()
            .map_or(fallback_name, |detail| detail.name.as_str())
            .trim()
            .to_owned();
        Ok(ArtistPurchaseOptionsResponse {
            links: detail.map(|detail| detail.store_links).unwrap_or_default(),
            bandcamp_search_url: if name.is_empty() {
                String::new()
            } else {
                format!(
                    "https://bandcamp.com/search?q={}&item_type=b",
                    mapping::quote_plus(&name)
                )
            },
        })
    }
}

/// Which discography section an item belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Album,
    Ep,
    Single,
}

/// Slice one page out of the discography, running albums, then EPs, then
/// singles.
fn paged(
    discography: Discography,
    offset: u32,
    limit: u32,
    complete: bool,
    source: CatalogSource,
) -> ArtistReleases {
    let tagged: Vec<(Section, ReleaseItem)> = discography
        .albums
        .into_iter()
        .map(|item| (Section::Album, item))
        .chain(discography.eps.into_iter().map(|item| (Section::Ep, item)))
        .chain(
            discography
                .singles
                .into_iter()
                .map(|item| (Section::Single, item)),
        )
        .collect();
    let total = u32::try_from(tagged.len()).unwrap_or(u32::MAX);
    let mut albums = Vec::new();
    let mut eps = Vec::new();
    let mut singles = Vec::new();
    let page: Vec<(Section, ReleaseItem)> = tagged
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    let returned = u32::try_from(page.len()).unwrap_or(u32::MAX);
    for (section, item) in page {
        match section {
            Section::Album => albums.push(item),
            Section::Ep => eps.push(item),
            Section::Single => singles.push(item),
        }
    }
    let next_offset = offset.checked_add(limit).filter(|next| *next < total);
    ArtistReleases {
        albums,
        singles,
        eps,
        offset,
        limit,
        returned_count: returned,
        next_offset,
        has_more: next_offset.is_some(),
        source_total_count: complete.then_some(total),
        warming: !complete,
        source,
        service_status: None,
    }
}
