//! The data folders the server boots on, and the check that keeps it off a
//! v2 or half-upgraded one.
//!
//! [`detect`] tells an empty, v2, v3 or interrupted layout apart without
//! changing anything, and [`check_bootable`] lets the server start only on
//! an empty or v3 one. [`refuse_lost_key`] keeps it off a v3 database whose
//! key file is gone. The offline upgrade in the tool reads the same layout
//! before it moves anything.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

use droppedneedle_kernel::config::AppConfig;
use droppedneedle_kernel::db::factory::sqlite_uri_path;
use droppedneedle_kernel::runtime_config::crypto::{KEY_FILE_NAME, NEW_KEY_FILE_NAME};
use droppedneedle_kernel::runtime_config::ingest::LEGACY_CONFIG_FILE_NAME;

/// Folder inside `config/` and inside `cache/` that keeps the v2 files.
pub const BACKUP_DIR_NAME: &str = "v2-backup";
/// Import report saved in `cache/` by a finished upgrade.
pub const REPORT_FILE_NAME: &str = "upgrade-report.json";
/// Marker in `cache/` while an upgrade is under way.
pub const MARKER_FILE_NAME: &str = ".upgrade-in-progress";
/// The v2 key file inside the config folder.
const V2_KEY_FILE_NAME: &str = ".env";
/// The variable in the v2 key file that holds the data key.
pub const DATA_ENC_KEY_VAR: &str = "DATA_ENC_KEY";

/// The folders one instance keeps its data in, as the server resolves them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDirs {
    /// Holds the encryption key (and, before v3 kept settings in the
    /// database, the settings file).
    pub config_dir: PathBuf,
    /// Holds the database and everything cached beside it.
    pub cache_dir: PathBuf,
    /// The database file.
    pub db_path: PathBuf,
    /// Installed plugins. Only read, for v2's plugin manifests.
    pub plugins_dir: PathBuf,
}

impl DataDirs {
    /// The folders a server started with `config` would use.
    #[must_use]
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            config_dir: config.config_dir(),
            cache_dir: config.cache_dir.clone(),
            db_path: config.library_db_path.clone(),
            plugins_dir: config.plugins_dir(),
        }
    }

    /// The settings file from before settings moved into the database: a
    /// v2 one, or a v3 one copied in once at the first start.
    #[must_use]
    pub fn legacy_config_file(&self) -> PathBuf {
        self.config_dir.join(LEGACY_CONFIG_FILE_NAME)
    }

    /// Where the v2 config files are kept.
    #[must_use]
    pub fn config_backup(&self) -> PathBuf {
        self.config_dir.join(BACKUP_DIR_NAME)
    }

    /// Where the v2 database and cache files are kept.
    #[must_use]
    pub fn cache_backup(&self) -> PathBuf {
        self.cache_dir.join(BACKUP_DIR_NAME)
    }

    /// Where a finished upgrade saves its import report.
    #[must_use]
    pub fn report_file(&self) -> PathBuf {
        self.cache_dir.join(REPORT_FILE_NAME)
    }

    /// The marker an upgrade leaves while it is under way.
    #[must_use]
    pub fn marker(&self) -> PathBuf {
        self.cache_dir.join(MARKER_FILE_NAME)
    }
}

/// What a pair of data folders holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// No database and no key file: nothing has run here yet.
    Empty,
    /// A v2 instance.
    V2,
    /// A v3 instance.
    V3,
    /// An upgrade started here and did not finish.
    Interrupted,
    /// None of the above. Nothing is safe to assume.
    Unknown(UnknownLayout),
}

/// Why a layout could not be told apart.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UnknownLayout {
    /// The database file cannot be read as a database.
    #[error("the database {} cannot be read ({reason})", .path.display())]
    DatabaseUnreadable {
        /// The database file.
        path: PathBuf,
        /// What reading it reported.
        reason: String,
    },
    /// A v2 database sits beside a v3 key file.
    #[error("{} is a v2 database, but {} is a v3 key file", .database.display(), .key.display())]
    V2DatabaseWithV3Key {
        /// The database file.
        database: PathBuf,
        /// The v3 key file.
        key: PathBuf,
    },
    /// A v2 key file with no database to go with it.
    #[error("{} is a v2 key file, but there is no v2 database at {}", .key.display(), .database.display())]
    V2KeyWithoutDatabase {
        /// The v2 key file.
        key: PathBuf,
        /// Where the database was expected.
        database: PathBuf,
    },
}

/// Classify the folders. Read-only: nothing is created, locked or changed.
///
/// The database decides first. Every v3 migration stamps `user_version`
/// and v2 never did, so a stamped database is v3 and an unstamped one with
/// tables is v2. The key files then have to agree: v2 keeps its key in
/// `config/.env`, v3 in `config/data_enc.key`.
#[must_use]
pub fn detect(dirs: &DataDirs) -> Layout {
    if std::fs::symlink_metadata(dirs.marker()).is_ok() {
        return Layout::Interrupted;
    }
    let v3_key = dirs.config_dir.join(KEY_FILE_NAME);
    let has_v3_key = std::fs::symlink_metadata(&v3_key).is_ok();
    match probe_database(&dirs.db_path) {
        Database::V3 => Layout::V3,
        Database::V2 if has_v3_key => Layout::Unknown(UnknownLayout::V2DatabaseWithV3Key {
            database: dirs.db_path.clone(),
            key: v3_key,
        }),
        Database::V2 => Layout::V2,
        Database::Absent if has_v3_key => Layout::V3,
        Database::Absent if names_data_enc_key(&dirs.config_dir) => {
            Layout::Unknown(UnknownLayout::V2KeyWithoutDatabase {
                key: key_file_path(&dirs.config_dir),
                database: dirs.db_path.clone(),
            })
        }
        Database::Absent => Layout::Empty,
        Database::Unreadable(reason) => Layout::Unknown(UnknownLayout::DatabaseUnreadable {
            path: dirs.db_path.clone(),
            reason,
        }),
    }
}

/// Why the server will not start on these folders.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BootRefusal {
    /// The folders hold a v2 instance.
    #[error(
        "this is a DroppedNeedle v2 data directory. Stop the container and run \
         `droppedneedle-tool upgrade`, then start it again. See docs/upgrading-from-v2.md"
    )]
    V2,
    /// An upgrade stopped halfway.
    #[error(
        "an upgrade from v2 did not finish in this data directory. Run \
         `droppedneedle-tool upgrade` again to finish it, or \
         `droppedneedle-tool upgrade --undo --yes` to go back to v2"
    )]
    Interrupted,
    /// The folders hold something that is neither v2 nor v3.
    #[error(
        "cannot tell what this data directory holds: {0}. Nothing was changed. See \
         docs/upgrading-from-v2.md"
    )]
    Unknown(UnknownLayout),
    /// A v3 database whose key file is missing: a new key would leave every
    /// stored secret unreadable.
    #[error(
        "the key file {} is missing, but the database {} already holds this server's data. Put \
         back the data_enc.key saved with this database (from the same backup), or set \
         CONFIG_DIR to the folder that holds it. Nothing was changed. Only if that key is lost \
         for good: stop the server and run `droppedneedle-tool reset-key --yes`, which clears \
         every stored password and API key",
        .key.display(),
        .database.display()
    )]
    KeyMissing {
        /// Where the key file should be.
        key: PathBuf,
        /// The database it belongs to.
        database: PathBuf,
    },
    /// The key file is not the one the database's secrets were sealed with.
    #[error(
        "the key file {} does not belong to the database {}: it cannot open the passwords and \
         API keys stored there. Put back the data_enc.key saved with this database (from the \
         same backup; a key replaced by reset-key is kept as data_enc.key.old), or set \
         CONFIG_DIR to the folder that holds it. Nothing was changed. Only if that key is lost \
         for good: stop the server and run `droppedneedle-tool reset-key --yes`, which clears \
         every stored password and API key",
        .key.display(),
        .database.display()
    )]
    KeyMismatch {
        /// The key file.
        key: PathBuf,
        /// The database it does not belong to.
        database: PathBuf,
    },
    /// `reset-key` stopped before it moved its new key into place.
    #[error(
        "a reset-key did not finish here ({} is still waiting to be moved into place). Stop \
         the server and run `droppedneedle-tool reset-key --yes` again to finish it",
        .0.display()
    )]
    ResetUnfinished(PathBuf),
}

/// The refusal for a key that is missing or does not belong to the
/// database: a `reset-key` that stopped halfway says so instead.
#[must_use]
pub fn key_refusal(config_dir: &Path, db_path: &Path, missing: bool) -> BootRefusal {
    let new_key = config_dir.join(NEW_KEY_FILE_NAME);
    let key = config_dir.join(KEY_FILE_NAME);
    let database = db_path.to_path_buf();
    if std::fs::symlink_metadata(&new_key).is_ok() {
        BootRefusal::ResetUnfinished(new_key)
    } else if missing {
        BootRefusal::KeyMissing { key, database }
    } else {
        BootRefusal::KeyMismatch { key, database }
    }
}

/// The server's boot guard: start only on an empty or v3 layout.
pub fn check_bootable(dirs: &DataDirs) -> Result<(), BootRefusal> {
    match detect(dirs) {
        Layout::Empty | Layout::V3 => Ok(()),
        Layout::V2 => Err(BootRefusal::V2),
        Layout::Interrupted => Err(BootRefusal::Interrupted),
        Layout::Unknown(reason) => Err(BootRefusal::Unknown(reason)),
    }
}

/// Refuse a v3 database whose key file is missing, before any key is
/// minted: every secret the database holds was sealed with the lost key.
/// A database that holds no settings and no accounts passes, as a missing
/// one does: every sealed value lives in a settings row or in a table a user
/// owns, so nothing there can need the lost key (a dry run of the import
/// leaves such a database behind).
pub fn refuse_lost_key(config_dir: &Path, db_path: &Path) -> Result<(), BootRefusal> {
    let key = config_dir.join(KEY_FILE_NAME);
    if std::fs::symlink_metadata(&key).is_ok() {
        return Ok(());
    }
    match probe_database(db_path) {
        Database::V3 if holds_data(db_path) => Err(key_refusal(config_dir, db_path, true)),
        _ => Ok(()),
    }
}

/// Whether a v3 database holds settings or accounts. One that cannot be
/// read counts as holding them.
#[must_use]
pub fn holds_data(db_path: &Path) -> bool {
    let Ok(db) = open_read_only(db_path) else {
        return true;
    };
    ["config_sections", "auth_users"].iter().any(|table| {
        db.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table})"),
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or_else(|error| {
            // A table a schema this old does not have yet holds nothing.
            !matches!(error, rusqlite::Error::SqliteFailure(_, Some(ref message))
                if message.starts_with("no such table"))
        })
    })
}

/// What the database file is.
enum Database {
    /// Missing, or a file with no tables yet.
    Absent,
    V2,
    V3,
    Unreadable(String),
}

fn probe_database(db_path: &Path) -> Database {
    match std::fs::symlink_metadata(db_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Database::Absent,
        Err(error) => return Database::Unreadable(error.to_string()),
        Ok(meta) if !meta.is_file() => {
            return Database::Unreadable("it is not a regular file".to_owned());
        }
        Ok(_) => {}
    }
    match read_stamp(db_path) {
        Ok((stamp, _)) if stamp > 0 => Database::V3,
        Ok((_, 0)) => Database::Absent,
        Ok(_) => Database::V2,
        Err(error) => Database::Unreadable(error.to_string()),
    }
}

/// `user_version` and, for an unstamped file, how many tables it holds.
///
/// A cleanly closed database is opened immutable, which reads the file
/// alone and creates nothing beside it. A database with writes still in
/// its `-wal` file has to be read through that file, so it gets a normal
/// read-only connection.
fn read_stamp(db_path: &Path) -> rusqlite::Result<(i64, i64)> {
    let db = open_read_only(db_path)?;
    let stamp: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if stamp > 0 {
        return Ok((stamp, 0));
    }
    // sqlx creates its bookkeeping table before the first migration runs,
    // so on its own that table is still an empty database.
    let tables = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations'",
        [],
        |row| row.get(0),
    )?;
    Ok((stamp, tables))
}

/// A read-only connection that creates nothing beside the database: a
/// cleanly closed one is opened immutable, one with writes still in its
/// `-wal` file is read through it.
fn open_read_only(db_path: &Path) -> rusqlite::Result<Connection> {
    if wal_bytes(db_path) > 0 {
        Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    } else {
        Connection::open_with_flags(
            format!("file:{}?immutable=1", sqlite_uri_path(db_path)),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
    }
}

/// The `-wal` file beside a database.
#[must_use]
pub fn wal_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

/// Bytes still waiting in the `-wal` file beside a database.
#[must_use]
pub fn wal_bytes(db_path: &Path) -> u64 {
    std::fs::metadata(wal_path(db_path)).map_or(0, |meta| meta.len())
}

/// Expected v2 key-file path, for errors and refusal tests.
#[must_use]
pub fn key_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join(V2_KEY_FILE_NAME)
}

/// Parse dotenv text into variables. Enough for the v2 key file: blank
/// lines and `#` comments skipped, an optional `export` prefix tolerated,
/// and matching single or double quotes stripped from values.
#[must_use]
pub fn parse_dotenv(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let mut value = value.trim().to_owned();
        if value.len() >= 2
            && ((value.starts_with('\'') && value.ends_with('\''))
                || (value.starts_with('"') && value.ends_with('"')))
        {
            value = value[1..value.len() - 1].to_owned();
        }
        vars.insert(name.to_owned(), value);
    }
    vars
}

/// True when `config_dir` holds a v2 key file that names a data key. The
/// key itself is not checked: this only tells a v2 config folder from any
/// other.
#[must_use]
pub fn names_data_enc_key(config_dir: &Path) -> bool {
    std::fs::read_to_string(key_file_path(config_dir)).is_ok_and(|text| {
        parse_dotenv(&text)
            .get(DATA_ENC_KEY_VAR)
            .is_some_and(|value| !value.trim().is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_kernel::db::scratch::ScratchDir;

    fn dirs_in(root: &Path) -> DataDirs {
        let dirs = DataDirs::from_config(&AppConfig::with_root(0, root));
        std::fs::create_dir_all(&dirs.config_dir).unwrap();
        std::fs::create_dir_all(&dirs.cache_dir).unwrap();
        dirs
    }

    fn database(dirs: &DataDirs, stamp: i64, tables: &str) {
        let db = Connection::open(&dirs.db_path).unwrap();
        db.execute_batch(tables).unwrap();
        db.pragma_update(None, "user_version", stamp).unwrap();
    }

    fn v2_key(dirs: &DataDirs) {
        std::fs::write(dirs.config_dir.join(".env"), "DATA_ENC_KEY=abc\n").unwrap();
    }

    fn v3_key(dirs: &DataDirs) {
        std::fs::write(dirs.config_dir.join(KEY_FILE_NAME), "key").unwrap();
    }

    const V2_TABLES: &str = "CREATE TABLE auth_users (id TEXT PRIMARY KEY);";

    #[test]
    fn layouts_are_told_apart_by_the_database_stamp_and_the_key_files() {
        let scratch = ScratchDir::new("upgrade-detect").unwrap();
        let case = |name: &str| dirs_in(&scratch.join(name));

        assert_eq!(detect(&case("empty")), Layout::Empty);

        let v2 = case("v2");
        database(&v2, 0, V2_TABLES);
        v2_key(&v2);
        assert_eq!(detect(&v2), Layout::V2);

        // A lost key file does not make a v2 database anything else.
        let keyless = case("v2-keyless");
        database(&keyless, 0, V2_TABLES);
        assert_eq!(detect(&keyless), Layout::V2);

        // A v3 server that was stopped while it migrated a v2 database
        // leaves sqlx's table behind; the data is still v2's.
        let touched = case("v2-touched");
        database(
            &touched,
            0,
            "CREATE TABLE auth_users (id TEXT PRIMARY KEY);
             CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY);",
        );
        assert_eq!(detect(&touched), Layout::V2);

        let mixed = case("v2-with-v3-key");
        database(&mixed, 0, V2_TABLES);
        v3_key(&mixed);
        assert!(matches!(
            detect(&mixed),
            Layout::Unknown(UnknownLayout::V2DatabaseWithV3Key { .. })
        ));

        // The stamp wins over a v2 key file left lying around.
        let v3 = case("v3");
        database(&v3, 1, V2_TABLES);
        v2_key(&v3);
        assert_eq!(detect(&v3), Layout::V3);

        let v3_fresh = case("v3-no-database");
        v3_key(&v3_fresh);
        assert_eq!(detect(&v3_fresh), Layout::V3);

        let orphan_key = case("v2-key-only");
        v2_key(&orphan_key);
        assert!(matches!(
            detect(&orphan_key),
            Layout::Unknown(UnknownLayout::V2KeyWithoutDatabase { .. })
        ));

        let blank = case("blank-database");
        database(
            &blank,
            0,
            "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY);",
        );
        assert_eq!(detect(&blank), Layout::Empty);

        let garbage = case("garbage");
        std::fs::write(&garbage.db_path, "this is not a database, not even close").unwrap();
        assert!(matches!(
            detect(&garbage),
            Layout::Unknown(UnknownLayout::DatabaseUnreadable { .. })
        ));

        let interrupted = case("interrupted");
        database(&interrupted, 1, V2_TABLES);
        std::fs::write(interrupted.marker(), "{}").unwrap();
        assert_eq!(detect(&interrupted), Layout::Interrupted);
    }

    #[test]
    fn a_stamp_still_in_the_wal_file_counts() {
        let scratch = ScratchDir::new("upgrade-detect-wal").unwrap();
        let dirs = dirs_in(&scratch);
        // Held open so nothing is merged into the database file: its
        // stamp and tables exist in the -wal file only.
        let writer = Connection::open(&dirs.db_path).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        writer.execute_batch(V2_TABLES).unwrap();
        writer.pragma_update(None, "user_version", 1).unwrap();
        assert!(wal_bytes(&dirs.db_path) > 0);
        assert_eq!(detect(&dirs), Layout::V3);
    }

    #[test]
    fn dotenv_parsing_covers_key_file_shapes() {
        let vars = parse_dotenv(
            "# v2 key file\nDATA_ENC_KEY='abc123'\nEMPTY=\nexport OTHER=\"x y\"\nnot-a-pair\n",
        );
        assert_eq!(vars.get("DATA_ENC_KEY").unwrap(), "abc123");
        assert_eq!(vars.get("EMPTY").unwrap(), "");
        assert_eq!(vars.get("OTHER").unwrap(), "x y");
        assert!(!vars.contains_key("not-a-pair"));
    }
}
