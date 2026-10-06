//! Why a contribution needs review, in words a curator can act on.
//!
//! The verification worker and the store persist bare failure codes on the
//! job row. This catalog turns each one into a plain sentence plus the next
//! step. A code without an entry still reads sensibly (never "unknown
//! error"), and says what to do.

use super::models::{
    ContributionReason, FAILURE_MB_NOT_PROPAGATED, FAILURE_MB_UNAVAILABLE,
    FAILURE_RETURNED_RELEASE_MISMATCH, UNMAPPABLE_PROVIDER_PAYLOAD,
};

/// The reason shown for one persisted verification failure code.
pub fn verification_reason(code: &str) -> ContributionReason {
    let (message, action) = match code {
        FAILURE_MB_UNAVAILABLE => (
            "MusicBrainz did not answer while the release was being checked.",
            "Wait a few minutes, then choose Retry verification.",
        ),
        FAILURE_MB_NOT_PROPAGATED => (
            "MusicBrainz has not published the new release yet, even after two hours of retries.",
            "Open the release on MusicBrainz to confirm it was saved, then choose Retry verification.",
        ),
        FAILURE_RETURNED_RELEASE_MISMATCH => (
            "MusicBrainz returned a different release than the one recorded for this album.",
            "Check the release link, then record the right release as the result.",
        ),
        UNMAPPABLE_PROVIDER_PAYLOAD => (
            "MusicBrainz sent release data this version of DroppedNeedle cannot read.",
            "Try again after updating DroppedNeedle, or record the result again later.",
        ),
        "ATTACHMENT_CONTRADICTION" => (
            "The MusicBrainz release does not match this album's files closely enough to link it safely.",
            "Compare the tracklist on MusicBrainz with your files, fix whichever is wrong, then retry.",
        ),
        "VERIFIED_MBID_MISSING" | "VERIFICATION_EVIDENCE_MISSING" => (
            "The release check finished without a release to link.",
            "Choose Retry verification. If it happens again, record the release link by hand.",
        ),
        "EXISTING_IDENTITY_CONFLICT" => (
            "This album is already linked to a different MusicBrainz release.",
            "Re-identify the album first if the existing link is wrong, then retry.",
        ),
        "EXISTING_ARTIST_IDENTITY_CONFLICT" => (
            "The album's artist is already linked to a different MusicBrainz artist.",
            "Check the artist's MusicBrainz link on the artist page, then retry.",
        ),
        "LOCAL_INPUT_CHANGED" => (
            "The album's files changed while the release was being checked.",
            "Rebuild the contribution from the current files.",
        ),
        _ => (
            "The MusicBrainz release could not be linked automatically.",
            "Check the release on MusicBrainz, then choose Retry verification.",
        ),
    };
    ContributionReason {
        code: code.to_owned(),
        message: message.to_owned(),
        action: action.to_owned(),
    }
}
