import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import type { PluginSourceHealth, PluginSourceInfo, PluginSourcesResponse } from '$lib/types';

import { PluginQueryKeyFactory } from './PluginQueryKeyFactory';
import { PLUGIN_ENDPOINTS } from './endpoints';

const HEALTH: readonly PluginSourceHealth[] = ['ok', 'degraded', 'error', 'unknown'];

function toSourceInfo(source: components['schemas']['PluginSource']): PluginSourceInfo {
	return {
		key: source.key,
		plugin: source.plugin ?? '',
		display_name: source.display_name ?? source.key,
		has_client: source.has_client ?? false,
		has_indexer: source.has_indexer ?? false,
		target_source: source.target_source ?? '',
		configured: source.configured ?? false,
		health: HEALTH.find((health) => health === source.health) ?? 'unknown'
	};
}

export async function fetchPluginSources(signal?: AbortSignal): Promise<PluginSourcesResponse> {
	const data = await api.global.v3.GET(PLUGIN_ENDPOINTS.sources(), { signal });
	return { sources: data.sources.map(toSourceInfo) };
}

const getPluginSourcesQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: PluginQueryKeyFactory.sources(authStore.user?.id),
		queryFn: ({ signal }) => fetchPluginSources(signal)
	});

// Curator-visible plugin acquisition sources (user-level endpoint, so this
// query runs for every role and labels priority/review surfaces).
export const getPluginSourcesQuery = () => createQuery(() => getPluginSourcesQueryOptions());
