from typing import Literal

from infrastructure.msgspec_fastapi import AppStruct


class ConnectionStatus(AppStruct):
    # never carries the encrypted secret - only username + enabled flag
    service: str
    enabled: bool = False
    username: str = ""


class ConnectionsResponse(AppStruct):
    connections: list[ConnectionStatus] = []


class ConnectionActionResponse(AppStruct):
    service: str
    deleted: bool


class ScrobblePreferences(AppStruct):
    scrobble_to_lastfm: bool = False
    scrobble_to_listenbrainz: bool = False
    navidrome_handles_external_scrobbles: bool = True
    primary_music_source: str = "listenbrainz"
    # now-playing presence visibility to other users
    now_playing_visibility: str = "full"
    auto_request_personal_mix: bool = False
    # standing-grant state for auto-request (none|pending|approved|rejected|revoked);
    # admins read 'approved' by role whenever the toggle is on
    auto_request_state: str = "none"


class ScrobblePreferencesUpdate(AppStruct):
    scrobble_to_lastfm: bool | None = None
    scrobble_to_listenbrainz: bool | None = None
    navidrome_handles_external_scrobbles: bool | None = None
    primary_music_source: Literal["listenbrainz", "lastfm"] | None = None
    now_playing_visibility: Literal["full", "track_hidden", "offline"] | None = None
    auto_request_personal_mix: bool | None = None


class PersonalMixRefreshResponse(AppStruct):
    # the build runs in the background; the outcome arrives on the per-user SSE
    # stream as a personal_mix_refreshed event
    status: str = "started"  # started | already_running


class ListenBrainzConnectRequest(AppStruct):
    user_token: str
    username: str = ""


class SpotifyAuthUrlResponse(AppStruct):
    auth_url: str


class MediaServerConnectRequest(AppStruct):
    """Link a Navidrome/Jellyfin account with the user's own credentials.

    The password is verified against the live server and either stored encrypted
    (Navidrome - Subsonic token auth needs it per request) or exchanged for an
    access token and discarded (Jellyfin). It is never serialized back out.
    """

    username: str
    password: str


class PlexLinkPinResponse(AppStruct):
    pin_id: int
    auth_url: str


class PlexLinkPollResponse(AppStruct):
    completed: bool = False
    # set once completed: the linked Plex account's display name
    username: str = ""
