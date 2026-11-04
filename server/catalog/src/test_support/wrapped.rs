//! Scripted wrapped data.

use futures_util::future::BoxFuture;

use crate::wrapped::*;

/// Scripted wrapped data for tests.
#[derive(Debug, Clone)]
pub struct FakeWrappedData {
    year: i32,
    users: Vec<WrappedUserSummary>,
    per_user: std::collections::HashMap<String, UserWrappedResponse>,
    server: ServerWrappedResponse,
}

impl FakeWrappedData {
    /// Build a fake from its parts.
    pub fn new(
        year: i32,
        users: Vec<WrappedUserSummary>,
        per_user: std::collections::HashMap<String, UserWrappedResponse>,
        server: ServerWrappedResponse,
    ) -> Self {
        Self {
            year,
            users,
            per_user,
            server,
        }
    }

    /// Empty fake: no users, zeroed server stats.
    pub fn empty(year: i32) -> Self {
        Self {
            year,
            users: Vec::new(),
            per_user: std::collections::HashMap::new(),
            server: ServerWrappedResponse {
                year,
                total_users_tracked: 0,
                total_listens_estimated: 0,
                leaderboard: Vec::new(),
                top_artist_sitewide: None,
                top_album_sitewide: None,
            },
        }
    }
}

impl WrappedData for FakeWrappedData {
    fn current_year(&self) -> i32 {
        self.year
    }

    fn list_users(&self) -> BoxFuture<'_, Vec<WrappedUserSummary>> {
        let users = self.users.clone();
        Box::pin(async move { users })
    }

    fn user_wrapped(&self, user_id: &str) -> BoxFuture<'_, Option<UserWrappedResponse>> {
        let found = self.per_user.get(user_id).cloned();
        Box::pin(async move { found })
    }

    fn server_wrapped(&self) -> BoxFuture<'_, ServerWrappedResponse> {
        let server = self.server.clone();
        Box::pin(async move { server })
    }
}
