import {
	createInfiniteQuery,
	createQuery,
	keepPreviousData,
	queryOptions
} from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { API, CACHE_TTL } from '$lib/constants';
import { api } from '$lib/api/client';
import {
	LibraryQueryKeyFactory,
	type LibraryV3AlbumsParams,
	type LibraryV3ArtistsParams,
	type LibraryV3UserId
} from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import {
	toAlbumDetail,
	toAlbumSummary,
	toArtistSummary,
	toLibraryStats,
	toNativeAlbums
} from './libraryAdapters';
import { toNativeTrack } from './libraryTracks';
import type {
	Album,
	ArtistSort,
	LibraryAlbumStatus,
	LibraryAlbumDetail,
	LibraryAlbumSummary,
	LibraryArtistSummary,
	LibraryArtistAppearancesResponse,
	LibraryArtistScope,
	LibraryScanSchedule,
	LibraryMembershipResponse,
	NativeArtistsResponse,
	NativeTrackListItem,
	ScanFrequency
} from '$lib/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { ttl } from '$lib/stores/cacheTtl.svelte';
import { setQueryDataWithPersister } from '../QueryClient';

// Catalog reads for the local library. Each read calls the v3 catalog and
// adapts the view into the page model the library components render
// (libraryAdapters.ts); keys come from the v3 key set, which carries the
// user id because the views include per-caller favorite flags.

const userId = () => authStore.user?.id;

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

export const getLibraryAlbumsQueryOptions = (
	forUser: LibraryV3UserId,
	params: LibraryV3AlbumsParams
) =>
	queryOptions({
		staleTime: ttl('library', CACHE_TTL.LIBRARY_NATIVE),
		placeholderData: keepPreviousData,
		queryKey: LibraryQueryKeyFactory.catalog.albums(forUser, params),
		queryFn: async ({ signal }) =>
			toNativeAlbums(await api.global.v3.GET(LibraryV3Api.albums(params), { signal }))
	});

export const getLibraryAlbumsQuery = (getParams: Getter<LibraryV3AlbumsParams>) =>
	createQuery(() => getLibraryAlbumsQueryOptions(userId(), getParams()));

export interface LibraryArtistsParams {
	sortBy: ArtistSort;
	sortOrder: 'asc' | 'desc';
	q: string;
	scope: LibraryArtistScope;
}

const ARTISTS_PAGE_SIZE = 48;

// v3 has no appearance-count order; that choice sorts by album count.
function artistParams(params: LibraryArtistsParams, offset: number): LibraryV3ArtistsParams {
	return {
		limit: ARTISTS_PAGE_SIZE,
		offset,
		sort: params.sortBy === 'appearance_count' ? 'album_count' : params.sortBy,
		order: params.sortOrder,
		q: params.q || undefined,
		scope: params.scope === 'contributors' ? 'contributors' : 'album_artists'
	};
}

async function fetchArtists(
	params: LibraryV3ArtistsParams,
	signal?: AbortSignal
): Promise<NativeArtistsResponse> {
	const page = await api.global.v3.GET(LibraryV3Api.artists(params), { signal });
	return {
		items: page.items.map(toArtistSummary),
		total: page.total,
		album_artist_total: page.album_artist_total,
		contributor_total: page.contributor_total
	};
}

export const getLibraryArtistsInfiniteQuery = (getParams: Getter<LibraryArtistsParams>) =>
	createInfiniteQuery(() => {
		const params = getParams();
		return {
			staleTime: ttl('library', CACHE_TTL.LIBRARY_NATIVE),
			queryKey: LibraryQueryKeyFactory.catalog.artists(userId(), artistParams(params, 0)),
			initialPageParam: 0,
			queryFn: ({ pageParam = 0, signal }) => fetchArtists(artistParams(params, pageParam), signal),
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
		staleTime: ttl('library', CACHE_TTL.LIBRARY_NATIVE),
		queryKey: LibraryQueryKeyFactory.catalog.artistThumbs(userId()),
		queryFn: ({ signal }) =>
			fetchArtists(
				{ limit: ARTIST_THUMBS_LIMIT, offset: 0, sort: 'album_count', order: 'desc' },
				signal
			)
	}));

export const getLibraryStatsQueryOptions = (forUser: LibraryV3UserId) =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.catalog.stats(forUser),
		queryFn: async ({ signal }) =>
			toLibraryStats(await api.global.v3.GET(LibraryV3Api.stats(), { signal }))
	});

export const getLibraryStatsQuery = () => createQuery(() => getLibraryStatsQueryOptions(userId()));

const RECENTLY_ADDED_LIMIT = 20;

export const getLibraryRecentlyAddedQuery = () =>
	createQuery(() => ({
		staleTime: ttl('recentlyAdded', CACHE_TTL.LIBRARY_NATIVE),
		queryKey: LibraryQueryKeyFactory.catalog.recentlyAdded(userId(), RECENTLY_ADDED_LIMIT),
		queryFn: async ({ signal }) =>
			toNativeAlbums(
				await api.global.v3.GET(LibraryV3Api.recentlyAdded(RECENTLY_ADDED_LIMIT), { signal })
			)
	}));

export const getLibraryAlbumDetailQueryOptions = (forUser: LibraryV3UserId, albumId: string) =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.catalog.albumDetail(forUser, albumId),
		queryFn: async ({ signal }) =>
			toAlbumDetail(await api.global.v3.GET(LibraryV3Api.albumDetail(albumId), { signal }))
	});

export const getLibraryAlbumDetailQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return { ...getLibraryAlbumDetailQueryOptions(userId(), albumId), enabled: !!albumId };
	});

// Album pages are reachable by local id and by release-group id; once the
// canonical id is known the detail is stored under it too.
export const cacheCanonicalLibraryAlbumDetail = (album: LibraryAlbumDetail) =>
	setQueryDataWithPersister<LibraryAlbumDetail>(
		LibraryQueryKeyFactory.catalog.albumDetail(userId(), album.id),
		album
	);

export const getLibraryAlbumCopiesQueryOptions = (forUser: LibraryV3UserId, albumId: string) =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.catalog.albumCopies(forUser, albumId),
		queryFn: async ({ signal }) =>
			toNativeAlbums(await api.global.v3.GET(LibraryV3Api.albumCopies(albumId), { signal }))
	});

export const getLibraryAlbumCopiesQuery = (
	getAlbumId: Getter<string>,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			...getLibraryAlbumCopiesQueryOptions(userId(), albumId),
			enabled: getEnabled() && !!albumId
		};
	});

const ALBUM_TRACKS_PAGE = { limit: 500 };

export const getLibraryAlbumTracksQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			enabled: !!albumId,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.catalog.albumTracks(userId(), albumId, ALBUM_TRACKS_PAGE),
			queryFn: async ({ signal }) => {
				const page = await api.global.v3.GET(LibraryV3Api.albumTracks(albumId, ALBUM_TRACKS_PAGE), {
					signal
				});
				return { ...page, items: page.items.map(toNativeTrack) };
			}
		};
	});

export const getLibraryArtistDetailQueryOptions = (forUser: LibraryV3UserId, artistId: string) =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.catalog.artistDetail(forUser, artistId),
		queryFn: async ({ signal }) =>
			toArtistSummary(await api.global.v3.GET(LibraryV3Api.artistDetail(artistId), { signal }))
	});

export const getLibraryArtistDetailQuery = (getArtistId: Getter<string>) =>
	createQuery(() => {
		const artistId = getArtistId();
		return { ...getLibraryArtistDetailQueryOptions(userId(), artistId), enabled: !!artistId };
	});

export const cacheCanonicalLibraryArtistDetail = (artist: LibraryArtistSummary) =>
	setQueryDataWithPersister<LibraryArtistSummary>(
		LibraryQueryKeyFactory.catalog.artistDetail(userId(), artist.id),
		artist
	);

const ARTIST_ALBUMS_PAGE = { limit: 200 };

export const getLibraryArtistAlbumsQuery = (getArtistId: Getter<string>) =>
	createQuery(() => {
		const artistId = getArtistId();
		return {
			enabled: !!artistId,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.catalog.artistAlbums(userId(), artistId, ARTIST_ALBUMS_PAGE),
			queryFn: async ({ signal }) =>
				toNativeAlbums(
					await api.global.v3.GET(LibraryV3Api.artistAlbums(artistId, ARTIST_ALBUMS_PAGE), {
						signal
					})
				)
		};
	});

const ARTIST_APPEARANCES_PAGE_SIZE = 20;

// v3 lists the albums an artist appears on; the tracks they appear on come
// from the track list filtered by album and artist, one read per album.
async function fetchAppearances(
	artistId: string,
	offset: number,
	signal: AbortSignal
): Promise<LibraryArtistAppearancesResponse> {
	const page = await api.global.v3.GET(
		LibraryV3Api.artistAppearances(artistId, { limit: ARTIST_APPEARANCES_PAGE_SIZE, offset }),
		{ signal }
	);
	const items = await Promise.all(
		page.items.map(async (album) => {
			const tracks = await api.global.v3.GET(
				LibraryV3Api.tracks({
					limit: 200,
					offset: 0,
					sort: 'title',
					order: 'asc',
					albumId: album.id,
					artistId
				}),
				{ signal }
			);
			return { album: toAlbumSummary(album), tracks: tracks.items.map(toNativeTrack) };
		})
	);
	return {
		items,
		total: page.total,
		total_tracks: items.reduce((sum, item) => sum + item.tracks.length, 0),
		offset: page.offset,
		limit: page.limit
	};
}

export const getLibraryArtistAppearancesQuery = (getArtistId: Getter<string>) =>
	createInfiniteQuery(() => {
		const artistId = getArtistId();
		return {
			enabled: !!artistId,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.catalog.artistAppearances(userId(), artistId, {
				limit: ARTIST_APPEARANCES_PAGE_SIZE
			}),
			initialPageParam: 0,
			queryFn: ({ pageParam = 0, signal }) => fetchAppearances(artistId, pageParam, signal),
			getNextPageParam: (
				lastPage: LibraryArtistAppearancesResponse,
				allPages: LibraryArtistAppearancesResponse[]
			) => {
				const loaded = allPages.reduce((total, page) => total + page.items.length, 0);
				return loaded < lastPage.total ? loaded : undefined;
			}
		};
	});

// schedule route is admin-gated; pass `enabled` to keep it off for non-admins
export const getLibraryScanScheduleQuery = (enabled: () => boolean = () => true) =>
	createQuery(() => ({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.scanSchedule(),
		queryFn: async ({ signal }): Promise<LibraryScanSchedule> => {
			const data = await api.global.v3.GET(LibraryV3Api.schedule(), { signal });
			return {
				scan_frequency: (data.scan_frequency ?? 'manual') as ScanFrequency,
				daily_scan_time: data.daily_scan_time ?? '',
				last_scan: data.last_scan ?? null,
				last_scan_success: data.last_scan_success ?? false,
				server_timezone: data.server_timezone
			};
		}
	}));

// The per-MusicBrainz-album library status has no v3 route yet.
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
			queryKey: LibraryQueryKeyFactory.catalog.search(userId(), term),
			queryFn: async ({ signal }): Promise<LibrarySearchResults> => {
				const page = { limit: LIBRARY_SEARCH_LIMIT, offset: 0, q: term } as const;
				const [albums, artists, tracks] = await Promise.all([
					api.global.v3.GET(LibraryV3Api.albums({ ...page, sort: 'date_added', order: 'desc' }), {
						signal
					}),
					api.global.v3.GET(LibraryV3Api.artists({ ...page, sort: 'name', order: 'asc' }), {
						signal
					}),
					api.global.v3.GET(LibraryV3Api.tracks({ ...page, sort: 'date_added', order: 'desc' }), {
						signal
					})
				]);
				return {
					albums: albums.items.map(toAlbumSummary),
					artists: artists.items.map(toArtistSummary),
					tracks: tracks.items.map(toNativeTrack)
				};
			}
		};
	});

// MusicBrainz album search, used to match dropped files to a release group.
// The results must carry real release-group MBIDs, which the v3 search
// buckets (local catalog) cannot give, so this read waits on a v3 route.
export const getAlbumSearchQuery = (getTerm: Getter<string>) =>
	createQuery(() => {
		const term = getTerm().trim();
		return {
			enabled: term.length >= 2,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.catalog.albumSearch(userId(), term),
			queryFn: async ({ signal }): Promise<Album[]> => {
				const data = await api.global.get<{ results?: Album[] }>(API.search.albums(term, 20), {
					signal
				});
				return data.results ?? [];
			}
		};
	});
