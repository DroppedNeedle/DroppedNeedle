import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';

import { PluginQueryKeyFactory } from './PluginQueryKeyFactory';
import type { PluginListResponse, SourceListResponse, SourceSearchResponse } from './types';

type Getter<T> = () => T;

// Admin-only: the Settings -> Plugins roster.
export const getPluginsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: PluginQueryKeyFactory.list(),
		queryFn: ({ signal }) => api.global.get<PluginListResponse>(API.plugins.list(), { signal }),
		enabled: getEnabled()
	}));

// Curator: which enabled plugins offer an audio source (gates the Import-tab search).
export const getSourcePluginsQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		queryKey: PluginQueryKeyFactory.sources(),
		queryFn: ({ signal }) => api.global.get<SourceListResponse>(API.plugins.sources(), { signal }),
		enabled: getEnabled()
	}));

// Curator: search one source plugin. Keyed by plugin+query; short staleTime -
// external sources change and there's no invalidation signal for them.
export const getSourceSearchQuery = (getPlugin: Getter<string>, getQuery: Getter<string>) =>
	createQuery(() => {
		const plugin = getPlugin();
		const query = getQuery().trim();
		return {
			queryKey: PluginQueryKeyFactory.sourceSearch(plugin, query),
			enabled: !!plugin && query.length >= 2,
			staleTime: 60 * 1000,
			queryFn: ({ signal }) =>
				api.global.post<SourceSearchResponse>(
					API.plugins.sourceSearch(plugin),
					{ query },
					{ signal }
				)
		};
	});
