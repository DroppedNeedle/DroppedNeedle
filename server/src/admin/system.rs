//! `GET /api/v3/system/health`: which external services are degraded right
//! now. Drives the header status dot.
//!
//! Ports v2's `api/v1/routes/system.py` health route: same payload, same
//! posture (any signed-in user, since every user sees the dot). The data
//! comes from the process [`ServiceHealth`](crate::providers::health::ServiceHealth)
//! registry the provider clients feed.

use std::time::Instant;

use axum::{Json, extract::State};
use serde::Serialize;
use utoipa::ToSchema;

use super::AdminSetup;
use crate::providers::health::DegradedService;

/// One degraded service capability (v2 `ServiceHealthItem`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ServiceHealthItem {
    /// Lowercase service key, e.g. `musicbrainz`.
    pub service: String,
    /// What is affected, e.g. `metadata`.
    pub capability: String,
    /// Always `degraded`; v2 also defined `down` but never sent it.
    pub severity: String,
    /// One user-facing line.
    pub message: String,
    /// Service used instead. No v3 source names one yet, so it is null.
    pub fallback: Option<String>,
    /// Seconds since the service was first seen degraded.
    pub degraded_seconds: u64,
}

impl From<DegradedService> for ServiceHealthItem {
    fn from(entry: DegradedService) -> Self {
        Self {
            service: entry.service.to_owned(),
            capability: entry.capability.to_owned(),
            severity: "degraded".to_owned(),
            message: entry.message.to_owned(),
            fallback: None,
            degraded_seconds: entry.degraded_for.as_secs(),
        }
    }
}

/// Degraded services, empty when everything is healthy (v2
/// `SystemHealthResponse`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SystemHealthResponse {
    /// Live degraded entries, sorted by service then capability.
    pub degraded: Vec<ServiceHealthItem>,
}

/// Which external services are degraded right now.
#[utoipa::path(
    get,
    path = "/api/v3/system/health",
    responses(
        (status = 200, description = "Degraded services", body = SystemHealthResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn system_health(State(admin): State<AdminSetup>) -> Json<SystemHealthResponse> {
    let degraded = admin
        .providers
        .health
        .current(Instant::now())
        .into_iter()
        .map(ServiceHealthItem::from)
        .collect();
    Json(SystemHealthResponse { degraded })
}
