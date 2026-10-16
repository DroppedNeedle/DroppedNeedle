//! Jellyfin wire shapes for sign-in and the admin user import.
//!
//! The user id is required wherever it appears: it is the identity the
//! DroppedNeedle account is bound to.

use serde::Deserialize;

/// `POST /Users/AuthenticateByName` answer.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthenticationResultWire {
    /// The signed-in user.
    #[serde(rename = "User")]
    pub user: UserWire,
    /// User-scoped access token.
    #[serde(rename = "AccessToken")]
    pub access_token: String,
}

/// One Jellyfin user (`AuthenticateByName` and `GET /Users`).
#[derive(Debug, Clone, Deserialize)]
pub struct UserWire {
    /// User id; the provider uid.
    #[serde(rename = "Id")]
    pub id: String,
    /// Display name.
    #[serde(rename = "Name", default)]
    pub name: Option<String>,
    /// Set when the user has an avatar.
    #[serde(rename = "HasPrimaryImage", default)]
    pub has_primary_image: Option<bool>,
    /// Avatar tag; another way servers say an avatar exists.
    #[serde(rename = "PrimaryImageTag", default)]
    pub primary_image_tag: Option<String>,
}

impl UserWire {
    /// True when the server says this user has an avatar.
    pub fn has_avatar(&self) -> bool {
        self.has_primary_image == Some(true)
            || self
                .primary_image_tag
                .as_deref()
                .is_some_and(|tag| !tag.is_empty())
    }
}
