import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';

import { PluginQueryKeyFactory } from './PluginQueryKeyFactory';
import { PLUGIN_ENDPOINTS } from './endpoints';
import type { PluginFilesPage } from './types';

type Getter<T> = () => T;

// Admin-only: the Settings -> Plugins roster.
export const getPluginsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: PluginQueryKeyFactory.list(),
		queryFn: ({ signal }) => api.global.v3.GET(PLUGIN_ENDPOINTS.list(), { signal }),
		enabled: getEnabled()
	}));

// Admin-only: the files one plugin added to the library, newest first.
export const getPluginFilesQuery = (getName: Getter<string>) =>
	createInfiniteQuery(() => {
		const name = getName();
		return {
			queryKey: PluginQueryKeyFactory.files(name),
			initialPageParam: undefined as string | undefined,
			queryFn: ({ pageParam, signal }) =>
				api.global.v3.GET(PLUGIN_ENDPOINTS.files(name, pageParam), { signal }),
			getNextPageParam: (lastPage: PluginFilesPage) => lastPage.next_cursor ?? undefined
		};
	});
