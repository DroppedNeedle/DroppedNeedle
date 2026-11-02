//! The connect-apps settings the Jellyfin routes read.

use droppedneedle_auth::compat_auth::jellyfin::PolicyFacts;
use droppedneedle_kernel::runtime_config::sections::DownloadAccess;
use droppedneedle_media::stream::transcode::{OutFormat, TranscodeSettings};

/// The connect-apps settings subset the shim reads.
#[derive(Debug, Clone)]
pub struct JellyfinSettings {
    /// Kill switch, default off. Disabled → HTTP 404
    /// on every route, before handler lookup (no method enumeration).
    pub enabled: bool,
    /// User-editable advertised name, default "DroppedNeedle".
    pub server_name: String,
    /// User-editable advertised version, default "10.10.6".
    pub server_version: String,
    /// Transcoding master switch.
    pub transcoding_enabled: bool,
    /// Quality ceiling in kbps, default 320: a ceiling, never a trigger.
    pub transcode_max_bitrate_kbps: u32,
    /// Default transcode output (`mp3` or `opus`).
    pub transcode_default_format: String,
    /// Whether ffmpeg is present (silent direct fallback when absent).
    pub ffmpeg_available: bool,
    /// The admin's setting for who may download library files.
    pub download_access: DownloadAccess,
}

impl JellyfinSettings {
    /// The policy values that depend on these settings, for a user with
    /// `role`.
    pub fn policy_facts(&self, role: &str) -> PolicyFacts {
        PolicyFacts {
            audio_transcoding: self.transcoding_enabled && self.ffmpeg_available,
            content_downloading: self.download_access.admits(role),
        }
    }
}

impl JellyfinSettings {
    /// The transcode policy these settings set, for the one decision.
    pub fn transcode_policy(&self) -> TranscodeSettings {
        TranscodeSettings {
            transcoding_enabled: self.transcoding_enabled,
            default_format: if self.transcode_default_format.eq_ignore_ascii_case("opus") {
                OutFormat::Opus
            } else {
                OutFormat::Mp3
            },
            max_bitrate_kbps: i64::from(self.transcode_max_bitrate_kbps),
        }
    }
}

impl Default for JellyfinSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            server_name: "DroppedNeedle".to_owned(),
            server_version: "10.10.6".to_owned(),
            transcoding_enabled: true,
            transcode_max_bitrate_kbps: 320,
            transcode_default_format: "mp3".to_owned(),
            ffmpeg_available: true,
            download_access: DownloadAccess::Everyone,
        }
    }
}
