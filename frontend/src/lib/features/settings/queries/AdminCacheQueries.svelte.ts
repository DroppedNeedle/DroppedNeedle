import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { SettingsQueryKeyFactory } from './SettingsQueryKeyFactory';
import { applyEffects, cacheStatsChanged, createAppMutation } from '$lib/queries/effects';

export type CacheClearBody = components['schemas']['CacheClearBody'];

export const getCacheStatsQuery = () =>
	createQuery(() => ({
		enabled: authStore.isAdmin,
		queryKey: SettingsQueryKeyFactory.cacheStats(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/admin/cache/stats'), { signal })
	}));

export const createPrecacheRunMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: () => api.v3.POST(v3('/api/v3/admin/precache/run'))
	}));

export const createClearCacheMutation = () =>
	createAppMutation(() => ({
		mutationFn: (body: CacheClearBody) => api.v3.POST(v3('/api/v3/admin/cache/clear'), body),
		effects: () => [cacheStatsChanged(authStore.user?.id)],
		onError: () => applyEffects(cacheStatsChanged(authStore.user?.id))
	}));
