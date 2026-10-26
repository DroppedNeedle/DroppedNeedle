//! The management-artifact rule: any path with a
//! `.droppedneedle-management-` prefixed part or an exact `.recycle` part is
//! never library content.
//!
//! Scan and identify never write music files. Nothing in scan opens a
//! library file for writing; the publisher owns every file change.

use std::path::Path;

/// Reserved hidden namespace for management sidecars.
pub const MANAGEMENT_ARTIFACT_PREFIX: &str = ".droppedneedle-management-";
/// Default recycle bin directory. An exact name match, not a prefix: an
/// unrelated dot-prefixed user folder must keep scanning (v2 #465).
pub const RECYCLE_BIN_DIRECTORY_NAME: &str = ".recycle";

/// True when a path is a management artifact or lives under the recycle
/// bin. Applies to both files and directories.
pub fn is_management_artifact(path: &Path) -> bool {
    path.components().any(|component| {
        let text = component.as_os_str().to_string_lossy();
        text.starts_with(MANAGEMENT_ARTIFACT_PREFIX) || text == RECYCLE_BIN_DIRECTORY_NAME
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_rule_matches_v2() {
        assert!(is_management_artifact(Path::new(".recycle/a.flac")));
        assert!(is_management_artifact(Path::new(
            "x/.droppedneedle-management-9/a.flac"
        )));
        assert!(!is_management_artifact(Path::new(".recycle-bin/a.flac")));
        assert!(!is_management_artifact(Path::new("music/a.flac")));
    }
}
