//! A plugin's settings panel, served as a sealed page for the admin UI to
//! frame: a fixed wrapper with the plugin's panel script inlined.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    http::{StatusCode, header},
    response::Response,
};

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

/// Format one timestamp as an HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`).
fn http_date(when: SystemTime) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = when
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let time = secs % 86_400;
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let era_days = days + 719_468;
    let era = era_days.div_euclid(146_097);
    let day_of_era = era_days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u64;
    let month = (if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    }) as usize;
    let full_year = if month <= 2 { year + 1 } else { year };
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[(days.rem_euclid(7)) as usize],
        day,
        MONTHS[month - 1],
        full_year,
        time / 3_600,
        (time % 3_600) / 60,
        time % 60
    )
}

/// Serve one enabled plugin's panel page. The entry path resolves inside
/// the plugin's own directory; anything escaping it reads as missing.
pub async fn serve(
    host: &PluginHost,
    config: &ConfigStore,
    name: &str,
    if_none_match: &str,
) -> Result<Response, PluginError> {
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
    serve_panel_file(
        &plugin.directory,
        &normalized,
        name,
        if_none_match,
        &panel_page_policy(config),
    )
    .await
}

async fn serve_panel_file(
    directory: &str,
    entry: &str,
    plugin_name: &str,
    if_none_match: &str,
    policy: &str,
) -> Result<Response, PluginError> {
    let base = tokio::fs::canonicalize(directory)
        .await
        .map_err(|_| PluginError::NotFound)?;
    let mut joined = base.clone();
    for part in entry.split('/') {
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
    let mtime = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    // The build's version is part of the tag: a release that changes the
    // wrapper must not answer 304 for a page cached from the old one.
    let etag = format!(
        "\"{plugin_name}-{}-{:x}-{:x}\"",
        env!("CARGO_PKG_VERSION"),
        metadata.len(),
        mtime
    );
    // The policy is on the 304 too: a cached page takes its headers from
    // whichever answer the browser saw last.
    let builder = Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header("x-content-type-options", "nosniff")
        .header("content-security-policy", policy)
        .header("cache-control", "private, max-age=60, must-revalidate")
        .header("etag", etag.clone())
        .header(
            "last-modified",
            http_date(metadata.modified().unwrap_or(UNIX_EPOCH)),
        );
    let candidates: Vec<&str> = if_none_match
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();
    if candidates.iter().any(|token| {
        *token == "*" || *token == etag || token.strip_prefix("W/") == Some(etag.as_str())
    }) {
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .map_err(|error| PluginError::internal(&error));
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
    builder
        .status(StatusCode::OK)
        .body(Body::from(page))
        .map_err(|error| PluginError::internal(&error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn http_date_matches_the_reference_point() {
        // 2024-01-01T00:00:00Z is a Monday.
        let when = UNIX_EPOCH + Duration::from_secs(1_704_067_200);
        assert_eq!(http_date(when), "Mon, 01 Jan 2024 00:00:00 GMT");
        assert_eq!(http_date(UNIX_EPOCH), "Thu, 01 Jan 1970 00:00:00 GMT");
    }
}
