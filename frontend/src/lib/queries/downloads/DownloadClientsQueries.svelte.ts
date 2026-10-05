import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';

export type DownloadPolicySettings = components['schemas']['DownloadPolicyView'];
export type SabnzbdConnectionSettings = components['schemas']['SabnzbdConnection'];
export type SabnzbdStatus = components['schemas']['SabnzbdStatusResponse'];
export type SabnzbdTestResult = components['schemas']['SabnzbdTestResponse'];
export type SourcePriority = components['schemas']['SourcePriorityOrder'];
export type WantedWatcherSettings = components['schemas']['WantedWatcher'];

const sourcePriorityOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: [...DownloadQueryKeyFactory.all, 'source-priority'] as const,
		queryFn: ({ signal }) =>
			api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.sourcePriority(), { signal })
	});

export const getSourcePriorityQuery = () => createQuery(() => sourcePriorityOptions());

export function saveSourcePriority() {
	return createMutation(() => ({
		mutationFn: (order: string[]) =>
			api.global.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.sourcePriority(), { order }),
		onSuccess: () =>
			invalidateQueriesWithPersister({
				queryKey: [...DownloadQueryKeyFactory.all, 'source-priority']
			})
	}));
}

const sabnzbdOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.sabnzbd(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.sabnzbdConfig(), {
				signal
			})
	});

export const getSabnzbdConfigQuery = () => createQuery(() => sabnzbdOptions());

const sabnzbdStatusOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.sabnzbdStatus(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.sabnzbdStatus(), { signal })
	});

export const getSabnzbdStatusQuery = () => createQuery(() => sabnzbdStatusOptions());

const policyOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.policy(),
		queryFn: ({ signal }) => api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.policy(), { signal })
	});

// enabled-getter so non-admin pages can render without firing the admin-only
// policy endpoint (it 403s for plain users)
export const getDownloadPolicyQuery = (getEnabled: () => boolean = () => true) =>
	createQuery(() => ({ ...policyOptions(), enabled: getEnabled() }));

async function invalidateClients() {
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.sabnzbd() });
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.sabnzbdStatus() });
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.clientStatus() });
	await invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix });
}

export function saveSabnzbdConfig() {
	return createMutation(() => ({
		mutationFn: (config: SabnzbdConnectionSettings) =>
			api.global.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.sabnzbdConfig(), config),
		onSuccess: invalidateClients
	}));
}

export function testSabnzbd() {
	return createMutation(() => ({
		mutationFn: (config: SabnzbdConnectionSettings) =>
			api.global.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.sabnzbdTest(), config)
	}));
}

export function saveDownloadPolicy() {
	return createMutation(() => ({
		mutationFn: (policy: DownloadPolicySettings) =>
			api.global.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.policy(), policy),
		onSuccess: async () => {
			await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.policy() });
			await invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.policySummary()
			});
		}
	}));
}

const wantedSettingsOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.wantedSettings(),
		queryFn: ({ signal }) => api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.wanted(), { signal })
	});

export const getWantedSettingsQuery = () => createQuery(() => wantedSettingsOptions());

export function saveWantedSettings() {
	return createMutation(() => ({
		mutationFn: (settings: WantedWatcherSettings) =>
			api.global.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.wanted(), settings),
		onSuccess: () =>
			invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.wantedSettings() })
	}));
}
