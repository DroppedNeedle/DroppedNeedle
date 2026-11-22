import { v3 } from '$lib/api/v3/endpoint';
import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import {
	createAppMutation,
	deemixSettingsChanged,
	downloadPolicyChanged,
	downloadReadinessChanged,
	sourcePriorityChanged,
	usenetSettingsChanged,
	wantedSettingsChanged
} from '$lib/queries/effects';

export type DownloadPolicySettings = components['schemas']['DownloadPolicyView'];
export type SabnzbdConnectionSettings = components['schemas']['SabnzbdConnection'];
export type NzbgetConnectionSettings = components['schemas']['NzbgetConnection'];
export type UsenetClientTestResult = components['schemas']['UsenetClientTestResponse'];
export type DeemixConnectionSettings = components['schemas']['DeemixConnection'];
export type DeemixStatus = components['schemas']['DeemixStatusResponse'];
export type DeemixQuality = components['schemas']['DeemixQuality'];
export type DeemixWarning = components['schemas']['DeemixWarning'];
export type WantedWatcherSettings = components['schemas']['WantedWatcher'];

const sourcePriorityOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.sourcePriority(),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/settings/download-clients/source-priority'), { signal })
	});

export const getSourcePriorityQuery = () => createQuery(() => sourcePriorityOptions());

export function createSaveSourcePriorityMutation() {
	return createAppMutation(() => ({
		mutationFn: (order: string[]) =>
			api.v3.PUT(v3('/api/v3/settings/download-clients/source-priority'), { order }),
		effects: () => [sourcePriorityChanged()]
	}));
}

const sabnzbdOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.sabnzbd(),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/settings/download-clients/sabnzbd'), {
				signal
			})
	});

export const getSabnzbdConfigQuery = () => createQuery(() => sabnzbdOptions());

const sabnzbdStatusOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.sabnzbdStatus(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/acquire/sabnzbd/status'), { signal })
	});

export const getSabnzbdStatusQuery = () => createQuery(() => sabnzbdStatusOptions());

const nzbgetOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.nzbget(),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/settings/download-clients/nzbget'), {
				signal
			})
	});

export const getNzbgetConfigQuery = () => createQuery(() => nzbgetOptions());

const nzbgetStatusOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.nzbgetStatus(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/acquire/nzbget/status'), { signal })
	});

export const getNzbgetStatusQuery = () => createQuery(() => nzbgetStatusOptions());

const deemixOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.deemix(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/download-clients/deemix'), { signal })
	});

export const getDeemixConfigQuery = () => createQuery(() => deemixOptions());

const deemixStatusOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.deemixStatus(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/acquire/deemix/status'), { signal })
	});

export const getDeemixStatusQuery = () => createQuery(() => deemixStatusOptions());

const policyOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.policy(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/download-clients/policy'), { signal })
	});

// enabled-getter so non-admin pages can render without firing the admin-only
// policy endpoint (it 403s for plain users)
export const getDownloadPolicyQuery = (getEnabled: () => boolean = () => true) =>
	createQuery(() => ({ ...policyOptions(), enabled: getEnabled() }));

export function createSaveSabnzbdConfigMutation() {
	return createAppMutation(() => ({
		mutationFn: (config: SabnzbdConnectionSettings) =>
			api.v3.PUT(v3('/api/v3/settings/download-clients/sabnzbd'), config),
		effects: () => [usenetSettingsChanged(), downloadReadinessChanged()]
	}));
}

export function createTestSabnzbdMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (config: SabnzbdConnectionSettings) =>
			api.v3.POST(v3('/api/v3/settings/download-clients/sabnzbd/test'), config)
	}));
}

export function createSaveNzbgetConfigMutation() {
	return createAppMutation(() => ({
		mutationFn: (config: NzbgetConnectionSettings) =>
			api.v3.PUT(v3('/api/v3/settings/download-clients/nzbget'), config),
		effects: () => [usenetSettingsChanged(), downloadReadinessChanged()]
	}));
}

export function createTestNzbgetMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (config: NzbgetConnectionSettings) =>
			api.v3.POST(v3('/api/v3/settings/download-clients/nzbget/test'), config)
	}));
}

export function createSaveDeemixConfigMutation() {
	return createAppMutation(() => ({
		mutationFn: (config: DeemixConnectionSettings) =>
			api.v3.PUT(v3('/api/v3/settings/download-clients/deemix'), config),
		effects: () => [deemixSettingsChanged(), downloadReadinessChanged()]
	}));
}

export function createTestDeemixMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (config: DeemixConnectionSettings) =>
			api.v3.POST(v3('/api/v3/settings/download-clients/deemix/test'), config)
	}));
}

export function createSaveDownloadPolicyMutation() {
	return createAppMutation(() => ({
		mutationFn: (policy: DownloadPolicySettings) =>
			api.v3.PUT(v3('/api/v3/settings/download-clients/policy'), policy),
		effects: () => [downloadPolicyChanged()]
	}));
}

const wantedSettingsOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.wantedSettings(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/download-clients/wanted'), { signal })
	});

export const getWantedSettingsQuery = () => createQuery(() => wantedSettingsOptions());

export function createSaveWantedSettingsMutation() {
	return createAppMutation(() => ({
		mutationFn: (settings: WantedWatcherSettings) =>
			api.v3.PUT(v3('/api/v3/settings/download-clients/wanted'), settings),
		effects: () => [wantedSettingsChanged()]
	}));
}
