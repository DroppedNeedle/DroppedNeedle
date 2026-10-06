//! plex.tv wire shapes for the PIN sign-in and the admin user import.
//!
//! The account uuid is required: it is the identity the DroppedNeedle
//! account is bound to. Display fields stay optional because plex.tv omits
//! them for some account kinds.

use serde::Deserialize;

/// `POST /pins` answer.
#[derive(Debug, Clone, Deserialize)]
pub struct PinWire {
    /// PIN id to poll.
    pub id: i64,
    /// Short code for the browser URL.
    pub code: String,
}

/// `GET /pins/{id}` answer; the token appears once the user approves.
#[derive(Debug, Clone, Deserialize)]
pub struct PinPollWire {
    /// Account token, `null` while pending.
    #[serde(rename = "authToken", default)]
    pub auth_token: Option<String>,
}

/// `GET /user` answer, and one entry of `/home/users` or `/friends`.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountWire {
    /// Account uuid; the provider uid.
    pub uuid: String,
    /// Login name.
    #[serde(default)]
    pub username: Option<String>,
    /// Display title (home users and friends).
    #[serde(default)]
    pub title: Option<String>,
    /// Display name (`/user`).
    #[serde(rename = "friendlyName", default)]
    pub friendly_name: Option<String>,
    /// Account email.
    #[serde(default)]
    pub email: Option<String>,
    /// Avatar URL.
    #[serde(default)]
    pub thumb: Option<String>,
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|text| !text.trim().is_empty())
}

impl AccountWire {
    /// The name shown for the signed-in account (v2
    /// `parse_plex_user_profile`): friendly name, user name, title.
    pub fn profile_name(&self) -> String {
        non_empty(&self.friendly_name)
            .or_else(|| non_empty(&self.username))
            .or_else(|| non_empty(&self.title))
            .unwrap_or("Plex User")
            .to_owned()
    }

    /// The name shown in the import picker (v2 `parse_plex_account`):
    /// title, friendly name, user name.
    pub fn directory_name(&self) -> String {
        non_empty(&self.title)
            .or_else(|| non_empty(&self.friendly_name))
            .or_else(|| non_empty(&self.username))
            .unwrap_or("")
            .to_owned()
    }

    /// Email, when set.
    pub fn email(&self) -> Option<String> {
        non_empty(&self.email).map(str::to_owned)
    }

    /// Avatar, when set.
    pub fn thumb(&self) -> Option<String> {
        non_empty(&self.thumb).map(str::to_owned)
    }
}

/// `/home/users` wraps its list; `/friends` answers a bare list.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AccountListWire {
    /// `{"users": [...]}`.
    Wrapped {
        /// Raw entries, decoded one by one.
        users: Vec<serde_json::Value>,
    },
    /// `[...]`.
    Bare(Vec<serde_json::Value>),
}

impl AccountListWire {
    /// Every entry that carries a uuid.
    pub fn into_accounts(self) -> Vec<AccountWire> {
        let (Self::Wrapped { users: entries } | Self::Bare(entries)) = self;
        entries
            .into_iter()
            .filter_map(|entry| serde_json::from_value::<AccountWire>(entry).ok())
            .filter(|account| !account.uuid.trim().is_empty())
            .collect()
    }
}
