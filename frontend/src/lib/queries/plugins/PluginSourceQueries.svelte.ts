import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';

import { PluginQueryKeyFactory } from './PluginQueryKeyFactory';
import { PLUGIN_ENDPOINTS } from './endpoints';

type PluginSource = components['schemas']['PluginSource'];

// The server leaves a plugin's optional fields out; the screens read them
// filled in, with an unknown health value read as unknown.
export type PluginSourceHealth = 'ok' | 'degraded' | 'error' | 'unknown';
export type PluginSourceInfo = Required<Omit<PluginSource, 'health'>> & {
	health: PluginSourceHealth;
};
export interface PluginSourceList {
	sources: PluginSourceInfo[];
}

const HEALTH: readonly PluginSourceHealth[] = ['ok', 'degraded', 'error', 'unknown'];

function toSourceInfo(source: PluginSource): PluginSourceInfo {
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

export async function fetchPluginSources(signal?: AbortSignal): Promise<PluginSourceList> {
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
