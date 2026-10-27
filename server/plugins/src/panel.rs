//! A plugin's settings panel, served as a sealed page for the admin UI to
//! frame: a fixed wrapper with the plugin's panel script inlined.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use droppedneedle_kernel::runtime_config::ConfigStore;

use super::error::PluginError;
use super::host::PluginHost;

/// The fixed document around a plugin's panel script.
const PANEL_PAGE_HEAD: &[u8] = b"<!doctype html><html><head><meta charset=\"utf-8\">\
<style>body{font:13px/1.5 system-ui,sans-serif;margin:0;padding:12px;color:#111;background:#fff}\
#panel-root{min-height:40px}</style></head><body><div id=\"panel-root\"></div><script>";
const PANEL_PAGE_TAIL: &[u8] = b"</script></body></html>";

/// What the panel page may do. It is plugin code shown inside the admin UI,
/// so the page is sealed: `sandbox allow-scripts` gives it an origin of its
/// own with no cookies or storage, even when the address is opened in its
/// own tab, and only the app may frame it, along with whatever an admin
/// allowed to frame the app. Inline script and style run, pictures come from
/// `data:` and `blob:` only, and nothing else loads or connects. Never add
/// `allow-same-origin`: the script would then run as the app, with the
/// admin's session.
fn panel_page_policy(config: &ConfigStore) -> String {
    format!(
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
         img-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; \
         base-uri 'none'; form-action 'none'; frame-ancestors {}; sandbox allow-scripts",
        droppedneedle_kernel::runtime_config::sections::framed_page_ancestors(&config.live())
    )
}

/// The panel script with every `</script` (any case) broken up, so its text
/// cannot close the element it is inlined into. `<\/script` reads the same
/// inside a JavaScript string or regular expression. Not covered: a script
/// holding `<!--` and then `<script` makes the parser run past the closing
/// tag, which only breaks that plugin's own panel. Every other byte passes
/// through unchanged.
fn inline_panel_script(script: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(script.len() + 16);
    let mut rest = script;
    while let Some((&byte, tail)) = rest.split_first() {
        out.push(byte);
        if byte == b'<'
            && tail.first() == Some(&b'/')
            && tail
                .get(1..7)
                .is_some_and(|name| name.eq_ignore_ascii_case(b"script"))
        {
            out.push(b'\\');
        }
        rest = tail;
    }
    out
}

/// One panel answer. `page` is `None` when the caller's copy is current.
#[derive(Debug)]
pub struct PanelPage {
    /// What the page may do, sent on the 304 too: a cached page takes its
    /// headers from whichever answer the browser saw last.
    pub policy: String,
    /// The page's entity tag.
    pub etag: String,
    /// When the panel script last changed, as an HTTP date.
    pub last_modified: String,
    /// The page, unless the caller's copy is current.
    pub page: Option<Vec<u8>>,
}

/// One enabled plugin's panel page. The entry path resolves inside the
/// plugin's own directory; anything escaping it reads as missing.
pub async fn serve(
    host: &PluginHost,
    config: &ConfigStore,
    name: &str,
    if_none_match: &str,
) -> Result<PanelPage, PluginError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
        || name.contains('\0')
    {
        return Err(PluginError::NotFound);
    }
    let plugin = match host.get(name) {
        Some(plugin) if plugin.enabled && plugin.runtime.is_some() => plugin,
        _ => return Err(PluginError::NotFound),
    };
    let entry = plugin.manifest.ui_entry.clone();
    if entry.is_empty() {
        return Err(PluginError::NotFound);
    }
    let normalized = entry.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.starts_with("http://")
        || normalized.starts_with("https://")
        || normalized.split('/').any(|part| part == "..")
        || normalized.contains('\0')
    {
        return Err(PluginError::NotFound);
    }
    if plugin.directory.is_empty() {
        return Err(PluginError::NotFound);
    }
    let base = tokio::fs::canonicalize(&plugin.directory)
        .await
        .map_err(|_| PluginError::NotFound)?;
    let mut joined = base.clone();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(PluginError::NotFound);
        }
        joined.push(part);
    }
    let target = tokio::fs::canonicalize(&joined)
        .await
        .map_err(|_| PluginError::NotFound)?;
    if target != base && !target.starts_with(&base) {
        return Err(PluginError::NotFound);
    }
    let metadata = tokio::fs::metadata(&target)
        .await
        .map_err(|_| PluginError::NotFound)?;
    if !metadata.is_file() {
        return Err(PluginError::NotFound);
    }
    // The manifest entry is static and the plugin dir is admin-managed, so
    // the check-then-read below races with nothing untrusted.
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    let mtime = modified
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    // The build's version is part of the tag: a release that changes the
    // wrapper must not answer 304 for a page cached from the old one.
    let etag = format!(
        "\"{name}-{}-{:x}-{:x}\"",
        env!("CARGO_PKG_VERSION"),
        metadata.len(),
        mtime
    );
    let mut answer = PanelPage {
        policy: panel_page_policy(config),
        etag,
        last_modified: http_date(modified),
        page: None,
    };
    let current = if_none_match
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .any(|token| {
            token == "*" || token == answer.etag || token.strip_prefix("W/") == Some(&answer.etag)
        });
    if current {
        return Ok(answer);
    }
    let bytes = tokio::fs::read(&target)
        .await
        .map_err(|_| PluginError::NotFound)?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(PluginError::NotFound);
    }
    let mut page = PANEL_PAGE_HEAD.to_vec();
    page.extend(inline_panel_script(&bytes));
    page.extend_from_slice(PANEL_PAGE_TAIL);
    answer.page = Some(page);
    Ok(answer)
}

/// One timestamp as an HTTP date, held to the years the format can write.
fn http_date(when: SystemTime) -> String {
    // 9999-12-31T23:59:59Z, the last moment `httpdate` writes.
    const LAST: Duration = Duration::from_secs(253_402_300_799);
    let since_epoch = when.duration_since(UNIX_EPOCH).unwrap_or_default();
    httpdate::fmt_http_date(UNIX_EPOCH + since_epoch.min(LAST))
}
