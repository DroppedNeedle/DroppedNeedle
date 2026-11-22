//! Deemix as a download source: its web API client, the shapes it answers,
//! the one session DroppedNeedle keeps with it, the album and bitrate a
//! task takes, and the source the download worker drives.

pub mod client;
pub mod models;
pub mod pick;
pub mod session;
pub mod source;

use std::path::{Path, PathBuf};

/// The path DroppedNeedle reads a file at that Deemix reported at `path`:
/// its part below `location` (Deemix's downloads folder, on Deemix's
/// side) joined under `mount`. Repeated slashes count as one, and
/// `location` may or may not end in `/`. A relative path, a path with a
/// `.` or `..` component, the folder itself and a path outside `location`
/// have none.
#[must_use]
pub fn local_path(path: &str, location: &str, mount: &Path) -> Option<PathBuf> {
    let parts = |text: &str| -> Option<Vec<String>> {
        let parts: Vec<String> = text
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect();
        (!parts.iter().any(|part| part == "." || part == "..")).then_some(parts)
    };
    if !path.starts_with('/') {
        return None;
    }
    let path = parts(path)?;
    let location = parts(location)?;
    let below = path.strip_prefix(location.as_slice())?;
    if below.is_empty() {
        return None;
    }
    let mut out = mount.to_path_buf();
    out.extend(below);
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::local_path;

    /// Deemix's paths map under the mount whether or not its folder ends
    /// in `/`, with `//` read as `/`; a path outside its folder, or one
    /// with a `.` or `..` component, maps to nothing.
    #[test]
    fn deemix_paths_map_under_the_mount() {
        let mount = Path::new("/deemix-downloads");
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "/downloads//Radiohead - In Rainbows/06 - Faust Arp.mp3",
                "/downloads/",
                Some("/deemix-downloads/Radiohead - In Rainbows/06 - Faust Arp.mp3"),
            ),
            (
                "/downloads/Radiohead - Nude.mp3",
                "/downloads",
                Some("/deemix-downloads/Radiohead - Nude.mp3"),
            ),
            (
                "/downloads//A/CD2/01 - B.mp3",
                "/downloads/",
                Some("/deemix-downloads/A/CD2/01 - B.mp3"),
            ),
            ("/elsewhere/A/01.mp3", "/downloads/", None),
            ("/downloads2/A/01.mp3", "/downloads", None),
            ("/downloads/../etc/passwd", "/downloads/", None),
            ("/downloads/A/./01.mp3", "/downloads/", None),
            ("/downloads/", "/downloads/", None),
            ("downloads/A/01.mp3", "/downloads/", None),
        ];
        for (path, location, expected) in rows {
            assert_eq!(
                local_path(path, location, mount),
                expected.map(PathBuf::from),
                "{path} under {location}"
            );
        }
    }
}
