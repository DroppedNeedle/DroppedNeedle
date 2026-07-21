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
	LibraryAlbumDetail,
	LibraryAlbumSummary,
	LibraryArtistSummary,
	LibraryScanSchedule,
	LibraryStats,
	LibraryMembershipResponse,
	NativeAlbumsResponse,
	NativeArtistsResponse,
	NativeTrackListItem,
	NativeTrackPage
} from '$lib/types';
import { authStore } from '$lib/stores/authStore.svelte';

export interface LibraryAlbumsParams {
	page: number;
	sort: AlbumSort;
	q: string;
	format: string;
}

export const getLibraryMembershipQueryOptions = (
	userId: string | undefined,
	identifiers: string[]
) => {
	const albumIds = identifiers
		.map((id) => id.trim().toLowerCase())
		.filter((id, index, allIds) => Boolean(id) && allIds.indexOf(id) === index)
		.sort();
	return queryOptions({
		enabled: Boolean(userId && albumIds.length),
		staleTime: 30_000,
		queryKey: LibraryQueryKeyFactory.membership(userId, albumIds),
		queryFn: async ({ signal }) => {
			let ownedIds: string[] = [];
			let requestedIds: string[] = [];
			for (let offset = 0; offset < albumIds.length; offset += 500) {
				const membership = await api.global.post<LibraryMembershipResponse>(
					API.library.membership(),
					{ album_ids: albumIds.slice(offset, offset + 500) },
					{ signal }
				);
				ownedIds = ownedIds.concat(membership.owned_ids ?? []);
				requestedIds = requestedIds.concat(membership.requested_ids ?? []);
			}
			return {
				owned_ids: ownedIds.sort(),
				requested_ids: requestedIds.sort()
			};
		}
	});
};

export const getLibraryMembershipQuery = (getAlbumIds: Getter<string[]>) =>
	createQuery(() => getLibraryMembershipQueryOptions(authStore.user?.id, getAlbumIds()));

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

export const getLibraryRecentlyAddedQuery = () =>
	createQuery(() => ({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.recentlyAdded(),
		queryFn: ({ signal }) =>
			api.global.get<NativeAlbumsResponse>(API.library.recentlyAdded(20), { signal })
	}));

export const getLibraryAlbumDetailQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			enabled: !!albumId,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.albumDetail(albumId),
			queryFn: ({ signal }) =>
				api.global.get<LibraryAlbumDetail>(API.library.albumDetail(albumId), { signal })
		};
	});

export const getLibraryAlbumTracksQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			enabled: !!albumId,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.albumTracks(albumId),
			queryFn: ({ signal }) =>
				api.global.get<NativeTrackPage>(API.library.albumTracks(albumId), { signal })
		};
	});

export const getLibraryArtistDetailQuery = (getArtistId: Getter<string>) =>
	createQuery(() => {
		const artistId = getArtistId();
		return {
			enabled: !!artistId,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.artistDetail(artistId),
			queryFn: ({ signal }) =>
				api.global.get<LibraryArtistSummary>(API.library.artistDetail(artistId), { signal })
		};
	});

export const getLibraryArtistAlbumsQuery = (getArtistId: Getter<string>) =>
	createQuery(() => {
		const artistId = getArtistId();
		return {
			enabled: !!artistId,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.artistAlbums(artistId),
			queryFn: ({ signal }) =>
				api.global.get<NativeAlbumsResponse>(API.library.artistAlbums(artistId), { signal })
		};
	});

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
