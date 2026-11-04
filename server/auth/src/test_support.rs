//! What tests need from auth and production never uses: the statement
//! registry, a principal for the test sign-in layer, sessions seeded with
//! their ids, and emptying the request limiter between steps.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

use droppedneedle_kernel::db::statement::StatementFile;

use crate::session::extract::Transport;
use crate::session::rate_limit::{BoundedMap, RateLimiter};
use crate::session::store::{SessionRecord, SessionStoreError};
use crate::sqlite::Sessions;
use crate::users::roles::{Principal, Role, SessionKind};

/// Every `sql.rs` of this crate, one line each, for the schema test.
pub fn statement_files() -> Vec<StatementFile> {
    vec![crate::sqlite::sql::registered()]
}

impl Principal {
    /// A signed-in principal for the test sign-in layer, which stands in for
    /// the session gate on a slice router.
    pub fn for_tests(
        user_id: &str,
        role: Role,
        session_id: &str,
        session_kind: SessionKind,
    ) -> Self {
        Self::from_session(
            user_id.to_owned(),
            None,
            None,
            role,
            session_id.to_owned(),
            session_kind,
            Transport::Bearer,
        )
    }

    /// The same test principal shown under `username_display`.
    #[must_use]
    pub fn with_username_display(mut self, username_display: &str) -> Self {
        self.username_display = Some(username_display.to_owned());
        self
    }
}

impl Sessions {
    /// Store a row exactly as given, its id included. The server stores
    /// sessions through [`Sessions::insert`] and the other writers, which
    /// take the id from the generator.
    pub async fn seed(&self, record: SessionRecord) -> Result<(), SessionStoreError> {
        self.insert_record(record).await
    }
}

impl RateLimiter {
    /// Drop every bucket, as a freshly built limiter would start. Test
    /// rigs that drive one shared router through hundreds of requests
    /// call this between requests instead of rebuilding the whole app.
    pub fn reset(&self) {
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *buckets = BoundedMap::default();
    }
}
