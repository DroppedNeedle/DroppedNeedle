//! Settings bundle: the section service, its save effects,
//! the verify probes, and the HTTP surface.
//!
//! [`SettingsSetup`] is the one `AppState` field settings adds. It holds
//! the [`SettingsService`](crate::settings::services::SettingsService)
//! with its post-save fan-out wired (provider-cache invalidation over
//! the shared cache plus the jobs-owned events kick), the MusicBrainz
//! lifecycle over the same store, and the connection probes behind the
//! verify endpoints.
//!
//! [`SettingsSetup::gated_router`] serves `/api/v3/settings/*` behind
//! [`require_admin`]: every settings route is admin-only, like v2's
//! whole-router guard. [`SettingsSetup::me_router`] serves the
//! per-user section prefs (`/me/section-prefs`) inside the session
//! gate only.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};

use crate::auth::{
    session::middleware::CurrentSession,
    users::{UsersDeps, roles::Role},
};
use crate::ids::IdGenerator;
use crate::runtime_config::ConfigStore;
use crate::settings::effects::SaveEffects;
use crate::settings::error::SettingsError;
use crate::settings::musicbrainz::MusicBrainzLifecycle;
use crate::settings::section_prefs::{LinkStatus, SectionPrefsStore};
use crate::settings::services::{PolicyImpactBuckets, SettingsService};
use crate::settings::verify::{LiveProbes, VerifyProbes};

/// Section-prefs backends: the toggle store plus per-user link state.
pub struct SectionPrefsDeps {
    /// Toggle store.
    pub store: Arc<dyn SectionPrefsStore>,
    /// Link state.
    pub links: Arc<dyn LinkStatus>,
}

/// Everything the settings routers need, built once at boot.
#[derive(Clone)]
pub struct SettingsSetup {
    service: Arc<SettingsService>,
    users: UsersDeps,
    probes: Arc<dyn VerifyProbes>,
    lifecycle: Arc<MusicBrainzLifecycle>,
    prefs: Option<Arc<SectionPrefsDeps>>,
    buckets: Option<Arc<dyn PolicyImpactBuckets>>,
}

impl SettingsSetup {
    /// Build over the shared store, the wired fan-out, the id mint,
    /// the account rows for gating, and the outbound HTTP client.
    pub fn build(
        store: Arc<ConfigStore>,
        effects: Arc<dyn SaveEffects>,
        ids: Arc<dyn IdGenerator>,
        users: UsersDeps,
        http: reqwest::Client,
    ) -> Self {
        Self {
            service: Arc::new(SettingsService::new(
                store.clone(),
                effects.clone(),
                ids.clone(),
            )),
            users,
            probes: Arc::new(LiveProbes::new(http)),
            lifecycle: Arc::new(MusicBrainzLifecycle::new(store, ids, effects)),
            prefs: None,
            buckets: None,
        }
    }

    /// Attach the section-prefs backends.
    #[must_use]
    pub fn with_section_prefs(
        mut self,
        store: Arc<dyn SectionPrefsStore>,
        links: Arc<dyn LinkStatus>,
    ) -> Self {
        self.prefs = Some(Arc::new(SectionPrefsDeps { store, links }));
        self
    }

    /// Attach the policy-impact bucket counts.
    #[must_use]
    pub fn with_impact_buckets(mut self, buckets: Arc<dyn PolicyImpactBuckets>) -> Self {
        self.buckets = Some(buckets);
        self
    }

    /// Test bundle: scratch store, a live fan-out over a memory cache
    /// and a no-op kick (saves invalidate only their own scratch
    /// entries), live probes, and memory prefs. Tests that verify
    /// connections use [`SettingsSetup::for_tests_with_probes`] with a
    /// scripted fake instead, so no test touches live networks.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(ids: Arc<dyn IdGenerator>, users: UsersDeps) -> Result<Self, String> {
        let http = reqwest::Client::new();
        Self::for_tests_with_probes(ids, users, Arc::new(LiveProbes::new(http)))
    }

    /// Test bundle with scripted verify probes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests_with_probes(
        ids: Arc<dyn IdGenerator>,
        users: UsersDeps,
        probes: Arc<dyn VerifyProbes>,
    ) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};

        use crate::providers::InMemoryProviderCache;
        use crate::providers::cache::ProviderCache;
        use crate::runtime_config::Crypto;
        use crate::settings::effects::{LiveSaveEffects, NoopKick};
        use crate::settings::section_prefs::{MemorySectionPrefsStore, StaticLinkStatus};

        /// Scratch-dir sequence so parallel test states never share a store.
        static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "droppedneedle-settings-test-{}-{seq}",
            std::process::id()
        ));
        let crypto = Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| error.to_string())?;
        let store = Arc::new(
            ConfigStore::open(&dir.join("config.json"), crypto)
                .map_err(|error| error.to_string())?,
        );
        let cache: Arc<dyn ProviderCache> = Arc::new(InMemoryProviderCache::new());
        let effects: Arc<dyn SaveEffects> =
            Arc::new(LiveSaveEffects::new(cache, Arc::new(NoopKick)));
        Ok(Self {
            service: Arc::new(SettingsService::new(
                store.clone(),
                effects.clone(),
                ids.clone(),
            )),
            users,
            probes,
            lifecycle: Arc::new(MusicBrainzLifecycle::new(store, ids, effects)),
            prefs: Some(Arc::new(SectionPrefsDeps {
                store: Arc::new(MemorySectionPrefsStore::new()),
                links: Arc::new(StaticLinkStatus {
                    listenbrainz: false,
                    lastfm: false,
                }),
            })),
            buckets: None,
        })
    }

    /// The wired section service.
    pub fn service(&self) -> &SettingsService {
        &self.service
    }

    /// The verify probes.
    pub fn probes(&self) -> &Arc<dyn VerifyProbes> {
        &self.probes
    }

    /// The MusicBrainz lifecycle.
    pub fn lifecycle(&self) -> &MusicBrainzLifecycle {
        &self.lifecycle
    }

    /// The section-prefs backends, when wired.
    pub fn prefs(&self) -> Option<&Arc<SectionPrefsDeps>> {
        self.prefs.as_ref()
    }

    /// The policy-impact buckets, when wired.
    pub fn buckets(&self) -> Option<&Arc<dyn PolicyImpactBuckets>> {
        self.buckets.as_ref()
    }

    /// Mount the admin-only settings routes. Layers are applied by
    /// `create_app`, plus the [`require_admin`] gate below; handlers
    /// take `State<SettingsSetup>`.
    pub fn gated_router(&self) -> Router {
        use super::handlers as h;
        Router::new()
            .route(
                "/settings/preferences",
                get(h::get_preferences).put(h::put_preferences),
            )
            .route(
                "/settings/library/schedule",
                get(h::get_schedule).put(h::put_schedule),
            )
            .route(
                "/settings/library/watcher",
                get(h::get_watcher).put(h::put_watcher),
            )
            .route("/settings/library", get(h::get_library).put(h::put_library))
            .route(
                "/settings/library/paths",
                post(h::add_library_path).delete(h::remove_library_path),
            )
            .route(
                "/settings/advanced",
                get(h::get_advanced).put(h::put_advanced),
            )
            .route("/settings/cache-ttls", get(h::get_cache_ttls))
            .route(
                "/settings/jellyfin",
                get(h::get_jellyfin).put(h::put_jellyfin),
            )
            .route("/settings/jellyfin/verify", post(h::verify_jellyfin))
            .route(
                "/settings/navidrome",
                get(h::get_navidrome).put(h::put_navidrome),
            )
            .route("/settings/navidrome/verify", post(h::verify_navidrome))
            .route("/settings/plex", get(h::get_plex).put(h::put_plex))
            .route("/settings/plex/verify", post(h::verify_plex))
            .route("/settings/plex/libraries", get(h::get_plex_libraries))
            .route(
                "/settings/listenbrainz",
                get(h::get_listenbrainz).put(h::put_listenbrainz),
            )
            .route(
                "/settings/listenbrainz/verify",
                post(h::verify_listenbrainz),
            )
            .route("/settings/youtube", get(h::get_youtube).put(h::put_youtube))
            .route("/settings/youtube/verify", post(h::verify_youtube))
            .route(
                "/settings/free-music",
                get(h::get_free_music).put(h::put_free_music),
            )
            .route("/settings/get-it", get(h::get_get_it).put(h::put_get_it))
            .route("/settings/events", get(h::get_events).put(h::put_events))
            .route(
                "/settings/events/test-ticketmaster",
                post(h::test_ticketmaster),
            )
            .route("/settings/events/test-skiddle", post(h::test_skiddle))
            .route("/settings/wrapped", get(h::get_wrapped).put(h::put_wrapped))
            .route(
                "/settings/scrobble",
                get(h::get_scrobble).put(h::put_scrobble),
            )
            .route(
                "/settings/primary-source",
                get(h::get_primary_source).put(h::put_primary_source),
            )
            .route(
                "/settings/security",
                get(h::get_security).put(h::put_security),
            )
            .route("/settings/security/verify-hibp", post(h::verify_hibp))
            .route("/settings/oidc", get(h::get_oidc).put(h::put_oidc))
            .route("/settings/oidc/verify", post(h::verify_oidc))
            .route("/settings/lastfm", get(h::get_lastfm).put(h::put_lastfm))
            .route(
                "/settings/connect-apps",
                get(h::get_connect_apps).put(h::put_connect_apps),
            )
            .route(
                "/settings/musicbrainz",
                get(h::get_musicbrainz).put(h::put_musicbrainz),
            )
            .route(
                "/settings/musicbrainz/brainzmash/stage",
                post(h::stage_brainzmash),
            )
            .route(
                "/settings/musicbrainz/brainzmash/consent",
                post(h::consent_brainzmash),
            )
            .route("/settings/musicbrainz/verify", post(h::verify_musicbrainz))
            .route(
                "/settings/musicbrainz/activate",
                post(h::activate_brainzmash),
            )
            .route(
                "/settings/download-client/config",
                get(h::get_slskd).put(h::put_slskd),
            )
            .route("/settings/download-client/test", post(h::test_slskd))
            .route(
                "/settings/download-clients/sabnzbd",
                get(h::get_sabnzbd).put(h::put_sabnzbd),
            )
            .route(
                "/settings/download-clients/sabnzbd/test",
                post(h::test_sabnzbd),
            )
            .route(
                "/settings/download-clients/source-priority",
                get(h::get_source_priority).put(h::put_source_priority),
            )
            .route(
                "/settings/download-clients/policy",
                get(h::get_policy).put(h::put_policy),
            )
            .route(
                "/settings/download-clients/policy-summary",
                get(h::get_policy_summary),
            )
            .route(
                "/settings/download-clients/policy/impact",
                post(h::post_policy_impact),
            )
            .route(
                "/settings/download-clients/wanted",
                get(h::get_wanted).put(h::put_wanted),
            )
            .route(
                "/settings/indexers",
                get(h::list_indexers).post(h::create_indexer),
            )
            .route(
                "/settings/indexers/search-backend",
                get(h::get_search_backend).put(h::put_search_backend),
            )
            .route(
                "/settings/indexers/{id}",
                put(h::update_indexer).delete(h::delete_indexer),
            )
            .route("/settings/indexers/reorder", post(h::reorder_indexers))
            .route("/settings/indexers/test", post(h::test_indexer))
            .route(
                "/settings/prowlarr/config",
                get(h::get_prowlarr).put(h::put_prowlarr),
            )
            .route("/settings/prowlarr/test", post(h::test_prowlarr))
            .route(
                "/settings/library/sync",
                get(h::dropped_library_sync).put(h::dropped_library_sync_put),
            )
            .route(
                "/settings/home",
                get(h::dropped_home).put(h::dropped_home_put),
            )
            .layer(axum::middleware::from_fn_with_state(
                self.clone(),
                require_admin,
            ))
            .with_state(self.clone())
    }

    /// Mount the per-user section-prefs routes inside the session gate
    /// (no admin gate: every signed-in user owns their own toggles).
    pub fn me_router(&self) -> Router {
        use super::handlers as h;
        Router::new()
            .route(
                "/me/section-prefs",
                get(h::get_section_prefs).put(h::put_section_prefs),
            )
            .with_state(self.clone())
    }
}

/// Admin gate: 401 when no session is stashed or the account is gone,
/// 403 when the account is not an admin. The role rereads the user row
/// every request, so promotions and demotions land on the next call.
async fn require_admin(
    State(settings): State<SettingsSetup>,
    mut request: Request,
    next: Next,
) -> Response {
    let missing = || SettingsError::Unauthorized {
        message: "Authentication required".to_owned(),
    };
    let Some(session) = request.extensions().get::<CurrentSession>().cloned() else {
        return missing().into_response();
    };
    let user = match settings.users.users.get_by_id(&session.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return missing().into_response(),
        Err(error) => {
            return SettingsError::internal(
                &format!("settings gate lookup failed: {error}"),
                &*settings.service.ids,
            )
            .into_response();
        }
    };
    if user.role != Role::Admin {
        return SettingsError::Forbidden {
            message: "Admin role required".to_owned(),
        }
        .into_response();
    }
    request
        .extensions_mut()
        .insert(AdminUser { user_id: user.id });
    next.run(request).await
}

/// The gated admin principal, stashed for handlers that need it (the
/// BrainzMash consent records the consenting admin).
#[derive(Debug, Clone)]
pub struct AdminUser {
    /// Owning user id.
    pub user_id: String,
}
