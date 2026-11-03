//! The upgrade tool as a child process: `droppedneedle-tool upgrade` with
//! `--progress json`, read line by line through the types it writes them
//! with.
//!
//! The child gets a cleared environment: `PATH`, the proxy, certificate and
//! time zone variables plugins also get, and the data-folder settings the
//! server resolved. It runs in its own process group and is killed when its
//! handle drops, so a server that stops mid-run leaves an interrupted
//! upgrade, which the next start offers to finish or undo. Its stderr goes
//! to the log as it comes. Its exit status decides how the run ended: a
//! `result` line counts only on a clean exit, and a run that ends with no
//! line it can stand on is a failure with the status named.

use std::{
    ffi::OsString,
    path::PathBuf,
    process::{ExitStatus, Stdio},
};

use tokio::io::{AsyncBufReadExt as _, AsyncRead, BufReader};

use droppedneedle_kernel::config::AppConfig;

use crate::bootstrap::upgrade_progress::{
    CheckResult, Code, Event, FailedLine, Finished, Line, V2Files,
};

/// The tool's binary name, installed beside the server's.
const TOOL_NAME: &str = "droppedneedle-tool";

/// The variables passed through from the server's own environment, the
/// list plugins get (`plugins/src/process.rs`).
const PASSED_THROUGH: &[&str] = &[
    "TZ",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
];

/// Longest stdout line read. The longest real one is a `check` or `result`
/// line; anything past this is dropped with a warning.
const LONGEST_LINE: usize = 1024 * 1024;

/// Longest stderr line logged; the rest of a longer one is cut.
const LONGEST_LOG_LINE: usize = 8 * 1024;

/// How to start the tool. Built once: production finds it beside the
/// server's binary, tests run a script in its place.
#[derive(Debug, Clone)]
pub struct ToolCommand {
    program: Option<PathBuf>,
    leading: Vec<OsString>,
}

impl ToolCommand {
    /// The tool installed beside this server's binary, never one found on
    /// `PATH`.
    #[must_use]
    pub fn installed() -> Self {
        Self {
            program: None,
            leading: Vec::new(),
        }
    }

    /// `program` with `leading` arguments before the upgrade's own.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>, leading: Vec<OsString>) -> Self {
        Self {
            program: Some(program.into()),
            leading,
        }
    }

    fn program(&self) -> std::io::Result<PathBuf> {
        match &self.program {
            Some(program) => Ok(program.clone()),
            None => Ok(std::env::current_exe()?.with_file_name(TOOL_NAME)),
        }
    }
}

/// What a run does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// `upgrade --check`.
    Check,
    /// `upgrade`, with `--callbacks-changed` when the addresses it lists
    /// were acknowledged.
    Upgrade {
        /// Pass `--callbacks-changed`.
        callbacks_changed: bool,
    },
    /// `upgrade --fresh --yes`.
    Fresh,
    /// `upgrade --undo --yes`.
    Undo,
}

impl Task {
    fn args(self) -> Vec<&'static str> {
        let mut args = vec!["upgrade"];
        match self {
            Self::Check => args.push("--check"),
            Self::Upgrade { callbacks_changed } => {
                if callbacks_changed {
                    args.push("--callbacks-changed");
                }
            }
            Self::Fresh => args.extend(["--fresh", "--yes"]),
            Self::Undo => args.extend(["--undo", "--yes"]),
        }
        args.extend(["--progress", "json"]);
        args
    }
}

/// How a run ended, as its last line and exit status say.
#[derive(Debug, Clone)]
pub enum Ending {
    /// The check's findings.
    Checked(CheckResult),
    /// The run finished.
    Finished(Finished),
    /// The run failed, or stopped with no line to stand on.
    Failed(FailedLine),
}

/// The environment the tool runs with.
#[derive(Debug, Clone)]
pub struct ChildEnv(Vec<(OsString, OsString)>);

impl ChildEnv {
    /// `PATH` and [`PASSED_THROUGH`] from this process, and the data
    /// folders, base path, log filter and temporary folder `config`
    /// resolved, so the tool finds what the server found.
    #[must_use]
    pub fn for_config(config: &AppConfig) -> Self {
        let mut env: Vec<(OsString, OsString)> = Vec::new();
        for name in std::iter::once("PATH").chain(PASSED_THROUGH.iter().copied()) {
            if let Some(value) = std::env::var_os(name) {
                env.push((name.into(), value));
            }
        }
        for (name, value) in [
            ("ROOT_APP_DIR", config.root_app_dir.as_os_str()),
            ("CONFIG_DIR", config.config_dir.as_os_str()),
            ("CACHE_DIR", config.cache_dir.as_os_str()),
            ("LIBRARY_DB_PATH", config.library_db_path.as_os_str()),
            ("TMPDIR", config.temp_dir.as_os_str()),
        ] {
            env.push((name.into(), value.to_owned()));
        }
        env.push(("BASE_PATH".into(), config.base_path.clone().into()));
        env.push(("RUST_LOG".into(), config.log_filter.clone().into()));
        Self(env)
    }
}

/// A `step` or `progress` line as it comes.
pub type OnProgress<'a> = &'a (dyn Fn(Event) + Send + Sync);

/// Run `task` to its end, or until `stop` fires, which kills the child.
/// Its `step` and `progress` lines go to `progress` as they come.
pub async fn run(
    tool: &ToolCommand,
    env: &ChildEnv,
    task: Task,
    progress: OnProgress<'_>,
    stop: &droppedneedle_kernel::jobs::CancellationToken,
) -> Ending {
    let program = match tool.program() {
        Ok(program) => program,
        Err(error) => return started_nothing(&format!("cannot find the upgrade tool: {error}")),
    };
    let mut command = tokio::process::Command::new(&program);
    command
        .args(&tool.leading)
        .args(task.args())
        .env_clear()
        .envs(env.0.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Its own group, so a signal meant for the server's (a Ctrl-C in a
    // terminal) does not reach it: the server decides when it stops.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return started_nothing(&format!("cannot start {}: {error}", program.display()));
        }
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return started_nothing("the upgrade tool's output could not be read");
    };
    let reading = async { tokio::join!(read_lines(stdout, progress), log_lines(stderr)).0 };
    let mut reading = std::pin::pin!(reading);
    let (mut last, mut read, mut stopping, mut killed) = (None, false, false, false);
    let status = loop {
        tokio::select! {
            got = &mut reading, if !read => {
                last = got;
                read = true;
            }
            status = child.wait(), if read => break status,
            () = stop.cancelled(), if !stopping => stopping = true,
        }
        if stopping && !killed {
            killed = true;
            tracing::warn!("the server is stopping; the upgrade tool is stopped mid-run");
            if let Err(error) = child.start_kill() {
                tracing::warn!(%error, "cannot stop the upgrade tool");
            }
        }
    };
    judge(task, last, status)
}

/// What `last` and the exit status say about the run. The status decides:
/// a `check` line counts at the check's own exits (0, 1 for a blocker, 3
/// for addresses to acknowledge), a `result` line only at 0, and a `failed`
/// line only at a failing exit.
fn judge(task: Task, last: Option<Event>, status: std::io::Result<ExitStatus>) -> Ending {
    let status = match status {
        Ok(status) => status,
        Err(error) => return stopped_without_line(&format!("its exit was not seen: {error}")),
    };
    match (task, last, status.code()) {
        (Task::Check, Some(Event::Check(check)), Some(0 | 1 | 3)) => Ending::Checked(check),
        (Task::Upgrade { .. } | Task::Fresh | Task::Undo, Some(Event::Result(result)), Some(0)) => {
            Ending::Finished(result)
        }
        (_, Some(Event::Failed(failed)), Some(code)) if code != 0 => Ending::Failed(failed),
        _ => stopped_without_line(&format!("it ended ({status}) without saying how")),
    }
}

/// A run that ended with no line to stand on. Where the v2 files are is
/// worked out afterwards from the folders. `how` goes to the log only.
pub(super) fn stopped_without_line(how: &str) -> Ending {
    tracing::warn!(how, "the upgrade tool stopped without a result");
    Ending::Failed(FailedLine {
        code: Code::Internal,
        message: STOPPED.to_owned(),
        v2_files: V2Files::Untouched,
    })
}

/// A run whose tool never started: nothing moved. `why` goes to the log
/// only.
fn started_nothing(why: &str) -> Ending {
    tracing::error!(why, "the upgrade tool did not start");
    Ending::Failed(FailedLine {
        code: Code::Internal,
        message: NOT_STARTED.to_owned(),
        v2_files: V2Files::Untouched,
    })
}

/// The message of a run that stopped without saying how. Its cause, which
/// can name paths, is in the server log.
const STOPPED: &str =
    "The upgrade tool stopped without saying how it ended. The server log says why.";

/// The message of a run whose tool did not start.
const NOT_STARTED: &str = "The upgrade tool could not start. The server log says why.";

/// Read the child's progress lines, report steps as they come, and keep the
/// last line that ends a run.
async fn read_lines(stdout: impl AsyncRead + Unpin, progress: OnProgress<'_>) -> Option<Event> {
    let mut reader = BufReader::new(stdout);
    let mut last = None;
    let mut line = Vec::new();
    loop {
        match next_line(&mut reader, &mut line, LONGEST_LINE).await {
            Ok(Read::End) => return last,
            Ok(Read::TooLong) => {
                tracing::warn!("an upgrade progress line was too long and was dropped");
            }
            // `Line` parses only at version 1 (`Version`): a line of
            // another version, or none, is dropped here.
            Ok(Read::Line) => match serde_json::from_slice::<Line>(&line) {
                Ok(Line { event, .. }) => match event {
                    Event::Step(_) | Event::Progress(_) => progress(event),
                    ending => last = Some(ending),
                },
                Err(error) => {
                    tracing::warn!(%error, "an upgrade progress line did not read");
                }
            },
            Err(error) => {
                tracing::warn!(%error, "cannot read the upgrade tool's progress");
                return last;
            }
        }
    }
}

/// Copy the child's stderr into the log, line by line.
async fn log_lines(stderr: impl AsyncRead + Unpin) {
    let mut reader = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        match next_line(&mut reader, &mut line, LONGEST_LOG_LINE).await {
            Ok(Read::End) => return,
            Ok(Read::Line | Read::TooLong) => {
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end();
                if !text.is_empty() {
                    tracing::info!(target: "droppedneedle::upgrade_tool", "{text}");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "cannot read the upgrade tool's messages");
                return;
            }
        }
    }
}

/// What [`next_line`] read.
enum Read {
    /// A whole line, without its newline.
    Line,
    /// A line longer than the limit; `line` holds its start.
    TooLong,
    /// The end of the output.
    End,
}

/// Read one line into `line`, keeping at most `limit` bytes of it, so a
/// child that never writes a newline cannot grow the buffer without bound.
async fn next_line(
    reader: &mut BufReader<impl AsyncRead + Unpin>,
    line: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<Read> {
    line.clear();
    let mut read_any = false;
    let mut too_long = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(match (read_any, too_long) {
                (false, _) => Read::End,
                (true, true) => Read::TooLong,
                (true, false) => Read::Line,
            });
        }
        read_any = true;
        let (part, ended) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&available[..at], Some(at + 1)),
            None => (available, None),
        };
        let room = limit.saturating_sub(line.len());
        if part.len() > room {
            too_long = true;
        }
        line.extend_from_slice(&part[..part.len().min(room)]);
        let used = ended.unwrap_or(available.len());
        reader.consume(used);
        if ended.is_some() {
            return Ok(if too_long { Read::TooLong } else { Read::Line });
        }
    }
}
