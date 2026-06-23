import {
	createInfiniteQuery,
	createQuery,
	keepPreviousData,
	queryOptions
} from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { API, CACHE_TTL } from '$lib/constants';
import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import type {
	Album,
	AlbumSort,
	AlbumTracksInfo,
	ArtistSort,
	LibraryAlbumStatus,
	LibraryAlbumSummary,
	LibraryArtistSummary,
	LibraryScanSchedule,
	LibraryScanStatus,
	LibrarySettings,
	LibraryStats,
	LibraryUnmatchedResponse,
	NativeAlbumsResponse,
	NativeArtistsResponse,
	NativeTrackListItem,
	NativeTrackPage
} from '$lib/types';

export interface LibraryAlbumsParams {
	page: number;
	sort: AlbumSort;
	q: string;
	format: string;
}

export const getLibraryAlbumsQueryOptions = ({ page, sort, q, format }: LibraryAlbumsParams) =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.albums(page, sort, q, format),
		queryFn: ({ signal }) =>
			api.global.get<NativeAlbumsResponse>(
				API.library.albums(page, sort, q || undefined, format || undefined),
				{ signal }
			)
	});

export const getLibraryAlbumsQuery = (getParams: Getter<LibraryAlbumsParams>) =>
	createQuery(() => getLibraryAlbumsQueryOptions(getParams()));

export interface LibraryArtistsParams {
	sortBy: ArtistSort;
	sortOrder: 'asc' | 'desc';
	q: string;
}

const ARTISTS_PAGE_SIZE = 48;

export const getLibraryArtistsInfiniteQuery = (getParams: Getter<LibraryArtistsParams>) =>
	createInfiniteQuery(() => {
		const { sortBy, sortOrder, q } = getParams();
		return {
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.artists(sortBy, sortOrder, q),
			initialPageParam: 0,
			queryFn: ({ pageParam = 0, signal }) =>
				api.global.get<NativeArtistsResponse>(
					API.library.artists(ARTISTS_PAGE_SIZE, pageParam, sortBy, sortOrder, q || undefined),
					{ signal }
				),
			getNextPageParam: (lastPage: NativeArtistsResponse, allPages: NativeArtistsResponse[]) => {
				const loaded = allPages.reduce((n, p) => n + p.items.length, 0);
				return loaded < lastPage.total ? loaded : undefined;
			}
		};
	});

// separate from the paginated browse query so the hub avoids pulling a full 48-item page for a few thumbnails
const ARTIST_THUMBS_LIMIT = 12;

export const getLibraryArtistThumbsQuery = () =>
	createQuery(() => ({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.artistThumbs(),
		queryFn: ({ signal }) =>
			api.global.get<NativeArtistsResponse>(
				API.library.artists(ARTIST_THUMBS_LIMIT, 0, 'album_count', 'desc'),
				{ signal }
			)
	}));

export const getLibraryStatsQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.stats(),
		queryFn: ({ signal }) => api.global.get<LibraryStats>(API.library.stats(), { signal })
	});

export const getLibraryStatsQuery = () => createQuery(() => getLibraryStatsQueryOptions());

// schedule route is admin-gated; pass `enabled` to keep it off for non-admins
export const getLibraryScanScheduleQuery = (enabled: () => boolean = () => true) =>
	createQuery(() => ({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.scanSchedule(),
		queryFn: ({ signal }) =>
			api.global.get<LibraryScanSchedule>(API.library.scanSchedule(), { signal })
	}));

export const getLibraryAlbumStatusQueryOptions = (mbid: string) =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.album(mbid),
		queryFn: ({ signal }) => api.global.get<LibraryAlbumStatus>(API.library.album(mbid), { signal })
	});

export const getLibraryAlbumStatusQuery = (getMbid: Getter<string>) =>
	createQuery(() => getLibraryAlbumStatusQueryOptions(getMbid()));

// fast-poll only while scanning; when idle keep a lazy poll so a scheduled or
// external scan still surfaces without hammering scan/status every 2s. An
// in-page scan stays instant: the scan mutation invalidates and re-fetches this.
export const getLibraryScanStatusQuery = () =>
	createQuery(() => ({
		staleTime: CACHE_TTL.SCAN_STATUS,
		refetchInterval: (query: { state: { data?: LibraryScanStatus | undefined } }) =>
			query.state.data?.status === 'scanning'
				? CACHE_TTL.SCAN_STATUS
				: CACHE_TTL.SCAN_STATUS_IDLE,
		queryKey: LibraryQueryKeyFactory.scanStatus(),
		queryFn: ({ signal }) => api.global.get<LibraryScanStatus>(API.library.scanStatus(), { signal })
	}));

export const getLibraryUnmatchedQuery = () =>
	createQuery(() => ({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.unmatched(),
		queryFn: ({ signal }) =>
			api.global.get<LibraryUnmatchedResponse>(API.library.unmatched(), { signal })
	}));

interface LibrarySearchResults {
	albums: LibraryAlbumSummary[];
	artists: LibraryArtistSummary[];
	tracks: NativeTrackListItem[];
}

const LIBRARY_SEARCH_LIMIT = 6;

// fans out to album/artist/track endpoints in parallel since there's no combined endpoint; keepPreviousData avoids flashing empty mid-flight
export const getLibrarySearchQuery = (getTerm: Getter<string>) =>
	createQuery(() => {
		const term = getTerm().trim();
		return {
			enabled: term.length >= 2,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			placeholderData: keepPreviousData,
			queryKey: LibraryQueryKeyFactory.search(term),
			queryFn: async ({ signal }): Promise<LibrarySearchResults> => {
				const [albums, artists, tracks] = await Promise.all([
					api.global.get<NativeAlbumsResponse>(
						API.library.albums(1, 'recent', term, undefined, LIBRARY_SEARCH_LIMIT),
						{ signal }
					),
					api.global.get<NativeArtistsResponse>(
						API.library.artists(LIBRARY_SEARCH_LIMIT, 0, 'name', 'asc', term),
						{ signal }
					),
					api.global.get<NativeTrackPage>(
						API.library.tracks(LIBRARY_SEARCH_LIMIT, 0, 'recent', term),
						{ signal }
					)
				]);
				return { albums: albums.items, artists: artists.items, tracks: tracks.items };
			}
		};
	});

export const getAlbumSearchQuery = (getTerm: Getter<string>) =>
	createQuery(() => {
		const term = getTerm().trim();
		return {
			enabled: term.length >= 2,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.albumSearch(term),
			queryFn: async ({ signal }) => {
				const data = await api.global.get<{ results?: Album[] }>(API.search.albums(term), {
					signal
				});
				return data.results ?? [];
			}
		};
	});

export const getAlbumTracksQuery = (getMbid: Getter<string | null>) =>
	createQuery(() => {
		const mbid = getMbid();
		return {
			enabled: !!mbid,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.albumTracks(mbid ?? ''),
			queryFn: ({ signal }) =>
				api.global.get<AlbumTracksInfo>(API.album.tracks(mbid ?? ''), { signal })
		};
	});

// GET /settings/library is admin-gated; pass `enabled` to keep it off for non-admins so it never fires a 403
export const getLibrarySettingsQuery = (enabled: () => boolean = () => true) =>
	createQuery(() => ({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.settings(),
		queryFn: ({ signal }) => api.global.get<LibrarySettings>(API.library.settings(), { signal })
	}));
