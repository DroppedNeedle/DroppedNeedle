//! Seal tests: each secret seals under a passphrase-derived key with a
//! fresh nonce, and only the same passphrase opens the envelope again.

use droppedneedle_tool::export::{SealError, Sealer, secret_envelope_params, unseal};

/// Seals open again under the same passphrase; every seal takes a fresh
/// nonce and every envelope a fresh salt.
#[test]
fn seals_round_trip_with_a_fresh_nonce_and_salt() {
    let sealer = Sealer::generate("passphrase").unwrap();
    let envelope = sealer.secret_envelope();
    for plaintext in ["slskd-api-key", "", "unicode: héllo ✓"] {
        let blob = sealer.seal(plaintext).unwrap();
        assert_eq!(
            unseal("passphrase", &envelope, &blob).unwrap(),
            plaintext,
            "round trip of {plaintext:?}"
        );
    }

    let first = sealer.seal("same secret").unwrap();
    let second = sealer.seal("same secret").unwrap();
    assert_ne!(first, second, "fresh nonce per seal");
    assert_eq!(
        unseal("passphrase", &envelope, &first).unwrap(),
        "same secret"
    );
    assert_eq!(
        unseal("passphrase", &envelope, &second).unwrap(),
        "same secret"
    );

    let other = Sealer::generate("passphrase").unwrap().secret_envelope();
    assert_ne!(envelope.kdf.salt_b64, other.kdf.salt_b64, "fresh salt");
    assert_ne!(envelope.nonce_b64, other.nonce_b64, "fresh envelope nonce");
}

/// A wrong passphrase, a changed blob or a changed envelope never opens.
#[test]
fn unseal_fails_closed_on_any_mismatch() {
    let sealer = Sealer::generate("passphrase").unwrap();
    let envelope = sealer.secret_envelope();
    let blob = sealer.seal("secret").unwrap();

    let mut tampered = blob.clone();
    let first = tampered.remove(0);
    tampered.insert(0, if first == 'A' { 'B' } else { 'A' });

    // Another salt of the same length: the first base64 character carries
    // six whole bits of the first byte.
    let mut swapped_salt = envelope.clone();
    let first = swapped_salt.kdf.salt_b64.remove(0);
    swapped_salt
        .kdf
        .salt_b64
        .insert(0, if first == 'A' { 'B' } else { 'A' });

    let mut unknown_scheme = envelope.clone();
    unknown_scheme.scheme = "rot13".to_owned();

    let cases = [
        (
            "wrong passphrase",
            "wrong",
            &envelope,
            blob.as_str(),
            SealError::AuthFailed,
        ),
        (
            "tampered blob",
            "passphrase",
            &envelope,
            tampered.as_str(),
            SealError::AuthFailed,
        ),
        (
            "swapped envelope salt",
            "passphrase",
            &swapped_salt,
            blob.as_str(),
            SealError::AuthFailed,
        ),
        (
            "undecodable blob",
            "passphrase",
            &envelope,
            "!!!not-base64!!!",
            SealError::InvalidBlob,
        ),
        (
            "unknown scheme",
            "passphrase",
            &unknown_scheme,
            blob.as_str(),
            SealError::InvalidEnvelope,
        ),
    ];
    for (label, passphrase, envelope, blob, expected) in cases {
        assert_eq!(
            unseal(passphrase, envelope, blob),
            Err(expected),
            "case: {label}"
        );
    }
    assert_eq!(SealError::AuthFailed.code(), "ENVELOPE_AUTH_FAILED");
    assert_eq!(SealError::InvalidBlob.code(), "INVALID_SEALED_BLOB");
}

#[test]
fn envelope_params_match_the_spec_pin() {
    let params = secret_envelope_params();
    assert_eq!(params.scheme, "argon2id+xchacha20poly1305");
    assert_eq!(params.kdf.algo, "argon2id");
    assert_eq!(params.kdf.m_cost_kib, 65536);
    assert_eq!(params.kdf.t_cost, 3);
    assert_eq!(params.kdf.p_cost, 1);
}
