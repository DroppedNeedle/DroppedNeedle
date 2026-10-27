//! Item images. The address answers without sign-in, as on a real
//! Jellyfin, because Finamp and Jellify load pictures with no token. It
//! reads no token and never answers 401 (a 401 counts as a failed sign-in),
//! and it answers only to ids this server hands out: album and artist ids
//! are made with the data key, so nobody without an account can work one
//! out. A missing album or artist picture is a 404 that names the item;
//! everything else is the same empty 404, except a failed read, which is a
//! server error.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use droppedneedle_kernel::error::ApiError;

use super::params::CiParams;
use super::router::*;

// ===== Images (no sign-in) =====

/// The only types a picture goes out under.
const IMAGE_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// The cache of a picture asked for with a `tag`: the tag changes with the
/// picture, so the answer may be kept for a year.
const TAGGED_CACHE: &str = "public, max-age=31536000, immutable";

/// The cache of a picture asked for without a `tag` (Finamp never sends
/// one): the address stays the same when the picture changes, so a day.
const UNTAGGED_CACHE: &str = "public, max-age=86400";

/// The cache rule for a picture request. Jellyfin 10.10.6 checks no tag
/// and turns on its one-year cache only when a request carries one
/// (`ImageController.cs`, `GetImageInternal` and `GetImageResult`,
/// re-read 2026-10-27); without one it sends plain `public`. A day here
/// lets a changed cover reach an app that sends no tag within a day.
fn cache_control(q: &CiParams) -> &'static str {
    if q.get("tag").is_some_and(|tag| !tag.is_empty()) {
        TAGGED_CACHE
    } else {
        UNTAGGED_CACHE
    }
}

/// 1x1 opaque PNG for the library view's advertised `ImageTags.Primary`, so
/// the request resolves instead of 404ing (v2 `_LIBRARY_COVER_PNG`).
pub(super) const LIBRARY_COVER_PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC";

pub(super) fn library_png() -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(LIBRARY_COVER_PNG_B64)
        .unwrap_or_default()
}

/// Size bucket from the first present width/height-ish param (v2
/// `_image_size`).
pub(super) fn image_size(q: &CiParams) -> &'static str {
    for key in [
        "fillWidth",
        "maxWidth",
        "width",
        "fillHeight",
        "maxHeight",
        "height",
    ] {
        if let Some(raw) = q.get(key)
            && !raw.is_empty()
            && raw.bytes().all(|b| b.is_ascii_digit())
            && let Ok(px) = raw.parse::<u32>()
        {
            return if px <= 300 {
                "250"
            } else if px <= 750 {
                "500"
            } else {
                "1200"
            };
        }
    }
    "500"
}

pub(super) async fn image(
    State(state): State<JellyfinState>,
    Path((item_id, image_type)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let query = request.uri().query().map(str::to_owned);
    serve_image(&state, query.as_deref(), &item_id, &image_type).await
}

/// Indexed variant: the index is accepted and ignored (v2 takes an int and
/// never reads it either).
pub(super) async fn image_indexed(
    State(state): State<JellyfinState>,
    Path((item_id, image_type, _index)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let query = request.uri().query().map(str::to_owned);
    serve_image(&state, query.as_deref(), &item_id, &image_type).await
}

pub(super) async fn serve_image(
    state: &JellyfinState,
    query: Option<&str>,
    item_id: &str,
    image_type: &str,
) -> Result<Response, ApiError> {
    if !image_type.eq_ignore_ascii_case("primary") {
        return Ok(error(StatusCode::NOT_FOUND));
    }
    let Some((kind, internal)) = state.ids.from_jf_anonymous(item_id).await? else {
        return Ok(error(StatusCode::NOT_FOUND));
    };
    let q = CiParams::parse(query);
    let size = image_size(&q);
    let found = match kind.as_str() {
        "library" => Some(super::seams::CoverBytes {
            bytes: library_png(),
            content_type: "image/png".to_owned(),
        }),
        "album" => state.library.cover(&internal, size).await?,
        "artist" => state.library.artist_image(&internal, size).await?,
        _ => None,
    };
    Ok(match found {
        Some(cover) if IMAGE_TYPES.contains(&cover.content_type.as_str()) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", cover.content_type)
            .header("Cache-Control", cache_control(&q))
            .header("X-Content-Type-Options", "nosniff")
            .header("Content-Security-Policy", "sandbox; default-src 'none'")
            .body(Body::from(cover.bytes))
            .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR)),
        // A missing picture on an album or an artist names it, as Jellyfin
        // does. Other kinds stay an empty 404.
        None if matches!(kind.as_str(), "album" | "artist") => {
            match state.library.name_of(&kind, &internal).await? {
                Some(name) => json(
                    StatusCode::NOT_FOUND,
                    &format!("{name} does not have an image of type Primary"),
                ),
                None => error(StatusCode::NOT_FOUND),
            }
        }
        _ => error(StatusCode::NOT_FOUND),
    })
}
