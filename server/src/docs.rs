//! OpenAPI document built with utoipa.
//!
//! The document served at `/openapi.json` is the single contract source:
//! `server/openapi/check.sh` generates TypeScript from it, and CI fails on
//! drift between the committed files and a fresh generation.

use utoipa::{
    Modify, OpenApi, ToSchema,
    openapi::{Content, RefOr, Response, schema::Ref},
};

use droppedneedle_kernel::error::ErrorEnvelope;

/// The info block, and the schemas no operation references. Every other
/// schema comes in with the operations that name it. Each one here is kept
/// for the reason beside it.
#[derive(OpenApi)]
#[openapi(
    components(schemas(
        // The error envelope: `ErrorBodies` names it on every 4xx and 5xx
        // answer after the operations are collected.
        ErrorEnvelope,
        droppedneedle_kernel::error::ErrorBody,
        // Event stream payloads: the stream answers `text/event-stream`, so
        // no operation names them, and the web UI types its events from
        // these.
        droppedneedle_kernel::events::ActivityChanged,
        droppedneedle_kernel::events::WantedNotice,
        droppedneedle_kernel::events::AutoDownloadEnqueued,
        droppedneedle_kernel::events::RequestImported,
        droppedneedle_kernel::events::PlaylistImported,
        droppedneedle_kernel::events::DropImportUpdated,
        droppedneedle_kernel::events::PersonalMixRefreshed,
        droppedneedle_kernel::events::ConcertsNew,
        droppedneedle_kernel::events::DownloadProgress,
        droppedneedle_kernel::events::DownloadsChanged,
        droppedneedle_kernel::events::SearchJobUpdated,
        // Chart query enums the web UI imports by name; no operation refers
        // to them.
        droppedneedle_catalog::reads::discover::models::ChartRange,
        droppedneedle_catalog::reads::discover::models::ChartSource,
        // The value type of the `service_status` maps; no operation refers
        // to it by name.
        droppedneedle_catalog::reads::catalog::models::SourceStatus,
        // Query structs: the operations list their fields as parameters,
        // and the schemas stay so the document does not change here.
        droppedneedle_catalog::reads::collections::models::FavoriteQuery,
        droppedneedle_acquire::requests::models::HistoryQuery,
        droppedneedle_acquire::requests::models::KindQuery,
        droppedneedle_media::playback::models::NowPlayingDeleteQuery,
        droppedneedle_auth::routes::models::OidcCallbackQuery,
        // The avatar upload names its own `AvatarUploadBody`; this older
        // shape stays so the document does not change here.
        droppedneedle_auth::users::models::AvatarUpload,
    )),
    info(
        title = "DroppedNeedle v3",
        version = "3.0.0",
        description = "Native API contract. Generated clients must never be hand-edited."
    )
)]
struct Unreferenced;

/// Native API contract document: [`Unreferenced`] plus every mounted
/// area's routes.
pub struct ApiDoc;

impl OpenApi for ApiDoc {
    fn openapi() -> utoipa::openapi::OpenApi {
        let mut doc = Unreferenced::openapi();
        doc.merge(crate::app::document());
        ErrorBodies.modify(&mut doc);
        doc
    }
}

/// Every 4xx and 5xx answer is the shared error envelope, so the document
/// names its schema once here instead of on each route.
struct ErrorBodies;

impl Modify for ErrorBodies {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let envelope = Ref::from_schema_name(ErrorEnvelope::name());
        for item in openapi.paths.paths.values_mut() {
            let operations = [
                &mut item.get,
                &mut item.put,
                &mut item.post,
                &mut item.delete,
                &mut item.options,
                &mut item.head,
                &mut item.patch,
                &mut item.trace,
            ];
            for operation in operations.into_iter().flatten() {
                for (status, response) in &mut operation.responses.responses {
                    if let RefOr::T(Response { content, .. }) = response
                        && (status.starts_with('4') || status.starts_with('5'))
                        && content.is_empty()
                    {
                        content.insert(
                            "application/json".to_owned(),
                            Content::new(Some(envelope.clone())),
                        );
                    }
                }
            }
        }
    }
}
