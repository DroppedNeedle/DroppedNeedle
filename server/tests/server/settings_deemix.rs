//! Deemix's Test and its probe against the in-repo mock, which answers
//! from the recordings of deemix-webui 4.8.0: what the settings card is
//! told about the address, the Deezer sign-in and what to change.

use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::settings::wiring::SettingsSetup;
use droppedneedle_acquire::imports::health::DeemixQuality;
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_kernel::runtime_config::sections::DeemixConnection;
use droppedneedle_testkit::deemix::{DeemixMock, snapshots};
use droppedneedle_testkit::usenet::{ServerGuard, serve_loopback};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::ScratchDir;
use crate::common::auth::{AuthRig, test_principal, with_test_principal};

struct Rig {
    settings: SettingsSetup,
    auth: AuthRig,
    admin_id: String,
    mock: DeemixMock,
    base: String,
    mount: ScratchDir,
    _server: ServerGuard,
}

impl Rig {
    async fn open(tag: &str) -> Self {
        let auth = AuthRig::new(tag).await;
        let admin = auth.seed_user("brenda", Role::Admin).await;
        let settings = crate::common::bundles::settings_at(
            &auth.platform,
            SettingsSetup::live_probes(
                &auth.platform,
                &droppedneedle_providers::Providers::closed(),
            ),
        );
        let mock = DeemixMock::new();
        let (base, server) = serve_loopback(mock.router()).await.expect("mock serves");
        Self {
            settings,
            auth,
            admin_id: admin.id,
            mock,
            base,
            mount: ScratchDir::new(&format!("deemix-{tag}")),
            _server: server,
        }
    }

    fn app(&self) -> Router {
        let router = droppedneedle::settings::wiring::routes()
            .with_state(self.settings.clone())
            .into_router();
        with_test_principal(
            router,
            test_principal(&self.admin_id, Role::Admin, "sess-1", SessionKind::Standard),
        )
    }

    fn body(&self, url: &str, mount: &Path) -> Value {
        json!({ "enabled": true, "url": url, "downloads_mount": mount })
    }

    async fn save(&self) {
        self.save_at(&self.base, &self.mount).await;
    }

    async fn save_at(&self, url: &str, mount: &Path) {
        let body = self.body(url, mount);
        let (status, answer) = call(
            self.app(),
            "PUT",
            "/api/v3/settings/download-clients/deemix",
            body,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
    }

    async fn test(&self, url: &str, mount: &Path) -> Value {
        let body = self.body(url, mount);
        let (status, answer) = call(
            self.app(),
            "POST",
            "/api/v3/settings/download-clients/deemix/test",
            body,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        answer
    }

    fn saved(&self) -> DeemixConnection {
        DeemixConnection {
            enabled: true,
            url: self.base.clone(),
            downloads_mount: self.mount.display().to_string(),
        }
    }

    /// Change one of the settings the signed-out `/connect` answers.
    fn deemix_setting(&self, name: &str, value: Value) {
        self.mock.state().connect["settingsData"]["settings"][name] = value;
    }
}

async fn call(app: Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request builds");
    let response = app.oneshot(request).await.expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (
        status,
        serde_json::from_slice(&bytes).expect("body is json"),
    )
}

fn warnings(answer: &Value) -> Vec<String> {
    answer["warnings"]
        .as_array()
        .expect("warnings listed")
        .iter()
        .map(|warning| warning.as_str().expect("a name").to_owned())
        .collect()
}

/// Test reads the saved address on the kept session and never signs in:
/// a saved Deezer sign-in is only reported. The probe signs that session
/// in once, and Test then shows the recorded free account's quality and
/// that the default quality settings take none of it.
#[tokio::test]
async fn test_reads_the_kept_session_the_probe_signs_in() {
    let rig = Rig::open("deemix-kept").await;
    rig.mock.offer_saved_login();
    rig.save().await;

    let answer = rig.test(&rig.base, &rig.mount).await;
    assert_eq!(answer["reachable"], true);
    assert_eq!(answer["webui_version"], "4.8.0");
    assert_eq!(answer["deezer_reachable"], true);
    assert_eq!(answer["signed_in"], false);
    assert_eq!(answer["sign_in_offered"], true);
    assert_eq!(answer["account_quality"], Value::Null);
    assert_eq!(warnings(&answer), ["overwrite_off"]);
    assert_eq!(
        answer["message"],
        "Deemix has a saved Deezer sign-in; DroppedNeedle signs in when it needs to."
    );
    assert!(
        rig.mock.calls_to("/api/loginArl").is_empty(),
        "Test signed in"
    );

    let probed = rig.settings.deemix().probe(&rig.saved()).await;
    assert!(probed.signed_in, "{}", probed.message);
    assert_eq!(probed.account_quality, Some(DeemixQuality::Mp3At128));
    assert_eq!(rig.mock.calls_to("/api/loginArl").len(), 1);

    let answer = rig.test(&rig.base, &rig.mount).await;
    assert_eq!(answer["signed_in"], true);
    assert_eq!(answer["sign_in_offered"], false);
    assert_eq!(answer["account_quality"], "mp3_128");
    assert_eq!(answer["best_quality"], Value::Null);
    assert_eq!(answer["message"], "Connected");
    assert_eq!(warnings(&answer), ["overwrite_off", "quality_not_taken"]);
    assert_eq!(
        rig.mock.calls_to("/api/loginArl").len(),
        1,
        "Test signed in"
    );
    let last = rig
        .mock
        .calls_to("/api/connect")
        .pop()
        .expect("Test read /connect");
    let cookie = last.cookie.expect("Test sent the kept session");
    assert!(rig.mock.state().signed_in.contains(&cookie));

    // An account that streams MP3 320 gets it under the default settings.
    rig.mock.state().connect_signed_in["currentUser"]["can_stream_hq"] = json!(true);
    let answer = rig.test(&rig.base, &rig.mount).await;
    assert_eq!(answer["account_quality"], "mp3_320");
    assert_eq!(answer["best_quality"], "mp3_320");
    assert_eq!(warnings(&answer), ["overwrite_off"]);
}

/// A save that changes only Deemix's folder keeps the signed-in session;
/// a save that changes its address drops it before Test reads, so Test
/// never shows the session from before the address changed.
#[tokio::test]
async fn test_after_a_save_keeps_the_session_unless_the_address_changes() {
    let rig = Rig::open("deemix-resaved").await;
    rig.mock.offer_saved_login();
    rig.save().await;
    let probed = rig.settings.deemix().probe(&rig.saved()).await;
    assert!(probed.signed_in, "{}", probed.message);
    let signed_in = rig.mock.state().signed_in.clone();

    let moved = ScratchDir::new("deemix-resaved-moved");
    rig.save_at(&rig.base, &moved).await;
    let answer = rig.test(&rig.base, &moved).await;
    assert_eq!(answer["signed_in"], true, "{answer}");
    let last = rig
        .mock
        .calls_to("/api/connect")
        .pop()
        .expect("Test read /connect");
    assert!(
        last.cookie
            .is_some_and(|cookie| signed_in.contains(&cookie)),
        "a folder save dropped the session"
    );

    let other = rig.base.replace("127.0.0.1", "localhost");
    rig.save_at(&other, &moved).await;
    let answer = rig.test(&other, &moved).await;
    assert_eq!(answer["signed_in"], false, "{answer}");
    let last = rig
        .mock
        .calls_to("/api/connect")
        .pop()
        .expect("Test read /connect");
    assert_eq!(
        last.cookie, None,
        "Test sent the session from before the address changed"
    );
    assert_eq!(rig.mock.calls_to("/api/loginArl").len(), 1);
}

/// An address that is not the saved one is read on a new session, through
/// `/connect` only: the card says to save first.
#[tokio::test]
async fn test_of_an_unsaved_address_reads_connect_on_a_new_session() {
    let rig = Rig::open("deemix-unsaved").await;
    rig.mock.offer_saved_login();
    rig.save().await;
    rig.settings.deemix().probe(&rig.saved()).await;
    let queue_reads = rig.mock.calls_to("/api/getQueue").len();
    let other = rig.base.replace("127.0.0.1", "localhost");

    let answer = rig.test(&other, &rig.mount).await;
    assert_eq!(answer["reachable"], true);
    assert_eq!(answer["signed_in"], false);
    assert_eq!(answer["message"], "Save to see your Deezer account.");
    let last = rig
        .mock
        .calls_to("/api/connect")
        .pop()
        .expect("Test read /connect");
    assert_eq!(
        last.cookie, None,
        "Test sent the kept session to another address"
    );
    assert_eq!(rig.mock.calls_to("/api/getQueue").len(), queue_reads);
}

/// An address that misses Deemix's root reaches its web UI's page, which
/// is not Deemix, and no text of it reaches the card.
#[tokio::test]
async fn test_of_a_page_says_it_is_not_deemix() {
    let rig = Rig::open("deemix-html").await;

    let answer = rig.test(&format!("{}/deemix/", rig.base), &rig.mount).await;
    assert_eq!(answer["reachable"], false);
    assert_eq!(answer["message"], "This address isn't Deemix.");
    assert_eq!(answer["webui_version"], Value::Null);
}

/// Each warning is raised by what it names, and only then.
#[tokio::test]
async fn test_warns_of_what_to_change() {
    let rig = Rig::open("deemix-warnings").await;
    rig.save().await;

    // The recorded Deemix: single-user mode, overwrite off, and no saved
    // sign-in to use.
    let answer = rig.test(&rig.base, &rig.mount).await;
    assert_eq!(warnings(&answer), ["overwrite_off"]);
    assert_eq!(answer["sign_in_offered"], false);
    assert_eq!(
        answer["message"],
        "Not signed in to Deezer. Sign in on Deemix's page."
    );

    rig.deemix_setting("overwriteFile", json!("y"));
    rig.deemix_setting("fallbackISRC", json!(true));
    rig.deemix_setting("executeCommand", json!("mv %folder% /elsewhere"));
    let answer = rig.test(&rig.base, &rig.mount).await;
    assert_eq!(warnings(&answer), ["fallback_on", "command_set"]);
    rig.deemix_setting("fallbackISRC", json!(false));
    rig.deemix_setting("executeCommand", json!(""));
    rig.mock.offer_saved_login();
    rig.deemix_setting("overwriteFile", json!("y"));
    assert_eq!(
        warnings(&rig.test(&rig.base, &rig.mount).await),
        Vec::<String>::new()
    );

    let missing = rig.mount.join("missing");
    let answer = rig.test(&rig.base, &missing).await;
    assert_eq!(warnings(&answer), ["mount_unusable"]);

    let app_folder = rig.auth.platform.config.cache_dir.clone();
    std::fs::create_dir_all(&app_folder).expect("cache folder creates");
    let answer = rig.test(&rig.base, &app_folder).await;
    assert_eq!(warnings(&answer), ["mount_overlaps"]);

    // A finished track Deemix wrote to `/downloads//Radiohead - Nude.mp3`
    // is looked for in the downloads folder. One that is gone proves
    // nothing: it was moved away, or just imported.
    rig.mock.walk_queue(&[snapshots::TRACK[2]]);
    let answer = rig.test(&rig.base, &rig.mount).await;
    assert_eq!(warnings(&answer), Vec::<String>::new());
    let elsewhere = ScratchDir::new("deemix-warnings-elsewhere");
    let outside = elsewhere.join("Radiohead - Nude.mp3");
    std::fs::write(&outside, b"mp3").expect("file writes");
    let listed = rig.mount.join("Radiohead - Nude.mp3");
    std::os::unix::fs::symlink(&outside, &listed).expect("link made");
    let answer = rig.test(&rig.base, &rig.mount).await;
    assert_eq!(warnings(&answer), ["mapping_wrong"]);
    std::fs::remove_file(&listed).expect("link removed");
    std::fs::write(&listed, b"mp3").expect("file writes");
    let answer = rig.test(&rig.base, &rig.mount).await;
    assert_eq!(warnings(&answer), Vec::<String>::new());

    // A file outside Deemix's own folder has no place in the mount.
    rig.deemix_setting("downloadLocation", json!("/music/"));
    let answer = rig.test(&rig.base, &rig.mount).await;
    assert_eq!(warnings(&answer), ["mapping_wrong"]);
    rig.deemix_setting("downloadLocation", json!("/downloads/"));

    // In multi-user mode Deemix keeps no sign-in to offer.
    rig.mock.multi_user();
    let answer = rig.test(&rig.base, &rig.mount).await;
    assert_eq!(warnings(&answer), ["single_user_off"]);
    assert_eq!(rig.mock.calls_to("/api/loginArl").len(), 0);
}
