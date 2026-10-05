//! Item images (anonymous, like v2).

use crate::auth::compat_auth::jellyfin::JellyfinPasswordStore;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::Response;

use super::params::CiParams;
use super::router::*;
use super::seams::{IdMap, LibraryRead, PlaybackSessions, StreamEngine};

// ===== Images (anonymous) =====

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

pub(super) async fn image<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((item_id, image_type)): Path<(String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let query = request.uri().query().map(str::to_owned);
    serve_image(&state, query.as_deref(), &item_id, &image_type).await
}

/// Indexed variant: the index is accepted and ignored (v2 takes an int and
/// never reads it either).
pub(super) async fn image_indexed<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((item_id, image_type, _index)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let query = request.uri().query().map(str::to_owned);
    serve_image(&state, query.as_deref(), &item_id, &image_type).await
}

pub(super) async fn serve_image<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    query: Option<&str>,
    item_id: &str,
    image_type: &str,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(state) {
        return denied;
    }
    if !image_type.eq_ignore_ascii_case("primary") {
        return error(StatusCode::NOT_FOUND);
    }
    let Some((kind, internal)) = state.ids.from_jf(item_id).await else {
        return error(StatusCode::NOT_FOUND);
    };
    let q = CiParams::parse(query);
    let size = image_size(&q);
    let found = match kind.as_str() {
        "library" => Some(super::seams::CoverBytes {
            bytes: library_png(),
            content_type: "image/png".to_owned(),
        }),
        "album" => state.library.cover(&internal, size).await,
        "track" => match state.library.track("", &internal).await {
            Some(track) => match track.rg_mbid.as_deref() {
                Some(rg) => state.library.cover(rg, size).await,
                None => None,
            },
            None => None,
        },
        "artist" => state.library.artist_image(&internal).await,
        _ => None,
    };
    match found {
        Some(cover) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", cover.content_type)
            .header("Cache-Control", "public, max-age=31536000, immutable")
            .body(Body::from(cover.bytes))
            .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR)),
        None => error(StatusCode::NOT_FOUND),
    }
}
