import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API, CACHE_TTL } from '$lib/constants';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import type {
	DownloadPolicySettings,
	SabnzbdConnectionSettings,
	SabnzbdTestResult,
	SourcePriority,
	WantedWatcherSettings
} from '$lib/types';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';

const sourcePriorityOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: [...DownloadQueryKeyFactory.all, 'source-priority'] as const,
		queryFn: ({ signal }) =>
			api.global.get<SourcePriority>(API.downloadClients.sourcePriority(), { signal })
	});

export const getSourcePriorityQuery = () => createQuery(() => sourcePriorityOptions());

export function saveSourcePriority() {
	return createMutation(() => ({
		mutationFn: (order: string[]) =>
			api.global.put<SourcePriority>(API.downloadClients.sourcePriority(), { order }),
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
			api.global.get<SabnzbdConnectionSettings>(API.downloadClients.sabnzbd(), { signal })
	});

export const getSabnzbdConfigQuery = () => createQuery(() => sabnzbdOptions());

const policyOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.policy(),
		queryFn: ({ signal }) =>
			api.global.get<DownloadPolicySettings>(API.downloadClients.policy(), { signal })
	});

// enabled-getter so non-admin pages can render without firing the admin-only
// policy endpoint (it 403s for plain users)
export const getDownloadPolicyQuery = (getEnabled: () => boolean = () => true) =>
	createQuery(() => ({ ...policyOptions(), enabled: getEnabled() }));

async function invalidateClients() {
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.sabnzbd() });
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.clientStatus() });
	await invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix });
}

export function saveSabnzbdConfig() {
	return createMutation(() => ({
		mutationFn: (config: SabnzbdConnectionSettings) =>
			api.global.put<SabnzbdConnectionSettings>(API.downloadClients.sabnzbd(), config),
		onSuccess: invalidateClients
	}));
}

export function testSabnzbd() {
	return createMutation(() => ({
		mutationFn: (config: SabnzbdConnectionSettings) =>
			api.global.post<SabnzbdTestResult>(API.downloadClients.sabnzbdTest(), config)
	}));
}

export function saveDownloadPolicy() {
	return createMutation(() => ({
		mutationFn: (policy: DownloadPolicySettings) =>
			api.global.put<DownloadPolicySettings>(API.downloadClients.policy(), policy),
		onSuccess: () => invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.policy() })
	}));
}

const wantedSettingsOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.wantedSettings(),
		queryFn: ({ signal }) =>
			api.global.get<WantedWatcherSettings>(API.downloadClients.wanted(), { signal })
	});

export const getWantedSettingsQuery = () => createQuery(() => wantedSettingsOptions());

export function saveWantedSettings() {
	return createMutation(() => ({
		mutationFn: (settings: WantedWatcherSettings) =>
			api.global.put<WantedWatcherSettings>(API.downloadClients.wanted(), settings),
		onSuccess: () =>
			invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.wantedSettings() })
	}));
}
