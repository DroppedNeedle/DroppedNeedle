import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';
import {
	createAppMutation,
	downloadReadinessChanged,
	usenetSettingsChanged
} from '$lib/queries/effects';

export type ProwlarrConnectionSettings = components['schemas']['ProwlarrConnection'];

const getProwlarrConfigQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.prowlarr(),
		queryFn: ({ signal }) =>
			api.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.prowlarrConfig(), {
				signal
			})
	});

export const getProwlarrConfigQuery = () => createQuery(() => getProwlarrConfigQueryOptions());

export function saveProwlarrConfigMutation() {
	return createAppMutation(() => ({
		mutationFn: (connection: ProwlarrConnectionSettings) =>
			api.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.prowlarrConfig(), connection),
		// A Prowlarr save flips Usenet readiness, so status surfaces and Home
		// (integration_status) refresh alongside the config.
		effects: () => [usenetSettingsChanged(), downloadReadinessChanged()]
	}));
}

export function testProwlarrMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (connection: ProwlarrConnectionSettings) =>
			api.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.prowlarrTest(), connection)
	}));
}
