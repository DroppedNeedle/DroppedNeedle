//! Advertised OpenSubsonic extensions.
//!
//! Clients turn features on only for extensions the server lists, so every
//! extension the Subsonic API serves is listed: API-key auth, form posts,
//! transcode offsets, synced lyrics (`getLyricsBySongId`, version 2 adding
//! word timing, singers and translations), playback reports, the
//! index-based play queue, and the transcode decision and stream
//! endpoints. Sound similarity (`getSonicSimilarTracks`, `findSonicPath`)
//! is listed only while it is served: sound analysis on and nine in ten
//! of the library's tracks analysed ([`crate::sound::CompatSound::serves`]).

/// Extension name plus its advertised versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extension {
    /// OpenSubsonic extension name.
    pub name: &'static str,
    /// Advertised versions.
    pub versions: &'static [u32],
}

/// The extensions always served: songLyrics at versions 1 and 2, the rest
/// at version 1.
pub const ALWAYS: [Extension; 7] = [
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
        versions: &[1, 2],
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

/// Sound similarity, version 1, while it is served.
pub const SONIC_SIMILARITY: Extension = Extension {
    name: "sonicSimilarity",
    versions: &[1],
};

/// The advertised set: [`ALWAYS`], then [`SONIC_SIMILARITY`] when
/// `sonic`.
pub fn advertised(sonic: bool) -> Vec<Extension> {
    let mut list = ALWAYS.to_vec();
    if sonic {
        list.push(SONIC_SIMILARITY);
    }
    list
}
