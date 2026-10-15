//! plex.tv, the Plex account service: its API root and the device shape
//! its `/resources` answer lists. Both the Plex sign-in and the Plex
//! media server adapter read them.

use serde::Deserialize;

/// plex.tv account API root.
pub const PLEX_TV_BASE: &str = "https://plex.tv/api/v2";

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
