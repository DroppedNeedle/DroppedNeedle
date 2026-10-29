import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { SettingsQueryKeyFactory } from './SettingsQueryKeyFactory';
import { ADMIN_CACHE_ENDPOINTS } from './endpoints';
import { applyEffects, cacheStatsChanged, createAppMutation } from '$lib/queries/effects';

export type CacheClearBody = components['schemas']['CacheClearBody'];

export const getCacheStatsQuery = () =>
	createQuery(() => ({
		enabled: authStore.isAdmin,
		queryKey: SettingsQueryKeyFactory.cacheStats(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(ADMIN_CACHE_ENDPOINTS.stats(), { signal })
	}));

export const createPrecacheRunMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: () => api.v3.POST(ADMIN_CACHE_ENDPOINTS.precacheRun())
	}));

export const createClearCacheMutation = () =>
	createAppMutation(() => ({
		mutationFn: (body: CacheClearBody) => api.v3.POST(ADMIN_CACHE_ENDPOINTS.clear(), body),
		effects: () => [cacheStatsChanged(authStore.user?.id)],
		onError: () => applyEffects(cacheStatsChanged(authStore.user?.id))
	}));
