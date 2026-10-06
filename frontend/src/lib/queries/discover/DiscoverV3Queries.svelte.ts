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
export type DiscoveryBatchDetailV3 = components['schemas']['DiscoveryBatchDetail'];
export type DiscoveryBatchItemStatusV3 = components['schemas']['DiscoveryBatchItemStatus'];
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

export const getDiscoveryBatchesV3Query = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		staleTime: 15_000,
		queryKey: DiscoverQueryKeyFactory.v3.batches(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(DiscoverV3Api.batches(), { signal })
	}));

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
