//! What a committed change to a section sets off.
//!
//! Each section declares its effects on its type
//! ([`Section::EFFECTS`](super::Section::EFFECTS)). The server runs them
//! on the section's change notice, so every save runs them, whoever made
//! it.

/// A provider cache family a section's values feed. The name is the
/// provider cache's source name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheRoot {
    /// ListenBrainz reads.
    ListenBrainz,
    /// Last.fm reads.
    LastFm,
    /// MusicBrainz reads.
    MusicBrainz,
    /// AudioDB reads.
    AudioDb,
    /// AcoustID lookups.
    AcoustId,
}

impl CacheRoot {
    /// Every root, for the test that each one has cached reads to drop.
    pub const ALL: &'static [Self] = &[
        Self::ListenBrainz,
        Self::LastFm,
        Self::MusicBrainz,
        Self::AudioDb,
        Self::AcoustId,
    ];

    /// The provider cache's source name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ListenBrainz => "listenbrainz",
            Self::LastFm => "lastfm",
            Self::MusicBrainz => "musicbrainz",
            Self::AudioDb => "audiodb",
            Self::AcoustId => "acoustid",
        }
    }
}

/// One thing to do after a section changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Drop the cached reads of these providers.
    Invalidate(&'static [CacheRoot]),
    /// Drop cached responses under these key prefixes.
    ClearPrefixes(&'static [&'static str]),
    /// Run the events sweep now.
    KickEventsSweep,
    /// Ask the download worker for a pass now, so requests held for want
    /// of a source start.
    WakeDownloads,
}
