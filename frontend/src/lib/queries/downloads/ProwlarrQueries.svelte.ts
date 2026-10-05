import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';

export type ProwlarrConnectionSettings = components['schemas']['ProwlarrConnection'];
export type ProwlarrTestResult = components['schemas']['ProwlarrTestResponse'];

const getProwlarrConfigQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.prowlarr(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.prowlarrConfig(), {
				signal
			})
	});

export const getProwlarrConfigQuery = () => createQuery(() => getProwlarrConfigQueryOptions());

async function invalidateProwlarr() {
	// SABnzbd-shape sweep: a Prowlarr save flips is_usenet_ready(), so status
	// surfaces and Home (integration_status) must refresh alongside the config.
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.prowlarr() });
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.sabnzbd() });
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.sabnzbdStatus() });
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.clientStatus() });
	await invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix });
}

export function saveProwlarrConfigMutation() {
	return createMutation(() => ({
		mutationFn: (connection: ProwlarrConnectionSettings) =>
			api.global.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.prowlarrConfig(), connection),
		onSuccess: invalidateProwlarr
	}));
}

export function testProwlarrMutation() {
	return createMutation(() => ({
		mutationFn: (connection: ProwlarrConnectionSettings) =>
			api.global.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.prowlarrTest(), connection)
	}));
}
