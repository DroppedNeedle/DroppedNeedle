//! Close-out briefs: production hasher + SQLite adapters + md5 swap pins.
//!
//! Adapter-owned behavior only: Argon2id params and scheme dispatch, the
//! SQLite stores over scratch 0001 databases (throttled `last_seen_at`,
//! sealed federated tokens, touch-on-verify app passwords, the local
//! credential shape), and the post-swap md5 vectors. Slice behavior (login
//! flows, HTTP shapes, compat envelopes, federated journeys) stays in the
//! slice briefs; nothing here duplicates them.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use droppedneedle::auth::compat_auth::subsonic::md5_hex;
use droppedneedle::auth::federated::SessionIssuer as _;
use droppedneedle::auth::federated::oidc::OidcStateStore as _;
use droppedneedle::auth::federated::password_import::{
    HashScheme, LocalCredential as FederatedCredential, PasswordCheck, PasswordHasher,
    verify_and_maybe_rehash,
};
use droppedneedle::auth::federated::users::{FederatedUserStore as _, NewFederatedUser};
use droppedneedle::auth::prod::{
    Argon2idHasher, OWASP_M_COST_KIB, OWASP_P_COST, OWASP_T_COST, ProdAuth,
};
use droppedneedle::auth::session::login::{
    CredentialLookup as _, LoginContext, LoginRequest, LoginService, PasswordVerifier,
    TransportParam,
};
use droppedneedle::auth::session::store::{
    SessionKind, SessionRecord, SessionStore as _, now_unix,
};
use droppedneedle::auth::session::tokens;
use droppedneedle::auth::users::memory::{
    CounterIdGenerator, ManualClock, Sha256TestHasher, TestRig,
};
use droppedneedle::auth::users::models::{
    AppPasswordRecord, LastFmConnection, LocalCredential as UsersCredential, PasswordReset,
    RecoveryCode, UserRecord,
};
use droppedneedle::auth::users::roles::{Role, SessionKind as UserSessionKind};
use droppedneedle::auth::users::services::{self, verify_app_password};
use droppedneedle::auth::users::stores::{
    Clock, LastFmStore as _, RecoveryStore as _, SessionManager as _, UserStore as _,
};
use droppedneedle::db::{DbConfig, DbRuntime, Lane, open_runtime};
use droppedneedle::ids::IdGenerator;
use droppedneedle::runtime_config::crypto::Crypto;
use rusqlite::params;
use sqlx::Row as _;

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
        std::env::temp_dir().join(format!("auth-close-{name}-{}-{seq}", std::process::id()));
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

fn user_row(id: &str, username: &str) -> UserRecord {
    UserRecord {
        id: id.to_owned(),
        username: Some(username.to_owned()),
        username_display: Some(username.to_owned()),
        display_name: format!("{username} Doe"),
        email: None,
        avatar_url: None,
        role: Role::User,
        created_at: TEST_NOW,
        last_login_at: None,
    }
}

fn session_row(id: &str, user_id: &str, token_hash: String, at: i64) -> SessionRecord {
    SessionRecord {
        id: id.to_owned(),
        user_id: user_id.to_owned(),
        token_hash,
        kind: SessionKind::Standard,
        label: None,
        issued_at: at,
        expires_at: tokens::expires_at(at),
        last_seen_at: at,
        revoked: false,
        user_agent: Some("close-test".to_owned()),
    }
}

/// Raw single-text read for asserting at-rest shapes the ports hide.
async fn raw_text(pool: &sqlx::SqlitePool, sql: &str, arg: &str) -> Option<String> {
    let row = sqlx::query(sql)
        .bind(arg)
        .fetch_optional(pool)
        .await
        .unwrap()?;
    row.try_get::<Option<String>, _>(0).unwrap()
}

// ---------------------------------------------------------------------------
// Production hasher
// ---------------------------------------------------------------------------

#[test]
fn argon2id_params_match_the_owasp_pin() {
    assert_eq!(
        (OWASP_M_COST_KIB, OWASP_T_COST, OWASP_P_COST),
        (19456, 2, 1)
    );
    let hasher = Argon2idHasher::new();
    let first = hasher.hash_argon2id(LEGACY_PASSWORD);
    assert!(
        first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
        "{first}"
    );
    let second = hasher.hash_argon2id(LEGACY_PASSWORD);
    assert_ne!(first, second, "random salt per hash");
    assert!(hasher.verify_argon2id(LEGACY_PASSWORD, &first));
    assert!(hasher.verify_argon2id(LEGACY_PASSWORD, &second));
    assert!(!hasher.verify_argon2id("wrong password here!", &first));
    assert!(!hasher.verify_argon2id(LEGACY_PASSWORD, "not-a-hash"));
    assert!(!hasher.verify_argon2id(LEGACY_PASSWORD, ""));
}

#[test]
fn bcrypt_legacy_verifies_then_rehashes_to_argon2id() {
    let hasher = Argon2idHasher::new();
    assert!(hasher.verify_bcrypt(LEGACY_PASSWORD, LEGACY_BCRYPT));
    assert!(!hasher.verify_bcrypt("wrong password here!", LEGACY_BCRYPT));
    assert!(!hasher.verify_bcrypt(LEGACY_PASSWORD, "not-a-hash"));

    let stored = FederatedCredential::imported_bcrypt(LEGACY_BCRYPT.to_owned());
    let PasswordCheck::Valid { upgraded } =
        verify_and_maybe_rehash(&hasher, LEGACY_PASSWORD, Some(&stored))
    else {
        panic!("legacy row must verify");
    };
    let upgraded = upgraded.expect("bcrypt success must rehash");
    assert_eq!(upgraded.scheme, HashScheme::Argon2id);
    assert!(hasher.verify_argon2id(LEGACY_PASSWORD, &upgraded.hash));

    let native = FederatedCredential::argon2id(hasher.hash_argon2id(LEGACY_PASSWORD));
    assert_eq!(
        verify_and_maybe_rehash(&hasher, LEGACY_PASSWORD, Some(&native)),
        PasswordCheck::Valid { upgraded: None }
    );
    assert_eq!(
        verify_and_maybe_rehash(&hasher, LEGACY_PASSWORD, None),
        PasswordCheck::Invalid,
        "unknown users cost one dummy verify, then fail"
    );
}

#[test]
fn scheme_dispatch_never_sniffs() {
    let hasher = Argon2idHasher::new();
    let argon_hash = hasher.hash_argon2id(LEGACY_PASSWORD);
    assert!(hasher.verify(LEGACY_PASSWORD, &format!("bcrypt${LEGACY_BCRYPT}")));
    assert!(hasher.verify(LEGACY_PASSWORD, &format!("argon2id${argon_hash}")));
    assert!(!hasher.verify("wrong password here!", &format!("bcrypt${LEGACY_BCRYPT}")));
    for stored in [
        LEGACY_BCRYPT.to_owned(),
        argon_hash.clone(),
        format!("scrypt${argon_hash}"),
        "garbage".to_owned(),
        String::new(),
    ] {
        assert!(
            !hasher.verify(LEGACY_PASSWORD, &stored),
            "untagged or mistagged rows fail closed: {stored}"
        );
    }
    PasswordHasher::dummy_verify(&hasher);
    PasswordVerifier::dummy_verify(&hasher);
}

// ---------------------------------------------------------------------------
// Sessions over auth_tokens
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_store_round_trip_and_rejections() {
    let fx = fixture("sessions").await;
    let user = user_row("user-1", "ada");
    fx.auth.users.insert(user).await.unwrap();

    let live = tokens::hash_token("live-token");
    fx.auth
        .sessions
        .insert(session_row("sess-live", "user-1", live.clone(), TEST_NOW))
        .await
        .unwrap();
    let found = fx
        .auth
        .sessions
        .lookup_valid(&live, TEST_NOW)
        .await
        .unwrap();
    assert_eq!(found.map(|row| row.user_id).as_deref(), Some("user-1"));

    assert!(
        fx.auth
            .sessions
            .lookup_valid(&tokens::hash_token("unknown"), TEST_NOW)
            .await
            .unwrap()
            .is_none()
    );

    let revoked = tokens::hash_token("revoked-token");
    fx.auth
        .sessions
        .insert(session_row(
            "sess-revoked",
            "user-1",
            revoked.clone(),
            TEST_NOW,
        ))
        .await
        .unwrap();
    assert!(
        fx.auth
            .session_manager
            .revoke_scoped("user-1", "sess-revoked")
            .await
            .unwrap()
    );
    assert!(
        fx.auth
            .sessions
            .lookup_valid(&revoked, TEST_NOW)
            .await
            .unwrap()
            .is_none()
    );

    let old = tokens::hash_token("old-token");
    let mut row = session_row(
        "sess-old",
        "user-1",
        old.clone(),
        TEST_NOW - tokens::SESSION_MAX_AGE_SECS - 1,
    );
    row.expires_at = TEST_NOW - 1;
    fx.auth.sessions.insert(row).await.unwrap();
    assert!(
        fx.auth
            .sessions
            .lookup_valid(&old, TEST_NOW)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn last_seen_at_touch_is_throttled() {
    let fx = fixture("touch").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();

    let stale = tokens::hash_token("stale-token");
    fx.auth
        .sessions
        .insert(session_row("sess-stale", "user-1", stale.clone(), TEST_NOW))
        .await
        .unwrap();
    // Ten minutes later the read rewrites last_seen; the observation lands on
    // the next read, so the touch itself needs no second write to assert.
    let first = fx
        .auth
        .sessions
        .lookup_valid(&stale, TEST_NOW + 600)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.last_seen_at, TEST_NOW);
    let second = fx
        .auth
        .sessions
        .lookup_valid(&stale, TEST_NOW + 600)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.last_seen_at, TEST_NOW + 600);

    let fresh = tokens::hash_token("fresh-token");
    fx.auth
        .sessions
        .insert(session_row("sess-fresh", "user-1", fresh.clone(), TEST_NOW))
        .await
        .unwrap();
    let first = fx
        .auth
        .sessions
        .lookup_valid(&fresh, TEST_NOW + 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.last_seen_at, TEST_NOW);
    let second = fx
        .auth
        .sessions
        .lookup_valid(&fresh, TEST_NOW + 61)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        second.last_seen_at, TEST_NOW,
        "no rewrite inside the window"
    );

    let edge = tokens::hash_token("edge-token");
    fx.auth
        .sessions
        .insert(session_row("sess-edge", "user-1", edge.clone(), TEST_NOW))
        .await
        .unwrap();
    fx.auth
        .sessions
        .lookup_valid(&edge, TEST_NOW + 300)
        .await
        .unwrap()
        .unwrap();
    let second = fx
        .auth
        .sessions
        .lookup_valid(&edge, TEST_NOW + 300)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        second.last_seen_at,
        TEST_NOW + 300,
        "exactly five minutes touches"
    );
}

#[tokio::test]
async fn session_manager_lists_revokes_and_replaces() {
    let fx = fixture("manager").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();
    fx.auth
        .users
        .insert(user_row("user-2", "grace"))
        .await
        .unwrap();

    let live = tokens::hash_token("live-token");
    fx.auth
        .sessions
        .insert(session_row("sess-live", "user-1", live.clone(), TEST_NOW))
        .await
        .unwrap();
    let mut old = session_row(
        "sess-old",
        "user-1",
        tokens::hash_token("old-token"),
        TEST_NOW - 10,
    );
    old.expires_at = TEST_NOW - 1;
    fx.auth.sessions.insert(old).await.unwrap();

    let listed = fx
        .auth
        .session_manager
        .list_for_user("user-1")
        .await
        .unwrap();
    assert_eq!(listed.len(), 1, "expired rows stay out of listings");
    assert_eq!(listed[0].id, "sess-live");
    assert_eq!(listed[0].kind, UserSessionKind::Standard);

    assert!(
        fx.auth
            .session_manager
            .owner_by_hash(&live)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        fx.auth
            .session_manager
            .owner_by_hash(&tokens::hash_token("an-app-secret"))
            .await
            .unwrap()
            .is_none(),
        "compat secrets never resolve as native sessions"
    );

    assert!(
        !fx.auth
            .session_manager
            .revoke_scoped("user-2", "sess-live")
            .await
            .unwrap()
    );
    assert!(
        fx.auth
            .session_manager
            .revoke_scoped("user-1", "sess-live")
            .await
            .unwrap()
    );
    assert!(
        fx.auth
            .session_manager
            .list_for_user("user-1")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        fx.auth
            .session_manager
            .owner_by_hash(&live)
            .await
            .unwrap()
            .is_none()
    );

    let first = fx
        .auth
        .session_manager
        .replace_companion(
            "sess-comp-1",
            "user-1",
            &tokens::hash_token("comp-1"),
            "phone",
            TEST_NOW,
            tokens::expires_at(TEST_NOW),
        )
        .await
        .unwrap();
    assert_eq!(first.kind, UserSessionKind::Companion);
    fx.auth
        .session_manager
        .replace_companion(
            "sess-comp-2",
            "user-1",
            &tokens::hash_token("comp-2"),
            "phone",
            TEST_NOW,
            tokens::expires_at(TEST_NOW),
        )
        .await
        .unwrap();
    let listed = fx
        .auth
        .session_manager
        .list_for_user("user-1")
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].id, "sess-comp-2",
        "remint revokes the prior label token"
    );

    let revoked = fx
        .auth
        .session_manager
        .revoke_all_for_user("user-1")
        .await
        .unwrap();
    assert_eq!(
        revoked, 2,
        "the live companion row plus the expired-but-unrevoked row"
    );
    assert!(
        fx.auth
            .session_manager
            .list_for_user("user-1")
            .await
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Local credentials over auth_providers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_credential_shape_and_guarded_rehash() {
    let fx = fixture("local").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();
    let hasher = Argon2idHasher::new();
    let hash = hasher.hash_argon2id(LEGACY_PASSWORD);

    fx.auth
        .users
        .insert_local_credential(UsersCredential {
            id: "cred-1".to_owned(),
            user_id: "user-1".to_owned(),
            scheme: HashScheme::Argon2id.as_tag().to_owned(),
            hash: hash.clone(),
        })
        .await
        .unwrap();

    let stored = fx
        .auth
        .users
        .local_credential("user-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.scheme, "argon2id");
    assert!(hasher.verify_argon2id(LEGACY_PASSWORD, &stored.hash));

    let raw = raw_text(
        fx.runtime.pool(),
        "SELECT provider_data FROM auth_providers WHERE user_id = ? AND provider = 'local'",
        "user-1",
    )
    .await
    .unwrap();
    let document: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        document.get("password_hash").and_then(|v| v.as_str()),
        Some(hash.as_str())
    );
    assert_eq!(
        document.get("scheme").and_then(|v| v.as_str()),
        Some("argon2id")
    );

    assert!(
        !fx.auth
            .users
            .replace_local_hash("user-1", "stale-hash", "argon2id", &hash)
            .await
            .unwrap(),
        "a concurrent change must fail the guard, not silently win"
    );
    let rotated = hasher.hash_argon2id("a brand new password 99");
    assert!(
        fx.auth
            .users
            .replace_local_hash("user-1", &hash, "argon2id", &rotated)
            .await
            .unwrap()
    );
    let stored = fx
        .auth
        .users
        .local_credential("user-1")
        .await
        .unwrap()
        .unwrap();
    assert!(hasher.verify_argon2id("a brand new password 99", &stored.hash));
}

#[tokio::test]
async fn v2_import_rows_default_to_bcrypt_and_rehash_on_login() {
    let fx = fixture("import").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();
    // v2 verbatim shape: {"password_hash": ...} with no scheme tag.
    let legacy = serde_json::json!({ "password_hash": LEGACY_BCRYPT }).to_string();
    fx.runtime
        .lane()
        .write(Lane::Foreground, "test.seed", move |tx| {
            tx.execute(
                "INSERT INTO auth_providers (id, user_id, provider, provider_uid, provider_data, created_at) \
                 VALUES ('cred-legacy', 'user-1', 'local', 'ada', ?, '2023-11-14T22:13:20+00:00')",
                params![legacy],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let stored = fx
        .auth
        .users
        .local_credential("user-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.scheme, "bcrypt");

    // The real login path: verify, mint, and persist the upgrade in the same
    // transaction as the session row.
    let login = LoginService::new(
        fx.auth.sessions.clone(),
        fx.auth.hasher.clone(),
        fx.auth.credentials.clone(),
    );
    let now = now_unix();
    let success = login
        .login(
            LoginRequest {
                username: "ada".to_owned(),
                password: LEGACY_PASSWORD.to_owned(),
                transport: TransportParam::Cookie,
            },
            LoginContext {
                base_path: String::new(),
                secure: false,
                user_agent: None,
                now_unix: now,
            },
        )
        .await
        .unwrap();
    assert_eq!(success.user_id, "user-1");
    let found = fx
        .auth
        .sessions
        .lookup_valid(&tokens::hash_token(&success.raw_token), now)
        .await
        .unwrap();
    assert_eq!(found.map(|row| row.user_id).as_deref(), Some("user-1"));

    let stored = fx
        .auth
        .users
        .local_credential("user-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.scheme, "argon2id");
    let hasher = Argon2idHasher::new();
    assert!(hasher.verify_argon2id(LEGACY_PASSWORD, &stored.hash));
    let raw = raw_text(
        fx.runtime.pool(),
        "SELECT provider_data FROM auth_providers WHERE user_id = ? AND provider = 'local'",
        "user-1",
    )
    .await
    .unwrap();
    let document: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        document.get("scheme").and_then(|v| v.as_str()),
        Some("argon2id"),
        "the tag flips with the hash"
    );

    fx.auth
        .users
        .insert(user_row("user-2", "grace"))
        .await
        .unwrap();
    fx.runtime
        .lane()
        .write(Lane::Foreground, "test.seed", move |tx| {
            tx.execute(
                "INSERT INTO auth_providers (id, user_id, provider, provider_uid, provider_data, created_at) \
                 VALUES ('cred-broken', 'user-2', 'local', 'grace', 'not json', '2023-11-14T22:13:20+00:00')",
                params![],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        fx.auth
            .users
            .local_credential("user-2")
            .await
            .unwrap()
            .is_none(),
        "corrupt rows read as no credential"
    );
}

#[tokio::test]
async fn rename_keeps_the_local_credential_resolvable() {
    let fx = fixture("rename").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();
    fx.auth
        .users
        .insert_local_credential(UsersCredential {
            id: "cred-1".to_owned(),
            user_id: "user-1".to_owned(),
            scheme: HashScheme::Argon2id.as_tag().to_owned(),
            hash: Argon2idHasher::new().hash_argon2id(LEGACY_PASSWORD),
        })
        .await
        .unwrap();

    assert!(
        fx.auth
            .users
            .update_username("user-1", "ada2", "Ada2")
            .await
            .unwrap()
    );
    assert!(
        fx.auth
            .users
            .local_credential("user-1")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        fx.auth.credentials.local_user("ada2").await.is_some(),
        "the local binding tracks the rename"
    );
    assert!(fx.auth.credentials.local_user("ada").await.is_none());
}

#[tokio::test]
async fn credential_lookup_tags_hashes_and_hides_absences() {
    let fx = fixture("lookup").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();
    fx.runtime
        .lane()
        .write(Lane::Foreground, "test.seed", move |tx| {
            let legacy = serde_json::json!({ "password_hash": LEGACY_BCRYPT }).to_string();
            tx.execute(
                "INSERT INTO auth_providers (id, user_id, provider, provider_uid, provider_data, created_at) \
                 VALUES ('cred-legacy', 'user-1', 'local', 'ada', ?, '2023-11-14T22:13:20+00:00')",
                params![legacy],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fx.auth
        .users
        .insert(user_row("user-2", "grace"))
        .await
        .unwrap();

    let found = fx.auth.credentials.local_user("ada").await.unwrap();
    assert_eq!(found.user_id, "user-1");
    assert_eq!(found.stored_hash, format!("bcrypt${LEGACY_BCRYPT}"));
    assert!(Argon2idHasher::new().verify(LEGACY_PASSWORD, &found.stored_hash));

    assert!(fx.auth.credentials.local_user("nobody").await.is_none());
    assert!(fx.auth.credentials.local_user("grace").await.is_none());
}

// ---------------------------------------------------------------------------
// Federated import: sealed tokens
// ---------------------------------------------------------------------------

#[tokio::test]
async fn federated_tokens_are_sealed_at_rest() {
    let fx = fixture("federated").await;
    assert!(!fx.auth.federated.has_any_users().await.unwrap());
    let created = fx
        .auth
        .federated
        .create_user(NewFederatedUser {
            display_name: "Ada Doe".to_owned(),
            role: "user".to_owned(),
            email: Some("ada@example.com".to_owned()),
            avatar_url: None,
            username: "ada".to_owned(),
            username_display: "Ada".to_owned(),
        })
        .await
        .unwrap();
    assert!(fx.auth.federated.has_any_users().await.unwrap());

    let first_json = r#"{"access_token":"canary-at-9f27","refresh_token":"canary-rt-51"}"#;
    let binding = fx
        .auth
        .federated
        .create_provider(&created.id, "oidc", "sub-1", first_json)
        .await
        .unwrap();
    let raw = raw_text(
        fx.runtime.pool(),
        "SELECT provider_data FROM auth_providers WHERE id = ?",
        &binding.id,
    )
    .await
    .unwrap();
    assert!(raw.starts_with("v3:"), "{raw}");
    assert!(
        !raw.contains("canary"),
        "no plaintext token material at rest"
    );
    assert_eq!(fx.crypto.decrypt(&raw).unwrap(), first_json);

    let rotated_json = r#"{"access_token":"rotated-at-33"}"#;
    fx.auth
        .federated
        .update_provider_tokens(&binding.id, rotated_json)
        .await
        .unwrap();
    let rotated = raw_text(
        fx.runtime.pool(),
        "SELECT provider_data FROM auth_providers WHERE id = ?",
        &binding.id,
    )
    .await
    .unwrap();
    assert_ne!(rotated, raw, "fresh nonce per seal");
    assert_eq!(fx.crypto.decrypt(&rotated).unwrap(), rotated_json);

    let by_provider = fx
        .auth
        .federated
        .get_provider("oidc", "sub-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_provider.user_id, created.id);
    assert!(
        fx.auth
            .federated
            .get_user_by_email("ada@example.com")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        fx.auth
            .federated
            .get_user_by_username("ada")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        fx.auth
            .federated
            .get_user_by_id(&created.id)
            .await
            .unwrap()
            .is_some()
    );
}

// ---------------------------------------------------------------------------
// Session issuing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn issuer_mints_a_verifiable_session_and_stamps_login() {
    let fx = fixture("issuer").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();

    let raw = fx
        .auth
        .issuer
        .issue_session("user-1", Some("close-test"))
        .await
        .unwrap();
    assert_eq!(raw.len(), 44, "v2 token format: 32 bytes urlsafe-b64");
    let found = fx
        .auth
        .sessions
        .lookup_valid(&tokens::hash_token(&raw), now_unix())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.user_id, "user-1");
    assert_eq!(found.kind, SessionKind::Standard);

    let user = fx.auth.users.get_by_id("user-1").await.unwrap().unwrap();
    assert!(user.last_login_at.is_some_and(|at| at > TEST_NOW));
}

// ---------------------------------------------------------------------------
// App passwords: decrypt-free verify with touch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn app_password_verify_is_decrypt_free_and_touches_on_success() {
    let fx = fixture("app-passwords").await;
    let rig = TestRig::new().unwrap();
    let mut deps = rig.deps.clone();
    deps.users = Arc::new(fx.auth.users.clone());
    deps.app_passwords = Arc::new(fx.auth.app_passwords.clone());
    deps.passwords = Arc::new(Argon2idHasher::new());
    deps.clock = Arc::clone(&fx.clock) as Arc<dyn Clock>;

    deps.users.insert(user_row("user-1", "ada")).await.unwrap();
    let secret = tokens::mint_token().unwrap();
    let digest = tokens::hash_token(&secret);
    let sealed = fx.crypto.encrypt(&secret).unwrap();
    deps.app_passwords
        .insert(AppPasswordRecord {
            id: "ap-1".to_owned(),
            user_id: "user-1".to_owned(),
            name: "phone".to_owned(),
            secret_sha256: digest.clone(),
            secret_encrypted: sealed,
            created_at: TEST_NOW,
            last_used_at: None,
            last_client: None,
        })
        .await
        .unwrap();

    let authed = verify_app_password(&deps, &secret, Some("Symfonium"))
        .await
        .unwrap();
    assert_eq!(authed.user_id, "user-1");
    assert_eq!(authed.app_password_id, "ap-1");
    let row = deps.app_passwords.get_by_id("ap-1").await.unwrap().unwrap();
    assert_eq!(row.last_used_at, Some(TEST_NOW));
    assert_eq!(row.last_client.as_deref(), Some("Symfonium"));
    assert_eq!(fx.crypto.decrypt(&row.secret_encrypted).unwrap(), secret);

    assert!(
        verify_app_password(&deps, "wrong-secret", None)
            .await
            .is_none()
    );
    assert!(verify_app_password(&deps, "", None).await.is_none());
    assert!(deps.app_passwords.revoke("ap-1").await.unwrap());
    assert!(verify_app_password(&deps, &secret, None).await.is_none());
    assert!(
        deps.app_passwords
            .get_active_by_sha256(&digest)
            .await
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Recovery codes + OIDC states
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_codes_replace_and_expire() {
    let fx = fixture("recovery").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();

    fx.auth
        .recovery
        .store(RecoveryCode {
            user_id: "user-1".to_owned(),
            code_hash: "hash-1".to_owned(),
            created_at: TEST_NOW,
            expires_at: TEST_NOW + 900,
        })
        .await
        .unwrap();
    assert!(
        fx.auth
            .recovery
            .find_live_by_hash("hash-1", TEST_NOW)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        fx.auth
            .recovery
            .find_live_by_hash("hash-1", TEST_NOW + 901)
            .await
            .unwrap()
            .is_none()
    );
    fx.auth.recovery.delete_for_user("user-1").await.unwrap();
    assert!(
        fx.auth
            .recovery
            .find_live_by_hash("hash-1", TEST_NOW)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn oidc_states_consume_exactly_once() {
    let fx = fixture("oidc").await;
    fx.auth
        .oidc_states
        .store_state("state-1", "verifier-1")
        .await
        .unwrap();
    assert_eq!(
        fx.auth
            .oidc_states
            .consume_state("state-1")
            .await
            .unwrap()
            .as_deref(),
        Some("verifier-1")
    );
    assert!(
        fx.auth
            .oidc_states
            .consume_state("state-1")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fx.auth
            .oidc_states
            .consume_state("unknown")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn lastfm_links_round_trip() {
    let fx = fixture("lastfm").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();
    assert!(fx.auth.lastfm.get("user-1").await.unwrap().is_none());
    let link = LastFmConnection {
        configured: true,
        api_key_encrypted: Some(fx.crypto.encrypt("key-1").unwrap()),
        shared_secret_encrypted: Some(fx.crypto.encrypt("secret-1").unwrap()),
        username: Some("ada-fm".to_owned()),
        session_key_encrypted: Some(fx.crypto.encrypt("session-1").unwrap()),
    };
    fx.auth.lastfm.upsert("user-1", link.clone()).await.unwrap();
    assert_eq!(fx.auth.lastfm.get("user-1").await.unwrap(), Some(link));
    assert!(fx.auth.lastfm.delete("user-1").await.unwrap());
    assert!(fx.auth.lastfm.get("user-1").await.unwrap().is_none());
}

// ---------------------------------------------------------------------------
// md5 swap pins
// ---------------------------------------------------------------------------

#[test]
fn md5_swap_keeps_rfc_and_v2_spot_pins() {
    for (input, expected) in [
        ("", "d41d8cd98f00b204e9800998ecf8427e"),
        ("a", "0cc175b9c0f1b6a831c399e269772661"),
        ("abc", "900150983cd24fb0d6963f7d28e17f72"),
        ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
        (
            "abcdefghijklmnopqrstuvwxyz",
            "c3fcd3d76192e4007dfb496cca67e13b",
        ),
    ] {
        assert_eq!(md5_hex(input), expected, "{input:?}");
    }
    assert_eq!(
        md5_hex("alice-secretpepper"),
        "22b231fb88294b6a25be3d978438fdc1"
    );
}

// ---------------------------------------------------------------------------
// Fix-up briefs: remint races, recovery sweep, atomic reset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn companion_concurrent_remint_leaves_exactly_one_live_row() {
    let fx = fixture("remint-race").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();
    let mut handles = Vec::new();
    for n in 0..8u32 {
        let manager = fx.auth.session_manager.clone();
        handles.push(tokio::spawn(async move {
            let id = format!("sess-race-{n}");
            let digest = tokens::hash_token(&format!("race-token-{n}"));
            manager
                .replace_companion(
                    &id,
                    "user-1",
                    &digest,
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
        1,
        "concurrent remints converge on one live same-label row"
    );
}

#[tokio::test]
async fn recovery_store_sweeps_expired_rows() {
    let fx = fixture("recovery-sweep").await;
    fx.auth
        .users
        .insert(user_row("user-1", "ada"))
        .await
        .unwrap();
    fx.auth
        .users
        .insert(user_row("user-2", "grace"))
        .await
        .unwrap();
    // An expired row, written around the sweep (store itself sweeps).
    fx.runtime
        .lane()
        .write(Lane::Foreground, "test.seed", move |tx| {
            tx.execute(
                "INSERT INTO auth_password_recovery_codes \
                 (user_id, code_hash, created_at, expires_at) \
                 VALUES ('user-1', 'hash-old', '2020-01-01T00:00:00+00:00', \
                 '2020-01-01T00:15:00+00:00')",
                params![],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let now = now_unix();
    fx.auth
        .recovery
        .store(RecoveryCode {
            user_id: "user-2".to_owned(),
            code_hash: "hash-fresh".to_owned(),
            created_at: now,
            expires_at: now + 900,
        })
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_password_recovery_codes")
        .fetch_one(fx.runtime.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "minting sweeps rows already expired");
    assert!(
        fx.auth
            .recovery
            .find_live_by_hash("hash-fresh", now)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn recovery_reset_revokes_sessions_and_consumes_code() {
    let fx = fixture("recovery-reset").await;
    let rig = TestRig::new().unwrap();
    // SQLite under test, rig clock and fast hasher on top.
    let mut deps = rig.deps.clone();
    deps.users = Arc::new(fx.auth.users.clone());
    deps.sessions = Arc::new(fx.auth.session_manager.clone());
    deps.recovery = Arc::new(fx.auth.recovery.clone());

    deps.users.insert(user_row("user-1", "ada")).await.unwrap();
    deps.users
        .insert_local_credential(UsersCredential {
            id: "cred-1".to_owned(),
            user_id: "user-1".to_owned(),
            scheme: HashScheme::Argon2id.as_tag().to_owned(),
            hash: Sha256TestHasher::test_hash("old password here"),
        })
        .await
        .unwrap();
    fx.auth
        .sessions
        .insert(session_row(
            "sess-1",
            "user-1",
            tokens::hash_token("prior-token"),
            TEST_NOW,
        ))
        .await
        .unwrap();
    let minted = services::admin_mint_recovery_code(&deps, "user-1")
        .await
        .unwrap();
    assert_eq!(
        deps.sessions.list_for_user("user-1").await.unwrap().len(),
        1
    );

    services::reset_password(
        &deps,
        &PasswordReset {
            username: "ada".to_owned(),
            recovery_code: minted.recovery_code,
            new_password: "a brand new password 99".to_owned(),
        },
    )
    .await
    .unwrap();

    assert!(
        deps.sessions
            .list_for_user("user-1")
            .await
            .unwrap()
            .is_empty(),
        "reset kills prior sessions"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_password_recovery_codes")
        .fetch_one(fx.runtime.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "reset consumes the code");
    let stored = deps
        .users
        .local_credential("user-1")
        .await
        .unwrap()
        .unwrap();
    assert!(
        deps.passwords
            .verify_argon2id("a brand new password 99", &stored.hash)
    );
}
