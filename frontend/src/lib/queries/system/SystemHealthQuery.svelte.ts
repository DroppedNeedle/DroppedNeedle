import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { SYSTEM_ENDPOINTS } from './endpoints';
import { SystemQueryKeyFactory } from './SystemQueryKeyFactory';

/** Which external services are currently degraded - drives the header health dot.
 * Polls on a slow cadence (a service outage is minutes-scale, not seconds) and on
 * window focus so a returning user sees current state. Not user-specific. */
export const getSystemHealthQuery = () =>
	createQuery(() => ({
		queryKey: SystemQueryKeyFactory.health(),
		queryFn: ({ signal }) => api.global.v3.GET(SYSTEM_ENDPOINTS.health(), { signal }),
		refetchInterval: 60_000,
		refetchOnWindowFocus: true,
		staleTime: 30_000
	}));
