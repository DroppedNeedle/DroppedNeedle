//! Thin Axum handlers for unified search.
//!
//! Each handler answers one route: extract, validate, call the service,
//! render. Status mapping lives in [`SearchError`](super::error::SearchError).
//! Authentication is the deny-by-default middleware alone (any signed-in
//! user, as in v2): these routes take no principal because every role sees the
//! same catalog and no per-user data is read or written.

use axum::{
    Json,
    extract::{FromRequest, Path, Query, Request, State},
};
use serde::de::DeserializeOwned;

use super::error::SearchError;
use super::models::{BucketQuery, EnrichmentBatchRequest, SearchQuery, SuggestQuery};
use super::service::{Bucket, BucketLimits, enrich_with_degradation};
use super::{DEFAULT_BUCKET_LIMIT, DEFAULT_SUGGEST_LIMIT, MAX_BUCKET_LIMIT, MAX_SUGGEST_LIMIT};

/// Query extractor that renders failures in the shared envelope instead
/// of Axum's default plain-text 400.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = SearchError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| SearchError::InvalidQuery {
                message: format!("Invalid query: {cause}"),
            })
    }
}

/// JSON body extractor that renders failures in the shared envelope.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = SearchError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| SearchError::InvalidQuery {
                message: format!("Invalid request body: {cause}"),
            })
    }
}

fn service_response<T: serde::Serialize>(
    result: Result<T, sqlx::Error>,
    deps: &super::SearchDeps,
) -> Result<Json<T>, SearchError> {
    result
        .map(Json)
        .map_err(|cause| SearchError::internal(&cause, deps.ids.as_ref()))
}

/// Ranked hits across the selected buckets plus each bucket's standout.
#[utoipa::path(
    get,
    path = "/api/v3/search",
    operation_id = "unified_search",
    params(SearchQuery),
    responses(
        (status = 200, description = "Ranked hits per bucket", body = super::models::SearchResponse),
        (status = 400, description = "Blank query or bad limit"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn search(
    State(deps): State<super::SearchDeps>,
    ValidQuery(query): ValidQuery<SearchQuery>,
) -> Result<Json<super::models::SearchResponse>, SearchError> {
    if query.q.trim().is_empty() {
        return Err(SearchError::InvalidQuery {
            message: "Query must not be blank".to_owned(),
        });
    }
    let limits = BucketLimits {
        artists: check_limit(query.limit_artists, DEFAULT_BUCKET_LIMIT)?,
        albums: check_limit(query.limit_albums, DEFAULT_BUCKET_LIMIT)?,
        tracks: check_limit(query.limit_tracks, DEFAULT_BUCKET_LIMIT)?,
    };
    let buckets = parse_buckets(query.buckets.as_deref())?;
    service_response(deps.service.search(&query.q, limits, &buckets).await, &deps)
}

/// One page of one bucket for drill-down views.
#[utoipa::path(
    get,
    path = "/api/v3/search/{bucket}",
    params(BucketQuery),
    responses(
        (status = 200, description = "One page of the bucket", body = super::models::SearchBucketResponse),
        (status = 400, description = "Blank query or bad limit"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown bucket"),
    )
)]
pub async fn search_bucket(
    State(deps): State<super::SearchDeps>,
    Path(bucket): Path<String>,
    ValidQuery(query): ValidQuery<BucketQuery>,
) -> Result<Json<super::models::SearchBucketResponse>, SearchError> {
    let bucket = Bucket::parse(&bucket).ok_or(SearchError::UnknownBucket)?;
    if query.q.trim().is_empty() {
        return Err(SearchError::InvalidQuery {
            message: "Query must not be blank".to_owned(),
        });
    }
    let limit = check_page_limit(query.limit)?;
    let offset = query.offset.unwrap_or(0);
    service_response(
        deps.service
            .search_bucket(bucket, &query.q, limit, offset)
            .await,
        &deps,
    )
}

/// Merged typeahead across buckets. Queries under two characters return
/// an empty 200, kept from v2 so typing never errors.
#[utoipa::path(
    get,
    path = "/api/v3/search/suggest",
    params(SuggestQuery),
    responses(
        (status = 200, description = "Merged suggestions", body = super::models::SuggestResponse),
        (status = 400, description = "Bad limit"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn suggest(
    State(deps): State<super::SearchDeps>,
    ValidQuery(query): ValidQuery<SuggestQuery>,
) -> Result<Json<super::models::SuggestResponse>, SearchError> {
    let limit = check_suggest_limit(query.limit)?;
    if query.q.trim().len() < 2 {
        return Ok(Json(super::models::SuggestResponse {
            results: Vec::new(),
            status: super::models::SearchRemoteStatus::Ok,
        }));
    }
    service_response(deps.service.suggest(&query.q, limit).await, &deps)
}

/// The single enrich-batch method: one POST enriches a mixed batch of
/// artists and albums. v2's duplicate GET form is gone; GET on this path
/// is a 405. Provider failures degrade into typed notes, never a 5xx.
#[utoipa::path(
    post,
    path = "/api/v3/search/enrich/batch",
    request_body = EnrichmentBatchRequest,
    responses(
        (status = 200, description = "Batch counts plus degradations", body = super::models::EnrichmentResponse),
        (status = 400, description = "Malformed batch body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn enrich_batch(
    State(deps): State<super::SearchDeps>,
    ValidJson(body): ValidJson<EnrichmentBatchRequest>,
) -> Json<super::models::EnrichmentResponse> {
    Json(enrich_with_degradation(deps.enrichment.as_ref(), deps.ids.as_ref(), body).await)
}

/// Cap a unified-search limit at its default and ceiling.
fn check_limit(value: Option<u32>, default: u32) -> Result<u32, SearchError> {
    let limit = value.unwrap_or(default);
    if limit > MAX_BUCKET_LIMIT {
        return Err(SearchError::InvalidQuery {
            message: format!("Limit must be between 0 and {MAX_BUCKET_LIMIT}"),
        });
    }
    Ok(limit)
}

/// Cap a drill-down page size. Zero pages answer nothing, so the floor is 1.
fn check_page_limit(value: Option<u32>) -> Result<u32, SearchError> {
    let limit = value.unwrap_or(50);
    if limit == 0 || limit > MAX_BUCKET_LIMIT {
        return Err(SearchError::InvalidQuery {
            message: format!("Limit must be between 1 and {MAX_BUCKET_LIMIT}"),
        });
    }
    Ok(limit)
}

/// Cap a typeahead limit.
fn check_suggest_limit(value: Option<u32>) -> Result<u32, SearchError> {
    let limit = value.unwrap_or(DEFAULT_SUGGEST_LIMIT);
    if limit == 0 || limit > MAX_SUGGEST_LIMIT {
        return Err(SearchError::InvalidQuery {
            message: format!("Limit must be between 1 and {MAX_SUGGEST_LIMIT}"),
        });
    }
    Ok(limit)
}

/// Parse the unified-search bucket filter. Absent means every bucket;
/// unknown tokens are a 400 naming the offender.
fn parse_buckets(filter: Option<&str>) -> Result<Vec<Bucket>, SearchError> {
    let Some(filter) = filter else {
        return Ok(vec![Bucket::Artists, Bucket::Albums, Bucket::Tracks]);
    };
    let mut buckets = Vec::new();
    for token in filter.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        buckets.push(
            Bucket::parse(token).ok_or_else(|| SearchError::InvalidQuery {
                message: format!("Unknown bucket: {token}"),
            })?,
        );
    }
    if buckets.is_empty() {
        return Ok(vec![Bucket::Artists, Bucket::Albums, Bucket::Tracks]);
    }
    Ok(buckets)
}
