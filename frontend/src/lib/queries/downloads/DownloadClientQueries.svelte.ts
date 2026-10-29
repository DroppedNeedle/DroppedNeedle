import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';
import {
	createAppMutation,
	downloadReadinessChanged,
	slskdSettingsChanged
} from '$lib/queries/effects';

export type DownloadClientConfig = components['schemas']['SlskdConnection'];

const getDownloadClientConfigQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.clientConfig(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.slskdConfig(), { signal })
	});

export const getDownloadClientConfigQuery = () =>
	createQuery(() => getDownloadClientConfigQueryOptions());

const getDownloadClientStatusQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.clientStatus(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.slskdStatus(), { signal })
	});

export const getDownloadClientStatusQuery = () =>
	createQuery(() => getDownloadClientStatusQueryOptions());

export function saveDownloadClientConfig() {
	return createAppMutation(() => ({
		mutationFn: (config: DownloadClientConfig) =>
			api.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.slskdConfig(), config),
		// Home reads integration_status.download_client; refresh it or its
		// "Configure Download Client" prompt lingers until the cache expires
		effects: () => [slskdSettingsChanged(), downloadReadinessChanged()]
	}));
}

export function testDownloadClient() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (config: DownloadClientConfig) =>
			api.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.slskdTest(), config)
	}));
}
