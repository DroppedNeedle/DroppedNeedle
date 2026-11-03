//! The one-time code a server logs for someone at the host to sign in
//! with when no account can: upgrade mode's sign-in, and first-admin setup
//! on a server started fresh from it.
//!
//! A code is 160 random bits in Crockford's base32, logged in groups of
//! four. It is held in memory only: it cannot be serialized or displayed,
//! and its `Debug` shows nothing of it.

use super::tokens::{TokenError, secret_eq};

/// Random bytes in a code: 160 bits, 32 characters to type.
const CODE_BYTES: usize = 20;

/// The alphabet: Crockford's base32, which leaves out I, L, O and U so a
/// code read off a log is typed back right.
const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A one-time code, as minted.
#[derive(Clone)]
pub struct OneTimeCode(String);

impl OneTimeCode {
    /// A fresh code from the system random source.
    pub fn mint() -> Result<Self, TokenError> {
        let mut bytes = [0u8; CODE_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| TokenError::RngUnavailable)?;
        let mut code = String::with_capacity(CODE_BYTES * 8 / 5);
        let (mut bits, mut held) = (0u32, 0u32);
        for byte in bytes {
            bits = (bits << 8) | u32::from(byte);
            held += 8;
            while held >= 5 {
                held -= 5;
                code.push(char::from(CODE_ALPHABET[((bits >> held) & 31) as usize]));
            }
        }
        Ok(Self(code))
    }

    /// Whether `typed` is this code, compared in constant time. Spaces and
    /// dashes are dropped, case is ignored, and the letters Crockford's
    /// base32 reads as digits count as them.
    pub fn matches(&self, typed: &str) -> bool {
        secret_eq(&normalised(typed), &self.0)
    }

    /// The code in groups of four, as logged.
    pub fn spaced(&self) -> String {
        self.0
            .as_bytes()
            .chunks(4)
            .map(|group| String::from_utf8_lossy(group).into_owned())
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl std::fmt::Debug for OneTimeCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OneTimeCode(..)")
    }
}

/// A typed code as minted.
fn normalised(typed: &str) -> String {
    typed
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .map(|ch| match ch.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect()
}
