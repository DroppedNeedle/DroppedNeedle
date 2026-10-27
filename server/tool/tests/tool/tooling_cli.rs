//! Tooling CLI tests: every `droppedneedle-tool` subcommand runs
//! as the real binary, plus the in-place upgrade's failure and recovery
//! paths, the conflict-merge decisions, the offline restore guards, and
//! the dev-only covers-debug gate.
//!
//! Nothing here touches the network; every database is a scratch file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use droppedneedle::bootstrap::data_dirs;
use droppedneedle_tool::tooling::upgrade;

use crate::common;

/// Scratch directory, removed when the test ends.
/// The settings stored in the database at `db`, key by key; empty when it
/// holds none (or does not exist).
fn settings_in(db: &Path) -> serde_json::Map<String, serde_json::Value> {
    let Ok(connection) =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return serde_json::Map::new();
    };
    let Ok(mut statement) = connection.prepare("SELECT key, value_json FROM config_sections")
    else {
        return serde_json::Map::new();
    };
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("settings rows read")
        .map(|row| {
            let (key, text) = row.expect("settings row reads");
            (key, serde_json::from_str(&text).expect("a row is JSON"))
        })
        .collect()
}

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
) -> (droppedneedle_tool::tooling::fixture::V2Fixture, PathBuf) {
    let fixture = droppedneedle_tool::tooling::fixture::build_v2_fixture(&root.join("v2"))
        .expect("fixture builds");
    droppedneedle_tool::tooling::fixture::delete_orphan_rows(
        &fixture.root.join("cache").join("library.db"),
    )
    .expect("repair runs");
    let request = droppedneedle_tool::export::ExportRequest {
        v2: droppedneedle_tool::export::v2dir::V2Dirs::under_root(&fixture.root),
        db_path: None,
        passphrase: droppedneedle_kernel::runtime_config::Secret::new("operator-passphrase"),
        exported_at: None,
        v2_commit: None,
    };
    let path = root.join("export.json");
    droppedneedle_tool::export::export_v2_to_file(&request, &path).expect("export writes");
    (fixture, path)
}

#[test]
fn export_writes_then_refuses_a_keyless_instance() {
    let root = scratch_dir("export");
    let fixture = droppedneedle_tool::tooling::fixture::build_v2_fixture(&root.join("v2"))
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
    let other = droppedneedle_tool::tooling::fixture::build_v2_fixture(&root.join("other-v2"))
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
        !settings_in(&db).is_empty(),
        "real import writes the v3 settings"
    );
    assert!(!config_dir.join("config.json").exists());
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
    assert!(settings_in(&db).is_empty(), "zero writes");
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
    assert!(settings_in(&db).is_empty(), "zero writes");

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
    droppedneedle_kernel::schema::apply_migrations(&pool)
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
    let _server = droppedneedle_kernel::db::datalock::DataLock::shared(&db).expect("server lock");

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

/// Run the tool against the data directory under `root` the way a
/// container does: no path flags, only the environment the server reads.
fn tool_in(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(tool())
        .args(args)
        .env_clear()
        .env("ROOT_APP_DIR", root)
        .output()
        .expect("tool runs")
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A v2 data directory without orphan rows. Upgrading it never changes
/// the v2 database, so its files can be compared byte for byte.
fn clean_v2(root: &Path) {
    droppedneedle_tool::tooling::fixture::build_v2_fixture(root).expect("fixture builds");
    droppedneedle_tool::tooling::fixture::delete_orphan_rows(
        &root.join("cache").join("library.db"),
    )
    .expect("repair runs");
}

/// Folders (`None`) and files (their bytes) by relative path.
type Tree = BTreeMap<PathBuf, Option<Vec<u8>>>;

/// Everything under `dir`. The data lock files are left out: taking a
/// lock creates its file and nothing ever removes it.
fn tree(dir: &Path) -> Tree {
    fn walk(top: &Path, dir: &Path, found: &mut Tree) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("entry reads").path();
            if path
                .file_name()
                .is_some_and(|name| name == "library.db.lock" || name == "library.db.server.lock")
            {
                continue;
            }
            let relative = path.strip_prefix(top).expect("under top").to_owned();
            if path.is_dir() {
                found.insert(relative, None);
                walk(top, &path, found);
            } else {
                found.insert(relative, Some(std::fs::read(&path).expect("file reads")));
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(dir, dir, &mut found);
    found
}

/// The config and cache folders of the data directory under `root`.
fn data_trees(root: &Path) -> (Tree, Tree) {
    (tree(&root.join("config")), tree(&root.join("cache")))
}

/// The same two folders as the upgrade keeps them, in its backup.
fn backup_trees(root: &Path) -> (Tree, Tree) {
    (
        tree(&root.join("config").join("v2-backup")),
        tree(&root.join("cache").join("v2-backup")),
    )
}

fn schema_stamp(db: &Path) -> i64 {
    rusqlite::Connection::open(db)
        .expect("database opens")
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("stamp reads")
}

/// True when the data directory under `root` holds a migrated v3 database
/// and none of the upgrade's temporary files.
fn is_v3(root: &Path) -> bool {
    let cache = root.join("cache");
    schema_stamp(&cache.join("library.db")) > 0
        && root.join("config").join("data_enc.key").is_file()
        && !cache.join(".upgrade-work").exists()
        && !cache.join(".upgrade-in-progress").exists()
}

fn upgrade_dirs(root: &Path) -> data_dirs::DataDirs {
    data_dirs::DataDirs::from_config(&droppedneedle_kernel::config::AppConfig::with_root(0, root))
}

/// The upgrade through the library, for the failure points the tool has
/// no switch for.
async fn upgrade_with_fault(
    root: &Path,
    fault: upgrade::Fault,
) -> Result<upgrade::Upgraded, upgrade::UpgradeError> {
    let request = upgrade::UpgradeRequest {
        dirs: upgrade_dirs(root),
        keep_backup: true,
        v2_commit: None,
        fault: Some(fault),
    };
    upgrade::run(&request, &mut |_| {}).await
}

/// One command turns a v2 data directory into a v3 one where it stands:
/// the v2 files end up in the two backup folders, the import matches an
/// export and import done by hand, and nothing temporary is left.
#[test]
fn upgrade_turns_a_v2_directory_into_v3_in_place() {
    let scratch = scratch_dir("upgrade");
    let root = scratch.join("data");
    let fixture =
        droppedneedle_tool::tooling::fixture::build_v2_fixture(&root).expect("fixture builds");

    let run = tool_in(&root, &["upgrade"]);
    assert!(run.status.success(), "stderr: {}", stderr_of(&run));
    assert!(is_v3(&root));
    let settings = settings_in(&root.join("cache").join("library.db"));
    assert_eq!(settings["instance_id"], fixture.instance_id.as_str());
    assert!(
        settings.contains_key("key_check"),
        "the new key's check is stored"
    );
    assert!(
        !root.join("config").join("config.json").exists(),
        "v3 keeps its settings in the database"
    );

    // The v2 files are kept, each in its own folder's backup.
    let config_backup = root.join("config").join("v2-backup");
    let cache_backup = root.join("cache").join("v2-backup");
    assert!(config_backup.join(".env").is_file());
    assert!(config_backup.join("config.json").is_file());
    assert_eq!(schema_stamp(&cache_backup.join("library.db")), 0);

    // The fixture's follow and approval of a deleted user are removed from
    // the v2 database, and the summary says so.
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("Removed 2 follows and approvals"),
        "stdout: {stdout}"
    );
    let orphans: i64 = rusqlite::Connection::open(cache_backup.join("library.db"))
        .expect("v2 database opens")
        .query_row(
            "SELECT (SELECT count(*) FROM user_followed_artists
                     WHERE user_id NOT IN (SELECT id FROM auth_users))
                  + (SELECT count(*) FROM auto_download_approvals
                     WHERE user_id NOT IN (SELECT id FROM auth_users))",
            [],
            |row| row.get(0),
        )
        .expect("orphans count");
    assert_eq!(orphans, 0);

    // The same counts as the export and import commands give by hand.
    let manual_root = scratch.join("manual");
    let (manual_fixture, export) = repaired_export(&manual_root);
    let pass = passphrase_file(&manual_root, "operator-passphrase");
    let manual = Command::new(tool())
        .args(["import", "--file"])
        .arg(&export)
        .args(["--db"])
        .arg(manual_root.join("v3").join("cache").join("library.db"))
        .args(["--config-dir"])
        .arg(manual_root.join("v3").join("config"))
        .args(["--passphrase-file"])
        .arg(&pass)
        .args(["--v2-config"])
        .arg(manual_fixture.root.join("config").join("config.json"))
        .output()
        .expect("tool runs");
    assert!(manual.status.success(), "stderr: {}", stderr_of(&manual));
    let manual: serde_json::Value =
        serde_json::from_slice(&manual.stdout).expect("import prints one JSON object");
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("cache").join("upgrade-report.json")).expect("report saved"),
    )
    .expect("report parses");
    assert_eq!(report["entities"], manual["entities"]);
    assert_eq!(report["entities"]["user"]["imported"], 2);

    // A second run finds v3 and changes nothing.
    let before = data_trees(&root);
    let again = tool_in(&root, &["upgrade"]);
    assert!(again.status.success(), "stderr: {}", stderr_of(&again));
    assert!(String::from_utf8_lossy(&again.stdout).contains("Nothing to do"));
    assert_eq!(data_trees(&root), before);

    // The backup goes once the user asks, and only the backup.
    let removed = tool_in(&root, &["remove-v2-backup"]);
    assert!(removed.status.success(), "stderr: {}", stderr_of(&removed));
    assert!(!config_backup.exists() && !cache_backup.exists());
    assert!(is_v3(&root));
}

/// Every text value in every table of the database at `db` that is, or
/// holds in JSON, `v3:` ciphertext.
fn sealed_values(db: &Path) -> Vec<String> {
    let connection = rusqlite::Connection::open(db).expect("database opens");
    let tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .expect("tables list")
        .query_map([], |row| row.get(0))
        .expect("tables read")
        .collect::<Result<_, _>>()
        .expect("table names read");
    let mut found = Vec::new();
    for table in tables {
        let columns: Vec<String> = connection
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .expect("columns list")
            .query_map([], |row| row.get(0))
            .expect("columns read")
            .collect::<Result<_, _>>()
            .expect("column names read");
        for column in columns {
            let hits: i64 = connection
                .query_row(
                    &format!(
                        "SELECT count(*) FROM \"{table}\" WHERE CAST(\"{column}\" AS TEXT) \
                         GLOB 'v3:*' OR CAST(\"{column}\" AS TEXT) GLOB '*\"v3:*'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .expect("column scans");
            if hits > 0 {
                found.push(format!("{table}.{column}"));
            }
        }
    }
    found
}

/// A key that is lost for good: `reset-key` refuses while the key file
/// belongs to the database, and an import refuses a key that does not.
/// With the key lost, it clears every sealed value, keeps the replaced
/// key, prints no secret, and the server starts on the result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_key_clears_every_secret_of_a_lost_key() {
    let scratch = scratch_dir("reset-key");
    let root = scratch.join("data");
    droppedneedle_tool::tooling::fixture::build_v2_fixture(&root).expect("fixture builds");
    let upgraded = tool_in(&root, &["upgrade"]);
    assert!(
        upgraded.status.success(),
        "stderr: {}",
        stderr_of(&upgraded)
    );
    let db = root.join("cache").join("library.db");
    let config = root.join("config");
    let key = config.join("data_enc.key");
    assert!(!sealed_values(&db).is_empty());

    let before = (tree(&config), settings_in(&db), sealed_values(&db));
    let refused = tool_in(&root, &["reset-key", "--yes"]);
    assert_eq!(refused.status.code(), Some(1), "{}", stderr_of(&refused));
    assert!(stderr_of(&refused).contains("belongs to this database"));
    assert_eq!(
        (tree(&config), settings_in(&db), sealed_values(&db)),
        before
    );

    std::fs::remove_file(&key).expect("key removes");
    droppedneedle_kernel::runtime_config::Crypto::mint_file(&key).expect("another key");
    let wrong = std::fs::read(&key).expect("key reads");
    let (_fixture, export) = repaired_export(&scratch.join("export"));
    let pass = passphrase_file(&scratch, "operator-passphrase");
    let import = Command::new(tool())
        .args(["import", "--file"])
        .arg(&export)
        .args(["--db"])
        .arg(&db)
        .args(["--config-dir"])
        .arg(&config)
        .args(["--passphrase-file"])
        .arg(&pass)
        .output()
        .expect("tool runs");
    assert_eq!(import.status.code(), Some(1), "{}", stderr_of(&import));
    assert!(
        stderr_of(&import).contains("does not belong"),
        "{}",
        stderr_of(&import)
    );
    assert_eq!(settings_in(&db), before.1);

    let reset = tool_in(&root, &["reset-key", "--yes"]);
    assert!(reset.status.success(), "stderr: {}", stderr_of(&reset));
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&reset.stdout),
        stderr_of(&reset)
    );
    assert!(!output.contains("plaintext"), "{output}");
    assert!(output.contains("app passwords revoked"), "{output}");
    assert_eq!(sealed_values(&db), Vec::<String>::new());
    assert_eq!(
        std::fs::read(config.join("data_enc.key.old")).expect("old key kept"),
        wrong
    );
    assert_ne!(std::fs::read(&key).expect("new key"), wrong);
    assert!(!config.join("data_enc.key.new").exists());

    let built = droppedneedle::bootstrap::build_with(
        droppedneedle_kernel::config::AppConfig::with_root(0, &root),
        common::closed_outbound(),
    )
    .await
    .expect("the server starts after the reset");
    drop(built.state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener binds");
    let stopped =
        droppedneedle::bootstrap::serve(listener, built.router, built.background, async {}).await;
    assert!(stopped.outcome.is_ok(), "{:?}", stopped.outcome);
}

/// A locked-out admin who only ever used single sign-on gets a recovery
/// code from the host, with the server stopped. The web reset takes the
/// code once and gives the account its first password; the admin route
/// still refuses such an account.
#[tokio::test]
async fn recovery_code_from_the_host_gives_a_locked_out_admin_a_password() {
    use crate::common::auth::AuthRig;
    use droppedneedle_auth::prod::{AuthDb, Passwords, Sessions, Users};
    use droppedneedle_auth::session::login::{
        LoginContext, LoginRequest, LoginService, TransportParam,
    };
    use droppedneedle_auth::session::store::{SessionKind, SessionRecord};
    use droppedneedle_auth::session::tokens;
    use droppedneedle_auth::users::models::PasswordReset;
    use droppedneedle_auth::users::services;

    const NEW_PASSWORD: &str = "a brand new password 99";
    let scratch = scratch_dir("recovery-code");
    let root = scratch.join("data");
    droppedneedle_tool::tooling::fixture::build_v2_fixture(&root).expect("fixture builds");
    let upgraded = tool_in(&root, &["upgrade"]);
    assert!(
        upgraded.status.success(),
        "stderr: {}",
        stderr_of(&upgraded)
    );
    let db_path = root.join("cache").join("library.db");
    let count = |sql: &str| -> i64 {
        rusqlite::Connection::open(&db_path)
            .expect("database opens")
            .query_row(sql, [], |row| row.get(0))
            .expect("count reads")
    };
    const CODES: &str = "SELECT count(*) FROM auth_password_recovery_codes";
    const ALICE_CODES: &str = "SELECT count(*) FROM auth_password_recovery_codes WHERE user_id = \
        (SELECT id FROM auth_users WHERE username = 'alice')";
    const ALICE_SIGN_INS: &str = "SELECT group_concat(provider, ',') FROM (SELECT provider \
        FROM auth_providers WHERE user_id = (SELECT id FROM auth_users WHERE username = 'alice') \
        ORDER BY provider)";
    let alice_sign_ins = || -> String {
        rusqlite::Connection::open(&db_path)
            .expect("database opens")
            .query_row(ALICE_SIGN_INS, [], |row| row.get(0))
            .expect("providers read")
    };
    // Alice, the admin, signs in through single sign-on only.
    rusqlite::Connection::open(&db_path)
        .expect("database opens")
        .execute_batch(
            "DELETE FROM auth_providers WHERE provider = 'local' AND provider_uid = 'alice';
             INSERT INTO auth_providers (id, user_id, provider, provider_uid, created_at)
             SELECT 'prov-alice-oidc', id, 'oidc', 'sub-alice', '2024-01-01T00:00:00+00:00'
             FROM auth_users WHERE username = 'alice';",
        )
        .expect("alice is single sign-on only");
    assert_eq!(alice_sign_ins(), "oidc");
    let before = count(CODES);

    let refused = |args: &[&str], says: &str| {
        let run = tool_in(&root, args);
        assert_eq!(run.status.code(), Some(1), "stderr: {}", stderr_of(&run));
        assert!(run.stdout.is_empty(), "a refusal prints no code");
        assert!(
            stderr_of(&run).contains(says),
            "stderr: {}",
            stderr_of(&run)
        );
    };
    {
        let _server =
            droppedneedle_kernel::db::datalock::DataLock::shared(&db_path).expect("server lock");
        refused(&["recovery-code", "alice"], "Stop the server");
        refused(&["recovery-code", "--list-admins"], "Stop the server");
    }
    // A dash-led argument is a flag, never a username, and help answers
    // even while the server holds the data directory.
    refused(&["recovery-code", "-x"], "unknown flag");
    {
        let _server =
            droppedneedle_kernel::db::datalock::DataLock::shared(&db_path).expect("server lock");
        let help = tool_in(&root, &["recovery-code", "-h"]);
        assert!(help.status.success(), "stderr: {}", stderr_of(&help));
        assert!(String::from_utf8_lossy(&help.stdout).contains("--list-admins"));
    }
    refused(&["recovery-code", "nobody"], "No user named");
    refused(&["recovery-code", "--", "--nobody"], "No user named");
    assert_eq!(count(CODES), before, "a refusal writes nothing");

    // Only the admin is listed: bob is trusted.
    let listed = tool_in(&root, &["recovery-code", "--list-admins"]);
    assert!(listed.status.success(), "stderr: {}", stderr_of(&listed));
    assert_eq!(String::from_utf8_lossy(&listed.stdout), "alice\n");

    // An account with a password gets a plain code.
    let bob = tool_in(&root, &["recovery-code", "bob"]);
    assert!(bob.status.success(), "stderr: {}", stderr_of(&bob));
    assert!(!stderr_of(&bob).contains("no password yet"));

    let run = tool_in(&root, &["recovery-code", "Alice"]);
    assert!(run.status.success(), "stderr: {}", stderr_of(&run));
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let code = stdout.trim_end_matches('\n');
    let groups: Vec<&str> = code.split('-').collect();
    assert!(
        groups.len() == 5
            && groups
                .iter()
                .all(|group| group.len() == 4 && group.bytes().all(|b| b.is_ascii_alphanumeric())),
        "stdout is the code and nothing else: {stdout:?}"
    );
    let stderr = stderr_of(&run);
    assert!(!stderr.contains(code), "the code prints once");
    assert!(stderr.contains("no password yet"), "stderr: {stderr}");
    let stored: i64 = count(&format!(
        "SELECT count(*) FROM auth_password_recovery_codes WHERE code_hash LIKE '%{}%'",
        code.replace('-', "")
    ));
    assert_eq!(stored, 0, "only a hash is kept");
    assert_eq!(count(ALICE_CODES), 1);

    // The services the web routes call, over the same database.
    let runtime =
        droppedneedle_kernel::db::open_runtime(&droppedneedle_kernel::db::DbConfig::new(&db_path))
            .await
            .expect("database opens");
    let auth_db = AuthDb::new(
        runtime.db(),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    // The rig's settings live apart from the imported ones, which are
    // sealed with the server's own key.
    let rig_settings = crate::common::ScratchDir::new("cli-rig-settings");
    let platform = crate::common::TestPlatform::on(runtime.db(), &rig_settings)
        .settings(crate::common::config_store(&rig_settings))
        .build()
        .await;
    let rig = AuthRig::on(&platform);
    let deps = rig.deps.clone();
    let alice = deps
        .users
        .get_by_username("alice")
        .await
        .expect("store reads")
        .expect("alice exists");

    // An admin in the web UI cannot do what the host command just did, and
    // a refused attempt leaves the host's code alone.
    assert!(
        services::admin_mint_recovery_code(
            &deps,
            &common::auth::test_admin("host-admin"),
            &alice.id
        )
        .await
        .is_err(),
        "the admin route refuses an account with no password"
    );

    let sessions = Sessions::new(
        &auth_db,
        std::sync::Arc::new(droppedneedle_kernel::ids::UuidGenerator),
    );
    let login = LoginService::new(sessions.clone(), Passwords::new(), Users::new(&auth_db));
    let sign_in = || {
        login.login(
            LoginRequest {
                username: "alice".to_owned(),
                password: NEW_PASSWORD.to_owned(),
                transport: TransportParam::Bearer,
            },
            LoginContext {
                user_agent: None,
                now_unix: droppedneedle_kernel::clock::Clock::now_unix(
                    &droppedneedle_kernel::clock::SystemClock,
                ),
            },
        )
    };
    assert!(sign_in().await.is_err(), "no password before the reset");
    let now =
        droppedneedle_kernel::clock::Clock::now_unix(&droppedneedle_kernel::clock::SystemClock);
    sessions
        .seed(SessionRecord {
            id: "sess-alice".to_owned(),
            user_id: alice.id.clone(),
            token_hash: tokens::hash_token("a session from before the reset"),
            kind: SessionKind::Standard,
            label: None,
            issued_at: now,
            expires_at: tokens::expires_at(now),
            last_seen_at: now,
            revoked: false,
            user_agent: None,
        })
        .await
        .expect("session inserts");
    let reset = PasswordReset {
        username: "alice".to_owned(),
        recovery_code: code.to_owned(),
        new_password: NEW_PASSWORD.to_owned(),
    };
    services::reset_password(&deps, &reset)
        .await
        .expect("the code sets the first password");
    assert!(
        services::reset_password(&deps, &reset).await.is_err(),
        "a used code is refused"
    );
    // Read before the sign-in below, which sweeps revoked sessions.
    let revoked: i64 =
        sqlx::query_scalar("SELECT revoked FROM auth_tokens WHERE id = 'sess-alice'")
            .fetch_one(runtime.db().sqlx())
            .await
            .expect("session reads");
    assert_eq!(revoked, 1, "the reset ends the sessions from before it");
    assert_eq!(sign_in().await.expect("sign-in works").user_id, alice.id);
    runtime.shutdown().await;
    assert_eq!(count(ALICE_CODES), 0, "the code is gone once used");
}

/// A run that cannot finish leaves the v2 data directory as it found it,
/// byte for byte, whether it stops before or after the files moved.
#[tokio::test]
async fn a_failed_upgrade_leaves_the_v2_directory_as_it_was() {
    let scratch = scratch_dir("upgrade-fails");
    let root = scratch.join("data");
    clean_v2(&root);
    let key_file = root.join("config").join(".env");
    let key = std::fs::read(&key_file).expect("key reads");
    let original = data_trees(&root);

    // A running server holds the data lock: refused.
    {
        let _server = droppedneedle_kernel::db::datalock::DataLock::shared(
            &root.join("cache").join("library.db"),
        )
        .expect("server lock");
        let run = tool_in(&root, &["upgrade"]);
        assert!(!run.status.success());
        assert!(
            stderr_of(&run).contains("Stop the server"),
            "stderr: {}",
            stderr_of(&run)
        );
        assert_eq!(data_trees(&root), original);
    }

    // No key file: refused before anything moves.
    std::fs::remove_file(&key_file).expect("key removed");
    let keyless = data_trees(&root);
    let run = tool_in(&root, &["upgrade"]);
    assert!(!run.status.success());
    assert!(
        stderr_of(&run).contains("V2_KEY_NOT_FOUND"),
        "stderr: {}",
        stderr_of(&run)
    );
    assert_eq!(data_trees(&root), keyless);

    // A key that does not open the stored secrets: the export fails after
    // the files moved aside, and they are moved back.
    let other_key =
        droppedneedle_tool::export::fernet::FernetKey::generate().expect("key generates");
    std::fs::write(
        &key_file,
        format!("DATA_ENC_KEY={}\n", other_key.to_base64()),
    )
    .expect("key writes");
    let mismatched = data_trees(&root);
    let run = tool_in(&root, &["upgrade"]);
    assert!(!run.status.success());
    assert!(
        stderr_of(&run).contains("V2_KEY_MISMATCH"),
        "stderr: {}",
        stderr_of(&run)
    );
    assert_eq!(data_trees(&root), mismatched);

    // The import fails just before it commits: the v3 files it had created
    // are deleted and the v2 files are moved back.
    std::fs::write(&key_file, key).expect("key restored");
    let failed = upgrade_with_fault(&root, upgrade::Fault::ImportFails).await;
    assert!(
        matches!(
            failed,
            Err(upgrade::UpgradeError::ImportFailed { dry_run: false, .. })
        ),
        "outcome: {failed:?}"
    );
    assert_eq!(data_trees(&root), original);
}

/// A killed run leaves a marker. The server refuses to start on it, and
/// the next run puts the v2 files back before it does anything else.
#[tokio::test]
async fn an_interrupted_upgrade_is_rolled_back_first() {
    let scratch = scratch_dir("upgrade-interrupted");
    let root = scratch.join("data");
    clean_v2(&root);
    // A server that refused to start on this folder left its lock file.
    let server_lock = root.join("cache").join("library.db.server.lock");
    std::fs::write(&server_lock, b"").expect("server lock writes");
    let original = data_trees(&root);
    let dirs = upgrade_dirs(&root);

    // Killed while the files were moving aside: some are in the backup,
    // the rest still in place. Going back deletes none of them.
    let killed = upgrade_with_fault(&root, upgrade::Fault::DiesWhileMoving).await;
    assert!(
        matches!(killed, Err(upgrade::UpgradeError::InjectedCrash)),
        "outcome: {killed:?}"
    );
    assert_eq!(
        data_dirs::check_bootable(&dirs),
        Err(data_dirs::BootRefusal::Interrupted)
    );
    let undone = tool_in(&root, &["upgrade", "--undo", "--yes"]);
    assert!(undone.status.success(), "stderr: {}", stderr_of(&undone));
    assert_eq!(data_trees(&root), original);

    // The import fails and the run is killed while it puts the files back:
    // the v3 files are gone, some v2 files are in place again and the rest
    // still in the backup. The next run finishes the job and deletes none.
    let killed = upgrade_with_fault(&root, upgrade::Fault::DiesWhileRollingBack).await;
    assert!(
        matches!(killed, Err(upgrade::UpgradeError::InjectedCrash)),
        "outcome: {killed:?}"
    );
    assert!(root.join("config").join(".env").is_file());
    assert!(!root.join("cache").join("library.db").exists());
    assert_eq!(
        data_dirs::check_bootable(&dirs),
        Err(data_dirs::BootRefusal::Interrupted)
    );
    // The next server boot writes its lock file again before the rollback
    // goes on.
    std::fs::write(&server_lock, b"").expect("server lock writes");
    let undone = tool_in(&root, &["upgrade", "--undo", "--yes"]);
    assert!(undone.status.success(), "stderr: {}", stderr_of(&undone));
    assert_eq!(data_trees(&root), original);

    // Killed right after the import: the next run deletes the v3 files,
    // puts v2 back, and upgrades again from the start.
    let killed = upgrade_with_fault(&root, upgrade::Fault::DiesAfterImport).await;
    assert!(
        matches!(killed, Err(upgrade::UpgradeError::InjectedCrash)),
        "outcome: {killed:?}"
    );
    assert_eq!(
        data_dirs::check_bootable(&dirs),
        Err(data_dirs::BootRefusal::Interrupted)
    );
    let run = tool_in(&root, &["upgrade"]);
    assert!(run.status.success(), "stderr: {}", stderr_of(&run));
    assert!(
        stderr_of(&run).contains("An earlier upgrade did not finish"),
        "stderr: {}",
        stderr_of(&run)
    );
    assert!(is_v3(&root));
    assert_eq!(data_dirs::check_bootable(&dirs), Ok(()));
    assert_eq!(backup_trees(&root), original);
}

/// `--undo --yes` deletes the v3 data and puts the v2 files back as they
/// were. `--no-backup` keeps no v2 files, so there is nothing to undo.
#[test]
fn undo_returns_the_v2_files_and_no_backup_keeps_none() {
    let scratch = scratch_dir("upgrade-undo");
    let root = scratch.join("data");
    clean_v2(&root);
    let original = data_trees(&root);

    let run = tool_in(&root, &["upgrade"]);
    assert!(run.status.success(), "stderr: {}", stderr_of(&run));
    assert_eq!(backup_trees(&root), original);
    let upgraded = data_trees(&root);

    // Undo deletes the v3 data, so it asks for --yes first.
    let unconfirmed = tool_in(&root, &["upgrade", "--undo"]);
    assert!(!unconfirmed.status.success());
    assert!(
        stderr_of(&unconfirmed).contains("--yes"),
        "stderr: {}",
        stderr_of(&unconfirmed)
    );
    assert_eq!(data_trees(&root), upgraded);

    // A running server holds the data lock: refused.
    {
        let _server = droppedneedle_kernel::db::datalock::DataLock::shared(
            &root.join("cache").join("library.db"),
        )
        .expect("server lock");
        let refused = tool_in(&root, &["upgrade", "--undo", "--yes"]);
        assert!(!refused.status.success());
        assert!(
            stderr_of(&refused).contains("Stop the server"),
            "stderr: {}",
            stderr_of(&refused)
        );
        assert_eq!(data_trees(&root), upgraded);
    }

    // A link left in the cache folder is deleted with the v3 data. What it
    // points at, outside the two folders, is not.
    let outside = scratch.join("outside");
    std::fs::create_dir_all(&outside).expect("outside folder");
    std::fs::write(outside.join("keep.txt"), "mine").expect("outside file");
    let link = root.join("cache").join("link");
    std::os::unix::fs::symlink(&outside, &link).expect("link");

    let undone = tool_in(&root, &["upgrade", "--undo", "--yes"]);
    assert!(undone.status.success(), "stderr: {}", stderr_of(&undone));
    assert_eq!(data_trees(&root), original);
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert_eq!(
        std::fs::read(outside.join("keep.txt")).expect("target survives"),
        b"mine"
    );

    // Back on v2 there is nothing to undo, backup or not.
    let already_v2 = tool_in(&root, &["upgrade", "--undo", "--yes"]);
    assert!(!already_v2.status.success());
    assert!(
        stderr_of(&already_v2).contains("already holds v2 data"),
        "stderr: {}",
        stderr_of(&already_v2)
    );
    assert_eq!(data_trees(&root), original);

    let run = tool_in(&root, &["upgrade", "--no-backup"]);
    assert!(run.status.success(), "stderr: {}", stderr_of(&run));
    assert!(is_v3(&root));
    assert!(!root.join("config").join("v2-backup").exists());
    assert!(!root.join("cache").join("v2-backup").exists());

    let nothing_to_undo = tool_in(&root, &["upgrade", "--undo", "--yes"]);
    assert!(!nothing_to_undo.status.success());
    assert!(
        stderr_of(&nothing_to_undo).contains("no complete v2 backup"),
        "stderr: {}",
        stderr_of(&nothing_to_undo)
    );
    assert!(is_v3(&root));
}

/// A migrated merge target plus its repaired export, ready for one seed.
/// The scratch guard comes last; keep it alive for the whole test.
async fn merge_target(
    tag: &str,
) -> (
    droppedneedle_tool::tooling::fixture::V2Fixture,
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
    droppedneedle_kernel::schema::apply_migrations(&pool)
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
) -> droppedneedle_tool::r#import::ImportReport {
    let (crypto, _) = droppedneedle_kernel::runtime_config::Crypto::load_or_generate(config_dir)
        .expect("v3 key loads");
    droppedneedle_tool::r#import::run_import(droppedneedle_tool::r#import::ImportRequest {
        export_bytes: std::fs::read(export).expect("export reads"),
        passphrase: "operator-passphrase".to_owned(),
        pool: pool.clone(),
        legacy: droppedneedle_kernel::runtime_config::LegacyConfig::at(config_dir, false),
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
        droppedneedle_tool::r#import::ExitCode::OkWithDrops
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
         VALUES (?, ?, ?, 'Fixture Artist One', 'approved', 9999999999000)",
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

    let approval: (String, i64) = sqlx::query_as(
        "SELECT state, requested_at FROM auto_download_approvals
         WHERE user_id = ? AND artist_mbid_lower = ?",
    )
    .bind(&fixture.alice_id)
    .bind(fixture.artist_one_mbid.to_lowercase())
    .fetch_one(&pool)
    .await
    .expect("approval reads back");
    assert_eq!(approval, ("approved".to_owned(), 1_740_000_000_000));
}

#[tokio::test]
async fn restore_round_trips_and_refuses() {
    use droppedneedle_kernel::db::{BackupService, CancelFlag};

    let root = scratch_dir("restore");
    let live = root.join("live.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite:{}?mode=rwc", live.display()))
        .await
        .expect("writable pool opens");
    droppedneedle_kernel::schema::apply_migrations(&pool)
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
    let service = BackupService::new(
        &live,
        &backup_dir,
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
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
    let refused =
        droppedneedle_tool::tooling::restore::restore_backup(&taken.path, &occupied, false);
    assert!(
        matches!(
            refused,
            Err(droppedneedle_tool::tooling::restore::RestoreError::TargetNotEmpty(_))
        ),
        "occupied target refuses: {refused:?}"
    );

    // A folder with only lock files in it is empty: a server that booted
    // there leaves its lock file behind.
    let locked = root.join("locked");
    std::fs::create_dir_all(&locked).expect("locked dir");
    std::fs::write(locked.join("library.db.server.lock"), b"").expect("lock writes");
    droppedneedle_tool::tooling::restore::restore_backup(&taken.path, &locked, false)
        .expect("restores beside a server lock file");

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
        droppedneedle_tool::tooling::restore::restore_backup(&future, &root.join("new1"), false);
    assert!(
        matches!(
            too_new,
            Err(
                droppedneedle_tool::tooling::restore::RestoreError::BackupTooNew { .. }
                    | droppedneedle_tool::tooling::restore::RestoreError::Refused(_)
            )
        ),
        "newer backup refuses: {too_new:?}"
    );
    let allowed =
        droppedneedle_tool::tooling::restore::restore_backup(&future, &root.join("new2"), true)
            .expect("allow-downgrade restores");
    assert_eq!(allowed.user_version, 9999);
}

/// The covers-debug route serves with the flag, 404s without it, and never
/// appears in the OpenAPI document.
#[tokio::test]
async fn covers_debug_is_dev_only() {
    use axum::http::StatusCode;
    use droppedneedle_kernel::config::AppConfig;

    let flagged =
        common::TestApp::start_configured("covers-debug", AppConfig::with_tooling_routes).await;
    let mbid = uuid::Uuid::new_v4().to_string();
    let uri = format!("/__tooling__/covers/debug/artist/{mbid}");
    let (status, _, body) = flagged.raw("GET", &uri, &[], None).await;
    assert_eq!(status, StatusCode::OK);
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

    let (status, _, body) = flagged
        .raw("GET", "/__tooling__/covers/debug/artist/nope", &[], None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let debug: serde_json::Value = serde_json::from_slice(&body).expect("debug parses");
    assert_eq!(
        debug.get("is_valid_mbid"),
        Some(&serde_json::Value::Bool(false))
    );

    let prod_like = common::TestApp::start("covers-debug-off").await;
    let (status, _, _) = prod_like.raw("GET", &uri, &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    use utoipa::OpenApi as _;
    let openapi =
        serde_json::to_string(&droppedneedle::docs::ApiDoc::openapi()).expect("openapi renders");
    assert!(
        !openapi.contains("__tooling__"),
        "tooling stays out of the API contract"
    );
}
