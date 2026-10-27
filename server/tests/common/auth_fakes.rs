//! Fakes of what auth reaches outside the process: Last.fm's sign-in
//! calls, a media server's user list and the breach check.

use std::collections::{HashMap, HashSet};

use droppedneedle_auth::federated::users::PROVIDER_PLEX;
use droppedneedle_auth::users::hibp::{HibpHttp, HibpHttpError, RANGE_PREFIX_LEN};
use droppedneedle_auth::users::stores::{
    BoxFuture, DirectoryError, DirectoryUser, LastFmAuthClient, LastFmError, UserDirectory,
};
use tokio::sync::Mutex;

/// Scripted Last.fm auth client. Unknown tokens exchange as unapproved,
/// exactly like the real flow before browser approval.
#[derive(Debug, Default)]
pub struct FakeLastFmAuthClient {
    state: Mutex<FakeLastFmState>,
}

#[derive(Debug, Default)]
struct FakeLastFmState {
    token: (String, String),
    sessions: HashMap<String, (String, String)>,
}

impl FakeLastFmAuthClient {
    /// Fake whose token step returns `token`/`auth_url`.
    pub fn new(token: &str, auth_url: &str) -> Self {
        Self {
            state: Mutex::new(FakeLastFmState {
                token: (token.to_owned(), auth_url.to_owned()),
                sessions: HashMap::new(),
            }),
        }
    }

    /// Approve `token` for `username` with `session_key`.
    pub async fn approve(&self, token: &str, username: &str, session_key: &str) {
        self.state.lock().await.sessions.insert(
            token.to_owned(),
            (username.to_owned(), session_key.to_owned()),
        );
    }
}

impl LastFmAuthClient for FakeLastFmAuthClient {
    fn request_token<'a>(
        &'a self,
        _api_key: &'a str,
        _shared_secret: &'a str,
    ) -> BoxFuture<'a, Result<(String, String), LastFmError>> {
        Box::pin(async move { Ok(self.state.lock().await.token.clone()) })
    }

    fn exchange_session<'a>(
        &'a self,
        _api_key: &'a str,
        _shared_secret: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, Result<(String, String), LastFmError>> {
        Box::pin(async move {
            self.state
                .lock()
                .await
                .sessions
                .get(token)
                .cloned()
                .ok_or(LastFmError::TokenNotAuthorized)
        })
    }
}

/// Scripted media-server user directory serving a fixed list.
#[derive(Debug)]
pub struct FakeUserDirectory {
    provider: &'static str,
    users: Vec<DirectoryUser>,
}

impl FakeUserDirectory {
    /// Fake Plex directory serving `users`.
    pub fn plex(users: Vec<DirectoryUser>) -> Self {
        Self {
            provider: PROVIDER_PLEX,
            users,
        }
    }
}

impl UserDirectory for FakeUserDirectory {
    fn provider(&self) -> &'static str {
        self.provider
    }

    fn list_users(&self) -> BoxFuture<'_, Result<Vec<DirectoryUser>, DirectoryError>> {
        Box::pin(async move { Ok(self.users.clone()) })
    }
}

/// Scripted breach-check range client: `hits` holds full uppercase digests.
#[derive(Debug, Default)]
pub struct FakeHibpHttp {
    hits: HashSet<String>,
}

impl FakeHibpHttp {
    /// Fake that reports every digest in `hits` as breached.
    pub fn with_hits(hits: &[&str]) -> Self {
        Self {
            hits: hits.iter().map(|hit| hit.to_string()).collect(),
        }
    }
}

impl HibpHttp for FakeHibpHttp {
    fn range<'a>(
        &'a self,
        prefix: &'a str,
    ) -> BoxFuture<'a, Result<HashSet<String>, HibpHttpError>> {
        Box::pin(async move {
            Ok(self
                .hits
                .iter()
                .filter(|hit| hit.starts_with(prefix))
                .filter_map(|hit| hit.get(RANGE_PREFIX_LEN..).map(str::to_owned))
                .collect())
        })
    }
}
