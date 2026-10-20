//! Helpers the provider clients share: the base address every request is
//! built under, and the typed failures for an unusable address or answer.

use serde::de::DeserializeOwned;

use super::{
    degradation::{IntegrationStatus, record_current},
    error::{PayloadKind, ProviderError},
    executor::{Reply, Url, short},
};

/// A provider's address as a base requests are built under: an http or
/// https URL that can carry a path, or `None` (logged at build time), so
/// every call refuses instead of sending somewhere unintended.
pub(crate) fn parse_base(raw: &str, provider: &'static str) -> Option<Url> {
    let base = Url::parse(raw.trim())
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && !url.cannot_be_a_base());
    if base.is_none() {
        tracing::error!(provider, "the provider address is not an http or https URL");
    }
    base
}

/// The refusal for a call whose provider address is unusable.
pub(crate) fn unusable_base(provider: &'static str) -> ProviderError {
    ProviderError::Invalid {
        provider,
        reason: "the provider address is not usable",
    }
}

/// Decode an answer sent with a success status. A body that does not
/// decode is noted for the request; the service answered, so it never
/// counts as an outage.
pub(crate) fn decode<T: DeserializeOwned>(reply: &Reply) -> Result<T, ProviderError> {
    reply.json().inspect_err(note_payload)
}

/// An unusable answer body (too large, undecodable, or breaking the
/// provider's contract, such as a missing identity), noted for the request.
pub(crate) fn payload(provider: &'static str, kind: PayloadKind, message: &str) -> ProviderError {
    let error = ProviderError::Payload {
        provider,
        kind,
        message: short(message),
    };
    note_payload(&error);
    error
}

fn note_payload(error: &ProviderError) {
    tracing::debug!(provider = error.source(), %error, "provider answer unusable");
    record_current(error.source(), IntegrationStatus::Error, true);
}
