//! What the setup guide's look answers, and the candidates it asks.

use droppedneedle_acquire::setup_folders::{FolderLookState, FoundFolder};
use serde::Serialize;
use utoipa::ToSchema;

/// A service the setup guide looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetupServiceKind {
    Slskd,
    Sabnzbd,
    Nzbget,
    Prowlarr,
    Lidarr,
    Jellyfin,
    Navidrome,
    Plex,
    Deemix,
}

/// What answered at a candidate address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeOutcome {
    /// The name does not resolve to an address on this network.
    NotFound,
    /// Nothing answered in time, or the connection was refused.
    NoAnswer,
    /// The service answered as itself.
    Found,
    /// Something answered, but not as that service.
    SomethingElse,
}

/// Something to know about a service that answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceNote {
    /// SABnzbd answered but refuses requests to this name until it is on
    /// its host whitelist.
    SabnzbdHostNotWhitelisted,
}

/// One candidate service and what answered there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct FoundService {
    /// Which service was looked for.
    pub kind: SetupServiceKind,
    /// The fixed address asked, never one taken from an answer.
    pub url: String,
    /// What answered.
    pub outcome: ProbeOutcome,
    /// The version it reported, when it gave a short plain one.
    pub version: Option<String>,
    /// Something to know before setting it up.
    pub note: Option<ServiceNote>,
}

/// Everything the setup guide found. A look that failed is part of the
/// answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SetupFound {
    /// One per candidate, in the candidates' order.
    pub services: Vec<FoundService>,
    /// The folders worth suggesting, and those already set.
    pub folders: Vec<FoundFolder>,
    /// How the folder look went.
    pub folder_look: FolderLookState,
}

/// The fixed base address of each service the setup guide asks.
#[derive(Debug, Clone)]
pub struct SetupCandidates {
    pub slskd: String,
    pub sabnzbd: String,
    pub nzbget: String,
    pub prowlarr: String,
    pub lidarr: String,
    pub jellyfin: String,
    pub navidrome: String,
    pub plex: String,
    pub deemix: String,
}

impl SetupCandidates {
    /// The names and ports the projects' own compose examples and our docs
    /// use, on the container network.
    pub fn live() -> Self {
        Self {
            slskd: "http://slskd:5030".to_owned(),
            sabnzbd: "http://sabnzbd:8080".to_owned(),
            nzbget: "http://nzbget:6789".to_owned(),
            prowlarr: "http://prowlarr:9696".to_owned(),
            lidarr: "http://lidarr:8686".to_owned(),
            jellyfin: "http://jellyfin:8096".to_owned(),
            navidrome: "http://navidrome:4533".to_owned(),
            plex: "http://plex:32400".to_owned(),
            deemix: "http://deemix:6595".to_owned(),
        }
    }

    /// Each one at `base`.
    pub fn all_at(base: &str) -> Self {
        let at = || base.to_owned();
        Self {
            slskd: at(),
            sabnzbd: at(),
            nzbget: at(),
            prowlarr: at(),
            lidarr: at(),
            jellyfin: at(),
            navidrome: at(),
            plex: at(),
            deemix: at(),
        }
    }

    /// Every candidate, in the order the answer lists them.
    pub fn list(&self) -> [(SetupServiceKind, &str); 9] {
        use SetupServiceKind as Kind;
        [
            (Kind::Slskd, &self.slskd),
            (Kind::Sabnzbd, &self.sabnzbd),
            (Kind::Nzbget, &self.nzbget),
            (Kind::Prowlarr, &self.prowlarr),
            (Kind::Lidarr, &self.lidarr),
            (Kind::Jellyfin, &self.jellyfin),
            (Kind::Navidrome, &self.navidrome),
            (Kind::Plex, &self.plex),
            (Kind::Deemix, &self.deemix),
        ]
    }
}
