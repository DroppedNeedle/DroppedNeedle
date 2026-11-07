//! OpenAPI document built with utoipa.
//!
//! The document served at `/openapi.json` is the single contract source:
//! `server/openapi/check.sh` generates TypeScript from it, and CI fails on
//! drift between the committed files and a fresh generation.

use utoipa::{
    Modify, OpenApi, ToSchema,
    openapi::{
        Content, RefOr, Response,
        schema::{Ref, Schema},
        security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme},
    },
};

use droppedneedle_kernel::{
    error::ErrorEnvelope,
    mount::{SESSION_SCHEME, UPGRADE_SESSION_SCHEME, WRAPPED_KEY_SCHEME},
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
        droppedneedle_catalog::metadata::models::SourceStatus,
        // The library lists' query enums: a query parameter names its
        // schema, but the path macro does not collect it.
        droppedneedle_catalog::local::models::AlbumListSort,
        droppedneedle_catalog::local::models::ArtistListSort,
        droppedneedle_catalog::local::models::ArtistListScope,
        droppedneedle_catalog::local::models::TrackListSort,
        droppedneedle_catalog::local::models::ListOrder,
        droppedneedle_catalog::local::models::FormatMode,
        droppedneedle_catalog::local::models::TracksMode,
        droppedneedle_catalog::local::models::RatingMode,
        // The `details` of a refused custom CSS sheet.
        crate::appearance::models::CssRefusal,
        crate::appearance::check::CssProblem,
    )),
    info(
        title = "DroppedNeedle v3",
        version = "3.0.0",
        description = "Native API contract. Generated clients must never be hand-edited."
    )
)]
struct Unreferenced;

/// Native API contract document: [`Unreferenced`] plus every mounted
/// area's routes and upgrade mode's.
pub struct ApiDoc;

impl OpenApi for ApiDoc {
    fn openapi() -> utoipa::openapi::OpenApi {
        let mut doc = Unreferenced::openapi();
        doc.merge(crate::app::document());
        doc.merge(crate::upgrade_mode::routes::document());
        ErrorBodies.modify(&mut doc);
        SecuritySchemes.modify(&mut doc);
        OptionalRefDescriptions.modify(&mut doc);
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
        schemes.insert(
            UPGRADE_SESSION_SCHEME.to_owned(),
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "droppedneedle_upgrade",
                "The upgrade page's own sign-in, served only by a server started on v2 data, \
                 as `droppedneedle_upgrade` or `__Secure-droppedneedle_upgrade`. No session \
                 satisfies it.",
            ))),
        );
    }
}

/// Every 4xx and 5xx answer is the shared error envelope, so the document
/// names its schema once here instead of on each route. A HEAD answer has
/// no body, and neither do the errors in [`BODILESS_ERRORS`].
struct ErrorBodies;

/// Error answers sent with no body, as (path, status). The held file's
/// audio answers a range outside the file with a bare 416, as a player
/// expects. The image routes answer missing art with a bare 404, so an
/// `<img>` draws nothing and the web UI keeps its own placeholder.
const BODILESS_ERRORS: &[(&str, &str)] = &[
    ("/api/v3/held/files/{file_id}/audio", "416"),
    ("/api/v3/covers/release-group/{release_group_id}", "404"),
    ("/api/v3/covers/release/{release_id}", "404"),
    ("/api/v3/covers/artist/{artist_id}", "404"),
    ("/api/v3/library/albums/{id}/artwork", "404"),
    ("/api/v3/library/artists/{id}/artwork", "404"),
];

impl Modify for ErrorBodies {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let envelope = Ref::from_schema_name(ErrorEnvelope::name());
        for (path, item) in &mut openapi.paths.paths {
            let operations = [
                &mut item.get,
                &mut item.put,
                &mut item.post,
                &mut item.delete,
                &mut item.options,
                &mut item.patch,
                &mut item.trace,
            ];
            for operation in operations.into_iter().flatten() {
                for (status, response) in &mut operation.responses.responses {
                    let bodiless = BODILESS_ERRORS.contains(&(path.as_str(), status.as_str()));
                    if let RefOr::T(Response { content, .. }) = response
                        && (status.starts_with('4') || status.starts_with('5'))
                        && content.is_empty()
                        && !bodiless
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

/// utoipa puts the doc comment of an optional field whose type is a named
/// schema on the `$ref` inside the field's `oneOf`, where the TypeScript
/// generator does not read it. This moves it up to the field.
struct OptionalRefDescriptions;

impl Modify for OptionalRefDescriptions {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components
                .schemas
                .values_mut()
                .for_each(lift_ref_descriptions);
        }
    }
}

fn lift_ref_descriptions(schema: &mut RefOr<Schema>) {
    let RefOr::T(schema) = schema else {
        return;
    };
    match schema {
        Schema::Object(object) => {
            for property in object.properties.values_mut() {
                if let RefOr::T(Schema::OneOf(one_of)) = property
                    && one_of.description.is_none()
                {
                    one_of.description = one_of.items.iter_mut().find_map(|item| match item {
                        RefOr::Ref(reference) if !reference.description.is_empty() => {
                            Some(std::mem::take(&mut reference.description))
                        }
                        _ => None,
                    });
                }
                lift_ref_descriptions(property);
            }
        }
        Schema::AllOf(all_of) => all_of.items.iter_mut().for_each(lift_ref_descriptions),
        Schema::OneOf(one_of) => one_of.items.iter_mut().for_each(lift_ref_descriptions),
        _ => {}
    }
}
