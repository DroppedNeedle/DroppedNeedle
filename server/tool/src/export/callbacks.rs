//! Addresses outside services call back on, which change from v2 to v3.
//!
//! v2 registered `/api/v1` addresses with a single sign-on provider, with
//! Spotify and, per open edit, with MusicBrainz. v3 answers on its own
//! paths, so before the upgrade moves anything, [`check_callbacks`] reads
//! the stopped v2 instance (its `config.json` and its database, read-only)
//! and lists what the operator has to change. The upgrade and the export
//! stop on that list until the operator says it is done
//! (`--callbacks-changed`). Every value read here is plain text, so no key
//! is needed.

use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::export::bundle::carried_rows;
use crate::export::error::ExportError;
use crate::export::exporter::{open_v2_db, table_exists};
use crate::export::sections::contributions::CALLBACK_TOKENS;
use crate::export::sql::{ADMINS, AdminRow};
use crate::export::v2dir::read_config;
use droppedneedle_acquire::imports::spotify_oauth::{SPOTIFY_CALLBACK_PATH, redirect_uri};

/// v2's Spotify callback path.
pub const SPOTIFY_V2_CALLBACK_PATH: &str = "/api/v1/me/connections/spotify/auth/callback";
/// v2's single sign-on callback path.
pub const OIDC_V2_CALLBACK_PATH: &str = "/api/v1/auth/oidc/callback";
/// v3's single sign-on callback path.
pub const OIDC_CALLBACK_PATH: &str = "/api/v3/auth/oidc/callback";

/// Said of a single sign-on address that does not end in v2's path: the
/// import cannot tell what v3's should be, so it keeps v2's.
const OIDC_KEPT: &str = "The upgrade keeps v2's single sign-on address in v3's settings. \
     After it, enter the new one under Settings > Security too.";

/// The flag that lets the upgrade or the export go on past the list.
pub const CALLBACKS_CHANGED_FLAG: &str = "--callbacks-changed";

/// v2's single sign-on redirect address with v2's callback path replaced
/// by v3's. `None` when the address does not end in v2's path.
#[must_use]
pub fn v3_oidc_redirect(v2_redirect: &str) -> Option<String> {
    v2_redirect
        .trim()
        .strip_suffix(OIDC_V2_CALLBACK_PATH)
        .map(|base| format!("{base}{OIDC_CALLBACK_PATH}"))
}

/// A feature whose callback address changes with the upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallbackFeature {
    /// Signing in through an OpenID Connect provider.
    SingleSignOn,
    /// Spotify playlist import.
    Spotify,
    /// Albums being added to MusicBrainz in its release editor.
    MusicbrainzContributions,
}

/// An address, whole when v2's settings name the host, else a path (the
/// base path included) to put after the scheme and host DroppedNeedle is
/// opened at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// A full address.
    Full(String),
    /// A path after DroppedNeedle's scheme and host.
    Path(String),
}

impl Address {
    fn from_parts(text: String) -> Self {
        if text.starts_with('/') {
            Self::Path(text)
        } else {
            Self::Full(text)
        }
    }

    /// The address, or the path, as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Full(text) | Self::Path(text) => text,
        }
    }

    fn line(&self) -> String {
        match self {
            Self::Full(text) => text.clone(),
            Self::Path(path) => format!(
                "your DroppedNeedle host, such as https://music.example.com, followed by {path}"
            ),
        }
    }
}

/// One address to change at an outside service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressChange {
    /// The feature that uses it.
    pub feature: CallbackFeature,
    /// The address v2 registered.
    pub old: Address,
    /// The address v3 uses.
    pub new: Address,
}

/// The v2 admin accounts, as far as getting back in matters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Admins {
    /// Some admin signs in with a username and password.
    pub any_password: bool,
    /// Usernames of the admins, oldest account first.
    pub usernames: Vec<String>,
    /// Display names of admins with no username, whom `recovery-code`
    /// cannot reach.
    pub unnamed: Vec<String>,
}

/// What a v2 instance registered with outside services that v3 answers
/// elsewhere.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallbackCheck {
    /// Addresses to change, single sign-on first.
    pub changes: Vec<AddressChange>,
    /// Edits open in MusicBrainz's release editor whose return link stops
    /// working.
    pub open_contributions: u64,
    /// The admins, read when single sign-on is on.
    pub admins: Option<Admins>,
}

/// One item the operator acknowledged, as the upgrade report keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Acknowledged {
    /// The feature.
    pub feature: CallbackFeature,
    /// Its new address (a path when v2 did not name the host); none for
    /// MusicBrainz contributions, which have nothing to change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_address: Option<String>,
}

/// Read what the stopped v2 instance registered with outside services:
/// `config_dir` holds its `config.json` and `db_path` is its database.
/// `base_path` is v3's deployment base path. A read that fails is an
/// error, never an empty result; a table v2 does not have holds nothing.
pub fn check_callbacks(
    config_dir: &Path,
    db_path: &Path,
    base_path: &str,
) -> Result<CallbackCheck, ExportError> {
    let config = read_config(config_dir)?;
    let single_sign_on = oidc_change(&config, base_path);
    let db = open_v2_db(db_path)?;
    let admins = match single_sign_on {
        Some(_) => Some(read_admins(&db)?),
        None => None,
    };
    drop(db);
    Ok(CallbackCheck {
        changes: single_sign_on
            .into_iter()
            .chain(spotify_change(&config, base_path))
            .collect(),
        open_contributions: carried_rows(db_path, &CALLBACK_TOKENS)?,
        admins,
    })
}

/// A text field of a v2 settings section, trimmed; empty when absent.
fn text<'a>(section: Option<&'a Map<String, Value>>, field: &str) -> &'a str {
    section
        .and_then(|section| section.get(field))
        .and_then(Value::as_str)
        .map_or("", str::trim)
}

/// v2 signs in through single sign-on only when it is on and has an
/// issuer, a client id and a redirect address. When that address does not
/// end in v2's path, the new one is the base path and v3's path.
fn oidc_change(config: &Map<String, Value>, base_path: &str) -> Option<AddressChange> {
    let section = config.get("oidc_settings").and_then(Value::as_object);
    let enabled = section
        .and_then(|section| section.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let redirect = text(section, "redirect_uri");
    if !enabled
        || text(section, "issuer").is_empty()
        || text(section, "client_id").is_empty()
        || redirect.is_empty()
    {
        return None;
    }
    let new = v3_oidc_redirect(redirect).map_or_else(
        || Address::Path(format!("{base_path}{OIDC_CALLBACK_PATH}")),
        Address::Full,
    );
    Some(AddressChange {
        feature: CallbackFeature::SingleSignOn,
        old: Address::Full(redirect.to_owned()),
        new,
    })
}

/// A Spotify app is registered once v2 holds its client id. Both
/// addresses are built the way v2 and v3 build them: the saved origin,
/// then the base path, then the callback path.
fn spotify_change(config: &Map<String, Value>, base_path: &str) -> Option<AddressChange> {
    let section = config.get("spotify_settings").and_then(Value::as_object);
    if text(section, "client_id").is_empty() {
        return None;
    }
    let origin = text(section, "spotify_redirect_origin");
    let address = |path: &str| Address::from_parts(redirect_uri(path, origin, "", base_path));
    Some(AddressChange {
        feature: CallbackFeature::Spotify,
        old: address(SPOTIFY_V2_CALLBACK_PATH),
        new: address(SPOTIFY_CALLBACK_PATH),
    })
}

fn read_admins(db: &rusqlite::Connection) -> Result<Admins, ExportError> {
    let mut admins = Admins::default();
    for table in ["auth_users", "auth_providers"] {
        if !table_exists(db, table)? {
            return Ok(admins);
        }
    }
    let failed = |error: rusqlite::Error| ExportError::V2Database {
        table: "auth_users".to_owned(),
        detail: error.to_string(),
    };
    let mut statement = db.prepare(ADMINS.text()).map_err(failed)?;
    let rows = statement.query_map([], AdminRow::read).map_err(failed)?;
    for row in rows {
        let admin = row.map_err(failed)?;
        admins.any_password |= admin.has_password;
        match admin.username.filter(|name| !name.trim().is_empty()) {
            Some(username) => admins.usernames.push(username),
            None => admins.unnamed.push(admin.display_name),
        }
    }
    Ok(admins)
}

impl CallbackCheck {
    /// Nothing changes with the upgrade.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.open_contributions == 0
    }

    /// Display names of the admins, when single sign-on is on and no
    /// admin can be got back in another way: none has a password and none
    /// has a username for `recovery-code`.
    #[must_use]
    pub fn unreachable_admins(&self) -> Option<&[String]> {
        self.admins
            .as_ref()
            .filter(|admins| {
                !admins.any_password && admins.usernames.is_empty() && !admins.unnamed.is_empty()
            })
            .map(|admins| admins.unnamed.as_slice())
    }

    /// Whether the run stops here. An admin nobody can get back in stops
    /// it even when the operator acknowledged the list.
    #[must_use]
    pub fn stops(&self, acknowledged: bool) -> bool {
        self.unreachable_admins().is_some() || (!acknowledged && !self.is_empty())
    }

    /// The items, as the upgrade report keeps them.
    #[must_use]
    pub fn acknowledged(&self) -> Vec<Acknowledged> {
        let mut items: Vec<Acknowledged> = self
            .changes
            .iter()
            .map(|change| Acknowledged {
                feature: change.feature,
                new_address: Some(change.new.as_str().to_owned()),
            })
            .collect();
        if self.open_contributions > 0 {
            items.push(Acknowledged {
                feature: CallbackFeature::MusicbrainzContributions,
                new_address: None,
            });
        }
        items
    }

    /// The message a stopped run prints. `command` is `upgrade` or
    /// `export`, the command to run again. A run that already has
    /// `--callbacks-changed` stops only for an admin nobody could get back
    /// in, so that is all it is told.
    #[must_use]
    pub fn stop_message(&self, command: &str, acknowledged: bool) -> String {
        let admins = self.unreachable_admins().map(|names| {
            format!(
                "No admin could get back in if single sign-on failed: {} {} no password and no \
                 username. Set a username for {} in v2 (Profile).",
                names.join(", "),
                if names.len() == 1 { "has" } else { "have" },
                if names.len() == 1 {
                    "it"
                } else {
                    "one of them"
                },
            )
        });
        if acknowledged && let Some(admins) = admins {
            return format!(
                "{admins}\nThen run the {command} again with {CALLBACKS_CHANGED_FLAG}."
            );
        }
        let mut lines = vec!["Before upgrading, change these addresses.".to_owned()];
        lines.extend(self.blocks());
        lines.push(String::new());
        lines.push(
            "If your provider allows only one address, change it as you run the upgrade, not \
             before."
                .to_owned(),
        );
        if let Some(admins) = admins {
            lines.push(String::new());
            lines.push(admins);
        }
        lines.push(format!(
            "When done, run the {command} again with {CALLBACKS_CHANGED_FLAG}."
        ));
        lines.join("\n")
    }

    /// What a run going on with `--callbacks-changed` prints first.
    #[must_use]
    pub fn acknowledged_message(&self) -> String {
        let mut lines = vec![format!(
            "Going on with {CALLBACKS_CHANGED_FLAG}. These change with the upgrade."
        )];
        lines.extend(self.blocks());
        lines.join("\n")
    }

    /// One block per item, each after a blank line.
    fn blocks(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for change in &self.changes {
            lines.push(String::new());
            lines.push(
                match change.feature {
                    CallbackFeature::SingleSignOn => {
                        "Single sign-on: change the redirect address at your provider."
                    }
                    CallbackFeature::Spotify => {
                        "Spotify playlist import: change the redirect URI in the Spotify \
                         developer dashboard."
                    }
                    CallbackFeature::MusicbrainzContributions => "MusicBrainz contributions:",
                }
                .to_owned(),
            );
            lines.push(format!("  from  {}", change.old.line()));
            lines.push(format!("  to    {}", change.new.line()));
            if change.feature == CallbackFeature::SingleSignOn {
                if matches!(change.new, Address::Path(_)) {
                    lines.push(format!("  {OIDC_KEPT}"));
                }
                lines.extend(self.admin_lines());
            }
        }
        if self.open_contributions > 0 {
            let count = self.open_contributions;
            let (edits, them) = if count == 1 {
                ("edit", "it")
            } else {
                ("edits", "them")
            };
            lines.push(String::new());
            lines.push(format!(
                "MusicBrainz contributions: {count} {edits} open in MusicBrainz's release editor \
                 will not come back. Finish {them} in v2 first, or start {them} again from v3."
            ));
        }
        lines
    }

    /// How an admin gets back in if single sign-on fails after the upgrade.
    /// An admin nobody can reach has its own paragraph instead.
    fn admin_lines(&self) -> Vec<String> {
        let Some(admins) = &self.admins else {
            return Vec::new();
        };
        if self.unreachable_admins().is_some() {
            return Vec::new();
        }
        let mut lines = Vec::new();
        if let (false, Some(username)) = (admins.any_password, admins.usernames.first()) {
            lines.push(format!(
                "  No admin has a password. If single sign-on fails after the upgrade, stop the \
                 server and run droppedneedle-tool recovery-code {username}."
            ));
        }
        for name in &admins.unnamed {
            lines.push(format!(
                "  Admin {name} has no username, so recovery-code cannot reach it. Set one in v2 \
                 (Profile)."
            ));
        }
        lines
    }
}
