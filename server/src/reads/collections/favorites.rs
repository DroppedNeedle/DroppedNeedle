//! Native favorites: albums, artists, and tracks by id.
//!
//! v2 has no native favorites endpoints (only remote-server ones, which
//! `remotes` serves), so this is new v3 surface. Favorites are strictly per-user: the
//! list shows only the caller's rows, and counts cover all kinds.

use axum::{
    Json,
    extract::{Path, State},
};

use super::{
    auth::Principal,
    error::{CollectionsError, ValidJson, ValidQuery},
    models::{
        FavoriteCounts, FavoriteItem, FavoriteListResponse, FavoriteQuery, FavoriteStatusResponse,
        SetFavoriteBody,
    },
    state::{CollectionsState, FavoriteRow, now_epoch, read_store, write_store},
};

/// Favoritable kinds.
const KINDS: [&str; 3] = ["album", "artist", "track"];

/// Validated kind or 400.
fn clean_kind(raw: &str) -> Result<String, CollectionsError> {
    if KINDS.contains(&raw) {
        Ok(raw.to_owned())
    } else {
        Err(CollectionsError::InvalidInput {
            message: "Kind must be album, artist, or track".to_owned(),
        })
    }
}

/// List the caller's favorites, optionally filtered by kind. Counts ignore
/// the filter so one call fills every tab.
pub fn list_favorites(
    state: &CollectionsState,
    caller: &Principal,
    query: &FavoriteQuery,
) -> Result<FavoriteListResponse, CollectionsError> {
    state.check_injection()?;
    let filter = query.kind.as_deref().map(clean_kind).transpose()?;
    let rows = read_store(&state.favorites.favorites, "favorite")?;
    let mut items = Vec::new();
    let mut counts = FavoriteCounts {
        album: 0,
        artist: 0,
        track: 0,
    };
    for ((user_id, kind, item_id), row) in rows.iter() {
        if *user_id != caller.user_id {
            continue;
        }
        match kind.as_str() {
            "album" => counts.album += 1,
            "artist" => counts.artist += 1,
            "track" => counts.track += 1,
            _ => {}
        }
        if filter.as_ref().is_some_and(|want| want != kind) {
            continue;
        }
        items.push(FavoriteItem {
            kind: kind.clone(),
            item_id: item_id.clone(),
            name: row.name.clone(),
            favorited_at: row.favorited_at,
        });
    }
    items.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.item_id.cmp(&b.item_id)));
    Ok(FavoriteListResponse { items, counts })
}

/// Favorite or unfavorite one item. Idempotent both ways.
pub fn set_favorite(
    state: &CollectionsState,
    caller: &Principal,
    kind: &str,
    item_id: &str,
    body: &SetFavoriteBody,
) -> Result<FavoriteStatusResponse, CollectionsError> {
    state.check_injection()?;
    let kind = clean_kind(kind)?;
    let key = (caller.user_id.clone(), kind.clone(), item_id.to_owned());
    let mut rows = write_store(&state.favorites.favorites, "favorite")?;
    if body.favorited {
        let name = body
            .name
            .clone()
            .or_else(|| rows.get(&key).and_then(|existing| existing.name.clone()));
        rows.insert(
            key,
            FavoriteRow {
                name,
                favorited_at: now_epoch(),
            },
        );
    } else {
        rows.remove(&key);
    }
    Ok(FavoriteStatusResponse {
        kind,
        item_id: item_id.to_owned(),
        favorited: body.favorited,
    })
}

/// List the caller's favorites.
#[utoipa::path(
    get,
    path = "/api/v3/favorites",
    params(("kind" = Option<String>, Query, description = "Kind filter: album, artist, or track")),
    responses(
        (status = 200, description = "Caller favorites", body = FavoriteListResponse),
        (status = 400, description = "Bad kind filter"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_favorites_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    ValidQuery(query): ValidQuery<FavoriteQuery>,
) -> Result<Json<FavoriteListResponse>, CollectionsError> {
    list_favorites(&state, &caller, &query).map(Json)
}

/// Favorite or unfavorite one item.
#[utoipa::path(
    put,
    path = "/api/v3/favorites/{kind}/{item_id}",
    params(
        ("kind" = String, Path, description = "Kind: album, artist, or track"),
        ("item_id" = String, Path, description = "Favorited item id")
    ),
    request_body = SetFavoriteBody,
    responses(
        (status = 200, description = "Favorite status", body = FavoriteStatusResponse),
        (status = 400, description = "Bad kind or body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn set_favorite_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path((kind, item_id)): Path<(String, String)>,
    ValidJson(body): ValidJson<SetFavoriteBody>,
) -> Result<Json<FavoriteStatusResponse>, CollectionsError> {
    set_favorite(&state, &caller, &kind, &item_id, &body).map(Json)
}
