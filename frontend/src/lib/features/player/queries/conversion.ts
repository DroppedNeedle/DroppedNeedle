import { ConnectAppsQueryKeyFactory } from '$lib/queries/featureKeys';
import type { components } from '$lib/api/v3/openapi';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { queryOptions } from '@tanstack/svelte-query';

export type Conversion = components['schemas']['Conversion'];

/** Whether the server converts a track the browser cannot play. The player reads it. */
export const getPlaybackConversionQueryOptions = () =>
	queryOptions({
		queryKey: ConnectAppsQueryKeyFactory.conversion(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/playback/conversion'), { signal })
	});
