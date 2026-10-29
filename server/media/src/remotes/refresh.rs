//! Media-server rescans after a Library Management run.
//!
//! The library queues one job per server on the jobs table when a run
//! finishes ([`MEDIA_SERVER_REFRESH`], keyed by the server), and a burst of
//! runs joins the one queued job. This handler sends it with the admin's
//! shared credential, through the server's own client. Retries follow the
//! kind's backoff; a server that is not set up, refuses the credential or
//! has no such call fails for good, so it is never retried for ever.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;

use super::adapter::AdapterError;
use super::connections::{ConnectionResolver, Reads, ResolveError};
use super::models::SourceName;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobHandler, KindSpec, Outcome, PAYLOAD_INVALID};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::LibraryManagement;
use droppedneedle_library::management::after_apply::{
    MEDIA_SERVER_REFRESH, RefreshCode, RefreshJob, RefreshTarget,
};

/// How rescans run: the default attempts and backoff, and room for Plex's
/// section list and one call per section.
pub const SPEC: KindSpec =
    KindSpec::new(MEDIA_SERVER_REFRESH).run_timeout(Duration::from_secs(120));

/// Sends each queued rescan.
#[derive(Clone)]
pub struct RefreshJobs {
    resolver: Arc<ConnectionResolver>,
    http: reqwest::Client,
    settings: Live<LibraryManagement>,
}

impl RefreshJobs {
    /// Rescans through the app's one connection resolver, over `http`,
    /// while `settings` still switch the server on.
    pub fn new(
        resolver: Arc<ConnectionResolver>,
        http: reqwest::Client,
        settings: Live<LibraryManagement>,
    ) -> Self {
        Self {
            resolver,
            http,
            settings,
        }
    }

    async fn send(&self, target: RefreshTarget) -> Outcome {
        // An unreadable section is never read as its defaults: the rescan
        // waits for it.
        let Some(settings) = self.settings.read() else {
            return retry(RefreshCode::Failed);
        };
        if !RefreshTarget::switched_on(&settings.external_refresh).contains(&target) {
            return fail(RefreshCode::NotConfigured);
        }
        let source = match target {
            RefreshTarget::Jellyfin => SourceName::Jellyfin,
            RefreshTarget::Plex => SourceName::Plex,
            RefreshTarget::Navidrome => SourceName::Navidrome,
        };
        let connection = match self.resolver.resolve_shared(source) {
            Ok(connection) => connection,
            Err(ResolveError::NotConfigured | ResolveError::Stale) => {
                return fail(RefreshCode::NotConfigured);
            }
            Err(ResolveError::Store(cause)) => {
                tracing::warn!(server = target.as_str(), %cause, "media server rescan waits");
                return retry(RefreshCode::Failed);
            }
        };
        let client = self
            .resolver
            .client(&self.http, connection, Reads::Background);
        let Err(error) = client.rescan().await else {
            return Outcome::Done;
        };
        tracing::info!(server = target.as_str(), %error, "media server rescan did not go through");
        match error {
            AdapterError::NotConfigured | AdapterError::Unsupported(_) => {
                fail(RefreshCode::NotConfigured)
            }
            AdapterError::Auth => fail(RefreshCode::AuthFailed),
            AdapterError::NotFound => fail(RefreshCode::NotFound),
            AdapterError::Transport(_) => retry(RefreshCode::Unreachable),
            AdapterError::Api(_) => retry(RefreshCode::Failed),
        }
    }
}

fn fail(code: RefreshCode) -> Outcome {
    Outcome::Fail {
        code: code.as_str(),
    }
}

fn retry(code: RefreshCode) -> Outcome {
    Outcome::Retry {
        code: code.as_str(),
        at: None,
    }
}

impl JobHandler for RefreshJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let jobs = self.clone();
        Box::pin(async move {
            match job.payload::<RefreshJob>() {
                Ok(RefreshJob { target }) => jobs.send(target).await,
                Err(_) => Outcome::Fail {
                    code: PAYLOAD_INVALID,
                },
            }
        })
    }
}
