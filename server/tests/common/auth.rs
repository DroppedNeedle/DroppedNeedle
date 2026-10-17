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
use droppedneedle_auth::federated::password_import::PasswordHasher as _;
use droppedneedle_auth::prod::{Argon2idHasher, ProdAuth};
use droppedneedle_auth::session::extract::Transport;
use droppedneedle_auth::session::middleware::CurrentSession;
use droppedneedle_auth::session::store::{SessionKind, SessionRecord, SessionStore as _};
use droppedneedle_auth::session::tokens;
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_auth::users::models::{LocalCredential, UserRecord};
use droppedneedle_auth::users::roles::Role;
use droppedneedle_auth::users::stores::{Clock, UserStore as _};
use droppedneedle_auth::wiring::AuthSetup;
use droppedneedle_kernel::db::DbRuntime;
use droppedneedle_kernel::http_client::HttpClientFactory;
use droppedneedle_kernel::ids::{IdGenerator, UuidGenerator};
use droppedneedle_kernel::runtime_config::secret_sections::LastFmSettings;
use droppedneedle_kernel::runtime_config::sections::SecuritySettings;
use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};

use super::auth_fakes::ManualClock;
use super::{ScratchDir, closed_outbound, config_store, scratch_runtime};

/// The rig's clock at build: 2024-01-01T00:00:00Z.
pub const TEST_NOW: i64 = 1_704_067_200;

/// Every seeded user's password.
pub const PASSWORD: &str = "correct horse battery staple";

/// The production hash of [`PASSWORD`], computed once per test binary so
/// seeding stays fast.
pub fn password_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| Argon2idHasher::new().hash_argon2id(PASSWORD))
}

/// Production auth over a migrated database.
pub struct AuthRig {
    /// The users routes' dependencies, as the production builder wires them.
    pub deps: UsersDeps,
    /// The SQLite stores behind `deps`, for seeding and assertions.
    pub auth: ProdAuth,
    /// The clock behind `deps` and the session list.
    pub clock: Arc<ManualClock>,
    /// The settings `deps` read live.
    pub config: Arc<ConfigStore>,
    runtime: Option<DbRuntime>,
    _scratch: ScratchDir,
}

impl AuthRig {
    /// A rig on its own copy of the migrated database.
    pub async fn new(tag: &str) -> Self {
        let scratch = ScratchDir::new(&format!("auth-{tag}"));
        let runtime = scratch_runtime(&scratch.join("auth.db")).await;
        let mut rig = Self::build(&runtime, scratch);
        rig.runtime = Some(runtime);
        rig
    }

    /// A rig on the caller's database, so its users sit beside the rows
    /// the test seeds. The caller keeps the runtime open.
    pub fn on(runtime: &DbRuntime) -> Self {
        Self::build(runtime, ScratchDir::new("auth-on"))
    }

    fn build(runtime: &DbRuntime, scratch: ScratchDir) -> Self {
        let crypto = Arc::new(Crypto::from_key_bytes(&[7u8; 32]).expect("test key"));
        let config = config_store(&scratch);
        config
            .save(SecuritySettings {
                hibp_check: false,
                ..SecuritySettings::default()
            })
            .expect("security settings save");
        config
            .save_secret(LastFmSettings {
                enabled: true,
                ..LastFmSettings::default()
            })
            .expect("Last.fm settings save");
        let clock = Arc::new(ManualClock::new(TEST_NOW));
        let ids: Arc<dyn IdGenerator> = Arc::new(UuidGenerator);
        let auth = ProdAuth::new(
            runtime.pool(),
            runtime.lane(),
            Arc::clone(&crypto),
            Arc::clone(&ids),
            Arc::clone(&clock) as Arc<dyn Clock>,
            &scratch,
        );
        let http = HttpClientFactory::new().expect("http factory builds");
        let deps = AuthSetup::build_with_upstreams(
            auth.clone(),
            Arc::clone(&config),
            Arc::clone(&crypto),
            &http,
            ids,
            Arc::clone(&clock) as Arc<dyn Clock>,
            "",
            &closed_outbound().sign_in(),
            Arc::new(
                droppedneedle_media::remotes::plex::ConfiguredPlexServer::new(
                    http.shared().clone(),
                    Arc::clone(&config),
                ),
            ),
            droppedneedle::bootstrap::sign_in_links(
                runtime.pool().clone(),
                runtime.lane().clone(),
                crypto,
                Arc::clone(&config),
            ),
        )
        .users;
        Self {
            deps,
            auth,
            clock,
            config,
            runtime: None,
            _scratch: scratch,
        }
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
            .insert(SessionRecord {
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
}

/// What the session gate stashes for `user` on `session_id`.
pub fn test_principal(user: &UserRecord, session_id: &str, companion: bool) -> CurrentSession {
    CurrentSession {
        user_id: user.id.clone(),
        session_id: session_id.to_owned(),
        kind: if companion {
            SessionKind::Companion
        } else {
            SessionKind::Standard
        },
        transport: Transport::Cookie,
    }
}

async fn insert_test_principal(
    State(session): State<CurrentSession>,
    mut request: Request,
    next: Next,
) -> Response {
    request.extensions_mut().insert(session);
    next.run(request).await
}

/// Sign every request to `router` in as `session`, standing in for the
/// session gate (no token lookup, no origin check).
pub fn with_test_principal(router: Router, session: CurrentSession) -> Router {
    router.layer(middleware::from_fn_with_state(
        session,
        insert_test_principal,
    ))
}
