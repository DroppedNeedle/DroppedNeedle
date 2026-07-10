import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API, CACHE_TTL } from '$lib/constants';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import type {
	PluginListResponse,
	PluginSettingsResponse,
	PluginSettingsValue,
	PluginTestResult,
	PluginToggleResponse
} from '$lib/types';

export const PluginQueryKeyFactory = {
	all: ['plugins'] as const,
	list: () => [...PluginQueryKeyFactory.all, 'list'] as const,
	settings: (id: string) => [...PluginQueryKeyFactory.all, 'settings', id] as const
};

const getPluginsQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: PluginQueryKeyFactory.list(),
		queryFn: ({ signal }) => api.global.get<PluginListResponse>(API.plugins.list(), { signal })
	});

export const getPluginsQuery = () => createQuery(() => getPluginsQueryOptions());

const getPluginSettingsQueryOptions = (id: string) =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: PluginQueryKeyFactory.settings(id),
		queryFn: ({ signal }) =>
			api.global.get<PluginSettingsResponse>(API.plugins.settings(id), { signal })
	});

export const getPluginSettingsQuery = (id: () => string) =>
	createQuery(() => getPluginSettingsQueryOptions(id()));

async function invalidatePlugins(id?: string) {
	await invalidateQueriesWithPersister({ queryKey: PluginQueryKeyFactory.list() });
	if (id) {
		await invalidateQueriesWithPersister({ queryKey: PluginQueryKeyFactory.settings(id) });
	}
}

export function togglePluginMutation() {
	return createMutation(() => ({
		mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
			api.global.post<PluginToggleResponse>(
				enabled ? API.plugins.enable(id) : API.plugins.disable(id),
				{}
			),
		onSuccess: (_data: PluginToggleResponse, vars: { id: string; enabled: boolean }) =>
			invalidatePlugins(vars.id)
	}));
}

export function savePluginSettingsMutation() {
	return createMutation(() => ({
		mutationFn: ({ id, values }: { id: string; values: Record<string, PluginSettingsValue> }) =>
			api.global.put<PluginSettingsResponse>(API.plugins.settings(id), { values }),
		onSuccess: (
			_data: PluginSettingsResponse,
			vars: { id: string; values: Record<string, PluginSettingsValue> }
		) => invalidatePlugins(vars.id)
	}));
}

export function testPluginMutation() {
	return createMutation(() => ({
		mutationFn: ({ id, values }: { id: string; values?: Record<string, PluginSettingsValue> }) =>
			api.global.post<PluginTestResult>(API.plugins.test(id), { values: values ?? {} })
	}));
}
