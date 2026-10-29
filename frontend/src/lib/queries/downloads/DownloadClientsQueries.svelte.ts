import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';
import {
	createAppMutation,
	downloadPolicyChanged,
	downloadReadinessChanged,
	sourcePriorityChanged,
	usenetSettingsChanged,
	wantedSettingsChanged
} from '$lib/queries/effects';

export type DownloadPolicySettings = components['schemas']['DownloadPolicyView'];
export type SabnzbdConnectionSettings = components['schemas']['SabnzbdConnection'];
export type SabnzbdTestResult = components['schemas']['SabnzbdTestResponse'];
export type WantedWatcherSettings = components['schemas']['WantedWatcher'];

const sourcePriorityOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.sourcePriority(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.sourcePriority(), { signal })
	});

export const getSourcePriorityQuery = () => createQuery(() => sourcePriorityOptions());

export function saveSourcePriority() {
	return createAppMutation(() => ({
		mutationFn: (order: string[]) =>
			api.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.sourcePriority(), { order }),
		effects: () => [sourcePriorityChanged()]
	}));
}

const sabnzbdOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.sabnzbd(),
		queryFn: ({ signal }) =>
			api.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.sabnzbdConfig(), {
				signal
			})
	});

export const getSabnzbdConfigQuery = () => createQuery(() => sabnzbdOptions());

const sabnzbdStatusOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.sabnzbdStatus(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.sabnzbdStatus(), { signal })
	});

export const getSabnzbdStatusQuery = () => createQuery(() => sabnzbdStatusOptions());

const policyOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.policy(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.policy(), { signal })
	});

// enabled-getter so non-admin pages can render without firing the admin-only
// policy endpoint (it 403s for plain users)
export const getDownloadPolicyQuery = (getEnabled: () => boolean = () => true) =>
	createQuery(() => ({ ...policyOptions(), enabled: getEnabled() }));

export function saveSabnzbdConfig() {
	return createAppMutation(() => ({
		mutationFn: (config: SabnzbdConnectionSettings) =>
			api.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.sabnzbdConfig(), config),
		effects: () => [usenetSettingsChanged(), downloadReadinessChanged()]
	}));
}

export function testSabnzbd() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (config: SabnzbdConnectionSettings) =>
			api.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.sabnzbdTest(), config)
	}));
}

export function saveDownloadPolicy() {
	return createAppMutation(() => ({
		mutationFn: (policy: DownloadPolicySettings) =>
			api.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.policy(), policy),
		effects: () => [downloadPolicyChanged()]
	}));
}

const wantedSettingsOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.wantedSettings(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.wanted(), { signal })
	});

export const getWantedSettingsQuery = () => createQuery(() => wantedSettingsOptions());

export function saveWantedSettings() {
	return createAppMutation(() => ({
		mutationFn: (settings: WantedWatcherSettings) =>
			api.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.wanted(), settings),
		effects: () => [wantedSettingsChanged()]
	}));
}
