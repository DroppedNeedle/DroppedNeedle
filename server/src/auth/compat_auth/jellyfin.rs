//! Jellyfin auth contract: token extraction order, 401s with empty
//! bodies, and the login echo.
//!
//! Tokens arrive as `Token="..."` inside `Authorization` (or the legacy
//! `X-Emby-Authorization`), as `X-Emby-Token` / `X-MediaBrowser-Token`,
//! or as `?ApiKey=` / `?api_key=`, in that order. Unknown or missing
//! tokens fail 401 and never raise. `AuthenticateByName` takes the app
//! password as `Pw` and echoes it verbatim as `AccessToken` alongside a
//! fresh `SessionInfo` and the full non-null user object strict clients
//! (Finamp, Manet) hard-cast on.

use sha2::{Digest, Sha256};

/// Stable server id: `sha256("droppedneedle-jellyfin-server")` hex,
/// first 32 chars (v2 `models.py` parity, restart-stable).
pub fn server_id() -> String {
    format!("{:x}", Sha256::digest(b"droppedneedle-jellyfin-server"))[..32].to_owned()
}

/// Unauthorized, the only auth failure status. Bodies are always empty.
pub const UNAUTHORIZED: u16 = 401;

/// v2 length caps, kept verbatim.
pub const MAX_USERNAME_LENGTH: usize = 256;
/// v2 length caps, kept verbatim.
pub const MAX_AUTH_VALUE_LENGTH: usize = 1024;
/// v2 length caps, kept verbatim.
pub const MAX_CLIENT_NAME_LENGTH: usize = 256;

/// Minimal user view the login echo needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinUser {
    /// `auth_users.id`.
    pub id: String,
    /// Lowercased login identifier.
    pub username: Option<String>,
    /// Preferred casing for display.
    pub username_display: Option<String>,
    /// Display name fallback.
    pub display_name: String,
    /// `user`, `trusted`, or `admin`.
    pub role: String,
}

/// Display name rule: display casing, else username, else display name.
/// Empty strings count as absent (v2 `or`-chain parity).
pub fn effective_name(user: &JellyfinUser) -> &str {
    user.username_display
        .as_deref()
        .filter(|name| !name.is_empty())
        .or_else(|| user.username.as_deref().filter(|name| !name.is_empty()))
        .unwrap_or(user.display_name.as_str())
}

/// Auth-relevant request inputs for extraction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JellyfinRequest<'a> {
    /// `Authorization` header value.
    pub authorization: Option<&'a str>,
    /// Legacy `X-Emby-Authorization` header value.
    pub emby_authorization: Option<&'a str>,
    /// `X-Emby-Token` header value.
    pub emby_token: Option<&'a str>,
    /// `X-MediaBrowser-Token` header value.
    pub mediabrowser_token: Option<&'a str>,
    /// `?ApiKey=` query value.
    pub query_apikey: Option<&'a str>,
    /// `?api_key=` query value.
    pub query_api_key: Option<&'a str>,
}

fn media_browser_header<'a>(request: &'a JellyfinRequest<'a>) -> &'a str {
    request
        .authorization
        .filter(|header| !header.is_empty())
        .or_else(|| {
            request
                .emby_authorization
                .filter(|header| !header.is_empty())
        })
        .unwrap_or("")
}

/// Find `Key="value"` in a header (first match, v2 regex parity).
fn find_quoted(header: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = header.find(needle.as_str())? + needle.len();
    let end = header[start..].find('"')? + start;
    Some(header[start..end].to_owned())
}

/// Extract the token: header `Token="..."` (non-empty), then the two
/// direct headers, then the two query keys. Empty values fall through.
pub fn extract_token(request: &JellyfinRequest) -> Option<String> {
    let header = media_browser_header(request);
    if !header.is_empty()
        && let Some(token) = find_quoted(header, "Token")
        && !token.is_empty()
    {
        return Some(token);
    }
    for value in [request.emby_token, request.mediabrowser_token]
        .into_iter()
        .flatten()
    {
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    for value in [request.query_apikey, request.query_api_key]
        .into_iter()
        .flatten()
    {
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

/// Extract `Client="..."` from the media-browser header.
pub fn extract_client(request: &JellyfinRequest) -> Option<String> {
    find_quoted(media_browser_header(request), "Client")
}

/// Extract `(Device, DeviceId)` from the media-browser header.
pub fn extract_device(request: &JellyfinRequest) -> (Option<String>, Option<String>) {
    let header = media_browser_header(request);
    (
        find_quoted(header, "Device"),
        find_quoted(header, "DeviceId"),
    )
}

/// Store failure. The router maps this to 500 with an empty body, never
/// to 401: a broken store must not look like bad credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinStoreError;

/// App-password lookups. The production adapter reads
/// `connect_app_passwords` and `auth_users` only: account passwords and
/// native tokens are unreachable here by construction, so presenting one
/// fails exactly like an unknown credential (401, empty body).
pub trait JellyfinPasswordStore: Clone + Send + Sync + 'static {
    /// User for a presented token (SHA-256 lookup inside).
    fn user_for_token(
        &self,
        token: &str,
    ) -> impl Future<Output = Result<Option<JellyfinUser>, JellyfinStoreError>> + Send;

    /// User for a username plus app password. The username rule is v2
    /// verbatim: the stored lowercased username must equal the input
    /// stripped and lowercased (display names never match).
    fn user_for_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> impl Future<Output = Result<Option<JellyfinUser>, JellyfinStoreError>> + Send;

    /// Best-effort use stamp (production throttles to ~5 min per secret).
    fn note_use(
        &self,
        secret_plaintext: &str,
        client: Option<&str>,
    ) -> impl Future<Output = ()> + Send;
}

/// Auth denial: always 401 with an empty body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JellyfinDenied {
    status: u16,
}

impl JellyfinDenied {
    /// The 401 denial.
    pub fn unauthorized() -> Self {
        Self {
            status: UNAUTHORIZED,
        }
    }

    /// The credential store failed: 500, never a 401 that would count
    /// toward the caller's lockout.
    pub fn unavailable() -> Self {
        Self { status: 500 }
    }

    /// The wire status (401, or 500 for a store failure).
    pub fn status(&self) -> u16 {
        self.status
    }

    /// The wire body (always empty).
    pub fn body(&self) -> &'static [u8] {
        b""
    }
}

/// `AuthenticateByName`: check the username plus app password.
pub async fn authenticate_by_name<S: JellyfinPasswordStore>(
    store: &S,
    username: &str,
    password: &str,
    client: Option<&str>,
) -> Result<JellyfinUser, JellyfinDenied> {
    if password.is_empty()
        || password.len() > MAX_AUTH_VALUE_LENGTH
        || username.len() > MAX_USERNAME_LENGTH
        || client.is_some_and(|value| value.len() > MAX_CLIENT_NAME_LENGTH)
    {
        return Err(JellyfinDenied::unauthorized());
    }
    let user = store
        .user_for_credentials(username, password)
        .await
        .map_err(|_| JellyfinDenied::unavailable())?;
    match user {
        Some(user) => {
            store.note_use(password, client).await;
            Ok(user)
        }
        None => Err(JellyfinDenied::unauthorized()),
    }
}

/// Resolve a request token to its user. `None`, empty, overlong, and
/// unknown tokens all deny identically.
pub async fn resolve_token<S: JellyfinPasswordStore>(
    store: &S,
    token: Option<&str>,
) -> Result<JellyfinUser, JellyfinDenied> {
    let Some(token) = token else {
        return Err(JellyfinDenied::unauthorized());
    };
    if token.is_empty() || token.len() > MAX_AUTH_VALUE_LENGTH {
        return Err(JellyfinDenied::unauthorized());
    }
    let user = store
        .user_for_token(token)
        .await
        .map_err(|_| JellyfinDenied::unavailable())?;
    match user {
        Some(user) => {
            store.note_use(token, None).await;
            Ok(user)
        }
        None => Err(JellyfinDenied::unauthorized()),
    }
}

/// Facts for the fresh `SessionInfo` in the login echo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    /// Random uuid-hex session id (caller-generated).
    pub id: String,
    /// Client label from the auth header.
    pub client: Option<String>,
    /// Device name from the auth header.
    pub device_name: Option<String>,
    /// Device id from the auth header.
    pub device_id: Option<String>,
    /// UTC ISO-8601 activity timestamp with microseconds, v2
    /// `datetime.now(timezone.utc).isoformat()` shape
    /// (`2026-09-28T12:00:00.123456+00:00`). The Jellyfin router formats
    /// it; this contract only pins the shape.
    pub last_activity: String,
}

/// Render the login echo: full non-null user object, the app password
/// echoed verbatim as `AccessToken`, fresh `SessionInfo`. Key order and
/// `None`-stripping match v2 (`msgspec` struct order, `None` dropped).
pub fn login_echo_json(user: &JellyfinUser, access_token: &str, session: &SessionFacts) -> String {
    let server_id = server_id();
    let name = effective_name(user);
    let is_admin = user.role == "admin";
    let mut out = String::with_capacity(2048);
    out.push_str("{\"User\":{");
    push_kv(&mut out, true, "Id", &user.id);
    push_kv(&mut out, false, "Name", name);
    push_kv(&mut out, false, "ServerId", &server_id);
    push_bool(&mut out, false, "HasPassword", true);
    push_bool(&mut out, false, "HasConfiguredPassword", true);
    push_bool(&mut out, false, "HasConfiguredEasyPassword", false);
    out.push_str(",\"Configuration\":{");
    push_bool(&mut out, true, "PlayDefaultAudioTrack", true);
    push_bool(&mut out, false, "DisplayMissingEpisodes", false);
    push_raw(&mut out, false, "GroupedFolders", "[]");
    push_kv(&mut out, false, "SubtitleMode", "Default");
    push_bool(&mut out, false, "DisplayCollectionsView", false);
    push_bool(&mut out, false, "EnableLocalPassword", false);
    push_raw(&mut out, false, "OrderedViews", "[]");
    push_raw(&mut out, false, "LatestItemsExcludes", "[]");
    push_raw(&mut out, false, "MyMediaExcludes", "[]");
    push_bool(&mut out, false, "HidePlayedInLatest", true);
    push_bool(&mut out, false, "RememberAudioSelections", true);
    push_bool(&mut out, false, "RememberSubtitleSelections", true);
    push_bool(&mut out, false, "EnableNextEpisodeAutoPlay", true);
    out.push_str("},\"Policy\":{");
    push_bool(&mut out, true, "IsAdministrator", is_admin);
    push_bool(&mut out, false, "IsHidden", false);
    push_bool(&mut out, false, "IsDisabled", false);
    push_bool(&mut out, false, "EnableAllFolders", true);
    push_raw(&mut out, false, "EnabledFolders", "[]");
    push_bool(&mut out, false, "EnableAllChannels", true);
    push_raw(&mut out, false, "EnabledChannels", "[]");
    push_bool(&mut out, false, "EnableAllDevices", true);
    push_raw(&mut out, false, "EnabledDevices", "[]");
    push_bool(&mut out, false, "EnableMediaPlayback", true);
    push_bool(&mut out, false, "EnableAudioPlaybackTranscoding", true);
    push_bool(&mut out, false, "EnableVideoPlaybackTranscoding", true);
    push_bool(&mut out, false, "EnablePlaybackRemuxing", true);
    push_bool(&mut out, false, "EnableContentDownloading", true);
    push_bool(&mut out, false, "EnableRemoteAccess", true);
    push_bool(&mut out, false, "EnableSyncTranscoding", true);
    push_bool(&mut out, false, "EnableUserPreferenceAccess", true);
    push_bool(&mut out, false, "EnableLiveTvAccess", false);
    push_bool(&mut out, false, "EnableLiveTvManagement", false);
    push_bool(&mut out, false, "EnableContentDeletion", false);
    push_bool(&mut out, false, "EnableMediaConversion", false);
    push_bool(&mut out, false, "EnablePublicSharing", false);
    push_bool(&mut out, false, "EnableRemoteControlOfOtherUsers", false);
    push_bool(&mut out, false, "EnableSharedDeviceControl", false);
    push_raw(&mut out, false, "InvalidLoginAttemptCount", "0");
    push_raw(&mut out, false, "RemoteClientBitrateLimit", "0");
    push_kv(&mut out, false, "SyncPlayAccess", "CreateAndJoinGroups");
    push_raw(&mut out, false, "BlockedTags", "[]");
    push_raw(&mut out, false, "AllowedTags", "[]");
    push_raw(&mut out, false, "AccessSchedules", "[]");
    push_raw(&mut out, false, "BlockUnratedItems", "[]");
    out.push_str("}},");
    push_kv(&mut out, true, "AccessToken", access_token);
    out.push_str(",\"SessionInfo\":{");
    push_kv(&mut out, true, "Id", &session.id);
    push_kv(&mut out, false, "UserId", &user.id);
    push_kv(&mut out, false, "UserName", name);
    push_kv(&mut out, false, "LastActivityDate", &session.last_activity);
    if let Some(client) = session.client.as_deref() {
        push_kv(&mut out, false, "Client", client);
    }
    push_kv(
        &mut out,
        false,
        "DeviceName",
        session.device_name.as_deref().unwrap_or(""),
    );
    if let Some(device_id) = session.device_id.as_deref() {
        push_kv(&mut out, false, "DeviceId", device_id);
    }
    push_bool(&mut out, false, "IsActive", true);
    push_bool(&mut out, false, "SupportsRemoteControl", false);
    push_bool(&mut out, false, "SupportsMediaControl", false);
    push_bool(&mut out, false, "HasCustomDeviceName", false);
    push_kv(&mut out, false, "ServerId", &server_id);
    out.push_str("},");
    push_kv(&mut out, true, "ServerId", &server_id);
    out.push('}');
    out
}

fn push_kv(out: &mut String, first: bool, key: &str, value: &str) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(key);
    out.push_str("\":\"");
    json_escape_into(out, value);
    out.push('"');
}

fn push_bool(out: &mut String, first: bool, key: &str, value: bool) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(if value { "true" } else { "false" });
}

fn push_raw(out: &mut String, first: bool, key: &str, raw: &str) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(raw);
}

fn json_escape_into(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_id_is_restart_stable_and_pinned() {
        assert_eq!(server_id(), "2f54b621b8fde6d933fab26bd6378f85");
    }

    #[test]
    fn token_extraction_follows_v2_order() {
        let header = "MediaBrowser Token=\"abc\", Client=\"Finamp\"";
        let request = JellyfinRequest {
            authorization: Some(header),
            query_apikey: Some("query-wins-never"),
            ..JellyfinRequest::default()
        };
        assert_eq!(extract_token(&request).as_deref(), Some("abc"));
        assert_eq!(extract_client(&request).as_deref(), Some("Finamp"));
    }

    #[test]
    fn empty_token_capture_falls_through() {
        let request = JellyfinRequest {
            authorization: Some("MediaBrowser Token=\"\""),
            emby_token: Some("direct"),
            ..JellyfinRequest::default()
        };
        assert_eq!(extract_token(&request).as_deref(), Some("direct"));
    }
}
