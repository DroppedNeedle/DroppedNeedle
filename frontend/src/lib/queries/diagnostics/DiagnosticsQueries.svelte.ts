import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';

import { DiagnosticsQueryKeyFactory } from './DiagnosticsQueryKeyFactory';

export const DIAGNOSTICS_ENDPOINTS = {
	queueStats: () => v3('/api/v3/admin/queue-stats'),
	providerStats: () => v3('/api/v3/admin/provider-stats')
} as const;

/** Gauges must stay fresh; polling is cheap, the server reads in-memory state. */
const POLL_INTERVAL_MS = 5_000;

/**
 * Background jobs and their wake-up channels. `enabled` comes from the
 * caller: a closed settings tab or hidden window must issue no requests.
 */
export const getQueueStatsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: DiagnosticsQueryKeyFactory.queueStats(),
		enabled: getEnabled(),
		staleTime: 0, // gauges must be live: never serve a persisted snapshot on re-entry
		refetchInterval: POLL_INTERVAL_MS,
		refetchIntervalInBackground: false,
		refetchOnWindowFocus: false,
		queryFn: ({ signal }) => api.global.v3.GET(DIAGNOSTICS_ENDPOINTS.queueStats(), { signal })
	}));

/** Provider rate limiters plus the outbound request slots. Same polling posture. */
export const getProviderStatsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: DiagnosticsQueryKeyFactory.providerStats(),
		enabled: getEnabled(),
		staleTime: 0,
		refetchInterval: POLL_INTERVAL_MS,
		refetchIntervalInBackground: false,
		refetchOnWindowFocus: false,
		queryFn: ({ signal }) => api.global.v3.GET(DIAGNOSTICS_ENDPOINTS.providerStats(), { signal })
	}));
