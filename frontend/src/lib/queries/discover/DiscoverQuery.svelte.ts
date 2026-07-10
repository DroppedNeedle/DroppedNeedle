import { api } from '$lib/api/client';
import { API, CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { discoverHasContent } from '$lib/utils/discoverContent';
import type { DiscoverResponse, HomeSection, PlaylistSuggestionsResponse } from '$lib/types';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

// Re-check the backend on visits so a stale cache gets revalidated promptly; the actual
// rebuild cadence is bounded server-side (STALE_REVALIDATE_SECONDS), so this stays cheap.
const DISCOVER_REVALIDATE_MS = 10_000;

// Client-side stale-while-revalidate: while the backend is still building (an empty,
// `refreshing` response), keep showing the last good recommendations instead of dropping
// the user back to the build screen. The last good copy survives a backend redeploy via
// the IndexedDB persister, so a restart no longer re-shows "Building...".
async function fetchDiscover(
	userId: string | null | undefined,
	signal?: AbortSignal
): Promise<DiscoverResponse> {
	const fresh = await api.global.get<DiscoverResponse>(API.discover(), { signal });
	if (!discoverHasContent(fresh) && fresh.refreshing) {
		// lazy import: keep the browser-only QueryClient module out of the module graph
		// at load time (server-side unit tests mock @tanstack/svelte-query)
		const { queryClient } = await import('$lib/queries/QueryClient');
		const prev = queryClient.getQueryData<DiscoverResponse>(
			DiscoverQueryKeyFactory.discover(userId)
		);
		if (discoverHasContent(prev)) {
			return { ...prev, refreshing: true } as DiscoverResponse;
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

// While the backend is still building (`refreshing === true`), poll gently so the page
// fills in zone-by-zone as the background build completes; stop as soon as it settles.
const DISCOVER_REFRESHING_POLL_MS = 8_000;

export const getDiscoverQuery = () =>
	createQuery(() => ({
		staleTime: DISCOVER_REVALIDATE_MS,
		queryKey: DiscoverQueryKeyFactory.discover(authStore.user?.id),
		queryFn: ({ signal }) => fetchDiscover(authStore.user?.id, signal),
		refetchInterval: (query: { state: { data?: DiscoverResponse | undefined } }) =>
			query.state.data?.refreshing ? DISCOVER_REFRESHING_POLL_MS : false
	}));

export const getRadioQuery = (getParams: Getter<{ seedType: string; seedId: string }>) =>
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
		enabled: !!getParams().seedId
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
