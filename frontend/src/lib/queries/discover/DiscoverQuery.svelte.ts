import { api } from '$lib/api/client';
import { API, CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { discoverHasContent } from '$lib/utils/discoverContent';
import type { DiscoverResponse, HomeSection, PlaylistSuggestionsResponse } from '$lib/types';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

// the server bounds rebuild frequency, so visits can revalidate the persisted cache
const DISCOVER_REVALIDATE_MS = 10_000;

// keep the persisted recommendations while the server finishes its SWR rebuild
async function fetchDiscover(
	userId: string | null | undefined,
	signal?: AbortSignal
): Promise<DiscoverResponse> {
	const fresh = await api.global.get<DiscoverResponse>(API.discover(), {
		signal,
		timeoutMs: 15_000
	});
	if (!discoverHasContent(fresh) && fresh.refreshing) {
		// keep the browser-only QueryClient out of the server test module graph
		const { queryClient } = await import('$lib/queries/QueryClient');
		const prev = queryClient.getQueryData<DiscoverResponse>(
			DiscoverQueryKeyFactory.discover(userId)
		);
		if (prev && discoverHasContent(prev)) {
			return {
				...prev,
				refreshing: true,
				refresh_started_at: fresh.refresh_started_at,
				section_status: Object.fromEntries(
					Object.keys(prev.section_status ?? {}).map((section) => [section, 'updating'])
				)
			} as DiscoverResponse;
		}
	}
	return fresh;
}

export const getDiscoverQueryOptions = (userId: string | null | undefined) =>
	queryOptions({
		staleTime: DISCOVER_REVALIDATE_MS,
		queryKey: DiscoverQueryKeyFactory.discover(userId),
		queryFn: ({ signal }) => fetchDiscover(userId, signal)
	});

export const getDiscoverQuery = () =>
	createQuery(() => ({
		staleTime: DISCOVER_REVALIDATE_MS,
		queryKey: DiscoverQueryKeyFactory.discover(authStore.user?.id),
		queryFn: ({ signal }) => fetchDiscover(authStore.user?.id, signal),
		refetchInterval: (query: { state: { data?: DiscoverResponse | undefined } }) =>
			query.state.data?.refreshing ? 10_000 : false
	}));

export const getRadioQuery = (
	getParams: Getter<{ seedType: string; seedId: string; enabled?: boolean }>
) =>
	createQuery(() => ({
		staleTime: CACHE_TTL.DISCOVER,
		queryKey: DiscoverQueryKeyFactory.radio(
			authStore.user?.id,
			getParams().seedType,
			getParams().seedId
		),
		queryFn: ({ signal }) =>
			api.global.post<HomeSection>(
				API.discoverRadio(),
				{
					seed_type: getParams().seedType,
					seed_id: getParams().seedId
				},
				{ signal }
			),
		enabled: (getParams().enabled ?? true) && !!getParams().seedId
	}));

export const getPlaylistSuggestionsQuery = (
	getParams: Getter<{
		playlistId: string;
		count?: number;
		enabled?: boolean;
	}>
) =>
	createQuery(() => ({
		staleTime: CACHE_TTL.DISCOVER,
		queryKey: DiscoverQueryKeyFactory.playlistSuggestions(
			authStore.user?.id,
			getParams().playlistId
		),
		queryFn: ({ signal }) =>
			api.global.post<PlaylistSuggestionsResponse>(
				API.discoverPlaylistSuggestions(),
				{
					playlist_id: getParams().playlistId,
					count: getParams().count ?? 15
				},
				{ signal }
			),
		enabled: (getParams().enabled ?? true) && !!getParams().playlistId
	}));
