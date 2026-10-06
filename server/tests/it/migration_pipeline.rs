//! Stage-11 pipeline E2E briefs: a live v2 fixture dir travels export →
//! validator → dry-run → import, then the migrated instance proves itself.
//!
//! One behavior per brief, each on its own scratch fixture: refusal of the
//! orphaned export, the operator repair, imported counts, login with v2
//! passwords (rehashing to Argon2id), follows intact, compat re-auth,
//! settings landed, and idempotent re-import. Nothing here touches the
//! network; every database is a scratch file.

use std::path::{Path, PathBuf};

use droppedneedle::db::WriteLane;
use droppedneedle::runtime_config::{Crypto, Secret};
use droppedneedle::tooling::fixture::{UNKNOWN_USER_ID, build_v2_fixture, delete_orphan_rows};

/// Scratch directory per brief, removed on drop.
fn scratch_dir(tag: &str) -> crate::common::ScratchDir {
    crate::common::ScratchDir::new(&format!("pipeline-{tag}"))
}

fn export_request(v2_root: &Path, passphrase: &str) -> droppedneedle::export::ExportRequest {
    droppedneedle::export::ExportRequest {
        v2_root: v2_root.to_path_buf(),
        db_path: None,
        passphrase: Secret::new(passphrase.to_owned()),
        exported_at: None,
        v2_commit: None,
    }
}

fn issue_codes(report: &droppedneedle::r#import::ValidationReport) -> (Vec<&str>, Vec<&str>) {
    (
        report.errors.iter().map(|issue| issue.code).collect(),
        report.warnings.iter().map(|issue| issue.code).collect(),
    )
}

const PASSPHRASE: &str = "operator-passphrase";

/// A fixture plus its first export, orphans included.
fn orphaned_export(root: &Path) -> (droppedneedle::tooling::fixture::V2Fixture, String) {
    let v2_root = root.join("v2");
    let fixture = build_v2_fixture(&v2_root).expect("fixture builds");
    let export_path = root.join("export.json");
    droppedneedle::export::export_v2_to_file(&export_request(&v2_root, PASSPHRASE), &export_path)
        .expect("export writes");
    let text = std::fs::read_to_string(&export_path).expect("export reads back");
    (fixture, text)
}

/// A fixture plus its export after the operator repair: the orphan rows
/// are dropped from v2 and the file is re-exported clean.
fn repaired_export(root: &Path) -> (droppedneedle::tooling::fixture::V2Fixture, String) {
    let (fixture, _) = orphaned_export(root);
    let export_path = root.join("export.json");
    assert_eq!(
        delete_orphan_rows(&root.join("v2").join("cache").join("library.db")).expect("repair runs"),
        2
    );
    droppedneedle::export::export_v2_to_file(
        &export_request(&root.join("v2"), PASSPHRASE),
        &export_path,
    )
    .expect("re-export writes");
    let text = std::fs::read_to_string(&export_path).expect("re-export reads back");
    (fixture, text)
}

/// A migrated scratch v3 target with the conflict seeds: strangers holding
/// both fixture emails, and a Plex binding under a third account. A
/// writable pool: the serving runtime's pool is read-only by design, and
/// the offline import owns its writes while the server is stopped.
async fn seeded_target(root: &Path) -> (PathBuf, sqlx::SqlitePool) {
    let v3_root = root.join("v3");
    std::fs::create_dir_all(v3_root.join("config")).expect("v3 config dir");
    std::fs::create_dir_all(v3_root.join("cache")).expect("v3 cache dir");
    let db_path = v3_root.join("cache").join("library.db");
    let pool = writable_pool(&db_path).await;
    droppedneedle::schema::apply_migrations(&pool)
        .await
        .expect("scratch schema migrates");
    seed_conflicts(&pool).await;
    (v3_root, pool)
}

#[tokio::test]
async fn orphaned_export_refuses_with_zero_writes() {
    let root = scratch_dir("refuses");
    let (_fixture, text) = orphaned_export(&root);
    let parsed = droppedneedle::export::parse_export(&text).expect("envelope parses");
    assert_eq!(parsed.doc.users.len(), 2);
    assert_eq!(parsed.doc.follows.len(), 4);
    assert_eq!(parsed.doc.approvals.len(), 4);

    let root_value: serde_json::Value = serde_json::from_str(&text).expect("json parses");
    let validation = droppedneedle::r#import::validate_export(&root_value);
    assert!(!validation.valid());
    let (errors, warnings) = issue_codes(&validation);
    assert!(errors.contains(&"DANGLING_USER_REF"), "errors: {errors:?}");
    assert!(
        warnings.contains(&"DANGLING_REVIEWER"),
        "warnings: {warnings:?}"
    );
    assert!(
        warnings.contains(&"REVOKED_APP_PASSWORD_KEPT"),
        "warnings: {warnings:?}"
    );

    let (v3_root, pool) = seeded_target(&root).await;
    let counts_before = table_counts(&pool).await;
    for dry_run in [true, false] {
        let report = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, dry_run).await;
        assert_eq!(
            report.exit.code,
            droppedneedle::r#import::ExitCode::FailedValidation,
            "dry_run={dry_run}"
        );
    }
    assert_eq!(table_counts(&pool).await, counts_before);
    assert!(!v3_root.join("config").join("config.json").exists());
}

#[tokio::test]
async fn operator_repair_converges_to_clean_import() {
    let root = scratch_dir("repair");
    let (_fixture, text) = repaired_export(&root);
    let root_value: serde_json::Value = serde_json::from_str(&text).expect("json parses");
    let validation = droppedneedle::r#import::validate_export(&root_value);
    assert!(validation.valid(), "errors: {:?}", validation.errors);
    let (_, warnings) = issue_codes(&validation);
    assert!(warnings.contains(&"DANGLING_REVIEWER"));

    let (v3_root, pool) = seeded_target(&root).await;
    let counts_before = table_counts(&pool).await;
    let dry_report = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, true).await;
    assert_eq!(
        dry_report.exit.code,
        droppedneedle::r#import::ExitCode::OkWithDrops
    );
    assert_eq!(table_counts(&pool).await, counts_before);
    assert!(!v3_root.join("config").join("config.json").exists());
    let report = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    assert_eq!(
        report.exit.code,
        droppedneedle::r#import::ExitCode::OkWithDrops
    );
    assert_eq!(report.entities, dry_report.entities, "dry-run parity");
    assert_eq!(
        report.secrets_reencrypted, dry_report.secrets_reencrypted,
        "dry-run parity"
    );
}

#[tokio::test]
async fn imported_counts_match_the_fixture() {
    // Two users in (both emails nulled onto the pre-seeded strangers), two
    // local providers in, the Plex binding kept on its existing owner,
    // three app passwords, one recovery code, three follows, three
    // approvals with one reviewer nulled.
    let root = scratch_dir("counts");
    let (_fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    let report = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    assert_eq!(
        report.exit.code,
        droppedneedle::r#import::ExitCode::OkWithDrops
    );
    let count = |entity: &str| report.entities.get(entity).expect("entity counted").clone();
    assert_eq!(count("user").imported, 2);
    assert_eq!(count("user").nulled_field, 2);
    assert_eq!(count("provider").imported, 2);
    assert_eq!(count("provider").conflict_kept_existing, 1);
    assert_eq!(count("app_password").imported, 3);
    assert_eq!(count("recovery_code").imported, 1);
    assert_eq!(count("follow").imported, 3);
    assert_eq!(count("approval").imported, 3);
    assert_eq!(count("approval").nulled_field, 1);
    assert_eq!(count("event_city").imported, 2);
    assert_eq!(count("event_seen").imported, 1);
    // Eight settings seals (slskd, sabnzbd, two indexers, AudioDB, one
    // plugin token, the Last.fm key pair) plus three app passwords; v2's
    // global Last.fm session key is dropped and counts none.
    assert_eq!(report.secrets_reencrypted, 11);
    // The v2 instance Last.fm key pair opens under the v3 key, so linking
    // and scrobbling keep working after the move.
    let config_dir = v3_root.join("config");
    let store = droppedneedle::runtime_config::ConfigStore::open(
        &config_dir.join("config.json"),
        Crypto::load(&config_dir).expect("v3 key loads"),
    )
    .expect("v3 config opens");
    let lastfm: droppedneedle::runtime_config::secret_sections::LastFmSettings =
        store.get_raw().expect("lastfm settings read");
    assert_eq!(lastfm.api_key.expose(), "lastfm-public-key");
    assert_eq!(lastfm.shared_secret.expose(), "lastfm-shared-plaintext");

    let revoked: Vec<(String, i64)> =
        sqlx::query_as("SELECT name, revoked FROM connect_app_passwords WHERE revoked = 1")
            .fetch_all(&pool)
            .await
            .expect("revoked rows read");
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].0, "old-laptop");

    // No sessions survive the import itself: the file carries none.
    let tokens: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_tokens")
        .fetch_one(&pool)
        .await
        .expect("tokens count");
    assert_eq!(tokens, 0);
}

#[tokio::test]
async fn reviewers_land_kept_or_nulled() {
    let root = scratch_dir("reviewers");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;

    // Bob's approval was reviewed by Alice: a valid reviewer survives
    // verbatim.
    let kept: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT reviewed_by_id, reviewed_by_name FROM auto_download_approvals WHERE user_id = ?",
    )
    .bind(&fixture.bob_id)
    .fetch_one(&pool)
    .await
    .expect("bob approval reads");
    assert_eq!(
        kept,
        (Some(fixture.alice_id.clone()), Some("Alice".to_owned()))
    );

    // Alice's second approval was reviewed by a deleted user: the id nulls
    // and the name stays as audit residue.
    let nulled: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT reviewed_by_id, reviewed_by_name FROM auto_download_approvals \
         WHERE user_id = ? AND artist_mbid = ?",
    )
    .bind(&fixture.alice_id)
    .bind(&fixture.artist_two_mbid)
    .fetch_one(&pool)
    .await
    .expect("alice approval reads");
    assert_eq!(nulled, (None, Some("Gone".to_owned())));
}

#[tokio::test]
async fn v2_passwords_log_in_and_rehash() {
    let root = scratch_dir("login");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    let lane =
        WriteLane::open(&v3_root.join("cache").join("library.db")).expect("writer lane opens");
    login_rehashes(
        &pool,
        &lane,
        "alice",
        &fixture.alice_id,
        &fixture.secrets.alice_password,
    )
    .await;
    login_rehashes(
        &pool,
        &lane,
        "bob",
        &fixture.bob_id,
        &fixture.secrets.bob_password,
    )
    .await;
}

#[tokio::test]
async fn follows_survive_intact() {
    let root = scratch_dir("follows");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    assert_follows_intact(&pool, &fixture).await;
}

#[tokio::test]
async fn compat_clients_reauth_on_surviving_secrets() {
    let root = scratch_dir("compat");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    let lane =
        WriteLane::open(&v3_root.join("cache").join("library.db")).expect("writer lane opens");
    assert_compat_reauth(&pool, &lane, &v3_root, &fixture).await;
}

#[tokio::test]
async fn settings_land_with_r8_and_no_plaintext() {
    let root = scratch_dir("settings");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    assert_settings_landed(&v3_root, &fixture);

    let config_text = std::fs::read_to_string(v3_root.join("config").join("config.json"))
        .expect("v3 config reads");
    for secret in [
        fixture.secrets.slskd_key.as_str(),
        fixture.secrets.audiodb_key.as_str(),
        fixture.secrets.indexer_keys[0].as_str(),
        fixture.secrets.indexer_keys[1].as_str(),
        fixture.secrets.plugin_token.as_str(),
        fixture.secrets.alice_phone_secret.as_str(),
        fixture.secrets.alice_old_secret.as_str(),
        fixture.secrets.bob_legacy_secret.as_str(),
    ] {
        assert!(
            !config_text.contains(secret),
            "v3 config leaks a plaintext secret"
        );
    }
}

#[tokio::test]
async fn reimport_writes_nothing() {
    let root = scratch_dir("reimport");
    let (_fixture, text) = repaired_export(&root);
    let (v3_root, pool) = seeded_target(&root).await;
    run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM import_runs")
        .fetch_one(&pool)
        .await
        .expect("import runs count");
    assert_eq!(runs, 1);

    let config_path = v3_root.join("config").join("config.json");
    let bytes_before = std::fs::read(&config_path).expect("v3 config reads");
    let again = run_pipeline(&text, PASSPHRASE, &v3_root, &pool, false).await;
    for (entity, counts) in &again.entities {
        assert_eq!(counts.imported, 0, "entity {entity} wrote nothing twice");
    }
    assert_eq!(
        std::fs::read(&config_path).expect("v3 config re-reads"),
        bytes_before,
        "config bytes stay stable across re-imports"
    );
    let runs_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM import_runs")
        .fetch_one(&pool)
        .await
        .expect("import runs recount");
    assert_eq!(runs_after, 1, "a pure re-import writes no audit row");
}

/// Pre-seed the target with the conflict cases: strangers holding both
/// fixture emails, and a Plex binding under a third account that collides
/// with Bob's.
async fn seed_conflicts(pool: &sqlx::SqlitePool) {
    for (id, email, username) in [
        ("carol-id", "alice@example.com", "carol"),
        ("dave-id", "bob@example.com", "dave"),
    ] {
        sqlx::query(
            "INSERT INTO auth_users (id, display_name, email, role, created_at, username)
             VALUES (?, ?, ?, 'user', '2026-01-01T00:00:00+00:00', ?)",
        )
        .bind(id)
        .bind(username)
        .bind(email)
        .bind(username)
        .execute(pool)
        .await
        .expect("conflict user seeds");
    }
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at, username)
         VALUES ('mallory-id', 'Mallory', 'user', '2026-01-01T00:00:00+00:00', 'mallory')",
    )
    .execute(pool)
    .await
    .expect("mallory seeds");
    sqlx::query(
        "INSERT INTO auth_providers (id, user_id, provider, provider_uid, provider_data,
                                     created_at)
         VALUES ('prov-mallory', 'mallory-id', 'plex', 'plex-uid-bob',
                 '{\"token\": \"mallory\"}', '2026-01-01T00:00:00+00:00')",
    )
    .execute(pool)
    .await
    .expect("plex binding seeds");
}

/// Row counts per migrated table, for the zero-writes proofs.
async fn table_counts(pool: &sqlx::SqlitePool) -> Vec<(String, i64)> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .expect("tables list");
    let mut counts = Vec::new();
    for table in tables {
        let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM \"{table}\""))
            .fetch_one(pool)
            .await
            .expect("table counts");
        counts.push((table, total));
    }
    counts
}

/// One-connection writable pool over a scratch file.
async fn writable_pool(db_path: &std::path::Path) -> sqlx::SqlitePool {
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite:{}?mode=rwc", db_path.display()))
        .await
        .expect("writable pool opens")
}

/// Drive the real import pipeline (dry-run or real) over the scratch pool.
async fn run_pipeline(
    export_text: &str,
    passphrase: &str,
    v3_root: &Path,
    pool: &sqlx::SqlitePool,
    dry_run: bool,
) -> droppedneedle::r#import::ImportReport {
    let config_dir = v3_root.join("config");
    let crypto = if dry_run {
        Crypto::from_key_bytes(&[9u8; 32]).expect("dry key builds")
    } else {
        Crypto::load_or_generate(&config_dir).expect("v3 key loads")
    };
    droppedneedle::r#import::run_import(droppedneedle::r#import::ImportRequest {
        export_bytes: export_text.as_bytes().to_vec(),
        passphrase: passphrase.to_owned(),
        pool: pool.clone(),
        config_path: config_dir.join("config.json"),
        crypto,
        v2_config_path: Some(
            v3_root
                .join("..")
                .join("v2")
                .join("config")
                .join("config.json"),
        ),
        dry_run,
        fault_before_commit: false,
        fault_after_commit: false,
    })
    .await
}

/// One user logs in with their v2 password through the production login
/// path; success persists the Argon2id rehash, and a wrong password still
/// fails.
async fn login_rehashes(
    pool: &sqlx::SqlitePool,
    lane: &droppedneedle::db::WriteLane,
    username: &str,
    user_id: &str,
    password: &str,
) {
    use droppedneedle::auth::session::login::{
        LoginContext, LoginError, LoginRequest, LoginService, TransportParam,
    };
    use droppedneedle::auth::sqlite::{AuthDb, SqliteCredentialLookup, SqliteSessionStore};

    let before: String = sqlx::query_scalar(
        "SELECT provider_data FROM auth_providers WHERE user_id = ? AND provider = 'local'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("provider row reads");
    assert!(before.contains("$2b$"), "bcrypt carried verbatim: {before}");

    let auth_db = AuthDb::new(pool, lane);
    let hasher = droppedneedle::auth::passwords::Argon2idHasher::new();
    let mut sessions = SqliteSessionStore::new(&auth_db);
    sessions.set_rehash_queue(hasher.rehash_queue().clone());
    let login = LoginService::new(sessions, hasher, SqliteCredentialLookup::new(&auth_db));
    let context = || LoginContext {
        user_agent: None,
        now_unix: 1_769_000_000,
    };
    let success = login
        .login(
            LoginRequest {
                username: username.to_owned(),
                password: password.to_owned(),
                transport: TransportParam::Bearer,
            },
            context(),
        )
        .await
        .expect("v2 password logs in");
    assert_eq!(success.user_id, user_id);

    let after: String = sqlx::query_scalar(
        "SELECT provider_data FROM auth_providers WHERE user_id = ? AND provider = 'local'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("provider row re-reads");
    assert!(after.contains("$argon2id$"), "rehash persisted: {after}");
    assert!(
        after.contains("\"scheme\":\"argon2id\""),
        "tag flipped: {after}"
    );

    let denied = login
        .login(
            LoginRequest {
                username: username.to_owned(),
                password: "wrong-password".to_owned(),
                transport: TransportParam::Bearer,
            },
            context(),
        )
        .await;
    assert!(matches!(denied, Err(LoginError::InvalidCredentials)));
}

/// Every imported follow reads back with its v2 values.
async fn assert_follows_intact(
    pool: &sqlx::SqlitePool,
    fixture: &droppedneedle::tooling::fixture::V2Fixture,
) {
    let rows: Vec<(String, String, String, i64, f64, f64)> = sqlx::query_as(
        "SELECT user_id, artist_mbid, artist_name, auto_download, followed_at, updated_at
         FROM user_followed_artists ORDER BY user_id, artist_mbid",
    )
    .fetch_all(pool)
    .await
    .expect("follows read back");
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|row| row.0 == fixture.alice_id
        && row.1 == fixture.artist_one_mbid
        && row.2 == "Fixture Artist One"
        && row.3 == 1));
    assert!(
        rows.iter()
            .any(|row| row.0 == fixture.alice_id && row.1 == fixture.artist_two_mbid && row.3 == 0)
    );
    assert!(
        rows.iter()
            .any(|row| row.0 == fixture.bob_id && row.1 == fixture.artist_one_mbid)
    );
    assert!(rows.iter().all(|row| row.0 != UNKNOWN_USER_ID));
}

/// The compat re-auth production path: the surviving secret resolves by its
/// recomputed SHA-256, decrypts under the v3 key, stamps use; the revoked
/// secret resolves nowhere.
async fn assert_compat_reauth(
    pool: &sqlx::SqlitePool,
    lane: &droppedneedle::db::WriteLane,
    v3_root: &Path,
    fixture: &droppedneedle::tooling::fixture::V2Fixture,
) {
    use droppedneedle::auth::session::tokens::hash_token;
    use droppedneedle::auth::sqlite::{AuthDb, SqliteAppPasswordStore};
    use droppedneedle::auth::users::stores::AppPasswordStore;

    let auth_db = AuthDb::new(pool, lane);
    let store = SqliteAppPasswordStore::new(&auth_db);
    let crypto = Crypto::load(&v3_root.join("config")).expect("v3 key loads");

    let row = store
        .get_active_by_sha256(&hash_token(&fixture.secrets.alice_phone_secret))
        .await
        .expect("lookup runs")
        .expect("surviving secret resolves");
    assert_eq!(row.user_id, fixture.alice_id);
    assert_eq!(row.name, "phone");
    assert_eq!(
        crypto
            .decrypt(&row.secret_encrypted)
            .expect("v3 ciphertext opens"),
        fixture.secrets.alice_phone_secret
    );
    store
        .touch(&row.secret_sha256, 1_769_000_100, Some("fixture-client"))
        .await
        .expect("touch runs");
    let touched = store
        .get_active_by_sha256(&row.secret_sha256)
        .await
        .expect("re-lookup runs")
        .expect("row still active");
    assert_eq!(touched.last_client.as_deref(), Some("fixture-client"));

    let revoked = store
        .get_active_by_sha256(&hash_token(&fixture.secrets.alice_old_secret))
        .await
        .expect("revoked lookup runs");
    assert!(revoked.is_none(), "revocation survived the import");

    let legacy = store
        .get_active_by_sha256(&hash_token(&fixture.secrets.bob_legacy_secret))
        .await
        .expect("legacy lookup runs")
        .expect("legacy-plaintext secret resolves");
    assert_eq!(legacy.user_id, fixture.bob_id);
}

/// Settings assertions: R8 carried the legacy frequency, secrets decrypt
/// under the v3 key, dropped fields stayed behind, order and ids survived.
fn assert_settings_landed(v3_root: &Path, fixture: &droppedneedle::tooling::fixture::V2Fixture) {
    let text = std::fs::read_to_string(v3_root.join("config").join("config.json"))
        .expect("v3 config reads");
    let config: serde_json::Value = serde_json::from_str(&text).expect("v3 config parses");
    assert_eq!(
        config
            .get("instance_id")
            .and_then(serde_json::Value::as_str),
        Some(fixture.instance_id.as_str())
    );
    assert_eq!(
        config
            .get("library_scan_schedule")
            .and_then(|section| section.get("scan_frequency"))
            .and_then(serde_json::Value::as_str),
        Some("6hr"),
        "R8 carried the legacy frequency"
    );
    let crypto = Crypto::load(&v3_root.join("config")).expect("v3 key loads");
    let slskd = config
        .get("download_client")
        .and_then(|section| section.get("api_key"))
        .and_then(serde_json::Value::as_str)
        .expect("slskd key present");
    assert_eq!(
        crypto.decrypt(slskd).expect("slskd key opens"),
        fixture.secrets.slskd_key
    );
    let audio = config
        .get("advanced_settings")
        .and_then(|section| section.get("audiodb_api_key"))
        .and_then(serde_json::Value::as_str)
        .expect("audiodb key present");
    assert_eq!(
        crypto.decrypt(audio).expect("audiodb key opens"),
        fixture.secrets.audiodb_key
    );
    assert!(
        config
            .get("advanced_settings")
            .and_then(|section| section.get("artist_discovery_warm_interval"))
            .is_none(),
        "dropped advanced field stayed behind"
    );
    let indexers = config
        .get("indexers")
        .and_then(serde_json::Value::as_array)
        .expect("indexers present");
    assert_eq!(indexers.len(), 2);
    assert_eq!(
        indexers[0].get("name").and_then(serde_json::Value::as_str),
        Some("first"),
        "indexer order survived"
    );
}
