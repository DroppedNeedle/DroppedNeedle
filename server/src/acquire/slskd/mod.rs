//! slskd/Soulseek acquisition.
//!
//! The client surface is [`SlskdRepository`]: album and track acquisition
//! search over the query ladders, download enqueue/status keyed by
//! `(username, filenames)`, and the mount diagnosis. Quality tiers, the
//! closed quality recipe, and timeouts come from [`DownloadPolicy`]. The
//! in-repo [`MockSlskd`] server is the executable record of the
//! live-verified quirks; the `acquire_slskd` contract tests run against it
//! on loopback and never touch a live slskd instance.

pub mod client;
pub mod error;
pub mod folders;
pub mod http;
pub mod locate;
#[cfg(any(test, feature = "test-support"))]
pub mod mock;
pub mod models;
pub mod policy;
pub mod query;
pub mod repository;

pub use client::SlskdClient;
pub use error::SlskdError;
pub use http::{HttpFault, HttpReply, ReqwestSlskdHttp, SlskdHttp};
pub use locate::Locator;
#[cfg(any(test, feature = "test-support"))]
pub use mock::{MOCK_API_KEY, MockSlskd, REJECT_MARKER};
pub use models::{
    SlskdEnqueueResponse, SlskdFile, SlskdOptions, SlskdSearchResponse, SlskdTransfer,
    SlskdUserSearchResponse,
};
pub use policy::{
    DownloadPolicy, QualityRecipeEntry, RecipeError, lossless_detail_step, validate_quality_recipe,
};
pub use query::{album_query_ladder, sanitize_query, stripped_album_title, track_query_ladder};
pub use repository::{
    EnqueueFile, MountDiagnosis, SearchResult, ServiceStatus, SlskdRepository, TaskHandle,
    TaskStatus, aggregate_status, extension_from_filename, match_transfers, state_flags,
};
