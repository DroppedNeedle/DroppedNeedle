//! Offline tooling: upgrade, export, validate, import, dry-run, restore,
//! recovery-code, reset-key, public-address, content-security-policy.
//!
//! Thin operator surface over the migration engines:
//! [`upgrade`](droppedneedle_tool::tooling::upgrade) turns a v2 data directory
//! into a v3 one in place, [`export`](droppedneedle_tool::export) builds sealed
//! envelopes from a v2 instance, [`import`](droppedneedle_tool::import)
//! validates and applies them, and
//! [`restore`](droppedneedle_tool::tooling::restore) revives server backups.
//! [`recovery-code`](droppedneedle_tool::tooling::recovery_code) lets a
//! locked-out admin reset a password; its stdout is the code and nothing
//! else.
//! [`public-address`](droppedneedle_tool::tooling::host_settings) states the
//! address the web UI is opened at, for an admin whose saves are refused,
//! and `content-security-policy` chooses how that policy is sent, for an
//! admin whose browser shows a blank page under it.
//! Everything is file plus SQLite; nothing here touches the network.
//!
//! The operator passphrase never travels as a CLI argument: it comes from
//! `--passphrase-file` or from stdin (first line). `upgrade` makes its own
//! and keeps it in memory. Import and dry-run print exactly one JSON
//! report object to stdout.
//!
//! Ordering note: import and dry-run migrate the target schema and probe
//! the write lock before the pipeline decides anything, so a refused run
//! (bad passphrase, invalid file) can still create or migrate the target
//! database file. Zero writes means zero import writes: no entities, no
//! config, no audit row.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use droppedneedle::bootstrap::data_dirs::{DataDirs, Layout};
use droppedneedle_kernel::config::AppConfig;
use droppedneedle_kernel::db::datalock::DataLock;
use droppedneedle_kernel::runtime_config::Secret;
use droppedneedle_kernel::runtime_config::sections::ContentSecurityPolicyMode;
use droppedneedle_tool::export::v2dir::V2Dirs;
use droppedneedle_tool::export::{ExportRequest, export_v2_to_file};
use droppedneedle_tool::r#import::ExitCode;
use droppedneedle_tool::tooling::host_settings;
use droppedneedle_tool::tooling::recovery_code;
use droppedneedle_tool::tooling::reset_key;
use droppedneedle_tool::tooling::upgrade::{
    self, OfflineImport, Step, Summary, UpgradeError, UpgradeRequest, Upgraded,
};

/// Usage text, printed for `help` and on bad invocations.
const USAGE: &str = "\
droppedneedle-tool: offline tooling for a DroppedNeedle data directory

Usage:
  droppedneedle-tool upgrade [--no-backup] [--v2-commit <tag>]
  droppedneedle-tool upgrade --undo --yes
  droppedneedle-tool remove-v2-backup
  droppedneedle-tool export --v2-root <dir> --out <file> [--db <path>]
      [--v2-commit <sha>] [--passphrase-file <file>]
  droppedneedle-tool validate <file> [--v2-root <dir>] [--passphrase-file <file>]
  droppedneedle-tool import --file <export> --db <v3.db> --config-dir <dir>
      [--cache-dir <dir>] [--passphrase-file <file>] [--v2-config <config.json>]
  droppedneedle-tool dry-run --file <export> --db <v3.db> --config-dir <dir>
      [--cache-dir <dir>] [--passphrase-file <file>] [--v2-config <config.json>]
  droppedneedle-tool restore --backup <file> --target-dir <dir> [--allow-downgrade]
  droppedneedle-tool recovery-code [--] <username>
  droppedneedle-tool recovery-code --list-admins
  droppedneedle-tool reset-key --yes
  droppedneedle-tool public-address <address>...
  droppedneedle-tool public-address --show | --clear
  droppedneedle-tool content-security-policy enforce | report-only | off

upgrade turns the v2 data in the server's config and cache folders into
v3 data, in place. Stop the server first. The v2 files are kept in a
v2-backup folder inside each of the two, unless --no-backup. It finds the
folders the way the server does (/app/config and /app/cache unless the
environment says otherwise). upgrade --undo --yes deletes everything in
the two folders except the v2-backup folders, then puts the v2 files back.
remove-v2-backup deletes the kept v2 files.

restore copies a server backup into an empty folder as library.db, with
the undo copies it names in undo/ beside it (read from the undo folder
next to the backup). The settings come with the database. Keep the
instance's config/data_enc.key with it: without that key, stored
passwords and API keys cannot be read.

The passphrase comes from --passphrase-file or stdin, never from argv.
Export writes <file> plus <name>.bundle.sqlite beside it; keep the two
together. Import and dry-run print exactly one JSON report to stdout.
--cache-dir defaults to the folder holding --db.

recovery-code prints a one-time password recovery code for an account.
droppedneedle-tool recovery-code --help says how to use it.

reset-key gives an instance whose config/data_enc.key is lost for good a
new key, clearing every stored password and API key.
droppedneedle-tool reset-key --help says what it clears and changes.

public-address states the address the web UI is opened at, for when every
save is answered \"Origin not allowed\".
droppedneedle-tool public-address --help says how to use it.

content-security-policy chooses how the web UI's content-security policy
is sent, for when the web UI shows a blank page.
droppedneedle-tool content-security-policy --help says how to use it.
";

/// Help for `content-security-policy`, printed without touching the data
/// directory.
const CONTENT_SECURITY_POLICY_HELP: &str = "\
Usage:
  droppedneedle-tool content-security-policy enforce
  droppedneedle-tool content-security-policy report-only
  droppedneedle-tool content-security-policy off

Chooses how the web UI's content-security policy is sent. It is the
Content-security policy setting on the Security tab, for an admin who
cannot reach that tab: a reverse proxy that rewrites the page (an injected
script, a script optimiser) can leave the web UI blank while the policy is
enforced. Stop the server first, run it, then start the server.

enforce      the browser refuses what the policy does not allow
report-only  the browser lists in its console what the policy would refuse
             and refuses nothing
off          no policy is sent

It finds the data the way upgrade does. Settings live in the database: on
an instance this version has not started yet, it first copies in the old
config/config.json, as the server's first start would.
";

/// Help for `public-address`, printed without touching the data directory.
const PUBLIC_ADDRESS_HELP: &str = "\
Usage:
  droppedneedle-tool public-address <address>...
  droppedneedle-tool public-address --show
  droppedneedle-tool public-address --clear

States the address the web UI is opened at, for when a reverse proxy leaves
the server with the wrong idea of its own address and every save is
answered \"Origin not allowed\". It is the Public address setting on the
Security tab, for an admin who cannot save it there. Stop the server first,
run it, then start the server.

<address> is what the browser's address bar shows, up to the host and port:
https://music.example.com or http://192.168.1.5:8688. No path, not even the
base path. Give up to eight; together they replace the saved list. Only list
addresses at which this DroppedNeedle opens: a page at a listed address can
make changes as whoever is signed in.

--show prints the saved list, one address a line. --clear empties it.

It finds the data the way upgrade does. Settings live in the database: on
an instance this version has not started yet, even --show first copies in
the old config/config.json, as the server's first start would.
";

/// Help for `recovery-code`, printed without touching the data directory.
const RECOVERY_CODE_HELP: &str = "\
Usage:
  droppedneedle-tool recovery-code [--] <username>
  droppedneedle-tool recovery-code --list-admins

Prints a one-time password recovery code for an account, for when nobody
can sign in to make one. Stop the server first, run it, start the server,
then enter the username, the code and a new password on the Forgot
password page within 15 minutes. An account that only signs in through
single sign-on, Plex or Jellyfin gets its first password this way and
keeps its other sign-in.

<username> is the DroppedNeedle username; --list-admins prints the admin
usernames. Put -- before a username that starts with a dash.

It finds the data the way upgrade does and opens the database the way a
server start does, so a database from an older version is backed up and
migrated first. Whoever holds the code can take over that account.
";

/// Help for `reset-key`, printed without touching the data directory.
const RESET_KEY_HELP: &str = "\
Usage:
  droppedneedle-tool reset-key --yes

For when the server refuses to start because config/data_enc.key is missing
or does not belong to the database, and the right key is lost for good.
First try to put back the data_enc.key saved with the database. This
command cannot bring back what the old key sealed: it clears it.

It mints a new key and clears every stored password and API key: the
download client, indexer, Lidarr, single sign-on, Spotify and plugin
secrets in Settings, every app password, and every user's link to Plex,
Jellyfin, Navidrome, Spotify, ListenBrainz and Last.fm. It prints which
settings it cleared, never their values. The old key is kept as
config/data_enc.key.old (or .old.1, .old.2), which still opens backups made
before the reset.

It replaces a lost key and does not change the Jellyfin ids of what you
already have: stored album and artist ids survive the reset, and only items
added afterwards get ids under the new key. Subsonic picture links,
corrections links and transcode links made before stop opening.

It refuses, changing nothing, while the key file belongs to the database.
Stop the server first. It finds the data the way upgrade does.
";

/// Flags that take no value.
const BARE_FLAGS: &[&str] = &["--allow-downgrade", "--no-backup", "--undo", "--yes"];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let outcome = match command {
        "upgrade" => run_upgrade(&args[1..]),
        "remove-v2-backup" => run_remove_backup(&args[1..]),
        "export" => run_export(&args[1..]),
        "validate" => run_validate(&args[1..]),
        "import" => run_import_cmd(&args[1..], false),
        "dry-run" => run_import_cmd(&args[1..], true),
        "restore" => run_restore(&args[1..]),
        "recovery-code" => run_recovery_code(&args[1..]),
        "reset-key" => run_reset_key(&args[1..]),
        "public-address" => run_public_address(&args[1..]),
        "content-security-policy" => run_content_security_policy(&args[1..]),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            return;
        }
        unknown => {
            eprintln!("unknown command {unknown:?}\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(message) = outcome {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

/// One parsed `--flag value` pair walk. Returns the value for `flag`, or an
/// error naming the missing flag.
fn flag_value(args: &[String], flag: &str) -> Result<Option<String>, String> {
    let mut index = 0;
    while index < args.len() {
        if args[index] == flag {
            return args
                .get(index + 1)
                .cloned()
                .map(Some)
                .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"));
        }
        index += 1;
    }
    Ok(None)
}

/// Required `--flag value`.
fn required_flag(args: &[String], flag: &str) -> Result<String, String> {
    match flag_value(args, flag)? {
        Some(value) => Ok(value),
        None => Err(format!("missing required {flag}\n{USAGE}")),
    }
}

/// True when the bare `flag` is present.
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

/// Reject anything that is not a known flag or its value. `positional`
/// allows one trailing non-flag argument (the validate file).
fn check_flags(args: &[String], known: &[&str], positional: bool) -> Result<(), String> {
    let mut index = 0;
    let mut positionals = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if known.contains(&arg) {
            if BARE_FLAGS.contains(&arg) {
                index += 1;
            } else {
                index += 2;
            }
            continue;
        }
        if arg.starts_with("--") {
            return Err(format!("unknown flag {arg:?}\n{USAGE}"));
        }
        positionals += 1;
        index += 1;
    }
    if !positional && positionals > 0 || positional && positionals > 1 {
        return Err(format!("unexpected argument\n{USAGE}"));
    }
    Ok(())
}

/// Read the operator passphrase: `--passphrase-file`, else the first stdin
/// line. Never from argv.
fn read_passphrase(args: &[String]) -> Result<Secret, String> {
    if let Some(path) = flag_value(args, "--passphrase-file")? {
        let text = std::fs::read_to_string(&path).map_err(|_| format!("cannot read {path}"))?;
        return Ok(Secret::new(text.trim_end_matches(['\r', '\n']).to_owned()));
    }
    use std::io::BufRead as _;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|_| "cannot read passphrase from stdin".to_owned())?;
    Ok(Secret::new(line.trim_end_matches(['\r', '\n']).to_owned()))
}

/// `export --v2-root <dir> --out <file> [...]`.
fn run_export(args: &[String]) -> Result<(), String> {
    check_flags(
        args,
        &[
            "--v2-root",
            "--out",
            "--db",
            "--v2-commit",
            "--passphrase-file",
        ],
        false,
    )?;
    let request = ExportRequest {
        v2: V2Dirs::under_root(std::path::Path::new(&required_flag(args, "--v2-root")?)),
        db_path: flag_value(args, "--db")?.map(PathBuf::from),
        passphrase: read_passphrase(args)?,
        exported_at: None,
        v2_commit: flag_value(args, "--v2-commit")?,
    };
    let out = required_flag(args, "--out")?;
    export_v2_to_file(&request, PathBuf::from(&out).as_path())
        .map_err(|error| format!("{}: {error}", error.code()))?;
    println!("wrote {out}");
    Ok(())
}

/// `validate <file> [--v2-root <dir>] [--passphrase-file <file>]`. With a
/// passphrase the content digest is checked before anything else.
fn run_validate(args: &[String]) -> Result<(), String> {
    check_flags(args, &["--v2-root", "--passphrase-file"], true)?;
    // Positional file: skip flag values so a flag's value never misreads.
    let mut file: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--v2-root" || args[index] == "--passphrase-file" {
            index += 2;
            continue;
        }
        file = Some(args[index].clone());
        index += 1;
    }
    let file = file.ok_or_else(|| format!("validate needs an export file\n{USAGE}"))?;
    // The same parse path the pipeline takes: lenient envelope parse, then
    // the validator owns every structural and semantic decision. Validate
    // and import agree by construction, and each finding prints once under
    // its stable validator code.
    let bytes = std::fs::read(&file).map_err(|_| format!("cannot read {file}"))?;
    let parsed = droppedneedle_tool::r#import::ExportFile::parse(&bytes)
        .map_err(|error| error.to_string())?;
    if flag_value(args, "--passphrase-file")?.is_some() {
        let passphrase = read_passphrase(args)?;
        droppedneedle_tool::r#import::verify_content_digest(
            &parsed.root,
            &parsed.envelope,
            passphrase.expose(),
        )
        .map_err(|error| {
            format!(
                "{}: {error} (wrong passphrase or modified file)",
                error.code()
            )
        })?;
    }
    let mut report = droppedneedle_tool::r#import::validate_export(&parsed.root);
    if report.valid() {
        let folder = folder_of(std::path::Path::new(&file));
        report
            .errors
            .extend(droppedneedle_tool::r#import::check_attachments(
                &parsed.root,
                &folder,
            ));
    }
    for issue in &report.warnings {
        println!(
            "warning {} at {}: {}",
            issue.code, issue.path, issue.message
        );
    }
    print_left_behind(&parsed.root);
    print_notes(&parsed.root);
    if let Some(v2_root) = flag_value(args, "--v2-root")? {
        let instance_id = parsed
            .root
            .get("instance_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        check_instance_match(instance_id, &v2_root)?;
    }
    if report.valid() {
        println!("valid: {} warning(s)", report.warnings.len());
        Ok(())
    } else {
        for issue in &report.errors {
            println!("error {} at {}: {}", issue.code, issue.path, issue.message);
        }
        Err(format!("invalid: {} error(s)", report.errors.len()))
    }
}

/// Print the export's notes, one line each.
fn print_notes(root: &serde_json::Value) {
    let notes = root
        .get("notes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str);
    for note in notes {
        println!("note: {note}");
    }
}

/// Print what the export leaves behind in v2, one line per table.
fn print_left_behind(root: &serde_json::Value) {
    let Some(items) = root
        .get("left_behind")
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    if items.is_empty() {
        return;
    }
    println!("left behind in v2 (not carried to v3):");
    for item in items {
        let table = item
            .get("table")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let rows = item
            .get("rows")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        let reason = item
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        println!("  {table}: {rows} ({reason})");
    }
}

/// Optional `--v2-root` cross-check: the file must name the instance it was
/// exported from.
fn check_instance_match(export_instance: &str, v2_root: &str) -> Result<(), String> {
    let dirs = V2Dirs::under_root(std::path::Path::new(v2_root));
    let config = droppedneedle_tool::export::v2dir::read_config(&dirs.config_dir)
        .map_err(|error| format!("{}: {error}", error.code()))?;
    let v2_instance = config
        .get("instance_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if v2_instance == export_instance {
        Ok(())
    } else {
        Err(format!(
            "INSTANCE_MISMATCH: export names {export_instance:?} but {v2_root} holds {v2_instance:?}"
        ))
    }
}

/// `import` / `dry-run --file <export> --db <v3.db> --config-dir <dir> [...]`.
/// Prints exactly one JSON report to stdout.
fn run_import_cmd(args: &[String], dry_run: bool) -> Result<(), String> {
    check_flags(
        args,
        &[
            "--file",
            "--db",
            "--config-dir",
            "--cache-dir",
            "--passphrase-file",
            "--v2-config",
        ],
        false,
    )?;
    let export_path = required_flag(args, "--file")?;
    let db_path = PathBuf::from(required_flag(args, "--db")?);
    let config_dir = PathBuf::from(required_flag(args, "--config-dir")?);
    // The bundle sits beside the export file; avatars land in the v3
    // cache folder, which holds the database unless told otherwise.
    let attachments_dir = folder_of(std::path::Path::new(&export_path));
    let cache_dir = match flag_value(args, "--cache-dir")? {
        Some(dir) => PathBuf::from(dir),
        None => folder_of(&db_path),
    };
    let export_bytes =
        std::fs::read(&export_path).map_err(|_| format!("cannot read {export_path}"))?;
    let passphrase = read_passphrase(args)?;
    let v2_config = flag_value(args, "--v2-config")?.map(PathBuf::from);
    let runtime = new_runtime()?;
    let verb = if dry_run { "dry-run" } else { "import" };
    if let Some(parent) = db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create database dir: {error}"))?;
    }
    // Held for the whole run: a server holding its shared lock refuses
    // the import, and a server cannot start until the import ends.
    let data_lock = DataLock::exclusive(&db_path)
        .map_err(|error| format!("{error} (stop the server before importing)"))?;
    let report = runtime
        .block_on(upgrade::run_offline_import(
            &data_lock,
            OfflineImport {
                export_bytes,
                passphrase,
                db_path,
                config_dir,
                cache_dir,
                attachments_dir,
                v2_config,
                dry_run,
                fault_before_commit: false,
            },
        ))
        .map_err(|error| error.to_string())?;
    println!("{}", report.to_json());
    match report.exit.code {
        ExitCode::Ok | ExitCode::OkWithDrops => Ok(()),
        _ => Err(format!("{verb} failed (see report above)")),
    }
}

/// The folder holding `path`; `.` for a bare file name.
fn folder_of(path: &std::path::Path) -> PathBuf {
    path.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
}

/// The single-threaded runtime the import pipeline runs on.
fn new_runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start runtime: {error}"))
}

/// The config and cache folders the server would use, from the same
/// environment.
fn data_dirs() -> Result<DataDirs, String> {
    AppConfig::load()
        .map(|config| DataDirs::from_config(&config))
        .map_err(|error| error.to_string())
}

/// `upgrade [--no-backup] [--v2-commit <tag>]`, or `upgrade --undo --yes`.
fn run_upgrade(args: &[String]) -> Result<(), String> {
    check_flags(
        args,
        &["--no-backup", "--undo", "--yes", "--v2-commit"],
        false,
    )?;
    let undo = has_flag(args, "--undo");
    let v2_commit = flag_value(args, "--v2-commit")?;
    if undo && (has_flag(args, "--no-backup") || v2_commit.is_some()) {
        return Err(format!("--undo goes with --yes only\n{USAGE}"));
    }
    if has_flag(args, "--yes") && !undo {
        return Err(format!("--yes goes with --undo only\n{USAGE}"));
    }
    let dirs = data_dirs()?;
    if undo {
        return run_undo(&dirs, has_flag(args, "--yes"));
    }
    let request = UpgradeRequest {
        dirs,
        keep_backup: !has_flag(args, "--no-backup"),
        v2_commit,
        fault: None,
    };
    let outcome = new_runtime()?
        .block_on(upgrade::run(&request, &mut print_step))
        .map_err(|error| upgrade_failure(&error))?;
    match outcome {
        Upgraded::NothingToDo(Layout::V3) => {
            println!("This data directory is already on v3. Nothing to do.");
        }
        Upgraded::NothingToDo(_) => println!(
            "No v2 data in {} and {}. Nothing to do.",
            request.dirs.config_dir.display(),
            request.dirs.cache_dir.display()
        ),
        Upgraded::Done(summary) => print_summary(&request.dirs, &summary),
    }
    Ok(())
}

/// One progress line per step, on stderr so stdout stays the summary.
fn print_step(step: Step) {
    match step {
        Step::Recovering => {
            eprintln!("An earlier upgrade did not finish. Putting the v2 files back first.");
        }
        Step::MovingAside => eprintln!("Moving the v2 files into the v2-backup folders."),
        Step::Exporting => eprintln!("Exporting from v2."),
        Step::OrphansRemoved(count) => eprintln!(
            "Removed {count} follows and approvals of deleted users from the v2 database. \
             Exporting again."
        ),
        Step::DryRun => eprintln!("Checking the import with a dry run."),
        Step::Importing => eprintln!("Importing into v3."),
        Step::RollingBack => eprintln!("The upgrade failed. Putting the v2 files back."),
    }
}

/// The failure as text. A failed import adds the errors its report lists.
fn upgrade_failure(error: &UpgradeError) -> String {
    let mut text = format!("upgrade failed: {error}");
    let failed_import = match error {
        UpgradeError::RollbackFailed { cause, .. } => cause.as_ref(),
        other => other,
    };
    if let UpgradeError::ImportFailed { report, .. } = failed_import {
        for item in report.items.iter().filter(|item| item.outcome == "error") {
            text.push_str(&format!(
                "\n  {} {}: {}",
                item.entity, item.key, item.detail
            ));
        }
    }
    text
}

/// What the upgrade moved, what it left, and where the v2 files are now.
fn print_summary(dirs: &DataDirs, summary: &Summary) {
    let report = &summary.report;
    println!("Upgrade complete. This data directory is now on v3.");

    let mut moved: Vec<(&String, u64)> = report
        .entities
        .iter()
        .map(|(name, counts)| (name, counts.imported))
        .filter(|(_, imported)| *imported > 0)
        .collect();
    moved.sort();
    if !moved.is_empty() {
        println!("\nMoved to v3:");
        for (name, imported) in moved {
            println!("  {}: {imported}", name.replace('_', " "));
        }
    }
    let attention: u64 = report
        .entities
        .values()
        .map(|counts| {
            counts.conflict_kept_existing
                + counts.dropped_unknown_user
                + counts.dropped_invalid
                + counts.nulled_field
                + counts.error
        })
        .sum();
    if attention > 0 {
        println!(
            "\n{attention} record(s) were left out or changed on the way. The report lists each."
        );
    }
    if summary.orphans_removed > 0 {
        println!(
            "\nRemoved {} follows and approvals that belonged to deleted users.",
            summary.orphans_removed
        );
    }
    if !report.left_behind.is_empty() {
        println!("\nLeft behind in v2 (not carried to v3):");
        for item in &report.left_behind {
            println!("  {}: {} ({})", item.table, item.rows, item.reason);
        }
    }
    if !report.notes.is_empty() {
        println!("\nWorth a look:");
        for note in &report.notes {
            println!("  {note}");
        }
    }
    if !report.settings_defaulted.is_empty() {
        println!(
            "\nSettings v2 had nothing saved for, now at v3 defaults:\n  {}",
            report.settings_defaulted.join(", ")
        );
    }

    if summary.backup_kept {
        println!(
            "\nYour v2 files are kept in:\n  {}\n  {}",
            dirs.config_backup().display(),
            dirs.cache_backup().display()
        );
        println!("Remove them once you are happy with v3:  droppedneedle-tool remove-v2-backup");
        println!(
            "Go back to v2:                           droppedneedle-tool upgrade --undo --yes"
        );
    } else {
        println!("\nThe v2 files were deleted, as asked with --no-backup.");
    }
    println!("\nFull report: {}", dirs.report_file().display());
    println!("Start the server now.");
}

/// `upgrade --undo`: back to v2. It deletes the v3 data, so it wants
/// `--yes`.
fn run_undo(dirs: &DataDirs, yes: bool) -> Result<(), String> {
    let (config, cache) = (dirs.config_dir.display(), dirs.cache_dir.display());
    if !yes {
        return Err(format!(
            "upgrade --undo deletes everything in {config} and {cache} except the v2-backup \
             folders, then puts the v2 files back. Nothing was changed. Run it again with \
             --yes to go ahead."
        ));
    }
    upgrade::undo(dirs).map_err(|error| format!("undo failed: {error}"))?;
    println!("Back on v2. The v2 files are in {config} and {cache} again; the v3 data is gone.");
    println!("Set the image tag back to your v2 version, then start the server.");
    Ok(())
}

/// `remove-v2-backup`: delete the v2 files a finished upgrade kept.
fn run_remove_backup(args: &[String]) -> Result<(), String> {
    check_flags(args, &[], false)?;
    let removed = upgrade::remove_backup(&data_dirs()?).map_err(|error| error.to_string())?;
    if removed.is_empty() {
        println!("No v2 backup found. Nothing to remove.");
    }
    for dir in removed {
        println!("removed {}", dir.display());
    }
    Ok(())
}

/// `restore --backup <file> --target-dir <dir> [--allow-downgrade]`.
fn run_restore(args: &[String]) -> Result<(), String> {
    check_flags(
        args,
        &["--backup", "--target-dir", "--allow-downgrade"],
        false,
    )?;
    let backup = required_flag(args, "--backup")?;
    let target = required_flag(args, "--target-dir")?;
    let summary = droppedneedle_tool::tooling::restore::restore_backup(
        PathBuf::from(&backup).as_path(),
        PathBuf::from(&target).as_path(),
        has_flag(args, "--allow-downgrade"),
    )
    .map_err(|error| error.to_string())?;
    match serde_json::to_string_pretty(&summary) {
        Ok(text) => println!("{text}"),
        Err(_) => return Err("cannot render restore summary".to_owned()),
    }
    Ok(())
}

/// `recovery-code <username>`, `recovery-code -- <username>` or
/// `recovery-code --list-admins`. The code (or the usernames) is the only
/// thing on stdout; what to do with it goes to stderr. A first argument
/// that starts with a dash is a flag unless it follows `--`.
fn run_recovery_code(args: &[String]) -> Result<(), String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let username = match args.as_slice() {
        ["-h" | "--help"] => {
            println!("{RECOVERY_CODE_HELP}");
            return Ok(());
        }
        ["--list-admins"] => return run_list_admins(),
        ["--", username] => *username,
        [username] if !username.starts_with('-') => *username,
        [] => {
            return Err(format!(
                "recovery-code needs a username\n{RECOVERY_CODE_HELP}"
            ));
        }
        [flag] => return Err(format!("unknown flag {flag:?}\n{RECOVERY_CODE_HELP}")),
        _ => {
            return Err(format!(
                "recovery-code takes one username\n{RECOVERY_CODE_HELP}"
            ));
        }
    };
    let issued = new_runtime()?
        .block_on(recovery_code::issue(&data_dirs()?, username))
        .map_err(|error| error.to_string())?;
    println!("{}", issued.code.recovery_code);
    eprintln!(
        "Recovery code for {username}, good for {} minutes and one use. Start the server, \
         open \"Forgot password?\" on the sign-in page and enter this username, the code and \
         a new password.",
        droppedneedle_auth::users::services::RECOVERY_TTL_SECS / 60
    );
    if !issued.has_local_password {
        eprintln!(
            "{username} has no password yet. Using the code gives the account one; its other \
             sign-in methods stay as they are."
        );
    }
    Ok(())
}

/// `recovery-code --list-admins`: admin usernames, one per line.
fn run_list_admins() -> Result<(), String> {
    let names = new_runtime()?
        .block_on(recovery_code::admin_usernames(&data_dirs()?))
        .map_err(|error| error.to_string())?;
    if names.is_empty() {
        return Err("No admin account has a username.".to_owned());
    }
    for name in names {
        println!("{name}");
    }
    Ok(())
}

/// `reset-key --yes`. Prints counts and setting names, never a value.
fn run_reset_key(args: &[String]) -> Result<(), String> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--yes"] => {}
        ["-h" | "--help"] => {
            println!("{RESET_KEY_HELP}");
            return Ok(());
        }
        _ => return Err(format!("reset-key needs --yes\n{RESET_KEY_HELP}")),
    }
    let done = new_runtime()?
        .block_on(reset_key::reset(&data_dirs()?))
        .map_err(|error| error.to_string())?;
    match &done.forgotten {
        None => println!("Finished a reset-key that had stopped before moving the new key in."),
        Some(forgotten) => {
            println!("A new data key is in place. Cleared:");
            println!("  settings values: {}", forgotten.settings.len());
            for path in &forgotten.settings {
                println!("    {path}");
            }
            println!("  app passwords revoked: {}", forgotten.app_passwords);
            println!(
                "  single sign-on token sets dropped: {}",
                forgotten.sign_in_tokens
            );
            println!(
                "  service links removed (users link them again): {}",
                forgotten.connections
            );
        }
    }
    if let Some(old) = &done.old_key {
        println!(
            "The old key is kept as {}. It still opens backups made before this reset.",
            old.display()
        );
    }
    println!(
        "Re-enter the cleared settings. Stored Jellyfin ids are kept, so Finamp and Jellify \
         keep their pins; Subsonic picture, corrections and transcode links made before stop \
         opening. If nobody can sign in, run droppedneedle-tool recovery-code."
    );
    if let Some(config) = &done.unread_config {
        eprintln!(
            "{} is still there. Settings live in the database, so it stays unread.",
            config.display()
        );
    }
    Ok(())
}

/// `content-security-policy enforce|report-only|off`. The saved mode is the
/// only thing on stdout.
fn run_content_security_policy(args: &[String]) -> Result<(), String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mode = match args.as_slice() {
        ["-h" | "--help"] => {
            println!("{CONTENT_SECURITY_POLICY_HELP}");
            return Ok(());
        }
        ["enforce"] => ContentSecurityPolicyMode::Enforce,
        ["report-only"] => ContentSecurityPolicyMode::ReportOnly,
        ["off"] => ContentSecurityPolicyMode::Off,
        _ => {
            return Err(format!(
                "content-security-policy takes one of enforce, report-only or off\n\
                 {CONTENT_SECURITY_POLICY_HELP}"
            ));
        }
    };
    let saved = new_runtime()?
        .block_on(host_settings::set_content_security_policy(
            &data_dirs()?,
            mode,
        ))
        .map_err(|error| error.to_string())?;
    println!(
        "{}",
        match saved {
            ContentSecurityPolicyMode::Enforce => "enforce",
            ContentSecurityPolicyMode::ReportOnly => "report-only",
            ContentSecurityPolicyMode::Off => "off",
        }
    );
    eprintln!("Saved. Start the server.");
    Ok(())
}

/// `public-address <address>...`, `public-address --show` or
/// `public-address --clear`. The saved list is the only thing on stdout.
fn run_public_address(args: &[String]) -> Result<(), String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let dirs = match args.as_slice() {
        ["-h" | "--help"] => {
            println!("{PUBLIC_ADDRESS_HELP}");
            return Ok(());
        }
        [] => {
            return Err(format!(
                "public-address needs an address\n{PUBLIC_ADDRESS_HELP}"
            ));
        }
        _ => data_dirs()?,
    };
    let runtime = new_runtime()?;
    let saved = match args.as_slice() {
        ["--show"] => runtime.block_on(host_settings::public_address(&dirs)),
        ["--clear"] => runtime.block_on(host_settings::set_public_address(&dirs, Vec::new())),
        addresses => {
            if let Some(flag) = addresses.iter().find(|arg| arg.starts_with('-')) {
                return Err(format!("unknown flag {flag:?}\n{PUBLIC_ADDRESS_HELP}"));
            }
            let addresses = addresses
                .iter()
                .map(|&address| address.to_owned())
                .collect();
            runtime.block_on(host_settings::set_public_address(&dirs, addresses))
        }
    }
    .map_err(|error| error.to_string())?;
    for address in &saved {
        println!("{address}");
    }
    match (args.as_slice(), saved.is_empty()) {
        (["--show"], true) => eprintln!("No public address is saved."),
        (["--show"], false) => {}
        (_, true) => eprintln!("Cleared. Start the server."),
        (_, false) => eprintln!(
            "Saved. Start the server: changes made from a page at one of these addresses are \
             now accepted."
        ),
    }
    Ok(())
}
