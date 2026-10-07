import { createMutation, createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { userIdSegment } from '../userKeySegment';
import { ADMIN_CACHE_ENDPOINTS } from './endpoints';

export type CacheClearBody = components['schemas']['CacheClearBody'];

export const AdminCacheQueryKeyFactory = {
	prefix: ['admin', 'cache'] as const,
	stats: (userId: string | null | undefined) =>
		[...AdminCacheQueryKeyFactory.prefix, userIdSegment(userId), 'stats'] as const
};

export const getCacheStatsQuery = () =>
	createQuery(() => ({
		enabled: authStore.isAdmin,
		queryKey: AdminCacheQueryKeyFactory.stats(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(ADMIN_CACHE_ENDPOINTS.stats(), { signal })
	}));

export const createPrecacheRunMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.v3.POST(ADMIN_CACHE_ENDPOINTS.precacheRun())
	}));

export const createClearCacheMutation = () =>
	createMutation(() => ({
		mutationFn: (body: CacheClearBody) => api.global.v3.POST(ADMIN_CACHE_ENDPOINTS.clear(), body),
		onSettled: () =>
			invalidateQueriesWithPersister({
				queryKey: AdminCacheQueryKeyFactory.stats(authStore.user?.id)
			})
	}));
