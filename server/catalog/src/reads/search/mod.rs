//! Unified search: native `/api/v3/search` read surface.
//!
//! MusicBrainz artists and albums joined with the library's own artists,
//! albums and tracks: unified search, typeahead suggest, per-bucket
//! drill-down, and one single-method enrich-batch. MusicBrainz runs
//! through the [`catalog`](crate::reads::catalog) service; listen counts
//! come from [`enrichment::SearchCounts`] behind the
//! [`ports::EnrichmentPort`] seam.
//!
//! Wiring: the app mounts [`routes`] inside the session gate, which also
//! registers the paths in the API document. Auth is the session gate
//! alone: every role sees the same catalog.

pub mod enrichment;
pub mod error;
pub mod handlers;
pub mod models;
pub mod ports;
pub mod service;

use std::sync::Arc;

use droppedneedle_kernel::mount::{Routes, routes};

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
/// when no providers are wired).
#[derive(Clone)]
pub struct SearchDeps {
    /// Local catalog search over the baseline pool.
    pub service: service::SearchService,
    /// Enrichment seam (the provider adapters implement this).
    pub enrichment: Arc<dyn ports::EnrichmentPort>,
}

impl SearchDeps {
    /// Wire search deps from their parts.
    pub fn new(
        service: service::SearchService,
        enrichment: Arc<dyn ports::EnrichmentPort>,
    ) -> Self {
        Self {
            service,
            enrichment,
        }
    }
}

/// The search routes, for any signed-in user. The static `suggest` and
/// `enrich/batch` segments win over the `{bucket}` capture: Axum's router
/// prefers static segments.
pub fn routes() -> Routes<SearchDeps> {
    use handlers::*;

    Routes::new()
        .user(routes!(search))
        .user(routes!(suggest))
        .user(routes!(enrich_batch))
        .user(routes!(search_bucket))
}
