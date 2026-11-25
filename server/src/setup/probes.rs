//! One keyless GET per candidate service, through the factory's
//! `lan_probe` client.
//!
//! A request carries no key, cookie, `Authorization` header or sign-in
//! parameter, so nothing is given away to whatever answers. The routes
//! were read keyless on 2026-11-25 against slskd 0.25.1, SABnzbd 5.1.3,
//! NZBGet v26.3, Prowlarr v3.1.0.5799, Lidarr v3.1.6.5079, Jellyfin,
//! Navidrome v0.64.2 and Deemix webui 4.8.0. SABnzbd and Navidrome are
//! asked for JSON (`output=json`, `f=json`) as their API docs describe,
//! and Plex through `Accept`; Plex's `/identity` is not documented by Plex
//! and was not read at the source. Never asked: Lidarr's or Prowlarr's
//! `/initialize.json` (it hands out the key while sign-in is off),
//! Jellyfin's user list, a default login, or Deemix's sign-in.

use std::net::IpAddr;

use droppedneedle_acquire::usenet::sabnzbd::HOSTNAME_REFUSED;
use droppedneedle_kernel::http_client::{
    BodyError, LAN_PROBE_TIMEOUT, LanProbeFailure, keep_lan_address, lan_probe_failure, read_capped,
};
use reqwest::{Client, StatusCode, Url, header};
use serde::Deserialize;

use super::models::{FoundService, ProbeOutcome, ServiceNote, SetupServiceKind};

/// The most of an answer read.
const ANSWER_CAP: usize = 64 * 1024;
/// The longest version shown.
const VERSION_MAX: usize = 32;

/// What a probe decided, before it becomes a [`FoundService`].
struct Verdict {
    outcome: ProbeOutcome,
    version: Option<String>,
    note: Option<ServiceNote>,
}

impl Verdict {
    fn just(outcome: ProbeOutcome) -> Self {
        Self {
            outcome,
            version: None,
            note: None,
        }
    }

    fn found(version: Option<&str>) -> Self {
        Self {
            outcome: ProbeOutcome::Found,
            version: version.and_then(plain_version),
            note: None,
        }
    }
}

/// Ask the service `kind` at `base` what it is.
pub(super) async fn probe(client: &Client, kind: SetupServiceKind, base: &str) -> FoundService {
    let verdict = match tokio::time::timeout(LAN_PROBE_TIMEOUT, ask(client, kind, base)).await {
        Ok(verdict) => verdict,
        Err(_) => Verdict::just(ProbeOutcome::NoAnswer),
    };
    FoundService {
        kind,
        url: base.to_owned(),
        outcome: verdict.outcome,
        version: verdict.version,
        note: verdict.note,
    }
}

/// The route asked, relative to the base.
fn route(kind: SetupServiceKind) -> &'static str {
    match kind {
        SetupServiceKind::Slskd => "api/v0/session/enabled",
        SetupServiceKind::Sabnzbd => "api?mode=version&output=json",
        SetupServiceKind::Nzbget => "",
        SetupServiceKind::Prowlarr | SetupServiceKind::Lidarr => "ping",
        SetupServiceKind::Jellyfin => "System/Info/Public",
        SetupServiceKind::Navidrome => "rest/ping?f=json",
        SetupServiceKind::Plex => "identity",
        SetupServiceKind::Deemix => "api/connect",
    }
}

async fn ask(client: &Client, kind: SetupServiceKind, base: &str) -> Verdict {
    let Some(url) = Url::parse(base)
        .ok()
        .filter(|url| url.scheme() == "http" || url.scheme() == "https")
        .and_then(|url| url.join(route(kind)).ok())
    else {
        return Verdict::just(ProbeOutcome::NotFound);
    };
    // A name goes through the client's resolver, which keeps only private
    // addresses; an address written out is held to the same rule here.
    let literal = url
        .host_str()
        .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
        .and_then(|host| host.parse::<IpAddr>().ok());
    if literal.is_some_and(|address| !keep_lan_address(address)) {
        return Verdict::just(ProbeOutcome::NotFound);
    }
    let response = match client
        .get(url)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return Verdict::just(match lan_probe_failure(&error) {
                LanProbeFailure::NotFound => ProbeOutcome::NotFound,
                LanProbeFailure::NoAnswer => ProbeOutcome::NoAnswer,
                LanProbeFailure::Other => ProbeOutcome::SomethingElse,
            });
        }
    };
    let status = response.status();
    if status.is_redirection() {
        return Verdict::just(ProbeOutcome::SomethingElse);
    }
    let server = response
        .headers()
        .get(header::SERVER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = match read_capped(response, ANSWER_CAP).await {
        Ok(body) => body,
        Err(BodyError::TooLarge) => return Verdict::just(ProbeOutcome::SomethingElse),
        Err(BodyError::Transport(_)) => return Verdict::just(ProbeOutcome::NoAnswer),
    };
    judge(kind, status, server.as_deref(), &body)
        .unwrap_or_else(|| Verdict::just(ProbeOutcome::SomethingElse))
}

/// Whether the answer is the service's own. `None` when it is not.
fn judge(
    kind: SetupServiceKind,
    status: StatusCode,
    server: Option<&str>,
    body: &[u8],
) -> Option<Verdict> {
    let ok = status == StatusCode::OK;
    match kind {
        SetupServiceKind::Slskd => {
            (ok && serde_json::from_slice::<bool>(body).is_ok()).then(|| Verdict::found(None))
        }
        SetupServiceKind::Sabnzbd => {
            if status == StatusCode::FORBIDDEN
                && String::from_utf8_lossy(body).contains(HOSTNAME_REFUSED)
            {
                return Some(Verdict {
                    note: Some(ServiceNote::SabnzbdHostNotWhitelisted),
                    ..Verdict::found(None)
                });
            }
            let answer: SabnzbdVersion = ok.then(|| serde_json::from_slice(body).ok())??;
            Some(Verdict::found(Some(&answer.version)))
        }
        SetupServiceKind::Nzbget => {
            let version = server?.strip_prefix("nzbget-")?;
            Some(Verdict::found(Some(version)))
        }
        SetupServiceKind::Prowlarr | SetupServiceKind::Lidarr => {
            let answer: Ping = ok.then(|| serde_json::from_slice(body).ok())??;
            (answer.status == "OK").then(|| Verdict::found(None))
        }
        SetupServiceKind::Jellyfin => {
            let answer: JellyfinInfo = ok.then(|| serde_json::from_slice(body).ok())??;
            Some(Verdict::found(Some(&answer.version)))
        }
        SetupServiceKind::Navidrome => {
            // Answered whatever the status: the Subsonic API reports a
            // missing sign-in inside a 200.
            let answer: SubsonicAnswer = serde_json::from_slice(body).ok()?;
            let inner = answer.response;
            (inner.server_type == "navidrome").then(|| {
                let version = inner.server_version.as_deref();
                Verdict::found(version.and_then(|text| text.split_whitespace().next()))
            })
        }
        SetupServiceKind::Plex => {
            let answer: PlexIdentity = ok.then(|| serde_json::from_slice(body).ok())??;
            Some(Verdict::found(Some(&answer.container.version)))
        }
        SetupServiceKind::Deemix => {
            let answer: DeemixConnect = ok.then(|| serde_json::from_slice(body).ok())??;
            Some(Verdict::found(Some(&answer.update.webui_version)))
        }
    }
}

/// `version` when it is short and plain enough to show.
fn plain_version(version: &str) -> Option<String> {
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-');
    let version = version.trim();
    (!version.is_empty() && version.len() <= VERSION_MAX && version.bytes().all(plain))
        .then(|| version.to_owned())
}

/// SABnzbd's `mode=version` answer.
#[derive(Deserialize)]
struct SabnzbdVersion {
    version: String,
}

/// Prowlarr's and Lidarr's `/ping`.
#[derive(Deserialize)]
struct Ping {
    status: String,
}

/// Jellyfin's `/System/Info/Public`, the two fields read.
#[derive(Deserialize)]
struct JellyfinInfo {
    #[serde(rename = "Id")]
    _id: String,
    #[serde(rename = "Version")]
    version: String,
}

/// A Subsonic API answer.
#[derive(Deserialize)]
struct SubsonicAnswer {
    #[serde(rename = "subsonic-response")]
    response: SubsonicResponse,
}

/// Navidrome names itself in every Subsonic answer, signed in or not.
#[derive(Deserialize)]
struct SubsonicResponse {
    #[serde(rename = "type")]
    server_type: String,
    #[serde(rename = "serverVersion", default)]
    server_version: Option<String>,
}

/// Plex's `/identity`, asked for as JSON.
#[derive(Deserialize)]
struct PlexIdentity {
    #[serde(rename = "MediaContainer")]
    container: PlexContainer,
}

#[derive(Deserialize)]
struct PlexContainer {
    #[serde(rename = "machineIdentifier")]
    _machine: String,
    version: String,
}

/// Deemix's `/api/connect`, its versions only. The rest of the answer,
/// the saved Deezer sign-in among it, is never decoded.
#[derive(Deserialize)]
struct DeemixConnect {
    update: DeemixVersions,
}

#[derive(Deserialize)]
struct DeemixVersions {
    #[serde(rename = "webuiVersion")]
    webui_version: String,
}
