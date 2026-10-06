//! Unified search: native `/api/v3/search` read surface.
//!
//! MusicBrainz artists and albums joined with the library's own artists,
//! albums and tracks: unified search, typeahead suggest, per-bucket
//! drill-down, and one single-method enrich-batch. MusicBrainz runs
//! through the [`catalog`](crate::reads::catalog) service; enrichment runs
//! behind the [`ports::EnrichmentPort`] seam.
//!
//! Wiring: [`router`] is mounted under `/api/v3` inside the session gate
//! via `ReadsSetup::search_router`, with the handler paths and model
//! schemas in the utoipa document. Auth is the deny-by-default middleware
//! alone: every role sees the same catalog.

pub mod error;
pub mod handlers;
pub mod models;
pub mod ports;
pub mod service;

use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};

/// Default per-bucket cap for unified search, kept from v2.
pub const DEFAULT_BUCKET_LIMIT: u32 = 10;
/// Ceiling for unified-search and drill-down limits, kept from v2.
pub const MAX_BUCKET_LIMIT: u32 = 100;
/// Default typeahead cap, kept from v2.
pub const DEFAULT_SUGGEST_LIMIT: u32 = 5;
/// Ceiling for typeahead limits, kept from v2.
pub const MAX_SUGGEST_LIMIT: u32 = 10;

/// Search dependencies, built once and injected by constructor. The pool
/// serves local catalog reads; the port answers enrichment (unconfigured
/// when no providers are wired); the ids mint error ids.
#[derive(Clone)]
pub struct SearchDeps {
    /// Local catalog search over the baseline pool.
    pub service: service::SearchService,
    /// Enrichment seam (the provider adapters implement this).
    pub enrichment: Arc<dyn ports::EnrichmentPort>,
    /// Fresh ids for error correlation.
    pub ids: Arc<dyn crate::ids::IdGenerator>,
}

impl SearchDeps {
    /// Wire search deps from their parts.
    pub fn new(
        service: service::SearchService,
        enrichment: Arc<dyn ports::EnrichmentPort>,
        ids: Arc<dyn crate::ids::IdGenerator>,
    ) -> Self {
        Self {
            service,
            enrichment,
            ids,
        }
    }
}

/// Mount the search routes. Registration order matters: the static
/// `suggest` and `enrich/batch` segments must win over the `{bucket}`
/// capture, which Axum's router guarantees by preferring static segments.
/// The session gate and 405 fallback are applied by the app, not here.
pub fn router(deps: SearchDeps) -> Router {
    Router::new()
        .route("/api/v3/search", get(handlers::search))
        .route("/api/v3/search/suggest", get(handlers::suggest))
        .route("/api/v3/search/enrich/batch", post(handlers::enrich_batch))
        .route("/api/v3/search/{bucket}", get(handlers::search_bucket))
        .with_state(deps)
}
