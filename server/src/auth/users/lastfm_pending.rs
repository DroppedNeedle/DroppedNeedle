//! Pending Last.fm sign-in tokens, bound to the user who asked for them.
//!
//! With one instance app key, every user's `auth.getToken` token belongs to
//! the same Last.fm application. Without a binding, a user who learned
//! another user's approved token could exchange it and link that person's
//! Last.fm account to their own profile. So each token is recorded (as a
//! hash) against the requesting user, and only that user can exchange it.
//! v2 kept a similar in-memory list (five tokens, ten minutes).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::auth::session::tokens::hash_token;

/// How long a token waits for approval (v2 `TOKEN_TTL_SECONDS`).
pub const PENDING_TTL: Duration = Duration::from_secs(600);
/// Pending tokens kept per user; the oldest goes first (v2 parity).
pub const PENDING_PER_USER: usize = 5;

/// In-memory pending tokens. A restart drops them; the user starts again.
#[derive(Debug, Default)]
pub struct LastFmPending {
    tokens: Mutex<HashMap<String, (String, Instant)>>,
}

impl LastFmPending {
    /// Record `token` as requested by `user_id`.
    pub fn remember(&self, user_id: &str, token: &str) {
        let Ok(mut tokens) = self.tokens.lock() else {
            tracing::error!("last.fm pending tokens lock poisoned");
            return;
        };
        tokens.retain(|_, (_, at)| at.elapsed() < PENDING_TTL);
        let mut mine: Vec<(String, Instant)> = tokens
            .iter()
            .filter(|(_, (owner, _))| owner == user_id)
            .map(|(hash, (_, at))| (hash.clone(), *at))
            .collect();
        if mine.len() >= PENDING_PER_USER {
            mine.sort_by_key(|(_, at)| *at);
            for (hash, _) in mine.iter().take(mine.len() + 1 - PENDING_PER_USER) {
                tokens.remove(hash);
            }
        }
        tokens.insert(hash_token(token), (user_id.to_owned(), Instant::now()));
    }

    /// True when `user_id` asked for `token` and it has not expired.
    pub fn is_pending_for(&self, user_id: &str, token: &str) -> bool {
        let Ok(tokens) = self.tokens.lock() else {
            return false;
        };
        tokens
            .get(&hash_token(token))
            .is_some_and(|(owner, at)| owner == user_id && at.elapsed() < PENDING_TTL)
    }

    /// Forget `token` once it has been exchanged.
    pub fn forget(&self, token: &str) {
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.remove(&hash_token(token));
        }
    }
}
