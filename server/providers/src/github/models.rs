//! GitHub REST wire shapes, read by the releases client in `github.rs`.
//!
//! Tolerant of unknown fields. A release's `tag_name` is its identity: a
//! release without one fails decoding, and so does the list it is in. The
//! other fields are optional. A missing flag is read as the less trusting
//! answer: a release whose draft state is unknown is hidden, and one whose
//! prerelease state is unknown is never offered as the latest release.

use serde::Deserialize;

/// The releases of one repository, newest first.
///
/// Verified against api.github.com/repos/DroppedNeedle/DroppedNeedle/releases on 2026-10-20: tests/fixtures/github/releases_list.json.
/// A repository that does not exist answers 404 on the same path with no list, recorded on 2026-10-20: tests/fixtures/github/releases_not_found.json.
pub type ReleaseList = Vec<Release>;

/// One release in the list.
#[derive(Debug, Deserialize)]
pub struct Release {
    /// The release tag, such as "v2.4.1".
    pub tag_name: String,
    /// The display name. `null` or absent falls back to the tag.
    pub name: Option<String>,
    /// The release notes. `null` or absent means no notes.
    pub body: Option<String>,
    /// The publish time, RFC 3339.
    pub published_at: Option<String>,
    /// The release page.
    pub html_url: Option<String>,
    /// Whether this is a prerelease. Absent reads as a prerelease.
    pub prerelease: Option<bool>,
    /// Whether this is still a draft. Absent reads as a draft.
    pub draft: Option<bool>,
}
