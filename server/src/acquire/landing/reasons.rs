//! What each landing reason means to a person, and what to do about it.
//!
//! Every check verdict, landing outcome and held file carries a stable
//! code (`size_mismatch`, `weak_match`, ...). This catalog turns a code
//! into one plain sentence and one suggested action, stored next to the
//! code on the decision and held rows so the download and held views show
//! words instead of internal error text. The free-form `detail` stays as
//! the specifics (which file, which marker), never as the explanation.

/// A reason as the user reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reason {
    pub code: &'static str,
    /// What happened, in one plain sentence.
    pub message: &'static str,
    /// What the user can do about it.
    pub action: &'static str,
}

const fn reason(code: &'static str, message: &'static str, action: &'static str) -> Reason {
    Reason {
        code,
        message,
        action,
    }
}

/// Every code a landing can record.
pub const CATALOG: &[Reason] = &[
    reason(
        "files_missing",
        "The download client said files were ready, but they are not on disk.",
        "Wait a few minutes and retry import. If it keeps happening, check that the download folder is shared with DroppedNeedle.",
    ),
    reason(
        "files_unlisted",
        "The download client could not say where the finished files are.",
        "Check that the download client is running and reachable, then retry import.",
    ),
    reason(
        "size_mismatch",
        "A file on disk is not the size the source advertised. It may still be downloading or be damaged.",
        "Retry import once the download client shows it finished, or let DroppedNeedle try another source.",
    ),
    reason(
        "too_many_files",
        "The download holds far more files than one album can, so nothing was imported or copied.",
        "Look at the download folder yourself and import the right album from it by hand.",
    ),
    reason(
        "corrupt",
        "None of the audio files could be read; they are damaged or not really audio.",
        "Nothing to do: DroppedNeedle skips this source and tries the next one.",
    ),
    reason(
        "no_audio",
        "The download contains no music files.",
        "Nothing to do: DroppedNeedle skips this source and tries the next one.",
    ),
    reason(
        "sample",
        "The files are short samples or previews, not the full tracks.",
        "Nothing to do: DroppedNeedle skips this source and tries the next one.",
    ),
    reason(
        "wrong_edition",
        "The files are a different kind of release than you asked for (a live album, box set or similar).",
        "Nothing to do: DroppedNeedle skips this source. If you wanted that release, request it instead.",
    ),
    reason(
        "not_pinned_edition",
        "The files are tagged as a different edition than the one you picked for this album.",
        "Nothing to do: DroppedNeedle skips this source. To accept this edition, change the album's edition first.",
    ),
    reason(
        "wrong_album",
        "The files belong to a different album by the same artist.",
        "Nothing to do: DroppedNeedle skips this source and tries the next one.",
    ),
    reason(
        "quality_rejected",
        "The files are outside the quality range in your download settings.",
        "Nothing to do: DroppedNeedle tries another source. Widen the quality range in settings to accept files like these.",
    ),
    reason(
        "not_an_upgrade",
        "The files are not better than the copy you already have.",
        "Nothing to do: your current files stay. DroppedNeedle tries another source for the upgrade.",
    ),
    reason(
        "no_release",
        "MusicBrainz has no release of this album to check the files against.",
        "Review the held files and import them by hand, or check the album on MusicBrainz.",
    ),
    reason(
        "musicbrainz_unavailable",
        "MusicBrainz could not be reached to check the files.",
        "Nothing to do: DroppedNeedle tries again on its next pass.",
    ),
    reason(
        "tag_mismatch",
        "The files' tags name a different track or release than the one they would replace.",
        "Review the held files: import the ones that are right, discard the rest.",
    ),
    reason(
        "weak_match",
        "The files do not match the album closely enough to import them safely.",
        "Review the held files: import them if they are right, or discard them.",
    ),
    reason(
        "mostly_unmatched",
        "Most files do not belong to any track of the album.",
        "Review the held files: import the ones that are right, discard the rest.",
    ),
    reason(
        "wrong_track",
        "The downloaded track is not the one you asked for.",
        "Review the held file, or let DroppedNeedle try another source.",
    ),
    reason(
        "fingerprint_mismatch",
        "The audio itself sounds like a different recording than the track it is named after.",
        "Listen to the held file: import it if it is right, otherwise discard it.",
    ),
    reason(
        "target_occupied",
        "Another file already sits where this track would go in your library.",
        "Review the held file against the one in your library and keep the better one.",
    ),
    reason(
        "upgrade_pending",
        "These are better files for tracks you already have; they wait so your current files are not replaced unseen.",
        "Review the held files and import them to replace your current copies.",
    ),
    reason(
        "no_tracks",
        "No file of the download could be imported.",
        "Review the held files, or let DroppedNeedle try another source.",
    ),
    reason(
        "tracks_missing",
        "Some tracks of the album were not in this download.",
        "Nothing to do: DroppedNeedle asks other sources for the missing tracks. You can also search for them yourself.",
    ),
    reason(
        "local_fault",
        "Something on this server stopped the import (for example a full disk or a folder it cannot write).",
        "Check free space and folder permissions, then retry import.",
    ),
];

/// The reason for `code`. A code missing from the catalog (a bug) reads
/// as a server-side problem rather than an empty explanation.
pub fn explain(code: &str) -> &'static Reason {
    CATALOG
        .iter()
        .find(|reason| reason.code == code)
        .unwrap_or_else(|| {
            tracing::warn!(code, "landing reason missing from the catalog");
            CATALOG
                .iter()
                .find(|reason| reason.code == "local_fault")
                .unwrap_or(&CATALOG[0])
        })
}

#[cfg(test)]
mod tests {
    use super::{CATALOG, explain};

    #[test]
    fn every_reason_has_words_and_an_action() {
        for reason in CATALOG {
            assert!(!reason.message.is_empty(), "{}", reason.code);
            assert!(!reason.action.is_empty(), "{}", reason.code);
            assert!(!reason.message.contains('\u{2014}'), "{}", reason.code);
            assert!(!reason.action.contains('\u{2014}'), "{}", reason.code);
        }
        let mut codes: Vec<_> = CATALOG.iter().map(|reason| reason.code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), CATALOG.len(), "duplicate codes");
        assert_eq!(explain("size_mismatch").code, "size_mismatch");
        assert_eq!(explain("not_a_code").code, "local_fault");
    }
}
