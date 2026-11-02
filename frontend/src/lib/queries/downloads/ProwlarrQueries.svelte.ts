import { v3 } from '$lib/api/v3/endpoint';
import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
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
			api.v3.GET(v3('/api/v3/settings/prowlarr/config'), {
				signal
			})
	});

export const getProwlarrConfigQuery = () => createQuery(() => getProwlarrConfigQueryOptions());

export function createSaveProwlarrConfigMutation() {
	return createAppMutation(() => ({
		mutationFn: (connection: ProwlarrConnectionSettings) =>
			api.v3.PUT(v3('/api/v3/settings/prowlarr/config'), connection),
		// A Prowlarr save flips Usenet readiness, so status surfaces and Home
		// (integration_status) refresh alongside the config.
		effects: () => [usenetSettingsChanged(), downloadReadinessChanged()]
	}));
}

export function createTestProwlarrMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (connection: ProwlarrConnectionSettings) =>
			api.v3.POST(v3('/api/v3/settings/prowlarr/test'), connection)
	}));
}
