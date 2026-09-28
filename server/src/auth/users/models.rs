//! Domain records and `/api/v3` transport shapes.
//!
//! Wire format is snake_case. Times are unix epoch seconds (UTC). No shape
//! here carries a secret: raw tokens and recovery codes appear only in the
//! single create/mint response that issues them.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::roles::{Role, SessionKind};

/// One account row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    /// Primary key.
    pub id: String,
    /// Lowercased login identifier, when the account has one.
    pub username: Option<String>,
    /// Preferred username casing.
    pub username_display: Option<String>,
    /// Display name.
    pub display_name: String,
    /// Lowercased email, when set.
    pub email: Option<String>,
    /// Served avatar URL, when an avatar is set.
    pub avatar_url: Option<String>,
    /// Account role.
    pub role: Role,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Last login time, unix seconds.
    pub last_login_at: Option<i64>,
}

/// A local-password credential row (the `local` auth provider).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCredential {
    /// Provider row id.
    pub id: String,
    /// Owning user id.
    pub user_id: String,
    /// Scheme tag: `bcrypt` for imports, `argon2id` for new hashes.
    pub scheme: String,
    /// Scheme-specific encoded hash. Never leaves the store except to the
    /// hasher.
    pub hash: String,
}

/// One session row as the management surface sees it. No token hash:
/// listings must never expose credential material. The sibling session
/// slice owns the full row (with hash) for middleware and login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSession {
    /// Session id.
    pub id: String,
    /// Owning user id.
    pub user_id: String,
    /// Standard or companion.
    pub kind: SessionKind,
    /// Human label: the companion device name, or the browser user agent.
    pub label: String,
    /// Issue time, unix seconds.
    pub created_at: i64,
    /// Last use time, unix seconds.
    pub last_seen_at: i64,
    /// Absolute expiry, unix seconds.
    pub expires_at: i64,
}

/// What the native session lookup resolves a token hash to. The sibling
/// middleware resolves through its own store; this shape exists so the
/// management port can pin native/compat credential separation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOwner {
    /// Session id.
    pub session_id: String,
    /// Owning user id.
    pub user_id: String,
    /// Standard or companion.
    pub kind: SessionKind,
}

/// One app-password row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPasswordRecord {
    /// Row id.
    pub id: String,
    /// Owning user id.
    pub user_id: String,
    /// User-given name.
    pub name: String,
    /// SHA-256 hex of the secret. Decrypt-free verification key.
    pub secret_sha256: String,
    /// `v3:` ciphertext of the secret, for export-time re-encryption.
    pub secret_encrypted: String,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Last verification time, unix seconds.
    pub last_used_at: Option<i64>,
    /// Client label from the last verification, when the protocol sent one.
    pub last_client: Option<String>,
}

/// A per-user Last.fm link: own API credentials plus the linked session.
/// Session keys are ciphertext at rest; plaintext exists only in memory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LastFmConnection {
    /// Whether the user stored API credentials.
    pub configured: bool,
    /// `v3:` ciphertext of the user's Last.fm API key, when configured.
    pub api_key_encrypted: Option<String>,
    /// `v3:` ciphertext of the user's Last.fm shared secret, when configured.
    pub shared_secret_encrypted: Option<String>,
    /// Linked Last.fm username, when a session was exchanged.
    pub username: Option<String>,
    /// `v3:` ciphertext of the session key, when linked.
    pub session_key_encrypted: Option<String>,
}

/// A pending recovery code row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryCode {
    /// Owning user id (single active code per user).
    pub user_id: String,
    /// SHA-256 hex of the canonical code.
    pub code_hash: String,
    /// Issue time, unix seconds.
    pub created_at: i64,
    /// Expiry time, unix seconds.
    pub expires_at: i64,
}

/// The authenticated identity a compat shim verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPasswordAuth {
    /// Owning user id.
    pub user_id: String,
    /// The app-password row that verified.
    pub app_password_id: String,
}

/// Public account shape.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct UserResponse {
    /// User id.
    pub id: String,
    /// Lowercased login identifier, when set.
    pub username: Option<String>,
    /// Preferred username casing, when set.
    pub username_display: Option<String>,
    /// Display name.
    pub display_name: String,
    /// Email, when set.
    pub email: Option<String>,
    /// Served avatar URL, when an avatar is set.
    pub avatar_url: Option<String>,
    /// Account role.
    pub role: Role,
    /// Bound provider names (e.g. `local`).
    pub providers: Vec<String>,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Last login time, unix seconds.
    pub last_login_at: Option<i64>,
}

/// Own profile. Identity only; connected services and library stats belong
/// to the connections and library slices.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ProfileResponse {
    /// The account.
    #[serde(flatten)]
    pub user: UserResponse,
}

/// Change display name.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DisplayNameUpdate {
    /// New display name, 1-64 chars after trimming.
    pub display_name: String,
}

/// Change username.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UsernameUpdate {
    /// New username: 3-32 chars of letters, digits, `.`, `_`, `-`.
    pub username: String,
}

/// Change email. Null or blank clears it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmailUpdate {
    /// New email, or null to clear.
    pub email: Option<String>,
}

/// Change password on an account that already has one.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PasswordChange {
    /// Current password, proving ownership.
    pub current_password: String,
    /// New password, at least 12 chars.
    pub new_password: String,
}

/// Set the first local password on an account without one.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LocalPasswordSet {
    /// New password, at least 12 chars.
    pub new_password: String,
}

/// Avatar upload. JSON with base64 bytes: the server has no multipart
/// support, and one JSON shape keeps every client on the same parser.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AvatarUpload {
    /// `image/jpeg`, `image/png`, `image/webp`, or `image/gif`.
    pub content_type: String,
    /// Base64 image bytes, at most 5 MiB decoded.
    pub image_base64: String,
}

/// One session row for the sessions UI.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct SessionView {
    /// Session id.
    pub id: String,
    /// `standard` or `companion`.
    pub kind: String,
    /// Companion device name, or the browser user agent.
    pub label: String,
    /// Issue time, unix seconds.
    pub created_at: i64,
    /// Last use time, unix seconds.
    pub last_seen_at: i64,
    /// Absolute expiry, unix seconds.
    pub expires_at: i64,
    /// True for the session making this request.
    pub current: bool,
}

/// Own sessions.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct SessionListResponse {
    /// All live sessions, current first.
    pub sessions: Vec<SessionView>,
}

/// Mint a named companion token.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeviceSessionMint {
    /// Device label, 1-80 chars. Reminting a label revokes the prior token.
    pub label: String,
}

/// A minted companion token. The raw token appears here once, never again.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct DeviceSessionResponse {
    /// Session id.
    pub id: String,
    /// Device label.
    pub label: String,
    /// Raw Bearer [REDACTED] Shown once.
    pub token: String,
    /// Absolute expiry, unix seconds.
    pub expires_at: i64,
}

/// Admin user creation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminUserCreate {
    /// Login username, 3-32 chars.
    pub username: String,
    /// Initial password, at least 12 chars.
    pub password: String,
    /// Display name; defaults to the username casing.
    pub display_name: Option<String>,
    /// Optional email.
    pub email: Option<String>,
    /// Defaults to `user`.
    pub role: Option<Role>,
}

/// Admin user listing.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AdminUserListResponse {
    /// One page of users.
    pub users: Vec<UserResponse>,
    /// Total user count.
    pub total: u64,
}

/// Admin role change.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RoleUpdate {
    /// New role.
    pub role: Role,
}

/// A minted recovery code. The code appears here once, never again.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct RecoveryCodeResponse {
    /// Display code (`XXXX-XXXX-XXXX-XXXX-XXXX`).
    pub recovery_code: String,
    /// Expiry time, unix seconds.
    pub expires_at: i64,
}

/// Public password reset with a recovery code.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PasswordReset {
    /// Account username.
    pub username: String,
    /// Recovery code from the admin (dashes and case ignored).
    pub recovery_code: String,
    /// New password, at least 12 chars.
    pub new_password: String,
}

/// Per-user Last.fm link status. Never carries secrets.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct LastFmStatusResponse {
    /// Whether API credentials are stored.
    pub configured: bool,
    /// Whether a session is linked.
    pub linked: bool,
    /// Linked Last.fm username, when linked.
    pub username: Option<String>,
}

/// Store per-user Last.fm API credentials. Either field may be the
/// `lastfm****` mask sentinel, meaning keep the stored value.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LastFmCredentialsSet {
    /// Last.fm API key, or the mask sentinel to keep it.
    pub api_key: String,
    /// Last.fm shared secret, or the mask sentinel to keep it.
    pub shared_secret: String,
}

/// Confirmation that credentials were stored.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct LastFmConfiguredResponse {
    /// Always true when this response is returned.
    pub configured: bool,
    /// Whether a session is still linked (a key change unlinks).
    pub linked: bool,
}

/// A Last.fm sign-in token plus the URL to approve it at.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct LastFmTokenResponse {
    /// Single-use token for the session exchange.
    pub token: String,
    /// Approval URL to open in a browser.
    pub auth_url: String,
}

/// Exchange an approved token for a session.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LastFmSessionExchange {
    /// The token from the token step, after browser approval.
    pub token: String,
}

/// A linked Last.fm session.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct LastFmSessionResponse {
    /// Linked Last.fm username.
    pub username: String,
    /// Always true when this response is returned.
    pub linked: bool,
}

/// One app password, without secret material.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AppPasswordView {
    /// Row id.
    pub id: String,
    /// User-given name.
    pub name: String,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Last verification time, unix seconds.
    pub last_used_at: Option<i64>,
    /// Client label from the last verification, when sent.
    pub last_client: Option<String>,
}

/// Own app passwords.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AppPasswordListResponse {
    /// Active app passwords, oldest first.
    pub app_passwords: Vec<AppPasswordView>,
}

/// Create an app password.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppPasswordCreate {
    /// Name; defaults to `App password`.
    pub name: Option<String>,
}

/// A created app password. The secret appears here once, never again.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AppPasswordCreatedResponse {
    /// Row id.
    pub id: String,
    /// Name.
    pub name: String,
    /// Raw secret. Shown once.
    pub secret: String,
    /// Creation time, unix seconds.
    pub created_at: i64,
}

/// One app password with its owner, for admin oversight.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AdminAppPasswordView {
    /// Row id.
    pub id: String,
    /// Owning user id.
    pub user_id: String,
    /// Owner username or display fallback.
    pub owner_username: String,
    /// Owner display name.
    pub owner_display_name: String,
    /// Name.
    pub name: String,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Last verification time, unix seconds.
    pub last_used_at: Option<i64>,
    /// Client label from the last verification, when sent.
    pub last_client: Option<String>,
}

/// Every active app password across users.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AdminAppPasswordListResponse {
    /// Active app passwords, by owner then age.
    pub app_passwords: Vec<AdminAppPasswordView>,
}

/// One media-server account offered for import. Display only: the import
/// re-reads the directory server-side and never trusts echoed fields.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ImportCandidateView {
    /// `jellyfin` or `plex`.
    pub provider: String,
    /// Provider-side id (Jellyfin user id, Plex account uuid).
    pub provider_uid: String,
    /// Display name on the provider.
    pub display_name: String,
    /// Account image URL, when the provider exposes one.
    pub avatar_url: Option<String>,
    /// Account email, when the provider exposes one.
    pub email: Option<String>,
    /// True when a binding for this `(provider, provider_uid)` exists.
    pub already_imported: bool,
}

/// Importable accounts on one provider.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ImportCandidateListResponse {
    /// Every account the directory enumerated.
    pub candidates: Vec<ImportCandidateView>,
}

/// Import a batch of accounts from one provider.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportUsersRequest {
    /// `jellyfin` or `plex`.
    pub provider: String,
    /// Provider uids to import, matched against a fresh directory listing.
    pub provider_uids: Vec<String>,
}

/// A finished import batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ImportUsersResponse {
    /// Newly created accounts, in request order.
    pub imported: Vec<UserResponse>,
    /// Existing accounts the provider was linked to (email match).
    pub linked: Vec<UserResponse>,
    /// Uids skipped: unknown, already bound, or failed.
    pub skipped: Vec<String>,
    /// Count of `imported`, for the admin summary line.
    pub total_imported: u64,
}
