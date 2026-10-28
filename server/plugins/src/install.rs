//! Install and update plugins from GitHub, pinned to an exact commit.
//!
//! An admin pastes a repository URL. With no version in it, the host takes
//! the latest release; a `/releases/tag/<tag>`, `/tree/<ref>` or
//! `/commit/<sha>` URL (or an explicit ref) picks one. Either way the
//! choice resolves to a commit SHA before anything downloads, and the
//! archive is fetched by that SHA, so a moved tag cannot swap the code
//! between the preview and the install. The pin is written next to the
//! code as `.droppedneedle-install.json`, so it travels with the folder.
//!
//! Installing stores code and never runs it: the plugin arrives disabled.
//! The archive must be one top-level folder with `plugin.toml` at its
//! root, with no symlinks, no path tricks, and inside the size caps.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::host::PluginHost;
use super::manifest::{PluginManifest, load_manifest};
use super::models::PluginRefKind;
use super::runtime::BoxFuture;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::http_client::{BodyError, read_capped};

/// Largest install archive (32 MiB).
pub const MAX_PLUGIN_ZIP_BYTES: usize = 32 * 1024 * 1024;
/// Most files in one archive.
pub const MAX_PLUGIN_ZIP_ENTRIES: usize = 2000;
/// Largest single file in an archive (32 MiB).
pub const MAX_PLUGIN_ZIP_FILE_BYTES: usize = 32 * 1024 * 1024;
/// Largest total unpacked size (256 MiB).
pub const MAX_PLUGIN_ZIP_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;
/// File recording where a plugin came from, inside its folder.
pub const INSTALL_RECORD: &str = ".droppedneedle-install.json";
/// Shown with every install preview.
pub const TRUST_WARNING: &str = "Only install plugins you trust. A plugin runs as its own program on your server with the same access to files and the network as DroppedNeedle itself. It is not sandboxed.";

/// GitHub API origin.
pub const API_BASE: &str = "https://api.github.com";
/// GitHub archive download origin.
pub const CODELOAD_BASE: &str = "https://codeload.github.com";

/// Where installs reach GitHub.
#[derive(Debug, Clone)]
pub struct GitHubHosts {
    /// API origin; production uses [`API_BASE`].
    pub api: String,
    /// Archive download origin; production uses [`CODELOAD_BASE`].
    pub codeload: String,
}

/// What GitHub answered for one URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// 200. The body, capped at [`MAX_PLUGIN_ZIP_BYTES`] while streaming.
    Found(Vec<u8>),
    /// 404 and nothing else: no such repository, release, ref or commit.
    NotFound,
    /// 401, or a 403 that is not a rate limit.
    Denied,
    /// 429, or a 403 that carries a rate-limit signal.
    RateLimited {
        /// Seconds until GitHub takes requests again, when it said.
        retry_after: Option<u64>,
    },
    /// Any other status.
    Failed(u16),
}

/// A fetch that got no usable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The body is over [`MAX_PLUGIN_ZIP_BYTES`].
    TooLarge,
    /// The request or the download broke: a fixed kind for the log, never
    /// the transport's own text, which can carry the URL.
    Transport(&'static str),
}

/// Fetch one URL and say what came back.
pub trait ArchiveFetcher: Send + Sync {
    /// GET one URL.
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Fetched, FetchError>>;
}

/// Production fetcher over the shared HTTP client.
pub struct ReqwestFetcher {
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
}

impl ReqwestFetcher {
    /// Wrap a shared HTTP client; `clock` dates GitHub's rate-limit reset.
    pub fn new(http: reqwest::Client, clock: Arc<dyn Clock>) -> Self {
        Self { http, clock }
    }
}

/// What a status other than 200 means, or `None` for a 200 whose body is
/// still to read. Only a 404 is "not found".
///
/// From GitHub's "Rate limits for the REST API" page, read 2026-10-13: an
/// exhausted hourly limit is a 403 or 429 with `x-ratelimit-remaining: 0`
/// and `x-ratelimit-reset` as the Unix second it lifts; a secondary limit
/// is a 403 or 429 with `retry-after` in seconds when GitHub sends it. A
/// secondary limit sent as a bare 403 cannot be told from a refusal
/// without reading the body, so it counts as denied.
fn refusal(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    now: u64,
) -> Option<Fetched> {
    let number = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.trim().parse::<u64>().ok())
    };
    let retry_after = number("retry-after");
    let exhausted = number("x-ratelimit-remaining") == Some(0);
    let limited = status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || (status == reqwest::StatusCode::FORBIDDEN && (exhausted || retry_after.is_some()));
    Some(match status {
        reqwest::StatusCode::OK => return None,
        reqwest::StatusCode::NOT_FOUND => Fetched::NotFound,
        _ if limited => Fetched::RateLimited {
            retry_after: retry_after.or_else(|| {
                number("x-ratelimit-reset")
                    .filter(|_| exhausted)
                    .map(|reset| reset.saturating_sub(now))
            }),
        },
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => Fetched::Denied,
        other => Fetched::Failed(other.as_u16()),
    })
}

impl ArchiveFetcher for ReqwestFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Fetched, FetchError>> {
        Box::pin(async move {
            let transport = |error: reqwest::Error| {
                FetchError::Transport(if error.is_timeout() {
                    "timed out"
                } else if error.is_connect() {
                    "could not connect"
                } else {
                    "request failed"
                })
            };
            let response = self
                .http
                .get(url)
                .header("accept", "application/vnd.github+json")
                .send()
                .await
                .map_err(transport)?;
            let now = u64::try_from(self.clock.now_unix()).unwrap_or(0);
            if let Some(answer) = refusal(response.status(), response.headers(), now) {
                return Ok(answer);
            }
            match read_capped(response, MAX_PLUGIN_ZIP_BYTES).await {
                Ok(body) => Ok(Fetched::Found(body)),
                Err(BodyError::TooLarge) => Err(FetchError::TooLarge),
                Err(error @ BodyError::Transport(_)) => Err(FetchError::Transport(error.kind())),
            }
        })
    }
}

/// One file inside an install archive.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// Slash-separated path inside the archive.
    pub path: String,
    /// Whether the entry is a symlink (always refused).
    pub is_symlink: bool,
    /// File bytes.
    pub data: Vec<u8>,
}

/// Read every file of an install archive: a zip of stored and deflated
/// entries (what GitHub serves). The caps apply while reading, so an
/// archive at the download cap cannot balloon past the unpacked budget.
pub fn unpack_zip(bytes: &[u8]) -> Result<Vec<ArchiveEntry>, String> {
    use std::io::Read as _;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("plugin archive is not a readable zip: {error}"))?;
    if archive.len() > MAX_PLUGIN_ZIP_ENTRIES {
        return Err(format!(
            "plugin archive holds {} entries; the cap is {MAX_PLUGIN_ZIP_ENTRIES}",
            archive.len(),
        ));
    }
    let mut entries = Vec::new();
    let mut total: u64 = 0;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|error| format!("plugin archive entry {index} is unreadable: {error}"))?;
        if file.is_dir() {
            continue;
        }
        match file.compression() {
            zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated => {}
            method => {
                return Err(format!(
                    "plugin archive uses {method:?} compression; only stored and deflated entries install"
                ));
            }
        }
        let name = file
            .name()
            .map_err(|error| {
                format!("plugin archive entry {index} has an unreadable name: {error}")
            })?
            .into_owned();
        if file.enclosed_name().is_none() {
            return Err(format!("plugin archive entry {name:?} escapes its root"));
        }
        let is_symlink = file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000);
        let mut data = Vec::new();
        file.take(MAX_PLUGIN_ZIP_FILE_BYTES as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|error| format!("plugin archive entry is unreadable: {error}"))?;
        if data.len() > MAX_PLUGIN_ZIP_FILE_BYTES {
            return Err(format!(
                "plugin archive entry {name:?} tops the per-file cap"
            ));
        }
        total += data.len() as u64;
        if total > MAX_PLUGIN_ZIP_DECOMPRESSED_BYTES as u64 {
            return Err("plugin archive unpacks past the total cap".to_owned());
        }
        entries.push(ArchiveEntry {
            path: name,
            is_symlink,
            data,
        });
    }
    Ok(entries)
}

/// Every way an install can fail. `Denied`, `RateLimited` and `Download`
/// are GitHub's fault and `Io` is ours; the rest are user-facing 400s with
/// the message below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// Not a public GitHub repository URL.
    InvalidUrl,
    /// GitHub refused the request (401, or a 403 that is not a rate limit).
    Denied,
    /// GitHub is rate limiting this server.
    RateLimited {
        /// Seconds until GitHub takes requests again, when it said.
        retry_after: Option<u64>,
    },
    /// GitHub did not answer, or answered with a failure.
    Download,
    /// GitHub answered with a body that does not decode.
    Unreadable,
    /// No public repository at that address.
    NoRepository,
    /// The repository has no such release, tag, branch or commit.
    RefNotFound(String),
    /// Archive over the cap.
    TooLarge,
    /// Too many archived files.
    TooManyFiles,
    /// One archived file over the cap.
    OversizedFile,
    /// Traversal or absolute paths inside the archive.
    UnsafePaths,
    /// Symlinks inside the archive.
    Symlinks,
    /// Archive layout is not one top-level folder.
    Layout,
    /// No `plugin.toml` at the repository root.
    NoManifest,
    /// The manifest failed validation.
    InvalidManifest(String),
    /// The archive did not unpack.
    ArchiveUnreadable(String),
    /// The plugin was not installed from GitHub, so there is nothing to
    /// update from.
    NoSource,
    /// An update of a release install found no releases left.
    NoReleasesNow,
    /// The repository at the recorded address has another GitHub id now:
    /// it was deleted and created again, maybe by someone else.
    RepositoryReplaced,
    /// Local filesystem failure. The message is log-only.
    Io(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl => f.write_str(
                "Enter a public GitHub repository URL, e.g. https://github.com/owner/repo",
            ),
            Self::Denied => f.write_str("GitHub refused the request"),
            Self::RateLimited { .. } => {
                f.write_str("GitHub is rate limiting this server, try again later")
            }
            Self::Download => f.write_str("GitHub could not be reached"),
            Self::Unreadable => f.write_str("GitHub sent an answer DroppedNeedle could not read"),
            Self::NoRepository => f.write_str("GitHub has no public repository at that address"),
            Self::RefNotFound(what) => write!(f, "GitHub has no {what} in that repository"),
            Self::TooLarge => f.write_str("That repository is too large to install as a plugin"),
            Self::TooManyFiles => f.write_str("That repository has too many files"),
            Self::OversizedFile => f.write_str("That repository contains an oversized file"),
            Self::UnsafePaths => f.write_str("The archive contains unsafe paths"),
            Self::Symlinks => f.write_str("The archive contains symlinks"),
            Self::Layout => f.write_str("Unexpected archive layout"),
            Self::NoManifest => f.write_str(
                "No plugin.toml at the repository root, so this is not a DroppedNeedle plugin",
            ),
            Self::InvalidManifest(reason) | Self::ArchiveUnreadable(reason) | Self::Io(reason) => {
                f.write_str(reason)
            }
            Self::NoSource => f.write_str(
                "This plugin was not installed from GitHub, so there is nothing to update from",
            ),
            Self::NoReleasesNow => f.write_str(
                "This repository has no releases now. Pick a branch, tag or commit to install",
            ),
            Self::RepositoryReplaced => f.write_str(
                "The repository at this address was replaced since the plugin was installed. Install it again if you trust the new one",
            ),
        }
    }
}

impl std::error::Error for InstallError {}

/// Where a plugin came from: written beside its code at install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecord {
    /// The URL the admin gave.
    pub repository_url: String,
    /// `owner/repo`.
    pub repository: String,
    /// GitHub's id for the repository, which a rename keeps and a
    /// re-created repository does not. `None` on records written before it
    /// was kept: the next update fills it.
    #[serde(default)]
    pub repository_id: Option<u64>,
    /// What the admin asked for.
    pub ref_kind: PluginRefKind,
    /// The release tag, tag, branch or commit asked for.
    pub reference: String,
    /// The exact commit installed.
    pub commit: String,
    /// Unix seconds.
    pub installed_at: i64,
}

impl InstallRecord {
    /// Read the record in one plugin folder, when there is one.
    pub fn read(plugin_dir: &Path) -> Option<Self> {
        let path = plugin_dir.join(INSTALL_RECORD);
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes)
            .inspect_err(|error| {
                tracing::warn!(path = %path.display(), %error, "install record is unreadable");
            })
            .ok()
    }

    /// What an update fetches. A reference the admin asked for wins;
    /// otherwise releases move to the newest release, branches (the
    /// default one included) to their tip, and tags and commits stay on the
    /// installed commit. An update never changes the kind of install by
    /// itself: a release install with no releases left stops, and never
    /// falls back to the default branch.
    pub fn update_source(&self, asked: Option<&str>) -> Result<InstallSource, InstallError> {
        let url = format!("https://github.com/{}", self.repository);
        let asked = asked
            .map(str::trim)
            .filter(|reference| !reference.is_empty());
        let reference = match (asked, self.ref_kind) {
            (Some(reference), _) => reference,
            (None, PluginRefKind::Release) => {
                let mut source = parse_source(&url, None)?;
                source.wanted = Wanted::NewestRelease;
                return Ok(source);
            }
            (None, PluginRefKind::Branch) => &self.reference,
            (None, PluginRefKind::Tag | PluginRefKind::Commit) => &self.commit,
        };
        parse_source(&url, Some(reference))
    }

    /// Refuse an update whose repository is not the one installed from.
    /// A record without an id cannot tell, so it lets the update through.
    pub fn same_repository(&self, resolved: &ResolvedSource) -> Result<(), InstallError> {
        match self.repository_id {
            Some(id) if id != resolved.repository_id => Err(InstallError::RepositoryReplaced),
            _ => Ok(()),
        }
    }

    /// Write through a synced temp file beside the record, then rename it
    /// over the record, so a crash leaves the old record or the new one,
    /// never a partial one.
    fn write(&self, plugin_dir: &Path) -> Result<(), InstallError> {
        use std::io::Write as _;

        let json =
            serde_json::to_vec_pretty(self).map_err(|error| InstallError::Io(error.to_string()))?;
        let temp = plugin_dir.join(format!(
            "{INSTALL_RECORD}.{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let mut file = std::fs::File::create_new(&temp)
            .map_err(|error| InstallError::Io(error.to_string()))?;
        let synced = file.write_all(&json).and_then(|()| file.sync_all());
        drop(file);
        let swapped = synced.and_then(|()| std::fs::rename(&temp, plugin_dir.join(INSTALL_RECORD)));
        if let Err(error) = swapped {
            let _ = std::fs::remove_file(&temp);
            return Err(InstallError::Io(error.to_string()));
        }
        Ok(())
    }
}

/// A repository plus the version to install, before resolving to a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSource {
    /// The URL as given.
    pub url: String,
    /// Repository owner.
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// What to install.
    pub wanted: Wanted,
}

/// What the admin asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// The newest release (or the default branch when there are none).
    LatestRelease,
    /// The newest release and nothing else.
    NewestRelease,
    /// One release tag.
    Tag(String),
    /// A branch, tag or commit named in a `/tree/` URL.
    Tree(String),
    /// One commit.
    Commit(String),
}

/// A source resolved to an exact commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    /// The source.
    pub source: InstallSource,
    /// GitHub's id for the repository.
    pub repository_id: u64,
    /// What the reference is.
    pub ref_kind: PluginRefKind,
    /// The tag, branch or commit.
    pub reference: String,
    /// The exact commit.
    pub commit: String,
}

/// A downloaded, validated plugin that is not installed yet.
#[derive(Debug, Clone)]
pub struct StagedPlugin {
    /// Where it came from.
    pub resolved: ResolvedSource,
    /// Its manifest.
    pub manifest: PluginManifest,
    /// Archive entries.
    entries: Vec<ArchiveEntry>,
    root: String,
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment.contains("..")
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn valid_ref(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 200
        && !text.contains("..")
        && !text.starts_with('/')
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'))
}

fn is_commit_sha(text: &str) -> bool {
    text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// Parse a GitHub repository URL. `reference` (a tag, branch or commit)
/// overrides any version in the URL.
pub fn parse_source(url: &str, reference: Option<&str>) -> Result<InstallSource, InstallError> {
    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("https://github.com/")
        .ok_or(InstallError::InvalidUrl)?;
    let rest = rest.trim_end_matches('/');
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next().unwrap_or_default().to_owned();
    let repo = parts.next().unwrap_or_default();
    let repo = repo.strip_suffix(".git").unwrap_or(repo).to_owned();
    if !valid_segment(&owner) || !valid_segment(&repo) {
        return Err(InstallError::InvalidUrl);
    }
    let tail = parts.next().unwrap_or_default();
    let mut wanted = if tail.is_empty() {
        Wanted::LatestRelease
    } else if let Some(tag) = tail.strip_prefix("releases/tag/") {
        Wanted::Tag(tag.to_owned())
    } else if let Some(tree) = tail.strip_prefix("tree/") {
        Wanted::Tree(tree.to_owned())
    } else if let Some(commit) = tail.strip_prefix("commit/") {
        Wanted::Commit(commit.to_owned())
    } else if tail == "releases/latest" || tail == "releases" {
        Wanted::LatestRelease
    } else {
        return Err(InstallError::InvalidUrl);
    };
    if let Some(reference) = reference.map(str::trim).filter(|text| !text.is_empty()) {
        wanted = if is_commit_sha(reference) {
            Wanted::Commit(reference.to_owned())
        } else {
            Wanted::Tree(reference.to_owned())
        };
    }
    let reference_ok = match &wanted {
        Wanted::LatestRelease | Wanted::NewestRelease => true,
        Wanted::Tag(text) | Wanted::Tree(text) => valid_ref(text),
        Wanted::Commit(text) => is_commit_sha(text),
    };
    if !reference_ok {
        return Err(InstallError::InvalidUrl);
    }
    Ok(InstallSource {
        url: trimmed.to_owned(),
        owner,
        repo,
        wanted,
    })
}

/// `GET /repos/{owner}/{repo}`, of which only the id is read. Probed
/// 2026-10-27 against the public API: `id` is a JSON integer, and an
/// unknown repository answers 404.
#[derive(Deserialize)]
struct Repository {
    id: u64,
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
}

#[derive(Deserialize)]
struct CommitInfo {
    sha: String,
}

/// The body behind one URL, or `None` when GitHub has nothing there. Only
/// a 404 is "nothing there": a refusal, a rate limit or a failure is an
/// error, so it can never be mistaken for a repository with no releases.
fn body_of(answer: Result<Fetched, FetchError>) -> Result<Option<Vec<u8>>, InstallError> {
    match answer {
        Ok(Fetched::Found(body)) => Ok(Some(body)),
        Ok(Fetched::NotFound) => Ok(None),
        Ok(Fetched::Denied) => Err(InstallError::Denied),
        Ok(Fetched::RateLimited { retry_after }) => Err(InstallError::RateLimited { retry_after }),
        Ok(Fetched::Failed(status)) => {
            tracing::warn!(status, "GitHub failed a plugin request");
            Err(InstallError::Download)
        }
        Err(FetchError::TooLarge) => Err(InstallError::TooLarge),
        Err(FetchError::Transport(reason)) => {
            tracing::warn!(%reason, "plugin download failed");
            Err(InstallError::Download)
        }
    }
}

/// Decode one GitHub answer, or say it could not be read.
fn decode<T: serde::de::DeserializeOwned>(body: &[u8], what: &str) -> Result<T, InstallError> {
    serde_json::from_slice(body).map_err(|error| {
        tracing::warn!(what, %error, "GitHub sent an unreadable answer to a plugin request");
        InstallError::Unreadable
    })
}

/// Resolve a commit-ish (tag, branch or SHA) to a commit SHA.
async fn commit_for(
    fetcher: &dyn ArchiveFetcher,
    hosts: &GitHubHosts,
    source: &InstallSource,
    reference: &str,
) -> Result<Option<String>, InstallError> {
    let url = format!(
        "{}/repos/{}/{}/commits/{reference}",
        hosts.api, source.owner, source.repo
    );
    let answer = fetcher.fetch(&url).await;
    // This endpoint answers 422, not 404, for a tag, branch or commit the
    // repository does not have (probed 2026-10-13 against the public API),
    // and 409 for a repository with no commits (GitHub's "Get a commit"
    // page). Both mean "no such ref" here and nowhere else.
    if matches!(answer, Ok(Fetched::Failed(409 | 422))) {
        return Ok(None);
    }
    let Some(body) = body_of(answer)? else {
        return Ok(None);
    };
    let info: CommitInfo = decode(&body, "commit")?;
    if !is_commit_sha(&info.sha) {
        tracing::warn!("GitHub named a commit that is not a SHA");
        return Err(InstallError::Unreadable);
    }
    Ok(Some(info.sha.to_ascii_lowercase()))
}

/// Resolve what the admin asked for to an exact commit.
pub async fn resolve(
    fetcher: &dyn ArchiveFetcher,
    hosts: &GitHubHosts,
    source: InstallSource,
) -> Result<ResolvedSource, InstallError> {
    let url = format!("{}/repos/{}/{}", hosts.api, source.owner, source.repo);
    let repository: Repository = match body_of(fetcher.fetch(&url).await)? {
        Some(body) => decode(&body, "repository")?,
        None => return Err(InstallError::NoRepository),
    };
    let (ref_kind, reference) = match &source.wanted {
        Wanted::LatestRelease | Wanted::NewestRelease => {
            let url = format!(
                "{}/repos/{}/{}/releases/latest",
                hosts.api, source.owner, source.repo
            );
            match body_of(fetcher.fetch(&url).await)? {
                Some(body) => {
                    let release: LatestRelease = decode(&body, "latest release")?;
                    if !valid_ref(&release.tag_name) {
                        tracing::warn!("GitHub named a release tag that is not a usable ref");
                        return Err(InstallError::Unreadable);
                    }
                    (PluginRefKind::Release, release.tag_name)
                }
                None if source.wanted == Wanted::NewestRelease => {
                    return Err(InstallError::NoReleasesNow);
                }
                // No releases: take the default branch as it is now.
                None => (PluginRefKind::Branch, "HEAD".to_owned()),
            }
        }
        Wanted::Tag(tag) => (PluginRefKind::Tag, tag.clone()),
        Wanted::Tree(reference) => (PluginRefKind::Branch, reference.clone()),
        Wanted::Commit(sha) => (PluginRefKind::Commit, sha.to_ascii_lowercase()),
    };
    let commit = commit_for(fetcher, hosts, &source, &reference)
        .await?
        .ok_or_else(|| {
            InstallError::RefNotFound(match ref_kind {
                PluginRefKind::Branch if reference == "HEAD" => "default branch".to_owned(),
                PluginRefKind::Commit => format!("commit {reference}"),
                _ => format!("release, tag or branch named {reference}"),
            })
        })?;
    // GitHub's answer to a commit pin must start with the SHA asked, which
    // for a full SHA means equal.
    if ref_kind == PluginRefKind::Commit && !commit.starts_with(&reference) {
        tracing::warn!(
            asked = %reference,
            answered = %commit,
            "GitHub answered a pinned commit with another commit"
        );
        return Err(InstallError::Unreadable);
    }
    Ok(ResolvedSource {
        source,
        repository_id: repository.id,
        ref_kind,
        reference,
        commit,
    })
}

/// Download and validate one resolved source without installing it.
pub async fn stage(
    fetcher: &dyn ArchiveFetcher,
    hosts: &GitHubHosts,
    resolved: ResolvedSource,
) -> Result<StagedPlugin, InstallError> {
    let url = format!(
        "{}/{}/{}/zip/{}",
        hosts.codeload, resolved.source.owner, resolved.source.repo, resolved.commit
    );
    let archive = body_of(fetcher.fetch(&url).await)?
        .ok_or_else(|| InstallError::RefNotFound(format!("commit {}", resolved.commit)))?;
    if archive.len() > MAX_PLUGIN_ZIP_BYTES {
        return Err(InstallError::TooLarge);
    }
    let entries = unpack_zip(&archive).map_err(InstallError::ArchiveUnreadable)?;
    let root = check_entries(&entries)?;
    let manifest = manifest_from_entries(&entries, &root)?;
    Ok(StagedPlugin {
        resolved,
        manifest,
        entries,
        root,
    })
}

/// Check archive shape and safety; returns the single top-level folder.
fn check_entries(entries: &[ArchiveEntry]) -> Result<String, InstallError> {
    if entries.is_empty() {
        return Err(InstallError::Layout);
    }
    if entries.len() > MAX_PLUGIN_ZIP_ENTRIES {
        return Err(InstallError::TooManyFiles);
    }
    let mut total = 0usize;
    let mut roots: HashSet<&str> = HashSet::new();
    for entry in entries {
        if entry.is_symlink {
            return Err(InstallError::Symlinks);
        }
        if entry.data.len() > MAX_PLUGIN_ZIP_FILE_BYTES {
            return Err(InstallError::OversizedFile);
        }
        total += entry.data.len();
        if total > MAX_PLUGIN_ZIP_DECOMPRESSED_BYTES {
            return Err(InstallError::TooLarge);
        }
        if entry.path.starts_with('/') || entry.path.contains('\\') {
            return Err(InstallError::UnsafePaths);
        }
        let mut parts = entry.path.split('/');
        roots.insert(parts.next().unwrap_or_default());
        if parts.any(|part| part.is_empty() || part == "..") && !entry.path.ends_with('/') {
            return Err(InstallError::UnsafePaths);
        }
    }
    let mut roots = roots.into_iter();
    match (roots.next(), roots.next()) {
        (Some(root), None) if !root.is_empty() && root != ".." => {
            if !entries
                .iter()
                .any(|entry| entry.path == format!("{root}/plugin.toml"))
            {
                return Err(InstallError::NoManifest);
            }
            Ok(root.to_owned())
        }
        _ => Err(InstallError::Layout),
    }
}

/// Validate the manifest straight from the archive, through the same rules
/// a folder on disk gets.
fn manifest_from_entries(
    entries: &[ArchiveEntry],
    root: &str,
) -> Result<PluginManifest, InstallError> {
    let path = format!("{root}/plugin.toml");
    let entry = entries
        .iter()
        .find(|entry| entry.path == path)
        .ok_or(InstallError::NoManifest)?;
    super::manifest::parse_manifest(root, &entry.data)
        .map_err(|error| InstallError::InvalidManifest(error.to_string()))
}

impl PluginHost {
    /// Write a staged plugin into the plugins directory, replacing any
    /// older copy in one swap, and reload. Settings live in config and the
    /// data folder stays put, so an update keeps both. The install record
    /// says it was installed at `installed_at` (unix seconds).
    pub fn install_staged(
        &self,
        staged: &StagedPlugin,
        installed_at: i64,
    ) -> Result<String, InstallError> {
        let name = staged.manifest.name.clone();
        let staging = self.dir().join(format!(".installing-{name}"));
        let _ = std::fs::remove_dir_all(&staging);
        let written = write_entries(&staging, &staged.root, &staged.entries).and_then(|()| {
            InstallRecord {
                repository_url: staged.resolved.source.url.clone(),
                repository: format!(
                    "{}/{}",
                    staged.resolved.source.owner, staged.resolved.source.repo
                ),
                repository_id: Some(staged.resolved.repository_id),
                ref_kind: staged.resolved.ref_kind,
                reference: staged.resolved.reference.clone(),
                commit: staged.resolved.commit.clone(),
                installed_at,
            }
            .write(&staging)
        });
        let checked = written.and_then(|()| {
            let manifest = load_manifest(&staging)
                .map_err(|error| InstallError::InvalidManifest(error.to_string()))?;
            if manifest.name != name {
                return Err(InstallError::InvalidManifest(
                    "plugin.toml changed while installing".to_owned(),
                ));
            }
            swap_into_place(self.dir(), &staging, &name).map_err(InstallError::Io)
        });
        if let Err(error) = checked {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        self.load_all();
        tracing::info!(
            plugin = %name,
            commit = %staged.resolved.commit,
            reference = %staged.resolved.reference,
            "plugin installed"
        );
        Ok(name)
    }

    /// Where one installed plugin came from, when it came from GitHub.
    pub fn install_record(&self, name: &str) -> Option<InstallRecord> {
        let plugin = self.get(name)?;
        InstallRecord::read(Path::new(&plugin.directory))
    }

    /// Fill in the repository id on a record written before ids were
    /// kept, after an update that found nothing new to install.
    pub fn keep_repository_id(&self, name: &str, repository_id: u64) -> Result<(), InstallError> {
        let Some(plugin) = self.get(name) else {
            return Ok(());
        };
        let dir = Path::new(&plugin.directory);
        match InstallRecord::read(dir) {
            Some(record) if record.repository_id.is_none() => InstallRecord {
                repository_id: Some(repository_id),
                ..record
            }
            .write(dir),
            _ => Ok(()),
        }
    }
}

fn write_entries(staging: &Path, root: &str, entries: &[ArchiveEntry]) -> Result<(), InstallError> {
    let prefix = format!("{root}/");
    for entry in entries {
        let Some(remainder) = entry.path.strip_prefix(&prefix) else {
            continue;
        };
        if remainder.is_empty() || remainder.ends_with('/') {
            continue;
        }
        let mut target = staging.to_path_buf();
        for part in remainder.split('/') {
            if part.is_empty() || part == ".." || part == "." {
                return Err(InstallError::UnsafePaths);
            }
            target.push(part);
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| InstallError::Io(error.to_string()))?;
        }
        std::fs::write(&target, &entry.data)
            .map_err(|error| InstallError::Io(error.to_string()))?;
        // Keep executables runnable for plugins that ship a program.
        #[cfg(unix)]
        if entry.data.starts_with(b"#!") || remainder.starts_with("bin/") {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
        }
    }
    Ok(())
}

/// Two renames, never a gap: the live folder steps aside first, so a crash
/// mid-swap leaves the old or the new code in place, never neither.
fn swap_into_place(dir: &Path, staging: &Path, name: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let placed = dir.join(name);
    if placed.exists() {
        let backup = dir.join(format!(".swap-backup-{name}"));
        let _ = std::fs::remove_dir_all(&backup);
        std::fs::rename(&placed, &backup).map_err(|error| error.to_string())?;
        if let Err(error) = std::fs::rename(staging, &placed) {
            let _ = std::fs::rename(&backup, &placed);
            return Err(error.to_string());
        }
        let _ = std::fs::remove_dir_all(&backup);
        return Ok(());
    }
    std::fs::rename(staging, &placed).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_parse_into_what_to_install() {
        let latest = parse_source("https://github.com/acme/toy.git/", None).unwrap();
        assert_eq!(
            (latest.owner.as_str(), latest.repo.as_str()),
            ("acme", "toy")
        );
        assert_eq!(latest.wanted, Wanted::LatestRelease);
        let tag = parse_source("https://github.com/acme/toy/releases/tag/v1.2.0", None).unwrap();
        assert_eq!(tag.wanted, Wanted::Tag("v1.2.0".to_owned()));
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let pinned = parse_source("https://github.com/acme/toy", Some(sha)).unwrap();
        assert_eq!(pinned.wanted, Wanted::Commit(sha.to_owned()));
        for bad in [
            "http://github.com/acme/toy",
            "https://gitlab.com/acme/toy",
            "https://github.com/acme",
            "https://github.com/acme/../x",
            "https://github.com/acme/toy/tree/../../x",
            "https://github.com/acme/toy/commit/nothex",
            "https://github.com/acme/toy/issues",
        ] {
            assert_eq!(
                parse_source(bad, None),
                Err(InstallError::InvalidUrl),
                "{bad}"
            );
        }
    }

    #[test]
    fn only_a_404_reads_as_not_found() {
        use reqwest::StatusCode;
        use reqwest::header::{HeaderMap, HeaderValue};

        let sorted = |status: u16, pairs: &[(&'static str, &'static str)]| {
            let mut headers = HeaderMap::new();
            for (name, value) in pairs {
                headers.insert(*name, HeaderValue::from_static(value));
            }
            refusal(StatusCode::from_u16(status).unwrap(), &headers, 1000)
        };
        let limited = |retry_after| Some(Fetched::RateLimited { retry_after });
        let exhausted = [
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1600"),
        ];
        assert_eq!(sorted(200, &[]), None);
        assert_eq!(sorted(404, &[]), Some(Fetched::NotFound));
        assert_eq!(sorted(401, &[]), Some(Fetched::Denied));
        assert_eq!(sorted(403, &[]), Some(Fetched::Denied));
        assert_eq!(sorted(403, &exhausted), limited(Some(600)));
        assert_eq!(sorted(403, &[("retry-after", "30")]), limited(Some(30)));
        assert_eq!(sorted(429, &[]), limited(None));
        assert_eq!(sorted(422, &[]), Some(Fetched::Failed(422)));
        assert_eq!(sorted(500, &[]), Some(Fetched::Failed(500)));
    }

    #[test]
    fn archives_need_one_root_with_a_manifest_and_no_tricks() {
        let file = |path: &str| ArchiveEntry {
            path: path.to_owned(),
            is_symlink: false,
            data: b"x".to_vec(),
        };
        assert_eq!(
            check_entries(&[file("toy-abc/plugin.toml"), file("toy-abc/plugin.py")]),
            Ok("toy-abc".to_owned())
        );
        assert_eq!(
            check_entries(&[file("a/plugin.toml"), file("b/x")]),
            Err(InstallError::Layout)
        );
        assert_eq!(
            check_entries(&[file("a/plugin.py")]),
            Err(InstallError::NoManifest)
        );
        assert_eq!(
            check_entries(&[file("a/plugin.toml"), file("a/../../etc/passwd")]),
            Err(InstallError::UnsafePaths)
        );
        let mut link = file("a/link");
        link.is_symlink = true;
        assert_eq!(
            check_entries(&[file("a/plugin.toml"), link]),
            Err(InstallError::Symlinks)
        );
    }

    #[test]
    fn zip_reader_round_trips_and_refuses_escapes() {
        use std::io::Write as _;

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("root/plugin.toml", deflated).unwrap();
        writer.write_all(b"[plugin]").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let entries = unpack_zip(&bytes).unwrap();
        assert_eq!(entries[0].path, "root/plugin.toml");
        assert_eq!(entries[0].data, b"[plugin]");
        assert!(unpack_zip(b"not a zip").is_err());

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("../escape.toml", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"[plugin]").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(unpack_zip(&bytes).is_err());
    }
}
