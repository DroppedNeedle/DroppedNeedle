//! plex.tv wire shapes, read by the client in `plextv.rs`.
//!
//! Tolerant of unknown fields. A PIN's `id` and `code` and an account's
//! `uuid` are their identities: an answer without one fails decoding. The
//! account uuid is what a DroppedNeedle account is bound to. Display
//! fields stay optional because plex.tv omits them for some account kinds.
//! A list decodes entry by entry, so one odd entry (a client device with
//! no token, an account without a uuid) never sinks the rest.

use serde::Deserialize;
use serde::de::IgnoredAny;

/// `POST /pins` answer.
#[derive(Debug, Clone, Deserialize)]
pub struct Pin {
    /// PIN id to poll.
    pub id: i64,
    /// Short code for the browser URL.
    pub code: String,
}

/// `GET /pins/{id}` answer; the token appears once the user approves.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PinPoll {
    /// Account token, `null` while pending.
    #[serde(rename = "authToken", default)]
    pub auth_token: Option<String>,
}

/// `GET /user` answer, and one entry of `/home/users` or `/friends`.
#[derive(Debug, Clone, Deserialize)]
pub struct Account {
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

impl Account {
    /// The name shown for the signed-in account (v2
    /// `parse_plex_user_profile`): friendly name, user name, title.
    #[must_use]
    pub fn profile_name(&self) -> String {
        non_empty(&self.friendly_name)
            .or_else(|| non_empty(&self.username))
            .or_else(|| non_empty(&self.title))
            .unwrap_or("Plex User")
            .to_owned()
    }

    /// The name shown in the import picker (v2 `parse_plex_account`):
    /// title, friendly name, user name.
    #[must_use]
    pub fn directory_name(&self) -> String {
        non_empty(&self.title)
            .or_else(|| non_empty(&self.friendly_name))
            .or_else(|| non_empty(&self.username))
            .unwrap_or("")
            .to_owned()
    }

    /// Email, when set.
    #[must_use]
    pub fn email(&self) -> Option<String> {
        non_empty(&self.email).map(str::to_owned)
    }

    /// Avatar, when set.
    #[must_use]
    pub fn thumb(&self) -> Option<String> {
        non_empty(&self.thumb).map(str::to_owned)
    }
}

/// `/home/users` wraps its list; `/friends` answers a bare list.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum AccountList {
    /// `{"users": [...]}`.
    Wrapped {
        /// The entries.
        users: Vec<Lenient<Account>>,
    },
    /// `[...]`.
    Bare(Vec<Lenient<Account>>),
}

impl AccountList {
    /// Every entry that carries a uuid.
    pub(crate) fn into_accounts(self) -> Vec<Account> {
        let (Self::Wrapped { users: entries } | Self::Bare(entries)) = self;
        entries
            .into_iter()
            .filter_map(Lenient::into_read)
            .filter(|account| !account.uuid.trim().is_empty())
            .collect()
    }
}

/// One plex.tv `/resources` device. Client devices carry no token, so
/// every field is optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Resource {
    /// Device id; a server's matches its machine id.
    pub client_identifier: Option<String>,
    /// Comma-separated roles (`server`, `player`, ...).
    pub provides: Option<String>,
    /// The account's token for this server.
    pub access_token: Option<String>,
}

impl Resource {
    /// Whether this device is a media server.
    #[must_use]
    pub fn is_server(&self) -> bool {
        self.provides
            .as_deref()
            .is_some_and(|provides| provides.contains("server"))
    }
}

/// One list entry: read when it decodes, skipped when it does not.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Lenient<T> {
    /// An entry of the expected shape.
    Read(T),
    /// Anything else.
    Skipped(IgnoredAny),
}

impl<T> Lenient<T> {
    pub(crate) fn into_read(self) -> Option<T> {
        match self {
            Self::Read(value) => Some(value),
            Self::Skipped(_) => None,
        }
    }
}
