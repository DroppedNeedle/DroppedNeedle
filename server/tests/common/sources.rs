//! The shipping source of the workspace, for gates that read code as text:
//! every `.rs` file under the `src/` of each member that `server/Cargo.toml`
//! lists, except the testkit's mock servers. A new crate is walked without
//! editing any gate, no `tests/` or build folder is ever reached, and no
//! folder is skipped by name.

use std::path::{Component, Path, PathBuf};

/// One source file.
pub struct Source {
    /// Path from `server/`, with `/` separators: `auth/src/lib.rs`,
    /// `src/app.rs`.
    pub path: String,
    /// The whole file, comments included.
    pub text: String,
}

/// Every shipping source file of the workspace, sorted by path. For the
/// server package's test binaries, whose manifest directory is `server/`.
pub fn workspace_sources() -> Vec<Source> {
    let server = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(server.join("Cargo.toml")).expect("manifest reads");
    let manifest: toml::Table = manifest.parse().expect("manifest parses");
    let members = manifest["workspace"]["members"]
        .as_array()
        .expect("workspace members");
    let mut dirs: Vec<PathBuf> = members
        .iter()
        .map(|member| member.as_str().expect("member path"))
        .filter(|member| *member != "testkit")
        .map(|member| server.join(member).join("src"))
        .collect();
    assert!(dirs.len() > 1, "the workspace lists its crates");
    let mut sources = Vec::new();
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("src lists") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                sources.push(Source {
                    path: relative(&server, &path),
                    text: std::fs::read_to_string(&path).expect("source reads"),
                });
            }
        }
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    sources
}

fn relative(server: &Path, path: &Path) -> String {
    let path = path.strip_prefix(server).expect("source under server/");
    path.components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}
