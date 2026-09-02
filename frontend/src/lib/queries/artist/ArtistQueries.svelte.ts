import { API, CACHE_TTL } from '$lib/constants';
import { createInfiniteQuery, createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { ArtistQueryKeyFactory } from './ArtistQueryKeyFactory';
import { api } from '$lib/api/client';
import type {
	ArtistInfoBasic,
	ArtistInfoExtended,
	ArtistReleases,
	LastFmArtistEnrichment,
	ReleaseGroup,
	SimilarArtistsResponse,
	TopAlbumsResponse,
	TopSongsResponse
} from '$lib/types';
import type { MusicSource } from '$lib/stores/musicSource';
import { authStore } from '$lib/stores/authStore.svelte';
import { extractServiceStatus } from '$lib/utils/serviceStatus';
import { setQueryDataWithPersister } from '../QueryClient';

export const getBasicArtistQueryOptions = (artistId: string) =>
	queryOptions({
		staleTime: CACHE_TTL.ARTIST_DETAIL_BASIC,
		queryKey: ArtistQueryKeyFactory.basic(artistId),
		queryFn: async ({ signal }) => {
			const data = await api.global.get<ArtistInfoBasic>(API.artist.basic(artistId), { signal });
			// mirrors albumPageState: the degraded payload carries
			// service_status and api.global bypasses the header-recording
			// fetch wrapper
			extractServiceStatus(data);
			return data;
		}
	});

export const getBasicArtistQuery = (getArtistId: Getter<string>) =>
	createQuery(() => getBasicArtistQueryOptions(getArtistId()));

export const getExtendedArtistQueryOptions = (artistId: string) =>
	queryOptions({
		staleTime: CACHE_TTL.ARTIST_DETAIL_EXTENDED,
		queryKey: ArtistQueryKeyFactory.extended(artistId),
		// A fast extended query can finish before the provider page observes its lazy fields.
		notifyOnChangeProps: 'all',
		queryFn: ({ signal }) =>
			api.global.get<ArtistInfoExtended>(API.artist.extended(artistId), {
				signal
			})
	});

export const getExtendedArtistQuery = (getArtistId: Getter<string>) =>
	createQuery(() => getExtendedArtistQueryOptions(getArtistId()));

export const getSimilarArtistsQuery = (
	getParams: Getter<{ artistId: string; source: MusicSource; enabled?: boolean }>
) =>
	createQuery(() => {
		const { artistId, source, enabled = true } = getParams();
		return {
			enabled,
			staleTime: CACHE_TTL.ARTIST_DISCOVERY,
			queryKey: ArtistQueryKeyFactory.similarArtists(authStore.user?.id, artistId, source),
			queryFn: ({ signal }) =>
				api.global.get<SimilarArtistsResponse>(API.artist.similarArtists(artistId, source), {
					signal
				})
		};
	});

export const getArtistTopAlbumsQuery = (
	getParams: Getter<{ artistId: string; source: MusicSource; enabled?: boolean }>
) =>
	createQuery(() => {
		const { artistId, source, enabled = true } = getParams();
		return {
			enabled,
			staleTime: CACHE_TTL.ARTIST_DISCOVERY,
			queryKey: ArtistQueryKeyFactory.topAlbums(authStore.user?.id, artistId, source),
			queryFn: ({ signal }) =>
				api.global.get<TopAlbumsResponse>(API.artist.topAlbums(artistId, source), {
					signal
				})
		};
	});

export const getArtistTopSongsQuery = (
	getParams: Getter<{ artistId: string; source: MusicSource; enabled?: boolean }>
) =>
	createQuery(() => {
		const { artistId, source, enabled = true } = getParams();
		return {
			enabled,
			staleTime: CACHE_TTL.ARTIST_DISCOVERY,
			queryKey: ArtistQueryKeyFactory.topSongs(authStore.user?.id, artistId, source),
			queryFn: ({ signal }) =>
				api.global.get<TopSongsResponse>(API.artist.topSongs(artistId, source), {
					signal
				})
		};
	});

export const getArtistLastFmEnrichmentQuery = (
	getParams: Getter<{ artistId: string; artistName?: string }>
) =>
	createQuery(() => {
		const { artistId, artistName } = getParams();
		return {
			staleTime: CACHE_TTL.ARTIST_DETAIL_LASTFM,
			queryKey: ArtistQueryKeyFactory.lastFmEnrichment(artistId, artistName),
			queryFn: ({ signal }) =>
				api.global.get<LastFmArtistEnrichment>(API.artist.lastFmEnrichment(artistId, artistName!), {
					signal
				}),
			enabled: () => !!artistName
		};
	});

const BATCH_SIZE = 50;

// A3: while the backend walker completes a large catalog, page 1 arrives partial
// (warming=true, source_total_count=null). Poll page 0 until any response reports
// warming false/absent, then stop. Payloads without the flag never poll.
const WARMING_POLL_INTERVAL_MS = 2_000;

export const getArtistReleasesInfiniteQuery = (getArtistId: Getter<string>) =>
	createInfiniteQuery(() => ({
		staleTime: CACHE_TTL.ARTIST_DETAIL_BASIC,
		queryKey: ArtistQueryKeyFactory.releases(getArtistId()),
		initialPageParam: 0,
		queryFn: async ({ pageParam = 0, signal }) => {
			const response = await api.global.get<ArtistReleases>(
				API.artist.releases(getArtistId(), pageParam, BATCH_SIZE),
				{ signal }
			);
			// mirrors the basic query: the degraded discography payload
			// carries service_status and api.global bypasses the
			// header-recording fetch wrapper
			extractServiceStatus(response);
			return response;
		},
		getNextPageParam: (lastPage) => {
			if (!lastPage.has_more) {
				return undefined;
			}
			if (lastPage.next_offset != null) {
				return lastPage.next_offset;
			}
			return undefined;
		},
		refetchInterval: (query: { state: { data?: { pages?: Array<{ warming?: boolean }> } } }) =>
			query.state.data?.pages?.[0]?.warming === true ? WARMING_POLL_INTERVAL_MS : false
	}));

type ArtistReleasesInfiniteQuery = ReturnType<typeof getArtistReleasesInfiniteQuery>;

export const updateArtistReleaseInCache = (
	artistId: string,
	updatedData: Partial<ReleaseGroup> & Pick<ReleaseGroup, 'id'>
) => {
	const queryKey = ArtistQueryKeyFactory.releases(artistId);
	return setQueryDataWithPersister(queryKey, (prevData: ArtistReleasesInfiniteQuery['data']) => {
		if (!prevData) return prevData;
		const updatedPages = prevData.pages.map((page) => {
			const updateRelease = (originalRelease: ReleaseGroup) => {
				if (originalRelease.id === updatedData.id) {
					return { ...originalRelease, ...updatedData };
				}
				return originalRelease;
			};

			return {
				...page,
				albums: page.albums.map(updateRelease),
				singles: page.singles.map(updateRelease),
				eps: page.eps.map(updateRelease)
			};
		});
		return { ...prevData, pages: updatedPages };
	});
};
