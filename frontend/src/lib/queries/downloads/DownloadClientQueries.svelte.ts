import { v3 } from '$lib/api/v3/endpoint';
import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import {
	createAppMutation,
	downloadReadinessChanged,
	slskdSettingsChanged
} from '$lib/queries/effects';

export type DownloadClientConfig = components['schemas']['SlskdConnection'];

const getDownloadClientConfigQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.clientConfig(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/download-client/config'), { signal })
	});

export const getDownloadClientConfigQuery = () =>
	createQuery(() => getDownloadClientConfigQueryOptions());

const getDownloadClientStatusQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.clientStatus(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/acquire/slskd/status'), { signal })
	});

export const getDownloadClientStatusQuery = () =>
	createQuery(() => getDownloadClientStatusQueryOptions());

export function createSaveDownloadClientConfigMutation() {
	return createAppMutation(() => ({
		mutationFn: (config: DownloadClientConfig) =>
			api.v3.PUT(v3('/api/v3/settings/download-client/config'), config),
		// Home reads integration_status.download_client; refresh it or its
		// "Configure Download Client" prompt lingers until the cache expires
		effects: () => [slskdSettingsChanged(), downloadReadinessChanged()]
	}));
}

export function createTestDownloadClientMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (config: DownloadClientConfig) =>
			api.v3.POST(v3('/api/v3/settings/download-client/test'), config)
	}));
}
