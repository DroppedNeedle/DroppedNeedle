//! Advertised OpenSubsonic extensions.
//!
//! Clients turn features on only for extensions the server lists, so every
//! extension the Subsonic API serves is listed: API-key auth, form posts,
//! transcode offsets, synced lyrics (`getLyricsBySongId`), playback reports,
//! the index-based play queue, and the transcode decision and stream
//! endpoints. Navidrome advertises the same set.

/// Extension name plus its advertised versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extension {
    /// OpenSubsonic extension name.
    pub name: &'static str,
    /// Advertised versions.
    pub versions: &'static [u32],
}

/// The advertised set: every served extension, all v1.
pub const ADVERTISED: [Extension; 7] = [
    Extension {
        name: "apiKeyAuthentication",
        versions: &[1],
    },
    Extension {
        name: "formPost",
        versions: &[1],
    },
    Extension {
        name: "transcodeOffset",
        versions: &[1],
    },
    Extension {
        name: "songLyrics",
        versions: &[1],
    },
    Extension {
        name: "playbackReport",
        versions: &[1],
    },
    Extension {
        name: "indexBasedQueue",
        versions: &[1],
    },
    Extension {
        name: "transcoding",
        versions: &[1],
    },
];
