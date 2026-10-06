import type { components } from '$lib/api/v3/openapi';

export type RemoteSource = components['schemas']['SourceName'];
export type RemoteHub = components['schemas']['HubView'];
export type RemoteAlbumPage = components['schemas']['RemotesAlbumPage'];
export type RemoteAlbum = components['schemas']['RemotesAlbumView'];
export type RemoteArtistPage = components['schemas']['RemotesArtistPage'];
export type RemoteArtist = components['schemas']['RemotesArtistView'];
export type RemoteTrackPage = components['schemas']['RemotesTrackPage'];
export type RemoteTrack = components['schemas']['RemotesTrackView'];
export type RemoteStats = components['schemas']['RemotesStatsView'];
export type RemoteSessions = components['schemas']['SessionsView'];
export type RemoteLyrics = components['schemas']['RemotesLyricsView'];
export type RemoteMatch = components['schemas']['MatchView'];
export type RemoteFolders = components['schemas']['FolderResolutionView'];
export type RemoteFolderSave = components['schemas']['FolderSave'];
