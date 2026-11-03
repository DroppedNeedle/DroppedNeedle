import { historyQuery } from '$lib/features/requests/queries/historyQuery';
import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import { type HistoryParams } from './historyQuery';
import { RequestQueryKeyFactory } from './RequestQueryKeyFactory';
import { STALE_LIVE } from '$lib/queries/staleTime';

// Live progress list: `downloads.changed` refreshes it as download states move;
// the interval is a safety net for a missed event and for changes no download
// event announces.
export const getActiveRequestsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: RequestQueryKeyFactory.active(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/requests/active'), { signal }),
		enabled: getEnabled() && !!authStore.user?.id,
		staleTime: STALE_LIVE,
		refetchInterval: 60_000,
		refetchIntervalInBackground: false
	}));

// Cheap badge count: admins poll this instead of the full list.
export const getActiveRequestCountQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: RequestQueryKeyFactory.activeCount(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/requests/active/count'), { signal }),
		enabled: getEnabled() && !!authStore.user?.id,
		staleTime: STALE_LIVE,
		refetchInterval: 30_000,
		refetchIntervalInBackground: false
	}));

export const getRequestHistoryQuery = (
	getParams: Getter<HistoryParams>,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => {
		const params = getParams();
		return {
			queryKey: RequestQueryKeyFactory.history(authStore.user?.id, params),
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.v3.GET(v3('/api/v3/requests/history', { query: historyQuery(params) }), { signal }),
			enabled: getEnabled() && !!authStore.user?.id
		};
	});

export const getApprovalsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: RequestQueryKeyFactory.approvals(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/requests/approvals'), { signal }),
		enabled: getEnabled() && !!authStore.user?.id
	}));

// Note: the approvals-count endpoint has exactly one reader,
// getPendingApprovalCountQuery in the following slice (nav badge). Do not
// add a second hook here; a duplicate key would split the badge cache.
