//! Compat settings, read per request.
//!
//! The kill switches, advertised names and transcode policy live in the
//! `connect_apps` settings section, who may download library files in
//! `security_settings` and the stated public addresses in
//! `public_address`. [`LiveSettings`] resolves them on every
//! request, so a change in Settings takes effect at once instead of at the
//! next restart. Tests pin fixed values with `From<Settings>` and
//! `From<JellyfinSettings>`.

use std::sync::Arc;

use crate::jellyfin::seams::JellyfinSettings;
use crate::subsonic::Settings;
use droppedneedle_kernel::runtime_config::origin::Origin;
use droppedneedle_kernel::runtime_config::sections::{
    AudioFormat, ConnectApps, DownloadAccess, PublicAddress, SecuritySettings,
};
use droppedneedle_kernel::runtime_config::{ConfigStore, Live};

/// Where the compat settings come from.
pub trait SettingsSource: Send + Sync {
    /// Subsonic settings for one request.
    fn subsonic(&self) -> Settings;
    /// Jellyfin settings for one request.
    fn jellyfin(&self) -> JellyfinSettings;
    /// The addresses the admin stated under Settings > Security.
    fn public_addresses(&self) -> Vec<Origin>;
}

/// Shared handle to the settings source.
#[derive(Clone)]
pub struct LiveSettings(Arc<dyn SettingsSource>);

impl LiveSettings {
    /// Settings read through live handles on `config` on every call.
    /// `can_transcode` is whether the stream engine was built with an
    /// ffmpeg.
    pub fn from_config(config: &ConfigStore, can_transcode: bool) -> Self {
        Self(Arc::new(ConfigSettings {
            connect_apps: config.live(),
            security: config.live(),
            public_address: config.live(),
            ffmpeg_available: can_transcode,
        }))
    }

    /// Fixed settings for both protocols.
    pub fn fixed(subsonic: Settings, jellyfin: JellyfinSettings) -> Self {
        Self(Arc::new(Fixed {
            subsonic,
            jellyfin,
            public_addresses: Vec::new(),
        }))
    }

    /// These settings, fixed as they are now, with `addresses` as the
    /// stated public addresses.
    pub fn with_public_addresses(&self, addresses: Vec<Origin>) -> Self {
        Self(Arc::new(Fixed {
            subsonic: self.subsonic(),
            jellyfin: self.jellyfin(),
            public_addresses: addresses,
        }))
    }

    /// Subsonic settings now.
    pub fn subsonic(&self) -> Settings {
        self.0.subsonic()
    }

    /// Jellyfin settings now.
    pub fn jellyfin(&self) -> JellyfinSettings {
        self.0.jellyfin()
    }

    /// The stated public addresses now.
    pub fn public_addresses(&self) -> Vec<Origin> {
        self.0.public_addresses()
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
    public_addresses: Vec<Origin>,
}

impl SettingsSource for Fixed {
    fn subsonic(&self) -> Settings {
        self.subsonic.clone()
    }

    fn jellyfin(&self) -> JellyfinSettings {
        self.jellyfin.clone()
    }

    fn public_addresses(&self) -> Vec<Origin> {
        self.public_addresses.clone()
    }
}

struct ConfigSettings {
    connect_apps: Live<ConnectApps>,
    security: Live<SecuritySettings>,
    public_address: Live<PublicAddress>,
    ffmpeg_available: bool,
}

impl ConfigSettings {
    /// The section, or the defaults (both protocols off) when it cannot
    /// be read: a broken config must never open the compat APIs.
    fn section(&self) -> ConnectApps {
        self.connect_apps.get().map_or_else(
            |error| {
                tracing::warn!(%error, "connect_apps settings unreadable; compat APIs stay off");
                ConnectApps::default()
            },
            |section| ConnectApps::clone(&section),
        )
    }
}

impl ConfigSettings {
    /// Who may download library files. Admins only when the setting cannot
    /// be read: a broken config must never widen a permission.
    fn download_access(&self) -> DownloadAccess {
        match self.security.get() {
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

    /// Empty when the list cannot be read: then every app gets the address
    /// it used, as with no list.
    fn public_addresses(&self) -> Vec<Origin> {
        match self.public_address.get() {
            Ok(section) => section.stated(),
            Err(error) => {
                tracing::warn!(%error, "public address settings unreadable; apps get the address they used");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_kernel::runtime_config::Crypto;

    /// What both protocols read as the download setting from settings
    /// holding `config`, copied in as a first start copies `config.json`.
    fn download_access(tag: &str, config: &str) -> [DownloadAccess; 2] {
        let dir = std::env::temp_dir().join(format!("dn-compat-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch folder");
        std::fs::write(dir.join("config.json"), config).expect("config writes");
        let crypto = Crypto::from_key_bytes(&[7u8; 32]).expect("test key");
        let store = ConfigStore::scratch(&dir, crypto).expect("store opens");
        let live = LiveSettings::from_config(&store, false);
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
