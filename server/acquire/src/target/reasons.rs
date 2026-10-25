//! Why a single-track acquisition skipped a step or failed. Each reason is
//! one of the crate's [`Reason`]s; its sentence and action come from there.

use std::fmt;

use crate::reason::Reason;

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

impl From<TrackReason> for Reason {
    fn from(reason: TrackReason) -> Self {
        match reason {
            TrackReason::AlbumLookupUnavailable => Reason::AlbumLookupUnavailable,
            TrackReason::RecordingUnknown => Reason::RecordingUnknown,
            TrackReason::NoAlbumForRecording => Reason::NoAlbumForRecording,
            TrackReason::NoAlbumFolder => Reason::NoAlbumFolder,
            TrackReason::NoAlbumRelease => Reason::NoAlbumRelease,
            TrackReason::AlbumReleasesExhausted => Reason::AlbumReleasesExhausted,
            TrackReason::TrackNotFound => Reason::TrackNotFound,
        }
    }
}

impl TrackReason {
    /// The track reason `reason` is, when it is one.
    pub fn from_reason(reason: Reason) -> Option<Self> {
        Some(match reason {
            Reason::AlbumLookupUnavailable => Self::AlbumLookupUnavailable,
            Reason::RecordingUnknown => Self::RecordingUnknown,
            Reason::NoAlbumForRecording => Self::NoAlbumForRecording,
            Reason::NoAlbumFolder => Self::NoAlbumFolder,
            Reason::NoAlbumRelease => Self::NoAlbumRelease,
            Reason::AlbumReleasesExhausted => Self::AlbumReleasesExhausted,
            Reason::TrackNotFound => Self::TrackNotFound,
            _ => return None,
        })
    }

    /// Stable machine code.
    pub fn code(self) -> &'static str {
        Reason::from(self).code()
    }

    /// What happened, in one sentence.
    pub fn message(self) -> &'static str {
        Reason::from(self).message()
    }
}

impl fmt::Display for TrackReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = Reason::from(*self);
        write!(
            f,
            "{}: {} {}",
            reason.code(),
            reason.message(),
            reason.action()
        )
    }
}
