//! What a committed change to a section sets off.
//!
//! Each section declares its effects on its type
//! ([`Section::EFFECTS`](super::Section::EFFECTS)). The server runs them
//! on the section's change notice, so every save runs them, whoever made
//! it.

/// One thing to do after a section changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Run the events sweep now.
    KickEventsSweep,
    /// Ask the download worker for a pass now, so requests held for want
    /// of a source start.
    WakeDownloads,
    /// Take the saved library roots in now, and scan the roots that are
    /// new or changed.
    RefreshLibraryRoots,
}
