//! slskd/Soulseek acquisition.
//!
//! The client surface is [`SlskdRepository`]: album and track acquisition
//! search over the query ladders, download enqueue/status keyed by
//! `(username, filenames)`, and the mount diagnosis. The quality band, the
//! saved recipe and the search timings come from [`DownloadPolicy`]. The
//! in-repo [`MockSlskd`] server is the executable record of the
//! live-verified quirks; the `acquire_slskd` contract tests run against it
//! on loopback and never touch a live slskd instance.

pub mod client;
pub mod folders;
pub mod http;
pub mod locate;
pub mod models;
pub mod policy;
pub mod query;
pub mod repository;

pub use client::SlskdClient;
pub use http::{HttpFault, HttpReply, ReqwestSlskdHttp, SlskdHttp};
pub use locate::Locator;
pub use models::{
    SlskdEnqueueResponse, SlskdFile, SlskdOptions, SlskdSearchResponse, SlskdTransfer,
    SlskdUserSearchResponse,
};
pub use policy::DownloadPolicy;
pub use query::{album_query_ladder, sanitize_query, stripped_album_title, track_query_ladder};
pub use repository::{
    EnqueueFile, MountDiagnosis, SearchResult, ServiceStatus, SlskdRepository, TaskHandle,
    TaskStatus, aggregate_status, extension_from_filename, match_transfers, state_flags,
};
