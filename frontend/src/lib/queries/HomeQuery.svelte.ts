import { api } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { ttl } from '$lib/stores/cacheTtl.svelte';
import type { HomeResponse } from '$lib/types';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { toHomeResponse } from './chartAdapters';
import { HOME_ENDPOINTS } from './charts/endpoints';
import { HomeQueryKeyFactory } from './HomeQueryKeyFactory';

function sectionCount(d: HomeResponse | null | undefined): number {
	if (!d) return 0;
	const sections = [
		d.recently_added,
		d.library_artists,
		d.library_albums,
		d.trending_artists,
		d.popular_albums,
		d.recently_played,
		d.genre_list,
		d.favorite_artists,
		d.your_top_albums,
		d.weekly_exploration
	];
	return sections.filter((s) => s != null).length;
}

// keep the richer cached response while the server finishes its SWR rebuild
async function fetchHome(
	userId: string | null | undefined,
	signal?: AbortSignal
): Promise<HomeResponse> {
	const fresh = toHomeResponse(
		await api.global.v3.GET(HOME_ENDPOINTS.home(), { signal, timeoutMs: 15_000 })
	);
	if (fresh.refreshing) {
		const { queryClient } = await import('$lib/queries/QueryClient');
		const prev = queryClient.getQueryData<HomeResponse>(HomeQueryKeyFactory.home(userId));
		if (prev && sectionCount(prev) > sectionCount(fresh)) {
			return { ...prev, refreshing: true };
		}
	}
	return fresh;
}

export const getHomeQueryOptions = (userId: string | null | undefined) =>
	queryOptions({
		staleTime: ttl('home', CACHE_TTL.HOME),
		queryKey: HomeQueryKeyFactory.home(userId),
		queryFn: ({ signal }) => fetchHome(userId, signal)
	});

export const getHomeQuery = () =>
	createQuery(() => ({
		...getHomeQueryOptions(authStore.user?.id),
		refetchInterval: (query: { state: { data?: HomeResponse | undefined } }) =>
			query.state.data?.refreshing ? 10_000 : false
	}));
