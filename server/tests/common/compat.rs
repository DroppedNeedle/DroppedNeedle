//! The compat APIs over a library the app scanned itself: the committed
//! fixture tracks planted as `reference/compat/library.json` lays them out,
//! both protocols turned on in Settings, and an app password made by the
//! user who plays. Also the golden files those journeys compare against.

use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use droppedneedle_compat::http::redact_target;
use serde_json::{Value, json};

use super::TestApp;

/// The account that plays.
pub const PLAYER: &str = "alice";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn goldens_dir() -> PathBuf {
    manifest_dir().join("tests/fixtures/compat")
}

/// Whether this run rewrites the goldens, from the values of `COMPAT_BLESS`
/// and `CI`. A CI run never rewrites one: with both set the answer is the
/// refusal, so a bless can neither pass as a compare nor commit itself.
pub fn bless_requested(compat_bless: Option<&str>, ci: Option<&str>) -> Result<bool, String> {
    let blessing = compat_bless.is_some();
    let in_ci = ci.is_some_and(|value| !value.is_empty());
    if blessing && in_ci {
        return Err(
            "COMPAT_BLESS and CI are both set: goldens are never rewritten in CI. \
             Unset COMPAT_BLESS here, or bless on a workstation and commit the diff."
                .to_owned(),
        );
    }
    Ok(blessing)
}

/// [`bless_requested`] for this process. Panics on the refusal.
pub fn blessing() -> bool {
    let read = |name: &str| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned());
    match bless_requested(read("COMPAT_BLESS").as_deref(), read("CI").as_deref()) {
        Ok(blessing) => blessing,
        Err(refusal) => panic!("{refusal}"),
    }
}

/// The app with the reference library scanned and both protocols on.
pub struct CompatApp {
    pub app: TestApp,
    /// The player's app password.
    pub secret: String,
    /// The player's id as the Jellyfin API names it.
    pub user_id: String,
    music: PathBuf,
    layout: Value,
}

impl CompatApp {
    /// The app with no ffmpeg, so it never transcodes: every answer is the
    /// same on a host with ffmpeg and one without.
    pub async fn start(tag: &str) -> Self {
        Self::start_with(tag, super::closed_outbound()).await
    }

    /// As [`CompatApp::start`], transcoding through the ffmpeg stand-in
    /// with the default policy (on, mp3, at most 320 kbps).
    pub async fn transcoding(tag: &str) -> Self {
        let mut outbound = super::closed_outbound();
        outbound.ffmpeg = Some(super::ffmpeg_stand_in());
        Self::start_with(tag, outbound).await
    }

    /// As [`CompatApp::start`], reaching the outside world as `outbound`
    /// says.
    pub async fn start_with(tag: &str, outbound: droppedneedle::bootstrap::Outbound) -> Self {
        Self::planted(tag, outbound, &[]).await
    }

    /// As [`CompatApp::start_with`], with the other files the reference
    /// recording `folder` planted beside the tracks (its `capture.json`
    /// names them under `extra_files`).
    pub async fn start_as_recorded(
        tag: &str,
        outbound: droppedneedle::bootstrap::Outbound,
        folder: &str,
    ) -> Self {
        let capture: Value = serde_json::from_slice(
            &std::fs::read(
                manifest_dir()
                    .join("tests/reference/compat")
                    .join(folder)
                    .join("capture.json"),
            )
            .expect("capture reads"),
        )
        .expect("capture parses");
        let extras = capture["extra_files"]
            .as_array()
            .expect("the recording names extra files")
            .clone();
        Self::planted(tag, outbound, &extras).await
    }

    /// Plant the layout's tracks plus `extras` (`{fixture, path}`), then
    /// scan them.
    async fn planted(
        tag: &str,
        outbound: droppedneedle::bootstrap::Outbound,
        extras: &[Value],
    ) -> Self {
        let app = TestApp::start_with(tag, outbound).await;
        let layout: Value = serde_json::from_slice(
            &std::fs::read(manifest_dir().join("tests/reference/compat/library.json"))
                .expect("library layout reads"),
        )
        .expect("library layout parses");
        let fixtures = manifest_dir().join("tests/fixtures/library");
        let music = app.config.root_app_dir.join("music");
        let tracks = layout["tracks"].as_array().expect("tracks");
        for file in tracks.iter().chain(extras) {
            let planted = music.join(file["path"].as_str().expect("planted path"));
            std::fs::create_dir_all(planted.parent().expect("album folder")).expect("mkdir");
            std::fs::copy(
                fixtures.join(file["fixture"].as_str().expect("fixture name")),
                &planted,
            )
            .expect("fixture copies");
        }
        app.add_library_folder("music", &music).await;
        Self::signed_in(app, music, layout).await
    }

    /// The app with both protocols on and no library folder: the test
    /// writes the catalog rows it reads.
    pub async fn unscanned(tag: &str) -> Self {
        let app = TestApp::start(tag).await;
        let music = app.config.root_app_dir.join("music");
        Self::signed_in(app, music, json!({ "tracks": [] })).await
    }

    /// Turn both protocols on and give the player an app password. A slow
    /// host can answer these busy, so each is asked again until it is
    /// carried out.
    async fn signed_in(app: TestApp, music: PathBuf, layout: Value) -> Self {
        let admin = app.login_admin().await;
        let admin = [("authorization", admin.as_str())];
        let (status, mut apps) = app
            .call_through_busy("GET", "/api/v3/settings/connect-apps", &admin, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{apps}");
        apps["subsonic_enabled"] = json!(true);
        apps["jellyfin_enabled"] = json!(true);
        let (status, saved) = app
            .call_through_busy("PUT", "/api/v3/settings/connect-apps", &admin, Some(apps))
            .await;
        assert_eq!(status, StatusCode::OK, "{saved}");

        let player = app.login_user(PLAYER).await;
        let (status, created) = app
            .call_through_busy(
                "POST",
                "/api/v3/me/app-passwords",
                &[("authorization", player.authorization.as_str())],
                Some(json!({"name": "Journey"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().expect("secret").to_owned();

        let token = format!("MediaBrowser Client=\"Journey\", Token=\"{secret}\"");
        let (status, me) = app
            .call_through_busy(
                "GET",
                "/jellyfin/Users/Me",
                &[("X-Emby-Authorization", token.as_str())],
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{me}");
        let user_id = me["Id"].as_str().expect("jellyfin user id").to_owned();

        let rig = Self {
            app,
            secret,
            user_id,
            music,
            layout,
        };
        rig.jellyfin_ids_caught_up().await;
        rig
    }

    /// Wait until the stored Jellyfin ids are loaded and hold every item
    /// written so far, so a picture request without sign-in finds them.
    pub async fn jellyfin_ids_caught_up(&self) {
        let ids = self.app.state().compat.jellyfin_ids().clone();
        self.app
            .until("the stored jellyfin ids caught up", || {
                let ids = ids.clone();
                async move { (ids.ready() && ids.caught_up().await).then_some(()) }
            })
            .await;
    }

    /// A new app password for the account signed in as `authorization`.
    pub async fn app_password(&self, authorization: &str) -> String {
        let (status, created) = self
            .app
            .call(
                "POST",
                "/api/v3/me/app-passwords",
                &[("authorization", authorization)],
                Some(json!({"name": "Journey"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        created["secret"].as_str().expect("secret").to_owned()
    }

    /// Save the connect-apps settings with the fields of `patch` changed,
    /// as the admin would.
    pub async fn set_connect_apps(&self, patch: Value) {
        let admin = self.app.login_admin().await;
        let admin = [("authorization", admin.as_str())];
        let (status, mut apps) = self
            .app
            .call("GET", "/api/v3/settings/connect-apps", &admin, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{apps}");
        for (key, value) in patch.as_object().expect("patch is an object") {
            apps[key] = value.clone();
        }
        let (status, saved) = self
            .app
            .call("PUT", "/api/v3/settings/connect-apps", &admin, Some(apps))
            .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
    }

    /// What the ffmpeg stand-in ran for sources in the folder of the
    /// layout's track `key`, one `<muxer> <bitrate>` line per run.
    pub fn ffmpeg_runs(&self, key: &str) -> Vec<String> {
        let folder = self.track_path(key);
        let folder = folder.parent().expect("album folder");
        std::fs::read_to_string(folder.join(".ffmpeg-runs"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The admin's app password.
    pub async fn admin_secret(&self) -> String {
        let admin = self.app.login_admin().await;
        self.app_password(&admin).await
    }

    /// A new account with the trusted role, and its app password.
    pub async fn trusted_secret(&self, username: &str) -> String {
        let login = self.app.login_user(username).await;
        let admin = self.app.login_admin().await;
        let (status, body) = self
            .app
            .call(
                "PUT",
                &format!("/api/v3/admin/users/{}/role", login.user_id),
                &[("authorization", admin.as_str())],
                Some(json!({"role": "trusted"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        self.app_password(&login.authorization).await
    }

    /// Turn the online lyrics lookup on or off, as the owner would.
    pub async fn set_lyrics_lookup(&self, enabled: bool) {
        let admin = self.app.login_admin().await;
        let (status, body) = self
            .app
            .call(
                "PUT",
                "/api/v3/settings/lyrics",
                &[("authorization", admin.as_str())],
                Some(json!({ "enabled": enabled })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// Wait until the Jellyfin side marks the track `title` as having
    /// lyrics, which it does once the lyrics sweep after the scan is done.
    pub async fn wait_for_lyrics_mark(&self, title: &str) {
        let auth = self.jellyfin_auth();
        let headers = [("X-Emby-Authorization", auth.as_str())];
        self.app
            .until("the lyrics mark", || async {
                let (status, tracks) = self
                    .app
                    .call(
                        "GET",
                        "/jellyfin/Items?Recursive=true&IncludeItemTypes=Audio",
                        &headers,
                        None,
                    )
                    .await;
                assert_eq!(status, StatusCode::OK, "{tracks}");
                (named(&tracks["Items"], "Name", title)["HasLyrics"] == json!(true)).then_some(())
            })
            .await;
    }

    /// Subsonic query credentials for `client`.
    pub fn subsonic_auth(&self, client: &str) -> String {
        format!("u={PLAYER}&p={}&v=1.16.1&c={client}&f=json", self.secret)
    }

    /// The Jellyfin authorization header value.
    pub fn jellyfin_auth(&self) -> String {
        format!("MediaBrowser Client=\"Journey\", Token=\"{}\"", self.secret)
    }

    /// The bytes of the planted file behind the layout's track `key`.
    pub fn file(&self, key: &str) -> Vec<u8> {
        std::fs::read(self.track_path(key)).expect("planted file reads")
    }

    /// Where the layout's track `key` is planted.
    fn track_path(&self, key: &str) -> PathBuf {
        let track = self.layout["tracks"]
            .as_array()
            .expect("tracks")
            .iter()
            .find(|track| track["key"] == key)
            .expect("track in the layout");
        self.music
            .join(track["path"].as_str().expect("planted path"))
    }

    pub async fn stop(self) {
        self.app.stop().await;
    }
}

/// Subsonic URL for `call` (`endpoint` or `endpoint?query`) plus credentials.
pub fn rest(call: &str, auth: &str) -> String {
    let sep = if call.contains('?') { '&' } else { '?' };
    format!("/subsonic/rest/{call}{sep}{auth}")
}

/// The first object in `items` whose `key` is `name`.
pub fn named<'a>(items: &'a Value, key: &str, name: &str) -> &'a Value {
    items
        .as_array()
        .and_then(|items| items.iter().find(|item| item[key] == name))
        .unwrap_or_else(|| panic!("no {key} {name:?} in {items}"))
}

/// The requests of one journey and what came back, in the step format of
/// the reference recordings.
///
/// What differs from run to run is made stable on the way in: every id
/// becomes a placeholder numbered by first appearance, so answers still
/// refer to each other, and every digit of a timestamp becomes 0, which
/// keeps the format a client parses.
pub struct Trace<'a> {
    rig: &'a CompatApp,
    steps: Vec<Value>,
    ids: Vec<String>,
    id: regex::Regex,
    time: regex::Regex,
}

impl<'a> Trace<'a> {
    pub fn new(rig: &'a CompatApp) -> Self {
        Self {
            rig,
            steps: Vec::new(),
            ids: Vec::new(),
            id: regex::Regex::new(
                "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{32}",
            )
            .expect("id pattern"),
            time: regex::Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:\d\d)?$")
                .expect("time pattern"),
        }
    }

    /// Every step so far.
    pub fn body(&self) -> Value {
        json!({ "steps": self.steps })
    }

    /// The last answer as it was recorded.
    pub fn recorded(&self) -> Value {
        self.steps
            .last()
            .map_or(Value::Null, |step| step["body"].clone())
    }

    pub async fn get(&mut self, uri: &str, headers: &[(&str, &str)]) -> Value {
        self.call("GET", uri, headers, None).await
    }

    /// Send one request, asking again while it answers busy, and record its
    /// status, framing headers and body. Returns the body as it came, for
    /// the ids the next request needs.
    pub async fn call(
        &mut self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        json_body: Option<Value>,
    ) -> Value {
        let (status, header_map, raw) =
            super::through_busy(|| self.rig.app.raw(method, uri, headers, json_body.clone())).await;
        let header = |name: &str| header_map.get(name).and_then(|value| value.to_str().ok());
        let mut headers_out = serde_json::Map::new();
        for name in [
            "content-type",
            "content-length",
            "content-range",
            "accept-ranges",
        ] {
            if let Some(value) = header(name) {
                headers_out.insert(name.to_owned(), Value::String(value.to_owned()));
            }
        }
        let live = if raw.is_empty() {
            Value::Null
        } else if header("content-type").is_some_and(|kind| kind.contains("json")) {
            serde_json::from_slice(&raw).expect("json parses")
        } else {
            json!({ "byte_len": raw.len(), "head": raw.iter().take(8).collect::<Vec<_>>() })
        };
        let mut body = live.clone();
        self.settle(&mut body);
        // The production redactor first, so no trace carries the app
        // password.
        let target = self.settle_text(&redact_target(uri));
        let step = json!({
            "request": format!("{method} {target}"),
            "status": status.as_u16(),
            "headers": headers_out,
            "body": body,
        });
        assert!(
            !step.to_string().contains(&self.rig.secret),
            "the app password reached a trace: {method} {target}"
        );
        self.steps.push(step);
        live
    }

    fn settle(&mut self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, value) in map.iter_mut() {
                    if key == "minutesAgo" {
                        *value = json!("__MINUTES_AGO__");
                    } else {
                        self.settle(value);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.settle(item)),
            Value::String(text) => *text = self.settle_text(text),
            _ => {}
        }
    }

    fn settle_text(&mut self, text: &str) -> String {
        if self.time.is_match(text) {
            return text
                .chars()
                .map(|c| if c.is_ascii_digit() { '0' } else { c })
                .collect();
        }
        let text = mask_api_keys(text);
        let id = self.id.clone();
        id.replace_all(&text, |found: &regex::Captures<'_>| {
            let seen = &found[0];
            let at = self
                .ids
                .iter()
                .position(|id| id == seen)
                .unwrap_or_else(|| {
                    self.ids.push(seen.to_owned());
                    self.ids.len() - 1
                });
            format!("<id-{}>", at + 1)
        })
        .into_owned()
    }
}

/// PlaybackInfo advertises its stream URL with `api_key=<token>`, and a
/// Subsonic picture link carries `sig=<signature>`: both masked as `***`.
fn mask_api_keys(text: &str) -> String {
    ["api_key=", "sig="]
        .iter()
        .fold(text.to_owned(), |text, key| mask_value(&text, key))
}

fn mask_value(text: &str, key: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(key) {
        out.push_str(&rest[..start + key.len()]);
        out.push_str("***");
        rest = &rest[start + key.len()..];
        rest = rest.find('&').map_or("", |end| &rest[end..]);
    }
    out.push_str(rest);
    out
}

/// Compare `actual` with the golden. Leaves match exactly, except a string
/// starting with `re:`, a regex the whole actual string must match.
fn diff_golden(expected: &Value, actual: &Value, path: &str, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(exp), Value::Object(got)) => {
            let mut keys: Vec<&String> = exp.keys().chain(got.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let at = format!("{path}.{key}");
                match (exp.get(key), got.get(key)) {
                    (Some(e), Some(a)) => diff_golden(e, a, &at, out),
                    (Some(_), None) => out.push(format!("{at}: missing in actual")),
                    _ => out.push(format!("{at}: unexpected in actual")),
                }
            }
        }
        (Value::Array(exp), Value::Array(got)) if exp.len() == got.len() => {
            for (i, (e, a)) in exp.iter().zip(got).enumerate() {
                diff_golden(e, a, &format!("{path}[{i}]"), out);
            }
        }
        (Value::String(pattern), Value::String(text)) if pattern.starts_with("re:") => {
            let regex = regex::Regex::new(&format!("^(?:{})$", &pattern[3..])).expect("regex");
            if !regex.is_match(text) {
                out.push(format!("{path}: {text:?} does not match {pattern:?}"));
            }
        }
        (exp, got) if exp != got => out.push(format!("{path}: {got} != {exp}")),
        _ => {}
    }
}

/// Assert `actual` matches the golden called `name`, or rewrite it under
/// `COMPAT_BLESS=1`. `name` is a bare file name, so nothing outside the
/// goldens folder can be written.
pub fn assert_golden(name: &str, actual: &Value) {
    assert_eq!(
        Path::new(name).file_name().and_then(|file| file.to_str()),
        Some(name),
        "a golden is named by a bare file name"
    );
    let path = goldens_dir().join(name);
    if blessing() {
        let text = serde_json::to_string_pretty(actual).expect("serializes");
        std::fs::write(path, format!("{text}\n")).expect("bless writes");
        return;
    }
    let raw = std::fs::read_to_string(path).expect("golden reads");
    let expected: Value = serde_json::from_str(&raw).expect("golden parses");
    let mut drift = Vec::new();
    diff_golden(&expected, actual, "$", &mut drift);
    assert!(drift.is_empty(), "{name} drifted:\n{}", drift.join("\n"));
}

/// Every golden recorded from the scanned library.
pub fn library_goldens() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(goldens_dir())
        .expect("goldens list")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("library_"))
        })
        .collect();
    found.sort();
    found
}
