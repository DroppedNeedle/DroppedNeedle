//! What every area is built from: the deployment config, the database, the
//! settings, the data key, the HTTP clients, ids, the clock, the event hub
//! and the loop supervisor.
//!
//! [`Platform::new`] is the only constructor and the one place the event hub
//! is made, so every publisher holds the hub open streams read from the
//! moment it is built. Production opens the parts in the server's
//! bootstrap; tests open them on a scratch copy of the migrated database.
//! Areas take `&Platform` while they are built and keep only what they use.

use std::sync::Arc;

use crate::{
    clock::Clock,
    config::AppConfig,
    db::Db,
    events::EventHub,
    http_client::HttpClientFactory,
    ids::IdGenerator,
    jobs::Supervisor,
    runtime_config::{ConfigStore, Crypto},
};

/// The shared values every area is built from. Cheap to clone.
#[derive(Clone)]
#[non_exhaustive]
pub struct Platform {
    /// Deployment configuration.
    pub config: AppConfig,
    /// The one database handle.
    pub db: Db,
    /// The settings; areas take live handles from it with `live::<S>()`.
    pub settings: Arc<ConfigStore>,
    /// The data key that seals stored secrets.
    pub crypto: Arc<Crypto>,
    /// Outbound HTTP clients.
    pub http: HttpClientFactory,
    /// Fresh ids.
    pub ids: Arc<dyn IdGenerator>,
    /// Wall time: the clock the supervisor stamps its rows with.
    pub clock: Arc<dyn Clock>,
    /// The live event hub behind the event stream.
    pub events: EventHub,
    /// The loop supervisor every background loop runs on.
    pub supervisor: Supervisor,
}

/// The opened parts a [`Platform`] is made from.
pub struct PlatformParts {
    /// Deployment configuration.
    pub config: AppConfig,
    /// The one database handle.
    pub db: Db,
    /// The settings over `db`.
    pub settings: Arc<ConfigStore>,
    /// The data key.
    pub crypto: Arc<Crypto>,
    /// Outbound HTTP clients.
    pub http: HttpClientFactory,
    /// Fresh ids.
    pub ids: Arc<dyn IdGenerator>,
    /// The started loop supervisor; its clock becomes the platform's.
    pub supervisor: Supervisor,
}

impl Platform {
    /// The platform over `parts`, with a new event hub.
    pub fn new(parts: PlatformParts) -> Self {
        let PlatformParts {
            config,
            db,
            settings,
            crypto,
            http,
            ids,
            supervisor,
        } = parts;
        Self {
            config,
            db,
            settings,
            crypto,
            http,
            ids,
            clock: Arc::clone(supervisor.clock()),
            events: EventHub::new(),
            supervisor,
        }
    }
}
