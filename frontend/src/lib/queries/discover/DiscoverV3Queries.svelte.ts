import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { ttl } from '$lib/stores/cacheTtl.svelte';
import { discoverHasContent } from '$lib/utils/discoverContent';
import { DiscoverV3Api } from './DiscoverV3Api';
import {
	DiscoverQueryKeyFactory,
	type DiscoverV3CacheCheckItem,
	type DiscoverV3RadioParams,
	type DiscoverV3UserId
} from './DiscoverQueryKeyFactory';

export type DiscoverResponseV3 = components['schemas']['DiscoverResponse'];
export type DiscoverQueueResponseV3 = components['schemas']['DiscoverQueueResponse'];
export type DiscoverQueuePreviewV3 = components['schemas']['DiscoverQueuePreview'];
export type DiscoverQueueStatusResponseV3 = components['schemas']['DiscoverQueueStatusResponse'];
export type QueueGenerateResponseV3 = components['schemas']['QueueGenerateResponse'];
export type RefreshResponseV3 = components['schemas']['RefreshResponse'];
export type QueueEnrichmentV3 = components['schemas']['QueueEnrichment'];
export type IgnoredReleasesResponseV3 = components['schemas']['IgnoredReleasesResponse'];
export type DiscoveryBatchListResponseV3 = components['schemas']['DiscoveryBatchListResponse'];
export type DiscoveryBatchDetailV3 = components['schemas']['DiscoveryBatchDetail'];
export type DiscoveryBatchItemStatusV3 = components['schemas']['DiscoveryBatchItemStatus'];
export type ChartSectionV3 = components['schemas']['ChartSection'];
export type RadioPlanRequestV3 = components['schemas']['RadioPlanRequest'];
export type RadioPlanResponseV3 = components['schemas']['RadioPlanResponse'];
export type PlaylistSuggestionsResponseV3 = components['schemas']['PlaylistSuggestionsResponse'];
export type AlbumPreviewResponseV3 = components['schemas']['AlbumPreviewResponse'];
export type TrackPreviewResponseV3 = components['schemas']['TrackPreviewResponse'];
export type YouTubeSearchResponseV3 = components['schemas']['YouTubeSearchResponse'];
export type YouTubeQuotaResponseV3 = components['schemas']['YouTubeQuotaResponse'];
export type TrackCacheCheckResponseV3 = components['schemas']['TrackCacheCheckResponse'];
export type QueueValidateResponseV3 = components['schemas']['QueueValidateResponse'];

// Floor between intentional revalidates; active rebuilds stream via the
// refreshing-poll lane below; tab switches never revalidate.
const DISCOVER_REVALIDATE_MS = 60_000;

// Keep the persisted recommendations while the server finishes its SWR rebuild.
async function fetchDiscoverHome(
	userId: DiscoverV3UserId,
	signal?: AbortSignal
): Promise<DiscoverResponseV3> {
	const fresh = await api.global.v3.GET(DiscoverV3Api.home(), {
		signal,
		timeoutMs: 15_000
	});
	if (!discoverHasContent(fresh) && fresh.refreshing) {
		// Keep the browser-only QueryClient out of the server test module graph.
		const { queryClient } = await import('$lib/queries/QueryClient');
		const prev = queryClient.getQueryData<DiscoverResponseV3>(
			DiscoverQueryKeyFactory.v3.home(userId)
		);
		if (prev && discoverHasContent(prev)) {
			return {
				...prev,
				refreshing: true,
				refresh_started_at: fresh.refresh_started_at,
				section_status: Object.fromEntries(
					Object.keys(prev.section_status ?? {}).map((section) => [section, 'updating'])
				)
			} as DiscoverResponseV3;
		}
	}
	return fresh;
}

export const getDiscoverHomeV3QueryOptions = (userId: DiscoverV3UserId) =>
	queryOptions({
		enabled: Boolean(userId),
		staleTime: DISCOVER_REVALIDATE_MS,
		refetchOnWindowFocus: false,
		queryKey: DiscoverQueryKeyFactory.v3.home(userId),
		queryFn: ({ signal }) => fetchDiscoverHome(userId, signal),
		refetchInterval: (query: { state: { data?: DiscoverResponseV3 | undefined } }) =>
			query.state.data?.refreshing ? 10_000 : false
	});

export const getDiscoverHomeV3Query = () =>
	createQuery(() => getDiscoverHomeV3QueryOptions(authStore.user?.id));

export const getDiscoverQueueV3QueryOptions = (
	userId: DiscoverV3UserId,
	count: number | null = null
) =>
	queryOptions({
		enabled: Boolean(userId),
		staleTime: ttl('discover', CACHE_TTL.DISCOVER_QUEUE),
		queryKey: DiscoverQueryKeyFactory.v3.queue(userId, count),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DiscoverV3Api.queue(count), { signal })
	});

export const getDiscoverQueueV3Query = (getCount: Getter<number | null> = () => null) =>
	createQuery(() => getDiscoverQueueV3QueryOptions(authStore.user?.id, getCount()));

export const getDiscoverQueueStatusV3Query = () =>
	createQuery(() => ({
		enabled: Boolean(authStore.user?.id),
		staleTime: ttl('discover', CACHE_TTL.DISCOVER),
		queryKey: DiscoverQueryKeyFactory.v3.queueStatus(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DiscoverV3Api.queueStatus(), {
				signal
			})
	}));

export const getDiscoverQueueEnrichmentV3Query = (getMbid: Getter<string>) =>
	createQuery(() => {
		const mbid = getMbid();
		return {
			enabled: Boolean(authStore.user?.id && mbid),
			staleTime: ttl('discover', CACHE_TTL.DISCOVER),
			queryKey: DiscoverQueryKeyFactory.v3.queueEnrich(authStore.user?.id, mbid),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(DiscoverV3Api.queueEnrich(mbid), { signal })
		};
	});

export const getDiscoverIgnoredV3Query = () =>
	createQuery(() => ({
		enabled: Boolean(authStore.user?.id),
		staleTime: ttl('discover', CACHE_TTL.DISCOVER),
		queryKey: DiscoverQueryKeyFactory.v3.ignored(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DiscoverV3Api.ignored(), { signal })
	}));

export const getDiscoveryBatchesV3Query = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		staleTime: 15_000,
		queryKey: DiscoverQueryKeyFactory.v3.batches(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DiscoverV3Api.batches(), { signal })
	}));

export const getDiscoveryBatchV3Query = (getBatchId: Getter<string>) =>
	createQuery(() => {
		const batchId = getBatchId();
		return {
			enabled: Boolean(authStore.user?.id && batchId),
			staleTime: 15_000,
			queryKey: DiscoverQueryKeyFactory.v3.batch(authStore.user?.id, batchId),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(DiscoverV3Api.batch(batchId), { signal })
		};
	});

export interface DiscoverV3RadioInput {
	seedType: string;
	seedId: string;
	count?: number;
	source?: string | null;
	enabled?: boolean;
}

export const getDiscoverRadioV3Query = (getParams: Getter<DiscoverV3RadioInput>) =>
	createQuery(() => {
		const params = getParams();
		const radioParams: DiscoverV3RadioParams = {};
		if (params.count !== undefined) radioParams.count = params.count;
		if (params.source !== undefined) radioParams.source = params.source;
		return {
			staleTime: ttl('discover', CACHE_TTL.DISCOVER),
			queryKey: DiscoverQueryKeyFactory.v3.radio(
				authStore.user?.id,
				params.seedType,
				params.seedId,
				radioParams
			),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.POST(
					DiscoverV3Api.radio(),
					{
						seed_type: params.seedType,
						seed_id: params.seedId,
						...(params.count !== undefined ? { count: params.count } : {}),
						...(params.source !== undefined ? { source: params.source } : {})
					},
					{ signal }
				),
			enabled: (params.enabled ?? true) && !!params.seedId
		};
	});

export const getDiscoverRadioPlanV3Query = (getBody: Getter<RadioPlanRequestV3>) =>
	createQuery(() => {
		const body = getBody();
		return {
			staleTime: ttl('discover', CACHE_TTL.DISCOVER),
			queryKey: DiscoverQueryKeyFactory.v3.radioPlan(authStore.user?.id, {
				mode: body.mode,
				seedType: body.seed_type,
				seedId: body.seed_id ?? null
			}),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.POST(DiscoverV3Api.radioPlan(), body, { signal }),
			enabled: Boolean(authStore.user?.id)
		};
	});

export interface DiscoverV3PlaylistSuggestionsInput {
	playlistId: string;
	count?: number;
	source?: string | null;
	enabled?: boolean;
}

export const getDiscoverPlaylistSuggestionsV3Query = (
	getParams: Getter<DiscoverV3PlaylistSuggestionsInput>
) =>
	createQuery(() => {
		const params = getParams();
		return {
			staleTime: ttl('discover', CACHE_TTL.DISCOVER),
			queryKey: DiscoverQueryKeyFactory.v3.playlistSuggestions(
				authStore.user?.id,
				params.playlistId,
				params.count ?? 15
			),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.POST(
					DiscoverV3Api.playlistSuggestions(),
					{
						playlist_id: params.playlistId,
						count: params.count ?? 15,
						...(params.source !== undefined ? { source: params.source } : {})
					},
					{ signal }
				),
			enabled: (params.enabled ?? true) && !!params.playlistId
		};
	});

export const getDiscoverAlbumPreviewV3Query = (
	getParams: Getter<{ artist: string; album: string; count?: number | null }>
) =>
	createQuery(() => {
		const params = getParams();
		const count = params.count ?? null;
		return {
			staleTime: ttl('discover', CACHE_TTL.DISCOVER),
			queryKey: DiscoverQueryKeyFactory.v3.albumPreview(
				authStore.user?.id,
				params.artist,
				params.album,
				count
			),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(
					DiscoverV3Api.albumPreview(params.artist, params.album, count),
					{ signal }
				),
			enabled: Boolean(authStore.user?.id && params.artist && params.album)
		};
	});

export const getDiscoverTrackPreviewV3Query = (
	getParams: Getter<{ artist: string; track: string }>
) =>
	createQuery(() => {
		const params = getParams();
		return {
			staleTime: ttl('discover', CACHE_TTL.DISCOVER),
			queryKey: DiscoverQueryKeyFactory.v3.trackPreview(
				authStore.user?.id,
				params.artist,
				params.track
			),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(
					DiscoverV3Api.trackPreview(params.artist, params.track),
					{ signal }
				),
			enabled: Boolean(authStore.user?.id && params.artist && params.track)
		};
	});

export const getDiscoverYouTubeSearchV3Query = (
	getParams: Getter<{ artist: string; album: string }>
) =>
	createQuery(() => {
		const params = getParams();
		return {
			staleTime: ttl('discover', CACHE_TTL.DISCOVER),
			queryKey: DiscoverQueryKeyFactory.v3.youtubeSearch(
				authStore.user?.id,
				params.artist,
				params.album
			),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(
					DiscoverV3Api.youtubeSearch(params.artist, params.album),
					{ signal }
				),
			enabled: Boolean(authStore.user?.id && params.artist && params.album)
		};
	});

export const getDiscoverYouTubeTrackSearchV3Query = (
	getParams: Getter<{ artist: string; track: string }>
) =>
	createQuery(() => {
		const params = getParams();
		return {
			staleTime: ttl('discover', CACHE_TTL.DISCOVER),
			queryKey: DiscoverQueryKeyFactory.v3.youtubeTrackSearch(
				authStore.user?.id,
				params.artist,
				params.track
			),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(
					DiscoverV3Api.youtubeTrackSearch(params.artist, params.track),
					{ signal }
				),
			enabled: Boolean(authStore.user?.id && params.artist && params.track)
		};
	});

export const getDiscoverYouTubeQuotaV3Query = () =>
	createQuery(() => ({
		enabled: Boolean(authStore.user?.id),
		staleTime: ttl('discover', CACHE_TTL.DISCOVER),
		queryKey: DiscoverQueryKeyFactory.v3.youtubeQuota(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DiscoverV3Api.youtubeQuota(), { signal })
	}));

export const getDiscoverYouTubeCacheCheckV3QueryOptions = (
	userId: DiscoverV3UserId,
	items: DiscoverV3CacheCheckItem[]
) =>
	queryOptions({
		enabled: Boolean(userId && items.length > 0),
		staleTime: ttl('discover', CACHE_TTL.DISCOVER),
		queryKey: DiscoverQueryKeyFactory.v3.youtubeCacheCheck(userId, items),
		queryFn: ({ signal }) =>
			api.global.v3.POST(
				DiscoverV3Api.youtubeCacheCheck(),
				{ items },
				{ signal }
			)
	});

export const getDiscoverYouTubeCacheCheckV3Query = (getItems: Getter<DiscoverV3CacheCheckItem[]>) =>
	createQuery(() => getDiscoverYouTubeCacheCheckV3QueryOptions(authStore.user?.id, getItems()));

export const getDiscoverQueueValidateV3QueryOptions = (
	userId: DiscoverV3UserId,
	releaseGroupMbids: string[]
) =>
	queryOptions({
		enabled: Boolean(userId && releaseGroupMbids.length > 0),
		staleTime: ttl('discover', CACHE_TTL.DISCOVER),
		queryKey: DiscoverQueryKeyFactory.v3.queueValidate(userId, releaseGroupMbids),
		queryFn: ({ signal }) =>
			api.global.v3.POST(
				DiscoverV3Api.queueValidate(),
				{ release_group_mbids: releaseGroupMbids },
				{ signal }
			)
	});

export const getDiscoverQueueValidateV3Query = (getMbids: Getter<string[]>) =>
	createQuery(() => getDiscoverQueueValidateV3QueryOptions(authStore.user?.id, getMbids()));
