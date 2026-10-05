//! Settings sections: secret masks that survive a re-save, unit scaling,
//! revision checks, the admin gate, verify and save journeys over HTTP
//! with scripted probes, the live probes against a loopback stub, and the
//! cache sweep after each save.

use crate::common;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{FIXED_ID, FixedIdGenerator};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::memory::TestRig;
use droppedneedle::auth::users::roles::{Role, SessionKind};
use droppedneedle::runtime_config::mask::{
    INDEXER_API_KEY_MASK, JELLYFIN_API_KEY_MASK, LISTENBRAINZ_TOKEN_MASK, NAVIDROME_PASSWORD_MASK,
    OIDC_SECRET_MASK, PLEX_TOKEN_MASK, PROWLARR_API_KEY_MASK, SABNZBD_API_KEY_MASK,
    SKIDDLE_KEY_MASK, SLSKD_API_KEY_MASK, TICKETMASTER_KEY_MASK, WRAPPED_API_KEY_MASK,
    YOUTUBE_API_KEY_MASK,
};
use droppedneedle::runtime_config::{ConfigStore, Crypto};
use droppedneedle::settings::effects::{SaveEffects, SavedSection};
use droppedneedle::settings::models::{
    AdvancedSettingsDto, EventsSettingsDto, JellyfinConnectionDto, LibrarySettingsDto,
    LibrarySettingsSaveRequest, ListenBrainzConnectionDto, NavidromeConnectionDto,
    NewznabIndexerDto, OidcConnectionDto, PlexConnectionDto, ProwlarrConnectionDto,
    SabnzbdConnectionDto, SlskdConnectionDto, WrappedSettingsDto, YouTubeConnectionDto,
};
use droppedneedle::settings::musicbrainz::BRAINZMASH_DISCLOSURE_VERSION;
use droppedneedle::settings::services::SettingsService;
use droppedneedle::settings::verify::{
    JellyfinVerdict, ListenBrainzVerdict, LiveProbes, NewznabVerdict, PlexVerdict, ProbeVerdict,
    ProwlarrVerdict, SabnzbdMountDiagnosis, SabnzbdVerdict, VerifyProbes, VersionVerdict,
    require_service_url,
};
use droppedneedle::settings::wiring::SettingsSetup;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// Scratch-dir sequence so parallel tests never share a store.
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// Post-save fan-out recorder: which sections fired, in order.
#[derive(Default)]
struct RecorderEffects {
    calls: Mutex<Vec<SavedSection>>,
}

impl RecorderEffects {
    fn calls(&self) -> Vec<SavedSection> {
        self.calls.lock().expect("recorder reads").clone()
    }
}

impl SaveEffects for RecorderEffects {
    fn after_save<'a>(&'a self, section: SavedSection) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.calls.lock().expect("recorder writes").push(section);
        })
    }
}

/// Scratch service with a recording fan-out.
fn scratch_service() -> (SettingsService, Arc<RecorderEffects>) {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "droppedneedle-settings-sections-{}-{seq}",
        std::process::id()
    ));
    let crypto = Crypto::from_key_bytes(&[7u8; 32]).expect("test crypto builds");
    let store = ConfigStore::open(&dir.join("config.json"), crypto).expect("store opens");
    let effects = Arc::new(RecorderEffects::default());
    let ids = Arc::new(FixedIdGenerator::new(FIXED_ID));
    (
        SettingsService::new(Arc::new(store), effects.clone(), ids),
        effects,
    )
}

/// Scripted verify probes: canned verdicts plus a call log. HTTP
/// briefs run against this, never the network.
struct FakeProbes {
    calls: Mutex<Vec<String>>,
    valid: bool,
    rate_limited: bool,
}

impl FakeProbes {
    fn passing() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            valid: true,
            rate_limited: false,
        }
    }

    fn failing() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            valid: false,
            rate_limited: false,
        }
    }

    fn recorded(&self, name: &str, args: &[&str]) {
        let mut calls = self.calls.lock().expect("fake log writes");
        calls.push(format!("{name}({})", args.join(",")));
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("fake log reads").clone()
    }

    fn verdict(&self, name: &str) -> ProbeVerdict {
        if self.valid {
            ProbeVerdict::ok(format!("fake {name} ok"))
        } else {
            ProbeVerdict::failed(format!("fake {name} failed"))
        }
    }
}

impl VerifyProbes for FakeProbes {
    fn jellyfin<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, JellyfinVerdict> {
        Box::pin(async move {
            self.recorded("jellyfin", &[url, key]);
            let base = self.verdict("jellyfin");
            JellyfinVerdict {
                valid: base.valid,
                message: base.message,
                users: vec![("u1".to_owned(), "Ada".to_owned())],
            }
        })
    }

    fn navidrome<'a>(
        &'a self,
        url: &'a str,
        username: &'a str,
        password: &'a str,
    ) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            self.recorded("navidrome", &[url, username, password]);
            self.verdict("navidrome")
        })
    }

    fn plex<'a>(&'a self, url: &'a str, token: &'a str) -> BoxFuture<'a, PlexVerdict> {
        Box::pin(async move {
            self.recorded("plex", &[url, token]);
            let base = self.verdict("plex");
            PlexVerdict {
                valid: base.valid,
                message: base.message,
                libraries: vec![("1".to_owned(), "Music".to_owned())],
            }
        })
    }

    fn plex_libraries<'a>(
        &'a self,
        url: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>, String>> {
        Box::pin(async move {
            self.recorded("plex_libraries", &[url, token]);
            Ok(vec![("1".to_owned(), "Music".to_owned())])
        })
    }

    fn listenbrainz<'a>(
        &'a self,
        base: &'a str,
        username: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, ListenBrainzVerdict> {
        Box::pin(async move {
            self.recorded("listenbrainz", &[base, username, token]);
            ListenBrainzVerdict {
                valid: self.valid,
                message: "fake listenbrainz".to_owned(),
                rate_limited: self.rate_limited,
            }
        })
    }

    fn youtube<'a>(&'a self, base: &'a str, key: &'a str) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            self.recorded("youtube", &[base, key]);
            self.verdict("youtube")
        })
    }

    fn ticketmaster<'a>(&'a self, base: &'a str, key: &'a str) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            self.recorded("ticketmaster", &[base, key]);
            self.verdict("ticketmaster")
        })
    }

    fn skiddle<'a>(&'a self, base: &'a str, key: &'a str) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            self.recorded("skiddle", &[base, key]);
            self.verdict("skiddle")
        })
    }

    fn slskd<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, VersionVerdict> {
        Box::pin(async move {
            self.recorded("slskd", &[url, key]);
            VersionVerdict {
                valid: self.valid,
                version: Some("9.9".to_owned()),
                message: "fake slskd".to_owned(),
            }
        })
    }

    fn sabnzbd<'a>(
        &'a self,
        url: &'a str,
        key: &'a str,
        downloads_mount: &'a str,
    ) -> BoxFuture<'a, SabnzbdVerdict> {
        Box::pin(async move {
            self.recorded("sabnzbd", &[url, key, downloads_mount]);
            SabnzbdVerdict {
                valid: self.valid,
                version: Some("9.9".to_owned()),
                message: "fake sabnzbd".to_owned(),
                categories: vec!["music".to_owned()],
                complete_dir: Some("/complete".to_owned()),
                diagnosis: Some(SabnzbdMountDiagnosis {
                    mount_has_files: true,
                    resolvable_downloads: 1,
                    sampled_downloads: 1,
                    mount_message: None,
                }),
            }
        })
    }

    fn prowlarr<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, ProwlarrVerdict> {
        Box::pin(async move {
            self.recorded("prowlarr", &[url, key]);
            ProwlarrVerdict {
                valid: self.valid,
                version: Some("9.9".to_owned()),
                message: "fake prowlarr".to_owned(),
                indexer_count: Some(2),
            }
        })
    }

    fn newznab<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, NewznabVerdict> {
        Box::pin(async move {
            self.recorded("newznab", &[url, key]);
            NewznabVerdict {
                valid: self.valid,
                version: Some("9.9".to_owned()),
                message: "fake newznab".to_owned(),
                supports_audio_search: true,
                category_count: 3,
                suggested_url: None,
            }
        })
    }

    fn musicbrainz<'a>(&'a self, api_url: &'a str) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            self.recorded("musicbrainz", &[api_url]);
            self.verdict("musicbrainz")
        })
    }

    fn oidc<'a>(&'a self, issuer: &'a str) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            self.recorded("oidc", &[issuer]);
            self.verdict("oidc")
        })
    }
}

// --- service round-trips -----------------------------------------------------

/// Every secret-section save echoes the mask, never ciphertext, and
/// re-saving the echo keeps the stored secret intact.
#[tokio::test]
async fn secret_saves_echo_masks_and_resaves_preserve() {
    let (service, _) = scratch_service();
    // (save, raw read, dto with the secret set, secret field, mask, plaintext)
    macro_rules! check_secret {
        ($save:ident, $raw:ident, $dto:expr, $field:ident, $mask:expr, $plain:expr) => {{
            let echoed = service.$save(&$dto).await.expect("saves");
            assert_eq!(echoed.$field, $mask, stringify!($save));
            service.$save(&echoed).await.expect("resaves");
            let raw = service.$raw().expect("raw reads");
            assert_eq!(raw.$field.expose(), $plain, stringify!($save));
        }};
    }
    check_secret!(
        save_slskd,
        get_slskd_raw,
        SlskdConnectionDto {
            url: "http://slskd:5030".to_owned(),
            api_key: "slskd-secret".to_owned(),
            ..Default::default()
        },
        api_key,
        SLSKD_API_KEY_MASK,
        "slskd-secret"
    );
    check_secret!(
        save_sabnzbd,
        get_sabnzbd_raw,
        SabnzbdConnectionDto {
            url: "http://sab:8080".to_owned(),
            api_key: "sab-secret".to_owned(),
            ..Default::default()
        },
        api_key,
        SABNZBD_API_KEY_MASK,
        "sab-secret"
    );
    check_secret!(
        save_prowlarr,
        get_prowlarr_raw,
        ProwlarrConnectionDto {
            url: "http://prowlarr:9696".to_owned(),
            api_key: "prowlarr-secret".to_owned(),
            ..Default::default()
        },
        api_key,
        PROWLARR_API_KEY_MASK,
        "prowlarr-secret"
    );
    check_secret!(
        save_jellyfin,
        get_jellyfin_raw,
        JellyfinConnectionDto {
            jellyfin_url: "http://jellyfin:8096".to_owned(),
            api_key: "jellyfin-secret".to_owned(),
            ..Default::default()
        },
        api_key,
        JELLYFIN_API_KEY_MASK,
        "jellyfin-secret"
    );
    check_secret!(
        save_navidrome,
        get_navidrome_raw,
        NavidromeConnectionDto {
            navidrome_url: "http://navidrome:4533".to_owned(),
            username: "ada".to_owned(),
            password: "navidrome-secret".to_owned(),
            ..Default::default()
        },
        password,
        NAVIDROME_PASSWORD_MASK,
        "navidrome-secret"
    );
    check_secret!(
        save_plex,
        get_plex_raw,
        PlexConnectionDto {
            plex_url: "http://plex:32400".to_owned(),
            plex_token: "plex-secret".to_owned(),
            ..Default::default()
        },
        plex_token,
        PLEX_TOKEN_MASK,
        "plex-secret"
    );
    check_secret!(
        save_listenbrainz,
        get_listenbrainz_raw,
        ListenBrainzConnectionDto {
            username: "ada".to_owned(),
            user_token: "lb-secret".to_owned(),
            ..Default::default()
        },
        user_token,
        LISTENBRAINZ_TOKEN_MASK,
        "lb-secret"
    );
    check_secret!(
        save_youtube,
        get_youtube_raw,
        YouTubeConnectionDto {
            api_key: "youtube-secret".to_owned(),
            ..Default::default()
        },
        api_key,
        YOUTUBE_API_KEY_MASK,
        "youtube-secret"
    );
    check_secret!(
        save_wrapped,
        get_wrapped_raw,
        WrappedSettingsDto {
            api_key: "wrapped-secret".to_owned(),
        },
        api_key,
        WRAPPED_API_KEY_MASK,
        "wrapped-secret"
    );
    check_secret!(
        save_oidc,
        get_oidc_raw,
        OidcConnectionDto {
            issuer: "https://id.example.com".to_owned(),
            client_secret: "oidc-secret".to_owned(),
            ..Default::default()
        },
        client_secret,
        OIDC_SECRET_MASK,
        "oidc-secret"
    );
    // Events carries two secrets in one section.
    let events = EventsSettingsDto {
        ticketmaster_api_key: "tm-secret".to_owned(),
        skiddle_api_key: "skiddle-secret".to_owned(),
        ..Default::default()
    };
    let echoed = service.save_events(&events).await.expect("saves");
    assert_eq!(echoed.ticketmaster_api_key, TICKETMASTER_KEY_MASK);
    assert_eq!(echoed.skiddle_api_key, SKIDDLE_KEY_MASK);
    service.save_events(&echoed).await.expect("resaves");
    let raw = service.get_events_raw().expect("raw reads");
    assert_eq!(raw.ticketmaster_api_key.expose(), "tm-secret");
    assert_eq!(raw.skiddle_api_key.expose(), "skiddle-secret");
}

/// Indexers create, list masked, update, reorder, test raw, and delete.
#[tokio::test]
async fn indexers_crud_masked() {
    let (service, _) = scratch_service();
    let first = NewznabIndexerDto {
        name: "First".to_owned(),
        url: "https://first.example.com/api".to_owned(),
        api_key: "first-secret".to_owned(),
        ..Default::default()
    };
    let saved = service.save_indexer(&first).await.expect("creates");
    assert!(!saved.id.is_empty());
    let second = NewznabIndexerDto {
        name: "Second".to_owned(),
        url: "https://second.example.com/api".to_owned(),
        api_key: "second-secret".to_owned(),
        ..Default::default()
    };
    let saved_two = service.save_indexer(&second).await.expect("creates");
    assert_ne!(saved.id, saved_two.id);

    let listed = service.list_indexers().expect("lists");
    assert_eq!(listed.len(), 2);
    assert!(
        listed
            .iter()
            .all(|indexer| indexer.api_key == INDEXER_API_KEY_MASK)
    );
    assert_eq!(
        service
            .get_indexer_raw(&saved.id)
            .expect("raw reads")
            .expect("found")
            .api_key
            .expose(),
        "first-secret"
    );

    // A masked-key update keeps the stored key per element.
    let mut edited = listed
        .into_iter()
        .find(|indexer| indexer.id == saved.id)
        .expect("finds first");
    edited.name = "First Renamed".to_owned();
    let kept = service.save_indexer(&edited).await.expect("updates");
    assert_eq!(kept.id, saved.id);
    assert_eq!(
        service
            .get_indexer_raw(&saved.id)
            .expect("raw reads")
            .expect("found")
            .api_key
            .expose(),
        "first-secret"
    );

    service
        .reorder_indexers(&[saved_two.id.clone(), saved.id.clone()])
        .await
        .expect("reorders");
    let listed = service.list_indexers().expect("lists");
    assert_eq!(listed[0].id, saved_two.id);

    service.delete_indexer(&saved.id).await.expect("deletes");
    service
        .delete_indexer("missing-id")
        .await
        .expect("unknown delete is silent");
    let listed = service.list_indexers().expect("lists");
    assert_eq!(listed.len(), 1);
}

/// Advanced tunables scale frontend units to backend units, mask the
/// AudioDB key, and fire the Advanced fan-out.
#[tokio::test]
async fn advanced_round_trip_scales_and_masks() {
    use droppedneedle::runtime_config::mask::AUDIODB_API_KEY_MASK;
    use droppedneedle::runtime_config::secret_sections::AdvancedSettings;

    let (service, effects) = scratch_service();
    let dto = AdvancedSettingsDto {
        cache_ttl_album_library: 48,
        cache_ttl_search: 30,
        frontend_ttl_home: 10,
        discover_queue_polling_interval: 8,
        audiodb_api_key: "audiodb-secret".to_owned(),
        ..Default::default()
    };
    let echoed = service.save_advanced(&dto).await.expect("saves");
    assert_eq!(echoed.cache_ttl_album_library, 48);
    assert_eq!(echoed.audiodb_api_key, AUDIODB_API_KEY_MASK);
    assert_eq!(effects.calls(), vec![SavedSection::Advanced]);

    let stored: AdvancedSettings = service.store.get_raw().expect("raw reads");
    assert_eq!(stored.cache_ttl_album_library, 48 * 3600);
    assert_eq!(stored.cache_ttl_search, 30 * 60);
    assert_eq!(stored.frontend_ttl_home, 10 * 60000);
    assert_eq!(stored.discover_queue_polling_interval, 8 * 1000);
    assert_eq!(stored.audiodb_api_key.expose(), "audiodb-secret");

    // The echo re-saves cleanly: the mask keeps the stored key.
    service.save_advanced(&echoed).await.expect("resaves");
    let stored: AdvancedSettings = service.store.get_raw().expect("raw reads");
    assert_eq!(stored.audiodb_api_key.expose(), "audiodb-secret");

    let ttls = service.get_cache_ttls().expect("reads ttls");
    assert_eq!(ttls.home, 10 * 60000);
}

/// Library settings round-trip with CAS: a stale token is a 409, and
/// path add/remove normalize through the same save.
#[tokio::test]
async fn library_round_trip_cas_and_paths() {
    use droppedneedle::runtime_config::mask::ACOUSTID_KEY_MASK;
    use droppedneedle::runtime_config::secret_sections::TypedLibrary;

    let (service, effects) = scratch_service();
    let view = service.get_library().expect("reads default");
    assert!(!view.policy_revision.is_empty());

    // Stale token rejected before anything persists.
    let stale = LibrarySettingsSaveRequest {
        settings: LibrarySettingsDto::default(),
        expected_policy_revision: "wrong".to_owned(),
    };
    let error = service.save_library(&stale).await.expect_err("stale fails");
    assert!(matches!(
        error,
        droppedneedle::settings::error::SettingsError::StaleRevision { .. }
    ));

    let dir = std::env::temp_dir().join(format!(
        "droppedneedle-settings-library-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("music")).expect("fixture dir builds");
    let root = dir.join("music").to_string_lossy().into_owned();

    let with_key = LibrarySettingsDto {
        acoustid_api_key: "acoustid-secret".to_owned(),
        ..Default::default()
    };
    let saved = service
        .save_library(&LibrarySettingsSaveRequest {
            settings: with_key,
            expected_policy_revision: view.policy_revision.clone(),
        })
        .await
        .expect("saves");
    assert_eq!(saved.acoustid_api_key, ACOUSTID_KEY_MASK);
    assert_eq!(effects.calls(), vec![SavedSection::Library]);
    let stored: TypedLibrary = service.store.get_raw().expect("raw reads");
    assert_eq!(stored.acoustid_api_key.expose(), "acoustid-secret");

    let added = service.add_library_path(&root).await.expect("adds path");
    assert_eq!(added.library_roots.len(), 1);
    assert_eq!(added.library_roots[0].path, root);
    // Re-adding is a silent no-op; the key survives path edits.
    let again = service.add_library_path(&root).await.expect("re-adds");
    assert_eq!(again.library_roots.len(), 1);
    assert_eq!(again.acoustid_api_key, ACOUSTID_KEY_MASK);

    let missing = service.add_library_path("").await.expect_err("blank fails");
    assert!(format!("{missing:?}").contains("required"));

    let removed = service.remove_library_path(&root).await.expect("removes");
    assert!(removed.library_roots.is_empty());
    service
        .remove_library_path("/nothing/here")
        .await
        .expect("unknown remove is silent");
}

// --- HTTP journeys -----------------------------------------------------------

/// One scratch deployment: memory auth, scripted probes, wired settings.
struct Rig {
    settings: SettingsSetup,
    #[allow(dead_code)]
    auth: TestRig,
    admin_id: String,
    user_id: String,
    probes: Arc<FakeProbes>,
}

impl Rig {
    async fn open(passing: bool) -> Self {
        let rig = TestRig::new().expect("rig builds");
        let admin = rig.seed_user("brenda", Role::Admin).await;
        let user = rig.seed_user("molly", Role::User).await;
        let probes = Arc::new(if passing {
            FakeProbes::passing()
        } else {
            FakeProbes::failing()
        });
        let settings = SettingsSetup::for_tests_with_probes(
            Arc::new(FixedIdGenerator::new(FIXED_ID)),
            rig.deps.clone(),
            probes.clone(),
        )
        .expect("settings bundle builds");
        Self {
            settings,
            auth: rig,
            admin_id: admin.id,
            user_id: user.id,
            probes,
        }
    }

    /// Both routers with an injected session for `user_id`, or anonymous.
    fn app(&self, user_id: Option<&str>) -> Router {
        let router = self
            .settings
            .gated_router()
            .merge(self.settings.me_router());
        match user_id {
            Some(user_id) => {
                let user_id = user_id.to_owned();
                router.layer(axum::middleware::from_fn(
                    move |mut req: Request<Body>, next: axum::middleware::Next| {
                        let user_id = user_id.clone();
                        async move {
                            req.extensions_mut().insert(CurrentSession {
                                user_id,
                                session_id: "sess-1".to_owned(),
                                kind: SessionKind::Standard,
                                transport: Transport::Bearer,
                            });
                            next.run(req).await
                        }
                    },
                ))
            }
            None => router,
        }
    }

    fn admin_app(&self) -> Router {
        self.app(Some(self.admin_id.clone()).as_deref())
    }

    fn user_app(&self) -> Router {
        self.app(Some(self.user_id.clone()).as_deref())
    }
}

async fn call(app: Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(body.map_or_else(Body::empty, |json| {
            Body::from(serde_json::to_vec(&json).expect("body serializes"))
        }))
        .expect("request builds");
    let response = app.oneshot(request).await.expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, json)
}

/// Anonymous callers get 401, signed-in non-admins get 403, admins pass.
#[tokio::test]
async fn settings_routes_are_admin_gated() {
    let rig = Rig::open(true).await;
    for (method, uri) in [
        ("GET", "/settings/preferences"),
        ("PUT", "/settings/preferences"),
        ("GET", "/settings/jellyfin"),
        ("POST", "/settings/jellyfin/verify"),
        ("GET", "/settings/indexers"),
        ("POST", "/settings/indexers/test"),
        ("GET", "/settings/musicbrainz"),
        ("POST", "/settings/musicbrainz/brainzmash/stage"),
        ("GET", "/settings/library/sync"),
    ] {
        let body = (method == "PUT" || method == "POST").then(|| json!({}));
        let (status, _) = call(rig.app(None), method, uri, body.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} anon");
        let (status, denied) = call(rig.user_app(), method, uri, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri} user");
        assert_eq!(denied["error"]["code"], "FORBIDDEN");
    }
    // Section prefs are per-user: a plain user passes. Anonymous
    // rejection belongs to the app-level session gate (covered by the
    // auth journeys); the bare router has no session to extend.
    let (status, _) = call(rig.user_app(), "GET", "/me/section-prefs", None).await;
    assert_eq!(status, StatusCode::OK);
    let request = Request::builder()
        .method("GET")
        .uri("/me/section-prefs")
        .body(Body::empty())
        .expect("request builds");
    let response = rig.app(None).oneshot(request).await.expect("answers");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// Verify endpoints resolve masked secrets to the stored ones before
/// probing, and map verdicts into the shared shapes.
#[tokio::test]
async fn verify_resolves_masks_and_maps_verdicts() {
    let rig = Rig::open(true).await;

    // Seed a real Jellyfin key, then verify the masked echo: the fake
    // must observe the stored key, not the sentinel.
    let (status, saved) = call(
        rig.admin_app(),
        "PUT",
        "/settings/jellyfin",
        Some(json!({
            "jellyfin_url": "http://jellyfin:8096",
            "api_key": "jellyfin-live",
            "user_id": "",
            "enabled": true,
            "login_enabled": false,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["api_key"], JELLYFIN_API_KEY_MASK);
    let (status, verdict) = call(
        rig.admin_app(),
        "POST",
        "/settings/jellyfin/verify",
        Some(saved.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(verdict["success"], true);
    assert_eq!(verdict["users"][0]["name"], "Ada");
    assert!(
        rig.probes
            .calls()
            .iter()
            .any(|call| call == "jellyfin(http://jellyfin:8096,jellyfin-live)"),
        "probe saw the resolved key: {:?}",
        rig.probes.calls()
    );

    // A failing probe maps to a body verdict, never an error status.
    let rig = Rig::open(false).await;
    let (status, verdict) = call(
        rig.admin_app(),
        "POST",
        "/settings/navidrome/verify",
        Some(json!({
            "navidrome_url": "http://navidrome:4533",
            "username": "ada",
            "password": "wrong",
            "enabled": true,
            "playlist_sync_enabled": false,
            "playlist_sync_path": "",
            "playlist_sync_scope": "public",
            "playlist_sync_remove_deleted": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(verdict["valid"], false);

    // ListenBrainz rate limiting answers 429.
    let rig = TestRig::new().expect("rig builds");
    let admin = rig.seed_user("brenda", Role::Admin).await;
    let probes = Arc::new(FakeProbes {
        calls: Mutex::new(Vec::new()),
        valid: false,
        rate_limited: true,
    });
    let settings = SettingsSetup::for_tests_with_probes(
        Arc::new(FixedIdGenerator::new(FIXED_ID)),
        rig.deps.clone(),
        probes,
    )
    .expect("settings bundle builds");
    let app = settings.gated_router().layer(axum::middleware::from_fn(
        move |mut req: Request<Body>, next: axum::middleware::Next| {
            let admin_id = admin.id.clone();
            async move {
                req.extensions_mut().insert(CurrentSession {
                    user_id: admin_id,
                    session_id: "sess-1".to_owned(),
                    kind: SessionKind::Standard,
                    transport: Transport::Bearer,
                });
                next.run(req).await
            }
        },
    ));
    let (status, limited) = call(
        app,
        "POST",
        "/settings/listenbrainz/verify",
        Some(json!({"username": "ada", "user_token": "tok", "enabled": true})),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited["error"]["code"], "RATE_LIMITED");
}

/// Dropped sections answer 410 with the decision ref on both methods.
#[tokio::test]
async fn dropped_routes_answer_gone() {
    let rig = Rig::open(true).await;
    for (method, uri) in [
        ("GET", "/settings/library/sync"),
        ("PUT", "/settings/library/sync"),
        ("GET", "/settings/home"),
        ("PUT", "/settings/home"),
    ] {
        let body = (method == "PUT").then(|| json!({}));
        let (status, gone) = call(rig.admin_app(), method, uri, body).await;
        assert_eq!(status, StatusCode::GONE, "{method} {uri}");
        assert_eq!(gone["error"]["code"], "SECTION_DROPPED");
    }
}

/// Admin edits an indexer, verifies, saves, and the provider-facing
/// read rebuilds from the saved config (no stale cache).
#[tokio::test]
async fn indexer_edit_verify_save_rebuild() {
    let rig = Rig::open(true).await;

    // Create with a live key.
    let (status, created) = call(
        rig.admin_app(),
        "POST",
        "/settings/indexers",
        Some(json!({
            "id": "",
            "type": "newznab",
            "name": "Stub",
            "url": "http://stub:8080/api",
            "api_key": "indexer-live",
            "categories": [3000],
            "enabled": true,
            "priority": 1,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = created["id"].as_str().expect("id returns").to_owned();
    assert!(!id.is_empty());

    // Verify the masked echo: the fake observes the stored key.
    let listed = call(rig.admin_app(), "GET", "/settings/indexers", None).await;
    assert_eq!(listed.0, StatusCode::OK);
    let echo = listed.1.as_array().expect("list reads")[0].clone();
    assert_eq!(echo["api_key"], INDEXER_API_KEY_MASK);
    let (status, verdict) = call(
        rig.admin_app(),
        "POST",
        "/settings/indexers/test",
        Some(echo.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(verdict["valid"], true);
    assert!(
        rig.probes
            .calls()
            .iter()
            .any(|call| call == "newznab(http://stub:8080/api,indexer-live)"),
        "probe saw the resolved key: {:?}",
        rig.probes.calls()
    );

    // Edit and save; the path id wins over the body id.
    let (status, saved) = call(
        rig.admin_app(),
        "PUT",
        &format!("/settings/indexers/{id}"),
        Some(json!({
            "id": "ignored",
            "type": "newznab",
            "name": "Stub Renamed",
            "url": "http://stub:8080/api",
            "api_key": INDEXER_API_KEY_MASK,
            "categories": [3000],
            "enabled": true,
            "priority": 1,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["id"], id);

    // The provider-facing read rebuilds from saved config.
    let (status, listed) = call(rig.admin_app(), "GET", "/settings/indexers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed[0]["name"], "Stub Renamed");
    assert_eq!(listed[0]["api_key"], INDEXER_API_KEY_MASK);

    // Delete removes it; reorder of the rest holds.
    let (status, _) = call(
        rig.admin_app(),
        "DELETE",
        &format!("/settings/indexers/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, listed) = call(rig.admin_app(), "GET", "/settings/indexers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().expect("list reads").len(), 0);
}

/// The BrainzMash ceremony runs over HTTP: stage, consent, verify the
/// binding, activate. A stale binding is a 409.
#[tokio::test]
async fn brainzmash_ceremony_http() {
    let rig = Rig::open(true).await;
    let (status, staged) = call(
        rig.admin_app(),
        "POST",
        "/settings/musicbrainz/brainzmash/stage",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pending = staged["pending_brainzmash"].clone();
    assert!(pending.is_object());
    let binding = json!({
        "access_revision": pending["access_revision"],
        "source_id": pending["source_id"],
        "generation": pending["generation"],
        "disclosure_version": pending["disclosure_version"],
    });

    let (status, _) = call(
        rig.admin_app(),
        "POST",
        "/settings/musicbrainz/brainzmash/consent",
        Some(binding.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, verified) = call(
        rig.admin_app(),
        "POST",
        "/settings/musicbrainz/verify",
        Some(binding.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(verified["pending_brainzmash"]["verified"], true);

    let (status, active) = call(
        rig.admin_app(),
        "POST",
        "/settings/musicbrainz/activate",
        Some(binding),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(active["source_mode"], "brainzmash");
    assert!(active["active_brainzmash"].is_object());

    // A stale binding no longer matches anything.
    let (status, stale) = call(
        rig.admin_app(),
        "POST",
        "/settings/musicbrainz/activate",
        Some(json!({
            "access_revision": "stale",
            "source_id": "stale",
            "generation": 1,
            "disclosure_version": BRAINZMASH_DISCLOSURE_VERSION,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["error"]["code"], "CONFLICT");
}

/// Section prefs round-trip per user; unknown pages and keys are 400s.
#[tokio::test]
async fn section_prefs_http() {
    let rig = Rig::open(true).await;
    let (status, full) = call(rig.user_app(), "GET", "/me/section-prefs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(full["pages"]["home"].is_array());
    assert!(full["pages"]["discover"].is_array());
    assert!(full["pages"]["sidebar"].is_array());

    let (status, updated) = call(
        rig.user_app(),
        "PUT",
        "/me/section-prefs",
        Some(json!({
            "page": "home",
            "sections": [{"key": "trending_artists", "enabled": false}],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(updated["pages"]["home"].is_array());
    assert!(
        updated
            .get("pages")
            .expect("pages")
            .get("discover")
            .is_none()
    );
    let toggled = updated["pages"]["home"]
        .as_array()
        .expect("page reads")
        .iter()
        .find(|item| item["key"] == "trending_artists")
        .expect("finds key");
    assert_eq!(toggled["enabled"], false);

    let (status, _) = call(
        rig.user_app(),
        "PUT",
        "/me/section-prefs",
        Some(json!({"page": "nope", "sections": []})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, bad) = call(
        rig.user_app(),
        "PUT",
        "/me/section-prefs",
        Some(json!({
            "page": "home",
            "sections": [{"key": "bogus", "enabled": false}],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        bad["error"]["message"]
            .as_str()
            .expect("message")
            .contains("bogus")
    );
}

// --- probe-unit briefs ---------------------------------------------------------

/// Service URLs are required, http(s), and slash-trimmed.
#[test]
fn service_url_gate() {
    assert_eq!(
        require_service_url("http://x:1/", "Thing URL").expect("trims"),
        "http://x:1"
    );
    let blank = require_service_url("  ", "Thing URL").expect_err("blank fails");
    assert!(format!("{blank:?}").contains("required"));
    let scheme = require_service_url("ftp://x", "Thing URL").expect_err("scheme fails");
    assert!(format!("{scheme:?}").contains("http(s)"));
}

// --- live probes against loopback stubs --------------------------------------

/// One stub origin answering every upstream shape the probes speak.
/// Query-driven dispatch keeps Plex `/` and the Newznab homepage apart
/// on the same root.
fn stub_app() -> Router {
    use axum::Json;
    use axum::extract::Query;
    use axum::http::HeaderMap;
    use axum::response::{Html, IntoResponse};

    async fn root(Query(query): Query<HashMap<String, String>>) -> impl IntoResponse {
        if query.contains_key("t") {
            // A bare indexer URL answers its homepage, not caps.
            return Html("<html>homepage</html>").into_response();
        }
        Json(json!({"MediaContainer": {"friendlyName": "StubPlex", "version": "1.40"}}))
            .into_response()
    }

    async fn api(Query(query): Query<HashMap<String, String>>) -> impl IntoResponse {
        if query.contains_key("t") {
            let caps = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps><server title="Stub" version="2.0"/>
<limits max="100" default="50"/>
<searching><search available="yes"/><audio-search available="yes"/></searching>
<categories><category id="3000" name="Audio"><subcat id="3040" name="MP3"/></category></categories>
</caps>"#;
            return (
                [(axum::http::header::CONTENT_TYPE, "text/xml")],
                caps.to_owned(),
            )
                .into_response();
        }
        match query.get("mode").map(String::as_str) {
            Some("version") => Json(json!({"version": "4.3.0"})).into_response(),
            Some("get_cats") => Json(json!({"categories": ["movies", "music"]})).into_response(),
            Some("get_config") => Json(json!({
                "config": {"misc": {"complete_dir": "/downloads/complete"}}
            }))
            .into_response(),
            Some("history") => Json(json!({"history": {"slots": []}})).into_response(),
            _ => Json(json!({})).into_response(),
        }
    }

    Router::new()
        .route("/", axum::routing::get(root))
        .route("/api", axum::routing::get(api))
        .route(
            "/System/Info",
            axum::routing::get(|| async {
                Json(json!({"ServerName": "Stub", "Version": "10.11.0"}))
            }),
        )
        .route(
            "/Users",
            axum::routing::get(|| async { Json(json!([{"Id": "u1", "Name": "Ada"}])) }),
        )
        .route(
            "/rest/ping",
            axum::routing::get(|| async {
                Json(json!({"subsonic-response": {"status": "ok", "version": "1.16.1"}}))
            }),
        )
        .route(
            "/library/sections",
            axum::routing::get(|| async {
                Json(json!({"MediaContainer": {"Directory": [
                    {"key": "1", "title": "Music", "type": "artist"},
                    {"key": "2", "title": "TV", "type": "show"},
                ]}}))
            }),
        )
        .route(
            "/1/validate-token",
            axum::routing::get(|headers: HeaderMap| async move {
                match headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                {
                    Some("Token bad") => StatusCode::UNAUTHORIZED.into_response(),
                    Some("Token limited") => StatusCode::TOO_MANY_REQUESTS.into_response(),
                    _ => Json(json!({"valid": true, "user_name": "ada"})).into_response(),
                }
            }),
        )
        .route(
            "/1/user/{name}/listen-count",
            axum::routing::get(|| async { Json(json!({"payload": {"count": 7}})) }),
        )
        .route(
            "/youtube/v3/videos",
            axum::routing::get(|Query(query): Query<HashMap<String, String>>| async move {
                if query.get("key").map(String::as_str) == Some("bad") {
                    StatusCode::FORBIDDEN.into_response()
                } else {
                    Json(json!({"items": []})).into_response()
                }
            }),
        )
        .route(
            "/attractions.json",
            axum::routing::get(|| async { Json(json!({"_embedded": {}})) }),
        )
        .route(
            "/events/search/",
            axum::routing::get(|Query(query): Query<HashMap<String, String>>| async move {
                if query.get("api_key").map(String::as_str) == Some("bad") {
                    Json(json!({"error": 1, "errormessage": "bad key"}))
                } else {
                    Json(json!({"error": 0, "results": []}))
                }
            }),
        )
        .route(
            "/api/v0/application",
            axum::routing::get(|headers: HeaderMap| async move {
                if headers
                    .get("x-api-key")
                    .and_then(|value| value.to_str().ok())
                    == Some("bad")
                {
                    StatusCode::UNAUTHORIZED.into_response()
                } else {
                    Json(json!({"version": {"current": "0.21.0"}})).into_response()
                }
            }),
        )
        .route(
            "/api/v1/system/status",
            axum::routing::get(|| async { Json(json!({"version": "1.2.3"})) }),
        )
        .route(
            "/api/v1/indexer",
            axum::routing::get(|| async {
                Json(json!([
                    {"id": 1, "name": "A", "protocol": "usenet", "enable": true},
                    {"id": 2, "name": "B", "protocol": "usenet", "enable": false},
                ]))
            }),
        )
        .route(
            "/artist",
            axum::routing::get(|| async { Json(json!({"artists": []})) }),
        )
        .route(
            "/boom/artist",
            axum::routing::get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(|| async { Json(json!({"issuer": "https://stub.example"})) }),
        )
}

/// Serve the stub on a loopback port; returns the base URL.
async fn serve_stub() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub binds");
    let base = format!("http://{}", listener.local_addr().expect("addr reads"));
    tokio::spawn(async move {
        axum::serve(listener, stub_app())
            .await
            .expect("stub serves");
    });
    base
}

/// Every live probe passes against the stub and fails honestly: bad
/// credentials, rate limits, and dead upstreams are verdicts with the
/// v2-shaped messages.
#[tokio::test]
async fn live_probes_hit_stubs() {
    let base = serve_stub().await;
    let probes = LiveProbes::new(reqwest::Client::new());
    let dead = "http://127.0.0.1:9";

    let jellyfin = probes.jellyfin(&base, "key").await;
    assert!(jellyfin.valid);
    assert_eq!(jellyfin.users, vec![("u1".to_owned(), "Ada".to_owned())]);
    let jellyfin = probes.jellyfin(dead, "key").await;
    assert!(!jellyfin.valid);
    assert!(jellyfin.message.contains("Could not connect"));

    let navidrome = probes.navidrome(&base, "ada", "pw").await;
    assert!(navidrome.valid, "{}", navidrome.message);
    assert!(!probes.navidrome(dead, "ada", "pw").await.valid);

    let plex = probes.plex(&base, "token").await;
    assert!(plex.valid, "{}", plex.message);
    assert_eq!(plex.libraries, vec![("1".to_owned(), "Music".to_owned())]);
    assert!(!probes.plex(dead, "token").await.valid);
    let libraries = probes.plex_libraries(&base, "token").await.expect("lists");
    assert_eq!(libraries.len(), 1);

    let lb = probes.listenbrainz(&base, "ada", "tok").await;
    assert!(lb.valid);
    assert!(!lb.rate_limited);
    let lb = probes.listenbrainz(&base, "ada", "bad").await;
    assert!(!lb.valid);
    let lb = probes.listenbrainz(&base, "ada", "limited").await;
    assert!(lb.rate_limited);

    let youtube = probes.youtube(&base, "key").await;
    assert!(youtube.valid);
    let youtube = probes.youtube(&base, "bad").await;
    assert!(!youtube.valid);

    assert!(probes.ticketmaster(&base, "key").await.valid);
    assert!(!probes.ticketmaster(dead, "key").await.valid);
    assert!(probes.skiddle(&base, "key").await.valid);
    assert!(!probes.skiddle(&base, "bad").await.valid);

    let slskd = probes.slskd(&base, "key").await;
    assert!(slskd.valid, "{}", slskd.message);
    assert_eq!(slskd.version.as_deref(), Some("0.21.0"));
    assert!(!probes.slskd(&base, "bad").await.valid);

    let sabnzbd = probes.sabnzbd(&base, "key", "/mnt/downloads").await;
    assert!(sabnzbd.valid, "{}", sabnzbd.message);
    assert_eq!(sabnzbd.version.as_deref(), Some("4.3.0"));
    assert_eq!(
        sabnzbd.categories,
        vec!["movies".to_owned(), "music".to_owned()]
    );
    assert_eq!(sabnzbd.complete_dir.as_deref(), Some("/downloads/complete"));
    assert!(!probes.sabnzbd(dead, "key", "/mnt/downloads").await.valid);

    let prowlarr = probes.prowlarr(&base, "key").await;
    assert!(prowlarr.valid, "{}", prowlarr.message);
    assert_eq!(prowlarr.version.as_deref(), Some("1.2.3"));
    assert_eq!(prowlarr.indexer_count, Some(1));
    assert!(!probes.prowlarr(dead, "key").await.valid);

    let newznab = probes.newznab(&format!("{base}/api"), "key").await;
    assert!(newznab.valid, "{}", newznab.message);
    assert!(newznab.supports_audio_search);
    assert_eq!(newznab.category_count, 1);
    // A bare site URL fails with the one-click /api fix.
    let homepage = probes.newznab(&base, "key").await;
    assert!(!homepage.valid);
    assert_eq!(
        homepage.suggested_url.as_deref(),
        Some(format!("{base}/api").as_str())
    );

    assert!(probes.musicbrainz(&base).await.valid);
    assert!(!probes.musicbrainz(&format!("{base}/boom")).await.valid);

    let oidc = probes.oidc(&base).await;
    assert!(oidc.valid);
    assert!(!probes.oidc(&format!("{base}/noidc")).await.valid);
    assert!(!probes.oidc("  ").await.valid);
}

/// Live fan-out sweeps exactly the saved section's cache roots and kicks
/// the events sweep only for the Events section.
#[tokio::test]
async fn live_save_effects_sweep_only_the_saved_section() {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use droppedneedle::providers::InMemoryProviderCache;
    use droppedneedle::providers::ProviderCache as _;
    use droppedneedle::settings::effects::{FnKick, LiveSaveEffects};

    let cache = Arc::new(InMemoryProviderCache::new());
    for key in [
        "lb_x",
        "mb:artist:search:x",
        "audiodb_x",
        "acoustid:x",
        "lfm_x",
    ] {
        cache.set_bytes(key, vec![1], Duration::from_secs(60)).await;
    }
    let kicked = Arc::new(AtomicBool::new(false));
    let kick_flag = kicked.clone();
    let effects = LiveSaveEffects::new(
        cache.clone(),
        Arc::new(FnKick {
            kick_fn: move || kick_flag.store(true, Ordering::SeqCst),
        }),
    );

    effects.after_save(SavedSection::MusicBrainz).await;
    assert_eq!(cache.len().await, 4);
    assert!(!kicked.load(Ordering::SeqCst));

    effects.after_save(SavedSection::ListenBrainz).await;
    effects.after_save(SavedSection::Advanced).await;
    effects.after_save(SavedSection::Library).await;
    assert_eq!(cache.len().await, 1);
    assert!(!kicked.load(Ordering::SeqCst));

    effects.after_save(SavedSection::Events).await;
    assert!(kicked.load(Ordering::SeqCst));
    assert_eq!(cache.len().await, 1);

    effects.after_save(SavedSection::Other).await;
    assert_eq!(cache.len().await, 1);
}
