//! An artist's MusicBrainz link, read by anyone signed in and set or
//! cleared by a curator.
//!
//! - `GET /library/artists/{id}/musicbrainz-link`: the link, who set it
//!   and the revision a change names.
//! - `PUT` on the same path: link the artist by hand. The app's settling
//!   never moves a person's link.
//! - `DELETE` on the same path: clear a hand-set link; the app links the
//!   artist again where its albums and files prove one.
//!
//! Neither change ever merges artist records: one that would group the
//! artist with another is refused, and an admin merges them in Duplicate
//! artists.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};

use super::blocking;
use super::error::refused;
use crate::identify::artist_links::{
    ArtistLink, ArtistLinkError, LinkRefusal, LinkSource, StoredLink,
};
use crate::operations::reasons;
use crate::service::{Identification, ServiceError};
use crate::wiring::LibraryServices;
use droppedneedle_auth::{Curator, User};

/// Artist link routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .user(routes!(get_artist_link))
        .curator(routes!(set_artist_link, clear_artist_link))
}

/// Who decided an artist's link.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtistLinkSourceView {
    /// The app, from album matches and files. It follows them.
    App,
    /// A person. The app never changes it.
    Person,
    /// A person's choice carried over from v2. The app never changes it.
    V2,
}

/// Who set an artist's link, as the caller may know it.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtistLinkByView {
    /// The caller.
    You,
    /// Another person.
    Person,
    /// The app.
    App,
    /// Carried over from v2.
    V2,
}

/// An artist's MusicBrainz link.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ArtistLinkView {
    pub artist_id: String,
    /// The revision a change must name.
    pub artist_revision: i64,
    /// The linked MusicBrainz artist id; null when there is none.
    pub musicbrainz_artist_id: Option<String>,
    /// Null when there is no link.
    pub source: Option<ArtistLinkSourceView>,
    /// Null when there is no link.
    pub set_by: Option<ArtistLinkByView>,
    /// When it was set, unix milliseconds; null when there is no link.
    pub selected_at: Option<i64>,
}

/// Link an artist to a MusicBrainz artist.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetArtistLinkBody {
    /// The MusicBrainz artist id.
    pub musicbrainz_artist_id: String,
    /// The `artist_revision` as last read.
    pub expected_revision: i64,
}

/// The revision a clear names.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ArtistLinkRevisionQuery {
    /// The `artist_revision` as last read.
    pub expected_revision: i64,
}

fn view(artist_id: String, link: ArtistLink, caller_user_id: &str) -> ArtistLinkView {
    let by = |stored: &StoredLink| match stored.source {
        LinkSource::App => ArtistLinkByView::App,
        LinkSource::V2 => ArtistLinkByView::V2,
        LinkSource::Person if stored.selected_by.as_deref() == Some(caller_user_id) => {
            ArtistLinkByView::You
        }
        LinkSource::Person => ArtistLinkByView::Person,
    };
    ArtistLinkView {
        artist_id,
        artist_revision: link.artist_revision,
        musicbrainz_artist_id: link.link.as_ref().map(|stored| stored.mbid.clone()),
        source: link.link.as_ref().map(|stored| match stored.source {
            LinkSource::App => ArtistLinkSourceView::App,
            LinkSource::Person => ArtistLinkSourceView::Person,
            LinkSource::V2 => ArtistLinkSourceView::V2,
        }),
        set_by: link.link.as_ref().map(by),
        selected_at: link.link.as_ref().map(|stored| stored.selected_at_ms),
    }
}

impl From<ArtistLinkError> for ApiError {
    fn from(error: ArtistLinkError) -> Self {
        let refusal = match error {
            ArtistLinkError::Store(error) => return ServiceError::from(error).into(),
            ArtistLinkError::Refused(refusal) => refusal,
        };
        match refusal {
            LinkRefusal::NotFound => ApiError::not_found(),
            LinkRefusal::InvalidId => {
                refused(StatusCode::BAD_REQUEST, reasons::ARTIST_MBID_INVALID)
            }
            LinkRefusal::SpecialPurpose => {
                refused(StatusCode::BAD_REQUEST, reasons::ARTIST_MBID_SPECIAL)
            }
            LinkRefusal::NotLinkable => refused(StatusCode::CONFLICT, reasons::ARTIST_NOT_LINKABLE),
            LinkRefusal::Stale => refused(StatusCode::CONFLICT, reasons::ARTIST_LINK_STALE),
            LinkRefusal::NotPersons => {
                refused(StatusCode::CONFLICT, reasons::ARTIST_LINK_NOT_CHOSEN)
            }
            LinkRefusal::Shared(others) => {
                let reason = reasons::ARTIST_LINK_SHARED;
                let artists: Vec<_> = others
                    .into_iter()
                    .map(|(id, name)| json!({ "id": id, "name": name }))
                    .collect();
                ApiError::new(StatusCode::CONFLICT, reason.code, reason.message)
                    .with_details(json!({ "action": reason.action, "artists": artists }))
            }
        }
    }
}

/// An artist's MusicBrainz link: the id, who set it and when.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/artists/{id}/musicbrainz-link",
    params(("id" = String, Path, description = "Local artist id")),
    responses(
        (status = 200, description = "The artist's link", body = ArtistLinkView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown artist"),
    )
)]
pub async fn get_artist_link(
    State(identification): State<Identification>,
    User(caller): User,
    Path(artist_id): Path<String>,
) -> Result<Json<ArtistLinkView>, ApiError> {
    let identify = identification.identify.clone();
    let artist = artist_id.clone();
    let link = blocking(move || identify.artist_link(&artist).map_err(ServiceError::from))
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(view(artist_id, link, caller.user_id())))
}

/// Link an artist to a MusicBrainz artist by hand. The app's own linking
/// never changes it. Refused when another artist in the library is that
/// MusicBrainz artist: an admin merges them instead.
#[utoipa::path(
    put,
    tag = "library management",
    path = "/api/v3/library/artists/{id}/musicbrainz-link",
    params(("id" = String, Path, description = "Local artist id")),
    request_body = SetArtistLinkBody,
    responses(
        (status = 200, description = "The artist's link", body = ArtistLinkView),
        (status = 400, description = "Not a MusicBrainz artist id, or a special-purpose one (ARTIST_MBID_INVALID, ARTIST_MBID_SPECIAL)"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown artist"),
        (status = 409, description = "The artist changed since it was read (STALE_REVISION), cannot be linked (ARTIST_NOT_LINKABLE), or other artists are that MusicBrainz artist (ARTIST_LINK_SHARED, listed in details.artists)"),
    )
)]
pub async fn set_artist_link(
    State(identification): State<Identification>,
    caller: Curator,
    Path(artist_id): Path<String>,
    ValidJson(body): ValidJson<SetArtistLinkBody>,
) -> Result<Json<ArtistLinkView>, ApiError> {
    let identify = identification.identify.clone();
    let (artist, user) = (artist_id.clone(), caller.user_id().to_owned());
    let link = blocking(move || {
        identify.set_artist_link(
            &artist,
            &body.musicbrainz_artist_id,
            body.expected_revision,
            &user,
        )
    })
    .await?;
    Ok(Json(view(artist_id, link, caller.user_id())))
}

/// Clear an artist's hand-set link. The app then links the artist again
/// where its albums and files prove one. Refused when that would make it
/// the same MusicBrainz artist as another in the library.
#[utoipa::path(
    delete,
    tag = "library management",
    path = "/api/v3/library/artists/{id}/musicbrainz-link",
    params(("id" = String, Path, description = "Local artist id"), ArtistLinkRevisionQuery),
    responses(
        (status = 200, description = "The artist's link after the clear", body = ArtistLinkView),
        (status = 400, description = "No revision given"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown artist"),
        (status = 409, description = "The artist changed since it was read (STALE_REVISION), has no hand-set link (ARTIST_LINK_NOT_CHOSEN), or would be the same MusicBrainz artist as others (ARTIST_LINK_SHARED, listed in details.artists)"),
    )
)]
pub async fn clear_artist_link(
    State(identification): State<Identification>,
    caller: Curator,
    Path(artist_id): Path<String>,
    ValidQuery(query): ValidQuery<ArtistLinkRevisionQuery>,
) -> Result<Json<ArtistLinkView>, ApiError> {
    let identify = identification.identify.clone();
    let (artist, user) = (artist_id.clone(), caller.user_id().to_owned());
    let link =
        blocking(move || identify.clear_artist_link(&artist, query.expected_revision, &user))
            .await?;
    Ok(Json(view(artist_id, link, caller.user_id())))
}
