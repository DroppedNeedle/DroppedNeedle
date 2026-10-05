//! Offline migration tooling: export, validate, import, dry-run, restore.
//!
//! Thin operator surface over the migration engines: [`export`](droppedneedle::export)
//! builds sealed envelopes from a v2 instance dir,
//! [`import`](droppedneedle::import) validates and applies them, and
//! [`restore`](droppedneedle::tooling::restore) revives server backups.
//! Everything is file plus SQLite; nothing here touches the network.
//!
//! The operator passphrase never travels as a CLI argument: it comes from
//! `--passphrase-file` or from stdin (first line). Import and dry-run print
//! exactly one JSON report object to stdout.
//!
//! Ordering note: import and dry-run migrate the target schema and probe
//! the write lock before the pipeline decides anything, so a refused run
//! (bad passphrase, invalid file) can still create or migrate the target
//! database file. Zero writes means zero import writes: no entities, no
//! config, no audit row.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use droppedneedle::export::{ExportRequest, export_v2_to_file};
use droppedneedle::r#import::{ExitCode, ImportRequest, run_import};
use droppedneedle::runtime_config::{Crypto, Secret};

/// Usage text, printed for `help` and on bad invocations.
const USAGE: &str = "\
droppedneedle-tool: offline v2 migration tooling

Usage:
  droppedneedle-tool export --v2-root <dir> --out <file> [--db <path>]
      [--v2-commit <sha>] [--passphrase-file <file>]
  droppedneedle-tool validate <file> [--v2-root <dir>] [--passphrase-file <file>]
  droppedneedle-tool import --file <export> --db <v3.db> --config-dir <dir>
      [--passphrase-file <file>] [--v2-config <config.json>]
  droppedneedle-tool dry-run --file <export> --db <v3.db> --config-dir <dir>
      [--passphrase-file <file>] [--v2-config <config.json>]
  droppedneedle-tool restore --backup <file> --target-dir <dir> [--allow-downgrade]

The passphrase comes from --passphrase-file or stdin, never from argv.
Import and dry-run print exactly one JSON report to stdout.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let outcome = match command {
        "export" => run_export(&args[1..]),
        "validate" => run_validate(&args[1..]),
        "import" => run_import_cmd(&args[1..], false),
        "dry-run" => run_import_cmd(&args[1..], true),
        "restore" => run_restore(&args[1..]),
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
            if arg == "--allow-downgrade" {
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
        v2_root: PathBuf::from(required_flag(args, "--v2-root")?),
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
    let parsed =
        droppedneedle::r#import::ExportFile::parse(&bytes).map_err(|error| error.to_string())?;
    if flag_value(args, "--passphrase-file")?.is_some() {
        let passphrase = read_passphrase(args)?;
        droppedneedle::r#import::verify_content_digest(
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
    let report = droppedneedle::r#import::validate_export(&parsed.root);
    for issue in &report.warnings {
        println!(
            "warning {} at {}: {}",
            issue.code, issue.path, issue.message
        );
    }
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

/// Optional `--v2-root` cross-check: the file must name the instance it was
/// exported from.
fn check_instance_match(export_instance: &str, v2_root: &str) -> Result<(), String> {
    let config = droppedneedle::export::v2dir::read_config(std::path::Path::new(v2_root))
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
            "--passphrase-file",
            "--v2-config",
        ],
        false,
    )?;
    let export_path = required_flag(args, "--file")?;
    let db_path = required_flag(args, "--db")?;
    let config_dir = PathBuf::from(required_flag(args, "--config-dir")?);
    let export_bytes =
        std::fs::read(&export_path).map_err(|_| format!("cannot read {export_path}"))?;
    let passphrase = read_passphrase(args)?;
    let v2_config = flag_value(args, "--v2-config")?.map(PathBuf::from);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start runtime: {error}"))?;
    let verb = if dry_run { "dry-run" } else { "import" };
    // Held for the whole run: a server holding its shared lock refuses
    // the import, and a server cannot start until the import ends.
    if let Some(parent) = PathBuf::from(&db_path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create database dir: {error}"))?;
    }
    let _data_lock =
        droppedneedle::tooling::datalock::DataLock::exclusive(PathBuf::from(&db_path).as_path())
            .map_err(|error| format!("{error} (stop the server before importing)"))?;
    let report = runtime.block_on(async {
        // Writable pool, not the serving runtime: its pool turns read-only
        // after boot, while the offline import owns its writes. The server
        // must be stopped: the data lock above refuses a running server,
        // and the immediate-lock probe below refuses any other writer.
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&format!("sqlite:{db_path}?mode=rwc"))
            .await
            .map_err(|error| format!("cannot open target database: {error}"))?;
        droppedneedle::schema::apply_migrations(&pool)
            .await
            .map_err(|error| error.to_string())?;
        probe_write_lock(&pool).await?;
        let crypto = if dry_run {
            // Throwaway in-memory key: dry-run re-encrypts into a staged
            // config it discards, so no key file may be minted here.
            Crypto::from_key_bytes(&[42u8; 32]).map_err(|error| error.to_string())?
        } else {
            Crypto::load_or_generate(&config_dir).map_err(|error| error.to_string())?
        };
        Ok::<_, String>(
            run_import(ImportRequest {
                export_bytes,
                passphrase: passphrase.expose().to_owned(),
                pool,
                config_path: config_dir.join("config.json"),
                crypto,
                v2_config_path: v2_config,
                dry_run,
                fault_before_commit: false,
                fault_after_commit: false,
            })
            .await,
        )
    })?;
    println!("{}", report.to_json());
    match report.exit.code {
        ExitCode::Ok | ExitCode::OkWithDrops => Ok(()),
        _ => Err(format!("{verb} failed (see report above)")),
    }
}

/// Refuse a database another writer holds: one connection takes an
/// immediate lock and releases it, so a live server fails the import
/// before anything is decided.
async fn probe_write_lock(pool: &sqlx::SqlitePool) -> Result<(), String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("cannot open target database: {error}"))?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(|_| "target database is locked (stop the server before importing)".to_owned())?;
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("cannot probe target database: {error}"))?;
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
    let summary = droppedneedle::tooling::restore::restore_backup(
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
