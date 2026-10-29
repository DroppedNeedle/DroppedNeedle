import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';

import { HomeQueryKeyFactory } from './HomeQueryKeyFactory';
import { queryClient } from './QueryClient';
import { HOME_ENDPOINTS } from './charts/endpoints';

export type IntegrationStatus = components['schemas']['IntegrationStatus'];

const NONE_SET_UP: IntegrationStatus = {
	download_client: false,
	jellyfin: false,
	lastfm: false,
	library: false,
	listenbrainz: false,
	localfiles: false,
	navidrome: false,
	plex: false,
	youtube: false,
	youtube_api: false
};

export const getIntegrationStatusQueryOptions = (userId: string | null | undefined) =>
	queryOptions({
		queryKey: HomeQueryKeyFactory.integrationStatus(userId),
		enabled: Boolean(userId),
		queryFn: ({ signal }) => api.global.v3.GET(HOME_ENDPOINTS.integrationStatus(), { signal })
	});

// Loads the status. The app shell holds one for the whole session, so every
// reader below shares its answer; it goes through the app's client so a part
// rendered under another provider still sees the same copy.
export const getIntegrationStatusQuery = () =>
	createQuery(
		() => getIntegrationStatusQueryOptions(authStore.user?.id),
		() => queryClient
	);

/**
 * Which integrations are set up, all off until the first answer. `download_client`
 * means any acquisition source (slskd, Usenet or a plugin), as the server decides.
 * It only reads what the shell's query loaded.
 */
export function useIntegrationStatus() {
	const query = createQuery(
		() => ({ ...getIntegrationStatusQueryOptions(authStore.user?.id), enabled: false }),
		() => queryClient
	);
	const status = $derived({ ...NONE_SET_UP, ...query.data, loaded: query.data !== undefined });
	return {
		get current() {
			return status;
		}
	};
}

/** The cached answer for code outside a component; undefined before the first load. */
export function readIntegrationStatus(): IntegrationStatus | undefined {
	return queryClient.getQueryData<IntegrationStatus>(
		HomeQueryKeyFactory.integrationStatus(authStore.user?.id)
	);
}
