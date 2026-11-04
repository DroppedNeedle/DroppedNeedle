//! Case-insensitive path routing for the compat shims.
//!
//! Real Jellyfin
//! (ASP.NET Core) routes case-insensitively and clients rely on it:
//! Feishin POSTs lowercase `/jellyfin/users/authenticatebyname`. Axum
//! matches case-sensitively, so compat paths are canonicalized to the
//! route's registered casing before routing. Reconstruction from the
//! template is lossless because every compat path param is a hex/uuid
//! id, an int, or an already-lowercased value.
//!
//! Runs before routing, so the rate limits and the handlers see the
//! canonical form.

/// Path prefixes this canonicalization covers.
pub const PREFIXES: [&str; 2] = ["/subsonic", "/jellyfin"];

/// Whether a path falls under compat case handling (matched
/// case-insensitively).
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
/// Returns the canonical path when it differs, else `None`. Paths that are
/// already canonical and paths that match no template both give `None`.
pub fn canonicalize(templates: &[&str], path: &str) -> Option<String> {
    if !is_compat_path(path) {
        return None;
    }
    let got: Vec<&str> = path.split('/').collect();
    let mut best: Option<(Vec<bool>, &str)> = None;
    for template in templates {
        if !matches_template(template, &got) {
            continue;
        }
        let rank: Vec<bool> = template.split('/').map(|w| !is_param(w)).collect();
        if best.as_ref().is_none_or(|(top, _)| rank > *top) {
            best = Some((rank, template));
        }
    }
    let (_, template) = best?;
    let canon = template
        .split('/')
        .zip(&got)
        .map(|(w, g)| if is_param(w) { *g } else { w })
        .collect::<Vec<_>>()
        .join("/");
    (canon != path).then_some(canon)
}

/// Whether `got`, a path split on `/`, fits `template`: as many segments,
/// each parameter non-empty and each static segment equal ignoring case.
fn matches_template(template: &str, got: &[&str]) -> bool {
    let mut want = template.split('/');
    let fits = got.iter().all(|g| match want.next() {
        Some(w) if is_param(w) => !g.is_empty(),
        Some(w) => w.eq_ignore_ascii_case(g),
        None => false,
    });
    fits && want.next().is_none()
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
