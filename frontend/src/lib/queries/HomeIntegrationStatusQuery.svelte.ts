import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { ttl } from '$lib/stores/cacheTtl.svelte';

import { HomeQueryKeyFactory } from './HomeQueryKeyFactory';
import { HOME_ENDPOINTS } from './charts/endpoints';

export const getIntegrationStatusQuery = () =>
	createQuery(() => ({
		staleTime: ttl('home', CACHE_TTL.HOME),
		queryKey: HomeQueryKeyFactory.integrationStatus(authStore.user?.id),
		enabled: Boolean(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(HOME_ENDPOINTS.integrationStatus(), { signal })
	}));
