//! Advertised OpenSubsonic extensions, and the one choice v2
//! left open.
//!
//! THE DIVERGENCE (stage0-compat section 1.1): v2's
//! `capability_matrix.json` advertises 3 extensions and marks
//! `transcoding` implemented-but-unadvertised with blocker
//! `real_target_client_certification`, while v2's router advertises all
//! 4. v3 resolves this to ONE choice:
//!
//! THE MATRIX WINS: 3 advertised, `transcoding` stays unadvertised.
//!
//! Why: the matrix evidence says no real client was ever available
//! against an authorized non-deploy target, so every newly implemented
//! extension stays unadvertised until certified. Advertising
//! `transcoding` invites clients into the `getTranscodeDecision` /
//! `getTranscodeStream` flow, which no certified client has exercised;
//! stripping an advertised extension later breaks clients that adapted
//! to it, while adding one is always safe. The transcode endpoints keep
//! serving (implemented, tested) without being advertised, exactly like
//! the other three certified-pending extensions.

/// Extension name plus its advertised versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extension {
    /// OpenSubsonic extension name.
    pub name: &'static str,
    /// Advertised versions.
    pub versions: &'static [u32],
}

/// The advertised set: exactly 3, all v1 (matrix wins over router).
pub const ADVERTISED: [Extension; 3] = [
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
];

/// Implemented and served but NOT advertised, all v1, each blocked on
/// `real_target_client_certification` (matrix
/// `implemented_unadvertised_extensions`, pinned in `contract.json`).
pub const IMPLEMENTED_UNADVERTISED: [Extension; 4] = [
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

/// Deliberately unadvertised forever (matrix
/// `deliberately_unadvertised`).
pub const NEVER_ADVERTISED: [&str; 9] = [
    "sonicSimilarity",
    "songLyrics:2",
    "podcasts",
    "video",
    "radio",
    "chat",
    "shares",
    "jukebox",
    "userAdministration",
];

/// Whether `name` appears in the advertised set.
pub fn is_advertised(name: &str) -> bool {
    ADVERTISED.iter().any(|ext| ext.name == name)
}
