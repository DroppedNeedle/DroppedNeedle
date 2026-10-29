//! GitHub REST wire shapes, read by the clients in `github.rs`.
//!
//! Tolerant of unknown fields. A release's `tag_name`, a repository's `id`
//! and a commit's `sha` are their identities: an answer without one fails
//! decoding, and so does the list it is in. The other fields are optional.
//! A missing flag is read as the less trusting answer: a release whose
//! draft state is unknown is hidden, and one whose prerelease state is
//! unknown is never offered as the latest release.

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

/// `GET /repos/{owner}/{repo}`, of which only the id is read.
///
/// Probed 2026-10-27 against the public API: `id` is a JSON integer, kept
/// through a rename and new for a repository created again under the same
/// name. An unknown repository answers 404.
#[derive(Debug, Deserialize)]
pub struct Repository {
    /// GitHub's id for the repository.
    pub id: u64,
}

/// `GET /repos/{owner}/{repo}/commits/{ref}`, of which only the SHA is
/// read.
///
/// Probed 2026-10-13 against the public API: a tag, branch or commit the
/// repository lacks answers 422, not 404; GitHub's "Get a commit" page
/// gives 409 for a repository with no commits.
#[derive(Debug, Deserialize)]
pub struct Commit {
    /// The full commit SHA.
    pub sha: String,
}
