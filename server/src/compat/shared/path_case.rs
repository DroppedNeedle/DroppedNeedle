//! Case-insensitive path routing for the compat shims.
//!
//! Ports `backend/api/compat/common/path_case.py`. Real Jellyfin
//! (ASP.NET Core) routes case-insensitively and clients rely on it:
//! Feishin POSTs lowercase `/jellyfin/users/authenticatebyname`. Axum
//! matches case-sensitively, so compat paths are canonicalized to the
//! route's registered casing before routing. Reconstruction from the
//! template is lossless because every compat path param is a hex/uuid
//! id, an int, or an already-lowercased value.
//!
//! Must sit OUTSIDE the rate-limit middleware so its exact-path checks
//! see the canonical form.

/// Path prefixes this canonicalization covers.
pub const PREFIXES: [&str; 2] = ["/subsonic", "/jellyfin"];

/// Whether a path falls under compat case handling (matched
/// case-insensitively, like v2's `path.lower().startswith(...)`).
pub fn is_compat_path(path: &str) -> bool {
    let folded = path.to_lowercase();
    PREFIXES.iter().any(|prefix| folded.starts_with(prefix))
}

/// Canonicalize `path` against registered route templates (`{name}`
/// segments match anything and are preserved verbatim; static segments
/// match case-insensitively and are rewritten to registered casing).
/// Returns the canonical path when it differs, else `None` (v2 returns
/// `None` when already canonical, including non-matching paths).
pub fn canonicalize(templates: &[&str], path: &str) -> Option<String> {
    if !is_compat_path(path) {
        return None;
    }
    for template in templates {
        if let Some(canon) = match_template(template, path) {
            if canon != path {
                return Some(canon);
            }
            return None;
        }
    }
    None
}

fn match_template(template: &str, path: &str) -> Option<String> {
    let want: Vec<&str> = template.split('/').collect();
    let got: Vec<&str> = path.split('/').collect();
    if want.len() != got.len() {
        return None;
    }
    let mut out = Vec::with_capacity(want.len());
    for (w, g) in want.iter().zip(got.iter()) {
        if is_param(w) {
            if g.is_empty() {
                return None;
            }
            out.push((*g).to_owned());
        } else if w.eq_ignore_ascii_case(g) {
            out.push((*w).to_owned());
        } else {
            return None;
        }
    }
    Some(out.join("/"))
}

fn is_param(segment: &str) -> bool {
    segment.starts_with('{') && segment.ends_with('}') && segment.len() > 2
}
