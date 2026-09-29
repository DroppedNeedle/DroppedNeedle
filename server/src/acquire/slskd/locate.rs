//! On-disk locator for finished slskd transfers.
//!
//! Ported from `backend/repositories/slskd/slskd_repository.py`
//! (`_locate_file`, `_locate_partial`, `_walk_find`, `_fuzzy_file_key`).
//! slskd already knows its filenames (from the search/handle), so each one
//! resolves to its on-disk path via this locator; unresolved files are
//! skipped. Exact spelling is always tried before a normalized alias, and a
//! fuzzy track-number/title fallback runs last. Alias lookup is deliberately
//! bounded and fail-closed: confined to the resolved mount, regular files
//! only, and a hit only when exactly one matching file exists.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

/// Shared cap for all normalized fallback directory entries (v2
/// `_MAX_WALK_ENTRIES`).
const MAX_WALK_ENTRIES: usize = 10_000;

/// Locator bound to the configured mounts. Sync filesystem I/O only; the
/// repository offloads every call off the async runtime (v2).
#[derive(Debug, Clone)]
pub struct Locator {
    downloads_mount: PathBuf,
    incomplete_mount: Option<PathBuf>,
}

impl Locator {
    #[must_use]
    pub fn new(downloads_mount: PathBuf, incomplete_mount: Option<PathBuf>) -> Self {
        Self {
            downloads_mount,
            incomplete_mount,
        }
    }

    /// Resolve a finished transfer inside the mounted slskd downloads
    /// directory (v2 `_locate_file`).
    ///
    /// When the expected size is known, an exact-named hit whose bytes
    /// mismatch is a stale file from another peer's folder, not this
    /// transfer: steps 1/2/4 skip it (v2 #397) and the walk fallbacks refuse
    /// it, falling through to the peer-scoped/size-aware steps or to `None`.
    /// A single non-exact normalized alias still resolves on a size mismatch
    /// (search-advertised sizes are unreliable across a Unicode drift);
    /// final size rejection for that case belongs to the verifier
    /// (`SIZE_MISMATCH`), not the locator. Unknown size keeps the old
    /// name-only behavior everywhere.
    #[must_use]
    pub fn locate_file(
        &self,
        username: &str,
        remote_filename: &str,
        size: Option<i64>,
    ) -> Option<PathBuf> {
        let parts: Vec<&str> = remote_filename
            .split(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        if parts.is_empty() || parts.contains(&"..") {
            return None;
        }
        let mount = self.downloads_mount.canonicalize().ok()?;
        let basename = parts[parts.len() - 1];
        let normalised_basename = normalised_filename(basename);
        let expected_size = size.filter(|size| *size > 0);

        let within_mount = |candidate: &Path| -> Option<PathBuf> {
            let resolved = candidate.canonicalize().ok()?;
            if !resolved.starts_with(&mount) {
                tracing::warn!("slskd path escapes the downloads mount");
                return None;
            }
            Some(resolved)
        };
        let direct_hit_usable = |candidate: &Path| -> bool {
            // With a known expected size a byte-mismatched same-named file
            // is a stale leftover from another peer's folder (v2 #397).
            // Unknown size keeps the old name-only behavior, as does an
            // unreadable size (fail open; the verifier owns that file).
            match expected_size {
                None => true,
                Some(want) => candidate
                    .metadata()
                    .map(|meta| meta.len() as i64 == want)
                    .unwrap_or(true),
            }
        };
        let find_direct_exact = |directory: &Path| -> Option<PathBuf> {
            let candidate = within_mount(&directory.join(basename))?;
            candidate.is_file().then_some(candidate)
        };

        // 1. slskd's common layout: {mount}/{leaf remote folder}/{filename}.
        if parts.len() >= 2
            && let Some(leaf) = find_direct_exact(&mount.join(parts[parts.len() - 2]))
            && direct_hit_usable(&leaf)
        {
            return Some(leaf);
        }
        // 2. Flat layout: {mount}/{filename}.
        if let Some(flat) = find_direct_exact(&mount)
            && direct_hit_usable(&flat)
        {
            return Some(flat);
        }
        // 3. Peers that file by username: walk {mount}/{username}/ at any
        // depth (v2; scoped so a same-named track from another peer cannot
        // be picked up).
        let user_root = if username.is_empty() {
            None
        } else {
            within_mount(&mount.join(username)).filter(|root| root.is_dir())
        };
        if let Some(root) = &user_root
            && let Some(hit) = walk_find(root, &mount, &|entry| entry.file_name() == basename)
        {
            return Some(hit);
        }
        // 4. slskd may have sanitised the folder name — scan one level down
        // for it. A size-mismatched hit is skipped, not returned (v2 #397).
        if let Ok(children) = sorted_children(&mount) {
            for child in children {
                let Some(child_root) = within_mount(&child) else {
                    continue;
                };
                if !child_root.is_dir() {
                    continue;
                }
                if let Some(candidate) = find_direct_exact(&child_root)
                    && direct_hit_usable(&candidate)
                {
                    return Some(candidate);
                }
            }
        }
        // 5. Last resort: slskd may have sanitised the filename. An exact
        // byte-size match under the peer's folder recovers it; this stays
        // peer-scoped because a size-only walk across peers is unsafe (v2).
        if let (Some(want), Some(root)) = (expected_size, &user_root)
            && root.is_dir()
        {
            let hit = walk_find(root, &mount, &|entry| {
                entry
                    .metadata()
                    .map(|meta| meta.len() as i64 == want)
                    .unwrap_or(false)
            });
            if hit.is_some() {
                return hit;
            }
        }
        // 6. Whole-mount exact-name fallback for a file nested deeper than
        // the cheap steps look. Validates byte size when known (v2).
        let hit = walk_find(&mount, &mount, &|entry| {
            if entry.file_name() != basename {
                return false;
            }
            match expected_size {
                None => true,
                Some(want) => entry
                    .metadata()
                    .map(|meta| meta.len() as i64 == want)
                    .unwrap_or(false),
            }
        });
        if hit.is_some() {
            return hit;
        }

        // 7. NFC alias fallback. Every normalized phase shares one budget.
        // The peer scope runs before the whole mount so an alias cannot
        // cross peers merely because an unrelated same-sized file happens to
        // be encountered first (v2).
        let mut budget = EntryBudget::new(MAX_WALK_ENTRIES);
        if parts.len() >= 2 {
            match find_normalised_in_directory(
                &mount.join(parts[parts.len() - 2]),
                &mount,
                &normalised_basename,
                expected_size,
                &mut budget,
            ) {
                AliasOutcome::Hit(path) => return Some(path),
                AliasOutcome::FailClosed(kind, count) => {
                    log_unlocatable(&mount, basename, size, kind, count);
                    return None;
                }
                AliasOutcome::NotFound => {}
            }
        }
        match find_normalised_in_directory(
            &mount,
            &mount,
            &normalised_basename,
            expected_size,
            &mut budget,
        ) {
            AliasOutcome::Hit(path) => return Some(path),
            AliasOutcome::FailClosed(kind, count) => {
                log_unlocatable(&mount, basename, size, kind, count);
                return None;
            }
            AliasOutcome::NotFound => {}
        }
        if let Some(root) = &user_root
            && root.is_dir()
        {
            match walk_find_normalised(
                root,
                &mount,
                basename,
                &normalised_basename,
                expected_size,
                &mut budget,
            ) {
                AliasOutcome::Hit(path) => return Some(path),
                AliasOutcome::FailClosed(kind, count) => {
                    log_unlocatable(&mount, basename, size, kind, count);
                    return None;
                }
                AliasOutcome::NotFound => {}
            }
        }
        match walk_find_normalised(
            &mount,
            &mount,
            basename,
            &normalised_basename,
            expected_size,
            &mut budget,
        ) {
            AliasOutcome::Hit(path) => return Some(path),
            AliasOutcome::FailClosed(kind, count) => {
                log_unlocatable(&mount, basename, size, kind, count);
                return None;
            }
            AliasOutcome::NotFound => {}
        }

        // 8-9. Fuzzy basename fallback for peers that advertise a flat
        // "Artist - Album - NN - Title" name while slskd files the download
        // as "NN. Title" inside an album folder (v2 issue #229).
        // Fail-closed like the NFC phases: confined to the mount, regular
        // files only, sharing the same budget, a hit only when exactly one
        // candidate matches. The peer scope runs first; the mount-wide sweep
        // stays behind a mandatory exact byte-size gate.
        let expected_key = fuzzy_file_key(basename);
        if let Some(root) = &user_root
            && root.is_dir()
        {
            match walk_find_fuzzy(
                root,
                &mount,
                &expected_key,
                expected_size,
                false,
                &mut budget,
            ) {
                AliasOutcome::Hit(path) => return Some(path),
                AliasOutcome::FailClosed(kind, count) => {
                    log_unlocatable(&mount, basename, size, kind, count);
                    return None;
                }
                AliasOutcome::NotFound => {}
            }
        }
        if expected_size.is_some() {
            match walk_find_fuzzy(
                &mount,
                &mount,
                &expected_key,
                expected_size,
                true,
                &mut budget,
            ) {
                AliasOutcome::Hit(path) => return Some(path),
                AliasOutcome::FailClosed(kind, count) => {
                    log_unlocatable(&mount, basename, size, kind, count);
                    return None;
                }
                AliasOutcome::NotFound => {}
            }
        }

        log_unlocatable(&mount, basename, size, FailKind::None, 0);
        None
    }

    /// Find stranded partial bytes by exact basename (then NFC alias at
    /// most) inside the incomplete mount (v2 `_locate_partial`).
    ///
    /// `username` is intentionally ignored: slskd's incomplete layout
    /// (`incomplete/<album>/<file>`) is not username-scoped, so the safe key
    /// is the basename plus exactly-one confinement. No fuzzy phase, no
    /// size-only phase. The expected size is recorded for the log line only:
    /// partial files are short by definition, so an equality gate would
    /// never hit. An unknown size still allows the direct probes but skips
    /// the recursive sweep (the phase-9 require_size discipline, v2).
    #[must_use]
    pub fn locate_partial(
        &self,
        _username: &str,
        remote_filename: &str,
        size: Option<i64>,
    ) -> Option<PathBuf> {
        let root_setting = self.incomplete_mount.as_ref()?;
        let parts: Vec<&str> = remote_filename
            .split(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        if parts.is_empty() || parts.contains(&"..") {
            return None;
        }
        let incomplete = root_setting.canonicalize().ok()?;
        if !incomplete.is_dir() {
            return None;
        }
        let basename = parts[parts.len() - 1];
        let normalised_basename = normalised_filename(basename);

        let within_incomplete = |candidate: &Path| -> Option<PathBuf> {
            let resolved = candidate.canonicalize().ok()?;
            if !resolved.starts_with(&incomplete) {
                tracing::warn!("slskd path escapes the incomplete mount");
                return None;
            }
            Some(resolved)
        };

        // Direct probes: the flat file and the one-level album-dir layout
        // (incomplete/<album>/<file>) slskd most commonly produces (v2).
        let mut direct: HashSet<PathBuf> = HashSet::new();
        if let Some(flat) = within_incomplete(&incomplete.join(basename))
            && flat.is_file()
        {
            direct.insert(flat);
        }
        if let Ok(children) = sorted_children(&incomplete) {
            for child in children {
                let Some(child_root) = within_incomplete(&child) else {
                    continue;
                };
                if !child_root.is_dir() {
                    continue;
                }
                if let Some(candidate) = within_incomplete(&child_root.join(basename))
                    && candidate.is_file()
                {
                    direct.insert(candidate);
                }
            }
        }
        if direct.len() > 1 {
            tracing::warn!(
                basename,
                candidates = direct.len(),
                "slskd partial file not usable from the incomplete mount: ambiguous, refusing to guess"
            );
            return None;
        }
        if let Some(hit) = direct.into_iter().next() {
            return Some(hit);
        }

        if size.is_none_or(|size| size <= 0) {
            return None;
        }

        // Recursive exact-then-NFC sweep with its own budget (never shared
        // with the complete-mount phases, which may already be exhausted).
        // Exactly one basename match resolves; several fail closed (v2).
        let mut budget = EntryBudget::new(MAX_WALK_ENTRIES);
        let mut exact: HashSet<PathBuf> = HashSet::new();
        let mut aliased: HashSet<PathBuf> = HashSet::new();
        let mut stack = vec![incomplete.clone()];
        let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
        while let Some(current) = stack.pop() {
            let Some(current) = within_incomplete(&current) else {
                continue;
            };
            if !current.is_dir() || !seen_dirs.insert(current.clone()) {
                continue;
            }
            let entries = match std::fs::read_dir(&current) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                if !budget.take() {
                    tracing::warn!(
                        basename,
                        "slskd partial file not usable from the incomplete mount: entry budget exhausted, refusing to guess"
                    );
                    return None;
                }
                let Some(resolved) = within_incomplete(&entry.path()) else {
                    continue;
                };
                if resolved.is_dir() {
                    stack.push(resolved);
                    continue;
                }
                if !resolved.is_file() {
                    continue;
                }
                if entry.file_name() == basename {
                    exact.insert(resolved);
                    if exact.len() > 1 {
                        tracing::warn!(
                            basename,
                            candidates = exact.len(),
                            "slskd partial file not usable from the incomplete mount: ambiguous, refusing to guess"
                        );
                        return None;
                    }
                } else if normalised_filename(&entry.file_name().to_string_lossy())
                    == normalised_basename
                {
                    aliased.insert(resolved);
                }
            }
        }
        if let Some(hit) = exact.into_iter().next() {
            return Some(hit);
        }
        if aliased.len() > 1 {
            tracing::warn!(
                basename,
                candidates = aliased.len(),
                "slskd partial file not usable from the incomplete mount: ambiguous, refusing to guess"
            );
            return None;
        }
        aliased.into_iter().next()
    }
}

fn normalised_filename(value: &str) -> String {
    value.nfc().collect()
}

fn sorted_children(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut children: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    children.sort();
    Ok(children)
}

/// Shared cap for normalized fallback directory entries (v2 `_EntryBudget`).
struct EntryBudget {
    remaining: usize,
}

impl EntryBudget {
    fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailKind {
    Ambiguous,
    Budget,
    None,
}

enum AliasOutcome {
    Hit(PathBuf),
    FailClosed(FailKind, usize),
    NotFound,
}

/// Find the first matching regular file under `root` (v2 `_walk_find`).
/// Bounded, confined to the resolved mount, and keyed by resolved directory
/// paths so in-mount symlink loops cannot revisit forever.
fn walk_find(
    root: &Path,
    mount: &Path,
    predicate: &dyn Fn(&std::fs::DirEntry) -> bool,
) -> Option<PathBuf> {
    let mount = mount.canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    if !root.starts_with(&mount) || !root.is_dir() {
        return None;
    }
    let mut stack = vec![root];
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let mut seen = 0;
    while let Some(current) = stack.pop() {
        let current = current.canonicalize().ok()?;
        if !current.starts_with(&mount) || !current.is_dir() || !seen_dirs.insert(current.clone()) {
            continue;
        }
        let entries = std::fs::read_dir(&current).ok()?;
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_WALK_ENTRIES {
                return None;
            }
            let resolved = entry.path().canonicalize().ok()?;
            if !resolved.starts_with(&mount) {
                continue;
            }
            if resolved.is_dir() {
                stack.push(resolved);
                continue;
            }
            if !resolved.is_file() || !predicate(&entry) {
                continue;
            }
            return Some(resolved);
        }
    }
    None
}

fn confined_file(mount: &Path, candidate: &Path) -> Option<PathBuf> {
    let resolved = candidate.canonicalize().ok()?;
    if !resolved.starts_with(mount) || !resolved.is_file() {
        return None;
    }
    Some(resolved)
}

/// One immediate normalized alias in `directory`, or ambiguity/exhaustion
/// (v2 `_find_normalised_in_directory`).
fn find_normalised_in_directory(
    directory: &Path,
    mount: &Path,
    normalised_basename: &str,
    expected_size: Option<i64>,
    budget: &mut EntryBudget,
) -> AliasOutcome {
    let root = match directory.canonicalize() {
        Ok(resolved) if resolved.starts_with(mount) && resolved.is_dir() => resolved,
        _ => return AliasOutcome::NotFound,
    };
    let mut matches: HashSet<PathBuf> = HashSet::new();
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(_) => return AliasOutcome::NotFound,
    };
    for entry in entries.flatten() {
        if !budget.take() {
            return AliasOutcome::FailClosed(FailKind::Budget, 0);
        }
        let Some(resolved) = confined_file(mount, &entry.path()) else {
            continue;
        };
        if normalised_filename(&entry.file_name().to_string_lossy()) != normalised_basename {
            continue;
        }
        if let Some(want) = expected_size
            && resolved.metadata().map(|meta| meta.len() as i64).ok() != Some(want)
        {
            continue;
        }
        matches.insert(resolved);
        if matches.len() > 1 {
            return AliasOutcome::FailClosed(FailKind::Ambiguous, matches.len());
        }
    }
    match matches.into_iter().next() {
        Some(hit) => AliasOutcome::Hit(hit),
        None => AliasOutcome::NotFound,
    }
}

/// One normalized alias under `root`, confined and loop-safe (v2
/// `_walk_find_normalised`). When the size-gated set is empty but the
/// expected size is known, the already-collected in-memory alias set is
/// retried without the size gate: exactly one alias resolves (size rejection
/// is then the verifier's job), zero stays not-found, several fail closed as
/// ambiguous. The ungated retry keeps NON-EXACT aliases only: an
/// exact-named file whose bytes mismatch is a stale file from another peer's
/// folder (v2 #397), never a Unicode-drift alias.
fn walk_find_normalised(
    root: &Path,
    mount: &Path,
    basename: &str,
    normalised_basename: &str,
    expected_size: Option<i64>,
    budget: &mut EntryBudget,
) -> AliasOutcome {
    let root = match root.canonicalize() {
        Ok(resolved) if resolved.starts_with(mount) && resolved.is_dir() => resolved,
        _ => return AliasOutcome::NotFound,
    };
    let mut stack = vec![root];
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let mut matches: HashSet<PathBuf> = HashSet::new();
    let mut ungated: HashSet<PathBuf> = HashSet::new();
    while let Some(current) = stack.pop() {
        let current = match current.canonicalize() {
            Ok(resolved) => resolved,
            Err(_) => continue,
        };
        if !current.starts_with(mount) || !current.is_dir() || !seen_dirs.insert(current.clone()) {
            continue;
        }
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if !budget.take() {
                return AliasOutcome::FailClosed(FailKind::Budget, 0);
            }
            let resolved = match entry.path().canonicalize() {
                Ok(resolved) if resolved.starts_with(mount) => resolved,
                _ => continue,
            };
            if resolved.is_dir() {
                stack.push(resolved);
                continue;
            }
            if !resolved.is_file() {
                continue;
            }
            if normalised_filename(&entry.file_name().to_string_lossy()) != normalised_basename {
                continue;
            }
            if expected_size.is_none() {
                matches.insert(resolved);
            } else {
                let want = expected_size.unwrap_or(0);
                if entry.file_name() != basename {
                    ungated.insert(resolved.clone());
                }
                if resolved.metadata().map(|meta| meta.len() as i64).ok() != Some(want) {
                    continue;
                }
                matches.insert(resolved);
            }
            if matches.len() > 1 {
                return AliasOutcome::FailClosed(FailKind::Ambiguous, matches.len());
            }
        }
    }
    if let Some(hit) = matches.into_iter().next() {
        return AliasOutcome::Hit(hit);
    }
    if expected_size.is_some() {
        if ungated.len() == 1 {
            return AliasOutcome::Hit(ungated.into_iter().next().unwrap_or_default());
        }
        if ungated.len() > 1 {
            return AliasOutcome::FailClosed(FailKind::Ambiguous, ungated.len());
        }
    }
    AliasOutcome::NotFound
}

/// Log a fail-closed miss with the minimal shape (v2 `_log_unlocatable`):
/// basename, size (or "unknown"), and the top-level entry count only.
/// Never candidate paths, usernames, hosts, secrets, remote full paths, or
/// exception text.
fn log_unlocatable(mount: &Path, basename: &str, size: Option<i64>, kind: FailKind, count: usize) {
    let top_level = std::fs::read_dir(mount)
        .map(|entries| entries.count() as i64)
        .unwrap_or(-1);
    let size_label = size.unwrap_or(0);
    match kind {
        FailKind::Budget => tracing::warn!(
            basename,
            size_label,
            top_level,
            "slskd file not locatable on the downloads mount: entry budget exhausted, refusing to guess"
        ),
        FailKind::Ambiguous => tracing::warn!(
            basename,
            size_label,
            top_level,
            count,
            "slskd file not locatable on the downloads mount: ambiguous, refusing to guess"
        ),
        FailKind::None => tracing::warn!(
            basename,
            size_label,
            top_level,
            "slskd file not locatable on the downloads mount: the on-disk layout may nest deeper or sanitise names beyond what get_file_path handles"
        ),
    }
}

/// `(track_number, title_core, extension)` split of a download basename
/// (v2 `_fuzzy_file_key`, issue #229).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FuzzyKey {
    track: Option<u32>,
    core: String,
    extension: String,
}

fn fuzzy_file_key(basename: &str) -> FuzzyKey {
    let normalised: String = basename.nfc().collect::<String>().to_lowercase();
    let (stem, extension) = match normalised.rfind('.') {
        Some(dot) if dot > 0 => (
            normalised[..dot].to_owned(),
            normalised[dot + 1..].to_owned(),
        ),
        _ => (normalised.clone(), String::new()),
    };
    // Split on dashes/underscores; strip an `Artist - Album` prefix up to a
    // standalone track-number segment, else from the first NN token (v2).
    let segments: Vec<&str> = stem
        .split(['-', '\u{2013}', '\u{2014}', '_'])
        .map(str::trim)
        .collect();
    let mut remainder = stem.clone();
    let mut stripped = false;
    for (index, segment) in segments.iter().enumerate() {
        let trimmed = segment.trim_end_matches('.');
        if !trimmed.is_empty()
            && trimmed.len() <= 3
            && trimmed.chars().all(|ch| ch.is_ascii_digit())
        {
            remainder = stem
                .split(['-', '\u{2013}', '\u{2014}', '_'])
                .skip(index)
                .collect::<Vec<_>>()
                .join("-");
            stripped = true;
            break;
        }
    }
    if !stripped {
        // Fall back to the first NN token.
        let chars: Vec<char> = stem.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            if chars[index].is_ascii_digit() {
                let mut end = index;
                while end < chars.len() && chars[end].is_ascii_digit() && end - index < 3 {
                    end += 1;
                }
                let before_ok = index == 0 || !chars[index - 1].is_alphanumeric();
                let after_ok = end >= chars.len() || !chars[end].is_alphanumeric();
                if before_ok && after_ok {
                    let byte: usize = chars[..index].iter().map(|ch| ch.len_utf8()).sum();
                    remainder = stem[byte..].to_owned();
                    break;
                }
                index = end;
            } else {
                index += 1;
            }
        }
    }
    let remainder = remainder.trim().to_owned();
    // Split a leading track token (`^\d{1,3}\b` over `[.-_\s]*`, v2).
    let mut track = None;
    let mut title_part = remainder.as_str();
    let leading: String = remainder
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if !leading.is_empty() && leading.len() <= 3 {
        let after_digits = &remainder[leading.len()..];
        let boundary_ok = after_digits
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
        if boundary_ok {
            track = leading.parse().ok();
            title_part = after_digits
                .trim_start_matches(['.', '-', '_'])
                .trim_start();
        }
    }
    let core: String = title_part
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .collect();
    FuzzyKey {
        track,
        core,
        extension,
    }
}

fn fuzzy_keys_match(expected: &FuzzyKey, candidate_name: &str) -> bool {
    let candidate = fuzzy_file_key(candidate_name);
    if expected.extension != candidate.extension {
        return false;
    }
    if let (Some(want), Some(have)) = (expected.track, candidate.track)
        && want != have
    {
        return false;
    }
    if expected.core.is_empty() || candidate.core.is_empty() {
        return false;
    }
    let (short, long) = if expected.core.len() <= candidate.core.len() {
        (&expected.core, &candidate.core)
    } else {
        (&candidate.core, &expected.core)
    };
    // Both cores non-trivial; the shorter contained in the longer (v2).
    short.chars().count() >= 2 && long.contains(short)
}

/// Collect fuzzy basename matches under `root`, confined and loop-safe
/// (v2 `_walk_find_fuzzy`). `require_size` gates the mount-wide sweep on a
/// known expected size.
fn walk_find_fuzzy(
    root: &Path,
    mount: &Path,
    expected: &FuzzyKey,
    expected_size: Option<i64>,
    require_size: bool,
    budget: &mut EntryBudget,
) -> AliasOutcome {
    let root = match root.canonicalize() {
        Ok(resolved) if resolved.starts_with(mount) && resolved.is_dir() => resolved,
        _ => return AliasOutcome::NotFound,
    };
    let mut stack = vec![root];
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let mut matches: HashSet<PathBuf> = HashSet::new();
    while let Some(current) = stack.pop() {
        let current = match current.canonicalize() {
            Ok(resolved) => resolved,
            Err(_) => continue,
        };
        if !current.starts_with(mount) || !current.is_dir() || !seen_dirs.insert(current.clone()) {
            continue;
        }
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if !budget.take() {
                return AliasOutcome::FailClosed(FailKind::Budget, 0);
            }
            let resolved = match entry.path().canonicalize() {
                Ok(resolved) if resolved.starts_with(mount) => resolved,
                _ => continue,
            };
            if resolved.is_dir() {
                stack.push(resolved);
                continue;
            }
            if !resolved.is_file() {
                continue;
            }
            if !fuzzy_keys_match(expected, &entry.file_name().to_string_lossy()) {
                continue;
            }
            if let Some(want) = expected_size {
                if resolved.metadata().map(|meta| meta.len() as i64).ok() != Some(want) {
                    continue;
                }
            } else if require_size {
                continue;
            }
            matches.insert(resolved);
            if matches.len() > 1 {
                return AliasOutcome::FailClosed(FailKind::Ambiguous, matches.len());
            }
        }
    }
    match matches.into_iter().next() {
        Some(hit) => AliasOutcome::Hit(hit),
        None => AliasOutcome::NotFound,
    }
}
