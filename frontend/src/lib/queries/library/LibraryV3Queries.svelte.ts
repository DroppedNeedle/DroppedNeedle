import {
	createInfiniteQuery,
	createQuery,
	keepPreviousData,
	queryOptions
} from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { ttl } from '$lib/stores/cacheTtl.svelte';
import { setQueryDataWithPersister } from '../QueryClient';
import { LibraryV3Api } from './LibraryV3Api';
import {
	LibraryQueryKeyFactory,
	type LibraryV3ArtistsParams,
	type LibraryV3AlbumsParams,
	type LibraryV3PageParams,
	type LibraryV3TracksParams,
	type LibraryV3UserId
} from './LibraryQueryKeyFactory';

export type AlbumPageV3 = components['schemas']['AlbumPage'];
export type ArtistPageV3 = components['schemas']['ArtistPage'];
export type AlbumViewV3 = components['schemas']['AlbumView'];
export type ArtistViewV3 = components['schemas']['ArtistView'];
export type TrackPageV3 = components['schemas']['TrackPage'];
export type TrackViewV3 = components['schemas']['TrackView'];
export type LyricsViewV3 = components['schemas']['LyricsView'];
export type GenreListV3 = components['schemas']['GenreList'];
export type StatsViewV3 = components['schemas']['StatsView'];
export type ReviewsResponseV3 = components['schemas']['ReviewsResponse'];
export type EditionPinResponseV3 = components['schemas']['EditionPinResponse'];
export type ScanRunsResponseV3 = components['schemas']['ScanRunsResponse'];
export type RunDetailResponseV3 = components['schemas']['RunDetailResponse'];
export type RootsResponseV3 = components['schemas']['RootsResponse'];

export const getLibraryAlbumsV3QueryOptions = (
	userId: LibraryV3UserId,
	params: LibraryV3AlbumsParams
) =>
	queryOptions({
		enabled: Boolean(userId),
		staleTime: ttl('library', CACHE_TTL.LIBRARY_NATIVE),
		placeholderData: keepPreviousData,
		queryKey: LibraryQueryKeyFactory.v3.albums(userId, params),
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.albums(params), { signal })
	});

export const getLibraryAlbumsV3Query = (getParams: Getter<LibraryV3AlbumsParams>) =>
	createQuery(() => getLibraryAlbumsV3QueryOptions(authStore.user?.id, getParams()));

export interface LibraryV3ArtistsParamsInput {
	sortBy: LibraryV3ArtistsParams['sort'];
	sortOrder: LibraryV3ArtistsParams['order'];
	q: string;
	scope: 'all' | 'album_artists' | 'contributors';
}

const ARTISTS_PAGE_SIZE = 48;

export const getLibraryArtistsV3InfiniteQuery = (getParams: Getter<LibraryV3ArtistsParamsInput>) =>
	createInfiniteQuery(() => {
		const { sortBy, sortOrder, q, scope } = getParams();
		return {
			enabled: Boolean(authStore.user?.id),
			staleTime: ttl('library', CACHE_TTL.LIBRARY_NATIVE),
			queryKey: LibraryQueryKeyFactory.v3.artists(authStore.user?.id, {
				limit: ARTISTS_PAGE_SIZE,
				offset: 0,
				sort: sortBy,
				order: sortOrder,
				q: q || undefined,
				scope
			}),
			initialPageParam: 0,
			queryFn: ({ pageParam = 0, signal }) =>
				api.global.v3.GET(
					LibraryV3Api.artists({
						limit: ARTISTS_PAGE_SIZE,
						offset: pageParam,
						sort: sortBy,
						order: sortOrder,
						q: q || undefined,
						scope
					}),
					{ signal }
				),
			getNextPageParam: (lastPage: ArtistPageV3, allPages: ArtistPageV3[]) => {
				const loaded = allPages.reduce((n, p) => n + p.items.length, 0);
				return loaded < lastPage.total ? loaded : undefined;
			}
		};
	});

// Separate from the paged browse query so the hub avoids pulling a full page for thumbnails.
const ARTIST_THUMBS_LIMIT = 12;

export const getLibraryArtistThumbsV3Query = () =>
	createQuery(() => ({
		enabled: Boolean(authStore.user?.id),
		staleTime: ttl('library', CACHE_TTL.LIBRARY_NATIVE),
		queryKey: LibraryQueryKeyFactory.v3.artistThumbs(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(
				LibraryV3Api.artists({
					limit: ARTIST_THUMBS_LIMIT,
					offset: 0,
					sort: 'album_count',
					order: 'desc'
				}),
				{ signal }
			)
	}));

export const getLibraryStatsV3QueryOptions = (userId: LibraryV3UserId) =>
	queryOptions({
		enabled: Boolean(userId),
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.v3.stats(userId),
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.stats(), { signal })
	});

export const getLibraryStatsV3Query = () =>
	createQuery(() => getLibraryStatsV3QueryOptions(authStore.user?.id));

export const getLibraryRecentlyAddedV3Query = (getLimit: Getter<number> = () => 20) =>
	createQuery(() => {
		const limit = getLimit();
		return {
			enabled: Boolean(authStore.user?.id),
			staleTime: ttl('recentlyAdded', CACHE_TTL.LIBRARY_NATIVE),
			queryKey: LibraryQueryKeyFactory.v3.recentlyAdded(authStore.user?.id, limit),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(LibraryV3Api.recentlyAdded(limit), { signal })
		};
	});

export const getLibraryAlbumDetailV3QueryOptions = (userId: LibraryV3UserId, albumId: string) =>
	queryOptions({
		enabled: Boolean(userId && albumId),
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.v3.albumDetail(userId, albumId),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LibraryV3Api.albumDetail(albumId), { signal })
	});

export const getLibraryAlbumDetailV3Query = (getAlbumId: Getter<string>) =>
	createQuery(() => getLibraryAlbumDetailV3QueryOptions(authStore.user?.id, getAlbumId()));

export const cacheCanonicalLibraryAlbumDetailV3 = (userId: LibraryV3UserId, album: AlbumViewV3) =>
	setQueryDataWithPersister<AlbumViewV3>(
		LibraryQueryKeyFactory.v3.albumDetail(userId, album.id),
		album
	);

export const getLibraryAlbumCopiesV3QueryOptions = (userId: LibraryV3UserId, albumId: string) =>
	queryOptions({
		enabled: Boolean(userId && albumId),
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.v3.albumCopies(userId, albumId),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LibraryV3Api.albumCopies(albumId), { signal })
	});

export const getLibraryAlbumCopiesV3Query = (getAlbumId: Getter<string>) =>
	createQuery(() => getLibraryAlbumCopiesV3QueryOptions(authStore.user?.id, getAlbumId()));

export const getLibraryAlbumTracksV3Query = (
	getAlbumId: Getter<string>,
	getPage: Getter<LibraryV3PageParams> = () => ({})
) =>
	createQuery(() => {
		const albumId = getAlbumId();
		const page = getPage();
		return {
			enabled: Boolean(authStore.user?.id && albumId),
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.v3.albumTracks(authStore.user?.id, albumId, page),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(LibraryV3Api.albumTracks(albumId, page), { signal })
		};
	});

export const getLibraryArtistDetailV3QueryOptions = (userId: LibraryV3UserId, artistId: string) =>
	queryOptions({
		enabled: Boolean(userId && artistId),
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.v3.artistDetail(userId, artistId),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LibraryV3Api.artistDetail(artistId), { signal })
	});

export const getLibraryArtistDetailV3Query = (getArtistId: Getter<string>) =>
	createQuery(() => getLibraryArtistDetailV3QueryOptions(authStore.user?.id, getArtistId()));

export const getLibraryArtistAlbumsV3Query = (
	getArtistId: Getter<string>,
	getPage: Getter<LibraryV3PageParams> = () => ({})
) =>
	createQuery(() => {
		const artistId = getArtistId();
		const page = getPage();
		return {
			enabled: Boolean(authStore.user?.id && artistId),
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.v3.artistAlbums(authStore.user?.id, artistId, page),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(LibraryV3Api.artistAlbums(artistId, page), { signal })
		};
	});

export const getLibraryArtistAppearancesV3Query = (
	getArtistId: Getter<string>,
	getPage: Getter<LibraryV3PageParams> = () => ({})
) =>
	createQuery(() => {
		const artistId = getArtistId();
		const page = getPage();
		return {
			enabled: Boolean(authStore.user?.id && artistId),
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.v3.artistAppearances(authStore.user?.id, artistId, page),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(LibraryV3Api.artistAppearances(artistId, page), {
					signal
				})
		};
	});

export const getLibraryTracksV3QueryOptions = (
	userId: LibraryV3UserId,
	params: LibraryV3TracksParams
) =>
	queryOptions({
		enabled: Boolean(userId),
		staleTime: ttl('library', CACHE_TTL.LIBRARY_NATIVE),
		placeholderData: keepPreviousData,
		queryKey: LibraryQueryKeyFactory.v3.tracks(userId, params),
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.tracks(params), { signal })
	});

export const getLibraryTracksV3Query = (getParams: Getter<LibraryV3TracksParams>) =>
	createQuery(() => getLibraryTracksV3QueryOptions(authStore.user?.id, getParams()));

export const getLibraryTrackDetailV3QueryOptions = (userId: LibraryV3UserId, trackId: string) =>
	queryOptions({
		enabled: Boolean(userId && trackId),
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.v3.trackDetail(userId, trackId),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LibraryV3Api.trackDetail(trackId), { signal })
	});

export const getLibraryTrackDetailV3Query = (getTrackId: Getter<string>) =>
	createQuery(() => getLibraryTrackDetailV3QueryOptions(authStore.user?.id, getTrackId()));

export const getLibraryLyricsV3QueryOptions = (userId: LibraryV3UserId, trackId: string) =>
	queryOptions({
		enabled: Boolean(userId && trackId),
		staleTime: CACHE_TTL.LYRICS,
		queryKey: LibraryQueryKeyFactory.v3.lyrics(userId, trackId),
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.lyrics(trackId), { signal })
	});

export const getLibraryLyricsV3Query = (getTrackId: Getter<string>) =>
	createQuery(() => getLibraryLyricsV3QueryOptions(authStore.user?.id, getTrackId()));

export const getLibraryGenresV3Query = () =>
	createQuery(() => ({
		enabled: Boolean(authStore.user?.id),
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.v3.genres(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.genres(), { signal })
	}));

export const getLibraryGenreTracksV3Query = (
	getName: Getter<string>,
	getPage: Getter<LibraryV3PageParams> = () => ({})
) =>
	createQuery(() => {
		const name = getName();
		const page = getPage();
		return {
			enabled: Boolean(authStore.user?.id && name),
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.v3.genreTracks(authStore.user?.id, name, page),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(LibraryV3Api.genreTracks(name, page), { signal })
		};
	});

export const getLibraryReviewsV3QueryOptions = (userId: LibraryV3UserId, albumId: string) =>
	queryOptions({
		enabled: Boolean(userId && albumId),
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.v3.reviews(userId, albumId),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LibraryV3Api.reviews(albumId), { signal })
	});

export const getLibraryReviewsV3Query = (getAlbumId: Getter<string>) =>
	createQuery(() => getLibraryReviewsV3QueryOptions(authStore.user?.id, getAlbumId()));

export const getLibraryEditionPinV3QueryOptions = (userId: LibraryV3UserId, albumId: string) =>
	queryOptions({
		enabled: Boolean(userId && albumId),
		staleTime: CACHE_TTL.ALBUM_DETAIL_EDITIONS,
		queryKey: LibraryQueryKeyFactory.v3.editionPin(userId, albumId),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LibraryV3Api.editionPin(albumId), { signal })
	});

export const getLibraryEditionPinV3Query = (getAlbumId: Getter<string>) =>
	createQuery(() => getLibraryEditionPinV3QueryOptions(authStore.user?.id, getAlbumId()));

export const getLibraryScanRunsV3Query = () =>
	createQuery(() => ({
		enabled: Boolean(authStore.user?.id),
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.v3.scanRuns(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.scanRuns(), { signal })
	}));

export const getLibraryScanRunV3Query = (getRunId: Getter<string>) =>
	createQuery(() => {
		const runId = getRunId();
		return {
			enabled: Boolean(authStore.user?.id && runId),
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LibraryQueryKeyFactory.v3.scanRun(authStore.user?.id, runId),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(LibraryV3Api.scanRun(runId), { signal })
		};
	});

export const getLibraryRootsV3Query = () =>
	createQuery(() => ({
		enabled: Boolean(authStore.user?.id),
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: LibraryQueryKeyFactory.v3.roots(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.roots(), { signal })
	}));
