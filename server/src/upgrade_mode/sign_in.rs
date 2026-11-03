//! Upgrade mode's own sign-in.
//!
//! Two ways in, both checked here and nowhere else:
//! - An admin's v2 password. The admins' local password hashes are read
//!   once, read-only, from the v2 database when upgrade mode opens; a
//!   folder an upgrade left halfway is read from its backup first. Each
//!   sign-in checks one hash at every cost an admin's hash is at, dummies
//!   where the name matches no admin, so an unknown name, a non-admin and
//!   a wrong password take the same time and get the same 401. Two admins
//!   whose names differ only in case cannot sign in with a password.
//! - A one-time code, logged at the start and again each time one is used.
//!   It is for an admin with no local password (single sign-on only), and
//!   is always accepted.
//!
//! A sign-in gives an upgrade session: 32 random bytes in its own cookie,
//! held hashed in memory for 12 hours at most and gone when upgrade mode
//! ends. It is never carried into v3, where people sign in as usual.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{sync::Semaphore, time::Instant};

use droppedneedle_auth::session::{
    one_time_code::OneTimeCode,
    tokens::{self, secret_eq},
};

use super::sql::{V2_ADMIN_PASSWORDS, V2AdminPassword};
use crate::bootstrap::data_dirs::{DataDirs, open_read_only};

/// How long an upgrade session lasts at most.
pub const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);

/// Most upgrade sessions held at once. A sign-in past it ends the one
/// closest to its end.
const MOST_SESSIONS: usize = 32;

/// How long a sign-in waits for a turn to check a password before the
/// server answers busy.
const VERIFY_WAIT: Duration = Duration::from_secs(5);

/// Password checks run at once. bcrypt is CPU work; two is enough for the
/// few admins of one server and keeps a burst of guesses off the cores.
const VERIFY_TURNS: usize = 2;

/// The cost of the dummy hash when no admin's hash says one.
const DEFAULT_BCRYPT_COST: u32 = 12;

/// Why a sign-in could not be checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckError {
    /// Every turn to check a password stayed taken.
    Busy,
    /// The system random source failed.
    Random,
}

/// One v2 admin who can sign in with a password.
#[derive(Clone)]
struct V2Admin {
    /// The local sign-in name, trimmed and lowercased.
    name: String,
    /// The bcrypt hash v2 stored.
    hash: String,
    /// The cost it was made at.
    cost: u32,
}

/// The sign-in side of upgrade mode.
pub struct SignIn {
    admins: Vec<V2Admin>,
    /// A bcrypt hash of a random password at each cost an admin's hash is
    /// at. Every sign-in checks one hash at each of these costs, real or
    /// dummy, so which name it gave cannot be told from the time it took.
    dummies: BTreeMap<u32, String>,
    turns: Arc<Semaphore>,
    code: Mutex<OneTimeCode>,
    sessions: Mutex<HashMap<String, Instant>>,
}

impl SignIn {
    /// Read the v2 admins' password hashes and mint the first code.
    /// Blocking: it reads the v2 database and makes the dummy hashes.
    pub fn open(dirs: &DataDirs, interrupted: bool) -> Result<Self, CheckError> {
        let admins = read_admins(dirs, interrupted);
        let mut costs: BTreeSet<u32> = admins.iter().map(|admin| admin.cost).collect();
        if costs.is_empty() {
            costs.insert(DEFAULT_BCRYPT_COST);
        }
        let mut dummies = BTreeMap::new();
        for cost in costs {
            let password = tokens::mint_token().map_err(|_| CheckError::Random)?;
            match bcrypt::hash(password, cost) {
                Ok(hash) => {
                    dummies.insert(cost, hash);
                }
                Err(error) => {
                    tracing::warn!(%error, cost, "cannot make the upgrade sign-in's dummy hash");
                }
            }
        }
        // An admin whose cost has no dummy could be told apart by time.
        let admins = admins
            .into_iter()
            .filter(|admin| dummies.contains_key(&admin.cost))
            .collect();
        Ok(Self {
            admins,
            dummies,
            turns: Arc::new(Semaphore::new(VERIFY_TURNS)),
            code: Mutex::new(mint_code()?),
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Log the current code, with what it is for.
    pub fn log_code(&self) {
        let code = lock(&self.code).spaced();
        tracing::warn!(
            code = %code,
            "upgrade sign-in code: it signs in to the upgrade page once, for when no admin can \
             sign in with a v2 password"
        );
    }

    /// Whether `password` is the v2 password of the admin named `name`.
    /// Every call checks one hash at each admin cost: the admin's own at
    /// its cost, a dummy at the others. A hash that does not read is
    /// followed by the dummy at its cost, so it takes the same time.
    pub async fn password(&self, name: &str, password: String) -> Result<bool, CheckError> {
        let name = name.trim().to_lowercase();
        let admin = self.admins.iter().find(|admin| admin.name == name);
        let checks: Vec<(String, bool, String)> = self
            .dummies
            .iter()
            .map(|(cost, dummy)| match admin {
                Some(admin) if admin.cost == *cost => (admin.hash.clone(), true, dummy.clone()),
                _ => (dummy.clone(), false, dummy.clone()),
            })
            .collect();
        let turn = tokio::time::timeout(VERIFY_WAIT, Arc::clone(&self.turns).acquire_owned())
            .await
            .map_err(|_| CheckError::Busy)?
            .map_err(|_| CheckError::Busy)?;
        let matched = tokio::task::spawn_blocking(move || {
            let _turn = turn;
            let mut matched = false;
            for (hash, real, dummy) in checks {
                match bcrypt::verify(&password, &hash) {
                    Ok(true) => matched |= real,
                    Ok(false) => {}
                    Err(_) => {
                        let _ = bcrypt::verify(&password, &dummy);
                    }
                }
            }
            matched
        })
        .await
        .unwrap_or(false);
        Ok(matched)
    }

    /// The code not yet used, which Continue hands to the app: on a server
    /// started fresh its first-admin setup asks for the same code.
    pub fn current_code(&self) -> OneTimeCode {
        lock(&self.code).clone()
    }

    /// Whether `typed` is the current code. A code that matches is used
    /// up: a new one is minted and logged.
    pub fn code(&self, typed: &str) -> Result<bool, CheckError> {
        let mut code = lock(&self.code);
        if !code.matches(typed) {
            return Ok(false);
        }
        *code = mint_code()?;
        drop(code);
        self.log_code();
        Ok(true)
    }

    /// Start a session: its raw token, for the cookie. Only its hash is
    /// kept.
    pub fn start_session(&self) -> Result<String, CheckError> {
        let token = tokens::mint_token().map_err(|_| CheckError::Random)?;
        let now = Instant::now();
        let mut sessions = lock(&self.sessions);
        sessions.retain(|_, ends| *ends > now);
        if sessions.len() >= MOST_SESSIONS
            && let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, ends)| **ends)
                .map(|(hash, _)| hash.clone())
        {
            sessions.remove(&oldest);
        }
        sessions.insert(tokens::hash_token(&token), now + SESSION_LIFETIME);
        Ok(token)
    }

    /// Whether `token` is a session this upgrade mode started and that has
    /// not ended.
    pub fn holds(&self, token: &str) -> bool {
        let presented = tokens::hash_token(token);
        let now = Instant::now();
        lock(&self.sessions)
            .iter()
            .any(|(hash, ends)| *ends > now && secret_eq(hash, &presented))
    }
}

/// The admins' password hashes. A folder an upgrade left halfway may keep
/// the v2 database in its backup, so that is read first. A database that
/// does not read leaves the code as the only way in, which the log says.
fn read_admins(dirs: &DataDirs, interrupted: bool) -> Vec<V2Admin> {
    let mut places: Vec<PathBuf> = Vec::new();
    if interrupted && let Some(name) = dirs.db_path.file_name() {
        places.push(dirs.cache_backup().join(name));
    }
    places.push(dirs.db_path.clone());
    for place in places {
        if std::fs::symlink_metadata(&place).is_err() {
            continue;
        }
        match read_admins_at(&place) {
            Ok(admins) => {
                let admins = without_shared_names(admins);
                if admins.is_empty() {
                    tracing::warn!(
                        "no v2 admin has a password; sign in to the upgrade page with the code"
                    );
                }
                return admins;
            }
            Err(error) => tracing::warn!(
                database = %place.display(),
                %error,
                "cannot read the v2 admins' passwords"
            ),
        }
    }
    tracing::warn!("no v2 database to check passwords against; sign in with the code");
    Vec::new()
}

fn read_admins_at(db_path: &Path) -> Result<Vec<V2Admin>, String> {
    let db = open_read_only(db_path)?;
    let mut statement = db
        .prepare(V2_ADMIN_PASSWORDS.text())
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], V2AdminPassword::read)
        .map_err(|error| error.to_string())?;
    let mut admins = Vec::new();
    for row in rows {
        let row = row.map_err(|error| error.to_string())?;
        let name = row.provider_uid.trim().to_lowercase();
        let Some((hash, cost)) = row
            .password_hash
            .and_then(|hash| bcrypt_cost(&hash).map(|cost| (hash, cost)))
        else {
            tracing::warn!(
                name,
                "a v2 admin's password hash is not one this page reads; that admin signs in with the code"
            );
            continue;
        };
        admins.push(V2Admin { name, hash, cost });
    }
    Ok(admins)
}

/// The admins without those whose name another shares apart from case:
/// which one a sign-in meant cannot be told, so neither signs in with a
/// password (the code still works).
fn without_shared_names(admins: Vec<V2Admin>) -> Vec<V2Admin> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for admin in &admins {
        *seen.entry(admin.name.clone()).or_default() += 1;
    }
    admins
        .into_iter()
        .filter(|admin| {
            let shared = seen.get(&admin.name).is_some_and(|count| *count > 1);
            if shared {
                tracing::warn!(
                    name = admin.name,
                    "two v2 admins have this sign-in name apart from case; neither signs in \
                     with a password here, use the code"
                );
            }
            !shared
        })
        .collect()
}

/// The cost of a bcrypt hash as v2 stored them (`$2a$`, `$2b$` or `$2y$`,
/// a cost from 4 to 31, 60 characters), or `None` for anything else.
fn bcrypt_cost(hash: &str) -> Option<u32> {
    let bcrypt = ["$2a$", "$2b$", "$2y$"]
        .iter()
        .any(|prefix| hash.starts_with(prefix));
    let cost: u32 = hash.get(4..6)?.parse().ok()?;
    (bcrypt && hash.len() == 60 && hash.get(6..7) == Some("$") && (4..=31).contains(&cost))
        .then_some(cost)
}

/// A fresh code.
fn mint_code() -> Result<OneTimeCode, CheckError> {
    OneTimeCode::mint().map_err(|_| CheckError::Random)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
