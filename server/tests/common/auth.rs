//! The auth rig: the production auth stores and users services over a
//! migrated database, built by the production builder with plex.tv,
//! Last.fm and the breach check on a closed port. Plus the sign-in layer
//! that stands in for the session gate on a slice router.
//!
//! The rig's settings turn Last.fm on and the breach check off; a test
//! that needs otherwise saves the section into `config`.

use std::sync::{Arc, OnceLock};

use axum::{
    Router,
    extract::{Request, State},
    middleware::{self, Next},
    response::Response,
};
use droppedneedle_auth::prod::{Passwords, ProdAuth};
use droppedneedle_auth::session::middleware::SessionAuth;
use droppedneedle_auth::session::store::{SessionKind, SessionRecord};
use droppedneedle_auth::session::tokens;
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_auth::users::models::{LocalCredential, UserRecord};
use droppedneedle_auth::users::roles::{Admin, Principal, Role};
use droppedneedle_auth::wiring::AuthSetup;
use droppedneedle_kernel::clock::{Clock, ManualClock};
use droppedneedle_kernel::db::DbRuntime;
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::secret_sections::LastFmSettings;
use droppedneedle_kernel::runtime_config::sections::SecuritySettings;
use droppedneedle_kernel::runtime_config::{ConfigStore, Secret};

use super::{ScratchDir, TestPlatform, closed_outbound, scratch_runtime};

/// The rig's clock at build: 2024-01-01T00:00:00Z.
pub const TEST_NOW: i64 = 1_704_067_200;

/// Every seeded user's password.
pub const PASSWORD: &str = "correct horse battery staple";

/// The production hash of [`PASSWORD`], computed once per test binary so
/// seeding stays fast.
pub fn password_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| Passwords::new().hash(PASSWORD).expect("password hashes"))
}

/// Production auth over a migrated database.
pub struct AuthRig {
    /// The users routes' dependencies, as the production builder wires them.
    pub deps: UsersDeps,
    /// The SQLite stores behind `deps`, for seeding and assertions.
    pub auth: ProdAuth,
    /// The clock behind `deps` and the session list.
    pub clock: Arc<ManualClock>,
    /// The settings `deps` read live: the platform's.
    pub config: Arc<ConfigStore>,
    /// What the rig was built from, for the test's other bundles.
    pub platform: Platform,
    gate: SessionAuth,
    runtime: Option<DbRuntime>,
    _scratch: Option<ScratchDir>,
}

impl AuthRig {
    /// A rig on its own copy of the migrated database, whose platform runs
    /// on the rig's clock.
    pub async fn new(tag: &str) -> Self {
        let scratch = ScratchDir::new(&format!("auth-{tag}"));
        let runtime = scratch_runtime(&scratch.join("auth.db")).await;
        let clock = Arc::new(ManualClock::at(TEST_NOW * 1000));
        let platform = TestPlatform::on(runtime.db(), &scratch)
            .clock(Arc::clone(&clock) as Arc<dyn Clock>)
            .build()
            .await;
        let mut rig = Self::build(platform, clock);
        rig.runtime = Some(runtime);
        rig._scratch = Some(scratch);
        rig
    }

    /// A rig on the caller's platform, so its users sit beside the rows
    /// the test seeds. Auth runs on the platform's clock; the rig's own
    /// clock only stamps what it seeds.
    pub fn on(platform: &Platform) -> Self {
        Self::build(platform.clone(), Arc::new(ManualClock::at(TEST_NOW * 1000)))
    }

    fn build(platform: Platform, clock: Arc<ManualClock>) -> Self {
        let config = Arc::clone(&platform.settings);
        config
            .save_blocking(SecuritySettings {
                hibp_check: false,
                ..SecuritySettings::default()
            })
            .expect("security settings save");
        config
            .save_secret_blocking(LastFmSettings {
                enabled: true,
                ..LastFmSettings::default()
            })
            .expect("Last.fm settings save");
        let auth = ProdAuth::new(&platform);
        let setup = AuthSetup::build(
            &platform,
            auth.clone(),
            &closed_outbound().sign_in(),
            &droppedneedle_providers::Providers::closed().lastfm,
            Arc::new(
                droppedneedle_media::remotes::plex::ConfiguredPlexServer::new(
                    platform.http.shared().clone(),
                    config.live(),
                    droppedneedle_auth::federated::settings::InstallIds::new(Arc::clone(&config)),
                ),
            ),
            droppedneedle::bootstrap::sign_in_links(&platform),
        );
        Self {
            deps: setup.users,
            gate: setup.session_auth,
            auth,
            clock,
            config,
            platform,
            runtime: None,
            _scratch: None,
        }
    }

    /// Save `api_key` as Last.fm's instance key, with Last.fm on.
    pub fn set_lastfm_instance_key(&self, api_key: &str) {
        self.config
            .save_secret_blocking(LastFmSettings {
                enabled: true,
                api_key: Secret::new(api_key),
                shared_secret: Secret::new("instance-secret"),
            })
            .expect("Last.fm settings save");
    }

    /// The rig's own database. Only a rig from [`AuthRig::new`] has one.
    pub fn runtime(&self) -> &DbRuntime {
        self.runtime
            .as_ref()
            .expect("the rig was built on its own database")
    }

    /// Seed one user, id `user-<username>`, with the local password
    /// [`PASSWORD`].
    pub async fn seed_user(&self, username: &str, role: Role) -> UserRecord {
        let row = UserRecord {
            id: format!("user-{username}"),
            username: Some(username.to_owned()),
            username_display: Some(username.to_owned()),
            display_name: username.to_owned(),
            email: None,
            avatar_url: None,
            role,
            created_at: self.clock.now_unix(),
            last_login_at: None,
        };
        self.auth
            .users
            .insert_with_local_credential(
                row.clone(),
                LocalCredential {
                    id: format!("cred-{username}"),
                    user_id: row.id.clone(),
                    scheme: "argon2id".to_owned(),
                    hash: password_hash().to_owned(),
                },
            )
            .await
            .expect("user seeds");
        row
    }

    /// Seed one standard session for `user_id`, returning its raw token.
    pub async fn seed_session(&self, user_id: &str, session_id: &str) -> String {
        let now = self.clock.now_unix();
        let token = format!("raw-token-for-{session_id}");
        self.auth
            .sessions
            .seed(SessionRecord {
                id: session_id.to_owned(),
                user_id: user_id.to_owned(),
                token_hash: tokens::hash_token(&token),
                kind: SessionKind::Standard,
                label: None,
                issued_at: now,
                expires_at: tokens::expires_at(now),
                last_seen_at: now,
                revoked: false,
                user_agent: None,
            })
            .await
            .expect("session seeds");
        token
    }

    /// The production session gate over the rig's sessions, as the
    /// production builder makes it.
    pub fn session_gate(&self) -> SessionAuth {
        self.gate.clone()
    }

    /// Sign `user_id` in with the raw `token`: one standard session issued
    /// now by the platform clock, which the session gate reads.
    pub async fn sign_in(&self, user_id: &str, token: &str) {
        let now = self.gate.clock.now_unix();
        let token_hash = tokens::hash_token(token);
        self.auth
            .sessions
            .seed(SessionRecord {
                id: format!("signed-in-{}", &token_hash[..16]),
                user_id: user_id.to_owned(),
                token_hash,
                kind: SessionKind::Standard,
                label: None,
                issued_at: now,
                expires_at: tokens::expires_at(now),
                last_seen_at: now,
                revoked: false,
                user_agent: None,
            })
            .await
            .expect("session signs in");
    }
}

/// A signed-in principal for `user_id`, as the session gate would resolve
/// it for a token of `session_kind` on `session_id`.
pub fn test_principal(
    user_id: &str,
    role: Role,
    session_id: &str,
    session_kind: SessionKind,
) -> Principal {
    Principal::for_tests(user_id, role, session_id, session_kind)
}

/// An admin's proof for `user_id`, for calling an admin service directly.
pub fn test_admin(user_id: &str) -> Admin {
    Admin::try_from(test_principal(
        user_id,
        Role::Admin,
        "sess-test-admin",
        SessionKind::Standard,
    ))
    .expect("an admin principal proves the role")
}

async fn insert_test_principal(
    State(principal): State<Principal>,
    mut request: Request,
    next: Next,
) -> Response {
    request.extensions_mut().insert(principal);
    next.run(request).await
}

/// Sign every request to `router` in as `principal`, standing in for the
/// session gate (no token lookup, no origin check).
pub fn with_test_principal(router: Router, principal: Principal) -> Router {
    router.layer(middleware::from_fn_with_state(
        principal,
        insert_test_principal,
    ))
}
