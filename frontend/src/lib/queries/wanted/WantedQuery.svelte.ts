import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { WantedQueryKeyFactory } from './WantedQueryKeyFactory';
import { WANTED_ENDPOINTS } from './endpoints';
import type { WantedWatchesResponse } from './types';

// Watch rows change only when the background sweep runs; SSE wanted_* events
// invalidate for those. The read-only "still hunting" rows move on the retry
// ladder's minutes-scale though, so a slow poll while the tab is open keeps
// their attempt counters honest.
export const getWantedWatchesQuery = (getEnabled: () => boolean) =>
	createQuery(() => ({
		queryKey: WantedQueryKeyFactory.list(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.get<WantedWatchesResponse>(WANTED_ENDPOINTS.list(), { signal }),
		enabled: getEnabled() && !!authStore.user?.id,
		refetchInterval: 30_000
	}));
