//! Search review: what the automatic downloader does with a search's
//! candidates before it grabs one.
//!
//! The worker searches every source for
//! the album, ranks the results the way a manual search does, and then:
//! takes the first recommended candidate in source order (the album looks
//! complete and in an accepted quality), or, when nothing is recommended,
//! parks the download on that search and waits for its owner to pick one
//! or turn them all down. The setting can also park every album, or turn
//! the look off so the worker grabs its best guess as before.

use super::candidates::{Candidate, CandidateTier};

/// `download_policy.search_review`: wait for a pick on every album.
pub const ALWAYS: &str = "always";
/// `download_policy.search_review`: never wait, grab the best guess.
pub const OFF: &str = "off";

/// When the automatic downloader waits for a person to pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewMode {
    /// Never: each source grabs its own best guess.
    Off,
    /// Only when no candidate is recommended.
    #[default]
    WhenUnsure,
    /// Always.
    Always,
}

impl ReviewMode {
    /// The mode for a stored setting; anything unknown reads as the
    /// default.
    pub fn from_setting(value: &str) -> Self {
        match value {
            OFF => Self::Off,
            ALWAYS => Self::Always,
            _ => Self::WhenUnsure,
        }
    }
}

/// What to do with one search's candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Download the candidate at this index.
    Take(usize),
    /// Wait for a person to pick.
    Park,
    /// Nothing was found; the normal search reports why.
    Nothing,
}

/// Judge the candidates, listed source by source in try order with each
/// source's best first. The first recommended one wins, so an earlier
/// source's good copy beats a later source's.
/// Candidates outside the download settings count for nothing: when only
/// those were found, the download searches as usual rather than waiting
/// for a person.
pub fn verdict(mode: ReviewMode, candidates: &[Candidate]) -> Verdict {
    if candidates.iter().all(Candidate::is_refused) {
        return Verdict::Nothing;
    }
    if mode == ReviewMode::WhenUnsure
        && let Some(index) = candidates
            .iter()
            .position(|candidate| candidate.view.tier == CandidateTier::Recommended)
    {
        return Verdict::Take(index);
    }
    Verdict::Park
}
