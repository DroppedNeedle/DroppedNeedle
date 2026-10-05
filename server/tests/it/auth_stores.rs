//! The production auth stores and services over scratch SQLite databases.
//!
//! What lives here is what only the real adapters can prove: hashing pins,
//! the v2 bcrypt import path, sealed federated tokens, decrypt-free app
//! passwords, and the guards that must hold inside one write transaction
//! (atomic account creation, the last-admin rule, the app-password cap,
//! the first federated admin) plus the sweeps that keep auth tables small.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use droppedneedle::auth::compat_auth::subsonic::md5_hex;
use droppedneedle::auth::federated::oidc::OidcStateStore as _;
use droppedneedle::auth::federated::password_import::PasswordHasher as _;
use droppedneedle::auth::federated::users::{
    FederatedProfile, FederatedUserStore as _, find_or_create_federated_user,
};
use droppedneedle::auth::prod::{
    Argon2idHasher, OWASP_M_COST_KIB, OWASP_P_COST, OWASP_T_COST, ProdAuth,
};
use droppedneedle::auth::session::login::{
    LoginContext, LoginRequest, LoginService, PasswordVerifier as _, TransportParam,
};
use droppedneedle::auth::session::store::{
    SessionKind, SessionRecord, SessionStore as _, now_unix,
};
use droppedneedle::auth::session::tokens;
use droppedneedle::auth::users::UsersDeps;
use droppedneedle::auth::users::hibp::{HibpHttp, HibpHttpError, HibpScreen};
use droppedneedle::auth::users::memory::{
    CounterIdGenerator, ManualClock, Sha256TestHasher, StaticSecurityPolicy, TestRig,
};
use droppedneedle::auth::users::models::{
    AppPasswordRecord, LocalCredential, PasswordReset, UserRecord,
};
use droppedneedle::auth::users::roles::{Role, SessionKind as UserSessionKind};
use droppedneedle::auth::users::services::{self, verify_app_password};
use droppedneedle::auth::users::stores::{
    AppPasswordStore as _, BoxFuture, Clock, HibpPolicy, RoleChange, SessionManager as _,
    UserDeletion, UserStore as _,
};
use droppedneedle::db::{DbConfig, DbRuntime, Lane, open_runtime};
use droppedneedle::ids::IdGenerator;
use droppedneedle::runtime_config::crypto::Crypto;
use rusqlite::params;

/// Fixed test clock: 2023-11-14T22:13:20Z.
const TEST_NOW: i64 = 1_700_000_000;
/// v2-style legacy hash (bcrypt cost 12) of `correct horse battery staple`.
const LEGACY_BCRYPT: &str = "$2b$12$H1xO3d1.j6f.mzdNztsl/elXm4PG9ZcXMLsN4nN7jScoFYouUwFSC";
/// Password behind [`LEGACY_BCRYPT`].
const LEGACY_PASSWORD: &str = "correct horse battery staple";

static SCRATCH_SEQ: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    runtime: DbRuntime,
    auth: ProdAuth,
    crypto: Arc<Crypto>,
    clock: Arc<ManualClock>,
}

async fn fixture(name: &str) -> Fixture {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("auth-stores-{name}-{}-{seq}", std::process::id()));
    let runtime = open_runtime(&DbConfig::new(&dir.join("auth.db")))
        .await
        .unwrap();
    let crypto = Arc::new(Crypto::from_key_bytes(&[7u8; 32]).unwrap());
    let ids: Arc<dyn IdGenerator> = Arc::new(CounterIdGenerator::new("id-"));
    let clock = Arc::new(ManualClock::new(TEST_NOW));
    let auth = ProdAuth::new(
        runtime.pool(),
        runtime.lane(),
        Arc::clone(&crypto),
        ids,
        Arc::clone(&clock) as Arc<dyn Clock>,
        &dir,
    );
    Fixture {
        runtime,
        auth,
        crypto,
        clock,
    }
}

impl Fixture {
    /// Users services over the SQLite stores, with the fast test hasher and
    /// the fixture clock.
    fn deps(&self) -> UsersDeps {
        let mut deps = TestRig::new().unwrap().deps;
        deps.users = Arc::new(self.auth.users.clone());
        deps.sessions = Arc::new(self.auth.session_manager.clone());
        deps.app_passwords = Arc::new(self.auth.app_passwords.clone());
        deps.recovery = Arc::new(self.auth.recovery.clone());
        deps.clock = Arc::clone(&self.clock) as Arc<dyn Clock>;
        deps
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.runtime.pool())
            .await
            .unwrap()
    }
}

fn user_row(id: &str, username: &str, role: Role) -> UserRecord {
    UserRecord {
        id: id.to_owned(),
        username: Some(username.to_owned()),
        username_display: Some(username.to_owned()),
        display_name: format!("{username} Doe"),
        email: None,
        avatar_url: None,
        role,
        created_at: TEST_NOW,
        last_login_at: None,
    }
}

fn session_row(id: &str, user_id: &str, token: &str, at: i64) -> SessionRecord {
    SessionRecord {
        id: id.to_owned(),
        user_id: user_id.to_owned(),
        token_hash: tokens::hash_token(token),
        kind: SessionKind::Standard,
        label: None,
        issued_at: at,
        expires_at: tokens::expires_at(at),
        last_seen_at: at,
        revoked: false,
        user_agent: None,
    }
}

fn app_password(id: &str, user_id: &str, secret: &str, crypto: &Crypto) -> AppPasswordRecord {
    AppPasswordRecord {
        id: id.to_owned(),
        user_id: user_id.to_owned(),
        name: id.to_owned(),
        secret_sha256: tokens::hash_token(secret),
        secret_encrypted: crypto.encrypt(secret).unwrap(),
        created_at: TEST_NOW,
        last_used_at: None,
        last_client: None,
    }
}

#[test]
fn argon2id_params_match_the_owasp_pin() {
    assert_eq!(
        (OWASP_M_COST_KIB, OWASP_T_COST, OWASP_P_COST),
        (19456, 2, 1)
    );
    let hasher = Argon2idHasher::new();
    let hash = hasher.hash_argon2id(LEGACY_PASSWORD);
    assert!(
        hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
        "{hash}"
    );
    assert_ne!(hash, hasher.hash_argon2id(LEGACY_PASSWORD), "salted");
    assert!(hasher.verify_argon2id(LEGACY_PASSWORD, &hash));
    // Scheme dispatch reads the tag only; untagged or unknown rows fail.
    assert!(hasher.verify(LEGACY_PASSWORD, &format!("bcrypt${LEGACY_BCRYPT}")));
    for stored in [LEGACY_BCRYPT.to_owned(), hash, String::new()] {
        assert!(!hasher.verify(LEGACY_PASSWORD, &stored), "{stored}");
    }
}

#[test]
fn md5_keeps_rfc_and_v2_spot_values() {
    assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(
        md5_hex("alice-secretpepper"),
        "22b231fb88294b6a25be3d978438fdc1"
    );
}

#[tokio::test]
async fn v2_bcrypt_rows_log_in_and_rehash_to_argon2id() {
    let fx = fixture("import").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada", Role::User))
        .await
        .unwrap();
    // v2 verbatim shape: {"password_hash": ...} with no scheme tag.
    let legacy = serde_json::json!({ "password_hash": LEGACY_BCRYPT }).to_string();
    fx.runtime
        .lane()
        .write(Lane::Foreground, "test.seed", move |tx| {
            tx.execute(
                "INSERT INTO auth_providers (id, user_id, provider, provider_uid, provider_data, \
                 created_at) VALUES ('cred-1', 'user-1', 'local', 'ada', ?, '2023-11-14T22:13:20+00:00')",
                params![legacy],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let login = LoginService::new(
        fx.auth.sessions.clone(),
        fx.auth.hasher.clone(),
        fx.auth.credentials.clone(),
    );
    let success = login
        .login(
            LoginRequest {
                username: "ADA".to_owned(),
                password: LEGACY_PASSWORD.to_owned(),
                transport: TransportParam::Bearer,
            },
            LoginContext {
                user_agent: None,
                now_unix: now_unix(),
            },
        )
        .await
        .unwrap();
    assert_eq!(success.user_id, "user-1");
    let stored = fx
        .auth
        .users
        .local_credential("user-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.scheme, "argon2id", "the login persisted the upgrade");
    assert!(Argon2idHasher::new().verify_argon2id(LEGACY_PASSWORD, &stored.hash));
}

#[tokio::test]
async fn sessions_reject_unknown_revoked_and_expired_tokens() {
    let fx = fixture("sessions").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada", Role::User))
        .await
        .unwrap();
    let sessions = &fx.auth.sessions;
    sessions
        .insert(session_row("live", "user-1", "live-token", TEST_NOW))
        .await
        .unwrap();
    sessions
        .insert(session_row("revoked", "user-1", "revoked-token", TEST_NOW))
        .await
        .unwrap();
    assert!(
        fx.auth
            .session_manager
            .revoke_scoped("user-1", "revoked")
            .await
            .unwrap()
    );
    for (token, live) in [
        ("live-token", true),
        ("revoked-token", false),
        ("unknown", false),
    ] {
        let found = sessions
            .lookup_valid(&tokens::hash_token(token), TEST_NOW)
            .await
            .unwrap();
        assert_eq!(found.is_some(), live, "{token}");
    }
    let later = TEST_NOW + tokens::SESSION_MAX_AGE_SECS + 1;
    assert!(
        sessions
            .lookup_valid(&tokens::hash_token("live-token"), later)
            .await
            .unwrap()
            .is_none(),
        "absolute lifetime"
    );
}

#[tokio::test]
async fn logins_sweep_dead_token_rows() {
    let fx = fixture("token-sweep").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada", Role::User))
        .await
        .unwrap();
    let sessions = &fx.auth.sessions;
    let long_ago = TEST_NOW - 2 * tokens::SESSION_MAX_AGE_SECS;
    sessions
        .insert(session_row("expired", "user-1", "expired-token", long_ago))
        .await
        .unwrap();
    sessions
        .insert(session_row("revoked", "user-1", "revoked-token", TEST_NOW))
        .await
        .unwrap();
    fx.auth
        .session_manager
        .revoke_scoped("user-1", "revoked")
        .await
        .unwrap();
    sessions
        .insert(session_row("fresh", "user-1", "fresh-token", TEST_NOW))
        .await
        .unwrap();
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM auth_tokens").await,
        1,
        "the next login removes rows no request can use"
    );
}

#[tokio::test]
async fn user_creation_is_one_transaction() {
    let fx = fixture("atomic-create").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada", Role::Admin))
        .await
        .unwrap();
    fx.auth
        .users
        .insert_local_credential(LocalCredential {
            id: "cred-taken".to_owned(),
            user_id: "user-1".to_owned(),
            scheme: "argon2id".to_owned(),
            hash: "h".to_owned(),
        })
        .await
        .unwrap();
    // The credential insert fails (its id is taken): the account row must
    // not survive without a password.
    let outcome = fx
        .auth
        .users
        .insert_with_local_credential(
            user_row("user-2", "grace", Role::User),
            LocalCredential {
                id: "cred-taken".to_owned(),
                user_id: "user-2".to_owned(),
                scheme: "argon2id".to_owned(),
                hash: "h".to_owned(),
            },
        )
        .await;
    assert!(outcome.is_err());
    assert!(fx.auth.users.get_by_id("user-2").await.unwrap().is_none());
}

#[tokio::test]
async fn first_user_insert_refuses_once_anyone_exists() {
    let fx = fixture("first-user").await;
    let users = &fx.auth.users;
    let credential = |id: &str, user_id: &str| LocalCredential {
        id: id.to_owned(),
        user_id: user_id.to_owned(),
        scheme: "argon2id".to_owned(),
        hash: "h".to_owned(),
    };
    // A federated first login got there first: setup must not add a
    // second admin.
    let mut federated = user_row("fed-1", "ignored", Role::Admin);
    federated.username = None;
    federated.username_display = None;
    users.insert(federated).await.unwrap();
    let created = users
        .insert_first_user(
            user_row("admin-1", "ada", Role::Admin),
            credential("cred-1", "admin-1"),
        )
        .await
        .unwrap();
    assert!(!created);
    assert!(users.get_by_id("admin-1").await.unwrap().is_none());
}

#[tokio::test]
async fn last_admin_guard_runs_inside_the_write() {
    let fx = fixture("last-admin").await;
    let users = &fx.auth.users;
    users
        .insert(user_row("admin-1", "ada", Role::Admin))
        .await
        .unwrap();
    users
        .insert(user_row("admin-2", "grace", Role::Admin))
        .await
        .unwrap();
    // Two demotions racing: whichever runs second sees one admin left.
    let (first, second) = tokio::join!(
        users.set_role("admin-1", Role::User),
        users.set_role("admin-2", Role::User)
    );
    let mut outcomes = vec![first.unwrap(), second.unwrap()];
    outcomes.sort_by_key(|outcome| *outcome != RoleChange::Changed);
    assert_eq!(outcomes, vec![RoleChange::Changed, RoleChange::LastAdmin]);
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM auth_users WHERE role = 'admin'")
            .await,
        1
    );
    let remaining = if users.get_by_id("admin-1").await.unwrap().unwrap().role == Role::Admin {
        "admin-1"
    } else {
        "admin-2"
    };
    assert_eq!(
        users.delete(remaining).await.unwrap(),
        UserDeletion::LastAdmin
    );
}

#[tokio::test]
async fn users_named_by_library_history_are_not_deleted() {
    let fx = fixture("restrict-delete").await;
    fx.auth
        .users
        .insert(user_row("admin-1", "ada", Role::Admin))
        .await
        .unwrap();
    fx.auth
        .users
        .insert(user_row("user-2", "grace", Role::User))
        .await
        .unwrap();
    // A side connection without foreign-key enforcement seeds the history
    // row without building the artists it names.
    let side = rusqlite::Connection::open(fx.runtime.path()).unwrap();
    side.busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    side.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    side.execute(
        "INSERT INTO library_artist_reconciliation_dismissals (left_artist_id, \
         right_artist_id, left_artist_revision, right_artist_revision, dismissed_by_user_id, \
         created_at, updated_at) VALUES ('artist-a', 'artist-b', 1, 1, 'user-2', 1.0, 1.0)",
        params![],
    )
    .unwrap();
    drop(side);
    assert_eq!(
        fx.auth.users.delete("user-2").await.unwrap(),
        UserDeletion::Referenced(vec!["artist reconciliation dismissals"])
    );
    assert!(fx.auth.users.get_by_id("user-2").await.unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn app_password_cap_holds_under_parallel_creates() {
    let fx = fixture("ap-cap").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada", Role::User))
        .await
        .unwrap();
    let mut handles = Vec::new();
    for n in 0..services::MAX_ACTIVE_APP_PASSWORDS + 5 {
        let store = fx.auth.app_passwords.clone();
        let row = app_password(&format!("ap-{n}"), "user-1", &format!("s-{n}"), &fx.crypto);
        handles.push(tokio::spawn(async move {
            store
                .insert_capped(row, services::MAX_ACTIVE_APP_PASSWORDS)
                .await
        }));
    }
    let mut inserted = 0;
    for handle in handles {
        if handle.await.unwrap().unwrap() {
            inserted += 1;
        }
    }
    assert_eq!(inserted, services::MAX_ACTIVE_APP_PASSWORDS);
}

#[tokio::test]
async fn app_password_verify_is_decrypt_free_and_stamps_use() {
    let fx = fixture("ap-verify").await;
    let deps = fx.deps();
    deps.users
        .insert(user_row("user-1", "ada", Role::User))
        .await
        .unwrap();
    let secret = tokens::mint_token().unwrap();
    let mut row = app_password("ap-1", "user-1", &secret, &fx.crypto);
    // Verification must never decrypt: a ciphertext from another key still
    // verifies, which is what keeps key rotation safe.
    row.secret_encrypted = "v3:not-decryptable".to_owned();
    deps.app_passwords.insert_capped(row, 25).await.unwrap();

    let authed = verify_app_password(&deps, &secret, Some("Symfonium"))
        .await
        .unwrap();
    assert_eq!(authed.user_id, "user-1");
    let row = deps.app_passwords.get_by_id("ap-1").await.unwrap().unwrap();
    assert_eq!(row.last_used_at, Some(TEST_NOW));
    assert_eq!(row.last_client.as_deref(), Some("Symfonium"));
    assert!(deps.app_passwords.revoke("ap-1").await.unwrap());
    assert!(verify_app_password(&deps, &secret, None).await.is_none());
}

#[tokio::test]
async fn federated_tokens_are_sealed_at_rest() {
    let fx = fixture("federated").await;
    let profile = FederatedProfile {
        provider_uid: "sub-1".to_owned(),
        display_name: "Ada".to_owned(),
        email: None,
        email_verified: false,
        avatar_url: None,
        token_json: r#"{"access_token":"canary-at-9f27"}"#.to_owned(),
    };
    let user = find_or_create_federated_user(&fx.auth.federated, "oidc", &profile)
        .await
        .unwrap();
    let binding = fx
        .auth
        .federated
        .get_provider("oidc", "sub-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.user_id, user.id);
    let raw: String = sqlx::query_scalar("SELECT provider_data FROM auth_providers WHERE id = ?")
        .bind(&binding.id)
        .fetch_one(fx.runtime.pool())
        .await
        .unwrap();
    assert!(raw.starts_with("v3:") && !raw.contains("canary"), "{raw}");
    assert_eq!(fx.crypto.decrypt(&raw).unwrap(), profile.token_json);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_federated_logins_race_to_exactly_one_admin() {
    let fx = fixture("first-admin").await;
    let mut handles = Vec::new();
    for n in 0..4 {
        let store = fx.auth.federated.clone();
        handles.push(tokio::spawn(async move {
            let profile = FederatedProfile {
                provider_uid: format!("sub-{n}"),
                display_name: format!("User {n}"),
                email: None,
                email_verified: false,
                avatar_url: None,
                token_json: "{}".to_owned(),
            };
            find_or_create_federated_user(&store, "oidc", &profile).await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM auth_users WHERE role = 'admin'")
            .await,
        1
    );
}

#[tokio::test]
async fn unverified_federated_email_never_claims_an_account() {
    let fx = fixture("email-link").await;
    let mut owner = user_row("user-1", "ada", Role::Admin);
    owner.email = Some("ada@example.com".to_owned());
    fx.auth.users.insert(owner).await.unwrap();
    let profile = |verified: bool, uid: &str| FederatedProfile {
        provider_uid: uid.to_owned(),
        display_name: "Someone".to_owned(),
        email: Some("ada@example.com".to_owned()),
        email_verified: verified,
        avatar_url: None,
        token_json: "{}".to_owned(),
    };
    let stranger = find_or_create_federated_user(&fx.auth.federated, "oidc", &profile(false, "a"))
        .await
        .unwrap();
    assert_ne!(stranger.id, "user-1");
    assert_eq!(stranger.email, None, "an unverified address is not stored");
    let linked = find_or_create_federated_user(&fx.auth.federated, "oidc", &profile(true, "b"))
        .await
        .unwrap();
    assert_eq!(linked.id, "user-1");
}

#[tokio::test]
async fn companion_concurrent_remint_leaves_exactly_one_live_row() {
    let fx = fixture("remint-race").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada", Role::User))
        .await
        .unwrap();
    let mut handles = Vec::new();
    for n in 0..8u32 {
        let manager = fx.auth.session_manager.clone();
        handles.push(tokio::spawn(async move {
            manager
                .replace_companion(
                    &format!("sess-{n}"),
                    "user-1",
                    &tokens::hash_token(&format!("token-{n}")),
                    "phone",
                    TEST_NOW,
                    tokens::expires_at(TEST_NOW),
                )
                .await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }
    let rows = fx
        .auth
        .session_manager
        .list_for_user("user-1")
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| row.kind == UserSessionKind::Companion)
            .count(),
        1
    );
}

/// HIBP range client that counts calls and reports nothing breached.
#[derive(Default)]
struct CountingHibp(std::sync::atomic::AtomicUsize);

impl HibpHttp for CountingHibp {
    fn range<'a>(
        &'a self,
        _prefix: &'a str,
    ) -> BoxFuture<'a, Result<std::collections::HashSet<String>, HibpHttpError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(std::collections::HashSet::new()) })
    }
}

#[tokio::test]
async fn recovery_reset_checks_the_code_first_then_revokes_and_consumes() {
    let fx = fixture("recovery").await;
    let mut deps = fx.deps();
    let hibp = Arc::new(CountingHibp::default());
    deps.screen = Arc::new(HibpScreen::new(hibp.clone()));
    deps.security = Arc::new(StaticSecurityPolicy {
        policy: HibpPolicy {
            check: true,
            local_path: String::new(),
        },
    });
    deps.users
        .insert(user_row("user-1", "ada", Role::User))
        .await
        .unwrap();
    deps.users
        .insert_local_credential(LocalCredential {
            id: "cred-1".to_owned(),
            user_id: "user-1".to_owned(),
            scheme: "argon2id".to_owned(),
            hash: Sha256TestHasher::test_hash("old password here"),
        })
        .await
        .unwrap();
    fx.auth
        .sessions
        .insert(session_row("sess-1", "user-1", "prior-token", TEST_NOW))
        .await
        .unwrap();
    let reset = |code: &str| PasswordReset {
        username: "ada".to_owned(),
        recovery_code: code.to_owned(),
        new_password: "a brand new password 99".to_owned(),
    };

    // A wrong code fails before the breach screen: the public route cannot
    // be used to drive HIBP lookups.
    assert!(
        services::reset_password(&deps, &reset("WRONG-CODE"))
            .await
            .is_err()
    );
    assert_eq!(hibp.0.load(Ordering::SeqCst), 0);

    let minted = services::admin_mint_recovery_code(&deps, "user-1")
        .await
        .unwrap();
    services::reset_password(&deps, &reset(&minted.recovery_code))
        .await
        .unwrap();
    assert_eq!(hibp.0.load(Ordering::SeqCst), 1);
    assert!(
        deps.sessions
            .list_for_user("user-1")
            .await
            .unwrap()
            .is_empty(),
        "reset kills prior sessions"
    );
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM auth_password_recovery_codes")
            .await,
        0
    );
}

#[tokio::test]
async fn oidc_states_consume_once_and_expired_ones_are_swept() {
    let fx = fixture("oidc-states").await;
    fx.runtime
        .lane()
        .write(Lane::Foreground, "test.seed", |tx| {
            tx.execute(
                "INSERT INTO auth_oidc_states (state, created_at, expires_at, code_verifier) \
                 VALUES ('stale', '2020-01-01T00:00:00+00:00', '2020-01-01T00:10:00+00:00', 'v')",
                params![],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let states = &fx.auth.oidc_states;
    states.store_state("state-1", "verifier-1").await.unwrap();
    assert_eq!(
        fx.count("SELECT COUNT(*) FROM auth_oidc_states").await,
        1,
        "storing a state sweeps expired ones"
    );
    assert_eq!(
        states.consume_state("state-1").await.unwrap().as_deref(),
        Some("verifier-1")
    );
    assert!(states.consume_state("state-1").await.unwrap().is_none());
}
