import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api, ApiError } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import {
	REMOTE_ENDPOINTS,
	type RemoteAlbumParams,
	type RemoteArtistParams,
	type RemoteMixParams,
	type RemotePageParams,
	type RemoteRandomParams,
	type RemoteTrackParams
} from './endpoints';
import { invalidateQueriesWithPersister, queryClient } from '../QueryClient';
import { RemoteQueryKeyFactory } from './RemoteQueryKeyFactory';
import { remoteApi } from './remoteApi';
import type { RemoteAlbumPage, RemoteFolders, RemoteSource, RemoteTrack } from './types';

type Source = Getter<RemoteSource>;
type Enabled = Getter<boolean>;

const userId = () => authStore.user?.id;
const authed = (getEnabled: Enabled) => getEnabled() && !!userId();

// Each call site passes its own load closure so the typed client infers
// the response from that call's registry URL; reads shared with the screens
// live in remoteApi.
function query<T>(
	build: () => {
		key: readonly unknown[];
		load: (signal: AbortSignal) => Promise<T>;
		getEnabled: Enabled;
		extraGate?: boolean;
	}
) {
	return createQuery(() => {
		const { key, load, getEnabled, extraGate = true } = build();
		return {
			queryKey: key,
			queryFn: ({ signal }: { signal: AbortSignal }) => load(signal),
			enabled: authed(getEnabled) && extraGate
		};
	});
}

export const getRemoteHubQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.hub(userId(), getSource()),
		load: (signal: AbortSignal) => api.global.v3.GET(REMOTE_ENDPOINTS.hub(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteConnectionQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.connection(userId(), getSource()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.connection(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteArtistIndexQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.artistIndex(userId(), getSource()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.artistIndex(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteFavoritesQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.favorites(userId(), getSource()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.favorites(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteHistoryQuery = (
	getSource: Source,
	getParams: Getter<RemotePageParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.history(userId(), getSource(), getParams()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.history(getSource(), getParams()), { signal }),
		getEnabled
	}));

export const getRemoteStatsQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.stats(userId(), getSource()),
		load: (signal: AbortSignal) => remoteApi.stats(getSource(), signal),
		getEnabled
	}));

export const getRemoteInfoArtistQuery = (
	getSource: Source,
	getId: Getter<string>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.infoArtist(userId(), getSource(), getId()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.infoArtist(getSource(), getId()), { signal }),
		getEnabled,
		extraGate: getId().length > 0
	}));

export const getRemoteMixQuery = (
	getSource: Source,
	getId: Getter<string>,
	getParams: Getter<RemoteMixParams> = () => ({}),
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.mix(userId(), getSource(), getId(), getParams()),
		load: (signal: AbortSignal) =>
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
		load: (signal: AbortSignal) =>
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
		load: (signal: AbortSignal) =>
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
		load: (signal: AbortSignal) =>
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
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.discovery(getSource(), getCount()), { signal }),
		getEnabled
	}));

export const getRemoteFoldersQuery = (getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.folders(userId()),
		load: (signal: AbortSignal) => api.global.v3.GET(REMOTE_ENDPOINTS.folders(), { signal }),
		getEnabled
	}));

export const getRemoteRecentQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.recent(userId(), getSource()),
		load: (signal: AbortSignal) => remoteApi.recent(getSource(), {}, signal),
		getEnabled
	}));

export const getRemoteGenresQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.genres(userId(), getSource()),
		load: (signal: AbortSignal) => remoteApi.genres(getSource(), signal),
		getEnabled
	}));

export const getRemoteMoodsQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.moods(userId(), getSource()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.moods(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteFiltersQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.filters(userId(), getSource()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.filters(getSource()), { signal }),
		getEnabled
	}));

export const getRemoteAnalyticsQuery = (getSource: Source, getEnabled: Enabled = () => true) =>
	query(() => ({
		key: RemoteQueryKeyFactory.analytics(userId(), getSource()),
		load: (signal: AbortSignal) =>
			api.global.v3.GET(REMOTE_ENDPOINTS.analytics(getSource()), { signal }),
		getEnabled
	}));

/** One page of a server's artists. */
export const getRemoteArtistsQuery = (
	getSource: Source,
	getParams: Getter<RemoteArtistParams>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.artists(userId(), getSource(), getParams()),
		load: (signal: AbortSignal) => remoteApi.artists(getSource(), getParams(), signal),
		getEnabled
	}));

/** One page of a server's tracks. */
export const getRemoteTracksQuery = (
	getSource: Source,
	getParams: Getter<RemoteTrackParams>,
	getEnabled: Enabled = () => true
) =>
	query(() => ({
		key: RemoteQueryKeyFactory.tracks(userId(), getSource(), getParams()),
		load: (signal: AbortSignal) => remoteApi.tracks(getSource(), getParams(), signal),
		getEnabled
	}));

export const REMOTE_ALBUMS_PAGE_SIZE = 48;

/**
 * A server's albums a page at a time. A search answers in one page from the
 * server's search, which takes no sort or filter.
 */
export const getRemoteAlbumsInfiniteQuery = (
	getSource: Source,
	getParams: Getter<RemoteAlbumParams>,
	getSearch: Getter<string>
) =>
	createInfiniteQuery(() => {
		const source = getSource();
		const params = getParams();
		const search = getSearch().trim();
		return {
			queryKey: RemoteQueryKeyFactory.albums(userId(), source, params, search),
			initialPageParam: 0,
			queryFn: async ({
				pageParam,
				signal
			}: {
				pageParam: number;
				signal: AbortSignal;
			}): Promise<RemoteAlbumPage> => {
				if (search) {
					const found = await api.global.v3.GET(REMOTE_ENDPOINTS.search(source, { q: search }), {
						signal
					});
					return { items: found.albums, total: found.albums.length, limit: 0, offset: 0 };
				}
				return remoteApi.albums(
					source,
					{ ...params, limit: REMOTE_ALBUMS_PAGE_SIZE, offset: pageParam },
					signal
				);
			},
			getNextPageParam: (last: RemoteAlbumPage, pages: RemoteAlbumPage[]) => {
				const loaded = pages.reduce((n, page) => n + page.items.length, 0);
				return last.items.length > 0 && loaded < last.total ? loaded : undefined;
			},
			enabled: !!userId()
		};
	});

/** An album's tracks, for playing or queueing it from a list. */
export const fetchRemoteAlbumTracks = (source: RemoteSource, id: string) =>
	queryClient.fetchQuery({
		queryKey: RemoteQueryKeyFactory.albumTracks(userId(), source, id),
		queryFn: ({ signal }) => remoteApi.albumTracks(source, id, signal)
	});

/** A further page of tracks, for playing a whole list in the background. */
export const fetchRemoteTracksPage = (
	source: RemoteSource,
	params: RemoteTrackParams,
	signal: AbortSignal
) =>
	queryClient.fetchQuery({
		queryKey: RemoteQueryKeyFactory.tracks(userId(), source, params),
		queryFn: () => remoteApi.tracks(source, params, signal)
	});

/**
 * Tracks of the chosen genres. A server that browses one genre at a time
 * reads only the first.
 */
export async function fetchRemoteGenreSongs(
	source: RemoteSource,
	genres: string[],
	multi: boolean,
	params: RemotePageParams
): Promise<RemoteTrack[]> {
	const chosen = multi ? genres : genres.slice(0, 1);
	if (chosen.length === 0) return [];
	const page = await queryClient.fetchQuery({
		queryKey: RemoteQueryKeyFactory.genreSongs(userId(), source, chosen, params),
		queryFn: ({ signal }) =>
			api.global.v3.GET(
				multi
					? REMOTE_ENDPOINTS.genresSongs(source, chosen, params)
					: REMOTE_ENDPOINTS.genreSongs(source, chosen[0], params),
				{ signal }
			)
	});
	return page.items;
}

/** The error's code from the server, or `fallback` when it has none. */
export function remoteErrorCode(error: unknown, fallback: string): string {
	return error instanceof ApiError ? error.code || fallback : fallback;
}

/**
 * The Navidrome folder choice as last read, for keys that must change with
 * it; 'unresolved' until the folders have been read in this session.
 */
export function navidromeFolderScope(userId: string | null | undefined): string {
	const folders = queryClient.getQueryData<RemoteFolders>(RemoteQueryKeyFactory.folders(userId));
	return folders ? `${folders.mode}:${[...folders.folder_ids].sort().join(',')}` : 'unresolved';
}

/** Reloads everything shown for one server, for a page's refresh button. */
export const refreshRemoteSource = (source: RemoteSource) =>
	invalidateQueriesWithPersister({ queryKey: RemoteQueryKeyFactory.source(userId(), source) });
