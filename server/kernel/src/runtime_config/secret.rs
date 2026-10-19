//! Redacted secret holder.
//!
//! `Secret` serializes as its inner string (both the ciphertext store shape
//! and the masked API shape need the plain JSON string) but its `Debug`
//! output never contains the value. There is no `Display`
//! impl: writing a secret to a log takes an explicit `expose()` call, which
//! is easy to grep for in review.
//!
//! A value the store decrypted refuses to serialize. The section around it
//! is a [`Plain`](super::plain::Plain), which the compiler already keeps
//! out of every serializer; this catches a reference taken through it.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A secret value that refuses to print itself.
#[derive(Clone, Default)]
pub struct Secret {
    value: String,
    /// Set when the store decrypted this value: it then never serializes.
    opened: bool,
}

impl Secret {
    /// Wrap a ciphertext, a mask, or a value submitted to be saved.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            opened: false,
        }
    }

    /// Borrow the inner value. The only way the plaintext leaves this type.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// Mutably borrow the inner value. A decrypted value stays one.
    pub fn expose_mut(&mut self) -> &mut String {
        &mut self.value
    }

    /// Whether no secret is set. Empty is absence, never failure.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Hold a decrypted value, which refuses to serialize from now on.
    pub(crate) fn open(&mut self, plaintext: String) {
        self.value = plaintext;
        self.opened = true;
    }

    /// Hold a ciphertext or a mask, which serializes.
    pub(crate) fn close(&mut self, value: String) {
        self.value = value;
        self.opened = false;
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Eq for Secret {}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.opened {
            return Err(serde::ser::Error::custom(
                "a decrypted secret cannot be serialized",
            ));
        }
        serializer.serialize_str(&self.value)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::new(value)
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
