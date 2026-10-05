//! Seal briefs: each secret seals under a passphrase-derived key with a
//! fresh nonce, and only the same passphrase opens the envelope again.

use droppedneedle::export::{SealError, Sealer, secret_envelope_params, unseal};

#[test]
fn seal_round_trip() {
    let sealer = Sealer::generate("correct horse").unwrap();
    let envelope = sealer.secret_envelope();
    for plaintext in ["slskd-api-key", "", "unicode: héllo ✓"] {
        let blob = sealer.seal(plaintext).unwrap();
        assert_eq!(
            unseal("correct horse", &envelope, &blob).unwrap(),
            plaintext
        );
    }
}

#[test]
fn every_seal_uses_a_fresh_nonce() {
    let sealer = Sealer::generate("passphrase").unwrap();
    let first = sealer.seal("same secret").unwrap();
    let second = sealer.seal("same secret").unwrap();
    assert_ne!(first, second);
    let envelope = sealer.secret_envelope();
    assert_eq!(
        unseal("passphrase", &envelope, &first).unwrap(),
        "same secret"
    );
    assert_eq!(
        unseal("passphrase", &envelope, &second).unwrap(),
        "same secret"
    );
}

#[test]
fn envelopes_carry_distinct_salts() {
    let first = Sealer::generate("passphrase").unwrap().secret_envelope();
    let second = Sealer::generate("passphrase").unwrap().secret_envelope();
    assert_ne!(first.kdf.salt_b64, second.kdf.salt_b64);
    assert_ne!(first.nonce_b64, second.nonce_b64);
}

#[test]
fn wrong_passphrase_fails_closed() {
    let sealer = Sealer::generate("right").unwrap();
    let envelope = sealer.secret_envelope();
    let blob = sealer.seal("secret").unwrap();
    assert_eq!(
        unseal("wrong", &envelope, &blob),
        Err(SealError::AuthFailed)
    );
    assert_eq!(SealError::AuthFailed.code(), "ENVELOPE_AUTH_FAILED");
}

#[test]
fn tampered_blob_fails_closed() {
    let sealer = Sealer::generate("passphrase").unwrap();
    let envelope = sealer.secret_envelope();
    let mut blob = sealer.seal("secret").unwrap();
    let first = blob.remove(0);
    blob.insert(0, if first == 'A' { 'B' } else { 'A' });
    assert_eq!(
        unseal("passphrase", &envelope, &blob),
        Err(SealError::AuthFailed)
    );
}

#[test]
fn swapped_envelope_salt_fails_closed() {
    let sealer = Sealer::generate("passphrase").unwrap();
    let blob = sealer.seal("secret").unwrap();
    let mut envelope = sealer.secret_envelope();
    envelope.kdf.salt_b64 = Sealer::generate("passphrase")
        .unwrap()
        .secret_envelope()
        .kdf
        .salt_b64;
    assert_eq!(
        unseal("passphrase", &envelope, &blob),
        Err(SealError::AuthFailed)
    );
}

#[test]
fn undecodable_blob_is_rejected_before_opening() {
    let sealer = Sealer::generate("passphrase").unwrap();
    let envelope = sealer.secret_envelope();
    assert_eq!(
        unseal("passphrase", &envelope, "!!!not-base64!!!"),
        Err(SealError::InvalidBlob)
    );
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

#[test]
fn unseal_rejects_an_unknown_scheme() {
    let sealer = Sealer::generate("passphrase").unwrap();
    let mut envelope = sealer.secret_envelope();
    let blob = sealer.seal("secret").unwrap();
    envelope.scheme = "rot13".to_owned();
    assert_eq!(
        unseal("passphrase", &envelope, &blob),
        Err(SealError::InvalidEnvelope)
    );
}
