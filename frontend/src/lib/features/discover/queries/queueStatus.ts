import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { PROVIDER } from '$lib/queries/persistedCache';
import { STALE_LIVE } from '$lib/queries/staleTime';
import { authStore } from '$lib/stores/authStore.svelte';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

export type QueueStatus = components['schemas']['DiscoverQueueStatusResponse'];

/** How many of the caller's picks are waiting, for Home's way into Discover. */
export const getDiscoverQueueStatusQuery = () =>
	createQuery(() => ({
		meta: PROVIDER,
		queryKey: DiscoverQueryKeyFactory.v3.queueStatus(authStore.user?.id),
		queryFn: ({ signal }): Promise<QueueStatus> =>
			api.v3.GET(v3('/api/v3/discover/queue/status'), { signal }),
		enabled: Boolean(authStore.user?.id),
		staleTime: STALE_LIVE
	}));
