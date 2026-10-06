//! Tooling CLI tests: every `droppedneedle-tool` subcommand runs
//! as the real binary, plus the conflict-merge decisions, the offline
//! restore guards, and the dev-only covers-debug gate.
//!
//! Nothing here touches the network; every database is a scratch file.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::common;

/// Scratch directory, removed when the test ends.
fn scratch_dir(tag: &str) -> crate::common::ScratchDir {
    crate::common::ScratchDir::new(tag)
}

/// The compiled tool binary under test: Cargo's per-test variable when
/// present, else the binary beside the test executable's target dir.
fn tool() -> PathBuf {
    for var in [
        "CARGO_BIN_EXE_droppedneedle_tool",
        "CARGO_BIN_EXE_droppedneedle-tool",
    ] {
        if let Ok(path) = std::env::var(var) {
            return PathBuf::from(path);
        }
    }
    std::env::current_exe()
        .expect("test exe path")
        .parent()
        .and_then(|deps| deps.parent())
        .expect("target dir")
        .join("droppedneedle-tool")
}

fn passphrase_file(dir: &std::path::Path, passphrase: &str) -> PathBuf {
    let path = dir.join("passphrase.txt");
    std::fs::write(&path, passphrase).expect("passphrase file writes");
    path
}

/// A repaired (orphan-free) export file plus its fixture, via the library.
fn repaired_export(
    root: &std::path::Path,
) -> (droppedneedle::tooling::fixture::V2Fixture, PathBuf) {
    let fixture = droppedneedle::tooling::fixture::build_v2_fixture(&root.join("v2"))
        .expect("fixture builds");
    droppedneedle::tooling::fixture::delete_orphan_rows(
        &fixture.root.join("cache").join("library.db"),
    )
    .expect("repair runs");
    let request = droppedneedle::export::ExportRequest {
        v2_root: fixture.root.clone(),
        db_path: None,
        passphrase: droppedneedle::runtime_config::Secret::new("operator-passphrase"),
        exported_at: None,
        v2_commit: None,
    };
    let path = root.join("export.json");
    droppedneedle::export::export_v2_to_file(&request, &path).expect("export writes");
    (fixture, path)
}

#[test]
fn export_writes_then_refuses_a_keyless_instance() {
    let root = scratch_dir("export");
    let fixture = droppedneedle::tooling::fixture::build_v2_fixture(&root.join("v2"))
        .expect("fixture builds");
    let out = root.join("out.json");

    // Passphrase over stdin: the happy path writes exactly one file.
    let mut child = Command::new(tool())
        .args(["export", "--v2-root"])
        .arg(&fixture.root)
        .args(["--out"])
        .arg(&out)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tool spawns");
    use std::io::Write as _;
    child
        .stdin
        .as_mut()
        .expect("stdin pipes")
        .write_all(b"operator-passphrase\n")
        .expect("passphrase writes");
    let status = child.wait_with_output().expect("tool runs");
    assert!(
        status.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(out.is_file());

    // Without the v2 key file the exporter refuses and writes nothing.
    std::fs::remove_file(fixture.root.join("config").join(".env")).expect("key removed");
    let missing = root.join("missing.json");
    let pass = passphrase_file(&root, "operator-passphrase");
    let status = Command::new(tool())
        .args(["export", "--v2-root"])
        .arg(&fixture.root)
        .args(["--out"])
        .arg(&missing)
        .args(["--passphrase-file"])
        .arg(&pass)
        .output()
        .expect("tool runs");
    assert!(!status.status.success());
    assert!(
        String::from_utf8_lossy(&status.stderr).contains("V2_KEY_NOT_FOUND"),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(!missing.exists());
}

#[test]
fn validate_accepts_rejects_and_cross_checks() {
    let root = scratch_dir("validate");
    let (_fixture, clean) = repaired_export(&root);
    let text = std::fs::read_to_string(&clean).expect("export reads");

    let status = Command::new(tool())
        .args(["validate"])
        .arg(&clean)
        .output()
        .expect("tool runs");
    assert!(
        status.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    // A bumped version fails closed.
    let bad_version = root.join("bad-version.json");
    std::fs::write(
        &bad_version,
        text.replace("\"format_version\": 2", "\"format_version\": 99"),
    )
    .expect("mutant writes");
    let status = Command::new(tool())
        .args(["validate"])
        .arg(&bad_version)
        .output()
        .expect("tool runs");
    assert!(!status.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(
        combined.contains("UNSUPPORTED_FORMAT_VERSION"),
        "output: {combined}"
    );

    // A dropped section in the file is an error.
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("json parses");
    value
        .get_mut("settings")
        .expect("settings present")
        .as_object_mut()
        .expect("settings object")
        .insert(
            "library_sync_settings".to_owned(),
            serde_json::json!({"sync_frequency": "6hr"}),
        );
    let dropped = root.join("dropped.json");
    std::fs::write(
        &dropped,
        serde_json::to_string_pretty(&value).expect("json renders"),
    )
    .expect("mutant writes");
    let status = Command::new(tool())
        .args(["validate"])
        .arg(&dropped)
        .output()
        .expect("tool runs");
    assert!(!status.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(combined.contains("DROPPED_SECTION"), "output: {combined}");

    // With the passphrase, the digest check runs first: the clean file
    // passes and the modified one fails it.
    let pass = passphrase_file(&root, "operator-passphrase");
    for (file, ok) in [(&clean, true), (&dropped, false)] {
        let status = Command::new(tool())
            .args(["validate"])
            .arg(file)
            .args(["--passphrase-file"])
            .arg(&pass)
            .output()
            .expect("tool runs");
        let stderr = String::from_utf8_lossy(&status.stderr);
        assert_eq!(status.status.success(), ok, "stderr: {stderr}");
        assert_eq!(
            stderr.contains("CHECKSUM_MISMATCH"),
            !ok,
            "stderr: {stderr}"
        );
    }

    // The optional --v2-root cross-check catches an instance mismatch.
    let other = droppedneedle::tooling::fixture::build_v2_fixture(&root.join("other-v2"))
        .expect("second fixture builds");
    let status = Command::new(tool())
        .args(["validate"])
        .arg(&clean)
        .args(["--v2-root"])
        .arg(&other.root)
        .output()
        .expect("tool runs");
    assert!(!status.status.success());
    assert!(
        String::from_utf8_lossy(&status.stderr).contains("INSTANCE_MISMATCH"),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

#[test]
fn validate_accepts_a_matching_v2_root() {
    let root = scratch_dir("validate-match");
    let (fixture, clean) = repaired_export(&root);
    let status = Command::new(tool())
        .args(["validate"])
        .arg(&clean)
        .args(["--v2-root"])
        .arg(&fixture.root)
        .output()
        .expect("tool runs");
    assert!(
        status.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

#[test]
fn import_and_dry_run_match_through_the_binary() {
    let root = scratch_dir("import");
    let (_fixture, export) = repaired_export(&root);
    let pass = passphrase_file(&root, "operator-passphrase");
    let db = root.join("v3.db");
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).expect("config dir");

    let run = |verb: &str| {
        Command::new(tool())
            .args([verb, "--file"])
            .arg(&export)
            .args(["--db"])
            .arg(&db)
            .args(["--config-dir"])
            .arg(&config_dir)
            .args(["--passphrase-file"])
            .arg(&pass)
            .output()
            .expect("tool runs")
    };
    let dry = run("dry-run");
    assert!(
        dry.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let dry_report: serde_json::Value =
        serde_json::from_slice(&dry.stdout).expect("dry-run prints one JSON object");
    assert_eq!(
        dry_report.get("dry_run"),
        Some(&serde_json::Value::Bool(true))
    );

    let real = run("import");
    assert!(
        real.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&real.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&real.stdout).expect("import prints one JSON object");
    assert_eq!(report.get("dry_run"), Some(&serde_json::Value::Bool(false)));
    assert_eq!(
        report.get("entities"),
        dry_report.get("entities"),
        "dry-run parity"
    );
    assert!(
        config_dir.join("config.json").is_file(),
        "real import writes the v3 config"
    );
}

#[test]
fn import_with_the_wrong_passphrase_writes_nothing() {
    let root = scratch_dir("passphrase");
    let (_fixture, export) = repaired_export(&root);
    let wrong = passphrase_file(&root, "wrong-passphrase");
    let db = root.join("v3.db");
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).expect("config dir");

    let status = Command::new(tool())
        .args(["import", "--file"])
        .arg(&export)
        .args(["--db"])
        .arg(&db)
        .args(["--config-dir"])
        .arg(&config_dir)
        .args(["--passphrase-file"])
        .arg(&wrong)
        .output()
        .expect("tool runs");
    assert!(!status.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("failure still prints one report");
    assert_eq!(
        report.get("exit").and_then(|exit| exit.get("code")),
        Some(&serde_json::json!("ENVELOPE_AUTH_FAILED"))
    );
    assert!(!config_dir.join("config.json").exists(), "zero writes");
}

#[test]
fn import_and_dry_run_read_the_passphrase_from_stdin() {
    let root = scratch_dir("stdin-pass");
    let (_fixture, export) = repaired_export(&root);
    let db = root.join("v3.db");
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).expect("config dir");

    for verb in ["dry-run", "import"] {
        let mut child = Command::new(tool())
            .args([verb, "--file"])
            .arg(&export)
            .args(["--db"])
            .arg(&db)
            .args(["--config-dir"])
            .arg(&config_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tool spawns");
        use std::io::Write as _;
        child
            .stdin
            .as_mut()
            .expect("stdin pipes")
            .write_all(b"operator-passphrase\n")
            .expect("passphrase writes");
        let out = child.wait_with_output().expect("tool runs");
        assert!(
            out.status.success(),
            "{verb} stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("one JSON report on stdout");
        let code = report
            .get("exit")
            .and_then(|exit| exit.get("code"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(
            code == "OK" || code == "OK_WITH_DROPS",
            "{verb} report: {code}"
        );
    }
}

#[test]
fn import_with_an_empty_passphrase_fails_closed() {
    let root = scratch_dir("empty-pass");
    let (_fixture, export) = repaired_export(&root);
    let db = root.join("v3.db");
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).expect("config dir");

    // An empty passphrase file.
    let empty = passphrase_file(&root, "");
    let status = Command::new(tool())
        .args(["import", "--file"])
        .arg(&export)
        .args(["--db"])
        .arg(&db)
        .args(["--config-dir"])
        .arg(&config_dir)
        .args(["--passphrase-file"])
        .arg(&empty)
        .output()
        .expect("tool runs");
    assert!(!status.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("failure still prints one report");
    assert_eq!(
        report.get("exit").and_then(|exit| exit.get("code")),
        Some(&serde_json::json!("ENVELOPE_AUTH_FAILED"))
    );
    assert!(!config_dir.join("config.json").exists(), "zero writes");

    // Closed stdin reads as empty and fails the same way.
    let dry_db = root.join("dry.db");
    let status = Command::new(tool())
        .args(["dry-run", "--file"])
        .arg(&export)
        .args(["--db"])
        .arg(&dry_db)
        .args(["--config-dir"])
        .arg(&config_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("tool runs");
    assert!(!status.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("failure still prints one report");
    assert_eq!(
        report.get("exit").and_then(|exit| exit.get("code")),
        Some(&serde_json::json!("ENVELOPE_AUTH_FAILED"))
    );
}

#[tokio::test]
async fn import_refuses_a_locked_database() {
    let root = scratch_dir("locked");
    let (_fixture, export) = repaired_export(&root);
    let pass = passphrase_file(&root, "operator-passphrase");
    let db = root.join("v3.db");
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).expect("config dir");

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite:{}?mode=rwc", db.display()))
        .await
        .expect("writable pool opens");
    droppedneedle::schema::apply_migrations(&pool)
        .await
        .expect("schema migrates");
    pool.close().await;

    // Another writer holds the database for the duration of the CLI run.
    let held = rusqlite::Connection::open(&db).expect("lock connection opens");
    held.execute_batch("BEGIN IMMEDIATE")
        .expect("write lock taken");
    let status = Command::new(tool())
        .args(["import", "--file"])
        .arg(&export)
        .args(["--db"])
        .arg(&db)
        .args(["--config-dir"])
        .arg(&config_dir)
        .args(["--passphrase-file"])
        .arg(&pass)
        .output()
        .expect("tool runs");
    drop(held);

    assert!(!status.status.success());
    assert!(
        status.stdout.is_empty(),
        "a pre-pipeline refusal prints no report"
    );
    assert!(!String::from_utf8_lossy(&status.stderr).is_empty());
}

/// A running server holds the database lock; the import refuses before it
/// opens the database at all.
#[test]
fn import_refuses_while_the_server_holds_the_database() {
    let root = scratch_dir("server-lock");
    let (_fixture, export) = repaired_export(&root);
    let pass = passphrase_file(&root, "operator-passphrase");
    let db = root.join("v3.db");
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    let _server = droppedneedle::tooling::datalock::DataLock::shared(&db).expect("server lock");

    let status = Command::new(tool())
        .args(["import", "--file"])
        .arg(&export)
        .args(["--db"])
        .arg(&db)
        .args(["--config-dir"])
        .arg(&config_dir)
        .args(["--passphrase-file"])
        .arg(&pass)
        .output()
        .expect("tool runs");
    assert!(!status.status.success());
    assert!(
        String::from_utf8_lossy(&status.stderr).contains("stop the server"),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(!db.exists(), "the database was never opened");
}

/// A migrated merge target plus its repaired export, ready for one seed.
/// The scratch guard comes last; keep it alive for the whole test.
async fn merge_target(
    tag: &str,
) -> (
    droppedneedle::tooling::fixture::V2Fixture,
    PathBuf,
    sqlx::SqlitePool,
    PathBuf,
    crate::common::ScratchDir,
) {
    let root = scratch_dir(tag);
    let (fixture, export) = repaired_export(&root);
    let db_path = root.join("v3.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite:{}?mode=rwc", db_path.display()))
        .await
        .expect("writable pool opens");
    droppedneedle::schema::apply_migrations(&pool)
        .await
        .expect("schema migrates");
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    (fixture, export, pool, config_dir, root)
}

async fn run_merge(
    export: &std::path::Path,
    pool: &sqlx::SqlitePool,
    config_dir: &std::path::Path,
) -> droppedneedle::r#import::ImportReport {
    let crypto =
        droppedneedle::runtime_config::Crypto::load_or_generate(config_dir).expect("v3 key loads");
    droppedneedle::r#import::run_import(droppedneedle::r#import::ImportRequest {
        export_bytes: std::fs::read(export).expect("export reads"),
        passphrase: "operator-passphrase".to_owned(),
        pool: pool.clone(),
        config_path: config_dir.join("config.json"),
        crypto,
        v2_config_path: None,
        attachments_dir: export.parent().map(std::path::Path::to_path_buf),
        cache_dir: None,
        dry_run: false,
        fault_before_commit: false,
        fault_after_commit: false,
        fault_after_sections: None,
    })
    .await
}

/// Same-id users keep the existing row, untouched.
#[tokio::test]
async fn same_id_user_conflict_keeps_existing_row() {
    let (fixture, export, pool, config_dir, _root) = merge_target("merge-user").await;
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, email, role, created_at, username)
         VALUES (?, 'Old Alice', 'old@example.com', 'user',
                 '2026-01-01T00:00:00+00:00', 'alice')",
    )
    .bind(&fixture.alice_id)
    .execute(&pool)
    .await
    .expect("stale alice seeds");

    let report = run_merge(&export, &pool, &config_dir).await;
    assert_eq!(
        report.exit.code,
        droppedneedle::r#import::ExitCode::OkWithDrops
    );
    assert_eq!(
        report
            .entities
            .get("user")
            .expect("user counted")
            .conflict_kept_existing,
        1
    );

    let display: String = sqlx::query_scalar("SELECT display_name FROM auth_users WHERE id = ?")
        .bind(&fixture.alice_id)
        .fetch_one(&pool)
        .await
        .expect("user reads back");
    assert_eq!(display, "Old Alice");
}

/// Follows merge: auto_download OR, followed_at min, updated_at max.
#[tokio::test]
async fn follow_conflict_merges_or_min_max() {
    let (fixture, export, pool, config_dir, _root) = merge_target("merge-follow").await;
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES (?, 'Alice', 'user', '')",
    )
    .bind(&fixture.alice_id)
    .execute(&pool)
    .await
    .expect("alice seeds");
    sqlx::query(
        "INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower,
             artist_name, auto_download, followed_at, updated_at)
         VALUES (?, ?, ?, 'Fixture Artist One', 0, 9999999999.0, 100.0)",
    )
    .bind(&fixture.alice_id)
    .bind(&fixture.artist_one_mbid)
    .bind(fixture.artist_one_mbid.to_lowercase())
    .execute(&pool)
    .await
    .expect("narrow follow seeds");

    let report = run_merge(&export, &pool, &config_dir).await;
    assert_eq!(
        report
            .entities
            .get("follow")
            .expect("follow counted")
            .conflict_kept_existing,
        1
    );

    let merged: (i64, f64, f64) = sqlx::query_as(
        "SELECT auto_download, followed_at, updated_at FROM user_followed_artists
         WHERE user_id = ? AND artist_mbid_lower = ?",
    )
    .bind(&fixture.alice_id)
    .bind(fixture.artist_one_mbid.to_lowercase())
    .fetch_one(&pool)
    .await
    .expect("follow reads back");
    assert_eq!(merged, (1, 1_700_000_000.0, 1_760_000_000.0));
}

/// Approvals keep the most-permissive state with the earliest request.
#[tokio::test]
async fn approval_conflict_keeps_most_permissive() {
    let (fixture, export, pool, config_dir, _root) = merge_target("merge-approval").await;
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES (?, 'Alice', 'user', '')",
    )
    .bind(&fixture.alice_id)
    .execute(&pool)
    .await
    .expect("alice seeds");
    sqlx::query(
        "INSERT INTO auto_download_approvals (user_id, artist_mbid, artist_mbid_lower,
             artist_name, state, requested_at)
         VALUES (?, ?, ?, 'Fixture Artist One', 'approved', 9999999999.0)",
    )
    .bind(&fixture.alice_id)
    .bind(&fixture.artist_one_mbid)
    .bind(fixture.artist_one_mbid.to_lowercase())
    .execute(&pool)
    .await
    .expect("approved approval seeds");

    let report = run_merge(&export, &pool, &config_dir).await;
    assert_eq!(
        report
            .entities
            .get("approval")
            .expect("approval counted")
            .conflict_kept_existing,
        1
    );

    let approval: (String, f64) = sqlx::query_as(
        "SELECT state, requested_at FROM auto_download_approvals
         WHERE user_id = ? AND artist_mbid_lower = ?",
    )
    .bind(&fixture.alice_id)
    .bind(fixture.artist_one_mbid.to_lowercase())
    .fetch_one(&pool)
    .await
    .expect("approval reads back");
    assert_eq!(approval, ("approved".to_owned(), 1_740_000_000.0));
}

#[tokio::test]
async fn restore_round_trips_and_refuses() {
    use droppedneedle::db::{BackupService, CancelFlag};

    let root = scratch_dir("restore");
    let live = root.join("live.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite:{}?mode=rwc", live.display()))
        .await
        .expect("writable pool opens");
    droppedneedle::schema::apply_migrations(&pool)
        .await
        .expect("schema migrates");
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at)
         VALUES ('restore-alice', 'Alice', 'admin', '2026-01-01T00:00:00+00:00')",
    )
    .execute(&pool)
    .await
    .expect("row seeds");
    pool.close().await;

    let backup_dir = root.join("backups");
    let service = BackupService::new(&live, &backup_dir);
    let taken = service
        .backup(None, &CancelFlag::never())
        .await
        .expect("backup runs");

    // The binary restores into an empty directory.
    let target = root.join("restored");
    let status = Command::new(tool())
        .args(["restore", "--backup"])
        .arg(&taken.path)
        .args(["--target-dir"])
        .arg(&target)
        .output()
        .expect("tool runs");
    assert!(
        status.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let summary: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("restore prints one JSON object");
    assert!(summary.get("restored").is_some());
    let check = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite:{}?mode=ro",
            target.join("library.db").display()
        ))
        .await
        .expect("restored pool opens");
    let name: String = sqlx::query_scalar("SELECT display_name FROM auth_users WHERE id = ?")
        .bind("restore-alice")
        .fetch_one(&check)
        .await
        .expect("restored row reads");
    assert_eq!(name, "Alice");
    check.close().await;

    // Occupied targets refuse.
    let occupied = root.join("occupied");
    std::fs::create_dir_all(&occupied).expect("occupied dir");
    std::fs::write(occupied.join("something.db"), b"junk").expect("junk writes");
    let refused = droppedneedle::tooling::restore::restore_backup(&taken.path, &occupied, false);
    assert!(
        matches!(
            refused,
            Err(droppedneedle::tooling::restore::RestoreError::TargetNotEmpty(_))
        ),
        "occupied target refuses: {refused:?}"
    );

    // A backup newer than the binary refuses without --allow-downgrade.
    let future = root.join("future.db");
    std::fs::copy(&taken.path, &future).expect("backup copies");
    let stamp = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite:{}?mode=rwc", future.display()))
        .await
        .expect("future pool opens");
    sqlx::query("PRAGMA user_version = 9999")
        .execute(&stamp)
        .await
        .expect("stamp writes");
    stamp.close().await;
    let too_new =
        droppedneedle::tooling::restore::restore_backup(&future, &root.join("new1"), false);
    assert!(
        matches!(
            too_new,
            Err(
                droppedneedle::tooling::restore::RestoreError::BackupTooNew { .. }
                    | droppedneedle::tooling::restore::RestoreError::Refused(_)
            )
        ),
        "newer backup refuses: {too_new:?}"
    );
    let allowed =
        droppedneedle::tooling::restore::restore_backup(&future, &root.join("new2"), true)
            .expect("allow-downgrade restores");
    assert_eq!(allowed.user_version, 9999);
}

/// The covers-debug route serves with the flag, 404s without it, and never
/// appears in the OpenAPI document.
#[tokio::test]
async fn covers_debug_is_dev_only() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    let mut flagged = common::hooked_state();
    flagged.config = flagged.config.clone().with_tooling_routes();
    let app = droppedneedle::create_app(flagged);
    let mbid = uuid::Uuid::new_v4().to_string();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/__tooling__/covers/debug/artist/{mbid}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("route serves");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body reads");
    let debug: serde_json::Value = serde_json::from_slice(&body).expect("debug parses");
    assert_eq!(
        debug.get("is_valid_mbid"),
        Some(&serde_json::Value::Bool(true))
    );
    assert!(debug.get("recommendation").is_some());
    assert_eq!(
        debug
            .get("sizes")
            .and_then(|sizes| sizes.as_array())
            .map(Vec::len),
        Some(2)
    );

    let bad = droppedneedle::create_app({
        let mut flagged = common::hooked_state();
        flagged.config = flagged.config.clone().with_tooling_routes();
        flagged
    });
    let response = bad
        .oneshot(
            Request::builder()
                .uri("/__tooling__/covers/debug/artist/nope")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("route serves");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body reads");
    let debug: serde_json::Value = serde_json::from_slice(&body).expect("debug parses");
    assert_eq!(
        debug.get("is_valid_mbid"),
        Some(&serde_json::Value::Bool(false))
    );

    let prod_like = droppedneedle::create_app(common::prod_like_state());
    let response = prod_like
        .oneshot(
            Request::builder()
                .uri(format!("/__tooling__/covers/debug/artist/{mbid}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("prod app serves");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    use utoipa::OpenApi as _;
    let openapi =
        serde_json::to_string(&droppedneedle::docs::ApiDoc::openapi()).expect("openapi renders");
    assert!(
        !openapi.contains("__tooling__"),
        "tooling stays out of the API contract"
    );
}
