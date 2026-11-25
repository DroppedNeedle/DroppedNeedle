import { ADMIN, key } from '$lib/queries/keys';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import { STALE_LIVE } from '$lib/queries/staleTime';

/**
 * Admin-only runtime gauges. They describe the shared server process, the
 * same for every admin.
 */
export const DiagnosticsQueryKeyFactory = {
	all: () => key(ADMIN, 'diagnostics'),
	queueStats: () => key(ADMIN, 'diagnostics', 'queue-stats'),
	providerStats: () => key(ADMIN, 'diagnostics', 'provider-stats'),
	recentProblems: (reference: string) => key(ADMIN, 'diagnostics', 'recent-problems', reference)
};

/** Gauges must stay fresh; polling is cheap, the server reads in-memory state. */
const POLL_INTERVAL_MS = 5_000;

/**
 * Background jobs and the latest checkpoint pass. `enabled` comes from the
 * caller: a closed settings tab or hidden window must issue no requests.
 */
export const getQueueStatsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: DiagnosticsQueryKeyFactory.queueStats(),
		enabled: getEnabled(),
		staleTime: STALE_LIVE, // gauges must be live: never serve a persisted snapshot on re-entry
		refetchInterval: POLL_INTERVAL_MS,
		refetchIntervalInBackground: false,
		refetchOnWindowFocus: false,
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/admin/queue-stats'), { signal })
	}));

/** Rate limiters and outbound call counts. Same polling posture. */
export const getProviderStatsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: DiagnosticsQueryKeyFactory.providerStats(),
		enabled: getEnabled(),
		staleTime: STALE_LIVE,
		refetchInterval: POLL_INTERVAL_MS,
		refetchIntervalInBackground: false,
		refetchOnWindowFocus: false,
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/admin/provider-stats'), { signal })
	}));

/** The server's recent warnings and errors, only those of one ref when `getRef` gives one. */
export const getRecentProblemsQuery = (getRef: Getter<string>) =>
	createQuery(() => {
		const reference = getRef().trim();
		return {
			queryKey: DiagnosticsQueryKeyFactory.recentProblems(reference),
			staleTime: STALE_LIVE,
			refetchOnWindowFocus: false,
			queryFn: ({ signal }) =>
				api.v3.GET(
					v3('/api/v3/admin/problems/recent', { query: reference ? { ref: reference } : {} }),
					{ signal }
				)
		};
	});
