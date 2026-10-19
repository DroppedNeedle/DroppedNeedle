//! The shipping source of the workspace, for gates that read code as text:
//! every `.rs` file under the `src/` of each member that `server/Cargo.toml`
//! lists, except the testkit's mock servers. A new crate is walked without
//! editing any gate, no `tests/` or build folder is ever reached, no
//! folder is skipped by name, and a member whose `src/` holds no source
//! fails the walk rather than shrinking it.

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
    let dirs: Vec<PathBuf> = members
        .iter()
        .map(|member| member.as_str().expect("member path"))
        .filter(|member| *member != "testkit")
        .map(|member| server.join(member).join("src"))
        .collect();
    assert!(dirs.len() > 1, "the workspace lists its crates");
    let mut sources = Vec::new();
    for src in dirs {
        let before = sources.len();
        let mut pending = vec![src.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).expect("src lists") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                    sources.push(Source {
                        path: relative(&server, &path),
                        text: std::fs::read_to_string(&path).expect("source reads"),
                    });
                }
            }
        }
        assert!(
            sources.len() > before,
            "{} holds no source file",
            relative(&server, &src)
        );
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

impl Source {
    /// The file with comment lines blanked, so docs may name the rules a
    /// gate looks for. Line numbers still point at the source.
    pub fn code(&self) -> String {
        self.text
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("//") {
                    ""
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// [`Self::code`] with each inline `#[cfg(test)] mod name { .. }` block
    /// blanked, wherever it sits. A `mod name;` declaration is kept: the
    /// file it names is walked on its own. Line numbers still point at the
    /// source.
    pub fn shipping_code(&self) -> String {
        let code = self.code();
        let mut lines: Vec<&str> = code.lines().collect();
        let mut at = 0;
        while at < lines.len() {
            if lines[at].trim() != "#[cfg(test)]" {
                at += 1;
                continue;
            }
            let mut head = at + 1;
            while lines
                .get(head)
                .is_some_and(|line| line.trim().is_empty() || line.trim().starts_with("#["))
            {
                head += 1;
            }
            let Some(line) = lines.get(head) else {
                break;
            };
            if !is_mod_line(line) || !line.contains('{') {
                at = head;
                continue;
            }
            let end = block_end(&lines, head);
            for blank in &mut lines[at..=end] {
                *blank = "";
            }
            at = end + 1;
        }
        lines.join("\n")
    }
}

/// `mod name`, with any visibility.
fn is_mod_line(line: &str) -> bool {
    let line = line.trim_start();
    let line = match line.strip_prefix("pub") {
        Some(rest) if rest.starts_with('(') => rest.find(')').map_or(rest, |at| &rest[at + 1..]),
        Some(rest) => rest,
        None => line,
    };
    line.trim_start().starts_with("mod ")
}

/// The line on which the block opened on line `start` closes. Braces in
/// strings, character literals and comments do not count; a block left
/// open runs to the end of the file.
fn block_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0usize;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut in_raw: Option<String> = None;
    for (index, line) in lines.iter().enumerate().skip(start) {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if let Some(close) = &in_raw {
                let ahead: String = chars[i..].iter().take(close.chars().count()).collect();
                if &ahead == close {
                    i += close.chars().count() - 1;
                    in_raw = None;
                }
            } else if in_block_comment {
                if c == '*' && next == Some('/') {
                    in_block_comment = false;
                    i += 1;
                }
            } else if in_string {
                if c == '\\' {
                    i += 1;
                } else if c == '"' {
                    in_string = false;
                }
            } else if c == '/' && next == Some('/') {
                break;
            } else if c == '/' && next == Some('*') {
                in_block_comment = true;
                i += 1;
            } else if c == 'r'
                && (next == Some('"') || next == Some('#'))
                && !chars[..i]
                    .last()
                    .is_some_and(|&p| p.is_alphanumeric() || p == '_')
            {
                // A raw string, which may span lines.
                let hashes = chars[i + 1..].iter().take_while(|&&h| h == '#').count();
                if chars.get(i + 1 + hashes) == Some(&'"') {
                    let close: String = std::iter::once('"')
                        .chain(std::iter::repeat_n('#', hashes))
                        .collect();
                    in_raw = Some(close);
                    i += 1 + hashes;
                }
            } else if c == '"' {
                in_string = true;
            } else if c == '\'' && next == Some('\\') {
                // An escaped character literal such as '\''.
                if let Some(close) = chars[i + 2..].iter().position(|&q| q == '\'') {
                    i += 2 + close;
                }
            } else if c == '\'' && chars.get(i + 2) == Some(&'\'') {
                // A character literal such as '{' (a lifetime has no
                // closing quote).
                i += 2;
            } else if c == '{' {
                depth += 1;
            } else if c == '}' {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index;
                }
            }
            i += 1;
        }
    }
    lines.len() - 1
}
