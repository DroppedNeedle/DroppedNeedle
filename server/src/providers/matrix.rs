//! The operation/source degradation matrix.
//!
//! "Only a dead primary source fails the request" is per-operation, not
//! global. [`role`] says what one source means to one operation; [`apply`]
//! turns a provider call's `Result` into the request's answer: a dead
//! optional source records into the request context and yields `Ok(None)`
//! (the request succeeds degraded), while a dead identity-critical or
//! required source records and yields the typed failure.
//!
//! Absence (`404`) is never a failure for any role: it records `Ok` and
//! yields `Ok(None)`, because `None`/empty means absence everywhere in v3.
//! Unknown (operation, source) pairs default to [`SourceRole::Optional`]:
//! new sources degrade openly until the table explicitly promotes them.

use super::{
    degradation::{IntegrationStatus, ProviderOutcome, record_current},
    error::ProviderError,
};

/// What the request is trying to do. The matrix reads one row per operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationKind {
    /// Turn user input (text, file tags, fingerprint) into canonical
    /// provider identities. MusicBrainz is identity-critical here.
    Identify,
    /// Resolve a known MBID to its canonical record. MusicBrainz is
    /// identity-critical here.
    Resolve,
    /// Free-text search across providers. MusicBrainz is the primary source.
    Search,
    /// Optional enrichment: genres, bios, stats, similar artists. Every
    /// source degrades.
    Enrich,
    /// Cover art. Every source degrades to placeholders and background
    /// warming.
    Artwork,
    /// Lyrics. Every source degrades.
    Lyrics,
}

/// What one source means to one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceRole {
    /// The operation's answer *is* this source's identity mapping. Only a
    /// dead identity-critical source fails identity operations.
    IdentityCritical,
    /// The operation's primary source: its death fails the request, but the
    /// operation is not about identity mapping.
    Required,
    /// Nice to have: its death records degradation and yields `None`.
    Optional,
}

/// The matrix row for one operation: which sources are critical to it.
///
/// Explicit rows only; anything unlisted is [`SourceRole::Optional`].
#[must_use]
pub const fn role(operation: OperationKind, source: &str) -> SourceRole {
    match operation {
        OperationKind::Identify | OperationKind::Resolve => {
            if is_musicbrainz(source) {
                SourceRole::IdentityCritical
            } else {
                SourceRole::Optional
            }
        }
        OperationKind::Search => {
            if is_musicbrainz(source) {
                SourceRole::Required
            } else {
                SourceRole::Optional
            }
        }
        OperationKind::Enrich | OperationKind::Artwork | OperationKind::Lyrics => {
            SourceRole::Optional
        }
    }
}

const fn is_musicbrainz(source: &str) -> bool {
    let bytes = source.as_bytes();
    let expected = b"musicbrainz";
    if bytes.len() != expected.len() {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != expected[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// Apply the matrix to one provider call: record the outcome into the
/// request context and convert the `Result` into the request's answer.
///
/// - Success records `Ok` and yields `Ok(Some(value))`.
/// - `NotFound` records `Ok` (absence is a successful answer) and yields
///   `Ok(None)` for every role.
/// - Any other failure records `Error` (flagging deterministic payload and
///   switch-off failures separately) and yields `Ok(None)` for
///   [`SourceRole::Optional`], or the typed failure for `IdentityCritical`
///   and `Required`.
pub fn apply<T>(
    operation: OperationKind,
    source: &'static str,
    result: Result<T, ProviderError>,
) -> Result<Option<T>, ProviderError> {
    match result {
        Ok(value) => {
            record_current(source, IntegrationStatus::Ok, false);
            Ok(Some(value))
        }
        Err(failure) => apply_failure(operation, failure),
    }
}

fn apply_failure<T>(
    operation: OperationKind,
    failure: ProviderError,
) -> Result<Option<T>, ProviderError> {
    let source = failure.source();
    if matches!(failure, ProviderError::NotFound { .. }) {
        record_current(source, IntegrationStatus::Ok, false);
        return Ok(None);
    }
    record_current(source, IntegrationStatus::Error, failure.is_deterministic());
    match role(operation, source) {
        SourceRole::Optional => Ok(None),
        SourceRole::IdentityCritical | SourceRole::Required => Err(failure),
    }
}

/// Convert one provider call into a typed [`ProviderOutcome`] without role
/// logic, for aggregation boundaries that combine several outcomes before
/// consulting the matrix.
///
/// Returns `None` for absence (`NotFound`): there is no outcome to
/// aggregate, because absence is not degradation and carries no data.
#[must_use]
pub fn outcome_from<T>(
    source: &'static str,
    result: Result<T, ProviderError>,
) -> Option<ProviderOutcome<T>> {
    match result {
        Ok(value) => Some(ProviderOutcome::ok(value, source)),
        Err(failure) => {
            let message = failure.to_string();
            if matches!(failure, ProviderError::NotFound { .. }) {
                None
            } else if failure.is_deterministic() {
                Some(ProviderOutcome::deterministic_error(source, message))
            } else {
                Some(ProviderOutcome::error(source, message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::degradation;

    fn transport(source: &'static str) -> ProviderError {
        ProviderError::Transport {
            provider: source,
            message: "reset".to_owned(),
        }
    }

    #[test]
    fn musicbrainz_is_critical_for_identity_required_for_search() {
        assert_eq!(
            role(OperationKind::Identify, "musicbrainz"),
            SourceRole::IdentityCritical
        );
        assert_eq!(
            role(OperationKind::Resolve, "musicbrainz"),
            SourceRole::IdentityCritical
        );
        assert_eq!(
            role(OperationKind::Search, "musicbrainz"),
            SourceRole::Required
        );
        assert_eq!(
            role(OperationKind::Identify, "acoustid"),
            SourceRole::Optional
        );
        assert_eq!(role(OperationKind::Enrich, "lastfm"), SourceRole::Optional);
        assert_eq!(
            role(OperationKind::Artwork, "coverartarchive"),
            SourceRole::Optional
        );
        assert_eq!(role(OperationKind::Lyrics, "lrclib"), SourceRole::Optional);
        assert_eq!(
            role(OperationKind::Search, "something-new"),
            SourceRole::Optional
        );
    }

    #[tokio::test]
    async fn dead_optional_source_records_and_yields_none() {
        let (answer, context) = degradation::scoped(async {
            apply::<String>(OperationKind::Enrich, "lastfm", Err(transport("lastfm")))
        })
        .await;
        assert_eq!(answer, Ok(None));
        assert_eq!(
            context.degraded_summary().get("lastfm"),
            Some(&IntegrationStatus::Error)
        );
    }

    #[tokio::test]
    async fn dead_musicbrainz_fails_identity_with_the_typed_error() {
        let failure = transport("musicbrainz");
        let (answer, context) = degradation::scoped(async {
            apply::<String>(OperationKind::Identify, "musicbrainz", Err(failure))
        })
        .await;
        assert_eq!(answer, Err(transport("musicbrainz")));
        assert!(context.has_degradation());
    }

    #[tokio::test]
    async fn absence_is_none_and_records_ok_for_every_role() {
        for operation in [
            OperationKind::Identify,
            OperationKind::Search,
            OperationKind::Enrich,
        ] {
            let (answer, context) = degradation::scoped(async {
                apply::<String>(
                    operation,
                    "musicbrainz",
                    Err(ProviderError::NotFound {
                        provider: "musicbrainz",
                    }),
                )
            })
            .await;
            assert_eq!(answer, Ok(None), "{operation:?}");
            assert!(!context.has_degradation(), "{operation:?}");
            assert_eq!(
                context.summary().get("musicbrainz"),
                Some(&IntegrationStatus::Ok),
                "{operation:?}"
            );
        }
    }
}
