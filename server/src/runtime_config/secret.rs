//! Redacted secret holder.
//!
//! `Secret` serializes as its inner string (both the ciphertext store shape
//! and the masked API shape need the plain JSON string) but its `Debug`
//! output never contains the value. There is deliberately no `Display`
//! impl: writing a secret to a log takes an explicit `expose()` call, which
//! is easy to grep for in review.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A secret value that refuses to print itself.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a plaintext, ciphertext, or mask-sentinel string.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the inner value. The only way the plaintext leaves this type.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Mutably borrow the inner value, for decrypt/mask in place.
    pub fn expose_mut(&mut self) -> &mut String {
        &mut self.0
    }

    /// Whether no secret is set. Empty is absence, never failure.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_value() {
        let secret = Secret::new("hunter2-live-key");
        let shown = format!("{secret:?}");
        assert!(!shown.contains("hunter2-live-key"));
        assert!(shown.contains("[redacted]"));
    }

    #[test]
    fn serde_round_trip_preserves_the_string() {
        let secret = Secret::new("v3:opaque-ciphertext");
        let json = serde_json::to_string(&secret).unwrap();
        assert_eq!(json, "\"v3:opaque-ciphertext\"");
        let back: Secret = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expose(), "v3:opaque-ciphertext");
    }
}
