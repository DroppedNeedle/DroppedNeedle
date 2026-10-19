//! Case-insensitive path routing for the compat shims.
//!
//! Ports v2's compat path-case middleware. Real Jellyfin
//! (ASP.NET Core) routes case-insensitively and clients rely on it:
//! Feishin POSTs lowercase `/jellyfin/users/authenticatebyname`. Axum
//! matches case-sensitively, so compat paths are canonicalized to the
//! route's registered casing before routing. Reconstruction from the
//! template is lossless because every compat path param is a hex/uuid
//! id, an int, or an already-lowercased value.
//!
//! Must sit outside the rate-limit middleware so its exact-path checks
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
/// When several templates match, the one the router would pick wins: at
/// the first segment where they differ, a static segment beats a
/// parameter, so `items/latest` is `Items/Latest`, never an item id.
/// Returns the canonical path when it differs, else `None` (v2 returns
/// `None` when already canonical, including non-matching paths).
pub fn canonicalize(templates: &[&str], path: &str) -> Option<String> {
    if !is_compat_path(path) {
        return None;
    }
    let mut best: Option<(Vec<bool>, String)> = None;
    for template in templates {
        let Some(canon) = match_template(template, path) else {
            continue;
        };
        let rank: Vec<bool> = template.split('/').map(|w| !is_param(w)).collect();
        if best.as_ref().is_none_or(|(top, _)| rank > *top) {
            best = Some((rank, canon));
        }
    }
    best.map(|(_, canon)| canon).filter(|canon| canon != path)
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

#[cfg(test)]
mod tests {
    use super::canonicalize;

    /// A static segment wins over a parameter whatever the registration
    /// order, so lower-case `latest`, `filters` and `me` reach their own
    /// handlers rather than being read as ids.
    #[test]
    fn a_static_segment_beats_a_parameter() {
        let templates = [
            "/jellyfin/Items/{item_id}",
            "/jellyfin/Items/Latest",
            "/jellyfin/Users/{user_id}",
            "/jellyfin/Users/Me",
        ];
        let canon = |path| canonicalize(&templates, path);
        assert_eq!(
            canon("/jellyfin/items/latest").as_deref(),
            Some("/jellyfin/Items/Latest")
        );
        assert_eq!(
            canon("/jellyfin/users/me").as_deref(),
            Some("/jellyfin/Users/Me")
        );
        assert_eq!(
            canon("/jellyfin/items/ab12").as_deref(),
            Some("/jellyfin/Items/ab12")
        );
        assert_eq!(canon("/jellyfin/Items/Latest"), None);
    }
}
