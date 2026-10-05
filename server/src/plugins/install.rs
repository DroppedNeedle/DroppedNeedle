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

use serde::{Deserialize, Serialize};

use super::host::PluginHost;
use super::manifest::{PluginManifest, load_manifest};
use super::runtime::BoxFuture;

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

/// GitHub endpoints. Tests point these at a scripted fetcher.
const API_BASE: &str = "https://api.github.com";
const CODELOAD_BASE: &str = "https://codeload.github.com";

/// Fetch one URL. `Ok(None)` means 404 (or any non-200); anything else
/// is the body, capped at [`MAX_PLUGIN_ZIP_BYTES`] while streaming.
pub trait ArchiveFetcher: Send + Sync {
    /// GET one URL.
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>>;
}

/// Production fetcher over the shared HTTP client.
pub struct ReqwestFetcher {
    http: reqwest::Client,
}

impl ReqwestFetcher {
    /// Wrap a shared HTTP client.
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }
}

impl ArchiveFetcher for ReqwestFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(async move {
            let mut response = self
                .http
                .get(url)
                .header("accept", "application/vnd.github+json")
                .send()
                .await
                .map_err(|error| format!("download failed: {error}"))?;
            if response.status() != reqwest::StatusCode::OK {
                return Ok(None);
            }
            if response
                .content_length()
                .is_some_and(|declared| declared > MAX_PLUGIN_ZIP_BYTES as u64)
            {
                return Err("too large".to_owned());
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| format!("download failed: {error}"))?
            {
                body.extend_from_slice(&chunk);
                if body.len() > MAX_PLUGIN_ZIP_BYTES {
                    return Err("too large".to_owned());
                }
            }
            Ok(Some(body))
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

/// Unpack an install archive. Production reads zips ([`ZipUnpacker`]).
pub trait ArchiveUnpacker: Send + Sync {
    /// Every file in the archive.
    fn unpack(&self, bytes: &[u8]) -> Result<Vec<ArchiveEntry>, String>;
}

/// Zip reader over stored and deflated entries (what GitHub serves). The
/// caps apply while reading, so an archive at the download cap cannot
/// balloon past the unpacked budget.
#[derive(Debug, Default)]
pub struct ZipUnpacker;

impl ArchiveUnpacker for ZipUnpacker {
    fn unpack(&self, bytes: &[u8]) -> Result<Vec<ArchiveEntry>, String> {
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
            let name = file.name().to_owned();
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
}

/// Every way an install can fail. All but `Io` are user-facing 400s with
/// the message below; `Io` is a 500 with a fixed body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// Not a public GitHub repository URL.
    InvalidUrl,
    /// GitHub did not answer, or the download failed.
    Download,
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
    /// Local filesystem failure. The message is log-only.
    Io(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl => f.write_str(
                "Enter a public GitHub repository URL, e.g. https://github.com/owner/repo",
            ),
            Self::Download => f.write_str(
                "Could not reach GitHub or download that repository. Check the URL is public and try again",
            ),
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
    /// `release`, `tag`, `branch` or `commit`: what the admin asked for.
    pub ref_kind: String,
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
        let bytes = std::fs::read(plugin_dir.join(INSTALL_RECORD)).ok()?;
        serde_json::from_slice(&bytes).ok()
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
    /// `release`, `tag`, `branch` or `commit`.
    pub ref_kind: String,
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
        Wanted::LatestRelease => true,
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

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
}

#[derive(Deserialize)]
struct CommitInfo {
    sha: String,
}

fn fetch_failed(reason: String) -> InstallError {
    if reason.contains("too large") {
        InstallError::TooLarge
    } else {
        tracing::warn!(%reason, "plugin download failed");
        InstallError::Download
    }
}

/// Resolve a commit-ish (tag, branch or SHA) to a commit SHA.
async fn commit_for(
    fetcher: &dyn ArchiveFetcher,
    source: &InstallSource,
    reference: &str,
) -> Result<Option<String>, InstallError> {
    let url = format!(
        "{API_BASE}/repos/{}/{}/commits/{reference}",
        source.owner, source.repo
    );
    let Some(body) = fetcher.fetch(&url).await.map_err(fetch_failed)? else {
        return Ok(None);
    };
    let info: CommitInfo = serde_json::from_slice(&body).map_err(|_| InstallError::Download)?;
    if !is_commit_sha(&info.sha) {
        return Err(InstallError::Download);
    }
    Ok(Some(info.sha.to_ascii_lowercase()))
}

/// Resolve what the admin asked for to an exact commit.
pub async fn resolve(
    fetcher: &dyn ArchiveFetcher,
    source: InstallSource,
) -> Result<ResolvedSource, InstallError> {
    let (ref_kind, reference) = match &source.wanted {
        Wanted::LatestRelease => {
            let url = format!(
                "{API_BASE}/repos/{}/{}/releases/latest",
                source.owner, source.repo
            );
            match fetcher.fetch(&url).await.map_err(fetch_failed)? {
                Some(body) => {
                    let release: LatestRelease =
                        serde_json::from_slice(&body).map_err(|_| InstallError::Download)?;
                    if !valid_ref(&release.tag_name) {
                        return Err(InstallError::Download);
                    }
                    ("release".to_owned(), release.tag_name)
                }
                // No releases: take the default branch as it is now.
                None => ("branch".to_owned(), "HEAD".to_owned()),
            }
        }
        Wanted::Tag(tag) => ("tag".to_owned(), tag.clone()),
        Wanted::Tree(reference) => ("branch".to_owned(), reference.clone()),
        Wanted::Commit(sha) => ("commit".to_owned(), sha.to_ascii_lowercase()),
    };
    let commit = commit_for(fetcher, &source, &reference)
        .await?
        .ok_or_else(|| {
            InstallError::RefNotFound(match ref_kind.as_str() {
                "branch" if reference == "HEAD" => "default branch".to_owned(),
                "commit" => format!("commit {reference}"),
                _ => format!("release, tag or branch named {reference}"),
            })
        })?;
    Ok(ResolvedSource {
        source,
        ref_kind,
        reference,
        commit,
    })
}

/// Download and validate one resolved source without installing it.
pub async fn stage(
    fetcher: &dyn ArchiveFetcher,
    unpacker: &dyn ArchiveUnpacker,
    resolved: ResolvedSource,
) -> Result<StagedPlugin, InstallError> {
    let url = format!(
        "{CODELOAD_BASE}/{}/{}/zip/{}",
        resolved.source.owner, resolved.source.repo, resolved.commit
    );
    let archive = fetcher
        .fetch(&url)
        .await
        .map_err(fetch_failed)?
        .ok_or_else(|| InstallError::RefNotFound(format!("commit {}", resolved.commit)))?;
    if archive.len() > MAX_PLUGIN_ZIP_BYTES {
        return Err(InstallError::TooLarge);
    }
    let entries = unpacker
        .unpack(&archive)
        .map_err(InstallError::ArchiveUnreadable)?;
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
    /// data folder stays put, so an update keeps both.
    pub fn install_staged(&self, staged: &StagedPlugin) -> Result<String, InstallError> {
        let name = staged.manifest.name.clone();
        let staging = self.dir().join(format!(".installing-{name}"));
        let _ = std::fs::remove_dir_all(&staging);
        let written = write_entries(&staging, &staged.root, &staged.entries).and_then(|()| {
            let record = InstallRecord {
                repository_url: staged.resolved.source.url.clone(),
                repository: format!(
                    "{}/{}",
                    staged.resolved.source.owner, staged.resolved.source.repo
                ),
                ref_kind: staged.resolved.ref_kind.clone(),
                reference: staged.resolved.reference.clone(),
                commit: staged.resolved.commit.clone(),
                installed_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_secs() as i64)
                    .unwrap_or(0),
            };
            let json = serde_json::to_vec_pretty(&record)
                .map_err(|error| InstallError::Io(error.to_string()))?;
            std::fs::write(staging.join(INSTALL_RECORD), json)
                .map_err(|error| InstallError::Io(error.to_string()))
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
        let entries = ZipUnpacker.unpack(&bytes).unwrap();
        assert_eq!(entries[0].path, "root/plugin.toml");
        assert_eq!(entries[0].data, b"[plugin]");
        assert!(ZipUnpacker.unpack(b"not a zip").is_err());

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("../escape.toml", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"[plugin]").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(ZipUnpacker.unpack(&bytes).is_err());
    }
}
