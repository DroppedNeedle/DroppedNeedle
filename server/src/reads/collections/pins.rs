//! Edition pins: display lane only.
//!
//! A pin steers the soft display hint (`selected_release_mbid`): the pinned
//! release when set, else the catalog default. It never becomes catalog
//! identity. Enforcement is structural: the pin service takes only the pin
//! store plus the read-only edition catalog, so no pin code path can even
//! name the identity store. The pin-hint brief then asserts the identity
//! table stays byte-identical with zero writes across pin set and clear.
//!
//! Setting and clearing are curator-gated (v2 parity); reading is open to
//! any authenticated user. Edition acquisition dispatch is stage 7, not here.

use axum::{
    Json,
    extract::{Path, State},
};

use super::{
    auth::Principal,
    error::{CollectionsError, ValidJson},
    models::{EditionPinBody, EditionPinResponse},
    state::{
        CollectionsState, EditionCatalog, PinHintRow, PinHintStore, now_epoch, read_store,
        write_store,
    },
};

/// Pin service. Holds only the pin store and the read-only catalog: no
/// identity access exists at this type, so pins cannot write identity rows.
pub struct PinHintService<'a> {
    /// Pin-hint rows.
    pins: &'a PinHintStore,
    /// Read-only edition catalog.
    catalog: &'a EditionCatalog,
}

impl<'a> PinHintService<'a> {
    /// Build the service from its two stores. Callers pass narrow references;
    /// the identity store is not among them by construction.
    pub fn new(pins: &'a PinHintStore, catalog: &'a EditionCatalog) -> Self {
        Self { pins, catalog }
    }

    /// Display answer for one album: the pin when set, else the catalog
    /// default. Unknown albums are 404.
    pub fn display(&self, album_id: &str) -> Result<EditionPinResponse, CollectionsError> {
        let albums = read_store(&self.catalog.albums, "edition-catalog")?;
        let entry = albums.get(album_id).ok_or(CollectionsError::NotFound)?;
        let pins = read_store(&self.pins.pins, "pin-hint")?;
        let pinned = pins
            .get(album_id)
            .map(|row| row.pinned_release_mbid.clone());
        let (selected, hint_source) = match pinned.clone() {
            Some(pinned) => (Some(pinned), "pin"),
            None => (Some(entry.default_release_mbid.clone()), "default"),
        };
        Ok(EditionPinResponse {
            album_id: album_id.to_owned(),
            pinned_release_mbid: pinned,
            selected_release_mbid: selected,
            hint_source: hint_source.to_owned(),
        })
    }

    /// Pin one edition. The release must be a known edition of the album.
    pub fn set_pin(
        &self,
        album_id: &str,
        caller: &Principal,
        body: &EditionPinBody,
    ) -> Result<EditionPinResponse, CollectionsError> {
        caller.require_curator()?;
        let albums = read_store(&self.catalog.albums, "edition-catalog")?;
        let entry = albums.get(album_id).ok_or(CollectionsError::NotFound)?;
        if !entry.editions.iter().any(|mbid| mbid == &body.release_mbid) {
            return Err(CollectionsError::InvalidInput {
                message: "Unknown edition for this album".to_owned(),
            });
        }
        write_store(&self.pins.pins, "pin-hint")?.insert(
            album_id.to_owned(),
            PinHintRow {
                pinned_release_mbid: body.release_mbid.clone(),
                pinned_by: caller.user_id.clone(),
                pinned_at: now_epoch(),
            },
        );
        Ok(EditionPinResponse {
            album_id: album_id.to_owned(),
            pinned_release_mbid: Some(body.release_mbid.clone()),
            selected_release_mbid: Some(body.release_mbid.clone()),
            hint_source: "pin".to_owned(),
        })
    }

    /// Clear the pin. Unknown albums are 404; clearing an unpinned album is
    /// a no-op returning the default display.
    pub fn clear_pin(
        &self,
        album_id: &str,
        caller: &Principal,
    ) -> Result<EditionPinResponse, CollectionsError> {
        caller.require_curator()?;
        let albums = read_store(&self.catalog.albums, "edition-catalog")?;
        let entry = albums.get(album_id).ok_or(CollectionsError::NotFound)?;
        write_store(&self.pins.pins, "pin-hint")?.remove(album_id);
        Ok(EditionPinResponse {
            album_id: album_id.to_owned(),
            pinned_release_mbid: None,
            selected_release_mbid: Some(entry.default_release_mbid.clone()),
            hint_source: "default".to_owned(),
        })
    }
}

/// Read the pin display lane for one album.
#[utoipa::path(
    get,
    path = "/api/v3/library/albums/{album_id}/edition-pin",
    params(("album_id" = String, Path, description = "Album id")),
    responses(
        (status = 200, description = "Pin display", body = EditionPinResponse),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn get_pin_handler(
    State(state): State<CollectionsState>,
    _caller: Principal,
    Path(album_id): Path<String>,
) -> Result<Json<EditionPinResponse>, CollectionsError> {
    state.check_injection()?;
    PinHintService::new(&state.pins, &state.edition_catalog)
        .display(&album_id)
        .map(Json)
}

/// Pin one edition for an album.
#[utoipa::path(
    put,
    path = "/api/v3/library/albums/{album_id}/edition-pin",
    params(("album_id" = String, Path, description = "Album id")),
    request_body = EditionPinBody,
    responses(
        (status = 200, description = "Pin display", body = EditionPinResponse),
        (status = 400, description = "Unknown edition for this album"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn set_pin_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(album_id): Path<String>,
    ValidJson(body): ValidJson<EditionPinBody>,
) -> Result<Json<EditionPinResponse>, CollectionsError> {
    state.check_injection()?;
    PinHintService::new(&state.pins, &state.edition_catalog)
        .set_pin(&album_id, &caller, &body)
        .map(Json)
}

/// Clear the pin for an album.
#[utoipa::path(
    delete,
    path = "/api/v3/library/albums/{album_id}/edition-pin",
    params(("album_id" = String, Path, description = "Album id")),
    responses(
        (status = 200, description = "Pin display", body = EditionPinResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn clear_pin_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(album_id): Path<String>,
) -> Result<Json<EditionPinResponse>, CollectionsError> {
    state.check_injection()?;
    PinHintService::new(&state.pins, &state.edition_catalog)
        .clear_pin(&album_id, &caller)
        .map(Json)
}
