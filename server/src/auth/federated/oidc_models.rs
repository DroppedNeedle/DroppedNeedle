//! OpenID Connect wire shapes: discovery, token response, JWKS, JWT header.
//!
//! Required fields fail decoding: a discovery document without its
//! endpoints or key set, a token response without `access_token`, a JWK
//! without `kty`, a JWT header without `alg`.

use serde::Deserialize;

/// `/.well-known/openid-configuration`, the fields we use.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryWire {
    /// The issuer identifier; `id_token`s must carry it as `iss`.
    pub issuer: String,
    /// Where browsers are sent to sign in.
    pub authorization_endpoint: String,
    /// Where authorization codes are exchanged.
    pub token_endpoint: String,
    /// Optional profile endpoint.
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    /// The provider's signing keys.
    pub jwks_uri: String,
}

/// Token endpoint answer.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenWire {
    /// Bearer token for userinfo.
    pub access_token: String,
    /// Refresh token, when the provider issues one.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Signed identity token.
    #[serde(default)]
    pub id_token: Option<String>,
}

/// One JSON Web Key. Members we do not verify with are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    /// Key type: `RSA`, `EC` or `OKP`.
    pub kty: String,
    /// Key id the JWT header names.
    #[serde(default)]
    pub kid: Option<String>,
    /// Algorithm this key is meant for, when the provider pins one.
    #[serde(default)]
    pub alg: Option<String>,
    /// `sig` or `enc`.
    #[serde(default, rename = "use")]
    pub usage: Option<String>,
    /// RSA modulus.
    #[serde(default)]
    pub n: Option<String>,
    /// RSA public exponent.
    #[serde(default)]
    pub e: Option<String>,
    /// Curve name for `EC` and `OKP` keys.
    #[serde(default)]
    pub crv: Option<String>,
    /// Curve x coordinate (or the whole Ed25519 key).
    #[serde(default)]
    pub x: Option<String>,
    /// Curve y coordinate.
    #[serde(default)]
    pub y: Option<String>,
}

/// A JWKS document. Keys are decoded one by one so a single key in a
/// format we do not know does not hide the others.
#[derive(Debug, Clone, Deserialize)]
pub struct JwksWire {
    /// Raw keys.
    pub keys: Vec<serde_json::Value>,
}

impl JwksWire {
    /// Every key that decodes.
    pub fn into_keys(self) -> Vec<Jwk> {
        self.keys
            .into_iter()
            .filter_map(|key| serde_json::from_value(key).ok())
            .collect()
    }
}

/// The JOSE header of a compact JWS.
#[derive(Debug, Clone, Deserialize)]
pub struct JwtHeader {
    /// Signature algorithm.
    pub alg: String,
    /// Which published key signed it.
    #[serde(default)]
    pub kid: Option<String>,
}
