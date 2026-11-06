//! What is in the import folder: its album-shaped folders and what their
//! files look like now.
//!
//! The walk only stats. It never follows a link (a link is skipped, file or
//! folder), and skips every name that starts with `.`, so a copy tool's
//! temporary files are never taken and a folder can be hidden while it
//! fills. Audio is grouped the way an upload is: one unit per folder that
//! holds audio, with disc folders (`CD1`, `Disc 2`) folded into their
//! album, and the loose files at the top as one more. Blocking.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash as _, Hasher as _};
use std::io;
use std::path::{Path, PathBuf};

use crate::drop_import::extract::{is_audio, is_disc_folder};

/// How deep the walk goes below the import folder.
const MAX_DEPTH: usize = 8;

/// One album-shaped folder as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// The album folder below the import folder (`""` for the loose files
    /// at the top).
    pub folder: String,
    /// What every file below the album folder looks like: names, sizes and
    /// modification times. It changes while anything is still arriving.
    pub digest: u64,
    /// Its audio files, below the import folder.
    pub audio: Vec<String>,
    /// Each audio file's size and modification time in milliseconds, as an
    /// import-folder held row records them.
    pub listed: HashMap<String, (i64, i64)>,
}

/// One file the walk saw: its path below the import folder, size and
/// modification time.
struct Seen {
    rel: String,
    size: u64,
    mtime_ns: i128,
}

/// Every album-shaped folder below `root`. An unreadable `root` is an
/// error; an unreadable folder below it is skipped.
pub fn units(root: &Path) -> io::Result<Vec<Unit>> {
    let mut seen: Vec<Seen> = Vec::new();
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if depth == 0 => return Err(error),
            Err(error) => {
                tracing::info!(dir = %dir.display(), %error, "import folder: a folder does not read");
                continue;
            }
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_symlink() {
                tracing::info!(rel, "import folder: a link is skipped");
            } else if kind.is_dir() {
                if depth + 1 < MAX_DEPTH {
                    stack.push((entry.path(), rel, depth + 1));
                }
            } else if kind.is_file() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let mtime_ns = meta
                    .modified()
                    .ok()
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |at| i128::try_from(at.as_nanos()).unwrap_or(i128::MAX));
                seen.push(Seen {
                    rel,
                    size: meta.len(),
                    mtime_ns,
                });
            }
        }
    }
    seen.sort_by(|left, right| left.rel.cmp(&right.rel));
    let identities: HashMap<&str, (i64, i64)> = seen
        .iter()
        .map(|file| {
            let size = i64::try_from(file.size).unwrap_or(i64::MAX);
            let modified = i64::try_from(file.mtime_ns / 1_000_000).unwrap_or(i64::MAX);
            (file.rel.as_str(), (size, modified))
        })
        .collect();
    let mut audio: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in &seen {
        if is_audio(Path::new(&file.rel)) {
            audio
                .entry(album_folder(&file.rel))
                .or_default()
                .push(file.rel.clone());
        }
    }
    Ok(audio
        .into_iter()
        .map(|(folder, audio)| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for file in seen.iter().filter(|file| below(&folder, &file.rel)) {
                file.rel.hash(&mut hasher);
                file.size.hash(&mut hasher);
                file.mtime_ns.hash(&mut hasher);
            }
            let listed = audio
                .iter()
                .filter_map(|rel| Some((rel.clone(), *identities.get(rel.as_str())?)))
                .collect();
            Unit {
                folder,
                digest: hasher.finish(),
                audio,
                listed,
            }
        })
        .collect())
}

/// The non-audio files below one album folder, for the library to file
/// beside it: `(path below the import folder, path below the album
/// folder)`. A folder below it that is an album of its own (it holds audio
/// and is no disc folder) keeps its files. The loose files at the top have
/// none.
pub fn sidecars(root: &Path, folder: &str) -> Vec<(String, String)> {
    if folder.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(root.join(folder), String::new(), 0)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if name.starts_with('.') || kind.is_symlink() {
                continue;
            }
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_dir() && depth + 1 < MAX_DEPTH {
                // A folder that is an album of its own keeps its files.
                if !is_disc_folder(&name_of(&rel)) && holds_audio(&entry.path()) {
                    continue;
                }
                stack.push((entry.path(), rel, depth + 1));
            } else if kind.is_file() && !is_audio(Path::new(&rel)) {
                found.push((format!("{folder}/{rel}"), rel));
            }
        }
    }
    found.sort();
    found
}

/// The last part of a path below the import folder.
fn name_of(rel: &str) -> String {
    rel.rsplit('/').next().unwrap_or(rel).to_owned()
}

/// Whether `dir` holds an audio file directly (never through a link).
fn holds_audio(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file()) && is_audio(&entry.path())
        })
    })
}

/// The album folder an audio file at `rel` belongs to: its own folder, or
/// the one above when its own is a disc folder (never the top).
fn album_folder(rel: &str) -> String {
    let parent = match rel.rsplit_once('/') {
        Some((parent, _)) => parent,
        None => return String::new(),
    };
    match parent.rsplit_once('/') {
        Some((up, own)) if is_disc_folder(own) => up.to_owned(),
        _ => parent.to_owned(),
    }
}

/// Whether the file at `rel` lies in album folder `folder`: anywhere below
/// it, or directly at the top for the loose files.
fn below(folder: &str, rel: &str) -> bool {
    if folder.is_empty() {
        !rel.contains('/')
    } else {
        rel.strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}
