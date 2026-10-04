import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import { REQUESTS_ENDPOINTS, type HistoryParams } from './endpoints';
import { RequestQueryKeyFactory } from './RequestQueryKeyFactory';

// Live progress list: polls on a short cadence while the tab is visible so
// download states advance without a manual refresh.
export const getActiveRequestsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: RequestQueryKeyFactory.active(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(REQUESTS_ENDPOINTS.active(), { signal }),
		enabled: getEnabled() && !!authStore.user?.id,
		staleTime: 0,
		refetchInterval: 5_000,
		refetchIntervalInBackground: false
	}));

// Cheap R10 badge count: admins poll this instead of the full list.
export const getActiveRequestCountQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: RequestQueryKeyFactory.activeCount(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(REQUESTS_ENDPOINTS.activeCount(), { signal }),
		enabled: getEnabled() && !!authStore.user?.id,
		staleTime: 0,
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
				api.global.v3.GET(REQUESTS_ENDPOINTS.history(params), { signal }),
			enabled: getEnabled() && !!authStore.user?.id
		};
	});

export const getApprovalsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: RequestQueryKeyFactory.approvals(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(REQUESTS_ENDPOINTS.approvals(), { signal }),
		enabled: getEnabled() && !!authStore.user?.id
	}));

// Note: the approvals-count endpoint has exactly one reader,
// getPendingApprovalCountQuery in the following slice (nav badge). Do not
// add a second hook here; a duplicate key would split the badge cache.
