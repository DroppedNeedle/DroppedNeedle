//! Why a download, a held file or an upload sits where it does.
//!
//! One [`Reason`] covers every hold, refusal, failure and wait of the
//! acquire crate. Its stored spelling is its serde name; the sentence a
//! person reads and what they can do about it are worked out from it,
//! never stored. Per-area enums (the track-to-album path, the worker's
//! outcomes) convert into it, and nothing reads a reason back out of text.
//!
//! Two separate questions, two exhaustive answers:
//! - [`Reason::kind`] says where a reason shows and what it offers.
//! - [`Reason::against_source`] says whether it counts against the source
//!   that delivered the files: blocklisted and the track asked of another
//!   source, or not.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One reason, stored and sent by its snake_case spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    // Holds: a person decides.
    WeakMatch,
    NoArtistEvidence,
    MostlyUnmatched,
    TagMismatch,
    FingerprintMismatch,
    FingerprintUnconfirmed,
    AlbumVersionOnly,
    NoRelease,
    NoTracks,
    VersionLowerQuality,
    NoBetterThanYours,
    OwnedElsewhere,
    OwnedOnAlbum,
    NoAlbumMatch,
    AlbumNotFound,
    TagsDisagree,
    NotChosenEdition,
    Samples,
    TooManyFiles,
    Interrupted,
    ImportFailed,
    HeldByEarlierVersion,
    // Library: our side could not take the files yet.
    TargetOccupied,
    UpgradeBlocked,
    LibraryChanged,
    LibraryUnavailable,
    NamingScriptFailed,
    LibraryFolderMissing,
    LocalFault,
    // Source: this download is skipped and another tried.
    FilesMissing,
    FilesUnlisted,
    OutsideDownloadsFolder,
    SizeMismatch,
    Corrupt,
    NoAudio,
    Sample,
    WrongEdition,
    NotPinnedEdition,
    WrongAlbum,
    QualityRejected,
    NotAnUpgrade,
    WrongTrack,
    Stalled,
    QueueTimeout,
    QueueGaveWay,
    NeverStarted,
    Deadline,
    SourceFailed,
    NoSource,
    DownloadFailed,
    AlbumLookupUnavailable,
    RecordingUnknown,
    NoAlbumForRecording,
    NoAlbumFolder,
    NoAlbumRelease,
    AlbumReleasesExhausted,
    TrackNotFound,
    // Wait: nothing is wrong yet; the work resumes by itself.
    NoDownloadSource,
    SettingsUnreadable,
    JournalUnreadable,
    MusicbrainzUnavailable,
    HeldForReview,
    TracksMissing,
    // Settled: a final word that needs nothing.
    AlreadyOwned,
    Cancelled,
    MovedOn,
}

/// Where a reason shows and what it offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReasonKind {
    /// A person decides; shown in the held list.
    Hold,
    /// Our side could not take the files yet; they can be retried.
    Library,
    /// This download is skipped and another source tried.
    Source,
    /// Nothing is wrong yet; the work waits and resumes by itself.
    Wait,
    /// A final word that needs nothing.
    Settled,
}

impl Reason {
    /// Every reason, in declaration order.
    pub const ALL: [Reason; 66] = [
        Self::WeakMatch,
        Self::NoArtistEvidence,
        Self::MostlyUnmatched,
        Self::TagMismatch,
        Self::FingerprintMismatch,
        Self::FingerprintUnconfirmed,
        Self::AlbumVersionOnly,
        Self::NoRelease,
        Self::NoTracks,
        Self::VersionLowerQuality,
        Self::NoBetterThanYours,
        Self::OwnedElsewhere,
        Self::OwnedOnAlbum,
        Self::NoAlbumMatch,
        Self::AlbumNotFound,
        Self::TagsDisagree,
        Self::NotChosenEdition,
        Self::Samples,
        Self::TooManyFiles,
        Self::Interrupted,
        Self::ImportFailed,
        Self::HeldByEarlierVersion,
        Self::TargetOccupied,
        Self::UpgradeBlocked,
        Self::LibraryChanged,
        Self::LibraryUnavailable,
        Self::NamingScriptFailed,
        Self::LibraryFolderMissing,
        Self::LocalFault,
        Self::FilesMissing,
        Self::FilesUnlisted,
        Self::OutsideDownloadsFolder,
        Self::SizeMismatch,
        Self::Corrupt,
        Self::NoAudio,
        Self::Sample,
        Self::WrongEdition,
        Self::NotPinnedEdition,
        Self::WrongAlbum,
        Self::QualityRejected,
        Self::NotAnUpgrade,
        Self::WrongTrack,
        Self::Stalled,
        Self::QueueTimeout,
        Self::QueueGaveWay,
        Self::NeverStarted,
        Self::Deadline,
        Self::SourceFailed,
        Self::NoSource,
        Self::DownloadFailed,
        Self::AlbumLookupUnavailable,
        Self::RecordingUnknown,
        Self::NoAlbumForRecording,
        Self::NoAlbumFolder,
        Self::NoAlbumRelease,
        Self::AlbumReleasesExhausted,
        Self::TrackNotFound,
        Self::NoDownloadSource,
        Self::SettingsUnreadable,
        Self::JournalUnreadable,
        Self::MusicbrainzUnavailable,
        Self::HeldForReview,
        Self::TracksMissing,
        Self::AlreadyOwned,
        Self::Cancelled,
        Self::MovedOn,
    ];

    /// The stored spelling.
    pub const fn code(self) -> &'static str {
        match self {
            Self::WeakMatch => "weak_match",
            Self::NoArtistEvidence => "no_artist_evidence",
            Self::MostlyUnmatched => "mostly_unmatched",
            Self::TagMismatch => "tag_mismatch",
            Self::FingerprintMismatch => "fingerprint_mismatch",
            Self::FingerprintUnconfirmed => "fingerprint_unconfirmed",
            Self::AlbumVersionOnly => "album_version_only",
            Self::NoRelease => "no_release",
            Self::NoTracks => "no_tracks",
            Self::VersionLowerQuality => "version_lower_quality",
            Self::NoBetterThanYours => "no_better_than_yours",
            Self::OwnedElsewhere => "owned_elsewhere",
            Self::OwnedOnAlbum => "owned_on_album",
            Self::NoAlbumMatch => "no_album_match",
            Self::AlbumNotFound => "album_not_found",
            Self::TagsDisagree => "tags_disagree",
            Self::NotChosenEdition => "not_chosen_edition",
            Self::Samples => "samples",
            Self::TooManyFiles => "too_many_files",
            Self::Interrupted => "interrupted",
            Self::ImportFailed => "import_failed",
            Self::HeldByEarlierVersion => "held_by_earlier_version",
            Self::TargetOccupied => "target_occupied",
            Self::UpgradeBlocked => "upgrade_blocked",
            Self::LibraryChanged => "library_changed",
            Self::LibraryUnavailable => "library_unavailable",
            Self::NamingScriptFailed => "naming_script_failed",
            Self::LibraryFolderMissing => "library_folder_missing",
            Self::LocalFault => "local_fault",
            Self::FilesMissing => "files_missing",
            Self::FilesUnlisted => "files_unlisted",
            Self::OutsideDownloadsFolder => "outside_downloads_folder",
            Self::SizeMismatch => "size_mismatch",
            Self::Corrupt => "corrupt",
            Self::NoAudio => "no_audio",
            Self::Sample => "sample",
            Self::WrongEdition => "wrong_edition",
            Self::NotPinnedEdition => "not_pinned_edition",
            Self::WrongAlbum => "wrong_album",
            Self::QualityRejected => "quality_rejected",
            Self::NotAnUpgrade => "not_an_upgrade",
            Self::WrongTrack => "wrong_track",
            Self::Stalled => "stalled",
            Self::QueueTimeout => "queue_timeout",
            Self::QueueGaveWay => "queue_gave_way",
            Self::NeverStarted => "never_started",
            Self::Deadline => "deadline",
            Self::SourceFailed => "source_failed",
            Self::NoSource => "no_source",
            Self::DownloadFailed => "download_failed",
            Self::AlbumLookupUnavailable => "album_lookup_unavailable",
            Self::RecordingUnknown => "recording_unknown",
            Self::NoAlbumForRecording => "no_album_for_recording",
            Self::NoAlbumFolder => "no_album_folder",
            Self::NoAlbumRelease => "no_album_release",
            Self::AlbumReleasesExhausted => "album_releases_exhausted",
            Self::TrackNotFound => "track_not_found",
            Self::NoDownloadSource => "no_download_source",
            Self::SettingsUnreadable => "settings_unreadable",
            Self::JournalUnreadable => "journal_unreadable",
            Self::MusicbrainzUnavailable => "musicbrainz_unavailable",
            Self::HeldForReview => "held_for_review",
            Self::TracksMissing => "tracks_missing",
            Self::AlreadyOwned => "already_owned",
            Self::Cancelled => "cancelled",
            Self::MovedOn => "moved_on",
        }
    }

    /// The reason a stored spelling names, when it names one exactly.
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.code() == code)
    }

    /// The reason a stored spelling stands for. A spelling this version
    /// does not write (a retired code, a v2 code carried over) reads as
    /// [`Reason::HeldByEarlierVersion`], never as a fault of ours. The
    /// spelling is logged once at start (see [`log_unknown_spellings`]).
    pub fn from_stored(code: &str) -> Self {
        Self::from_code(code).unwrap_or(Self::HeldByEarlierVersion)
    }

    /// What happened, in one plain sentence.
    pub fn message(self) -> &'static str {
        match self {
            Self::WeakMatch => {
                "The files do not match the album closely enough to import them safely."
            }
            Self::NoArtistEvidence => {
                "The files have no artist tags, and nothing shows they are by this artist."
            }
            Self::MostlyUnmatched => "Most files do not belong to any track of the album.",
            Self::TagMismatch => {
                "The files' tags name a different track or release than the one they were matched to."
            }
            Self::FingerprintMismatch => {
                "The audio sounds like a different recording than the track it is named after."
            }
            Self::FingerprintUnconfirmed => {
                "AcoustID could not confirm these files are the tracks they would replace, so your copies stay."
            }
            Self::AlbumVersionOnly => "Only the album version was found.",
            Self::NoRelease => {
                "MusicBrainz has no release of this album to check the files against."
            }
            Self::NoTracks => "No file of the download could be imported.",
            Self::VersionLowerQuality => {
                "This edition's version turned up only in lower quality than yours, so your version stays."
            }
            Self::NoBetterThanYours => {
                "Your copy is as good or better, so it stayed. The held file is still here."
            }
            Self::OwnedElsewhere => "You already have this track on another copy of this album.",
            Self::OwnedOnAlbum => "You now have this track on this album.",
            Self::NoAlbumMatch => "DroppedNeedle couldn't work out which album this is.",
            Self::AlbumNotFound => {
                "MusicBrainz has no release of the album you picked that these files can be matched to."
            }
            Self::TagsDisagree => {
                "These files are tagged with an album they don't match closely enough to import on their own."
            }
            Self::NotChosenEdition => {
                "These files don't match the edition your library uses for this album."
            }
            Self::Samples => "These look like sample clips or previews, not full tracks.",
            Self::TooManyFiles => {
                "The folder holds more files than one album can, so nothing was imported."
            }
            Self::Interrupted => {
                "The server restarted before this import finished. The files were kept."
            }
            Self::ImportFailed => "Something went wrong importing this folder.",
            Self::HeldByEarlierVersion => "An earlier version of DroppedNeedle held these files.",
            Self::TargetOccupied => {
                "Another file already sits where this would go in your library."
            }
            Self::UpgradeBlocked => {
                "Your copy could not be moved to the recycle bin, so the better file was not added. Both are kept."
            }
            Self::LibraryChanged => {
                "Something in the library changed while these files were going in, so nothing was changed."
            }
            Self::LibraryUnavailable => "The library couldn't take the files right now.",
            Self::NamingScriptFailed => {
                "The import naming script gave no usable path for these files, so they wait where they are."
            }
            Self::LibraryFolderMissing => {
                "The library folder isn't there, so the files wait where they are."
            }
            Self::LocalFault => {
                "Something on this server stopped the import (for example a full disk or a folder it cannot write)."
            }
            Self::FilesMissing => {
                "The download client said files were ready, but they are not on disk."
            }
            Self::FilesUnlisted => {
                "The download client could not say where the finished files are."
            }
            Self::OutsideDownloadsFolder => {
                "The download client pointed at files outside its downloads folder, or at a link, so nothing was read."
            }
            Self::SizeMismatch => {
                "A file on disk is not the size the source advertised. It may still be downloading or be damaged."
            }
            Self::Corrupt => {
                "None of the audio files could be read; they are damaged or not really audio."
            }
            Self::NoAudio => "The download contains no music files.",
            Self::Sample => "The files are short samples or previews, not the full tracks.",
            Self::WrongEdition => {
                "The files are a different kind of release than you asked for (a live album, box set or similar)."
            }
            Self::NotPinnedEdition => {
                "The files are tagged as a different edition than the one you picked for this album."
            }
            Self::WrongAlbum => "The files belong to a different album by the same artist.",
            Self::QualityRejected => {
                "The files are outside the quality range in your download settings."
            }
            Self::NotAnUpgrade => "The files are not better than the copy you already have.",
            Self::WrongTrack => "The downloaded track is not the one you asked for.",
            Self::Stalled => "The transfer stopped moving and timed out.",
            Self::QueueTimeout => "The peer kept the files in its upload queue for too long.",
            Self::QueueGaveWay => {
                "The best copy stayed queued at the peer, so the next one was tried."
            }
            Self::NeverStarted => {
                "The download client accepted the files but never started the transfer."
            }
            Self::Deadline => "The download took longer than the time allowed for one source.",
            Self::SourceFailed => "The source stopped sending before every file arrived.",
            Self::NoSource => "No download source had this release.",
            Self::DownloadFailed => "The download did not finish.",
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
            Self::NoDownloadSource => "Waiting: no download source configured.",
            Self::SettingsUnreadable => {
                "The download settings cannot be read, so the files wait where they are."
            }
            Self::JournalUnreadable => {
                "The download's records could not be read just now, so the files wait where they are."
            }
            Self::MusicbrainzUnavailable => "MusicBrainz could not be reached to check the files.",
            Self::HeldForReview => {
                "Some files of this download are held for you to check before they reach the library."
            }
            Self::TracksMissing => "Some tracks of the album were not found.",
            Self::AlreadyOwned => "Every track in this folder is already in your library.",
            Self::Cancelled => "This download was stopped, so it will not be retried.",
            Self::MovedOn => "You moved this download on from a slow source.",
        }
    }

    /// What the person can do about it.
    pub fn action(self) -> &'static str {
        match self {
            Self::WeakMatch => {
                "Review the held files: import them if they are right, or discard them."
            }
            Self::NoArtistEvidence => {
                "Listen to the held files and import the right ones. With an AcoustID key (Settings > Library), untagged files are checked by their sound."
            }
            Self::MostlyUnmatched => {
                "Review the held files: import the ones that are right, discard the rest."
            }
            Self::TagMismatch => {
                "Review the held files: import or match the ones that are right, discard the rest."
            }
            Self::FingerprintMismatch => {
                "Listen to it: import or match it if it is right, otherwise discard it."
            }
            Self::FingerprintUnconfirmed => {
                "Re-check them later, or listen and import them if they are right."
            }
            Self::AlbumVersionOnly => {
                "Choose Get it from the album on the request to take the album's track as this song."
            }
            Self::NoRelease => {
                "Review the held files and import them by hand, or check the album on MusicBrainz."
            }
            Self::NoTracks => "Review the held files, or let DroppedNeedle try another source.",
            Self::VersionLowerQuality => {
                "Discard it, or choose Use this version to make it your copy; yours goes to the recycle bin."
            }
            Self::NoBetterThanYours => {
                "Discard it, or choose Use this file to replace yours; yours goes to the recycle bin."
            }
            Self::OwnedElsewhere => {
                "Discard it, or choose Import to keep a second copy here; yours stays as it is."
            }
            Self::OwnedOnAlbum => {
                "Discard it, or choose Import: it replaces yours only if it is better, and yours then goes to the recycle bin."
            }
            Self::NoAlbumMatch => "Pick the album by hand with Match, or discard the folder.",
            Self::AlbumNotFound => "Check that you picked the right album, then try Match again.",
            Self::TagsDisagree => "Pick the album by hand with Match, or discard the folder.",
            Self::NotChosenEdition => {
                "Use Match to import them anyway, or change the album's edition in the library first."
            }
            Self::Samples => "Discard them, or use Match if they really are full tracks.",
            Self::TooManyFiles => {
                "Split it into one folder per album, or import the right album from it by hand."
            }
            Self::Interrupted => "Pick the album with Match, or discard the files.",
            Self::ImportFailed => "Try Match again. If it keeps failing, check the server log.",
            Self::HeldByEarlierVersion => {
                "Review them: import the ones that are right, discard the rest."
            }
            Self::TargetOccupied => {
                "Compare it with the one in your library: discard it if it is the same, otherwise move yours and try again."
            }
            Self::UpgradeBlocked => {
                "Check that the recycle bin folder exists and can be written, then try again."
            }
            Self::LibraryChanged => "Nothing to do: DroppedNeedle tries again by itself.",
            Self::LibraryUnavailable => {
                "Check that a library folder is set up and has free space, then try again."
            }
            Self::NamingScriptFailed => {
                "Fix the import naming script in Settings, Library, then try again."
            }
            Self::LibraryFolderMissing => {
                "Check the library folder in Settings, Library, and that the server can write to it. Downloads go ahead by themselves once it is back."
            }
            Self::LocalFault => "Check free space and folder permissions, then retry import.",
            Self::FilesMissing => {
                "Wait a few minutes and retry import. If it keeps happening, check that the download folder is shared with DroppedNeedle."
            }
            Self::FilesUnlisted => {
                "Check that the download client is running and reachable, then retry import."
            }
            Self::OutsideDownloadsFolder => {
                "Check the downloads folder set for this client in Settings, and that the download itself is not a link to somewhere else. A plugin must put its files in its own data folder or its downloads folder setting."
            }
            Self::SizeMismatch => {
                "Retry import once the download client shows it finished, or let DroppedNeedle try another source."
            }
            Self::Corrupt => {
                "A download tries another source when one is left. For an upload, drop in a fresh copy."
            }
            Self::NoAudio | Self::Sample | Self::WrongAlbum => {
                "Nothing to do: DroppedNeedle skips this source and tries the next one."
            }
            Self::WrongEdition => {
                "Nothing to do: DroppedNeedle skips this source. If you wanted that release, request it instead."
            }
            Self::NotPinnedEdition => {
                "Nothing to do: DroppedNeedle skips this source. To accept this edition, change the album's edition first."
            }
            Self::QualityRejected => {
                "Nothing to do: DroppedNeedle tries another source. Widen the quality range in settings to accept files like these."
            }
            Self::NotAnUpgrade => {
                "Nothing to do: your current files stay. DroppedNeedle tries another source for the upgrade."
            }
            Self::WrongTrack => "Review the held file, or let DroppedNeedle try another source.",
            Self::Stalled | Self::QueueTimeout | Self::Deadline | Self::SourceFailed => {
                "Nothing to do: DroppedNeedle retries later, or retry it now."
            }
            Self::DownloadFailed => "Nothing to do: DroppedNeedle retries later, or retry it now.",
            Self::NeverStarted => "Check that the download client is running, then retry.",
            Self::NoSource => {
                "DroppedNeedle retries later. You can also search by hand or pick another edition."
            }
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
            Self::NoDownloadSource => {
                "Turn on slskd, SABnzbd or a plugin source in Settings, Downloads. It starts by itself."
            }
            Self::SettingsUnreadable => {
                "Save Settings, Downloads again. The import then goes ahead by itself."
            }
            Self::JournalUnreadable => "Nothing to do: DroppedNeedle tries again by itself.",
            Self::MusicbrainzUnavailable => {
                "Downloads try again by themselves. For an upload, try Match again in a few minutes."
            }
            Self::HeldForReview => "Open the held list and import or discard them.",
            Self::TracksMissing => {
                "DroppedNeedle looks for the missing tracks again later. You can also search for them yourself."
            }
            Self::AlreadyOwned => "Nothing to do.",
            Self::Cancelled => "Retry it to start again.",
            Self::MovedOn => "Nothing to do: it starts on the next source shortly.",
            Self::QueueGaveWay => "Nothing to do: the next copy is downloading.",
        }
    }

    /// Where the reason shows and what it offers.
    pub fn kind(self) -> ReasonKind {
        match self {
            Self::WeakMatch
            | Self::NoArtistEvidence
            | Self::MostlyUnmatched
            | Self::TagMismatch
            | Self::FingerprintMismatch
            | Self::FingerprintUnconfirmed
            | Self::AlbumVersionOnly
            | Self::NoRelease
            | Self::NoTracks
            | Self::VersionLowerQuality
            | Self::NoBetterThanYours
            | Self::OwnedElsewhere
            | Self::OwnedOnAlbum
            | Self::NoAlbumMatch
            | Self::AlbumNotFound
            | Self::TagsDisagree
            | Self::NotChosenEdition
            | Self::Samples
            | Self::TooManyFiles
            | Self::Interrupted
            | Self::ImportFailed
            | Self::HeldByEarlierVersion => ReasonKind::Hold,
            Self::TargetOccupied
            | Self::UpgradeBlocked
            | Self::LibraryChanged
            | Self::LibraryUnavailable
            | Self::NamingScriptFailed
            | Self::LibraryFolderMissing
            | Self::LocalFault => ReasonKind::Library,
            Self::FilesMissing
            | Self::FilesUnlisted
            | Self::OutsideDownloadsFolder
            | Self::SizeMismatch
            | Self::Corrupt
            | Self::NoAudio
            | Self::Sample
            | Self::WrongEdition
            | Self::NotPinnedEdition
            | Self::WrongAlbum
            | Self::QualityRejected
            | Self::NotAnUpgrade
            | Self::WrongTrack
            | Self::Stalled
            | Self::QueueTimeout
            | Self::QueueGaveWay
            | Self::NeverStarted
            | Self::Deadline
            | Self::SourceFailed
            | Self::NoSource
            | Self::DownloadFailed
            | Self::AlbumLookupUnavailable
            | Self::RecordingUnknown
            | Self::NoAlbumForRecording
            | Self::NoAlbumFolder
            | Self::NoAlbumRelease
            | Self::AlbumReleasesExhausted
            | Self::TrackNotFound => ReasonKind::Source,
            Self::NoDownloadSource
            | Self::SettingsUnreadable
            | Self::JournalUnreadable
            | Self::MusicbrainzUnavailable
            | Self::HeldForReview
            | Self::TracksMissing => ReasonKind::Wait,
            Self::AlreadyOwned | Self::Cancelled | Self::MovedOn => ReasonKind::Settled,
        }
    }

    /// Whether the reason counts against the source that delivered the
    /// files: the release is blocklisted and the track asked of another
    /// source. False when the files were fine and our side could not take
    /// them, when the work only waits, and for whatever no source
    /// delivered.
    pub fn against_source(self) -> bool {
        match self {
            Self::WeakMatch
            | Self::NoArtistEvidence
            | Self::MostlyUnmatched
            | Self::TagMismatch
            | Self::FingerprintMismatch
            | Self::AlbumVersionOnly
            | Self::NoRelease
            | Self::NoTracks
            | Self::Corrupt
            | Self::NoAudio
            | Self::Sample
            | Self::WrongEdition
            | Self::NotPinnedEdition
            | Self::WrongAlbum
            | Self::QualityRejected
            | Self::NotAnUpgrade
            | Self::WrongTrack
            | Self::Stalled
            | Self::QueueTimeout
            | Self::NeverStarted
            | Self::Deadline
            | Self::SourceFailed
            | Self::DownloadFailed
            | Self::TracksMissing => true,
            Self::FingerprintUnconfirmed
            | Self::VersionLowerQuality
            | Self::NoBetterThanYours
            | Self::OwnedElsewhere
            | Self::OwnedOnAlbum
            | Self::NoAlbumMatch
            | Self::AlbumNotFound
            | Self::TagsDisagree
            | Self::NotChosenEdition
            | Self::Samples
            | Self::TooManyFiles
            | Self::Interrupted
            | Self::ImportFailed
            | Self::HeldByEarlierVersion
            | Self::TargetOccupied
            | Self::UpgradeBlocked
            | Self::LibraryChanged
            | Self::LibraryUnavailable
            | Self::NamingScriptFailed
            | Self::LibraryFolderMissing
            | Self::LocalFault
            | Self::FilesMissing
            | Self::FilesUnlisted
            | Self::OutsideDownloadsFolder
            | Self::SizeMismatch
            | Self::QueueGaveWay
            | Self::NoSource
            | Self::AlbumLookupUnavailable
            | Self::RecordingUnknown
            | Self::NoAlbumForRecording
            | Self::NoAlbumFolder
            | Self::NoAlbumRelease
            | Self::AlbumReleasesExhausted
            | Self::TrackNotFound
            | Self::NoDownloadSource
            | Self::SettingsUnreadable
            | Self::JournalUnreadable
            | Self::MusicbrainzUnavailable
            | Self::HeldForReview
            | Self::AlreadyOwned
            | Self::Cancelled
            | Self::MovedOn => false,
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// What is wrong with the library folder a download waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderProblem {
    Missing,
    NotWritable,
    NotAnswering,
}

/// The sentence of a download that waits for its library folder.
pub fn library_folder_wait(folder: Option<&std::path::Path>, problem: FolderProblem) -> String {
    let problem = match problem {
        FolderProblem::Missing => "isn't there",
        FolderProblem::NotWritable => "can't be written to",
        FolderProblem::NotAnswering => "isn't answering",
    };
    match folder {
        Some(folder) => format!("Waiting: library folder {} {problem}.", folder.display()),
        None => format!("Waiting: the library folder {problem}."),
    }
}

/// Log each stored spelling this version does not know, once per start.
/// Readers take such a spelling as [`Reason::HeldByEarlierVersion`]
/// without a word, so this is the one place it is logged.
pub fn log_unknown_spellings(spellings: &[String]) {
    for code in spellings {
        if Reason::from_code(code).is_none() {
            tracing::warn!(
                code = %code,
                "stored reason this version does not write; it reads as held by an earlier version"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Reason, ReasonKind};
    use crate::landing::specs::Disposition;

    #[test]
    fn every_reason_has_words_a_spelling_and_survives_storage() {
        let mut codes: Vec<&str> = Reason::ALL.iter().map(|reason| reason.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), Reason::ALL.len(), "duplicate spellings");
        for reason in Reason::ALL {
            assert!(!reason.message().is_empty(), "{reason}");
            assert!(!reason.action().is_empty(), "{reason}");
            for text in [reason.message(), reason.action()] {
                assert!(!text.contains('\u{2014}'), "{reason}");
            }
            let json = serde_json::to_string(&reason).unwrap_or_default();
            assert_eq!(json, format!("\"{}\"", reason.code()));
            assert_eq!(Reason::from_code(reason.code()), Some(reason));
        }
        for retired in [
            "upgrade_pending",
            "upgrade_restore_failed",
            "management:x",
            "",
        ] {
            assert_eq!(Reason::from_stored(retired), Reason::HeldByEarlierVersion);
        }
        assert_eq!(Reason::LocalFault.kind(), ReasonKind::Library);
        assert_eq!(Reason::AlbumVersionOnly.kind(), ReasonKind::Hold);
    }

    /// What the code before typed reasons did with each reason. A held
    /// file counted against its source unless `is_local_hold` listed its
    /// code (`target_occupied`, `upgrade_pending`, `upgrade_blocked`,
    /// `upgrade_restore_failed`, `version_lower_quality`); a landing's
    /// rejection did when its `Disposition` was `Permanent`; a worker
    /// failover did when it blocklisted the release.
    #[derive(Debug, Clone, Copy)]
    enum Before {
        /// Held by a landing; `local` is what `is_local_hold` answered.
        Held { local: bool },
        /// Rejected by a landing with this disposition.
        Rejected(Disposition),
        /// Failed over by the worker; `blocklisted` when it quarantined.
        FailedOver { blocklisted: bool },
        /// The client could not list the landed files: the landing waited
        /// or kept the files for a retry, nothing blocklisted.
        Waited,
        /// Every source refused at enqueue: settled, nothing blocklisted.
        Unserved,
        /// Held after a person's import or match, never by a landing:
        /// nothing is said against a source.
        PersonHeld,
        /// No landing or worker path produces it; the answer chosen.
        NotProduced(bool),
    }

    impl Before {
        fn against_source(self) -> bool {
            match self {
                Before::Held { local } => !local,
                Before::Rejected(disposition) => disposition == Disposition::Permanent,
                Before::FailedOver { blocklisted } => blocklisted,
                Before::Waited | Before::Unserved | Before::PersonHeld => false,
                Before::NotProduced(answer) => answer,
            }
        }
    }

    /// `against_source` equals what each landing hold, landing rejection
    /// and worker outcome did before reasons were typed, and every reason
    /// is listed at least once. Today's behaviour is kept where the two
    /// sources disagreed: none do.
    #[test]
    fn against_source_keeps_what_each_outcome_did_before() {
        use Before::{FailedOver, Held, NotProduced, PersonHeld, Rejected, Unserved, Waited};
        use Disposition::{LocalFault, Permanent, Temporary};
        let table: &[(Reason, Before)] = &[
            (Reason::WeakMatch, Held { local: false }),
            (Reason::NoArtistEvidence, Held { local: false }),
            (Reason::MostlyUnmatched, Held { local: false }),
            (Reason::TagMismatch, Held { local: false }),
            (Reason::FingerprintMismatch, Held { local: false }),
            (Reason::AlbumVersionOnly, Held { local: false }),
            (Reason::NoRelease, Held { local: false }),
            (Reason::NoTracks, Held { local: false }),
            (Reason::WrongTrack, Held { local: false }),
            (Reason::VersionLowerQuality, Held { local: true }),
            (Reason::TargetOccupied, Held { local: true }),
            (Reason::UpgradeBlocked, Held { local: true }),
            // `upgrade_pending` and `upgrade_restore_failed` read as this.
            (Reason::HeldByEarlierVersion, Held { local: true }),
            (Reason::FilesMissing, Rejected(Temporary)),
            (Reason::FilesMissing, Rejected(LocalFault)),
            (Reason::SizeMismatch, Rejected(LocalFault)),
            (Reason::TooManyFiles, Rejected(LocalFault)),
            (Reason::Corrupt, Rejected(Permanent)),
            (Reason::NoAudio, Rejected(Permanent)),
            (Reason::Sample, Rejected(Permanent)),
            (Reason::WrongEdition, Rejected(Permanent)),
            (Reason::NotPinnedEdition, Rejected(Permanent)),
            (Reason::WrongAlbum, Rejected(Permanent)),
            (Reason::QualityRejected, Rejected(Permanent)),
            (Reason::NotAnUpgrade, Rejected(Permanent)),
            (Reason::WrongTrack, Rejected(Permanent)),
            (Reason::MusicbrainzUnavailable, Rejected(Temporary)),
            (Reason::JournalUnreadable, Rejected(Temporary)),
            (Reason::SettingsUnreadable, Rejected(Temporary)),
            (Reason::LibraryChanged, Rejected(Temporary)),
            (Reason::NamingScriptFailed, Rejected(LocalFault)),
            (Reason::LibraryFolderMissing, Rejected(LocalFault)),
            (Reason::LocalFault, Rejected(LocalFault)),
            (Reason::OutsideDownloadsFolder, Rejected(LocalFault)),
            (Reason::Stalled, FailedOver { blocklisted: true }),
            (Reason::QueueTimeout, FailedOver { blocklisted: true }),
            (Reason::NeverStarted, FailedOver { blocklisted: true }),
            (Reason::Deadline, FailedOver { blocklisted: true }),
            (Reason::SourceFailed, FailedOver { blocklisted: true }),
            (Reason::DownloadFailed, FailedOver { blocklisted: true }),
            // A landing short of tracks fails over with the release
            // blocklisted.
            (Reason::TracksMissing, FailedOver { blocklisted: true }),
            (Reason::MovedOn, FailedOver { blocklisted: false }),
            (Reason::QueueGaveWay, FailedOver { blocklisted: false }),
            (Reason::FilesUnlisted, Waited),
            (Reason::NoSource, Unserved),
            (Reason::AlbumLookupUnavailable, Unserved),
            (Reason::RecordingUnknown, Unserved),
            (Reason::NoAlbumForRecording, Unserved),
            (Reason::NoAlbumFolder, Unserved),
            (Reason::NoAlbumRelease, Unserved),
            (Reason::AlbumReleasesExhausted, Unserved),
            (Reason::TrackNotFound, Unserved),
            // Uploads have no source; the queue's own states and the
            // reasons no code produces yet say nothing against one.
            (Reason::FingerprintUnconfirmed, PersonHeld),
            (Reason::NoBetterThanYours, PersonHeld),
            (Reason::OwnedElsewhere, PersonHeld),
            (Reason::OwnedOnAlbum, PersonHeld),
            (Reason::NoAlbumMatch, NotProduced(false)),
            (Reason::AlbumNotFound, NotProduced(false)),
            (Reason::TagsDisagree, NotProduced(false)),
            (Reason::NotChosenEdition, NotProduced(false)),
            (Reason::Samples, NotProduced(false)),
            (Reason::Interrupted, NotProduced(false)),
            (Reason::ImportFailed, NotProduced(false)),
            (Reason::LibraryUnavailable, NotProduced(false)),
            (Reason::NoDownloadSource, NotProduced(false)),
            (Reason::HeldForReview, NotProduced(false)),
            (Reason::AlreadyOwned, NotProduced(false)),
            (Reason::Cancelled, NotProduced(false)),
        ];
        for (reason, before) in table {
            assert_eq!(
                reason.against_source(),
                before.against_source(),
                "{reason} ({before:?})"
            );
        }
        for reason in Reason::ALL {
            assert!(
                table.iter().any(|(listed, _)| *listed == reason),
                "{reason} is not in the table"
            );
        }
    }
}
