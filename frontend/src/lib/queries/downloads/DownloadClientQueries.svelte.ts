import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';

export type DownloadClientConfig = components['schemas']['SlskdConnection'];

const getDownloadClientConfigQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.clientConfig(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.slskdConfig(), { signal })
	});

export const getDownloadClientConfigQuery = () =>
	createQuery(() => getDownloadClientConfigQueryOptions());

const getDownloadClientStatusQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.clientStatus(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.slskdStatus(), { signal })
	});

export const getDownloadClientStatusQuery = () =>
	createQuery(() => getDownloadClientStatusQueryOptions());

export function saveDownloadClientConfig() {
	return createMutation(() => ({
		mutationFn: (config: DownloadClientConfig) =>
			api.global.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.slskdConfig(), config),
		onSuccess: async () => {
			await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.clientConfig() });
			await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.clientStatus() });
			// Home reads integration_status.download_client; invalidate or its
			// "Configure Download Client" prompt lingers until the cache expires
			await invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix });
		}
	}));
}

export function testDownloadClient() {
	return createMutation(() => ({
		mutationFn: (config: DownloadClientConfig) =>
			api.global.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.slskdTest(), config)
	}));
}
