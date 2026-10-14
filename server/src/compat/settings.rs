//! Compat settings, read per request.
//!
//! The kill switches, advertised names and transcode policy live in the
//! `connect_apps` settings section, and who may download library files in
//! `security_settings`. [`LiveSettings`] resolves them on every
//! request, so a change in Settings takes effect at once instead of at the
//! next restart. Tests pin fixed values with `From<Settings>` and
//! `From<JellyfinSettings>`.

use std::sync::Arc;

use crate::compat::jellyfin::seams::JellyfinSettings;
use crate::compat::subsonic::Settings;
use crate::runtime_config::ConfigStore;
use crate::runtime_config::sections::{AudioFormat, ConnectApps, DownloadAccess, SecuritySettings};

/// Where the compat settings come from.
pub trait SettingsSource: Send + Sync {
    /// Subsonic settings for one request.
    fn subsonic(&self) -> Settings;
    /// Jellyfin settings for one request.
    fn jellyfin(&self) -> JellyfinSettings;
}

/// Shared handle to the settings source.
#[derive(Clone)]
pub struct LiveSettings(Arc<dyn SettingsSource>);

impl LiveSettings {
    /// Settings read from the config store on every call.
    pub fn from_config(store: Arc<ConfigStore>) -> Self {
        Self(Arc::new(ConfigSettings {
            store,
            ffmpeg_available: crate::stream::transcode::ffmpeg_available(),
        }))
    }

    /// Fixed settings for both protocols.
    pub fn fixed(subsonic: Settings, jellyfin: JellyfinSettings) -> Self {
        Self(Arc::new(Fixed { subsonic, jellyfin }))
    }

    /// Subsonic settings now.
    pub fn subsonic(&self) -> Settings {
        self.0.subsonic()
    }

    /// Jellyfin settings now.
    pub fn jellyfin(&self) -> JellyfinSettings {
        self.0.jellyfin()
    }
}

impl From<Settings> for LiveSettings {
    fn from(subsonic: Settings) -> Self {
        Self::fixed(subsonic, JellyfinSettings::default())
    }
}

impl From<JellyfinSettings> for LiveSettings {
    fn from(jellyfin: JellyfinSettings) -> Self {
        Self::fixed(Settings::default(), jellyfin)
    }
}

struct Fixed {
    subsonic: Settings,
    jellyfin: JellyfinSettings,
}

impl SettingsSource for Fixed {
    fn subsonic(&self) -> Settings {
        self.subsonic.clone()
    }

    fn jellyfin(&self) -> JellyfinSettings {
        self.jellyfin.clone()
    }
}

struct ConfigSettings {
    store: Arc<ConfigStore>,
    ffmpeg_available: bool,
}

impl ConfigSettings {
    /// The section, or the defaults (both protocols off) when it cannot
    /// be read: a broken config must never open the compat APIs.
    fn section(&self) -> ConnectApps {
        self.store.get::<ConnectApps>().unwrap_or_else(|error| {
            tracing::warn!(%error, "connect_apps settings unreadable; compat APIs stay off");
            ConnectApps::default()
        })
    }
}

impl ConfigSettings {
    /// Who may download library files. Admins only when the setting cannot
    /// be read: a broken config must never widen a permission.
    fn download_access(&self) -> DownloadAccess {
        match self.store.get::<SecuritySettings>() {
            Ok(security) => security.library_download_access,
            Err(error) => {
                tracing::warn!(%error, "security settings unreadable; compat downloads are admin-only");
                DownloadAccess::Admin
            }
        }
    }
}

fn output_format(format: AudioFormat) -> String {
    match format {
        AudioFormat::Opus => "opus".to_owned(),
        AudioFormat::Mp3 | AudioFormat::Flac => "mp3".to_owned(),
    }
}

impl SettingsSource for ConfigSettings {
    fn subsonic(&self) -> Settings {
        let apps = self.section();
        Settings {
            enabled: apps.subsonic_enabled,
            server_name: apps.advertise_server_name,
            server_version: apps.advertise_server_version,
            transcoding_enabled: apps.transcoding_enabled,
            transcode_default_format: output_format(apps.transcode_default_format),
            transcode_max_bitrate_kbps: apps.transcode_max_bitrate_kbps,
            ffmpeg_available: self.ffmpeg_available,
            base_url: String::new(),
            download_access: self.download_access(),
        }
    }

    fn jellyfin(&self) -> JellyfinSettings {
        let apps = self.section();
        JellyfinSettings {
            enabled: apps.jellyfin_enabled,
            server_name: apps.advertise_server_name,
            server_version: apps.advertise_server_version,
            transcoding_enabled: apps.transcoding_enabled,
            transcode_max_bitrate_kbps: apps.transcode_max_bitrate_kbps.clamp(32, 1411) as u32,
            transcode_default_format: output_format(apps.transcode_default_format),
            ffmpeg_available: self.ffmpeg_available,
            download_access: self.download_access(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_config::Crypto;

    /// What both protocols read as the download setting from a config file
    /// holding `config`.
    fn download_access(tag: &str, config: &str) -> [DownloadAccess; 2] {
        let dir = std::env::temp_dir().join(format!("dn-compat-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch folder");
        std::fs::write(dir.join("config.json"), config).expect("config writes");
        let crypto = Crypto::from_key_bytes(&[7u8; 32]).expect("test key");
        let store = ConfigStore::open(&dir.join("config.json"), crypto).expect("store opens");
        let live = LiveSettings::from_config(Arc::new(store));
        std::fs::remove_dir_all(&dir).expect("scratch folder removed");
        [
            live.jellyfin().download_access,
            live.subsonic().download_access,
        ]
    }

    /// A security section that cannot be decoded never opens downloads in
    /// player apps: they are left to admins, not to everyone.
    #[test]
    fn unreadable_security_settings_leave_downloads_to_admins() {
        for (tag, section) in [
            ("unknown", r#"{"library_download_access": "nobody"}"#),
            ("mistyped", "7"),
        ] {
            let config = format!(r#"{{"security_settings": {section}}}"#);
            assert_eq!(
                download_access(tag, &config),
                [DownloadAccess::Admin; 2],
                "{section}"
            );
        }
        // A section that was never saved is the default, not a fault.
        assert_eq!(
            download_access("absent", "{}"),
            [DownloadAccess::Everyone; 2]
        );
    }
}
