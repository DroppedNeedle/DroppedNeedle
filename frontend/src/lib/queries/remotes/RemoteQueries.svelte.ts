import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import {
	REMOTE_ENDPOINTS,
	type RemoteAlbumParams,
	type RemoteArtistParams,
	type RemoteLyricsParams,
	type RemoteMixParams,
	type RemotePageParams,
	type RemoteRandomParams,
	type RemoteTrackParams
} from './endpoints';
import { RemoteQueryKeyFactory } from './RemoteQueryKeyFactory';
import type { RemoteAlbum, RemoteArtist, RemoteSource } from './types';

type Source = Getter<RemoteSource>;
type Enabled = Getter<boolean>;

const userId = () => authStore.user?.id;
const authed = (getEnabled: Enabled) => getEnabled() && !!userId();

// Each call site passes its own fetch closure so the typed client infers
// the response from that call's registry URL. Five reads stay on the
// untyped client because the contract spells no response schema for their
// routes (album/artist detail, genres, recent, recently-added); those name
// their contract-aliased payload type explicitly until the backend
// annotates the routes.
function query<T>(
	build: () => {
		key: readonly unknown[];
		fetch: (signal: AbortSignal) => Promise<T>;
		getEnabled: Enabled;
		extraGate?: boolean;
	}
) {
	return createQuery(() => {
		const { key, fetch, getEnabled, extraGate = true } = build();
		return {
			queryKey: key,
			queryFn: ({ signal }: { signal: AbortSignal }) => fetch(signal),
			enabled: authed(getEnabled) && extraGate
		};
	});
}

export const getRemoteHubQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.hub(userId(), getSource()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.hub(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteConnectionQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.connection(userId(), getSource()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.connection(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteAlbumsQuery = (
	getSource: Source,
	getParams: Getter<RemoteAlbumParams>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.albums(userId(), getSource(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.albums(getSource(), getParams()), { signal }),
		getEnabled
	}));

export const getRemoteAlbumDetailQuery = (
	getSource: Source,
	getId: Getter<string>,
	getEnabled: Enabled = () => true
) =>
	query<RemoteAlbum>(() => ({
		key: RemoteQueryKeyFactory.albumDetail(userId(), getSource(), getId()),
		fetch: (signal: AbortSignal) =>
			api.global.get<RemoteAlbum>(REMOTE_ENDPOINTS.album(getSource(), getId()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteAlbumTracksQuery = (
	getSource: Source,
	getId: Getter<string>,
	getParams: Getter<RemotePageParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.albumTracks(userId(), getSource(), getId(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.albumTracks(getSource(), getId(), getParams()), {
				signal
			}),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteArtistsQuery = (
	getSource: Source,
	getParams: Getter<RemoteArtistParams>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.artists(userId(), getSource(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.artists(getSource(), getParams()), { signal }),
		getEnabled
	}));

export const getRemoteArtistIndexQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.artistIndex(userId(), getSource()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.artistIndex(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteArtistDetailQuery = (
	getSource: Source,
	getId: Getter<string>,
	getEnabled: Enabled = () => true
) =>
	query<RemoteArtist>(() => ({
		key: RemoteQueryKeyFactory.artistDetail(userId(), getSource(), getId()),
		fetch: (signal: AbortSignal) =>
			api.global.get<RemoteArtist>(REMOTE_ENDPOINTS.artist(getSource(), getId()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteTracksQuery = (
	getSource: Source,
	getParams: Getter<RemoteTrackParams>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.tracks(userId(), getSource(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.tracks(getSource(), getParams()), { signal }),
		getEnabled
	}));

export const getRemoteSearchQuery = (
	getSource: Source,
	getText: Getter<string>,
	getLimit: Getter<number> = () => 20,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.search(userId(), getSource(), getText(), getLimit()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.search(getSource(), { q: getText(), limit: getLimit() }), {
				signal
			}),
		getEnabled,
		extraGate: getText().trim().length > 0
	}));

export const getRemoteFavoritesQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.favorites(userId(), getSource()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.favorites(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteRecentQuery = (
	getSource: Source,
	getParams: Getter<RemotePageParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query<RemoteAlbum[]>(() => ({
		key: RemoteQueryKeyFactory.recent(userId(), getSource(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.get<RemoteAlbum[]>(REMOTE_ENDPOINTS.recent(getSource(), getParams()), { signal }),
		getEnabled
	}));

export const getRemoteRecentlyAddedQuery = (
	getSource: Source,
	getParams: Getter<RemotePageParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query<RemoteAlbum[]>(() => ({
		key: RemoteQueryKeyFactory.recentlyAdded(userId(), getSource(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.get<RemoteAlbum[]>(REMOTE_ENDPOINTS.recentlyAdded(getSource(), getParams()), {
				signal
			}),
		getEnabled
	}));

export const getRemoteHistoryQuery = (
	getSource: Source,
	getParams: Getter<RemotePageParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.history(userId(), getSource(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.history(getSource(), getParams()), { signal }),
		getEnabled
	}));

export const getRemoteStatsQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.stats(userId(), getSource()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.stats(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteSessionsQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.sessions(userId(), getSource()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.sessions(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteGenresQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query<string[]>(() => ({
		key: RemoteQueryKeyFactory.genres(userId(), getSource()),
		fetch: (signal: AbortSignal) =>
			api.global.get<string[]>(REMOTE_ENDPOINTS.genres(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteGenreSongsQuery = (
	getSource: Source,
	getGenre: Getter<string>,
	getParams: Getter<RemotePageParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.genreSongs(userId(), getSource(), getGenre(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.genreSongs(getSource(), getGenre(), getParams()), {
				signal
			}),
		getEnabled,
		extraGate: getGenre().length > 0
	}));

export const getRemotePlaylistsQuery = (
	getSource: Source,
	getParams: Getter<RemotePageParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.playlists(userId(), getSource(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.playlists(getSource(), getParams()), { signal }),
		getEnabled
	}));

export const getRemotePlaylistDetailQuery = (
	getSource: Source,
	getId: Getter<string>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.playlistDetail(userId(), getSource(), getId()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.playlist(getSource(), getId()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteInfoAlbumQuery = (
	getSource: Source,
	getId: Getter<string>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.infoAlbum(userId(), getSource(), getId()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.infoAlbum(getSource(), getId()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteInfoArtistQuery = (
	getSource: Source,
	getId: Getter<string>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.infoArtist(userId(), getSource(), getId()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.infoArtist(getSource(), getId()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteLyricsQuery = (
	getSource: Source,
	getId: Getter<string>,
	getParams: Getter<RemoteLyricsParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.lyrics(userId(), getSource(), getId(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.lyrics(getSource(), getId(), getParams()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteMatchQuery = (
	getSource: Source,
	getMbid: Getter<string>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.match(userId(), getSource(), getMbid()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.match(getSource(), getMbid()), { signal }),
		getEnabled,
		extraGate: getMbid().length > 0
	}));

export const getRemoteMixQuery = (
	getSource: Source,
	getId: Getter<string>,
	getParams: Getter<RemoteMixParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.mix(userId(), getSource(), getId(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.mix(getSource(), getId(), getParams()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteSimilarQuery = (
	getSource: Source,
	getId: Getter<string>,
	getParams: Getter<RemotePageParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.similar(userId(), getSource(), getId(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.similar(getSource(), getId(), getParams()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteTopQuery = (
	getSource: Source,
	getArtist: Getter<string>,
	getLimit: Getter<number | undefined> = () => undefined,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.top(userId(), getSource(), getArtist(), getLimit()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.top(getSource(), getArtist(), getLimit()), { signal }),
		getEnabled,
		extraGate: getArtist().length > 0
	}));

export const getRemoteRandomQuery = (
	getSource: Source,
	getParams: Getter<RemoteRandomParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.random(userId(), getSource(), getParams()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.random(getSource(), getParams()), { signal }),
		getEnabled
	}));

export const getRemoteDiscoveryQuery = (
	getSource: Source,
	getCount: Getter<number | undefined> = () => undefined,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.discovery(userId(), getSource(), getCount()),
		fetch: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.discovery(getSource(), getCount()), { signal }),
		getEnabled
	}));

export const getRemoteFoldersQuery = (getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.folders(userId()),
		fetch: (signal: AbortSignal) => api.global.v3.GET(REMOTE_ENDPOINTS.folders(), { signal }),
		getEnabled
	}));

// Byte routes are plain URLs for <img> tags, not queries.
export const remoteImageUrl = (source: RemoteSource, id: string, size?: number): string =>
	REMOTE_ENDPOINTS.image(source, id, size);

export const remotePlaylistCoverUrl = (source: RemoteSource, id: string, size?: number): string =>
	REMOTE_ENDPOINTS.playlistCover(source, id, size);
