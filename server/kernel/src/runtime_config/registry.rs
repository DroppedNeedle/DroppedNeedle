//! The section list: every settings section, once, by kind.
//!
//! The list classes each section (plain or secret; the indexers and the
//! plugins have their own store methods) and is the source of the stored
//! key list, the [`Listed`] impls that [`Live`](super::store::Live) reads
//! through, and the effects table. Adding a section means adding it here.

use super::effects::Effect;
use super::error::ConfigError;
use super::plain::Plain;
use super::secret_sections::*;
use super::sections::*;
use super::store::{ConfigStore, INSTANCE_ID_KEY};

/// A section on the list, which a [`Live`](super::store::Live) handle can
/// read.
pub trait Listed: Section + std::fmt::Debug + Send + Sync + 'static {
    /// Open the section from its stored value (`None` reads as the
    /// default). Plain sections decode; secret sections also decrypt.
    fn open(
        store: &ConfigStore,
        value: Option<&serde_json::Value>,
    ) -> Result<Plain<Self>, ConfigError>;
}

/// Walks the list.
pub trait Visitor {
    /// A section without secrets.
    fn plain<S: PlainSection + Listed>(&mut self);
    /// A section with secret fields.
    fn secret<S: SecretSection + Listed>(&mut self);
    /// The indexers, whose keys sit in a list.
    fn indexers(&mut self);
    /// The plugins, whose secrets the manifests name.
    fn plugins(&mut self);
}

macro_rules! section_list {
    (plain: [$($plain:ty),* $(,)?], secret: [$($secret:ty),* $(,)?] $(,)?) => {
        $(
            impl PlainSection for $plain {}

            impl Listed for $plain {
                fn open(
                    store: &ConfigStore,
                    value: Option<&serde_json::Value>,
                ) -> Result<Plain<Self>, ConfigError> {
                    store.open_plain(value)
                }
            }
        )*
        $(
            impl Listed for $secret {
                fn open(
                    store: &ConfigStore,
                    value: Option<&serde_json::Value>,
                ) -> Result<Plain<Self>, ConfigError> {
                    store.open_secret(value)
                }
            }
        )*

        /// The key of every section on the list.
        pub const SECTION_KEYS: &[&str] = &[
            $(<$plain as Section>::KEY,)*
            $(<$secret as Section>::KEY,)*
            <Indexers as Section>::KEY,
            <Plugins as Section>::KEY,
        ];

        /// Every settings key v3 stores: the sections plus the instance id.
        /// The one-time copy of `config.json` ([`super::ingest`]) copies
        /// only these and names any other key it leaves behind.
        pub const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
            $(<$plain as Section>::KEY,)*
            $(<$secret as Section>::KEY,)*
            <Indexers as Section>::KEY,
            <Plugins as Section>::KEY,
            INSTANCE_ID_KEY,
        ];

        /// Visit every section on the list.
        pub fn for_each(visitor: &mut impl Visitor) {
            $(visitor.plain::<$plain>();)*
            $(visitor.secret::<$secret>();)*
            visitor.indexers();
            visitor.plugins();
        }
    };
}

section_list! {
    plain: [
        UserPreferences,
        LibraryScanSchedule,
        FilesystemWatcher,
        WantedWatcher,
        SourcePriority,
        UsenetBackendSetting,
        PrimaryMusicSource,
        GetIt,
        SecuritySettings,
        PublicAddress,
        ConnectApps,
        DownloadPolicy,
        MusicBrainzSettings,
        LyricsSettings,
        InternalState,
        LibraryManagement,
        EditionPreferences,
    ],
    secret: [
        ListenBrainzConnection,
        YouTubeConnection,
        SpotifySettings,
        EventsSettings,
        WrappedSettings,
        OidcConnection,
        LastFmSettings,
        TypedLibrary,
        AdvancedSettings,
        JellyfinConnection,
        NavidromeConnection,
        PlexConnection,
        SlskdConnection,
        DownloadClients,
        ProwlarrConnection,
        LidarrImportConnection,
    ],
}

/// Every section that declares effects, with them.
#[must_use]
pub fn effects() -> Vec<(&'static str, &'static [Effect])> {
    struct Collect(Vec<(&'static str, &'static [Effect])>);

    impl Collect {
        fn add<S: Section>(&mut self) {
            if !S::EFFECTS.is_empty() {
                self.0.push((S::KEY, S::EFFECTS));
            }
        }
    }

    impl Visitor for Collect {
        fn plain<S: PlainSection + Listed>(&mut self) {
            self.add::<S>();
        }
        fn secret<S: SecretSection + Listed>(&mut self) {
            self.add::<S>();
        }
        fn indexers(&mut self) {
            self.add::<Indexers>();
        }
        fn plugins(&mut self) {
            self.add::<Plugins>();
        }
    }

    let mut collect = Collect(Vec::new());
    for_each(&mut collect);
    collect.0
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use serde::Serialize;
    use serde::de::DeserializeOwned;

    use super::*;
    use crate::runtime_config::Crypto;
    use crate::runtime_config::store::TestDir;

    #[test]
    fn every_key_is_listed_once() {
        let unique: HashSet<&str> = KNOWN_TOP_LEVEL_KEYS.iter().copied().collect();
        assert_eq!(unique.len(), KNOWN_TOP_LEVEL_KEYS.len());
        assert!(!KNOWN_TOP_LEVEL_KEYS.contains(&"key_check"));
    }

    /// Fills every string in `T`'s default with its own sentinel, wherever
    /// a sentinel decodes and is kept (a lenient enum swallows one).
    /// Returns the value and the sentinels its
    /// `Debug` does not show: those sit in `Secret` fields, so this finds
    /// the secrets from the type.
    fn fill<T: Default + Serialize + DeserializeOwned + std::fmt::Debug>(
        tag: &str,
    ) -> (T, Vec<String>) {
        let mut value = serde_json::to_value(T::default()).unwrap();
        let mut paths = Vec::new();
        string_paths(&value, &mut Vec::new(), &mut paths);
        let mut placed = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let sentinel = format!("sentinel-{tag}-{index}");
            let slot = slot(&mut value, path);
            let before = std::mem::replace(slot, serde_json::Value::String(sentinel.clone()));
            let kept = serde_json::from_value::<T>(value.clone())
                .ok()
                .and_then(|decoded| serde_json::to_string(&decoded).ok())
                .is_some_and(|text| text.contains(sentinel.as_str()));
            if kept {
                placed.push(sentinel);
            } else {
                *self::slot(&mut value, path) = before;
            }
        }
        let filled: T = serde_json::from_value(value).unwrap();
        let shown = format!("{filled:?}");
        let hidden = placed
            .into_iter()
            .filter(|sentinel| !shown.contains(sentinel.as_str()))
            .collect();
        (filled, hidden)
    }

    fn string_paths(value: &serde_json::Value, at: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        match value {
            serde_json::Value::String(_) => out.push(at.clone()),
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    at.push(key.clone());
                    string_paths(child, at, out);
                    at.pop();
                }
            }
            _ => {}
        }
    }

    fn slot<'a>(value: &'a mut serde_json::Value, path: &[String]) -> &'a mut serde_json::Value {
        path.iter().fold(value, |at, key| &mut at[key.as_str()])
    }

    fn assert_absent(form: &str, text: &str, sentinels: &[String]) {
        for sentinel in sentinels {
            assert!(
                !text.contains(sentinel.as_str()),
                "{form} form holds secret {sentinel}: {text}"
            );
        }
    }

    struct NoLeaks {
        store: ConfigStore,
    }

    impl Visitor for NoLeaks {
        fn plain<S: PlainSection + Listed>(&mut self) {
            let (_, hidden) = fill::<S>(S::KEY);
            assert!(hidden.is_empty(), "{} holds a secret but is plain", S::KEY);
        }

        fn secret<S: SecretSection + Listed>(&mut self) {
            let (filled, hidden) = fill::<S>(S::KEY);
            assert!(!hidden.is_empty(), "{} shows no secret", S::KEY);
            let sealed = self.store.seal(filled, S::default()).unwrap();
            let stored = serde_json::to_string(&sealed).unwrap();
            assert_absent("stored", &stored, &hidden);
            let masked = self.store.mask_sealed(sealed).unwrap();
            let masked = serde_json::to_string(&masked).unwrap();
            assert_absent("masked", &masked, &hidden);
        }

        fn indexers(&mut self) {
            let (mut indexer, hidden) = fill::<NewznabIndexer>("indexer");
            assert!(!hidden.is_empty(), "an indexer shows no secret");
            indexer.id = String::new();
            self.store.save_indexer_blocking(indexer).unwrap();
            let stored = self.store.stored_text(Indexers::KEY).unwrap();
            assert_absent("stored", &stored, &hidden);
            let masked = serde_json::to_string(&self.store.get_indexers().unwrap()).unwrap();
            assert_absent("masked", &masked, &hidden);
        }

        fn plugins(&mut self) {
            let sentinel = "sentinel-plugin-secret".to_owned();
            let secret_keys: HashSet<String> = HashSet::from(["token".to_owned()]);
            let config = PluginConfig {
                enabled: true,
                settings: HashMap::from([
                    ("token".to_owned(), sentinel.clone()),
                    ("shown".to_owned(), "plain value".to_owned()),
                ]),
            };
            self.store
                .save_plugin_blocking("sentinel", config, &secret_keys)
                .unwrap();
            let stored = self.store.stored_text(Plugins::KEY).unwrap();
            assert_absent("stored", &stored, std::slice::from_ref(&sentinel));
            let masked = self
                .store
                .get_plugin_masked("sentinel", &secret_keys)
                .unwrap();
            let masked = serde_json::to_string(&masked).unwrap();
            assert_absent("masked", &masked, std::slice::from_ref(&sentinel));
        }
    }

    /// Every section on the list, its secrets filled with sentinels, holds
    /// none of them in its stored or its masked form.
    #[test]
    fn no_section_on_the_list_leaks_a_secret() {
        let dir = TestDir::new("leaks");
        let store =
            ConfigStore::scratch(dir.path(), Crypto::from_key_bytes(&[5u8; 32]).unwrap()).unwrap();
        for_each(&mut NoLeaks { store });
    }
}
