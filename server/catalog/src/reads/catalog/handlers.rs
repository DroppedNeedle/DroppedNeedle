//! Thin Axum handlers for the artist and album pages. Each extracts,
//! validates its parameters, calls the catalog, and renders; status
//! mapping lives in `From<CatalogError> for ApiError`.

use axum::{
    Json,
    extract::{Path, State},
};

use droppedneedle_auth::User;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidQuery;
use droppedneedle_providers::RequestPriority;

use super::CatalogDeps;
use super::error::CatalogError;
use super::models::{
    AlbumArtistQuery, AlbumBasicInfo, AlbumEditionsResponse, AlbumInfo, AlbumLastFmQuery,
    AlbumTracksInfo, ArtistExtendedInfo, ArtistInfo, ArtistLastFmQuery,
    ArtistPurchaseOptionsResponse, ArtistPurchaseQuery, ArtistReleases, DiscoveryQuery,
    LastFmAlbumEnrichment, LastFmArtistEnrichment, MoreByArtistResponse, PluginDetails,
    PurchaseOptionsResponse, ReleasesQuery, SimilarAlbumsResponse, SimilarArtistsResponse,
    TopAlbumsResponse, TopSongsResponse,
};

type Answer<T> = Result<Json<T>, ApiError>;

fn answer<T>(result: Result<T, CatalogError>) -> Answer<T> {
    result.map(Json).map_err(ApiError::from)
}

/// A count within `1..=max`, defaulting when absent.
fn count(value: Option<u32>, default: u32, max: u32) -> Result<u32, ApiError> {
    let value = value.unwrap_or(default);
    if value == 0 || value > max {
        return Err(ApiError::invalid_input(format!(
            "count must be between 1 and {max}"
        )));
    }
    Ok(value)
}

/// The artist header.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/artists/{artist_mbid}",
    params(("artist_mbid" = String, Path, description = "Artist MBID")),
    responses(
        (status = 200, description = "Artist header", body = ArtistInfo),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown artist"),
        (status = 503, description = "MusicBrainz down and the artist is not in the library"),
    )
)]
pub async fn artist(
    State(deps): State<CatalogDeps>,
    User(user): User,
    Path(mbid): Path<String>,
) -> Answer<ArtistInfo> {
    answer(deps.catalog.artist(user.user_id(), &mbid).await)
}

/// Biography, portrait and artist images.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/artists/{artist_mbid}/extended",
    params(("artist_mbid" = String, Path, description = "Artist MBID")),
    responses(
        (status = 200, description = "Biography and images; fields absent when unknown", body = ArtistExtendedInfo),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn artist_extended(
    State(deps): State<CatalogDeps>,
    Path(mbid): Path<String>,
) -> Answer<ArtistExtendedInfo> {
    answer(deps.catalog.artist_extended(&mbid).await)
}

/// One page of the discography.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/artists/{artist_mbid}/releases",
    params(("artist_mbid" = String, Path, description = "Artist MBID"), ReleasesQuery),
    responses(
        (status = 200, description = "Discography page", body = ArtistReleases),
        (status = 400, description = "Not a MusicBrainz id or bad paging"),
        (status = 401, description = "Not authenticated"),
        (status = 503, description = "MusicBrainz down and the library has no albums by the artist"),
    )
)]
pub async fn artist_releases(
    State(deps): State<CatalogDeps>,
    Path(mbid): Path<String>,
    ValidQuery(query): ValidQuery<ReleasesQuery>,
) -> Answer<ArtistReleases> {
    let limit = count(query.limit, 50, 200)?;
    answer(
        deps.catalog
            .artist_releases(&mbid, query.offset.unwrap_or(0), limit)
            .await,
    )
}

/// Similar artists.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/artists/{artist_mbid}/similar",
    params(("artist_mbid" = String, Path, description = "Artist MBID"), DiscoveryQuery),
    responses(
        (status = 200, description = "Similar artists", body = SimilarArtistsResponse),
        (status = 400, description = "Not a MusicBrainz id or bad count"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn similar_artists(
    State(deps): State<CatalogDeps>,
    User(user): User,
    Path(mbid): Path<String>,
    ValidQuery(query): ValidQuery<DiscoveryQuery>,
) -> Answer<SimilarArtistsResponse> {
    let count = count(query.count, 15, 50)?;
    answer(
        deps.catalog
            .similar_artists(
                user.user_id(),
                &mbid,
                count,
                query.source,
                RequestPriority::UserInitiated,
            )
            .await,
    )
}

/// The artist's most played songs.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/artists/{artist_mbid}/top-songs",
    params(("artist_mbid" = String, Path, description = "Artist MBID"), DiscoveryQuery),
    responses(
        (status = 200, description = "Top songs", body = TopSongsResponse),
        (status = 400, description = "Not a MusicBrainz id or bad count"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn top_songs(
    State(deps): State<CatalogDeps>,
    User(user): User,
    Path(mbid): Path<String>,
    ValidQuery(query): ValidQuery<DiscoveryQuery>,
) -> Answer<TopSongsResponse> {
    let count = count(query.count, 10, 50)?;
    answer(
        deps.catalog
            .top_songs(
                user.user_id(),
                &mbid,
                count,
                query.source,
                RequestPriority::UserInitiated,
            )
            .await,
    )
}

/// The artist's most played albums.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/artists/{artist_mbid}/top-albums",
    params(("artist_mbid" = String, Path, description = "Artist MBID"), DiscoveryQuery),
    responses(
        (status = 200, description = "Top albums", body = TopAlbumsResponse),
        (status = 400, description = "Not a MusicBrainz id or bad count"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn top_albums(
    State(deps): State<CatalogDeps>,
    User(user): User,
    Path(mbid): Path<String>,
    ValidQuery(query): ValidQuery<DiscoveryQuery>,
) -> Answer<TopAlbumsResponse> {
    let count = count(query.count, 10, 50)?;
    answer(
        deps.catalog
            .top_albums(
                user.user_id(),
                &mbid,
                count,
                query.source,
                RequestPriority::UserInitiated,
            )
            .await,
    )
}

/// Last.fm biography, tags and similar artists.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/artists/{artist_mbid}/lastfm",
    params(("artist_mbid" = String, Path, description = "Artist MBID"), ArtistLastFmQuery),
    responses(
        (status = 200, description = "Last.fm data; empty when Last.fm is not set up", body = LastFmArtistEnrichment),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn artist_lastfm(
    State(deps): State<CatalogDeps>,
    User(user): User,
    Path(mbid): Path<String>,
    ValidQuery(query): ValidQuery<ArtistLastFmQuery>,
) -> Answer<LastFmArtistEnrichment> {
    answer(
        deps.catalog
            .artist_lastfm(user.user_id(), &mbid, &query.artist_name)
            .await,
    )
}

/// The artist's own store pages.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/artists/{artist_mbid}/purchase-options",
    params(("artist_mbid" = String, Path, description = "Artist MBID"), ArtistPurchaseQuery),
    responses(
        (status = 200, description = "Store pages plus a Bandcamp search", body = ArtistPurchaseOptionsResponse),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn artist_purchase_options(
    State(deps): State<CatalogDeps>,
    Path(mbid): Path<String>,
    ValidQuery(query): ValidQuery<ArtistPurchaseQuery>,
) -> Answer<ArtistPurchaseOptionsResponse> {
    answer(
        deps.catalog
            .artist_purchase_options(&mbid, &query.name)
            .await,
    )
}

/// Header, tracklist and artwork in one call.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}",
    params(("album_id" = String, Path, description = "Release-group MBID, or a library album id identified as one")),
    responses(
        (status = 200, description = "Album page", body = AlbumInfo),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album"),
        (status = 503, description = "MusicBrainz down and the album is not in the library"),
    )
)]
pub async fn album(State(deps): State<CatalogDeps>, Path(id): Path<String>) -> Answer<AlbumInfo> {
    answer(deps.catalog.album(&id).await)
}

/// The album header.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}/basic",
    params(("album_id" = String, Path, description = "Release-group MBID, or a library album id identified as one")),
    responses(
        (status = 200, description = "Album header", body = AlbumBasicInfo),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album"),
        (status = 503, description = "MusicBrainz down and the album is not in the library"),
    )
)]
pub async fn album_basic(
    State(deps): State<CatalogDeps>,
    Path(id): Path<String>,
) -> Answer<AlbumBasicInfo> {
    answer(deps.catalog.album_basic(&id).await)
}

/// The tracklist of the edition the page shows.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}/tracks",
    params(("album_id" = String, Path, description = "Release-group MBID, or a library album id identified as one")),
    responses(
        (status = 200, description = "Tracklist", body = AlbumTracksInfo),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album"),
        (status = 503, description = "MusicBrainz down and the album is not in the library"),
    )
)]
pub async fn album_tracks(
    State(deps): State<CatalogDeps>,
    Path(id): Path<String>,
) -> Answer<AlbumTracksInfo> {
    answer(deps.catalog.album_tracks(&id).await)
}

/// Every edition of the album.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}/editions",
    params(("album_id" = String, Path, description = "Release-group MBID, or a library album id identified as one")),
    responses(
        (status = 200, description = "Editions", body = AlbumEditionsResponse),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album"),
        (status = 503, description = "MusicBrainz down"),
    )
)]
pub async fn album_editions(
    State(deps): State<CatalogDeps>,
    Path(id): Path<String>,
) -> Answer<AlbumEditionsResponse> {
    answer(deps.catalog.album_editions(&id).await)
}

/// The tracklist of one edition, any release, for comparing editions side
/// by side before choosing one.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}/editions/{release_mbid}/tracks",
    params(
        ("album_id" = String, Path, description = "Release-group MBID, or a library album id"),
        ("release_mbid" = String, Path, description = "Release MBID"),
    ),
    responses(
        (status = 200, description = "Tracklist", body = AlbumTracksInfo),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown release"),
        (status = 503, description = "MusicBrainz down"),
    )
)]
pub async fn edition_tracks(
    State(deps): State<CatalogDeps>,
    Path((_album, release)): Path<(String, String)>,
) -> Answer<AlbumTracksInfo> {
    answer(deps.catalog.edition_tracks(&release).await)
}

/// Drop the album's cached upstream answers and rebuild the header.
#[utoipa::path(
    post,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}/refresh",
    params(("album_id" = String, Path, description = "Release-group MBID, or a library album id identified as one")),
    responses(
        (status = 200, description = "Fresh album header", body = AlbumBasicInfo),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album"),
        (status = 503, description = "MusicBrainz down and the album is not in the library"),
    )
)]
pub async fn album_refresh(
    State(deps): State<CatalogDeps>,
    Path(id): Path<String>,
) -> Answer<AlbumBasicInfo> {
    answer(deps.catalog.album_refresh(&id).await)
}

/// Albums by similar artists.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}/similar",
    params(("album_id" = String, Path, description = "Release-group MBID"), AlbumArtistQuery),
    responses(
        (status = 200, description = "Similar albums", body = SimilarAlbumsResponse),
        (status = 400, description = "Not a MusicBrainz id or bad count"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn similar_albums(
    State(deps): State<CatalogDeps>,
    User(user): User,
    Path(id): Path<String>,
    ValidQuery(query): ValidQuery<AlbumArtistQuery>,
) -> Answer<SimilarAlbumsResponse> {
    let count = count(query.count, 10, 30)?;
    answer(
        deps.catalog
            .similar_albums(user.user_id(), &id, &query.artist_id, count)
            .await,
    )
}

/// The artist's other albums.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}/more-by-artist",
    params(("album_id" = String, Path, description = "Release-group MBID"), AlbumArtistQuery),
    responses(
        (status = 200, description = "Other albums by the artist", body = MoreByArtistResponse),
        (status = 400, description = "Not a MusicBrainz id or bad count"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn more_by_artist(
    State(deps): State<CatalogDeps>,
    Path(id): Path<String>,
    ValidQuery(query): ValidQuery<AlbumArtistQuery>,
) -> Answer<MoreByArtistResponse> {
    let count = count(query.count, 10, 30)?;
    answer(
        deps.catalog
            .more_by_artist(&id, &query.artist_id, count)
            .await,
    )
}

/// Last.fm summary and tags for the album.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}/lastfm",
    params(("album_id" = String, Path, description = "Release-group MBID, or a library album id identified as one"), AlbumLastFmQuery),
    responses(
        (status = 200, description = "Last.fm data; empty when Last.fm is not set up", body = LastFmAlbumEnrichment),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn album_lastfm(
    State(deps): State<CatalogDeps>,
    User(user): User,
    Path(id): Path<String>,
    ValidQuery(query): ValidQuery<AlbumLastFmQuery>,
) -> Answer<LastFmAlbumEnrichment> {
    answer(
        deps.catalog
            .album_lastfm(user.user_id(), &id, &query.artist_name, &query.album_name)
            .await,
    )
}

/// What metadata plugins add about the album: notes, tags, links and an
/// image. Empty when no plugin knows the album.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}/plugin-details",
    params(("album_id" = String, Path, description = "Release-group MBID, or a library album id identified as one")),
    responses(
        (status = 200, description = "Plugin details; empty when no plugin answers", body = PluginDetails),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 503, description = "MusicBrainz down"),
    )
)]
pub async fn album_plugin_details(
    State(deps): State<CatalogDeps>,
    Path(id): Path<String>,
) -> Answer<PluginDetails> {
    answer(deps.catalog.album_plugin_details(&id).await)
}

/// Where to buy the album.
#[utoipa::path(
    get,
    tag = "catalog",
    path = "/api/v3/albums/{album_id}/purchase-options",
    params(("album_id" = String, Path, description = "Release-group MBID, or a library album id identified as one")),
    responses(
        (status = 200, description = "Store links plus a Bandcamp search", body = PurchaseOptionsResponse),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 503, description = "MusicBrainz down"),
    )
)]
pub async fn album_purchase_options(
    State(deps): State<CatalogDeps>,
    Path(id): Path<String>,
) -> Answer<PurchaseOptionsResponse> {
    answer(deps.catalog.album_purchase_options(&id).await)
}
