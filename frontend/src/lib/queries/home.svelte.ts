import { HomeQueryKeyFactory } from '$lib/queries/featureKeys';

import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { STALE_SLOW } from '$lib/queries/staleTime';
import { authStore } from '$lib/stores/authStore.svelte';
import type { HomeResponse } from '$lib/queries/discover/types';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { PERSIST_PROVIDER } from '$lib/queries/persistedCache';
import type { components } from '$lib/api/v3/openapi';
import { queryClient } from '$lib/queries/QueryClient';

function sectionCount(d: HomeResponse | null | undefined): number {
	return d?.sections.length ?? 0;
}

// keep the richer cached response while the server finishes its SWR rebuild
async function fetchHome(
	userId: string | null | undefined,
	signal?: AbortSignal
): Promise<HomeResponse> {
	const fresh = await api.v3.GET(v3('/api/v3/home'), { signal, timeoutMs: 15_000 });
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
		staleTime: STALE_SLOW,
		meta: PERSIST_PROVIDER,
		queryKey: HomeQueryKeyFactory.home(userId),
		queryFn: ({ signal }) => fetchHome(userId, signal)
	});

export const getHomeQuery = () =>
	createQuery(() => ({
		...getHomeQueryOptions(authStore.user?.id),
		refetchInterval: (query: { state: { data?: HomeResponse | undefined } }) =>
			query.state.data?.refreshing ? 10_000 : false
	}));

export type IntegrationStatus = components['schemas']['IntegrationStatus'];

const NONE_SET_UP: IntegrationStatus = {
	download_client: false,
	jellyfin: false,
	lastfm: false,
	library: false,
	listenbrainz: false,
	localfiles: false,
	navidrome: false,
	plex: false,
	youtube: false,
	youtube_api: false
};

const getIntegrationStatusQueryOptions = (userId: string | null | undefined) =>
	queryOptions({
		queryKey: HomeQueryKeyFactory.integrationStatus(userId),
		enabled: Boolean(userId),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/home/integration-status'), { signal })
	});

// Loads the status. The app shell holds one for the whole session, so every
// reader below shares its answer; it goes through the app's client so a part
// rendered under another provider still sees the same copy.
export const getIntegrationStatusQuery = () =>
	createQuery(
		() => getIntegrationStatusQueryOptions(authStore.user?.id),
		() => queryClient
	);

/**
 * Which integrations are set up, all off until the first answer. `download_client`
 * means any acquisition source (slskd, Usenet or a plugin), as the server decides.
 * It only reads what the shell's query loaded.
 */
export function useIntegrationStatus() {
	const query = createQuery(
		() => ({ ...getIntegrationStatusQueryOptions(authStore.user?.id), enabled: false }),
		() => queryClient
	);
	const status = $derived({ ...NONE_SET_UP, ...query.data, loaded: query.data !== undefined });
	return {
		get current() {
			return status;
		}
	};
}

/** The cached answer for code outside a component; undefined before the first load. */
export function readIntegrationStatus(): IntegrationStatus | undefined {
	return queryClient.getQueryData<IntegrationStatus>(
		HomeQueryKeyFactory.integrationStatus(authStore.user?.id)
	);
}
