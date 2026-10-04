import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { FollowQueryKeyFactory } from './FollowQueryKeyFactory';
import { FOLLOW_ENDPOINTS } from './endpoints';
import { authStore } from '$lib/stores/authStore.svelte';
import { REQUESTS_ENDPOINTS } from '../requests/endpoints';

type Getter<T> = () => T;

export const getAutoDownloadApprovalsQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.adminApprovals(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(FOLLOW_ENDPOINTS.adminApprovals(), { signal }),
		enabled: getEnabled()
	}));

// The bulk "Lidarr Import" approval cards (LidarrImport D3).
export const getAutoDownloadApprovalBatchesQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.adminApprovalBatches(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(FOLLOW_ENDPOINTS.adminApprovalBatches(), {
				signal
			}),
		enabled: getEnabled()
	}));

export const getPendingApprovalCountQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.pendingApprovalCount(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(REQUESTS_ENDPOINTS.approvalsCount(), { signal }),
		enabled: getEnabled() && !!authStore.user?.id,
		staleTime: 0,
		refetchInterval: 120_000,
		refetchIntervalInBackground: false,
		refetchOnReconnect: 'always' as const,
		refetchOnWindowFocus: 'always' as const
	}));
