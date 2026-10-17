//! Unpack an upload and split it into album-shaped units.
//!
//! Every `.zip` in the job's staging folder is unpacked (audio only, no
//! path escapes, no zip bombs), then the audio is grouped: one unit per
//! folder holding audio (disc folders folded into their album), plus one
//! for loose files. Blocking file work: callers run it on a blocking
//! thread.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// More entries than this in one archive is not a purchase.
const MAX_ARCHIVE_ENTRIES: usize = 4096;
/// Far above any real purchase, far below a zip bomb.
const MAX_ARCHIVE_BYTES: u64 = 64 * (1 << 30);
/// Disk kept free beyond an archive's unpacked size.
const FREE_SPACE_MARGIN: u64 = 512 * (1 << 20);
/// The unit name for audio dropped without a folder.
pub const LOOSE_UNIT_NAME: &str = "Loose tracks";

/// One album-shaped unit: a folder name and its audio files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub folder_name: String,
    pub files: Vec<PathBuf>,
}

/// Whether a file name is audio the library imports.
pub fn is_audio(path: &Path) -> bool {
    droppedneedle_library::tags::format_for_path(path).is_ok()
}

/// Whether an upload's file name is something a drop accepts: an archive
/// or audio.
pub fn accepted_upload(name: &str) -> bool {
    let path = Path::new(name);
    let zip = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
    zip || is_audio(path)
}

/// A file-system-safe single path component for staging (never for the
/// library: the naming template owns that).
pub fn safe_component(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|ch: char| ch == ' ' || ch == '.');
    if trimmed.is_empty() {
        "upload".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The user's own name for a staged upload: `003_Album.zip` -> `Album`.
fn display_stem(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bytes = stem.as_bytes();
    if bytes.len() > 4 && bytes[..3].iter().all(u8::is_ascii_digit) && bytes[3] == b'_' {
        stem[4..].to_owned()
    } else {
        stem
    }
}

/// Unpack every archive in `staging_dir`, then group the audio into
/// units. Notes say what was skipped and why, in plain words.
pub fn extract_and_group(staging_dir: &Path) -> std::io::Result<(Vec<Unit>, Vec<String>)> {
    let mut notes = Vec::new();
    for child in sorted_children(staging_dir)? {
        let is_zip = child.is_file()
            && child
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
        if !is_zip {
            continue;
        }
        let display = display_stem(&child);
        let mut target = staging_dir.join(safe_component(&display));
        if target.exists() {
            target = staging_dir.join(safe_component(&display_stem_raw(&child)));
        }
        let label = format!("{display}.zip");
        if let Err(note) = safe_extract(&child, &target, &label, &mut notes) {
            notes.push(note);
        }
        if let Err(error) = std::fs::remove_file(&child) {
            tracing::warn!(path = %child.display(), %error, "unpacked archive not removed");
        }
    }

    // One unit per folder that holds audio, with disc folders ("CD1",
    // "Disc 2") folded into their album folder, the way beets groups an
    // import. A zip holding several album folders gives one unit each.
    let mut units: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut loose = Vec::new();
    for child in sorted_children(staging_dir)? {
        let kind = std::fs::symlink_metadata(&child)?.file_type();
        if kind.is_dir() {
            let mut audio = Vec::new();
            walk_audio(&child, 0, &mut audio);
            for file in audio {
                let album_dir = album_folder(&file, staging_dir);
                units.entry(album_dir).or_default().push(file);
            }
        } else if kind.is_file() && is_audio(&child) {
            loose.push(child);
        }
    }
    let mut out: Vec<Unit> = units
        .into_iter()
        .map(|(dir, mut files)| {
            files.sort();
            let folder_name = dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            Unit { folder_name, files }
        })
        .collect();
    if !loose.is_empty() {
        out.push(Unit {
            folder_name: LOOSE_UNIT_NAME.to_owned(),
            files: loose,
        });
    }
    Ok((out, notes))
}

/// Whether a folder name is only a disc marker: `CD1`, `Disc 2`, `disk-3`.
fn is_disc_folder(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    ["cd", "disc", "disk"].iter().any(|prefix| {
        lower.strip_prefix(prefix).is_some_and(|rest| {
            let digits = rest.trim_start_matches([' ', '-', '_', '.']);
            !digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit())
        })
    })
}

/// The album folder an audio file belongs to: its own folder, or the one
/// above when its own is a disc folder (never above `staging_dir`).
fn album_folder(file: &Path, staging_dir: &Path) -> PathBuf {
    let Some(dir) = file.parent() else {
        return staging_dir.to_path_buf();
    };
    let is_disc = dir
        .file_name()
        .is_some_and(|name| is_disc_folder(&name.to_string_lossy()));
    match dir.parent() {
        Some(up) if is_disc && up != staging_dir && up.starts_with(staging_dir) => up.to_path_buf(),
        _ => dir.to_path_buf(),
    }
}

/// The stem with its staging prefix kept, for a second zip of one name.
fn display_stem_raw(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn sorted_children(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut children: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    children.sort();
    Ok(children)
}

/// Audio under one folder, never following symlinks.
fn walk_audio(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk_audio(&path, depth + 1, out);
        } else if kind.is_file() && is_audio(&path) {
            out.push(path);
        }
    }
}

/// Extract only audio entries, refusing traversal, absolute paths and
/// archives that unpack far larger than they claim. An entry holding more
/// than its header declares fails to read, so the archive is refused as
/// damaged; the cap on bytes written is our own bound on top. A file that
/// fails part way is removed. An `Err` is a note for the person: the
/// archive was not usable at all.
fn safe_extract(
    archive: &Path,
    target_dir: &Path,
    label: &str,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let corrupt = || format!("Couldn't read {label}: the archive is damaged.");
    let file = std::fs::File::open(archive).map_err(|_| corrupt())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| corrupt())?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!("Skipped {label}: it holds too many files."));
    }
    let mut declared: u64 = 0;
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index).map_err(|_| corrupt())?;
        declared = declared.saturating_add(entry.size());
    }
    if declared > MAX_ARCHIVE_BYTES {
        return Err(format!("Skipped {label}: the archive is too large."));
    }
    // Room to unpack it, with some to spare for the library copy step.
    match droppedneedle_library::adapters::free_bytes_for(target_dir.parent().unwrap_or(target_dir))
    {
        Ok(free) if free < declared.saturating_add(FREE_SPACE_MARGIN) => {
            return Err(format!(
                "Skipped {label}: there isn't enough free disk space to unpack it."
            ));
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "free space unknown; unpacking anyway"),
    }
    // Headers can lie. zip already fails a read past an entry's declared
    // size; this cap keeps the limit ours whatever the reader does.
    let cap = declared
        .saturating_add(declared / 10)
        .saturating_add(1 << 20)
        .min(MAX_ARCHIVE_BYTES);
    let mut skipped = 0usize;
    let mut written: u64 = 0;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|_| corrupt())?;
        if entry.is_dir() {
            continue;
        }
        let raw = PathBuf::from(entry.name().map_err(|_| corrupt())?.as_ref());
        let escapes = raw.is_absolute()
            || raw
                .components()
                .any(|part| !matches!(part, Component::Normal(_)));
        if escapes || !is_audio(&raw) {
            skipped += 1;
            continue;
        }
        let mut dest = target_dir.to_path_buf();
        for part in raw.components() {
            dest.push(safe_component(&part.as_os_str().to_string_lossy()));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|_| corrupt())?;
        }
        let mut out = std::fs::File::create(&dest).map_err(|_| corrupt())?;
        let copied = copy_entry(&mut entry, &mut out, cap, &mut written);
        drop(out);
        if copied != Copied::Done
            && let Err(error) = std::fs::remove_file(&dest)
        {
            tracing::warn!(path = %dest.display(), %error, "partly unpacked entry not removed");
        }
        match copied {
            Copied::Done => {}
            Copied::OverCap => {
                notes.push(format!(
                    "Stopped unpacking {label}: it unpacks far larger than it claims."
                ));
                return Ok(());
            }
            Copied::Failed => return Err(corrupt()),
        }
    }
    if skipped > 0 {
        let noun = if skipped == 1 { "file" } else { "files" };
        notes.push(format!("{label}: ignored {skipped} non-audio {noun}."));
    }
    Ok(())
}

/// How copying one archive entry ended.
#[derive(Debug, PartialEq, Eq)]
enum Copied {
    Done,
    /// The bytes written passed the archive's cap.
    OverCap,
    /// A read or write failed: a damaged entry, one longer than its
    /// header says, or a full disk.
    Failed,
}

/// Copy one entry to `out`, adding what it writes to `written`.
fn copy_entry(
    entry: &mut impl std::io::Read,
    out: &mut impl std::io::Write,
    cap: u64,
    written: &mut u64,
) -> Copied {
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let Ok(read) = entry.read(&mut buffer) else {
            return Copied::Failed;
        };
        if read == 0 {
            return Copied::Done;
        }
        *written = written.saturating_add(read as u64);
        if *written > cap {
            return Copied::OverCap;
        }
        if out.write_all(&buffer[..read]).is_err() {
            return Copied::Failed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_names_read_as_the_users_own() {
        assert_eq!(display_stem(Path::new("/s/003_My Album.zip")), "My Album");
        assert_eq!(display_stem(Path::new("/s/Album.zip")), "Album");
        assert_eq!(safe_component("a/b:c?. "), "a_b_c_");
        assert_eq!(safe_component(" .. "), "upload");
        assert!(accepted_upload("x.ZIP"));
        assert!(accepted_upload("01 Song.flac"));
        assert!(!accepted_upload("cover.jpg"));
    }

    #[test]
    fn disc_folders_fold_into_their_album() {
        let staging = Path::new("/s/job");
        assert!(is_disc_folder("CD1") && is_disc_folder("Disc 2") && is_disc_folder("disk-3"));
        assert!(!is_disc_folder("Discovery") && !is_disc_folder("CD"));
        assert_eq!(
            album_folder(Path::new("/s/job/Shop/Album/CD2/01.flac"), staging),
            PathBuf::from("/s/job/Shop/Album")
        );
        assert_eq!(
            album_folder(Path::new("/s/job/Shop/Other/01.flac"), staging),
            PathBuf::from("/s/job/Shop/Other")
        );
    }
}
