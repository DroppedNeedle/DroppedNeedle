import { api } from '$lib/api/client';
import {
	REMOTE_ENDPOINTS,
	type RemoteAlbumParams,
	type RemoteArtistParams,
	type RemoteLyricsParams,
	type RemoteMatchHints,
	type RemotePageParams,
	type RemoteTrackParams
} from './endpoints';
import type { RemoteSource } from './types';

type Signal = AbortSignal | undefined;

export const remoteApi = {
	albums: (source: RemoteSource, params: RemoteAlbumParams, signal?: Signal) =>
		api.global.v3.GET(REMOTE_ENDPOINTS.albums(source, params), { signal }),
	albumTracks: (source: RemoteSource, id: string, signal?: Signal) =>
		api.global.v3.GET(REMOTE_ENDPOINTS.albumTracks(source, id), { signal }),
	artists: (source: RemoteSource, params: RemoteArtistParams, signal?: Signal) =>
		api.global.v3.GET(REMOTE_ENDPOINTS.artists(source, params), { signal }),
	tracks: (source: RemoteSource, params: RemoteTrackParams, signal?: Signal) =>
		api.global.v3.GET(REMOTE_ENDPOINTS.tracks(source, params), { signal }),
	stats: (source: RemoteSource, signal?: Signal) =>
		api.global.v3.GET(REMOTE_ENDPOINTS.stats(source), { signal }),
	genres: (source: RemoteSource, signal?: Signal) =>
		api.global.v3.GET(REMOTE_ENDPOINTS.genres(source), { signal }),
	lyrics: (source: RemoteSource, id: string, params: RemoteLyricsParams, signal?: Signal) =>
		api.global.v3.GET(REMOTE_ENDPOINTS.lyrics(source, id, params), { signal }),
	match: (source: RemoteSource, mbid: string, hints: RemoteMatchHints, signal?: Signal) =>
		api.global.v3.GET(REMOTE_ENDPOINTS.match(source, mbid, hints), { signal }),
	recent: (source: RemoteSource, params: RemotePageParams, signal?: Signal) =>
		api.global.v3.GET(REMOTE_ENDPOINTS.recent(source, params), { signal })
} as const;
