//! OpenAPI document built with utoipa.
//!
//! The document served at `/openapi.json` is the single contract source:
//! `server/openapi/check.sh` generates TypeScript from it, and CI fails on
//! drift between the committed files and a fresh generation.

use utoipa::{
    Modify, OpenApi, ToSchema,
    openapi::{
        Content, RefOr, Response,
        schema::Ref,
        security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme},
    },
};

use droppedneedle_kernel::{
    error::ErrorEnvelope,
    mount::{SESSION_SCHEME, WRAPPED_KEY_SCHEME},
};

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
        // The value type of the `service_status` maps; no operation refers
        // to it by name.
        droppedneedle_catalog::reads::catalog::models::SourceStatus,
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
        SecuritySchemes.modify(&mut doc);
        doc
    }
}

/// The schemes each operation's `security` names. The mount stamps every
/// operation with its posture group (`kernel::mount`).
struct SecuritySchemes;

impl Modify for SecuritySchemes {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let schemes = &mut openapi
            .components
            .get_or_insert_with(Default::default)
            .security_schemes;
        schemes.insert(
            SESSION_SCHEME.to_owned(),
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "A session token. A browser sends the same session as the \
                         `droppedneedle_sid` or `__Secure-droppedneedle_sid` cookie. \
                         The scope names the posture, not a stored role: `user` is any \
                         signed-in account, `curator` is trusted or admin, `admin` is \
                         admin only.",
                    ))
                    .build(),
            ),
        );
        schemes.insert(
            WRAPPED_KEY_SCHEME.to_owned(),
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-Wrapped-API-Key",
                "The shared secret of the wrapped API. No session satisfies it.",
            ))),
        );
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
