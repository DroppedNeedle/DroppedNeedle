import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';

import { PluginQueryKeyFactory } from './PluginQueryKeyFactory';
import { PLUGIN_ENDPOINTS } from './endpoints';

type Getter<T> = () => T;

// Admin-only: the Settings -> Plugins roster.
export const getPluginsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: PluginQueryKeyFactory.list(),
		queryFn: ({ signal }) => api.global.v3.GET(PLUGIN_ENDPOINTS.list(), { signal }),
		enabled: getEnabled()
	}));
