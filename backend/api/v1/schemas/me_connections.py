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
    primary_music_source: str = "listenbrainz"
    # now-playing presence visibility to other users
    now_playing_visibility: str = "full"


class ScrobblePreferencesUpdate(AppStruct):
    scrobble_to_lastfm: bool | None = None
    scrobble_to_listenbrainz: bool | None = None
    primary_music_source: Literal["listenbrainz", "lastfm"] | None = None
    now_playing_visibility: Literal["full", "track_hidden", "offline"] | None = None


class ListenBrainzConnectRequest(AppStruct):
    user_token: str
    username: str = ""
