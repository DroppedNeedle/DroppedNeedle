//! OIDC `id_token` verification.
//!
//! The token is a compact JWS. We check its signature against the
//! provider's published keys (JWKS), or against the client secret for the
//! HMAC algorithms, and then the claims OpenID Connect Core section 3.1.3.7
//! asks a client to check: issuer, audience, authorized party, expiry,
//! not-before and the nonce we sent with the login.
//!
//! v2 decoded the `id_token` payload without checking anything. Here a
//! token that fails any check fails the login.

use base64::Engine as _;
use base64::alphabet::URL_SAFE;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use hmac::{Hmac, Mac};
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde_json::{Map, Value};
use thiserror::Error;

use super::oidc_models::{Jwk, JwtHeader};
use crate::auth::session::tokens::constant_time_eq;

/// Allowed clock difference between us and the provider, in seconds.
pub const CLOCK_LEEWAY_SECS: i64 = 60;

/// Base64url that accepts both padded and unpadded input. JWS segments
/// are unpadded, but some providers pad their JWK members.
const B64URL: GeneralPurpose = GeneralPurpose::new(
    &URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// What a valid `id_token` for this login must say.
#[derive(Debug, Clone, Copy)]
pub struct IdTokenRules<'a> {
    /// The `issuer` from the provider's discovery document.
    pub issuer: &'a str,
    /// Our client id; must be in `aud`.
    pub client_id: &'a str,
    /// Our client secret, for providers that sign with HS256/384/512.
    pub client_secret: Option<&'a str>,
    /// The nonce sent in the authorize URL.
    pub nonce: &'a str,
    /// Now, unix seconds.
    pub now_unix: i64,
}

/// Why an `id_token` was refused. Messages carry no token material.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdTokenError {
    /// Not three base64url segments with JSON header and payload.
    #[error("id_token is not a well-formed JWT")]
    Malformed,
    /// `none`, an unknown algorithm, or HMAC without a client secret.
    #[error("id_token algorithm {0} is not accepted")]
    Algorithm(String),
    /// No published key fits the token's `kid` and algorithm. The caller
    /// refreshes the key set once, since the provider may have rotated.
    #[error("no published signing key matches the id_token")]
    UnknownKey,
    /// A matching key exists but the signature does not verify.
    #[error("id_token signature does not verify")]
    Signature,
    /// A claim check failed; names the claim.
    #[error("id_token {0} claim is not valid for this login")]
    Claim(&'static str),
}

/// Verify `token` and return its claims.
pub fn verify_id_token(
    token: &str,
    keys: &[Jwk],
    rules: &IdTokenRules<'_>,
) -> Result<Map<String, Value>, IdTokenError> {
    let mut parts = token.split('.');
    let (Some(header_b64), Some(payload_b64), Some(signature_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(IdTokenError::Malformed);
    };
    let header: JwtHeader = decode_json(header_b64)?;
    let payload: Value = decode_json(payload_b64)?;
    let Value::Object(claims) = payload else {
        return Err(IdTokenError::Malformed);
    };
    let signature = B64URL
        .decode(signature_b64)
        .map_err(|_| IdTokenError::Malformed)?;
    let signed = &token.as_bytes()[..header_b64.len() + 1 + payload_b64.len()];

    verify_signature(&header, signed, &signature, keys, rules.client_secret)?;
    check_claims(&claims, rules)?;
    Ok(claims)
}

fn decode_json<T: serde::de::DeserializeOwned>(segment: &str) -> Result<T, IdTokenError> {
    let bytes = B64URL
        .decode(segment)
        .map_err(|_| IdTokenError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| IdTokenError::Malformed)
}

/// The key families a JWS algorithm can be verified with.
#[derive(Clone, Copy)]
enum Family {
    Rsa(&'static signature::RsaParameters),
    Ec(&'static signature::EcdsaVerificationAlgorithm, &'static str),
    Ed25519,
    Hmac,
}

fn family(alg: &str) -> Option<Family> {
    Some(match alg {
        "RS256" => Family::Rsa(&signature::RSA_PKCS1_2048_8192_SHA256),
        "RS384" => Family::Rsa(&signature::RSA_PKCS1_2048_8192_SHA384),
        "RS512" => Family::Rsa(&signature::RSA_PKCS1_2048_8192_SHA512),
        "PS256" => Family::Rsa(&signature::RSA_PSS_2048_8192_SHA256),
        "PS384" => Family::Rsa(&signature::RSA_PSS_2048_8192_SHA384),
        "PS512" => Family::Rsa(&signature::RSA_PSS_2048_8192_SHA512),
        "ES256" => Family::Ec(&signature::ECDSA_P256_SHA256_FIXED, "P-256"),
        "ES384" => Family::Ec(&signature::ECDSA_P384_SHA384_FIXED, "P-384"),
        "EdDSA" => Family::Ed25519,
        "HS256" | "HS384" | "HS512" => Family::Hmac,
        _ => return None,
    })
}

fn verify_signature(
    header: &JwtHeader,
    signed: &[u8],
    signature: &[u8],
    keys: &[Jwk],
    client_secret: Option<&str>,
) -> Result<(), IdTokenError> {
    let Some(family) = family(&header.alg) else {
        return Err(IdTokenError::Algorithm(header.alg.clone()));
    };
    if matches!(family, Family::Hmac) {
        let secret = client_secret
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| IdTokenError::Algorithm(header.alg.clone()))?;
        return verify_hmac(&header.alg, secret.as_bytes(), signed, signature);
    }
    let candidates: Vec<&Jwk> = keys
        .iter()
        .filter(|key| key.usage.as_deref().is_none_or(|usage| usage == "sig"))
        .filter(|key| key.alg.as_deref().is_none_or(|alg| alg == header.alg))
        .filter(|key| match (&header.kid, &key.kid) {
            (Some(wanted), Some(kid)) => wanted == kid,
            (Some(_), None) => false,
            (None, _) => true,
        })
        .filter(|key| fits(family, key))
        .collect();
    if candidates.is_empty() {
        return Err(IdTokenError::UnknownKey);
    }
    if candidates
        .iter()
        .any(|key| verify_with(family, key, signed, signature))
    {
        Ok(())
    } else {
        Err(IdTokenError::Signature)
    }
}

fn fits(family: Family, key: &Jwk) -> bool {
    match family {
        Family::Rsa(_) => key.kty == "RSA",
        Family::Ec(_, curve) => key.kty == "EC" && key.crv.as_deref() == Some(curve),
        Family::Ed25519 => key.kty == "OKP" && key.crv.as_deref() == Some("Ed25519"),
        Family::Hmac => false,
    }
}

fn verify_with(family: Family, key: &Jwk, signed: &[u8], signature: &[u8]) -> bool {
    let member = |value: &Option<String>| value.as_deref().and_then(|v| B64URL.decode(v).ok());
    match family {
        Family::Rsa(params) => {
            let (Some(n), Some(e)) = (member(&key.n), member(&key.e)) else {
                return false;
            };
            RsaPublicKeyComponents { n, e }
                .verify(params, signed, signature)
                .is_ok()
        }
        Family::Ec(algorithm, _) => {
            let (Some(x), Some(y)) = (member(&key.x), member(&key.y)) else {
                return false;
            };
            let mut point = Vec::with_capacity(1 + x.len() + y.len());
            point.push(0x04);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            UnparsedPublicKey::new(algorithm, point)
                .verify(signed, signature)
                .is_ok()
        }
        Family::Ed25519 => {
            let Some(x) = member(&key.x) else {
                return false;
            };
            UnparsedPublicKey::new(&signature::ED25519, x)
                .verify(signed, signature)
                .is_ok()
        }
        Family::Hmac => false,
    }
}

fn verify_hmac(
    alg: &str,
    secret: &[u8],
    signed: &[u8],
    signature: &[u8],
) -> Result<(), IdTokenError> {
    fn check<M: Mac + hmac::digest::KeyInit>(
        secret: &[u8],
        signed: &[u8],
        signature: &[u8],
    ) -> Result<(), IdTokenError> {
        let mut mac = <M as hmac::digest::KeyInit>::new_from_slice(secret)
            .map_err(|_| IdTokenError::Signature)?;
        mac.update(signed);
        // `verify_slice` compares in constant time.
        mac.verify_slice(signature)
            .map_err(|_| IdTokenError::Signature)
    }
    match alg {
        "HS256" => check::<Hmac<sha2::Sha256>>(secret, signed, signature),
        "HS384" => check::<Hmac<sha2::Sha384>>(secret, signed, signature),
        "HS512" => check::<Hmac<sha2::Sha512>>(secret, signed, signature),
        other => Err(IdTokenError::Algorithm(other.to_owned())),
    }
}

fn check_claims(claims: &Map<String, Value>, rules: &IdTokenRules<'_>) -> Result<(), IdTokenError> {
    let text = |name: &str| claims.get(name).and_then(Value::as_str);
    let time = |name: &str| {
        claims
            .get(name)
            .and_then(|value| value.as_i64().or_else(|| value.as_f64().map(|v| v as i64)))
    };

    if text("iss") != Some(rules.issuer) {
        return Err(IdTokenError::Claim("iss"));
    }
    let audience_ok = match claims.get("aud") {
        Some(Value::String(aud)) => aud == rules.client_id,
        Some(Value::Array(auds)) => auds.iter().any(|aud| aud.as_str() == Some(rules.client_id)),
        _ => false,
    };
    if !audience_ok {
        return Err(IdTokenError::Claim("aud"));
    }
    if let Some(azp) = claims.get("azp")
        && azp.as_str() != Some(rules.client_id)
    {
        return Err(IdTokenError::Claim("azp"));
    }
    match time("exp") {
        Some(exp) if rules.now_unix <= exp.saturating_add(CLOCK_LEEWAY_SECS) => {}
        _ => return Err(IdTokenError::Claim("exp")),
    }
    if let Some(iat) = time("iat")
        && iat > rules.now_unix.saturating_add(CLOCK_LEEWAY_SECS)
    {
        return Err(IdTokenError::Claim("iat"));
    }
    if let Some(nbf) = time("nbf")
        && nbf > rules.now_unix.saturating_add(CLOCK_LEEWAY_SECS)
    {
        return Err(IdTokenError::Claim("nbf"));
    }
    match text("nonce") {
        Some(nonce) if constant_time_eq(nonce, rules.nonce) => {}
        _ => return Err(IdTokenError::Claim("nonce")),
    }
    if text("sub").is_none_or(str::is_empty) {
        return Err(IdTokenError::Claim("sub"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _};
    use serde_json::json;

    const NOW: i64 = 1_800_000_000;

    struct Signer {
        pair: EcdsaKeyPair,
        jwk: Jwk,
    }

    fn signer() -> Signer {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let point = pair.public_key().as_ref();
        let jwk = Jwk {
            kty: "EC".to_owned(),
            kid: Some("k1".to_owned()),
            alg: Some("ES256".to_owned()),
            usage: Some("sig".to_owned()),
            crv: Some("P-256".to_owned()),
            x: Some(B64URL.encode(&point[1..33])),
            y: Some(B64URL.encode(&point[33..65])),
            n: None,
            e: None,
        };
        Signer { pair, jwk }
    }

    fn sign(signer: &Signer, header: Value, claims: Value) -> String {
        let head = format!(
            "{}.{}",
            B64URL.encode(header.to_string()),
            B64URL.encode(claims.to_string())
        );
        let signature = signer
            .pair
            .sign(&SystemRandom::new(), head.as_bytes())
            .unwrap();
        format!("{head}.{}", B64URL.encode(signature.as_ref()))
    }

    fn claims() -> Value {
        json!({
            "iss": "https://idp.test", "aud": ["droppedneedle", "other"],
            "azp": "droppedneedle", "sub": "user-1", "exp": NOW + 300,
            "iat": NOW, "nonce": "n-1"
        })
    }

    fn rules() -> IdTokenRules<'static> {
        IdTokenRules {
            issuer: "https://idp.test",
            client_id: "droppedneedle",
            client_secret: None,
            nonce: "n-1",
            now_unix: NOW,
        }
    }

    #[test]
    fn accepts_a_valid_token_and_rejects_every_tampered_claim() {
        let key = signer();
        let header = json!({"alg": "ES256", "kid": "k1"});
        let token = sign(&key, header.clone(), claims());
        let verified = verify_id_token(&token, std::slice::from_ref(&key.jwk), &rules()).unwrap();
        assert_eq!(verified["sub"], "user-1");

        for (field, value, claim) in [
            ("iss", json!("https://evil.test"), "iss"),
            ("aud", json!("someone-else"), "aud"),
            ("azp", json!("someone-else"), "azp"),
            ("exp", json!(NOW - 120), "exp"),
            ("nbf", json!(NOW + 600), "nbf"),
            ("nonce", json!("replayed"), "nonce"),
        ] {
            let mut tampered = claims();
            tampered[field] = value;
            let token = sign(&key, header.clone(), tampered);
            assert_eq!(
                verify_id_token(&token, std::slice::from_ref(&key.jwk), &rules()),
                Err(IdTokenError::Claim(claim)),
                "{field}"
            );
        }
    }

    #[test]
    fn refuses_forged_unsigned_and_unknown_key_tokens() {
        let key = signer();
        let other = signer();
        let forged = sign(&other, json!({"alg": "ES256", "kid": "k1"}), claims());
        assert_eq!(
            verify_id_token(&forged, std::slice::from_ref(&key.jwk), &rules()),
            Err(IdTokenError::Signature)
        );
        let unsigned = format!(
            "{}.{}.",
            B64URL.encode(json!({"alg": "none"}).to_string()),
            B64URL.encode(claims().to_string())
        );
        assert_eq!(
            verify_id_token(&unsigned, std::slice::from_ref(&key.jwk), &rules()),
            Err(IdTokenError::Algorithm("none".to_owned()))
        );
        let rotated = sign(&key, json!({"alg": "ES256", "kid": "k2"}), claims());
        assert_eq!(
            verify_id_token(&rotated, std::slice::from_ref(&key.jwk), &rules()),
            Err(IdTokenError::UnknownKey)
        );
        // HMAC tokens need our client secret; without one they are refused.
        let hmac = format!(
            "{}.{}.c2ln",
            B64URL.encode(json!({"alg": "HS256"}).to_string()),
            B64URL.encode(claims().to_string())
        );
        assert_eq!(
            verify_id_token(&hmac, &[], &rules()),
            Err(IdTokenError::Algorithm("HS256".to_owned()))
        );
    }
}
