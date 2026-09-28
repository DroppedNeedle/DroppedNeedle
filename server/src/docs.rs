//! OpenAPI document built with utoipa.
//!
//! The document served at `/openapi.json` is the single contract source:
//! `server/openapi/check.sh` generates TypeScript from it, and CI fails on
//! drift between the committed files and a fresh generation.

use utoipa::OpenApi;

use crate::{
    error::{ErrorBody, ErrorEnvelope},
    handlers::HealthResponse,
};

/// Native API contract document.
#[derive(OpenApi)]
#[openapi(
    paths(crate::handlers::health),
    components(schemas(HealthResponse, ErrorEnvelope, ErrorBody)),
    info(
        title = "DroppedNeedle v3",
        version = "3.0.0",
        description = "Native API contract. Generated clients must never be hand-edited."
    )
)]
pub struct ApiDoc;
