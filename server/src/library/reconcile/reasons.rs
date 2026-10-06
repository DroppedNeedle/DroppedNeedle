//! Why a duplicate group needs a look, or why a request was refused: one
//! stable code, one plain sentence, and what to do about it.

pub use crate::library::operations::reasons::Reason;

const fn reason(code: &'static str, message: &'static str, action: &'static str) -> Reason {
    Reason {
        code,
        message,
        action,
    }
}

// Why a group is listed. The codes match v2's.
pub const CONFLICTING_PROVIDER_IDENTITIES: Reason = reason(
    "CONFLICTING_PROVIDER_IDENTITIES",
    "These records share a name but point at different MusicBrainz artists.",
    "If they really are different people, mark them as distinct. Otherwise fix the wrong album's match.",
);
pub const AMBIGUOUS_CREDIT_STRUCTURE: Reason = reason(
    "AMBIGUOUS_CREDIT_STRUCTURE",
    "An album by one of these records has an artist credit that could not be split cleanly.",
    "Check the album's artist credit, or mark the records as distinct if they are different people.",
);
pub const INCOMPLETE_PROVIDER_PROOF: Reason = reason(
    "INCOMPLETE_PROVIDER_PROOF",
    "Only some of these records are matched to MusicBrainz yet, so they cannot be merged safely.",
    "Identify the unmatched albums; the records merge once every one points at the same artist.",
);
pub const NAME_MATCH_WITHOUT_PROVIDER_PROOF: Reason = reason(
    "NAME_MATCH_WITHOUT_PROVIDER_PROOF",
    "These records only share a name; none of them is matched to MusicBrainz.",
    "Identify their albums, or mark the records as distinct if they are different people.",
);
pub const RESOLVED_AUTOMATICALLY: Reason = reason(
    "AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE",
    "These records were merged automatically because MusicBrainz proved they are one artist.",
    "Nothing to do.",
);

// Refusals.
pub const GROUP_NOT_FOUND: Reason = reason(
    "ARTIST_GROUP_NOT_FOUND",
    "This duplicate artist group no longer exists.",
    "Reload the list; the records may have been merged or marked distinct already.",
);
pub const GROUP_RESOLVED: Reason = reason(
    "ARTIST_GROUP_RESOLVED",
    "A group that was merged automatically cannot be marked distinct.",
    "Nothing to do.",
);
pub const GROUP_STALE: Reason = reason(
    "STALE_REVISION",
    "The artist records in this group changed since you opened it.",
    "Reload the group and check it again.",
);
pub const CURSOR_INVALID: Reason = reason(
    "ARTIST_GROUP_CURSOR_INVALID",
    "The page cursor is not valid.",
    "Reload the list from the start.",
);
pub const LIMIT_INVALID: Reason = reason(
    "ARTIST_GROUP_LIMIT_INVALID",
    "The page size must be between 1 and 100.",
    "Ask for a smaller page.",
);

/// The listed reason for a stored code. Unknown codes from an automatic
/// merge read as the generic automatic merge.
pub fn for_code(code: &str) -> Reason {
    match code {
        "CONFLICTING_PROVIDER_IDENTITIES" => CONFLICTING_PROVIDER_IDENTITIES,
        "AMBIGUOUS_CREDIT_STRUCTURE" => AMBIGUOUS_CREDIT_STRUCTURE,
        "INCOMPLETE_PROVIDER_PROOF" => INCOMPLETE_PROVIDER_PROOF,
        "NAME_MATCH_WITHOUT_PROVIDER_PROOF" => NAME_MATCH_WITHOUT_PROVIDER_PROOF,
        _ => RESOLVED_AUTOMATICALLY,
    }
}
