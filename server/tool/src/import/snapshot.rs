//! What the target holds: the read-only view the merge decisions run against,
//! read under the import's transaction.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use super::apply::ImportError;
use super::sql::{
    self, AppPasswordRow, ApprovalRow, EventCityRow, FollowRow, ProviderRow, RecoveryRow, UserRow,
};

/// Concerts city key: the table's (user, latitude, longitude) primary key.
pub(super) type CityKey = (String, u64, u64);

/// A coordinate as a key part. SQLite compares REAL keys by value, so
/// -0.0 folds into 0.0 here as it does there.
pub(super) fn coord_bits(value: f64) -> u64 {
    (value + 0.0).to_bits()
}

/// Read-only view of the target database the merge decisions run against.
pub(super) struct DatabaseSnapshot {
    pub(super) users: HashMap<String, UserRow>,
    pub(super) emails: HashMap<String, String>,
    pub(super) usernames: HashMap<String, String>,
    pub(super) providers: HashMap<(String, String), ProviderRow>,
    pub(super) app_passwords: HashMap<(String, String), Vec<AppPasswordRow>>,
    pub(super) app_password_shas: HashSet<String>,
    pub(super) recoveries: HashMap<String, RecoveryRow>,
    pub(super) follows: HashMap<(String, String), FollowRow>,
    pub(super) approvals: HashMap<(String, String), ApprovalRow>,
    pub(super) event_cities: HashMap<CityKey, EventCityRow>,
    pub(super) event_seen: HashMap<String, f64>,
}

impl DatabaseSnapshot {
    pub(super) fn load(conn: &Connection) -> Result<Self, ImportError> {
        let users = sql::all_rows(conn, sql::USERS, UserRow::read)?;
        let providers = sql::all_rows(conn, sql::PROVIDERS, ProviderRow::read)?;
        let app_passwords = sql::all_rows(conn, sql::APP_PASSWORDS, AppPasswordRow::read)?;
        let recoveries = sql::all_rows(conn, sql::RECOVERY_CODES, RecoveryRow::read)?;
        let follows = sql::all_rows(conn, sql::FOLLOWS, FollowRow::read)?;
        let approvals = sql::all_rows(conn, sql::APPROVALS, ApprovalRow::read)?;
        let event_cities = sql::all_rows(conn, sql::EVENT_CITIES, EventCityRow::read)?;
        let event_seen = sql::all_rows(conn, sql::EVENT_SEEN, sql::EventSeenRow::read)?;

        let mut snapshot = Self {
            users: HashMap::new(),
            emails: HashMap::new(),
            usernames: HashMap::new(),
            providers: HashMap::new(),
            app_passwords: HashMap::new(),
            app_password_shas: HashSet::new(),
            recoveries: HashMap::new(),
            follows: HashMap::new(),
            approvals: HashMap::new(),
            event_cities: HashMap::new(),
            event_seen: event_seen
                .into_iter()
                .map(|seen| (seen.user_id, seen.seen_at))
                .collect(),
        };
        for city in event_cities {
            snapshot.event_cities.insert(
                (
                    city.user_id.clone(),
                    coord_bits(city.latitude),
                    coord_bits(city.longitude),
                ),
                city,
            );
        }
        for user in users {
            if let Some(email) = user.email.clone() {
                snapshot.emails.insert(email, user.id.clone());
            }
            if let Some(username) = user.username.clone() {
                snapshot.usernames.insert(username, user.id.clone());
            }
            snapshot.users.insert(user.id.clone(), user);
        }
        for provider in providers {
            snapshot.providers.insert(
                (provider.provider.clone(), provider.provider_uid.clone()),
                provider,
            );
        }
        for password in app_passwords {
            snapshot
                .app_password_shas
                .insert(password.secret_sha256.clone());
            snapshot
                .app_passwords
                .entry((password.user_id.clone(), password.name.clone()))
                .or_default()
                .push(password);
        }
        for recovery in recoveries {
            snapshot
                .recoveries
                .insert(recovery.user_id.clone(), recovery);
        }
        for follow in follows {
            snapshot.follows.insert(
                (follow.user_id.clone(), follow.artist_mbid_lower.clone()),
                follow,
            );
        }
        for approval in approvals {
            snapshot.approvals.insert(
                (approval.user_id.clone(), approval.artist_mbid_lower.clone()),
                approval,
            );
        }
        Ok(snapshot)
    }
}
