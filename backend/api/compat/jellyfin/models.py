"""Jellyfin PascalCase response structs (reference/jellyfin-music-api.md)."""

from __future__ import annotations

import hashlib
from typing import Any

import msgspec

# Deterministic so it survives restarts without prefs; clients key persistent
# state by it (reference s2.3).
SERVER_ID = hashlib.sha256(b"droppedneedle-jellyfin-server").hexdigest()[:32]


class AuthenticateRequest(msgspec.Struct, kw_only=True):
    Username: str = ""
    Pw: str = ""


class PlaybackInfoBody(msgspec.Struct, kw_only=True):
    MaxStreamingBitrate: int | None = None
    StartTimeTicks: int | None = None
    UserId: str | None = None


class CreatePlaylistDto(msgspec.Struct, kw_only=True):
    Name: str = ""
    Ids: list[str] = []
    IsPublic: bool = False
    UserId: str | None = None


class PlaybackStartInfo(msgspec.Struct, kw_only=True):
    ItemId: str | None = None
    PlaySessionId: str | None = None


class PlaybackStopInfo(msgspec.Struct, kw_only=True):
    ItemId: str | None = None
    PlaySessionId: str | None = None
    PositionTicks: int | None = None
    RunTimeTicks: int | None = None
    Failed: bool = False


class PlaybackProgressInfo(msgspec.Struct, kw_only=True):
    ItemId: str | None = None
    PlaySessionId: str | None = None
    PositionTicks: int | None = None
    RunTimeTicks: int | None = None
    IsPaused: bool = False


class PublicSystemInfo(msgspec.Struct, kw_only=True):
    LocalAddress: str
    ServerName: str
    Version: str
    ProductName: str = "Jellyfin Server"
    OperatingSystem: str = ""
    Id: str = SERVER_ID
    StartupWizardCompleted: bool = True


class SystemInfo(msgspec.Struct, kw_only=True):
    LocalAddress: str
    ServerName: str
    Version: str
    ProductName: str = "Jellyfin Server"
    OperatingSystem: str = ""
    Id: str = SERVER_ID
    StartupWizardCompleted: bool = True
    HasPendingRestart: bool = False
    IsShuttingDown: bool = False
    SupportsLibraryMonitor: bool = True


class UserConfiguration(msgspec.Struct, kw_only=True):
    """Real Jellyfin sends this fully populated; strict clients hard-cast the
    bools, so an empty {} crashes them (Finamp: 'Null is not a subtype of bool',
    issue #144). Fields/defaults verified against jellyfin-sdk-typescript
    user-configuration.ts and Finamp lib/models/jellyfin_models.dart (its 9
    required fields are all non-optional here)."""

    AudioLanguagePreference: str | None = None
    PlayDefaultAudioTrack: bool = True
    SubtitleLanguagePreference: str | None = None
    DisplayMissingEpisodes: bool = False
    GroupedFolders: list[str] = []
    SubtitleMode: str = "Default"
    DisplayCollectionsView: bool = False
    EnableLocalPassword: bool = False
    OrderedViews: list[str] = []
    LatestItemsExcludes: list[str] = []
    MyMediaExcludes: list[str] = []
    HidePlayedInLatest: bool = True
    RememberAudioSelections: bool = True
    RememberSubtitleSelections: bool = True
    EnableNextEpisodeAutoPlay: bool = True


class SessionInfo(msgspec.Struct, kw_only=True):
    """Minimal session object for AuthenticationResult. Finamp requires UserId,
    LastActivityDate and the four bools non-null when SessionInfo is present
    (jellyfin_models.dart), so an empty {} crashes it (issue #144)."""

    Id: str
    UserId: str
    UserName: str
    LastActivityDate: str
    Client: str | None = None
    DeviceName: str = ""
    DeviceId: str | None = None
    IsActive: bool = True
    SupportsRemoteControl: bool = False
    SupportsMediaControl: bool = False
    HasCustomDeviceName: bool = False
    ServerId: str = SERVER_ID


class UserDto(msgspec.Struct, kw_only=True):
    Id: str
    Name: str
    ServerId: str = SERVER_ID
    HasPassword: bool = True
    HasConfiguredPassword: bool = True
    # deprecated upstream but still sent by real servers; Finamp requires it non-null
    HasConfiguredEasyPassword: bool = False
    Configuration: UserConfiguration = msgspec.field(default_factory=UserConfiguration)
    Policy: dict[str, Any] = {}


class AuthenticationResult(msgspec.Struct, kw_only=True):
    User: UserDto
    AccessToken: str
    SessionInfo: SessionInfo
    ServerId: str = SERVER_ID


class NameGuidPair(msgspec.Struct, kw_only=True):
    Name: str
    Id: str


class UserItemDataDto(msgspec.Struct, kw_only=True):
    ItemId: str
    Key: str
    PlaybackPositionTicks: int = 0
    PlayCount: int = 0
    IsFavorite: bool = False
    Played: bool = False
    LastPlayedDate: str | None = None
    Rating: float | None = None
    PlayedPercentage: float | None = None


class BaseItemDto(msgspec.Struct, kw_only=True):
    Id: str
    Name: str
    Type: str
    ServerId: str = SERVER_ID
    IsFolder: bool = False
    MediaType: str = "Unknown"
    RunTimeTicks: int | None = None
    ProductionYear: int | None = None
    IndexNumber: int | None = None         # track number
    ParentIndexNumber: int | None = None   # disc number
    Album: str | None = None
    AlbumId: str | None = None
    AlbumArtist: str | None = None
    AlbumArtists: list[NameGuidPair] | None = None
    ArtistItems: list[NameGuidPair] | None = None
    Artists: list[str] | None = None
    AlbumPrimaryImageTag: str | None = None
    ImageTags: dict[str, str] = {}
    ParentId: str | None = None
    Genres: list[str] | None = None
    Container: str | None = None
    ChildCount: int | None = None
    CollectionType: str | None = None
    SortName: str | None = None
    DateCreated: str | None = None
    ProviderIds: dict[str, str] | None = None
    UserData: UserItemDataDto | None = None
    PlaylistItemId: str | None = None      # only on playlist members
    # Real Jellyfin sets these non-null on every item (DtoService.AttachBasicFields);
    # strict clients (Manet, Swift Codable) require them, so default them (_strip_none
    # keeps "FileSystem"/{}/[]).
    LocationType: str = "FileSystem"
    BackdropImageTags: list[str] = []
    ImageBlurHashes: dict[str, dict[str, str]] = {}


class BaseItemDtoQueryResult(msgspec.Struct, kw_only=True):
    Items: list[BaseItemDto] = []
    TotalRecordCount: int = 0
    StartIndex: int = 0


class MediaStream(msgspec.Struct, kw_only=True):
    Type: str = "Audio"
    Codec: str | None = None
    Index: int = 0
    BitRate: int | None = None
    Channels: int = 2
    ChannelLayout: str = "stereo"
    SampleRate: int | None = None
    BitDepth: int | None = None
    IsDefault: bool = True


class MediaSourceInfo(msgspec.Struct, kw_only=True):
    Id: str
    Protocol: str = "File"
    Container: str | None = None
    Size: int | None = None
    Bitrate: int | None = None
    RunTimeTicks: int | None = None
    SupportsDirectPlay: bool = True
    SupportsDirectStream: bool = True
    SupportsTranscoding: bool = False
    DefaultAudioStreamIndex: int = 0
    MediaStreams: list[MediaStream] = []
    Name: str | None = None
    IsRemote: bool = False
    # api_key must be embedded in the URL: clients (Finamp, Jellify, Manet) fetch it
    # without auth headers, so the stream 401s otherwise.
    DirectStreamUrl: str | None = None
    TranscodingUrl: str | None = None
    TranscodingSubProtocol: str | None = None
    TranscodingContainer: str | None = None


class PlaybackInfoResponse(msgspec.Struct, kw_only=True):
    MediaSources: list[MediaSourceInfo] = []
    PlaySessionId: str
    ErrorCode: str | None = None
