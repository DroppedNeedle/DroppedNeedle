//! Why a single-track acquisition skipped a step or failed, in words a
//! person can act on. Every reason has a stable code, one plain sentence,
//! and an action. The text rides on the source error the worker logs and,
//! for the lone-track fallback, in the task manifest.

use std::fmt;

use crate::acquire::downloads::manifest::RecordedReason;

/// One reason from the track-to-album path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackReason {
    /// MusicBrainz could not answer the album lookup.
    AlbumLookupUnavailable,
    /// MusicBrainz does not know the requested recording.
    RecordingUnknown,
    /// MusicBrainz lists no release carrying the recording.
    NoAlbumForRecording,
    /// No Soulseek peer shares the album with the track in it.
    NoAlbumFolder,
    /// No Usenet or plugin release of the album was found.
    NoAlbumRelease,
    /// Every album release was tried and none worked.
    AlbumReleasesExhausted,
    /// Nobody shares the track, inside the album or on its own.
    TrackNotFound,
}

impl TrackReason {
    /// Stable machine code.
    pub fn code(self) -> &'static str {
        match self {
            Self::AlbumLookupUnavailable => "album_lookup_unavailable",
            Self::RecordingUnknown => "recording_unknown",
            Self::NoAlbumForRecording => "no_album_for_recording",
            Self::NoAlbumFolder => "no_album_folder",
            Self::NoAlbumRelease => "no_album_release",
            Self::AlbumReleasesExhausted => "album_releases_exhausted",
            Self::TrackNotFound => "track_not_found",
        }
    }

    /// What happened, in one sentence.
    pub fn message(self) -> &'static str {
        match self {
            Self::AlbumLookupUnavailable => {
                "MusicBrainz could not be reached to find the album this track is on."
            }
            Self::RecordingUnknown => "MusicBrainz does not know this recording any more.",
            Self::NoAlbumForRecording => "MusicBrainz lists no album or single with this track.",
            Self::NoAlbumFolder => "Nobody on Soulseek shares the album with this track in it.",
            Self::NoAlbumRelease => "No release of the album was found for this track.",
            Self::AlbumReleasesExhausted => {
                "Every release of the album was tried and none delivered this track."
            }
            Self::TrackNotFound => {
                "Nobody shares this track, either as part of its album or on its own."
            }
        }
    }

    /// What the person can do about it.
    pub fn action(self) -> &'static str {
        match self {
            Self::AlbumLookupUnavailable => {
                "It tries again on its own. If it keeps happening, check the MusicBrainz settings."
            }
            Self::RecordingUnknown => {
                "Request the track again from its album page so it uses the current recording."
            }
            Self::NoAlbumForRecording | Self::NoAlbumFolder | Self::NoAlbumRelease => {
                "A single shared copy was used instead; check the result in the held list if it looks wrong."
            }
            Self::AlbumReleasesExhausted => {
                "A single shared copy was tried instead; request the whole album if this keeps failing."
            }
            Self::TrackNotFound => {
                "It retries later on its own. You can also request the whole album, or pick another edition."
            }
        }
    }

    /// The reason as the manifest records it.
    pub fn record(self) -> RecordedReason {
        RecordedReason {
            code: self.code().to_owned(),
            message: self.message().to_owned(),
            action: self.action().to_owned(),
        }
    }
}

impl fmt::Display for TrackReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} {}", self.code(), self.message(), self.action())
    }
}
