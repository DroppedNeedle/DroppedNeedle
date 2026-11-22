//! Whether a folder is clear of other folders: not the same as, inside or
//! holding any of them. The import folder uses it to stay clear of the
//! library, the recycle bin, the download folders and the app's folders.

use std::path::{Path, PathBuf};

use crate::landing::canonical_dirs;

/// Checks `real`, a folder's real path, against each set in order. Returns
/// the kind of the first set holding a folder that `real` is, holds or sits
/// inside, with that folder's path. A caller that must skip one folder, such
/// as a download client's own, leaves that entry out of its set. An entry
/// with the same path that it keeps is still checked.
pub fn folder_clear<K>(
    real: &Path,
    sets: impl IntoIterator<Item = (K, Vec<PathBuf>)>,
) -> Result<(), (K, PathBuf)> {
    for (kind, dirs) in sets {
        if let Some(other) = canonical_dirs(&dirs)
            .into_iter()
            .find(|other| overlaps(real, other))
        {
            return Err((kind, other));
        }
    }
    Ok(())
}

/// Whether one folder is, holds or sits inside the other, by whole path
/// components.
fn overlaps(left: &Path, right: &Path) -> bool {
    !right.as_os_str().is_empty() && (left.starts_with(right) || right.starts_with(left))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::folder_clear;
    use Kind::{Downloads, Library};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Library,
        Downloads,
    }

    #[test]
    fn folder_clear_compares_whole_paths() {
        // Case, kind, checked folder, the set's folders, expected overlap (Ok when clear).
        let rows: Vec<(&str, Kind, &str, Vec<&str>, Result<(), &str>)> = vec![
            (
                "equal",
                Library,
                "/clear-test/library",
                vec!["/clear-test/library"],
                Err("/clear-test/library"),
            ),
            (
                "inside",
                Library,
                "/clear-test/library/artist",
                vec!["/clear-test/library"],
                Err("/clear-test/library"),
            ),
            (
                "holds",
                Library,
                "/clear-test/library",
                vec!["/clear-test/library/artist"],
                Err("/clear-test/library/artist"),
            ),
            (
                "clear",
                Library,
                "/clear-test/library2",
                vec!["/clear-test/library"],
                Ok(()),
            ),
            // The caller left its own download folder out of the list.
            (
                "excluded entry",
                Downloads,
                "/clear-test/deemix",
                vec!["/clear-test/slskd"],
                Ok(()),
            ),
            // Another client's folder at the same path is still in the list.
            (
                "kept entry at the excluded path",
                Downloads,
                "/clear-test/deemix",
                vec!["/clear-test/deemix"],
                Err("/clear-test/deemix"),
            ),
        ];
        for (case, kind, checked, dirs, expected) in rows {
            let sets = [(
                kind,
                dirs.into_iter().map(PathBuf::from).collect::<Vec<_>>(),
            )];
            let got = folder_clear(Path::new(checked), sets);
            let expected = expected.map_err(|path| (kind, PathBuf::from(path)));
            assert_eq!(got, expected, "{case}");
        }
    }
}
