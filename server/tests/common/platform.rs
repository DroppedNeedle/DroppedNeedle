//! The platform a test rig builds its bundles from, made by the same
//! `Platform::new` production uses, over a scratch copy of the migrated
//! database.
//!
//! Build one per database, where the database opens, and hand it to every
//! bundle the test builds: a second supervisor on the same database would
//! clear the first one's loop rows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use droppedneedle_kernel::{
    clock::{Clock, SystemClock},
    config::AppConfig,
    db::Db,
    http_client::HttpClientFactory,
    ids::{IdGenerator, UuidGenerator},
    jobs::{CancellationToken, Supervisor},
    platform::{Platform, PlatformParts},
    runtime_config::{ConfigStore, Crypto, LegacyConfig},
};

/// The test key every rig seals with.
pub const TEST_KEY: [u8; 32] = [7u8; 32];

/// A platform under construction. Every part has a default.
pub struct TestPlatform {
    db: Db,
    dir: PathBuf,
    clock: Arc<dyn Clock>,
    config: Option<AppConfig>,
    settings: Option<Arc<ConfigStore>>,
    supervisor: Option<Supervisor>,
    ids: Option<Arc<dyn IdGenerator>>,
}

impl TestPlatform {
    /// A platform on `db`, with deployment paths under `dir`.
    pub fn on(db: &Db, dir: &Path) -> Self {
        Self {
            db: db.clone(),
            dir: dir.to_path_buf(),
            clock: Arc::new(SystemClock),
            config: None,
            settings: None,
            supervisor: None,
            ids: None,
        }
    }

    /// Start the supervisor on `clock` (the system clock by default); it
    /// becomes the platform's clock.
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Use `config` instead of a default rooted at the scratch folder.
    pub fn config(mut self, config: AppConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Use a settings store the test opened itself.
    pub fn settings(mut self, settings: Arc<ConfigStore>) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Use a supervisor the test already runs, and its clock.
    pub fn supervisor(mut self, supervisor: Supervisor) -> Self {
        self.supervisor = Some(supervisor);
        self
    }

    /// Use `ids` instead of fresh UUIDs.
    pub fn ids(mut self, ids: Arc<dyn IdGenerator>) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Open what is left at its default and make the platform.
    pub async fn build(self) -> Platform {
        let settings = match self.settings {
            Some(settings) => settings,
            None => Arc::new(
                ConfigStore::open(
                    self.db.clone(),
                    Crypto::from_key_bytes(&TEST_KEY).expect("test key"),
                    Arc::new(SystemClock),
                    LegacyConfig::at(&self.dir, false),
                )
                .await
                .expect("settings open on the test database"),
            ),
        };
        let supervisor = match self.supervisor {
            Some(supervisor) => supervisor,
            None => Supervisor::start(self.db.clone(), self.clock, CancellationToken::new())
                .await
                .expect("loop supervisor starts"),
        };
        Platform::new(PlatformParts {
            config: self
                .config
                .unwrap_or_else(|| AppConfig::with_root(0, &self.dir)),
            db: self.db,
            settings,
            crypto: Arc::new(Crypto::from_key_bytes(&TEST_KEY).expect("test key")),
            http: HttpClientFactory::new().expect("http factory builds"),
            ids: self.ids.unwrap_or_else(|| Arc::new(UuidGenerator)),
            supervisor,
        })
    }
}
