//! Artist pages: header, biography, discography, Last.fm and store links.

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::providers::degradation::scoped;
use crate::providers::lastfm;
use crate::providers::musicbrainz::{Criticality, MAX_PAGE_LIMIT, MbArtist, ReleaseGroupRef};
use crate::providers::wikidata::WikidataClient;
use crate::providers::{IntegrationStatus, RequestPriority, record_current};

use super::error::CatalogError;
use super::mapping;
use super::models::{
    ArtistExtendedInfo, ArtistImages, ArtistInfo, ArtistPurchaseOptionsResponse, ArtistReleases,
    CatalogSource, ExternalLink, LastFmArtistEnrichment, LastFmSimilarArtist, LastFmTag, LifeSpan,
    PurchaseLink, ReleaseItem,
};
use super::{Catalog, MISS_TTL, is_mbid, mb_error, mb_retry, record_mb_down, secs};

/// Most discography pages fetched per artist (v2 `_MAX_RG_PAGES`).
const MAX_RELEASE_GROUP_PAGES: u32 = 10;
/// How long a partial discography stands in while the rest is fetched.
const PARTIAL_DISCOGRAPHY_TTL: Duration = Duration::from_secs(60);
/// Last.fm answers are kept an hour (v2 `LASTFM_ENTITY_CACHE_TTL`).
pub(super) const LASTFM_TTL: Duration = Duration::from_secs(3600);
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseGroupList {
    /// Release groups in MusicBrainz order, duplicates dropped.
    pub items: Vec<ReleaseGroupItem>,
    /// Whether every page has been fetched.
    pub complete: bool,
}

/// The cached biography and portrait.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Biography {
    /// Wikipedia introduction.
    pub description: Option<String>,
    /// Wikidata/Commons portrait.
    pub image: Option<String>,
}

/// One cached TheAudioDB artist lookup: images, or a remembered miss.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AudioDbArtistEntry {
    images: Option<ArtistImages>,
}

fn artist_ttl(catalog: &Catalog, in_library: bool) -> Duration {
    let advanced = catalog.upstream().settings().advanced();
    secs(if in_library {
        advanced.cache_ttl_artist_library
    } else {
        advanced.cache_ttl_artist_non_library
    })
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
    /// The cached MusicBrainz artist detail, `None` for a definitive miss.
    pub(super) async fn artist_detail(&self, mbid: &str) -> Result<ArtistCore, CatalogError> {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let key = format!("mb:artist:detail:{namespace}:{mbid}");
        let catalog = self.clone();
        let mbid = mbid.to_owned();
        self.cached(&self.inner.flights.artist, key, move || async move {
            let (client, _) = catalog
                .upstream()
                .musicbrainz(RequestPriority::UserInitiated);
            let found = mb_retry(|| {
                client.lookup_artist(
                    &mbid,
                    &["aliases", "tags", "url-rels"],
                    Criticality::IdentityCritical,
                )
            })
            .await
            .map_err(mb_error)?;
            match found {
                None => Ok((None, Some(MISS_TTL))),
                Some(lookup) => {
                    let detail = ArtistDetail::from_wire(lookup.entity);
                    let owned = catalog
                        .artist_flags(std::slice::from_ref(&detail.mbid))
                        .await;
                    let ttl = artist_ttl(&catalog, !owned.is_empty());
                    Ok((Some(detail), Some(ttl)))
                }
            }
        })
        .await
    }

    /// `GET /artists/{artist_mbid}`: the header. A dead MusicBrainz falls
    /// back to the library's own row for the artist.
    pub async fn artist(&self, raw_mbid: &str) -> Result<ArtistInfo, CatalogError> {
        let mbid = checked_mbid(raw_mbid, "artist")?;
        let (result, context) = scoped(self.build_artist(&mbid)).await;
        result.map(|mut info| {
            info.service_status = mapping::service_status(&context);
            info
        })
    }

    async fn build_artist(&self, mbid: &str) -> Result<ArtistInfo, CatalogError> {
        let detail = match self.artist_detail(mbid).await {
            Ok(Some(detail)) => detail,
            Ok(None) => return Err(CatalogError::NotFound),
            Err(error) => {
                record_mb_down(&error);
                if matches!(error, CatalogError::Unavailable(_))
                    && let Some(local) = self
                        .local()
                        .artist(mbid)
                        .await
                        .map_err(CatalogError::database)?
                {
                    tracing::warn!(artist = %mbid, "musicbrainz unavailable; artist page from the library");
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
                        in_library: true,
                        source: CatalogSource::Library,
                        service_status: None,
                    });
                }
                return Err(error);
            }
        };
        let in_library = !self
            .artist_flags(std::slice::from_ref(&detail.mbid))
            .await
            .is_empty();
        let images = self
            .cached_artist_images(&detail.mbid)
            .await
            .unwrap_or_default();
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
            source: CatalogSource::Musicbrainz,
            service_status: None,
        })
    }

    /// TheAudioDB images already in the cache; never dials out, so the
    /// header stays fast.
    async fn cached_artist_images(&self, mbid: &str) -> Option<ArtistImages> {
        let bytes = self
            .upstream()
            .cache()
            .get_bytes(&audiodb_artist_key(mbid))
            .await?;
        serde_json::from_slice::<AudioDbArtistEntry>(&bytes)
            .ok()?
            .images
    }

    /// TheAudioDB images, fetched and cached with v2's found/miss
    /// lifetimes. Off or failing reads as no images.
    async fn fetch_artist_images(&self, mbid: &str, name: &str) -> Option<ArtistImages> {
        let advanced = self.upstream().settings().advanced();
        let client = self.upstream().audiodb(&advanced)?;
        let key = audiodb_artist_key(mbid);
        if let Some(bytes) = self.upstream().cache().get_bytes(&key).await
            && let Ok(entry) = serde_json::from_slice::<AudioDbArtistEntry>(&bytes)
        {
            return entry.images;
        }
        let mut outcome = client.artist_by_mbid(mbid).await;
        if matches!(outcome, crate::providers::audiodb::Outcome::Missing)
            && advanced.audiodb_name_search_fallback
            && !name.is_empty()
        {
            outcome = client.search_artist(name).await;
        }
        let (images, ttl) = match outcome {
            crate::providers::audiodb::Outcome::Found(artist) => (
                Some(ArtistImages {
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
                secs(advanced.cache_ttl_audiodb_found),
            ),
            crate::providers::audiodb::Outcome::Missing => {
                (None, secs(advanced.cache_ttl_audiodb_not_found))
            }
            // Already recorded by the client; nothing cached so the next
            // visit tries again.
            crate::providers::audiodb::Outcome::Unavailable { .. } => return None,
        };
        if let Ok(bytes) = serde_json::to_vec(&AudioDbArtistEntry {
            images: images.clone(),
        }) {
            self.upstream().cache().set_bytes(&key, bytes, ttl).await;
        }
        images
    }

    /// `GET /artists/{artist_mbid}/extended`: biography and portrait from
    /// Wikipedia/Wikidata, plus TheAudioDB images. Every failure reads as
    /// absent fields (v2 answered empty rather than erroring).
    pub async fn artist_extended(
        &self,
        raw_mbid: &str,
    ) -> Result<ArtistExtendedInfo, CatalogError> {
        let mbid = checked_mbid(raw_mbid, "artist")?;
        let detail = match self.artist_detail(&mbid).await {
            Ok(Some(detail)) => detail,
            Ok(None) => return Ok(ArtistExtendedInfo::default()),
            Err(error) => {
                tracing::warn!(artist = %mbid, %error, "extended artist info unavailable");
                return Ok(ArtistExtendedInfo::default());
            }
        };
        let (biography, images) = tokio::join!(
            self.biography(&detail),
            self.fetch_artist_images(&detail.mbid, &detail.name)
        );
        Ok(ArtistExtendedInfo {
            description: biography.description,
            image: biography.image,
            images: images.unwrap_or_default(),
        })
    }

    async fn biography(&self, detail: &ArtistDetail) -> Biography {
        if detail.wiki_url.is_none() && detail.wikidata_id.is_none() {
            return Biography::default();
        }
        let key = format!("artist_info:extended:{}", detail.mbid);
        let catalog = self.clone();
        let detail = detail.clone();
        let fetched = self
            .cached(&self.inner.flights.extended, key, move || async move {
                let http = catalog.upstream().http_get();
                let endpoints = catalog.upstream().endpoints();
                let wiki = WikidataClient::with_bases(
                    &http,
                    &endpoints.wikidata,
                    &endpoints.wikipedia,
                    &endpoints.commons,
                );
                let description = match &detail.wiki_url {
                    Some(url) => wiki.get_bio_extract(url, BIO_LANGUAGE).await,
                    None => Ok(None),
                };
                let image = match &detail.wikidata_id {
                    Some(id) => wiki.get_artist_image(id).await,
                    None => Ok(None),
                };
                let failed = description.is_err() || image.is_err();
                if failed {
                    record_current("wikidata", IntegrationStatus::Error, false);
                }
                let biography = Biography {
                    description: description.ok().flatten().filter(|text| !text.is_empty()),
                    image: image.ok().flatten(),
                };
                let ttl = (!failed).then(|| artist_ttl(&catalog, false));
                Ok((biography, ttl))
            })
            .await;
        fetched.unwrap_or_default()
    }

    /// An artist's release groups: the first page right away, the rest by
    /// a background walker (cached when complete).
    pub(super) async fn release_groups(
        &self,
        mbid: &str,
    ) -> Result<ReleaseGroupList, CatalogError> {
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let key = format!("mb:artist_rgs:{namespace}:{mbid}");
        let catalog = self.clone();
        let artist = mbid.to_owned();
        let store_key = key.clone();
        self.cached(
            &self.inner.flights.release_groups,
            key,
            move || async move {
                let (client, _) = catalog
                    .upstream()
                    .musicbrainz(RequestPriority::UserInitiated);
                let page = mb_retry(|| {
                    client.browse_artist_release_groups(
                        &artist,
                        MAX_PAGE_LIMIT,
                        0,
                        Criticality::IdentityCritical,
                    )
                })
                .await
                .map_err(mb_error)?;
                let total = page.count;
                let mut seen = HashSet::new();
                let items: Vec<ReleaseGroupItem> = page
                    .items
                    .into_iter()
                    .filter(|group| seen.insert(group.id.to_ascii_lowercase()))
                    .map(ReleaseGroupItem::from_wire)
                    .collect();
                if total <= items.len() as u64 {
                    let ttl = artist_ttl(&catalog, false);
                    return Ok((
                        ReleaseGroupList {
                            items,
                            complete: true,
                        },
                        Some(ttl),
                    ));
                }
                catalog.spawn_discography_walker(artist, store_key, items.clone(), total);
                Ok((
                    ReleaseGroupList {
                        items,
                        complete: false,
                    },
                    Some(PARTIAL_DISCOGRAPHY_TTL),
                ))
            },
        )
        .await
    }

    /// Fetch the rest of a large discography at background priority and
    /// cache the whole list. One walker per artist; a failure logs and
    /// leaves the partial list to expire so the next visit tries again.
    fn spawn_discography_walker(
        &self,
        artist: String,
        key: String,
        seed: Vec<ReleaseGroupItem>,
        total: u64,
    ) {
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
            let outcome = catalog.walk_discography(&artist, &key, seed, total).await;
            if let Err(error) = outcome {
                tracing::warn!(artist = %artist, %error, "discography walk failed; partial list stays");
            }
            catalog
                .inner
                .warming
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&key);
        });
    }

    async fn walk_discography(
        &self,
        artist: &str,
        key: &str,
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
            let page = mb_retry(|| {
                client.browse_artist_release_groups(
                    artist,
                    MAX_PAGE_LIMIT,
                    offset,
                    Criticality::IdentityCritical,
                )
            })
            .await
            .map_err(mb_error)?;
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
        };
        let bytes = serde_json::to_vec(&list)
            .map_err(|error| CatalogError::Internal(format!("discography encode: {error}")))?;
        self.upstream()
            .cache()
            .set_bytes(key, bytes, artist_ttl(self, false))
            .await;
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
        let preferences = self.upstream().settings().preferences();
        let primary = mapping::type_set(&preferences.primary_types);
        let secondary = mapping::type_set(&preferences.secondary_types);
        if primary.is_empty() {
            return Ok(paged(
                Vec::new(),
                offset,
                limit,
                true,
                CatalogSource::Musicbrainz,
            ));
        }
        let list = match self.release_groups(mbid).await {
            Ok(list) => list,
            Err(error) => {
                record_mb_down(&error);
                if matches!(error, CatalogError::Unavailable(_)) {
                    let local = self
                        .local()
                        .artist_albums(mbid)
                        .await
                        .map_err(CatalogError::database)?;
                    if !local.is_empty() {
                        tracing::warn!(artist = %mbid, "musicbrainz unavailable; discography from the library");
                        let tagged = local
                            .into_iter()
                            .map(|album| {
                                (
                                    Section::Album,
                                    ReleaseItem {
                                        id: album.release_group_mbid,
                                        title: Some(album.title),
                                        release_type: None,
                                        first_release_date: None,
                                        year: album.year,
                                        in_library: true,
                                        requested: false,
                                    },
                                )
                            })
                            .collect();
                        return Ok(paged(tagged, offset, limit, true, CatalogSource::Library));
                    }
                }
                return Err(error);
            }
        };
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
        let mut albums = Vec::new();
        let mut eps = Vec::new();
        let mut singles = Vec::new();
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
                Some("album") => albums.push(release),
                Some("ep") => eps.push(release),
                Some("single") => singles.push(release),
                _ => {}
            }
        }
        for section in [&mut albums, &mut eps, &mut singles] {
            section.sort_by_key(|item| (item.year.is_none(), std::cmp::Reverse(item.year)));
        }
        let tagged: Vec<(Section, ReleaseItem)> = albums
            .into_iter()
            .map(|item| (Section::Album, item))
            .chain(eps.into_iter().map(|item| (Section::Ep, item)))
            .chain(singles.into_iter().map(|item| (Section::Single, item)))
            .collect();
        Ok(paged(
            tagged,
            offset,
            limit,
            list.complete,
            CatalogSource::Musicbrainz,
        ))
    }

    /// `GET /artists/{artist_mbid}/lastfm`: Last.fm biography, tags and
    /// similar artists with the user's own key. Empty when Last.fm is off,
    /// the user has no key, or Last.fm does not know the artist.
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
        let key = format!("lfm_artist_info:{mbid}");
        let name = artist_name.to_owned();
        let value = self
            .cached(&self.inner.flights.other, key, move || async move {
                let info = match client.artist_info(&creds, &name, Some(&mbid)).await {
                    lastfm::Outcome::Found(info) => info,
                    lastfm::Outcome::Missing => {
                        return Ok((serde_json::Value::Null, Some(LASTFM_TTL)));
                    }
                    lastfm::Outcome::Unavailable { .. } => {
                        return Ok((serde_json::Value::Null, None));
                    }
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
                let value = serde_json::to_value(&enrichment)
                    .map_err(|error| CatalogError::Internal(format!("lastfm encode: {error}")))?;
                Ok((value, Some(LASTFM_TTL)))
            })
            .await?;
        Ok(serde_json::from_value(value).unwrap_or_default())
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
        let detail = match self.artist_detail(&mbid).await {
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

fn audiodb_artist_key(mbid: &str) -> String {
    format!("audiodb_artist:{}", mbid.to_ascii_lowercase())
}

/// Which discography section an item belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Album,
    Ep,
    Single,
}

/// Slice one page out of the section-ordered list.
fn paged(
    tagged: Vec<(Section, ReleaseItem)>,
    offset: u32,
    limit: u32,
    complete: bool,
    source: CatalogSource,
) -> ArtistReleases {
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
