import { api } from '$lib/api/client';
import {
	REMOTE_ENDPOINTS,
	type RemoteAlbumParams,
	type RemoteArtistParams,
	type RemoteLyricsParams,
	type RemotePageParams,
	type RemoteTrackParams
} from './endpoints';
import type {
	RemoteAlbum,
	RemoteAlbumPage,
	RemoteArtistPage,
	RemoteLyrics,
	RemoteMatch,
	RemoteSource,
	RemoteStats,
	RemoteTrackPage
} from './types';

// Remote reads the typed client cannot infer correctly yet: the contract
// reuses these operation ids for the library and auth routes (so
// openapi.d.ts declares them twice and the first, non-remote shape wins),
// or declares no response body. Each read names its remotes schema
// explicitly; switch them to `api.global.v3.GET` once the ids are unique.
type Signal = AbortSignal | undefined;

export const remoteApi = {
	albums: (source: RemoteSource, params: RemoteAlbumParams, signal?: Signal) =>
		api.global.get<RemoteAlbumPage>(REMOTE_ENDPOINTS.albums(source, params), { signal }),
	albumTracks: (source: RemoteSource, id: string, params: RemotePageParams, signal?: Signal) =>
		api.global.get<RemoteTrackPage>(REMOTE_ENDPOINTS.albumTracks(source, id, params), {
			signal
		}),
	artists: (source: RemoteSource, params: RemoteArtistParams, signal?: Signal) =>
		api.global.get<RemoteArtistPage>(REMOTE_ENDPOINTS.artists(source, params), { signal }),
	tracks: (source: RemoteSource, params: RemoteTrackParams, signal?: Signal) =>
		api.global.get<RemoteTrackPage>(REMOTE_ENDPOINTS.tracks(source, params), { signal }),
	stats: (source: RemoteSource, signal?: Signal) =>
		api.global.get<RemoteStats>(REMOTE_ENDPOINTS.stats(source), { signal }),
	genres: (source: RemoteSource, signal?: Signal) =>
		api.global.get<string[]>(REMOTE_ENDPOINTS.genres(source), { signal }),
	lyrics: (source: RemoteSource, id: string, params: RemoteLyricsParams, signal?: Signal) =>
		api.global.get<RemoteLyrics>(REMOTE_ENDPOINTS.lyrics(source, id, params), { signal }),
	match: (source: RemoteSource, mbid: string, signal?: Signal) =>
		api.global.get<RemoteMatch>(REMOTE_ENDPOINTS.match(source, mbid), { signal }),
	recent: (source: RemoteSource, params: RemotePageParams, signal?: Signal) =>
		api.global.get<RemoteAlbum[]>(REMOTE_ENDPOINTS.recent(source, params), { signal })
} as const;
